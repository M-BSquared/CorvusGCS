//! The segmented switch of the web interface: `.params-filter` and
//! `.link-kinds`, a recessed strip whose chosen segment is raised. Both are
//! this one control: segments as wide as their content, or stretched to a
//! given width with the content centred.

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// One segment.
#[derive(Debug, Clone)]
pub struct Segment {
    pub label: String,
    /// A count after the label; empty for none.
    pub count: String,
    pub icon: Option<&'static str>,
    pub enabled: bool,
}

impl Segment {
    pub fn new(label: impl Into<String>) -> Segment {
        Segment {
            label: label.into(),
            count: String::new(),
            icon: None,
            enabled: true,
        }
    }

    pub fn count(mut self, count: impl Into<String>) -> Segment {
        self.count = count.into();
        self
    }

    pub fn icon(mut self, icon: &'static str) -> Segment {
        self.icon = Some(icon);
        self
    }

    pub fn enabled(mut self, on: bool) -> Segment {
        self.enabled = on;
        self
    }
}

/// How the strip is laid out.
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    /// Stretch to this width, the segments equal and their content centred.
    pub width: Option<f32>,
    /// Vertical padding of a segment: 5 for a filter, 6 for the link kinds.
    pub pad_y: f32,
}

impl Opts {
    /// `.params-filter`.
    pub const FILTER: Opts = Opts {
        width: None,
        pad_y: 5.0,
    };

    /// `.link-kinds` across `width`.
    pub fn stretched(width: f32) -> Opts {
        Opts {
            width: Some(width),
            pad_y: 6.0,
        }
    }
}

const ICON: f32 = 13.0;

fn gals(ui: &egui::Ui, s: &Segment) -> (std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>) {
    (
        style::galley(ui, &s.label, 11.0, Weight::W600, style::text_3(), 0.3),
        style::galley(ui, &s.count, 11.0, Weight::W500, style::text_3(), 0.0),
    )
}

fn content_width(s: &Segment, label: &egui::Galley, count: &egui::Galley) -> f32 {
    let icon = if s.icon.is_some() { ICON + 6.0 } else { 0.0 };
    icon + label.size().x
        + if count.text().is_empty() {
            0.0
        } else {
            6.0 + count.size().x
        }
}

fn height(opts: &Opts) -> f32 {
    3.0 * 2.0 + 2.0 + opts.pad_y * 2.0 + 11.0 * 1.21
}

/// How wide the strip is drawn.
pub fn width(ui: &egui::Ui, segs: &[Segment], opts: &Opts) -> f32 {
    if let Some(w) = opts.width {
        return w;
    }
    let widths: f32 = segs
        .iter()
        .map(|s| {
            let (l, c) = gals(ui, s);
            20.0 + content_width(s, &l, &c)
        })
        .sum();
    2.0 + 6.0 + widths + 3.0 * (segs.len() as f32 - 1.0)
}

/// Draws the strip with its top left at `at`. Returns its rect and the index
/// pressed this frame.
pub fn show(
    ui: &mut egui::Ui,
    at: Pos2,
    segs: &[Segment],
    selected: usize,
    id: &str,
    opts: Opts,
) -> (Rect, Option<usize>) {
    let painter = ui.painter().clone();
    let g: Vec<_> = segs.iter().map(|s| gals(ui, s)).collect();
    let n = segs.len() as f32;
    let widths: Vec<f32> = match opts.width {
        Some(w) => vec![(w - 2.0 - 6.0 - 3.0 * (n - 1.0)) / n; segs.len()],
        None => segs
            .iter()
            .zip(&g)
            .map(|(s, (l, c))| 20.0 + content_width(s, l, c))
            .collect(),
    };
    let h = height(&opts);
    let rect = Rect::from_min_size(at, Vec2::new(width(ui, segs, &opts), h));
    painter.rect_filled(rect, style::RADIUS_BTN, style::glass(0.82));
    painter.rect_stroke(
        rect,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::tint(0.09)),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + 4.0;
    let mut clicked = None;
    for (i, (s, (lg, cg))) in segs.iter().zip(g).enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, rect.top() + 4.0), Vec2::new(widths[i], h - 8.0));
        let sense = if s.enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let resp = ui.interact(r, ui.scope_id().with((id, i)), sense);
        let sel = i == selected;
        if sel {
            painter.rect_filled(r, style::RADIUS_BTN - 2, style::surface_1());
        }
        let color = if sel || (resp.hovered() && s.enabled) {
            style::text_1()
        } else {
            style::text_3()
        };
        let alpha = if s.enabled { 1.0 } else { 0.45 };
        let mut lx = if opts.width.is_some() {
            r.center().x - content_width(s, &lg, &cg) / 2.0
        } else {
            r.left() + 10.0
        };
        if let Some(icon) = s.icon {
            icons::paint(
                ui,
                icon,
                Rect::from_min_size(Pos2::new(lx, r.center().y - ICON / 2.0), Vec2::splat(ICON)),
                if sel { style::accent() } else { color }.gamma_multiply(alpha),
            );
            lx += ICON + 6.0;
        }
        let l2 = style::galley(ui, &s.label, 11.0, Weight::W600, color.gamma_multiply(alpha), 0.3);
        painter.galley(Pos2::new(lx, r.center().y - l2.size().y / 2.0), l2, color);
        lx += lg.size().x + 6.0;
        if !cg.text().is_empty() {
            let cc = if sel { style::text_2() } else { style::text_3() };
            let c2 = style::galley(ui, cg.text(), 11.0, Weight::W500, cc.gamma_multiply(alpha), 0.0);
            painter.galley(Pos2::new(lx, r.center().y - c2.size().y / 2.0), c2, cc);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
        x += widths[i] + 3.0;
    }
    (rect, clicked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_is_as_tall_as_the_css_says() {
        assert!((height(&Opts::FILTER) - (8.0 + 10.0 + 13.31)).abs() < 1e-3);
        assert!(height(&Opts::stretched(100.0)) > height(&Opts::FILTER));
    }

    #[test]
    fn a_stretched_strip_keeps_its_width() {
        assert_eq!(Opts::stretched(240.0).width, Some(240.0));
        assert_eq!(Opts::FILTER.width, None);
    }
}
