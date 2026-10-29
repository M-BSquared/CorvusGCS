//! A pill: a short label in a rounded outline, with an optional icon or
//! status dot before it (`.calib-ready-chip`, `.calib-phase`,
//! `.calib-prep-item`). Drawn at a position and measured by the same code, so
//! a row of them can be laid out before it is painted.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// One pill.
pub struct Chip {
    pub text: String,
    pub size: f32,
    pub weight: Weight,
    pub letter: f32,
    /// Padding: top, right, bottom, left (CSS order).
    pub pad: (f32, f32, f32, f32),
    pub gap: f32,
    pub fg: Color32,
    pub bg: Color32,
    pub border: Color32,
    /// An icon before the text: its name, size and colour.
    pub icon: Option<(&'static str, f32, Color32)>,
    /// A status dot before the text: its colour and whether it glows.
    pub dot: Option<(Color32, bool)>,
}

impl Chip {
    pub fn new(text: impl Into<String>) -> Chip {
        Chip {
            text: text.into(),
            size: 11.0,
            weight: Weight::W600,
            letter: 0.0,
            pad: (5.0, 11.0, 5.0, 9.0),
            gap: 7.0,
            fg: style::text_2(),
            bg: style::surface_1(),
            border: style::border_soft(),
            icon: None,
            dot: None,
        }
    }

    fn lead(&self) -> f32 {
        if let Some((_, s, _)) = self.icon {
            s + self.gap
        } else if self.dot.is_some() {
            7.0 + self.gap
        } else {
            0.0
        }
    }

    /// Its size, border included.
    pub fn measure(&self, ui: &egui::Ui) -> Vec2 {
        let g = style::galley(ui, &self.text, self.size, self.weight, self.fg, self.letter);
        let line = g.size().y.max(self.icon.map_or(0.0, |i| i.1));
        Vec2::new(
            2.0 + self.pad.1 + self.pad.3 + self.lead() + g.size().x,
            2.0 + self.pad.0 + self.pad.2 + line,
        )
    }

    /// Paints it with its top left at `at`; returns its rectangle.
    pub fn paint(&self, ui: &egui::Ui, at: Pos2) -> Rect {
        let size = self.measure(ui);
        let rect = Rect::from_min_size(at, size);
        let p = ui.painter();
        let radius = (size.y / 2.0).min(255.0) as u8;
        p.rect_filled(rect, radius, self.bg);
        p.rect_stroke(rect, radius, Stroke::new(1.0, self.border), egui::StrokeKind::Inside);
        let mut x = rect.left() + 1.0 + self.pad.3;
        let cy = rect.center().y;
        if let Some((name, s, color)) = self.icon {
            icons::paint(
                ui,
                name,
                Rect::from_center_size(Pos2::new(x + s / 2.0, cy), Vec2::splat(s)),
                color,
            );
            x += s + self.gap;
        } else if let Some((color, lit)) = self.dot {
            kit::dot(p, Pos2::new(x + 3.5, cy), color, lit);
            x += 7.0 + self.gap;
        }
        let g = style::galley(ui, &self.text, self.size, self.weight, self.fg, self.letter);
        p.galley(Pos2::new(x, cy - g.size().y / 2.0), g, self.fg);
        rect
    }
}
