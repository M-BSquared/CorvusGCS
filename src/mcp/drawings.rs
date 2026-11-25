//! What an AI client drew, on the Home map of this window: the `drawings` topic
//! as shapes of the plugin layer, so the flat and the 3D map paint them alike,
//! and a pill that says how much is drawn and takes it away.

use std::sync::{Arc, Mutex};

use corvus_core::drawings::outline;
use corvus_core::mcp::geo;
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::net::stream::Stream;
use crate::plugins::shapes::{self, Shape, ShapeSet, Style};
use crate::ui::kit::{Button, Variant};
use crate::ui::style::{self, Weight};

const OWNER: &str = "corvus-ai";
/// An arrow's head is this share of the arrow, and at most this share of its last leg.
const HEAD_OF_ALL: f64 = 0.09;
const HEAD_OF_LEG: f64 = 0.6;
const HEAD_MIN_M: f64 = 1.5;
const HEAD_MAX_M: f64 = 400.0;
/// Half the opening of the head, in degrees.
const HEAD_HALF_DEG: f64 = 24.0;

#[derive(Default)]
struct Shared {
    shapes: Vec<Value>,
    changed: bool,
    show: Option<Vec<[f64; 2]>>,
}

/// The drawings of the backend as this window has them.
pub struct Drawings {
    server: Server,
    shared: Arc<Mutex<Shared>>,
    stream: Option<Stream>,
    clearing: Option<Pending<Result<Value, String>>>,
}

fn take_event(shared: &Mutex<Shared>, e: &Value) {
    let Ok(mut g) = shared.lock() else { return };
    match e.get("type").and_then(Value::as_str) {
        Some("snapshot") => {
            g.shapes = e.get("shapes").and_then(Value::as_array).cloned().unwrap_or_default();
            g.changed = true;
        }
        Some("show") => {
            let points: Vec<[f64; 2]> = e
                .get("points")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter_map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?]))
                        .collect()
                })
                .unwrap_or_default();
            if !points.is_empty() {
                g.show = Some(points);
            }
        }
        _ => {}
    }
}

impl Drawings {
    pub fn start(server: Server, ctx: &egui::Context) -> Drawings {
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let (s, c) = (shared.clone(), ctx.clone());
        let stream = Stream::start(server.clone(), "/api/events?topics=drawings".into(), move |ev| {
            if ev.name == "drawings" {
                if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                    take_event(&s, &v);
                    c.request_repaint();
                }
            }
        });
        Drawings {
            server,
            shared,
            stream: Some(stream),
            clearing: None,
        }
    }

    /// The drawings, when they changed since the last call.
    pub fn take_shapes(&self) -> Option<Vec<Shape>> {
        let mut g = self.shared.lock().ok()?;
        std::mem::take(&mut g.changed).then(|| shapes_of(&g.shapes))
    }

    /// The places the client asked to see, longitude first.
    pub fn take_show(&self) -> Option<Vec<[f64; 2]>> {
        self.shared.lock().ok()?.show.take()
    }

    fn count(&self) -> usize {
        self.shared.lock().map_or(0, |g| g.shapes.len())
    }

    /// The pill on the map, at `top`: how much is drawn, and Clear.
    pub fn show(&mut self, ctx: &egui::Context, host: Option<Rect>, below: f32) {
        if self.clearing.as_ref().and_then(Pending::take).is_some() {
            self.clearing = None;
        }
        let n = self.count();
        let Some(host) = host.filter(|_| n > 0) else { return };
        egui::Area::new(egui::Id::NULL.with("ai-drawings"))
            .order(egui::Order::Middle)
            .fixed_pos(host.min)
            .show(ctx, |ui| {
                let text = if n == 1 {
                    crate::i18n::tr("1 AI drawing").into_owned()
                } else {
                    crate::i18n::tr_fmt("{n} AI drawings", &[("n", &n.to_string())])
                };
                let g = style::galley(ui, &text, 12.0, Weight::W600, style::text_1(), 0.0);
                let clear = Button::new("Clear", Variant::Secondary)
                    .icon("trash-2")
                    .small()
                    .enabled(self.clearing.is_none());
                let (bw, bh) = (clear.width(ui), clear.height());
                let size = Vec2::new(12.0 + g.size().x + 8.0 + bw + 4.0, bh + 8.0);
                let r = Rect::from_min_size(
                    Pos2::new(host.center().x - size.x / 2.0, host.top() + below + 4.0),
                    size,
                );
                let painter = ui.painter().clone();
                painter.add(crate::ui::card::sheet(
                    "ai-drawings",
                    r,
                    255,
                    style::glass(0.8),
                    style::border_soft(),
                ));
                painter.galley(
                    Pos2::new(r.left() + 12.0, r.center().y - g.size().y / 2.0),
                    g,
                    style::text_1(),
                );
                let br = Rect::from_min_size(
                    Pos2::new(r.right() - 4.0 - bw, r.center().y - bh / 2.0),
                    Vec2::new(bw, bh),
                );
                if clear
                    .title("Take what the AI client drew off the map")
                    .show(ui, br, "ai-drawings-clear")
                {
                    let s = self.server.clone();
                    self.clearing = Some(Pending::spawn(ui.ctx(), move || {
                        http::post_json(&s, "/api/map/drawings/remove", &json!({"all": true}))
                    }));
                }
            });
    }

    pub fn shut_down(&mut self) {
        if let Some(mut s) = self.stream.take() {
            s.stop();
        }
    }
}

fn lon_lat(p: geo::Place) -> [f64; 2] {
    [p[1], p[0]]
}

/// The tip and the two wings of an arrow along `path`, and where its shaft ends.
fn head(path: &[geo::Place]) -> Option<([geo::Place; 3], geo::Place)> {
    let tip = *path.last()?;
    let before = *path.iter().rev().find(|p| geo::distance(**p, tip) > 0.01)?;
    let leg = geo::distance(before, tip);
    let long = (geo::length(path) * HEAD_OF_ALL)
        .clamp(HEAD_MIN_M, HEAD_MAX_M)
        .min(leg * HEAD_OF_LEG);
    let back = geo::bearing(tip, before);
    let side = long / HEAD_HALF_DEG.to_radians().cos();
    Some((
        [
            tip,
            geo::project(tip, back + HEAD_HALF_DEG, side),
            geo::project(tip, back - HEAD_HALF_DEG, side),
        ],
        geo::project(tip, back, long * 0.7),
    ))
}

/// The colour of words, which stand on a rim in the theme's background colour:
/// one that would vanish in it gives way to the theme's own ink.
fn readable(c: Color32) -> Color32 {
    let light = |c: Color32| 0.299 * f32::from(c.r()) + 0.587 * f32::from(c.g()) + 0.114 * f32::from(c.b());
    if (light(c) - light(style::bg())).abs() < 70.0 {
        style::text_1()
    } else {
        c
    }
}

/// Where a label of a shape stands: in the middle of a path, on top of an area.
fn label_at(kind: &str, path: &[geo::Place]) -> Option<geo::Place> {
    match kind {
        "line" | "arrow" => {
            let half = geo::length(path) / 2.0;
            let mut gone = 0.0;
            for w in path.windows(2) {
                let d = geo::distance(w[0], w[1]);
                if gone + d >= half {
                    return Some(geo::project(w[0], geo::bearing(w[0], w[1]), half - gone));
                }
                gone += d;
            }
            path.first().copied()
        }
        _ => path.iter().copied().max_by(|a, b| a[0].total_cmp(&b[0])),
    }
}

/// The backend's drawings as shapes of the map.
pub fn shapes_of(list: &[Value]) -> Vec<Shape> {
    let mut set = ShapeSet::new(OWNER);
    for raw in list {
        let Some(s) = raw.as_object() else { continue };
        let text = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("");
        let number = |k: &str| s.get(k).and_then(Value::as_f64).map(|n| n as f32);
        let (id, kind, label) = (text("id"), text("kind"), text("text"));
        let color: Color32 = shapes::hex(text("color")).unwrap_or(shapes::DEFAULT_COLOR);
        let look = Style::color(color);
        let words = Style::color(readable(color));
        let dashed = s.get("dashed") == Some(&Value::Bool(true));
        let path = outline(s);
        let coords: Vec<[f64; 2]> = path.iter().map(|p| lon_lat(*p)).collect();
        let Some(at) = coords.first().copied() else { continue };
        match kind {
            "line" => {
                set.line(id, &coords, look, number("width"), dashed);
            }
            "arrow" => {
                let mut shaft = coords.clone();
                let tip = head(&path);
                if let (Some((_, end)), Some(last)) = (tip, shaft.last_mut()) {
                    *last = lon_lat(end);
                }
                set.line(id, &shaft, look, number("width").or(Some(4.0)), dashed);
                if let Some((wings, _)) = tip {
                    let wings = wings.map(lon_lat);
                    set.polygon(&format!("{id}.head"), &wings, look, Some(1.0), Some(0.0), false);
                }
            }
            "polygon" | "circle" | "sector" => {
                let fill = number("fill_opacity").or(Some(0.18));
                set.polygon(id, &coords, look, fill, number("width"), dashed);
            }
            "marker" => {
                set.marker(
                    id,
                    at,
                    text("icon"),
                    label,
                    "",
                    look,
                    number("size"),
                    false,
                    false,
                    false,
                );
            }
            "text" => {
                set.text(id, at, label, words, number("size").or(Some(14.0)), false);
            }
            _ => {}
        }
        if !label.is_empty() && !matches!(kind, "marker" | "text") {
            if let Some(p) = label_at(kind, &path) {
                let size = number("size").or(Some(13.0));
                set.text(&format!("{id}.label"), lon_lat(p), label, words, size, false);
            }
        }
    }
    set.shapes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::shapes::Kind;

    const A: geo::Place = [48.0795, 11.6385];

    fn arrow(text: &str) -> Value {
        let b = geo::project(A, 90.0, 200.0);
        json!({"kind": "arrow", "id": "a", "color": "#FF5A52", "text": text,
               "points": [{"lat": A[0], "lon": A[1]}, {"lat": b[0], "lon": b[1]}]})
    }

    #[test]
    fn an_arrow_is_a_shaft_and_a_head_that_points_at_its_end() {
        let shapes = shapes_of(&[arrow("")]);
        assert_eq!(shapes.len(), 2);
        let Kind::Polygon { pts, .. } = &shapes[1].kind else {
            panic!()
        };
        let tip = [pts[0][1], pts[0][0]];
        assert!((geo::distance(A, tip) - 200.0).abs() < 0.1, "the tip is the last point");
        for w in &pts[1..] {
            let d = geo::distance(A, [w[1], w[0]]);
            assert!((175.0..190.0).contains(&d), "the wings lie behind it: {d}");
        }
        let Kind::Line { pts: shaft, .. } = &shapes[0].kind else {
            panic!()
        };
        let end = shaft[shaft.len() - 1];
        let d = geo::distance(A, [end[1], end[0]]);
        assert!((180.0..199.0).contains(&d), "the shaft ends inside the head: {d}");
        assert_eq!(shapes[0].color, Color32::from_rgb(0xFF, 0x5A, 0x52));
        assert_eq!(shapes_of(&[arrow("To the gate")]).len(), 3, "and a label");
    }

    #[test]
    fn an_arrow_of_no_length_is_a_line_without_a_head() {
        let p = json!({"lat": A[0], "lon": A[1]});
        let shapes = shapes_of(&[json!({"kind": "arrow", "id": "a", "color": "#FFFFFF", "points": [p, p]})]);
        assert!(shapes.iter().all(|s| matches!(s.kind, Kind::Line { .. })));
    }

    #[test]
    fn every_kind_is_drawn_and_junk_is_not() {
        let at = |v: Value| {
            let mut v = v;
            for (k, x) in [("lat", A[0]), ("lon", A[1])] {
                v[k] = json!(x);
            }
            v["color"] = json!("#2BC4E4");
            v
        };
        let shapes = shapes_of(&[
            at(json!({"kind": "circle", "id": "c", "radius_m": 40})),
            at(json!({"kind": "sector", "id": "s", "radius_m": 40, "from_deg": 0, "to_deg": 60})),
            at(json!({"kind": "marker", "id": "m", "icon": "flag", "text": "Here"})),
            at(json!({"kind": "text", "id": "t", "text": "Hello"})),
            json!({"kind": "circle", "id": "x"}),
            json!(7),
        ]);
        let keys: Vec<&str> = shapes.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["c", "s", "m", "t"]);
    }

    #[test]
    fn the_window_keeps_the_list_and_what_to_frame() {
        let shared = Mutex::new(Shared::default());
        take_event(&shared, &json!({"type": "snapshot", "shapes": [arrow("")]}));
        take_event(&shared, &json!({"type": "show", "points": [[11.6, 48.0], ["x", 1]]}));
        take_event(&shared, &json!({"type": "show", "points": []}));
        let g = shared.lock().unwrap();
        assert!(g.changed && g.shapes.len() == 1);
        assert_eq!(g.show, Some(vec![[11.6, 48.0]]));
    }
}
