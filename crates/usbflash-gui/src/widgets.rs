// SPDX-License-Identifier: GPL-3.0-or-later
//! Reusable building blocks of the interface.

use crate::theme::{Palette, SEMIBOLD};
use eframe::egui::epaint::RectShape;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, Frame, Margin, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, pos2, vec2,
};

/// Text in the semibold font.
pub fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).family(FontFamily::Name(SEMIBOLD.into()))
}

/// A bordered surface holding one topic. Takes the full available width.
pub fn card<R>(ui: &mut Ui, p: Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(12)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A card that reacts to the pointer, with an accent outline when selected.
pub fn selectable_card(ui: &mut Ui, p: Palette, id: egui::Id, selected: bool, add: impl FnOnce(&mut Ui)) -> Response {
    let background = ui.painter().add(Shape::Noop);
    let frame = Frame::new().inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    let response = ui.interact(frame.response.rect, id, Sense::click());
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), response.hovered(), 0.1);
    let fill = if selected {
        p.accent.gamma_multiply(if p.dark { 0.16 } else { 0.09 })
    } else {
        p.surface.lerp_to_gamma(p.surface_hover, hover)
    };
    let stroke = Stroke::new(if selected { 1.5 } else { 1.0 }, if selected { p.accent } else { p.border });
    ui.painter()
        .set(background, RectShape::new(frame.response.rect, CornerRadius::same(10), fill, stroke, StrokeKind::Inside));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A heading with an optional explanation below.
pub fn section(ui: &mut Ui, p: Palette, title: &str, description: &str) {
    ui.label(strong(title).size(15.0));
    if !description.is_empty() {
        ui.label(RichText::new(description).color(p.muted));
    }
}

/// A square tile with a centered icon on a tinted background.
pub fn icon_tile(ui: &mut Ui, icon: &str, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same((size * 0.25) as u8), color.gamma_multiply(0.16));
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(size * 0.58), color);
}

/// A small rounded label, for states and properties.
pub fn chip(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    Frame::new().fill(color.gamma_multiply(0.14)).corner_radius(99).inner_margin(Margin::symmetric(9, 3)).show(
        ui,
        |ui| {
            ui.label(RichText::new(text.into()).small().color(color));
        },
    );
}

fn filled_button(ui: &mut Ui, text: String, fill: Color32, enabled: bool, height: f32) -> Response {
    let button = egui::Button::new(strong(text).color(Color32::WHITE))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(8)
        .min_size(vec2(0.0, height));
    ui.add_enabled(enabled, button)
}

pub fn primary_button(ui: &mut Ui, p: Palette, text: impl Into<String>, enabled: bool) -> Response {
    filled_button(ui, text.into(), p.accent, enabled, 36.0)
}

pub fn danger_button(ui: &mut Ui, p: Palette, text: impl Into<String>, enabled: bool) -> Response {
    filled_button(ui, text.into(), p.bad, enabled, 36.0)
}

pub fn quiet_button(ui: &mut Ui, text: impl Into<String>, enabled: bool) -> Response {
    ui.add_enabled(enabled, egui::Button::new(text.into()).min_size(vec2(0.0, 36.0)))
}

/// A small icon-only button without a background until hovered.
pub fn icon_button(ui: &mut Ui, p: Palette, icon: &str, tooltip: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(icon).size(16.0).color(p.muted)).frame(false).min_size(Vec2::splat(28.0)))
        .on_hover_text(tooltip)
}

/// A value with its caption, as a tile of equal size next to others.
pub fn stat(ui: &mut Ui, p: Palette, icon: &str, caption: &str, value: &str, color: Color32) {
    card(ui, p, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon).color(p.muted));
            ui.label(RichText::new(caption).small().color(p.muted));
        });
        ui.add(egui::Label::new(strong(value).size(17.0).color(color)).truncate());
    });
}

/// A colored notice with an icon, a title and details.
pub fn notice(ui: &mut Ui, color: Color32, icon: &str, title: &str, lines: &[String], extra: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(color.gamma_multiply(0.11))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(12)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(icon).size(20.0).color(color));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(strong(title).color(color));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), extra);
                    });
                    for line in lines {
                        ui.add(egui::Label::new(line.as_str()).wrap());
                    }
                });
            });
        });
}

/// A selectable row with a radio mark, a title and a description.
pub fn option_row(ui: &mut Ui, p: Palette, id: egui::Id, selected: bool, title: &str, description: &str) -> bool {
    selectable_card(ui, p, id, selected, |ui| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
            let color = if selected { p.accent } else { p.muted };
            ui.painter().circle_stroke(rect.center(), 8.0, Stroke::new(1.5, color));
            if selected {
                ui.painter().circle_filled(rect.center(), 4.5, color);
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(strong(title));
                ui.label(RichText::new(description).small().color(p.muted));
            });
        });
    })
    .clicked()
}

/// One horizontal bar made of proportional segments.
pub fn segment_bar(ui: &mut Ui, p: Palette, height: f32, segments: &[(f64, Color32)]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same((height / 3.0) as u8);
    painter.rect_filled(rect, radius, p.surface_hover);
    let total: f64 = segments.iter().map(|s| s.0).sum();
    if total <= 0.0 {
        return;
    }
    let mut x = rect.left();
    let last = segments.iter().rposition(|s| s.0 > 0.0).unwrap_or(0);
    for (i, &(value, color)) in segments.iter().enumerate() {
        if value <= 0.0 {
            continue;
        }
        let w = (rect.width() as f64 * value / total) as f32;
        let right = if i == last { rect.right() } else { x + w };
        let r = Rect::from_min_max(pos2(x, rect.top()), pos2(right, rect.bottom()));
        let corners = CornerRadius {
            nw: if x <= rect.left() { radius.nw } else { 0 },
            sw: if x <= rect.left() { radius.sw } else { 0 },
            ne: if i == last { radius.ne } else { 0 },
            se: if i == last { radius.se } else { 0 },
        };
        painter.rect_filled(r, corners, color);
        if i != last {
            painter.line_segment([pos2(right, rect.top()), pos2(right, rect.bottom())], Stroke::new(2.0, p.surface));
        }
        x = right;
    }
}

/// A color dot with a label, for legends.
pub fn legend(ui: &mut Ui, p: Palette, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(RichText::new(text).small().color(p.muted));
    });
}

/// Bars showing a value across the device, e.g. read speed by position.
pub fn profile_chart(ui: &mut Ui, p: Palette, values: &[f32], height: f32) {
    let max = values.iter().copied().fold(0.0, f32::max);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(8), p.surface_hover);
    if max <= 0.0 || values.is_empty() {
        return;
    }
    for f in [0.25, 0.5, 0.75] {
        let y = rect.bottom() - rect.height() * f;
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, p.border));
    }
    let step = rect.width() / values.len() as f32;
    let gap = if step > 3.0 { 1.0 } else { 0.0 };
    for (i, &v) in values.iter().enumerate() {
        if v <= 0.0 {
            continue;
        }
        let h = (rect.height() - 8.0) * v / max;
        let x = rect.left() + i as f32 * step;
        let bar = Rect::from_min_max(pos2(x, rect.bottom() - h), pos2(x + step - gap, rect.bottom()));
        painter.rect_filled(bar, CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 }, p.accent.gamma_multiply(0.85));
    }
}

/// Underlined navigation between the views of a device.
pub fn nav_item(ui: &mut Ui, p: Palette, id: egui::Id, active: bool, icon: &str, text: &str) -> bool {
    let color = if active { p.accent } else { p.muted };
    let label = format!("{icon}  {text}");
    let galley = ui.painter().layout_no_wrap(
        label,
        egui::FontId::new(14.0, FontFamily::Name(SEMIBOLD.into())),
        Color32::PLACEHOLDER,
    );
    let size = vec2(galley.size().x + 20.0, 38.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let text_color = if response.hovered() && !active { p.text } else { color };
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, text_color);
    let line = ui.ctx().animate_bool_with_time(id.with("nav"), active, 0.12);
    if line > 0.0 {
        let half = rect.width() / 2.0 * line;
        let y = rect.bottom() - 1.0;
        ui.painter().line_segment(
            [pos2(rect.center().x - half, y), pos2(rect.center().x + half, y)],
            Stroke::new(2.5, p.accent),
        );
    }
    response.clicked()
}

/// A rounded progress bar in the accent color.
pub fn progress(ui: &mut Ui, p: Palette, fraction: f32, height: f32) {
    segment_bar(
        ui,
        p,
        height,
        &[(fraction.clamp(0.0, 1.0) as f64, p.accent), (1.0 - fraction.clamp(0.0, 1.0) as f64, p.surface_hover)],
    );
}
