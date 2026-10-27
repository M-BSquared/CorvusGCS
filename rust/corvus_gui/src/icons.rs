//! The Lucide icons the web interface draws, cut out of its own
//! `src/vendor/lucide.min.js` at build time and tinted here like
//! `currentColor` tints them in CSS.

use eframe::egui::{self, Color32, Rect};

mod svg {
    include!(concat!(env!("OUT_DIR"), "/icons.rs"));
}

/// One icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Power,
    ArrowUp,
    ArrowDown,
    Home,
    Route,
    X,
    ChevronDown,
}

impl Icon {
    fn source(self) -> (&'static str, &'static [u8]) {
        match self {
            Icon::Power => ("bytes://lucide/power.svg", svg::POWER),
            Icon::ArrowUp => ("bytes://lucide/arrow-up.svg", svg::ARROW_UP),
            Icon::ArrowDown => ("bytes://lucide/arrow-down.svg", svg::ARROW_DOWN),
            Icon::Home => ("bytes://lucide/home.svg", svg::HOME),
            Icon::Route => ("bytes://lucide/route.svg", svg::ROUTE),
            Icon::X => ("bytes://lucide/x.svg", svg::X),
            Icon::ChevronDown => ("bytes://lucide/chevron-down.svg", svg::CHEVRON_DOWN),
        }
    }
}

/// Paints `icon` into `rect` in `color`.
pub fn paint(ui: &egui::Ui, icon: Icon, rect: Rect, color: Color32) {
    let (uri, bytes) = icon.source();
    egui::Image::from_bytes(uri, bytes).tint(color).paint_at(ui, rect);
}
