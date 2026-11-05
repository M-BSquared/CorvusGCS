//! Text helpers shared by every widget: wrapped paragraphs and the status dot.

use std::sync::Arc;

use eframe::egui::{self, Color32, Galley, Pos2};

use crate::ui::style::{self, Weight};

/// Text wrapped to `width`, for hints and notes.
pub fn wrapped(ui: &egui::Ui, text: &str, size: f32, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: style::font(size, Weight::W400),
            color,
            line_height: Some(size * 1.4),
            ..Default::default()
        },
    );
    job.wrap.max_width = width;
    ui.painter().layout_job(job)
}

/// Text wrapped to `width` in a given weight and line height (a factor of
/// the size, as CSS `line-height`).
pub fn wrapped_weight(
    ui: &egui::Ui,
    text: &str,
    size: f32,
    weight: Weight,
    color: Color32,
    width: f32,
    line: f32,
) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: style::font(size, weight),
            color,
            line_height: Some(size * line),
            ..Default::default()
        },
    );
    job.wrap.max_width = width;
    ui.painter().layout_job(job)
}

/// A dot as `.link-status-dot`: 7 px, a halo when lit, a ring when off.
pub fn dot(painter: &egui::Painter, c: Pos2, color: Color32, lit: bool) {
    let ring = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 51);
    painter.circle_filled(c, 3.5 + 2.0, ring);
    if lit {
        painter.circle_filled(
            c,
            3.5 + 3.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24),
        );
    }
    painter.circle_filled(c, 3.5, color);
}
