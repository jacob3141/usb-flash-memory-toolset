// SPDX-License-Identifier: GPL-3.0-or-later
//! Turns operation results into the short texts shown in the interface.

use usbflash_core::capacity::CapacityReport;
use usbflash_core::health::{HealthReport, Verdict};
use usbflash_core::image::{BackupReport, RestoreReport};
use usbflash_core::reset::ResetReport;
use usbflash_core::units::{bytes, duration, speed};
use usbflash_core::wipe::{WipeReport, WipeStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Warning,
    Bad,
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub tone: Tone,
    pub title: String,
    pub lines: Vec<String>,
}

pub fn health(h: &HealthReport) -> Summary {
    let tone = match h.verdict {
        Verdict::Good => Tone::Good,
        Verdict::Warning => Tone::Warning,
        Verdict::Failing | Verdict::Dead => Tone::Bad,
    };
    Summary { tone, title: format!("Health: {}", h.verdict.label()), lines: h.reasons.clone() }
}

pub fn capacity(r: &CapacityReport) -> Summary {
    let mut lines = vec![format!(
        "Write {}, read {}, took {}",
        speed(r.write_bytes_per_sec),
        speed(r.read_bytes_per_sec),
        duration(r.duration_secs)
    )];
    if r.passed() {
        lines.insert(0, format!("All tested data of {} was stored correctly.", bytes(r.claimed_bytes)));
        if r.mode == usbflash_core::scan::ScanMode::Quick {
            lines.push("Quick test: only probes were checked. The full test is authoritative.".to_string());
        }
        return Summary { tone: Tone::Good, title: "Capacity test passed".to_string(), lines };
    }
    if let Some(real) = r.estimated_real_bytes {
        lines.insert(0, format!("Estimated real capacity: {}", bytes(real)));
    }
    if r.aborted {
        lines.insert(0, "The device stopped responding.".to_string());
    } else {
        lines.insert(0, format!("First failure at {}.", bytes(r.verified_bytes)));
    }
    let title = if r.looks_fake {
        format!("Fake capacity suspected: claims {}", bytes(r.claimed_bytes))
    } else {
        "Capacity test failed: data is not stored reliably".to_string()
    };
    Summary { tone: Tone::Bad, title, lines }
}

pub fn wipe(r: &WipeReport) -> Summary {
    let mut lines = vec![format!("{}: {} in {}", r.mode.label(), bytes(r.bytes), duration(r.duration_secs))];
    let (tone, title) = match r.status {
        WipeStatus::Verified => (Tone::Good, "Wiped and verified"),
        WipeStatus::WrittenUnverified => (Tone::Warning, "Wiped, not verified"),
        WipeStatus::Incomplete => (Tone::Bad, "Wipe incomplete: old data may remain"),
    };
    let failed = r.write_failures.total() + r.verify_failures.total();
    if failed > 0 {
        lines.push(format!("{} failed. Destroy this stick if it held sensitive data.", bytes(failed)));
    }
    Summary { tone, title: title.to_string(), lines }
}

pub fn reset(r: &ResetReport) -> Summary {
    let mut lines = r.problems.clone();
    if let Some(f) = &r.format {
        let label = if f.label.is_empty() { String::new() } else { format!(", label {}", f.label) };
        lines.push(format!("{}, {}{label}", f.filesystem, bytes(r.partition_size)));
    }
    lines.push(if r.data_erased {
        "Old data: overwritten and verified.".to_string()
    } else {
        "Old data: NOT erased, still recoverable.".to_string()
    });
    lines.push(format!("Took {}", duration(r.duration_secs)));
    let (tone, title) = if !r.success() {
        (Tone::Bad, "Factory reset had problems")
    } else if r.data_erased {
        (Tone::Good, "Factory reset complete")
    } else {
        (Tone::Warning, "Quick reset complete")
    };
    Summary { tone, title: title.to_string(), lines }
}

pub fn backup(r: &BackupReport) -> Summary {
    let mut lines = vec![r.path.clone(), format!("SHA-256: {}", r.sha256)];
    if !r.unreadable.is_empty() {
        lines.push(format!("{} unreadable, zero-filled in the image.", bytes(r.unreadable.total())));
    }
    let (tone, title) = if r.aborted {
        (Tone::Bad, "Backup incomplete: the device stopped responding")
    } else if r.unreadable.is_empty() {
        (Tone::Good, "Backup complete")
    } else {
        (Tone::Warning, "Backup complete, with unreadable areas")
    };
    Summary { tone, title: title.to_string(), lines }
}

pub fn restore(r: &RestoreReport) -> Summary {
    let ok = r.verify_failures.is_empty();
    Summary {
        tone: if ok { Tone::Good } else { Tone::Bad },
        title: if ok { "Image restored and verified" } else { "Restore verification failed" }.to_string(),
        lines: vec![r.path.clone(), format!("{} written", bytes(r.bytes))],
    }
}
