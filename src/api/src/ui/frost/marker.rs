//! The frosted glass inside a plugin: a paint callback that only says where
//! and how, for the program to run on its own renderer.

use std::sync::Arc;

use egui::{self, CornerRadius, Rect};

use super::Filter;

/// One half of a backdrop filter, as it stands in a plugin's paint list.
#[derive(Debug, Clone, Copy)]
pub struct Marker {
    pub id: u64,
    /// False for the copy of what lies underneath, true for drawing it back.
    pub draw: bool,
    pub radius: CornerRadius,
    pub filter: Filter,
}

fn shape(rect: Rect, marker: Marker) -> egui::Shape {
    egui::Shape::Callback(egui::PaintCallback {
        rect,
        callback: Arc::new(marker),
    })
}

/// The first half of a backdrop filter: copies what lies under `rect`.
pub fn copy_shape(rect: Rect, id: u64, filter: Filter) -> egui::Shape {
    shape(
        rect,
        Marker {
            id,
            draw: false,
            radius: CornerRadius::ZERO,
            filter,
        },
    )
}

/// The second half: the copy, filtered, inside `rect`'s rounded corners.
pub fn draw_shape(rect: Rect, id: u64, radius: CornerRadius, filter: Filter) -> egui::Shape {
    shape(
        rect,
        Marker {
            id,
            draw: true,
            radius,
            filter,
        },
    )
}

/// [`copy_shape`] painted now.
pub fn copy_backdrop(painter: &egui::Painter, id: u64, rect: Rect, filter: Filter) {
    painter.add(copy_shape(rect, id, filter));
}

/// [`draw_shape`] painted now.
pub fn draw_backdrop(painter: &egui::Painter, id: u64, rect: Rect, radius: CornerRadius, filter: Filter) {
    painter.add(draw_shape(rect, id, radius, filter));
}
