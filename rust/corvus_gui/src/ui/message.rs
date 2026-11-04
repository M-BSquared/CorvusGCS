//! `Corvus.ui.message` (`.ui-msg`): a line of feedback under a form, in a
//! tinted box with a dot before the first line.

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};

use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// How a message is dressed: `ok`, `err` or `warn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ok,
    Err,
    Warn,
}

/// The message at `x, y`, `w` wide; returns its bottom.
pub fn show(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str, kind: Kind) -> f32 {
    let (fg, bg) = match kind {
        Kind::Ok => (style::healthy(), style::healthy_dark()),
        Kind::Err => (style::critical(), style::critical_dark()),
        Kind::Warn => (style::warning(), style::warning_dark()),
    };
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, fg, w - 25.0 - 10.0 - 2.0, 1.4);
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 16.0 + 2.0));
    let p = ui.painter();
    p.rect_filled(rect, style::RADIUS_BTN, bg);
    p.rect_stroke(rect, style::RADIUS_BTN, Stroke::new(1.0, fg), egui::StrokeKind::Inside);
    let line = 11.5 * 1.4;
    p.circle_filled(Pos2::new(x + 1.0 + 10.0 + 3.5, y + 1.0 + 8.0 + line / 2.0), 3.5, fg);
    p.galley(Pos2::new(x + 1.0 + 25.0, y + 1.0 + 8.0), g, fg);
    rect.bottom()
}
