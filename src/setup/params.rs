//! The Parameters page of Setup (python/src/js/setup-parameters.js,
//! setup.css `.params-*`): download on demand, then an editor with search,
//! the All / Modified / Unsaved filter, a default and a reset per row,
//! per-row Apply and Apply all, and the defaults read from the vehicle.
//!
//! The numbers behave as the web page's do: a value is shown at the
//! shortest precision that still round-trips through a 32 bit float, an
//! integer type takes whole numbers in its range only, a draft outside the
//! firmware's own min and max is allowed with a warning, and nothing is
//! written while the vehicle is armed. Export, Import and the autopilot
//! reboot sit in the header, their dialogs in `params_dialogs`.

use std::cmp::Ordering;
use std::collections::HashMap;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::app::responsive::Vw;
use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::params_dialogs::{self as dialogs, ExportDlg, ImportDlg};
use crate::setup::params_files as files;
use crate::ui::card::{self, Look};
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

/// Rows drawn at once.
const CAP: usize = 200;
const POLL_SECS: f64 = 1.0;

/// MAV_PARAM_TYPE to the whole numbers it holds; REAL32 (9) is not here.
fn int_range(t: i64) -> Option<(f64, f64)> {
    Some(match t {
        1 => (0.0, 255.0),
        2 => (-128.0, 127.0),
        3 => (0.0, 65535.0),
        4 => (-32768.0, 32767.0),
        5 => (0.0, 4294967295.0),
        6 => (-2147483648.0, 2147483647.0),
        _ => return None,
    })
}

fn type_name(t: i64) -> &'static str {
    match t {
        1 => "UINT8",
        2 => "INT8",
        3 => "UINT16",
        4 => "INT16",
        5 => "UINT32",
        6 => "INT32",
        9 => "REAL32",
        _ => "",
    }
}

/// A parameter on the vehicle.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub value: f64,
    pub kind: i64,
}

/// What the firmware says about one parameter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub default: Option<f64>,
    pub short_desc: String,
    pub long_desc: String,
    pub units: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub values: Vec<(f64, String)>,
    pub bitmask: Vec<(i64, String)>,
    pub reboot_required: bool,
}

impl Meta {
    fn from_value(v: &Value) -> Meta {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(Value::as_f64);
        Meta {
            default: n("default"),
            short_desc: s("short_desc"),
            long_desc: s("long_desc"),
            units: s("units"),
            min: n("min"),
            max: n("max"),
            values: v
                .get("values")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| {
                            Some((
                                p.get(0)?.as_f64()?,
                                p.get(1).and_then(Value::as_str).unwrap_or("").to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            bitmask: v
                .get("bitmask")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| {
                            Some((
                                p.get(0)?.as_i64()?,
                                p.get(1).and_then(Value::as_str).unwrap_or("").to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            reboot_required: v.get("reboot_required") == Some(&Value::Bool(true)),
        }
    }
}

/// `Math.fround`.
fn fround(x: f64) -> f64 {
    f64::from(x as f32)
}

/// The value as text: whole numbers for an integer type, otherwise the
/// shortest precision (1 to 9 digits) that reads back as the same float.
pub fn format_value(value: f64, kind: i64) -> String {
    if !value.is_finite() {
        return crate::data::units::js_number(value);
    }
    if int_range(kind).is_some() {
        return crate::data::units::js_number(crate::data::units::js_round(value));
    }
    let target = fround(value);
    for digits in 1..=9usize {
        let text = format!("{:.*e}", digits - 1, value);
        if let Ok(back) = text.parse::<f64>() {
            if fround(back) == target {
                return crate::data::units::js_number(back);
            }
        }
    }
    crate::data::units::js_number(value)
}

/// Whether two values are the same parameter value.
pub fn values_equal(a: f64, b: f64, kind: i64) -> bool {
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
    if int_range(kind).is_some() {
        return crate::data::units::js_round(a) == crate::data::units::js_round(b);
    }
    fround(a) == fround(b)
}

/// What a draft text amounts to.
#[derive(Debug, Clone, PartialEq)]
pub enum Draft {
    Bad(String),
    Ok { value: f64, warn: String },
}

/// Reads a draft the way parseDraft does.
pub fn parse_draft(text: &str, param: &Param, meta: Option<&Meta>) -> Draft {
    let t = text.trim();
    let Some(value) = t.parse::<f64>().ok().filter(|v| v.is_finite() && !t.is_empty()) else {
        return Draft::Bad("not a number".into());
    };
    if let Some((lo, hi)) = int_range(param.kind) {
        if value.fract() != 0.0 {
            return Draft::Bad("whole numbers only".into());
        }
        if value < lo || value > hi {
            return Draft::Bad(format!(
                "{} to {} only",
                crate::data::units::js_number(lo),
                crate::data::units::js_number(hi)
            ));
        }
    }
    let mut warn = String::new();
    if let Some(m) = meta {
        let below = m.min.is_some_and(|lo| value < lo);
        let above = m.max.is_some_and(|hi| value > hi);
        if below || above {
            let lo = m.min.map(|v| format_value(v, param.kind)).unwrap_or_default();
            let hi = m.max.map(|v| format_value(v, param.kind)).unwrap_or_default();
            warn = if !lo.is_empty() && !hi.is_empty() {
                format!("outside {lo} to {hi}")
            } else if !lo.is_empty() {
                format!("below {lo}")
            } else {
                format!("above {hi}")
            };
        }
    }
    Draft::Ok { value, warn }
}

/// A sort key that puts punctuation before digits before letters, letters
/// without regard to case: how `localeCompare` orders `BAT_X` and `BAT1_X`.
fn collate(a: &str, b: &str) -> Ordering {
    let class = |c: char| {
        if c.is_alphabetic() {
            2
        } else if c.is_ascii_digit() {
            1
        } else {
            0
        }
    };
    let mut ai = a.chars();
    let mut bi = b.chars();
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let (cx, cy) = (class(x), class(y));
                if cx != cy {
                    return cx.cmp(&cy);
                }
                let (lx, ly) = (
                    x.to_lowercase().next().unwrap_or(x),
                    y.to_lowercase().next().unwrap_or(y),
                );
                if lx != ly {
                    return lx.cmp(&ly);
                }
            }
        }
    }
}

/// The parameters in name order.
pub fn sorted(mut params: Vec<Param>) -> Vec<Param> {
    params.sort_by(|a, b| collate(&a.name, &b.name));
    params
}

/// Which parameters the filter shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    All,
    Modified,
    Pending,
}

/// `A` then optional or required digits, then `tail`: `BAT\d*_`, `RC\d+_`.
#[derive(Clone, Copy)]
enum Digits {
    None,
    Opt,
    Req,
}

fn has_family(name: &str, base: &str, digits: Digits, tail: &str) -> bool {
    let Some(rest) = name.strip_prefix(base) else {
        return false;
    };
    let n = rest.chars().take_while(char::is_ascii_digit).count();
    match digits {
        Digits::None if n > 0 => return false,
        Digits::Req if n == 0 => return false,
        _ => {}
    }
    rest[n..].starts_with(tail)
}

/// What a parameter family is for, when the firmware gives no description.
pub fn family_of(name: &str) -> &'static str {
    use Digits::{None as N, Opt as O, Req as R};
    const T: &[(&str, Digits, &str, &str)] = &[
        ("MPC", N, "_", "Multicopter position and velocity control."),
        ("MC", N, "_", "Multicopter attitude and rate control."),
        ("FW", N, "_", "Fixed wing control."),
        ("VT", N, "_", "VTOL transition and control."),
        (
            "EKF2",
            N,
            "_",
            "State estimator: how sensors are fused into position and attitude.",
        ),
        (
            "EK2",
            N,
            "_",
            "State estimator: how sensors are fused into position and attitude.",
        ),
        (
            "EK3",
            N,
            "_",
            "State estimator: how sensors are fused into position and attitude.",
        ),
        (
            "COM",
            N,
            "_",
            "Commander: arming, failsafes and general vehicle behaviour.",
        ),
        ("NAV", N, "_", "Navigator: mission, loiter and failsafe behaviour."),
        ("RTL", N, "_", "Return to launch and landing behaviour."),
        ("LAND", N, "_", "Return to launch and landing behaviour."),
        ("GF", N, "_", "Geofence."),
        ("FENCE", N, "_", "Geofence."),
        ("BAT", O, "_", "Battery measurement and limits."),
        ("BATT", O, "_", "Battery measurement and limits."),
        ("CAL", N, "_", "Sensor calibration data."),
        ("SENS", N, "_", "Sensor drivers and settings."),
        ("SYS", N, "_", "System configuration."),
        ("MAV", N, "_", "MAVLink communication."),
        ("RC", R, "_", "Settings of one RC input channel."),
        ("RCMAP", N, "_", "Which RC channel controls which axis."),
        ("RC", N, "_", "RC input."),
        ("PWM", N, "_", "PWM outputs."),
        ("SDLOG", N, "_", "Logging."),
        ("LOG", N, "_", "Logging."),
        ("LNDMC", N, "_", "Multicopter land detection."),
        ("CA", N, "_", "Control allocation: how motors and servos are driven."),
        ("FD", N, "_", "Failure detector."),
        ("IMU", N, "_", "IMU processing and filtering."),
        ("ATC", N, "_", "Attitude control: the angle and rate controllers."),
        ("PSC", N, "_", "Position control."),
        ("WPNAV", N, "_", "Waypoint navigation speeds and accelerations."),
        ("LOIT", N, "_", "Loiter mode."),
        ("COMPASS", N, "_", "Compass."),
        ("INS", N, "_", "Inertial sensors: accelerometers and gyros."),
        ("AHRS", N, "_", "Attitude and heading reference."),
        ("ARMING", N, "_", "Arming checks."),
        ("FS", N, "_", "Failsafe."),
        ("GPS", O, "_", "GPS receiver settings."),
        ("MOT", N, "_", "Motor output."),
        ("SERVO", R, "_", "Settings of one servo or motor output."),
        ("SERIAL", R, "_", "Settings of one serial port."),
        ("SR", R, "_", "Telemetry stream rates of one serial port."),
        ("RNGFND", O, "_", "Rangefinder."),
        ("TERRAIN", N, "_", "Terrain following."),
        ("Q", N, "_", "QuadPlane (VTOL)."),
        ("TECS", N, "_", "Plane altitude and speed control."),
        ("ARSPD", O, "_", "Airspeed sensor."),
        ("NTF", N, "_", "Notifications: LEDs and buzzer."),
        ("BRD", N, "_", "Board settings."),
        ("SCHED", N, "_", "Scheduler."),
        ("SYSID", N, "_", "System and ground station identity."),
        ("AUTOTUNE", N, "_", "Autotune."),
        ("FLTMODE", R, "", "Flight mode slot."),
        ("FRAME", N, "_", "Frame type."),
        ("MIS", N, "_", "Mission."),
    ];
    T.iter()
        .find(|(b, d, t, _)| has_family(name, b, *d, t))
        .map_or("", |(_, _, _, text)| text)
}

/// The tooltip behind a row's info icon (makeInfoHint).
pub fn info_text(name: &str, meta: Option<&Meta>, kind: i64) -> String {
    let fmt = |v: f64| format_value(v, kind);
    let mut lines: Vec<String> = Vec::new();
    let unit = meta
        .filter(|m| !m.units.is_empty())
        .map(|m| format!(" {}", m.units))
        .unwrap_or_default();
    let short = meta.map_or("", |m| m.short_desc.as_str());
    let long = meta.map_or("", |m| m.long_desc.as_str());
    if !short.is_empty() {
        lines.push(short.into());
    }
    if !long.is_empty() && long != short {
        lines.push(long.into());
    }
    if short.is_empty() && long.is_empty() {
        let family = family_of(name);
        if !family.is_empty() {
            lines.push(family.into());
        }
        lines.push("The firmware gives no description for this parameter.".into());
    }
    if let Some(d) = meta.and_then(|m| m.default) {
        lines.push(format!("Default: {}{unit}", fmt(d)));
    }
    if let Some(m) = meta {
        if m.min.is_some() || m.max.is_some() {
            let lo = m.min.map(fmt).unwrap_or_default();
            let hi = m.max.map(|v| format!("{}{unit}", fmt(v))).unwrap_or_default();
            lines.push(format!("Range: {lo} to {hi}"));
        } else if !m.units.is_empty() {
            lines.push(format!("Unit: {}", m.units));
        }
        if !m.values.is_empty() {
            let v: Vec<String> = m
                .values
                .iter()
                .map(|(v, l)| {
                    if l.is_empty() {
                        fmt(*v)
                    } else {
                        format!("{} {l}", fmt(*v))
                    }
                })
                .collect();
            lines.push(format!("Values: {}", v.join(", ")));
        }
        if !m.bitmask.is_empty() {
            let b: Vec<String> = m.bitmask.iter().map(|(i, l)| format!("{i} {l}")).collect();
            lines.push(format!("Bits: {}", b.join(", ")));
        }
        if m.reboot_required {
            lines.push("Changing it needs a reboot.".into());
        }
    }
    lines.join("\n")
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Prompt { incomplete: Option<(u64, u64)> },
    Downloading { received: u64, count: u64 },
    Editor,
}

#[derive(Debug, Clone, PartialEq)]
enum MetaState {
    Idle,
    Loading { received: f64, size: f64 },
    Ready { cache: bool },
    Unavailable(String),
}

type Row = (String, &'static str);

/// Where the cells of one row go, relative to the row's top left.
struct Metrics {
    w: f32,
    h: f32,
    name_x: f32,
    name_w: f32,
    name_cy: f32,
    value_x: f32,
    value_w: f32,
    default_x: f32,
    default_text_w: f32,
    reset_x: f32,
    apply_x: f32,
    apply_w: f32,
    status_x: f32,
    status_w: f32,
    ctl_cy: f32,
    status_cy: f32,
}

enum Dialog {
    Reboot,
    Export(ExportDlg),
    Import(ImportDlg),
}

/// An import being written; the result is read once a second.
struct Upload {
    start: Option<Pending<Result<(), String>>>,
    poll: Option<Pending<Result<Value, String>>>,
    last_poll: f64,
    began: bool,
}
type WriteInFlight = (String, f64, Pending<Result<(), String>>);

/// The page's state.
pub struct ParamsPage {
    server: Server,
    phase: Phase,
    params: Vec<Param>,
    meta: Option<HashMap<String, Meta>>,
    meta_state: MetaState,
    search: String,
    mode: Mode,
    pending: HashMap<String, String>,
    row_status: HashMap<String, Row>,
    status: Option<(String, bool)>,
    adopt: Option<Pending<Result<Value, String>>>,
    start: Option<Pending<Result<(), String>>>,
    poll: Option<Pending<Result<Value, String>>>,
    meta_poll: Option<Pending<Result<Value, String>>>,
    meta_start: Option<Pending<Result<Value, String>>>,
    last_poll: f64,
    last_meta_poll: f64,
    writes: Vec<WriteInFlight>,
    errors: Vec<String>,
    notes: Vec<String>,
    dialog: Option<Dialog>,
    act: (&'static str, String),
    upload: Option<Upload>,
    reboot: Option<Pending<Result<(), String>>>,
    refresh: Option<Pending<Result<Value, String>>>,
}

impl ParamsPage {
    pub fn new(server: Server, ctx: &egui::Context) -> ParamsPage {
        let s = server.clone();
        ParamsPage {
            adopt: Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/params"))),
            server,
            phase: Phase::Prompt { incomplete: None },
            params: Vec::new(),
            meta: None,
            meta_state: MetaState::Idle,
            search: String::new(),
            mode: Mode::All,
            pending: HashMap::new(),
            row_status: HashMap::new(),
            status: None,
            start: None,
            poll: None,
            meta_poll: None,
            meta_start: None,
            last_poll: 0.0,
            last_meta_poll: 0.0,
            writes: Vec::new(),
            errors: Vec::new(),
            notes: Vec::new(),
            dialog: None,
            act: ("", String::new()),
            upload: None,
            reboot: None,
            refresh: None,
        }
    }

    /// Notices for the notification board.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Errors for the notification board.
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn matches(&self, p: &Param, query: &str) -> bool {
        if self.mode == Mode::Modified && !self.is_modified(p) {
            return false;
        }
        if self.mode == Mode::Pending && !self.pending.contains_key(&p.name) {
            return false;
        }
        if query.is_empty() {
            return true;
        }
        if p.name.to_lowercase().contains(query) {
            return true;
        }
        self.meta_of(&p.name)
            .is_some_and(|m| m.short_desc.to_lowercase().contains(query))
    }

    fn meta_of(&self, name: &str) -> Option<&Meta> {
        self.meta.as_ref().and_then(|m| m.get(name))
    }

    fn is_modified(&self, p: &Param) -> bool {
        self.meta_of(&p.name)
            .and_then(|m| m.default)
            .is_some_and(|d| !values_equal(p.value, d, p.kind))
    }

    fn set_params(&mut self, list: Vec<Param>) {
        self.params = sorted(list);
        let params = self.params.clone();
        let meta = self.meta.clone();
        self.pending.retain(|name, text| {
            let Some(p) = params.iter().find(|p| &p.name == name) else {
                return false;
            };
            match parse_draft(text, p, meta.as_ref().and_then(|m| m.get(name))) {
                Draft::Ok { value, .. } => !values_equal(value, p.value, p.kind),
                Draft::Bad(_) => true,
            }
        });
    }

    fn read_params(d: &Value) -> Vec<Param> {
        d.get("params")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        Some(Param {
                            name: p.get("name")?.as_str()?.to_string(),
                            value: p.get("value")?.as_f64()?,
                            kind: p.get("type").and_then(Value::as_i64).unwrap_or(9),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn show_editor(&mut self, ctx: &egui::Context, list: Vec<Param>) {
        self.set_params(list);
        self.phase = Phase::Editor;
        if self.meta.is_none() {
            self.load_meta(ctx);
        }
    }

    fn load_meta(&mut self, ctx: &egui::Context) {
        self.meta_state = MetaState::Loading {
            received: 0.0,
            size: 0.0,
        };
        let s = self.server.clone();
        self.meta_start = Some(Pending::spawn(ctx, move || {
            http::post_json(&s, "/api/params/metadata", &json!({}))
        }));
    }

    fn on_meta(&mut self, st: &Value) {
        let state = st.get("state").and_then(Value::as_str).unwrap_or("");
        if state == "ready" {
            if let Some(map) = st.get("params").and_then(Value::as_object) {
                self.meta = Some(map.iter().map(|(k, v)| (k.clone(), Meta::from_value(v))).collect());
                self.meta_state = MetaState::Ready {
                    cache: st.get("source").and_then(Value::as_str) == Some("cache"),
                };
                return;
            }
        }
        if state == "ready" || state == "loading" {
            self.meta_state = MetaState::Loading {
                received: st.get("received").and_then(Value::as_f64).unwrap_or(0.0),
                size: st.get("size").and_then(Value::as_f64).unwrap_or(0.0),
            };
            return;
        }
        self.meta_state = match st.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()) {
            Some(e) => MetaState::Unavailable(e.to_string()),
            None => MetaState::Idle,
        };
    }

    fn start_download(&mut self, ctx: &egui::Context) {
        self.status = Some(("Requesting parameter list\u{2026}".into(), false));
        let s = self.server.clone();
        self.start = Some(Pending::spawn(ctx, move || {
            let d = http::post_json(&s, "/api/params/download", &json!({}))?;
            if d.get("ok") == Some(&Value::Bool(true)) {
                Ok(())
            } else {
                Err(d
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Command rejected by vehicle")
                    .to_string())
            }
        }));
    }

    /// Takes in what the background requests have answered.
    fn take_answers(&mut self, ctx: &egui::Context, now: f64) {
        if let Some(r) = self.adopt.as_ref().and_then(Pending::take) {
            self.adopt = None;
            if let Ok(d) = r {
                if matches!(self.phase, Phase::Prompt { .. }) {
                    let list = Self::read_params(&d);
                    if d.get("complete") == Some(&Value::Bool(true)) && !list.is_empty() {
                        self.show_editor(ctx, list);
                    } else if d.get("state").and_then(Value::as_str) == Some("downloading") {
                        self.phase = Phase::Downloading { received: 0, count: 0 };
                    }
                }
            }
        }
        if let Some(r) = self.start.as_ref().and_then(Pending::take) {
            self.start = None;
            match r {
                Ok(()) => {
                    self.status = None;
                    self.phase = Phase::Downloading { received: 0, count: 0 };
                }
                Err(e) => {
                    self.status = Some((
                        if e.is_empty() {
                            "Could not start download".into()
                        } else {
                            e
                        },
                        true,
                    ))
                }
            }
        }
        if let Some(r) = self.poll.as_ref().and_then(Pending::take) {
            self.poll = None;
            if let Ok(d) = r {
                let received = d.get("received").and_then(Value::as_u64).unwrap_or(0);
                let count = d.get("count").and_then(Value::as_u64).unwrap_or(0);
                let state = d.get("state").and_then(Value::as_str).unwrap_or("");
                if matches!(self.phase, Phase::Downloading { .. }) {
                    self.phase = Phase::Downloading { received, count };
                    let list = Self::read_params(&d);
                    if d.get("complete") == Some(&Value::Bool(true)) || state == "complete" {
                        if !list.is_empty() && d.get("complete") == Some(&Value::Bool(true)) {
                            self.show_editor(ctx, list);
                        }
                    } else if state == "incomplete" {
                        self.phase = Phase::Prompt {
                            incomplete: Some((received, count)),
                        };
                    }
                }
            }
        }
        if let Some(r) = self.meta_start.as_ref().and_then(Pending::take) {
            self.meta_start = None;
            match r {
                Ok(st) => self.on_meta(&st),
                Err(e) => self.meta_state = MetaState::Unavailable(if e.is_empty() { "no answer".into() } else { e }),
            }
        }
        if let Some(r) = self.meta_poll.as_ref().and_then(Pending::take) {
            self.meta_poll = None;
            if let Ok(st) = r {
                self.on_meta(&st);
            }
        }
        let mut done = Vec::new();
        for (i, (_, _, p)) in self.writes.iter().enumerate() {
            if let Some(r) = p.take() {
                done.push((i, r));
            }
        }
        for (i, r) in done.into_iter().rev() {
            let (name, value, _) = self.writes.remove(i);
            match r {
                Ok(()) => {
                    if let Some(p) = self.params.iter_mut().find(|p| p.name == name) {
                        p.value = value;
                    }
                    self.pending.remove(&name);
                    let reboot = self.meta_of(&name).is_some_and(|m| m.reboot_required);
                    self.row_status.insert(
                        name,
                        (if reboot { "saved, reboot to apply" } else { "saved" }.into(), "ok"),
                    );
                }
                Err(e) => {
                    let msg = if e.is_empty() { "set failed".to_string() } else { e };
                    self.row_status.insert(name.clone(), (msg.clone(), "err"));
                    self.errors.push(format!("Could not set {name}: {msg}"));
                }
            }
        }
        self.take_file_answers(ctx, now);
        // The ~1 s polls while something is on its way.
        if matches!(self.phase, Phase::Downloading { .. }) && self.poll.is_none() && now - self.last_poll >= POLL_SECS {
            self.last_poll = now;
            let s = self.server.clone();
            self.poll = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/params")));
        }
        if matches!(self.meta_state, MetaState::Loading { .. })
            && self.meta_poll.is_none()
            && self.meta_start.is_none()
            && now - self.last_meta_poll >= POLL_SECS
        {
            self.last_meta_poll = now;
            let s = self.server.clone();
            self.meta_poll = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/params/metadata")));
        }
        if matches!(self.phase, Phase::Downloading { .. })
            || matches!(self.meta_state, MetaState::Loading { .. })
            || !self.writes.is_empty()
            || self.upload.is_some()
            || self.reboot.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn take_file_answers(&mut self, ctx: &egui::Context, now: f64) {
        let mut saved: Option<(String, u64)> = None;
        if let Some(Dialog::Export(d)) = &mut self.dialog {
            if let Some(r) = d.target.as_ref().and_then(Pending::take) {
                d.target = None;
                if let Ok(t) = r {
                    let get = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    if d.name.is_empty() {
                        d.name = get("filename");
                    }
                    if d.dir.is_empty() {
                        d.dir = get("dir");
                    }
                    let f = get("format");
                    d.format = if files::FORMATS.iter().any(|x| x.id == f) {
                        f
                    } else {
                        files::format_from_name(&d.name).unwrap_or("qgc").to_string()
                    };
                }
            }
            if let Some(r) = d.save.as_ref().and_then(Pending::take) {
                d.save = None;
                match r {
                    Ok(res) => {
                        saved = Some((
                            res.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
                            res.get("param_count").and_then(Value::as_u64).unwrap_or(0),
                        ));
                    }
                    Err(e) => d.message = Some(if e.is_empty() { "Export failed".into() } else { e }),
                }
            }
        }
        if let Some((path, count)) = saved {
            self.dialog = None;
            self.act = ("ok", format!("Saved {count} parameters to {path}"));
            self.notes.push(format!("Parameters exported to {path}"));
        }
        if let Some(r) = self.reboot.as_ref().and_then(Pending::take) {
            self.reboot = None;
            match r {
                Ok(()) => {
                    self.notes
                        .push("The autopilot is rebooting. The link returns in a few seconds.".into());
                    self.params.clear();
                    self.meta = None;
                    self.meta_state = MetaState::Idle;
                    self.phase = Phase::Prompt { incomplete: None };
                    self.status = None;
                }
                Err(e) => self.errors.push(format!(
                    "Reboot refused: {}",
                    if e.is_empty() { "no answer".to_string() } else { e }
                )),
            }
        }
        let mut finished: Option<Value> = None;
        let mut failed: Option<String> = None;
        if let Some(u) = &mut self.upload {
            if let Some(r) = u.start.as_ref().and_then(Pending::take) {
                u.start = None;
                match r {
                    Ok(()) => u.began = true,
                    Err(e) => failed = Some(if e.is_empty() { "Upload failed".into() } else { e }),
                }
            }
            if let Some(r) = u.poll.as_ref().and_then(Pending::take) {
                u.poll = None;
                if let Ok(v) = r {
                    if matches!(
                        v.get("state").and_then(Value::as_str),
                        Some("upload_complete" | "interrupted")
                    ) {
                        finished = Some(v);
                    }
                }
            }
            if u.began && u.poll.is_none() && finished.is_none() && now - u.last_poll >= POLL_SECS {
                u.last_poll = now;
                let s = self.server.clone();
                u.poll = Some(Pending::spawn(ctx, move || {
                    http::get_json(&s, "/api/params/upload/result")
                }));
            }
        }
        if let Some(e) = failed {
            self.upload = None;
            self.act = ("err", e.clone());
            self.errors.push(e);
        }
        if let Some(r) = finished {
            self.upload = None;
            let written = r.get("written").and_then(Value::as_u64).unwrap_or(0) as usize;
            let bad = r.get("failed").and_then(Value::as_u64).unwrap_or(0);
            let interrupted = r.get("state").and_then(Value::as_str) == Some("interrupted");
            if bad == 0 && !interrupted {
                let t = format!("Uploaded {}", files::plural(written, "parameter"));
                self.act = ("ok", t.clone());
                self.notes.push(t);
            } else {
                let mut t = format!("Uploaded {written}, {bad} failed");
                if interrupted {
                    t.push_str(", the link dropped before the end");
                }
                let names: Vec<&str> = r
                    .get("errors")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| e.get("name").and_then(Value::as_str))
                            .take(3)
                            .collect()
                    })
                    .unwrap_or_default();
                if !names.is_empty() {
                    t.push_str(&format!(": {}", names.join(", ")));
                }
                self.act = (if written > 0 { "ok" } else { "err" }, t.clone());
                self.errors.push(t);
            }
            let s = self.server.clone();
            self.refresh = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/params")));
        }
        if let Some(r) = self.refresh.as_ref().and_then(Pending::take) {
            self.refresh = None;
            if let Ok(d) = r {
                let list = Self::read_params(&d);
                if d.get("complete") == Some(&Value::Bool(true)) && !list.is_empty() {
                    self.set_params(list);
                }
            }
        }
    }

    fn open_export(&mut self, ctx: &egui::Context) {
        let s = self.server.clone();
        self.dialog = Some(Dialog::Export(ExportDlg::new(Pending::spawn(ctx, move || {
            http::get_json(&s, "/api/params/export/target")
        }))));
    }

    fn do_export(&mut self, ctx: &egui::Context) {
        let only = matches!(&self.dialog, Some(Dialog::Export(d)) if d.only_modified);
        let list: Vec<Value> = self
            .params
            .iter()
            .filter(|p| !only || self.is_modified(p))
            .map(|p| json!({"name": p.name, "value": p.value, "type": p.kind}))
            .collect();
        let s = self.server.clone();
        if let Some(Dialog::Export(d)) = &mut self.dialog {
            if list.is_empty() {
                d.message = Some("Nothing to export: every parameter is at its default.".into());
                return;
            }
            d.message = None;
            let body = json!({
                "filename": d.name.trim(),
                "dir": d.dir.trim(),
                "format": d.format,
                "params": list,
            });
            d.save = Some(Pending::spawn(ctx, move || {
                http::post_json(&s, "/api/params/export", &body)
            }));
        }
    }

    fn read_import(&mut self) {
        let loaded = matches!(self.phase, Phase::Editor) && !self.params.is_empty();
        let Some(Dialog::Import(d)) = &mut self.dialog else {
            return;
        };
        let raw = d.path.trim();
        let path = match (raw.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
            _ => std::path::PathBuf::from(raw),
        };
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("the file")
            .to_string();
        let text = match std::fs::read(&path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                d.message = Some(format!("Could not read file: {e}"));
                return;
            }
        };
        match files::parse(&text, &file) {
            Ok(parsed) => {
                let meta = &self.meta;
                let reboot = |n: &str| meta.as_ref().and_then(|m| m.get(n)).is_some_and(|m| m.reboot_required);
                let set = if loaded { Some(self.params.as_slice()) } else { None };
                d.message = None;
                d.preview = Some((files::preview(&parsed, set, &reboot), file));
            }
            Err(e) => d.message = Some(e),
        }
    }

    fn start_upload(&mut self, ctx: &egui::Context, list: Vec<(String, f64)>) {
        let body: Vec<Value> = list.iter().map(|(n, v)| json!({"name": n, "value": v})).collect();
        self.act = (
            "pending",
            format!("Uploading {}\u{2026}", files::plural(list.len(), "parameter")),
        );
        let s = self.server.clone();
        self.upload = Some(Upload {
            start: Some(Pending::spawn(ctx, move || {
                let d = http::post_json(&s, "/api/params/upload", &json!({"params": body}))?;
                if d.get("ok") == Some(&Value::Bool(true)) {
                    Ok(())
                } else {
                    Err(d
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Upload failed")
                        .to_string())
                }
            })),
            poll: None,
            last_poll: 0.0,
            began: false,
        });
    }

    fn start_reboot(&mut self, ctx: &egui::Context) {
        let s = self.server.clone();
        self.reboot = Some(Pending::spawn(ctx, move || {
            let d = http::post_json(&s, "/api/mavlink/reboot", &json!({}))?;
            if d.get("ok") == Some(&Value::Bool(false)) {
                Err(d.get("error").and_then(Value::as_str).unwrap_or("").to_string())
            } else {
                Ok(())
            }
        }));
    }

    /// The dialog that is open, drawn over the page.
    fn dialogs(&mut self, ctx: &egui::Context, armed: bool) {
        let total = self.params.len();
        let modified = self
            .meta
            .as_ref()
            .map(|_| self.params.iter().filter(|p| self.is_modified(p)).count());
        let mut close = false;
        let mut go = false;
        match &mut self.dialog {
            None => return,
            Some(Dialog::Reboot) => {
                let (c, g) = dialogs::reboot(ctx);
                close = c;
                go = g;
            }
            Some(Dialog::Export(d)) => match dialogs::export(ctx, d, total, modified) {
                dialogs::Out::Close => close = true,
                dialogs::Out::Go => go = true,
                dialogs::Out::None => {}
            },
            Some(Dialog::Import(d)) => match dialogs::import(ctx, d, armed) {
                dialogs::Out::Close => close = true,
                dialogs::Out::Go => go = true,
                dialogs::Out::None => {}
            },
        }
        if go {
            match &self.dialog {
                Some(Dialog::Reboot) => {
                    self.start_reboot(ctx);
                    close = true;
                }
                Some(Dialog::Export(_)) => self.do_export(ctx),
                Some(Dialog::Import(d)) if d.preview.is_none() => self.read_import(),
                Some(Dialog::Import(d)) => {
                    let list = d.preview.as_ref().map(|(p, _)| p.to_write.clone()).unwrap_or_default();
                    self.start_upload(ctx, list);
                    close = true;
                }
                None => {}
            }
        }
        if close {
            self.dialog = None;
        }
    }

    /// `.setup-head` of the page: the title, and the actions on the
    /// subtitle's line (under the title when it would be left under 280 px).
    fn head(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, armed: bool) -> f32 {
        let editor = matches!(self.phase, Phase::Editor) && !self.params.is_empty();
        let uploading = self.upload.is_some();
        let export = Button::new("Export", Variant::Primary)
            .icon("download")
            .small()
            .bold()
            .enabled(editor)
            .title("Save the parameters to a file");
        let import = Button::new("Import", Variant::Primary)
            .icon("upload")
            .small()
            .bold()
            .enabled(!armed && !uploading)
            .title("Write a parameter file (.params, .param or .json) to the vehicle");
        let reboot = Button::new("Reboot autopilot", Variant::Secondary)
            .icon("power")
            .small()
            .bold()
            .enabled(!armed && self.reboot.is_none());
        let bh = export.height();
        let color = match self.act.0 {
            "ok" => style::healthy(),
            "err" => style::critical(),
            _ => style::nav(),
        };
        let sg = kit::wrapped_weight(ui, &self.act.1, 11.5, Weight::W400, color, 420.0, 1.3);
        let has_status = !self.act.1.is_empty();
        let widths = [
            if has_status { sg.size().x } else { 0.0 },
            export.width(ui),
            import.width(ui),
            reboot.width(ui),
        ];
        let one_line: f32 = widths.iter().filter(|v| **v > 0.0).sum::<f32>()
            + 10.0 * (widths.iter().filter(|v| **v > 0.0).count() as f32 - 1.0);
        let wrap = w - one_line - 24.0 < 280.0;
        let head_w = if wrap { w } else { (w - one_line - 24.0).min(w) };
        let head_bottom = pagekit::header_wrapped(
            ui,
            x,
            y,
            head_w,
            "Parameters",
            "Every parameter with its default. Change values, load and save parameter files",
        );
        let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
        let mut used = 0.0;
        for (i, iw) in widths.iter().enumerate() {
            if *iw <= 0.0 {
                continue;
            }
            let line = lines.last_mut().unwrap();
            if !line.is_empty() && used + 10.0 + iw > w {
                lines.push(vec![i]);
                used = *iw;
            } else {
                used += if line.is_empty() { 0.0 } else { 10.0 };
                used += iw;
                line.push(i);
            }
        }
        let line_w = |l: &Vec<usize>| l.iter().map(|i| widths[*i]).sum::<f32>() + 10.0 * (l.len() as f32 - 1.0);
        let block_w = lines.iter().map(line_w).fold(0.0, f32::max);
        let line_h = |l: &Vec<usize>| {
            if l.contains(&0) {
                sg.size().y.max(bh)
            } else {
                bh
            }
        };
        let block_h: f32 = lines.iter().map(line_h).sum::<f32>() + 10.0 * (lines.len() as f32 - 1.0);
        let (bx, top, bottom) = if wrap {
            (x, head_bottom + 10.0, head_bottom + 10.0 + block_h)
        } else {
            (x + w - block_w, head_bottom - block_h, head_bottom)
        };
        let mut ly = top;
        let mut buttons = [Some(export), Some(import), Some(reboot)];
        let mut presses = [false; 3];
        for l in &lines {
            let mut cx = bx + block_w - line_w(l);
            let h = line_h(l);
            for &i in l {
                if i == 0 {
                    ui.painter()
                        .galley(Pos2::new(cx, ly + (h - sg.size().y) / 2.0), sg.clone(), color);
                } else if let Some(b) = buttons[i - 1].take() {
                    presses[i - 1] = b.show(
                        ui,
                        Rect::from_min_size(Pos2::new(cx, ly + (h - bh) / 2.0), Vec2::new(widths[i], bh)),
                        ("paramsact", i),
                    );
                }
                cx += widths[i] + 10.0;
            }
            ly += h + 10.0;
        }
        if presses[0] {
            self.open_export(&ui.ctx().clone());
        }
        if presses[1] {
            self.dialog = Some(Dialog::Import(ImportDlg {
                path: String::new(),
                message: None,
                preview: None,
            }));
        }
        if presses[2] {
            self.dialog = Some(Dialog::Reboot);
        }
        bottom + 26.0
    }

    fn write(&mut self, ctx: &egui::Context, name: &str, value: f64) {
        self.row_status.insert(name.to_string(), ("saving".into(), "pending"));
        let s = self.server.clone();
        let n = name.to_string();
        self.writes.push((
            name.to_string(),
            value,
            Pending::spawn(ctx, move || {
                let d = http::post_json(&s, "/api/params/set", &json!({ "name": n, "value": value }))?;
                if d.get("ok") == Some(&Value::Bool(true)) {
                    Ok(())
                } else {
                    Err(d
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Command rejected by vehicle")
                        .to_string())
                }
            }),
        ));
    }

    fn apply_all(&mut self, ctx: &egui::Context) {
        let mut todo = Vec::new();
        for (name, text) in &self.pending {
            let Some(p) = self.params.iter().find(|p| &p.name == name) else {
                continue;
            };
            if let Draft::Ok { value, .. } = parse_draft(text, p, self.meta_of(name)) {
                if !values_equal(value, p.value, p.kind) {
                    todo.push((name.clone(), value));
                }
            }
        }
        for (name, value) in todo {
            self.write(ctx, &name, value);
        }
    }

    /// The vehicle's armed flag, from telemetry.
    fn armed(state: &Value) -> bool {
        state.get("armed") == Some(&Value::Bool(true))
    }

    /// Draws the page's body into the column `x .. x + w` from `y`;
    /// returns the bottom and whether the pending bar should show.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx, now);
        let armed = Self::armed(state);
        let narrow = Vw::of(&ctx).max720;
        let y = self.head(ui, x, y, w, armed);
        self.dialogs(&ctx, armed);
        let pad = 19.0;
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let ix = x + pad;
        let iw = w - 2.0 * pad;
        let mut cy = y + pad;
        match self.phase.clone() {
            Phase::Prompt { incomplete } => cy = self.prompt(ui, &ctx, ix, cy, iw, incomplete),
            Phase::Downloading { received, count } => cy = self.progress(ui, ix, cy, iw, received, count),
            Phase::Editor => cy = self.editor(ui, &ctx, ix, cy, iw, armed, narrow),
        }
        let card = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + pad));
        card::fill(&painter, bg, card, card::Look::Page);
        card.bottom()
    }

    fn prompt(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        mut y: f32,
        w: f32,
        incomplete: Option<(u64, u64)>,
    ) -> f32 {
        let title = if incomplete.is_some() {
            "Download incomplete"
        } else {
            "Parameter Set"
        };
        y = pagekit::section_title(ui, x, y, w, title);
        let text = match incomplete {
            Some((r, c)) => format!(
                "The vehicle sent {r} of {c} parameters before the link lost the rest. Try again. A cable is more reliable than a radio for this."
            ),
            None => "Read every parameter from the vehicle to edit them. Until then, Corvus streams only what it needs to fly. The editor opens once the whole set has arrived, and the defaults are read from the vehicle right after.".into(),
        };
        let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_2(), w - 28.0, 1.55);
        let box_ = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 24.0));
        {
            let r = box_;
            card::paint(ui.painter(), r, Look::Note);
        }
        ui.painter().galley(Pos2::new(x + 14.0, y + 12.0), g, style::text_2());
        y = box_.bottom() + 12.0;
        let busy = self.start.is_some();
        let label = if incomplete.is_some() {
            "Download again"
        } else {
            "Download Parameters"
        };
        let b = Button::new(label, Variant::Primary)
            .icon("download")
            .bold()
            .enabled(!busy);
        let bw = b.width(ui) + 8.0;
        let bh = b.height() + 2.0;
        if b.show(ui, Rect::from_min_size(Pos2::new(x, y), Vec2::new(bw, bh)), "paramsdl") {
            self.start_download(ctx);
        }
        y += bh;
        if let Some((s, err)) = &self.status {
            let c = if *err { style::critical() } else { style::text_2() };
            y += 12.0 + pagekit::text(ui, Pos2::new(x, y + 0.0), s, 11.5, Weight::W400, c);
        }
        y
    }

    fn progress(&mut self, ui: &mut egui::Ui, x: f32, mut y: f32, w: f32, received: u64, count: u64) -> f32 {
        y = pagekit::section_title(ui, x, y, w, "Downloading Parameters");
        let bar = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 8.0));
        let painter = ui.painter();
        {
            let r = bar;
            card::paint(painter, r, Look::custom(style::surface_3(), style::border_soft(), 4));
        }
        let pct = if count > 0 {
            ((received as f64 / count as f64) * 100.0).round().min(100.0)
        } else {
            0.0
        };
        if pct > 0.0 {
            let fill = Rect::from_min_size(bar.min, Vec2::new(bar.width() * pct as f32 / 100.0, 8.0));
            painter.rect_filled(fill, 4, style::accent());
        }
        y = bar.bottom() + 8.0;
        let label = if count > 0 {
            format!("{received} / {count} parameters \u{00b7} {pct}%")
        } else {
            "Requesting parameter list\u{2026}".into()
        };
        y + pagekit::text(ui, Pos2::new(x, y), &label, 12.0, Weight::W400, style::text_2())
    }

    #[allow(clippy::too_many_arguments)]
    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        mut y: f32,
        w: f32,
        armed: bool,
        narrow: bool,
    ) -> f32 {
        // .params-head: the section title and a Reload button.
        let title = format!("Parameters ({})", self.params.len());
        let reload = Button::new("Reload", Variant::Secondary)
            .icon("refresh-cw")
            .small()
            .title("Read every parameter from the vehicle again");
        let rw = reload.width(ui);
        let rh = reload.height();
        let after = pagekit::section_title(ui, x, y, w - rw - 10.0, &title);
        if reload.show(
            ui,
            Rect::from_min_size(Pos2::new(x + w - rw, y - 2.0), Vec2::new(rw, rh)),
            "paramsreload",
        ) {
            self.start_download(ctx);
            self.phase = Phase::Downloading { received: 0, count: 0 };
        }
        y = after.max(y + rh) + 12.0 - 12.0;
        if armed {
            let g = kit::wrapped_weight(
                ui,
                "Parameter editing disabled while armed",
                11.5,
                Weight::W400,
                style::warning(),
                w - 28.0,
                1.4,
            );
            let r = Rect::from_min_size(Pos2::new(x, y + 12.0), Vec2::new(w, g.size().y + 24.0));
            {
                card::paint(ui.painter(), r, Look::Tone(Tone::Warn));
            }
            ui.painter().galley(r.min + Vec2::new(14.0, 12.0), g, style::warning());
            y = r.bottom();
        }
        y += 12.0;
        y = self.meta_note(ui, ctx, x, y);
        // the toolbar
        let field_h: f32 = 2.0 + 16.0 + 12.0 * 1.21;
        let query = self.search.trim().to_lowercase();
        let filtered: Vec<usize> = (0..self.params.len())
            .filter(|i| self.matches(&self.params[*i], &query))
            .collect();
        let modified = self
            .meta
            .as_ref()
            .map(|_| self.params.iter().filter(|p| self.is_modified(p)).count());
        let items = vec![
            ("All".to_string(), self.params.len().to_string(), true),
            (
                "Modified".to_string(),
                modified.map(|n| n.to_string()).unwrap_or_default(),
                self.meta.is_some(),
            ),
            (
                "Unsaved".to_string(),
                if self.pending.is_empty() {
                    String::new()
                } else {
                    self.pending.len().to_string()
                },
                true,
            ),
        ];
        let sel = match self.mode {
            Mode::All => 0,
            Mode::Modified => 1,
            Mode::Pending => 2,
        };
        let count_text = format!(
            "{} parameter{}",
            filtered.len(),
            if filtered.len() == 1 { "" } else { "s" }
        );
        let cg = style::galley(ui, &count_text, 11.0, Weight::W400, style::text_3(), 0.0);
        let seg_w = pagekit::segmented_width(ui, &items);
        let search_h = field_h.max(34.5);
        let seg_h = 3.0 * 2.0 + 2.0 + 5.0 * 2.0 + 11.0 * 1.21;
        let widths = [200.0, seg_w, cg.size().x];
        let heights = [search_h, seg_h, cg.size().y];
        y += 4.0;
        for line in toolbar_lines(w, seg_w, cg.size().x) {
            let lh = line.iter().map(|i| heights[*i]).fold(0.0, f32::max);
            let rest: f32 = line.iter().filter(|i| **i != 0).map(|i| widths[*i] + 10.0).sum();
            let mut cx = x;
            for &i in &line {
                let iw = if i == 0 { (w - rest).max(0.0) } else { widths[i] };
                let at = Pos2::new(cx, y + (lh - heights[i]) / 2.0);
                match i {
                    0 => {
                        kit::text_field(
                            ui,
                            Rect::from_min_size(at, Vec2::new(iw, search_h)),
                            &mut self.search,
                            "Search names and descriptions\u{2026}",
                            false,
                            "paramsearch",
                        );
                    }
                    1 => {
                        if let (_, Some(k)) = pagekit::segmented(ui, at, &items, sel, "pfilter") {
                            self.mode = [Mode::All, Mode::Modified, Mode::Pending][k];
                        }
                    }
                    _ => ui.painter().galley(at, cg.clone(), style::text_3()),
                }
                cx += iw + 10.0;
            }
            y += lh + 10.0;
        }
        y += -10.0 + 6.0;
        ui.painter().hline(
            egui::Rangef::new(x, x + w),
            y + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        y += 1.0;
        // the rows
        for &i in filtered.iter().take(CAP) {
            let p = self.params[i].clone();
            let m = self.metrics(ui, &p, w, narrow);
            self.row(ui, ctx, Pos2::new(x, y), &m, &p, armed);
            y += m.h;
        }
        let note = if filtered.len() > CAP {
            Some(format!(
                "Showing {CAP} of {}. Refine the search to see more.",
                filtered.len()
            ))
        } else if filtered.is_empty() {
            Some(
                if query.is_empty() && self.mode == Mode::Modified {
                    "Every parameter is at its default."
                } else if query.is_empty() && self.mode == Mode::Pending {
                    "No unsaved changes."
                } else {
                    "No parameter matches the search."
                }
                .to_string(),
            )
        } else {
            None
        };
        if let Some(n) = note {
            let g = style::galley(ui, &n, 11.0, Weight::W400, style::text_3(), 0.0);
            ui.painter().galley(
                Pos2::new(x + (w - g.size().x) / 2.0, y + 10.0),
                g.clone(),
                style::text_3(),
            );
            y += 20.0 + g.size().y;
        }
        // the unsaved bar closes the card as the sticky bar does at the end
        if !self.pending.is_empty() {
            y = self.pending_bar(ui, ctx, x, y + 4.0, w, armed);
        }
        y
    }

    fn meta_note(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, mut y: f32) -> f32 {
        match self.meta_state.clone() {
            MetaState::Loading { received, size } => {
                let pct = if size > 0.0 {
                    format!(" {}%", ((received / size) * 100.0).round().min(100.0))
                } else {
                    String::new()
                };
                let h = pagekit::text(
                    ui,
                    Pos2::new(x, y),
                    &format!("Reading the defaults from the vehicle\u{2026}{pct}"),
                    11.5,
                    Weight::W400,
                    style::text_2(),
                );
                y += h + 12.0;
            }
            MetaState::Unavailable(err) => {
                let g = style::galley(
                    ui,
                    &format!("Defaults are not available: {err}."),
                    11.5,
                    Weight::W400,
                    style::warning(),
                    0.0,
                );
                let b = Button::new("Try again", Variant::Secondary).small();
                let (bw, bh) = (b.width(ui), b.height());
                ui.painter()
                    .galley(Pos2::new(x, y + (bh - g.size().y) / 2.0), g.clone(), style::warning());
                if b.show(
                    ui,
                    Rect::from_min_size(Pos2::new(x + g.size().x + 10.0, y), Vec2::new(bw, bh)),
                    "metaretry",
                ) {
                    self.load_meta(ctx);
                }
                y += bh + 12.0;
            }
            MetaState::Ready { cache: true } => {
                let h = pagekit::text(
                    ui,
                    Pos2::new(x, y),
                    "Defaults from the copy kept on this computer, matched to the vehicle by checksum. Settings, Files turns this off.",
                    11.5,
                    Weight::W400,
                    style::text_2(),
                );
                y += h + 12.0;
            }
            _ => {}
        }
        y
    }

    fn pending_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, armed: bool) -> f32 {
        let n = self.pending.len();
        let bar = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 10.0 * 2.0 + 29.0));
        {
            let r = bar;
            card::paint(
                ui.painter(),
                r,
                Look::custom(style::surface_1(), style::nav(), style::RADIUS_BTN),
            );
        }
        let text = format!("{n} unsaved change{}", if n == 1 { "" } else { "s" });
        let g = style::galley(ui, &text, 12.0, Weight::W600, style::text_1(), 0.0);
        ui.painter().galley(
            Pos2::new(bar.left() + 12.0, bar.center().y - g.size().y / 2.0),
            g,
            style::text_1(),
        );
        let apply = Button::new("Apply all", Variant::Primary)
            .icon("check")
            .small()
            .enabled(!armed && !n.eq(&0) && self.writes.is_empty());
        let discard = Button::new("Discard", Variant::Secondary)
            .small()
            .enabled(self.writes.is_empty());
        let (aw, dw, bh) = (apply.width(ui), discard.width(ui), apply.height());
        let ar = Rect::from_min_size(
            Pos2::new(bar.right() - 12.0 - aw, bar.center().y - bh / 2.0),
            Vec2::new(aw, bh),
        );
        let dr = Rect::from_min_size(Pos2::new(ar.left() - 10.0 - dw, ar.top()), Vec2::new(dw, bh));
        if discard.show(ui, dr, "pdiscard") {
            self.pending.clear();
            self.row_status.clear();
        }
        if apply.show(ui, ar, "papply") {
            self.apply_all(ctx);
        }
        bar.bottom()
    }

    fn draft_of(&self, p: &Param) -> String {
        self.pending
            .get(&p.name)
            .cloned()
            .unwrap_or_else(|| format_value(p.value, p.kind))
    }

    /// The row's status text: the draft's problem, the last answer, or the
    /// draft's warning.
    fn status_of(&self, p: &Param, parsed: &Draft, changed: bool) -> (String, egui::Color32) {
        match (parsed, self.row_status.get(&p.name)) {
            (Draft::Bad(e), _) => (e.clone(), style::critical()),
            (_, Some((t, cls))) => (
                t.clone(),
                match *cls {
                    "ok" => style::healthy(),
                    "err" => style::critical(),
                    "pending" => style::nav(),
                    _ => style::text_3(),
                },
            ),
            (Draft::Ok { warn, .. }, None) if changed && !warn.is_empty() => (warn.clone(), style::warning()),
            _ => (String::new(), style::text_3()),
        }
    }

    /// Where a row's cells go, `.params-row`: five columns at
    /// `minmax(150px, 1.5fr) minmax(120px, 1fr) 96px 64px 84px`, or from 720
    /// px down `minmax(0, 1fr) auto auto` with the name above, the value,
    /// default and Apply beside each other and the status below. Padding is
    /// `7px 6px 7px 9px`, the gap 10.
    fn metrics(&self, ui: &egui::Ui, p: &Param, w: f32, narrow: bool) -> Metrics {
        let meta = self.meta_of(&p.name);
        let inner_l = 9.0;
        let inner_w = w - 9.0 - 6.0;
        if !narrow {
            let h = if self.meta.is_some() { 48.0 } else { 44.0 };
            let free = inner_w - 96.0 - 64.0 - 84.0 - 4.0 * 10.0;
            let name_w = (free * 0.6).max(150.0);
            let value_w = (free * 0.4).max(120.0);
            let value_x = inner_l + name_w + 10.0;
            let default_x = value_x + value_w + 10.0;
            let apply_x = default_x + 96.0 + 10.0;
            return Metrics {
                w,
                h,
                name_x: inner_l,
                name_w,
                name_cy: h / 2.0,
                value_x,
                value_w,
                default_x,
                default_text_w: 96.0 - 28.0,
                reset_x: default_x + 96.0 - 24.0,
                apply_x,
                apply_w: 64.0,
                status_x: apply_x + 64.0 + 10.0,
                status_w: 84.0,
                ctl_cy: h / 2.0,
                status_cy: h / 2.0,
            };
        }
        let name_h = ui
            .painter()
            .layout_no_wrap("M".into(), style::mono(12.0), style::text_1())
            .size()
            .y;
        let has_desc = meta.is_some_and(|m| !m.short_desc.is_empty());
        let name_block = name_h + if has_desc { 2.0 + 11.0 * 1.35 } else { 0.0 };
        let draft = self.draft_of(p);
        let parsed = parse_draft(&draft, p, meta);
        let changed = match &parsed {
            Draft::Ok { value, .. } => !values_equal(*value, p.value, p.kind),
            Draft::Bad(_) => true,
        };
        let default = meta.and_then(|m| m.default);
        let at_default =
            default.is_some_and(|d| matches!(&parsed, Draft::Ok { value, .. } if values_equal(*value, d, p.kind)));
        let default_text_w = default.map_or(0.0, |d| {
            style::galley_mono(ui, &format_value(d, p.kind), 11.5, style::text_3())
                .size()
                .x
        });
        let default_w = match default {
            Some(_) if !at_default => default_text_w + 4.0 + 24.0,
            Some(_) => default_text_w,
            None => 0.0,
        };
        let apply = Button::new("Apply", Variant::Primary).small();
        let (apply_w, apply_h) = (apply.width(ui), apply.height());
        let field_h: f32 = 2.0 + 12.0 + 12.0 * 1.21;
        let ctl_h = field_h
            .max(apply_h)
            .max(if default.is_some() && !at_default { 24.0 } else { 0.0 });
        let (text, _) = self.status_of(p, &parsed, changed);
        let status_h = if text.is_empty() {
            0.0
        } else {
            kit::wrapped_weight(ui, &text, 10.5, Weight::W400, style::text_3(), inner_w, 1.2)
                .size()
                .y
        };
        let value_w = (inner_w - default_w - apply_w - 20.0).max(60.0);
        let default_x = inner_l + value_w + 10.0;
        let ctl_top = 7.0 + name_block + 10.0;
        let status_top = ctl_top + ctl_h + 10.0;
        Metrics {
            w,
            h: status_top + status_h + 7.0 + 1.0,
            name_x: inner_l,
            name_w: inner_w,
            name_cy: 7.0 + name_block / 2.0,
            value_x: inner_l,
            value_w,
            default_x,
            default_text_w,
            reset_x: default_x + default_text_w + 4.0,
            apply_x: default_x + default_w + 10.0,
            apply_w,
            status_x: inner_l,
            status_w: inner_w,
            ctl_cy: ctl_top + ctl_h / 2.0,
            status_cy: status_top + status_h / 2.0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, at: Pos2, m: &Metrics, p: &Param, armed: bool) {
        let (w, h) = (m.w, m.h);
        let meta = self.meta_of(&p.name).cloned();
        let name = p.name.clone();
        let modified = self.is_modified(p);
        let painter = ui.painter().clone();
        let row = Rect::from_min_size(at, Vec2::new(w, h));
        let left = row.left();
        let (name_x, name_w, value_x, value_w) = (left + m.name_x, m.name_w, left + m.value_x, m.value_w);
        let (default_x, apply_x, status_x) = (left + m.default_x, left + m.apply_x, left + m.status_x);
        let ctl_y = row.top() + m.ctl_cy;
        painter.hline(
            row.x_range(),
            row.bottom() - 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        if modified {
            painter.rect_filled(
                Rect::from_min_size(row.min, Vec2::new(3.0, h - 1.0)),
                0,
                style::accent(),
            );
        }
        // name, info icon and the one line description
        let weight = if modified { Weight::W700 } else { Weight::W400 };
        let mono = ui
            .painter()
            .layout_no_wrap(name.clone(), style::mono(12.0), style::text_1());
        let _ = weight;
        let desc = meta.as_ref().map(|m| m.short_desc.clone()).unwrap_or_default();
        let name_h = mono.size().y;
        let desc_h = if desc.is_empty() { 0.0 } else { 2.0 + 11.0 * 1.35 };
        let top = row.top() + m.name_cy - (name_h + desc_h) / 2.0;
        let clip = Rect::from_min_size(Pos2::new(name_x, row.top()), Vec2::new(name_w, h));
        let np = ui.painter_at(clip);
        np.galley(Pos2::new(name_x, top), mono.clone(), style::text_1());
        if modified {
            np.galley(Pos2::new(name_x + 0.5, top), mono.clone(), style::text_1());
        }
        crate::ui::popover::info(
            ui,
            Pos2::new(
                (name_x + mono.size().x + 5.0 + 8.0).min(name_x + name_w - 8.0),
                top + name_h / 2.0,
            ),
            &name,
            &info_text(&name, meta.as_ref(), p.kind),
            ("pinfo", &name),
        );
        if !desc.is_empty() {
            let dg = style::galley(ui, &desc, 11.0, Weight::W400, style::text_3(), 0.0);
            np.galley(Pos2::new(name_x, top + name_h + 2.0), dg, style::text_3());
        }
        // the value
        let draft = self
            .pending
            .get(&name)
            .cloned()
            .unwrap_or_else(|| format_value(p.value, p.kind));
        let unit = meta.as_ref().map(|m| m.units.clone()).unwrap_or_default();
        let unit_w = if unit.is_empty() {
            0.0
        } else {
            style::galley(ui, &unit, 11.0, Weight::W400, style::text_3(), 0.0)
                .size()
                .x
                + 6.0
        };
        let field_h: f32 = 2.0 + 12.0 + 12.0 * 1.21;
        let field = Rect::from_min_size(
            Pos2::new(value_x, ctl_y - field_h / 2.0),
            Vec2::new(value_w - unit_w, field_h),
        );
        let parsed = parse_draft(&draft, p, meta.as_ref());
        let changed = match &parsed {
            Draft::Ok { value, .. } => !values_equal(*value, p.value, p.kind),
            Draft::Bad(_) => true,
        };
        let enum_values: Option<Vec<Choice>> = meta.as_ref().filter(|m| !m.values.is_empty()).map(|m| {
            let mut v: Vec<Choice> = m
                .values
                .iter()
                .map(|(val, label)| {
                    let t = format_value(*val, p.kind);
                    Choice::new(t.clone(), if label.is_empty() { t } else { format!("{label} ({t})") })
                })
                .collect();
            if !v.iter().any(|c| c.value == draft) {
                v.push(Choice::new(draft.clone(), format!("{draft} (not in the list)")));
            }
            v
        });
        let mut new_text: Option<String> = None;
        if let Some(choices) = &enum_values {
            if armed {
                let label = choices
                    .iter()
                    .find(|c| c.value == draft)
                    .map_or(draft.clone(), |c| c.label.clone());
                {
                    let r = field;
                    card::paint(&painter, r, Look::Raised);
                }
                painter.galley(
                    Pos2::new(field.left() + 11.0, field.center().y - 8.0),
                    style::galley(ui, &label, 12.0, Weight::W400, style::text_2(), 0.0),
                    style::text_2(),
                );
            } else if let Some(v) = kit::select(ui, field, choices, &draft, "", false, "", ("pval", &name)) {
                new_text = Some(v);
            }
        } else {
            let mut text = draft.clone();
            let border = match (&parsed, changed) {
                (Draft::Bad(_), _) => Some(style::critical()),
                (Draft::Ok { warn, .. }, _) if !warn.is_empty() => Some(style::warning()),
                (_, true) => Some(style::nav()),
                _ => None,
            };
            if let Some(c) = border {
                painter.rect_stroke(
                    field.expand(1.0),
                    style::RADIUS_BTN + 1,
                    Stroke::new(2.0, c.gamma_multiply(0.25)),
                    egui::StrokeKind::Inside,
                );
            }
            if armed {
                {
                    let r = field;
                    card::paint(&painter, r, Look::Raised);
                }
                painter.galley(
                    Pos2::new(field.left() + 9.0, field.center().y - 8.0),
                    style::galley_mono(ui, &text, 12.0, style::text_2()),
                    style::text_2(),
                );
            } else {
                let resp = kit::text_field(ui, field, &mut text, "", true, ("pval", &name));
                if resp.changed() {
                    new_text = Some(text);
                }
                if let Some(c) = border {
                    painter.rect_stroke(field, style::RADIUS_BTN, Stroke::new(1.0, c), egui::StrokeKind::Inside);
                }
                if resp.has_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Escape))
                    && self.pending.contains_key(&name)
                {
                    new_text = Some(format_value(p.value, p.kind));
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if let Draft::Ok { value, .. } = &parsed {
                        if changed && !armed {
                            self.write(ctx, &name, *value);
                        }
                    }
                }
            }
        }
        let bits = meta
            .as_ref()
            .filter(|m| !m.bitmask.is_empty())
            .map(|m| {
                format!(
                    " Bits: {}",
                    m.bitmask
                        .iter()
                        .map(|(i, l)| format!("{i} {l}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .unwrap_or_default();
        let tip = format!(
            "{}{}{bits}",
            type_name(p.kind),
            if type_name(p.kind).is_empty() { "" } else { "." }
        );
        if !tip.is_empty() {
            ui.interact(field, ui.scope_id().with(("ptip", &name)), egui::Sense::hover())
                .on_hover_text(tip);
        }
        if !unit.is_empty() {
            let g = style::galley(ui, &unit, 11.0, Weight::W400, style::text_3(), 0.0);
            painter.galley(
                Pos2::new(field.right() + 6.0, ctl_y - g.size().y / 2.0),
                g,
                style::text_3(),
            );
        }
        // the default and its reset
        let default = meta.as_ref().and_then(|m| m.default);
        if let Some(d) = default {
            let g = style::galley_mono(ui, &format_value(d, p.kind), 11.5, style::text_3());
            ui.painter_at(Rect::from_min_size(
                Pos2::new(default_x, row.top()),
                Vec2::new(m.default_text_w, h),
            ))
            .galley(Pos2::new(default_x, ctl_y - g.size().y / 2.0), g, style::text_3());
            let at_default = matches!(&parsed, Draft::Ok { value, .. } if values_equal(*value, d, p.kind));
            if !at_default {
                let r = Rect::from_min_size(Pos2::new(left + m.reset_x, ctl_y - 12.0), Vec2::splat(24.0));
                if !armed
                    && kit::icon_btn(
                        ui,
                        r,
                        "rotate-ccw",
                        &format!("Set to the default, {}", format_value(d, p.kind)),
                        false,
                        ("preset", &name),
                    )
                {
                    new_text = Some(format_value(d, p.kind));
                }
            }
        }
        // Apply
        let can = !armed && self.writes.is_empty() && matches!(&parsed, Draft::Ok { .. }) && changed;
        let apply = Button::new("Apply", if can { Variant::Primary } else { Variant::Inert })
            .small()
            .enabled(can);
        let bh = apply.height();
        if apply.show(
            ui,
            Rect::from_min_size(Pos2::new(apply_x, ctl_y - bh / 2.0), Vec2::new(m.apply_w, bh)),
            ("papplyrow", &name),
        ) {
            if let Draft::Ok { value, .. } = &parsed {
                self.write(ctx, &name, *value);
            }
        }
        let (text, color) = self.status_of(p, &parsed, changed);
        if !text.is_empty() {
            let g = kit::wrapped_weight(ui, &text, 10.5, Weight::W400, color, m.status_w, 1.2);
            painter.galley(
                Pos2::new(status_x, row.top() + m.status_cy - g.size().y / 2.0),
                g,
                color,
            );
        }
        if let Some(t) = new_text {
            let back_to_current = matches!(parse_draft(&t, p, meta.as_ref()), Draft::Ok { value, .. } if values_equal(value, p.value, p.kind));
            if back_to_current {
                self.pending.remove(&name);
            } else {
                self.pending.insert(name.clone(), t);
            }
            self.row_status.remove(&name);
        }
    }
}

/// `.params-toolbar` is a wrapping flex row of the search (basis 200), the
/// filter and the count: which of them share a line at width `w`.
fn toolbar_lines(w: f32, seg: f32, count: f32) -> Vec<Vec<usize>> {
    let mut lines = vec![vec![0]];
    let mut used = 200.0;
    for (i, iw) in [(1, seg), (2, count)] {
        if used + 10.0 + iw <= w {
            used += 10.0 + iw;
            lines.last_mut().unwrap().push(i);
        } else {
            lines.push(vec![i]);
            used = iw;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, value: f64, kind: i64) -> Param {
        Param {
            name: name.into(),
            value,
            kind,
        }
    }

    #[test]
    fn the_toolbar_wraps_like_a_flex_row() {
        assert_eq!(toolbar_lines(600.0, 180.0, 70.0), vec![vec![0, 1, 2]]);
        assert_eq!(toolbar_lines(400.0, 180.0, 70.0), vec![vec![0, 1], vec![2]]);
        assert_eq!(toolbar_lines(300.0, 180.0, 70.0), vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn values_as_the_page_writes_them() {
        assert_eq!(format_value(10.0, 6), "10");
        assert_eq!(format_value(9.6, 6), "10", "an integer type rounds");
        assert_eq!(format_value(0.1, 9), "0.1");
        assert_eq!(
            format_value(0.1f32 as f64, 9),
            "0.1",
            "the float's long tail is not shown"
        );
        assert_eq!(format_value(1.5, 9), "1.5");
        assert_eq!(format_value(0.0012, 9), "0.0012");
        assert_eq!(format_value(12.345, 9), "12.345");
        assert!(values_equal(0.1, 0.1f32 as f64, 9));
        assert!(values_equal(3.2, 3.4, 6), "whole numbers compare rounded");
        assert!(!values_equal(f64::NAN, 1.0, 9));
    }

    #[test]
    fn drafts_are_read_as_parse_draft_reads_them() {
        let int8 = p("A", 0.0, 2);
        assert_eq!(parse_draft("", &int8, None), Draft::Bad("not a number".into()));
        assert_eq!(parse_draft("x", &int8, None), Draft::Bad("not a number".into()));
        assert_eq!(parse_draft("1.5", &int8, None), Draft::Bad("whole numbers only".into()));
        assert_eq!(parse_draft("200", &int8, None), Draft::Bad("-128 to 127 only".into()));
        assert_eq!(
            parse_draft(" 7 ", &int8, None),
            Draft::Ok {
                value: 7.0,
                warn: String::new()
            }
        );
        let real = p("B", 1.0, 9);
        let meta = Meta {
            min: Some(0.0),
            max: Some(10.0),
            ..Default::default()
        };
        assert_eq!(
            parse_draft("12", &real, Some(&meta)),
            Draft::Ok {
                value: 12.0,
                warn: "outside 0 to 10".into()
            }
        );
        let lo = Meta {
            min: Some(0.0),
            ..Default::default()
        };
        assert_eq!(
            parse_draft("-1", &real, Some(&lo)),
            Draft::Ok {
                value: -1.0,
                warn: "below 0".into()
            }
        );
        let hi = Meta {
            max: Some(5.0),
            ..Default::default()
        };
        assert_eq!(
            parse_draft("6", &real, Some(&hi)),
            Draft::Ok {
                value: 6.0,
                warn: "above 5".into()
            }
        );
    }

    #[test]
    fn names_sort_as_the_page_sorts_them() {
        let names = ["BAT_X", "BAT1_X", "bat2_x", "COM_A", "BAT10_X", "BAT2_X"];
        let sorted: Vec<String> = sorted(names.iter().map(|n| p(n, 0.0, 9)).collect())
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(sorted, vec!["BAT_X", "BAT1_X", "BAT10_X", "BAT2_X", "bat2_x", "COM_A"]);
    }

    #[test]
    fn families_and_the_info_text() {
        assert_eq!(
            family_of("MPC_XY_VEL_MAX"),
            "Multicopter position and velocity control."
        );
        assert_eq!(family_of("BAT1_V_DIV"), "Battery measurement and limits.");
        assert_eq!(family_of("BAT_LOW_THR"), "Battery measurement and limits.");
        assert_eq!(family_of("RC3_MIN"), "Settings of one RC input channel.");
        assert_eq!(family_of("RC_MAP_ROLL"), "RC input.");
        assert_eq!(family_of("FLTMODE2"), "Flight mode slot.");
        assert_eq!(family_of("ZZZ_NOPE"), "");
        let meta = Meta {
            default: Some(2.0),
            units: "m".into(),
            min: Some(1.0),
            max: Some(9.0),
            reboot_required: true,
            ..Default::default()
        };
        assert_eq!(
            info_text("MPC_X", Some(&meta), 9),
            "Multicopter position and velocity control.\nThe firmware gives no description for this parameter.\nDefault: 2 m\nRange: 1 to 9 m\nChanging it needs a reboot."
        );
    }
}
