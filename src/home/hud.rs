//! The flight HUD at the bottom right of the map: compass, attitude
//! indicator and six readouts, drawn from the web HUD's own geometry
//! (python/src/js/instruments.js, the SVGs in index.html, flight.css).
//!
//! The dials are the web SVGs' 200-unit viewBox scaled to the width the CSS
//! gives them, so every tick, needle and label lands where it does there.
//! Heading, pitch and roll ease towards the telemetry with the same time
//! constants, so the needle sweeps rather than jumps.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::Value;

use crate::data::units::{js_round, js_to_fixed, Units};
use crate::home::mapwindow::{self, Placement};
use crate::home::topbar::is_airborne;
use crate::ui::style::{self, Weight};

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
    /// Where it sits and what is folded (`hud-panel.js`): the position and the
    /// pinned, compact, collapsed and readouts flags, in `~/.corvus/hud.json`.
    place: Placement,
    /// `ui.flight_hud`: on the map at all.
    pub visible: bool,
    /// `ui.hud_square`: square corners.
    pub square: bool,
    /// `ui.compass_nose_up`: the rose turns, the needle stays at the top.
    pub nose_up: bool,
}

/// How the panel is folded, from what the operator pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub compact: bool,
    pub readouts: bool,
    pub collapsed: bool,
    pub square: bool,
}

impl Look {
    /// `.is-square:not(.is-collapsed)` takes the 12 px frame.
    fn pad(&self) -> f32 {
        if self.square && !self.collapsed {
            12.0
        } else {
            PAD
        }
    }

    fn dial_gap(&self) -> f32 {
        if self.compact {
            8.0
        } else {
            DIAL_GAP
        }
    }

    /// The gap between the dials and the numbers.
    fn body_gap(&self) -> f32 {
        if self.compact {
            7.0
        } else {
            10.0
        }
    }

    /// `.is-compact`: smaller readout and cell text.
    fn readout_sizes(&self) -> (f32, f32) {
        if self.compact {
            (7.5, 11.0)
        } else {
            (8.5, 13.0)
        }
    }

    fn cell_sizes(&self) -> (f32, f32) {
        if self.compact {
            (7.5, 12.0)
        } else {
            (8.5, 15.0)
        }
    }

    /// The panel's width: `--hud-w`, 0.72 of it compact; the pill is as wide
    /// as its buttons.
    fn width(&self, hud_w: f32) -> f32 {
        if self.collapsed {
            3.0 * 20.0 + 2.0 + 2.0 * 5.0
        } else if self.compact {
            hud_w * 0.72
        } else {
            hud_w
        }
    }

    /// `--hud-corner` for the top corners.
    fn top_corner(&self, hud_w: f32, single: bool) -> u8 {
        if self.collapsed {
            9
        } else if self.square {
            style::RADIUS
        } else if single {
            (f32::from(style::RADIUS) + 14.0).round() as u8
        } else if self.compact {
            ((hud_w * 0.72 - 24.0) / 4.0 + 8.0).round() as u8
        } else {
            ((hud_w - 26.0) / 4.0 + 8.0).round() as u8
        }
    }
}

impl Hud {
    /// The panel as the station left it.
    pub fn new() -> Hud {
        Hud {
            place: Placement::load("hud.json"),
            visible: true,
            ..Default::default()
        }
    }

    fn look(&self) -> Look {
        Look {
            compact: self.place.flag("compact"),
            readouts: !self
                .place
                .extra
                .get("readouts")
                .is_some_and(|v| v == &Value::Bool(false)),
            collapsed: self.place.flag("collapsed"),
            square: self.square,
        }
    }

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
                ("ALT AMSL", d(), style::text_1()),
                ("FLIGHT TIME", d(), style::text_1()),
                ("GS", d(), style::nav()),
                ("V/S", d(), style::text_1()),
                ("HDG", d(), style::nav()),
                ("SAT", d(), style::text_1()),
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
                style::text_1(),
            ),
            (
                "FLIGHT TIME",
                format_clock(self.flight_seconds(s, now)),
                style::text_1(),
            ),
            (
                "GS",
                units.format_speed(num("groundspeed"), 1, false, false),
                style::nav(),
            ),
            (
                "V/S",
                units.format_speed(num("vspeed"), 1, true, false),
                style::text_1(),
            ),
            (
                "HDG",
                format!("{:03}\u{00B0}", js_round(num("heading")) as i64),
                style::nav(),
            ),
            ("SAT", sats, style::text_1()),
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

    /// The panel's height for the dial width the CSS gives it. Under 601 the
    /// two instruments stack, each as wide as the panel.
    pub fn height(ui: &egui::Ui, width: f32, single: bool, look: &Look) -> f32 {
        let pad = look.pad();
        if look.collapsed {
            return 20.0 + 2.0 * 5.0;
        }
        let readout = readout_height(ui, look);
        let cell = cell_height(ui, look);
        let block = |dial: f32| dial + 5.0 + readout;
        let instruments = if single {
            2.0 * block(width - 2.0 * pad - 2.0) + look.dial_gap()
        } else {
            block((width - 2.0 * pad - look.dial_gap()) / 2.0)
        };
        let numbers = if look.readouts {
            look.body_gap() + 1.0 + 3.0 * cell + 2.0 + 1.0 + 3.0
        } else {
            0.0
        };
        pad + instruments + numbers + pad
    }

    /// Draws the panel in `host` (the map): where it was left, or at the
    /// bottom right `inset` from the edges. `hud_w` is `--hud-w`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        host: Rect,
        inset: f32,
        state: &Value,
        units: &Units,
        now: f64,
        hud_w: f32,
        single: bool,
    ) {
        if !self.visible {
            return;
        }
        let look = self.look();
        let pinned = self.place.flag("pinned");
        let width = look.width(hud_w);
        let size = Vec2::new(width, Hud::height(ui, width, single, &look));
        let default = mapwindow::corner(host, size, None, Some(inset), None, Some(inset));
        let rect = self.place.rect(host, size, default);
        let painter = ui.painter().clone();
        let top = look.top_corner(hud_w, single);
        let radius = CornerRadius {
            nw: top,
            ne: top,
            sw: if look.collapsed { 9 } else { style::RADIUS },
            se: if look.collapsed { 9 } else { style::RADIUS },
        };
        style::glass_panel(&painter, "hud", rect, radius, 0.6);
        // The whole panel is the drag handle, and takes the pointer so the map
        // under it does not pan. Pinned, it only takes the pointer. The
        // buttons are made after it, so they win where they overlap.
        if pinned {
            ui.interact(rect, ui.scope_id().with("hud"), egui::Sense::click_and_drag());
        } else if self.place.drag(ui, ui.scope_id().with("hud"), rect, rect, host) {
            ui.ctx().request_repaint();
        }

        if !look.collapsed {
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            let ((h, p, r), moving) = self.ease(state, dt);
            if moving {
                ui.ctx().request_repaint();
            }
            self.body(ui, &painter, rect, state, units, now, (h, p, r), single, &look);
        }
        self.controls(ui, rect, &look, pinned);
    }

    /// The two dials, their readouts and the numbers.
    #[allow(clippy::too_many_arguments)]
    fn body(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: Rect,
        state: &Value,
        units: &Units,
        now: f64,
        (h, p, r): (f32, f32, f32),
        single: bool,
        look: &Look,
    ) {
        let pad = look.pad();
        let gap = look.dial_gap();
        let width = rect.width();
        let dial = if single {
            width - 2.0 * pad - 2.0
        } else {
            (width - 2.0 * pad - gap) / 2.0
        };
        let left = Rect::from_min_size(rect.min + Vec2::splat(pad), Vec2::splat(dial));
        let right = if single {
            left.translate(Vec2::new(0.0, dial + 5.0 + readout_height(ui, look) + gap))
        } else {
            left.translate(Vec2::new(dial + gap, 0.0))
        };
        let shown_h = (js_round(f64::from(norm_angle(h))) as i64) % 360;
        compass(ui, painter, left, shown_h as f32, self.nose_up);
        attitude(ui, painter, right, p, r);
        let sign = |v: f32| if v >= 0.0 { "+" } else { "" };
        let att = format!(
            "P {}{}\u{00B0}  R {}{}\u{00B0}",
            sign(p),
            js_to_fixed(f64::from(p), 1),
            sign(r),
            js_to_fixed(f64::from(r), 1)
        );
        readout(ui, painter, left, "HEADING", &format!("{shown_h:03}\u{00B0}"), look);
        readout(ui, painter, right, "ATTITUDE", &att, look);
        if !look.readouts {
            return;
        }

        let grid_top = right.bottom() + 5.0 + readout_height(ui, look) + look.body_gap();
        let grid = Rect::from_min_max(
            Pos2::new(rect.left() + pad + 3.0, grid_top),
            Pos2::new(rect.right() - pad - 3.0, rect.bottom() - pad - 3.0),
        );
        painter.rect_filled(grid, style::RADIUS_BTN, style::border_soft());
        painter.rect_stroke(
            grid,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        let inner = grid.shrink(1.0);
        let cw = (inner.width() - 1.0) / 2.0;
        let ch = cell_height(ui, look);
        let (label_size, value_size) = look.cell_sizes();
        let (px, py) = if look.compact { (7.0, 5.0) } else { (9.0, 7.0) };
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
            let l = style::galley(ui, label, label_size, Weight::W600, style::text_3(), 0.7);
            let v = style::galley(ui, &value, value_size, Weight::W600, color, 0.0);
            let lh = l.size().y;
            painter.galley(Pos2::new(cell.left() + px, cell.top() + py), l, style::text_3());
            let vy = cell.top() + py + lh + 2.0 + (value_size * 1.1 - v.size().y) / 2.0;
            painter.galley(Pos2::new(cell.left() + px, vy), v, color);
        }
    }

    /// The four buttons: on the top edge, shown while the pointer is over the
    /// panel; in the pill, always.
    fn controls(&mut self, ui: &mut egui::Ui, rect: Rect, look: &Look, pinned: bool) {
        let n = if look.collapsed { 3.0 } else { 4.0 };
        let bar_w = n * 20.0 + (n - 1.0) + if look.collapsed { 0.0 } else { 2.0 * 2.0 + 2.0 };
        let bar = if look.collapsed {
            Rect::from_min_size(rect.min + Vec2::splat(5.0), Vec2::new(bar_w, 20.0))
        } else {
            Rect::from_center_size(
                Pos2::new(rect.center().x, rect.top() - 10.0 + 13.0),
                Vec2::new(bar_w, 26.0),
            )
        };
        let hovered = ui.rect_contains_pointer(rect.union(bar));
        // A finger never hovers, so the buttons stay.
        let fade = if look.collapsed || crate::app::touch::on() {
            1.0
        } else {
            ui.ctx()
                .animate_bool_with_time(ui.scope_id().with("hudbar"), hovered, 0.14)
        };
        if fade <= 0.0 {
            return;
        }
        let painter = ui.painter().clone();
        if !look.collapsed {
            painter.rect_filled(bar, 7, style::glass(0.55 * fade));
            painter.rect_stroke(
                bar,
                7,
                Stroke::new(1.0, style::tint(0.06 * fade)),
                egui::StrokeKind::Inside,
            );
        }
        let origin = if look.collapsed {
            bar.min
        } else {
            bar.min + Vec2::new(1.0 + 2.0, 1.0 + 2.0)
        };
        let mut x = origin.x;
        let mut button = |ui: &mut egui::Ui, icon: &'static str, title: &str, active: bool, id: &'static str| {
            let r = Rect::from_min_size(Pos2::new(x, origin.y), Vec2::splat(20.0));
            x += 21.0;
            crate::ui::kit::icon_btn(ui, r, icon, title, active, ("hudbtn", id))
        };
        if button(
            ui,
            if pinned { "pin-off" } else { "pin" },
            if pinned { "Unlock position" } else { "Lock position" },
            pinned,
            "pin",
        ) {
            self.place.set_flag("pinned", !pinned);
        }
        if button(
            ui,
            if look.compact { "maximize-2" } else { "minimize-2" },
            if look.compact { "Full size" } else { "Compact size" },
            look.compact,
            "size",
        ) {
            self.place.set_flag("compact", !look.compact);
        }
        if !look.collapsed
            && button(
                ui,
                if look.readouts {
                    "chevrons-down-up"
                } else {
                    "chevrons-up-down"
                },
                if look.readouts {
                    "Hide the readouts"
                } else {
                    "Show the readouts"
                },
                !look.readouts,
                "readouts",
            )
        {
            self.place.set_flag("readouts", !look.readouts);
        }
        if button(
            ui,
            if look.collapsed { "chevron-up" } else { "chevron-down" },
            if look.collapsed { "Expand" } else { "Collapse" },
            false,
            "collapse",
        ) {
            self.place.set_flag("collapsed", !look.collapsed);
        }
    }
}

fn readout_height(ui: &egui::Ui, look: &Look) -> f32 {
    let (label, value) = look.readout_sizes();
    let l = style::galley(ui, "HEADING", label, Weight::W600, style::text_3(), 0.9)
        .size()
        .y;
    let v = style::galley(ui, "000", value, Weight::W600, style::text_1(), 0.0)
        .size()
        .y;
    l + 1.0 + v
}

fn cell_height(ui: &egui::Ui, look: &Look) -> f32 {
    let (label, value) = look.cell_sizes();
    let l = style::galley(ui, "ALT", label, Weight::W600, style::text_3(), 0.7)
        .size()
        .y;
    let py = if look.compact { 5.0 } else { 7.0 };
    py + l + 2.0 + value * 1.1 + py
}

fn readout(ui: &egui::Ui, painter: &egui::Painter, dial: Rect, label: &str, value: &str, look: &Look) {
    let (label_size, value_size) = look.readout_sizes();
    let l = style::galley(
        ui,
        label,
        label_size,
        Weight::W600,
        style::text_3(),
        if look.compact { 0.7 } else { 0.9 },
    );
    let v = style::galley(ui, value, value_size, Weight::W600, style::text_1(), 0.0);
    let y = dial.bottom() + 5.0;
    painter.galley(
        Pos2::new(dial.center().x - l.size().x / 2.0, y),
        l.clone(),
        style::text_3(),
    );
    painter.galley(
        Pos2::new(dial.center().x - v.size().x / 2.0, y + l.size().y + 1.0),
        v,
        style::text_1(),
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
fn compass(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, heading: f32, nose_up: bool) {
    let vb = ViewBox::new(rect);
    // Nose up: the rose turns by minus the heading and the needle points to
    // the top; otherwise north stays up and the needle turns in the rose.
    let (turn, heading) = if nose_up { (-heading, 0.0) } else { (0.0, heading) };
    painter.circle(
        vb.c,
        vb.w(88.0),
        style::tint(0.06),
        Stroke::new(vb.w(1.5), style::tint(0.16)),
    );
    painter.circle(
        vb.c,
        vb.w(74.0),
        style::input(0.55),
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
            (style::text_2(), 2.2)
        } else if major {
            (style::text_3(), 1.6)
        } else if medium {
            (style::tint(0.34), 1.1)
        } else {
            (style::tint(0.22), 0.9)
        };
        let (x1, y1) = polar(deg as f32 + turn, r1);
        let (x2, y2) = polar(deg as f32 + turn, 86.0);
        line_round(painter, vb.p(x1, y1), vb.p(x2, y2), vb.w(width), color);
    }
    for (d, t) in [(0.0, "N"), (90.0, "E"), (180.0, "S"), (270.0, "W")] {
        let (x, y) = polar(d + turn, 60.0);
        let color = if t == "N" { style::nav() } else { style::text_2() };
        text_centered(ui, painter, vb.p(x, y), t, vb.w(13.0), Weight::W700, color, vb.w(0.3));
    }
    for d in [30, 60, 120, 150, 210, 240, 300, 330] {
        let (x, y) = polar(d as f32 + turn, 60.0);
        text_centered(
            ui,
            painter,
            vb.p(x, y),
            &format!("{:02}", d / 10),
            vb.w(7.5),
            Weight::W600,
            style::text_3(),
            0.0,
        );
    }
    let rot = egui::emath::Rot2::from_angle(heading.to_radians());
    let needle =
        |pts: &[(f32, f32)]| -> Vec<Pos2> { pts.iter().map(|&(x, y)| vb.c + rot * Vec2::new(x, y) * vb.s).collect() };
    painter.add(Shape::convex_polygon(
        needle(&[(0.0, 43.0), (5.0, 9.0), (0.0, 3.0), (-5.0, 9.0)]),
        style::rgba(style::vehicle_rgb(), 0.28),
        Stroke::NONE,
    ));
    painter.add(Shape::convex_polygon(
        needle(&[(0.0, -50.0), (7.0, -9.0), (0.0, -3.0), (-7.0, -9.0)]),
        style::vehicle(),
        Stroke::NONE,
    ));
    painter.circle(
        vb.c,
        vb.w(4.0),
        style::surface_1(),
        Stroke::new(vb.w(1.6), style::vehicle()),
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
        painter.add(Shape::convex_polygon(sky, style::adi_sky(), Stroke::NONE));
    }
    if ground.len() > 2 {
        painter.add(Shape::convex_polygon(ground, style::adi_ground(), Stroke::NONE));
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
            (style::text_2(), 2.2)
        } else {
            (style::text_3(), 1.6)
        };
        line_round(painter, vb.p(x1, y1), vb.p(x2, y2), vb.w(width), color);
    }
    let pointer: Vec<Pos2> = [(0.0, -73.0), (5.0, -64.0), (-5.0, -64.0)]
        .iter()
        .map(|&(x, y)| vb.c + rot * Vec2::new(x, y) * vb.s)
        .collect();
    painter.add(Shape::convex_polygon(pointer, style::adi_mark(), Stroke::NONE));
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
        line_round(painter, vb.p(a.0, a.1), vb.p(b.0, b.1), vb.w(2.6), style::adi_mark());
    }
    painter.circle_filled(vb.c, vb.w(2.2), style::adi_mark());
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    fn look() -> super::Look {
        super::Look {
            compact: false,
            readouts: true,
            collapsed: false,
            square: false,
        }
    }

    #[test]
    fn compact_is_72_percent_with_the_corner_to_match() {
        let full = look();
        let compact = super::Look { compact: true, ..full };
        assert_eq!(full.width(320.0), 320.0);
        assert!((compact.width(320.0) - 230.4).abs() < 1e-3);
        // calc((var(--hud-w) - 26px) / 4 + 8px), and the compact 0.72 form.
        assert_eq!(full.top_corner(320.0, false), 82);
        assert_eq!(compact.top_corner(320.0, false), 60);
        assert_eq!(full.top_corner(240.0, true), 22);
    }

    #[test]
    fn square_takes_the_radius_and_a_wider_frame_until_collapsed() {
        let square = super::Look { square: true, ..look() };
        assert_eq!(square.top_corner(320.0, false), super::style::RADIUS);
        assert_eq!(square.pad(), 12.0);
        let pill = super::Look {
            collapsed: true,
            ..square
        };
        assert_eq!(pill.pad(), 8.0);
        assert_eq!(pill.top_corner(320.0, false), 9);
        assert_eq!(pill.width(320.0), 72.0);
    }

    #[test]
    fn the_panel_remembers_what_was_folded() {
        let dir = std::env::temp_dir().join(format!("corvus-hud-{}", std::process::id()));
        let path = dir.join("hud.json");
        let mut p = super::Placement::from_path(Some(path.clone()));
        p.set_flag("compact", true);
        p.set_flag("readouts", false);
        let h = super::Hud {
            place: super::Placement::from_path(Some(path)),
            ..Default::default()
        };
        let l = h.look();
        assert!(l.compact && !l.readouts && !l.collapsed);
        // Absent means shown: a panel saved before the control existed.
        let fresh = super::Hud::default();
        assert!(fresh.look().readouts);
        let _ = std::fs::remove_dir_all(dir);
        let _: Value = Value::Null;
    }
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
