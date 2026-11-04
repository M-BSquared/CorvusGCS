//! The cards of the web interface: a rounded surface with a 1 px border that
//! groups what belongs together. Every card in the app is painted here, so
//! the fill, the border and the corner radius change in one place.
//!
//! * `Look::Panel`: `.link-card` of the right panel, the opaque raised
//!   surface with the normal border.
//! * `Look::Page`: `.page-card` of the setup pages, the glass the page
//!   shows through, with the faint tint border.
//! * `Look::Inset`: a box inside a card (status, detail), the raised surface
//!   with the soft border and a rounder corner.
//!
//! A card's height is known only after its content is laid out, so the
//! surface is `reserve`d first (it paints under what comes next) and
//! `fill`ed once the height is there.

use eframe::egui::{self, Painter, Rect, Shape, Stroke};

use crate::ui::style;

/// Which card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Panel,
    Page,
    Inset,
}

impl Look {
    fn radius(self) -> u8 {
        match self {
            Look::Panel | Look::Page => style::RADIUS,
            Look::Inset => 10,
        }
    }

    fn colours(self) -> (egui::Color32, egui::Color32) {
        match self {
            Look::Panel => (style::surface_2(), style::border()),
            Look::Page => (style::glass(0.82), style::tint(0.09)),
            Look::Inset => (style::surface_2(), style::border_soft()),
        }
    }
}

/// Paints a card at `rect` now.
pub fn paint(painter: &Painter, rect: Rect, look: Look) {
    let (fill, line) = look.colours();
    painter.rect_filled(rect, look.radius(), fill);
    painter.rect_stroke(rect, look.radius(), Stroke::new(1.0, line), egui::StrokeKind::Inside);
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
    fn the_three_looks_differ_in_border_or_radius() {
        assert_eq!(Look::Panel.radius(), Look::Page.radius());
        assert_ne!(Look::Panel.colours().1, Look::Page.colours().1);
        assert_ne!(Look::Panel.radius(), Look::Inset.radius());
    }
}
