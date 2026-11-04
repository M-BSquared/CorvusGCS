//! The width classes of the web interface (`data-vw` of python/src/js/scale.js, the
//! rules of python/src/css/responsive.css).
//!
//! The web measures the window divided by the interface scale, the width in
//! the pixels everything inside is laid out in. egui's points are those
//! pixels already (the scale is the context's zoom factor), so the content
//! rectangle's width is the number to ask.

use eframe::egui;

/// Which of the breakpoints the window is at or under.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vw {
    pub width: f32,
    pub max1279: bool,
    pub max959: bool,
    pub max900: bool,
    pub max720: bool,
    pub max600: bool,
}

impl Vw {
    /// `BREAKPOINTS`: a token is there when the width is at most its number.
    pub fn at(width: f32) -> Vw {
        Vw {
            width,
            max1279: width <= 1279.0,
            max959: width <= 959.0,
            max900: width <= 900.0,
            max720: width <= 720.0,
            max600: width <= 600.0,
        }
    }

    pub fn of(ctx: &egui::Context) -> Vw {
        Vw::at(ctx.content_rect().width())
    }

    /// `--panel-w`: 380 px, 320 px from 959 down.
    pub fn panel_width(&self) -> f32 {
        if self.max959 {
            320.0
        } else {
            380.0
        }
    }

    /// `--hud-w`: 320 px, 290 under 1280, 270 under 960, 240 under 601.
    pub fn hud_width(&self) -> f32 {
        if self.max600 {
            240.0
        } else if self.max959 {
            270.0
        } else if self.max1279 {
            290.0
        } else {
            320.0
        }
    }

    /// `.top-bar`: 60 px high. The stylesheet says 52 under 601 and the page
    /// measures 60 there, `--top-h` winning, so 60 it is.
    pub fn top_height(&self) -> f32 {
        60.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tokens_turn_over_where_the_media_queries_did() {
        let v = Vw::at(1280.0);
        assert!(!v.max1279 && !v.max959);
        let v = Vw::at(1279.0);
        assert!(v.max1279 && !v.max959 && !v.max720);
        let v = Vw::at(959.0);
        assert!(v.max959 && !v.max900);
        let v = Vw::at(600.0);
        assert!(v.max600 && v.max720 && v.max900 && v.max959 && v.max1279);
    }

    #[test]
    fn the_sizes_follow_the_width() {
        assert_eq!(
            (Vw::at(1440.0).panel_width(), Vw::at(900.0).panel_width()),
            (380.0, 320.0)
        );
        let hud: Vec<f32> = [1440.0, 1200.0, 900.0, 560.0]
            .iter()
            .map(|w| Vw::at(*w).hud_width())
            .collect();
        assert_eq!(hud, [320.0, 290.0, 270.0, 240.0]);
        assert_eq!((Vw::at(601.0).top_height(), Vw::at(600.0).top_height()), (60.0, 60.0));
    }
}
