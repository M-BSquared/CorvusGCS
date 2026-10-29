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

/// One theme's tokens (the `--*` of src/css/themes.css), as `#RRGGBB`.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: u32,
    pub surface_1: u32,
    pub surface_2: u32,
    pub surface_3: u32,
    pub surface_hover: u32,
    pub accent: u32,
    pub accent_dim_alpha: f32,
    pub accent_bright: u32,
    pub accent_dark: u32,
    pub accent_hover: u32,
    pub healthy: u32,
    pub healthy_dark: u32,
    pub healthy_bright: u32,
    pub warning: u32,
    pub warning_dark: u32,
    pub critical: u32,
    pub critical_dark: u32,
    pub critical_bright: u32,
    pub nav: u32,
    pub nav_bright: u32,
    pub nav_dark: u32,
    pub vehicle: u32,
    pub track: u32,
    pub track_past: u32,
    pub text_1: u32,
    pub text_2: u32,
    pub text_3: u32,
    pub text_inverted: u32,
    pub text_disabled: u32,
    pub border: u32,
    pub border_soft: u32,
    pub border_strong: u32,
    pub terminal_bg: u32,
    pub terminal_cmd: u32,
    pub terminal_muted: u32,
    pub terminal_text: u32,
    pub adi_sky: u32,
    pub adi_ground: u32,
    pub adi_mark: u32,
    pub adi_mark_outline: (u8, u8, u8, u8),
    pub glass_rgb: u32,
    pub input_rgb: u32,
    pub tint_rgb: u32,
    pub shadow_pop: (u8, u8, u8, u8),
    /// `--blur-soft`: the Gaussian's CSS px, the saturate amount and the
    /// brightness the dark themes add.
    pub blur_soft: (f32, f32, f32),
}

const LIGHT: Palette = Palette {
    dark: false,
    bg: 0xF4F6F8,
    surface_1: 0xFFFFFF,
    surface_2: 0xF1F3F6,
    surface_3: 0xE8EBF0,
    surface_hover: 0xE2E6EC,
    accent: 0x1B1F26,
    accent_dim_alpha: 0.10,
    accent_bright: 0x343B45,
    accent_dark: 0xE2E6EC,
    accent_hover: 0x2C323B,
    healthy: 0x0F8B50,
    healthy_dark: 0xD8F1E3,
    healthy_bright: 0x0B7343,
    warning: 0xB07600,
    warning_dark: 0xFAEDCC,
    critical: 0xD62B26,
    critical_dark: 0xFBDCDB,
    critical_bright: 0xB32020,
    nav: 0x0B6FC0,
    nav_bright: 0x085A9C,
    nav_dark: 0xD8E9F8,
    vehicle: 0xD62B26,
    track: 0xB81E1B,
    track_past: 0xD4675F,
    text_1: 0x12151A,
    text_2: 0x4C555F,
    text_3: 0x79828D,
    text_inverted: 0xFFFFFF,
    text_disabled: 0xA9B1BA,
    border: 0xD3D8DF,
    border_soft: 0xE4E8ED,
    border_strong: 0xB6BEC8,
    terminal_bg: 0xFFFFFF,
    terminal_cmd: 0x0F8B50,
    terminal_muted: 0x79828D,
    terminal_text: 0x1B1F26,
    adi_sky: 0x3AA7D6,
    adi_ground: 0x006310,
    adi_mark: 0xF58A2B,
    adi_mark_outline: (12, 16, 22, 115),
    glass_rgb: 0xFFFFFF,
    input_rgb: 0xFFFFFF,
    tint_rgb: 0x000000,
    shadow_pop: (22, 28, 38, 26),
    blur_soft: (24.0, 1.8, 1.0),
};

/// `light-orange`: the default's surfaces with a burnt orange accent.
const LIGHT_ORANGE: Palette = Palette {
    accent: 0xCE5E00,
    accent_dim_alpha: 0.14,
    accent_bright: 0xB04E00,
    accent_dark: 0xFCE7D3,
    accent_hover: 0xB85300,
    terminal_cmd: 0xBD5500,
    ..LIGHT
};

/// The dark base, with green's accent family: the reference dark theme.
const GREEN: Palette = Palette {
    dark: true,
    bg: 0x0B0E12,
    surface_1: 0x11161D,
    surface_2: 0x171D25,
    surface_3: 0x1C232C,
    surface_hover: 0x222A34,
    accent: 0x3DA876,
    accent_dim_alpha: 0.16,
    accent_bright: 0x5BC896,
    accent_dark: 0x1A3D2E,
    accent_hover: 0x4FBF8A,
    healthy: 0x45D483,
    healthy_dark: 0x183A2A,
    healthy_bright: 0x63E89A,
    warning: 0xF5C842,
    warning_dark: 0x403716,
    critical: 0xFF514D,
    critical_dark: 0x421D1C,
    critical_bright: 0xFF726E,
    nav: 0x4CC9FF,
    nav_bright: 0x7DDEFF,
    nav_dark: 0x17394A,
    vehicle: 0xFF514D,
    track: 0xE4322F,
    track_past: 0xF0958A,
    text_1: 0xF2F4F7,
    text_2: 0xA5ADB8,
    text_3: 0x69737F,
    text_inverted: 0x0B0E12,
    text_disabled: 0x4A535E,
    border: 0x2A3038,
    border_soft: 0x20262E,
    border_strong: 0x39424D,
    terminal_bg: 0x0A0D10,
    terminal_cmd: 0x5BC896,
    terminal_muted: 0x6F7882,
    terminal_text: 0xD7DCE2,
    adi_sky: 0x4A92D1,
    adi_ground: 0x0E5200,
    adi_mark: 0xF58A2B,
    adi_mark_outline: (0, 0, 0, 128),
    glass_rgb: 0x11161D,
    input_rgb: 0x0B0E12,
    tint_rgb: 0xFFFFFF,
    shadow_pop: (0, 0, 0, 77),
    blur_soft: (24.0, 1.9, 1.1),
};

const BLUE: Palette = Palette {
    accent: 0x3B9EFF,
    accent_bright: 0x6FBCFF,
    accent_dark: 0x14304F,
    accent_hover: 0x55ADFF,
    nav: 0x35D6C4,
    nav_bright: 0x6BE8DA,
    nav_dark: 0x10403C,
    terminal_cmd: 0x6FBCFF,
    ..GREEN
};

const PINK: Palette = Palette {
    accent: 0xF0509B,
    accent_bright: 0xFF7DB8,
    accent_dark: 0x4A1733,
    accent_hover: 0xFF66A9,
    terminal_cmd: 0xFF7DB8,
    ..GREEN
};

const ORANGE: Palette = Palette {
    accent: 0xF58A2B,
    accent_bright: 0xFFAA5C,
    accent_dark: 0x4A2A10,
    accent_hover: 0xFF9C42,
    warning: 0xFFD966,
    warning_dark: 0x46400F,
    terminal_cmd: 0xFFAA5C,
    ..GREEN
};

/// The themes the web interface offers (js/theme.js THEMES).
pub const THEME_IDS: [&str; 6] = ["light", "light-orange", "green", "blue", "pink", "orange"];

/// The palette of a theme id; an unknown id is the default.
pub fn palette_of(id: &str) -> Palette {
    match id {
        "light-orange" => LIGHT_ORANGE,
        "green" => GREEN,
        "blue" => BLUE,
        "pink" => PINK,
        "orange" => ORANGE,
        _ => LIGHT,
    }
}

/// The theme a backend config asks for (Corvus.theme.fromConfig): `theme.name`
/// when it is one of the six, else the legacy `theme.accent` hex matched
/// against the swatches, else the default.
pub fn theme_from_config(cfg: &serde_json::Value) -> &'static str {
    let theme = cfg.get("theme");
    if let Some(name) = theme.and_then(|t| t.get("name")).and_then(|n| n.as_str()) {
        if let Some(id) = THEME_IDS.iter().find(|i| **i == name) {
            return id;
        }
    }
    if let Some(hex) = theme.and_then(|t| t.get("accent")).and_then(|a| a.as_str()) {
        let hex = hex.trim().to_lowercase();
        // The first swatch of each theme, as theme.js lists them.
        let swatches = [
            ("light", "#1b1f26"),
            ("light-orange", "#c2540a"),
            ("green", "#3da876"),
            ("blue", "#3b9eff"),
            ("pink", "#f0509b"),
            ("orange", "#f58a2b"),
        ];
        return swatches.iter().find(|(_, s)| *s == hex).map_or("light", |(id, _)| id);
    }
    "light"
}

static PALETTE: std::sync::RwLock<Palette> = std::sync::RwLock::new(LIGHT);

/// Switches every colour of the interface to `id`.
pub fn set_theme(id: &str) {
    if let Ok(mut p) = PALETTE.write() {
        *p = palette_of(id);
    }
}

/// The palette in force.
pub fn palette() -> Palette {
    PALETTE.read().map_or(LIGHT, |p| *p)
}

macro_rules! color_fns {
    ($($name:ident),* $(,)?) => {
        $(
            #[doc = concat!("`--", stringify!($name), "` of the theme in force.")]
            pub fn $name() -> Color32 {
                hex(PALETTE.read().map_or(LIGHT.$name, |p| p.$name))
            }
        )*
    };
}

color_fns!(
    bg,
    surface_1,
    surface_2,
    surface_3,
    surface_hover,
    accent,
    accent_bright,
    accent_dark,
    accent_hover,
    healthy,
    healthy_dark,
    healthy_bright,
    warning,
    warning_dark,
    critical,
    critical_dark,
    critical_bright,
    nav,
    nav_bright,
    nav_dark,
    vehicle,
    track,
    track_past,
    text_1,
    text_2,
    text_3,
    text_inverted,
    text_disabled,
    border,
    border_soft,
    border_strong,
    terminal_bg,
    terminal_cmd,
    terminal_muted,
    terminal_text,
    adi_sky,
    adi_ground,
    adi_mark,
);

/// `--vehicle-rgb`, for the translucent washes around the marker.
pub fn vehicle_rgb() -> u32 {
    palette().vehicle
}

/// `--accent-dim`: the accent at its focus-ring alpha.
pub fn accent_dim() -> Color32 {
    let p = palette();
    rgba(p.accent, p.accent_dim_alpha)
}

/// The accent at `a`.
pub fn accent_a(a: f32) -> Color32 {
    rgba(palette().accent, a)
}

/// `--adi-mark-outline`.
pub fn adi_mark_outline() -> Color32 {
    let (r, g, b, a) = palette().adi_mark_outline;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// `rgb(var(--tint-rgb) / a)`: black on the light themes, white on dark.
pub fn tint(a: f32) -> Color32 {
    rgba(palette().tint_rgb, a)
}

/// `rgb(var(--glass-rgb) / a)`: the floating-surface material.
pub fn glass(a: f32) -> Color32 {
    rgba(palette().glass_rgb, a)
}

/// `rgb(var(--input-rgb) / a)`: recessed field wells.
pub fn input(a: f32) -> Color32 {
    rgba(palette().input_rgb, a)
}

/// Whether the theme in force is a dark one.
pub fn is_dark() -> bool {
    palette().dark
}

pub const RADIUS: u8 = 8;
pub const RADIUS_BTN: u8 = 6;

/// `--shadow-pop`: `0 4px 16px` in the theme's shadow colour.
pub fn shadow_pop() -> egui::epaint::Shadow {
    let (r, g, b, a) = palette().shadow_pop;
    egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_rgba_unmultiplied(r, g, b, a),
    }
}

/// A `.glass` surface: its shadow, the frosted backdrop (`--blur-soft`,
/// see `frost`), the translucent glass colour at `fill` (0.6 for `.glass`,
/// 0.55 for the map rail, 0.38 for the attribution pill) and the hairline.
/// `key` names the panel, so its backdrop has a texture of its own.
pub fn glass_panel(
    painter: &egui::Painter,
    key: impl std::hash::Hash,
    rect: egui::Rect,
    radius: impl Into<egui::CornerRadius>,
    fill: f32,
) {
    use std::hash::{BuildHasher, BuildHasherDefault};
    let radius = radius.into();
    let id = BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default().hash_one(key);
    // The backdrop's edge is antialiased, so its last pixel row shows the raw
    // map through the fill, a bright hairline on a dark theme. It reaches a
    // pixel past the panel so the rim is covered as the interior is.
    let reach = rect.expand(1.0);
    let wide = egui::CornerRadius {
        nw: radius.nw.saturating_add(1),
        ne: radius.ne.saturating_add(1),
        sw: radius.sw.saturating_add(1),
        se: radius.se.saturating_add(1),
    };
    crate::ui::frost::copy_backdrop(painter, id, reach, crate::ui::frost::soft());
    painter.add(shadow_pop().as_shape(rect, radius));
    crate::ui::frost::draw_backdrop(painter, id, reach, wide, crate::ui::frost::soft());
    painter.rect_filled(rect, radius, glass(fill));
    painter.rect_stroke(rect, radius, glass_stroke(), egui::StrokeKind::Inside);
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

/// One run of text in JetBrains Mono with letter spacing.
pub fn galley_mono_letter(ui: &egui::Ui, text: &str, size: f32, color: Color32, letter: f32) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: mono(size),
            color,
            extra_letter_spacing: letter,
            ..Default::default()
        },
    );
    ui.painter().layout_job(job)
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
    let (theme, mut visuals) = if is_dark() {
        (egui::Theme::Dark, egui::Visuals::dark())
    } else {
        (egui::Theme::Light, egui::Visuals::light())
    };
    visuals.panel_fill = bg();
    visuals.window_fill = surface_1();
    visuals.extreme_bg_color = surface_1();
    visuals.selection.bg_fill = accent_dim();
    visuals.selection.stroke = egui::Stroke::new(1.0, accent());
    visuals.widgets.noninteractive.bg_stroke.color = border();
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, visuals);
    ctx.style_mut_of(theme, |s| {
        s.text_styles.insert(egui::TextStyle::Body, font(12.0, Weight::W400));
        s.text_styles.insert(egui::TextStyle::Button, font(12.0, Weight::W600));
        s.text_styles.insert(egui::TextStyle::Small, font(10.0, Weight::W400));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_config_names_the_theme() {
        assert_eq!(theme_from_config(&json!({"theme": {"name": "pink"}})), "pink");
        assert_eq!(theme_from_config(&json!({"theme": {"name": "plaid"}})), "light");
        assert_eq!(theme_from_config(&json!({"theme": {"accent": " #3B9EFF "}})), "blue");
        assert_eq!(theme_from_config(&json!({"theme": {"accent": "#123456"}})), "light");
        assert_eq!(theme_from_config(&json!({})), "light");
    }

    #[test]
    fn every_theme_is_what_themes_css_says() {
        assert!(!palette_of("light").dark && !palette_of("light-orange").dark);
        for id in ["green", "blue", "pink", "orange"] {
            let p = palette_of(id);
            assert!(p.dark, "{id}");
            assert_eq!(p.bg, 0x0B0E12, "{id} shares the dark base");
            assert_eq!(p.glass_rgb, 0x11161D);
        }
        assert_eq!(palette_of("light-orange").accent, 0xCE5E00);
        assert_eq!(palette_of("light-orange").bg, palette_of("light").bg);
        assert_eq!(palette_of("blue").nav, 0x35D6C4, "blue moves navigation to teal");
        assert_eq!(palette_of("orange").warning, 0xFFD966);
        assert_eq!(palette_of("green").accent, 0x3DA876);
        assert_eq!(palette_of("unknown").accent, 0x1B1F26);
        assert_eq!(THEME_IDS.len(), 6);
    }
}
