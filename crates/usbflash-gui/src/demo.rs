// SPDX-License-Identifier: GPL-3.0-or-later
//! Made-up devices and results, for working on the interface without hardware and
//! for screenshots. Only compiled with the `demo` feature. That build lists no real
//! disks and never calls a device operation, so it cannot touch anything.
//!
//!   cargo run -p usbflash-gui --features demo -- --tab health --scene result --theme light

use super::{Action, App, DeviceState, Pending, Tab, key};
use crate::jobs::{self, Job, Outcome};
use crate::summary::{self, Summary, Tone};
use eframe::egui::{self, ThemePreference};
use std::time::Duration;
use usbflash_core::analyze::{Analysis, ContentKind, ContentStats, PartitionReport};
use usbflash_core::device::{BusType, DeviceInfo, RangeList};
use usbflash_core::fsdetect::FsInfo;
use usbflash_core::health::{HealthReport, Verdict};
use usbflash_core::partition::{Partition, TableKind};
use usbflash_core::scan::{ScanMode, ScanReport};
use usbflash_core::smart::SmartOutcome;

const GIB: u64 = 1 << 30;

fn device(vendor: &str, model: &str, serial: &str, size: u64, mount: &[&str]) -> DeviceInfo {
    DeviceInfo {
        path: format!("\\\\.\\PhysicalDriveDemo{serial}"),
        vendor: vendor.into(),
        model: model.into(),
        serial: serial.into(),
        bus: BusType::Usb,
        removable_media: true,
        media_present: size > 0,
        size_bytes: size,
        logical_sector: 512,
        physical_sector: 512,
        read_only: false,
        is_system: false,
        mount_points: mount.iter().map(|s| s.to_string()).collect(),
        volumes: vec![],
    }
}

fn devices() -> Vec<DeviceInfo> {
    vec![
        device("SanDisk", "Ultra Flair", "4C530001", 29 * GIB, &["F:\\"]),
        device("Kingston", "DataTraveler", "DEMO0002", 15 * GIB, &[]),
        DeviceInfo { read_only: true, ..device("Generic", "Flash Disk", "DEMO0003", 4 * GIB, &["G:\\"]) },
        device("Intenso", "Rainbow", "DEMO0004", 0, &[]),
        DeviceInfo {
            bus: BusType::Nvme,
            is_system: true,
            removable_media: false,
            ..device("Demo", "NVMe SSD", "DEMO0005", 476 * GIB, &["C:\\"])
        },
    ]
}

fn fs(name: &str, label: &str, encrypted: bool, native: bool) -> FsInfo {
    FsInfo { name: name.into(), label: label.into(), uuid: String::new(), encrypted, windows_native: native }
}

fn part(index: u32, start: u64, size: u64, type_name: &str, f: FsInfo) -> PartitionReport {
    PartitionReport {
        partition: Partition {
            index,
            start,
            size,
            type_name: type_name.into(),
            type_id: String::new(),
            name: String::new(),
            bootable: false,
        },
        filesystem: Some(f),
    }
}

fn analysis_data() -> Analysis {
    Analysis {
        size_bytes: 29 * GIB,
        sector_size: 512,
        table_kind: TableKind::Mbr,
        disk_id: "DEMO".into(),
        partitions: vec![part(1, 1 << 20, 29 * GIB - (1 << 20), "FAT32 (LBA)", fs("FAT32", "HOLIDAY", false, true))],
        whole_device_fs: None,
        content: ContentKind::Data,
        stats: ContentStats { samples: 256, blank: 141, high_entropy: 38, data: 77, unreadable: 0 },
        findings: vec!["Contains data: wipe before giving this device away".into()],
    }
}

fn analysis_linux() -> Analysis {
    Analysis {
        size_bytes: 15 * GIB,
        sector_size: 512,
        table_kind: TableKind::Gpt,
        disk_id: "DEMO".into(),
        partitions: vec![
            part(1, 1 << 20, 4 * GIB, "Linux filesystem", fs("ext4", "boot", false, false)),
            part(2, 4 * GIB + (1 << 20), 10 * GIB, "LUKS", fs("LUKS2", "", true, false)),
        ],
        whole_device_fs: None,
        content: ContentKind::RandomOrEncrypted,
        stats: ContentStats { samples: 256, blank: 12, high_entropy: 230, data: 14, unreadable: 0 },
        findings: vec![
            "ext4: not readable on Windows without additional drivers".into(),
            "LUKS2 encrypted volume: contents cannot be inspected without the key".into(),
        ],
    }
}

fn health() -> HealthReport {
    let speed_profile = (0..128)
        .map(|i: i32| {
            let wave = ((i as f32) * 0.31).sin() * 6.0e6 + ((i as f32) * 0.07).cos() * 4.0e6;
            (if i > 92 { 62.0e6 } else { 118.0e6 }) + wave
        })
        .collect();
    HealthReport {
        verdict: Verdict::Good,
        reasons: vec!["No problems found.".into()],
        notes: vec![
            "No wear data available: most USB sticks do not report SMART, so this verdict is based on observed behavior only.".into(),
            "Reading alone cannot prove the flash is writable; the capacity test does.".into(),
        ],
        scan: Some(ScanReport {
            mode: ScanMode::Quick,
            bytes_read: 256 << 20,
            unreadable: RangeList::default(),
            avg_bytes_per_sec: 104.0e6,
            min_bytes_per_sec: 54.0e6,
            max_bytes_per_sec: 128.0e6,
            speed_profile,
            duration_secs: 3.0,
            aborted: false,
        }),
        smart: Some(SmartOutcome::Unavailable { reason: "the device does not support SMART over USB".into() }),
    }
}

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args();
    args.find(|a| a == name)?;
    args.next()
}

/// Fills the app with the scene selected on the command line.
pub fn apply(app: &mut App, ctx: &egui::Context) {
    app.devices = devices();
    app.show_all = true;
    match arg("--theme").as_deref() {
        Some("light") => app.theme = ThemePreference::Light,
        Some("dark") => app.theme = ThemePreference::Dark,
        _ => {}
    }
    ctx.set_theme(app.theme);
    app.tab = match arg("--tab").as_deref() {
        Some("health") => Tab::Health,
        Some("erase") => Tab::Erase,
        Some("image") => Tab::Image,
        _ => Tab::Overview,
    };

    let list = devices();
    let first = key(&list[0]);
    app.states.insert(
        first.clone(),
        DeviceState { analysis: Some(analysis_data()), analysis_requested: true, ..Default::default() },
    );
    app.states.insert(
        key(&list[1]),
        DeviceState { analysis: Some(analysis_linux()), analysis_requested: true, ..Default::default() },
    );
    app.selected = Some(first.clone());
    if let Some(i) = arg("--device").and_then(|s| s.parse::<usize>().ok()).filter(|&i| i < list.len()) {
        app.selected = Some(key(&list[i]));
    }

    let state = app.states.entry(first.clone()).or_default();
    if arg("--scene").as_deref() != Some("fresh") {
        state.health = Some(health());
    }
    match arg("--scene").as_deref() {
        Some("empty") => {
            app.devices.clear();
            app.states.clear();
            app.selected = None;
        }
        Some("running") => {
            app.jobs.insert(first, simulate(ctx, "Factory reset", true));
        }
        Some("result") => {
            state.last = Some(Summary {
                tone: Tone::Good,
                title: "Factory reset complete".into(),
                lines: vec![
                    "FAT32, 29.0 GiB, label USB".into(),
                    "Old data: overwritten and verified.".into(),
                    "Took 12m 41s".into(),
                ],
            });
            state.last_report = Some("{}".into());
            app.log.push((Tone::Good, "Factory reset: Factory reset complete".into()));
        }
        Some("failed") => {
            state.last = Some(Summary {
                tone: Tone::Bad,
                title: "Wipe incomplete: old data may remain".into(),
                lines: vec!["128.0 MiB failed. Destroy this stick if it held sensitive data.".into()],
            });
            state.last_report = Some("{}".into());
            app.log.push((Tone::Bad, "Wipe: Wipe incomplete: old data may remain".into()));
        }
        Some("dialog") => {
            app.pending = Some(Pending { action: Action::Reset, device: list[0].clone(), typed: String::new() });
        }
        Some("dialog-typed") => {
            let device = DeviceInfo { removable_media: false, size_bytes: 931 * GIB, ..list[0].clone() };
            app.pending = Some(Pending { action: Action::Reset, device, typed: String::new() });
        }
        _ => {}
    }
}

/// A job that only pretends to work.
pub fn simulate(ctx: &egui::Context, title: &'static str, destructive: bool) -> Job {
    jobs::spawn(ctx, title, destructive, move |c| {
        let total = 29 * GIB;
        c.begin_phase("Pass 1/1: writing random data", total);
        for i in 0..=300u64 {
            c.check_cancel()?;
            c.update(total / 300 * i, 0);
            std::thread::sleep(Duration::from_millis(200));
        }
        let h = health();
        let summary = if title == "Health check" {
            summary::health(&h)
        } else {
            Summary { tone: Tone::Good, title: format!("{title} complete"), lines: vec!["Simulated.".into()] }
        };
        Ok(Outcome::new(summary, "{}".into()))
    })
}
