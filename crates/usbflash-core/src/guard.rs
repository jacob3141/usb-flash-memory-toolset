// SPDX-License-Identifier: GPL-3.0-or-later
//! Safety policy: decides which devices may be written to.
//!
//! This is the most important file in the project. A bug here can destroy
//! someone's data, so the policy is a small pure function with exhaustive tests.
//! See `docs/SAFETY.md`.

use crate::device::DeviceInfo;
use crate::error::{Error, Result};

/// Size above which a device is unusually large for a flash stick and
/// front ends must ask for a typed confirmation.
pub const LARGE_DEVICE_BYTES: u64 = 512 * 1024 * 1024 * 1024;

/// Explicit user decisions that relax the default policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct Overrides {
    /// Allow USB/SD devices that report fixed media. Many USB sticks do, but so
    /// do external USB hard disks and SSDs.
    pub allow_fixed: bool,
}

/// How strongly a front end must confirm a destructive operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// A single explicit confirmation showing the device identity.
    Simple,
    /// The device might be an external disk: the user must type a word.
    Typed,
}

/// Returns `Ok` only if the device may be overwritten.
///
/// There is deliberately no override for the system disk or for internal buses.
pub fn check_destructive(d: &DeviceInfo, o: Overrides) -> Result<()> {
    if d.is_system {
        return Err(Error::Refused(format!("{} holds the running operating system or this program", d.path)));
    }
    if !d.bus.is_flash_bus() {
        return Err(Error::Refused(format!(
            "{} is connected via {}; only USB, SD and MMC devices can be modified",
            d.path,
            d.bus.label()
        )));
    }
    if !d.media_present || d.size_bytes == 0 {
        return Err(Error::Refused(format!(
            "{} reports no media or zero capacity; the controller is probably dead",
            d.path
        )));
    }
    if !d.removable_media && !o.allow_fixed {
        return Err(Error::Refused(format!(
            "{} reports fixed media and could be an external disk rather than a stick; \
             confirm explicitly to continue (CLI: --allow-fixed)",
            d.path
        )));
    }
    Ok(())
}

/// The confirmation a front end must obtain before a destructive operation.
pub fn required_confirmation(d: &DeviceInfo) -> Confirmation {
    if !d.removable_media || d.size_bytes >= LARGE_DEVICE_BYTES { Confirmation::Typed } else { Confirmation::Simple }
}

/// Verifies that `current` (fresh from the OS) is still the device the user chose.
pub fn check_identity(selected: &DeviceInfo, current: &[DeviceInfo]) -> Result<DeviceInfo> {
    let found = current
        .iter()
        .find(|d| d.path == selected.path)
        .ok_or_else(|| Error::DeviceChanged(format!("{} is no longer connected", selected.path)))?;
    if !selected.same_identity(found) {
        return Err(Error::DeviceChanged(format!(
            "{} is now a different device ({}, serial '{}'); select it again",
            selected.path,
            found.display_name(),
            found.serial
        )));
    }
    Ok(found.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::BusType;

    fn stick() -> DeviceInfo {
        DeviceInfo {
            path: "/dev/sdx".into(),
            vendor: "ACME".into(),
            model: "Stick".into(),
            serial: "123".into(),
            bus: BusType::Usb,
            removable_media: true,
            media_present: true,
            size_bytes: 16_000_000_000,
            logical_sector: 512,
            physical_sector: 512,
            read_only: false,
            is_system: false,
            mount_points: vec![],
            volumes: vec![],
        }
    }

    const ALLOW: Overrides = Overrides { allow_fixed: true };

    #[test]
    fn normal_stick_is_allowed() {
        assert!(check_destructive(&stick(), Overrides::default()).is_ok());
    }

    #[test]
    fn system_disk_is_refused_even_with_overrides() {
        let d = DeviceInfo { is_system: true, ..stick() };
        assert!(matches!(check_destructive(&d, ALLOW), Err(Error::Refused(_))));
    }

    #[test]
    fn internal_buses_are_refused_even_with_overrides() {
        for bus in [BusType::Sata, BusType::Nvme, BusType::Scsi, BusType::Virtual, BusType::Other] {
            let d = DeviceInfo { bus, ..stick() };
            assert!(matches!(check_destructive(&d, ALLOW), Err(Error::Refused(_))), "{bus:?}");
        }
    }

    #[test]
    fn sd_and_mmc_are_allowed() {
        for bus in [BusType::Sd, BusType::Mmc] {
            assert!(check_destructive(&DeviceInfo { bus, ..stick() }, Overrides::default()).is_ok());
        }
    }

    #[test]
    fn fixed_media_needs_the_override() {
        let d = DeviceInfo { removable_media: false, ..stick() };
        assert!(check_destructive(&d, Overrides::default()).is_err());
        assert!(check_destructive(&d, ALLOW).is_ok());
    }

    #[test]
    fn missing_media_is_refused() {
        let d = DeviceInfo { media_present: false, ..stick() };
        assert!(check_destructive(&d, ALLOW).is_err());
        let d = DeviceInfo { size_bytes: 0, ..stick() };
        assert!(check_destructive(&d, ALLOW).is_err());
    }

    #[test]
    fn risky_devices_need_typed_confirmation() {
        assert_eq!(required_confirmation(&stick()), Confirmation::Simple);
        let fixed = DeviceInfo { removable_media: false, ..stick() };
        assert_eq!(required_confirmation(&fixed), Confirmation::Typed);
        let large = DeviceInfo { size_bytes: LARGE_DEVICE_BYTES, ..stick() };
        assert_eq!(required_confirmation(&large), Confirmation::Typed);
    }

    #[test]
    fn identity_check_catches_replugged_devices() {
        let selected = stick();
        assert!(check_identity(&selected, &[stick()]).is_ok());
        assert!(matches!(check_identity(&selected, &[]), Err(Error::DeviceChanged(_))));
        let other = DeviceInfo { serial: "999".into(), ..stick() };
        assert!(matches!(check_identity(&selected, &[other]), Err(Error::DeviceChanged(_))));
    }
}
