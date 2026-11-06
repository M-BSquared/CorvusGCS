//! `.progress-bar` and its thin variants: a track with a fill from the left.
//! The corner is half the height, so 5 and 6 px bars round to 3 px and the
//! 8 px bar to 4.

use eframe::egui::{Color32, Painter, Rect, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::style;

/// One bar, drawn into the rectangle it is given.
pub struct Bar {
    fraction: f32,
    fill: Color32,
    track: Color32,
    bordered: bool,
    sweep: Option<f32>,
}

impl Bar {
    /// A determinate bar filled to `fraction` (0 to 1) in the accent.
    pub fn new(fraction: f32) -> Bar {
        Bar {
            fraction: fraction.clamp(0.0, 1.0),
            fill: style::accent(),
            track: style::surface_3(),
            bordered: false,
            sweep: None,
        }
    }

    /// An unknown amount: a segment a third of the bar wide crossing it.
    /// `phase` runs from 0 to 1 over one pass.
    pub fn indeterminate(phase: f32) -> Bar {
        Bar {
            sweep: Some(phase),
            ..Bar::new(0.0)
        }
    }

    pub fn fill(mut self, color: Color32) -> Bar {
        self.fill = color;
        self
    }

    pub fn track(mut self, color: Color32) -> Bar {
        self.track = color;
        self
    }

    /// The 1 px soft border of `.progress-bar`.
    pub fn bordered(mut self) -> Bar {
        self.bordered = true;
        self
    }

    pub fn show(self, painter: &Painter, rect: Rect) {
        let radius = (rect.height() / 2.0).ceil() as u8;
        if self.bordered {
            card::paint(painter, rect, Look::custom(self.track, style::border_soft(), radius));
        } else {
            painter.rect_filled(rect, radius, self.track);
        }
        if let Some(phase) = self.sweep {
            let k = phase * 1.35 - 0.35;
            let seg = Rect::from_min_size(
                rect.min + Vec2::new(rect.width() * k, 0.0),
                Vec2::new(rect.width() * 0.35, rect.height()),
            );
            painter.with_clip_rect(rect).rect_filled(
                seg.intersect(rect.expand2(Vec2::new(rect.width(), 0.0))),
                radius,
                self.fill,
            );
        } else if self.fraction > 0.0 {
            let fill = Rect::from_min_size(rect.min, Vec2::new(rect.width() * self.fraction, rect.height()));
            painter.rect_filled(fill, radius, self.fill);
        }
    }
}
