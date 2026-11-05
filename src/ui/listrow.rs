//! The background of a row in a list: nothing at rest, the hover wash under
//! the pointer, the navigation tint on the chosen one.

use eframe::egui::{Painter, Rect};

use crate::ui::style;

/// A row's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Rest,
    Hover,
    Selected,
}

impl Mark {
    /// The chosen row wins over hover, as it does in the web lists.
    pub fn of(selected: bool, hovered: bool) -> Mark {
        if selected {
            Mark::Selected
        } else if hovered {
            Mark::Hover
        } else {
            Mark::Rest
        }
    }
}

/// Paints the row's background; `radius` is the row's corner.
pub fn paint(painter: &Painter, rect: Rect, radius: u8, mark: Mark) {
    match mark {
        Mark::Rest => {}
        Mark::Hover => {
            painter.rect_filled(rect, radius, style::surface_hover());
        }
        Mark::Selected => {
            painter.rect_filled(rect, radius, style::nav().gamma_multiply(0.16));
        }
    }
}
