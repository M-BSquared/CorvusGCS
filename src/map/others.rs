//! The vehicles that are not being flown, on the Home map (`map-fleet.js`,
//! `.fleet-marker` and `.v-label` of `map.css`): the same marker in the
//! vehicle's own colour, a little smaller and without the halo, its name
//! under it once there is more than one, and a track of its own.

use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};

use crate::data::track::Point;
use crate::fleet::tracks::{Marks, Tracks};
use crate::fleet::{parse_hex, tint, Snapshot};
use crate::ui::style::{self, Weight};

/// One vehicle that is not flown.
#[derive(Debug, Clone, PartialEq)]
pub struct Other {
    pub id: String,
    pub name: String,
    /// `#RRGGBB`, or empty for the theme's colour.
    pub color: String,
    pub stale: bool,
}

/// The fleet as the map draws it.
#[derive(Debug, Default)]
pub struct Others {
    pub tracks: Tracks,
    pub marks: Marks,
    pub list: Vec<Other>,
    /// The vehicle the map draws as its own.
    pub owner: String,
    pub multi: bool,
    pub flown_color: Option<Color32>,
    pub flown_name: String,
    /// The vehicle whose marker was pressed, for the Vehicles dialog.
    pub click: Option<String>,
}

/// `(colour, past colour)` of a vehicle's lines.
pub fn line_colors(hex: &str, rainbow: bool, index: usize) -> (Color32, Color32) {
    let theme = style::track();
    let main = parse_hex(hex).unwrap_or(theme);
    let past = if rainbow {
        parse_hex(&crate::fleet::rainbow_tone(index)).unwrap_or(main)
    } else if hex.is_empty() {
        style::track_past()
    } else {
        parse_hex(&tint(hex, 0.45)).unwrap_or(main)
    };
    (main, past)
}

impl Others {
    /// One telemetry step: tracks, markers and which vehicle is the flown
    /// one. Returns true while a marker is still easing.
    pub fn sync(&mut self, snap: &Snapshot, owner: &str, dt: f32) -> bool {
        self.owner = owner.to_string();
        self.multi = snap.is_multi();
        self.flown_color = parse_hex(snap.color_of(owner));
        self.flown_name = snap.name_of(owner).to_string();
        self.list.clear();
        let mut keep: Vec<&str> = Vec::new();
        for v in &snap.vehicles {
            self.tracks.heard(&v.id);
            if v.id == owner {
                continue;
            }
            if !v.stale {
                if let Some(p) = v.position {
                    self.tracks.record(&v.id, p, Some(v.armed));
                }
            }
            if let Some(p) = v.position {
                self.marks.set(&v.id, p, v.heading);
                keep.push(&v.id);
                self.list.push(Other {
                    id: v.id.clone(),
                    name: v.name.clone(),
                    color: v.color.clone(),
                    stale: v.stale,
                });
            }
        }
        self.marks.retain(&keep);
        self.marks.step(dt)
    }

    /// The operator removed `id`.
    pub fn forget(&mut self, id: &str) {
        self.tracks.forget(id);
        self.marks.forget(id);
    }

    /// The vehicle whose marker is under `pos`.
    pub fn hit(&self, at: &dyn Fn([f64; 2]) -> Option<Pos2>, pos: Pos2) -> Option<String> {
        self.list.iter().find_map(|o| {
            let m = self.marks.get(&o.id)?;
            let c = at([m.lng, m.lat])?;
            (c.distance(pos) <= 17.0).then(|| o.id.clone())
        })
    }

    /// The tracks, then the markers and their names.
    pub fn paint(&self, painter: &egui::Painter, at: &dyn Fn([f64; 2]) -> Option<Pos2>, rainbow: bool) {
        let casing = style::bg();
        for (id, t) in self.tracks.each(&self.owner) {
            let hex = self.list.iter().find(|o| o.id == id).map_or("", |o| o.color.as_str());
            let (main, past_c) = line_colors(hex, rainbow, 0);
            for (i, f) in t.past.iter().enumerate() {
                let c = if rainbow { line_colors(hex, true, i).1 } else { past_c };
                paint_line(painter, f, at, casing, c, 4.0, 2.0, 0.8);
            }
            paint_line(painter, &t.current, at, casing, main, 5.0, 2.4, 0.95);
        }
        for o in &self.list {
            let Some(m) = self.marks.get(&o.id) else { continue };
            let Some(c) = at([m.lng, m.lat]) else { continue };
            let color = parse_hex(&o.color).unwrap_or_else(style::vehicle);
            let fade = if o.stale { 0.45 } else { 1.0 };
            crate::map::paint_vehicle_in(painter, c, m.heading, color, 40.0 / 48.0, false, fade);
            if self.multi {
                let text = if o.stale {
                    format!("{} (lost)", o.name)
                } else {
                    o.name.clone()
                };
                paint_label(painter, c, 16.0, &text, color, fade);
            }
        }
    }

    /// The flown vehicle's name under its marker, when there is more than one.
    pub fn paint_flown_label(&self, painter: &egui::Painter, c: Pos2) {
        if self.multi && !self.flown_name.is_empty() {
            let color = self.flown_color.unwrap_or_else(style::vehicle);
            paint_label(painter, c, 20.0, &self.flown_name, color, 1.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_line(
    painter: &egui::Painter,
    pts: &[Point],
    at: &dyn Fn([f64; 2]) -> Option<Pos2>,
    casing: Color32,
    color: Color32,
    casing_w: f32,
    w: f32,
    alpha: f32,
) {
    if pts.len() < 2 {
        return;
    }
    let line: Vec<Pos2> = pts.iter().filter_map(|p| at([p.lng, p.lat])).collect();
    if line.len() < 2 {
        return;
    }
    painter.add(egui::Shape::line(
        line.clone(),
        Stroke::new(casing_w, casing.gamma_multiply(0.5)),
    ));
    painter.add(egui::Shape::line(line, Stroke::new(w, color.gamma_multiply(alpha))));
}

/// `.v-label`: white text on a black 62 % chip with a 3 px stripe of the
/// vehicle's colour on its left.
fn paint_label(painter: &egui::Painter, c: Pos2, below: f32, text: &str, color: Color32, fade: f32) {
    let g = painter.layout_no_wrap(
        text.to_string(),
        style::font(11.0, Weight::W600),
        Color32::WHITE.gamma_multiply(fade),
    );
    let w = (g.size().x + 3.0 + 5.0 + 6.0).min(160.0);
    let r = egui::Rect::from_min_size(Pos2::new(c.x - w / 2.0, c.y + below), Vec2::new(w, 16.0));
    painter.rect_filled(r, 3, Color32::from_black_alpha(158).gamma_multiply(fade));
    painter.rect_filled(
        egui::Rect::from_min_size(r.min, Vec2::new(3.0, r.height())),
        egui::CornerRadius {
            nw: 3,
            sw: 3,
            ne: 0,
            se: 0,
        },
        color.gamma_multiply(fade),
    );
    painter
        .with_clip_rect(egui::Rect::from_min_max(r.min + Vec2::new(3.0, 0.0), r.max))
        .galley(
            Pos2::new(r.left() + 3.0 + 5.0, r.center().y - g.size().y / 2.0),
            g,
            Color32::WHITE,
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap() -> Snapshot {
        Snapshot::parse(
            &json!({
                "active": "main:1",
                "links": [{"id": "main"}],
                "vehicles": [
                    {"id": "main:1", "name": "Scout", "color": "#FF0000", "armed": true, "position": [11.64, 48.07]},
                    {"id": "main:2", "name": "Rover", "color": "#2BC4E4", "armed": false, "position": [11.65, 48.08], "heading": 90.0},
                    {"id": "main:3", "name": "Lost", "stale": true, "position": [11.66, 48.09]},
                    {"id": "main:4", "name": "Nowhere", "position": null}
                ]
            }),
            &|_| false,
        )
    }

    #[test]
    fn every_vehicle_but_the_flown_one_with_a_position_gets_a_marker() {
        let mut o = Others::default();
        o.sync(&snap(), "main:1", 0.1);
        let ids: Vec<&str> = o.list.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["main:2", "main:3"]);
        assert!(o.multi);
        assert_eq!(o.flown_name, "Scout");
        assert_eq!(o.flown_color, parse_hex("#FF0000"));
        assert!(o.marks.get("main:1").is_none());
    }

    #[test]
    fn a_lost_vehicle_is_drawn_but_not_tracked() {
        let mut o = Others::default();
        o.sync(&snap(), "main:1", 0.1);
        let tracked: Vec<&str> = o.tracks.each("main:1").map(|(id, _)| id).collect();
        assert_eq!(tracked, vec!["main:2"]);
    }

    #[test]
    fn the_track_goes_with_the_vehicle_that_stops_being_flown() {
        let mut o = Others::default();
        o.sync(&snap(), "main:1", 0.1);
        // main:2 is flown now: its track leaves the others, main:1's joins them.
        let theirs = o.tracks.take("main:2");
        assert!(theirs.is_some() || o.tracks.each("main:1").count() == 0);
    }

    #[test]
    fn a_removed_vehicle_loses_marker_and_track() {
        let mut o = Others::default();
        o.sync(&snap(), "main:1", 0.1);
        o.forget("main:2");
        assert!(o.marks.get("main:2").is_none());
        assert!(!o.tracks.has_lines("main:1"));
    }

    #[test]
    fn a_press_finds_the_marker_under_it() {
        let mut o = Others::default();
        o.sync(&snap(), "main:1", 0.1);
        for _ in 0..200 {
            o.marks.step(0.1);
        }
        let at = |p: [f64; 2]| {
            Some(Pos2::new(
                ((p[0] - 11.0) * 1000.0) as f32,
                ((p[1] - 48.0) * 1000.0) as f32,
            ))
        };
        let on = at([11.65, 48.08]).expect("on screen");
        assert_eq!(o.hit(&at, on + Vec2::new(5.0, 5.0)), Some("main:2".to_string()));
        assert_eq!(o.hit(&at, on + Vec2::new(40.0, 0.0)), None);
    }

    #[test]
    fn the_lines_use_the_vehicle_colour_and_a_quieter_one_for_earlier_flights() {
        let (main, past) = line_colors("#2BC4E4", false, 0);
        assert_eq!(main, Color32::from_rgb(0x2B, 0xC4, 0xE4));
        assert_eq!(past, parse_hex(&tint("#2BC4E4", 0.45)).expect("a colour"));
        let (_, rainbow) = line_colors("#2BC4E4", true, 1);
        assert_eq!(rainbow, parse_hex(&crate::fleet::rainbow_tone(1)).expect("a colour"));
        let (theme, theme_past) = line_colors("", false, 0);
        assert_eq!(theme, style::track());
        assert_eq!(theme_past, style::track_past());
    }
}
