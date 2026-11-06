// SPDX-License-Identifier: GPL-3.0-or-later
//! Device description and the block device abstraction.
//!
//! Every algorithm works on [`BlockDevice`], so it can be tested against plain
//! image files through [`FileDevice`] without any hardware.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::{Deref, DerefMut};
use std::path::Path;

/// Default transfer size of all sequential operations.
pub const CHUNK_SIZE: usize = 1 << 20;
/// Alignment of all I/O buffers. Unbuffered raw device access requires
/// sector-aligned memory and 4 KiB covers every sector size we support.
pub const BUFFER_ALIGN: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusType {
    Usb,
    Sd,
    Mmc,
    Sata,
    Nvme,
    Scsi,
    Virtual,
    Other,
}

impl BusType {
    /// Buses that can carry the removable flash media this tool is made for.
    pub fn is_flash_bus(self) -> bool {
        matches!(self, BusType::Usb | BusType::Sd | BusType::Mmc)
    }

    pub fn label(self) -> &'static str {
        match self {
            BusType::Usb => "USB",
            BusType::Sd => "SD",
            BusType::Mmc => "MMC",
            BusType::Sata => "SATA",
            BusType::Nvme => "NVMe",
            BusType::Scsi => "SCSI",
            BusType::Virtual => "Virtual",
            BusType::Other => "Other",
        }
    }
}

/// Everything known about a disk from the operating system, without reading it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Operating system path, e.g. `\\.\PhysicalDrive3` or `/dev/sdb`.
    pub path: String,
    pub vendor: String,
    pub model: String,
    pub serial: String,
    pub bus: BusType,
    /// The OS reports removable media. Many USB SSDs and some sticks report "fixed".
    pub removable_media: bool,
    pub media_present: bool,
    pub size_bytes: u64,
    pub logical_sector: u32,
    pub physical_sector: u32,
    pub read_only: bool,
    /// The disk carries the running OS or this program. Never written to.
    pub is_system: bool,
    pub mount_points: Vec<String>,
    /// Internal volume handles, e.g. `\\?\Volume{...}` or `/dev/sdb1`.
    pub volumes: Vec<String>,
}

impl DeviceInfo {
    pub fn display_name(&self) -> String {
        let name = format!("{} {}", self.vendor.trim(), self.model.trim());
        let name = name.trim();
        if name.is_empty() { "Unknown device".to_string() } else { name.to_string() }
    }

    /// True if `other` is still the same physical device as `self`.
    pub fn same_identity(&self, other: &DeviceInfo) -> bool {
        self.path == other.path
            && self.model == other.model
            && self.serial == other.serial
            && self.size_bytes == other.size_bytes
    }
}

/// Sector addressed storage.
///
/// Contract: `offset` and buffer length are multiples of [`sector_size`], and
/// buffers are aligned to [`BUFFER_ALIGN`]; use [`AlignedBuf`]. `FileDevice` asserts
/// this in debug builds so tests catch violations that raw devices would reject.
///
/// [`sector_size`]: BlockDevice::sector_size
pub trait BlockDevice: Send {
    fn size(&self) -> u64;
    fn sector_size(&self) -> u32;
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    /// Called once after an operation finished: releases volume locks and asks
    /// the OS to re-read the partition table.
    fn finish(&mut self) -> Result<()> {
        self.flush().map_err(Error::Io)
    }
}

/// Heap buffer aligned to [`BUFFER_ALIGN`].
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    capacity: usize,
}

// SAFETY: the buffer exclusively owns its allocation.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> Self {
        let capacity = len.max(1);
        let layout = Layout::from_size_align(capacity, BUFFER_ALIGN).expect("valid buffer layout");
        // SAFETY: the layout has a non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Self { ptr, len, capacity }
    }

    pub fn from_slice(data: &[u8]) -> Self {
        let mut b = Self::new(data.len());
        b.copy_from_slice(data);
        b
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.capacity, BUFFER_ALIGN).expect("valid layout");
        // SAFETY: allocated in `new` with exactly this layout.
        unsafe { dealloc(self.ptr, layout) };
    }
}

impl Deref for AlignedBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `len` initialized bytes.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl DerefMut for AlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` is valid for `len` bytes and uniquely borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

/// Reads an arbitrary byte range, handling sector alignment internally.
/// Returns an error if any covered sector cannot be read.
pub fn read_unaligned(dev: &mut dyn BlockDevice, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let ss = dev.sector_size() as u64;
    let start = offset / ss * ss;
    let end = (offset + len as u64).div_ceil(ss) * ss;
    let mut buf = AlignedBuf::new((end - start) as usize);
    dev.read_at(start, &mut buf)?;
    let skip = (offset - start) as usize;
    Ok(buf[skip..skip + len].to_vec())
}

/// Writes sector-aligned data from an unaligned source buffer.
pub fn write_sectors(dev: &mut dyn BlockDevice, offset: u64, data: &[u8]) -> io::Result<()> {
    let buf = AlignedBuf::from_slice(data);
    dev.write_at(offset, &buf)
}

/// Writes zeros to a sector-aligned byte range.
pub fn zero_range(dev: &mut dyn BlockDevice, offset: u64, len: u64) -> io::Result<()> {
    let ss = dev.sector_size() as usize;
    let chunk = CHUNK_SIZE / ss * ss;
    let buf = AlignedBuf::new(chunk);
    let mut pos = 0u64;
    while pos < len {
        let n = (len - pos).min(chunk as u64) as usize;
        dev.write_at(offset + pos, &buf[..n])?;
        pos += n as u64;
    }
    Ok(())
}

/// A plain file used as a device: for disk images and all automated tests.
pub struct FileDevice {
    file: File,
    size: u64,
    sector_size: u32,
}

impl FileDevice {
    pub fn open(path: &Path, sector_size: u32, writable: bool) -> Result<Self> {
        if !sector_size.is_power_of_two() || !(512..=4096).contains(&sector_size) {
            return Err(Error::Invalid(format!("unsupported sector size {sector_size}")));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .map_err(|e| Error::from_open(e, &path.display().to_string()))?;
        let size = file.metadata()?.len() / sector_size as u64 * sector_size as u64;
        Ok(Self { file, size, sector_size })
    }

    /// Creates or truncates an image of the given size.
    pub fn create(path: &Path, size: u64, sector_size: u32) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?;
        file.set_len(size)?;
        drop(file);
        Self::open(path, sector_size, true)
    }

    fn check(&self, offset: u64, len: usize, ptr: *const u8) {
        let ss = self.sector_size as u64;
        debug_assert_eq!(offset % ss, 0, "unaligned offset {offset}");
        debug_assert_eq!(len as u64 % ss, 0, "unaligned length {len}");
        debug_assert_eq!(ptr as usize % BUFFER_ALIGN, 0, "unaligned buffer");
    }
}

impl BlockDevice for FileDevice {
    fn size(&self) -> u64 {
        self.size
    }
    fn sector_size(&self) -> u32 {
        self.sector_size
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.check(offset, buf.len(), buf.as_ptr());
        positioned::read_exact_at(&self.file, buf, offset)
    }
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.check(offset, buf.len(), buf.as_ptr());
        positioned::write_all_at(&self.file, buf, offset)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

/// Positioned I/O that behaves the same on Windows and Unix.
pub(crate) mod positioned {
    use std::fs::File;
    use std::io;

    #[cfg(unix)]
    use std::os::unix::fs::FileExt;
    #[cfg(windows)]
    use std::os::windows::fs::FileExt;

    fn read_some(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
        #[cfg(unix)]
        return f.read_at(buf, off);
        #[cfg(windows)]
        return f.seek_read(buf, off);
    }

    fn write_some(f: &File, buf: &[u8], off: u64) -> io::Result<usize> {
        #[cfg(unix)]
        return f.write_at(buf, off);
        #[cfg(windows)]
        return f.seek_write(buf, off);
    }

    pub fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match read_some(f, buf, off) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    off += n as u64;
                    buf = &mut buf[n..];
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match write_some(f, buf, off) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    off += n as u64;
                    buf = &buf[n..];
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Merges failed byte ranges into a compact, sorted list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RangeList {
    pub ranges: Vec<ByteRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ByteRange {
    pub offset: u64,
    pub len: u64,
}

impl RangeList {
    /// Adds a range. Callers add ranges in ascending order; adjacent ones merge.
    pub fn push(&mut self, offset: u64, len: u64) {
        if let Some(last) = self.ranges.last_mut()
            && last.offset + last.len == offset
        {
            last.len += len;
            return;
        }
        self.ranges.push(ByteRange { offset, len });
    }

    pub fn total(&self) -> u64 {
        self.ranges.iter().map(|r| r.len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn first_offset(&self) -> Option<u64> {
        self.ranges.first().map(|r| r.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buffer_is_aligned_and_zeroed() {
        let b = AlignedBuf::new(8192);
        assert_eq!(b.as_ptr() as usize % BUFFER_ALIGN, 0);
        assert!(b.iter().all(|&x| x == 0));
    }

    #[test]
    fn range_list_merges_adjacent() {
        let mut r = RangeList::default();
        r.push(0, 10);
        r.push(10, 5);
        r.push(100, 1);
        assert_eq!(r.ranges.len(), 2);
        assert_eq!(r.total(), 16);
        assert_eq!(r.first_offset(), Some(0));
    }

    fn info() -> DeviceInfo {
        DeviceInfo {
            path: "p".into(),
            vendor: "V".into(),
            model: "M".into(),
            serial: "S".into(),
            bus: BusType::Usb,
            removable_media: true,
            media_present: true,
            size_bytes: 1,
            logical_sector: 512,
            physical_sector: 512,
            read_only: false,
            is_system: false,
            mount_points: vec![],
            volumes: vec![],
        }
    }

    #[test]
    fn identity_detects_a_swapped_stick() {
        let a = info();
        let mut b = info();
        assert!(a.same_identity(&b));
        b.serial = "other".into();
        assert!(!a.same_identity(&b));
    }
}
