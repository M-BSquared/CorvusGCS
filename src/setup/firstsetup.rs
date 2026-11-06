//! The First Setup page of Setup (python/src/js/setup-firstsetup.js, the `.fs-*`
//! rules of setup.css): a guided tour through the pages that already exist, in
//! the order a new controller needs them.
//!
//! Nothing is reimplemented. Each step hosts the real page (Firmware, Motors,
//! Calibration, Radio Control, Safety, Battery) under a short explanation, and
//! the wizard owns the way on. The one step with no page of its own, choosing
//! what kind of aircraft this is, is built here on `POST
//! /api/first-setup/airframe`, and the last step says what is done and what is
//! still open. A step shows as done when the vehicle says so (the airframe, the
//! calibrations, a link at all) or when the operator has been through it;
//! steps can be taken in any order, because a controller half set up by another
//! ground station needs the second half only.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::data::units::Units;
use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::battery::BatteryPage;
use crate::setup::calibration::CalibrationPage;
use crate::setup::checklist::Checklist;
use crate::setup::firmware::FirmwarePage;
use crate::setup::firstsetup_model::{self as model, HostPage, Kind, Payload, Reboot, State};
use crate::setup::geofence::Env;
use crate::setup::motors::MotorsPage;
use crate::setup::params_dialogs;
use crate::setup::radio::RadioPage;
use crate::setup::safety::SafetyPage;
use crate::ui::card;
use crate::ui::icons;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::modal;
use crate::ui::optioncards::{self, Card};
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

/// Whether a First Setup page exists, so the detector does not open a dialog
/// on top of the wizard (`Corvus.setupFirstSetup.isOpen`).
static OPEN: AtomicUsize = AtomicUsize::new(0);

/// `.fs-steps` and `.fs-bar` leave 14 and 18 px under them, and `.setup-page`
/// puts 4 px between its children.
const AFTER_HEAD: f32 = 28.0 + 4.0;
const AFTER_STEPS: f32 = 14.0 + 4.0;
const AFTER_BAR: f32 = 18.0 + 4.0;
/// A small button as the web fonts measure it.
const SMALL_H: f32 = 29.0;

type Answer = Pending<Result<Value, String>>;

/// The page a step hosts.
enum Hosted {
    Firmware(Box<FirmwarePage>),
    Motors(Box<MotorsPage>),
    Calibration(Box<CalibrationPage>),
    Radio(Box<RadioPage>),
    Safety(Box<SafetyPage>),
    Battery(Box<BatteryPage>),
}

/// `PAGE_FOR`: the page a step hosts, built. The one place that says which
/// native page stands behind which step.
fn host_page(page: HostPage, server: &Server, env: &Env, ctx: &egui::Context) -> Hosted {
    match page {
        HostPage::Firmware => Hosted::Firmware(Box::new(FirmwarePage::new(server.clone(), ctx))),
        HostPage::Motors => Hosted::Motors(Box::new(MotorsPage::new(server.clone(), ctx))),
        HostPage::Calibration => Hosted::Calibration(Box::new(CalibrationPage::new(server.clone(), ctx))),
        HostPage::Radio => Hosted::Radio(Box::new(RadioPage::new(server.clone(), ctx))),
        HostPage::Safety => Hosted::Safety(Box::new(SafetyPage::new(server.clone(), ctx, env.clone()))),
        HostPage::Battery => Hosted::Battery(Box::new(BatteryPage::new(server.clone(), ctx))),
    }
}

/// The page's state.
pub struct FirstSetupPage {
    server: Server,
    env: Env,
    payload: Option<Payload>,
    reading: Option<Answer>,
    /// Nobody named a step: the wizard looks at the controller first and
    /// opens where the work is.
    looking: bool,
    current: &'static str,
    hosted: Option<Hosted>,
    chosen: Option<i64>,
    written: bool,
    reboot: Reboot,
    write_req: Option<Answer>,
    reboot_req: Option<Answer>,
    confirm: bool,
    reboot_dialog: bool,
    finished: bool,
    quiet_path: Option<PathBuf>,
    armed: bool,
    bootloader: bool,
    errors: Vec<String>,
    notes: Vec<String>,
    warnings: Vec<String>,
}

impl Drop for FirstSetupPage {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `isOpen`: whether a First Setup page is up.
pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst) > 0
}

impl FirstSetupPage {
    /// Opens on `step` when one is named, else where the session left off,
    /// else where the controller needs work.
    pub fn new(server: Server, env: Env, ctx: &egui::Context, step: Option<&str>) -> FirstSetupPage {
        OPEN.fetch_add(1, Ordering::SeqCst);
        let remembered = model::memory().current.clone();
        let named = step
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or(Some(remembered).filter(|s| !s.is_empty()));
        let current = model::STEPS[model::index_of(named.as_deref().unwrap_or(""))].id;
        let mut p = FirstSetupPage {
            server,
            env,
            payload: None,
            reading: None,
            looking: named.is_none(),
            current,
            hosted: None,
            chosen: None,
            written: false,
            reboot: Reboot::Idle,
            write_req: None,
            reboot_req: None,
            confirm: false,
            reboot_dialog: false,
            finished: false,
            quiet_path: model::Quiet::default_path(),
            armed: false,
            bootloader: false,
            errors: Vec::new(),
            notes: Vec::new(),
            warnings: Vec::new(),
        };
        if named.is_some() {
            p.show_step(current, ctx);
        }
        p.refresh(ctx);
        p
    }

    /// Whether the operator pressed Finish setup: the page is to be left.
    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        match &mut self.hosted {
            Some(Hosted::Firmware(p)) => e.extend(p.take_errors()),
            Some(Hosted::Motors(p)) => e.extend(p.take_errors()),
            Some(Hosted::Calibration(p)) => e.extend(p.take_errors()),
            Some(Hosted::Radio(p)) => e.extend(p.take_errors()),
            Some(Hosted::Safety(p)) => e.extend(p.take_errors()),
            Some(Hosted::Battery(p)) => e.extend(p.take_errors()),
            None => {}
        }
        e
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        let mut n = std::mem::take(&mut self.notes);
        match &mut self.hosted {
            Some(Hosted::Firmware(p)) => n.extend(p.take_notes()),
            Some(Hosted::Calibration(p)) => n.extend(p.take_notes()),
            Some(Hosted::Radio(p)) => n.extend(p.take_notes()),
            Some(Hosted::Safety(p)) => n.extend(p.take_notes()),
            _ => {}
        }
        n
    }

    pub fn take_warnings(&mut self) -> Vec<String> {
        let mut w = std::mem::take(&mut self.warnings);
        match &mut self.hosted {
            Some(Hosted::Firmware(p)) => w.extend(p.take_warnings()),
            Some(Hosted::Calibration(p)) => w.extend(p.take_warnings()),
            _ => {}
        }
        w
    }

    /// `refresh`: ask the vehicle what it holds now.
    fn refresh(&mut self, ctx: &egui::Context) {
        if self.reading.is_some() {
            return;
        }
        let s = self.server.clone();
        self.reading = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/first-setup")));
    }

    /// `show`: make `id` the step, with its page built fresh.
    fn show_step(&mut self, id: &'static str, ctx: &egui::Context) {
        self.current = id;
        model::memory().current = id.to_string();
        self.looking = false;
        self.hosted = match model::STEPS[model::index_of(id)].kind {
            Kind::Page(page) => Some(host_page(page, &self.server, &self.env, ctx)),
            Kind::Aircraft | Kind::Finish => None,
        };
    }

    /// `go`: to the step `delta` away, marking this one done when `completed`.
    fn go(&mut self, delta: i32, completed: bool, ctx: &egui::Context) {
        if completed {
            model::memory().visited.insert(self.current);
        }
        if let Some(next) = model::step_at(self.current, delta) {
            self.show_step(model::STEPS[next].id, ctx);
            self.refresh(ctx);
        }
    }

    fn states(&self) -> [State; 8] {
        let visited = model::memory().visited.clone();
        model::step_states(self.payload.as_ref(), &visited, self.bootloader)
    }

    fn take_answers(&mut self, ctx: &egui::Context, state: &Value, now: f64) {
        self.armed = state.get("armed") == Some(&Value::Bool(true));
        self.bootloader = !model::bootloader_device(state).is_empty();
        let connected = state.get("connected") == Some(&Value::Bool(true));
        if self.reboot.follow(connected, now) {
            self.written = false;
            self.refresh(ctx);
        }
        if let Some(r) = self.reading.as_ref().and_then(Pending::take) {
            self.reading = None;
            if let Ok(v) = &r {
                self.payload = Some(Payload::from_json(v));
            }
            if self.looking {
                let step = model::initial_step(self.payload.as_ref().filter(|_| r.is_ok()), self.bootloader);
                self.show_step(step, ctx);
            }
        }
        if let Some(r) = self.write_req.as_ref().and_then(Pending::take) {
            self.write_req = None;
            match r {
                Ok(v) => {
                    self.written = true;
                    let (level, text) = model::written_note(&v);
                    if level == "warning" {
                        self.warnings.push(text);
                    } else {
                        self.notes.push(text);
                    }
                }
                Err(e) => self.errors.push(format!(
                    "Could not set the aircraft type: {}",
                    if e.is_empty() { "no answer" } else { &e }
                )),
            }
        }
        if let Some(r) = self.reboot_req.as_ref().and_then(Pending::take) {
            self.reboot_req = None;
            match r {
                Ok(_) => {
                    self.notes
                        .push("The autopilot is rebooting. The link returns in a few seconds.".into());
                    self.reboot = Reboot::Waiting { went_down: false };
                }
                Err(e) => self.errors.push(format!(
                    "Reboot refused: {}",
                    if e.is_empty() { "no answer" } else { &e }
                )),
            }
        }
    }

    fn write(&mut self, ctx: &egui::Context) {
        let Some(id) = self.chosen else {
            return;
        };
        if self.write_req.is_some() {
            return;
        }
        let s = self.server.clone();
        let body = model::airframe_body(id);
        self.write_req = Some(Pending::spawn(ctx, move || {
            post(&s, "/api/first-setup/airframe", &body)
        }));
    }

    fn start_reboot(&mut self, ctx: &egui::Context) {
        if self.reboot_req.is_some() {
            return;
        }
        let s = self.server.clone();
        self.reboot_req = Some(Pending::spawn(ctx, move || {
            post(&s, "/api/mavlink/reboot", &serde_json::json!({}))
        }));
    }

    /// Draws the page from `y`; returns the bottom.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        state: &Value,
        now: f64,
        units: &Units,
        checklist: &mut Checklist,
    ) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx, state, now);
        let mut y = pagekit::header(ui, x, y, "First Setup", "A new controller, set up step by step") + AFTER_HEAD;
        let states = self.states();
        y = self.stepper(ui, &ctx, x, y, w, &states) + AFTER_STEPS;
        y = self.bar(ui, &ctx, x, y, w) + AFTER_BAR;
        y = self.host(ui, &ctx, x, y, w, state, now, units, checklist, &states);
        self.dialogs(&ctx);
        y
    }

    /// `.fs-steps`: the numbered pills, wrapping.
    fn stepper(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, states: &[State; 8]) -> f32 {
        let (mut px, mut py) = (x, y);
        let h = 7.0 * 2.0 + 18.0 + 2.0;
        let mut go = None;
        for (i, step) in model::STEPS.iter().enumerate() {
            let active = step.id == self.current;
            let st = states[i];
            let lg = style::galley(ui, step.label, 11.5, Weight::W600, style::text_3(), 0.0);
            let pw = 2.0 + 24.0 + 18.0 + 8.0 + lg.size().x;
            if px > x && px + pw > x + w {
                px = x;
                py += h + 6.0;
            }
            let rect = Rect::from_min_size(Pos2::new(px, py), Vec2::new(pw, h));
            let resp = ui.interact(rect, ui.scope_id().with(("fsstep", i)), egui::Sense::click());
            let (border, color) = if active {
                (style::accent(), style::text_1())
            } else if st == State::Todo {
                (style::warning(), style::text_3())
            } else if st == State::Done {
                (style::border_soft(), style::text_2())
            } else {
                (style::border_soft(), style::text_3())
            };
            let p = ui.painter();
            p.rect_stroke(rect, 255, Stroke::new(1.0, border), egui::StrokeKind::Inside);
            let (num_bg, num_fg) = if active {
                (style::accent(), style::text_inverted())
            } else if st == State::Done {
                (style::accent_dim(), style::accent_bright())
            } else {
                (style::tint(0.08), style::text_2())
            };
            let c = Pos2::new(rect.left() + 1.0 + 12.0 + 9.0, rect.center().y);
            p.circle_filled(c, 9.0, num_bg);
            if st == State::Done {
                icons::paint(ui, "check", Rect::from_center_size(c, Vec2::splat(11.0)), num_fg);
            } else {
                let ng = style::galley(ui, &(i + 1).to_string(), 10.0, Weight::W400, num_fg, 0.0);
                ui.painter().galley(c - ng.size() / 2.0, ng, num_fg);
            }
            let lg = style::galley(ui, step.label, 11.5, Weight::W600, color, 0.0);
            ui.painter().galley(
                Pos2::new(c.x + 9.0 + 8.0, rect.center().y - lg.size().y / 2.0),
                lg,
                color,
            );
            if resp.clicked() {
                go = Some(step.id);
            }
            px += pw + 6.0;
        }
        if let Some(id) = go {
            self.show_step(id, ctx);
        }
        py + h
    }

    /// `.fs-bar`: what this step is and the way on.
    fn bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let i = model::index_of(self.current);
        let step = &model::STEPS[i];
        let last = i == model::STEPS.len() - 1;
        let prev = Button::new("Previous", Variant::Ghost)
            .icon("chevron-left")
            .enabled(i > 0);
        let skip = Button::new("Skip", Variant::Ghost);
        let next = Button::new("Next", Variant::Primary);
        let mut widths = vec![prev.width(ui)];
        if !last {
            widths.push(skip.width(ui));
            widths.push(next.width(ui));
        }
        let actions_w: f32 = widths.iter().sum::<f32>() + 8.0 * (widths.len() as f32 - 1.0);
        let bh = prev.height();
        let single = 320.0 + 24.0 + actions_w <= iw;
        let text_w = if single { iw - 24.0 - actions_w } else { iw };
        let tg = style::galley(ui, step.title, 13.0, Weight::W700, style::text_1(), 0.0);
        let bg_text = kit::wrapped_weight(ui, step.text, 12.0, Weight::W400, style::text_2(), text_w, 1.5);
        let text_h = tg.size().y + 4.0 + bg_text.size().y;
        let top = y + 19.0;
        let (text_y, actions_y, bottom) = if single {
            let row = text_h.max(bh);
            (top + (row - text_h) / 2.0, top + (row - bh) / 2.0, top + row)
        } else {
            (top, top + text_h + 12.0, top + text_h + 12.0 + bh)
        };
        let th = tg.size().y;
        painter.galley(Pos2::new(ix, text_y), tg, style::text_1());
        painter.galley(Pos2::new(ix, text_y + th + 4.0), bg_text, style::text_2());
        let ax = if single { ix + iw - actions_w } else { ix };
        let mut cx = ax;
        let mut delta = None;
        let mut buttons: Vec<(Button<'_>, i32, bool)> = vec![(prev, -1, false)];
        if !last {
            buttons.push((skip, 1, false));
            buttons.push((next, 1, true));
        }
        for (k, (b, d, completed)) in buttons.into_iter().enumerate() {
            let bw = widths[k];
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(cx, actions_y), Vec2::new(bw, bh)),
                ("fsbar", k),
            ) {
                delta = Some((d, completed));
            }
            cx += bw + 8.0;
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, bottom + 19.0)),
            card::Look::Page,
        );
        if let Some((d, completed)) = delta {
            self.go(d, completed, ctx);
        }
        bottom + 19.0
    }

    #[allow(clippy::too_many_arguments)]
    fn host(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        state: &Value,
        now: f64,
        units: &Units,
        checklist: &mut Checklist,
        states: &[State; 8],
    ) -> f32 {
        let kind = model::STEPS[model::index_of(self.current)].kind;
        match kind {
            Kind::Aircraft => self.aircraft(ui, ctx, x, y, w),
            Kind::Finish => self.finish(ui, ctx, x, y, w, states),
            Kind::Page(_) => {
                if self.looking || self.hosted.is_none() {
                    return Self::note(ui, x, y, "Looking at the controller\u{2026}");
                }
                // the page's own back button leads out of the wizard, so it is
                // not drawn; a view inside the page (a wizard, a sensor) still
                // needs the way back to the page itself
                let (inside, label) = match &self.hosted {
                    Some(Hosted::Calibration(p)) if p.in_wizard() => (true, "Calibration"),
                    Some(Hosted::Radio(p)) if p.in_wizard() => (true, "Radio Control"),
                    Some(Hosted::Safety(p)) if p.in_sensor() => (true, "Safety & Sensors"),
                    _ => (false, ""),
                };
                let mut y = y;
                if inside {
                    let (bh, back) = pagekit::back_button(ui, Pos2::new(x, y), label);
                    y += bh + 22.0;
                    if back {
                        match &mut self.hosted {
                            Some(Hosted::Calibration(p)) => p.close_wizard(),
                            Some(Hosted::Radio(p)) => p.close_wizard(),
                            Some(Hosted::Safety(p)) => p.close_sensor(),
                            _ => {}
                        }
                    }
                }
                match &mut self.hosted {
                    Some(Hosted::Firmware(p)) => p.show(ui, x, y, w, state, now),
                    Some(Hosted::Motors(p)) => p.show(ui, x, y, w, state, now),
                    Some(Hosted::Calibration(p)) => p.show(ui, x, y, w, state, now),
                    Some(Hosted::Radio(p)) => p.show(ui, x, y, w, state, now),
                    Some(Hosted::Safety(p)) => p.show(ui, x, y, w, state, checklist),
                    Some(Hosted::Battery(p)) => p.show(ui, x, y, w, state, units),
                    None => y,
                }
            }
        }
    }

    /// `.field-hint`: 10.5 px quiet text; returns the bottom.
    fn note(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
        let g = style::galley(ui, text, 10.5, Weight::W400, style::text_3(), 0.0);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(x, y), g, style::text_3());
        y + h
    }

    /// `buildAircraft`.
    fn aircraft(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let view = model::aircraft_view(
            self.payload.as_ref(),
            self.armed,
            self.written,
            self.reboot.active(),
            self.chosen,
        );
        let mut cy = y + 19.0;
        // the status
        let sg = style::galley(ui, &view.status, 13.0, Weight::W600, style::text_1(), 0.0);
        let sh = sg.size().y;
        painter.galley(Pos2::new(ix, cy), sg, style::text_1());
        cy += sh + 14.0;
        // the picker
        let presets = self.payload.as_ref().map(|p| p.presets.clone()).unwrap_or_default();
        let cards: Vec<Card> = presets
            .iter()
            .map(|p| Card::new(p.id.to_string(), p.label.clone(), p.group.clone()))
            .collect();
        let current = self.payload.as_ref().and_then(|p| p.preset_current);
        let selected = self.chosen.or(current).map(|v| v.to_string());
        let (bottom, pressed) = optioncards::show(ui, ix, cy, iw, &cards, selected.as_deref(), 3, "fsaircraft");
        if let Some(id) = pressed {
            self.chosen = id.parse().ok();
        }
        cy = bottom + 14.0;
        // the note
        let ng = kit::wrapped_weight(ui, &view.note, 10.5, Weight::W400, style::text_3(), iw, 1.4);
        let nh = ng.size().y;
        painter.galley(Pos2::new(ix, cy), ng, style::text_3());
        cy += nh + 14.0;
        // the buttons
        let mut apply = false;
        let mut reboot = false;
        let mut ax = ix;
        let mut row_h: f32 = 0.0;
        if view.show_apply {
            let b = Button::new("Use this aircraft type", Variant::Primary)
                .icon("check")
                .enabled(view.apply_enabled && self.write_req.is_none());
            let (bw, bh) = (b.width(ui), b.height());
            apply = b.show(ui, Rect::from_min_size(Pos2::new(ax, cy), Vec2::new(bw, bh)), "fsapply");
            ax += bw + 8.0;
            row_h = row_h.max(bh);
        }
        if view.show_reboot {
            let b = Button::new("Reboot autopilot", Variant::Secondary)
                .icon("power")
                .enabled(view.reboot_enabled && self.reboot_req.is_none());
            let (bw, bh) = (b.width(ui), b.height());
            reboot = b.show(
                ui,
                Rect::from_min_size(Pos2::new(ax, cy), Vec2::new(bw, bh)),
                "fsreboot",
            );
            row_h = row_h.max(bh);
        }
        cy += row_h;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        if apply {
            // an aircraft that already has a type asks before it changes
            if self.payload.as_ref().and_then(|p| p.preset_current).is_none() {
                self.write(ctx);
            } else {
                self.confirm = true;
            }
        }
        if reboot {
            self.reboot_dialog = true;
        }
        cy + 19.0
    }

    /// `buildFinish`.
    fn finish(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, states: &[State; 8]) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let (rows, open) = model::summary(states);
        let mut cy = y + 19.0;
        let mut open_step = None;
        let n = rows.len();
        for (i, row) in rows.iter().enumerate() {
            let btn = Button::new(row.action, Variant::Ghost).small();
            let bw = btn.width(ui);
            let bh = SMALL_H;
            let content = bh;
            let rh = 6.0 * 2.0 + content;
            let mid = cy + 6.0 + content / 2.0;
            let (icon, color) = match row.state {
                State::Done => ("circle-check", style::healthy()),
                State::Todo => ("circle-alert", style::warning()),
                State::Pending => ("circle", style::text_3()),
            };
            icons::paint(
                ui,
                icon,
                Rect::from_center_size(Pos2::new(ix + 4.0 + 11.0, mid), Vec2::splat(16.0)),
                color,
            );
            let ng = style::galley(ui, row.label, 13.0, Weight::W600, style::text_1(), 0.0);
            painter.galley(
                Pos2::new(ix + 4.0 + 22.0 + 10.0, mid - ng.size().y / 2.0),
                ng,
                style::text_1(),
            );
            let bx = ix + iw - 4.0 - bw;
            let wg = style::galley(ui, row.word, 11.5, Weight::W400, style::text_2(), 0.0);
            painter.galley(
                Pos2::new(bx - 10.0 - wg.size().x, mid - wg.size().y / 2.0),
                wg,
                style::text_2(),
            );
            if btn.show(
                ui,
                Rect::from_min_size(Pos2::new(bx, mid - bh / 2.0), Vec2::new(bw, bh)),
                ("fsrow", i),
            ) {
                open_step = Some(row.id);
            }
            cy += rh;
            if i + 1 < n {
                painter.hline(
                    egui::Rangef::new(ix, ix + iw),
                    cy + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                cy += 1.0 + 4.0;
            }
        }
        cy += 14.0;
        let hint = kit::wrapped_weight(
            ui,
            &model::finish_hint(open),
            10.5,
            Weight::W400,
            style::text_3(),
            iw,
            1.4,
        );
        let hh = hint.size().y;
        painter.galley(Pos2::new(ix, cy), hint, style::text_3());
        cy += hh + 14.0;
        let again = Button::new("Check again", Variant::Secondary).icon("refresh-cw");
        let done = Button::new("Finish setup", Variant::Primary).icon("circle-check");
        let (aw, dw, bh) = (again.width(ui), done.width(ui), done.height());
        let refresh = again.show(ui, Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(aw, bh)), "fsagain");
        let finish = done.show(
            ui,
            Rect::from_min_size(Pos2::new(ix + aw + 8.0, cy), Vec2::new(dw, bh)),
            "fsdone",
        );
        cy += bh;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        if let Some(id) = open_step {
            self.show_step(model::STEPS[model::index_of(id)].id, ctx);
        }
        if refresh {
            self.refresh(ctx);
        }
        if finish {
            self.finish_setup();
        }
        cy + 19.0
    }

    /// `finish`: this controller is not offered the wizard again, what was
    /// visited is forgotten, and the page is left.
    fn finish_setup(&mut self) {
        if let Some(p) = &self.payload {
            if !p.fingerprint.is_empty() {
                let mut quiet = model::Quiet::load(self.quiet_path.clone());
                quiet.silence(&p.fingerprint);
            }
        }
        model::memory().forget();
        self.notes.push("First setup finished.".into());
        self.finished = true;
    }

    /// The two questions the aircraft step asks.
    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.confirm {
            let question = model::change_question(self.payload.as_ref(), self.chosen.unwrap_or_default());
            let mut go = false;
            let closed = modal::show(
                ctx,
                "fsconfirm",
                modal::Size::Sm,
                "Change the aircraft type?",
                |ui, x, y, w| {
                    let g = kit::wrapped_weight(ui, &question, 12.0, Weight::W400, style::text_2(), w, 1.5);
                    ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
                    let (hit, bottom) = modal::actions(
                        ui,
                        x,
                        y + g.size().y + 18.0,
                        w,
                        vec![
                            Button::new("Cancel", Variant::Secondary),
                            Button::new("Change it", Variant::Primary),
                        ],
                    );
                    go = hit == Some(1);
                    bottom
                },
            );
            if closed || go {
                self.confirm = false;
            }
            if go {
                self.write(ctx);
            }
        }
        if self.reboot_dialog {
            let (closed, go) = params_dialogs::reboot(ctx);
            if closed {
                self.reboot_dialog = false;
            }
            if go {
                self.start_reboot(ctx);
            }
        }
    }
}

#[cfg(test)]
#[path = "firstsetup_flow.rs"]
mod flow;

fn post(s: &Server, path: &str, body: &Value) -> Result<Value, String> {
    let d = http::post_json(s, path, body)?;
    if d.get("ok") == Some(&Value::Bool(false)) {
        Err(d.get("error").and_then(Value::as_str).unwrap_or("").to_string())
    } else {
        Ok(d)
    }
}
