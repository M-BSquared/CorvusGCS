//! The flight HUD at the bottom right of the map: compass, attitude
//! indicator and six readouts, drawn from the web HUD's own geometry
//! (src/js/instruments.js, the SVGs in index.html, flight.css).
//!
//! The dials are the web SVGs' 200-unit viewBox scaled to the width the CSS
//! gives them, so every tick, needle and label lands where it does there.
//! Heading, pitch and roll ease towards the telemetry with the same time
//! constants, so the needle sweeps rather than jumps.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::Value;

use crate::style::{self, Weight};
use crate::topbar::is_airborne;
use crate::units::{js_round, js_to_fixed, Units};

/// `--hud-w`.
pub const WIDTH: f32 = 320.0;
const PAD: f32 = 8.0;
const DIAL_GAP: f32 = 10.0;
/// Pixels per degree of pitch, in viewBox units.
const PPD: f32 = 1.7;
const HDG_TAU: f32 = 0.18;
const PR_TAU: f32 = 0.16;
const HDG_EPS: f32 = 0.05;
const PR_EPS: f32 = 0.05;
const LANDED_ON_GROUND: i64 = 1;

/// Shortest signed turn from `cur` to `target`, in (-180, 180].
fn shortest_delta(cur: f32, target: f32) -> f32 {
    ((target - cur) % 360.0 + 540.0) % 360.0 - 180.0
}

fn approach(cur: f32, target: f32, dt: f32, tau: f32) -> f32 {
    cur + (target - cur) * (1.0 - (-dt / tau.max(1e-4)).exp())
}

/// `((a % 360) + 360) % 360`.
fn norm_angle(a: f32) -> f32 {
    ((a % 360.0) + 360.0) % 360.0
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn format_clock(seconds: f64) -> String {
    let t = seconds.floor().max(0.0) as u64;
    let (h, m, s) = (t / 3600, (t % 3600) / 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// What the HUD remembers between frames.
#[derive(Debug, Default)]
pub struct Hud {
    shown: Option<(f32, f32, f32)>,
    flight_start: Option<f64>,
    flight_end: Option<f64>,
}

impl Hud {
    /// Seconds in the air this flight: from take-off to touchdown or disarm;
    /// the last flight stays shown until the next take-off.
    pub fn flight_seconds(&mut self, s: &Value, now: f64) -> f64 {
        let armed = s.get("armed") == Some(&Value::Bool(true));
        let landed = s.get("landed_state").and_then(Value::as_i64);
        if armed && is_airborne(s) {
            if self.flight_start.is_none() || self.flight_end.is_some() {
                self.flight_start = Some(now);
                self.flight_end = None;
            }
        } else if self.flight_start.is_some()
            && self.flight_end.is_none()
            && (!armed || landed == Some(LANDED_ON_GROUND))
        {
            self.flight_end = Some(now);
        }
        match self.flight_start {
            None => 0.0,
            Some(start) => ((self.flight_end.unwrap_or(now) - start) / 1000.0).max(0.0),
        }
    }

    /// The six readouts, `—` without a link.
    pub fn cells(&mut self, s: &Value, units: &Units, now: f64) -> [(&'static str, String, Color32); 6] {
        let num = |k: &str| s.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        if s.get("connected") != Some(&Value::Bool(true)) {
            self.flight_start = None;
            self.flight_end = None;
            let d = || "\u{2014}".to_string();
            return [
                ("ALT AMSL", d(), style::TEXT_1),
                ("FLIGHT TIME", d(), style::TEXT_1),
                ("GS", d(), style::NAV),
                ("V/S", d(), style::TEXT_1),
                ("HDG", d(), style::NAV),
                ("SAT", d(), style::TEXT_1),
            ];
        }
        let sats = match s.get("gps_satellites") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(t)) => t.clone(),
            Some(Value::Null) | None => "undefined".into(),
            Some(other) => other.to_string(),
        };
        [
            (
                "ALT AMSL",
                units.format_length(num("altitude_amsl"), 0, false, false),
                style::TEXT_1,
            ),
            ("FLIGHT TIME", format_clock(self.flight_seconds(s, now)), style::TEXT_1),
            (
                "GS",
                units.format_speed(num("groundspeed"), 1, false, false),
                style::NAV,
            ),
            ("V/S", units.format_speed(num("vspeed"), 1, true, false), style::TEXT_1),
            (
                "HDG",
                format!("{:03}\u{00B0}", js_round(num("heading")) as i64),
                style::NAV,
            ),
            ("SAT", sats, style::TEXT_1),
        ]
    }

    /// Eases the shown heading, pitch and roll towards the state; true while
    /// still moving.
    fn ease(&mut self, s: &Value, dt: f32) -> ((f32, f32, f32), bool) {
        let get = |k: &str| {
            s.get(k)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
                .unwrap_or(0.0) as f32
        };
        let target = (get("heading"), get("pitch"), get("roll"));
        let Some((h, p, r)) = self.shown else {
            self.shown = Some(target);
            return (target, false);
        };
        let nh = h + shortest_delta(h, target.0) * (1.0 - (-dt / HDG_TAU).exp());
        let np = approach(p, target.1, dt, PR_TAU);
        let nr = approach(r, target.2, dt, PR_TAU);
        let done = shortest_delta(nh, target.0).abs() <= HDG_EPS
            && (np - target.1).abs() < PR_EPS
            && (nr - target.2).abs() < PR_EPS;
        let next = if done { target } else { (nh, np, nr) };
        self.shown = Some(next);
        (next, !done)
    }

    /// The panel's height for the dial width the CSS gives it.
    pub fn height(ui: &egui::Ui) -> f32 {
        let dial = (WIDTH - 2.0 * PAD - DIAL_GAP) / 2.0;
        let readout = readout_height(ui);
        let cell = cell_height(ui);
        PAD + dial + 5.0 + readout + 10.0 + 1.0 + 3.0 * cell + 2.0 + 1.0 + 3.0 + PAD
    }

    /// Draws the panel with its bottom right corner at `corner`.
    pub fn show(&mut self, ui: &mut egui::Ui, corner: Pos2, state: &Value, units: &Units, now: f64) {
        let size = Vec2::new(WIDTH, Hud::height(ui));
        let rect = Rect::from_min_size(corner - size, size);
        let painter = ui.painter().clone();
        let top = ((WIDTH - 26.0) / 4.0 + 8.0).round() as u8;
        let radius = CornerRadius {
            nw: top,
            ne: top,
            sw: style::RADIUS,
            se: style::RADIUS,
        };
        painter.add(style::shadow_pop().as_shape(rect, radius));
        painter.rect(
            rect,
            radius,
            style::glass_fill(),
            style::glass_stroke(),
            egui::StrokeKind::Inside,
        );
        // The panel takes the pointer, so the map under it does not pan.
        ui.interact(rect, ui.scope_id().with("hud"), egui::Sense::click_and_drag());

        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let ((h, p, r), moving) = self.ease(state, dt);
        if moving {
            ui.ctx().request_repaint();
        }

        let dial = (WIDTH - 2.0 * PAD - DIAL_GAP) / 2.0;
        let left = Rect::from_min_size(rect.min + Vec2::splat(PAD), Vec2::splat(dial));
        let right = left.translate(Vec2::new(dial + DIAL_GAP, 0.0));
        let shown_h = (js_round(f64::from(norm_angle(h))) as i64) % 360;
        compass(ui, &painter, left, shown_h as f32);
        attitude(ui, &painter, right, p, r);
        let sign = |v: f32| if v >= 0.0 { "+" } else { "" };
        let att = format!(
            "P {}{}\u{00B0}  R {}{}\u{00B0}",
            sign(p),
            js_to_fixed(f64::from(p), 1),
            sign(r),
            js_to_fixed(f64::from(r), 1)
        );
        readout(ui, &painter, left, "HEADING", &format!("{shown_h:03}\u{00B0}"));
        readout(ui, &painter, right, "ATTITUDE", &att);

        let grid_top = left.bottom() + 5.0 + readout_height(ui) + 10.0;
        let grid = Rect::from_min_max(
            Pos2::new(rect.left() + PAD + 3.0, grid_top),
            Pos2::new(rect.right() - PAD - 3.0, rect.bottom() - PAD - 3.0),
        );
        painter.rect(
            grid,
            style::RADIUS_BTN,
            style::BORDER_SOFT,
            Stroke::new(1.0, style::BORDER_SOFT),
            egui::StrokeKind::Inside,
        );
        let inner = grid.shrink(1.0);
        let cw = (inner.width() - 1.0) / 2.0;
        let ch = cell_height(ui);
        for (i, (label, value, color)) in self.cells(state, units, now).into_iter().enumerate() {
            let (col, row) = ((i % 2) as f32, (i / 2) as f32);
            let cell = Rect::from_min_size(
                Pos2::new(inner.left() + col * (cw + 1.0), inner.top() + row * (ch + 1.0)),
                Vec2::new(cw, ch),
            );
            let round = |nw, ne, sw, se| CornerRadius { nw, ne, sw, se };
            let r = match i {
                0 => round(5, 0, 0, 0),
                1 => round(0, 5, 0, 0),
                4 => round(0, 0, 5, 0),
                5 => round(0, 0, 0, 5),
                _ => CornerRadius::ZERO,
            };
            painter.rect_filled(cell, r, style::glass(0.4));
            let l = style::galley(ui, label, 8.5, Weight::W600, style::TEXT_3, 0.7);
            let v = style::galley(ui, &value, 15.0, Weight::W600, color, 0.0);
            let lh = l.size().y;
            painter.galley(Pos2::new(cell.left() + 9.0, cell.top() + 7.0), l, style::TEXT_3);
            let vy = cell.top() + 7.0 + lh + 2.0 + (15.0 * 1.1 - v.size().y) / 2.0;
            painter.galley(Pos2::new(cell.left() + 9.0, vy), v, color);
        }
    }
}

fn readout_height(ui: &egui::Ui) -> f32 {
    let l = style::galley(ui, "HEADING", 8.5, Weight::W600, style::TEXT_3, 0.9)
        .size()
        .y;
    let v = style::galley(ui, "000", 13.0, Weight::W600, style::TEXT_1, 0.0)
        .size()
        .y;
    l + 1.0 + v
}

fn cell_height(ui: &egui::Ui) -> f32 {
    let l = style::galley(ui, "ALT", 8.5, Weight::W600, style::TEXT_3, 0.7).size().y;
    7.0 + l + 2.0 + 15.0 * 1.1 + 7.0
}

fn readout(ui: &egui::Ui, painter: &egui::Painter, dial: Rect, label: &str, value: &str) {
    let l = style::galley(ui, label, 8.5, Weight::W600, style::TEXT_3, 0.9);
    let v = style::galley(ui, value, 13.0, Weight::W600, style::TEXT_1, 0.0);
    let y = dial.bottom() + 5.0;
    painter.galley(
        Pos2::new(dial.center().x - l.size().x / 2.0, y),
        l.clone(),
        style::TEXT_3,
    );
    painter.galley(
        Pos2::new(dial.center().x - v.size().x / 2.0, y + l.size().y + 1.0),
        v,
        style::TEXT_1,
    );
}

/// Maps the dial's 200-unit viewBox into `rect`.
struct ViewBox {
    c: Pos2,
    s: f32,
}

impl ViewBox {
    fn new(rect: Rect) -> ViewBox {
        ViewBox {
            c: rect.center(),
            s: rect.width() / 200.0,
        }
    }
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.c + Vec2::new(x, y) * self.s
    }
    fn w(&self, units: f32) -> f32 {
        units * self.s
    }
}

fn polar(deg: f32, r: f32) -> (f32, f32) {
    let rad = (deg - 90.0).to_radians();
    (rad.cos() * r, rad.sin() * r)
}

#[allow(clippy::too_many_arguments)]
fn text_centered(
    ui: &egui::Ui,
    painter: &egui::Painter,
    at: Pos2,
    s: &str,
    size: f32,
    w: Weight,
    color: Color32,
    letter: f32,
) {
    let g = style::galley(ui, s, size, w, color, letter);
    painter.galley(at - g.size() / 2.0, g, color);
}

/// The compass, north up: the card is fixed and the needle carries the
/// heading.
fn compass(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, heading: f32) {
    let vb = ViewBox::new(rect);
    painter.circle(
        vb.c,
        vb.w(88.0),
        style::tint(0.06),
        Stroke::new(vb.w(1.5), style::tint(0.16)),
    );
    painter.circle(
        vb.c,
        vb.w(74.0),
        style::rgba(0xFFFFFF, 0.55),
        Stroke::new(vb.w(1.0), style::tint(0.06)),
    );
    for deg in (0..360).step_by(5) {
        let (cardinal, major, medium) = (deg % 90 == 0, deg % 30 == 0, deg % 10 == 0);
        let r1 = if major {
            75.0
        } else if medium {
            79.0
        } else {
            82.0
        };
        let (color, width) = if cardinal {
            (style::TEXT_2, 2.2)
        } else if major {
            (style::TEXT_3, 1.6)
        } else if medium {
            (style::tint(0.34), 1.1)
        } else {
            (style::tint(0.22), 0.9)
        };
        let (x1, y1) = polar(deg as f32, r1);
        let (x2, y2) = polar(deg as f32, 86.0);
        line_round(painter, vb.p(x1, y1), vb.p(x2, y2), vb.w(width), color);
    }
    for (d, t) in [(0.0, "N"), (90.0, "E"), (180.0, "S"), (270.0, "W")] {
        let (x, y) = polar(d, 60.0);
        let color = if t == "N" { style::NAV } else { style::TEXT_2 };
        text_centered(ui, painter, vb.p(x, y), t, vb.w(13.0), Weight::W700, color, vb.w(0.3));
    }
    for d in [30, 60, 120, 150, 210, 240, 300, 330] {
        let (x, y) = polar(d as f32, 60.0);
        text_centered(
            ui,
            painter,
            vb.p(x, y),
            &format!("{:02}", d / 10),
            vb.w(7.5),
            Weight::W600,
            style::TEXT_3,
            0.0,
        );
    }
    let rot = egui::emath::Rot2::from_angle(heading.to_radians());
    let needle =
        |pts: &[(f32, f32)]| -> Vec<Pos2> { pts.iter().map(|&(x, y)| vb.c + rot * Vec2::new(x, y) * vb.s).collect() };
    painter.add(Shape::convex_polygon(
        needle(&[(0.0, 43.0), (5.0, 9.0), (0.0, 3.0), (-5.0, 9.0)]),
        style::rgba(style::VEHICLE_RGB, 0.28),
        Stroke::NONE,
    ));
    painter.add(Shape::convex_polygon(
        needle(&[(0.0, -50.0), (7.0, -9.0), (0.0, -3.0), (-7.0, -9.0)]),
        style::VEHICLE,
        Stroke::NONE,
    ));
    painter.circle(
        vb.c,
        vb.w(4.0),
        style::SURFACE_1,
        Stroke::new(vb.w(1.6), style::VEHICLE),
    );
}

fn line_round(painter: &egui::Painter, a: Pos2, b: Pos2, width: f32, color: Color32) {
    painter.line_segment([a, b], Stroke::new(width, color));
    painter.circle_filled(a, width / 2.0, color);
    painter.circle_filled(b, width / 2.0, color);
}

/// The part of a circle of radius `r` around `c` on one side of the line
/// through `o` with direction `d` (`side` > 0 is to the left of `d`).
fn circle_cut(c: Pos2, r: f32, o: Pos2, d: Vec2, side: f32) -> Vec<Pos2> {
    let n = 96;
    let mut out = Vec::new();
    let inside = |p: Pos2| (d.x * (p.y - o.y) - d.y * (p.x - o.x)) * side >= 0.0;
    let pts: Vec<Pos2> = (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            c + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect();
    for i in 0..n {
        let (p, q) = (pts[i], pts[(i + 1) % n]);
        let (ip, iq) = (inside(p), inside(q));
        if ip {
            out.push(p);
        }
        if ip != iq {
            // Where the edge p-q crosses the line.
            let fp = d.x * (p.y - o.y) - d.y * (p.x - o.x);
            let fq = d.x * (q.y - o.y) - d.y * (q.x - o.x);
            let t = fp / (fp - fq);
            out.push(p + (q - p) * t);
        }
    }
    out
}

/// A segment cut to the inside of a circle, or nothing.
fn clip_to_circle(c: Pos2, r: f32, a: Pos2, b: Pos2) -> Option<(Pos2, Pos2)> {
    let d = b - a;
    let f = a - c;
    let (qa, qb, qc) = (d.dot(d), 2.0 * f.dot(d), f.dot(f) - r * r);
    let disc = qb * qb - 4.0 * qa * qc;
    if disc <= 0.0 || qa == 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let t0 = ((-qb - s) / (2.0 * qa)).max(0.0);
    let t1 = ((-qb + s) / (2.0 * qa)).min(1.0);
    (t0 < t1).then(|| (a + d * t0, a + d * t1))
}

/// The attitude indicator: a flat sky and ground cut by the horizon, which
/// rolls and pitches behind a fixed aircraft mark and roll scale.
fn attitude(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, pitch: f32, roll: f32) {
    let vb = ViewBox::new(rect);
    let p = pitch.clamp(-30.0, 30.0);
    let r = roll.clamp(-60.0, 60.0);
    painter.circle(
        vb.c,
        vb.w(88.0),
        style::tint(0.06),
        Stroke::new(vb.w(1.5), style::tint(0.16)),
    );
    // rotate(-roll) translate(0, pitch * PPD), in viewBox units.
    let rot = egui::emath::Rot2::from_angle((-r).to_radians());
    let tf = |x: f32, y: f32| vb.c + rot * (Vec2::new(x, y + p * PPD) * vb.s);
    let ball = vb.w(74.0);
    let o = tf(0.0, 0.0);
    let dir = rot * Vec2::X;
    let sky = circle_cut(vb.c, ball, o, dir, -1.0);
    let ground = circle_cut(vb.c, ball, o, dir, 1.0);
    if sky.len() > 2 {
        painter.add(Shape::convex_polygon(sky, style::ADI_SKY, Stroke::NONE));
    }
    if ground.len() > 2 {
        painter.add(Shape::convex_polygon(ground, style::ADI_GROUND, Stroke::NONE));
    }
    if let Some((a, b)) = clip_to_circle(vb.c, ball, tf(-900.0, 0.0), tf(900.0, 0.0)) {
        painter.line_segment([a, b], Stroke::new(vb.w(1.8), Color32::WHITE));
    }
    let mark = Color32::from_rgba_unmultiplied(255, 255, 255, 217);
    for pv in [10.0f32, 20.0, 30.0, -10.0, -20.0, -30.0] {
        let y = -pv * PPD;
        if let Some((a, b)) = clip_to_circle(vb.c, ball, tf(-22.0, y), tf(22.0, y)) {
            line_round(painter, a, b, vb.w(1.3), mark);
        }
        let label = format!("{}", pv.abs() as i32);
        for (lx, anchor_right) in [(27.0, false), (-27.0, true)] {
            let at = tf(lx, y);
            if (at - vb.c).length() > ball - vb.w(4.0) {
                continue;
            }
            let g = style::galley(ui, &label, vb.w(7.0), Weight::W500, mark, 0.0);
            let offset = Vec2::new(if anchor_right { -g.size().x } else { 0.0 }, -g.size().y / 2.0);
            painter.galley(at + offset, g, mark);
        }
    }
    painter.circle_stroke(vb.c, ball, Stroke::new(vb.w(1.0), style::tint(0.14)));
    for tick in [-60.0f32, -30.0, 0.0, 30.0, 60.0] {
        let zero = tick == 0.0;
        let (x1, y1) = polar(tick, if zero { 75.0 } else { 79.0 });
        let (x2, y2) = polar(tick, 86.0);
        let (color, width) = if zero {
            (style::TEXT_2, 2.2)
        } else {
            (style::TEXT_3, 1.6)
        };
        line_round(painter, vb.p(x1, y1), vb.p(x2, y2), vb.w(width), color);
    }
    let pointer: Vec<Pos2> = [(0.0, -73.0), (5.0, -64.0), (-5.0, -64.0)]
        .iter()
        .map(|&(x, y)| vb.c + rot * Vec2::new(x, y) * vb.s)
        .collect();
    painter.add(Shape::convex_polygon(pointer, style::ADI_MARK, Stroke::NONE));
    for (a, b) in [((-32.0, 0.0), (-13.0, 0.0)), ((32.0, 0.0), (13.0, 0.0))] {
        line_round(
            painter,
            vb.p(a.0, a.1),
            vb.p(b.0, b.1),
            vb.w(4.4),
            style::adi_mark_outline(),
        );
    }
    for (a, b) in [((-32.0, 0.0), (-13.0, 0.0)), ((32.0, 0.0), (13.0, 0.0))] {
        line_round(painter, vb.p(a.0, a.1), vb.p(b.0, b.1), vb.w(2.6), style::ADI_MARK);
    }
    painter.circle_filled(vb.c, vb.w(2.2), style::ADI_MARK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clock_and_angles() {
        assert_eq!(format_clock(0.0), "0:00");
        assert_eq!(format_clock(65.9), "1:05");
        assert_eq!(format_clock(3725.0), "1:02:05");
        assert_eq!(shortest_delta(350.0, 10.0), 20.0);
        assert_eq!(shortest_delta(10.0, 350.0), -20.0);
        assert_eq!(norm_angle(-30.0), 330.0);
    }

    #[test]
    fn flight_time_runs_in_the_air_and_holds_after() {
        let mut h = Hud::default();
        let ground = json!({"connected": true, "armed": true, "landed_state": 1});
        let air = json!({"connected": true, "armed": true, "landed_state": 2});
        let down = json!({"connected": true, "armed": false, "landed_state": 1});
        assert_eq!(h.flight_seconds(&ground, 0.0), 0.0);
        assert_eq!(h.flight_seconds(&air, 1000.0), 0.0);
        assert_eq!(h.flight_seconds(&air, 61000.0), 60.0);
        assert_eq!(h.flight_seconds(&down, 70000.0), 69.0);
        assert_eq!(h.flight_seconds(&down, 99000.0), 69.0, "the last flight stays shown");
        assert_eq!(h.flight_seconds(&air, 100000.0), 0.0, "a new take-off starts again");
    }

    #[test]
    fn the_cells_as_the_web_hud_writes_them() {
        let mut h = Hud::default();
        let s = json!({"connected": true, "armed": false, "altitude_amsl": 610.4, "groundspeed": 6.04,
                       "vspeed": -0.26, "heading": 66.4, "gps_satellites": 18});
        let cells = h.cells(&s, &Units::default(), 0.0);
        let shown: Vec<(&str, &str)> = cells.iter().map(|(l, v, _)| (*l, v.as_str())).collect();
        assert_eq!(
            shown,
            vec![
                ("ALT AMSL", "610 m"),
                ("FLIGHT TIME", "0:00"),
                ("GS", "6.0 m/s"),
                ("V/S", "-0.3 m/s"),
                ("HDG", "066°"),
                ("SAT", "18"),
            ]
        );
        let off = h.cells(&json!({"connected": false}), &Units::default(), 0.0);
        assert!(off.iter().all(|(_, v, _)| v == "\u{2014}"));
    }

    #[test]
    fn a_segment_is_cut_to_the_ball() {
        let c = Pos2::new(0.0, 0.0);
        let (a, b) = clip_to_circle(c, 10.0, Pos2::new(-20.0, 0.0), Pos2::new(20.0, 0.0)).unwrap();
        assert!((a.x + 10.0).abs() < 1e-4 && (b.x - 10.0).abs() < 1e-4);
        assert!(clip_to_circle(c, 10.0, Pos2::new(-20.0, 15.0), Pos2::new(20.0, 15.0)).is_none());
        let half = circle_cut(c, 10.0, c, Vec2::X, -1.0);
        assert!(half.iter().all(|p| p.y <= 1e-3), "sky is the upper half");
    }
}
