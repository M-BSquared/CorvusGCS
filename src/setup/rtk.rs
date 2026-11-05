//! The RTK GPS page of Setup (python/src/js/setup-rtk.js, the `.rtk-*` rules of
//! setup.css). Mostly a status display of the route the corrections take, from
//! the base through this station to the aircraft's receiver; the settings sit
//! below it, in the two shapes that actually differ between operators: how
//! careful the survey is, and whether the corrections come off a cable or out
//! of an NTRIP caster.

use std::collections::HashSet;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::rtk_model::{self as model, Form};
use crate::ui::card::{self};
use crate::ui::chip::Chip;
use crate::ui::formfield as ff;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::{self, Tone};
use crate::ui::pagekit;
use crate::ui::progress::Bar;
use crate::ui::style::{self, Weight};

type Status = (&'static str, String);

/// `.rtk-input { max-width: 220px }`.
const INPUT_W: f32 = 220.0;

/// The page's state.
pub struct RtkPage {
    server: Server,
    status: Value,
    loaded: bool,
    read_failed: bool,
    poll: Option<Pending<Result<Value, String>>>,
    last_poll: f64,
    form: Form,
    /// Set while the operator is editing, so a poll landing mid edit cannot
    /// overwrite a half typed coordinate with the stored one.
    editing: bool,
    dirty: HashSet<&'static str>,
    commit: HashSet<&'static str>,
    saving: Option<Pending<Result<Value, String>>>,
    save_status: Status,
    restart: Option<Pending<Result<Value, String>>>,
    errors: Vec<String>,
}

impl RtkPage {
    pub fn new(server: Server, _ctx: &egui::Context) -> RtkPage {
        RtkPage {
            server,
            status: Value::Null,
            loaded: false,
            read_failed: false,
            poll: None,
            last_poll: f64::MIN,
            form: Form::default(),
            editing: false,
            dirty: HashSet::new(),
            commit: HashSet::new(),
            saving: None,
            save_status: ("", String::new()),
            restart: None,
            errors: Vec::new(),
        }
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn paint_status(&mut self, data: Value) {
        self.status = data;
        self.loaded = true;
        self.read_failed = false;
        if !self.editing {
            if let Some(s) = self.status.get("settings") {
                self.form = Form::from_settings(s);
            }
        }
    }

    fn take_answers(&mut self) {
        if let Some(r) = self.poll.as_ref().and_then(Pending::take) {
            self.poll = None;
            match r {
                Ok(v) => self.paint_status(v),
                Err(_) => {
                    self.read_failed = true;
                }
            }
        }
        if let Some(r) = self.saving.as_ref().and_then(Pending::take) {
            self.saving = None;
            match r {
                Ok(v) => {
                    let warning = v.get("warning").and_then(Value::as_str).unwrap_or("");
                    self.save_status = (
                        "ok",
                        if warning.is_empty() {
                            "saved".into()
                        } else {
                            warning.to_string()
                        },
                    );
                    self.editing = false;
                    if let Some(s) = v.get("status").filter(|s| s.is_object()) {
                        self.paint_status(s.clone());
                    }
                }
                Err(e) => {
                    let msg = if e.is_empty() { "could not save".to_string() } else { e };
                    self.save_status = ("err", msg.clone());
                    self.errors.push(format!("Could not save the RTK settings: {msg}"));
                }
            }
        }
        if let Some(r) = self.restart.as_ref().and_then(Pending::take) {
            self.restart = None;
            match r {
                Ok(v) => {
                    self.save_status = ("", String::new());
                    if let Some(s) = v.get("status").filter(|s| s.is_object()) {
                        self.paint_status(s.clone());
                    }
                }
                Err(e) => {
                    self.save_status = ("err", if e.is_empty() { "could not restart".into() } else { e });
                }
            }
        }
    }

    /// Sends the whole form; the backend replaces the block wholesale.
    fn save(&mut self, ctx: &egui::Context) {
        if self.saving.is_some() {
            return;
        }
        self.save_status = ("pending", "saving".into());
        let body = self.form.body();
        let s = self.server.clone();
        self.saving = Some(Pending::spawn(ctx, move || post(&s, "/api/rtk/settings", &body)));
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, _state: &Value, now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers();
        if self.poll.is_none() && now - self.last_poll >= model::POLL_MS {
            self.last_poll = now;
            let s = self.server.clone();
            self.poll = Some(Pending::spawn(&ctx, move || http::get_json(&s, "/api/rtk/status")));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let mut y = pagekit::header(
            ui,
            x,
            y,
            "RTK GPS",
            "Centimetre positioning from a base station on this computer, or from an NTRIP caster",
        ) + 28.0;
        y = self.status_card(ui, &ctx, x, y, w) + 26.0;
        y = self.settings_card(ui, &ctx, x, y, w);
        y
    }

    fn status_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Base station") + 12.0;
        let d = self.status.clone();
        let state = d.get("state").and_then(Value::as_str).unwrap_or("off").to_string();
        let (badge_text, tone, message) = if self.read_failed {
            (
                model::state_label("error"),
                "err",
                "Could not read the RTK status.".to_string(),
            )
        } else if !self.loaded {
            (
                "\u{2026}".to_string(),
                "",
                "Reading the base station\u{2026}".to_string(),
            )
        } else {
            (
                model::state_label(&state),
                model::state_tone(&state),
                d.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
            )
        };
        let (fg, bgc, bd) = match tone {
            "ok" => (style::healthy(), style::surface_2(), style::healthy()),
            "err" => (style::critical(), style::surface_2(), style::critical()),
            "pending" => (style::warning(), style::surface_2(), style::warning()),
            _ => (style::text_2(), style::surface_2(), style::border()),
        };
        let chip = Chip {
            size: 12.0,
            weight: Weight::W600,
            pad: (3.0, 10.0, 3.0, 10.0),
            fg,
            bg: bgc,
            border: bd,
            ..Chip::new(badge_text)
        };
        let cs = chip.measure(ui);
        chip.paint(ui, Pos2::new(ix, cy));
        if !message.is_empty() {
            let g = kit::wrapped_weight(
                ui,
                &message,
                13.0,
                Weight::W400,
                style::text_2(),
                (iw - cs.x - 10.0).max(80.0),
                1.21,
            );
            painter.galley(
                Pos2::new(ix + cs.x + 10.0, cy + (cs.y - g.size().y.min(cs.y)) / 2.0),
                g,
                style::text_2(),
            );
        }
        cy += cs.y + 12.0;
        // a warning, or the error
        let err = d.get("error").and_then(Value::as_str).unwrap_or("");
        let warn = d.get("warning").and_then(Value::as_str).unwrap_or("");
        let text = if err.is_empty() { warn } else { err };
        if !text.is_empty() {
            let col = if err.is_empty() {
                style::warning()
            } else {
                style::critical()
            };
            let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, col, iw, 1.21);
            painter.galley(Pos2::new(ix, cy), g.clone(), col);
            cy += g.size().y + 12.0;
        }
        // the survey, when one is running or has just finished
        let survey = d.get("survey").filter(|s| s.is_object());
        let surveying = state == "surveying" || survey.is_some_and(|s| s.get("active") == Some(&Value::Bool(true)));
        let valid = survey.is_some_and(|s| s.get("valid") == Some(&Value::Bool(true)));
        if surveying || valid {
            let pct = d
                .get("survey_progress")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 100.0);
            let bar = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, 6.0));
            Bar::new((pct / 100.0) as f32)
                .track(style::surface_2())
                .show(&painter, bar);
            let g = kit::wrapped_weight(
                ui,
                &model::survey_sentence(&d, pct),
                12.0,
                Weight::W400,
                style::text_2(),
                iw,
                1.21,
            );
            painter.galley(Pos2::new(ix, cy + 6.0 + 6.0), g.clone(), style::text_2());
            cy += 6.0 + 6.0 + g.size().y + 12.0;
        }
        // the three parties, as rows
        let rows = [
            ("Receiver", model::receiver_text(&d)),
            ("Connection", model::port_text(&d)),
            ("Corrections from the base", model::describe_corrections(&d)),
            ("Sent to the aircraft", model::describe_injected(&d)),
            ("Aircraft GPS", model::fix_text(&d)),
        ];
        for (label, value) in rows {
            let lg = style::galley(ui, label, 13.0, Weight::W400, style::text_1(), 0.0);
            let vg = style::galley_mono(ui, &value, 13.0, style::text_2());
            let mid = cy + 10.0 + 8.75;
            painter.galley(Pos2::new(ix, mid - lg.size().y / 2.0), lg, style::text_1());
            painter.galley(
                Pos2::new(ix + iw - vg.size().x, mid - vg.size().y / 2.0),
                vg,
                style::text_2(),
            );
            painter.hline(
                egui::Rangef::new(ix, ix + iw),
                cy + 38.0 - 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
            cy += 38.5 + 12.0;
        }
        // the restart sits in a row of its own, which is there (and takes its
        // gap) even while the button is not
        if model::can_restart(&d) {
            let b = Button::new("Survey again", Variant::Secondary)
                .icon("rotate-ccw")
                .small()
                .enabled(self.restart.is_none());
            let (bw, bh) = (b.width(ui), b.height());
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(bw, bh)),
                "rtkrestart",
            ) {
                self.save_status = ("pending", "restarting the survey".into());
                let s = self.server.clone();
                self.restart = Some(Pending::spawn(ctx, move || post(&s, "/api/rtk/restart", &json!({}))));
            }
            cy += bh;
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    fn settings_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Where the corrections come from") + 12.0;
        cy = message::boxed(ui, ix, cy, iw, "Corvus looks for an RTK receiver on this computer by itself and starts correcting as soon as one is plugged in. Nothing here needs changing for that. The flight controller's own port is never used, so the aircraft's telemetry link is never at risk.", Tone::Plain) + 12.0;

        let mut save = false;
        // the master switch
        let on = self.form.enabled;
        cy = ff::field(
            ui,
            ix,
            cy,
            iw,
            "RTK corrections",
            "Off by default. While off, Corvus opens no serial port looking for a base.",
            |ui, top| {
                if kit::switch(ui, Pos2::new(ix, top), on, true, "rtkenabled") {
                    self.form.enabled = !on;
                    self.editing = true;
                    save = true;
                }
                22.0
            },
        ) + 12.0;
        // the source
        let source = self.form.source.clone();
        let mut next_source = None;
        cy = ff::field(
            ui,
            ix,
            cy,
            iw,
            "Source",
            "A receiver on a cable needs no network at all. A caster needs one, and gives corrections without a second receiver.",
            |ui, top| {
                let choices = [
                    Choice::new("usb", "Base station on this computer"),
                    Choice::new("ntrip", "NTRIP caster (needs a network)"),
                ];
                next_source = ff::select(ui, ix, top, iw, &choices, &source, "rtksource");
                ff::CONTROL_H
            },
        ) + 12.0;
        if let Some(s) = next_source {
            self.form.source = s;
            self.editing = true;
        }
        if self.form.source == "usb" {
            let mode = self.form.mode.clone();
            let mut next_mode = None;
            cy = ff::field(
                ui,
                ix,
                cy,
                iw,
                "Base position",
                "A survey costs a few minutes and is no more accurate than what it converges on. A known mark skips both.",
                |ui, top| {
                    let choices = [
                        Choice::new("survey", "Work it out (survey-in)"),
                        Choice::new("fixed", "I know where the base is"),
                    ];
                    next_mode = ff::select(ui, ix, top, iw, &choices, &mode, "rtkmode");
                    ff::CONTROL_H
                },
            ) + 12.0;
            if let Some(m) = next_mode {
                self.form.mode = m;
                self.editing = true;
            }
            if self.form.mode == "fixed" {
                let defs: [(&str, &str, &'static str, &str); 4] = [
                    ("Latitude (\u{b0})", "Decimal degrees.", "fixed-latitude", ""),
                    ("Longitude (\u{b0})", "Decimal degrees.", "fixed-longitude", ""),
                    (
                        "Height (m)",
                        "Height above the ellipsoid, not above sea level. A survey record usually gives one or the other, and in most of Europe they differ by tens of metres.",
                        "fixed-altitude",
                        "",
                    ),
                    ("How well it is known (m)", "The accuracy of the mark itself.", "fixed-accuracy", ""),
                ];
                for (i, (label, hint, key, _)) in defs.iter().enumerate() {
                    cy = self.text_field(ui, ix, cy, iw, label, hint, key, i, true, false, "") + 12.0;
                    save |= self.take_commit(key);
                }
            } else {
                cy = self.text_field(
                    ui,
                    ix,
                    cy,
                    iw,
                    "Stop at this accuracy (m)",
                    "How well the base has to know its own position before it starts correcting. Two metres is the usual answer: the aircraft's fix is relative to the base, so this is an offset of the whole flight, not an error in it.",
                    "survey-accuracy",
                    100,
                    true,
                    false,
                    "",
                ) + 12.0;
                save |= self.take_commit("survey-accuracy");
                cy = self.text_field(
                    ui,
                    ix,
                    cy,
                    iw,
                    "Survey for at least (s)",
                    "Both this and the accuracy above have to be met before the survey ends.",
                    "survey-duration",
                    101,
                    true,
                    false,
                    "",
                ) + 12.0;
                save |= self.take_commit("survey-duration");
            }
        } else {
            let defs: [(&str, &str, &'static str, bool); 5] = [
                (
                    "Caster address",
                    "The hostname, without http:// and without the mountpoint.",
                    "ntrip-host",
                    false,
                ),
                ("Port", "2101 unless the provider says otherwise.", "ntrip-port", false),
                (
                    "Mountpoint",
                    "The stream's name on the caster.",
                    "ntrip-mountpoint",
                    false,
                ),
                ("Username", "", "ntrip-username", false),
                (
                    "Password",
                    "Stored in the config file on this computer and never sent back to this page.",
                    "ntrip-password",
                    true,
                ),
            ];
            for (i, (label, hint, key, secret)) in defs.iter().enumerate() {
                let placeholder = if *secret && self.form.has_password {
                    "stored"
                } else {
                    ""
                };
                cy = self.text_field(ui, ix, cy, iw, label, hint, key, 200 + i, false, *secret, placeholder) + 12.0;
                save |= self.take_commit(key);
            }
        }
        if !self.save_status.1.is_empty() {
            let col = pagekit::status_color(self.save_status.0);
            let g = style::galley(ui, &self.save_status.1, 11.5, Weight::W400, col, 0.0);
            painter.galley(Pos2::new(ix, cy), g.clone(), col);
            cy += g.size().y + 12.0;
        }
        cy -= 12.0;
        if save {
            self.save(ctx);
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    /// A caption, a text box (220 px at most) and a hint. `slot` names which
    /// part of the form the text is.
    #[allow(clippy::too_many_arguments)]
    fn text_field(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        label: &str,
        hint: &str,
        key: &'static str,
        slot: usize,
        mono: bool,
        secret: bool,
        placeholder: &str,
    ) -> f32 {
        let mut text = self.slot(key).clone();
        let mut changed = false;
        let mut lost = false;
        let bottom = ff::field(ui, x, y, w, label, hint, |ui, top| {
            let resp = ff::input(
                ui,
                x,
                top,
                INPUT_W.min(w),
                &mut text,
                placeholder,
                mono,
                secret,
                ("rtk", key, slot),
            );
            changed = resp.changed();
            lost = resp.lost_focus();
            ff::CONTROL_H
        });
        if changed {
            *self.slot(key) = text;
            self.editing = true;
            self.dirty.insert(key);
        }
        if lost && self.dirty.contains(key) {
            self.commit.insert(key);
        }
        bottom
    }

    fn slot(&mut self, key: &str) -> &mut String {
        match key {
            "survey-accuracy" => &mut self.form.accuracy,
            "survey-duration" => &mut self.form.duration,
            "fixed-latitude" => &mut self.form.fixed[0],
            "fixed-longitude" => &mut self.form.fixed[1],
            "fixed-altitude" => &mut self.form.fixed[2],
            "fixed-accuracy" => &mut self.form.fixed[3],
            "ntrip-host" => &mut self.form.ntrip[0],
            "ntrip-port" => &mut self.form.ntrip[1],
            "ntrip-mountpoint" => &mut self.form.ntrip[2],
            "ntrip-username" => &mut self.form.ntrip[3],
            _ => &mut self.form.ntrip[4],
        }
    }

    /// Whether the box lost focus with a change, which is when the web page's
    /// `change` event fires and the form is saved.
    fn take_commit(&mut self, key: &'static str) -> bool {
        if self.commit.remove(key) {
            self.dirty.remove(key);
            true
        } else {
            false
        }
    }
}

fn post(s: &Server, path: &str, body: &Value) -> Result<Value, String> {
    let d = http::post_json(s, path, body)?;
    if d.get("ok") == Some(&Value::Bool(false)) {
        Err(d.get("error").and_then(Value::as_str).unwrap_or("").to_string())
    } else {
        Ok(d)
    }
}
