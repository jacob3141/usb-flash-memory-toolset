// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw image backup and restore.
//!
//! Backup is the way to rescue data from an unknown or failing stick before wiping
//! it: unreadable areas are zero-filled and listed, like GNU ddrescue's first pass.

use crate::device::{AlignedBuf, BlockDevice, RangeList};
use crate::error::{Error, Result};
use crate::progress::Ctx;
use crate::sweep::{Sweep, chunk_size};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct BackupReport {
    pub path: String,
    pub bytes: u64,
    /// Hash of the image file, including zero-filled unreadable areas.
    pub sha256: String,
    pub unreadable: RangeList,
    pub aborted: bool,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub verify_failures: RangeList,
    pub duration_secs: f64,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Copies the device to a new file. Never overwrites an existing file.
pub fn backup(dev: &mut dyn BlockDevice, path: &Path, ctx: &mut Ctx) -> Result<BackupReport> {
    let begin = Instant::now();
    let mut file = OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::Invalid(format!("{} already exists", path.display()))
        } else {
            Error::from_open(e, &path.display().to_string())
        }
    })?;
    let ss = dev.sector_size() as u64;
    let size = dev.size() / ss * ss;
    let chunk = chunk_size(dev.sector_size()) as u64;
    let mut buf = AlignedBuf::new(chunk as usize);
    let mut hash = Sha256::new();
    let mut sweep = Sweep::default();
    let mut offset = 0;
    let mut aborted = false;

    ctx.begin_phase("Backing up to image file", size);
    while offset < size {
        ctx.check_cancel()?;
        let len = chunk.min(size - offset) as usize;
        sweep.read(dev, offset, &mut buf[..len]);
        file.write_all(&buf[..len])?;
        hash.update(&buf[..len]);
        offset += len as u64;
        ctx.update(offset, sweep.bad.total());
        if sweep.gave_up() {
            aborted = true;
            break;
        }
    }
    file.sync_all()?;

    Ok(BackupReport {
        path: path.display().to_string(),
        bytes: offset,
        sha256: hex(&hash.finalize()),
        unreadable: sweep.bad,
        aborted,
        duration_secs: begin.elapsed().as_secs_f64(),
    })
}

/// Writes an image file to the device and verifies it by reading back.
pub fn restore(dev: &mut dyn BlockDevice, path: &Path, ctx: &mut Ctx) -> Result<RestoreReport> {
    let begin = Instant::now();
    let mut file = File::open(path).map_err(|e| Error::from_open(e, &path.display().to_string()))?;
    let len = file.metadata()?.len();
    let ss = dev.sector_size() as u64;
    if len == 0 || len % ss != 0 {
        return Err(Error::Invalid(format!("the image size {len} is not a multiple of the sector size {ss}")));
    }
    if len > dev.size() {
        return Err(Error::Invalid(format!(
            "the image ({}) is larger than the device ({})",
            crate::units::bytes(len),
            crate::units::bytes(dev.size())
        )));
    }
    let chunk = chunk_size(dev.sector_size()) as u64;
    let mut buf = AlignedBuf::new(chunk as usize);
    let mut hash = Sha256::new();

    ctx.begin_phase("Writing image to device", len);
    let mut offset = 0;
    while offset < len {
        ctx.check_cancel()?;
        let n = chunk.min(len - offset) as usize;
        file.read_exact(&mut buf[..n])?;
        hash.update(&buf[..n]);
        dev.write_at(offset, &buf[..n])?;
        offset += n as u64;
        ctx.update(offset, 0);
    }
    dev.flush()?;

    ctx.begin_phase("Verifying", len);
    file.seek(SeekFrom::Start(0))?;
    let mut read_back = AlignedBuf::new(chunk as usize);
    let mut failures = RangeList::default();
    let mut offset = 0;
    while offset < len {
        ctx.check_cancel()?;
        let n = chunk.min(len - offset) as usize;
        file.read_exact(&mut buf[..n])?;
        let ok = dev.read_at(offset, &mut read_back[..n]).is_ok() && buf[..n] == read_back[..n];
        if !ok {
            failures.push(offset, n as u64);
        }
        offset += n as u64;
        ctx.update(offset, failures.total());
    }

    Ok(RestoreReport {
        path: path.display().to_string(),
        bytes: len,
        sha256: hex(&hash.finalize()),
        verify_failures: failures,
        duration_secs: begin.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::MemDevice;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("usbflash-img-{name}-{}", std::process::id()));
        std::fs::remove_file(&p).ok();
        p
    }

    #[test]
    fn backup_and_restore_round_trip() {
        let p = temp("rt");
        let mut src = MemDevice::new(3 << 20, 512);
        for (i, b) in src.data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let a = backup(&mut src, &p, &mut Ctx::silent()).unwrap();
        assert_eq!(a.bytes, 3 << 20);
        assert!(a.unreadable.is_empty());

        let mut dst = MemDevice::new(4 << 20, 512);
        let b = restore(&mut dst, &p, &mut Ctx::silent()).unwrap();
        assert_eq!(a.sha256, b.sha256);
        assert!(b.verify_failures.is_empty());
        assert_eq!(dst.data[..3 << 20], src.data[..]);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn backup_never_overwrites_existing_files() {
        let p = temp("exists");
        std::fs::write(&p, b"precious").unwrap();
        let mut d = MemDevice::new(1 << 20, 512);
        assert!(backup(&mut d, &p, &mut Ctx::silent()).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"precious");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn unreadable_areas_are_zero_filled_and_listed() {
        let p = temp("bad");
        let mut d = MemDevice::new(2 << 20, 512);
        d.data.fill(0x11);
        d.bad.push((1 << 20, (1 << 20) + 512));
        let r = backup(&mut d, &p, &mut Ctx::silent()).unwrap();
        assert_eq!(r.unreadable.total(), 64 * 1024);
        let img = std::fs::read(&p).unwrap();
        assert_eq!(img.len(), 2 << 20);
        assert_eq!(img[1 << 20], 0);
        assert_eq!(img[(1 << 20) + 64 * 1024], 0x11);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn restore_rejects_oversized_and_misaligned_images() {
        let p = temp("size");
        std::fs::write(&p, vec![0u8; 2 << 20]).unwrap();
        let mut d = MemDevice::new(1 << 20, 512);
        assert!(matches!(restore(&mut d, &p, &mut Ctx::silent()), Err(Error::Invalid(_))));
        std::fs::write(&p, vec![0u8; 1000]).unwrap();
        assert!(matches!(restore(&mut d, &p, &mut Ctx::silent()), Err(Error::Invalid(_))));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn restore_detects_dropped_writes() {
        let p = temp("drop");
        std::fs::write(&p, vec![0x77u8; 2 << 20]).unwrap();
        let mut d = MemDevice::new(2 << 20, 512);
        d.dropped_writes.push((1 << 20, 2 << 20));
        let r = restore(&mut d, &p, &mut Ctx::silent()).unwrap();
        assert_eq!(r.verify_failures.first_offset(), Some(1 << 20));
        std::fs::remove_file(p).ok();
    }
}
