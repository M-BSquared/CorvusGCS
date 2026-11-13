//! The mission the vehicle holds, on the Home map: its route, its numbered
//! points and the leg being flown. The Mission page draws the same plan with
//! more detail and the means to edit it; this is what an operator watching a
//! flight needs, and it is drawn only for a plan the vehicle is known to fly
//! (the tracker's claim), never for one that was edited since.

use eframe::egui::{self, Color32, Pos2, Shape, Stroke};

use crate::ui::style::{self, Weight};

const AMBER: Color32 = Color32::from_rgb(0xFF, 0xC2, 0x1A);
const INK: Color32 = Color32::from_black_alpha(178);

/// Where a point stands in the flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Done,
    Current,
    Ahead,
}

/// One numbered point of the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub at: [f64; 2],
    pub number: usize,
    pub stage: Stage,
}

/// What to draw: the route as `[lon, lat]` stations, the points, and the leg
/// being flown.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlownRoute {
    pub route: Vec<[f64; 2]>,
    pub marks: Vec<Mark>,
    pub leg: Option<([f64; 2], [f64; 2])>,
}

/// Draws it; `at` turns a point into a screen position, or None where it
/// cannot be seen.
pub fn paint(painter: &egui::Painter, at: &dyn Fn([f64; 2]) -> Option<Pos2>, flown: &FlownRoute) {
    let line: Vec<Pos2> = flown.route.iter().filter_map(|c| at(*c)).collect();
    if line.len() >= 2 {
        painter.add(Shape::line(line.clone(), Stroke::new(5.0, INK)));
        painter.extend(Shape::dashed_line(&line, Stroke::new(2.6, AMBER), 7.8, 5.2));
    }
    if let Some((a, b)) = flown.leg {
        if let (Some(a), Some(b)) = (at(a), at(b)) {
            painter.line_segment([a, b], Stroke::new(4.0, style::healthy()));
        }
    }
    for m in &flown.marks {
        let Some(c) = at(m.at) else { continue };
        let (fill, ring, text) = match m.stage {
            Stage::Done => (Color32::from_gray(150), INK, Color32::WHITE),
            Stage::Current => (style::healthy(), INK, Color32::WHITE),
            Stage::Ahead => (AMBER, INK, Color32::from_black_alpha(220)),
        };
        let r = if m.stage == Stage::Current { 9.0 } else { 7.5 };
        painter.circle_filled(c, r + 1.5, ring);
        painter.circle_filled(c, r, fill);
        let g = painter.layout_no_wrap(m.number.to_string(), style::font(9.5, Weight::W700), text);
        painter.galley(c - g.size() / 2.0, g, text);
    }
}
