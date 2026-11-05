//! `Corvus.ui.message` (`.ui-msg`) and the notice boxes (`.params-desc`,
//! `.params-banner`): the one painter and the one `Tone` every page uses for
//! a line of feedback or a note that stands in a box.

use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// How a message is dressed: plain text, or `Ok`, `Err` or `Warn` in a tinted
/// box with a status dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Ok,
    Err,
    Warn,
}

/// The message at `x, y`, `w` wide; returns its bottom. The box has the
/// 1 px border of the CSS inside its padding, so it is 18 px taller than its
/// lines.
pub fn show(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str, tone: Tone) -> f32 {
    if tone == Tone::Plain {
        let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, style::text_2(), w - 20.0, 1.4);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(x + 10.0, y + 8.0), g, style::text_2());
        return y + h + 16.0;
    }
    let fg = match tone {
        Tone::Plain => return y,
        Tone::Ok => style::healthy(),
        Tone::Err => style::critical(),
        Tone::Warn => style::warning(),
    };
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, fg, w - 25.0 - 10.0 - 2.0, 1.4);
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 16.0 + 2.0));
    let p = ui.painter();
    card::paint(p, rect, Look::Tone(tone));
    let line = 11.5 * 1.4;
    p.circle_filled(Pos2::new(x + 1.0 + 10.0 + 3.5, y + 1.0 + 8.0 + line / 2.0), 3.5, fg);
    p.galley(Pos2::new(x + 1.0 + 25.0, y + 1.0 + 8.0), g, fg);
    rect.bottom()
}

/// `.params-desc`, an inset box of explaining text, or with `Tone::Warn` the
/// amber `.params-banner` that gates a card. 12 px of padding across 14 px,
/// inside the 1 px border. Returns the bottom.
pub fn boxed(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str, tone: Tone) -> f32 {
    let (color, size, lh) = match tone {
        Tone::Plain => (style::text_2(), 12.0, 1.55),
        Tone::Ok => (style::healthy(), 11.5, 1.4),
        Tone::Err => (style::critical(), 11.5, 1.4),
        Tone::Warn => (style::warning(), 11.5, 1.4),
    };
    let g = kit::wrapped_weight(ui, text, size, Weight::W400, color, w - 30.0, lh);
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 26.0));
    card::paint(ui.painter(), rect, Look::Tone(tone));
    ui.painter().galley(rect.min + Vec2::new(15.0, 13.0), g, color);
    rect.bottom()
}
