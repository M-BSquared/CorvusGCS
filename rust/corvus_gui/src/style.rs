//! The web interface's design tokens (src/css/themes.css, default light
//! theme) and its type, so a block here is the same size, weight and colour
//! as the element it copies. One CSS pixel is one egui point: the web app is
//! px-absolute at an interface scale of 1, and so is this.

use eframe::egui::{self, text::LayoutJob, Color32, FontFamily, FontId, Galley, TextFormat};
use std::sync::Arc;

/// `#RRGGBB`.
const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// `rgb(r g b / a)`.
pub fn rgba(rgb: u32, a: f32) -> Color32 {
    let c = hex(rgb);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0).round() as u8)
}

pub const BG: Color32 = hex(0xF4F6F8);
pub const SURFACE_1: Color32 = hex(0xFFFFFF);
pub const SURFACE_3: Color32 = hex(0xE8EBF0);
pub const SURFACE_HOVER: Color32 = hex(0xE2E6EC);
pub const ACCENT: Color32 = hex(0x1B1F26);
pub const ACCENT_BRIGHT: Color32 = hex(0x343B45);
pub const ACCENT_DARK: Color32 = hex(0xE2E6EC);
pub const ACCENT_HOVER: Color32 = hex(0x2C323B);
pub const HEALTHY: Color32 = hex(0x0F8B50);
pub const HEALTHY_DARK: Color32 = hex(0xD8F1E3);
pub const HEALTHY_BRIGHT: Color32 = hex(0x0B7343);
pub const WARNING: Color32 = hex(0xB07600);
pub const WARNING_DARK: Color32 = hex(0xFAEDCC);
pub const CRITICAL: Color32 = hex(0xD62B26);
pub const CRITICAL_DARK: Color32 = hex(0xFBDCDB);
pub const NAV: Color32 = hex(0x0B6FC0);
pub const VEHICLE_RGB: u32 = 0xD62B26;
pub const VEHICLE: Color32 = hex(VEHICLE_RGB);
pub const TRACK: Color32 = hex(0xB81E1B);
pub const TRACK_PAST: Color32 = hex(0xD4675F);
pub const TEXT_1: Color32 = hex(0x12151A);
pub const TEXT_2: Color32 = hex(0x4C555F);
pub const TEXT_3: Color32 = hex(0x79828D);
pub const TEXT_INVERTED: Color32 = hex(0xFFFFFF);
pub const BORDER: Color32 = hex(0xD3D8DF);
pub const BORDER_SOFT: Color32 = hex(0xE4E8ED);
pub const BORDER_STRONG: Color32 = hex(0xB6BEC8);
pub const SURFACE_2: Color32 = hex(0xF1F3F6);
pub const TEXT_DISABLED: Color32 = hex(0xA9B1BA);
pub const NAV_BRIGHT: Color32 = hex(0x085A9C);
pub const TERMINAL_CMD: Color32 = hex(0x0F8B50);
pub const TERMINAL_MUTED: Color32 = hex(0x79828D);
pub const TERMINAL_TEXT: Color32 = hex(0x1B1F26);
pub const ADI_SKY: Color32 = hex(0x3AA7D6);
pub const ADI_GROUND: Color32 = hex(0x006310);
pub const ADI_MARK: Color32 = hex(0xF58A2B);

/// `--adi-mark-outline: rgba(12, 16, 22, 0.45)`.
pub fn adi_mark_outline() -> Color32 {
    Color32::from_rgba_unmultiplied(12, 16, 22, 115)
}

/// `rgb(var(--tint-rgb) / a)`: black on the light theme.
pub fn tint(a: f32) -> Color32 {
    rgba(0x000000, a)
}

/// `rgb(var(--glass-rgb) / a)`: white on the light theme.
pub fn glass(a: f32) -> Color32 {
    rgba(0xFFFFFF, a)
}

pub const RADIUS: u8 = 8;
pub const RADIUS_BTN: u8 = 6;

/// `--shadow-pop: 0 4px 16px rgba(22, 28, 38, 0.10)`.
pub fn shadow_pop() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_rgba_unmultiplied(22, 28, 38, 26),
    }
}

/// The `.glass` material. CSS blurs what is under it (`backdrop-filter`),
/// which egui cannot; the fill is raised from 0.6 to stand in for the blur,
/// so text on it reads as it does in the web app.
pub fn glass_fill() -> Color32 {
    glass(0.78)
}

/// `border: 1px solid rgb(var(--tint-rgb) / 0.06)`.
pub fn glass_stroke() -> egui::Stroke {
    egui::Stroke::new(1.0, tint(0.06))
}

/// A font weight the interface uses; each is its own egui family over the
/// same variable font (see `fonts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    W400,
    W500,
    W600,
    W700,
}

impl Weight {
    pub fn value(self) -> f32 {
        match self {
            Weight::W400 => 400.0,
            Weight::W500 => 500.0,
            Weight::W600 => 600.0,
            Weight::W700 => 700.0,
        }
    }

    pub fn family(self) -> FontFamily {
        FontFamily::Name(format!("inter-{}", self.value() as u32).into())
    }
}

/// `font: <weight> <size>px var(--sans)`.
pub fn font(size: f32, weight: Weight) -> FontId {
    FontId::new(size, weight.family())
}

/// `font: <size>px var(--mono)`.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("mono-400".into()))
}

/// One run of text in JetBrains Mono.
pub fn galley_mono(ui: &egui::Ui, text: &str, size: f32, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_string(), mono(size), color)
}

/// One run of text as CSS sets it: size, weight, colour, letter spacing.
pub fn galley(ui: &egui::Ui, text: &str, size: f32, weight: Weight, color: Color32, letter: f32) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: font(size, weight),
            color,
            extra_letter_spacing: letter,
            ..Default::default()
        },
    );
    ui.painter().layout_job(job)
}

/// Applies the palette to egui's own widgets (text fields, popups).
pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE_1;
    visuals.extreme_bg_color = SURFACE_1;
    visuals.selection.bg_fill = rgba(0x1B1F26, 0.10);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.noninteractive.bg_stroke.color = BORDER;
    ctx.set_theme(egui::Theme::Light);
    ctx.set_visuals_of(egui::Theme::Light, visuals);
    ctx.style_mut_of(egui::Theme::Light, |s| {
        s.text_styles.insert(egui::TextStyle::Body, font(12.0, Weight::W400));
        s.text_styles.insert(egui::TextStyle::Button, font(12.0, Weight::W600));
        s.text_styles.insert(egui::TextStyle::Small, font(10.0, Weight::W400));
    });
}
