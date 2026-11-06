// SPDX-License-Identifier: GPL-3.0-or-later
//! Colors, typography and global widget styling, for light and dark mode.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke, TextStyle, Theme,
    ThemePreference, Vec2, Visuals,
};
use egui_phosphor::Variant;
use std::sync::Arc;

/// Named font family for headings and emphasis.
pub const SEMIBOLD: &str = "semibold";

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub background: Color32,
    pub sidebar: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

const DARK: Palette = Palette {
    dark: true,
    background: Color32::from_rgb(21, 23, 28),
    sidebar: Color32::from_rgb(16, 18, 22),
    surface: Color32::from_rgb(30, 33, 39),
    surface_hover: Color32::from_rgb(38, 42, 50),
    border: Color32::from_rgb(46, 50, 58),
    text: Color32::from_rgb(232, 234, 237),
    muted: Color32::from_rgb(148, 155, 167),
    accent: Color32::from_rgb(92, 150, 247),
    good: Color32::from_rgb(63, 185, 132),
    warn: Color32::from_rgb(229, 168, 59),
    bad: Color32::from_rgb(239, 91, 91),
};

const LIGHT: Palette = Palette {
    dark: false,
    background: Color32::from_rgb(245, 246, 248),
    sidebar: Color32::from_rgb(235, 237, 241),
    surface: Color32::from_rgb(255, 255, 255),
    surface_hover: Color32::from_rgb(241, 243, 247),
    border: Color32::from_rgb(221, 224, 230),
    text: Color32::from_rgb(27, 30, 36),
    muted: Color32::from_rgb(98, 107, 122),
    accent: Color32::from_rgb(37, 99, 235),
    good: Color32::from_rgb(18, 133, 86),
    warn: Color32::from_rgb(170, 110, 20),
    bad: Color32::from_rgb(211, 47, 47),
};

/// The palette for the mode the given UI is drawn in.
pub fn palette(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

/// Installs fonts and styles. The mode follows the system unless changed.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    for (theme, p) in [(Theme::Dark, DARK), (Theme::Light, LIGHT)] {
        let mut style = (*ctx.style_of(theme)).clone();
        style.visuals = visuals(theme, p);
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(14.0, 7.0);
        style.spacing.interact_size.y = 24.0;
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
            (TextStyle::Heading, FontId::new(22.0, FontFamily::Name(SEMIBOLD.into()))),
        ]
        .into();
        ctx.set_style_of(theme, style);
    }
    ctx.set_theme(ThemePreference::System);
}

fn visuals(theme: Theme, p: Palette) -> Visuals {
    let mut v = if theme == Theme::Dark { Visuals::dark() } else { Visuals::light() };
    let radius = CornerRadius::same(8);
    v.panel_fill = p.background;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    let shadow_alpha = if p.dark { 110 } else { 40 };
    let shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.extreme_bg_color = if p.dark { p.sidebar } else { p.surface_hover };
    v.faint_bg_color = p.surface_hover;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);

    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    for (state, fill) in [
        (&mut w.inactive, p.surface_hover),
        (&mut w.hovered, p.border),
        (&mut w.active, p.border),
        (&mut w.open, p.surface_hover),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.bg_stroke = Stroke::new(1.0, p.border);
        state.expansion = 0.0;
    }
    v
}

fn system_font(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// Prefers the platform's interface font and falls back to the embedded default.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let regular = system_font(&[
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/google-noto-vf/NotoSans[wght].ttf",
    ]);
    let semibold = system_font(&[
        "C:\\Windows\\Fonts\\seguisb.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    ]);
    if let Some(data) = regular {
        fonts.font_data.insert("ui".into(), Arc::new(FontData::from_owned(data)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    egui_phosphor::add_to_fonts(&mut fonts, Variant::Regular);

    let mut heading = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(data) = semibold {
        fonts.font_data.insert(SEMIBOLD.into(), Arc::new(FontData::from_owned(data)));
        heading.insert(0, SEMIBOLD.into());
    }
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), heading);
    ctx.set_fonts(fonts);
}
