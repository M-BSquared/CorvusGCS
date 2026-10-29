//! The Lucide icons the web interface draws, cut out of its own
//! `src/vendor/lucide.min.js` at build time and tinted here like
//! `currentColor` tints them in CSS. Named as the web names them
//! (`data-lucide="house"`).

use eframe::egui::{self, Color32, Rect};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/icons.rs"));
}

/// Paints the icon `name` into `rect` in `color`; an unknown name paints
/// nothing (build.rs fails the build for a name it cannot find).
pub fn paint(ui: &egui::Ui, name: &str, rect: Rect, color: Color32) {
    if let Some(bytes) = generated::svg(name) {
        egui::Image::from_bytes(format!("bytes://lucide/{name}.svg"), bytes)
            .tint(color)
            .paint_at(ui, rect);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_icon_the_interface_uses_is_there() {
        for name in [
            "power",
            "house",
            "chevron-right",
            "plug",
            "refresh-cw",
            "crosshair",
            "puzzle",
            "server",
        ] {
            assert!(super::generated::svg(name).is_some(), "{name}");
        }
        assert!(super::generated::svg("no-such-icon").is_none());
    }
}
