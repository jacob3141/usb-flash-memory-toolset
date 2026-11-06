// SPDX-License-Identifier: GPL-3.0-or-later
//! FAT12/16/32 formatter following Microsoft's "FAT: General Overview of On-Disk Format".
//!
//! The FAT variant is defined by the cluster count alone, so the layout planner
//! picks a cluster size first and verifies that the count fits the variant.

use super::FormatInfo;
use crate::device::{BlockDevice, write_sectors, zero_range};
use crate::error::{Error, Result};
use crate::pattern::os_random;
use crate::progress::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatKind {
    Fat12,
    Fat16,
    Fat32,
}

impl FatKind {
    fn name(self) -> &'static str {
        match self {
            FatKind::Fat12 => "FAT12",
            FatKind::Fat16 => "FAT16",
            FatKind::Fat32 => "FAT32",
        }
    }
    fn fits(self, clusters: u64) -> bool {
        match self {
            FatKind::Fat12 => (1..4085).contains(&clusters),
            FatKind::Fat16 => (4085..65525).contains(&clusters),
            FatKind::Fat32 => (65525..0x0FFF_FFF5).contains(&clusters),
        }
    }
    fn entry_bits(self) -> u64 {
        match self {
            FatKind::Fat12 => 12,
            FatKind::Fat16 => 16,
            FatKind::Fat32 => 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub kind: FatKind,
    pub sector_size: u64,
    pub total_sectors: u64,
    pub sec_per_cluster: u64,
    pub reserved: u64,
    pub fat_sectors: u64,
    pub root_entries: u64,
    pub root_sectors: u64,
    pub clusters: u64,
}

const FATS: u64 = 2;
const ROOT_ENTRIES: u64 = 512;

fn try_layout(kind: FatKind, ss: u64, total: u64, cluster_bytes: u64) -> Option<Layout> {
    let spc = cluster_bytes / ss;
    if spc == 0 || !spc.is_power_of_two() || spc > 128 {
        return None;
    }
    let (min_reserved, root_entries) = match kind {
        FatKind::Fat32 => (32, 0),
        _ => (1, ROOT_ENTRIES),
    };
    let root_sectors = (root_entries * 32).div_ceil(ss);

    // The FAT must cover the clusters, which shrink as the FAT grows. Iterating
    // from zero converges on the smallest sufficient FAT.
    let mut fat_sectors = 0u64;
    let (reserved, clusters) = loop {
        let meta = min_reserved + FATS * fat_sectors + root_sectors;
        // Keep the data area cluster aligned, which also aligns it to flash pages.
        let reserved = min_reserved + (spc - meta % spc) % spc;
        let data_start = reserved + FATS * fat_sectors + root_sectors;
        let clusters = total.checked_sub(data_start)? / spc;
        let needed = ((clusters + 2) * kind.entry_bits()).div_ceil(8).div_ceil(ss);
        if needed <= fat_sectors {
            break (reserved, clusters);
        }
        fat_sectors = needed;
    };
    kind.fits(clusters).then_some(Layout {
        kind,
        sector_size: ss,
        total_sectors: total,
        sec_per_cluster: spc,
        reserved,
        fat_sectors,
        root_entries,
        root_sectors,
        clusters,
    })
}

/// Chooses the layout for a volume of `total` sectors.
pub fn plan(ss: u64, total: u64) -> Result<Layout> {
    let bytes = ss * total;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= (1u64 << 32) * ss {
        return Err(Error::Invalid("volume is too large for FAT; use exFAT".into()));
    }
    let candidates: Vec<(FatKind, u64)> = if bytes >= 512 * MIB {
        // Microsoft's default cluster sizes for FAT32.
        let c = match bytes {
            b if b <= 8 * GIB => 4096,
            b if b <= 16 * GIB => 8192,
            b if b <= 32 * GIB => 16384,
            _ => 32768,
        };
        // Beyond 2 TiB with 512 byte sectors only larger clusters would fit.
        vec![(FatKind::Fat32, c)]
    } else {
        // Small sticks: the smallest cluster that fits the variant.
        let kind = if bytes >= 16 * MIB { FatKind::Fat16 } else { FatKind::Fat12 };
        [512, 1024, 2048, 4096, 8192, 16384, 32768].into_iter().map(|c| (kind, c)).collect()
    };
    candidates
        .into_iter()
        .filter(|&(_, c)| c >= ss)
        .find_map(|(k, c)| try_layout(k, ss, total, c))
        .ok_or_else(|| Error::Invalid(format!("no valid FAT layout for a volume of {bytes} bytes")))
}

fn sanitize_label(label: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let clean = label
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .map(|c| if c.is_ascii_alphanumeric() || " _-!#$%&'()@^`{}~".contains(c) { c as u8 } else { b'_' });
    for (o, c) in out.iter_mut().zip(clean) {
        *o = c;
    }
    if out == [b' '; 11] {
        out.copy_from_slice(b"NO NAME    ");
    }
    out
}

fn boot_sector(l: &Layout, hidden: u64, volume_id: u32, label: &[u8; 11]) -> Vec<u8> {
    let mut s = vec![0u8; l.sector_size as usize];
    let fat32 = l.kind == FatKind::Fat32;
    s[0..3].copy_from_slice(if fat32 { &[0xEB, 0x58, 0x90] } else { &[0xEB, 0x3C, 0x90] });
    s[3..11].copy_from_slice(b"MSWIN4.1");
    s[11..13].copy_from_slice(&(l.sector_size as u16).to_le_bytes());
    s[13] = l.sec_per_cluster as u8;
    s[14..16].copy_from_slice(&(l.reserved as u16).to_le_bytes());
    s[16] = FATS as u8;
    s[17..19].copy_from_slice(&(l.root_entries as u16).to_le_bytes());
    if !fat32 && l.total_sectors < 65536 {
        s[19..21].copy_from_slice(&(l.total_sectors as u16).to_le_bytes());
    } else {
        s[32..36].copy_from_slice(&(l.total_sectors as u32).to_le_bytes());
    }
    s[21] = 0xF8;
    s[24..26].copy_from_slice(&63u16.to_le_bytes());
    s[26..28].copy_from_slice(&255u16.to_le_bytes());
    s[28..32].copy_from_slice(&(hidden as u32).to_le_bytes());
    let fstype: &[u8; 8] = match l.kind {
        FatKind::Fat12 => b"FAT12   ",
        FatKind::Fat16 => b"FAT16   ",
        FatKind::Fat32 => b"FAT32   ",
    };
    let ext = if fat32 {
        s[36..40].copy_from_slice(&(l.fat_sectors as u32).to_le_bytes());
        s[44..48].copy_from_slice(&2u32.to_le_bytes()); // root directory cluster
        s[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo sector
        s[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
        64
    } else {
        s[22..24].copy_from_slice(&(l.fat_sectors as u16).to_le_bytes());
        36
    };
    s[ext] = 0x80;
    s[ext + 2] = 0x29;
    s[ext + 3..ext + 7].copy_from_slice(&volume_id.to_le_bytes());
    s[ext + 7..ext + 18].copy_from_slice(label);
    s[ext + 18..ext + 26].copy_from_slice(fstype);
    s[510] = 0x55;
    s[511] = 0xAA;
    s
}

fn fs_info(l: &Layout) -> Vec<u8> {
    let mut s = vec![0u8; l.sector_size as usize];
    s[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    s[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    s[488..492].copy_from_slice(&((l.clusters - 1) as u32).to_le_bytes());
    s[492..496].copy_from_slice(&3u32.to_le_bytes());
    s[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
    s
}

/// Formats `start..start + size` of the device.
pub fn format(dev: &mut dyn BlockDevice, start: u64, size: u64, label: &str, ctx: &mut Ctx) -> Result<FormatInfo> {
    let ss = dev.sector_size() as u64;
    let l = plan(ss, size / ss)?;
    let label_bytes = sanitize_label(label);
    let mut id = [0u8; 4];
    os_random(&mut id)?;
    let boot = boot_sector(&l, start / ss, u32::from_le_bytes(id), &label_bytes);

    let cluster_bytes = l.sec_per_cluster * ss;
    let fat_bytes = l.fat_sectors * ss;
    let root_bytes = if l.kind == FatKind::Fat32 { cluster_bytes } else { l.root_sectors * ss };
    let meta = l.reserved * ss + FATS * fat_bytes + root_bytes;

    ctx.begin_phase(format!("Creating {} filesystem", l.kind.name()), meta);
    ctx.check_cancel()?;
    zero_range(dev, start, meta)?;

    // FAT copies with the reserved entries.
    let mut first = vec![0u8; ss as usize];
    match l.kind {
        FatKind::Fat12 => first[..3].copy_from_slice(&[0xF8, 0xFF, 0xFF]),
        FatKind::Fat16 => first[..4].copy_from_slice(&[0xF8, 0xFF, 0xFF, 0xFF]),
        FatKind::Fat32 => {
            first[..4].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes());
            first[4..8].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
            // Cluster 2 holds the root directory.
            first[8..12].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
        }
    }
    for i in 0..FATS {
        write_sectors(dev, start + l.reserved * ss + i * fat_bytes, &first)?;
    }

    // Volume label entry as first root directory entry.
    if &label_bytes != b"NO NAME    " {
        let mut root = vec![0u8; ss as usize];
        root[..11].copy_from_slice(&label_bytes);
        root[11] = 0x08;
        write_sectors(dev, start + l.reserved * ss + FATS * fat_bytes, &root)?;
    }

    if l.kind == FatKind::Fat32 {
        let info = fs_info(&l);
        write_sectors(dev, start + ss, &info)?;
        write_sectors(dev, start + 6 * ss, &boot)?;
        write_sectors(dev, start + 7 * ss, &info)?;
    }
    // The boot sector goes last: until here the volume is not recognized as valid.
    write_sectors(dev, start, &boot)?;
    ctx.update(meta, 0);

    Ok(FormatInfo {
        filesystem: l.kind.name().to_string(),
        cluster_size: cluster_bytes as u32,
        clusters: l.clusters,
        label: String::from_utf8_lossy(&label_bytes).trim().to_string(),
        mbr_type: match l.kind {
            FatKind::Fat12 => 0x01,
            FatKind::Fat16 => 0x0E,
            FatKind::Fat32 => 0x0C,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsdetect::detect_in_buffer;
    use crate::testutil::MemDevice;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    fn check_invariants(l: &Layout) {
        let data = l.reserved + FATS * l.fat_sectors + l.root_sectors;
        assert!(l.clusters * l.sec_per_cluster + data <= l.total_sectors, "{l:?}");
        assert_eq!(data % l.sec_per_cluster, 0, "data area not aligned: {l:?}");
        let needed = ((l.clusters + 2) * l.kind.entry_bits()).div_ceil(8).div_ceil(l.sector_size);
        assert!(l.fat_sectors >= needed, "FAT too small: {l:?}");
        assert!(l.kind.fits(l.clusters), "{l:?}");
    }

    #[test]
    fn variant_follows_size() {
        for (bytes, kind) in [
            (MIB, FatKind::Fat12),
            (8 * MIB, FatKind::Fat12),
            (16 * MIB, FatKind::Fat16),
            (128 * MIB, FatKind::Fat16),
            (511 * MIB, FatKind::Fat16),
            (512 * MIB, FatKind::Fat32),
            (8 * GIB, FatKind::Fat32),
            (32 * GIB, FatKind::Fat32),
            (256 * GIB, FatKind::Fat32),
            (2047 * GIB, FatKind::Fat32),
        ] {
            let l = plan(512, bytes / 512).unwrap();
            assert_eq!(l.kind, kind, "{bytes}");
            check_invariants(&l);
        }
    }

    #[test]
    fn layouts_are_valid_across_many_sizes() {
        for ss in [512u64, 4096] {
            let mut bytes = 2 * MIB;
            while bytes < 4 * GIB {
                let l = plan(ss, bytes / ss).unwrap_or_else(|e| panic!("{bytes} @ {ss}: {e}"));
                check_invariants(&l);
                bytes = bytes * 9 / 8 / ss * ss;
            }
        }
    }

    #[test]
    fn formatted_volume_is_detected_with_label() {
        for (bytes, name) in [(4 * MIB, "FAT12"), (64 * MIB, "FAT16"), (600 * MIB, "FAT32")] {
            let mut d = MemDevice::new(bytes as usize, 512);
            let info = format(&mut d, 0, bytes, "my stick", &mut Ctx::silent()).unwrap();
            assert_eq!(info.filesystem, name);
            let f = detect_in_buffer(&d.data[..128 * 1024]).unwrap();
            assert_eq!(f.name, name);
            assert_eq!(f.label, "MY STICK");
            assert!(f.windows_native);
        }
    }

    #[test]
    fn fat32_has_consistent_backup_and_fsinfo() {
        let bytes = 600 * MIB;
        let mut d = MemDevice::new(bytes as usize, 512);
        format(&mut d, 0, bytes, "", &mut Ctx::silent()).unwrap();
        assert_eq!(d.data[..512], d.data[6 * 512..7 * 512]);
        assert_eq!(&d.data[512..516], &0x4161_5252u32.to_le_bytes());
        assert_eq!(d.data[512..1024], d.data[7 * 512..8 * 512]);
        // Empty label stays "NO NAME" and no label entry is written.
        assert_eq!(&d.data[71..82], b"NO NAME    ");
    }

    #[test]
    fn both_fats_are_identical() {
        let bytes = 64 * MIB;
        let mut d = MemDevice::new(bytes as usize, 512);
        format(&mut d, 0, bytes, "x", &mut Ctx::silent()).unwrap();
        let l = plan(512, bytes / 512).unwrap();
        let a = (l.reserved * 512) as usize;
        let f = (l.fat_sectors * 512) as usize;
        assert_eq!(d.data[a..a + f], d.data[a + f..a + 2 * f]);
        assert_eq!(&d.data[a..a + 4], &[0xF8, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn label_is_sanitized() {
        assert_eq!(&sanitize_label("abc"), b"ABC        ");
        assert_eq!(&sanitize_label("a/b:c"), b"A_B_C      ");
        assert_eq!(&sanitize_label("waaaaay too long name"), b"WAAAAAY TOO");
        assert_eq!(&sanitize_label(""), b"NO NAME    ");
    }

    #[test]
    fn too_large_is_rejected() {
        assert!(plan(512, 1u64 << 33).is_err());
    }
}
