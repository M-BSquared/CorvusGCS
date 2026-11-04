//! The PID Tuning page of Setup (src/js/setup-tuning.js, the `.tune-*` rules
//! of setup.css).
//!
//! The control cascade, one block per loop on one page, innermost first: the
//! rate controller, the attitude controller, and for a multicopter the
//! velocity and position controllers. Each block holds the loop's live graphs
//! (the setpoint beside what the vehicle did) and gains at the left and its
//! own tune button at the right. The autotune is one run on the vehicle: a
//! block tuned by it starts that run, a block tuned together with another says
//! so on its button, and a block no autotune covers shows a greyed button with
//! the reason. The strip above carries the run's progress, what the autopilot
//! is doing, and whether a reboot is needed.
//!
//! The autotune runs in flight and gains are written on the ground; the page
//! says which it wants before the operator takes off, rather than letting a
//! refused write say it.

use std::collections::HashMap;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::check::{self, Check};
use crate::setup::params_dialogs;
use crate::setup::schema_form::{FormState, Look};
use crate::setup::tuning_model::{self as model, Buf, ChartSpec, Doc, Group};
use crate::ui::card;
use crate::ui::chart::{self, Series};
use crate::ui::chip::Chip;
use crate::ui::icons;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

type Status = (&'static str, String);

/// A sample is kept at most this often (ms): the web page draws at about 10 Hz
/// and a graph of 600 points is 30 s of it.
const SAMPLE_GAP_MS: f64 = 45.0;

/// The page's state.
pub struct TuningPage {
    server: Server,
    doc: Option<Doc>,
    loading: Option<Pending<Result<Value, String>>>,
    status: Status,
    form: FormState,
    check: Check,
    check_card: Option<check::Summary>,
    check_say: Option<check::Summary>,
    fresh: bool,
    bufs: HashMap<String, Buf>,
    last_push: f64,
    start: Option<f64>,
    restart_pending: bool,
    busy: Option<(bool, Pending<Result<(), String>>)>,
    reboot_modal: bool,
    reboot: Option<Pending<Result<(), String>>>,
    steps_open: bool,
    tab: usize,
    scroll_to: Option<String>,
    loop_tops: Vec<(String, f32)>,
    run_start: Option<f64>,
    last_phase: String,
    errors: Vec<String>,
    notes: Vec<String>,
    warnings: Vec<String>,
    retried: bool,
}

impl TuningPage {
    pub fn new(server: Server, ctx: &egui::Context) -> TuningPage {
        let mut p = TuningPage {
            server,
            doc: None,
            loading: None,
            status: ("pending", "Reading tuning parameters\u{2026}".into()),
            form: FormState::default(),
            check: Check::default(),
            check_card: None,
            check_say: None,
            fresh: false,
            bufs: HashMap::new(),
            last_push: 0.0,
            start: None,
            restart_pending: false,
            busy: None,
            reboot_modal: false,
            reboot: None,
            steps_open: false,
            tab: 0,
            scroll_to: None,
            loop_tops: Vec::new(),
            run_start: None,
            last_phase: String::new(),
            errors: Vec::new(),
            notes: Vec::new(),
            warnings: Vec::new(),
            retried: false,
        };
        p.stream(ctx, true);
        p.load(ctx);
        p
    }

    /// Asks the autopilot for the setpoint messages at a tuning rate for as
    /// long as the page is open. A refusal is not fatal: the response traces
    /// still draw, and the setpoint ones are simply slower or absent.
    fn stream(&self, ctx: &egui::Context, on: bool) {
        let s = self.server.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = http::post_json(
                &s,
                "/api/tuning/stream",
                &json!({"enabled": on, "rate_hz": model::STREAM_HZ}),
            );
            ctx.request_repaint();
        });
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        e.extend(self.form.take_errors());
        e
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    fn load(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        self.status = ("pending", "Reading tuning parameters\u{2026}".into());
        let s = self.server.clone();
        let path = if std::mem::take(&mut self.fresh) {
            "/api/tuning?fresh=1"
        } else {
            "/api/tuning"
        };
        self.loading = Some(Pending::spawn(ctx, move || http::get_json(&s, path)));
    }

    fn take_answers(&mut self, ctx: &egui::Context) {
        if let Some(summary) = self.check.take(&mut self.form.wanted) {
            self.form.mark_checked(&summary.rows);
            self.check_card = Some(summary.clone());
            self.check_say = Some(summary);
            self.fresh = true;
            self.load(ctx);
        }
        if let Some(r) = self.loading.as_ref().and_then(Pending::take) {
            self.loading = None;
            match r {
                Ok(v) => {
                    let doc = model::parse_doc(&v);
                    self.status = if doc.connected {
                        ("ok", format!("{} parameters read", doc.received))
                    } else {
                        (
                            "err",
                            if doc.error.is_empty() {
                                "No tuning parameters received".into()
                            } else {
                                doc.error.clone()
                            },
                        )
                    };
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
                            "Could not read the tuning parameters".into()
                        } else {
                            e
                        },
                    );
                }
            }
        }
        if let Some(doc) = &mut self.doc {
            for g in &mut doc.groups {
                for s in &mut g.sections {
                    self.form.take_answers(&mut s.fields);
                }
                if let Some(f) = g.axes.as_mut().and_then(|a| a.field.as_mut()) {
                    self.form.take_answers(std::slice::from_mut(f));
                }
                // the module is read at boot, so switching it on changes
                // nothing until the autopilot restarts: the page says so and
                // holds the start back until then
                if g.autotune && !g.enable_param.is_empty() {
                    let value = g
                        .sections
                        .iter()
                        .flat_map(|s| &s.fields)
                        .find(|f| f.param == g.enable_param)
                        .map(|f| f.value);
                    match value {
                        Some(1.0) => {
                            if g.enabled == Some(false) {
                                self.restart_pending = true;
                            }
                            g.enabled = Some(true);
                        }
                        Some(v) if v == 0.0 && g.enabled == Some(true) => {
                            g.enabled = Some(false);
                            self.restart_pending = false;
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some((enable, p)) = self.busy.as_ref() {
            if let Some(r) = p.take() {
                let enable = *enable;
                self.busy = None;
                match r {
                    Ok(()) => self
                        .notes
                        .push(if enable { "Autotune started" } else { "Autotune stopped" }.into()),
                    Err(e) => self.errors.push(if e.is_empty() {
                        if enable {
                            "Could not start the autotune".into()
                        } else {
                            "Could not stop the autotune".into()
                        }
                    } else {
                        e
                    }),
                }
                // a finished tune writes new gains: the fields would still
                // show the ones it replaced
                if !enable {
                    self.load(ctx);
                }
            }
        }
        if let Some(r) = self.reboot.as_ref().and_then(Pending::take) {
            self.reboot = None;
            match r {
                Ok(()) => {
                    self.restart_pending = false;
                    self.notes
                        .push("The autopilot is rebooting. The link returns in a few seconds.".into());
                }
                Err(e) => self.errors.push(format!(
                    "Reboot refused: {}",
                    if e.is_empty() { "no answer" } else { &e }
                )),
            }
        }
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        let now_ms = now;
        let start = *self.start.get_or_insert(now_ms);
        self.take_answers(&ctx);
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let connected = state.get("connected") == Some(&Value::Bool(true));
        let landed = state.get("landed_state").and_then(Value::as_i64).unwrap_or(0);
        let phase = state
            .get("autotune_state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let progress = state.get("autotune_progress").and_then(Value::as_f64).unwrap_or(0.0);
        if !connected {
            self.restart_pending = false;
            self.retried = false;
        } else if !self.retried
            && self.loading.is_none()
            && !self.check.busy()
            && self.doc.as_ref().is_some_and(|d| !d.connected)
        {
            self.retried = true;
            self.load(&ctx);
        }
        // a tune that just finished has written new gains into the vehicle
        if self.last_phase == "running" && !phase.is_empty() && phase != "running" {
            if phase == "done" {
                self.notes.push("Autotune complete".into());
            } else {
                self.warnings.push("Autotune did not finish".into());
            }
            self.load(&ctx);
        }
        if phase == "running" && self.run_start.is_none() {
            self.run_start = Some(now_ms);
        }
        if phase != "running" {
            self.run_start = None;
        }
        self.last_phase = phase.clone();
        if now_ms - self.last_push >= SAMPLE_GAP_MS {
            self.last_push = now_ms;
            if let Some(doc) = &self.doc {
                for g in &doc.groups {
                    for spec in &g.charts {
                        self.bufs
                            .entry(spec.id.clone())
                            .or_default()
                            .push(spec, state, (now_ms - start) / 1000.0);
                    }
                }
            }
        }

        let idle = self.loading.is_none() && !self.check.busy();
        let can_check = idle
            && self
                .doc
                .as_ref()
                .is_some_and(|d| d.connected && !d.check_names().is_empty());
        let flying = model::is_flying(connected, armed, landed);
        let link = if connected {
            ready_chip("Link up", true)
        } else {
            ready_chip("No link", false)
        };
        let flight = ready_chip(model::flight_label(armed, landed), flying);
        let (mut y, pressed) = pagekit::head_actions_with(
            ui,
            x,
            y,
            w,
            "PID Tuning",
            "The control cascade, one loop at a time, by hand or by autotune",
            // the link chip already says when there is no vehicle
            &(
                self.status.0,
                if connected || self.status.0 != "err" {
                    self.status.1.clone()
                } else {
                    String::new()
                },
            ),
            vec![
                Button::new("Reload", Variant::Primary)
                    .icon("refresh-cw")
                    .small()
                    .actions()
                    .enabled(self.loading.is_none()),
                Button::new("Check values", Variant::Secondary)
                    .icon("list-checks")
                    .small()
                    .actions()
                    .title("Read every value back from the vehicle, write again what did not stick, and show what the vehicle holds now")
                    .enabled(can_check),
            ],
            vec![link, flight],
            0.0,
            "tunehead",
        );
        match pressed {
            Some(0) => {
                self.check_card = None;
                self.load(&ctx);
            }
            Some(1) => {
                if let Some(doc) = &self.doc {
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
                    let names = doc.check_names();
                    self.check.start(&ctx, &self.server, &wanted, names);
                }
            }
            _ => {}
        }
        if armed {
            y = pagekit::warning_banner(
                ui,
                x,
                y,
                w,
                "Gains are read-only while armed. Land and disarm to change them. The autotune is the opposite: it only runs in flight.",
            ) + 14.0;
        }
        let doc = self.doc.clone().unwrap_or_default();
        let loops: Vec<&Group> = doc.groups.iter().filter(|g| !g.autotune).collect();
        let autotune = doc.groups.iter().find(|g| g.autotune);

        // the tab strip is a jump list: every loop is on the page already
        if !loops.is_empty() {
            let labels: Vec<&str> = loops
                .iter()
                .map(|g| {
                    if g.title.is_empty() {
                        g.id.as_str()
                    } else {
                        g.title.as_str()
                    }
                })
                .collect();
            let (ty, hit) = pagekit::tabs(ui, x, y, w, &labels, self.tab.min(labels.len() - 1), "tunetabs");
            y = ty;
            if let Some(i) = hit {
                self.tab = i;
                self.scroll_to = Some(loops[i].id.clone());
            }
        }
        y += 4.0;
        if let Some(id) = self.scroll_to.take() {
            if let Some((_, top)) = self.loop_tops.iter().find(|(g, _)| *g == id) {
                let r = Rect::from_min_size(Pos2::new(x, *top - 12.0), Vec2::new(w, 40.0));
                ui.scroll_to_rect(r, Some(egui::Align::TOP));
            }
        }
        let painter = ui.painter().clone();
        let (ix, iw) = (x + 19.0, w - 38.0);
        if doc.groups.is_empty() {
            let bg = card::reserve(&painter);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "PID Tuning") + 10.0;
            let g = kit::wrapped_weight(
                ui,
                "Connect to a vehicle to read its controller gains. The page shows only the loops the connected firmware actually reports, so a multicopter and a fixed wing each get their own.",
                12.0,
                Weight::W400,
                style::text_2(),
                iw,
                1.4,
            );
            painter.galley(Pos2::new(ix, cy), g.clone(), style::text_2());
            cy += g.size().y;
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
                card::Look::Page,
            );
            return cy + 19.0;
        }
        if let Some(c) = self.check_card.clone() {
            y = check::card(ui, x, y, w, &c) + 26.0;
        }
        let ctxv = Vehicle {
            connected,
            armed,
            landed,
            phase,
            progress,
            run_start: self.run_start,
            now: now_ms,
        };
        if let Some(g) = autotune {
            y = self.run_strip(ui, &ctx, g, x, y, w, &ctxv) + 26.0;
            y = self.autotune_card(ui, &ctx, g, x, y, w, &ctxv) + 26.0 + 10.0;
        }
        self.loop_tops.clear();
        for g in &loops {
            self.loop_tops.push((g.id.clone(), y));
            y = self.loop_block(ui, &ctx, g, autotune, x, y, w, &ctxv, state) + 14.0 + 12.0 + 10.0;
        }
        if let Some(g) = autotune {
            if !g.sections.is_empty() {
                let server = self.server.clone();
                for s in &g.sections {
                    let bg = card::reserve(&painter);
                    let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, &s.title) + 10.0;
                    cy = self
                        .form
                        .show_with(ui, &ctx, &server, &s.fields, ix, cy, iw, armed, Look::TUNING);
                    card::fill(
                        &painter,
                        bg,
                        Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
                        card::Look::Page,
                    );
                    y = cy + 19.0 + 26.0;
                }
                y -= 26.0;
            } else {
                y -= 36.0;
            }
        } else {
            y -= 36.0;
        }
        self.reboot_dialog(&ctx);
        y
    }

    fn reboot_dialog(&mut self, ctx: &egui::Context) {
        if !self.reboot_modal {
            return;
        }
        let (closed, go) = params_dialogs::reboot(ctx);
        if closed {
            self.reboot_modal = false;
        }
        if go {
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

    /// The run, as a strip: progress, what the autopilot is doing, anything
    /// blocking the start. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    fn run_strip(
        &mut self,
        ui: &mut egui::Ui,
        _ctx: &egui::Context,
        g: &Group,
        x: f32,
        y: f32,
        w: f32,
        v: &Vehicle,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = y + 19.0;
        // the head: the module's name and the phase chip
        let label = style::galley(ui, &g.label, 13.0, Weight::W600, style::text_1(), 0.0);
        let phase_text = model::phase_text(&v.phase);
        let (fg, bgc, bd) = phase_colors(&v.phase);
        let chip = Chip {
            size: 9.5,
            weight: Weight::W700,
            letter: 0.7,
            pad: (3.0, 9.0, 3.0, 9.0),
            fg,
            bg: bgc,
            border: bd,
            ..Chip::new(phase_text.to_uppercase())
        };
        let cs = chip.measure(ui);
        let head_h = cs.y.max(label.size().y);
        painter.galley(
            Pos2::new(ix, cy + (head_h - label.size().y) / 2.0),
            label,
            style::text_1(),
        );
        chip.paint(ui, Pos2::new(ix + iw - cs.x, cy + (head_h - cs.y) / 2.0));
        cy += head_h + 9.0;
        // the bar
        let (share, indeterminate, percent) = model::bar_share(&v.phase, g.progress, v.progress);
        let elapsed = v
            .run_start
            .map(|s| format!("{} elapsed", model::clock((v.now - s) / 1000.0)))
            .unwrap_or_default();
        let pg = style::galley(ui, &percent, 12.0, Weight::W600, style::text_1(), 0.0);
        let eg = style::galley(ui, &elapsed, 11.0, Weight::W400, style::text_3(), 0.0);
        let bar_w = iw - 10.0 - 36.0 - 10.0 - eg.size().x - if eg.size().x > 0.0 { 0.0 } else { -10.0 };
        let bar = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(bar_w.max(40.0), 8.0));
        painter.rect_filled(bar, 4, style::surface_3());
        let fill_col = if v.phase == "done" {
            style::healthy()
        } else {
            style::nav()
        };
        if indeterminate {
            // `tuneSweep`: a third of the bar sweeping across, 1.5 s
            let k = ((v.now / 1500.0).fract() as f32 * 1.35) - 0.35;
            let seg = Rect::from_min_size(
                Pos2::new(bar.left() + bar.width() * k, bar.top()),
                Vec2::new(bar.width() * 0.35, 8.0),
            );
            painter.with_clip_rect(bar).rect_filled(
                seg.intersect(bar.expand2(Vec2::new(bar.width(), 0.0))),
                4,
                fill_col,
            );
            _ctx.request_repaint();
        } else if share > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(bar.min, Vec2::new(bar.width() * (share / 100.0) as f32, 8.0)),
                4,
                fill_col,
            );
        }
        let px = bar.right() + 10.0;
        painter.galley(
            Pos2::new(px + 36.0 - pg.size().x, cy + 4.0 - pg.size().y / 2.0),
            pg,
            style::text_1(),
        );
        painter.galley(
            Pos2::new(px + 36.0 + 10.0, cy + 4.0 - eg.size().y / 2.0),
            eg,
            style::text_3(),
        );
        if v.run_start.is_some() {
            _ctx.request_repaint();
        }
        cy += 8.0 + 9.0;
        // what the autopilot is doing
        let color = match v.phase.as_str() {
            "failed" => style::critical(),
            "done" => style::healthy(),
            _ => style::text_2(),
        };
        let g_msg = kit::wrapped_weight(
            ui,
            &model::run_message(&v.phase, g),
            12.5,
            Weight::W400,
            color,
            iw,
            1.55,
        );
        painter.galley(Pos2::new(ix, cy), g_msg.clone(), color);
        cy += g_msg.size().y;
        // a reboot is asked for in words and offered as a button
        let reason = model::reboot_reason(self.restart_pending, g);
        if !reason.is_empty() {
            cy += 9.0;
            let b = Button::new("Reboot autopilot", Variant::Secondary)
                .icon("power")
                .small()
                .enabled(!v.armed && v.connected);
            let (bw, bh) = (b.width(ui), b.height());
            let tw = (iw - 28.0 - bw - 10.0).max(240.0f32.min(iw - 28.0));
            let tg = kit::wrapped_weight(ui, reason, 11.5, Weight::W400, style::warning(), tw, 1.4);
            let h = tg.size().y.max(bh) + 24.0;
            let r = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, h));
            painter.rect_filled(r, style::RADIUS_BTN, style::warning_dark());
            painter.rect_stroke(
                r,
                style::RADIUS_BTN,
                Stroke::new(1.0, style::warning()),
                egui::StrokeKind::Inside,
            );
            painter.galley(
                Pos2::new(r.left() + 14.0, r.center().y - tg.size().y / 2.0),
                tg.clone(),
                style::warning(),
            );
            if b.show(
                ui,
                Rect::from_min_size(
                    Pos2::new(r.left() + 14.0 + tg.size().x + 10.0, r.center().y - bh / 2.0),
                    Vec2::new(bw, bh),
                ),
                "tunereboot",
            ) {
                self.reboot_modal = true;
            }
            cy = r.bottom();
        } else {
            let gate = model::gate_text(v.connected, v.armed, v.landed, model::no_axis_selected(axes_field(g)));
            if !gate.is_empty() && v.phase != "running" {
                cy = pagekit::warning_banner(ui, ix, cy + 9.0, iw, gate);
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

    /// What is set before a tune: the axes, and the steps to take first.
    #[allow(clippy::too_many_arguments)]
    fn autotune_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        g: &Group,
        x: f32,
        y: f32,
        w: f32,
        v: &Vehicle,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = y + 19.0;
        let server = self.server.clone();
        if let Some(a) = &g.axes {
            if let (true, Some(f)) = (a.selectable, a.field.as_ref()) {
                cy = self.form.show_with(
                    ui,
                    ctx,
                    &server,
                    std::slice::from_ref(f),
                    ix,
                    cy,
                    iw,
                    v.armed,
                    Look::TUNING,
                ) - 7.0
                    + 0.0;
            } else {
                cy = fixed_axes(ui, a, ix, cy, iw);
            }
        }
        cy += 11.0;
        // the preconditions, before the aircraft leaves the ground
        let head = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, 14.5));
        let resp = ui.interact(head, ui.scope_id().with("tunesteps"), egui::Sense::click());
        let tri = if self.steps_open {
            [
                Pos2::new(ix, cy + 4.0),
                Pos2::new(ix + 8.0, cy + 4.0),
                Pos2::new(ix + 4.0, cy + 10.0),
            ]
        } else {
            [
                Pos2::new(ix + 1.0, cy + 3.0),
                Pos2::new(ix + 1.0, cy + 11.0),
                Pos2::new(ix + 7.0, cy + 7.0),
            ]
        };
        painter.add(egui::Shape::convex_polygon(tri.to_vec(), style::text_2(), Stroke::NONE));
        let sg = style::galley(ui, "Before you start", 12.0, Weight::W600, style::text_2(), 0.0);
        painter.galley(Pos2::new(ix + 16.0, cy), sg, style::text_2());
        if resp.clicked() {
            self.steps_open = !self.steps_open;
        }
        cy += 14.5;
        if self.steps_open {
            cy += 8.0;
            for (i, line) in g.steps.iter().enumerate() {
                let tg = kit::wrapped_weight(ui, line, 12.0, Weight::W400, style::text_2(), iw - 20.0, 1.5);
                let n = style::galley(ui, &format!("{}.", i + 1), 12.0, Weight::W400, style::text_2(), 0.0);
                painter.galley(Pos2::new(ix + 20.0 - 6.0 - n.size().x, cy), n, style::text_2());
                painter.galley(Pos2::new(ix + 20.0, cy), tg.clone(), style::text_2());
                cy += tg.size().y + 6.0;
            }
            cy -= 6.0;
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    /// One controller: its graphs and gains on the left, its tune button on
    /// the right. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    fn loop_block(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        g: &Group,
        autotune: Option<&Group>,
        x: f32,
        y: f32,
        w: f32,
        v: &Vehicle,
        state: &Value,
    ) -> f32 {
        let painter = ui.painter().clone();
        let t = style::galley(
            ui,
            if g.title.is_empty() { &g.id } else { &g.title },
            15.0,
            Weight::W600,
            style::text_1(),
            0.0,
        );
        let mut cy = y + t.size().y + 10.0;
        painter.galley(Pos2::new(x, y), t, style::text_1());
        if !g.hint.is_empty() {
            let tg = kit::wrapped_weight(ui, &g.hint, 12.0, Weight::W400, style::text_2(), w - 24.0, 1.55);
            let r = Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, tg.size().y + 20.0 + 2.0));
            painter.rect_filled(r, style::RADIUS_BTN, style::surface_2());
            painter.rect_stroke(
                r,
                style::RADIUS_BTN,
                Stroke::new(1.0, style::border_soft()),
                egui::StrokeKind::Inside,
            );
            painter.galley(Pos2::new(r.left() + 13.0, r.top() + 11.0), tg, style::text_2());
            cy = r.bottom() + 10.0;
        }
        let narrow = crate::app::responsive::Vw::of(ctx).max720;
        let side_w = 270.0;
        let (mx, mw) = if narrow { (x, w) } else { (x, w - side_w - 14.0) };
        let mut side_y = cy;
        if narrow {
            side_y = self.tune_side(ui, g, autotune, x, cy, w, v) + 12.0;
            cy = side_y;
        } else {
            self.tune_side(ui, g, autotune, x + w - side_w, cy, side_w, v);
        }
        let mut my = cy;
        // the graphs: setpoint against response
        if !g.charts.is_empty() {
            let cols = (((mw + 12.0) / (260.0 + 12.0)).floor() as usize).max(1);
            let cw = (mw - 12.0 * (cols as f32 - 1.0)) / cols as f32;
            let host_h = 211.0;
            for (i, spec) in g.charts.iter().enumerate() {
                let (col, row) = (i % cols, i / cols);
                let r = Rect::from_min_size(
                    Pos2::new(mx + col as f32 * (cw + 12.0), my + row as f32 * (host_h + 12.0)),
                    Vec2::new(cw, host_h),
                );
                self.chart_host(ui, spec, r, state);
            }
            let rows = g.charts.len().div_ceil(cols) as f32;
            my += rows * host_h + (rows - 1.0) * 12.0 + 12.0;
        }
        let server = self.server.clone();
        for s in &g.sections {
            let bg = card::reserve(&painter);
            let (ix, iw) = (mx + 19.0, mw - 38.0);
            let mut sy = pagekit::section_title(ui, ix, my + 19.0, iw, &s.title) + 10.0;
            sy = self
                .form
                .show_with(ui, ctx, &server, &s.fields, ix, sy, iw, v.armed, Look::TUNING);
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(mx, my), Pos2::new(mx + mw, sy + 19.0)),
                card::Look::Page,
            );
            my = sy + 19.0 + 24.0;
        }
        my -= 24.0;
        if narrow {
            my
        } else {
            my.max(side_y)
        }
    }

    /// One graph: the response and, when the vehicle sends one, the setpoint.
    fn chart_host(&mut self, ui: &mut egui::Ui, spec: &ChartSpec, r: Rect, state: &Value) {
        let painter = ui.painter().clone();
        painter.rect_filled(r, style::RADIUS_BTN, style::surface_1());
        painter.rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        let title = style::galley(ui, &spec.title, 11.0, Weight::W600, style::text_2(), 0.3);
        painter.galley(Pos2::new(r.left() + 13.0, r.top() + 9.0), title, style::text_2());
        // the live pill: a pulsing dot and the word
        let word = style::galley(ui, "LIVE", 9.5, Weight::W400, style::text_3(), 0.6);
        let pill_w = 7.0 + 5.0 + word.size().x;
        let px = r.right() - 13.0 - pill_w;
        let t = ui.input(|i| i.time);
        let pulse = 0.7 + 0.3 * ((t / 1.6).fract() as f32 * std::f32::consts::TAU).cos();
        painter.circle_filled(
            Pos2::new(px + 3.5, r.top() + 9.0 + 6.5),
            3.5,
            style::healthy().gamma_multiply(pulse),
        );
        painter.galley(
            Pos2::new(px + 12.0, r.top() + 9.0 + (13.0 - word.size().y) / 2.0),
            word,
            style::text_3(),
        );
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        let rect = Rect::from_min_size(
            Pos2::new(r.left() + 9.0, r.top() + 26.0),
            Vec2::new(r.width() - 18.0, 180.0),
        );
        if !ui.is_rect_visible(rect) {
            return;
        }
        let Some(buf) = self.bufs.get(&spec.id) else { return };
        // the setpoint is drawn only when the vehicle is actually sending one:
        // a dotted line pinned at zero would claim the controller commands nothing
        let live = state.get("setpoints_live") == Some(&Value::Bool(true));
        let actual: Vec<Option<f64>> = buf.actual.iter().map(|v| Some(*v)).collect();
        let mut series = vec![Series {
            y: &actual,
            color: style::nav(),
            width: 1.6,
            dotted: false,
        }];
        if spec.setpoint.is_some() && live {
            series.push(Series {
                y: &buf.sp,
                color: style::healthy(),
                width: 1.4,
                dotted: true,
            });
        }
        chart::draw(ui, rect, &buf.t, &series, "Time (s)", &spec.unit);
    }

    /// The tune button beside one controller. The autotune is one run on the
    /// vehicle, so what a button does depends on how the firmware covers the
    /// loop. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    fn tune_side(
        &mut self,
        ui: &mut egui::Ui,
        g: &Group,
        autotune: Option<&Group>,
        x: f32,
        y: f32,
        w: f32,
        v: &Vehicle,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Autotune") + 10.0;
        let tunable = g.tune.mode != "manual" && autotune.is_some();
        let running = v.phase == "running";
        let (share_has, progress) = (autotune.is_none_or(|a| a.progress), v.progress);
        let status = if tunable {
            model::side_status(&v.phase, share_has, progress)
        } else {
            "Set by hand".to_string()
        };
        let col = match v.phase.as_str() {
            "running" if tunable => style::nav(),
            "done" if tunable => style::healthy(),
            "failed" if tunable => style::critical(),
            _ => style::text_2(),
        };
        let sg = style::galley(ui, &status, 12.0, Weight::W600, col, 0.0);
        painter.galley(Pos2::new(ix, cy), sg, col);
        cy += 12.0 * 1.4 + 10.0;
        let (label, icon, variant) = if running && tunable {
            ("Stop autotune", "octagon-x", Variant::Danger)
        } else if tunable {
            ("Start autotune", "play", Variant::Primary)
        } else {
            ("No autotune", "ban", Variant::Primary)
        };
        let startable = tunable
            && autotune.is_some_and(|a| {
                !running
                    && self.busy.is_none()
                    && model::is_flying(v.connected, v.armed, v.landed)
                    && !model::no_axis_selected(axes_field(a))
                    && model::reboot_reason(self.restart_pending, a).is_empty()
            });
        let enabled = if running {
            self.busy.is_none() && v.connected
        } else {
            startable
        };
        let b = Button::new(label, variant).icon(icon).enabled(enabled);
        let (bw, bh) = (b.width(ui), b.height());
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(bw, bh)),
            ("tunestart", &g.id),
        ) {
            let enable = !running;
            let s = self.server.clone();
            self.busy = Some((
                enable,
                Pending::spawn(ui.ctx(), move || {
                    let d = http::post_json(&s, "/api/autotune", &json!({"axis": "all", "enabled": enable}))?;
                    if d.get("ok") == Some(&Value::Bool(false)) {
                        Err(d.get("error").and_then(Value::as_str).unwrap_or("").to_string())
                    } else {
                        Ok(())
                    }
                }),
            ));
        }
        cy += bh + 10.0;
        let ng = kit::wrapped_weight(ui, &g.tune.text, 11.5, Weight::W400, style::text_3(), iw, 1.5);
        painter.galley(Pos2::new(ix, cy), ng.clone(), style::text_3());
        cy += ng.size().y;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }
}

/// What the vehicle and the run are doing this frame.
struct Vehicle {
    connected: bool,
    armed: bool,
    landed: i64,
    phase: String,
    progress: f64,
    run_start: Option<f64>,
    now: f64,
}

fn axes_field(g: &Group) -> Option<&crate::setup::schema_form::Field> {
    g.axes.as_ref().filter(|a| a.selectable).and_then(|a| a.field.as_ref())
}

/// `calib-phase` colours for the autotune's phase.
fn phase_colors(phase: &str) -> (egui::Color32, egui::Color32, egui::Color32) {
    match phase {
        "running" => (style::nav(), style::nav_dark(), style::nav()),
        "done" => (style::healthy(), style::healthy_dark(), style::healthy()),
        "failed" => (style::critical_bright(), style::critical_dark(), style::critical()),
        _ => (style::text_3(), style::surface_2(), style::border_soft()),
    }
}

/// `readyChip`: a dot and a word, green when ready, red when not.
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

/// A stack with no axis selection (PX4's multicopter autotune) still gets the
/// row, ticked and greyed with the reason: an operator looking for the per
/// axis boxes learns why they are not there rather than that they are missing.
fn fixed_axes(ui: &mut egui::Ui, a: &model::Axes, x: f32, y: f32, w: f32) -> f32 {
    let painter = ui.painter().clone();
    let label_w = (w - 12.0 - 110.0).clamp(150.0, 240.0);
    let (cx, cw) = (x + label_w + 12.0, w - label_w - 12.0);
    let top = y + 7.0;
    let lg = style::galley(
        ui,
        if a.label.is_empty() { "Axes to tune" } else { &a.label },
        12.0,
        Weight::W400,
        style::text_1(),
        0.0,
    );
    painter.galley(Pos2::new(x, top + (18.0 - lg.size().y) / 2.0), lg, style::text_1());
    let mut bx = cx;
    for l in &a.labels {
        let g = style::galley(ui, l, 11.5, Weight::W400, style::text_3(), 0.0);
        let r = Rect::from_min_size(Pos2::new(bx, top + 2.0), Vec2::splat(14.0));
        painter.rect_filled(r, 3, style::text_3());
        painter.rect_stroke(r, 3, Stroke::new(1.0, style::border_strong()), egui::StrokeKind::Inside);
        painter.line(
            vec![
                r.left_top() + Vec2::new(3.2, 7.2),
                r.left_top() + Vec2::new(6.0, 10.0),
                r.left_top() + Vec2::new(11.0, 4.2),
            ],
            Stroke::new(1.8, egui::Color32::WHITE),
        );
        painter.galley(
            Pos2::new(r.right() + 6.0, top + (18.0 - g.size().y) / 2.0),
            g.clone(),
            style::text_3(),
        );
        bx += 14.0 + 6.0 + g.size().x + 14.0;
    }
    let mut cy = top + 18.0;
    if !a.reason.is_empty() {
        cy += 6.0;
        let g = kit::wrapped_weight(ui, &a.reason, 10.5, Weight::W400, style::text_3(), cw, 1.4);
        painter.galley(Pos2::new(cx, cy), g.clone(), style::text_3());
        cy += g.size().y;
    }
    let _ = icons::paint;
    cy + 7.0
}

impl Drop for TuningPage {
    fn drop(&mut self) {
        // hand the setpoint stream rate back to the firmware; best effort
        let s = self.server.clone();
        std::thread::spawn(move || {
            let _ = http::post_json(
                &s,
                "/api/tuning/stream",
                &json!({"enabled": false, "rate_hz": model::STREAM_HZ}),
            );
        });
    }
}
