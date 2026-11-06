// SPDX-License-Identifier: GPL-3.0-or-later
//! The main window.

use crate::jobs::{self, Job, Outcome};
use crate::summary::{self, Summary, Tone};
use crate::theme::{self, Palette, palette};
use crate::widgets::{self as w, strong};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, Margin, RichText, ThemePreference, Vec2};
use egui_phosphor::regular as icon;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use usbflash_core::analyze::{Analysis, ContentKind};
use usbflash_core::format::FsChoice;
use usbflash_core::guard::{self, Confirmation};
use usbflash_core::health::{HealthReport, Verdict};
use usbflash_core::report::{Report, VERSION};
use usbflash_core::reset::{EraseChoice, ResetOptions, TableChoice};
use usbflash_core::scan::ScanMode;
use usbflash_core::smart::SmartOutcome;
use usbflash_core::units::{bytes, duration, speed};
use usbflash_core::wipe::WipeMode;
use usbflash_core::{DeviceInfo, Error, Overrides, Target, ops};

#[cfg(feature = "demo")]
#[path = "demo.rs"]
mod demo;

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
    fn title(&self) -> &'static str {
        match self {
            Action::Reset => "Factory reset",
            Action::Wipe => "Wipe",
            Action::CapacityTest => "Capacity test",
            Action::Restore(_) => "Restore",
        }
    }

    fn description(&self) -> String {
        match self {
            Action::Reset => "Restore the factory state?".to_string(),
            Action::Wipe => "Overwrite the entire device?".to_string(),
            Action::CapacityTest => "Run the destructive capacity test?".to_string(),
            Action::Restore(p) => format!("Write {} to this device?", p.display()),
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

fn tone_color(p: Palette, t: Tone) -> Color32 {
    match t {
        Tone::Good => p.good,
        Tone::Warning => p.warn,
        Tone::Bad => p.bad,
    }
}

fn tone_icon(t: Tone) -> &'static str {
    match t {
        Tone::Good => icon::CHECK_CIRCLE,
        Tone::Warning => icon::WARNING,
        Tone::Bad => icon::WARNING_CIRCLE,
    }
}

fn device_icon(d: &DeviceInfo) -> &'static str {
    if d.is_system || !d.bus.is_flash_bus() { icon::HARD_DRIVE } else { icon::USB }
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
    show_activity: bool,
    theme: ThemePreference,
    // Options of the erase view.
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
        theme::apply(ctx);
        // The demo build never looks at the disks of the machine it runs on.
        #[cfg(feature = "demo")]
        let monitor = std::sync::mpsc::channel().1;
        #[cfg(not(feature = "demo"))]
        let monitor = jobs::start_monitor(ctx.clone());
        #[allow(unused_mut)]
        let mut app = Self {
            monitor,
            devices: vec![],
            monitor_error: None,
            selected: None,
            show_all: std::env::args().any(|a| a == "--all"),
            tab: Tab::Overview,
            states: HashMap::new(),
            jobs: HashMap::new(),
            pending: None,
            log: vec![],
            show_activity: false,
            theme: ThemePreference::System,
            quick_reset: false,
            wipe_mode: WipeMode::Random,
            filesystem: FsChoice::Auto,
            table: TableChoice::Auto,
            label: String::new(),
            full_scan: false,
            full_capacity: false,
        };
        #[cfg(feature = "demo")]
        demo::apply(&mut app, ctx);
        app
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
            // With "All disks" on, still start on a device that can be worked on.
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
                        self.log.push((o.summary.tone, o.summary.title.clone()));
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
    #[cfg(not(feature = "demo"))]
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

    #[cfg(feature = "demo")]
    fn auto_analyze(&mut self, _ctx: &egui::Context) {}

    fn run_health(&mut self, ctx: &egui::Context, d: DeviceInfo) {
        #[cfg(feature = "demo")]
        {
            self.jobs.insert(key(&d), demo::simulate(ctx, "Health check", false));
            return;
        }
        #[allow(unreachable_code)]
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
        #[cfg(feature = "demo")]
        {
            let _ = path;
            self.jobs.insert(key(&d), demo::simulate(ctx, "Backup", false));
            return;
        }
        #[allow(unreachable_code)]
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
        #[cfg(feature = "demo")]
        {
            self.jobs.insert(key(&d), demo::simulate(ctx, action.title(), true));
            return;
        }
        // Reaching this point means the user confirmed in the dialog, typing the
        // word if the device might be an external disk.
        #[allow(unreachable_code)]
        let overrides = Overrides { allow_fixed: true };
        let target = Target::Device(d.clone());
        let device = d.clone();
        let title = action.title();
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

    // ---- Sidebar ------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = palette(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            w::icon_tile(ui, icon::USB, 34.0, p.accent);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(strong("USB Flash").size(15.0));
                ui.label(RichText::new("Memory Toolset").small().color(p.muted));
            });
        });

        egui::Panel::bottom("sidebar-footer").frame(egui::Frame::new().inner_margin(Margin::symmetric(0, 8))).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.show_all, "Show all disks");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (glyph, next, tip) = match self.theme {
                            ThemePreference::System => {
                                (icon::CIRCLE_HALF, ThemePreference::Light, "Theme: system. Switch to light")
                            }
                            ThemePreference::Light => {
                                (icon::SUN, ThemePreference::Dark, "Theme: light. Switch to dark")
                            }
                            ThemePreference::Dark => {
                                (icon::MOON, ThemePreference::System, "Theme: dark. Follow the system")
                            }
                        };
                        if w::icon_button(ui, p, glyph, tip).clicked() {
                            self.theme = next;
                            ctx.set_theme(next);
                        }
                    });
                });
            },
        );

        ui.add_space(14.0);
        let visible = self.visible();
        ui.label(RichText::new(format!("DEVICES  ·  {}", visible.len())).small().color(p.muted));
        if let Some(e) = &self.monitor_error {
            ui.colored_label(p.bad, e);
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for d in visible {
                self.device_card(ui, p, &d);
            }
        });
    }

    fn device_card(&mut self, ui: &mut egui::Ui, p: Palette, d: &DeviceInfo) {
        let k = key(d);
        let active = self.selected.as_ref() == Some(&k);
        let job = self.jobs.get(&k).map(|j| {
            let progress = j.progress.lock().map(|x| x.clone()).unwrap_or_default();
            (j.title.clone(), progress.fraction(), progress.total > 0)
        });
        let last = self.states.get(&k).and_then(|s| s.last.clone());
        let id = egui::Id::unique(("device", &k));
        let response = w::selectable_card(ui, p, id, active, |ui| {
            ui.horizontal(|ui| {
                let color = if d.is_system || !d.bus.is_flash_bus() {
                    p.muted
                } else if !d.media_present {
                    p.bad
                } else {
                    p.accent
                };
                w::icon_tile(ui, device_icon(d), 36.0, color);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(egui::Label::new(strong(d.display_name())).truncate());
                    let size = if d.media_present { bytes(d.size_bytes) } else { "No media".to_string() };
                    let mounts = d.mount_points.join(" ");
                    ui.label(RichText::new(format!("{size}   {mounts}")).small().color(p.muted));
                });
            });
            if let Some((title, fraction, known)) = job {
                ui.add_space(2.0);
                if known {
                    w::progress(ui, p, fraction, 5.0);
                }
                ui.label(RichText::new(title).small().color(p.accent));
            } else if d.is_system || !d.bus.is_flash_bus() {
                let text = if d.is_system {
                    "System disk, protected".to_string()
                } else {
                    "Internal disk, protected".to_string()
                };
                ui.label(RichText::new(format!("{}  {text}", icon::LOCK)).small().color(p.muted));
            } else if d.read_only {
                ui.label(RichText::new(format!("{}  Write protected", icon::LOCK)).small().color(p.warn));
            } else if let Some(s) = last {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{}  {}", tone_icon(s.tone), s.title))
                            .small()
                            .color(tone_color(p, s.tone)),
                    )
                    .truncate(),
                );
            }
        });
        if response.clicked() {
            self.selected = Some(k);
        }
    }

    // ---- Main area ----------------------------------------------------------

    fn details(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = palette(ui);
        let Some(d) = self.selected_device() else {
            self.empty_state(ui, p);
            return;
        };
        let busy = self.jobs.contains_key(&key(&d));
        let refused = refuse_reason(&d);

        ui.horizontal(|ui| {
            w::icon_tile(ui, device_icon(&d), 52.0, if refused.is_some() { p.muted } else { p.accent });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add(egui::Label::new(strong(d.display_name()).size(21.0)).truncate());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let size = if d.media_present { bytes(d.size_bytes) } else { "No media".to_string() };
                    w::chip(ui, size, p.muted);
                    w::chip(ui, d.bus.label(), p.muted);
                    if !d.mount_points.is_empty() {
                        w::chip(ui, d.mount_points.join(" "), p.muted);
                    }
                    if d.read_only {
                        w::chip(ui, "Write protected", p.warn);
                    }
                    if !d.removable_media && d.bus.is_flash_bus() {
                        w::chip(ui, "Fixed media", p.warn);
                    }
                    let serial = if d.serial.is_empty() { "No serial" } else { d.serial.as_str() };
                    ui.label(RichText::new(serial).small().color(p.muted));
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if refused.is_none()
                    && w::danger_button(ui, p, format!("{}  Factory reset", icon::SPARKLE), !busy).clicked()
                {
                    self.request(Action::Reset, d.clone());
                }
            });
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (tab, glyph, name) in [
                (Tab::Overview, icon::CHART_PIE_SLICE, "Overview"),
                (Tab::Health, icon::HEARTBEAT, "Health"),
                (Tab::Erase, icon::ERASER, "Erase"),
                (Tab::Image, icon::ARCHIVE, "Backup"),
            ] {
                let id = egui::Id::unique(("tab", name));
                if w::nav_item(ui, p, id, self.tab == tab, glyph, name) {
                    self.tab = tab;
                }
            }
        });
        let line = ui.cursor().min.y - 8.0;
        let width = ui.available_rect_before_wrap().x_range();
        ui.painter().hline(width, line, egui::Stroke::new(1.0, p.border));
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.set_max_width(ui.available_width() - 12.0);
            ui.add_space(2.0);
            self.job_card(ui, p, &d);
            self.result_card(ui, p, &d);
            if let Some(reason) = refused.as_ref().filter(|_| d.media_present || self.tab != Tab::Overview) {
                w::notice(
                    ui,
                    p.muted,
                    icon::LOCK,
                    "This device cannot be modified",
                    std::slice::from_ref(reason),
                    |_| {},
                );
            }
            match self.tab {
                Tab::Overview => self.view_overview(ui, p, ctx, &d),
                Tab::Health => self.view_health(ui, p, ctx, &d),
                Tab::Erase => self.view_erase(ui, p, &d),
                Tab::Image => self.view_image(ui, p, ctx, &d),
            }
            ui.add_space(8.0);
        });
    }

    fn empty_state(&self, ui: &mut egui::Ui, p: Palette) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 2.0 - 90.0).max(20.0));
            w::icon_tile(ui, icon::USB, 88.0, p.accent);
            ui.add_space(10.0);
            ui.label(strong("Plug in a USB stick").size(20.0));
            ui.label(
                RichText::new("It appears here automatically. Nothing happens until you ask for it.").color(p.muted),
            );
        });
    }

    fn job_card(&mut self, ui: &mut egui::Ui, p: Palette, d: &DeviceInfo) {
        let Some(job) = self.jobs.get(&key(d)) else { return };
        let progress = job.progress.lock().map(|x| x.clone()).unwrap_or_default();
        w::card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(18.0).color(p.accent));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(strong(&job.title));
                    ui.label(RichText::new(&progress.phase).small().color(p.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::quiet_button(ui, "Cancel", true).clicked() {
                        job.cancel();
                    }
                    if progress.total > 0 {
                        ui.label(strong(format!("{:.0} %", progress.fraction() * 100.0)).size(20.0));
                    }
                });
            });
            if progress.total > 0 {
                w::progress(ui, p, progress.fraction(), 8.0);
                let eta = progress.eta_secs.map(duration).unwrap_or_else(|| "--".into());
                let mut text = format!(
                    "{} of {}   ·   {}   ·   {eta} left",
                    bytes(progress.done),
                    bytes(progress.total),
                    speed(progress.bytes_per_sec)
                );
                if progress.errors > 0 {
                    text.push_str(&format!("   ·   {} failed", bytes(progress.errors)));
                }
                ui.label(RichText::new(text).small().color(if progress.errors > 0 { p.warn } else { p.muted }));
            }
            if job.destructive {
                ui.label(RichText::new(format!("{}  Do not unplug this device.", icon::WARNING)).small().color(p.warn));
            }
        });
    }

    fn result_card(&mut self, ui: &mut egui::Ui, p: Palette, d: &DeviceInfo) {
        let k = key(d);
        let Some(state) = self.states.get(&k) else { return };
        let Some(s) = state.last.clone() else { return };
        let report = state.last_report.clone();
        let mut dismiss = false;
        w::notice(ui, tone_color(p, s.tone), tone_icon(s.tone), &s.title, &s.lines, |ui| {
            if w::icon_button(ui, p, icon::X, "Dismiss").clicked() {
                dismiss = true;
            }
            if let Some(json) = report
                && ui.button(format!("{}  Save report", icon::DOWNLOAD_SIMPLE)).clicked()
            {
                save_report(&json, d);
            }
        });
        if dismiss && let Some(state) = self.states.get_mut(&k) {
            state.last = None;
        }
    }

    fn view_overview(&mut self, ui: &mut egui::Ui, p: Palette, ctx: &egui::Context, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        if !d.media_present {
            w::notice(
                ui,
                p.bad,
                icon::WARNING_CIRCLE,
                "No media detected",
                &["The controller answers but reports no flash. This is the typical failure of a dead stick."
                    .to_string()],
                |_| {},
            );
            return;
        }
        let state = self.states.get(&key(d));
        if let Some(e) = state.and_then(|s| s.analysis_error.clone()) {
            let mut retry = false;
            w::notice(ui, p.bad, icon::WARNING_CIRCLE, "Analysis failed", &[e], |ui| {
                retry = ui.add_enabled(!busy, egui::Button::new("Try again")).clicked();
            });
            if retry {
                if let Some(s) = self.states.get_mut(&key(d)) {
                    s.analysis_requested = false;
                    s.analysis_error = None;
                }
                ctx.request_repaint();
            }
            return;
        }
        let Some(a) = state.and_then(|s| s.analysis.as_ref()) else {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().color(p.accent));
                ui.label(RichText::new("Analyzing...").color(p.muted));
            });
            return;
        };

        ui.columns(3, |c| {
            let content = match a.content {
                ContentKind::Blank => p.good,
                ContentKind::RandomOrEncrypted => p.accent,
                ContentKind::Data => p.warn,
                ContentKind::Unreadable => p.bad,
            };
            w::stat(&mut c[0], p, icon::FILES, "Content", a.content.label(), content);
            w::stat(&mut c[1], p, icon::STACK, "Filesystem", &a.fs_summary(), p.text);
            w::stat(&mut c[2], p, icon::TABLE, "Partition table", a.table_kind.label(), p.text);
        });

        w::card(ui, p, |ui| {
            w::section(ui, p, "Layout", "");
            let colors = [p.accent, p.good, p.warn, p.bad];
            let mut segments = vec![];
            let mut position = 0u64;
            let mut sorted: Vec<_> = a.partitions.iter().collect();
            sorted.sort_by_key(|x| x.partition.start);
            for (i, part) in sorted.iter().enumerate() {
                let gap = part.partition.start.saturating_sub(position);
                segments.push((gap as f64, p.surface_hover));
                segments.push((part.partition.size as f64, colors[i % colors.len()]));
                position = part.partition.start + part.partition.size;
            }
            if a.partitions.is_empty() {
                let color = if a.whole_device_fs.is_some() { p.accent } else { p.surface_hover };
                segments.push((a.size_bytes as f64, color));
            } else {
                segments.push((a.size_bytes.saturating_sub(position) as f64, p.surface_hover));
            }
            w::segment_bar(ui, p, 18.0, &segments);

            if let Some(f) = &a.whole_device_fs {
                ui.label(format!("{} on the whole device{}", f.name, label_suffix(&f.label)));
            }
            if sorted.is_empty() && a.whole_device_fs.is_none() {
                ui.label(RichText::new("No partitions and no filesystem found.").color(p.muted));
            }
            for (i, part) in sorted.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(3), colors[i % colors.len()]);
                    let name = part.filesystem.as_ref().map_or("Unknown filesystem", |f| f.name.as_str());
                    ui.label(strong(name));
                    if let Some(f) = &part.filesystem {
                        if f.encrypted {
                            w::chip(ui, format!("{} Encrypted", icon::LOCK), p.accent);
                        }
                        if !f.label.is_empty() {
                            ui.label(RichText::new(&f.label).color(p.muted));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(bytes(part.partition.size));
                        ui.label(RichText::new(&part.partition.type_name).small().color(p.muted));
                    });
                });
            }
        });

        w::card(ui, p, |ui| {
            w::section(ui, p, "What is on it", "");
            let s = &a.stats;
            let blank = if p.dark { Color32::from_gray(90) } else { Color32::from_gray(190) };
            w::segment_bar(
                ui,
                p,
                10.0,
                &[
                    (s.data as f64, p.warn),
                    (s.high_entropy as f64, p.accent),
                    (s.blank as f64, blank),
                    (s.unreadable as f64, p.bad),
                ],
            );
            ui.horizontal_wrapped(|ui| {
                w::legend(ui, p, p.warn, &format!("{} data", s.data));
                w::legend(ui, p, p.accent, &format!("{} random or encrypted", s.high_entropy));
                w::legend(ui, p, blank, &format!("{} blank", s.blank));
                w::legend(ui, p, p.bad, &format!("{} unreadable", s.unreadable));
            });
            ui.label(
                RichText::new(format!("Spot check of {} areas spread over the device.", s.samples))
                    .small()
                    .color(p.muted),
            );
        });

        if !a.findings.is_empty() {
            w::card(ui, p, |ui| {
                w::section(ui, p, "Findings", "");
                for f in &a.findings {
                    ui.horizontal_top(|ui| {
                        ui.label(RichText::new(icon::INFO).color(p.muted));
                        ui.add(egui::Label::new(f.as_str()).wrap());
                    });
                }
            });
        }
    }

    fn view_health(&mut self, ui: &mut egui::Ui, p: Palette, ctx: &egui::Context, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        let health = self.states.get(&key(d)).and_then(|s| s.health.clone());

        w::card(ui, p, |ui| {
            match &health {
                Some(h) => {
                    let (color, glyph) = match h.verdict {
                        Verdict::Good => (p.good, icon::CHECK_CIRCLE),
                        Verdict::Warning => (p.warn, icon::WARNING),
                        Verdict::Failing => (p.bad, icon::WARNING_CIRCLE),
                        Verdict::Dead => (p.bad, icon::X_CIRCLE),
                    };
                    ui.horizontal_top(|ui| {
                        w::icon_tile(ui, glyph, 52.0, color);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.label(strong(h.verdict.label()).size(21.0).color(color));
                            for r in &h.reasons {
                                ui.add(egui::Label::new(r.as_str()).wrap());
                            }
                        });
                    });
                }
                None => {
                    w::section(ui, p, "Read test", "Reads the device and measures errors and speed. Changes nothing.")
                }
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let text = if health.is_some() { "Run again" } else { "Run health check" };
                if w::primary_button(ui, p, format!("{}  {text}", icon::HEARTBEAT), !busy && d.media_present).clicked()
                {
                    self.run_health(ctx, d.clone());
                }
                ui.checkbox(&mut self.full_scan, "Read every sector (slow)");
            });
        });

        if let Some(h) = &health {
            if let Some(s) = &h.scan {
                ui.columns(3, |c| {
                    w::stat(&mut c[0], p, icon::GAUGE, "Average read", &speed(s.avg_bytes_per_sec), p.text);
                    w::stat(&mut c[1], p, icon::ARROW_DOWN, "Slowest", &speed(s.min_bytes_per_sec), p.text);
                    let (text, color) = if s.unreadable.is_empty() {
                        ("None".to_string(), p.good)
                    } else {
                        (bytes(s.unreadable.total()), p.bad)
                    };
                    w::stat(&mut c[2], p, icon::WARNING, "Unreadable", &text, color);
                });
                w::card(ui, p, |ui| {
                    w::section(ui, p, "Speed across the device", "");
                    w::profile_chart(ui, p, &s.speed_profile, 90.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Start").small().color(p.muted));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new("End").small().color(p.muted));
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "Peak {}, {} read",
                                        speed(s.max_bytes_per_sec),
                                        bytes(s.bytes_read)
                                    ))
                                    .small()
                                    .color(p.muted),
                                );
                            });
                        });
                    });
                });
            }
            if let Some(SmartOutcome::Available { attributes }) = &h.smart {
                w::card(ui, p, |ui| {
                    w::section(ui, p, "SMART", "");
                    egui::Grid::new("smart").striped(true).num_columns(3).spacing([24.0, 6.0]).show(ui, |ui| {
                        for x in ["Attribute", "Value", "Raw"] {
                            ui.label(RichText::new(x).small().color(p.muted));
                        }
                        ui.end_row();
                        for a in attributes {
                            ui.label(a.name);
                            ui.label(a.current.to_string());
                            ui.label(a.raw.to_string());
                            ui.end_row();
                        }
                    });
                });
            }
            for n in &h.notes {
                ui.label(RichText::new(n).small().color(p.muted));
            }
        }

        w::card(ui, p, |ui| {
            w::section(
                ui,
                p,
                "Capacity test",
                "Writes a pattern and reads it back, to find bad flash and sticks that lie about their size. \
                 Destroys all data.",
            );
            ui.horizontal(|ui| {
                let allowed = refuse_reason(d).is_none();
                if w::quiet_button(ui, format!("{}  Run capacity test", icon::RULER), !busy && allowed).clicked() {
                    self.request(Action::CapacityTest, d.clone());
                }
                ui.checkbox(&mut self.full_capacity, "Test every sector (slow, but authoritative)");
            });
        });
    }

    fn view_erase(&mut self, ui: &mut egui::Ui, p: Palette, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        if refuse_reason(d).is_some() {
            return;
        }

        w::card(ui, p, |ui| {
            w::section(ui, p, "How to erase", "");
            for (mode, description) in [
                (WipeMode::Random, "Recommended. Leaves random data, which reveals nothing."),
                (WipeMode::Zero, "Faster on some sticks. The device reads as blank afterwards."),
                (WipeMode::RandomThenZero, "Takes twice as long, and adds wear to the flash."),
            ] {
                let id = egui::Id::unique(("wipe-mode", mode.label()));
                if w::option_row(ui, p, id, self.wipe_mode == mode, mode.label(), description) {
                    self.wipe_mode = mode;
                }
            }
            ui.label(
                RichText::new(
                    "One pass is enough on flash. No software reaches the controller's spare blocks, so physically \
                     destroy sticks that held highly sensitive data.",
                )
                .small()
                .color(p.muted),
            );
        });

        w::card(ui, p, |ui| {
            w::section(ui, p, "Factory reset", "Erases the device, then creates a fresh partition and filesystem.");
            egui::Grid::new("reset-options").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new("Filesystem").color(p.muted));
                egui::ComboBox::from_id_salt("fs").width(280.0).selected_text(fs_label(self.filesystem)).show_ui(
                    ui,
                    |ui| {
                        for c in [FsChoice::Auto, FsChoice::Fat, FsChoice::Exfat, FsChoice::None] {
                            ui.selectable_value(&mut self.filesystem, c, fs_label(c));
                        }
                    },
                );
                ui.end_row();
                ui.label(RichText::new("Partition table").color(p.muted));
                egui::ComboBox::from_id_salt("table").width(280.0).selected_text(table_label(self.table)).show_ui(
                    ui,
                    |ui| {
                        for c in [TableChoice::Auto, TableChoice::Mbr, TableChoice::Gpt, TableChoice::None] {
                            ui.selectable_value(&mut self.table, c, table_label(c));
                        }
                    },
                );
                ui.end_row();
                ui.label(RichText::new("Label").color(p.muted));
                ui.add(egui::TextEdit::singleline(&mut self.label).desired_width(280.0).hint_text("Optional"));
                ui.end_row();
            });
            ui.checkbox(&mut self.quick_reset, "Quick: only clear the metadata");
            if self.quick_reset {
                ui.label(
                    RichText::new(format!(
                        "{}  Old data stays recoverable. Not for sticks with private data.",
                        icon::WARNING
                    ))
                    .small()
                    .color(p.warn),
                );
            }
            ui.horizontal(|ui| {
                if w::danger_button(ui, p, format!("{}  Factory reset", icon::SPARKLE), !busy).clicked() {
                    self.request(Action::Reset, d.clone());
                }
                if w::quiet_button(ui, format!("{}  Only wipe", icon::ERASER), !busy).clicked() {
                    self.request(Action::Wipe, d.clone());
                }
            });
        });
    }

    fn view_image(&mut self, ui: &mut egui::Ui, p: Palette, ctx: &egui::Context, d: &DeviceInfo) {
        let busy = self.jobs.contains_key(&key(d));
        w::card(ui, p, |ui| {
            w::section(
                ui,
                p,
                "Back up to an image file",
                "Saves the whole device, including filesystems Windows cannot read. Read errors are tolerated and listed.",
            );
            let label = format!("{}  Back up...", icon::DOWNLOAD_SIMPLE);
            if w::primary_button(ui, p, label, !busy && d.media_present).clicked() {
                let name = format!("{}.img", d.display_name().replace(' ', "_"));
                if let Some(path) =
                    rfd::FileDialog::new().set_file_name(name).add_filter("Disk image", &["img"]).save_file()
                {
                    self.run_backup(ctx, d.clone(), path);
                }
            }
        });
        w::card(ui, p, |ui| {
            w::section(
                ui,
                p,
                "Restore an image file",
                "Writes an image to this device and verifies it. Destroys all data.",
            );
            let allowed = refuse_reason(d).is_none();
            if w::quiet_button(ui, format!("{}  Restore...", icon::UPLOAD_SIMPLE), !busy && allowed).clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("Disk image", &["img", "iso", "bin"]).pick_file()
            {
                self.request(Action::Restore(path), d.clone());
            }
        });
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.pending.take() else { return };
        let mut decision = None;
        let word_needed = guard::required_confirmation(&pending.device) == Confirmation::Typed;
        egui::Modal::new(egui::Id::unique("confirm")).frame(modal_frame(ctx)).show(ctx, |ui| {
            let p = palette(ui);
            ui.set_width(440.0);
            ui.horizontal(|ui| {
                w::icon_tile(ui, icon::WARNING, 42.0, p.bad);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(strong(pending.action.description()).size(17.0));
                    ui.label(RichText::new("All data on this device will be lost.").color(p.bad));
                });
            });
            ui.add_space(4.0);
            let d = &pending.device;
            egui::Frame::new().fill(p.surface_hover).corner_radius(10).inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(strong(d.display_name()));
                ui.spacing_mut().interact_size.y = 18.0;
                egui::Grid::new("confirm-device").num_columns(2).spacing([14.0, 3.0]).show(ui, |ui| {
                    let mounts = d.mount_points.join(", ");
                    let serial = if d.serial.is_empty() { "unknown" } else { d.serial.as_str() };
                    let rows = [
                        ("Size", bytes(d.size_bytes)),
                        ("Serial", serial.to_string()),
                        ("Connection", format!("{}, {}", d.bus.label(), d.path)),
                        ("Mounted at", if mounts.is_empty() { "not mounted".to_string() } else { mounts }),
                    ];
                    for (k, v) in rows {
                        ui.label(RichText::new(k).small().color(p.muted));
                        ui.label(RichText::new(v).small());
                        ui.end_row();
                    }
                });
            });
            if word_needed {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "This device reports fixed media or is unusually large. It may be an external disk \
                             rather than a flash stick. Type ERASE to confirm.",
                        )
                        .color(p.warn),
                    )
                    .wrap(),
                );
                ui.add(egui::TextEdit::singleline(&mut pending.typed).desired_width(f32::INFINITY).hint_text("ERASE"));
            }
            ui.add_space(4.0);
            let confirmed = !word_needed || pending.typed == "ERASE";
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if w::danger_button(ui, p, "Erase and continue", confirmed).clicked() {
                    decision = Some(true);
                }
                if w::quiet_button(ui, "Cancel", true).clicked() {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => self.run_action(ctx, pending.action, pending.device),
            Some(false) => {}
            None => self.pending = Some(pending),
        }
    }

    fn activity(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui);
        ui.horizontal(|ui| {
            let caret = if self.show_activity { icon::CARET_DOWN } else { icon::CARET_UP };
            if w::icon_button(ui, p, caret, "Show activity").clicked() {
                self.show_activity = !self.show_activity;
            }
            match self.log.last() {
                Some((tone, line)) => {
                    ui.add(egui::Label::new(RichText::new(line).small().color(tone_color(p, *tone))).truncate());
                }
                None => {
                    ui.label(RichText::new("No activity yet").small().color(p.muted));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("v{VERSION}  ·  GPL-3.0-or-later")).small().color(p.muted));
            });
        });
        if self.show_activity {
            egui::ScrollArea::vertical().max_height(110.0).stick_to_bottom(true).auto_shrink([false, true]).show(
                ui,
                |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for (tone, line) in &self.log {
                        ui.label(
                            RichText::new(format!("{}  {line}", tone_icon(*tone))).small().color(tone_color(p, *tone)),
                        );
                    }
                },
            );
        }
    }
}

fn fs_label(c: FsChoice) -> &'static str {
    match c {
        FsChoice::Auto => "Automatic (FAT up to 32 GiB, else exFAT)",
        FsChoice::Fat => "FAT32 / FAT16",
        FsChoice::Exfat => "exFAT",
        FsChoice::None => "None (blank device)",
    }
}

fn table_label(c: TableChoice) -> &'static str {
    match c {
        TableChoice::Auto => "Automatic (MBR, GPT above 2 TiB)",
        TableChoice::Mbr => "MBR",
        TableChoice::Gpt => "GPT",
        TableChoice::None => "None",
    }
}

fn refuse_reason(d: &DeviceInfo) -> Option<String> {
    // Fixed media can be confirmed in the dialog, so it does not block the buttons.
    match guard::check_destructive(d, Overrides { allow_fixed: true }) {
        Ok(()) => d.read_only.then(|| {
            "It is write protected. Without a lock switch, this usually means the controller locked the flash \
             permanently."
                .to_string()
        }),
        Err(Error::Refused(reason)) => Some(reason),
        Err(e) => Some(e.to_string()),
    }
}

fn label_suffix(label: &str) -> String {
    if label.is_empty() { String::new() } else { format!(", label {label}") }
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
        let p = palette(ui);

        egui::Panel::left("devices")
            .resizable(true)
            .default_size(290.0)
            .size_range(240.0..=420.0)
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(14))
            .show(ui, |ui| self.sidebar(ui, &ctx));
        egui::Panel::bottom("activity")
            .frame(egui::Frame::new().fill(p.background).inner_margin(Margin::symmetric(14, 4)))
            .show(ui, |ui| self.activity(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.background).inner_margin(Margin {
                left: 22,
                right: 22,
                top: 18,
                bottom: 4,
            }))
            .show(ui, |ui| self.details(ui, &ctx));
        self.confirm_dialog(&ctx);

        // Keeps the speed and ETA figures moving even between progress events.
        if !self.jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

fn modal_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::window(&ctx.global_style()).inner_margin(22)
}
