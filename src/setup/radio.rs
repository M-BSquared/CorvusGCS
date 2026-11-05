//! The Radio Control page of Setup (python/src/js/setup-control.js, the `.rc-*`
//! rules of setup.css).
//!
//! Two views behind one entry, the same shape as Calibration. The overview:
//! the drawn transmitter and the live channel bars, then the configuration the
//! backend describes in two tabs (Radio: input mode, stick channels, the
//! per channel calibration table; Flight modes: the mode switch and its six
//! slots, every other switch). The wizard: PX4 has no RC calibration, so the
//! whole procedure belongs to the ground station. It watches RC_CHANNELS while
//! the operator sweeps every control, then writes the endpoints that were
//! seen into `RC<n>_MIN`, `_MAX`, `_TRIM` and `_REV`.

use std::collections::BTreeMap;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::check::{self, Check};
use crate::setup::radio_model::{self as model, Doc, Measurement, Step, Tab, STICKS};
use crate::setup::rc_drawing::{Action, Info, MapField, Prompt, Transmitter};
use crate::setup::rc_transmitter::{self as tx, Glide, Store};
use crate::setup::schema_form::{Field, FormState, Look, RowHook};
use crate::ui::card;
use crate::ui::chip::Chip;
use crate::ui::icons;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

type Status = (&'static str, String);

/// How wide the page's column must be for the drawing and the bars to share a
/// row (`@container (min-width: 880px)`), and from where the bars lose their
/// notes (`max-width: 1180px`).
const ROW_MIN: f32 = 880.0;
const NOTES_MIN: f32 = 1180.0;

/// The two shares of the top row: the drawing takes 62 percent, the bars the
/// rest (no less than 280 px).
pub fn top_row_widths(w: f32) -> (f32, f32) {
    let free = w - 14.0;
    let bars = (free * 0.38).max(280.0);
    (free - bars, bars)
}

/// The channel bars' grid: label, bar, pulse and (wide enough) the note.
fn channel_columns(w: f32, column: f32) -> (f32, f32, f32, f32) {
    let inner = w - 8.0;
    let note = if column > NOTES_MIN { 110.0 } else { 0.0 };
    let gaps = if note > 0.0 { 30.0 } else { 20.0 };
    let bar = inner - 46.0 - 66.0 - note - gaps;
    (46.0, bar.max(40.0), 66.0, note)
}

struct Detect {
    field: Field,
    baseline: Vec<f64>,
    deadline: f64,
}

struct Wizard {
    step: Step,
    stick: usize,
    m: Measurement,
    tx: Transmitter,
    busy: Option<Pending<Result<Value, String>>>,
    status: Status,
    monitor: Monitor,
}

/// The live channel bars: each channel's value eased between samples.
#[derive(Default)]
struct Monitor {
    glide: Glide,
    count: usize,
}

enum Sub {
    Overview,
    Wizard(Box<Wizard>),
}

/// The page's state.
pub struct RadioPage {
    server: Server,
    doc: Option<Doc>,
    loading: Option<Pending<Result<Value, String>>>,
    status: Status,
    form: FormState,
    check: Check,
    check_card: Option<check::Summary>,
    check_say: Option<check::Summary>,
    fresh: bool,
    tab: Tab,
    tx: Transmitter,
    monitor: Monitor,
    detect: Option<Detect>,
    sub: Sub,
    latest: Vec<f64>,
    errors: Vec<String>,
    notes: Vec<String>,
    retried: bool,
    now: f64,
}

impl RadioPage {
    pub fn new(server: Server, ctx: &egui::Context) -> RadioPage {
        let mut p = RadioPage {
            server,
            doc: None,
            loading: None,
            status: ("pending", "Reading radio configuration\u{2026}".into()),
            form: FormState::default(),
            check: Check::default(),
            check_card: None,
            check_say: None,
            fresh: false,
            tab: Tab::Radio,
            tx: Transmitter::new(Store::load(Store::default_path())),
            monitor: Monitor::default(),
            detect: None,
            sub: Sub::Overview,
            latest: Vec::new(),
            errors: Vec::new(),
            notes: Vec::new(),
            retried: false,
            now: 0.0,
        };
        p.stream(ctx, true);
        p.load(ctx);
        p
    }

    /// Raises the channel rate for as long as the page is open. Best effort
    /// in both directions: a firmware that refuses streams at its default,
    /// which makes the bars coarser and nothing worse.
    fn stream(&self, ctx: &egui::Context, on: bool) {
        let s = self.server.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = http::post_json(
                &s,
                "/api/rc/stream",
                &json!({"enabled": on, "rate_hz": model::STREAM_HZ}),
            );
            ctx.request_repaint();
        });
    }

    pub fn in_wizard(&self) -> bool {
        matches!(self.sub, Sub::Wizard(_))
    }

    pub fn close_wizard(&mut self) {
        self.sub = Sub::Overview;
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        e.extend(self.form.take_errors());
        e
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn load(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        self.status = ("pending", "Reading radio configuration\u{2026}".into());
        let s = self.server.clone();
        let path = if std::mem::take(&mut self.fresh) {
            "/api/rc?fresh=1"
        } else {
            "/api/rc"
        };
        self.loading = Some(Pending::spawn(ctx, move || http::get_json(&s, path)));
    }

    /// Feeds the transmitter what the vehicle says about the radio.
    fn feed_transmitter(tx: &mut Transmitter, doc: &Doc) {
        let mut sticks = BTreeMap::new();
        for role in tx::ROLES {
            if let Some(ch) = doc.assignments.get(tx::role_param(role)).filter(|c| **c >= 1) {
                sticks.insert(role.to_string(), *ch);
            }
        }
        let mut info = BTreeMap::new();
        for s in &doc.sections {
            for r in s.rows.iter().filter(|r| r.calibrated) {
                // only a calibrated channel may set the scale its switch
                // positions are read against: PX4's untouched 1000 and 2000
                // would put a three position switch's middle slot wherever the
                // receiver's idle sits
                info.insert(
                    r.channel,
                    Info {
                        min: r.min,
                        max: r.max,
                        reversed: r.reversed,
                    },
                );
            }
        }
        tx.set_doc(sticks, info);
    }

    fn take_answers(&mut self, ctx: &egui::Context, now: f64) {
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
                                "No radio configuration received".into()
                            } else {
                                doc.error.clone()
                            },
                        )
                    };
                    Self::feed_transmitter(&mut self.tx, &doc);
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
                            "Could not read radio configuration".into()
                        } else {
                            e
                        },
                    );
                }
            }
        }
        if let Some(doc) = &mut self.doc {
            for s in &mut doc.sections {
                self.form.take_answers(&mut s.fields);
            }
            self.form.take_answers(&mut doc.table);
        }
        // a mapping write changes what every other row means, so the page
        // reads again rather than patching
        if std::mem::take(&mut self.form.written) && self.doc.is_some() {
            self.load(ctx);
        }
        if let Some(d) = &self.detect {
            if now > d.deadline {
                let p = d.field.param.clone();
                self.detect = None;
                self.form.set_status(&p, "err", "nothing moved".into());
            }
        }
    }

    /// The channel the moved control is on, when one moved.
    fn sample_detect(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.detect else { return };
        let Some(ch) = tx::moved_channel(&d.baseline, &self.latest) else {
            return;
        };
        let field = d.field.clone();
        self.detect = None;
        self.form.set_status(&field.param, "ok", format!("channel {ch}"));
        let server = self.server.clone();
        self.form.write_value(ctx, &server, &field, ch as f64);
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        let now_ms = now;
        self.now = now_ms;
        self.latest = state
            .get("rc_channels")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
            .unwrap_or_default();
        self.take_answers(&ctx, now_ms);
        self.tx.update(state, now_ms);
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let connected = state.get("connected") == Some(&Value::Bool(true));
        if !connected {
            self.retried = false;
        } else if !self.retried
            && self.loading.is_none()
            && !self.check.busy()
            && self.doc.as_ref().is_some_and(|d| !d.connected)
        {
            self.retried = true;
            self.load(&ctx);
        }
        self.sample_detect(&ctx);
        if matches!(self.sub, Sub::Wizard(_)) {
            self.wizard(ui, &ctx, x, y, w, state, armed, connected, now_ms)
        } else {
            self.overview(ui, &ctx, x, y, w, state, armed, now_ms)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn overview(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        state: &Value,
        armed: bool,
        now: f64,
    ) -> f32 {
        let idle = self.loading.is_none() && !self.check.busy();
        let can_check = idle
            && self
                .doc
                .as_ref()
                .is_some_and(|d| d.connected && !d.check_names().is_empty());
        let (mut y, pressed) = pagekit::head_actions(
            ui,
            x,
            y,
            w,
            "Radio Control",
            "Transmitter calibration, channel assignment and switch mapping",
            &self.status,
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
            "radiohead",
        );
        y -= 4.0;
        match pressed {
            Some(0) => {
                self.check_card = None;
                self.load(ctx);
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
                    self.check.start(ctx, &self.server, &wanted, names);
                }
            }
            _ => {}
        }
        if armed {
            y = pagekit::warning_banner(ui, x, y, w, "Radio configuration is read-only while armed") + 14.0;
        }
        let doc = self.doc.clone().unwrap_or_default();
        let labels = model::assignment_labels(&doc);
        let fields: Vec<MapField> = doc
            .channel_fields()
            .into_iter()
            .map(|f| MapField {
                param: f.param.clone(),
                label: f.label.clone(),
                value: f.value,
            })
            .collect();

        // the drawing and the bars
        let server = self.server.clone();
        let (txw, monw) = if w >= ROW_MIN { top_row_widths(w) } else { (w, w) };
        let mut opened = false;
        let action;
        let tx_bottom = {
            let painter = ui.painter().clone();
            let bg = card::reserve(&painter);
            let (ix, iw) = (x + 19.0, txw - 38.0);
            let mut cy = y + 19.0;
            self.tx_head(ui, "Transmitter", ix, cy, iw, true);
            cy += 42.0 + 10.0;
            cy += self.tx.figure(ui, ix, cy, iw, true) + 10.0;
            let b = Button::new("Calibrate radio", Variant::Primary)
                .icon("radio")
                .small()
                .enabled(!armed);
            let (bw, bh) = (b.width(ui), b.height());
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(ix + (iw - bw) / 2.0, cy + 2.0), Vec2::new(bw, bh)),
                "radiocalibrate",
            ) {
                opened = true;
            }
            cy += 2.0 + bh + 4.0 + 10.0;
            let (b, a) = self.tx.inspector(ui, ix, cy, iw, &fields);
            action = a;
            cy = b;
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + txw, cy + 19.0)),
                card::Look::Page,
            );
            cy + 19.0 + 14.0
        };
        let (mx, my) = if w >= ROW_MIN {
            (x + txw + 14.0, y)
        } else {
            (x, tx_bottom)
        };
        let mon_bottom = self.monitor_card(ui, mx, my, monw, "Live channels", &labels, None, x, w, state, now) + 14.0;
        y = if w >= ROW_MIN {
            tx_bottom.max(mon_bottom)
        } else {
            mon_bottom
        };
        if let Some(Action::Map { param, channel }) = action {
            if !armed {
                if let Some(f) = doc.channel_fields().into_iter().find(|f| f.param == param) {
                    let f = f.clone();
                    self.form.write_value(ctx, &server, &f, f64::from(channel));
                }
            }
        }
        if opened {
            self.tx.leave();
            self.sub = Sub::Wizard(Box::new(Wizard {
                step: Step::Intro,
                stick: 0,
                m: Measurement::default(),
                tx: Transmitter::new(Store::load(Store::default_path())),
                busy: None,
                status: ("", String::new()),
                monitor: Monitor::default(),
            }));
            if let Sub::Wizard(wz) = &mut self.sub {
                Self::feed_transmitter(&mut wz.tx, &doc);
            }
            return y;
        }

        // the tabs
        let (ty, hit) = pagekit::tabs(
            ui,
            x,
            y,
            w,
            &model::TABS.iter().map(|t| t.1).collect::<Vec<_>>(),
            model::TABS.iter().position(|t| t.0 == self.tab).unwrap_or(0),
            "radiotabs",
        );
        y = ty;
        if let Some(i) = hit {
            let t = model::TABS[i].0;
            if t != self.tab {
                self.tab = t;
                self.cancel_detect("cancelled");
            }
        }
        y = self.sections(ui, ctx, &doc, x, y, w, armed);
        y - 14.0
    }

    fn cancel_detect(&mut self, reason: &str) {
        if let Some(d) = self.detect.take() {
            self.form.set_status(&d.field.param, "err", reason.into());
        }
    }

    /// `.rc-monitor-head` of the transmitter card: its title, and the stick
    /// mode with Forget controls at the right (`interactive` only).
    fn tx_head(&mut self, ui: &mut egui::Ui, title: &str, x: f32, y: f32, w: f32, interactive: bool) {
        let g = style::galley(ui, &title.to_uppercase(), 13.0, Weight::W600, style::text_2(), 0.6);
        let th = g.size().y + 9.0;
        let ty = y + (42.0 - (th + 12.0)) / 2.0;
        let rule_w = g.size().x;
        ui.painter().galley(Pos2::new(x, ty + 1.0), g, style::text_2());
        ui.painter().hline(
            egui::Rangef::new(x, x + rule_w),
            ty + th + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        if !interactive {
            return;
        }
        let forget = Button::new("Forget controls", Variant::Ghost)
            .icon("eraser")
            .small()
            .title("Drop every control this station learned, for a new transmitter");
        let (fw, fh) = (forget.width(ui), forget.height());
        let fx = x + w - fw;
        if forget.show(
            ui,
            Rect::from_min_size(Pos2::new(fx, y + (42.0 - fh) / 2.0), Vec2::new(fw, fh)),
            "radioforget",
        ) {
            self.tx.forget_all();
        }
        let sel_w = 110.0;
        let sel_h = 26.5;
        let choices: Vec<Choice> = (1..=4)
            .map(|m| Choice::new(m.to_string(), format!("Mode {m}")))
            .collect();
        let rect = Rect::from_min_size(
            Pos2::new(fx - 8.0 - sel_w, y + (42.0 - sel_h) / 2.0),
            Vec2::new(sel_w, sel_h),
        );
        let cur = self.tx.mode().to_string();
        if let Some(v) = kit::select_padded(
            ui,
            rect,
            &choices,
            &cur,
            "",
            false,
            "Which thumb carries throttle",
            "radiomode",
            (8.0, 24.0),
        ) {
            if let Ok(m) = v.parse::<u8>() {
                self.tx.set_mode(m);
            }
        }
    }

    /// The channel bars card (`.rc-monitor`); returns its bottom.
    #[allow(clippy::too_many_arguments)]
    fn monitor_card(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        title: &str,
        labels: &BTreeMap<u32, String>,
        highlight: Option<u32>,
        column_x: f32,
        column_w: f32,
        state: &Value,
        now: f64,
    ) -> f32 {
        let _ = column_x;
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let cy = y + 19.0;
        let monitor = &mut self.monitor;
        let bottom = draw_monitor(ui, monitor, ix, cy, iw, title, labels, highlight, column_w, state, now);
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, bottom + 19.0)),
            card::Look::Page,
        );
        bottom + 19.0
    }

    /// The sections of the active tab, as cards.
    #[allow(clippy::too_many_arguments)]
    fn sections(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        x: f32,
        mut y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        let (ix, iw) = (x + 19.0, w - 38.0);
        let server = self.server.clone();
        if let Some(c) = self.check_card.clone() {
            y = check::card(ui, x, y, w, &c) + 26.0 - 14.0;
            y += 14.0;
        }
        if doc.sections.is_empty() {
            let bg = card::reserve(&painter);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Radio Control") + 10.0;
            let g = kit::wrapped_weight(
                ui,
                "Connect to a vehicle to read its transmitter configuration. The page shows only what the connected firmware actually reports, so a mapping a firmware does not have is one row fewer rather than an error.",
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
            return cy + 19.0 + 26.0;
        }
        let dups = model::duplicate_channels(&doc.assignments);
        let listed: Vec<&model::Section> = doc
            .sections
            .iter()
            .filter(|s| model::tab_of(&s.id) == self.tab)
            .collect();
        if listed.is_empty() {
            let bg = card::reserve(&painter);
            let label = model::TABS.iter().find(|t| t.0 == self.tab).map_or("Radio", |t| t.1);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, label) + 10.0;
            let g = kit::wrapped_weight(
                ui,
                "The connected firmware reports no parameters for this tab.",
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
            y = cy + 19.0 + 26.0;
        }
        let mut begin_detect: Option<Field> = None;
        let busy_detect = self.detect.is_some();
        for s in listed {
            let bg = card::reserve(&painter);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, &s.title) + 10.0;
            if !s.hint.is_empty() {
                let g = kit::wrapped_weight(ui, &s.hint, 10.5, Weight::W400, style::text_3(), iw, 1.4);
                painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
                cy += g.size().y + 10.0;
            }
            if s.channels {
                cy = self.channel_table(ui, ctx, doc, s, ix, cy, iw, armed);
            } else {
                // a Detect button beside every channel picker
                let roles: Vec<(String, bool)> = s
                    .fields
                    .iter()
                    .zip(&s.roles)
                    .map(|(f, r)| (f.param.clone(), r == "channel"))
                    .collect();
                let is_channel = |p: &str| roles.iter().any(|(n, c)| n == p && *c);
                let mut after = |ui: &mut egui::Ui, f: &Field, ux: f32, top: f32, h: f32| -> f32 {
                    if !is_channel(&f.param) {
                        return ux;
                    }
                    let title = format!("Detect the channel for {}", f.label);
                    let mut b = Button::new("Detect", Variant::Ghost)
                        .icon("crosshair")
                        .small()
                        .title(&title)
                        .enabled(!armed && !busy_detect);
                    b.pad = Some((5.0, 9.0));
                    let (bw, bh) = (b.width(ui), b.height());
                    if b.show(
                        ui,
                        Rect::from_min_size(Pos2::new(ux, top + (h - bh) / 2.0), Vec2::new(bw, bh)),
                        ("radiodetect", &f.param),
                    ) {
                        begin_detect = Some(f.clone());
                    }
                    ux + bw + 8.0
                };
                let channel_of = |f: &Field| f.value as u32;
                let warn = |f: &Field| is_channel(&f.param) && f.value >= 1.0 && dups.contains(&channel_of(f));
                let mut hook = RowHook {
                    after: Some(&mut after),
                    warn: Some(&warn),
                };
                cy = self
                    .form
                    .show_hooked(ui, ctx, &server, &s.fields, ix, cy, iw, armed, Look::SAFETY, &mut hook);
                if s.id == "modes" {
                    cy = self.mode_strip(ui, s, ix, cy + 4.0, iw);
                }
            }
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
                card::Look::Page,
            );
            y = cy + 19.0 + 26.0;
        }
        if let Some(f) = begin_detect {
            self.start_detect(f);
        }
        if !dups.is_empty() {
            let g = kit::wrapped_weight(
                ui,
                &model::conflict_text(&dups),
                11.5,
                Weight::W400,
                style::critical_bright(),
                w - 24.0 - 23.0,
                1.5,
            );
            let r = Rect::from_min_size(Pos2::new(x, y - 14.0), Vec2::new(w, g.size().y + 20.0));
            painter.rect_filled(r, 10, style::critical_dark());
            painter.rect_stroke(r, 10, Stroke::new(1.0, style::critical()), egui::StrokeKind::Inside);
            icons::paint(
                ui,
                "triangle-alert",
                Rect::from_min_size(Pos2::new(r.left() + 13.0, r.top() + 10.0), Vec2::splat(15.0)),
                style::critical_bright(),
            );
            painter.galley(
                Pos2::new(r.left() + 13.0 + 15.0 + 8.0, r.top() + 10.0),
                g,
                style::critical_bright(),
            );
            y = r.bottom() + 14.0;
        }
        y
    }

    fn start_detect(&mut self, f: Field) {
        if self.detect.is_some() {
            return;
        }
        if self.latest.is_empty() || self.latest.iter().all(|v| *v <= 0.0) {
            self.form.set_status(&f.param, "err", "no RC signal".into());
            return;
        }
        self.form.set_status(&f.param, "pending", "move it now\u{2026}".into());
        self.detect = Some(Detect {
            field: f,
            baseline: self.latest.clone(),
            deadline: self.now + model::DETECT_TIMEOUT_MS,
        });
    }
}

/// A title with its rule under the words only, as a flex row item has it
/// (`.rc-monitor-head`): returns the height of the title with its margin.
fn short_title(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 13.0, Weight::W600, style::text_2(), 0.6);
    let (gw, gh) = (g.size().x, g.size().y);
    ui.painter().galley(Pos2::new(x, y + 1.0), g, style::text_2());
    ui.painter().hline(
        egui::Rangef::new(x, x + gw),
        y + gh + 9.5,
        Stroke::new(1.0, style::border_soft()),
    );
    gh + 9.0 + 12.0
}

/// The channel bars (`channelMonitor`): a head with the signal chip, then one
/// bar per channel the receiver delivers, eased between samples. Returns the
/// bottom.
#[allow(clippy::too_many_arguments)]
fn draw_monitor(
    ui: &mut egui::Ui,
    m: &mut Monitor,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    labels: &BTreeMap<u32, String>,
    highlight: Option<u32>,
    column: f32,
    state: &Value,
    now: f64,
) -> f32 {
    let painter = ui.painter().clone();
    let channels: Vec<f64> = state
        .get("rc_channels")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
        .unwrap_or_default();
    let live = state.get("rc_live") == Some(&Value::Bool(true)) && !channels.is_empty();
    let head_h = short_title(ui, x, y, title);
    let rssi = state.get("rc_rssi").and_then(Value::as_f64).unwrap_or(-1.0);
    let text = if live {
        if rssi >= 0.0 {
            format!(
                "{} channels \u{b7} RSSI {}%",
                channels.len(),
                crate::data::units::js_number(rssi)
            )
        } else {
            format!("{} channels", channels.len())
        }
    } else {
        "No signal".to_string()
    };
    let chip = Chip {
        pad: (4.0, 10.0, 4.0, 8.0),
        fg: if live { style::text_1() } else { style::text_2() },
        dot: Some((if live { style::healthy() } else { style::critical() }, true)),
        ..Chip::new(text)
    };
    let cs = chip.measure(ui);
    chip.paint(ui, Pos2::new(x + w - cs.x, y + (head_h - cs.y) / 2.0));
    let top = y + head_h + 10.0;
    if !live {
        let g = kit::wrapped_weight(
            ui,
            "No channel data. Switch the transmitter on and bind the receiver. The bars appear as soon as RC_CHANNELS arrives.",
            11.5,
            Weight::W400,
            style::text_3(),
            w,
            1.5,
        );
        painter.galley(Pos2::new(x, top), g.clone(), style::text_3());
        return top + g.size().y;
    }
    for (i, v) in channels.iter().enumerate() {
        m.glide
            .set(&(i + 1).to_string(), (*v > 0.0).then(|| tx::pwm_fraction(*v)), now);
    }
    for k in channels.len() + 1..=m.count.max(channels.len()) {
        m.glide.drop_key(&k.to_string());
    }
    m.count = channels.len();
    m.glide.commit(now);
    let (lw, bw, vw, nw) = channel_columns(w, column);
    let mut row_y = top;
    for (i, v) in channels.iter().enumerate() {
        let ch = i as u32 + 1;
        let row = Rect::from_min_size(Pos2::new(x, row_y), Vec2::new(w, 17.5));
        if highlight == Some(ch) {
            painter.rect_filled(row, 6, style::surface_2());
        }
        let mut cx = x + 4.0;
        let g = style::galley_mono(ui, &format!("CH {ch}"), 10.5, style::text_3());
        painter.galley(Pos2::new(cx, row.center().y - g.size().y / 2.0), g, style::text_3());
        cx += lw + 10.0;
        let track = Rect::from_min_size(Pos2::new(cx, row.center().y - 5.0), Vec2::new(bw, 10.0));
        painter.rect_filled(track, 5, style::surface_2());
        let f = m.glide.get(&ch.to_string(), now).unwrap_or(0.0) as f32;
        let inner = track.shrink(1.0);
        if f > 0.0 {
            painter.with_clip_rect(inner).rect_filled(
                Rect::from_min_size(inner.min, Vec2::new(inner.width() * f, inner.height())),
                0,
                style::nav(),
            );
        }
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(track.center().x - 0.5, track.top() + 1.0),
                Vec2::new(1.0, 8.0),
            ),
            0,
            style::border_strong(),
        );
        painter.rect_stroke(
            track,
            5,
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        cx += bw + 10.0;
        let txt = if *v > 0.0 {
            format!("{} \u{b5}s", crate::data::units::js_number(*v))
        } else {
            "\u{2014}".to_string()
        };
        let g = style::galley_mono(ui, &txt, 10.5, style::text_2());
        painter.galley(
            Pos2::new(cx + vw - g.size().x, row.center().y - g.size().y / 2.0),
            g,
            style::text_2(),
        );
        if nw > 0.0 {
            if let Some(note) = labels.get(&ch) {
                let g = style::galley(ui, note, 10.5, Weight::W400, style::text_2(), 0.0);
                let clip = Rect::from_min_size(Pos2::new(cx + vw + 10.0, row.top()), Vec2::new(nw, row.height()));
                ui.painter_at(clip).galley(
                    Pos2::new(clip.left(), row.center().y - g.size().y / 2.0),
                    g,
                    style::text_2(),
                );
            }
        }
        row_y += 17.5 + 3.0;
    }
    if m.glide.moving(now) {
        ui.ctx().request_repaint();
    }
    row_y - 3.0
}

impl RadioPage {
    /// The six flight mode positions with the live one lit: the only place the
    /// operator can check the slot order without taking off.
    fn mode_strip(&mut self, ui: &mut egui::Ui, s: &model::Section, x: f32, y: f32, w: f32) -> f32 {
        let slots: Vec<&Field> = s
            .fields
            .iter()
            .zip(&s.roles)
            .filter(|(_, r)| r.as_str() == "mode")
            .map(|(f, _)| f)
            .collect();
        if slots.is_empty() {
            return y;
        }
        let map = s
            .fields
            .iter()
            .find(|f| f.param == "RC_MAP_FLTMODE" || f.param == "FLTMODE_CH");
        let channel = map.map_or(0, |f| f.value as usize);
        let ardupilot = map.is_some_and(|f| f.param == "FLTMODE_CH");
        let pulse = if channel > 0 {
            self.latest.get(channel - 1).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        let active = if pulse > 0.0 {
            model::active_slot(pulse, ardupilot)
        } else {
            None
        };
        let cols = if crate::app::responsive::Vw::of(ui.ctx()).max720 {
            3
        } else {
            6
        };
        let cw = (w - 6.0 * (cols as f32 - 1.0)) / cols as f32;
        let painter = ui.painter().clone();
        let slot_h = 2.0 + 16.0 + 10.0 * 1.21 + 3.0 + 11.5 * 1.21;
        let mut bottom = y;
        for (i, f) in slots.iter().enumerate() {
            let (col, row) = (i % cols, i / cols);
            let r = Rect::from_min_size(
                Pos2::new(x + col as f32 * (cw + 6.0), y + row as f32 * (slot_h + 6.0)),
                Vec2::new(cw, slot_h),
            );
            let on = active == Some(i);
            painter.rect_filled(r, 9, if on { style::surface_2() } else { style::surface_1() });
            painter.rect_stroke(
                r,
                9,
                Stroke::new(1.0, if on { style::nav() } else { style::border_soft() }),
                egui::StrokeKind::Inside,
            );
            let g = style::galley_mono(
                ui,
                &(i + 1).to_string(),
                10.0,
                if on { style::nav() } else { style::text_3() },
            );
            let gh = g.size().y;
            painter.galley(Pos2::new(r.left() + 10.0, r.top() + 9.0), g, style::text_3());
            let name = style::galley(ui, &model::slot_label(f), 11.5, Weight::W400, style::text_1(), 0.0);
            let clip = Rect::from_min_size(
                Pos2::new(r.left() + 10.0, r.top() + 9.0 + gh + 3.0),
                Vec2::new(cw - 20.0, 16.0),
            );
            ui.painter_at(clip).galley(clip.min, name, style::text_1());
            bottom = r.bottom();
        }
        let note = if map.is_some_and(|f| f.value as u32 != 0) {
            "The highlighted position is where the switch is right now."
        } else {
            "Assign a flight mode channel above to see which position the switch is in."
        };
        let g = kit::wrapped_weight(ui, note, 10.5, Weight::W400, style::text_3(), w, 1.4);
        painter.galley(Pos2::new(x, bottom + 6.0), g.clone(), style::text_3());
        bottom + 6.0 + g.size().y
    }

    /// The per channel calibration table: the numbers themselves, editable,
    /// with the live pulse beside them. Under 580 px it scrolls sideways
    /// inside its card rather than widening it (`.rc-cal-table`,
    /// `.rc-cal-row { min-width: 580px }`).
    #[allow(clippy::too_many_arguments)]
    fn channel_table(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        s: &model::Section,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        if w >= model::CAL_TABLE_MIN_W {
            return self.channel_rows(ui, ctx, doc, s, x, y, w, armed);
        }
        let height = model::cal_table_height(s.rows.len());
        let area = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, height));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        child.set_clip_rect(area.expand2(Vec2::new(0.0, 4.0)).intersect(ui.clip_rect()));
        egui::ScrollArea::horizontal()
            .id_salt("rc-cal-table")
            .auto_shrink([false, true])
            .show(&mut child, |ui| {
                let o = ui.cursor().min;
                let bottom = self.channel_rows(ui, ctx, doc, s, o.x, o.y, model::CAL_TABLE_MIN_W, armed);
                ui.allocate_space(Vec2::new(model::CAL_TABLE_MIN_W, bottom - o.y));
            });
        y + height
    }

    #[allow(clippy::too_many_arguments)]
    fn channel_rows(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        s: &model::Section,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        let server = self.server.clone();
        let fixed = 74.0 + 3.0 * 96.0 + 128.0 + 5.0 * 8.0;
        let first = (w - fixed).max(120.0);
        let col_x = |i: usize| -> f32 {
            let widths = [first, 74.0, 96.0, 96.0, 96.0, 128.0];
            x + widths[..i].iter().sum::<f32>() + 8.0 * i as f32
        };
        let rule = Stroke::new(1.0, style::border_soft());
        for (i, t) in ["Channel", "Now", "Min", "Centre", "Max", "Reversed"]
            .iter()
            .enumerate()
        {
            let g = style::galley(ui, &t.to_uppercase(), 10.0, Weight::W700, style::text_3(), 0.4);
            painter.galley(Pos2::new(col_x(i), y + 6.0), g, style::text_3());
        }
        let mut cy = y + 6.0 + 12.1 + 6.0;
        painter.hline(egui::Rangef::new(x, x + w), cy + 0.5, rule);
        cy += 1.0;
        let count = s.rows.len();
        for (n, r) in s.rows.iter().enumerate() {
            let top = cy + 6.0;
            let mid = top + 14.75;
            let g = style::galley(
                ui,
                &format!("CH {}", r.channel),
                11.5,
                Weight::W600,
                style::text_1(),
                0.0,
            );
            let gw = g.size().x;
            painter.galley(Pos2::new(col_x(0), mid - g.size().y / 2.0), g, style::text_1());
            if !r.calibrated {
                let chip = Chip {
                    size: 9.5,
                    weight: Weight::W700,
                    letter: 0.3,
                    pad: (1.0, 6.0, 1.0, 6.0),
                    fg: style::critical_bright(),
                    bg: style::critical_dark(),
                    border: style::critical(),
                    ..Chip::new("not calibrated")
                };
                let cs = chip.measure(ui);
                chip.paint(ui, Pos2::new(col_x(0) + gw + 6.0, mid - cs.y / 2.0));
            }
            let pulse = self.latest.get(r.channel as usize - 1).copied().unwrap_or(0.0);
            let now = if pulse > 0.0 {
                format!("{} \u{b5}s", crate::data::units::js_number(pulse))
            } else {
                "\u{2014}".to_string()
            };
            let g = style::galley_mono(ui, &now, 11.0, style::text_2());
            painter.galley(Pos2::new(col_x(1), mid - g.size().y / 2.0), g, style::text_2());
            for (k, slot) in r.fields.iter().enumerate() {
                let cx = col_x(2 + k);
                let Some(idx) = slot else {
                    let g = style::galley(ui, "\u{2014}", 11.0, Weight::W400, style::text_3(), 0.0);
                    painter.galley(Pos2::new(cx, mid - g.size().y / 2.0), g, style::text_3());
                    continue;
                };
                let f = doc.table[*idx].clone();
                let (h, nw, sw) = if k == 3 {
                    (26.5, None, Some(110.0))
                } else {
                    (27.5, Some(74.0), None)
                };
                let ux = self.form.control_with(
                    ui,
                    ctx,
                    &server,
                    &f,
                    Pos2::new(cx, mid - h / 2.0),
                    if k == 3 { 128.0 } else { 96.0 },
                    h,
                    armed,
                    true,
                    sw,
                    nw,
                );
                if let Some((cls, text)) = self.form.status_of(&f.param) {
                    let col = pagekit::status_color(cls);
                    let g = style::galley(ui, &text, 10.5, Weight::W400, col, 0.0);
                    ui.painter_at(Rect::from_min_size(
                        Pos2::new(ux, top),
                        Vec2::new(col_x(2 + k) + if k == 3 { 128.0 } else { 96.0 } - ux, 29.5),
                    ))
                    .galley(Pos2::new(ux, mid - g.size().y / 2.0), g, col);
                }
            }
            cy += 6.0 + 29.5 + 6.0;
            if n + 1 < count {
                painter.hline(egui::Rangef::new(x, x + w), cy + 0.5, rule);
                cy += 1.0;
            }
        }
        cy
    }
}

impl RadioPage {
    /// The calibration wizard.
    #[allow(clippy::too_many_arguments)]
    fn wizard(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        state: &Value,
        armed: bool,
        connected: bool,
        now: f64,
    ) -> f32 {
        let rc_live = state.get("rc_live") == Some(&Value::Bool(true));
        let latest = self.latest.clone();
        let doc = self.doc.clone().unwrap_or_default();
        let labels = model::assignment_labels(&doc);
        let blocked = armed || !connected;
        let server = self.server.clone();
        let Sub::Wizard(wz) = &mut self.sub else {
            return y;
        };
        wz.tx.update(state, now);
        // widen the measurement with the frame that just arrived
        if matches!(wz.step, Step::Sweep | Step::Sticks) {
            wz.m.accumulate(&latest);
        }
        // a write on its way
        if let Some(r) = wz.busy.as_ref().and_then(Pending::take) {
            wz.busy = None;
            match r {
                Ok(v) => {
                    let n = v.get("writes").and_then(Value::as_array).map_or(0, Vec::len);
                    self.notes.push("Radio calibration written to the vehicle".into());
                    wz.status = ("ok", format!("{n} parameters written"));
                    self.fresh = true;
                    self.sub = Sub::Overview;
                    self.load(ctx);
                    return y;
                }
                Err(e) => {
                    let msg = if e.is_empty() {
                        "The vehicle rejected the calibration".to_string()
                    } else {
                        e
                    };
                    wz.status = ("err", msg.clone());
                    self.errors.push(format!("Radio calibration failed: {msg}"));
                }
            }
        }
        let Sub::Wizard(wz) = &mut self.sub else {
            return y;
        };
        let busy = wz.busy.is_some();
        let step = wz.step;
        let idx = model::STEPS.iter().position(|s| *s == step).unwrap_or(0);

        let mut y = pagekit::header(
            ui,
            x,
            y,
            "Radio Calibration",
            "Measure what the transmitter sends, then write it to the vehicle",
        ) + 28.0;
        if blocked {
            let text = if armed {
                "Cannot calibrate the radio while armed. Disarm first."
            } else {
                "No link to the vehicle. Connect before calibrating."
            };
            y = pagekit::warning_banner(ui, x, y - 14.0, w, text) + 14.0;
        }

        // the stage
        let (headline, detail, hint): (String, String, Option<String>) = match step {
            Step::Intro => (
                "Before you start".into(),
                "Propellers off. Battery disconnected or the vehicle on a bench. Transmitter switched on and the receiver bound. The channel bars below must be moving when you wiggle a stick. Every endpoint this wizard writes comes from what it sees there.".into(),
                None,
            ),
            Step::Centre => (
                "Centre the sticks, throttle fully down".into(),
                "Let the sticks rest at their centre and pull the throttle all the way down. Leave the switches wherever they normally sit. This is the resting position written as each channel's centre.".into(),
                None,
            ),
            Step::Sweep => (
                "Move everything through its full travel".into(),
                "Sweep every stick to all four corners and flip every switch and knob end to end, slowly. The bars follow; the numbers beside them are the widest travel seen so far.".into(),
                Some(model::sweep_hint(&wz.m.swept())),
            ),
            Step::Sticks => {
                let st = &STICKS[wz.stick];
                let known = wz.m.sticks.get(st.id);
                (
                    format!("{}: {}", st.label, st.action),
                    "Hold it there and press Next. The channel that moved is the one this axis is bound to, and the direction it moved decides whether PX4 has to reverse it.".into(),
                    Some(match known {
                        Some(m) => format!(
                            "Measured: channel {}{}",
                            m.channel,
                            if m.reversed { " (reversed)" } else { " (normal)" }
                        ),
                        None => "Nothing measured for this axis yet.".into(),
                    }),
                )
            }
            Step::Review => (
                "Review the measurement".into(),
                "Nothing has been written yet. Check the endpoints below, then write them to the vehicle.".into(),
                None,
            ),
        };
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = y + 19.0;
        let chip = Chip {
            size: 10.0,
            weight: Weight::W700,
            letter: 0.4,
            pad: (3.0, 9.0, 3.0, 9.0),
            fg: style::text_3(),
            bg: style::surface_2(),
            border: style::border_soft(),
            ..Chip::new(format!("STEP {} OF {}", idx + 1, model::STEPS.len()))
        };
        let cs = chip.measure(ui);
        chip.paint(ui, Pos2::new(ix, cy));
        cy += cs.y + 8.0;
        let g = kit::wrapped_weight(ui, &headline, 16.0, Weight::W700, style::text_1(), iw, 1.35);
        painter.galley(Pos2::new(ix, cy), g.clone(), style::text_1());
        cy += g.size().y + 8.0;
        let g = kit::wrapped_weight(ui, &detail, 12.0, Weight::W400, style::text_2(), iw, 1.55);
        painter.galley(Pos2::new(ix, cy), g.clone(), style::text_2());
        cy += g.size().y;
        if let Some(h) = hint {
            cy += 8.0 + 2.0;
            let g = ui.painter().layout(h, style::mono(11.0), style::text_3(), iw);
            painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
            cy += g.size().y;
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        y = cy + 19.0 + 14.0;

        // what to move, and the bars: the prompt tells the drawing where to point
        let axes = ["left_x", "left_y", "right_x", "right_y"];
        wz.tx.prompt = match step {
            Step::Centre => Prompt {
                controls: axes.iter().map(|a| a.to_string()).collect(),
                ..Prompt::default()
            },
            Step::Sweep => {
                let swept = wz.m.swept();
                let map = wz.tx.channel_map();
                Prompt {
                    controls: map.keys().cloned().collect(),
                    swept: map
                        .iter()
                        .filter(|(_, c)| swept.contains(c))
                        .map(|(k, _)| k.clone())
                        .collect(),
                    ..Prompt::default()
                }
            }
            Step::Sticks => {
                let stick = &STICKS[wz.stick];
                let roles = tx::axis_roles(wz.tx.mode());
                match axes
                    .iter()
                    .find(|a| roles.iter().any(|(ax, r)| ax == *a && *r == stick.id))
                {
                    Some(axis) => Prompt {
                        controls: vec![axis.to_string()],
                        gimbal: Some(if axis.starts_with("left") { "left" } else { "right" }),
                        direction: Some(stick.direction),
                        swept: Vec::new(),
                    },
                    None => Prompt::default(),
                }
            }
            _ => Prompt::default(),
        };
        let highlight = (step == Step::Sticks)
            .then(|| wz.m.sticks.get(STICKS[wz.stick].id).map(|m| m.channel))
            .flatten();
        let (txw, monw) = if w >= ROW_MIN { top_row_widths(w) } else { (w, w) };
        let tx_bottom = {
            let bg = card::reserve(&painter);
            let (ix, iw) = (x + 19.0, txw - 38.0);
            let mut cy = y + 19.0;
            // no tools in the head here, so it is the title's own height
            cy += short_title(ui, ix, cy, "What to move") + 10.0;
            let hint = format!(
                "Drawn in Mode {}. Change the stick mode on the Radio Control page if the sticks here are not where yours are.",
                wz.tx.mode()
            );
            let g = kit::wrapped_weight(ui, &hint, 10.5, Weight::W400, style::text_3(), iw, 1.4);
            painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
            cy += g.size().y + 10.0;
            cy += wz.tx.figure(ui, ix, cy, iw, false);
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + txw, cy + 19.0)),
                card::Look::Page,
            );
            cy + 19.0 + 14.0
        };
        let (mx, my) = if w >= ROW_MIN {
            (x + txw + 14.0, y)
        } else {
            (x, tx_bottom)
        };
        let mon_bottom = {
            let bg = card::reserve(&painter);
            let b = draw_monitor(
                ui,
                &mut wz.monitor,
                mx + 19.0,
                my + 19.0,
                monw - 38.0,
                "Live channels",
                &labels,
                highlight,
                w,
                state,
                now,
            );
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(mx, my), Pos2::new(mx + monw, b + 19.0)),
                card::Look::Page,
            );
            b + 19.0 + 14.0
        };
        y = if w >= ROW_MIN {
            tx_bottom.max(mon_bottom)
        } else {
            mon_bottom
        };

        // the review
        if step == Step::Review {
            let bg = card::reserve(&painter);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "What will be written") + 10.0;
            let rows = wz.m.review_rows();
            if rows.is_empty() {
                let g = kit::wrapped_weight(
                    ui,
                    "No channel was swept far enough to write. Start over and move every stick and switch through its full travel.",
                    12.0,
                    Weight::W400,
                    style::text_2(),
                    iw,
                    1.4,
                );
                painter.galley(Pos2::new(ix, cy), g.clone(), style::text_2());
                cy += g.size().y;
            } else {
                let fixed = 74.0 + 3.0 * 96.0 + 128.0 + 5.0 * 8.0;
                let first = (iw - fixed).max(120.0);
                let widths = [first, 74.0, 96.0, 96.0, 96.0, 128.0];
                let col = |i: usize| ix + widths[..i].iter().sum::<f32>() + 8.0 * i as f32;
                for (i, t) in ["Channel", "Bound to", "Min", "Centre", "Max", "Direction"]
                    .iter()
                    .enumerate()
                {
                    let g = style::galley(ui, &t.to_uppercase(), 10.0, Weight::W700, style::text_3(), 0.4);
                    painter.galley(Pos2::new(col(i), cy + 6.0), g, style::text_3());
                }
                cy += 6.0 + 12.1 + 6.0;
                painter.hline(
                    egui::Rangef::new(ix, ix + iw),
                    cy + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                cy += 1.0;
                let n = rows.len();
                for (k, r) in rows.iter().enumerate() {
                    let h = 14.5;
                    let mid = cy + 6.0 + h / 2.0;
                    let put = |i: usize, t: &str, mono: bool, weight: Weight, color: egui::Color32| {
                        let g = if mono {
                            style::galley_mono(ui, t, 11.0, color)
                        } else {
                            style::galley(ui, t, 11.5, weight, color, 0.0)
                        };
                        painter.galley(Pos2::new(col(i), mid - g.size().y / 2.0), g, color);
                    };
                    put(0, &format!("CH {}", r.channel), false, Weight::W600, style::text_1());
                    put(
                        1,
                        if r.role.is_empty() { "\u{2014}" } else { &r.role },
                        false,
                        Weight::W400,
                        style::text_2(),
                    );
                    put(
                        2,
                        &crate::data::units::js_number(r.min),
                        true,
                        Weight::W400,
                        style::text_2(),
                    );
                    put(
                        3,
                        &crate::data::units::js_number(r.trim),
                        true,
                        Weight::W400,
                        style::text_2(),
                    );
                    put(
                        4,
                        &crate::data::units::js_number(r.max),
                        true,
                        Weight::W400,
                        style::text_2(),
                    );
                    put(
                        5,
                        if r.set_reverse {
                            if r.reversed {
                                "Reversed"
                            } else {
                                "Normal"
                            }
                        } else {
                            "unchanged"
                        },
                        false,
                        Weight::W400,
                        style::text_2(),
                    );
                    cy += 6.0 + h + 6.0;
                    if k + 1 < n {
                        painter.hline(
                            egui::Rangef::new(ix, ix + iw),
                            cy + 0.5,
                            Stroke::new(1.0, style::border_soft()),
                        );
                        cy += 1.0;
                    }
                }
                let mut warns: Vec<String> = Vec::new();
                let missing = wz.m.missing();
                if !missing.is_empty() {
                    warns.push(format!(
                        "No channel was measured for: {}. Those axes keep whatever mapping the vehicle already has.",
                        missing.join(", ")
                    ));
                }
                let clash = wz.m.clash();
                if !clash.is_empty() {
                    warns.push(clash);
                }
                for t in warns {
                    cy += 10.0;
                    let g = kit::wrapped_weight(ui, &t, 11.5, Weight::W400, style::critical_bright(), iw - 22.0, 1.5);
                    let r = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, g.size().y + 18.0));
                    painter.rect_filled(r, 9, style::critical_dark());
                    painter.rect_stroke(r, 9, Stroke::new(1.0, style::critical()), egui::StrokeKind::Inside);
                    painter.galley(r.min + Vec2::new(11.0, 9.0), g, style::critical_bright());
                    cy = r.bottom();
                }
            }
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
                card::Look::Page,
            );
            y = cy + 19.0 + 14.0;
        }

        // the buttons
        let can_next = !busy && !blocked && model::can_advance(step, rc_live, &latest, &wz.m);
        let can_apply = !busy && !blocked && !wz.m.review_rows().is_empty();
        let mut btns: Vec<(&'static str, Button<'_>)> = Vec::new();
        if step != Step::Intro {
            btns.push((
                "back",
                Button::new("Back", Variant::Secondary)
                    .icon("chevron-left")
                    .enabled(!busy),
            ));
        }
        if step != Step::Review {
            btns.push((
                "next",
                Button::new("Next", Variant::Primary)
                    .icon("chevron-right")
                    .enabled(can_next),
            ));
        } else {
            btns.push((
                "apply",
                Button::new("Write to vehicle", Variant::Primary)
                    .icon("check")
                    .enabled(can_apply),
            ));
        }
        if step != Step::Intro {
            btns.push((
                "restart",
                Button::new("Start over", Variant::Secondary)
                    .icon("rotate-cw")
                    .enabled(!busy),
            ));
        }
        let mut bx = x;
        let mut hit: Option<&'static str> = None;
        let by = y + 4.0;
        let mut bh_max = 0.0f32;
        for (name, b) in btns {
            let (bw, bh) = (b.width(ui), b.height());
            bh_max = bh_max.max(bh);
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(bx, by), Vec2::new(bw, bh)),
                ("radiowiz", name),
            ) {
                hit = Some(name);
            }
            bx += bw + 8.0;
        }
        y = by + bh_max;
        if !wz.status.1.is_empty() {
            let col = pagekit::status_color(wz.status.0);
            let g = style::galley(ui, &wz.status.1, 11.5, Weight::W400, col, 0.0);
            ui.painter().galley(Pos2::new(x, y + 6.0), g.clone(), col);
            y += 6.0 + g.size().y;
        }
        match hit {
            Some("next") => match step {
                Step::Intro => wz.step = Step::Centre,
                Step::Centre => {
                    wz.m.capture_centre(&latest);
                    wz.step = Step::Sweep;
                }
                Step::Sweep => {
                    wz.step = Step::Sticks;
                    wz.stick = 0;
                }
                Step::Sticks => {
                    if let Some(found) = wz.m.detect_stick(&latest) {
                        wz.m.sticks.insert(STICKS[wz.stick].id, found);
                    }
                    if wz.stick < STICKS.len() - 1 {
                        wz.stick += 1;
                    } else {
                        wz.step = Step::Review;
                    }
                }
                Step::Review => {}
            },
            Some("back") => {
                if step == Step::Sticks && wz.stick > 0 {
                    wz.stick -= 1;
                } else {
                    let i = idx.saturating_sub(1);
                    wz.step = model::STEPS[i];
                    if wz.step == Step::Sticks {
                        wz.stick = STICKS.len() - 1;
                    }
                }
            }
            Some("restart") => {
                wz.m = Measurement::default();
                wz.stick = 0;
                wz.step = Step::Intro;
                wz.status = ("", String::new());
            }
            Some("apply") => {
                let body = wz.m.request();
                wz.status = ("pending", "Writing the calibration\u{2026}".into());
                let s = server.clone();
                wz.busy = Some(Pending::spawn(ctx, move || {
                    http::post_json(&s, "/api/rc/calibrate", &body)
                }));
            }
            _ => {}
        }
        y
    }
}

impl Drop for RadioPage {
    fn drop(&mut self) {
        // hand the stream rate back to the firmware; best effort and short
        let s = self.server.clone();
        std::thread::spawn(move || {
            let _ = http::post_json(
                &s,
                "/api/rc/stream",
                &json!({"enabled": false, "rate_hz": model::STREAM_HZ}),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drawing_takes_62_percent_of_the_row_and_the_bars_the_rest() {
        let (a, b) = top_row_widths(901.0);
        assert!((a - 549.94).abs() < 0.2 && (b - 337.06).abs() < 0.2, "{a} {b}");
        // the web page measured 549.8 and 337.2 at 901 px
        assert_eq!(a + b + 14.0, 901.0);
        let (a, b) = top_row_widths(800.0);
        assert_eq!(b, 786.0 * 0.38_f32.max(280.0 / 786.0));
        assert!(a > 0.0);
    }

    #[test]
    fn a_narrow_column_loses_the_channel_notes_first() {
        let (l, bar, v, n) = channel_columns(299.2, 901.0);
        assert_eq!((l, v, n), (46.0, 66.0, 0.0));
        assert!((bar - 159.2).abs() < 0.01, "the web bar is 159.2 wide, got {bar}");
        assert_eq!(channel_columns(400.0, 1300.0).3, 110.0);
    }
}
