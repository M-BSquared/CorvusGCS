//! The drawn transmitter's model (`python/src/js/rc-transmitter.js`): the layout of
//! the handset, the four stick modes, how a pulse becomes a position, the
//! eased reading between two RC_CHANNELS samples, and the controls this
//! station has learned. No drawing here; `rc_drawing.rs` paints it.
//!
//! The layout is the web page's own `LAYOUT` (`rc_transmitter.json`, dumped by
//! running it), read once, so a control is where the web drawing puts it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::{json, Value};

/// The display scale of a channel with no calibration of its own.
pub const PWM_MIN: f64 = 900.0;
pub const PWM_MAX: f64 = 2100.0;
/// How far a control must move for Learn to call it the one that moved.
pub const LEARN_TRAVEL_US: f64 = 250.0;
pub const LEARN_TIMEOUT_MS: f64 = 8000.0;
const GLIDE_MIN_MS: f64 = 16.0;
const GLIDE_MAX_MS: f64 = 80.0;
const GLIDE_DEFAULT_MS: f64 = 50.0;
pub const LEVER_LEAN: f64 = 12.0;
pub const STICK_R: f64 = 27.0;
pub const KNOB_SPAN: f64 = 140.0;
pub const DEFAULT_MODE: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Axis,
    Switch,
    Knob,
}

/// Where a callout's text sits in the margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutSide {
    Left,
    Right,
    TopLeft,
    TopRight,
}

#[derive(Debug, Clone)]
pub struct Callout {
    pub side: CalloutSide,
    pub y: f64,
    pub stub: Option<f64>,
    pub target: [f64; 2],
}

/// One bindable control of the drawn model.
#[derive(Debug, Clone)]
pub struct Control {
    pub id: String,
    pub kind: Kind,
    pub label: String,
    pub positions: u32,
    pub out: f64,
    pub cx: f64,
    pub cy: f64,
    pub lever: f64,
    pub r: f64,
    pub callout: Callout,
}

#[derive(Debug, Clone, Copy)]
pub struct Gimbal {
    pub cx: f64,
    pub cy: f64,
    pub r: f64,
}

pub struct Layout {
    pub raw: Value,
    pub controls: Vec<Control>,
    pub left: Gimbal,
    pub right: Gimbal,
}

fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The web page's layout.
pub fn layout() -> &'static Layout {
    static L: OnceLock<Layout> = OnceLock::new();
    L.get_or_init(|| {
        let all: Value = serde_json::from_str(include_str!("rc_transmitter.json")).expect("the transmitter layout");
        let raw = all["layout"].clone();
        let controls = raw["controls"]
            .as_array()
            .expect("controls")
            .iter()
            .map(|c| {
                let co = &c["callout"];
                Control {
                    id: c["id"].as_str().unwrap_or("").into(),
                    kind: match c["kind"].as_str() {
                        Some("axis") => Kind::Axis,
                        Some("knob") => Kind::Knob,
                        _ => Kind::Switch,
                    },
                    label: c["label"].as_str().unwrap_or("").into(),
                    positions: c["positions"].as_u64().unwrap_or(2) as u32,
                    out: if c["side"] == "left" { -1.0 } else { 1.0 },
                    cx: f(c, "cx"),
                    cy: f(c, "cy"),
                    lever: f(c, "lever"),
                    r: f(c, "r"),
                    callout: Callout {
                        side: match co["side"].as_str() {
                            Some("left") => CalloutSide::Left,
                            Some("right") => CalloutSide::Right,
                            Some("top-left") => CalloutSide::TopLeft,
                            _ => CalloutSide::TopRight,
                        },
                        y: f(co, "y"),
                        stub: co.get("stub").and_then(Value::as_f64),
                        target: [
                            co["target"][0].as_f64().unwrap_or(0.0),
                            co["target"][1].as_f64().unwrap_or(0.0),
                        ],
                    },
                }
            })
            .collect();
        let g = |i: usize| Gimbal {
            cx: f(&raw["gimbals"][i], "cx"),
            cy: f(&raw["gimbals"][i], "cy"),
            r: f(&raw["gimbals"][i], "r"),
        };
        Layout {
            left: g(0),
            right: g(1),
            raw,
            controls,
        }
    })
}

pub fn control(id: &str) -> Option<&'static Control> {
    layout().controls.iter().find(|c| c.id == id)
}

/// `MODES`: gimbal axis to stick role, for stick mode 1 to 4; an unknown mode
/// is mode 2.
pub fn axis_roles(mode: u8) -> [(&'static str, &'static str); 4] {
    match mode {
        1 => [
            ("left_y", "pitch"),
            ("left_x", "yaw"),
            ("right_y", "throttle"),
            ("right_x", "roll"),
        ],
        3 => [
            ("left_y", "pitch"),
            ("left_x", "roll"),
            ("right_y", "throttle"),
            ("right_x", "yaw"),
        ],
        4 => [
            ("left_y", "throttle"),
            ("left_x", "roll"),
            ("right_y", "pitch"),
            ("right_x", "yaw"),
        ],
        _ => [
            ("left_y", "throttle"),
            ("left_x", "yaw"),
            ("right_y", "pitch"),
            ("right_x", "roll"),
        ],
    }
}

/// The stick role an axis carries in a mode.
pub fn role_of(mode: u8, axis: &str) -> Option<&'static str> {
    axis_roles(mode).into_iter().find(|a| a.0 == axis).map(|a| a.1)
}

/// The parameter each stick role is mapped by.
pub fn role_param(role: &str) -> &'static str {
    match role {
        "throttle" => "RC_MAP_THROTTLE",
        "roll" => "RC_MAP_ROLL",
        "pitch" => "RC_MAP_PITCH",
        _ => "RC_MAP_YAW",
    }
}

pub fn role_label(role: &str) -> &'static str {
    match role {
        "throttle" => "Throttle",
        "roll" => "Roll",
        "pitch" => "Pitch",
        _ => "Yaw",
    }
}

pub const ROLES: [&str; 4] = ["throttle", "roll", "pitch", "yaw"];

/// `fractionOf`: 0 to 1 between two endpoints, 0.5 when they span nothing.
pub fn fraction_of(pulse: f64, min: Option<f64>, max: Option<f64>) -> f64 {
    let lo = min.filter(|v| *v != 0.0).unwrap_or(PWM_MIN);
    let hi = max.filter(|v| *v != 0.0).unwrap_or(PWM_MAX);
    let span = hi - lo;
    if span <= 0.0 || span.is_nan() {
        return 0.5;
    }
    ((pulse - lo) / span).clamp(0.0, 1.0)
}

/// `positionIndex`: which of `positions` equal slots a 0 to 1 value sits in.
pub fn position_index(fraction: f64, positions: f64) -> usize {
    let n = js_round(if positions.is_nan() || positions == 0.0 {
        2.0
    } else {
        positions
    })
    .clamp(2.0, 6.0);
    let fr = if fraction.is_nan() { 0.0 } else { fraction }.clamp(0.0, 1.0);
    (fr * n).floor().clamp(0.0, n - 1.0) as usize
}

fn js_round(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// `positionLabel`.
pub fn position_label(index: usize, positions: u32) -> String {
    if positions <= 2 {
        if index == 0 { "Down" } else { "Up" }.into()
    } else if positions == 3 {
        ["Down", "Middle", "Up"]
            .get(index)
            .map_or_else(|| (index + 1).to_string(), |s| (*s).into())
    } else {
        format!("Position {}", index + 1)
    }
}

/// `leverLift`: where a bat handle points, +1 up, 0 out, -1 down.
pub fn lever_lift(index: f64, positions: f64) -> f64 {
    let n = js_round(if positions.is_nan() || positions == 0.0 {
        2.0
    } else {
        positions
    })
    .max(2.0);
    let i = if index.is_nan() { 0.0 } else { index }.clamp(0.0, n - 1.0);
    i / (n - 1.0) * 2.0 - 1.0
}

/// `leverTip`: the handle's tip, leaning outboard in up and down alike.
pub fn lever_tip(c: &Control, lift: f64) -> [f64; 2] {
    let lean = LEVER_LEAN.to_radians();
    [
        c.cx + c.out * lift.abs() * c.lever * lean.sin(),
        c.cy - lift * c.lever * lean.cos(),
    ]
}

/// Normalised pulse position inside the display scale (`pwmFraction`).
pub fn pwm_fraction(pulse: f64) -> f64 {
    if pulse <= 0.0 {
        0.0
    } else {
        ((pulse - PWM_MIN) / (PWM_MAX - PWM_MIN)).clamp(0.0, 1.0)
    }
}

/// A learned control: its channel (0 when only the position count is set)
/// and the positions it has.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub channel: u32,
    pub positions: Option<u32>,
}

/// What this station remembers about the handset on the bench: the learned
/// controls and the stick mode. Kept in a file beside the backend's own
/// (`~/.corvus`), because it describes the handset and not the aircraft.
#[derive(Debug, Clone, PartialEq)]
pub struct Store {
    pub bindings: BTreeMap<String, Binding>,
    pub mode: u8,
    path: Option<PathBuf>,
}

impl Store {
    pub fn default_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        Some(PathBuf::from(home).join(".corvus").join("gui-rc-transmitter.json"))
    }

    /// Reads a store; a missing or damaged file is an empty one (mode 2).
    pub fn load(path: Option<PathBuf>) -> Store {
        let mut s = Store {
            bindings: BTreeMap::new(),
            mode: DEFAULT_MODE,
            path,
        };
        let Some(text) = s.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return s;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return s;
        };
        if let Some(m) = v["mode"].as_u64().filter(|m| (1..=4).contains(m)) {
            s.mode = m as u8;
        }
        if let Some(map) = v["bindings"].as_object() {
            for (id, e) in map {
                if control(id).is_none() {
                    continue;
                }
                let channel = e["channel"].as_f64().unwrap_or(0.0);
                let positions = e["positions"]
                    .as_f64()
                    .filter(|p| *p >= 2.0)
                    .map(|p| js_round(p) as u32);
                if channel >= 1.0 {
                    s.bindings.insert(
                        id.clone(),
                        Binding {
                            channel: js_round(channel) as u32,
                            positions,
                        },
                    );
                } else if let Some(p) = positions {
                    // positions alone, set on a control that is not bound yet
                    s.bindings.insert(
                        id.clone(),
                        Binding {
                            channel: 0,
                            positions: Some(p),
                        },
                    );
                }
            }
        }
        s
    }

    /// Writes the store; a full disk or a read only folder leaves the
    /// drawing working, as a full browser quota does.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let bindings: serde_json::Map<String, Value> = self
            .bindings
            .iter()
            .map(|(id, b)| {
                let mut e = serde_json::Map::new();
                if b.channel >= 1 {
                    e.insert("channel".into(), json!(b.channel));
                }
                if let Some(p) = b.positions {
                    e.insert("positions".into(), json!(p));
                }
                (id.clone(), Value::Object(e))
            })
            .collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, json!({"mode": self.mode, "bindings": bindings}).to_string());
    }
}

/// Eases displayed positions from one RC_CHANNELS sample to the next at a
/// constant rate (`createGlide`): the store coalesces pushes to 30 Hz, so
/// samples land 33 or 67 ms apart, and painting each as it comes turns a
/// smooth sweep into a stutter. A glide as long as the measured sample spacing
/// turns it back into motion for the price of one interval of lag.
#[derive(Debug, Default)]
pub struct Glide {
    tracks: BTreeMap<String, Track>,
    spacing: Option<f64>,
    last_change: Option<f64>,
    pending: bool,
}

#[derive(Debug, Clone)]
struct Track {
    from: Option<f64>,
    to: Option<f64>,
    start: f64,
    dur: f64,
    fresh: bool,
}

fn value_at(t: &Track, now: f64) -> Option<f64> {
    let (Some(from), Some(to)) = (t.from, t.to) else {
        return t.to;
    };
    if t.dur <= 0.0 {
        return Some(to);
    }
    let k = (now - t.start) / t.dur;
    Some(if k >= 1.0 {
        to
    } else if k <= 0.0 {
        from
    } else {
        from + (to - from) * k
    })
}

impl Glide {
    /// Where `key` should be; `None` is no reading, drawn at rest at once.
    pub fn set(&mut self, key: &str, value: Option<f64>, now: f64) {
        let next = value.filter(|v| v.is_finite());
        let Some(track) = self.tracks.get(key).cloned() else {
            self.tracks.insert(
                key.into(),
                Track {
                    from: next,
                    to: next,
                    start: 0.0,
                    dur: 0.0,
                    fresh: false,
                },
            );
            return;
        };
        if track.to == next {
            return;
        }
        let from = if next.is_none() || track.to.is_none() {
            next
        } else {
            value_at(&track, now)
        };
        self.tracks.insert(
            key.into(),
            Track {
                from,
                to: next,
                start: now,
                dur: 0.0,
                fresh: true,
            },
        );
        self.pending = true;
    }

    /// Starts the glide of what `set` changed, as long as the samples are
    /// apart (measured), held between 16 and 80 ms.
    pub fn commit(&mut self, now: f64) {
        if !self.pending {
            return;
        }
        if let Some(last) = self.last_change {
            let gap = now - last;
            if (GLIDE_MIN_MS / 2.0..=GLIDE_MAX_MS * 2.0).contains(&gap) {
                let s = self.spacing.unwrap_or(GLIDE_DEFAULT_MS);
                self.spacing = Some(s + (gap - s) * 0.3);
            }
        }
        self.last_change = Some(now);
        let dur = self
            .spacing
            .unwrap_or(GLIDE_DEFAULT_MS)
            .clamp(GLIDE_MIN_MS, GLIDE_MAX_MS);
        for t in self.tracks.values_mut() {
            if t.fresh {
                t.dur = dur;
                t.fresh = false;
            }
        }
        self.pending = false;
    }

    pub fn get(&self, key: &str, now: f64) -> Option<f64> {
        self.tracks.get(key).and_then(|t| value_at(t, now))
    }

    pub fn drop_key(&mut self, key: &str) {
        self.tracks.remove(key);
    }

    /// Whether anything is still on its way.
    pub fn moving(&self, now: f64) -> bool {
        self.tracks
            .values()
            .any(|t| t.to.is_some() && t.from.is_some() && now < t.start + t.dur)
    }
}

/// The channel that moved most since `baseline`, if past `LEARN_TRAVEL_US`
/// (Learn and Detect, which must agree on one flip of one switch).
pub fn moved_channel(baseline: &[f64], now: &[f64]) -> Option<usize> {
    let mut best = 0;
    let mut travel = 0.0;
    for (i, (b, n)) in baseline.iter().zip(now).enumerate() {
        let t = (n - b).abs();
        if t > travel {
            travel = t;
            best = i + 1;
        }
    }
    (travel >= LEARN_TRAVEL_US).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cases() -> Value {
        serde_json::from_str(include_str!("rc_transmitter.json")).unwrap()
    }

    #[test]
    fn the_layout_is_the_web_pages() {
        let l = layout();
        assert_eq!(l.controls.len(), 12);
        let ids: Vec<&str> = l.controls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            ["left_y", "left_x", "right_y", "right_x", "SA", "SB", "SF", "SD", "SC", "SH", "S1", "S2"]
        );
        let sa = control("SA").unwrap();
        assert_eq!(
            (sa.kind, sa.positions, sa.cx, sa.cy, sa.out),
            (Kind::Switch, 3, 334.0, 212.0, -1.0)
        );
        assert_eq!(control("SH").unwrap().out, 1.0);
        assert_eq!(control("SF").unwrap().positions, 2);
        assert_eq!(control("S2").unwrap().kind, Kind::Knob);
        assert_eq!(control("SA").unwrap().callout.stub, Some(280.0));
        assert_eq!(control("S1").unwrap().callout.side, CalloutSide::TopLeft);
        // each shoulder mirrors the other about the centre line
        for (a, b) in [("SA", "SD"), ("SB", "SC"), ("SF", "SH")] {
            let (a, b) = (control(a).unwrap(), control(b).unwrap());
            assert_eq!(a.cx + b.cx, 880.0, "{} and {} mirror", a.id, b.id);
            assert_eq!(a.cy, b.cy);
        }
        assert_eq!((l.left.cx + l.right.cx) / 2.0, 440.0);
    }

    #[test]
    fn the_stick_modes_put_throttle_where_the_web_page_does() {
        let modes = &cases()["modes"];
        for m in 1..=4u8 {
            for (axis, role) in axis_roles(m) {
                assert_eq!(modes[m.to_string()][axis], role, "mode {m} {axis}");
            }
        }
        assert_eq!(role_of(2, "left_y"), Some("throttle"));
        assert_eq!(role_of(9, "left_y"), Some("throttle"), "an unknown mode is mode 2");
        assert_eq!(role_param("pitch"), "RC_MAP_PITCH");
    }

    #[test]
    fn a_pulse_becomes_a_position_as_the_web_page_reads_it() {
        let s = &cases()["samples"];
        for c in s["frac"].as_array().unwrap() {
            let a = &c["a"];
            let opt = |i: usize| a[i].as_f64().filter(|v| *v != 0.0);
            let got = fraction_of(a[0].as_f64().unwrap(), opt(1), opt(2));
            assert!((got - c["r"].as_f64().unwrap()).abs() < 1e-12, "{a}");
        }
        for c in s["pos"].as_array().unwrap() {
            let a = &c["a"];
            let got = position_index(a[0].as_f64().unwrap(), a[1].as_f64().unwrap());
            assert_eq!(got as u64, c["r"].as_u64().unwrap(), "{a}");
        }
        for c in s["lift"].as_array().unwrap() {
            let a = &c["a"];
            let got = lever_lift(a[0].as_f64().unwrap(), a[1].as_f64().unwrap());
            assert!((got - c["r"].as_f64().unwrap()).abs() < 1e-12, "{a}");
        }
        for c in s["label"].as_array().unwrap() {
            let a = &c["a"];
            let got = position_label(a[0].as_u64().unwrap() as usize, a[1].as_u64().unwrap() as u32);
            assert_eq!(got, c["r"].as_str().unwrap(), "{a}");
        }
    }

    #[test]
    fn a_handle_leans_outboard_in_up_and_down_alike() {
        let sa = control("SA").unwrap();
        let (up, down, mid) = (lever_tip(sa, 1.0), lever_tip(sa, -1.0), lever_tip(sa, 0.0));
        assert!(up[1] < sa.cy && down[1] > sa.cy);
        assert!((up[0] - sa.cx).abs() > 0.0 && up[0] < sa.cx, "left shoulder leans left");
        assert!((up[0] - down[0]).abs() < 1e-9);
        assert_eq!(mid[0], sa.cx);
        let sd = control("SD").unwrap();
        assert!(lever_tip(sd, 1.0)[0] > sd.cx);
    }

    #[test]
    fn the_store_keeps_what_was_learned_and_forgets_what_is_not_a_control() {
        let dir = std::env::temp_dir().join(format!("corvus-rc-{}", std::process::id()));
        let path = dir.join("tx.json");
        let mut s = Store::load(Some(path.clone()));
        assert_eq!((s.mode, s.bindings.len()), (2, 0));
        s.mode = 3;
        s.bindings.insert(
            "SA".into(),
            Binding {
                channel: 5,
                positions: Some(3),
            },
        );
        s.bindings.insert(
            "SB".into(),
            Binding {
                channel: 0,
                positions: Some(2),
            },
        );
        s.save();
        let back = Store::load(Some(path.clone()));
        assert_eq!(back.mode, 3);
        assert_eq!(
            back.bindings["SA"],
            Binding {
                channel: 5,
                positions: Some(3)
            }
        );
        assert_eq!(
            back.bindings["SB"],
            Binding {
                channel: 0,
                positions: Some(2)
            }
        );
        std::fs::write(
            &path,
            r#"{"mode": 9, "bindings": {"SX": {"channel": 3}, "SC": {"channel": 2.4}}}"#,
        )
        .unwrap();
        let odd = Store::load(Some(path));
        assert_eq!(odd.mode, 2, "a mode outside 1 to 4 is mode 2");
        assert!(!odd.bindings.contains_key("SX"));
        assert_eq!(odd.bindings["SC"].channel, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_glide_lasts_as_long_as_the_samples_are_apart() {
        let mut g = Glide::default();
        g.set("a", Some(0.0), 0.0);
        g.commit(0.0);
        assert_eq!(g.get("a", 0.0), Some(0.0));
        g.set("a", Some(1.0), 100.0);
        g.commit(100.0);
        assert_eq!(g.get("a", 100.0), Some(0.0), "starts where it was");
        let mid = g.get("a", 125.0).unwrap();
        assert!(mid > 0.0 && mid < 1.0);
        assert!(g.moving(110.0));
        assert_eq!(g.get("a", 400.0), Some(1.0));
        assert!(!g.moving(400.0));
        // no reading is drawn at rest at once
        g.set("a", None, 500.0);
        g.commit(500.0);
        assert_eq!(g.get("a", 500.0), None);
        g.set("a", Some(0.5), 600.0);
        g.commit(600.0);
        assert_eq!(g.get("a", 600.0), Some(0.5), "a reading after none appears at once");
    }

    #[test]
    fn the_channel_that_moved_most_wins_past_the_threshold() {
        assert_eq!(
            moved_channel(&[1500.0, 1500.0, 1000.0], &[1510.0, 1500.0, 1600.0]),
            Some(3)
        );
        assert_eq!(moved_channel(&[1500.0, 1500.0], &[1510.0, 1480.0]), None);
        assert_eq!(moved_channel(&[1500.0, 1000.0], &[1900.0, 1300.0]), Some(1));
    }
}
