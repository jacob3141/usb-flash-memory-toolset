// SPDX-License-Identifier: GPL-3.0-or-later
//! Background work: the device monitor and one job thread per running operation.
//! Several sticks can be processed at the same time.

use crate::summary::Summary;
use eframe::egui;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use usbflash_core::analyze::Analysis;
use usbflash_core::health::HealthReport;
use usbflash_core::{CancelFlag, Ctx, DeviceInfo, Progress, Result, ops};

const MONITOR_INTERVAL: Duration = Duration::from_secs(2);

/// Reports the device list whenever it changes. Handles plugging and unplugging.
pub fn start_monitor(ctx: egui::Context) -> Receiver<std::result::Result<Vec<DeviceInfo>, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut last = None;
        loop {
            let current = ops::list_devices().map_err(|e| e.to_string());
            if last.as_ref() != Some(&current) {
                if tx.send(current.clone()).is_err() {
                    return;
                }
                last = Some(current);
                ctx.request_repaint();
            }
            std::thread::sleep(MONITOR_INTERVAL);
        }
    });
    rx
}

/// What a finished job hands back to the UI.
pub struct Outcome {
    pub summary: Summary,
    /// Full machine readable report, for saving to a file.
    pub report_json: String,
    pub analysis: Option<Analysis>,
    pub health: Option<HealthReport>,
    /// The device content changed, so the analysis must be repeated.
    pub modified: bool,
}

impl Outcome {
    pub fn new(summary: Summary, report_json: String) -> Self {
        Self { summary, report_json, analysis: None, health: None, modified: false }
    }
}

pub struct Job {
    pub title: String,
    pub progress: Arc<Mutex<Progress>>,
    pub cancel: CancelFlag,
    pub done: Receiver<Result<Outcome>>,
    pub destructive: bool,
}

impl Job {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Starts `work` on its own thread.
pub fn spawn(
    egui_ctx: &egui::Context,
    title: impl Into<String>,
    destructive: bool,
    work: impl FnOnce(&mut Ctx) -> Result<Outcome> + Send + 'static,
) -> Job {
    let progress = Arc::new(Mutex::new(Progress::default()));
    let cancel = CancelFlag::default();
    let (tx, rx) = channel();
    let sink_progress = progress.clone();
    let sink_ctx = egui_ctx.clone();
    let finish_ctx = egui_ctx.clone();
    let mut ctx = Ctx::new(cancel.clone(), move |p| {
        if let Ok(mut slot) = sink_progress.lock() {
            slot.clone_from(p);
        }
        sink_ctx.request_repaint();
    });
    std::thread::spawn(move || {
        let result = work(&mut ctx);
        let _ = tx.send(result);
        finish_ctx.request_repaint();
    });
    Job { title: title.into(), progress, cancel, done: rx, destructive }
}
