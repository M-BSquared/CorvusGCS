//! The Calibration page of Setup (python/src/js/setup-calibration.js, the
//! `.calib-*` rules of setup.css).
//!
//! Two views behind one entry: the list of the seven calibrations as cards,
//! and a wizard for one of them. The wizard draws the aircraft in the attitude
//! the autopilot is asking for (`calib_figure.rs`) and follows the autopilot's
//! own STATUSTEXT (`calib_protocol.rs`), because "rotate to a pending side:
//! back" is not an instruction an operator can act on with the aircraft in
//! both hands. Above the list sits the flight controller's rotation on the
//! airframe, since every accelerometer and compass calibration is measured
//! through it.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::net::stream::Stream;
use crate::setup::calib_figure::{Figure, View};
use crate::setup::calib_protocol::{self as protocol, Phase, Pose, Procedure, Session, Side};
use crate::setup::params_dialogs;
use crate::setup::schema_form::{parse_fields, Field, FormState, Look};
use crate::ui::card;
use crate::ui::chip::Chip;
use crate::ui::icons;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::message::Tone;
use crate::ui::modal;
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

/// No autopilot word within this long of a successful start means something
/// is wrong with the link, not with the operator.
pub const START_TIMEOUT_MS: f64 = 8000.0;
/// The autopilot going quiet mid calibration usually means it is waiting for
/// a position the operator has not realised it wants.
pub const STALL_TIMEOUT_MS: f64 = 45000.0;
const TRANSCRIPT_MAX: usize = 60;
const TOP_H: f32 = 60.0;

/// The cue as the icon beside the instruction (`CUE_ICON`).
pub fn cue_icon(cue: &str, fallback: &'static str) -> &'static str {
    match cue {
        "wait" => "hourglass",
        "rotate" => "rotate-3d",
        "hold" => "hand",
        "spin" => "refresh-cw",
        "warn" => "triangle-alert",
        "battery_on" => "plug-zap",
        "battery_off" => "unplug",
        "blow" => "wind",
        "shield" => "shield",
        "done" => "circle-check",
        "failed" => "circle-x",
        "cancelled" => "ban",
        _ => fallback,
    }
}

/// What the stage's watchdog says, if anything: the autopilot never answered,
/// or it has gone quiet. A pause the autopilot asked for is not a stall.
pub fn watchdog(phase: Phase, seen: bool, confirm: bool, quiet_ms: f64) -> Option<String> {
    if !matches!(phase, Phase::Starting | Phase::Running) {
        return None;
    }
    if !seen && quiet_ms > START_TIMEOUT_MS {
        Some("No reply from the autopilot yet. Check the link, or abort.".into())
    } else if confirm {
        None
    } else if seen && quiet_ms > STALL_TIMEOUT_MS {
        Some(format!(
            "Autopilot silent for {} s. It may want the next position.",
            crate::data::units::js_number(crate::data::units::js_round(quiet_ms / 1000.0))
        ))
    } else {
        None
    }
}

/// How many pose chips fit a row: six at 760 px and over, else
/// `auto-fit, minmax(96px, 1fr)`.
pub fn pose_columns(window_w: f32, w: f32) -> usize {
    if window_w >= 760.0 {
        6
    } else {
        (((w + 8.0) / (96.0 + 8.0)).floor() as usize).max(1)
    }
}

/// The wizard's figure sizes from the window's height (`--calib-fig-h`,
/// `--calib-pose-h`): what is left of the window once the rest is laid out.
pub fn figure_heights(window_h: f32, has_poses: bool, gate: bool) -> (f32, f32) {
    let pose_h = (window_h * 0.1).clamp(48.0, 88.0);
    let strip = if has_poses { pose_h + 50.0 } else { 0.0 };
    let gate = if gate { 76.0 } else { 0.0 };
    let fig_h = (window_h - TOP_H - 310.0 - strip - gate).clamp(150.0, 300.0);
    (fig_h, pose_h)
}

/// The stage's figure column: `minmax(200px, min(400px, fig_h * 240 / 178 + 14))`.
pub fn figure_column(fig_h: f32) -> f32 {
    (fig_h * 240.0 / 178.0 + 14.0).clamp(200.0, 400.0)
}

fn post_ok(ctx: &egui::Context, server: &Server, path: &'static str, body: Value) -> Pending<Result<(), String>> {
    let s = server.clone();
    Pending::spawn(ctx, move || {
        let d = http::post_json(&s, path, &body)?;
        if d.get("ok") == Some(&Value::Bool(false)) {
            Err(d
                .get("error")
                .and_then(Value::as_str)
                .filter(|e| !e.is_empty())
                .unwrap_or("The vehicle rejected the command.")
                .to_string())
        } else {
            Ok(())
        }
    })
}

/// The flight controller's rotation, from `GET /api/mounting`.
#[derive(Default)]
struct Mounting {
    loading: Option<Pending<Result<Value, String>>>,
    loaded: bool,
    connected: bool,
    hint: String,
    fields: Vec<Field>,
    reboot_params: Vec<String>,
    form: FormState,
    show_reboot: bool,
}

/// What a button of the wizard asked the vehicle for, and what it was for.
enum Req {
    Start(Pending<Result<(), String>>),
    Confirm(protocol::Confirm, Pending<Result<(), String>>),
    Abort(Pending<Result<(), String>>),
}

struct Wizard {
    session: Session,
    figure: Figure,
    poses: Vec<(Pose, Figure)>,
    preview: Option<Pose>,
    req: Option<Req>,
    log: Vec<(String, String)>,
    log_count: usize,
    log_open: bool,
    log_placeholder: &'static str,
    inbox: Arc<Mutex<Vec<Value>>>,
    _stream: Stream,
    modal: bool,
    shown_failed: bool,
}

impl Wizard {
    fn new(ctx: &egui::Context, server: &Server, proc: &'static Procedure) -> Wizard {
        let inbox: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = inbox.clone();
        let repaint = ctx.clone();
        let stream = Stream::start(server.clone(), "/api/events?topics=console".into(), move |ev| {
            if ev.name != "console" {
                return;
            }
            let Ok(entry) = serde_json::from_str::<Value>(&ev.data) else {
                return;
            };
            if let Ok(mut q) = sink.lock() {
                q.push(entry);
            }
            repaint.request_repaint();
        });
        Wizard {
            session: Session::new(proc),
            figure: Figure::new(
                View {
                    compact: false,
                    spin: false,
                    marker: proc.marker,
                },
                proc.start_pose,
            ),
            poses: proc
                .poses
                .iter()
                .map(|p| {
                    (
                        *p,
                        Figure::new(
                            View {
                                compact: true,
                                spin: false,
                                marker: None,
                            },
                            *p,
                        ),
                    )
                })
                .collect(),
            preview: None,
            req: None,
            log: Vec::new(),
            log_count: 0,
            log_open: false,
            log_placeholder: "Nothing from the autopilot yet.",
            inbox,
            _stream: stream,
            modal: false,
            shown_failed: false,
        }
    }

    fn append(&mut self, text: &str, level: &str) {
        self.log.push((
            if level.is_empty() { "info" } else { level }.to_string(),
            text.to_string(),
        ));
        self.log_count += 1;
        if self.log.len() > TRANSCRIPT_MAX {
            let extra = self.log.len() - TRANSCRIPT_MAX;
            self.log.drain(..extra);
        }
    }

    fn clear_log(&mut self, placeholder: &'static str) {
        self.log.clear();
        self.log_count = 0;
        self.log_placeholder = placeholder;
    }
}

enum Sub {
    List,
    Wizard(Box<Wizard>),
}

/// The page's state.
pub struct CalibrationPage {
    server: Server,
    sub: Sub,
    mount: Mounting,
    supported: Option<Vec<String>>,
    caps: Option<Pending<Result<Value, String>>>,
    last_stack: String,
    last_connected: Option<bool>,
    errors: Vec<String>,
    notes: Vec<String>,
    warnings: Vec<String>,
    /// The reboot confirmation is open, and the reboot on its way.
    reboot_modal: bool,
    reboot: Option<Pending<Result<(), String>>>,
}

impl CalibrationPage {
    pub fn new(server: Server, ctx: &egui::Context) -> CalibrationPage {
        let mut p = CalibrationPage {
            server,
            sub: Sub::List,
            mount: Mounting::default(),
            supported: None,
            caps: None,
            last_stack: String::new(),
            last_connected: None,
            errors: Vec::new(),
            notes: Vec::new(),
            warnings: Vec::new(),
            reboot_modal: false,
            reboot: None,
        };
        p.load_mounting(ctx);
        p.load_caps(ctx);
        p
    }

    pub fn in_wizard(&self) -> bool {
        matches!(self.sub, Sub::Wizard(_))
    }

    /// Back to the list (the wizard's own back button).
    pub fn close_wizard(&mut self) {
        self.sub = Sub::List;
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        e.extend(self.mount.form.take_errors());
        e
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    fn load_mounting(&mut self, ctx: &egui::Context) {
        if self.mount.loading.is_some() {
            return;
        }
        let s = self.server.clone();
        self.mount.loading = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/mounting")));
    }

    /// A different aircraft is a different feature set, and the capabilities
    /// are cached per stack, so a change of stack is what asks again.
    fn load_caps(&mut self, ctx: &egui::Context) {
        if self.caps.is_some() {
            return;
        }
        let s = self.server.clone();
        self.caps = Some(Pending::spawn(ctx, move || {
            http::get_json(&s, "/api/mavlink/capabilities")
        }));
    }

    fn take_answers(&mut self, ctx: &egui::Context, state: &Value) {
        if let Some(r) = self.mount.loading.as_ref().and_then(Pending::take) {
            self.mount.loading = None;
            self.mount.loaded = true;
            let doc = r.unwrap_or(Value::Null);
            self.mount.connected = doc.get("connected").is_some_and(|c| c == &Value::Bool(true));
            let o = doc.get("orientation").cloned().unwrap_or(Value::Null);
            self.mount.hint = o.get("hint").and_then(Value::as_str).unwrap_or("").to_string();
            let raw = o.get("fields").cloned().unwrap_or(Value::Null);
            self.mount.fields = parse_fields(&raw);
            self.mount.reboot_params = raw
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|f| f.get("reboot") == Some(&Value::Bool(true)))
                        .filter_map(|f| f.get("param").and_then(Value::as_str).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
        }
        if let Some(r) = self.caps.as_ref().and_then(Pending::take) {
            self.caps = None;
            self.supported = r.ok().and_then(|d| {
                let list: Vec<String> = d
                    .get("calibrations")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                (!list.is_empty()).then_some(list)
            });
        }
        self.mount.form.take_answers(&mut self.mount.fields);
        if std::mem::take(&mut self.mount.form.written)
            && self
                .mount
                .fields
                .iter()
                .any(|f| self.mount.reboot_params.contains(&f.param))
        {
            self.mount.show_reboot = true;
        }
        let stack = state
            .get("autopilot_stack")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let connected = state.get("connected") == Some(&Value::Bool(true));
        if stack != self.last_stack {
            self.last_stack = stack;
            self.load_caps(ctx);
            self.load_mounting(ctx);
        } else if self.last_connected.is_some_and(|c| c != connected) {
            self.load_mounting(ctx);
        }
        self.last_connected = Some(connected);
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx, state);
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let connected = state.get("connected") == Some(&Value::Bool(true));
        let bottom = if matches!(self.sub, Sub::Wizard(_)) {
            self.wizard(ui, &ctx, x, y, w, armed, connected, now)
        } else {
            self.list(ui, &ctx, x, y, w, armed, connected)
        };
        self.reboot_dialog(&ctx);
        bottom
    }

    fn is_supported(&self, kind: &str) -> bool {
        self.supported.as_ref().is_none_or(|l| l.iter().any(|k| k == kind))
    }

    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
        connected: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        // the readiness strip, in the title row where other pages keep actions
        let link_text = if connected { "Link up" } else { "No link" };
        let arm_text = if armed { "Armed" } else { "Disarmed" };
        let chips = [(link_text, connected), (arm_text, !armed)];
        let looks: Vec<Chip> = chips.iter().map(|(t, ok)| ready_chip(t, *ok)).collect();
        let sizes: Vec<Vec2> = looks.iter().map(|c| c.measure(ui)).collect();
        let chips_w: f32 = sizes.iter().map(|s| s.x).sum::<f32>() + 8.0 * (sizes.len() as f32 - 1.0);
        let wrap = w - chips_w - 24.0 < 280.0;
        let head_bottom = pagekit::header_wrapped(
            ui,
            x,
            y,
            if wrap { w } else { w - chips_w - 24.0 },
            "Calibration",
            "Guided sensor calibration",
        );
        let ch = sizes[0].y;
        let (mut cx, cy) = if wrap {
            (x, head_bottom + 12.0)
        } else {
            (x + w - chips_w, head_bottom - ch)
        };
        for c in &looks {
            let r = c.paint(ui, Pos2::new(cx, cy));
            cx = r.right() + 8.0;
        }
        let mut y = (if wrap { cy + ch } else { head_bottom }) + 22.0;

        // the flight controller's rotation
        y = pagekit::section_title(ui, x, y, w, "Flight controller orientation");
        y = self.mounting_card(ui, ctx, x, y, w, armed) + 14.0;

        // the calibrations
        y = pagekit::section_title(ui, x, y, w, "Sensor Calibration");
        let cols = (((w + 10.0) / (320.0 + 10.0)).floor() as usize).max(1);
        let cw = (w - 10.0 * (cols as f32 - 1.0)) / cols as f32;
        let ch = 83.6;
        let mut opened = None;
        for (i, p) in protocol::PROCEDURES.iter().enumerate() {
            let (col, row) = ((i % cols) as f32, (i / cols) as f32);
            let r = Rect::from_min_size(
                Pos2::new(x + col * (cw + 10.0), y + row * (ch + 10.0)),
                Vec2::new(cw, ch),
            );
            let supported = self.is_supported(p.kind);
            if calib_card(ui, r, p, armed || !supported, supported) {
                opened = Some(p);
            }
        }
        let rows = protocol::PROCEDURES.len().div_ceil(cols) as f32;
        y += rows * ch + (rows - 1.0) * 10.0;
        if armed {
            y = pagekit::warning_banner(ui, x, y + 14.0, w, "Cannot calibrate while armed. Disarm first.") + 14.0;
        }
        let _ = painter;
        if let Some(p) = opened {
            self.sub = Sub::Wizard(Box::new(Wizard::new(ctx, &self.server, p)));
        }
        y
    }

    #[allow(clippy::too_many_arguments)]
    fn mounting_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, armed: bool) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = y + 19.0;
        let note = |ui: &egui::Ui, cy: f32, text: &str| -> f32 {
            let g = kit::wrapped_weight(ui, text, 10.5, Weight::W400, style::text_3(), iw, 1.4);
            ui.painter().galley(Pos2::new(ix, cy), g.clone(), style::text_3());
            g.size().y
        };
        if self.mount.fields.is_empty() {
            let text = if !self.mount.loaded {
                ""
            } else if self.mount.connected {
                "The connected firmware reports no board rotation."
            } else {
                "Connect to a vehicle to read how the flight controller is mounted."
            };
            cy += note(ui, cy, text);
        } else {
            if !self.mount.hint.is_empty() {
                cy += note(ui, cy, &self.mount.hint) + 10.0;
            }
            let server = self.server.clone();
            let fields = self.mount.fields.clone();
            cy = self
                .mount
                .form
                .show_with(ui, ctx, &server, &fields, ix, cy, iw, armed, Look::CALIB)
                + 10.0;
            if self.mount.show_reboot {
                let b = Button::new("Reboot autopilot", Variant::Secondary)
                    .icon("power")
                    .small()
                    .enabled(!armed);
                let (bw, bh) = (b.width(ui), b.height());
                let text = "Reboot for the new rotation to take effect, then calibrate the accelerometer, the compass and the level horizon again.";
                let tw = (iw - bw - 10.0).max(120.0);
                let g = kit::wrapped_weight(ui, text, 10.5, Weight::W400, style::text_3(), tw, 1.4);
                let h = g.size().y.max(bh);
                painter.galley(Pos2::new(ix, cy + (h - g.size().y) / 2.0), g.clone(), style::text_3());
                if b.show(
                    ui,
                    Rect::from_min_size(
                        Pos2::new(ix + g.size().x + 10.0, cy + (h - bh) / 2.0),
                        Vec2::new(bw, bh),
                    ),
                    "calibmountreboot",
                ) {
                    self.reboot_modal = true;
                }
                cy += h + 10.0;
            }
            cy += note(
                ui,
                cy,
                "Where the flight controller and the GPS sit is set on the Motors page, on the drawing of the airframe. The estimator uses those positions in flight; no calibration reads them.",
            );
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    /// The reboot confirmation, and the reboot it asks for.
    fn reboot_dialog(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.reboot.as_ref().and_then(Pending::take) {
            self.reboot = None;
            match r {
                Ok(()) => self
                    .notes
                    .push("The autopilot is rebooting. The link returns in a few seconds.".into()),
                Err(e) => self.errors.push(format!(
                    "Reboot refused: {}",
                    if e.is_empty() { "no answer" } else { &e }
                )),
            }
        }
        if !self.reboot_modal {
            return;
        }
        let (closed, go) = params_dialogs::reboot(ctx);
        if closed {
            self.reboot_modal = false;
        }
        if go {
            self.mount.show_reboot = false;
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
    }

    #[allow(clippy::too_many_arguments)]
    fn wizard(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
        connected: bool,
        now: f64,
    ) -> f32 {
        let server = self.server.clone();
        let time = ui.input(|i| i.time);
        let now_ms = time * 1000.0;
        let Sub::Wizard(wz) = &mut self.sub else {
            return y;
        };
        let proc = wz.session.procedure;
        let _ = now;

        // what the autopilot said
        let entries: Vec<Value> = wz.inbox.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for e in entries {
            let name = e.get("name").and_then(Value::as_str).unwrap_or("");
            if name != "STATUSTEXT" && name != "CALIBRATE" {
                continue;
            }
            let body = e.get("text").and_then(Value::as_str).unwrap_or("");
            let was_terminal = wz.session.state().phase.terminal();
            let changed = wz.session.ingest(body, now_ms);
            let level = e.get("level").and_then(Value::as_str).unwrap_or("info");
            if changed || body.to_lowercase().contains("[cal]") {
                wz.append(body, level);
            }
            if changed && !was_terminal && wz.session.state().phase.terminal() {
                let ph = wz.session.state().phase;
                let msg = format!(
                    "{} calibration {}",
                    proc.label,
                    if ph == Phase::Done { "complete" } else { ph.key() }
                );
                if ph == Phase::Done {
                    self.notes.push(msg);
                } else {
                    self.warnings.push(msg);
                }
            }
        }
        // what the vehicle answered to a button
        let Sub::Wizard(wz) = &mut self.sub else {
            return y;
        };
        if let Some(req) = wz.req.take() {
            let done = match &req {
                Req::Start(p) | Req::Abort(p) => p.take(),
                Req::Confirm(_, p) => p.take(),
            };
            match (req, done) {
                (Req::Start(_), Some(r)) => {
                    match r {
                        Ok(()) => {
                            // restarts the start timer only if the autopilot is still silent
                            wz.session.begin(now_ms);
                        }
                        Err(message) => {
                            let phase = wz.session.state().phase;
                            if phase == Phase::Running {
                                wz.append(&message, "warning");
                            } else if !phase.terminal() {
                                wz.session
                                    .finish(Phase::Failed, "Could not start the calibration", &message);
                                self.errors.push(if message.is_empty() {
                                    "Calibration could not be started".into()
                                } else {
                                    message
                                });
                            }
                        }
                    }
                }
                (Req::Confirm(c, _), Some(r)) => match r {
                    Ok(()) => {
                        wz.session.confirm_placement();
                        wz.append(&format!("Confirmed: {}", c.pose.label()), "info");
                    }
                    Err(message) => {
                        let m = if message.is_empty() {
                            "The vehicle did not take the position".to_string()
                        } else {
                            message
                        };
                        wz.append(&m, "critical");
                        self.warnings.push(m);
                    }
                },
                (Req::Abort(_), Some(r)) => match r {
                    Ok(()) => wz.session.finish(
                        Phase::Cancelled,
                        "Calibration cancelled",
                        "The vehicle stopped the calibration.",
                    ),
                    Err(message) => {
                        let m = if message.is_empty() {
                            "The vehicle did not confirm the abort.".to_string()
                        } else {
                            message
                        };
                        wz.session.finish(Phase::Failed, "Abort failed", &m);
                        self.errors.push(if m.is_empty() {
                            "Could not abort the calibration".into()
                        } else {
                            m
                        });
                    }
                },
                (req, None) => wz.req = Some(req),
            }
        }
        let Sub::Wizard(wz) = &mut self.sub else {
            return y;
        };
        let st = wz.session.state().clone();
        let running = matches!(st.phase, Phase::Starting | Phase::Running);
        let terminal = st.phase.terminal();
        let busy = wz.req.is_some();
        let blocked = armed || !connected;
        let window_h = ctx.content_rect().height();
        let window_w = ctx.content_rect().width();
        let (fig_h, pose_h) = figure_heights(window_h, !proc.poses.is_empty(), blocked && !running);

        // the title row: Start or Abort in the same slot, bottom aligned
        let start_label = format!("Start {} calibration", proc.label.to_lowercase());
        let slot: Option<Button<'_>> = if running {
            Some(
                Button::new("Abort calibration", Variant::Danger)
                    .icon("octagon-x")
                    .enabled(!busy),
            )
        } else if !terminal {
            Some(
                Button::new(
                    &start_label,
                    if proc.danger { Variant::Danger } else { Variant::Primary },
                )
                .icon("play")
                .enabled(!busy && !blocked),
            )
        } else {
            None
        };
        let slot_size = slot.as_ref().map(|b| (b.width(ui), b.height()));
        let head_w = slot_size.map_or(w, |s| (w - s.0 - 20.0).max(200.0));
        let title = format!("{} Calibration", proc.label);
        let head_bottom = pagekit::header_wrapped(ui, x, y, head_w, &title, proc.summary);
        let mut pressed = false;
        if let (Some(b), Some((bw, bh))) = (slot, slot_size) {
            pressed = b.show(
                ui,
                Rect::from_min_size(Pos2::new(x + w - bw, head_bottom - bh), Vec2::new(bw, bh)),
                "calibslot",
            );
        }
        let mut y = head_bottom + 28.0;
        if blocked && !running {
            let text = if armed {
                "Cannot calibrate while armed. Disarm first."
            } else {
                "No link to the vehicle. Connect before calibrating."
            };
            y = pagekit::warning_banner(ui, x, y - 14.0, w, text) + 14.0;
        }

        // the stage
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 21.0, w - 42.0);
        let fig_col = figure_column(fig_h).min((iw - 24.0 - 240.0).max(200.0));
        let text_x = ix + fig_col + 24.0;
        let text_w = iw - fig_col - 24.0;
        let fig_panel_w = fig_col;
        let svg_w = fig_panel_w - 14.0;
        let (vw, vh) = wz.figure.view.size();
        let k = (svg_w / vw as f32).min(fig_h / vh as f32);
        let panel_h = vh as f32 * k + 14.0;

        // text column
        let phase_text = match st.phase {
            Phase::Idle => "Ready",
            Phase::Starting => "Starting",
            Phase::Running => "In progress",
            Phase::Done => "Complete",
            Phase::Failed => "Failed",
            Phase::Cancelled => "Cancelled",
        };
        let (pfg, pbg, pbd) = match st.phase {
            Phase::Starting | Phase::Running => (style::nav(), style::nav_dark(), style::nav()),
            Phase::Done => (style::healthy(), style::healthy_dark(), style::healthy()),
            Phase::Failed => (style::critical_bright(), style::critical_dark(), style::critical()),
            Phase::Cancelled => (style::warning(), style::warning_dark(), style::warning()),
            Phase::Idle => (style::text_3(), style::surface_2(), style::border_soft()),
        };
        let phase_chip = Chip {
            size: 9.5,
            weight: Weight::W700,
            letter: 0.7,
            pad: (3.0, 9.0, 3.0, 9.0),
            fg: pfg,
            bg: pbg,
            border: pbd,
            ..Chip::new(phase_text.to_uppercase())
        };
        let phase_size = phase_chip.measure(ui);
        let words_w = text_w - 52.0 - 14.0;
        let headline = kit::wrapped_weight(ui, &st.headline, 22.0, Weight::W600, style::text_1(), words_w, 1.2);
        let detail = (!st.detail.is_empty())
            .then(|| kit::wrapped_weight(ui, &st.detail, 13.5, Weight::W400, style::text_2(), words_w, 1.4));
        let words_h = headline.size().y + detail.as_ref().map_or(0.0, |d| 4.0 + d.size().y);
        let instr_h = words_h.max(52.0);
        let mut text_h = phase_size.y + 8.0 + instr_h;
        if st.progress.is_some() {
            text_h += 8.0 + 4.0 + 5.0;
        }
        let quiet = now_ms - st.last_event_at;
        let dog = watchdog(st.phase, st.seen_vehicle_message, st.confirm.is_some(), quiet);
        let dog_g = dog
            .as_ref()
            .map(|t| kit::wrapped_weight(ui, t, 11.5, Weight::W400, style::warning(), text_w - 22.0, 1.5));
        if let Some(g) = &dog_g {
            text_h += 8.0 + g.size().y + 18.0;
        }
        // follow ups under the instruction
        let mut buttons: Vec<(&'static str, Button<'_>)> = Vec::new();
        if running && st.confirm.is_some() {
            buttons.push((
                "confirm",
                Button::new("In position, continue", Variant::Primary)
                    .icon("check")
                    .enabled(!busy),
            ));
        }
        if matches!(st.phase, Phase::Failed | Phase::Cancelled) {
            buttons.push((
                "retry",
                Button::new("Try again", Variant::Secondary)
                    .icon("rotate-cw")
                    .enabled(!busy && !blocked),
            ));
        }
        if st.phase == Phase::Done && proc.reboot {
            buttons.push((
                "reboot",
                Button::new("Reboot autopilot", Variant::Secondary)
                    .icon("power")
                    .enabled(!busy && !blocked),
            ));
        }
        if terminal {
            buttons.push((
                "done",
                Button::new("Back to calibrations", Variant::Secondary).icon("chevron-left"),
            ));
        }
        let btn_sizes: Vec<(f32, f32)> = buttons.iter().map(|b| (b.1.width(ui), b.1.height())).collect();
        let mut btn_rows: Vec<Vec<usize>> = Vec::new();
        {
            let mut row_w = 0.0;
            for (i, (bw, _)) in btn_sizes.iter().enumerate() {
                if btn_rows.is_empty() || (row_w + 8.0 + bw > text_w && row_w > 0.0) {
                    btn_rows.push(Vec::new());
                    row_w = 0.0;
                }
                row_w += if row_w > 0.0 { 8.0 } else { 0.0 } + bw;
                btn_rows.last_mut().expect("a row").push(i);
            }
        }
        let btn_h = btn_sizes.first().map_or(0.0, |b| b.1);
        let actions_h = if btn_rows.is_empty() {
            0.0
        } else {
            btn_rows.len() as f32 * btn_h + (btn_rows.len() as f32 - 1.0) * 8.0
        };
        if actions_h > 0.0 {
            text_h += 8.0 + 4.0 + actions_h;
        }
        // the preparation checklist, while idle
        let mut prep_chips: Vec<Chip> = Vec::new();
        let prep_title = "Before you start";
        let mut prep_rows: Vec<Vec<usize>> = Vec::new();
        if st.phase == Phase::Idle {
            let (ibg, ibd, ic) = if proc.danger {
                (style::critical_dark(), style::critical(), style::critical())
            } else {
                (style::surface_2(), style::border_soft(), style::nav())
            };
            for (icon, text) in proc.prep {
                prep_chips.push(Chip {
                    size: 12.5,
                    weight: Weight::W500,
                    pad: (6.0, 12.0, 6.0, 9.0),
                    gap: 8.0,
                    fg: style::text_1(),
                    bg: ibg,
                    border: ibd,
                    icon: Some((icon, 16.0, ic)),
                    ..Chip::new(*text)
                });
            }
            let sizes: Vec<Vec2> = prep_chips.iter().map(|c| c.measure(ui)).collect();
            let mut row_w = 0.0;
            for (i, s) in sizes.iter().enumerate() {
                if prep_rows.is_empty() || (row_w + 8.0 + s.x > text_w && row_w > 0.0) {
                    prep_rows.push(Vec::new());
                    row_w = 0.0;
                }
                row_w += if row_w > 0.0 { 8.0 } else { 0.0 } + s.x;
                prep_rows.last_mut().expect("a row").push(i);
            }
            let row_h = sizes.first().map_or(0.0, |s| s.y);
            let prep_h =
                1.0 + 14.0 + 13.0 + 10.0 + prep_rows.len() as f32 * row_h + (prep_rows.len() as f32 - 1.0) * 8.0;
            text_h += 8.0 + 8.0 + prep_h;
        }
        let inner_h = panel_h.max(text_h);
        let stage = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, inner_h + 38.0));
        // the figure panel
        let panel = Rect::from_min_size(
            Pos2::new(ix, y + 19.0 + (inner_h - panel_h) / 2.0),
            Vec2::new(fig_panel_w, panel_h),
        );
        card::paint(
            &painter,
            panel,
            card::Look::custom(style::surface_2(), style::border_soft(), style::RADIUS),
        );
        wz.figure.set(
            wz.preview.unwrap_or(st.pose),
            st.spin,
            st.marker,
            Some(st.from),
            st.cue == "rotate",
        );
        if wz.preview.is_some() && st.phase != Phase::Idle {
            wz.preview = None;
        }
        let fig_rect = Rect::from_center_size(panel.center(), Vec2::new(svg_w, panel_h - 14.0));
        let moving = wz.figure.paint(ui, fig_rect, time);
        let mut anim = moving;

        // text column, centred on the panel
        let mut ty = y + 19.0 + (inner_h - text_h) / 2.0;
        phase_chip.paint(ui, Pos2::new(text_x, ty));
        ty += phase_size.y + 8.0;
        let cue_color = match st.cue.as_str() {
            "wait" | "rotate" | "spin" => (style::nav(), style::nav_dark(), style::nav()),
            "hold" | "warn" | "battery_on" | "battery_off" | "blow" | "shield" | "cancelled" => {
                (style::warning(), style::warning_dark(), style::warning())
            }
            "done" => (style::healthy(), style::healthy_dark(), style::healthy()),
            "failed" => (style::critical_bright(), style::critical_dark(), style::critical()),
            _ => (style::accent_bright(), style::surface_2(), style::border_soft()),
        };
        let cue = Rect::from_min_size(Pos2::new(text_x, ty + (instr_h - 52.0) / 2.0), Vec2::splat(52.0));
        painter.circle_filled(cue.center(), 26.0, cue_color.1);
        painter.circle_stroke(cue.center(), 25.25, Stroke::new(1.5, cue_color.2));
        let icon_rect = Rect::from_center_size(cue.center(), Vec2::splat(28.0));
        let icon = cue_icon(&st.cue, proc.icon);
        if st.cue == "spin" {
            // `calibCueSpin`: a turn every 2.4 s
            let angle = (time / 2.4).fract() as f32 * std::f32::consts::TAU;
            icons::paint_rotated(ui, icon, icon_rect, cue_color.0, angle);
            anim = true;
        } else if st.cue == "wait" {
            // `calibMarkerPulse`: 1.6 s, 1 to 0.35 and back
            let phase = (time / 1.6).fract() as f32 * std::f32::consts::TAU;
            icons::paint(
                ui,
                icon,
                icon_rect,
                cue_color.0.gamma_multiply(0.675 + 0.325 * phase.cos()),
            );
            anim = true;
        } else {
            icons::paint(ui, icon, icon_rect, cue_color.0);
        }
        let wx = text_x + 52.0 + 14.0;
        let wy = ty + (instr_h - words_h) / 2.0;
        let hh = headline.size().y;
        painter.galley(Pos2::new(wx, wy), headline, style::text_1());
        if let Some(d) = detail {
            painter.galley(Pos2::new(wx, wy + hh + 4.0), d, style::text_2());
        }
        ty += instr_h;
        if let Some(p) = st.progress {
            ty += 8.0 + 4.0;
            let bar = Rect::from_min_size(Pos2::new(text_x, ty), Vec2::new(text_w, 5.0));
            painter.rect_filled(bar, 3, style::surface_3());
            let fw = text_w * (p.clamp(0.0, 100.0) as f32) / 100.0;
            if fw > 0.0 {
                painter.rect_filled(
                    Rect::from_min_size(bar.min, Vec2::new(fw, 5.0)),
                    3,
                    if st.phase == Phase::Done {
                        style::healthy()
                    } else {
                        style::nav()
                    },
                );
            }
            ty += 5.0;
        }
        if let Some(g) = dog_g {
            ty += 8.0;
            let r = Rect::from_min_size(Pos2::new(text_x, ty), Vec2::new(text_w, g.size().y + 18.0));
            card::paint(&painter, r, card::Look::Tone(Tone::Warn));
            painter.galley(r.min + Vec2::new(11.0, 9.0), g, style::warning());
            ty = r.bottom();
        }
        let mut hit: Option<&'static str> = None;
        if actions_h > 0.0 {
            ty += 8.0 + 4.0;
            for (ri, row) in btn_rows.iter().enumerate() {
                let mut bx = text_x;
                let by = ty + ri as f32 * (btn_h + 8.0);
                for &i in row {
                    let (bw, bh) = btn_sizes[i];
                    let (name, _) = &buttons[i];
                    let name = *name;
                    let b = std::mem::replace(&mut buttons[i].1, Button::new("", Variant::Ghost));
                    if b.show(
                        ui,
                        Rect::from_min_size(Pos2::new(bx, by), Vec2::new(bw, bh)),
                        ("calibact", name),
                    ) {
                        hit = Some(name);
                    }
                    bx += bw + 8.0;
                }
            }
            ty += actions_h;
        }
        if st.phase == Phase::Idle {
            ty += 8.0 + 8.0;
            let line = if proc.danger {
                style::critical()
            } else {
                style::border_soft()
            };
            painter.hline(
                egui::Rangef::new(text_x, text_x + text_w),
                ty + 0.5,
                Stroke::new(1.0, line),
            );
            let ty0 = ty + 1.0 + 14.0;
            let (icon, tcolor) = if proc.danger {
                ("triangle-alert", style::critical())
            } else {
                ("list-checks", style::nav())
            };
            icons::paint(
                ui,
                icon,
                Rect::from_center_size(Pos2::new(text_x + 6.5, ty0 + 6.5), Vec2::splat(13.0)),
                tcolor,
            );
            let g = style::galley(ui, &prep_title.to_uppercase(), 11.0, Weight::W600, style::text_2(), 0.5);
            painter.galley(
                Pos2::new(text_x + 19.0, ty0 + (13.0 - g.size().y) / 2.0),
                g,
                style::text_2(),
            );
            let mut ry = ty0 + 13.0 + 10.0;
            for row in &prep_rows {
                let mut bx = text_x;
                let mut rh = 0.0f32;
                for &i in row {
                    let r = prep_chips[i].paint(ui, Pos2::new(bx, ry));
                    bx = r.right() + 8.0;
                    rh = rh.max(r.height());
                }
                ry += rh + 8.0;
            }
        }
        card::fill(&painter, bg, stage, card::Look::Page);
        y = stage.bottom() + 12.0;

        // the position strip: every position the autopilot will ask for
        if !wz.poses.is_empty() {
            let cols = pose_columns(window_w, w);
            let cw = (w - 8.0 * (cols as f32 - 1.0)) / cols as f32;
            let cell_h = 2.0 + 8.0 + pose_h + 2.0 + 12.5 + 10.0;
            let mut clicked: Option<Pose> = None;
            for (i, (pose, fig)) in wz.poses.iter_mut().enumerate() {
                let (col, row) = ((i % cols) as f32, (i / cols) as f32);
                let r = Rect::from_min_size(
                    Pos2::new(x + col * (cw + 8.0), y + row * (cell_h + 8.0)),
                    Vec2::new(cw, cell_h),
                );
                let side = st.side(*pose);
                let disabled = running && side == Side::Done;
                let resp = ui.interact(
                    r,
                    ui.scope_id().with(("calibpose", pose.key())),
                    if disabled {
                        egui::Sense::hover()
                    } else {
                        egui::Sense::click()
                    },
                );
                let (bg, bd, fg) = match side {
                    Side::Active => (style::nav_dark(), style::nav(), style::text_1()),
                    Side::Done => (style::surface_1(), style::healthy(), style::text_2()),
                    Side::Failed => (style::surface_1(), style::critical(), style::text_2()),
                    Side::Pending => (
                        style::surface_1(),
                        if resp.hovered() && !disabled {
                            style::border_strong()
                        } else {
                            style::border_soft()
                        },
                        style::text_2(),
                    ),
                };
                card::paint(&painter, r, card::Look::custom(bg, bd, style::RADIUS));
                let fade = if side == Side::Done { 0.45 } else { 1.0 };
                let inner_w = cw - 14.0;
                let fig_area =
                    Rect::from_min_size(Pos2::new(r.left() + 7.0, r.top() + 9.0), Vec2::new(inner_w, pose_h));
                fig.paint(ui, fig_area, time);
                if fade < 1.0 {
                    painter.rect_filled(fig_area, 0, style::surface_1().gamma_multiply(1.0 - fade));
                }
                let g = style::galley(ui, pose.label(), 10.5, Weight::W600, fg.gamma_multiply(fade), 0.2);
                painter.galley(
                    Pos2::new(r.center().x - g.size().x / 2.0, r.top() + 9.0 + pose_h + 2.0),
                    g,
                    fg.gamma_multiply(fade),
                );
                // the state mark
                match side {
                    Side::Done | Side::Failed => {
                        let c = Pos2::new(r.right() - 1.0 - 5.0 - 10.0, r.top() + 1.0 + 5.0 + 10.0);
                        let col = if side == Side::Done {
                            style::healthy()
                        } else {
                            style::critical()
                        };
                        painter.circle_filled(c, 10.0, col);
                        icons::paint(
                            ui,
                            if side == Side::Done { "check" } else { "x" },
                            Rect::from_center_size(c, Vec2::splat(13.0)),
                            Color32::WHITE,
                        );
                    }
                    Side::Active => {
                        let pulse = ((time / 1.4).fract() as f32 * std::f32::consts::TAU).cos() * 0.325 + 0.675;
                        painter.circle_filled(
                            Pos2::new(r.right() - 8.0 - 4.0, r.top() + 7.0 + 4.0),
                            4.0,
                            style::nav().gamma_multiply(pulse),
                        );
                        anim = true;
                    }
                    Side::Pending => {
                        painter.circle_filled(
                            Pos2::new(r.right() - 8.0 - 4.0, r.top() + 7.0 + 4.0),
                            4.0,
                            style::border_strong(),
                        );
                    }
                }
                if resp.clicked() && st.phase == Phase::Idle {
                    clicked = Some(*pose);
                }
            }
            if let Some(p) = clicked {
                wz.preview = Some(p);
            }
            let rows = wz.poses.len().div_ceil(cols) as f32;
            y += rows * cell_h + (rows - 1.0) * 8.0 + 12.0;
        }

        // the autopilot's own words, folded away until something fails
        if st.phase == Phase::Failed && !wz.shown_failed {
            wz.log_open = true;
        }
        wz.shown_failed = st.phase == Phase::Failed;
        let log_bg = card::reserve(&painter);
        let (lx, lw) = (x + 19.0, w - 38.0);
        let mut ly = y + 19.0;
        let summary = Rect::from_min_size(Pos2::new(lx, ly), Vec2::new(lw, 13.0));
        let resp = ui.interact(summary, ui.scope_id().with("calibloghead"), egui::Sense::click());
        icons::paint(
            ui,
            "info",
            Rect::from_center_size(Pos2::new(lx + 6.5, ly + 6.5), Vec2::splat(13.0)),
            style::nav(),
        );
        let g = style::galley(ui, "AUTOPILOT MESSAGES", 11.0, Weight::W600, style::text_2(), 0.5);
        painter.galley(
            Pos2::new(lx + 19.0, ly + (13.0 - g.size().y) / 2.0),
            g.clone(),
            style::text_2(),
        );
        let mut hx = lx + 19.0 + g.size().x + 6.0;
        if wz.log_count > 0 {
            let c = Chip {
                size: 10.0,
                weight: Weight::W700,
                pad: (1.0, 7.0, 1.0, 7.0),
                fg: style::text_3(),
                bg: style::surface_2(),
                border: Color32::TRANSPARENT,
                ..Chip::new(wz.log_count.to_string())
            };
            let r = c.paint(ui, Pos2::new(hx, ly + (13.0 - c.measure(ui).y) / 2.0));
            hx = r.right() + 6.0;
        }
        let ac = Pos2::new(hx + 4.0 + 3.0, ly + 6.5);
        let col = style::text_2();
        let pts = if wz.log_open {
            [
                Pos2::new(ac.x - 3.0, ac.y - 1.5),
                Pos2::new(ac.x, ac.y + 1.5),
                Pos2::new(ac.x + 3.0, ac.y - 1.5),
            ]
        } else {
            [
                Pos2::new(ac.x - 1.5, ac.y - 3.0),
                Pos2::new(ac.x + 1.5, ac.y),
                Pos2::new(ac.x - 1.5, ac.y + 3.0),
            ]
        };
        painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(1.5, col)));
        if resp.clicked() {
            wz.log_open = !wz.log_open;
        }
        ly += 13.0;
        if wz.log_open {
            ly += 8.0;
            let line_h = 11.5 * 1.5;
            let rows_h = if wz.log.is_empty() {
                4.0 * 2.0 + line_h
            } else {
                wz.log.len() as f32 * line_h + (wz.log.len() as f32 - 1.0) * 2.0
            };
            let box_h = (rows_h + 16.0 + 2.0).min(200.0);
            let rect = Rect::from_min_size(Pos2::new(lx, ly), Vec2::new(lw, box_h));
            card::paint(&painter, rect, card::Look::Console);
            let inner = rect.shrink2(Vec2::new(11.0, 9.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            child.set_clip_rect(inner);
            egui::ScrollArea::vertical()
                .id_salt("calib-log")
                .max_height(inner.height())
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(&mut child, |ui| {
                    if wz.log.is_empty() {
                        ui.label(
                            egui::RichText::new(wz.log_placeholder)
                                .font(style::mono(11.5))
                                .color(style::text_3()),
                        );
                    }
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (level, text) in &wz.log {
                        let c = match level.as_str() {
                            "warning" => style::warning(),
                            "critical" | "error" | "emergency" => style::critical(),
                            "info" => style::nav(),
                            "notice" | "success" => style::healthy(),
                            _ => style::text_3(),
                        };
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let (r, _) = ui.allocate_exact_size(Vec2::new(44.0, 11.5 * 1.5), egui::Sense::hover());
                            let g = style::galley(ui, &level.to_uppercase(), 9.0, Weight::W700, c, 0.5);
                            ui.painter()
                                .galley(Pos2::new(r.left(), r.center().y - g.size().y / 2.0), g, c);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(text)
                                        .font(style::mono(11.5))
                                        .color(style::terminal_text()),
                                )
                                .wrap(),
                            );
                        });
                    }
                });
            ly += box_h;
        }
        card::fill(
            &painter,
            log_bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, ly + 19.0)),
            card::Look::Page,
        );
        let bottom = ly + 19.0;
        if anim {
            ctx.request_repaint();
        }

        // the actions
        let modal_open = wz.modal;
        let (mut do_start, mut do_confirm, mut do_abort, mut do_retry, mut do_reboot, mut do_done) =
            (false, false, false, false, false, false);
        let mut reboot_asked = false;
        if pressed {
            if running {
                do_abort = true;
            } else {
                do_start = true;
            }
        }
        match hit {
            Some("confirm") => do_confirm = true,
            Some("retry") => do_retry = true,
            Some("reboot") => do_reboot = true,
            Some("done") => do_done = true,
            _ => {}
        }
        if do_start && !busy {
            if proc.danger && !modal_open {
                wz.modal = true;
            } else {
                Self::launch(wz, ctx, &server, proc, now_ms);
            }
        }
        if modal_open {
            let mut go = false;
            let closed = modal::show(
                ctx,
                "calibmotor",
                modal::Size::Sm,
                "Motor / ESC Calibration",
                |ui, mx, my, mw| {
                    let mut lines = Vec::new();
                    for (_, text) in proc.prep {
                        lines.push(kit::wrapped_weight(
                            ui,
                            text,
                            12.0,
                            Weight::W500,
                            style::text_1(),
                            mw - 28.0 - 28.0,
                            1.5,
                        ));
                    }
                    let h: f32 = lines.iter().map(|g| g.size().y.max(18.0)).sum::<f32>()
                        + 8.0 * (lines.len() as f32 - 1.0)
                        + 24.0;
                    let r = Rect::from_min_size(Pos2::new(mx, my), Vec2::new(mw, h));
                    card::paint(ui.painter(), r, card::Look::Tone(Tone::Err));
                    let mut ly = r.top() + 12.0;
                    for (g, (icon, _)) in lines.into_iter().zip(proc.prep) {
                        let lh = g.size().y.max(18.0);
                        icons::paint(
                            ui,
                            icon,
                            Rect::from_center_size(Pos2::new(r.left() + 15.0 + 9.0, ly + lh / 2.0), Vec2::splat(18.0)),
                            style::critical(),
                        );
                        ui.painter().galley(
                            Pos2::new(r.left() + 14.0 + 28.0, ly + (lh - g.size().y) / 2.0),
                            g,
                            style::text_1(),
                        );
                        ly += lh + 8.0;
                    }
                    let (hit, bottom) = modal::actions(
                        ui,
                        mx,
                        r.bottom() + 18.0,
                        mw,
                        vec![
                            Button::new("Cancel", Variant::Secondary),
                            Button::new("Calibrate Motors", Variant::Danger),
                        ],
                    );
                    go = hit == Some(1);
                    bottom
                },
            );
            if closed || go {
                wz.modal = false;
            }
            if go {
                Self::launch(wz, ctx, &server, proc, now_ms);
            }
        }
        if do_confirm {
            if let Some(c) = wz.session.state().confirm {
                if !busy {
                    wz.req = Some(Req::Confirm(
                        c,
                        post_ok(
                            ctx,
                            &server,
                            "/api/calibrate/position",
                            json!({ "position": c.position }),
                        ),
                    ));
                }
            }
        }
        if do_abort && !busy {
            wz.req = Some(Req::Abort(post_ok(ctx, &server, "/api/calibrate/cancel", json!({}))));
        }
        if do_retry {
            wz.session.reset();
            wz.clear_log("Nothing from the autopilot yet.");
            wz.log_open = false;
        }
        if do_reboot {
            reboot_asked = true;
        }
        if reboot_asked {
            self.reboot_modal = true;
        }
        if do_done {
            self.sub = Sub::List;
        }
        bottom
    }

    fn launch(wz: &mut Wizard, ctx: &egui::Context, server: &Server, proc: &'static Procedure, now_ms: f64) {
        wz.session.reset();
        wz.session.begin(now_ms);
        wz.clear_log("Waiting for the autopilot\u{2026}");
        wz.req = Some(Req::Start(post_ok(
            ctx,
            server,
            "/api/calibrate",
            json!({ "type": proc.kind }),
        )));
    }
}

/// The readiness chip: a status dot and a word, green when ready, red when not.
fn ready_chip(text: &str, ok: bool) -> Chip {
    if ok {
        Chip {
            fg: style::text_1(),
            dot: Some((style::healthy(), true)),
            ..Chip::new(text)
        }
    } else {
        Chip {
            fg: style::critical_bright(),
            bg: style::critical_dark(),
            border: style::critical(),
            dot: Some((style::critical(), true)),
            ..Chip::new(text)
        }
    }
}

/// One calibration of the list; true when pressed.
fn calib_card(ui: &mut egui::Ui, r: Rect, p: &Procedure, disabled: bool, supported: bool) -> bool {
    let resp = ui.interact(
        r,
        ui.scope_id().with(("calibcard", p.kind)),
        if disabled {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        },
    );
    let fade = if disabled { 0.45 } else { 1.0 };
    let hover = resp.hovered() && !disabled;
    let painter = ui.painter().clone();
    let (bg, bd) = if hover {
        (style::surface_2(), style::border_strong())
    } else {
        (style::surface_1(), style::border_soft())
    };
    card::paint(
        &painter,
        r,
        card::Look::custom(bg.gamma_multiply(fade), bd.gamma_multiply(fade), style::RADIUS),
    );
    let icon_box = Rect::from_min_size(Pos2::new(r.left() + 17.0, r.center().y - 19.0), Vec2::splat(38.0));
    let (ibg, ifg) = if p.danger {
        (style::critical_dark(), style::critical_bright())
    } else {
        (style::accent_dark(), style::accent_bright())
    };
    painter.rect_filled(icon_box, style::RADIUS_BTN, ibg.gamma_multiply(fade));
    icons::paint(
        ui,
        p.icon,
        Rect::from_center_size(icon_box.center(), Vec2::splat(18.0)),
        ifg.gamma_multiply(fade),
    );
    let bx = icon_box.right() + 14.0;
    let bw = r.right() - 17.0 - 15.0 - 14.0 - bx;
    let mut ty = r.top() + 15.0;
    let title = style::galley(
        ui,
        p.label,
        13.5,
        Weight::W600,
        style::text_1().gamma_multiply(fade),
        0.0,
    );
    painter.galley(Pos2::new(bx, ty), title.clone(), style::text_1().gamma_multiply(fade));
    if p.danger {
        let flag = Chip {
            size: 9.0,
            weight: Weight::W700,
            letter: 0.6,
            pad: (1.0, 5.0, 1.0, 5.0),
            fg: style::critical_bright().gamma_multiply(fade),
            bg: style::critical_dark().gamma_multiply(fade),
            border: style::critical().gamma_multiply(fade),
            ..Chip::new("PROPS OFF")
        };
        let fs = flag.measure(ui);
        flag.paint(
            ui,
            Pos2::new(bx + title.size().x + 8.0, ty + (title.size().y - fs.y) / 2.0),
        );
    }
    ty += title.size().y + 3.0;
    let desc = kit::wrapped_weight(
        ui,
        p.summary,
        11.5,
        Weight::W400,
        style::text_2().gamma_multiply(fade),
        bw,
        1.4,
    );
    painter.galley(Pos2::new(bx, ty), desc.clone(), style::text_2().gamma_multiply(fade));
    ty += desc.size().y + 3.0;
    if !supported {
        let g = kit::wrapped_weight(
            ui,
            "Not available on the connected autopilot.",
            11.5,
            Weight::W400,
            style::warning().gamma_multiply(fade),
            bw,
            1.4,
        );
        painter.galley(Pos2::new(bx, ty), g.clone(), style::warning().gamma_multiply(fade));
        ty += g.size().y + 3.0;
    }
    ty += 2.0;
    let mut mx = bx;
    let positions = if p.poses.is_empty() {
        "Stays still".to_string()
    } else {
        format!("{} positions", p.poses.len())
    };
    let mut items = vec![positions, p.duration.to_string()];
    if p.reboot {
        items.push("Reboot after".into());
    }
    for t in items {
        let g = style::galley_mono_letter(ui, &t.to_uppercase(), 10.0, style::text_3().gamma_multiply(fade), 0.4);
        painter.galley(Pos2::new(mx, ty), g.clone(), style::text_3().gamma_multiply(fade));
        mx += g.size().x + 14.0;
    }
    icons::paint(
        ui,
        "chevron-right",
        Rect::from_center_size(Pos2::new(r.right() - 17.0 - 7.5, r.center().y), Vec2::splat(15.0)),
        style::text_3().gamma_multiply(fade),
    );
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cue_icons_are_the_web_pages() {
        assert_eq!(cue_icon("wait", "x"), "hourglass");
        assert_eq!(cue_icon("rotate", "x"), "rotate-3d");
        assert_eq!(cue_icon("battery_on", "x"), "plug-zap");
        assert_eq!(cue_icon("cancelled", "x"), "ban");
        assert_eq!(cue_icon("idle", "compass"), "compass");
    }

    #[test]
    fn the_watchdog_blames_the_link_before_the_first_word_and_the_autopilot_after() {
        assert_eq!(watchdog(Phase::Idle, false, false, 99999.0), None);
        assert_eq!(watchdog(Phase::Running, false, false, 7999.0), None);
        assert_eq!(
            watchdog(Phase::Starting, false, false, 8001.0).as_deref(),
            Some("No reply from the autopilot yet. Check the link, or abort.")
        );
        assert_eq!(watchdog(Phase::Running, true, false, 44000.0), None);
        assert_eq!(
            watchdog(Phase::Running, true, false, 46400.0).as_deref(),
            Some("Autopilot silent for 46 s. It may want the next position.")
        );
        assert_eq!(
            watchdog(Phase::Running, true, true, 90000.0),
            None,
            "the pause it asked for is not a stall"
        );
        assert_eq!(watchdog(Phase::Done, true, false, 90000.0), None);
    }

    #[test]
    fn the_strip_and_the_figures_take_the_window_as_the_css_does() {
        // the 1440 by 900 window of the web page: figure 300, poses 88
        assert_eq!(figure_heights(900.0, true, false), (300.0, 88.0));
        // a short window gives the figures up, down to their floors
        assert_eq!(figure_heights(500.0, true, false), (150.0, 50.0));
        assert_eq!(figure_heights(300.0, true, true), (150.0, 48.0));
        // a calibration that never moves has no strip, so the stage grows
        assert_eq!(figure_heights(700.0, false, false).0, 300.0);
        assert_eq!(figure_heights(560.0, true, false).0, 150.0);
        assert_eq!(figure_column(300.0), 400.0);
        assert_eq!(figure_column(150.0), 200.0f32.max(150.0 * 240.0 / 178.0 + 14.0));
        assert_eq!(pose_columns(1440.0, 910.0), 6);
        assert_eq!(pose_columns(700.0, 500.0), 4);
        assert_eq!(pose_columns(700.0, 90.0), 1);
    }
}
