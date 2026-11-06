// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux backend: sysfs for discovery, block device nodes for access.

use crate::device::positioned::{read_exact_at, write_all_at};
use crate::device::{AlignedBuf, BlockDevice, BusType, DeviceInfo};
use crate::error::{Error, Result};
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;
const BLKRRPART: libc::c_ulong = 0x125F;
const SG_IO: libc::c_ulong = 0x2285;

fn read_trim(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn read_u64(path: impl AsRef<Path>) -> u64 {
    read_trim(path).parse().unwrap_or(0)
}

/// Unescapes octal sequences such as `\040` in /proc/self/mountinfo.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (source device, mount point) pairs of all mounted filesystems.
fn mounts() -> Vec<(String, String)> {
    let text = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    text.lines().filter_map(parse_mountinfo_line).collect()
}

fn parse_mountinfo_line(line: &str) -> Option<(String, String)> {
    let (left, right) = line.split_once(" - ")?;
    let mount_point = left.split(' ').nth(4)?;
    let source = right.split(' ').nth(1)?;
    Some((unescape_mount(source), unescape_mount(mount_point)))
}

/// Resolves a block device name to the top-level disks beneath it, following
/// partitions and device-mapper/md stacks (LVM, LUKS, RAID).
fn base_disks(name: &str, out: &mut HashSet<String>, depth: u32) {
    if depth > 16 {
        return;
    }
    let class = PathBuf::from("/sys/class/block").join(name);
    if class.join("partition").exists() {
        if let Ok(real) = fs::canonicalize(&class) {
            if let Some(parent) = real.parent().and_then(|p| p.file_name()) {
                base_disks(&parent.to_string_lossy(), out, depth + 1);
            }
        }
        return;
    }
    let slaves: Vec<String> = fs::read_dir(class.join("slaves"))
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    if slaves.is_empty() {
        out.insert(name.to_string());
    } else {
        for s in slaves {
            base_disks(&s, out, depth + 1);
        }
    }
}

fn device_basename(source: &str) -> Option<String> {
    let real = fs::canonicalize(source).ok()?;
    if !real.starts_with("/dev") {
        return None;
    }
    real.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Disks that must never be written: holding /, /boot, /usr, swap or this program.
fn protected_disks(mount_list: &[(String, String)]) -> HashSet<String> {
    const SYSTEM_MOUNTS: [&str; 8] = ["/", "/boot", "/boot/efi", "/efi", "/usr", "/var", "/etc", "/home"];
    let mut out = HashSet::new();
    let exe = std::env::current_exe().ok();
    for (source, mp) in mount_list {
        let holds_exe = exe.as_ref().is_some_and(|e| mp != "/" && e.starts_with(mp));
        if SYSTEM_MOUNTS.contains(&mp.as_str()) || holds_exe {
            if let Some(n) = device_basename(source) {
                base_disks(&n, &mut out, 0);
            }
        }
    }
    let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
    for line in swaps.lines().skip(1) {
        if let Some(n) = line.split_whitespace().next().and_then(device_basename) {
            base_disks(&n, &mut out, 0);
        }
    }
    out
}

fn bus_of(name: &str, sys: &Path) -> BusType {
    let dev_path = fs::canonicalize(sys.join("device")).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if dev_path.contains("/usb") {
        BusType::Usb
    } else if name.starts_with("mmcblk") {
        // eMMC soldered to the board is also mmcblk; it carries the OS in nearly
        // all such machines, which the protected disk check catches.
        if read_trim(sys.join("device/type")) == "SD" { BusType::Sd } else { BusType::Mmc }
    } else if name.starts_with("nvme") {
        BusType::Nvme
    } else if dev_path.contains("/ata") {
        BusType::Sata
    } else if name.starts_with("vd") || name.starts_with("xvd") {
        BusType::Virtual
    } else if name.starts_with("sd") {
        BusType::Scsi
    } else {
        BusType::Other
    }
}

/// The USB serial number lives on an ancestor of the block device in sysfs.
fn usb_serial(sys: &Path) -> String {
    let Ok(mut dir) = fs::canonicalize(sys.join("device")) else { return String::new() };
    for _ in 0..12 {
        let s = read_trim(dir.join("serial"));
        if !s.is_empty() {
            return s;
        }
        if !dir.pop() {
            break;
        }
    }
    String::new()
}

/// Lists all disks.
pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    let mount_list = mounts();
    let protected = protected_disks(&mount_list);
    if protected.is_empty() && mount_list.iter().any(|(_, m)| m == "/") {
        // Root on something without a block device (overlay, NFS, container):
        // fine in itself, but say nothing is protected only if it is a fact.
        if !Path::new("/proc/self/mountinfo").exists() {
            return Err(Error::Refused("cannot determine the system disk".to_string()));
        }
    }
    let mut out = vec![];
    let entries = fs::read_dir("/sys/block").map_err(Error::Io)?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ["loop", "ram", "zram", "dm-", "md", "sr", "nbd", "fd"].iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let sys = e.path();
        let size = read_u64(sys.join("size")) * 512;
        let logical = read_u64(sys.join("queue/logical_block_size")).max(512) as u32;
        let physical = read_u64(sys.join("queue/physical_block_size")).max(logical as u64) as u32;
        let bus = bus_of(&name, &sys);

        // Partitions of this disk are subdirectories with a "partition" file.
        let mut parts: Vec<String> = fs::read_dir(&sys)
            .map(|d| {
                d.flatten()
                    .filter(|p| p.path().join("partition").exists())
                    .map(|p| p.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        parts.push(name.clone());
        let volumes: Vec<String> = parts.iter().map(|p| format!("/dev/{p}")).collect();
        let mount_points = mount_list
            .iter()
            .filter(|(src, _)| device_basename(src).is_some_and(|n| parts.contains(&n)))
            .map(|(_, m)| m.clone())
            .collect();

        out.push(DeviceInfo {
            path: format!("/dev/{name}"),
            vendor: read_trim(sys.join("device/vendor")),
            model: read_trim(sys.join("device/model")),
            serial: if bus == BusType::Usb { usb_serial(&sys) } else { read_trim(sys.join("device/serial")) },
            bus,
            removable_media: read_trim(sys.join("removable")) == "1",
            media_present: size > 0,
            size_bytes: size,
            logical_sector: logical,
            physical_sector: physical,
            read_only: read_trim(sys.join("ro")) == "1",
            is_system: protected.contains(&name),
            mount_points,
            volumes,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

struct RawDisk {
    file: File,
    size: u64,
    sector: u32,
    exclusive: bool,
}

fn device_size(file: &File) -> Result<u64> {
    let mut size = 0u64;
    // SAFETY: BLKGETSIZE64 writes one u64 to the given pointer.
    let r = unsafe { libc::ioctl(file.as_raw_fd(), BLKGETSIZE64 as _, &mut size as *mut u64) };
    if r != 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    Ok(size)
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
        self.file.sync_all()
    }
    fn finish(&mut self) -> Result<()> {
        if !self.exclusive {
            return Ok(());
        }
        self.file.sync_all()?;
        // Ask the kernel to re-read the partition table. Our own O_EXCL handle does
        // not get in the way; udev may hold it briefly, hence the retries.
        for _ in 0..5 {
            // SAFETY: BLKRRPART takes no argument.
            if unsafe { libc::ioctl(self.file.as_raw_fd(), BLKRRPART as _) } == 0 {
                break;
            }
            sleep(Duration::from_millis(300));
        }
        Ok(())
    }
}

/// Opens a disk for reading. Usually needs root or membership in group `disk`.
pub fn open_read(info: &DeviceInfo) -> Result<Box<dyn BlockDevice>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(&info.path)
        .map_err(|e| Error::from_open(e, &info.path))?;
    let size = device_size(&file)?;
    Ok(Box::new(RawDisk { file, size, sector: info.logical_sector, exclusive: false }))
}

fn unmount_all(info: &DeviceInfo) -> Result<()> {
    for mp in &info.mount_points {
        let c = CString::new(mp.as_str()).map_err(|_| Error::Invalid(format!("bad mount point {mp}")))?;
        // SAFETY: `c` is a valid NUL terminated string.
        let mut r = unsafe { libc::umount2(c.as_ptr(), 0) };
        if r != 0 {
            // Busy: detach lazily. The user has confirmed the data is to be destroyed.
            // SAFETY: as above.
            r = unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) };
        }
        if r != 0 {
            let e = io::Error::last_os_error();
            return Err(Error::Refused(format!("cannot unmount {mp}: {e}")));
        }
    }
    Ok(())
}

/// Takes exclusive control of a disk for writing.
///
/// The caller, `ops::open_destructive`, has already applied the safety guards.
pub fn open_exclusive(info: &DeviceInfo) -> Result<Box<dyn BlockDevice>> {
    if info.read_only {
        return Err(Error::WriteProtected(format!(
            "{}: check for a lock switch; otherwise the controller has locked the flash permanently",
            info.path
        )));
    }
    unmount_all(info)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_DIRECT | libc::O_SYNC | libc::O_EXCL)
        .open(&info.path)
        .map_err(|e| Error::from_open(e, &info.path))?;
    let size = device_size(&file)?;
    Ok(Box::new(RawDisk { file, size, sector: info.logical_sector, exclusive: true }))
}

#[repr(C)]
struct SgIoHdr {
    interface_id: i32,
    dxfer_direction: i32,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut libc::c_void,
    cmdp: *const u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut libc::c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

/// Requests SMART data through SCSI/ATA translation.
pub fn read_smart(info: &DeviceInfo) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&info.path)
        .map_err(|e| Error::from_open(e, &info.path))?;
    let mut data = AlignedBuf::new(512);
    let mut sense = [0u8; 32];
    let cdb = super::SAT_SMART_READ_CDB;
    // SAFETY: an all-zero bit pattern is valid for this plain data struct.
    let mut h: SgIoHdr = unsafe { std::mem::zeroed() };
    h.interface_id = 'S' as i32;
    h.dxfer_direction = -3; // SG_DXFER_FROM_DEV
    h.cmd_len = cdb.len() as u8;
    h.mx_sb_len = sense.len() as u8;
    h.dxfer_len = 512;
    h.dxferp = data.as_mut_ptr() as *mut libc::c_void;
    h.cmdp = cdb.as_ptr();
    h.sbp = sense.as_mut_ptr();
    h.timeout = 5000;
    // SAFETY: all pointers in `h` outlive the synchronous ioctl.
    let r = unsafe { libc::ioctl(file.as_raw_fd(), SG_IO as _, &mut h as *mut SgIoHdr) };
    if r != 0 {
        return Err(Error::Unsupported(format!("SMART request failed: {}", io::Error::last_os_error())));
    }
    if h.status != 0 || h.host_status != 0 {
        return Err(Error::Unsupported("the device does not support SMART over USB".to_string()));
    }
    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mountinfo() {
        let l = "36 35 98:0 /mnt1 /mnt/my\\040stick rw,noatime master:1 - vfat /dev/sdb1 rw";
        assert_eq!(parse_mountinfo_line(l), Some(("/dev/sdb1".into(), "/mnt/my stick".into())));
        let no_tag = "36 35 98:0 / / rw - ext4 /dev/sda2 rw";
        assert_eq!(parse_mountinfo_line(no_tag), Some(("/dev/sda2".into(), "/".into())));
    }
}
