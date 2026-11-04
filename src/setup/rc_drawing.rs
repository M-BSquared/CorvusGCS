//! The drawn transmitter (`python/src/js/rc-transmitter.js` `create`, the `.rc-tx-*`
//! rules of setup.css): a line drawing of a twin stick handset moved by the
//! live RC_CHANNELS stream, every bindable control named by a leader line out
//! to the margin. It shows what the calibration wizard is asking for, tests
//! whether a control reaches the vehicle, and binds a drawn control to the
//! channel it sends on (learned by moving it).
//!
//! The drawing follows the operator's hand, not the pulse: a channel the
//! vehicle knows is reversed is inverted here. Nothing eases except the sticks
//! and knobs, and those only across one sample interval, because a switch
//! handle that eases into its position disagrees with the thumb for 150 ms.

use std::collections::BTreeMap;

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::Value;

use crate::setup::rc_transmitter::{
    self as model, CalloutSide, Control, Gimbal, Glide, Kind, Store, KNOB_SPAN, LEARN_TIMEOUT_MS, STICK_R,
};
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::style::{self, Weight};

const VIEW_W: f64 = 880.0;
const VIEW_H: f64 = 650.0;
const MAX_W: f32 = 620.0;
const SANS: f32 = 0.969;
const MONO: f32 = 1.02;

/// What the page wants pointed at (`setPrompt`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prompt {
    pub controls: Vec<String>,
    pub gimbal: Option<&'static str>,
    pub direction: Option<&'static str>,
    pub swept: Vec<String>,
}

/// A channel's calibrated endpoints, from the vehicle.
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub min: f64,
    pub max: f64,
    pub reversed: bool,
}

/// An `RC_MAP_*` field the inspector can assign a channel to.
#[derive(Debug, Clone)]
pub struct MapField {
    pub param: String,
    pub label: String,
    pub value: f64,
}

/// What the inspector asked the page to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Map { param: String, channel: u32 },
}

struct Learn {
    control: String,
    baseline: Vec<f64>,
    deadline: f64,
}

pub struct Transmitter {
    pub store: Store,
    sticks: BTreeMap<String, u32>,
    info: BTreeMap<u32, Info>,
    channels: Vec<f64>,
    live: bool,
    rssi: f64,
    armed: bool,
    selected: Option<String>,
    pub prompt: Prompt,
    learn: Option<Learn>,
    learn_status: Option<(&'static str, String)>,
    glide: Glide,
    now: f64,
}

impl Transmitter {
    pub fn new(store: Store) -> Transmitter {
        Transmitter {
            store,
            sticks: BTreeMap::new(),
            info: BTreeMap::new(),
            channels: Vec::new(),
            live: false,
            rssi: -1.0,
            armed: false,
            selected: None,
            prompt: Prompt::default(),
            learn: None,
            learn_status: None,
            glide: Glide::default(),
            now: 0.0,
        }
    }

    pub fn mode(&self) -> u8 {
        self.store.mode
    }

    pub fn set_mode(&mut self, mode: u8) {
        self.store.mode = if (1..=4).contains(&mode) {
            mode
        } else {
            model::DEFAULT_MODE
        };
        self.store.save();
    }

    /// Drops every learned binding (a new transmitter, not a new vehicle).
    pub fn forget_all(&mut self) {
        self.store.bindings.clear();
        self.store.save();
    }

    /// The vehicle's own view of the radio: the channel of each stick role,
    /// and the endpoints of each calibrated channel.
    pub fn set_doc(&mut self, sticks: BTreeMap<String, u32>, info: BTreeMap<u32, Info>) {
        self.sticks = sticks;
        self.info = info;
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The channel of a control: stick axes come from the vehicle's own
    /// `RC_MAP_*` through the mode, everything else is learned. 0 for none.
    pub fn channel_of(&self, id: &str) -> u32 {
        let Some(c) = model::control(id) else { return 0 };
        if c.kind == Kind::Axis {
            if let Some(ch) = model::role_of(self.store.mode, id).and_then(|r| self.sticks.get(r)) {
                if *ch >= 1 {
                    return *ch;
                }
            }
        }
        self.store.bindings.get(id).map_or(0, |b| b.channel)
    }

    pub fn positions_of(&self, id: &str) -> u32 {
        let c = model::control(id);
        self.store
            .bindings
            .get(id)
            .and_then(|b| b.positions)
            .filter(|p| *p >= 2)
            .or(c.map(|c| c.positions))
            .unwrap_or(2)
    }

    /// A control's 0 to 1 travel, `None` without a channel or a signal.
    pub fn travel_of(&self, id: &str) -> Option<f64> {
        let ch = self.channel_of(id);
        if ch == 0 || !self.live {
            return None;
        }
        let pulse = *self.channels.get(ch as usize - 1)?;
        if pulse <= 0.0 {
            return None;
        }
        let info = self.info.get(&ch);
        let mut f = model::fraction_of(pulse, info.map(|i| i.min), info.map(|i| i.max));
        if info.is_some_and(|i| i.reversed) {
            f = 1.0 - f;
        }
        Some(f)
    }

    /// `{control id: channel}` of every control that has one.
    pub fn channel_map(&self) -> BTreeMap<String, u32> {
        model::layout()
            .controls
            .iter()
            .filter_map(|c| {
                let ch = self.channel_of(&c.id);
                (ch > 0).then(|| (c.id.clone(), ch))
            })
            .collect()
    }

    /// One line for a control (`describe`).
    pub fn describe(&self, id: &str) -> String {
        let ch = self.channel_of(id);
        if ch == 0 {
            return "not bound".into();
        }
        let Some(t) = self.travel_of(id) else {
            return format!("CH{ch} \u{b7} no signal");
        };
        let Some(c) = model::control(id) else {
            return String::new();
        };
        if c.kind == Kind::Switch {
            let n = self.positions_of(id);
            format!(
                "CH{ch} \u{b7} {}",
                model::position_label(model::position_index(t, f64::from(n)), n)
            )
        } else {
            format!("CH{ch} \u{b7} {}%", crate::data::units::js_round(t * 100.0))
        }
    }

    /// Takes the latest telemetry; `now` in milliseconds.
    pub fn update(&mut self, state: &Value, now: f64) {
        self.now = now;
        self.channels = state
            .get("rc_channels")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
            .unwrap_or_default();
        self.live = state.get("rc_live").is_some_and(|v| v == &Value::Bool(true)) && !self.channels.is_empty();
        self.rssi = state.get("rc_rssi").and_then(Value::as_f64).unwrap_or(-1.0);
        self.armed = state.get("armed") == Some(&Value::Bool(true));
        for c in &model::layout().controls {
            if c.kind == Kind::Knob {
                self.glide.set(&c.id, self.travel_of(&c.id), now);
            }
        }
        for g in ["left", "right"] {
            for a in ["x", "y"] {
                let key = format!("{g}_{a}");
                self.glide.set(&key, self.travel_of(&key), now);
            }
        }
        self.glide.commit(now);
        if let Some(l) = &self.learn {
            if let Some(ch) = model::moved_channel(&l.baseline, &self.channels) {
                let id = l.control.clone();
                self.finish_learn(&id, ch as u32);
            } else if now > l.deadline {
                self.cancel_learn("nothing moved");
            }
        }
    }

    /// Whether the figure still moves (sticks and knobs easing).
    pub fn moving(&self) -> bool {
        self.glide.moving(self.now)
    }

    fn start_learn(&mut self, id: &str) {
        if self.learn.is_some() {
            return;
        }
        if !self.live || self.channels.is_empty() {
            self.learn_status = Some(("err", "no RC signal".into()));
            return;
        }
        self.learn_status = None;
        self.learn = Some(Learn {
            control: id.into(),
            baseline: self.channels.clone(),
            deadline: self.now + LEARN_TIMEOUT_MS,
        });
    }

    fn finish_learn(&mut self, id: &str, channel: u32) {
        self.learn = None;
        let positions = self.store.bindings.get(id).and_then(|b| b.positions);
        self.store
            .bindings
            .insert(id.into(), model::Binding { channel, positions });
        self.store.save();
        self.learn_status = Some(("ok", format!("channel {channel}")));
    }

    fn cancel_learn(&mut self, reason: &str) {
        if self.learn.take().is_some() && !reason.is_empty() {
            self.learn_status = Some(("err", reason.into()));
        }
    }

    fn select(&mut self, id: &str) {
        self.selected = if self.selected.as_deref() == Some(id) {
            None
        } else {
            Some(id.into())
        };
        self.cancel_learn("");
        self.learn_status = None;
    }

    /// Leaves the page: the pending Learn ends with it.
    pub fn leave(&mut self) {
        self.cancel_learn("");
        self.selected = None;
    }

    /// Draws the figure in the column `x .. x + w` from `y`; returns the
    /// bottom. Interactive: the controls and their labels can be selected.
    pub fn figure(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, interactive: bool) -> f32 {
        let fig_w = w.min(MAX_W);
        let k = fig_w / VIEW_W as f32;
        let (ox, oy) = (x + (w - fig_w) / 2.0, y);
        let fig_h = VIEW_H as f32 * k;
        let ctx = ui.ctx().clone();
        let p = Paint { ui, ox, oy, k };
        self.paint_shell(&p);
        let l = model::layout();
        // the controls, then the callouts, so a leader line is never hidden
        for c in &l.controls {
            match c.kind {
                Kind::Axis => {}
                Kind::Switch => self.paint_switch(&p, c),
                Kind::Knob => self.paint_knob(&p, c),
            }
        }
        for (name, g) in [("left", l.left), ("right", l.right)] {
            self.paint_gimbal(&p, name, g);
        }
        // gimbals are drawn after the shell and before the controls in the
        // web page, but nothing overlaps them, so the order above is the
        // visible one; the callouts follow all of it
        for c in &l.controls {
            self.paint_callout(&p, c);
        }
        if interactive {
            let mut clicked: Option<String> = None;
            for c in &l.controls {
                let mut rects = vec![callout_box(c)];
                match c.kind {
                    Kind::Switch => rects.push([c.cx - 22.0, c.cy - 22.0, 44.0, 44.0]),
                    Kind::Knob => rects.push([c.cx - c.r, c.cy - c.r, 2.0 * c.r, 2.0 * c.r]),
                    Kind::Axis => {}
                }
                for (i, r) in rects.iter().enumerate() {
                    let rect = Rect::from_min_size(p.at(r[0], r[1]), Vec2::new(r[2] as f32 * k, r[3] as f32 * k));
                    let resp = ui.interact(rect, ui.scope_id().with(("rctx", &c.id, i)), egui::Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        clicked = Some(c.id.clone());
                    }
                }
            }
            if let Some(id) = clicked {
                self.select(&id);
            }
        }
        if self.moving() || self.learn.is_some() {
            ctx.request_repaint();
        }
        fig_h
    }

    fn state_of(&self, c: &Control) -> CtlState {
        let travel = self.travel_of(&c.id);
        let index = travel.map(|t| model::position_index(t, f64::from(self.positions_of(&c.id))));
        let on = match (c.kind, travel, index) {
            (_, None, _) => false,
            (Kind::Switch, _, Some(i)) => i > 0,
            (Kind::Knob, Some(t), _) => (t - 0.5).abs() > 0.1,
            _ => false,
        };
        CtlState {
            bound: self.channel_of(&c.id) > 0,
            signal: travel.is_some(),
            index,
            on,
            selected: self.selected.as_deref() == Some(c.id.as_str()),
            prompted: self.prompt.controls.contains(&c.id),
            swept: self.prompt.swept.contains(&c.id),
        }
    }

    fn paint_shell(&self, p: &Paint) {
        let l = model::layout();
        let raw = &l.raw;
        let fv = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
        let rect = |v: &Value| Rect::from_min_size(p.at(fv(v, "x"), fv(v, "y")), p.sz(fv(v, "w"), fv(v, "h")));
        let line = |w: f64| Stroke::new(p.s(w), style::text_3());
        for key in ["mast", "hinge"] {
            let v = &raw["antenna"][key];
            p.rect(rect(v), fv(v, "rx"), style::surface_2(), line(2.5));
        }
        let chin = &raw["chin"];
        p.rect(rect(chin), fv(chin, "rx"), style::surface_2(), line(2.5));
        let body = body_points(p);
        p.ui.painter().add(Shape::Path(egui::epaint::PathShape {
            points: body,
            closed: true,
            fill: style::surface_1(),
            stroke: egui::epaint::PathStroke::new(p.s(3.0), style::text_3()),
        }));
        let b = &raw["brand"];
        p.text(
            b["label"].as_str().unwrap_or(""),
            21.0,
            Weight::W700,
            style::text_3(),
            4.0,
            fv(b, "x"),
            fv(b, "y"),
            Align::Mid,
            false,
        );
        // the screen
        let sc = &raw["screen"];
        let (sx, sy, sw, sh) = (fv(sc, "x"), fv(sc, "y"), fv(sc, "w"), fv(sc, "h"));
        p.rect(
            Rect::from_min_size(p.at(sx - 7.0, sy - 7.0), p.sz(sw + 14.0, sh + 14.0)),
            10.0,
            style::surface_3(),
            Stroke::new(p.s(1.5), style::border()),
        );
        p.rect(
            Rect::from_min_size(p.at(sx, sy), p.sz(sw, sh)),
            4.0,
            style::terminal_bg(),
            Stroke::new(p.s(1.5), style::border_strong()),
        );
        p.text(
            "CORVUS GCS",
            12.0,
            Weight::W400,
            style::text_3(),
            2.0,
            sx + sw / 2.0,
            sy + 24.0,
            Align::Mid,
            true,
        );
        let line1 = if self.live {
            format!("{} CH", self.channels.len())
        } else {
            "NO SIGNAL".into()
        };
        p.text(
            &line1,
            22.0,
            Weight::W400,
            style::text_1(),
            1.0,
            sx + sw / 2.0,
            sy + 54.0,
            Align::Mid,
            true,
        );
        if self.live && self.rssi >= 0.0 {
            p.text(
                &format!("RSSI {}%", crate::data::units::js_number(self.rssi)),
                13.0,
                Weight::W400,
                style::text_2(),
                0.0,
                sx + sw / 2.0,
                sy + 74.0,
                Align::Mid,
                true,
            );
        }
        // the menu pad and the wheel: inert
        let nav = &raw["nav"];
        let (ncx, ncy, nr) = (fv(nav, "cx"), fv(nav, "cy"), fv(nav, "r"));
        let flat = Stroke::new(p.s(1.5), style::border_strong());
        p.circle(ncx, ncy, nr, style::surface_2(), flat);
        p.circle(
            ncx,
            ncy,
            11.0,
            style::surface_3(),
            Stroke::new(p.s(1.5), style::border()),
        );
        let labels = nav["labels"].as_array().cloned().unwrap_or_default();
        let lab = |i: usize| labels.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        p.text(
            &lab(0),
            9.0,
            Weight::W700,
            style::text_3(),
            0.6,
            ncx,
            ncy - 21.0,
            Align::Mid,
            false,
        );
        p.text(
            &lab(2),
            9.0,
            Weight::W700,
            style::text_3(),
            0.6,
            ncx,
            ncy + 29.0,
            Align::Mid,
            false,
        );
        let wh = &raw["wheel"];
        let (wcx, wcy, wr) = (fv(wh, "cx"), fv(wh, "cy"), fv(wh, "r"));
        p.circle(wcx, wcy, wr, style::surface_2(), flat);
        for i in 0..24 {
            let a = f64::from(i) / 24.0 * std::f64::consts::TAU;
            p.line(
                [wcx + a.cos() * (wr - 10.0), wcy + a.sin() * (wr - 10.0)],
                [wcx + a.cos() * (wr - 2.0), wcy + a.sin() * (wr - 2.0)],
                Stroke::new(p.s(1.5), style::border_strong()),
            );
        }
        p.circle(
            wcx,
            wcy,
            17.0,
            style::surface_3(),
            Stroke::new(p.s(1.5), style::border()),
        );
        // trims: drawn only
        for t in raw["trims"].as_array().cloned().unwrap_or_default() {
            let (tx, ty, tw, th) = (fv(&t, "x"), fv(&t, "y"), fv(&t, "w"), fv(&t, "h"));
            p.rect(
                Rect::from_min_size(p.at(tx, ty), p.sz(tw, th)),
                tw.min(th) / 2.0,
                style::surface_2(),
                Stroke::new(p.s(1.0), style::border()),
            );
            let (cx, cy) = (tx + tw / 2.0, ty + th / 2.0);
            let mark = style::text_3();
            let tri = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
                p.ui.painter().add(Shape::convex_polygon(
                    vec![p.at(a[0], a[1]), p.at(b[0], b[1]), p.at(c[0], c[1])],
                    mark,
                    Stroke::NONE,
                ));
            };
            let thumb_stroke = Stroke::new(p.s(1.0), style::border_strong());
            if t["axis"] == "v" {
                tri([cx - 4.0, ty + 12.0], [cx + 4.0, ty + 12.0], [cx, ty + 5.0]);
                tri(
                    [cx - 4.0, ty + th - 11.0],
                    [cx + 4.0, ty + th - 11.0],
                    [cx, ty + th - 4.0],
                );
                p.rect(
                    Rect::from_min_size(p.at(tx + 3.0, cy - 7.0), p.sz(tw - 6.0, 14.0)),
                    4.0,
                    style::surface_3(),
                    thumb_stroke,
                );
            } else {
                tri([tx + 13.0, cy - 4.0], [tx + 13.0, cy + 4.0], [tx + 6.0, cy]);
                tri(
                    [tx + tw - 13.0, cy - 4.0],
                    [tx + tw - 13.0, cy + 4.0],
                    [tx + tw - 6.0, cy],
                );
                p.rect(
                    Rect::from_min_size(p.at(cx - 7.0, ty + 3.0), p.sz(14.0, th - 6.0)),
                    4.0,
                    style::surface_3(),
                    thumb_stroke,
                );
            }
        }
        let m = &raw["modeText"];
        p.text(
            &format!("MODE {}", self.store.mode),
            13.0,
            Weight::W400,
            style::text_3(),
            1.5,
            fv(m, "x"),
            fv(m, "y"),
            Align::Mid,
            true,
        );
    }

    fn paint_gimbal(&self, p: &Paint, name: &str, g: Gimbal) {
        let sig_x = self.glide.get(&format!("{name}_x"), self.now);
        let sig_y = self.glide.get(&format!("{name}_y"), self.now);
        let signal = self.travel_of(&format!("{name}_x")).is_some() || self.travel_of(&format!("{name}_y")).is_some();
        let prompted = self.prompt.controls.iter().any(|c| c.starts_with(name));
        let boot = if prompted {
            Stroke::new(p.s(3.5), style::nav())
        } else {
            Stroke::new(p.s(2.0), style::border_strong())
        };
        p.circle(g.cx, g.cy, g.r, style::surface_2(), boot);
        p.circle(
            g.cx,
            g.cy,
            g.r - 13.0,
            style::surface_3(),
            Stroke::new(p.s(1.0), style::border()),
        );
        for i in 0..40 {
            let a = f64::from(i) / 40.0 * std::f64::consts::TAU;
            p.line(
                [g.cx + a.cos() * (g.r - 22.0), g.cy + a.sin() * (g.r - 22.0)],
                [g.cx + a.cos() * (g.r - 15.0), g.cy + a.sin() * (g.r - 15.0)],
                Stroke::new(p.s(1.5), style::border_strong().gamma_multiply(0.55)),
            );
        }
        p.circle(g.cx, g.cy, 4.0, style::border_strong(), Stroke::NONE);
        // the wizard's arrow, inside the well
        if let (Some(gim), Some(dir)) = (self.prompt.gimbal, self.prompt.direction) {
            if gim == name {
                let step = match dir {
                    "up" => [0.0, -1.0],
                    "down" => [0.0, 1.0],
                    "left" => [-1.0, 0.0],
                    _ => [1.0, 0.0],
                };
                let reach = g.r - 22.0;
                let tip = [g.cx + step[0] * reach, g.cy + step[1] * reach];
                let barb = 15.0;
                let base = [tip[0] - step[0] * barb, tip[1] - step[1] * barb];
                let perp = [step[1] * barb, step[0] * barb];
                let st = Stroke::new(p.s(5.0), style::nav());
                p.line([g.cx, g.cy], tip, st);
                p.line([base[0] - perp[0], base[1] - perp[1]], tip, st);
                p.line(tip, [base[0] + perp[0], base[1] + perp[1]], st);
            }
        }
        let reach = g.r - STICK_R - 8.0;
        let dx = sig_x.map_or(0.0, |f| (f * 2.0 - 1.0) * reach);
        let dy = sig_y.map_or(0.0, |f| -(f * 2.0 - 1.0) * reach);
        let (cx, cy) = (g.cx + dx, g.cy + dy);
        let (shaft, rim, rim_line, knurl, dish, dish_line) = if signal {
            (
                style::accent_pressed(),
                style::accent(),
                style::accent_pressed(),
                style::accent_pressed(),
                style::accent_hover(),
                style::accent_pressed(),
            )
        } else {
            (
                style::border_strong(),
                style::surface_1(),
                style::text_3(),
                style::text_3(),
                style::surface_2(),
                style::border_strong(),
            )
        };
        if dx != 0.0 || dy != 0.0 {
            p.round_line([g.cx, g.cy], [cx, cy], Stroke::new(p.s(11.0), shaft));
        }
        p.circle(cx, cy, STICK_R, rim, Stroke::new(p.s(2.5), rim_line));
        for i in 0..20 {
            let a = f64::from(i) / 20.0 * std::f64::consts::TAU;
            p.line(
                [cx + a.cos() * (STICK_R - 7.0), cy + a.sin() * (STICK_R - 7.0)],
                [cx + a.cos() * (STICK_R - 1.5), cy + a.sin() * (STICK_R - 1.5)],
                Stroke::new(p.s(1.5), knurl.gamma_multiply(0.7)),
            );
        }
        p.circle(cx, cy, STICK_R - 9.0, dish, Stroke::new(p.s(1.5), dish_line));
    }

    /// The nut's outline: selection, the wizard's prompt and a swept control
    /// are three statements, so three colours; later wins as in the CSS.
    fn ring(&self, st: &CtlState, base: Stroke, p: &Paint) -> (Stroke, bool) {
        let heavy = |c: Color32| Stroke::new(p.s(3.5), c);
        if st.swept {
            (heavy(style::healthy()), false)
        } else if st.prompted {
            (heavy(style::nav()), false)
        } else if st.selected {
            (heavy(style::accent()), false)
        } else if st.on {
            (Stroke::new(base.width, style::accent()), false)
        } else {
            (base, !st.signal)
        }
    }

    fn paint_switch(&self, p: &Paint, c: &Control) {
        let st = self.state_of(c);
        let (stroke, dashed) = self.ring(&st, Stroke::new(p.s(2.0), style::border_strong()), p);
        if dashed {
            p.dashed_circle(c.cx, c.cy, 12.0, style::surface_2(), stroke);
        } else {
            p.circle(c.cx, c.cy, 12.0, style::surface_2(), stroke);
        }
        p.circle(
            c.cx,
            c.cy,
            5.0,
            style::surface_3(),
            Stroke::new(p.s(1.0), style::border()),
        );
        // no signal parks the handle up and dimmed
        let lift = st.index.map_or(1.0, |i| {
            model::lever_lift(i as f64, f64::from(self.positions_of(&c.id)))
        });
        let tip = model::lever_tip(c, lift);
        let fade = if st.signal { 1.0 } else { 0.4 };
        let col = if st.on { style::accent() } else { style::text_2() };
        p.round_line([c.cx, c.cy], tip, Stroke::new(p.s(7.0), col.gamma_multiply(fade)));
        p.circle(
            tip[0],
            tip[1],
            9.0,
            col.gamma_multiply(fade),
            Stroke::new(p.s(1.5), style::surface_1().gamma_multiply(fade)),
        );
    }

    fn paint_knob(&self, p: &Paint, c: &Control) {
        let st = self.state_of(c);
        let (stroke, _) = self.ring(&st, Stroke::new(p.s(2.0), style::border_strong()), p);
        p.circle(c.cx, c.cy, c.r, style::surface_2(), stroke);
        p.circle(
            c.cx,
            c.cy,
            c.r - 7.0,
            style::surface_3(),
            Stroke::new(p.s(1.0), style::border()),
        );
        let f = self.glide.get(&c.id, self.now).unwrap_or(0.5);
        let angle = ((f * 2.0 - 1.0) * KNOB_SPAN).to_radians();
        // the pointer runs from just above the centre to the rim, turned
        let turn = |y: f64| [c.cx - y * angle.sin(), c.cy + y * angle.cos()];
        let col = if st.on { style::accent() } else { style::text_2() };
        let fade = if st.signal { 1.0 } else { 0.4 };
        p.round_line(
            turn(-3.0),
            turn(-c.r + 3.0),
            Stroke::new(p.s(4.0), col.gamma_multiply(fade)),
        );
    }

    fn paint_callout(&self, p: &Paint, c: &Control) {
        let st = self.state_of(c);
        let (text_x, stub, anchor) = callout_geometry(c.callout.side);
        let stub = c.callout.stub.or(stub);
        let y = c.callout.y;
        let (lead, width) = if st.swept {
            (style::healthy(), 1.5)
        } else if st.prompted {
            (style::nav(), 2.5)
        } else if st.selected || st.on {
            (style::accent(), 1.5)
        } else {
            (style::border_strong(), 1.5)
        };
        let dot = if st.swept {
            style::healthy()
        } else if st.prompted {
            style::nav()
        } else if st.selected || st.on {
            style::accent()
        } else {
            style::border_strong()
        };
        let pts: Vec<[f64; 2]> = match stub {
            None => vec![[text_x, y + 22.0], c.callout.target],
            Some(s) => vec![[text_x, y], [s, y], c.callout.target],
        };
        p.ui.painter().add(Shape::line(
            pts.iter().map(|q| p.at(q[0], q[1])).collect(),
            Stroke::new(p.s(width), lead),
        ));
        p.circle(c.callout.target[0], c.callout.target[1], 3.5, dot, Stroke::NONE);
        let name = if st.swept {
            style::healthy_bright()
        } else if st.prompted {
            style::nav_bright()
        } else if st.selected {
            style::accent_bright()
        } else if st.bound {
            style::text_1()
        } else {
            style::text_2()
        };
        match c.label.chars().last() {
            Some(arrow @ ('\u{2195}' | '\u{2194}')) => {
                // the double arrow is drawn: the sans face has none, and a
                // fallback face would bring a colour glyph
                let word = c.label.trim_end_matches(arrow).trim_end();
                let size = 16.0 * f64::from(p.k);
                let g = style::galley(p.ui, word, size as f32, Weight::W700, name, 0.0);
                let (gap, aw) = (0.3 * size, 0.55 * size);
                let total = f64::from(g.size().x) + gap + aw;
                let left = match anchor {
                    Align::Start => p.at(text_x, 0.0).x,
                    Align::Mid => p.at(text_x, 0.0).x - total as f32 / 2.0,
                    Align::End => p.at(text_x, 0.0).x - total as f32,
                };
                let base_y = p.at(0.0, y - 8.0).y;
                p.ui.painter()
                    .galley(Pos2::new(left, base_y - size as f32 * SANS), g.clone(), name);
                let cx = left + g.size().x + gap as f32 + aw as f32 / 2.0;
                let cy = base_y - size as f32 * 0.36;
                let (half, head) = (size as f32 * 0.42, size as f32 * 0.2);
                let st = Stroke::new((1.6 * f64::from(p.k)).max(1.0) as f32, name);
                let (dx, dy) = if arrow == '\u{2195}' { (0.0, 1.0) } else { (1.0, 0.0) };
                let (a, b) = (
                    Pos2::new(cx - dx * half, cy - dy * half),
                    Pos2::new(cx + dx * half, cy + dy * half),
                );
                p.ui.painter().line_segment([a, b], st);
                for (tip, sign) in [(a, 1.0f32), (b, -1.0)] {
                    let back = Pos2::new(tip.x + sign * dx * head, tip.y + sign * dy * head);
                    p.ui.painter()
                        .line_segment([tip, Pos2::new(back.x + dy * head, back.y + dx * head)], st);
                    p.ui.painter()
                        .line_segment([tip, Pos2::new(back.x - dy * head, back.y - dx * head)], st);
                }
            }
            _ => p.text(&c.label, 16.0, Weight::W700, name, 0.0, text_x, y - 8.0, anchor, false),
        }
        let value = if st.on { style::accent_bright() } else { style::text_3() };
        p.text(
            &self.describe(&c.id),
            13.0,
            Weight::W400,
            value,
            0.0,
            text_x,
            y + 13.0,
            anchor,
            true,
        );
    }

    /// The inspector under the drawing: what the selected control sends on
    /// and how to teach it; the legend while nothing is selected. Returns the
    /// bottom and what to write.
    pub fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        fields: &[MapField],
    ) -> (f32, Option<Action>) {
        let Some(id) = self.selected.clone() else {
            let g = kit::wrapped_weight(
                ui,
                "Click a control, on the drawing or on its label, to see the channel it sends on, or to teach it one.",
                11.5,
                Weight::W400,
                style::text_3(),
                w,
                1.5,
            );
            ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_3());
            return (y + g.size().y, None);
        };
        let Some(c) = model::control(&id) else {
            return (y, None);
        };
        let painter = ui.painter().clone();
        let bg = crate::ui::card::reserve(&painter);
        let (ix, iw) = (x + 13.0, w - 26.0);
        let mut cy = y + 11.0;
        let title = style::galley(ui, &c.label, 12.5, Weight::W700, style::text_1(), 0.0);
        let value = style::galley_mono(ui, &self.describe(&id), 11.0, style::text_2());
        let head_h = title.size().y.max(value.size().y);
        painter.galley(
            Pos2::new(ix, cy + (head_h - title.size().y) / 2.0),
            title,
            style::text_1(),
        );
        painter.galley(
            Pos2::new(ix + iw - value.size().x, cy + (head_h - value.size().y) / 2.0),
            value,
            style::text_2(),
        );
        cy += head_h + 8.0;
        let channel = self.channel_of(&id);
        let mut action = None;
        let note = |ui: &egui::Ui, cy: f32, text: &str| -> f32 {
            let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, style::text_3(), iw, 1.5);
            ui.painter().galley(Pos2::new(ix, cy), g.clone(), style::text_3());
            g.size().y
        };
        if c.kind == Kind::Axis {
            let role = model::role_of(self.store.mode, &id).unwrap_or("throttle");
            let text = if channel > 0 {
                format!(
                    "{} is on channel {channel} ({}). Change it in Stick channels below, or re-run the calibration.",
                    model::role_label(role),
                    model::role_param(role)
                )
            } else {
                format!(
                    "{} is unassigned: {} is 0. Run the radio calibration, or set it in Stick channels below.",
                    model::role_label(role),
                    model::role_param(role)
                )
            };
            cy += note(ui, cy, &text);
        } else {
            let learning = self.learn.is_some();
            let learn_btn = Button::new(
                if learning {
                    "Cancel"
                } else if channel > 0 {
                    "Re-learn channel"
                } else {
                    "Learn channel"
                },
                if learning { Variant::Secondary } else { Variant::Primary },
            )
            .icon(if learning { "x" } else { "crosshair" })
            .small();
            let (bw, bh) = (learn_btn.width(ui), learn_btn.height());
            let mut bx = ix;
            if learn_btn.show(ui, Rect::from_min_size(Pos2::new(bx, cy), Vec2::new(bw, bh)), "rclearn") {
                if learning {
                    self.cancel_learn("cancelled");
                } else {
                    self.start_learn(&id);
                }
            }
            bx += bw + 8.0;
            if c.kind == Kind::Switch {
                let n = self.positions_of(&id);
                let choices = vec![
                    Choice::new("2", "2 positions"),
                    Choice::new("3", "3 positions"),
                    Choice::new("6", "6 positions"),
                ];
                let sel = Rect::from_min_size(Pos2::new(bx, cy), Vec2::new(120.0, bh));
                if let Some(v) =
                    kit::select_padded(ui, sel, &choices, &n.to_string(), "", false, "", "rcpos", (8.0, 24.0))
                {
                    if let Ok(next) = v.parse::<u32>() {
                        let chan = self.store.bindings.get(&id).map_or(0, |b| b.channel);
                        self.store.bindings.insert(
                            id.clone(),
                            model::Binding {
                                channel: chan,
                                positions: Some(next),
                            },
                        );
                        self.store.save();
                    }
                }
                bx += 128.0;
            }
            if channel > 0 {
                let forget = Button::new("Forget", Variant::Ghost).icon("trash-2").small();
                let fw = forget.width(ui);
                if forget.show(
                    ui,
                    Rect::from_min_size(Pos2::new(bx, cy), Vec2::new(fw, bh)),
                    "rcforget",
                ) {
                    self.store.bindings.remove(&id);
                    self.store.save();
                }
                bx += fw + 8.0;
            }
            let status = if learning {
                Some(("pending", "move it now\u{2026}".to_string()))
            } else {
                self.learn_status.clone()
            };
            if let Some((cls, text)) = status {
                let col = crate::ui::pagekit::status_color(cls);
                let g = style::galley(ui, &text, 10.5, Weight::W400, col, 0.0);
                painter.galley(Pos2::new(bx, cy + (bh - g.size().y) / 2.0), g, col);
            }
            cy += bh + 8.0;
            if channel > 0 {
                let (b, a) = self.function_panel(ui, ix, cy, iw, channel, fields);
                cy = b;
                action = a;
            } else {
                cy += note(
                    ui,
                    cy,
                    "Press Learn, then move this control through its full travel. The channel that moves is the one it sends on. Once that is known, you can give the channel a function.",
                );
            }
        }
        let rect = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 11.0));
        painter.set(bg, Shape::rect_filled(rect, 10, style::surface_2()));
        painter.rect_stroke(
            rect,
            10,
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        (rect.bottom(), action)
    }

    /// What a channel does on the vehicle, and how to change it: a chip for
    /// every `RC_MAP_*` already pointing at it (with an X that unassigns it),
    /// and a picker that binds another one to it.
    fn function_panel(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        channel: u32,
        fields: &[MapField],
    ) -> (f32, Option<Action>) {
        if fields.is_empty() {
            return (y, None);
        }
        let armed = self.armed;
        let mut action = None;
        let painter = ui.painter().clone();
        let assigned: Vec<&MapField> = fields.iter().filter(|f| f.value as u32 == channel).collect();
        let mut cx = x;
        let mut cy = y;
        let row_h = 22.0;
        if assigned.is_empty() {
            let chip = crate::ui::chip::Chip {
                size: 11.0,
                weight: Weight::W400,
                pad: (3.0, 9.0, 3.0, 9.0),
                fg: style::text_3(),
                bg: style::surface_1(),
                border: style::border_soft(),
                ..crate::ui::chip::Chip::new(format!("Channel {channel} does nothing on this vehicle yet"))
            };
            let r = chip.paint(ui, Pos2::new(cx, cy));
            cx = r.right() + 8.0;
        }
        for f in &assigned {
            let g = style::galley(ui, &f.label, 11.0, Weight::W600, style::accent_bright(), 0.0);
            let drop_w = 18.0;
            let cw = 1.0 + 9.0 + g.size().x + 2.0 + drop_w + 4.0 + 1.0;
            if cx + cw > x + w && cx > x {
                cx = x;
                cy += row_h + 6.0;
            }
            let r = Rect::from_min_size(Pos2::new(cx, cy), Vec2::new(cw, row_h));
            painter.rect_filled(r, 255, style::accent_dark());
            painter.rect_stroke(r, 255, Stroke::new(1.0, style::accent()), egui::StrokeKind::Inside);
            painter.galley(
                Pos2::new(r.left() + 10.0, r.center().y - g.size().y / 2.0),
                g.clone(),
                style::accent_bright(),
            );
            let xr = Rect::from_min_size(
                Pos2::new(r.right() - 5.0 - drop_w, r.center().y - 9.0),
                Vec2::new(drop_w, 18.0),
            );
            if !armed
                && kit::icon_btn(
                    ui,
                    xr,
                    "x",
                    &format!("Unassign {}", f.label),
                    false,
                    ("rcdrop", &f.param),
                )
            {
                action = Some(Action::Map {
                    param: f.param.clone(),
                    channel: 0,
                });
            }
            cx = r.right() + 6.0;
        }
        // the picker
        let choices: Vec<Choice> = std::iter::once(Choice::new("", "Assign a function\u{2026}"))
            .chain(
                fields
                    .iter()
                    .filter(|f| f.value as u32 != channel)
                    .map(|f| Choice::new(f.param.clone(), f.label.clone())),
            )
            .collect();
        let widest = choices
            .iter()
            .map(|c| {
                style::galley(ui, &c.label, 12.0, Weight::W400, style::text_1(), 0.0)
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        let sw = (widest + 8.0 + 26.0 + 2.0).clamp(170.0, 240.0);
        if cx + sw > x + w && cx > x {
            cx = x;
            cy += row_h + 6.0;
        }
        let sel = Rect::from_min_size(Pos2::new(cx, cy - 3.0), Vec2::new(sw, 28.5));
        if armed {
            painter.rect_filled(sel, style::RADIUS_BTN, style::surface_2());
            painter.rect_stroke(
                sel,
                style::RADIUS_BTN,
                Stroke::new(1.0, style::border()),
                egui::StrokeKind::Inside,
            );
            let g = style::galley(
                ui,
                "Assign a function\u{2026}",
                12.0,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            painter.galley(
                Pos2::new(sel.left() + 8.0, sel.center().y - g.size().y / 2.0),
                g,
                style::text_3(),
            );
        } else if let Some(v) = kit::select_padded(ui, sel, &choices, "", "", false, "", ("rcfn", channel), (8.0, 26.0))
        {
            if !v.is_empty() {
                action = Some(Action::Map { param: v, channel });
            }
        }
        (sel.bottom().max(cy + row_h), action)
    }
}

#[derive(Clone, Copy)]
struct CtlState {
    bound: bool,
    signal: bool,
    index: Option<usize>,
    on: bool,
    selected: bool,
    prompted: bool,
    swept: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Start,
    Mid,
    End,
}

/// The callout columns (`CALLOUT`): where the text sits, the default stub,
/// and which way the text runs.
fn callout_geometry(side: CalloutSide) -> (f64, Option<f64>, Align) {
    match side {
        CalloutSide::Left => (128.0, Some(176.0), Align::End),
        CalloutSide::Right => (752.0, Some(704.0), Align::Start),
        CalloutSide::TopLeft => (340.0, None, Align::Mid),
        CalloutSide::TopRight => (540.0, None, Align::Mid),
    }
}

/// The click box of a callout: x, y, width, height in drawing units.
fn callout_box(c: &Control) -> [f64; 4] {
    let (x, w) = match c.callout.side {
        CalloutSide::Left => (8.0, 128.0),
        CalloutSide::Right => (744.0, 128.0),
        CalloutSide::TopLeft => (288.0, 104.0),
        CalloutSide::TopRight => (488.0, 104.0),
    };
    [x, c.callout.y - 27.0, w, 46.0]
}

/// The case outline (`LAYOUT.body`: M, L, Q and Z) as a polygon.
pub fn body_outline(path: &str) -> Vec<[f64; 2]> {
    let t: Vec<&str> = path.split_whitespace().collect();
    let mut out: Vec<[f64; 2]> = Vec::new();
    let mut i = 0;
    let num = |s: &str| s.parse::<f64>().unwrap_or(0.0);
    while i < t.len() {
        match t[i] {
            "M" | "L" => {
                out.push([num(t[i + 1]), num(t[i + 2])]);
                i += 3;
            }
            "Q" => {
                let (c, e) = ([num(t[i + 1]), num(t[i + 2])], [num(t[i + 3]), num(t[i + 4])]);
                let s = *out.last().unwrap_or(&e);
                for k in 1..=12 {
                    let u = f64::from(k) / 12.0;
                    let v = 1.0 - u;
                    out.push([
                        v * v * s[0] + 2.0 * v * u * c[0] + u * u * e[0],
                        v * v * s[1] + 2.0 * v * u * c[1] + u * u * e[1],
                    ]);
                }
                i += 5;
            }
            _ => i += 1,
        }
    }
    out
}

fn body_points(p: &Paint) -> Vec<Pos2> {
    let path = model::layout().raw["body"].as_str().unwrap_or("");
    body_outline(path).iter().map(|q| p.at(q[0], q[1])).collect()
}

/// The drawing's frame: where the view's origin is and how big a unit is.
struct Paint<'a> {
    ui: &'a egui::Ui,
    ox: f32,
    oy: f32,
    k: f32,
}

impl Paint<'_> {
    fn at(&self, x: f64, y: f64) -> Pos2 {
        Pos2::new(self.ox + x as f32 * self.k, self.oy + y as f32 * self.k)
    }

    fn sz(&self, w: f64, h: f64) -> Vec2 {
        Vec2::new(w as f32 * self.k, h as f32 * self.k)
    }

    fn s(&self, w: f64) -> f32 {
        w as f32 * self.k
    }

    fn rect(&self, r: Rect, radius: f64, fill: Color32, stroke: Stroke) {
        let rad = (radius as f32 * self.k).round().clamp(0.0, 255.0) as u8;
        self.ui.painter().rect_filled(r, rad, fill);
        self.ui.painter().rect_stroke(r, rad, stroke, egui::StrokeKind::Middle);
    }

    fn circle(&self, x: f64, y: f64, r: f64, fill: Color32, stroke: Stroke) {
        self.ui.painter().circle(self.at(x, y), self.s(r), fill, stroke);
    }

    fn dashed_circle(&self, x: f64, y: f64, r: f64, fill: Color32, stroke: Stroke) {
        self.ui.painter().circle_filled(self.at(x, y), self.s(r), fill);
        let pts: Vec<Pos2> = (0..=48)
            .map(|i| {
                let a = f64::from(i) / 48.0 * std::f64::consts::TAU;
                self.at(x + a.cos() * r, y + a.sin() * r)
            })
            .collect();
        self.ui
            .painter()
            .extend(Shape::dashed_line(&pts, stroke, self.s(3.0), self.s(3.0)));
    }

    fn line(&self, a: [f64; 2], b: [f64; 2], stroke: Stroke) {
        self.ui
            .painter()
            .line_segment([self.at(a[0], a[1]), self.at(b[0], b[1])], stroke);
    }

    /// A line with round caps (`stroke-linecap: round`).
    fn round_line(&self, a: [f64; 2], b: [f64; 2], stroke: Stroke) {
        self.line(a, b, stroke);
        let r = stroke.width / 2.0;
        self.ui.painter().circle_filled(self.at(a[0], a[1]), r, stroke.color);
        self.ui.painter().circle_filled(self.at(b[0], b[1]), r, stroke.color);
    }

    /// Text with its baseline at `y` in drawing units.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        s: &str,
        size: f64,
        w: Weight,
        color: Color32,
        letter: f64,
        x: f64,
        base: f64,
        align: Align,
        mono: bool,
    ) {
        let size = self.s(size);
        let g = if mono {
            style::galley_mono_letter(self.ui, s, size, color, self.s(letter))
        } else {
            style::galley(self.ui, s, size, w, color, self.s(letter))
        };
        let top = self.at(0.0, base).y - size * if mono { MONO } else { SANS };
        let left = match align {
            Align::Start => self.at(x, 0.0).x,
            Align::Mid => self.at(x, 0.0).x - g.size().x / 2.0,
            Align::End => self.at(x, 0.0).x - g.size().x,
        };
        self.ui.painter().galley(Pos2::new(left, top), g, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_case_outline_is_a_closed_polygon_through_the_web_paths_corners() {
        let pts = body_outline(model::layout().raw["body"].as_str().unwrap());
        // 1 move, 8 lines and 8 curves of 12 points
        assert_eq!(pts.len(), 1 + 8 + 8 * 12);
        assert_eq!(pts[0], [356.0, 150.0]);
        assert!(pts.contains(&[736.0, 500.0]));
        assert!(pts
            .iter()
            .all(|p| (144.0..=736.0).contains(&p[0]) && (150.0..=604.0).contains(&p[1])));
    }

    fn live(channels: &[f64]) -> Value {
        serde_json::json!({"rc_live": true, "rc_channels": channels, "rc_rssi": 88})
    }

    fn tx() -> Transmitter {
        Transmitter::new(Store::load(None))
    }

    #[test]
    fn a_switch_reads_its_position_from_the_channel_it_was_taught() {
        let mut t = tx();
        t.store.bindings.insert(
            "SA".into(),
            model::Binding {
                channel: 5,
                positions: None,
            },
        );
        t.update(&live(&[1500.0, 1500.0, 1000.0, 1500.0, 2000.0]), 0.0);
        assert_eq!(t.channel_of("SA"), 5);
        assert_eq!(t.describe("SA"), "CH5 \u{b7} Up");
        t.update(&live(&[1500.0, 1500.0, 1000.0, 1500.0, 1500.0]), 40.0);
        assert_eq!(t.describe("SA"), "CH5 \u{b7} Middle");
        assert_eq!(t.describe("SB"), "not bound");
        t.update(&serde_json::json!({"rc_live": false, "rc_channels": []}), 80.0);
        assert_eq!(t.describe("SA"), "CH5 \u{b7} no signal");
    }

    #[test]
    fn a_stick_takes_its_channel_from_the_vehicle_through_the_mode() {
        let mut t = tx();
        t.set_doc(
            [("throttle".to_string(), 3), ("roll".to_string(), 1)].into(),
            BTreeMap::new(),
        );
        assert_eq!(t.channel_of("left_y"), 3, "mode 2 puts throttle on the left stick");
        assert_eq!(t.channel_of("right_x"), 1);
        assert_eq!(t.channel_of("right_y"), 0);
        t.store.mode = 1;
        assert_eq!(t.channel_of("right_y"), 3, "mode 1 puts it on the right");
        t.update(&live(&[1500.0, 1500.0, 2000.0]), 0.0);
        assert_eq!(t.describe("right_y"), "CH3 \u{b7} 92%");
    }

    #[test]
    fn a_calibrated_channel_sets_the_scale_and_a_reversed_one_follows_the_hand() {
        let mut t = tx();
        t.set_doc(
            BTreeMap::new(),
            [(
                5,
                Info {
                    min: 1100.0,
                    max: 1900.0,
                    reversed: true,
                },
            )]
            .into(),
        );
        t.store.bindings.insert(
            "SC".into(),
            model::Binding {
                channel: 5,
                positions: Some(3),
            },
        );
        t.update(&live(&[0.0, 0.0, 0.0, 0.0, 1100.0]), 0.0);
        let travel = t.travel_of("SC").unwrap();
        assert!(
            (travel - 1.0).abs() < 1e-9,
            "the low end of a reversed channel is the top of the hand"
        );
        assert_eq!(t.describe("SC"), "CH5 \u{b7} Up");
    }

    #[test]
    fn learn_binds_the_channel_that_moves_and_gives_up_after_eight_seconds() {
        let mut t = tx();
        t.update(&serde_json::json!({"rc_live": false, "rc_channels": []}), 0.0);
        t.start_learn("SA");
        assert_eq!(t.learn_status.as_ref().map(|s| s.1.as_str()), Some("no RC signal"));
        t.update(&live(&[1500.0, 1500.0, 1500.0]), 0.0);
        t.start_learn("SA");
        assert!(t.learn.is_some());
        t.update(&live(&[1505.0, 1500.0, 1500.0]), 100.0);
        assert!(t.learn.is_some(), "jitter does not win");
        t.update(&live(&[1505.0, 1500.0, 1900.0]), 200.0);
        assert!(t.learn.is_none());
        assert_eq!(t.store.bindings["SA"].channel, 3);
        t.start_learn("SB");
        t.update(&live(&[1505.0, 1500.0, 1900.0]), 9000.0);
        assert_eq!(t.learn_status.as_ref().map(|s| s.1.as_str()), Some("nothing moved"));
        assert!(!t.store.bindings.contains_key("SB"));
    }

    #[test]
    fn selecting_a_control_twice_deselects_it_and_forget_drops_every_binding() {
        let mut t = tx();
        t.select("SA");
        assert_eq!(t.selected(), Some("SA"));
        t.select("SB");
        assert_eq!(t.selected(), Some("SB"));
        t.select("SB");
        assert_eq!(t.selected(), None);
        t.store.bindings.insert(
            "SA".into(),
            model::Binding {
                channel: 5,
                positions: None,
            },
        );
        t.forget_all();
        assert!(t.store.bindings.is_empty());
        assert_eq!(t.channel_map().len(), 0);
    }
}
