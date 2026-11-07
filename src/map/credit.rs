//! The tile credit as MapLibre's compact attribution draws it
//! (python/src/css/map.css, `.maplibregl-ctrl-attrib`; python/src/js/map.js
//! `ownAttributionToggle`).
//!
//! The web asks MapLibre for `compact: true`, which keeps the credit folded
//! into a round glass disc with Lucide's `info` at every map width (the
//! 640 px rule is only for a control without that option). A click opens it
//! into a pill with the text on one line (the web wraps at `46ch`, here it grows to the text); unfolded, only the icon
//! folds it again, so a link in the text still works. Dragging the map folds
//! it. Hover warms the folded disc towards the accent, the icon alone while
//! open. Only colours animate (160 ms), never the width.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// The rail inset every control sits from the map edge.
const INSET: f32 = 14.0;
/// `.maplibregl-ctrl-attrib-button`: the glyph's box.
const ICON: f32 = 15.0;
/// Folded: padding 4 and a 1 px border around the glyph, a 25 px disc.
const PAD: f32 = 4.0;
const BORDER: f32 = 1.0;
/// Open: 6 px between glyph and text, 9 px right of it.
const GAP: f32 = 6.0;
const PAD_RIGHT: f32 = 9.0;
const SIZE: f32 = 10.0;
const LINE: f32 = 1.4;
const FADE: f32 = 0.16;

/// What is kept of the control between frames.
#[derive(Debug, Default)]
pub struct Credit {
    open: bool,
    /// Height of the pill as last drawn, for what stacks above it.
    height: f32,
}

/// The side of the folded disc.
pub fn disc_size() -> f32 {
    ICON + 2.0 * PAD + 2.0 * BORDER
}

/// The open pill around a text block of `text`.
pub fn open_size(text: Vec2) -> Vec2 {
    Vec2::new(
        2.0 * BORDER + PAD + ICON + GAP + text.x + PAD_RIGHT,
        2.0 * BORDER + 2.0 * PAD + text.y.max(ICON),
    )
}

/// `color-mix(in srgb, a t%, b)`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    b.lerp_to_gamma(a, t.clamp(0.0, 1.0))
}

impl Credit {
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The map was dragged.
    pub fn fold(&mut self) {
        self.open = false;
    }

    /// Height of the control as it was drawn, 25 px folded.
    #[cfg(test)]
    pub fn height(&self) -> f32 {
        if self.height > 0.0 {
            self.height
        } else {
            disc_size()
        }
    }

    /// Draws the control at the bottom left of `rect`, `lift` above the usual
    /// 14 px, and handles its click. Nothing is drawn for an empty `text`.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        text: &str,
        lift: f32,
        key: impl std::hash::Hash + std::fmt::Debug + Copy,
    ) {
        if text.is_empty() {
            self.height = 0.0;
            return;
        }
        // One line: the pill grows to the text and wraps only when the map
        // itself is too narrow for it.
        let max_w = (rect.width() - 2.0 * INSET - (open_size(Vec2::ZERO).x)).max(40.0);
        let text_g = crate::ui::kit::wrapped_weight(ui, text, SIZE, Weight::W400, style::text_1(), max_w, LINE);
        let size = if self.open {
            open_size(text_g.size())
        } else {
            Vec2::splat(disc_size())
        };
        self.height = size.y;
        let pill = Rect::from_min_size(
            Pos2::new(rect.left() + INSET, rect.bottom() - lift - INSET - size.y),
            size,
        );
        let icon = Rect::from_center_size(
            Pos2::new(pill.left() + BORDER + PAD + ICON / 2.0, pill.bottom() - disc_size() / 2.0),
            Vec2::splat(ICON),
        );
        let hit = if self.open { icon.expand(PAD) } else { pill };
        let resp = ui
            .interact(hit, ui.scope_id().with(("credit", key)), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            self.open = !self.open;
        }
        let hover = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("hover"), resp.hovered(), FADE);
        let warm = |t: f32| mix(style::accent(), style::text_1(), 0.55 * t);
        let (fill, text_colour, icon_colour) = if self.open {
            (0.45, style::text_1(), warm(hover))
        } else {
            let base = mix(style::text_2(), warm(1.0), hover);
            (0.38 + 0.12 * hover, base, base)
        };
        let painter = ui.painter_at(rect);
        let radius = egui::CornerRadius::same(255);
        style::glass_panel(&painter, ("credit", key), pill, radius, fill);
        if !self.open && hover > 0.0 {
            painter.rect_stroke(
                pill,
                radius,
                Stroke::new(1.0, style::accent().gamma_multiply(0.25 * hover)),
                egui::StrokeKind::Inside,
            );
        }
        icons::paint(ui, "info", icon, icon_colour);
        if self.open {
            let at = Pos2::new(
                pill.left() + BORDER + PAD + ICON + GAP,
                pill.center().y - text_g.size().y / 2.0,
            );
            painter.galley(at, text_g, text_colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folded_disc_is_the_25_px_circle_the_css_makes() {
        assert_eq!(disc_size(), 25.0);
    }

    #[test]
    fn one_line_of_text_keeps_the_disc_height_and_more_lines_grow_the_pill() {
        let one = open_size(Vec2::new(100.0, 14.0));
        assert_eq!(one.y, 25.0);
        assert_eq!(one.x, 2.0 + 4.0 + 15.0 + 6.0 + 100.0 + 9.0);
        assert_eq!(open_size(Vec2::new(100.0, 42.0)).y, 52.0);
    }

    #[test]
    fn color_mix_runs_from_the_second_colour_to_the_first() {
        let (a, b) = (Color32::from_rgb(0, 100, 200), Color32::from_rgb(200, 100, 0));
        assert_eq!(mix(a, b, 0.0), b);
        assert_eq!(mix(a, b, 1.0), a);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(100, 100, 100));
    }

    #[test]
    fn it_starts_folded_and_a_drag_folds_it() {
        let mut c = Credit::default();
        assert!(!c.is_open());
        assert_eq!(c.height(), 25.0);
        c.open = true;
        c.fold();
        assert!(!c.is_open());
    }
}
