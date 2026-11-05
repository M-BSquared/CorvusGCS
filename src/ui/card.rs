//! Every bordered box of the app is painted here: a rounded surface with a
//! 1 px border. The fill, the border and the corner radius of each kind live
//! in `Look`, so one change reaches every page.
//!
//! * `Look::Panel`: `.link-card` of the right panel, the opaque raised
//!   surface with the normal border.
//! * `Look::Page`: `.page-card` of the setup pages, the glass the page
//!   shows through, with the faint tint border.
//! * `Look::Inset`: a box inside a card (status, detail), the raised surface
//!   with the soft border and a rounder corner.
//! * `Look::Note`: `.params-desc`, the grey box that explains a card.
//! * `Look::Raised`: a box on a card with the normal border (a table, a
//!   readout).
//! * `Look::Quiet`: the sunken row (`Save to`, a list's box).
//! * `Look::Well`: a recessed value, the look of a text box.
//! * `Look::Console`: a terminal's black.
//! * `Look::Chosen`: a picked item, the accent dim with the accent border.
//! * `Look::Tone`: the tinted box of a message or a banner.
//! * `Look::Custom`: a box whose colours depend on state; keep it for those.
//!
//! A card's height is known only after its content is laid out, so the
//! surface is `reserve`d first (it paints under what comes next) and
//! `fill`ed once the height is there.

use eframe::egui::{self, Color32, Painter, Rect, Shape, Stroke};

use crate::ui::message::Tone;
use crate::ui::style;

/// Which box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Panel,
    Page,
    Inset,
    Note,
    Raised,
    Quiet,
    Well,
    Console,
    Chosen,
    Tone(Tone),
    Custom { fill: Color32, line: Color32, radius: u8 },
}

impl Look {
    fn radius(self) -> u8 {
        match self {
            Look::Panel | Look::Page => style::RADIUS,
            Look::Inset => 10,
            Look::Custom { radius, .. } => radius,
            _ => style::RADIUS_BTN,
        }
    }

    fn colours(self) -> (Color32, Color32) {
        match self {
            Look::Panel => (style::surface_2(), style::border()),
            Look::Page => (style::glass(0.82), style::tint(0.09)),
            Look::Inset | Look::Note | Look::Tone(Tone::Plain) => (style::surface_2(), style::border_soft()),
            Look::Raised => (style::surface_2(), style::border()),
            Look::Quiet => (style::surface_1(), style::border_soft()),
            Look::Well => (style::input(0.6), style::border()),
            Look::Console => (style::terminal_bg(), style::border_soft()),
            Look::Chosen => (style::accent_dim(), style::accent()),
            Look::Tone(Tone::Ok) => (style::healthy_dark(), style::healthy()),
            Look::Tone(Tone::Err) => (style::critical_dark(), style::critical()),
            Look::Tone(Tone::Warn) => (style::warning_dark(), style::warning()),
            Look::Custom { fill, line, .. } => (fill, line),
        }
    }

    /// A box of these colours and this corner.
    pub fn custom(fill: Color32, line: Color32, radius: u8) -> Look {
        Look::Custom { fill, line, radius }
    }
}

/// Paints a box at `rect` now.
pub fn paint(painter: &Painter, rect: Rect, look: Look) {
    let (fill, line) = look.colours();
    painter.rect_filled(rect, look.radius(), fill);
    painter.rect_stroke(rect, look.radius(), Stroke::new(1.0, line), egui::StrokeKind::Inside);
}

/// The surface of a floating window: the pop shadow, the glass at `alpha`
/// and a 1 px border in `line`, 12 px round. Put it in a `reserve`d slot.
pub fn window(rect: Rect, alpha: f32, line: Color32) -> Shape {
    Shape::Vec(vec![
        Shape::Rect(style::shadow_pop().as_shape(rect, 12)),
        Shape::rect_filled(rect, 12, style::glass(alpha)),
        Shape::rect_stroke(rect, 12, Stroke::new(1.0, line), egui::StrokeKind::Inside),
    ])
}

/// A place for a card's surface under the content that follows.
pub fn reserve(painter: &Painter) -> egui::layers::ShapeIdx {
    painter.add(Shape::Noop)
}

/// Paints into a place made by `reserve`.
pub fn fill(painter: &Painter, slot: egui::layers::ShapeIdx, rect: Rect, look: Look) {
    let (fill, line) = look.colours();
    painter.set(slot, Shape::rect_filled(rect, look.radius(), fill));
    painter.rect_stroke(rect, look.radius(), Stroke::new(1.0, line), egui::StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_looks_differ_in_border_or_radius() {
        assert_eq!(Look::Panel.radius(), Look::Page.radius());
        assert_ne!(Look::Panel.colours().1, Look::Page.colours().1);
        assert_ne!(Look::Panel.radius(), Look::Inset.radius());
    }
}
