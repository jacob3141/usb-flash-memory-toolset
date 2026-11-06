// SPDX-License-Identifier: GPL-3.0-or-later
//! Entry points for front ends. This is the safety choke point: every destructive
//! operation on a real device passes [`open_destructive`], which re-reads the
//! device list from the OS and applies the policy from [`crate::guard`].

use crate::analyze::{self, Analysis};
use crate::capacity::{self, CapacityReport};
use crate::device::{BlockDevice, DeviceInfo, FileDevice};
use crate::error::{Error, Result};
use crate::guard::{self, Overrides};
use crate::health::{self, HealthReport};
use crate::image::{self, BackupReport, RestoreReport};
use crate::platform;
use crate::progress::Ctx;
use crate::reset::{self, ResetOptions, ResetReport};
use crate::scan::{self, ScanMode, ScanReport};
use crate::smart::SmartOutcome;
use crate::wipe::{self, WipeMode, WipeReport};
use std::path::{Path, PathBuf};

/// What an operation works on.
#[derive(Debug, Clone)]
pub enum Target {
    /// A real device, as selected by the user from [`list_devices`].
    Device(DeviceInfo),
    /// An image file. Bypasses device guards as it cannot harm a disk.
    Image { path: PathBuf, sector_size: u32 },
}

impl Target {
    pub fn device_info(&self) -> Option<DeviceInfo> {
        match self {
            Target::Device(d) => Some(d.clone()),
            Target::Image { .. } => None,
        }
    }
}

/// Lists all disks known to the OS, including those we refuse to touch.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    platform::enumerate()
}

fn open_read(target: &Target) -> Result<Box<dyn BlockDevice>> {
    match target {
        Target::Device(d) => platform::open_read(d),
        Target::Image { path, sector_size } => Ok(Box::new(FileDevice::open(path, *sector_size, false)?)),
    }
}

/// Opens a target for writing, after checking identity and policy.
fn open_destructive(target: &Target, overrides: Overrides) -> Result<Box<dyn BlockDevice>> {
    match target {
        Target::Device(selected) => {
            let current = platform::enumerate()?;
            let fresh = guard::check_identity(selected, &current)?;
            guard::check_destructive(&fresh, overrides)?;
            platform::open_exclusive(&fresh)
        }
        Target::Image { path, sector_size } => Ok(Box::new(FileDevice::open(path, *sector_size, true)?)),
    }
}

/// Runs `f`, always releasing the device. An error from `f` takes precedence.
fn finishing<T>(mut dev: Box<dyn BlockDevice>, f: impl FnOnce(&mut dyn BlockDevice) -> Result<T>) -> Result<T> {
    let result = f(dev.as_mut());
    let finished = dev.finish();
    let value = result?;
    finished?;
    Ok(value)
}

pub fn analyze(target: &Target, ctx: &mut Ctx) -> Result<Analysis> {
    let mut dev = open_read(target)?;
    analyze::analyze(dev.as_mut(), ctx)
}

pub fn scan(target: &Target, mode: ScanMode, ctx: &mut Ctx) -> Result<ScanReport> {
    let mut dev = open_read(target)?;
    scan::scan(dev.as_mut(), mode, ctx)
}

/// Asks the device for SMART data. Most sticks do not have any.
pub fn smart(target: &Target) -> SmartOutcome {
    let Target::Device(d) = target else {
        return SmartOutcome::Unavailable { reason: "image files have no SMART data".to_string() };
    };
    let reason = match platform::read_smart(d) {
        Ok(raw) => match crate::smart::parse(&raw) {
            Some(attributes) => return SmartOutcome::Available { attributes },
            None => "the device returned no valid SMART data".to_string(),
        },
        Err(e) => e.to_string(),
    };
    SmartOutcome::Unavailable { reason }
}

/// Complete health check: read test, SMART and verdict.
pub fn health(device: &DeviceInfo, mode: ScanMode, ctx: &mut Ctx) -> Result<HealthReport> {
    if !device.media_present || device.size_bytes == 0 {
        return Ok(health::assess(device, None, None));
    }
    let target = Target::Device(device.clone());
    let scan = scan(&target, mode, ctx)?;
    let smart = smart(&target);
    Ok(health::assess(device, Some(scan), Some(smart)))
}

pub fn capacity_test(target: &Target, mode: ScanMode, overrides: Overrides, ctx: &mut Ctx) -> Result<CapacityReport> {
    finishing(open_destructive(target, overrides)?, |d| capacity::test(d, mode, ctx))
}

pub fn wipe(target: &Target, mode: WipeMode, verify: bool, overrides: Overrides, ctx: &mut Ctx) -> Result<WipeReport> {
    finishing(open_destructive(target, overrides)?, |d| wipe::wipe(d, mode, verify, ctx))
}

pub fn factory_reset(target: &Target, opts: &ResetOptions, overrides: Overrides, ctx: &mut Ctx) -> Result<ResetReport> {
    finishing(open_destructive(target, overrides)?, |d| reset::factory_reset(d, opts, ctx))
}

/// Saves the device to an image file. The file must not be on the device itself.
pub fn backup(target: &Target, dest: &Path, ctx: &mut Ctx) -> Result<BackupReport> {
    if let Target::Device(d) = target
        && path_is_on_device(dest, d)
    {
        return Err(Error::Invalid(format!("{} is on the device being backed up", dest.display())));
    }
    let mut dev = open_read(target)?;
    image::backup(dev.as_mut(), dest, ctx)
}

pub fn restore(target: &Target, source: &Path, overrides: Overrides, ctx: &mut Ctx) -> Result<RestoreReport> {
    if let Target::Device(d) = target
        && path_is_on_device(source, d)
    {
        return Err(Error::Invalid(format!("{} is on the device being overwritten", source.display())));
    }
    finishing(open_destructive(target, overrides)?, |d| image::restore(d, source, ctx))
}

fn path_is_on_device(path: &Path, d: &DeviceInfo) -> bool {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let Ok(abs) = parent.canonicalize() else { return false };
    let abs = abs.to_string_lossy().replace("\\\\?\\", "").to_lowercase();
    d.mount_points.iter().any(|m| {
        let m = m.to_lowercase();
        // "/" never occurs here: a device holding the root filesystem is a system disk.
        let m_trim = m.trim_end_matches(['\\', '/']);
        !m_trim.is_empty()
            && (abs == m_trim || abs.starts_with(&format!("{m_trim}\\")) || abs.starts_with(&format!("{m_trim}/")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::BusType;
    use crate::reset::EraseChoice;

    fn temp(name: &str, size: u64) -> PathBuf {
        let p = std::env::temp_dir().join(format!("usbflash-ops-{name}-{}", std::process::id()));
        FileDevice::create(&p, size, 512).unwrap();
        p
    }

    #[test]
    fn image_targets_work_end_to_end() {
        let p = temp("e2e", 32 << 20);
        let t = Target::Image { path: p.clone(), sector_size: 512 };
        let opts = ResetOptions { erase: EraseChoice::Quick, label: "TEST".into(), ..Default::default() };
        let r = factory_reset(&t, &opts, Overrides::default(), &mut Ctx::silent()).unwrap();
        assert!(r.success());
        let a = analyze(&t, &mut Ctx::silent()).unwrap();
        assert_eq!(a.partitions.len(), 1);
        let fs = a.partitions[0].filesystem.as_ref().unwrap();
        assert_eq!((fs.name.as_str(), fs.label.as_str()), ("FAT16", "TEST"));
        std::fs::remove_file(p).ok();
    }

    fn fake_device(mount: &str) -> DeviceInfo {
        DeviceInfo {
            path: "nonexistent-test-device".into(),
            vendor: String::new(),
            model: String::new(),
            serial: String::new(),
            bus: BusType::Usb,
            removable_media: true,
            media_present: true,
            size_bytes: 1 << 30,
            logical_sector: 512,
            physical_sector: 512,
            read_only: false,
            is_system: false,
            mount_points: vec![mount.to_string()],
            volumes: vec![],
        }
    }

    #[test]
    fn device_not_in_the_os_list_is_never_opened_for_writing() {
        let t = Target::Device(fake_device("/nowhere"));
        let r = wipe(&t, WipeMode::Zero, false, Overrides { allow_fixed: true }, &mut Ctx::silent());
        assert!(matches!(r, Err(Error::DeviceChanged(_)) | Err(Error::Unsupported(_))), "{r:?}");
    }

    #[test]
    fn destination_on_the_device_is_detected() {
        // Canonical, as mount points are: temp paths can contain 8.3 short names or symlinks.
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let mount = dir.to_string_lossy().replace("\\\\?\\", "");
        let d = fake_device(&mount);
        assert!(path_is_on_device(&dir.join("x.img"), &d));
        let other = fake_device("/definitely/not/mounted");
        assert!(!path_is_on_device(&dir.join("x.img"), &other));
    }
}
