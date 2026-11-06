// SPDX-License-Identifier: GPL-3.0-or-later
//! One-click factory reset: erase, one aligned partition, fresh filesystem.

use crate::device::{BlockDevice, zero_range};
use crate::error::{Error, Result};
use crate::format::{self, FormatInfo, FsChoice};
use crate::fsdetect;
use crate::partition::{TableKind, gpt, mbr, read_table};
use crate::pattern::os_random;
use crate::progress::Ctx;
use crate::wipe::{self, WipeMode, WipeReport, WipeStatus};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Removes what quick mode leaves behind at both ends: every partition table
/// location and the metadata of all common filesystems.
const QUICK_ERASE_BYTES: u64 = 32 * 1024 * 1024;
/// MBR cannot address more than 2^32 sectors.
const MBR_LIMIT_SECTORS: u64 = 1 << 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EraseChoice {
    /// Clear only the metadata at both ends. Fast, but old data stays recoverable.
    Quick,
    /// Overwrite every sector and verify.
    Full(WipeMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TableChoice {
    /// MBR up to 2 TiB, the factory default; GPT beyond.
    Auto,
    Mbr,
    Gpt,
    /// Filesystem directly on the device.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetOptions {
    pub erase: EraseChoice,
    pub filesystem: FsChoice,
    pub table: TableChoice,
    pub label: String,
}

impl Default for ResetOptions {
    fn default() -> Self {
        Self {
            erase: EraseChoice::Full(WipeMode::Random),
            filesystem: FsChoice::Auto,
            table: TableChoice::Auto,
            label: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetReport {
    pub wipe: Option<WipeReport>,
    pub format: Option<FormatInfo>,
    pub table: TableKind,
    pub partition_start: u64,
    pub partition_size: u64,
    /// Reading the result back confirmed that table and filesystem are valid.
    pub verified: bool,
    /// All old data was verifiably overwritten. False after a quick erase.
    pub data_erased: bool,
    pub problems: Vec<String>,
    pub duration_secs: f64,
}

impl ResetReport {
    pub fn success(&self) -> bool {
        self.verified && self.problems.is_empty()
    }
}

/// Partition start aligned to typical flash erase block sizes.
fn partition_start(size: u64, ss: u64) -> u64 {
    const MIB: u64 = 1024 * 1024;
    let start = if size >= GIB_1 {
        4 * MIB
    } else if size >= 16 * MIB {
        MIB
    } else {
        64 * 1024
    };
    start.max(ss * 34).div_ceil(ss) * ss
}
const GIB_1: u64 = 1024 * 1024 * 1024;

fn resolve_table(choice: TableChoice, fs: FsChoice, size: u64, ss: u64) -> TableKind {
    match choice {
        _ if fs == FsChoice::None => TableKind::None,
        TableChoice::Auto if size / ss > MBR_LIMIT_SECTORS => TableKind::Gpt,
        TableChoice::Auto | TableChoice::Mbr => TableKind::Mbr,
        TableChoice::Gpt => TableKind::Gpt,
        TableChoice::None => TableKind::None,
    }
}

fn erase_quick(dev: &mut dyn BlockDevice, ctx: &mut Ctx) -> Result<()> {
    let ss = dev.sector_size() as u64;
    let size = dev.size() / ss * ss;
    let n = QUICK_ERASE_BYTES.min(size) / ss * ss;
    ctx.begin_phase("Clearing old metadata", 2 * n);
    ctx.check_cancel()?;
    zero_range(dev, 0, n)?;
    ctx.update(n, 0);
    zero_range(dev, size - n, n)?;
    ctx.update(2 * n, 0);
    Ok(())
}

/// Resets the device. The partition table is written **last**: Windows and Linux
/// mount new partitions the moment they appear, and an announced but not yet
/// formatted volume would trigger "you need to format this disk" dialogs.
pub fn factory_reset(dev: &mut dyn BlockDevice, opts: &ResetOptions, ctx: &mut Ctx) -> Result<ResetReport> {
    let begin = Instant::now();
    let ss = dev.sector_size() as u64;
    let size = dev.size() / ss * ss;
    let mut problems = vec![];

    let table = resolve_table(opts.table, opts.filesystem, size, ss);
    if table == TableKind::Mbr && size / ss > MBR_LIMIT_SECTORS {
        return Err(Error::Invalid("the device is too large for MBR; use GPT".into()));
    }

    let (start, part_size) = match table {
        TableKind::None => (0, size),
        _ => {
            let s = partition_start(size, ss);
            let end = if table == TableKind::Gpt { gpt::usable_end(size, ss) } else { size };
            if end <= s {
                return Err(Error::Invalid("the device is too small for a partition".into()));
            }
            // End on a 1 MiB boundary where the device is large enough for it to matter.
            let granule = if size >= GIB_1 { 1024 * 1024 } else { ss };
            (s, (end - s) / granule * granule)
        }
    };

    let mut wipe_report = None;
    let mut data_erased = false;
    match opts.erase {
        EraseChoice::Quick => erase_quick(dev, ctx)?,
        EraseChoice::Full(mode) => {
            let r = wipe::wipe(dev, mode, true, ctx)?;
            data_erased = r.status == WipeStatus::Verified;
            if !data_erased {
                problems.push(format!(
                    "The wipe is incomplete: {} could not be overwritten or verified. \
                     Old data may remain; do not pass this stick on.",
                    crate::units::bytes(r.write_failures.total() + r.verify_failures.total())
                ));
            }
            let aborted = r.aborted;
            wipe_report = Some(r);
            if aborted {
                problems.push("The device stopped responding during the wipe.".to_string());
                return Ok(ResetReport {
                    wipe: wipe_report,
                    format: None,
                    table: TableKind::None,
                    partition_start: 0,
                    partition_size: 0,
                    verified: false,
                    data_erased: false,
                    problems,
                    duration_secs: begin.elapsed().as_secs_f64(),
                });
            }
        }
    }

    let format_info = format::format(dev, opts.filesystem, start, part_size, &opts.label, ctx)?;
    dev.flush()?;

    ctx.begin_phase("Writing partition table", 0);
    match (table, &format_info) {
        (TableKind::Mbr, Some(f)) => {
            let mut sig = [0u8; 4];
            os_random(&mut sig)?;
            mbr::write_single(dev, u32::from_le_bytes(sig), f.mbr_type, start, part_size)?;
        }
        (TableKind::Gpt, Some(_)) => {
            let (mut disk, mut part) = ([0u8; 16], [0u8; 16]);
            os_random(&mut disk)?;
            os_random(&mut part)?;
            for g in [&mut disk, &mut part] {
                // RFC 4122 version 4, in the on-disk field order.
                g[7] = (g[7] & 0x0F) | 0x40;
                g[8] = (g[8] & 0x3F) | 0x80;
            }
            let type_guid =
                gpt::parse_guid(gpt::BASIC_DATA).ok_or_else(|| Error::Invalid("bad partition type GUID".into()))?;
            gpt::write_single(dev, disk, part, type_guid, "USB Flash", start, part_size)?;
        }
        _ => {}
    }
    dev.flush()?;

    // Read everything back, as a user's OS would see it.
    ctx.begin_phase("Verifying result", 0);
    let verified = verify_result(dev, table, start, part_size, &format_info, &mut problems);

    Ok(ResetReport {
        wipe: wipe_report,
        format: format_info,
        table,
        partition_start: start,
        partition_size: part_size,
        verified,
        data_erased,
        problems,
        duration_secs: begin.elapsed().as_secs_f64(),
    })
}

fn verify_result(
    dev: &mut dyn BlockDevice,
    table: TableKind,
    start: u64,
    size: u64,
    format: &Option<FormatInfo>,
    problems: &mut Vec<String>,
) -> bool {
    let t = read_table(dev);
    let mut ok = true;
    if t.kind != table {
        problems.push(format!("Reading back found partition table {:?} instead of {table:?}", t.kind));
        ok = false;
    } else if table != TableKind::None {
        let matches = t.partitions.len() == 1 && t.partitions[0].start == start && t.partitions[0].size == size;
        if !matches || !t.issues.is_empty() {
            problems.push(format!("The partition table read back differently: {:?}", t.issues));
            ok = false;
        }
    }
    if let Some(f) = format {
        let found = fsdetect::probe(dev, start, size);
        if found.as_ref().map(|x| x.name.as_str()) != Some(f.filesystem.as_str()) {
            problems.push(format!("Reading back did not find the new {} filesystem", f.filesystem));
            ok = false;
        }
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::MemDevice;

    const MIB: usize = 1024 * 1024;

    fn opts(erase: EraseChoice, filesystem: FsChoice, table: TableChoice) -> ResetOptions {
        ResetOptions { erase, filesystem, table, label: "USB".into() }
    }

    fn dirty(size: usize, ss: u32) -> MemDevice {
        let mut d = MemDevice::new(size, ss);
        d.data.fill(0xCC);
        d
    }

    #[test]
    fn full_reset_erases_all_old_data_and_is_valid() {
        let mut d = dirty(64 * MIB, 512);
        let r = factory_reset(&mut d, &ResetOptions { label: "USB".into(), ..Default::default() }, &mut Ctx::silent())
            .unwrap();
        assert!(r.success(), "{:?}", r.problems);
        assert!(r.data_erased);
        assert_eq!(r.table, TableKind::Mbr);
        assert_eq!(r.format.as_ref().unwrap().filesystem, "FAT16");
        // No full sector of the old pattern survives anywhere.
        assert!(d.data.chunks(512).all(|s| s.iter().any(|&b| b != 0xCC)));
    }

    #[test]
    fn every_combination_reads_back_correctly() {
        for ss in [512u32, 4096] {
            for fs in [FsChoice::Auto, FsChoice::Fat, FsChoice::Exfat] {
                for table in [TableChoice::Auto, TableChoice::Mbr, TableChoice::Gpt, TableChoice::None] {
                    let mut d = dirty(64 * MIB, ss);
                    let r = factory_reset(&mut d, &opts(EraseChoice::Quick, fs, table), &mut Ctx::silent()).unwrap();
                    assert!(r.success(), "{ss} {fs:?} {table:?}: {:?}", r.problems);
                    assert!(!r.data_erased, "quick erase must not claim erasure");
                }
            }
        }
    }

    #[test]
    fn no_filesystem_leaves_a_blank_device() {
        let mut d = dirty(8 * MIB, 512);
        let r = factory_reset(
            &mut d,
            &opts(EraseChoice::Full(WipeMode::Zero), FsChoice::None, TableChoice::Auto),
            &mut Ctx::silent(),
        )
        .unwrap();
        assert!(r.success());
        assert_eq!(r.table, TableKind::None);
        assert!(d.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn partition_is_aligned_and_within_the_device() {
        for size in [8 * MIB, 64 * MIB, 2048 * MIB] {
            let mut d = MemDevice::new(size, 512);
            let r =
                factory_reset(&mut d, &opts(EraseChoice::Quick, FsChoice::Auto, TableChoice::Auto), &mut Ctx::silent())
                    .unwrap();
            assert!(r.success(), "{size}: {:?}", r.problems);
            assert_eq!(r.partition_start % (64 * 1024), 0);
            assert!(r.partition_start + r.partition_size <= size as u64);
        }
    }

    #[test]
    fn auto_filesystem_uses_exfat_above_32_gib() {
        let t = |size: u64, fs: FsChoice| match fs {
            FsChoice::Auto if size <= format::FAT_AUTO_LIMIT => "fat",
            FsChoice::Auto => "exfat",
            _ => "other",
        };
        assert_eq!(t(32 << 30, FsChoice::Auto), "fat");
        assert_eq!(t((32 << 30) + 1, FsChoice::Auto), "exfat");
    }

    #[test]
    fn table_resolution() {
        assert_eq!(resolve_table(TableChoice::Auto, FsChoice::Auto, 1 << 40, 512), TableKind::Mbr);
        assert_eq!(resolve_table(TableChoice::Auto, FsChoice::Auto, 3 << 40, 512), TableKind::Gpt);
        assert_eq!(resolve_table(TableChoice::Gpt, FsChoice::None, 1 << 40, 512), TableKind::None);
    }

    #[test]
    fn mbr_refuses_oversized_devices() {
        let mut d = MemDevice::fake(MIB, 3 << 41);
        let r = factory_reset(&mut d, &opts(EraseChoice::Quick, FsChoice::Exfat, TableChoice::Mbr), &mut Ctx::silent());
        assert!(matches!(r, Err(Error::Invalid(_))));
    }

    #[test]
    fn incomplete_wipe_is_reported_as_failure() {
        let mut d = dirty(8 * MIB, 512);
        d.dropped_writes.push((6 * MIB as u64, 7 * MIB as u64));
        let r = factory_reset(&mut d, &ResetOptions::default(), &mut Ctx::silent()).unwrap();
        assert!(!r.data_erased);
        assert!(!r.success());
    }

    #[test]
    fn dead_device_does_not_format() {
        let mut d = dirty(128 * MIB, 512);
        d.bad.push((0, 128 * MIB as u64));
        let r = factory_reset(&mut d, &ResetOptions::default(), &mut Ctx::silent()).unwrap();
        assert!(!r.success());
        assert!(r.format.is_none());
    }
}
