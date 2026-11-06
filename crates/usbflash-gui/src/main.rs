// SPDX-License-Identifier: GPL-3.0-or-later
//! Graphical interface of the USB Flash Memory Toolset.

// No console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod jobs;
mod summary;

fn main() -> eframe::Result {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("USB Flash Memory Toolset")
        .with_inner_size([1040.0, 700.0])
        .with_min_inner_size([760.0, 520.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("usb-flash-memory-toolset", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
