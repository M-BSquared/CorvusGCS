//! The Telemetry Radio page of Setup (python/src/js/setup-sik.js, the `.sik-*` rules
//! of setup.css): the settings of a SiK radio pair, side by side, because the
//! failure the page exists to fix is a pair that disagrees and that is only
//! visible with both ends on screen at once.
//!
//! A radio keeps its own EEPROM and is reached by AT commands down its serial
//! port, which the MAVLink bridge has to give up for the session, so there are
//! no live edits and no per-field apply: Load takes a snapshot, Save writes a
//! batch and reboots the radios into it. Every request here is a POST for that
//! reason, and a session takes seconds, so the requests wait for as long as a
//! reboot does.

use std::collections::HashMap;
use std::time::Duration;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::sik_model::{self as model, Edits, Field, Kind, Loaded, Side};
use crate::ui::card;
use crate::ui::formfield as ff;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::{self, Tone as Msg};
use crate::ui::modal;
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

type Answer = Pending<Result<Value, String>>;

/// What the busy request is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Task {
    Load,
    Save,
    Reset(Side),
}

/// A session reads and writes registers and waits out a reboot, and a remote
/// read goes over the air: far longer than the 10 s of an ordinary request.
const SESSION_TIMEOUT: Duration = Duration::from_secs(150);

/// The page's state.
pub struct SikPage {
    server: Server,
    status: Value,
    status_poll: Option<Answer>,
    status_asked: bool,
    port: String,
    baud: i64,
    remote: bool,
    loaded: Option<Loaded>,
    asked_remote: bool,
    edits: Edits,
    texts: HashMap<(&'static str, String), String>,
    show_advanced: bool,
    busy: Option<(Task, Answer)>,
    message: Option<(Msg, String)>,
    confirm_reset: Option<Side>,
    armed: bool,
    errors: Vec<String>,
}

impl SikPage {
    pub fn new(server: Server, _ctx: &egui::Context) -> SikPage {
        SikPage {
            server,
            status: Value::Null,
            status_poll: None,
            status_asked: false,
            port: String::new(),
            baud: 57600,
            remote: true,
            loaded: None,
            asked_remote: true,
            edits: Edits::default(),
            texts: HashMap::new(),
            show_advanced: false,
            busy: None,
            message: None,
            confirm_reset: None,
            armed: false,
            errors: Vec::new(),
        }
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn gate(&self) -> String {
        model::gate_reason(self.armed, &self.status)
    }

    fn blocked(&self) -> bool {
        model::blocked(&self.gate(), self.busy.is_some(), &self.port)
    }

    fn take_answers(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.status_poll.as_ref().and_then(Pending::take) {
            self.status_poll = None;
            match r {
                Ok(v) => {
                    self.status = v;
                    let ports = model::ports(&self.status);
                    self.port = model::preferred_port(&ports, &self.port);
                    self.baud = model::preferred_baud(&self.status, &self.port);
                }
                Err(_) => self.message = Some((Msg::Err, "Could not read the serial port list.".into())),
            }
        }
        let Some((task, answer)) = self.busy.as_ref() else {
            return;
        };
        let task = *task;
        let Some(r) = answer.take() else {
            return;
        };
        self.busy = None;
        match (task, r) {
            (Task::Load, Ok(v)) => {
                let loaded = Loaded::from_json(&v);
                let (kind, text) = loaded.summary(self.asked_remote);
                self.message = Some((if kind == "warn" { Msg::Warn } else { Msg::Ok }, text));
                self.loaded = Some(loaded);
                self.edits.clear();
                self.texts.clear();
            }
            (Task::Load, Err(e)) => self.fail(e, "Could not read the radio."),
            (Task::Save, Ok(v)) => {
                let (kind, text) = model::save_notice(&v);
                self.message = Some((if kind == "warn" { Msg::Warn } else { Msg::Ok }, text));
                self.edits.clear();
                self.texts.clear();
                self.start_load(ctx);
            }
            (Task::Save, Err(e)) => self.fail(e, "Could not write the radio."),
            (Task::Reset(side), Ok(_)) => {
                self.message = Some((
                    Msg::Ok,
                    format!("{} reset to factory defaults. Re-reading\u{2026}", side.label()),
                ));
                self.start_load(ctx);
            }
            (Task::Reset(_), Err(e)) => self.fail(e, "Could not reset the radio."),
        }
    }

    fn fail(&mut self, e: String, fallback: &str) {
        let e = if e.is_empty() { fallback.to_string() } else { e };
        self.errors.push(e.clone());
        self.message = Some((Msg::Err, e));
    }

    fn spawn(&mut self, ctx: &egui::Context, task: Task, path: &'static str, body: Value) {
        let s = self.server.clone();
        self.busy = Some((task, Pending::spawn(ctx, move || post(&s, path, &body))));
    }

    fn start_load(&mut self, ctx: &egui::Context) {
        if self.busy.is_some() || self.port.is_empty() {
            return;
        }
        self.message = Some((
            Msg::Plain,
            "Entering command mode. This takes a few seconds\u{2026}".into(),
        ));
        self.asked_remote = self.remote;
        let body = model::load_body(&self.port, self.baud, self.remote);
        self.spawn(ctx, Task::Load, "/api/sik/load", body);
    }

    fn start_save(&mut self, ctx: &egui::Context) {
        if self.busy.is_some() || self.loaded.is_none() {
            return;
        }
        let Some(body) = self.edits.save_body(&self.port, self.baud) else {
            self.message = Some((Msg::Plain, "Nothing has been changed.".into()));
            return;
        };
        self.message = Some((Msg::Plain, "Writing settings and rebooting the radios\u{2026}".into()));
        self.spawn(ctx, Task::Save, "/api/sik/save", body);
    }

    fn start_reset(&mut self, ctx: &egui::Context, side: Side) {
        if self.busy.is_some() || self.loaded.is_none() {
            return;
        }
        self.message = Some((
            Msg::Plain,
            format!("Resetting {} to factory defaults\u{2026}", side.label()),
        ));
        let body = model::reset_body(&self.port, self.baud, side);
        self.spawn(ctx, Task::Reset(side), "/api/sik/reset", body);
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value) -> f32 {
        let ctx = ui.ctx().clone();
        self.armed = state.get("armed") == Some(&Value::Bool(true));
        if !self.status_asked {
            self.status_asked = true;
            let s = self.server.clone();
            self.status_poll = Some(Pending::spawn(&ctx, move || http::get_json(&s, "/api/sik/status")));
        }
        self.take_answers(&ctx);
        let y = pagekit::header(
            ui,
            x,
            y,
            "Telemetry Radio",
            "Read and program a SiK radio pair: network ID, air rate, power and band",
        ) + 32.0;
        let narrow = crate::app::responsive::Vw::of(&ctx).max900;
        let mut y = self.port_card(ui, &ctx, x, y, w, narrow) + 28.0;
        if let Some(loaded) = self.loaded.clone() {
            y = self.link_card(ui, x, y, w, &loaded) + 26.0;
            if !loaded.mismatches.is_empty() {
                y = self.mismatch_card(ui, &ctx, x, y, w, &loaded) + 26.0;
            }
            y = desc_box(ui, x, y, w, model::COLUMNS_INTRO) + 14.0;
            y = self.columns(ui, x, y, w, &loaded, narrow) + 26.0;
            y = self.actions_card(ui, &ctx, x, y, w);
        }
        if let Some(side) = self.confirm_reset {
            self.reset_dialog(&ctx, side);
        }
        y
    }

    fn port_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, narrow: bool) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Radio port") + 2.0;
        cy = desc_box(ui, ix, cy, iw, model::PORT_INTRO) + 14.0;
        let gate = self.gate();
        if !gate.is_empty() {
            cy = pagekit::warning_banner(ui, ix, cy, iw, &gate) + 14.0;
        }
        let ports = model::ports(&self.status);
        let read = !self.status.is_null();
        let bauds = model::bauds(&self.status);
        let widths: [f32; 3] = if narrow {
            [iw; 3]
        } else {
            let unit = (iw - 24.0) / 4.0;
            [unit * 2.0, unit, unit]
        };
        let xs = if narrow {
            [ix; 3]
        } else {
            [ix, ix + widths[0] + 12.0, ix + widths[0] + widths[1] + 24.0]
        };
        let mut ys = [cy; 3];
        if narrow {
            ys[1] = cy + 53.5 + 12.0;
            ys[2] = ys[1] + 53.5 + 12.0;
        }
        let mut picked_port = None;
        let mut picked_baud = None;
        let mut picked_scope = None;
        // the port
        let choices: Vec<Choice> = if ports.is_empty() {
            vec![Choice::new(
                "",
                if read {
                    "No serial ports found"
                } else {
                    "Looking for serial ports\u{2026}"
                },
            )]
        } else {
            ports
                .iter()
                .map(|p| Choice::new(p.device.clone(), p.label.clone()))
                .collect()
        };
        let port = self.port.clone();
        ff::field_info(
            ui,
            xs[0],
            ys[0],
            "Serial port",
            "A radio on a USB cable, or the port the live link already runs on.",
            "sikport-info",
            |ui, top| {
                ui.add_enabled_ui(!ports.is_empty(), |ui| {
                    picked_port = ff::select(ui, xs[0], top, widths[0], &choices, &port, "sikport");
                });
                ff::CONTROL_H
            },
        );
        // the baud rate
        let baud_choices: Vec<Choice> = if read {
            bauds
                .iter()
                .map(|b| Choice::new(b.to_string(), format!("{b} baud")))
                .collect()
        } else {
            vec![Choice::new("57600", "57600")]
        };
        let baud = self.baud.to_string();
        ff::field_info(
            ui,
            xs[1],
            ys[1],
            "Baud rate",
            "The speed of this cable, not of the air link. If the radio does not answer, Corvus tries the other common rates before giving up.",
            "sikbaud-info",
            |ui, top| {
                ui.add_enabled_ui(read, |ui| {
                    picked_baud = ff::select(ui, xs[1], top, widths[1], &baud_choices, &baud, "sikbaud");
                });
                ff::CONTROL_H
            },
        );
        // the scope
        let scope_choices = [
            Choice::new("1", "Read both radios"),
            Choice::new("0", "This radio only"),
        ];
        let scope = if self.remote { "1" } else { "0" };
        ff::field_info(
            ui,
            xs[2],
            ys[2],
            "Scope",
            "Reading the far radio takes a few seconds longer and needs it powered.",
            "sikscope-info",
            |ui, top| {
                picked_scope = ff::select(ui, xs[2], top, widths[2], &scope_choices, scope, "sikscope");
                ff::CONTROL_H
            },
        );
        if let Some(p) = picked_port {
            self.port = p;
        }
        if let Some(b) = picked_baud.and_then(|b| b.parse().ok()) {
            self.baud = b;
        }
        if let Some(s) = picked_scope {
            self.remote = s == "1";
        }
        cy = if narrow { ys[2] } else { cy } + 53.5 + 14.0;
        let load = Button::new("Load settings", Variant::Primary)
            .icon("download")
            .enabled(!self.blocked());
        let (lw, lh) = (load.width(ui), load.height());
        if load.show(ui, Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(lw, lh)), "sikload") {
            self.start_load(ctx);
        }
        cy += lh;
        if let Some((kind, text)) = &self.message {
            cy = message::show(ui, ix, cy + 10.0, iw, text, *kind);
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 31.0)),
            card::Look::Page,
        );
        cy + 31.0
    }

    fn link_card(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, loaded: &Loaded) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Link") + 6.0;
        let dash = "\u{2014}".to_string();
        let version = |r: &Option<model::Radio>| match r {
            Some(r) if !r.version.is_empty() => r.version.clone(),
            _ => dash.clone(),
        };
        let mut rows: Vec<(&str, String)> = vec![
            ("This radio", version(&loaded.local)),
            (
                "Remote radio",
                if loaded.remote.is_some() {
                    version(&loaded.remote)
                } else {
                    "not answering".into()
                },
            ),
        ];
        let banner = loaded.firmware_differs();
        if let Some(l) = &loaded.link {
            rows.push(("Signal (this / remote)", l.rssi.clone()));
            rows.push(("Noise (this / remote)", l.noise.clone()));
            rows.push(("Fade margin (this / remote)", l.margin.clone()));
        }
        let n = rows.len();
        for (i, (label, value)) in rows.iter().enumerate() {
            let last = i + 1 == n && !banner;
            cy = info_row(ui, ix, iw, cy, label, value, last);
            if i == 1 && banner {
                cy += 6.0;
                cy = pagekit::warning_banner(ui, ix, cy, iw, model::FIRMWARE_WARNING);
            }
            if i + 1 < n {
                cy += 6.0;
            }
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    fn mismatch_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        loaded: &Loaded,
    ) -> f32 {
        let _ = ctx;
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "The two radios disagree") + 10.0;
        cy = desc_box(ui, ix, cy, iw, model::MISMATCH_INTRO) + 10.0;
        for m in &loaded.mismatches {
            let g = kit::wrapped_weight(ui, &m.sentence(), 12.0, Weight::W400, style::text_2(), iw - 18.0, 1.6);
            painter.circle_filled(Pos2::new(ix + 6.0, cy + 9.6), 2.0, style::text_2());
            cy += g.size().y;
            painter.galley(Pos2::new(ix + 18.0, cy - g.size().y), g, style::text_2());
        }
        cy += 10.0;
        let b = Button::new("Copy these to the remote radio", Variant::Secondary)
            .icon("copy")
            .enabled(!self.blocked());
        let (bw, bh) = (b.width(ui), b.height());
        if b.show(ui, Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(bw, bh)), "sikcopy") {
            self.edits.copy_to_remote(&loaded.mismatches);
            for m in &loaded.mismatches {
                if let Some(v) = self.edits.get(Side::Remote, &m.name) {
                    let t = crate::data::units::js_number(v.as_f64().unwrap_or(0.0));
                    self.texts.insert((Side::Remote.id(), m.name.clone()), t);
                }
            }
            self.message = Some((Msg::Plain, model::STAGED_NOTE.into()));
        }
        cy += bh;
        let rect = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0));
        card::fill(&painter, bg, rect, card::Look::Page);
        painter.rect_stroke(
            rect,
            style::RADIUS,
            Stroke::new(1.0, style::warning()),
            egui::StrokeKind::Inside,
        );
        cy + 19.0
    }

    fn columns(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, loaded: &Loaded, narrow: bool) -> f32 {
        let col_w = if narrow { w } else { (w - 14.0) / 2.0 };
        let mut bottom = y;
        let mut cy = y;
        for (i, side) in Side::ALL.into_iter().enumerate() {
            let cx = if narrow { x } else { x + i as f32 * (col_w + 14.0) };
            let top = if narrow { cy } else { y };
            let end = match loaded.radio(side) {
                Some(radio) => self.radio_card(ui, cx, top, col_w, side, radio),
                None if side == Side::Remote => self.unreachable_card(ui, cx, top, col_w),
                None => top,
            };
            if narrow && end > top {
                cy = end + 26.0;
            }
            bottom = bottom.max(end);
        }
        bottom
    }

    fn unreachable_card(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, Side::Remote.title()) + 2.0;
        cy = desc_box(ui, ix, cy, iw, model::UNREACHABLE);
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    fn radio_card(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, side: Side, radio: &model::Radio) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, side.title()) + 12.0;
        if !radio.board.is_empty() {
            cy = info_row(ui, ix, iw, cy, "Board", &radio.board, false) + 12.0;
        }
        let blocked = self.blocked();
        let fields: Vec<&Field> = radio.fields.iter().filter(|f| f.shown(self.show_advanced)).collect();
        for (i, f) in fields.iter().enumerate() {
            cy = self.field(ui, ix, cy, iw, side, f, blocked);
            if i + 1 < fields.len() {
                cy += 12.0;
            }
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    #[allow(clippy::too_many_arguments)]
    fn field(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, side: Side, f: &Field, blocked: bool) -> f32 {
        let dirty = self.edits.is_dirty(side, &f.name);
        let shown = self.edits.shown_value(side, f);
        let mut stage = None;
        let mut edited = None;
        let bottom = ff::field(ui, x, y, w, &f.caption(), &f.hint, |ui, top| {
            let rect = Rect::from_min_size(Pos2::new(x, top), Vec2::new(w, ff::CONTROL_H));
            if f.read_only {
                let mut text = crate::data::units::js_number(f.value);
                ui.add_enabled_ui(false, |ui| {
                    ff::input(
                        ui,
                        x,
                        top,
                        w,
                        &mut text,
                        "",
                        false,
                        false,
                        ("sikro", side.id(), &f.name),
                    );
                });
            } else if f.kind == Kind::Enum {
                let choices: Vec<Choice> = f
                    .options
                    .iter()
                    .map(|(v, l)| Choice::new(v.to_string(), l.clone()))
                    .collect();
                let selected = shown
                    .as_f64()
                    .map(|v| (v.round() as i64).to_string())
                    .unwrap_or_default();
                ui.add_enabled_ui(!blocked, |ui| {
                    if let Some(p) = ff::select(ui, x, top, w, &choices, &selected, ("siksel", side.id(), &f.name)) {
                        stage = model::parse_int(&p);
                    }
                });
            } else {
                let key = (side.id(), f.name.clone());
                let mut text = self
                    .texts
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| crate::data::units::js_number(shown.as_f64().unwrap_or(0.0)));
                ui.add_enabled_ui(!blocked, |ui| {
                    let r = ff::input(
                        ui,
                        x,
                        top,
                        w,
                        &mut text,
                        "",
                        false,
                        false,
                        ("sikin", side.id(), &f.name),
                    );
                    if r.changed() {
                        edited = Some(text.clone());
                    }
                });
                if let Some(t) = &edited {
                    self.texts.insert(key, t.clone());
                }
            }
            if dirty {
                let p = ui.painter();
                card::paint(p, rect, card::Look::Chosen);
            }
            ff::CONTROL_H
        });
        if let Some(t) = edited {
            stage = model::parse_int(&t);
        }
        self.edits.stage(side, f, stage);
        let mut bottom = bottom;
        if f.must_match {
            let g = style::galley(
                ui,
                &"must match at both ends".to_uppercase(),
                10.0,
                Weight::W400,
                style::text_3(),
                0.2,
            );
            ui.painter().galley(Pos2::new(x, bottom + 3.0), g, style::text_3());
            bottom += 3.0 + 12.0;
        }
        bottom
    }

    fn actions_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let blocked = self.blocked();
        let save_label = self.edits.save_label();
        let advanced = Button::new(
            if self.show_advanced {
                "Hide advanced settings"
            } else {
                "Show advanced settings"
            },
            Variant::Ghost,
        )
        .small()
        .icon(if self.show_advanced {
            "chevron-up"
        } else {
            "chevron-down"
        });
        let save = Button::new(&save_label, Variant::Primary)
            .icon("save")
            .enabled(!blocked);
        let resets: Vec<(Side, Button)> = Side::ALL
            .into_iter()
            .filter(|s| self.loaded.as_ref().is_some_and(|l| l.radio(*s).is_some()))
            .map(|s| {
                (
                    s,
                    Button::new(
                        if s == Side::Remote {
                            "Reset remote radio"
                        } else {
                            "Reset this radio"
                        },
                        Variant::Danger,
                    )
                    .small()
                    .icon("rotate-ccw")
                    .enabled(!blocked),
                )
            })
            .collect();
        let size = |b: &Button| (b.width(ui), b.height());
        let (aw, ah) = size(&advanced);
        let (sw, sh) = size(&save);
        let reset_sizes: Vec<(f32, f32)> = resets.iter().map(|(_, b)| size(b)).collect();
        let resets_w: f32 =
            reset_sizes.iter().map(|s| s.0).sum::<f32>() + 10.0 * reset_sizes.len().saturating_sub(1) as f32;
        let row_h = ah.max(sh).max(reset_sizes.iter().map(|s| s.1).fold(0.0, f32::max));
        let first_w = aw + 10.0 + sw;
        let wrap = !resets.is_empty() && first_w + 10.0 + resets_w > iw;
        let top = y + 19.0;
        let mid = top + row_h / 2.0;
        let mut hit_advanced = false;
        let mut hit_save = false;
        let mut hit_reset = None;
        if advanced.show(
            ui,
            Rect::from_min_size(Pos2::new(ix, mid - ah / 2.0), Vec2::new(aw, ah)),
            "sikadv",
        ) {
            hit_advanced = true;
        }
        if save.show(
            ui,
            Rect::from_min_size(Pos2::new(ix + aw + 10.0, mid - sh / 2.0), Vec2::new(sw, sh)),
            "siksave",
        ) {
            hit_save = true;
        }
        let mut bottom = top + row_h;
        let mut rx = ix + iw - resets_w;
        let rmid = if wrap {
            bottom += 10.0;
            let m = bottom + row_h / 2.0;
            bottom += row_h;
            m
        } else {
            mid
        };
        for ((side, b), (bw, bh)) in resets.into_iter().zip(reset_sizes) {
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(rx, rmid - bh / 2.0), Vec2::new(bw, bh)),
                ("sikreset", side.id()),
            ) {
                hit_reset = Some(side);
            }
            rx += bw + 10.0;
        }
        if hit_advanced {
            self.show_advanced = !self.show_advanced;
        }
        if hit_save {
            self.start_save(ctx);
        }
        if let Some(side) = hit_reset {
            self.confirm_reset = Some(side);
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, bottom + 19.0)),
            card::Look::Page,
        );
        bottom + 19.0
    }

    fn reset_dialog(&mut self, ctx: &egui::Context, side: Side) {
        let mut choice = None;
        let title = format!("Reset {} to factory defaults", side.label());
        let confirm = format!("Reset {}", side.label());
        let closed = modal::show(ctx, "sikreset", modal::Size::Sm, &title, |ui, x, y, w| {
            let lines = model::reset_detail(side);
            let gs: Vec<_> = lines
                .iter()
                .map(|l| kit::wrapped_weight(ui, l, 12.0, Weight::W500, style::text_1(), w - 28.0, 1.5))
                .collect();
            let h: f32 = gs.iter().map(|g| g.size().y).sum::<f32>() + 8.0 + 24.0 + 2.0;
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
            card::paint(ui.painter(), r, card::Look::Tone(crate::ui::message::Tone::Err));
            let mut ty = y + 13.0;
            for g in gs {
                let gh = g.size().y;
                ui.painter().galley(Pos2::new(x + 15.0, ty), g, style::text_1());
                ty += gh + 8.0;
            }
            let (hit, bottom) = modal::actions(
                ui,
                x,
                r.bottom() + 16.0,
                w,
                vec![
                    Button::new("Leave it alone", Variant::Secondary),
                    Button::new(&confirm, Variant::Danger),
                ],
            );
            choice = hit;
            bottom
        });
        if closed || choice == Some(0) {
            self.confirm_reset = None;
        } else if choice == Some(1) {
            self.confirm_reset = None;
            self.start_reset(ctx, side);
        }
    }
}

/// `.params-desc`: a grey box that explains a card; returns its bottom.
fn desc_box(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    message::boxed(ui, x, y, w, text, Msg::Plain)
}

/// `.page-row`: a label at the left and a mono value at the right, a rule
/// under it unless it is the last. Returns the bottom.
fn info_row(ui: &egui::Ui, x: f32, w: f32, y: f32, label: &str, value: &str, last: bool) -> f32 {
    let painter = ui.painter();
    let lg = style::galley(ui, label, 13.0, Weight::W400, style::text_1(), 0.0);
    let vg = style::galley_mono(ui, value, 13.0, style::text_2());
    let h = if last { 37.5 } else { 38.5 };
    let mid = y + 10.0 + 8.75;
    painter.galley(Pos2::new(x, mid - lg.size().y / 2.0), lg, style::text_1());
    painter.galley(
        Pos2::new(x + w - vg.size().x, mid - vg.size().y / 2.0),
        vg,
        style::text_2(),
    );
    if !last {
        painter.hline(
            egui::Rangef::new(x, x + w),
            y + h - 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
    }
    y + h
}

/// POST that waits as long as a radio session does; the backend's own
/// refusal comes back as the error.
fn post(s: &Server, path: &str, body: &Value) -> Result<Value, String> {
    let text = body.to_string();
    let r = http::request(s, "POST", path, Some(&text), "application/json", SESSION_TIMEOUT)
        .map_err(|e| format!("{}{path}: {e}", s.url()))?;
    let status = r.status;
    let raw = r.text().map_err(|e| format!("{}{path}: {e}", s.url()))?;
    let v: Value = serde_json::from_str(&raw).map_err(|_| format!("Invalid response from {path}"))?;
    let failed = !(200..300).contains(&status) || v.get("ok") != Some(&Value::Bool(true));
    if failed {
        let said = v.get("error").and_then(Value::as_str).unwrap_or("");
        return Err(said.to_string());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn answer(status: &str, body: &str) -> (Server, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(l.local_addr().unwrap().port());
        let body = body.to_string();
        let status = status.to_string();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            // The head and the body can arrive in two segments: read until quiet.
            s.set_read_timeout(Some(std::time::Duration::from_millis(150))).unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = s.read(&mut buf) {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&got).contains("\r\n\r\n") && got.len() > 60 {
                    s.set_read_timeout(Some(std::time::Duration::from_millis(30))).unwrap();
                }
            }
            let req = String::from_utf8_lossy(&got).to_string();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(reply.as_bytes()).unwrap();
            req
        });
        (server, h)
    }

    #[test]
    fn a_session_answer_is_the_data_and_a_refusal_is_the_backends_own_words() {
        let (server, h) = answer("200 OK", r#"{"ok": true, "applied": {}, "warnings": []}"#);
        let body = model::load_body("/dev/ttyUSB0", 57600, true);
        let v = post(&server, "/api/sik/load", &body).unwrap();
        assert_eq!(v["ok"], true);
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /api/sik/load HTTP/1.1"));
        assert!(req.contains(r#""device":"/dev/ttyUSB0""#));
        let (server, h) = answer("409 Conflict", r#"{"ok": false, "error": "the radio is not powered"}"#);
        assert_eq!(
            post(&server, "/api/sik/load", &body).unwrap_err(),
            "the radio is not powered"
        );
        h.join().unwrap();
    }

    #[test]
    fn the_gate_shuts_the_actions_while_armed_or_busy() {
        let status = json!({"can_configure": true, "ports": [{"device": "/dev/a", "kind": "sik", "is_link": false}]});
        let mut page = SikPage::new(Server::local(1), &egui::Context::default());
        page.status = status;
        page.port = "/dev/a".into();
        assert!(!page.blocked());
        page.armed = true;
        assert!(page.blocked());
        assert_eq!(page.gate(), model::ARMED_REASON);
        page.armed = false;
        page.busy = Some((Task::Load, Pending::ready(Ok(json!({})))));
        assert!(page.blocked());
    }

    #[test]
    fn a_finished_save_reads_the_radios_again_and_a_refusal_is_reported() {
        let ctx = egui::Context::default();
        let mut page = SikPage::new(Server::local(1), &ctx);
        page.port = "/dev/a".into();
        page.status_asked = true;
        page.busy = Some((Task::Save, Pending::ready(Ok(json!({"ok": true, "warnings": []})))));
        page.take_answers(&ctx);
        assert!(matches!(page.busy, Some((Task::Load, _))));
        assert_eq!(
            page.message.as_ref().map(|m| m.0),
            Some(Msg::Plain),
            "the read that follows owns the line"
        );
        page.busy = Some((Task::Reset(Side::Remote), Pending::ready(Err("not answering".into()))));
        page.take_answers(&ctx);
        assert!(page.busy.is_none());
        assert_eq!(page.take_errors(), vec!["not answering".to_string()]);
        assert_eq!(page.message, Some((Msg::Err, "not answering".into())));
    }
}
