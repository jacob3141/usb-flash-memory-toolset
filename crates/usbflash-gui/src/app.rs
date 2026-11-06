// SPDX-License-Identifier: GPL-3.0-or-later
//! The main window.

use crate::jobs::{self, Job, Outcome};
use crate::summary::{self, Summary, Tone};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Stroke, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use usbflash_core::analyze::{Analysis, ContentKind};
use usbflash_core::format::FsChoice;
use usbflash_core::guard::{self, Confirmation};
use usbflash_core::health::HealthReport;
use usbflash_core::report::{Report, VERSION};
use usbflash_core::reset::{EraseChoice, ResetOptions, TableChoice};
use usbflash_core::scan::ScanMode;
use usbflash_core::units::{bytes, duration, speed};
use usbflash_core::wipe::WipeMode;
use usbflash_core::{DeviceInfo, Error, Overrides, Target, ops};

const GOOD: Color32 = Color32::from_rgb(76, 175, 110);
const WARN: Color32 = Color32::from_rgb(224, 164, 60);
const BAD: Color32 = Color32::from_rgb(222, 84, 84);
const ACCENT: Color32 = Color32::from_rgb(90, 150, 230);
const CARD: Color32 = Color32::from_rgb(40, 43, 50);
const CARD_ACTIVE: Color32 = Color32::from_rgb(48, 62, 84);
const MUTED: Color32 = Color32::from_rgb(150, 155, 165);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Health,
    Erase,
    Image,
}

#[derive(Clone)]
enum Action {
    Reset,
    Wipe,
    CapacityTest,
    Restore(PathBuf),
}

impl Action {
    fn description(&self) -> String {
        match self {
            Action::Reset => "Restore the factory state".to_string(),
            Action::Wipe => "Overwrite the entire device".to_string(),
            Action::CapacityTest => "Run the destructive capacity test".to_string(),
            Action::Restore(p) => format!("Write the image {}", p.display()),
        }
    }
}

struct Pending {
    action: Action,
    device: DeviceInfo,
    typed: String,
}

/// Everything learned about one physical stick during this session.
#[derive(Default)]
struct DeviceState {
    analysis: Option<Analysis>,
    health: Option<HealthReport>,
    analysis_error: Option<String>,
    last: Option<Summary>,
    last_report: Option<String>,
    analysis_requested: bool,
}

fn key(d: &DeviceInfo) -> String {
    format!("{}|{}|{}|{}", d.path, d.model, d.serial, d.size_bytes)
}

fn tone_color(t: Tone) -> Color32 {
    match t {
        Tone::Good => GOOD,
        Tone::Warning => WARN,
        Tone::Bad => BAD,
    }
}

pub struct App {
    monitor: Receiver<Result<Vec<DeviceInfo>, String>>,
    devices: Vec<DeviceInfo>,
    monitor_error: Option<String>,
    selected: Option<String>,
    show_all: bool,
    tab: Tab,
    states: HashMap<String, DeviceState>,
    jobs: HashMap<String, Job>,
    pending: Option<Pending>,
    log: Vec<(Tone, String)>,
    // Options of the erase tab.
    quick_reset: bool,
    wipe_mode: WipeMode,
    filesystem: FsChoice,
    table: TableChoice,
    label: String,
    full_scan: bool,
    full_capacity: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        ctx.set_theme(egui::Theme::Dark);
        ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = Vec2::new(8.0, 8.0);
            s.spacing.button_padding = Vec2::new(12.0, 6.0);
        });
        Self {
            monitor: jobs::start_monitor(ctx.clone()),
            devices: vec![],
            monitor_error: None,
            selected: None,
            show_all: std::env::args().any(|a| a == "--all"),
            tab: Tab::Overview,
            states: HashMap::new(),
            jobs: HashMap::new(),
            pending: None,
            log: vec![],
            quick_reset: false,
            wipe_mode: WipeMode::Random,
            filesystem: FsChoice::Auto,
            table: TableChoice::Auto,
            label: String::new(),
            full_scan: false,
            full_capacity: false,
        }
    }

    fn visible(&self) -> Vec<DeviceInfo> {
        self.devices.iter().filter(|d| self.show_all || (d.bus.is_flash_bus() && !d.is_system)).cloned().collect()
    }

    fn selected_device(&self) -> Option<DeviceInfo> {
        let sel = self.selected.as_ref()?;
        self.devices.iter().find(|d| &key(d) == sel).cloned()
    }

    fn poll_monitor(&mut self) {
        while let Ok(update) = self.monitor.try_recv() {
            match update {
                Ok(list) => {
                    self.devices = list;
                    self.monitor_error = None;
                }
                Err(e) => self.monitor_error = Some(e),
            }
        }
        let visible = self.visible();
        let still_there = self.selected.as_ref().is_some_and(|s| visible.iter().any(|d| &key(d) == s));
        if !still_there {
            // With "All disks" on, still start on a device that can actually be worked on.
            let preferred = visible.iter().find(|d| d.bus.is_flash_bus() && !d.is_system);
            self.selected = preferred.or(visible.first()).map(key);
        }
    }

    fn poll_jobs(&mut self) {
        let mut finished = vec![];
        for (k, job) in &self.jobs {
            if let Ok(result) = job.done.try_recv() {
                finished.push((k.clone(), job.title.clone(), result));
            }
        }
        for (k, title, result) in finished {
            self.jobs.remove(&k);
            let state = self.states.entry(k).or_default();
            match result {
                Ok(o) => {
                    if let Some(a) = o.analysis {
                        state.analysis = Some(a);
                        state.analysis_error = None;
                    }
                    if let Some(h) = o.health {
                        state.health = Some(h);
                    }
                    if o.modified {
                        // Requests a fresh analysis on the next frame.
                        state.analysis = None;
                        state.analysis_requested = false;
                    }
                    if !o.report_json.is_empty() {
                        state.last_report = Some(o.report_json);
                        self.log.push((o.summary.tone, format!("{title}: {}", o.summary.title)));
                        state.last = Some(o.summary);
                    }
                }
                Err(e) => {
                    let (tone, text) = match e {
                        Error::Cancelled => {
                            (Tone::Warning, "Cancelled. The device may be in an incomplete state.".to_string())
                        }
                        e => (Tone::Bad, e.to_string()),
                    };
                    if title == "Analyze" {
                        state.analysis_error = Some(text.clone());
                    } else {
                        state.last =
                            Some(Summary { tone, title: format!("{title} did not finish"), lines: vec![text.clone()] });
                        state.analysis = None;
                        state.analysis_requested = false;
                    }
                    self.log.push((tone, format!("{title}: {text}")));
                }
            }
        }
    }

    /// Analyzes the selected device automatically, as this is read-only.
    fn auto_analyze(&mut self, ctx: &egui::Context) {
        let Some(d) = self.selected_device() else { return };
        let k = key(&d);
        if self.jobs.contains_key(&k) || !d.media_present {
            return;
        }
        let state = self.states.entry(k.clone()).or_default();
        if state.analysis_requested {
            return;
        }
        state.analysis_requested = true;
        let job = jobs::spawn(ctx, "Analyze", false, move |c| {
            let analysis = ops::analyze(&Target::Device(d), c)?;
            Ok(Outcome {
                analysis: Some(analysis),
                ..Outcome::new(Summary { tone: Tone::Good, title: String::new(), lines: vec![] }, String::new())
            })
        });
        self.jobs.insert(k, job);
    }

    fn run_health(&mut self, ctx: &egui::Context, d: DeviceInfo) {
        let mode = if self.full_scan { ScanMode::Full } else { ScanMode::Quick };
        let job = jobs::spawn(ctx, "Health check", false, {
            let d = d.clone();
            move |c| {
                let h = ops::health(&d, mode, c)?;
                let json = Report::new("health", Some(d), &h).to_json();
                Ok(Outcome { health: Some(h.clone()), ..Outcome::new(summary::health(&h), json) })
            }
        });
        self.jobs.insert(key(&d), job);
    }

    fn run_backup(&mut self, ctx: &egui::Context, d: DeviceInfo, path: PathBuf) {
        let job = jobs::spawn(ctx, "Backup", false, {
            let d = d.clone();
            move |c| {
                let r = ops::backup(&Target::Device(d.clone()), &path, c)?;
                Ok(Outcome::new(summary::backup(&r), Report::new("backup", Some(d), &r).to_json()))
            }
        });
        self.jobs.insert(key(&d), job);
    }

    fn run_action(&mut self, ctx: &egui::Context, action: Action, d: DeviceInfo) {
        // Reaching this point means the user confirmed in the dialog, typing the
        // word if the device might be an external disk.
        let overrides = Overrides { allow_fixed: true };
        let target = Target::Device(d.clone());
        let device = d.clone();
        let title = match action {
            Action::Reset => "Factory reset",
            Action::Wipe => "Wipe",
            Action::CapacityTest => "Capacity test",
            Action::Restore(_) => "Restore",
        };
        let opts = ResetOptions {
            erase: if self.quick_reset { EraseChoice::Quick } else { EraseChoice::Full(self.wipe_mode) },
            filesystem: self.filesystem,
            table: self.table,
            label: self.label.clone(),
        };
        let wipe_mode = self.wipe_mode;
        let capacity_mode = if self.full_capacity { ScanMode::Full } else { ScanMode::Quick };
        let job = jobs::spawn(ctx, title, true, move |c| {
            let (summary, json) = match action {
                Action::Reset => {
                    let r = ops::factory_reset(&target, &opts, overrides, c)?;
                    (summary::reset(&r), Report::new("factory-reset", Some(device), &r).to_json())
                }
                Action::Wipe => {
                    let r = ops::wipe(&target, wipe_mode, true, overrides, c)?;
                    (summary::wipe(&r), Report::new("wipe", Some(device), &r).to_json())
                }
                Action::CapacityTest => {
                    let r = ops::capacity_test(&target, capacity_mode, overrides, c)?;
                    (summary::capacity(&r), Report::new("capacity-test", Some(device), &r).to_json())
                }
                Action::Restore(path) => {
                    let r = ops::restore(&target, &path, overrides, c)?;
                    (summary::restore(&r), Report::new("restore", Some(device), &r).to_json())
                }
            };
            Ok(Outcome { modified: true, ..Outcome::new(summary, json) })
        });
        self.jobs.insert(key(&d), job);
    }

    fn request(&mut self, action: Action, device: DeviceInfo) {
        self.pending = Some(Pending { action, device, typed: String::new() });
    }

    // ---- Panels -------------------------------------------------------------

    fn device_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Devices").strong().size(15.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.checkbox(&mut self.show_all, "All disks");
            });
        });
        if let Some(e) = &self.monitor_error {
            ui.colored_label(BAD, e);
        }
        let visible = self.visible();
        if visible.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No USB stick found").color(MUTED));
                ui.label(RichText::new("Plug one in; it appears here automatically.").small().color(MUTED));
            });
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for d in visible {
                self.device_card(ui, &d);
            }
        });
    }

    fn device_card(&mut self, ui: &mut egui::Ui, d: &DeviceInfo) {
        let k = key(d);
        let active = self.selected.as_ref() == Some(&k);
        let job_progress = self.jobs.get(&k).map(|j| {
            let p = j.progress.lock().map(|p| p.clone()).unwrap_or_default();
            (j.title.clone(), p.fraction())
        });
        let response = egui::Frame::new()
            .fill(if active { CARD_ACTIVE } else { CARD })
            .corner_radius(CornerRadius::same(6))
            .inner_margin(10)
            .stroke(if active { Stroke::new(1.0, ACCENT) } else { Stroke::NONE })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(d.display_name()).strong());
                let size = if d.media_present { bytes(d.size_bytes) } else { "No media".to_string() };
                let mounts = d.mount_points.join(" ");
                ui.label(RichText::new(format!("{size}   {mounts}")).small().color(MUTED));
                if d.is_system {
                    ui.label(RichText::new("System disk, protected").small().color(WARN));
                } else if !d.bus.is_flash_bus() {
                    ui.label(RichText::new(format!("{} disk, protected", d.bus.label())).small().color(WARN));
                } else if d.read_only {
                    ui.label(RichText::new("Write protected").small().color(WARN));
                }
                if let Some((title, f)) = job_progress {
                    ui.add(egui::ProgressBar::new(f).desired_height(6.0));
                    ui.label(RichText::new(title).small().color(ACCENT));
                } else if let Some(Summary { tone, title, .. }) = self.states.get(&k).and_then(|s| s.last.as_ref()) {
                    ui.label(RichText::new(title).small().color(tone_color(*tone)));
                }
            })
            .response
            .interact(egui::Sense::click());
        if response.clicked() {
            self.selected = Some(k);
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(d) = self.selected_device() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Select a device").color(MUTED).size(16.0));
            });
            return;
        };
        ui.add_space(8.0);
        ui.label(RichText::new(d.display_name()).size(20.0).strong());
        let serial = if d.serial.is_empty() { "unknown".to_string() } else { d.serial.clone() };
        let size = if d.media_present { bytes(d.size_bytes) } else { "no media".to_string() };
        ui.label(
            RichText::new(format!("{size}  |  {}  |  Serial {serial}  |  {}", d.bus.label(), d.path)).color(MUTED),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (tab, name) in [
                (Tab::Overview, "Overview"),
                (Tab::Health, "Health"),
                (Tab::Erase, "Erase & Reset"),
                (Tab::Image, "Image"),
            ] {
                ui.selectable_value(&mut self.tab, tab, name);
            }
        });
        ui.separator();

        self.job_banner(ui, &d);
        self.result_banner(ui, &d);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
            Tab::Overview => self.tab_overview(ui, ctx, &d),
            Tab::Health => self.tab_health(ui, ctx, &d),
            Tab::Erase => self.tab_erase(ui, &d),
            Tab::Image => self.tab_image(ui, ctx, &d),
        });
    }

    fn job_banner(&mut self, ui: &mut egui::Ui, d: &DeviceInfo) {
        let Some(job) = self.jobs.get(&key(d)) else { return };
        let p = job.progress.lock().map(|p| p.clone()).unwrap_or_default();
        egui::Frame::new().fill(CARD).corner_radius(CornerRadius::same(6)).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&job.title).strong());
                ui.label(RichText::new(&p.phase).color(MUTED));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        job.cancel();
                    }
                });
            });
            let text = if p.total > 0 { format!("{:.1} %", p.fraction() * 100.0) } else { String::new() };
            ui.add(egui::ProgressBar::new(p.fraction()).text(text));
            if p.total > 0 {
                let eta = p.eta_secs.map(duration).unwrap_or_else(|| "--".into());
                let mut line =
                    format!("{} of {}  |  {}  |  {eta} left", bytes(p.done), bytes(p.total), speed(p.bytes_per_sec));
                if p.errors > 0 {
                    line.push_str(&format!("  |  {} failed", bytes(p.errors)));
                }
                ui.label(RichText::new(line).small().color(if p.errors > 0 { WARN } else { MUTED }));
            }
            if job.destructive {
                ui.label(RichText::new("Do not unplug this device.").small().color(WARN));
            }
        });
    }

    fn result_banner(&mut self, ui: &mut egui::Ui, d: &DeviceInfo) {
        let Some(state) = self.states.get(&key(d)) else { return };
        let Some(s) = &state.last else { return };
        let report = state.last_report.clone();
        let color = tone_color(s.tone);
        egui::Frame::new()
            .fill(color.gamma_multiply(0.15))
            .stroke(Stroke::new(1.0, color))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(12)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&s.title).strong().color(color));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(json) = report
                            && ui.button("Save report").clicked()
                        {
                            save_report(&json, d);
                        }
                    });
                });
                for l in &s.lines {
                    ui.label(l);
                }
            });
        ui.add_space(4.0);
    }

    fn tab_overview(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        if let Some(reason) = refuse_reason(d) {
            ui.colored_label(WARN, reason);
        } else {
            ui.add_space(4.0);
            let button = egui::Button::new(RichText::new("Factory reset").size(16.0).strong().color(Color32::WHITE))
                .fill(BAD.gamma_multiply(0.85))
                .min_size(Vec2::new(200.0, 40.0));
            if ui.add_enabled(!busy, button).clicked() {
                self.request(Action::Reset, d.clone());
            }
            let what = if self.quick_reset {
                "Clears the metadata (quick), then creates a fresh partition and filesystem."
            } else {
                "Overwrites every sector, verifies, then creates a fresh partition and filesystem."
            };
            ui.label(RichText::new(what).small().color(MUTED));
            ui.add_space(8.0);
        }

        let state = self.states.get(&key(d));
        if let Some(e) = state.and_then(|s| s.analysis_error.as_ref()) {
            ui.colored_label(BAD, format!("Analysis failed: {e}"));
            if ui.add_enabled(!busy, egui::Button::new("Analyze again")).clicked() {
                if let Some(s) = self.states.get_mut(&key(d)) {
                    s.analysis_requested = false;
                }
                ctx.request_repaint();
            }
            return;
        }
        if !d.media_present {
            ui.colored_label(
                BAD,
                "The device reports no media or zero capacity. This is the typical failure of a dead stick.",
            );
            return;
        }
        let Some(a) = state.and_then(|s| s.analysis.as_ref()) else {
            ui.label(RichText::new("Analyzing...").color(MUTED));
            return;
        };

        section(ui, "Content");
        let color = match a.content {
            ContentKind::Blank => GOOD,
            ContentKind::RandomOrEncrypted => ACCENT,
            ContentKind::Data => WARN,
            ContentKind::Unreadable => BAD,
        };
        ui.label(RichText::new(a.content.label()).color(color).strong());
        content_bar(ui, a);

        section(ui, "Layout");
        ui.label(format!("Partition table: {}", a.table_kind.label()));
        if let Some(f) = &a.whole_device_fs {
            ui.label(format!("Filesystem on the whole device: {}{}", f.name, label_suffix(&f.label)));
        }
        if !a.partitions.is_empty() {
            egui::Grid::new("partitions").striped(true).num_columns(5).spacing([16.0, 6.0]).show(ui, |ui| {
                for h in ["#", "Size", "Filesystem", "Label", "Partition type"] {
                    ui.label(RichText::new(h).small().color(MUTED));
                }
                ui.end_row();
                for p in &a.partitions {
                    ui.label(p.partition.index.to_string());
                    ui.label(bytes(p.partition.size));
                    match &p.filesystem {
                        Some(f) => {
                            let suffix = if f.encrypted { " (encrypted)" } else { "" };
                            ui.label(format!("{}{suffix}", f.name));
                            ui.label(&f.label);
                        }
                        None => {
                            ui.label(RichText::new("unknown").color(WARN));
                            ui.label("");
                        }
                    }
                    ui.label(&p.partition.type_name);
                    ui.end_row();
                }
            });
        }
        if !d.mount_points.is_empty() {
            ui.label(format!("Mounted at: {}", d.mount_points.join(", ")));
        }

        if !a.findings.is_empty() {
            section(ui, "Findings");
            for f in &a.findings {
                ui.label(format!("- {f}"));
            }
        }
    }

    fn tab_health(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        section(ui, "Read test (safe, changes nothing)");
        ui.checkbox(&mut self.full_scan, "Read every sector instead of samples (slow)");
        if ui.add_enabled(!busy, egui::Button::new("Run health check")).clicked() {
            self.run_health(ctx, d.clone());
        }

        if let Some(h) = self.states.get(&key(d)).and_then(|s| s.health.clone()) {
            if let Some(s) = &h.scan {
                ui.add_space(8.0);
                ui.label(format!(
                    "Read {} at avg {}, min {}, max {}",
                    bytes(s.bytes_read),
                    speed(s.avg_bytes_per_sec),
                    speed(s.min_bytes_per_sec),
                    speed(s.max_bytes_per_sec)
                ));
                speed_plot(ui, &s.speed_profile);
                if !s.unreadable.is_empty() {
                    ui.colored_label(
                        BAD,
                        format!("Unreadable: {} in {} area(s)", bytes(s.unreadable.total()), s.unreadable.ranges.len()),
                    );
                }
            }
            match &h.smart {
                Some(usbflash_core::smart::SmartOutcome::Available { attributes }) => {
                    section(ui, "SMART");
                    egui::Grid::new("smart").striped(true).num_columns(3).show(ui, |ui| {
                        for a in attributes {
                            ui.label(a.name);
                            ui.label(a.current.to_string());
                            ui.label(a.raw.to_string());
                            ui.end_row();
                        }
                    });
                }
                Some(usbflash_core::smart::SmartOutcome::Unavailable { reason }) => {
                    ui.label(RichText::new(format!("SMART not available: {reason}")).small().color(MUTED));
                }
                None => {}
            }
            for n in &h.notes {
                ui.label(RichText::new(n).small().color(MUTED));
            }
        }

        section(ui, "Capacity test (destroys all data)");
        ui.label("Writes a pattern and reads it back to find bad flash and fake capacity.");
        ui.checkbox(&mut self.full_capacity, "Test every sector instead of probes (slow, but authoritative)");
        let allowed = refuse_reason(d).is_none();
        if ui.add_enabled(!busy && allowed, egui::Button::new("Run capacity test")).clicked() {
            self.request(Action::CapacityTest, d.clone());
        }
    }

    fn tab_erase(&mut self, ui: &mut egui::Ui, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        let allowed = refuse_reason(d).is_none();
        if let Some(reason) = refuse_reason(d) {
            ui.colored_label(WARN, reason);
        }

        section(ui, "Erase pattern");
        for mode in [WipeMode::Random, WipeMode::Zero, WipeMode::RandomThenZero] {
            ui.radio_value(&mut self.wipe_mode, mode, mode.label());
        }
        ui.label(
            RichText::new(
                "One pass is enough on flash; more passes only add wear. Spare blocks of the controller cannot \
                 be reached by any software, so destroy sticks with highly sensitive data physically.",
            )
            .small()
            .color(MUTED),
        );

        section(ui, "Wipe");
        if ui.add_enabled(!busy && allowed, egui::Button::new("Wipe device")).clicked() {
            self.request(Action::Wipe, d.clone());
        }

        section(ui, "Factory reset");
        ui.checkbox(&mut self.quick_reset, "Quick: only clear metadata. Old data stays recoverable!");
        egui::Grid::new("reset-options").num_columns(2).show(ui, |ui| {
            ui.label("Filesystem");
            egui::ComboBox::from_id_salt("fs")
                .selected_text(match self.filesystem {
                    FsChoice::Auto => "Automatic (FAT up to 32 GiB, else exFAT)",
                    FsChoice::Fat => "FAT32 / FAT16",
                    FsChoice::Exfat => "exFAT",
                    FsChoice::None => "None (blank device)",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.filesystem,
                        FsChoice::Auto,
                        "Automatic (FAT up to 32 GiB, else exFAT)",
                    );
                    ui.selectable_value(&mut self.filesystem, FsChoice::Fat, "FAT32 / FAT16");
                    ui.selectable_value(&mut self.filesystem, FsChoice::Exfat, "exFAT");
                    ui.selectable_value(&mut self.filesystem, FsChoice::None, "None (blank device)");
                });
            ui.end_row();
            ui.label("Partition table");
            egui::ComboBox::from_id_salt("table")
                .selected_text(match self.table {
                    TableChoice::Auto => "Automatic (MBR, GPT above 2 TiB)",
                    TableChoice::Mbr => "MBR",
                    TableChoice::Gpt => "GPT",
                    TableChoice::None => "None",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.table, TableChoice::Auto, "Automatic (MBR, GPT above 2 TiB)");
                    ui.selectable_value(&mut self.table, TableChoice::Mbr, "MBR");
                    ui.selectable_value(&mut self.table, TableChoice::Gpt, "GPT");
                    ui.selectable_value(&mut self.table, TableChoice::None, "None");
                });
            ui.end_row();
            ui.label("Label");
            ui.add(egui::TextEdit::singleline(&mut self.label).desired_width(160.0).hint_text("optional"));
            ui.end_row();
        });
        if ui.add_enabled(!busy && allowed, egui::Button::new("Factory reset")).clicked() {
            self.request(Action::Reset, d.clone());
        }
    }

    fn tab_image(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        section(ui, "Back up to an image file");
        ui.label("Saves the whole device, including unknown filesystems. Read errors are tolerated and listed.");
        if ui.add_enabled(!busy && d.media_present, egui::Button::new("Back up...")).clicked() {
            let name = format!("{}.img", d.display_name().replace(' ', "_"));
            if let Some(path) =
                rfd::FileDialog::new().set_file_name(name).add_filter("Disk image", &["img"]).save_file()
            {
                self.run_backup(ctx, d.clone(), path);
            }
        }
        section(ui, "Restore an image file (destroys all data)");
        let allowed = refuse_reason(d).is_none();
        if ui.add_enabled(!busy && allowed, egui::Button::new("Restore...")).clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("Disk image", &["img", "iso", "bin"]).pick_file()
        {
            self.request(Action::Restore(path), d.clone());
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.pending.take() else { return };
        let mut decision = None;
        let word_needed = guard::required_confirmation(&p.device) == Confirmation::Typed;
        egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new(p.action.description()).size(17.0).strong());
            ui.add_space(4.0);
            egui::Frame::new().fill(CARD).corner_radius(CornerRadius::same(6)).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(p.device.display_name()).strong());
                ui.label(format!("Size: {}", bytes(p.device.size_bytes)));
                let serial = if p.device.serial.is_empty() { "unknown" } else { &p.device.serial };
                ui.label(format!("Serial: {serial}"));
                ui.label(format!("Connection: {}, {}", p.device.bus.label(), p.device.path));
                if !p.device.mount_points.is_empty() {
                    ui.label(format!("Currently mounted at: {}", p.device.mount_points.join(", ")));
                }
            });
            ui.colored_label(BAD, "All data on this device will be lost. This cannot be undone.");
            if word_needed {
                ui.colored_label(
                    WARN,
                    "This device reports fixed media or is unusually large. It may be an external disk rather \
                     than a flash stick. Type ERASE to confirm.",
                );
                ui.text_edit_singleline(&mut p.typed);
            }
            ui.add_space(4.0);
            let confirmed = !word_needed || p.typed == "ERASE";
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                let go = egui::Button::new(RichText::new("Erase and continue").color(Color32::WHITE)).fill(BAD);
                if ui.add_enabled(confirmed, go).clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) => self.run_action(ctx, p.action, p.device),
            Some(false) => {}
            None => self.pending = Some(p),
        }
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            if self.log.is_empty() {
                ui.label(RichText::new("Nothing done yet in this session.").small().color(MUTED));
            }
            for (tone, line) in &self.log {
                ui.label(RichText::new(line).small().color(tone_color(*tone)));
            }
        });
    }
}

fn refuse_reason(d: &DeviceInfo) -> Option<String> {
    // Fixed media can be confirmed in the dialog, so it does not block the buttons.
    match guard::check_destructive(d, Overrides { allow_fixed: true }) {
        Ok(()) => d.read_only.then(|| {
            "This device is write protected. Without a lock switch, this usually means the controller locked \
             the flash permanently."
                .to_string()
        }),
        Err(e) => Some(e.to_string()),
    }
}

fn label_suffix(label: &str) -> String {
    if label.is_empty() { String::new() } else { format!(", label {label}") }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong().size(14.0));
}

fn content_bar(ui: &mut egui::Ui, a: &Analysis) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().min(520.0), 10.0), egui::Sense::hover());
    let total = a.stats.samples.max(1) as f32;
    let mut x = rect.left();
    for (n, color) in [
        (a.stats.data, WARN),
        (a.stats.high_entropy, ACCENT),
        (a.stats.blank, Color32::from_gray(70)),
        (a.stats.unreadable, BAD),
    ] {
        let w = rect.width() * n as f32 / total;
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(w, rect.height()));
        ui.painter().rect_filled(r, CornerRadius::ZERO, color);
        x += w;
    }
    ui.label(
        RichText::new(format!(
            "Spot check of {} areas: {} data, {} random-looking, {} blank, {} unreadable",
            a.stats.samples, a.stats.data, a.stats.high_entropy, a.stats.blank, a.stats.unreadable
        ))
        .small()
        .color(MUTED),
    );
}

fn speed_plot(ui: &mut egui::Ui, profile: &[f32]) {
    let max = profile.iter().copied().fold(0.0, f32::max);
    if max <= 0.0 {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().min(520.0), 70.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), CARD);
    let w = rect.width() / profile.len() as f32;
    for (i, &v) in profile.iter().enumerate() {
        if v <= 0.0 {
            continue;
        }
        let h = rect.height() * (v / max);
        let r = egui::Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * w, rect.bottom() - h),
            Vec2::new((w - 1.0).max(1.0), h),
        );
        painter.rect_filled(r, CornerRadius::ZERO, ACCENT);
    }
    ui.label(RichText::new(format!("Read speed across the device. Peak: {}", speed(max as f64))).small().color(MUTED));
}

fn save_report(json: &str, d: &DeviceInfo) {
    let name = format!("usbflash-{}-report.json", d.display_name().replace(' ', "_"));
    if let Some(path) = rfd::FileDialog::new().set_file_name(name).add_filter("JSON", &["json"]).save_file()
        && let Err(e) = std::fs::write(&path, json)
    {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Could not save the report")
            .set_description(e.to_string())
            .show();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_monitor();
        self.poll_jobs();
        self.auto_analyze(&ctx);

        egui::Panel::bottom("status").resizable(true).default_size(90.0).show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Activity").small().strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(format!("v{VERSION}  |  GPL-3.0-or-later")).small().color(MUTED));
                });
            });
            self.log_panel(ui);
        });
        egui::Panel::left("devices").resizable(true).default_size(290.0).show(ui, |ui| {
            self.device_list(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.details(ui, &ctx);
        });
        self.confirm_dialog(&ctx);

        // Keeps the speed and ETA figures moving even between progress events.
        if !self.jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}
