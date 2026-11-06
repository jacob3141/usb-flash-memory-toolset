// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows backend: `\\.\PhysicalDriveN` access through DeviceIoControl.

use crate::device::positioned::{read_exact_at, write_all_at};
use crate::device::{AlignedBuf, BlockDevice, BusType, DeviceInfo};
use crate::error::{Error, Result};
use std::ffi::{OsStr, c_void};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::ptr::{null, null_mut};
use std::thread::sleep;
use std::time::Duration;
use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_MORE_DATA, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH, FILE_SHARE_READ, FILE_SHARE_WRITE, FindFirstVolumeW,
    FindNextVolumeW, FindVolumeClose, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    GetVolumePathNamesForVolumeNameW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
const IOCTL_DISK_IS_WRITABLE: u32 = 0x0007_0024;
const IOCTL_DISK_DELETE_DRIVE_LAYOUT: u32 = 0x0007_C100;
const IOCTL_DISK_UPDATE_PROPERTIES: u32 = 0x0007_0140;
const IOCTL_DISK_SET_DISK_ATTRIBUTES: u32 = 0x0007_C0F4;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004_D014;
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
const FSCTL_UNLOCK_VOLUME: u32 = 0x0009_001C;

const DISK_ATTRIBUTE_OFFLINE: u64 = 0x1;
const DISK_ATTRIBUTE_READ_ONLY: u64 = 0x2;
const MAX_DISKS: u32 = 64;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// Thin wrapper over DeviceIoControl for plain in/out buffers.
///
/// SAFETY of callers: `input` and `output` must point to valid buffers of the
/// given sizes. Handled by the typed helpers below.
unsafe fn ioctl(
    file: &File,
    code: u32,
    input: *const c_void,
    in_len: u32,
    output: *mut c_void,
    out_len: u32,
) -> io::Result<u32> {
    let mut returned = 0u32;
    // SAFETY: guaranteed by the caller; the handle is valid while `file` lives.
    let ok = unsafe {
        DeviceIoControl(file.as_raw_handle() as _, code, input, in_len, output, out_len, &mut returned, null_mut())
    };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(returned) }
}

fn ioctl_none(file: &File, code: u32) -> io::Result<()> {
    // SAFETY: no buffers are passed.
    unsafe { ioctl(file, code, null(), 0, null_mut(), 0) }.map(|_| ())
}

fn ioctl_out<T>(file: &File, code: u32, out: &mut T) -> io::Result<u32> {
    // SAFETY: `out` is a valid, exclusive reference of the given size.
    unsafe { ioctl(file, code, null(), 0, out as *mut T as *mut c_void, size_of::<T>() as u32) }
}

/// Opens a path without requesting data access, which needs no elevation.
fn open_query(path: &str) -> io::Result<File> {
    OpenOptions::new().access_mode(0).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path)
}

// ---- Device properties ------------------------------------------------------

#[repr(C)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional: [u8; 1],
}

fn query_storage_descriptor(file: &File) -> io::Result<Vec<u8>> {
    let query = StoragePropertyQuery { property_id: 0, query_type: 0, additional: [0] };
    let mut out = vec![0u8; 4096];
    // SAFETY: both buffers are valid for the given sizes.
    let n = unsafe {
        ioctl(
            file,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &query as *const _ as *const c_void,
            size_of::<StoragePropertyQuery>() as u32,
            out.as_mut_ptr() as *mut c_void,
            out.len() as u32,
        )
    }?;
    out.truncate(n as usize);
    Ok(out)
}

fn descriptor_string(desc: &[u8], field: usize) -> String {
    let Some(raw) = desc.get(field..field + 4) else { return String::new() };
    let offset = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    if offset == 0 || offset >= desc.len() {
        return String::new();
    }
    let end = desc[offset..].iter().position(|&c| c == 0).map_or(desc.len(), |e| offset + e);
    String::from_utf8_lossy(&desc[offset..end]).trim().to_string()
}

fn bus_from_descriptor(code: u32) -> BusType {
    match code {
        0x07 => BusType::Usb,
        0x0C => BusType::Sd,
        0x0D => BusType::Mmc,
        0x0B => BusType::Sata,
        0x11 => BusType::Nvme,
        0x01 | 0x0A => BusType::Scsi,
        0x0E | 0x0F => BusType::Virtual,
        _ => BusType::Other,
    }
}

#[repr(C)]
#[derive(Default)]
struct DiskGeometry {
    cylinders: i64,
    media_type: u32,
    tracks_per_cylinder: u32,
    sectors_per_track: u32,
    bytes_per_sector: u32,
}

#[repr(C)]
#[derive(Default)]
struct DiskGeometryEx {
    geometry: DiskGeometry,
    disk_size: i64,
    data: [u8; 8],
}

#[repr(C)]
#[derive(Default)]
struct AccessAlignment {
    version: u32,
    size: u32,
    bytes_per_cache_line: u32,
    bytes_offset_cache_line: u32,
    bytes_per_logical: u32,
    bytes_per_physical: u32,
    bytes_offset_physical: u32,
}

fn physical_sector(file: &File, fallback: u32) -> u32 {
    let query = StoragePropertyQuery { property_id: 6, query_type: 0, additional: [0] };
    let mut out = AccessAlignment::default();
    // SAFETY: both buffers are valid for the given sizes.
    let r = unsafe {
        ioctl(
            file,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &query as *const _ as *const c_void,
            size_of::<StoragePropertyQuery>() as u32,
            &mut out as *mut _ as *mut c_void,
            size_of::<AccessAlignment>() as u32,
        )
    };
    if r.is_ok() && out.bytes_per_physical != 0 { out.bytes_per_physical } else { fallback }
}

// ---- Volumes ----------------------------------------------------------------

struct VolumeInfo {
    /// `\\?\Volume{...}` without a trailing backslash.
    name: String,
    disks: Vec<u32>,
    mount_points: Vec<String>,
}

fn volume_disks(volume: &str) -> Vec<u32> {
    let Ok(file) = open_query(volume) else { return vec![] };
    // VOLUME_DISK_EXTENTS: u32 count, padding, then 24 byte extents.
    let mut buf = vec![0u64; 3 * 32 + 1];
    // SAFETY: the buffer is valid for the given size.
    let r = unsafe {
        ioctl(
            &file,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            null(),
            0,
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * 8) as u32,
        )
    };
    if r.is_err() {
        return vec![];
    }
    let count = (buf[0] & 0xFFFF_FFFF) as usize;
    (0..count.min(32)).map(|i| (buf[1 + i * 3] & 0xFFFF_FFFF) as u32).collect()
}

fn volume_mount_points(volume: &str) -> Vec<String> {
    let name = wide(&format!("{volume}\\"));
    let mut buf = vec![0u16; 1024];
    let mut needed = 0u32;
    // SAFETY: the buffers are valid for the given sizes.
    let mut ok =
        unsafe { GetVolumePathNamesForVolumeNameW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32, &mut needed) };
    // SAFETY: trivially safe.
    if ok == 0 && unsafe { GetLastError() } == ERROR_MORE_DATA {
        buf.resize(needed as usize, 0);
        // SAFETY: as above, with the buffer resized to the requested length.
        ok =
            unsafe { GetVolumePathNamesForVolumeNameW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32, &mut needed) };
    }
    if ok == 0 {
        return vec![];
    }
    buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

fn list_volumes() -> Vec<VolumeInfo> {
    let mut out = vec![];
    let mut name = [0u16; 260];
    // SAFETY: the buffer is valid for its length.
    let handle = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if handle == INVALID_HANDLE_VALUE {
        return out;
    }
    loop {
        let full = from_wide(&name);
        let trimmed = full.trim_end_matches('\\').to_string();
        out.push(VolumeInfo {
            disks: volume_disks(&trimmed),
            mount_points: volume_mount_points(&trimmed),
            name: trimmed,
        });
        // SAFETY: handle from FindFirstVolumeW; the buffer is valid for its length.
        if unsafe { FindNextVolumeW(handle, name.as_mut_ptr(), name.len() as u32) } == 0 {
            break;
        }
    }
    // SAFETY: handle from FindFirstVolumeW, closed once.
    unsafe { FindVolumeClose(handle) };
    out
}

/// Resolves the disks holding the volume that contains `path`.
fn disks_of_path(path: &[u16]) -> Vec<u32> {
    let mut root = [0u16; 260];
    let mut volume = [0u16; 64];
    // SAFETY: `path` is NUL terminated; output buffers are valid for their lengths.
    unsafe {
        if GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) == 0 {
            return vec![];
        }
        if GetVolumeNameForVolumeMountPointW(root.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) == 0 {
            return vec![];
        }
    }
    volume_disks(from_wide(&volume).trim_end_matches('\\'))
}

/// Disks that must never be written: those with the OS or the running program.
/// Resolved through the OS, because the system drive need not be `C:`.
fn protected_disks() -> Result<Vec<u32>> {
    let mut sys = [0u16; 260];
    // SAFETY: the buffer is valid for its length.
    let n = unsafe { GetSystemDirectoryW(sys.as_mut_ptr(), sys.len() as u32) };
    let mut disks = if n > 0 { disks_of_path(&sys) } else { vec![] };
    if disks.is_empty() {
        // Without knowing the system disk, no decision about others can be safe.
        return Err(Error::Refused("cannot determine which disk holds the operating system".to_string()));
    }
    if let Ok(exe) = std::env::current_exe() {
        let w: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
        disks.extend(disks_of_path(&w));
    }
    Ok(disks)
}

// ---- Enumeration ------------------------------------------------------------

/// Lists all physical disks.
pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    let protected = protected_disks()?;
    let volumes = list_volumes();
    let mut out = vec![];
    for n in 0..MAX_DISKS {
        let path = format!("\\\\.\\PhysicalDrive{n}");
        let Ok(file) = open_query(&path) else { continue };
        let Ok(desc) = query_storage_descriptor(&file) else { continue };
        if desc.len() < 40 {
            continue;
        }
        let u32_at = |o: usize| u32::from_le_bytes([desc[o], desc[o + 1], desc[o + 2], desc[o + 3]]);

        let mut geo = DiskGeometryEx::default();
        let geo_ok = ioctl_out(&file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &mut geo).is_ok();
        let size = if geo_ok { geo.disk_size.max(0) as u64 } else { 0 };
        let logical = if geo_ok && geo.geometry.bytes_per_sector != 0 { geo.geometry.bytes_per_sector } else { 512 };
        let writable = ioctl_none(&file, IOCTL_DISK_IS_WRITABLE);
        // ERROR_WRITE_PROTECT is 19. Other errors do not tell us anything about it.
        let read_only = writable.as_ref().is_err_and(|e| e.raw_os_error() == Some(19));

        let mine: Vec<&VolumeInfo> = volumes.iter().filter(|v| v.disks.contains(&n)).collect();
        out.push(DeviceInfo {
            vendor: descriptor_string(&desc, 12),
            model: descriptor_string(&desc, 16),
            serial: descriptor_string(&desc, 24),
            bus: bus_from_descriptor(u32_at(28)),
            removable_media: desc[10] != 0,
            media_present: geo_ok && size > 0,
            size_bytes: size,
            logical_sector: logical,
            physical_sector: physical_sector(&file, logical),
            read_only,
            is_system: protected.contains(&n),
            mount_points: mine.iter().flat_map(|v| v.mount_points.clone()).collect(),
            volumes: mine.iter().map(|v| v.name.clone()).collect(),
            path,
        });
    }
    Ok(out)
}

// ---- Device access ----------------------------------------------------------

/// A raw disk handle, plus the locked volumes of an exclusive session.
struct RawDisk {
    file: File,
    size: u64,
    sector: u32,
    locked_volumes: Vec<File>,
    finished: bool,
}

impl RawDisk {
    fn open(info: &DeviceInfo, write: bool) -> Result<File> {
        let mut o = OpenOptions::new();
        o.read(true)
            .write(write)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
        o.open(&info.path).map_err(|e| Error::from_open(e, &info.path))
    }

    fn query_size(file: &File) -> Result<u64> {
        let mut len = 0i64;
        ioctl_out(file, IOCTL_DISK_GET_LENGTH_INFO, &mut len)?;
        Ok(len.max(0) as u64)
    }

    fn release(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let flushed = self.flush();
        for v in self.locked_volumes.drain(..) {
            ioctl_none(&v, FSCTL_UNLOCK_VOLUME).ok();
        }
        // Tell Windows about the new layout so volumes mount without replugging.
        ioctl_none(&self.file, IOCTL_DISK_UPDATE_PROPERTIES).ok();
        flushed.map_err(Error::Io)
    }
}

impl Drop for RawDisk {
    fn drop(&mut self) {
        self.release().ok();
    }
}

impl BlockDevice for RawDisk {
    fn size(&self) -> u64 {
        self.size
    }
    fn sector_size(&self) -> u32 {
        self.sector
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_at(&self.file, buf, offset)
    }
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        write_all_at(&self.file, buf, offset)
    }
    fn flush(&mut self) -> io::Result<()> {
        match self.file.sync_all() {
            // Some USB bridges do not implement flushing. All writes are
            // write-through anyway, and verification catches what got lost.
            Err(e) if e.raw_os_error() == Some(ERROR_INVALID_FUNCTION as i32) => Ok(()),
            other => other,
        }
    }
    fn finish(&mut self) -> Result<()> {
        self.release()
    }
}

/// Opens a disk for reading. Fails without administrator rights.
pub fn open_read(info: &DeviceInfo) -> Result<Box<dyn BlockDevice>> {
    let file = RawDisk::open(info, false)?;
    let size = RawDisk::query_size(&file)?;
    Ok(Box::new(RawDisk { file, size, sector: info.logical_sector, locked_volumes: vec![], finished: true }))
}

/// Locks and dismounts a volume. The returned handle keeps the lock.
fn lock_volume(volume: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(volume)
        .map_err(|e| Error::from_open(e, volume))?;
    let mut locked = false;
    for _ in 0..5 {
        if ioctl_none(&file, FSCTL_LOCK_VOLUME).is_ok() {
            locked = true;
            break;
        }
        sleep(Duration::from_millis(300));
    }
    // A forced dismount also works with files open. The user has confirmed that
    // all data on this device is to be destroyed, so it is safe to go on.
    ioctl_none(&file, FSCTL_DISMOUNT_VOLUME)?;
    if !locked {
        ioctl_none(&file, FSCTL_LOCK_VOLUME).ok();
    }
    Ok(file)
}

/// Removes software write protection and the offline state Windows applies
/// to disks according to its SAN policy.
fn clear_disk_attributes(file: &File) {
    #[repr(C)]
    struct SetDiskAttributes {
        version: u32,
        persist: u8,
        reserved1: [u8; 3],
        attributes: u64,
        attributes_mask: u64,
        reserved2: [u32; 4],
    }
    // SAFETY: an all-zero bit pattern is valid for this plain data struct.
    let mut a: SetDiskAttributes = unsafe { zeroed() };
    a.version = size_of::<SetDiskAttributes>() as u32;
    a.attributes_mask = DISK_ATTRIBUTE_OFFLINE | DISK_ATTRIBUTE_READ_ONLY;
    // SAFETY: the input buffer is valid for its size. Failure is fine: the
    // following write test reports what matters.
    let _ = unsafe {
        ioctl(
            file,
            IOCTL_DISK_SET_DISK_ATTRIBUTES,
            &a as *const _ as *const c_void,
            size_of::<SetDiskAttributes>() as u32,
            null_mut(),
            0,
        )
    };
}

/// Takes exclusive control of a disk for writing.
///
/// The caller, `ops::open_destructive`, has already applied the safety guards.
pub fn open_exclusive(info: &DeviceInfo) -> Result<Box<dyn BlockDevice>> {
    let file = RawDisk::open(info, true)?;
    clear_disk_attributes(&file);
    if ioctl_none(&file, IOCTL_DISK_IS_WRITABLE).is_err_and(|e| e.raw_os_error() == Some(19)) {
        return Err(Error::WriteProtected(format!(
            "{}: check for a lock switch; otherwise the controller has locked the flash permanently",
            info.path
        )));
    }

    let mut locked_volumes = vec![];
    for v in &info.volumes {
        locked_volumes.push(lock_volume(v)?);
    }

    // Windows blocks raw writes to sectors in a volume it knows. Removing the
    // layout makes the disk raw; the operation writes a new one if it wants one.
    ioctl_none(&file, IOCTL_DISK_DELETE_DRIVE_LAYOUT).ok();
    ioctl_none(&file, IOCTL_DISK_UPDATE_PROPERTIES).ok();

    let size = RawDisk::query_size(&file)?;
    Ok(Box::new(RawDisk { file, size, sector: info.logical_sector, locked_volumes, finished: false }))
}

// ---- SMART over SAT ---------------------------------------------------------

#[repr(C)]
struct ScsiPassThroughDirect {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    timeout_value: u32,
    data_buffer: *mut c_void,
    sense_info_offset: u32,
    cdb: [u8; 16],
}

#[repr(C)]
struct PassThroughWithSense {
    sptd: ScsiPassThroughDirect,
    sense: [u8; 32],
}

/// Requests SMART data through SCSI/ATA translation.
pub fn read_smart(info: &DeviceInfo) -> Result<Vec<u8>> {
    let file = RawDisk::open(info, true).or_else(|_| RawDisk::open(info, false))?;
    let mut data = AlignedBuf::new(512);
    // SAFETY: an all-zero bit pattern is valid; the pointer is set right after.
    let mut p: PassThroughWithSense = unsafe { zeroed() };
    p.sptd.length = size_of::<ScsiPassThroughDirect>() as u16;
    p.sptd.cdb_length = 16;
    p.sptd.sense_info_length = 32;
    p.sptd.data_in = 1; // SCSI_IOCTL_DATA_IN
    p.sptd.data_transfer_length = 512;
    p.sptd.timeout_value = 5;
    p.sptd.data_buffer = data.as_mut_ptr() as *mut c_void;
    p.sptd.sense_info_offset = size_of::<ScsiPassThroughDirect>() as u32;
    p.sptd.cdb = super::SAT_SMART_READ_CDB;
    let len = size_of::<PassThroughWithSense>() as u32;
    let ptr = &mut p as *mut _ as *mut c_void;
    // SAFETY: `p` and `data` outlive the synchronous call.
    unsafe { ioctl(&file, IOCTL_SCSI_PASS_THROUGH_DIRECT, ptr, len, ptr, len) }
        .map_err(|e| Error::Unsupported(format!("SMART request failed: {e}")))?;
    if p.sptd.scsi_status != 0 {
        return Err(Error::Unsupported("the device does not support SMART over USB".to_string()));
    }
    Ok(data.to_vec())
}
