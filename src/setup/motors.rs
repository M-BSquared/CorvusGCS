//! The Motors page of Setup (python/src/js/setup-motors.js, the `.motors-*` rules
//! of setup-forms.css).
//!
//! The centrepiece is a drawing of the actual airframe: every motor at its
//! real distance from the centre of gravity, spinning the way its parameters
//! say it spins, so "which pin is the front left motor on?" is answered by
//! clicking the motor you are looking at. Beside it are the same numbers as
//! tables, the protocol forms the backend describes, and the motor test.
//!
//! The motor test spins real hardware. PX4 refuses it while armed, the
//! bridge bounds its duration so the vehicle stops the motor even if the link
//! dies, and the button stays off until the operator confirms the propellers
//! are removed. That confirmation is asked again every time the page opens,
//! and leaving the page stops any motor that is still turning.

use std::collections::HashMap;
use std::f32::consts::PI;

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::check::{self, Check};
use crate::setup::schema_form::{self, Field, FormState, Look};
use crate::ui::card;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

const VIEW: f32 = 340.0;
const CENTER: f32 = VIEW / 2.0;
const ARM_PX: f32 = 118.0;
const HUB_PX: f32 = 20.0;
const MIN_SPAN_M: f32 = 1e-4;
const SENSOR_REACH: f32 = 0.55;
pub const FRAME_MIN_M: f64 = 0.02;
pub const FRAME_MAX_M: f64 = 5.0;
const THROTTLE_STEPS: [u32; 7] = [5, 10, 15, 20, 30, 40, 50];
const DEFAULT_THROTTLE: u32 = 15;
const TEST_DURATION_S: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub bank: String,
    pub pin: i64,
    pub label: String,
    pub function: String,
    pub motor: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Motor {
    pub number: i64,
    pub label: String,
    pub x: f32,
    pub y: f32,
    pub spin: String,
    pub thrust: String,
    pub axis: (f32, f32),
    pub output: Option<(String, i64, String)>,
    pub fields: Vec<Field>,
    pub output_fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub struct Sensor {
    pub kind: String,
    pub label: String,
    pub tag: String,
    pub hint: String,
    pub x: f32,
    pub y: f32,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub adjustable: bool,
    pub hint: String,
    pub note: String,
    pub dims: Vec<(&'static str, &'static str, f64)>,
    pub center: (f64, f64),
    pub motors: Vec<FrameMotor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameMotor {
    pub x: f64,
    pub y: f64,
    pub x_param: String,
    pub y_param: String,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub hint: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub connected: bool,
    pub error: String,
    pub received: u64,
    pub motors: Vec<Motor>,
    pub sensors: Vec<Sensor>,
    pub outputs: Vec<Output>,
    pub sections: Vec<Section>,
    pub geometry: Vec<Field>,
    pub frame: Option<Frame>,
    pub family: String,
    pub family_label: String,
    pub preset: Option<i64>,
    pub fixed_count: bool,
    pub max_motors: usize,
    pub position_hint: String,
}

fn s_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn f_of(v: &Value, k: &str) -> f32 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

/// Reads `GET /api/motors`.
pub fn parse_doc(v: &Value) -> Doc {
    let arr = |k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let motors = arr("motors")
        .iter()
        .map(|m| Motor {
            number: m.get("number").and_then(Value::as_i64).unwrap_or(0),
            label: s_of(m, "label"),
            x: f_of(m, "x"),
            y: f_of(m, "y"),
            spin: s_of(m, "spin"),
            thrust: s_of(m, "thrust"),
            axis: m.get("axis").map_or((0.0, 0.0), |a| (f_of(a, "x"), f_of(a, "y"))),
            output: m.get("output").filter(|o| o.is_object()).map(|o| {
                (
                    s_of(o, "bank"),
                    o.get("pin").and_then(Value::as_i64).unwrap_or(0),
                    s_of(o, "label"),
                )
            }),
            fields: schema_form::parse_fields(m.get("fields").unwrap_or(&Value::Null)),
            output_fields: schema_form::parse_fields(m.get("output_fields").unwrap_or(&Value::Null)),
        })
        .collect();
    let sensors = arr("sensors")
        .iter()
        .map(|s| Sensor {
            kind: s_of(s, "kind"),
            label: s_of(s, "label"),
            tag: Some(s_of(s, "tag"))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| s_of(s, "label")),
            hint: s_of(s, "hint"),
            x: f_of(s, "x"),
            y: f_of(s, "y"),
            fields: schema_form::parse_fields(s.get("fields").unwrap_or(&Value::Null)),
        })
        .collect();
    let outputs = arr("outputs")
        .iter()
        .map(|o| Output {
            bank: s_of(o, "bank"),
            pin: o.get("pin").and_then(Value::as_i64).unwrap_or(0),
            label: s_of(o, "label"),
            function: s_of(o, "function"),
            motor: o.get("motor").and_then(Value::as_i64),
        })
        .collect();
    let sections = arr("sections")
        .iter()
        .map(|s| Section {
            title: s_of(s, "title"),
            hint: s_of(s, "hint"),
            fields: schema_form::parse_fields(s.get("fields").unwrap_or(&Value::Null)),
        })
        .collect();
    let frame = v.get("frame").filter(|f| f.is_object()).map(|f| {
        let num = |k: &str| f.get(k).and_then(Value::as_f64);
        let mut dims = Vec::new();
        for (key, label) in [
            ("diagonal", "Diagonal"),
            ("length", "Front to back"),
            ("width", "Left to right"),
        ] {
            if let Some(d) = num(key) {
                dims.push((key, label, d));
            }
        }
        Frame {
            adjustable: f.get("adjustable") == Some(&Value::Bool(true)),
            hint: s_of(f, "hint"),
            note: s_of(f, "note"),
            dims,
            center: f.get("center").map_or((0.0, 0.0), |c| {
                (
                    c.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                    c.get("y").and_then(Value::as_f64).unwrap_or(0.0),
                )
            }),
            motors: f
                .get("motors")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|m| FrameMotor {
                            x: m.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                            y: m.get("y").and_then(Value::as_f64).unwrap_or(0.0),
                            x_param: s_of(m, "x_param"),
                            y_param: s_of(m, "y_param"),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    });
    Doc {
        connected: v.get("connected") == Some(&Value::Bool(true)),
        error: s_of(v, "error"),
        received: v.get("received").and_then(Value::as_u64).unwrap_or(0),
        motors,
        sensors,
        outputs,
        sections,
        geometry: schema_form::parse_fields(v.get("geometry").unwrap_or(&Value::Null)),
        frame,
        family: Some(s_of(v, "airframe_family"))
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| "multirotor".into()),
        family_label: s_of(v, "airframe_label"),
        preset: v.get("airframe_preset").and_then(Value::as_i64),
        fixed_count: v.get("fixed_motor_count") == Some(&Value::Bool(true)),
        max_motors: v.get("max_motors").and_then(Value::as_u64).unwrap_or(12) as usize,
        position_hint: s_of(v, "position_hint"),
    }
}

/// The farthest a motor sits from the centre of gravity, in metres.
pub fn motor_span(motors: &[Motor]) -> f32 {
    motors.iter().map(|m| m.x.hypot(m.y)).fold(0.0, f32::max)
}

pub fn sensor_span(sensors: &[Sensor]) -> f32 {
    sensors.iter().map(|s| s.x.hypot(s.y)).fold(0.0, f32::max)
}

/// Where a motor is drawn, in the 340 unit box: to scale from the measured
/// positions, or evenly round the ring when none are set.
pub fn motor_point(m: &Motor, index: usize, total: usize, span: f32) -> (f32, f32) {
    if span < MIN_SPAN_M {
        let a = (index as f32 / total.max(1) as f32) * PI * 2.0 - PI / 2.0;
        return (CENTER + a.cos() * ARM_PX, CENTER + a.sin() * ARM_PX);
    }
    let scale = ARM_PX / span;
    (CENTER + m.y * scale, CENTER - m.x * scale)
}

/// Where a flight controller or GPS is drawn.
pub fn sensor_point(s: &Sensor, span: f32, sensors: &[Sensor]) -> (f32, f32) {
    let scale = if span >= MIN_SPAN_M {
        ARM_PX / span
    } else {
        let reach = sensor_span(sensors);
        if reach < MIN_SPAN_M {
            return (CENTER, CENTER);
        }
        (ARM_PX * SENSOR_REACH) / reach
    };
    (CENTER + s.y * scale, CENTER - s.x * scale)
}

/// The writes that resize the frame along `key` to `value` metres: every
/// motor moves about the frame centre, a length leaving widths alone.
pub fn frame_writes(frame: &Frame, key: &str, value: f64) -> Vec<(String, f64)> {
    let current = frame.dims.iter().find(|d| d.0 == key).map_or(0.0, |d| d.2);
    if !frame.adjustable || current <= 0.0 || value <= 0.0 {
        return Vec::new();
    }
    let k = value / current;
    let kx = if key == "width" { 1.0 } else { k };
    let ky = if key == "length" { 1.0 } else { k };
    let (cx, cy) = frame.center;
    let round = |v: f64| (v * 1e4).round() / 1e4;
    let mut writes = Vec::new();
    for m in &frame.motors {
        let x = round(cx + (m.x - cx) * kx);
        let y = round(cy + (m.y - cy) * ky);
        if (x - m.x).abs() > 1e-6 {
            writes.push((m.x_param.clone(), x));
        }
        if (y - m.y).abs() > 1e-6 {
            writes.push((m.y_param.clone(), y));
        }
    }
    writes
}

/// What the output list offers a motor: Unassigned, and every pin, one that
/// drives something else named with its function.
pub fn output_options(outputs: &[Output], motor: i64) -> Vec<Choice> {
    let mut v = vec![Choice::new("", "Unassigned")];
    for o in outputs {
        let mine = o.motor == Some(motor);
        let busy = !mine && o.function != "Disabled";
        v.push(Choice::new(
            format!("{}:{}", o.bank, o.pin),
            if busy {
                format!("{} ({})", o.label, o.function)
            } else {
                o.label.clone()
            },
        ));
    }
    v
}

/// `bank:pin` as the assign endpoint wants it.
pub fn parse_output(value: &str) -> Option<(String, i64)> {
    let cut = value.rfind(':')?;
    let pin: i64 = value[cut + 1..].parse().ok().filter(|p| *p > 0)?;
    Some((value[..cut].to_string(), pin))
}

/// The table's columns, in order: the motors' own fields, then the output pin
/// limits, each once by label.
pub fn columns(motors: &[Motor]) -> Vec<(bool, String, String)> {
    let mut cols: Vec<(bool, String, String)> = Vec::new();
    for pin in [false, true] {
        for m in motors {
            let list = if pin { &m.output_fields } else { &m.fields };
            for f in list {
                if !cols.iter().any(|c| c.0 == pin && c.1 == f.label) {
                    cols.push((pin, f.label.clone(), f.unit.clone()));
                }
            }
        }
    }
    cols
}

/// The page's state.
pub struct MotorsPage {
    server: Server,
    doc: Option<Doc>,
    loading: Option<Pending<Result<Value, String>>>,
    status: (&'static str, String),
    form: FormState,
    selected: i64,
    props_off: bool,
    throttle: u32,
    testing: i64,
    test_until: f64,
    test_status: (&'static str, String),
    test_req: Option<(i64, Pending<Result<(), String>>)>,
    stop_req: Option<Pending<Result<(), String>>>,
    assign: Option<(i64, Pending<Result<(), String>>)>,
    assign_status: HashMap<i64, (&'static str, String)>,
    count_req: Option<Pending<Result<(), String>>>,
    frame_req: Option<Pending<Result<(), String>>>,
    frame_text: HashMap<String, String>,
    frame_status: (&'static str, String),
    errors: Vec<String>,
    check: Check,
    /// The last check, as the card at the top shows it.
    check_card: Option<check::Summary>,
    /// Its verdict, said once the re-read has landed.
    check_say: Option<check::Summary>,
    /// The next read goes to the vehicle, not the cache.
    fresh: bool,
}

impl MotorsPage {
    pub fn new(server: Server, ctx: &egui::Context) -> MotorsPage {
        let mut p = MotorsPage {
            server,
            doc: None,
            loading: None,
            status: ("pending", "Reading motor configuration\u{2026}".into()),
            form: FormState::default(),
            selected: 1,
            props_off: false,
            throttle: DEFAULT_THROTTLE,
            testing: 0,
            test_until: 0.0,
            test_status: ("", String::new()),
            test_req: None,
            stop_req: None,
            assign: None,
            assign_status: HashMap::new(),
            count_req: None,
            frame_req: None,
            frame_text: HashMap::new(),
            frame_status: ("", String::new()),
            errors: Vec::new(),
            check: Check::default(),
            check_card: None,
            check_say: None,
            fresh: false,
        };
        p.load(ctx);
        p
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        e.extend(self.form.take_errors());
        e
    }

    fn load(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        self.status = ("pending", "Reading motor configuration\u{2026}".into());
        let s = self.server.clone();
        let path = if std::mem::take(&mut self.fresh) {
            "/api/motors?fresh=1"
        } else {
            "/api/motors"
        };
        self.loading = Some(Pending::spawn(ctx, move || http::get_json(&s, path)));
    }

    /// `fieldParams`: every editable parameter of the description.
    fn check_names(doc: &Doc) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut add = |fields: &[Field]| {
            for f in fields {
                if !names.contains(&f.param) {
                    names.push(f.param.clone());
                }
            }
        };
        for m in &doc.motors {
            add(&m.fields);
            add(&m.output_fields);
        }
        for s in &doc.sensors {
            add(&s.fields);
        }
        for s in &doc.sections {
            add(&s.fields);
        }
        add(&doc.geometry);
        names
    }

    fn post(ctx: &egui::Context, server: &Server, path: &'static str, body: Value) -> Pending<Result<(), String>> {
        let s = server.clone();
        Pending::spawn(ctx, move || {
            let d = http::post_json(&s, path, &body)?;
            if d.get("ok") == Some(&Value::Bool(true)) {
                Ok(())
            } else {
                Err(d
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Command rejected by vehicle")
                    .to_string())
            }
        })
    }

    fn take_answers(&mut self, ctx: &egui::Context, now: f64) {
        if let Some(summary) = self.check.take(&mut self.form.wanted) {
            self.form.mark_checked(&summary.rows);
            self.check_card = Some(summary.clone());
            self.check_say = Some(summary);
            self.fresh = true;
            self.load(ctx);
        }
        if let Some(p) = &self.loading {
            if let Some(r) = p.take() {
                self.loading = None;
                match r {
                    Ok(v) => {
                        let doc = parse_doc(&v);
                        self.status = if doc.connected {
                            let n = doc.motors.len();
                            (
                                "ok",
                                format!(
                                    "{n} motor{} \u{00b7} {} parameters read",
                                    if n == 1 { "" } else { "s" },
                                    doc.received
                                ),
                            )
                        } else {
                            (
                                "err",
                                if doc.error.is_empty() {
                                    "No motor configuration received".into()
                                } else {
                                    doc.error.clone()
                                },
                            )
                        };
                        if !doc.motors.iter().any(|m| m.number == self.selected) {
                            self.selected = doc.motors.first().map_or(0, |m| m.number);
                        }
                        self.doc = Some(doc);
                        if let Some(c) = self.check_say.take() {
                            self.status = (c.cls, c.text);
                        }
                    }
                    Err(e) => {
                        self.doc = Some(Doc::default());
                        self.status = (
                            "err",
                            if e.is_empty() {
                                "Could not read motor configuration".into()
                            } else {
                                e
                            },
                        );
                    }
                }
            }
        }
        // The form edits the fields it was handed; after a write the page reads again.
        if let Some(doc) = &mut self.doc {
            for m in &mut doc.motors {
                self.form.take_answers(&mut m.fields);
                self.form.take_answers(&mut m.output_fields);
            }
            for s in &mut doc.sensors {
                self.form.take_answers(&mut s.fields);
            }
            for s in &mut doc.sections {
                self.form.take_answers(&mut s.fields);
            }
            self.form.take_answers(&mut doc.geometry);
        }
        if std::mem::take(&mut self.form.written) {
            self.load(ctx);
        }
        if let Some((motor, p)) = &self.test_req {
            if let Some(r) = p.take() {
                let motor = *motor;
                self.test_req = None;
                match r {
                    Ok(()) => self.test_until = now + TEST_DURATION_S,
                    Err(e) => {
                        let msg = if e.is_empty() {
                            "motor test failed".to_string()
                        } else {
                            e
                        };
                        self.end_test("err", msg.clone());
                        self.errors.push(format!("Could not test Motor {motor}: {msg}"));
                    }
                }
            }
        }
        if self.testing != 0 && self.test_req.is_none() && self.test_until > 0.0 && now >= self.test_until {
            let label = self.motor_label(self.testing);
            self.end_test("ok", format!("{label} test finished"));
        }
        if let Some(p) = &self.stop_req {
            if let Some(r) = p.take() {
                self.stop_req = None;
                match r {
                    Ok(()) => self.end_test("ok", "motors stopped".into()),
                    Err(e) => self.end_test("err", if e.is_empty() { "stop failed".into() } else { e }),
                }
            }
        }
        if let Some((motor, p)) = &self.assign {
            if let Some(r) = p.take() {
                let motor = *motor;
                self.assign = None;
                match r {
                    Ok(()) => {
                        self.assign_status.insert(motor, ("ok", "assigned".into()));
                        self.load(ctx);
                    }
                    Err(e) => {
                        let msg = if e.is_empty() {
                            "assignment failed".to_string()
                        } else {
                            e
                        };
                        self.assign_status.insert(motor, ("err", msg.clone()));
                        self.errors
                            .push(format!("Could not assign {}: {msg}", self.motor_label(motor)));
                    }
                }
            }
        }
        if let Some(p) = &self.count_req {
            if let Some(r) = p.take() {
                self.count_req = None;
                match r {
                    Ok(()) => self.load(ctx),
                    Err(e) => self.errors.push(format!(
                        "Could not change the motor count: {}",
                        if e.is_empty() { "write failed".into() } else { e }
                    )),
                }
            }
        }
        if let Some(p) = &self.frame_req {
            if let Some(r) = p.take() {
                self.frame_req = None;
                match r {
                    Ok(()) => self.frame_status = ("ok", "frame resized".into()),
                    Err(e) => {
                        self.frame_status = ("err", e.clone());
                        self.errors.push(format!(
                            "The frame was only partly resized: {e}. Check values writes the motors that did not move."
                        ));
                    }
                }
                self.load(ctx);
            }
        }
        if self.testing != 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn motor_label(&self, number: i64) -> String {
        self.doc
            .as_ref()
            .and_then(|d| d.motors.iter().find(|m| m.number == number))
            .map_or_else(|| format!("Motor {number}"), |m| m.label.clone())
    }

    fn end_test(&mut self, cls: &'static str, text: String) {
        self.testing = 0;
        self.test_until = 0.0;
        self.test_status = (cls, text);
    }

    /// Stops a motor that is still turning; called when the page is left.
    pub fn stop_on_leave(&mut self) {
        if self.testing != 0 {
            let _ = http::post_json(&self.server, "/api/motors/test/stop", &json!({}));
            self.testing = 0;
        }
    }

    /// Draws the page from `y`; returns its bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx, now);
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let reload = Button::new("Reload", Variant::Primary)
            .icon("refresh-cw")
            .small()
            .enabled(self.loading.is_none() && !self.check.busy());
        let (rw, rh) = (reload.width(ui), reload.height());
        let check_btn = Button::new("Check values", Variant::Secondary)
            .icon("list-checks")
            .small()
            .title("Read every value back from the vehicle, write again what did not stick, and show what the vehicle holds now")
            .enabled(
                self.loading.is_none()
                    && !self.check.busy()
                    && self
                        .doc
                        .as_ref()
                        .is_some_and(|d| d.connected && !Self::check_names(d).is_empty()),
            );
        let cw = check_btn.width(ui);
        let color = match self.status.0 {
            "ok" => style::healthy(),
            "err" => style::critical(),
            _ => style::nav(),
        };
        let sg = style::galley(ui, &self.status.1, 11.5, Weight::W400, color, 0.0);
        let status_w = sg.size().x.min(420.0);
        let gap = if status_w > 0.0 { 10.0 } else { 0.0 };
        let actions_w = status_w + gap + rw + 10.0 + cw;
        // the actions wrap under the title when it would have under 280 px
        let wrap = w - actions_w - 24.0 < 280.0;
        let head_w = if wrap { w } else { (w - actions_w - 24.0).min(w) };
        let head_bottom = pagekit::header_wrapped(
            ui,
            x,
            y,
            head_w,
            "Motors",
            "Airframe layout, motor assignment, and output protocol",
        );
        let (ay, bottom) = if wrap {
            (head_bottom + 10.0, head_bottom + 12.0 + rh)
        } else {
            (head_bottom - rh, head_bottom)
        };
        let (status_x, reload_x, check_x) = if wrap {
            (x, x + status_w + gap, x + status_w + gap + rw + 10.0)
        } else {
            let check_x = x + w - cw;
            let reload_x = check_x - 10.0 - rw;
            (reload_x - gap - sg.size().x, reload_x, check_x)
        };
        if check_btn.show(
            ui,
            Rect::from_min_size(Pos2::new(check_x, ay), Vec2::new(cw, rh)),
            "motorscheck",
        ) {
            if let Some(doc) = &self.doc {
                let names = Self::check_names(doc);
                let wanted = self.form.wanted.clone();
                self.status = (
                    "pending",
                    if wanted.is_empty() {
                        "Reading every value back from the vehicle\u{2026}".to_string()
                    } else {
                        format!(
                            "Checking {} {} on the vehicle\u{2026}",
                            wanted.len(),
                            if wanted.len() == 1 { "change" } else { "changes" }
                        )
                    },
                );
                self.check.start(&ctx, &self.server, &wanted, names);
            }
        }
        if reload.show(
            ui,
            Rect::from_min_size(Pos2::new(reload_x, ay), Vec2::new(rw, rh)),
            "motorsreload",
        ) {
            self.check_card = None;
            self.load(&ctx);
        }
        ui.painter()
            .galley(Pos2::new(status_x, ay + rh / 2.0 - sg.size().y / 2.0), sg, color);
        let mut y = bottom + 26.0;
        if armed {
            let g = kit::wrapped_weight(
                ui,
                "Motor configuration is read-only while armed",
                11.5,
                Weight::W400,
                style::warning(),
                w - 28.0,
                1.4,
            );
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 24.0));
            {
                card::paint(ui.painter(), r, card::Look::Tone(Tone::Warn));
            }
            ui.painter().galley(r.min + Vec2::new(14.0, 12.0), g, style::warning());
            y = r.bottom() + 4.0;
        }
        let Some(doc) = self.doc.clone() else { return y };
        if doc.motors.is_empty() && doc.sections.is_empty() && doc.sensors.is_empty() {
            return self.empty_card(ui, x, y, w);
        }
        if let Some(c) = self.check_card.clone() {
            y = check::card(ui, x, y, w, &c) + 14.0;
        }
        let split = !doc.motors.is_empty() && w >= 1240.0;
        let (side_x, side_w, main_x, main_w) = if split {
            (x, 420.0, x + 420.0 + 14.0, w - 420.0 - 14.0)
        } else {
            (x, w, x, w)
        };
        let mut sy = y;
        if !doc.motors.is_empty() {
            sy = self.airframe_card(ui, &ctx, &doc, side_x, sy, side_w, armed) + 14.0;
        }
        let mut my = if split { y } else { sy };
        if !doc.motors.is_empty() {
            my = self.table_card(ui, &ctx, &doc, main_x, my, main_w, armed) + 14.0;
            my = self.test_card(ui, &ctx, &doc, main_x, my, main_w, armed, now) + 14.0;
        }
        // The protocol forms share rows: auto-fit, minmax(330px, 1fr).
        if !doc.sections.is_empty() {
            let cols = (((main_w + 14.0) / (330.0 + 14.0)).floor() as usize)
                .max(1)
                .min(doc.sections.len());
            let cw = (main_w - 14.0 * (cols as f32 - 1.0)) / cols as f32;
            let mut row_top = my;
            let mut row_bottom = my;
            for (i, s) in doc.sections.iter().enumerate() {
                let col = i % cols;
                if col == 0 && i > 0 {
                    row_top = row_bottom + 14.0;
                }
                let b = self.section_card(ui, &ctx, s, main_x + col as f32 * (cw + 14.0), row_top, cw, armed);
                row_bottom = if col == 0 { b } else { row_bottom.max(b) };
                if col == 0 {
                    row_bottom = b;
                }
            }
            my = row_bottom + 14.0;
        }
        (if split { sy.max(my) } else { my }) - 14.0
    }

    fn empty_card(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Motors") - 2.0;
        let g = kit::wrapped_weight(ui, "Connect to a vehicle to read its motor configuration. The page shows only what the connected firmware actually reports.", 12.0, Weight::W400, style::text_2(), iw - 28.0, 1.55);
        let r = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, g.size().y + 24.0));
        let card = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, r.bottom() + 19.0));
        card::paint(&painter, card, card::Look::Page);
        {
            card::paint(&painter, r, card::Look::Note);
        }
        painter.galley(r.min + Vec2::new(14.0, 12.0), g, style::text_2());
        cy = card.bottom();
        cy
    }

    #[allow(clippy::too_many_arguments)]
    fn airframe_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = painter.add(Shape::Noop);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Airframe") - 2.0;
        let sensors = &doc.sensors;
        let measured = motor_span(&doc.motors) >= MIN_SPAN_M;
        let span = if measured {
            motor_span(&doc.motors).max(sensor_span(sensors))
        } else {
            0.0
        };
        // two columns from 700 px: the drawing, then the numbers
        let two = iw >= 700.0;
        let fig_w = if two { iw.clamp(280.0, 340.0) } else { iw.min(340.0) };
        let info_x = if two { ix + fig_w + 28.0 } else { ix };
        let info_w = if two { iw - fig_w - 28.0 } else { iw };
        let size = fig_w.clamp(240.0, 340.0);
        let fx = ix + (fig_w - size) / 2.0;
        let fig = Rect::from_min_size(Pos2::new(fx, cy), Vec2::splat(size));
        self.diagram(ui, doc, fig, span);
        let mut fy = fig.bottom() + 6.0;
        let mut legend = vec![if span >= MIN_SPAN_M {
            format!(
                "Outer ring {} m from the centre of gravity",
                crate::setup::schema_form::format_number(f64::from(span * 1000.0).round() / 1000.0)
            )
        } else {
            "No motor positions set, motors shown evenly spaced".into()
        }];
        if doc.family != "multirotor" {
            legend.push(format!(
                "{} outline is schematic; motor positions are to scale",
                if doc.family_label.is_empty() {
                    "Airframe"
                } else {
                    &doc.family_label
                }
            ));
        }
        if !sensors.is_empty() {
            legend.push(if measured {
                "Flight controller and GPS to scale".into()
            } else {
                "Flight controller and GPS show only the direction of their offset".into()
            });
        }
        legend.push("Click a motor to select it".into());
        let (mut lx, mut ly) = (ix, fy);
        for item in &legend {
            let g = style::galley(ui, item, 10.5, Weight::W400, style::text_3(), 0.0);
            if lx > ix && lx + g.size().x > ix + if two { fig_w } else { iw } {
                lx = ix;
                ly += g.size().y + 3.0;
            }
            painter.galley(Pos2::new(lx, ly), g.clone(), style::text_3());
            lx += g.size().x + 14.0;
            fy = ly + g.size().y;
        }
        let mut iy = if two { cy } else { fy + 10.0 };
        if !doc.geometry.is_empty() {
            iy = self.geometry(ui, ctx, &doc.geometry, info_x, iy, info_w, armed) + 10.0;
        }
        if let Some(p) = doc.preset {
            let g = style::galley(
                ui,
                &format!("Airframe preset {p} (SYS_AUTOSTART)"),
                10.5,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            let gh = g.size().y;
            let gw = g.size().x;
            painter.galley(Pos2::new(info_x, iy + (16.0 - gh) / 2.0), g, style::text_3());
            crate::ui::popover::info(
                ui,
                Pos2::new(info_x + gw + 4.0 + 8.0, iy + 8.0),
                "Airframe preset",
                "The preset rewrites a whole configuration and needs a reboot, so it is changed in Parameters, not here.",
                "presetinfo",
            );
            iy += gh.max(16.0) + 10.0;
        }
        if let Some(frame) = &doc.frame {
            iy = self.frame_block(ui, ctx, frame, info_x, iy, info_w, armed) + 10.0;
        }
        if !sensors.is_empty() {
            iy = self.sensor_block(ui, ctx, doc, info_x, iy, info_w, armed) + 10.0;
        }
        cy = (if two { iy.max(fy) } else { iy }) - 10.0;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    #[allow(clippy::too_many_arguments)]
    fn geometry(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        fields: &[Field],
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        // Label over control, side by side, wrapping (`.motors-geometry`): a
        // label row 16 px tall when the label carries a hint icon, a 4 px
        // gap, then the control; 8 px between rows, 14 between fields.
        let (mut cx, mut cy, mut row_h) = (x, y, 0.0f32);
        let server = self.server.clone();
        for f in fields {
            let lg = style::galley(ui, &f.label.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
            let hinted = !f.hint.is_empty();
            let label_row_h = if hinted { 16.0 } else { lg.size().y };
            let label_row_w = lg.size().x + if hinted { 6.0 + 16.0 } else { 0.0 };
            let (control_w, control_h) = match &f.kind {
                schema_form::Kind::Enum(o) | schema_form::Kind::Sign(o) => {
                    let current = o
                        .iter()
                        .find(|(v, _)| *v == f.value.round() as i64)
                        .map_or(0.0, |(_, l)| {
                            style::galley(ui, l, 12.0, Weight::W400, style::text_1(), 0.0).size().x
                        });
                    ((current + 9.0 + 26.0 + 2.0).clamp(130.0, 260.0), 29.0)
                }
                _ => (80.0, 30.0),
            };
            let cw = control_w.max(label_row_w);
            if cx > x && cx + cw > x + w {
                cx = x;
                cy += row_h + 8.0;
                row_h = 0.0;
            }
            ui.painter().galley(
                Pos2::new(cx, cy + (label_row_h - lg.size().y) / 2.0),
                lg.clone(),
                style::text_3(),
            );
            if hinted {
                crate::ui::popover::info(
                    ui,
                    Pos2::new(cx + lg.size().x + 6.0 + 8.0, cy + 8.0),
                    &f.label,
                    &f.hint,
                    ("geohint", &f.param),
                );
            }
            let top = cy + label_row_h + 4.0;
            let (sw, nw) = match &f.kind {
                schema_form::Kind::Enum(_) | schema_form::Kind::Sign(_) => (Some(control_w), None),
                _ => (None, Some(80.0)),
            };
            self.form.control_with(
                ui,
                ctx,
                &server,
                f,
                Pos2::new(cx, top),
                cw,
                control_h,
                armed,
                false,
                sw,
                nw,
            );
            row_h = row_h.max(label_row_h + 4.0 + control_h);
            cx += cw + 14.0;
        }
        cy + row_h
    }

    fn diagram(&mut self, ui: &mut egui::Ui, doc: &Doc, fig: Rect, span: f32) {
        let painter = ui.painter_at(fig.expand(30.0));
        let sc = fig.width() / VIEW;
        let at = |x: f32, y: f32| Pos2::new(fig.left() + x * sc, fig.top() + y * sc);
        let p = |t: (f32, f32)| at(t.0, t.1);
        // ring, axes, nose and FWD
        let ring_c = at(CENTER, CENTER);
        if span < MIN_SPAN_M {
            let n = 36;
            for i in 0..n {
                if i % 2 == 0 {
                    let (a0, a1) = (i as f32 / n as f32 * PI * 2.0, (i as f32 + 1.0) / n as f32 * PI * 2.0);
                    let pts = (0..=6).map(|k| {
                        let a = a0 + (a1 - a0) * k as f32 / 6.0;
                        ring_c + Vec2::new(a.cos(), a.sin()) * ARM_PX * sc
                    });
                    painter.add(Shape::line(pts.collect(), Stroke::new(1.0, style::border())));
                }
            }
        } else {
            painter.circle_stroke(ring_c, ARM_PX * sc, Stroke::new(1.0, style::border()));
        }
        painter.line_segment(
            [at(CENTER, CENTER - ARM_PX), at(CENTER, CENTER + ARM_PX)],
            Stroke::new(1.0, style::border_soft()),
        );
        painter.line_segment(
            [at(CENTER - ARM_PX, CENTER), at(CENTER + ARM_PX, CENTER)],
            Stroke::new(1.0, style::border_soft()),
        );
        painter.add(Shape::convex_polygon(
            vec![
                at(CENTER, CENTER - ARM_PX - 22.0),
                at(CENTER - 9.0, CENTER - ARM_PX - 6.0),
                at(CENTER + 9.0, CENTER - ARM_PX - 6.0),
            ],
            style::accent(),
            Stroke::NONE,
        ));
        let fwd = style::galley(ui, "FWD", 10.0 * sc, Weight::W700, style::text_3(), 0.6 * sc);
        painter.galley(
            Pos2::new(
                at(CENTER, 0.0).x - fwd.size().x / 2.0,
                at(0.0, CENTER - ARM_PX - 28.0).y - fwd.size().y,
            ),
            fwd,
            style::text_3(),
        );
        // the silhouette
        let shape = |pts: &[(f32, f32)], fill: Color32| {
            painter.add(Shape::convex_polygon(
                pts.iter().map(|&(x, y)| at(x, y)).collect(),
                fill,
                Stroke::new(1.5 * sc, style::border()),
            ));
        };
        let total = doc.motors.len();
        let arm = |m: &Motor, i: usize| {
            let q = p(motor_point(m, i, total, span));
            painter.line_segment([ring_c, q], Stroke::new(3.0 * sc, style::text_3().gamma_multiply(0.5)));
            painter.circle_filled(q, 1.5 * sc, style::text_3().gamma_multiply(0.5));
        };
        let c = CENTER;
        let wing = |painter: &egui::Painter| {
            let (hs, wy, ty) = (128.0, c - 12.0, c + 96.0);
            let _ = painter;
            shape(
                &[
                    (c - hs, wy + 26.0),
                    (c - hs + 16.0, wy - 6.0),
                    (c - 14.0, wy - 14.0),
                    (c + 14.0, wy - 14.0),
                    (c + hs - 16.0, wy - 6.0),
                    (c + hs, wy + 26.0),
                    (c + hs - 10.0, wy + 32.0),
                    (c - hs + 10.0, wy + 32.0),
                ],
                style::surface_3(),
            );
            shape(
                &[
                    (c - 56.0, ty + 14.0),
                    (c - 48.0, ty - 2.0),
                    (c + 48.0, ty - 2.0),
                    (c + 56.0, ty + 14.0),
                    (c + 48.0, ty + 18.0),
                    (c - 48.0, ty + 18.0),
                ],
                style::surface_3(),
            );
            shape(
                &[
                    (c, c - 118.0),
                    (c + 11.0, c - 92.0),
                    (c + 13.0, c + 60.0),
                    (c + 7.0, ty + 22.0),
                    (c - 7.0, ty + 22.0),
                    (c - 13.0, c + 60.0),
                    (c - 11.0, c - 92.0),
                ],
                style::surface_2(),
            );
        };
        match doc.family.as_str() {
            "wing" => wing(&painter),
            "vtol" => {
                wing(&painter);
                for (i, m) in doc.motors.iter().enumerate() {
                    if m.thrust != "horizontal" {
                        arm(m, i);
                    }
                }
            }
            "rover" => {
                let r = Rect::from_min_max(at(c - 52.0, c - 96.0), at(c + 52.0, c + 96.0));
                {
                    painter.rect_filled(r, 18.0 * sc, style::surface_2());
                    painter.rect_stroke(
                        r,
                        18.0 * sc,
                        Stroke::new(1.5 * sc, style::border()),
                        egui::StrokeKind::Inside,
                    );
                }
                for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let w = Rect::from_min_max(
                        at(c + sx * 68.0 - 11.0, c + sy * 62.0 - 22.0),
                        at(c + sx * 68.0 + 11.0, c + sy * 62.0 + 22.0),
                    );
                    {
                        let r = w;
                        painter.rect_filled(r, 7.0 * sc, style::surface_3());
                        painter.rect_stroke(
                            r,
                            7.0 * sc,
                            Stroke::new(1.5 * sc, style::border()),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }
            "helicopter" => {
                painter.circle_stroke(at(c, c - 18.0), 96.0 * sc, Stroke::new(1.0, style::border()));
                shape(
                    &[
                        (c, c - 86.0),
                        (c + 22.0, c - 44.0),
                        (c + 20.0, c + 20.0),
                        (c + 7.0, c + 30.0),
                        (c + 7.0, c + 104.0),
                        (c - 7.0, c + 104.0),
                        (c - 7.0, c + 30.0),
                        (c - 20.0, c + 20.0),
                        (c - 22.0, c - 44.0),
                    ],
                    style::surface_2(),
                );
                painter.circle_stroke(at(c + 20.0, c + 104.0), 24.0 * sc, Stroke::new(1.0, style::border()));
            }
            _ => {
                for (i, m) in doc.motors.iter().enumerate() {
                    arm(m, i);
                }
            }
        }
        painter.circle(ring_c, 9.0 * sc, style::surface_3(), Stroke::new(1.0, style::border()));
        // flight controller and GPS
        for s in &doc.sensors {
            let q = p(sensor_point(s, span, &doc.sensors));
            if s.kind == "fc" {
                let r = Rect::from_center_size(q, Vec2::new(18.0, 12.0) * sc);
                {
                    painter.rect_filled(r, 3.0 * sc, style::accent_dim());
                    painter.rect_stroke(
                        r,
                        3.0 * sc,
                        Stroke::new(2.0 * sc, style::accent_bright()),
                        egui::StrokeKind::Inside,
                    );
                }
                let g = style::galley(ui, &s.tag, 10.0 * sc, Weight::W700, style::text_2(), 0.4 * sc);
                painter.galley(
                    Pos2::new(q.x - 13.0 * sc - g.size().x, q.y - g.size().y / 2.0),
                    g,
                    style::text_2(),
                );
            } else {
                painter.circle(q, 7.0 * sc, Color32::TRANSPARENT, Stroke::new(2.0 * sc, style::nav()));
                let g = style::galley(ui, &s.tag, 10.0 * sc, Weight::W700, style::text_2(), 0.4 * sc);
                painter.galley(Pos2::new(q.x + 11.0 * sc, q.y - g.size().y / 2.0), g, style::text_2());
            }
        }
        // the motors, clickable
        let time = ui.input(|i| i.time) as f32;
        for (i, m) in doc.motors.iter().enumerate() {
            let q = p(motor_point(m, i, total, span));
            let hit = Rect::from_center_size(q, Vec2::splat(HUB_PX * 2.0 * sc));
            let resp = ui
                .interact(hit, ui.scope_id().with(("motor", m.number)), egui::Sense::click())
                .on_hover_text(motor_aria(m));
            if resp.clicked() {
                self.selected = m.number;
            }
            let selected = self.selected == m.number;
            let testing = self.testing == m.number;
            let unassigned = m.output.is_none();
            let (mut fill, mut stroke_c, mut stroke_w) = (style::surface_2(), style::border(), 2.0);
            if resp.hovered() {
                fill = style::surface_3();
                stroke_c = style::text_3();
            }
            if selected {
                fill = style::accent_dim();
                stroke_c = style::accent_bright();
                stroke_w = 3.0;
            }
            if testing {
                let pulse = 0.775 + 0.225 * (time / 0.7 * PI * 2.0).cos();
                fill = style::critical_dark().gamma_multiply(pulse);
                stroke_c = style::critical().gamma_multiply(pulse);
                stroke_w = 3.0;
            }
            let r = HUB_PX * sc;
            if unassigned && !selected && !testing {
                stroke_c = style::warning();
                painter.circle_filled(q, r, fill);
                for k in 0..14 {
                    if k % 2 == 0 {
                        let (a0, a1) = (k as f32 / 14.0 * PI * 2.0, (k as f32 + 1.0) / 14.0 * PI * 2.0);
                        let pts: Vec<Pos2> = (0..=4)
                            .map(|j| {
                                q + Vec2::new(
                                    (a0 + (a1 - a0) * j as f32 / 4.0).cos(),
                                    (a0 + (a1 - a0) * j as f32 / 4.0).sin(),
                                ) * r
                            })
                            .collect();
                        painter.add(Shape::line(pts, Stroke::new(stroke_w * sc, stroke_c)));
                    }
                }
            } else {
                painter.circle(q, r, fill, Stroke::new(stroke_w * sc, stroke_c));
            }
            if !m.spin.is_empty() {
                let ccw = m.spin == "CCW";
                let (start, sweep) = (if ccw { 40.0f32 } else { 140.0 }, 250.0f32);
                let end = if ccw { start - sweep } else { start + sweep };
                let rr = (HUB_PX + 7.0) * sc;
                let pts: Vec<Pos2> = (0..=24)
                    .map(|k| {
                        let a = (start + (end - start) * k as f32 / 24.0).to_radians();
                        q + Vec2::new(a.cos(), a.sin()) * rr
                    })
                    .collect();
                let spin_c = style::text_3().gamma_multiply(0.8);
                painter.add(Shape::line(pts.clone(), Stroke::new(1.5 * sc, spin_c)));
                let tangent = end + if ccw { -90.0 } else { 90.0 };
                painter.add(Shape::convex_polygon(
                    triangle(*pts.last().unwrap_or(&q), tangent, 6.0 * sc),
                    spin_c,
                    Stroke::NONE,
                ));
            }
            if m.thrust == "horizontal" && (m.axis.0 != 0.0 || m.axis.1 != 0.0) {
                let (dx, dy) = (m.axis.1, -m.axis.0);
                let len = dx.hypot(dy);
                if len >= 0.2 {
                    let deg = (dy / len).atan2(dx / len).to_degrees();
                    let tip = q + Vec2::new(deg.to_radians().cos(), deg.to_radians().sin()) * (HUB_PX + 16.0) * sc;
                    painter.add(Shape::convex_polygon(
                        triangle(tip, deg, 7.0 * sc),
                        style::nav(),
                        Stroke::NONE,
                    ));
                }
            }
            let num = style::galley(ui, &m.number.to_string(), 15.0 * sc, Weight::W700, style::text_1(), 0.0);
            painter.galley(q - num.size() / 2.0, num, style::text_1());
            let out_text = m.output.as_ref().map_or("unassigned".to_string(), |o| o.2.clone());
            let oc = if unassigned {
                style::warning()
            } else if selected {
                style::text_2()
            } else {
                style::text_3()
            };
            let og = style::galley(ui, &out_text, 10.0 * sc, Weight::W600, oc, 0.0);
            painter.galley(
                Pos2::new(q.x - og.size().x / 2.0, q.y + (HUB_PX + 21.0) * sc - og.size().y * 0.8),
                og,
                oc,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn frame_block(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        frame: &Frame,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        painter.hline(
            egui::Rangef::new(x, x + w),
            y + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        let mut cy = y + 1.0 + 8.0;
        let head = style::galley(ui, "Frame size", 11.5, Weight::W600, style::text_2(), 0.0);
        let hw = head.size().x;
        painter.galley(Pos2::new(x, cy + (16.0 - head.size().y) / 2.0), head, style::text_2());
        if frame.adjustable && !frame.hint.is_empty() {
            crate::ui::popover::info(
                ui,
                Pos2::new(x + hw + 6.0 + 8.0, cy + 8.0),
                "Frame size",
                &frame.hint,
                "framehint",
            );
        }
        cy += 16.0 + 6.0;
        if !frame.adjustable {
            let g = kit::wrapped_weight(ui, &frame.note, 10.5, Weight::W400, style::text_3(), w, 1.4);
            painter.galley(Pos2::new(x, cy), g.clone(), style::text_3());
            return cy + g.size().y;
        }
        let mut dx = x;
        let field_h: f32 = 30.0;
        let row_top = cy;
        let mut row_bottom = cy;
        for (key, label, value) in &frame.dims {
            let lg = style::galley(ui, &label.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
            let unit_w = style::galley(ui, "m", 11.0, Weight::W400, style::text_3(), 0.0)
                .size()
                .x;
            let block_w = lg.size().x.max(76.0 + 5.0 + unit_w);
            if dx > x && dx + block_w > x + w {
                dx = x;
                row_bottom += 8.0;
            }
            let top = if dx == x && row_bottom > row_top {
                row_bottom
            } else {
                row_top
            };
            painter.galley(Pos2::new(dx, top), lg.clone(), style::text_3());
            let r = Rect::from_min_size(Pos2::new(dx, top + lg.size().y + 5.0), Vec2::new(76.0, field_h));
            let mut text = self
                .frame_text
                .get(*key)
                .cloned()
                .unwrap_or_else(|| schema_form::format_number(*value));
            if armed {
                {
                    card::paint(&painter, r, card::Look::Raised);
                }
                painter.galley(
                    Pos2::new(r.left() + 8.0, r.center().y - 8.0),
                    style::galley_mono(ui, &text, 12.0, style::text_3()),
                    style::text_3(),
                );
            } else {
                let resp = kit::text_field_padded(ui, r, &mut text, "", true, ("frame", *key), 9.0);
                if resp.changed() {
                    self.frame_text.insert((*key).to_string(), text.clone());
                    self.frame_status = ("", String::new());
                }
                if resp.lost_focus() && self.frame_text.contains_key(*key) {
                    self.resize_frame(ctx, frame, key, &text);
                }
            }
            painter.galley(
                Pos2::new(r.right() + 5.0, r.center().y - 7.5),
                style::galley(ui, "m", 11.0, Weight::W400, style::text_3(), 0.0),
                style::text_3(),
            );
            row_bottom = row_bottom.max(r.bottom());
            dx += block_w + 12.0;
        }
        cy = row_bottom;
        if !self.frame_status.1.is_empty() {
            let color = match self.frame_status.0 {
                "ok" => style::healthy(),
                "err" => style::critical(),
                _ => style::nav(),
            };
            cy += 6.0
                + pagekit::text(
                    ui,
                    Pos2::new(x, cy + 6.0),
                    &self.frame_status.1.clone(),
                    10.5,
                    Weight::W400,
                    color,
                );
        }
        cy
    }

    fn resize_frame(&mut self, ctx: &egui::Context, frame: &Frame, key: &str, text: &str) {
        let v = text.trim().parse::<f64>().ok().filter(|v| v.is_finite());
        let Some(value) = v.filter(|v| (FRAME_MIN_M..=FRAME_MAX_M).contains(v)) else {
            self.frame_status = ("err", format!("between {FRAME_MIN_M} and {FRAME_MAX_M} m"));
            return;
        };
        let writes = frame_writes(frame, key, value);
        self.frame_text.remove(key);
        if writes.is_empty() {
            self.frame_status = ("ok", "unchanged".into());
            return;
        }
        // all of them are wanted before the first is sent: a write lost
        // halfway leaves a distorted frame, and Check values writes the rest
        for (name, value) in &writes {
            self.form.wanted.insert(name.clone(), *value);
        }
        self.frame_status = ("pending", format!("moving {} motors", frame.motors.len()));
        let s = self.server.clone();
        self.frame_req = Some(Pending::spawn(ctx, move || {
            for (name, value) in writes {
                let d = http::post_json(&s, "/api/params/set", &json!({ "name": name, "value": value }))?;
                if d.get("ok") != Some(&Value::Bool(true)) {
                    return Err(d
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("write failed")
                        .to_string());
                }
            }
            Ok(())
        }));
    }

    #[allow(clippy::too_many_arguments)]
    fn sensor_block(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        painter.hline(
            egui::Rangef::new(x, x + w),
            y + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        let mut cy = y + 1.0 + 8.0;
        let head = style::galley(
            ui,
            "Flight controller and GPS",
            11.5,
            Weight::W600,
            style::text_2(),
            0.0,
        );
        let hw = head.size().x;
        painter.galley(Pos2::new(x, cy + (16.0 - head.size().y) / 2.0), head, style::text_2());
        if !doc.position_hint.is_empty() {
            crate::ui::popover::info(
                ui,
                Pos2::new(x + hw + 6.0 + 8.0, cy + 8.0),
                "Flight controller and GPS",
                &doc.position_hint,
                "sensorhint",
            );
        }
        cy += 16.0 + 6.0;
        let mut cols: Vec<(String, String)> = Vec::new();
        for s in &doc.sensors {
            for f in &s.fields {
                if !cols.iter().any(|c| c.0 == f.label) {
                    cols.push((f.label.clone(), f.unit.clone()));
                }
            }
        }
        // `.motors-sensor-name`: 12 px medium, the hint icon after it, 14 px
        // of room to its right
        let name_w = doc
            .sensors
            .iter()
            .map(|s| {
                style::galley(ui, &s.label, 12.0, Weight::W500, style::text_1(), 0.0)
                    .size()
                    .x
                    + if s.hint.is_empty() { 0.0 } else { 6.0 + 16.0 }
                    + 14.0
            })
            .fold(0.0, f32::max);
        let col_w = 76.0;
        let table_w = name_w + col_w * cols.len() as f32;
        let mut hx = x + name_w;
        for (label, unit) in &cols {
            let text = if unit.is_empty() {
                crate::i18n::tr(label).to_uppercase()
            } else {
                format!("{} {}", crate::i18n::tr(label).to_uppercase(), unit)
            };
            let g = style::galley(ui, &text, 10.0, Weight::W600, style::text_3(), 0.7);
            ui.painter_at(Rect::from_min_size(Pos2::new(hx, cy), Vec2::new(col_w, 16.0)))
                .galley(Pos2::new(hx + 4.0, cy), g, style::text_3());
            hx += col_w;
        }
        cy += 18.5;
        painter.hline(
            egui::Rangef::new(x, x + table_w.min(w)),
            cy - 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        let field_h: f32 = 28.0;
        let server = self.server.clone();
        let last = doc.sensors.len().saturating_sub(1);
        for (si, s) in doc.sensors.iter().enumerate() {
            let row_h = 4.0 + field_h + 4.0 + 1.0;
            let g = style::galley(ui, &s.label, 12.0, Weight::W500, style::text_1(), 0.0);
            let mid = cy + 4.0 + field_h / 2.0;
            painter.galley(Pos2::new(x, mid - g.size().y / 2.0), g.clone(), style::text_1());
            if !s.hint.is_empty() {
                crate::ui::popover::info(
                    ui,
                    Pos2::new(x + g.size().x + 6.0 + 8.0, mid),
                    &s.label,
                    &s.hint,
                    ("sensorrowhint", &s.label),
                );
            }
            let mut cx = x + name_w;
            for (label, _) in &cols {
                if let Some(f) = s.fields.iter().find(|f| &f.label == label) {
                    self.form.control(
                        ui,
                        ctx,
                        &server,
                        f,
                        Pos2::new(cx + 4.0, cy + 4.0),
                        col_w - 8.0,
                        field_h,
                        armed,
                        true,
                    );
                } else {
                    let d = style::galley(ui, "\u{2014}", 12.0, Weight::W400, style::text_3(), 0.0);
                    painter.galley(
                        Pos2::new(cx + col_w / 2.0 - d.size().x / 2.0, mid - d.size().y / 2.0),
                        d,
                        style::text_3(),
                    );
                }
                cx += col_w;
            }
            cy += row_h;
            if si < last {
                painter.hline(
                    egui::Rangef::new(x, x + table_w.min(w)),
                    cy - 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
            }
        }
        cy
    }

    #[allow(clippy::too_many_arguments)]
    fn table_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = painter.add(Shape::Noop);
        let (ix, iw) = (x + 19.0, w - 38.0);
        // the head: the title and the count actions, a rule under it
        let title = pagekit::section_title(ui, ix, y + 19.0, iw, "Motors");
        let count = doc.motors.len();
        if !doc.fixed_count {
            let add = Button::new("Add motor", Variant::Secondary)
                .icon("plus")
                .small()
                .enabled(!armed && count < doc.max_motors && self.count_req.is_none());
            let rem = Button::new("Remove last", Variant::Ghost)
                .icon("minus")
                .small()
                .enabled(!armed && count > 1 && self.count_req.is_none());
            let (aw, rw, bh) = (add.width(ui), rem.width(ui), add.height());
            let rr = Rect::from_min_size(Pos2::new(ix + iw - rw, y + 19.0 - 4.0), Vec2::new(rw, bh));
            let ar = Rect::from_min_size(Pos2::new(rr.left() - 8.0 - aw, rr.top()), Vec2::new(aw, bh));
            let note = style::galley(
                ui,
                &format!("{count} of {}", doc.max_motors),
                11.0,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            painter.galley(
                Pos2::new(ar.left() - 8.0 - note.size().x, rr.center().y - note.size().y / 2.0),
                note.clone(),
                style::text_3(),
            );
            if add.show(ui, ar, "motoradd") {
                self.form.wanted.insert("CA_ROTOR_COUNT".into(), (count + 1) as f64);
                self.count_req = Some(Self::post(
                    ctx,
                    &self.server,
                    "/api/params/set",
                    json!({ "name": "CA_ROTOR_COUNT", "value": count + 1 }),
                ));
            }
            if rem.show(ui, rr, "motorrem") {
                self.form.wanted.insert("CA_ROTOR_COUNT".into(), (count - 1) as f64);
                self.count_req = Some(Self::post(
                    ctx,
                    &self.server,
                    "/api/params/set",
                    json!({ "name": "CA_ROTOR_COUNT", "value": count - 1 }),
                ));
            }
        }
        let mut cy = title - 2.0;
        let cols = columns(&doc.motors);
        // table-layout: auto, as the browser sizes it: a column is as wide as
        // its header or its control, plus the 4 px padding each side
        let header_w = |ui: &egui::Ui, text: &str| {
            style::galley(ui, text, 10.0, Weight::W600, style::text_3(), 0.7)
                .size()
                .x
        };
        let chip_w = (header_w(ui, "MOTOR") + 8.0).max(36.0);
        let out_w = 124.0;
        let mut col_ws: Vec<f32> = cols
            .iter()
            .map(|(pin, label, unit)| {
                let text = if unit.is_empty() {
                    crate::i18n::tr(label).to_uppercase()
                } else {
                    format!("{} {}", crate::i18n::tr(label).to_uppercase(), unit)
                };
                let field = doc.motors.iter().find_map(|m| {
                    let list = if *pin { &m.output_fields } else { &m.fields };
                    list.iter().find(|f| &f.label == label)
                });
                let control = field.map_or(0.0, |f| FormState::compact_width(ui, f));
                (header_w(ui, &text) + 8.0).max(control + 8.0)
            })
            .collect();
        let pins = cols.iter().filter(|c| c.0).count();
        if pins > 0 {
            // the group header spans the pin columns, and stretches the last
            let group = header_w(ui, "OUTPUT PIN LIMITS") + 8.0;
            let have: f32 = col_ws[cols.len() - pins..].iter().sum();
            if group > have {
                if let Some(last) = col_ws.last_mut() {
                    *last += group - have;
                }
            }
        }
        let total_w = chip_w + out_w + col_ws.iter().sum::<f32>();
        let field_h: f32 = 28.0;
        let header_h = 19.0 + if pins > 0 { 15.5 } else { 0.0 };
        let row_h = 4.0 + field_h + 4.0 + 1.0;
        let body_h = header_h + doc.motors.len() as f32 * row_h;
        let area = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, body_h));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        child.set_clip_rect(area);
        let server = self.server.clone();
        egui::ScrollArea::horizontal()
            .id_salt("motorstable")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let painter = ui.painter().clone();
                let top = area.top();
                let mut hy = top;
                if pins > 0 {
                    let px = ix + chip_w + out_w + col_ws[..cols.len() - pins].iter().sum::<f32>();
                    let pin_w: f32 = col_ws[cols.len() - pins..].iter().sum();
                    let g = style::galley(ui, "OUTPUT PIN LIMITS", 10.0, Weight::W600, style::text_3(), 0.7);
                    painter.galley(Pos2::new(px + (pin_w - g.size().x) / 2.0, hy), g, style::text_3());
                    painter.hline(
                        egui::Rangef::new(px, px + pin_w),
                        hy + 15.0,
                        Stroke::new(1.0, style::border()),
                    );
                    hy += 15.5;
                }
                let mut hx = ix;
                for (label, w) in [("MOTOR", chip_w), ("OUTPUT", out_w)] {
                    let g = style::galley(ui, label, 10.0, Weight::W600, style::text_3(), 0.7);
                    painter.galley(Pos2::new(hx + 4.0, hy), g, style::text_3());
                    hx += w;
                }
                for ((_, label, unit), cw) in cols.iter().zip(&col_ws) {
                    let text = if unit.is_empty() {
                        crate::i18n::tr(label).to_uppercase()
                    } else {
                        format!("{} {}", crate::i18n::tr(label).to_uppercase(), unit)
                    };
                    let g = style::galley(ui, &text, 10.0, Weight::W600, style::text_3(), 0.7);
                    painter.galley(Pos2::new(hx + 4.0, hy), g, style::text_3());
                    hx += cw;
                }
                hy += 19.0;
                painter.hline(
                    egui::Rangef::new(ix, ix + total_w),
                    hy - 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                let last_motor = doc.motors.len().saturating_sub(1);
                for (mi, m) in doc.motors.iter().enumerate() {
                    let row = Rect::from_min_size(Pos2::new(ix, hy), Vec2::new(total_w, row_h));
                    let rr = ui.interact(row, ui.scope_id().with(("mrow", m.number)), egui::Sense::click());
                    let selected = self.selected == m.number;
                    let testing = self.testing == m.number;
                    if selected {
                        painter.rect_filled(row, 0, style::accent_dim());
                    } else if rr.hovered() {
                        painter.rect_filled(row, 0, style::surface_2());
                    }
                    if rr.clicked() {
                        self.selected = m.number;
                    }
                    // the chip
                    let cc = Pos2::new(ix + 4.0 + 14.0, hy + 4.0 + field_h / 2.0);
                    let (chip_fill, chip_stroke) = if testing {
                        (style::critical_dark(), style::critical())
                    } else if selected {
                        (style::accent_dim(), style::accent_bright())
                    } else if m.output.is_none() {
                        (style::surface_2(), style::warning())
                    } else {
                        (style::surface_2(), style::border())
                    };
                    painter.circle(cc, 13.0, chip_fill, Stroke::new(2.0, chip_stroke));
                    let ng = style::galley(ui, &m.number.to_string(), 12.5, Weight::W700, style::text_1(), 0.0);
                    painter.galley(cc - ng.size() / 2.0, ng, style::text_1());
                    // the output
                    let held = m.output.as_ref().map_or(String::new(), |o| format!("{}:{}", o.0, o.1));
                    let sel = Rect::from_min_size(Pos2::new(ix + chip_w + 4.0, hy + 4.0), Vec2::new(116.0, 27.0));
                    let choices = output_options(&doc.outputs, m.number);
                    if armed {
                        let label = choices
                            .iter()
                            .find(|c| c.value == held)
                            .map_or(String::new(), |c| c.label.clone());
                        {
                            let r = sel;
                            card::paint(&painter, r, card::Look::Raised);
                        }
                        let g = style::galley(ui, &label, 12.0, Weight::W400, style::text_3(), 0.0);
                        ui.painter_at(sel.shrink2(Vec2::new(7.0, 0.0))).galley(
                            Pos2::new(sel.left() + 7.0, sel.center().y - g.size().y / 2.0),
                            g,
                            style::text_3(),
                        );
                    } else if let Some(v) = kit::select_padded(
                        ui,
                        sel,
                        &choices,
                        &held,
                        "Unassigned",
                        false,
                        &format!("Output for {}", m.label),
                        ("out", m.number),
                        (7.0, 24.0),
                    ) {
                        if v != held && self.assign.is_none() {
                            let body = match parse_output(&v) {
                                Some((bank, pin)) => json!({ "motor": m.number, "bank": bank, "pin": pin }),
                                None => json!({ "motor": m.number, "output": Value::Null }),
                            };
                            self.assign_status.insert(m.number, ("pending", "assigning".into()));
                            self.assign = Some((m.number, Self::post(ctx, &server, "/api/motors/assign", body)));
                        }
                    }
                    if let Some((cls, t)) = self.assign_status.get(&m.number) {
                        let c = match *cls {
                            "ok" => style::healthy(),
                            "err" => style::critical(),
                            _ => style::nav(),
                        };
                        let g = style::galley(ui, t, 10.5, Weight::W400, c, 0.0);
                        painter.galley(Pos2::new(sel.left(), sel.bottom() + 2.0), g, c);
                    }
                    // the field cells
                    let mut cx = ix + chip_w + out_w;
                    for ((pin, label, _), col_w) in cols.iter().zip(col_ws.iter().copied()) {
                        let list = if *pin {
                            if m.output.is_some() {
                                &m.output_fields
                            } else {
                                &Vec::new()
                            }
                        } else {
                            &m.fields
                        };
                        match list.iter().find(|f| &f.label == label) {
                            Some(f) => {
                                self.form.control(
                                    ui,
                                    ctx,
                                    &server,
                                    f,
                                    Pos2::new(cx + 4.0, hy + 4.0),
                                    col_w - 8.0,
                                    field_h,
                                    armed,
                                    true,
                                );
                                if let Some((cls, t)) = self.form.status_of(&f.param) {
                                    let c = match cls {
                                        "ok" => style::healthy(),
                                        "err" => style::critical(),
                                        _ => style::nav(),
                                    };
                                    let g = style::galley(ui, &t, 10.5, Weight::W400, c, 0.0);
                                    ui.painter_at(Rect::from_min_size(
                                        Pos2::new(cx, hy + field_h + 6.0),
                                        Vec2::new(col_w, 14.0),
                                    ))
                                    .galley(
                                        Pos2::new(cx + 4.0, hy + field_h + 6.0),
                                        g,
                                        c,
                                    );
                                }
                            }
                            None => {
                                let d = style::galley(ui, "\u{2014}", 12.0, Weight::W400, style::text_3(), 0.0);
                                painter.galley(
                                    Pos2::new(
                                        cx + col_w / 2.0 - d.size().x / 2.0,
                                        hy + (field_h + 8.0 - d.size().y) / 2.0,
                                    ),
                                    d,
                                    style::text_3(),
                                );
                            }
                        }
                        cx += col_w;
                    }
                    hy += row_h;
                    if mi < last_motor {
                        painter.hline(
                            egui::Rangef::new(ix, ix + total_w),
                            hy - 0.5,
                            Stroke::new(1.0, style::border_soft()),
                        );
                    }
                }
                ui.allocate_space(Vec2::new(total_w, body_h));
            });
        cy += body_h;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    #[allow(clippy::too_many_arguments)]
    fn test_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
        now: f64,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = painter.add(Shape::Noop);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Motor test") - 2.0;
        // the danger strip
        let t = style::galley(
            ui,
            "Remove all propellers first.",
            12.0,
            Weight::W700,
            style::critical(),
            0.0,
        );
        let d = kit::wrapped_weight(
            ui,
            "A propeller on a motor that starts unexpectedly will cut. Test on a bench, with the aircraft held down.",
            11.0,
            Weight::W400,
            style::text_2(),
            iw - 20.0 - t.size().x - 8.0,
            1.45,
        );
        let strip_h = 14.0 + t.size().y.max(d.size().y);
        let strip = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, strip_h));
        {
            let r = strip;
            card::paint(&painter, r, card::Look::Tone(Tone::Err));
        }
        painter.galley(
            Pos2::new(strip.left() + 10.0, strip.top() + 7.0),
            t.clone(),
            style::critical(),
        );
        painter.galley(
            Pos2::new(strip.left() + 10.0 + t.size().x + 8.0, strip.top() + 7.0),
            d,
            style::text_2(),
        );
        cy = strip.bottom() + 10.0;
        // propellers removed: a switch
        let sw = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(36.0, 20.0));
        let resp = ui
            .interact(sw, ui.scope_id().with("props"), egui::Sense::click())
            .on_hover_text("Propellers are removed");
        if resp.clicked() && !armed {
            self.props_off = !self.props_off;
        }
        let on = self.props_off;
        card::paint(
            &painter,
            sw,
            card::Look::custom(
                if on { style::accent() } else { style::surface_3() },
                style::border(),
                10,
            ),
        );
        painter.circle_filled(
            Pos2::new(if on { sw.right() - 10.0 } else { sw.left() + 10.0 }, sw.center().y),
            8.0,
            if on { style::text_inverted() } else { style::surface_1() },
        );
        let lg = style::galley(ui, "Propellers are removed", 12.0, Weight::W400, style::text_1(), 0.0);
        painter.galley(
            Pos2::new(sw.right() + 12.0, sw.center().y - lg.size().y / 2.0),
            lg.clone(),
            style::text_1(),
        );
        // throttle: a stepped slider
        let tx = sw.right() + 12.0 + lg.size().x + 24.0;
        let tlabel = style::galley(ui, "Throttle", 12.0, Weight::W400, style::text_1(), 0.0);
        painter.galley(
            Pos2::new(tx, sw.center().y - tlabel.size().y / 2.0),
            tlabel.clone(),
            style::text_1(),
        );
        let track = egui::Rangef::new(
            tx + tlabel.size().x + 14.0,
            (ix + iw - 52.0).max(tx + tlabel.size().x + 150.0),
        );
        let line = Rect::from_x_y_ranges(track, egui::Rangef::new(sw.center().y - 2.0, sw.center().y + 2.0));
        painter.rect_filled(line, 2, style::surface_3());
        let steps = &THROTTLE_STEPS;
        let pos = |i: usize| track.min + (track.max - track.min) * i as f32 / (steps.len() - 1) as f32;
        let hit = Rect::from_x_y_ranges(
            egui::Rangef::new(track.min - 8.0, track.max + 8.0),
            egui::Rangef::new(sw.center().y - 12.0, sw.center().y + 12.0),
        );
        let sresp = ui.interact(hit, ui.scope_id().with("throttle"), egui::Sense::click_and_drag());
        if let Some(p) = sresp
            .interact_pointer_pos()
            .filter(|_| sresp.dragged() || sresp.clicked())
        {
            let i = (0..steps.len())
                .min_by(|a, b| (pos(*a) - p.x).abs().total_cmp(&(pos(*b) - p.x).abs()))
                .unwrap_or(0);
            self.throttle = steps[i];
        }
        let cur = steps.iter().position(|s| *s == self.throttle).unwrap_or(2);
        for i in 0..steps.len() {
            painter.circle_filled(Pos2::new(pos(i), sw.center().y), 2.0, style::text_3());
        }
        painter.circle(
            Pos2::new(pos(cur), sw.center().y),
            8.0,
            style::accent(),
            Stroke::new(2.0, style::text_1()),
        );
        let vg = style::galley(
            ui,
            &format!("{}%", self.throttle),
            12.0,
            Weight::W600,
            style::healthy(),
            0.0,
        );
        painter.galley(
            Pos2::new(track.max + 14.0, sw.center().y - vg.size().y / 2.0),
            vg,
            style::healthy(),
        );
        cy = sw.bottom() + 10.0;
        // Spin and Stop
        let motor = doc.motors.iter().find(|m| m.number == self.selected);
        let can = !armed && self.props_off && motor.is_some() && self.testing == 0 && self.test_req.is_none();
        let label = motor.map_or("Spin motor".to_string(), |m| format!("Spin {}", m.label));
        let spin = Button::new(&label, Variant::Primary).icon("play").small().enabled(can);
        let stop = Button::new("Stop all", Variant::Secondary).icon("octagon-x").small();
        let (sw_, stw, bh) = (spin.width(ui), stop.width(ui), spin.height());
        if spin.show(ui, Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(sw_, bh)), "spin") {
            if let Some(m) = motor {
                self.testing = m.number;
                self.test_until = 0.0;
                self.test_status = (
                    "pending",
                    format!("{} spinning at {}% for {}s", m.label, self.throttle, TEST_DURATION_S),
                );
                self.test_req = Some((
                    m.number,
                    Self::post(
                        ctx,
                        &self.server,
                        "/api/motors/test",
                        json!({ "motor": m.number, "throttle": self.throttle, "duration": TEST_DURATION_S }),
                    ),
                ));
            }
        }
        if stop.show(
            ui,
            Rect::from_min_size(Pos2::new(ix + sw_ + 8.0, cy), Vec2::new(stw, bh)),
            "stopall",
        ) {
            self.test_status = ("pending", "stopping".into());
            self.stop_req = Some(Self::post(ctx, &self.server, "/api/motors/test/stop", json!({})));
        }
        if !self.test_status.1.is_empty() {
            let c = match self.test_status.0 {
                "ok" => style::healthy(),
                "err" => style::critical(),
                _ => style::nav(),
            };
            let g = style::galley(ui, &self.test_status.1, 10.5, Weight::W400, c, 0.0);
            painter.galley(
                Pos2::new(ix + sw_ + 8.0 + stw + 8.0, cy + (bh - g.size().y) / 2.0),
                g,
                c,
            );
        }
        let _ = now;
        cy += bh;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    #[allow(clippy::too_many_arguments)]
    fn section_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        s: &Section,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = painter.add(Shape::Noop);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, &s.title) - 2.0;
        if !s.hint.is_empty() {
            let g = kit::wrapped_weight(ui, &s.hint, 10.5, Weight::W400, style::text_3(), iw, 1.4);
            painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
            cy += g.size().y + 10.0;
        }
        let server = self.server.clone();
        cy = self
            .form
            .show_with(ui, ctx, &server, &s.fields, ix, cy, iw, armed, Look::MOTORS);
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }
}

impl Drop for MotorsPage {
    fn drop(&mut self) {
        self.stop_on_leave();
    }
}

fn motor_aria(m: &Motor) -> String {
    let mut parts = vec![m.label.clone()];
    parts.push(
        m.output
            .as_ref()
            .map_or("not assigned".to_string(), |o| format!("on {}", o.2)),
    );
    if m.x.abs() > 1e-6 || m.y.abs() > 1e-6 {
        parts.push(format!(
            "{} metres forward, {} metres right",
            schema_form::format_number(f64::from(m.x)),
            schema_form::format_number(f64::from(m.y))
        ));
    }
    if !m.spin.is_empty() {
        parts.push(
            if m.spin == "CW" {
                "clockwise"
            } else {
                "counter-clockwise"
            }
            .into(),
        );
    }
    if m.thrust == "horizontal" {
        parts.push("thrusts forward".into());
    }
    parts.join(", ")
}

/// A filled arrow head: `tip`, pointing along `deg` (screen angles).
fn triangle(tip: Pos2, deg: f32, size: f32) -> Vec<Pos2> {
    let rad = deg.to_radians();
    let back = tip - Vec2::new(rad.cos(), rad.sin()) * size * 2.0;
    let n = Vec2::new(-rad.sin(), rad.cos()) * size;
    vec![tip, back + n, back - n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_reads_back_every_field_of_the_page_once() {
        let doc = parse_doc(&json!({
            "connected": true,
            "motors": [
                {"number": 1, "fields": [{"param": "CA_ROTOR0_PX", "kind": "number"}],
                 "output_fields": [{"param": "PWM_MAIN_DIS1", "kind": "number"}]},
                {"number": 2, "fields": [{"param": "CA_ROTOR0_PX", "kind": "number"}]}
            ],
            "geometry": [{"param": "CA_AIRFRAME", "kind": "enum", "options": []}]
        }));
        assert_eq!(
            MotorsPage::check_names(&doc),
            ["CA_ROTOR0_PX", "PWM_MAIN_DIS1", "CA_AIRFRAME"]
        );
    }

    fn motor(number: i64, x: f32, y: f32) -> Motor {
        Motor {
            number,
            label: format!("Motor {number}"),
            x,
            y,
            spin: String::new(),
            thrust: String::new(),
            axis: (0.0, 0.0),
            output: None,
            fields: vec![],
            output_fields: vec![],
        }
    }

    #[test]
    fn motors_sit_where_their_positions_put_them() {
        let m = vec![motor(1, 0.1, 0.1), motor(2, -0.1, -0.1)];
        let span = motor_span(&m);
        assert!((span - 0.1414).abs() < 1e-3);
        let (x, y) = motor_point(&m[0], 0, 2, span);
        assert!((x - (CENTER + 0.1 / span * ARM_PX)).abs() < 1e-3, "right is y");
        assert!((y - (CENTER - 0.1 / span * ARM_PX)).abs() < 1e-3, "forward is up");
        // without positions the motors are spread round the ring, the first on top
        let (x0, y0) = motor_point(&motor(1, 0.0, 0.0), 0, 4, 0.0);
        assert!((x0 - CENTER).abs() < 1e-3 && (y0 - (CENTER - ARM_PX)).abs() < 1e-3);
        let (x1, _) = motor_point(&motor(2, 0.0, 0.0), 1, 4, 0.0);
        assert!((x1 - (CENTER + ARM_PX)).abs() < 1e-3);
    }

    #[test]
    fn sensors_keep_only_a_direction_without_a_measured_frame() {
        let s = Sensor {
            kind: "fc".into(),
            label: "FC".into(),
            tag: "FC".into(),
            hint: String::new(),
            x: 0.02,
            y: 0.0,
            fields: vec![],
        };
        let (x, y) = sensor_point(&s, 0.0, std::slice::from_ref(&s));
        assert!((x - CENTER).abs() < 1e-3 && (y - (CENTER - ARM_PX * SENSOR_REACH)).abs() < 1e-3);
        let none = Sensor { x: 0.0, ..s.clone() };
        assert_eq!(sensor_point(&none, 0.0, std::slice::from_ref(&none)), (CENTER, CENTER));
    }

    fn frame() -> Frame {
        Frame {
            adjustable: true,
            dims: vec![
                ("diagonal", "Diagonal", 0.4),
                ("length", "Front to back", 0.2),
                ("width", "Left to right", 0.2),
            ],
            center: (0.0, 0.0),
            motors: vec![
                FrameMotor {
                    x: 0.1,
                    y: 0.1,
                    x_param: "CA_ROTOR0_PX".into(),
                    y_param: "CA_ROTOR0_PY".into(),
                },
                FrameMotor {
                    x: -0.1,
                    y: -0.1,
                    x_param: "CA_ROTOR1_PX".into(),
                    y_param: "CA_ROTOR1_PY".into(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_frame_resize_moves_every_motor_about_the_centre() {
        let w = frame_writes(&frame(), "diagonal", 0.8);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0], ("CA_ROTOR0_PX".to_string(), 0.2));
        assert_eq!(w[3], ("CA_ROTOR1_PY".to_string(), -0.2));
        let length = frame_writes(&frame(), "length", 0.4);
        assert_eq!(
            length.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["CA_ROTOR0_PX", "CA_ROTOR1_PX"],
            "a length leaves the widths alone"
        );
        assert!(
            frame_writes(&frame(), "diagonal", 0.4).is_empty(),
            "the same size writes nothing"
        );
        let mut fixed = frame();
        fixed.adjustable = false;
        assert!(frame_writes(&fixed, "diagonal", 0.8).is_empty());
    }

    #[test]
    fn the_output_list_names_what_a_busy_pin_drives() {
        let outs = vec![
            Output {
                bank: "MAIN".into(),
                pin: 1,
                label: "Main 1".into(),
                function: "Motor 1".into(),
                motor: Some(1),
            },
            Output {
                bank: "MAIN".into(),
                pin: 2,
                label: "Main 2".into(),
                function: "Servo 1".into(),
                motor: None,
            },
            Output {
                bank: "MAIN".into(),
                pin: 3,
                label: "Main 3".into(),
                function: "Disabled".into(),
                motor: None,
            },
        ];
        let c = output_options(&outs, 1);
        assert_eq!(c[0].label, "Unassigned");
        assert_eq!(c[1].label, "Main 1", "its own pin is not marked busy");
        assert_eq!(c[2].label, "Main 2 (Servo 1)");
        assert_eq!(c[3].label, "Main 3");
        assert_eq!(parse_output("MAIN:3"), Some(("MAIN".into(), 3)));
        assert_eq!(parse_output("AUX:x"), None);
        assert_eq!(parse_output(""), None);
        assert_eq!(parse_output("MAIN:0"), None);
    }

    #[test]
    fn the_description_is_read() {
        let v = json!({
            "connected": true, "received": 30, "airframe_family": "vtol", "max_motors": 8,
            "motors": [{"number": 1, "label": "Motor 1", "x": 0.2, "y": 0.1, "spin": "CW", "thrust": "horizontal",
                         "axis": {"x": 1.0, "y": 0.0}, "output": {"bank": "MAIN", "pin": 1, "label": "Main 1"},
                         "fields": [{"param": "CA_ROTOR0_KM", "label": "Spin", "kind": "sign", "value": 0.05, "options": []}],
                         "output_fields": [{"param": "PWM_MAIN_MIN1", "label": "Minimum", "kind": "number", "value": 1000}]},
                        {"number": 2, "label": "Motor 2", "output": null}],
            "frame": {"adjustable": true, "diagonal": 0.5, "center": {"x": 0, "y": 0},
                       "motors": [{"x": 0.2, "y": 0.1, "x_param": "A", "y_param": "B"}]},
            "sections": [{"id": "protocol", "title": "Protocol", "fields": []}]
        });
        let d = parse_doc(&v);
        assert!(d.connected);
        assert_eq!((d.family.as_str(), d.max_motors, d.motors.len()), ("vtol", 8, 2));
        assert_eq!(d.motors[0].output, Some(("MAIN".into(), 1, "Main 1".into())));
        assert!(d.motors[1].output.is_none());
        assert_eq!(d.motors[0].axis, (1.0, 0.0));
        assert_eq!(d.frame.unwrap().dims, vec![("diagonal", "Diagonal", 0.5)]);
        assert_eq!(parse_doc(&json!({})).family, "multirotor");
    }

    #[test]
    fn the_table_has_each_column_once_motor_fields_before_pin_limits() {
        let f = |label: &str, unit: &str| Field {
            param: "P".into(),
            label: label.into(),
            kind: schema_form::Kind::Number,
            value: 0.0,
            unit: unit.into(),
            hint: String::new(),
            min: None,
            max: None,
            also: vec![],
        };
        let mut a = motor(1, 0.0, 0.0);
        a.fields = vec![f("X", "m"), f("Y", "m")];
        a.output_fields = vec![f("Minimum", "us")];
        let mut b = motor(2, 0.0, 0.0);
        b.fields = vec![f("X", "m"), f("Spin", "")];
        b.output_fields = vec![f("Minimum", "us"), f("Maximum", "us")];
        let cols = columns(&[a, b]);
        let names: Vec<(bool, &str)> = cols.iter().map(|c| (c.0, c.1.as_str())).collect();
        assert_eq!(
            names,
            vec![
                (false, "X"),
                (false, "Y"),
                (false, "Spin"),
                (true, "Minimum"),
                (true, "Maximum")
            ]
        );
    }
}
