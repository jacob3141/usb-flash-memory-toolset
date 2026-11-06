// SPDX-License-Identifier: GPL-3.0-or-later
//! Combines all available signals into a single health verdict.
//!
//! Flash sticks expose almost no self-diagnostics, so most of the verdict comes
//! from observed behavior: does it respond, can it be read, and how fast.

use crate::device::DeviceInfo;
use crate::scan::ScanReport;
use crate::smart::{SmartAttribute, SmartOutcome};
use serde::Serialize;

/// Below this average read speed, a stick is degraded even by USB 1.1 standards.
const VERY_SLOW_READ: f64 = 2.0 * 1024.0 * 1024.0;
/// Sticks reporting less than this often have a controller in safe mode.
const SUSPICIOUSLY_SMALL: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Good,
    Warning,
    Failing,
    Dead,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Good => "Good",
            Verdict::Warning => "Warning",
            Verdict::Failing => "Failing",
            Verdict::Dead => "Dead",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub verdict: Verdict,
    /// Why the verdict is what it is.
    pub reasons: Vec<String>,
    /// Context that does not influence the verdict.
    pub notes: Vec<String>,
    pub scan: Option<ScanReport>,
    pub smart: Option<SmartOutcome>,
}

fn smart_signals(attrs: &[SmartAttribute], verdict: &mut Verdict, reasons: &mut Vec<String>) {
    for a in attrs {
        match a.id {
            197 | 198 if a.raw > 0 => {
                *verdict = (*verdict).max(Verdict::Failing);
                reasons.push(format!("SMART: {} = {}", a.name, a.raw));
            }
            5 | 187 | 196 if a.raw > 0 => {
                *verdict = (*verdict).max(Verdict::Warning);
                reasons.push(format!("SMART: {} = {}", a.name, a.raw));
            }
            231 | 233 | 202 | 177 if a.current <= 10 && a.current > 0 => {
                *verdict = (*verdict).max(Verdict::Warning);
                reasons.push(format!("SMART: {} is nearly exhausted ({})", a.name, a.current));
            }
            _ => {}
        }
    }
}

/// Derives the verdict. A pure function, so every rule has a test.
pub fn assess(device: &DeviceInfo, scan: Option<ScanReport>, smart: Option<SmartOutcome>) -> HealthReport {
    let mut verdict = Verdict::Good;
    let mut reasons = vec![];
    let mut notes = vec![];

    if !device.media_present || device.size_bytes == 0 {
        reasons.push(
            "The device reports no media or zero capacity. The controller is alive, but the flash is \
             not accessible; this is the typical failure of a dead stick."
                .to_string(),
        );
        return HealthReport { verdict: Verdict::Dead, reasons, notes, scan, smart };
    }

    if device.read_only {
        verdict = verdict.max(Verdict::Warning);
        reasons.push(
            "The device is write protected. Unless it has a lock switch, this usually means the \
             controller locked it after running out of good flash blocks. Such a stick cannot be \
             wiped anymore; destroy it physically if it holds sensitive data."
                .to_string(),
        );
    }

    if device.size_bytes < SUSPICIOUSLY_SMALL {
        notes.push(
            "Very small capacity: either a very old stick, or a controller in safe mode that \
             lost access to its flash."
                .to_string(),
        );
    }

    if let Some(s) = &scan {
        if s.aborted {
            verdict = verdict.max(if s.bytes_read == 0 { Verdict::Dead } else { Verdict::Failing });
            reasons.push("The device stopped responding during the read test.".to_string());
        } else if !s.unreadable.is_empty() {
            verdict = verdict.max(Verdict::Failing);
            reasons.push(format!(
                "{} could not be read, in {} area(s).",
                crate::units::bytes(s.unreadable.total()),
                s.unreadable.ranges.len()
            ));
        }
        if s.bytes_read > 0 && s.avg_bytes_per_sec < VERY_SLOW_READ {
            verdict = verdict.max(Verdict::Warning);
            reasons.push(format!(
                "Very slow reading at {}: heavy internal error correction, or a USB 1.1 connection.",
                crate::units::speed(s.avg_bytes_per_sec)
            ));
        }
    }

    match &smart {
        Some(SmartOutcome::Available { attributes }) => smart_signals(attributes, &mut verdict, &mut reasons),
        Some(SmartOutcome::Unavailable { .. }) | None => notes.push(
            "No wear data available: most USB sticks do not report SMART, so this verdict is based \
             on observed behavior only."
                .to_string(),
        ),
    }

    if reasons.is_empty() {
        reasons.push("No problems found.".to_string());
    }
    notes.push("Reading alone cannot prove the flash is writable; the capacity test does.".to_string());
    HealthReport { verdict, reasons, notes, scan, smart }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{BusType, RangeList};
    use crate::scan::ScanMode;

    fn stick() -> DeviceInfo {
        DeviceInfo {
            path: "x".into(),
            vendor: String::new(),
            model: String::new(),
            serial: String::new(),
            bus: BusType::Usb,
            removable_media: true,
            media_present: true,
            size_bytes: 8 << 30,
            logical_sector: 512,
            physical_sector: 512,
            read_only: false,
            is_system: false,
            mount_points: vec![],
            volumes: vec![],
        }
    }

    fn good_scan() -> ScanReport {
        ScanReport {
            mode: ScanMode::Full,
            bytes_read: 8 << 30,
            unreadable: RangeList::default(),
            avg_bytes_per_sec: 30e6,
            min_bytes_per_sec: 20e6,
            max_bytes_per_sec: 40e6,
            speed_profile: vec![],
            duration_secs: 100.0,
            aborted: false,
        }
    }

    #[test]
    fn healthy_stick_is_good() {
        assert_eq!(assess(&stick(), Some(good_scan()), None).verdict, Verdict::Good);
    }

    #[test]
    fn no_media_is_dead() {
        let d = DeviceInfo { media_present: false, ..stick() };
        assert_eq!(assess(&d, None, None).verdict, Verdict::Dead);
        let d = DeviceInfo { size_bytes: 0, ..stick() };
        assert_eq!(assess(&d, None, None).verdict, Verdict::Dead);
    }

    #[test]
    fn read_errors_mean_failing() {
        let mut s = good_scan();
        s.unreadable.push(0, 4096);
        assert_eq!(assess(&stick(), Some(s), None).verdict, Verdict::Failing);
    }

    #[test]
    fn unresponsive_device_is_failing_or_dead() {
        let mut s = good_scan();
        s.aborted = true;
        assert_eq!(assess(&stick(), Some(s.clone()), None).verdict, Verdict::Failing);
        s.bytes_read = 0;
        assert_eq!(assess(&stick(), Some(s), None).verdict, Verdict::Dead);
    }

    #[test]
    fn write_protection_is_a_warning() {
        let d = DeviceInfo { read_only: true, ..stick() };
        assert_eq!(assess(&d, Some(good_scan()), None).verdict, Verdict::Warning);
    }

    #[test]
    fn slow_reads_are_a_warning() {
        let mut s = good_scan();
        s.avg_bytes_per_sec = 1e6;
        assert_eq!(assess(&stick(), Some(s), None).verdict, Verdict::Warning);
    }

    #[test]
    fn smart_signals_count() {
        let attr = |id, current, raw| SmartAttribute { id, name: "x", current, worst: current, raw };
        let check = |a: Vec<SmartAttribute>| {
            assess(&stick(), Some(good_scan()), Some(SmartOutcome::Available { attributes: a })).verdict
        };
        assert_eq!(check(vec![attr(194, 60, 40)]), Verdict::Good);
        assert_eq!(check(vec![attr(5, 100, 2)]), Verdict::Warning);
        assert_eq!(check(vec![attr(197, 100, 1)]), Verdict::Failing);
        assert_eq!(check(vec![attr(231, 5, 5)]), Verdict::Warning);
    }

    #[test]
    fn worst_signal_wins() {
        let mut s = good_scan();
        s.unreadable.push(0, 4096);
        let d = DeviceInfo { read_only: true, ..stick() };
        assert_eq!(assess(&d, Some(s), None).verdict, Verdict::Failing);
    }
}
