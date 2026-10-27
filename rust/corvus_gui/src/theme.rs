//! Colours, taken from the web interface's default light theme
//! (src/css/themes.css), so the two read alike.

use eframe::egui::{self, Color32};

/// What a value is saying, which picks its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Accent,
    Healthy,
    Warning,
    Critical,
    Armed,
    Off,
}

/// The palette.
#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color32,
    pub surface: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub healthy: Color32,
    pub warning: Color32,
    pub notice: Color32,
    pub critical: Color32,
    pub border: Color32,
    pub disabled: Color32,
}

impl Theme {
    /// The default light theme.
    pub fn light() -> Theme {
        Theme {
            bg: Color32::from_rgb(0xF4, 0xF6, 0xF8),
            surface: Color32::WHITE,
            text: Color32::from_rgb(0x1B, 0x1F, 0x26),
            text_muted: Color32::from_rgb(0x79, 0x82, 0x8D),
            healthy: Color32::from_rgb(0x0F, 0x8B, 0x50),
            warning: Color32::from_rgb(0xB0, 0x76, 0x00),
            notice: Color32::from_rgb(0xCE, 0x5E, 0x00),
            critical: Color32::from_rgb(0xD6, 0x2B, 0x26),
            border: Color32::from_rgb(0xD3, 0xD8, 0xDF),
            disabled: Color32::from_rgb(0xA9, 0xB1, 0xBA),
        }
    }

    /// The colour of a tone.
    pub fn tone(&self, tone: Tone) -> Color32 {
        match tone {
            Tone::Plain | Tone::Accent => self.text,
            Tone::Healthy => self.healthy,
            Tone::Warning => self.warning,
            Tone::Critical => self.critical,
            Tone::Armed => self.notice,
            Tone::Off => self.disabled,
        }
    }

    /// Applies the palette to egui's own widgets.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = self.bg;
        visuals.window_fill = self.surface;
        visuals.extreme_bg_color = self.surface;
        visuals.widgets.noninteractive.bg_stroke.color = self.border;
        ctx.set_visuals(visuals);
    }
}
