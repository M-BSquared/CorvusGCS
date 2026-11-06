//! The Safety & Sensors page of Setup (python/src/js/setup-safety.js, the
//! `.safety-*` rules of setup-forms.css).
//!
//! The page is a description the backend builds (`GET /api/safety`): cards of
//! fields, each naming the parameter it writes. The cards sit in two stacks
//! side by side, split by estimated height the way the web page splits them,
//! so a card never moves when a status line grows.
//!
//! Whole here: Flight limits, Return to Launch, Failsafe actions, the
//! Landing fields and its Fast landing preset, the Geofence action, the
//! sensor tiles with their state and the sensor pages behind them (switch,
//! drivers, ports, hardware presets, extra parameters), Check values, reading
//! and re-reading, and the armed gate.
//! The geofence card and its large editor are `geofence_card.rs`; the
//! Landing card is `landing.rs`; the preflight checklist is `checklist.rs`.

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::check::{self, Check};
use crate::setup::checklist::Checklist;
use crate::setup::geofence::Env;
use crate::setup::geofence_card::{GeofenceCard, Props};
use crate::setup::schema_form::{self, Field, FormState};
use crate::setup::sensor_view::{self, Event, SensorUi};
use crate::setup::sensors::{self, Extras, Preset, Toggle};
use crate::ui::card::{self, Look};
use crate::ui::icons;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::message::Tone;
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

/// One card of the description.
#[derive(Debug, Clone)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub hint: String,
    pub kind: String,
    pub fields: Vec<Field>,
    pub actions: Vec<Action>,
    pub sensor: Option<Sensor>,
    pub toggle: Option<Toggle>,
    pub presets: Vec<Preset>,
    /// The geofence section: why the action does nothing, and the writes that
    /// make a stored area act.
    pub inactive: String,
    pub enable_writes: Vec<(String, f64)>,
}

/// A one press preset on a form card.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub hint: String,
    pub writes: Vec<(String, f64)>,
}

/// A sensor section: what its tile says.
#[derive(Debug, Clone, PartialEq)]
pub struct Sensor {
    pub icon: String,
    pub short: String,
    pub state: &'static str,
    pub detail: String,
}

/// The description as the page uses it.
#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub connected: bool,
    pub error: String,
    pub received: u64,
    pub sections: Vec<Section>,
    /// The parameters the operator named, with whether the firmware has each.
    pub extra: Vec<(Field, bool)>,
}

/// Reads `GET /api/safety`.
pub fn parse_doc(v: &Value) -> Doc {
    let sections = v
        .get("sections")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|s| {
                    let text = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    let toggle = s.get("toggle").filter(|t| t.is_object());
                    Section {
                        id: text("id"),
                        title: text("title"),
                        hint: text("hint"),
                        kind: text("kind"),
                        fields: schema_form::parse_fields(s.get("fields").unwrap_or(&Value::Null)),
                        actions: s
                            .get("actions")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .map(|x| Action {
                                        id: x.get("id").and_then(Value::as_str).unwrap_or("").into(),
                                        label: x.get("label").and_then(Value::as_str).unwrap_or("").into(),
                                        hint: x.get("hint").and_then(Value::as_str).unwrap_or("").into(),
                                        writes: x
                                            .get("writes")
                                            .and_then(Value::as_array)
                                            .map(|w| {
                                                w.iter()
                                                    .filter_map(|w| {
                                                        Some((
                                                            w.get("param")?.as_str()?.to_string(),
                                                            w.get("value")?.as_f64()?,
                                                        ))
                                                    })
                                                    .collect()
                                            })
                                            .unwrap_or_default(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        sensor: toggle.map(|t| Sensor {
                            icon: Some(text("icon"))
                                .filter(|i| !i.is_empty())
                                .unwrap_or_else(|| "radar".into()),
                            short: text("short"),
                            state: sensor_state(t),
                            detail: t.get("detail").and_then(Value::as_str).unwrap_or("").into(),
                        }),
                        toggle: toggle.map(sensors::parse_toggle),
                        presets: sensors::parse_presets(s.get("presets").unwrap_or(&Value::Null)),
                        inactive: text("inactive"),
                        enable_writes: s
                            .get("enable_writes")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|w| {
                                        let name = w.get("name").or_else(|| w.get("param"))?.as_str()?;
                                        Some((name.to_string(), w.get("value")?.as_f64()?))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Doc {
        connected: v.get("connected") == Some(&Value::Bool(true)),
        error: v.get("error").and_then(Value::as_str).unwrap_or("").to_string(),
        received: v.get("received").and_then(Value::as_u64).unwrap_or(0),
        sections,
        extra: v
            .get("extra")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let present = e.get("present") == Some(&Value::Bool(true));
                        let f = schema_form::parse_fields(&Value::Array(vec![e.clone()])).pop()?;
                        Some((f, present))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// `renderedParams`: each parameter the sections show, with its card.
pub fn rendered_params(doc: &Doc) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for s in &doc.sections {
        for f in &s.fields {
            out.insert(f.param.clone(), s.title.clone());
        }
    }
    out
}

/// `fieldParams`: every editable parameter of the description.
pub fn field_params(doc: &Doc) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let all = doc
        .sections
        .iter()
        .flat_map(|s| s.fields.iter())
        .chain(doc.extra.iter().map(|(f, _)| f));
    for f in all {
        if !names.contains(&f.param) {
            names.push(f.param.clone());
        }
    }
    names
}

/// `sensorState`: the toggle's own word when it has one, else on or off.
pub fn sensor_state(t: &Value) -> &'static str {
    match t.get("state").and_then(Value::as_str) {
        Some("on") => "on",
        Some("partial") => "partial",
        Some("off") => "off",
        _ if t.get("enabled") == Some(&Value::Bool(true)) => "on",
        _ => "off",
    }
}

/// `formWeight`: roughly how many lines a form card takes.
pub fn form_weight(s: &Section) -> usize {
    2 + s.actions.iter().map(|a| 3 + a.writes.len()).sum::<usize>()
        + s.fields
            .iter()
            .map(|f| if f.hint.is_empty() { 1 } else { 2 })
            .sum::<usize>()
}

/// `splitIndex`: where to cut the cards so the taller stack is the shortest.
pub fn split_index(weights: &[usize]) -> usize {
    let total: usize = weights.iter().sum();
    let (mut best, mut taller, mut left) = (weights.len(), usize::MAX, 0);
    for i in 1..=weights.len() {
        left += weights[i - 1];
        let t = left.max(total - left);
        if t < taller {
            taller = t;
            best = i;
        }
    }
    best
}

/// How many stacks `repeat(auto-fit, minmax(min(440px, 100%), 1fr))` makes.
pub fn stacks(w: f32, cards: usize) -> usize {
    (((w + 14.0) / (440.0 + 14.0)).floor() as usize)
        .clamp(1, 2)
        .min(cards.max(1))
}

enum Card {
    Section(usize),
    Sensors,
    Empty,
    SensorStatus(usize),
    SensorHardware(usize),
    SensorParams(usize),
    Checklist,
    /// The geofence card, with its section when the vehicle gave one.
    Geofence(Option<usize>),
}

/// A chain of writes on its way: a sensor's switch or a preset.
struct Chain {
    /// `on` for a switch, `None` for a preset.
    on: Option<bool>,
    label: String,
    reboot: bool,
    done: Pending<Result<(), String>>,
}

/// The page's state.
pub struct SafetyPage {
    server: Server,
    doc: Option<Doc>,
    loading: Option<Pending<Result<Value, String>>>,
    status: (&'static str, String),
    form: FormState,
    action_status: std::collections::HashMap<String, (&'static str, String)>,
    action: Option<(String, Pending<Result<(), String>>)>,
    errors: Vec<String>,
    /// The sensor whose page is open.
    view: Option<String>,
    sensors: SensorUi,
    extras: Extras,
    chain: Option<Chain>,
    check: Check,
    geofence: GeofenceCard,
    /// The last check, as the card at the top shows it.
    check_card: Option<check::Summary>,
    /// The last check's verdict, to say once the re-read has landed.
    check_say: Option<check::Summary>,
    /// The next read goes to the vehicle, not the cache.
    fresh: bool,
}

impl SafetyPage {
    pub fn new(server: Server, ctx: &egui::Context, env: Env) -> SafetyPage {
        let server_for_fence = server.clone();
        let mut p = SafetyPage {
            server,
            doc: None,
            loading: None,
            status: ("pending", "Reading safety configuration\u{2026}".into()),
            form: FormState::default(),
            action_status: Default::default(),
            action: None,
            errors: Vec::new(),
            view: None,
            sensors: SensorUi::default(),
            extras: Extras::load_default(),
            chain: None,
            check: Check::default(),
            geofence: GeofenceCard::new(server_for_fence, env, ctx),
            check_card: None,
            check_say: None,
            fresh: false,
        };
        p.load(ctx);
        p
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        self.geofence.take_notes()
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        e.extend(self.form.take_errors());
        e.extend(self.geofence.take_errors());
        e
    }

    /// Whether a sensor's page is open, so the back button leads to the
    /// overview and not out of the page.
    pub fn in_sensor(&self) -> bool {
        self.view.is_some()
    }

    pub fn close_sensor(&mut self) {
        self.view = None;
    }

    pub fn open_sensor(&mut self, id: &str) {
        self.view = Some(id.to_string());
        self.sensors.status = ("", String::new());
        self.sensors.preset_status = ("", String::new());
        self.sensors.extra_status = ("", String::new());
    }

    /// The section of the open sensor page, if the description still has it.
    fn open_section(&self) -> Option<usize> {
        let id = self.view.as_ref()?;
        self.doc
            .as_ref()?
            .sections
            .iter()
            .position(|s| &s.id == id && s.sensor.is_some())
    }

    fn load(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        self.status = ("pending", "Reading safety configuration\u{2026}".into());
        let s = self.server.clone();
        let mut query = Vec::new();
        let extra = self.extras.all();
        if !extra.is_empty() {
            query.push(format!("extra={}", extra.join(",")));
        }
        if std::mem::take(&mut self.fresh) {
            query.push("fresh=1".into());
        }
        let path = if query.is_empty() {
            "/api/safety".to_string()
        } else {
            format!("/api/safety?{}", query.join("&"))
        };
        self.loading = Some(Pending::spawn(ctx, move || http::get_json(&s, &path)));
    }

    fn take_answers(&mut self, ctx: &egui::Context) {
        self.geofence.poll(ctx);
        if self.geofence.take_reload() {
            self.fresh = true;
            self.load(ctx);
        }
        if let Some(r) = self.loading.as_ref().and_then(Pending::take) {
            self.loading = None;
            self.sensors.shown_on = None;
            match r {
                Ok(v) => {
                    let doc = parse_doc(&v);
                    self.status = if doc.connected {
                        ("ok", format!("{} parameters read", doc.received))
                    } else {
                        (
                            "err",
                            if doc.error.is_empty() {
                                "No safety configuration received".into()
                            } else {
                                doc.error.clone()
                            },
                        )
                    };
                    self.extras.prune(&rendered_params(&doc));
                    self.doc = Some(doc);
                }
                Err(e) => {
                    self.doc = Some(Doc::default());
                    self.status = (
                        "err",
                        if e.is_empty() {
                            "Could not read safety configuration".into()
                        } else {
                            e
                        },
                    );
                }
            }
            if let Some(c) = self.check_say.take() {
                self.status = (c.cls, c.text);
            }
            // A sensor that disappeared between reads leaves no page about nothing.
            if self.view.is_some() && self.open_section().is_none() {
                self.view = None;
            }
        }
        if let Some(doc) = &mut self.doc {
            for s in &mut doc.sections {
                self.form.take_answers(&mut s.fields);
            }
            let mut extra: Vec<Field> = doc.extra.iter().map(|(f, _)| f.clone()).collect();
            self.form.take_answers(&mut extra);
            for ((f, _), live) in doc.extra.iter_mut().zip(extra) {
                f.value = live.value;
            }
        }
        if let Some((key, p)) = &self.action {
            if let Some(r) = p.take() {
                let key = key.clone();
                self.action = None;
                match r {
                    Ok(()) => {
                        self.action_status.insert(key, ("ok", "applied".into()));
                    }
                    Err(e) => {
                        let msg = if e.is_empty() { "write failed".to_string() } else { e };
                        self.action_status.insert(key, ("err", msg.clone()));
                        self.errors.push(format!("Could not apply the preset: {msg}"));
                    }
                }
                self.load(ctx);
            }
        }
        if let Some(c) = self
            .chain
            .as_ref()
            .and_then(|c| c.done.take().map(|r| (c.on, c.label.clone(), c.reboot, r)))
        {
            let (on, label, reboot, r) = c;
            self.chain = None;
            match r {
                Ok(()) => match on {
                    Some(on) => {
                        let word = if on { "enabled" } else { "disabled" };
                        self.sensors.status = (
                            "ok",
                            if reboot {
                                format!("{word}. Reboot the autopilot for the driver to start")
                            } else {
                                word.to_string()
                            },
                        );
                    }
                    None => {
                        self.sensors.preset_status = (
                            "ok",
                            if reboot {
                                "applied. Reboot the autopilot for the driver to start".into()
                            } else {
                                "applied".into()
                            },
                        )
                    }
                },
                Err(e) => {
                    let msg = if e.is_empty() { "write failed".to_string() } else { e };
                    if on.is_some() {
                        self.sensors.status = ("err", msg.clone());
                        self.errors.push(format!("Could not configure {label}: {msg}"));
                    } else {
                        self.sensors.preset_status = ("err", msg.clone());
                        self.errors.push(format!("Could not apply {label}: {msg}"));
                    }
                }
            }
            self.load(ctx);
        }
        if let Some(summary) = self.check.take(&mut self.form.wanted) {
            self.form.mark_checked(&summary.rows);
            self.check_card = Some(summary.clone());
            self.check_say = Some(summary);
            self.fresh = true;
            self.load(ctx);
        }
    }

    fn run_action(&mut self, ctx: &egui::Context, key: String, a: &Action) {
        self.action_status
            .insert(key.clone(), ("pending", "applying\u{2026}".into()));
        for (n, v) in &a.writes {
            self.form.wanted.insert(n.clone(), *v);
        }
        let (s, writes) = (self.server.clone(), a.writes.clone());
        self.action = Some((key, Pending::spawn(ctx, move || write_all(&s, writes))));
    }

    /// `runWrites`: each write in order, every one wanted before the first
    /// is sent so a write lost halfway can be confirmed by Check values.
    fn run_chain(
        &mut self,
        ctx: &egui::Context,
        on: Option<bool>,
        label: &str,
        reboot: bool,
        writes: Vec<(String, f64)>,
    ) {
        if writes.is_empty() {
            return;
        }
        for (n, v) in &writes {
            self.form.wanted.insert(n.clone(), *v);
        }
        let s = self.server.clone();
        let status = (
            "pending",
            if on == Some(false) {
                "disabling\u{2026}"
            } else if on == Some(true) {
                "enabling\u{2026}"
            } else {
                "applying\u{2026}"
            }
            .to_string(),
        );
        if on.is_some() {
            self.sensors.status = status;
        } else {
            self.sensors.preset_status = status;
        }
        self.chain = Some(Chain {
            on,
            label: label.to_string(),
            reboot,
            done: Pending::spawn(ctx, move || write_all(&s, writes)),
        });
    }

    /// What the operator did on the open sensor page.
    fn handle(&mut self, ctx: &egui::Context, i: usize, event: Event, state_armed: bool) {
        let Some(doc) = &self.doc else { return };
        let Some(section) = doc.sections.get(i).cloned() else {
            return;
        };
        let Some(t) = section.toggle.clone() else { return };
        let sid = section.id.clone();
        match event {
            Event::Switch(_) | Event::Repick => {
                let on = match event {
                    Event::Switch(on) => on,
                    _ => {
                        if self.sensors.shown_on.unwrap_or(t.enabled) {
                            true
                        } else {
                            return;
                        }
                    }
                };
                if state_armed {
                    return;
                }
                let driver_id = self.sensors.driver_id(&sid, &t);
                let driver = t.drivers.iter().find(|d| d.id == driver_id);
                let port = self.sensors.port_text(&sid, &t);
                match sensors::sensor_writes(&t, driver, &port, on) {
                    Ok(writes) => {
                        if writes.is_empty() {
                            self.sensors.status = ("", String::new());
                        } else {
                            self.run_chain(ctx, Some(on), &t.label, t.reboot, writes);
                        }
                    }
                    Err(msg) => {
                        self.sensors.status = ("err", msg.clone());
                        self.sensors.shown_on = None;
                        if !msg.starts_with("pick the") {
                            self.errors.push(format!("Could not configure {}: {msg}", t.label));
                        }
                    }
                }
            }
            Event::Apply(id) => {
                let Some(p) = section.presets.iter().find(|p| p.id == id) else {
                    return;
                };
                if state_armed {
                    return;
                }
                let port = if p.serial {
                    self.sensors.preset_port_text(&sid, &t)
                } else {
                    String::new()
                };
                match sensors::preset_writes(&t, p, &port) {
                    Ok(writes) => {
                        if writes.is_empty() {
                            self.sensors.preset_status = ("", String::new());
                        } else {
                            self.run_chain(ctx, None, &p.label, p.reboot, writes);
                        }
                    }
                    Err(msg) => {
                        self.sensors.preset_status = ("err", msg.clone());
                        if !msg.starts_with("pick the") {
                            self.errors.push(format!("Could not apply {}: {msg}", p.label));
                        }
                    }
                }
            }
            Event::AddExtra(name) => {
                let rendered = rendered_params(doc);
                let problem = self.extras.problem(&sid, &name, &rendered);
                if problem.is_empty() {
                    self.extras.add(&sid, &name);
                    self.sensors.extra_text.clear();
                    self.sensors.extra_status = ("", String::new());
                    self.load(ctx);
                } else {
                    self.sensors.extra_status = ("err", problem);
                }
            }
            Event::RemoveExtra(name) => {
                self.extras.remove(&sid, &name);
            }
        }
    }

    /// Draws the page's head and cards from `y`; returns the bottom.
    #[allow(clippy::too_many_arguments)]
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, checklist: &mut Checklist) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx);
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let open = self.open_section();
        // .setup-head: the header, with Reload and Check values and the
        // status at its right.
        let (title, subtitle) = match (open, &self.doc) {
            (Some(i), Some(doc)) => {
                let s = &doc.sections[i];
                (
                    if s.title.is_empty() {
                        "Sensor".to_string()
                    } else {
                        s.title.clone()
                    },
                    s.hint.clone(),
                )
            }
            _ => (
                "Safety & Sensors".to_string(),
                "Flight limits, failsafe actions, and the sensors they rely on".to_string(),
            ),
        };
        let checking = self.check.busy();
        let busy_read = self.loading.is_some() || checking;
        let check_btn = Button::new("Check values", Variant::Secondary)
            .icon("list-checks")
            .small()
            .title("Read every value back from the vehicle, write again what did not stick, and show what the vehicle holds now")
            .enabled(
                !busy_read
                    && self.doc.as_ref().is_some_and(|d| d.connected && !field_params(d).is_empty()),
            );
        let reload = Button::new("Reload", Variant::Primary)
            .icon("refresh-cw")
            .small()
            .enabled(!busy_read);
        let (rw, rh) = (reload.width(ui), reload.height());
        let cw = check_btn.width(ui);
        let status_w = style::galley(ui, &self.status.1, 11.5, Weight::W400, style::text_2(), 0.0)
            .size()
            .x
            .min(420.0);
        let status_gap = if status_w > 0.0 { 10.0 } else { 0.0 };
        let actions_w = status_w + status_gap + rw + 10.0 + cw;
        // the title row, its actions wrapping under it when the title would
        // be left less than 280 px (.setup-head: flex-wrap, header 1 1 280px)
        let wrap = w - actions_w - 24.0 < 280.0;
        let head_w = if wrap { w } else { (w - actions_w - 24.0).min(w) };
        let head_bottom = pagekit::header_wrapped(ui, x, y, head_w, &title, &subtitle);
        let (ay, bottom) = if wrap {
            (head_bottom + 10.0, head_bottom + 12.0 + rh)
        } else {
            (head_bottom - rh, head_bottom)
        };
        let sg_color = match self.status.0 {
            "ok" => style::healthy(),
            "err" => style::critical(),
            _ => style::nav(),
        };
        let sg = style::galley(ui, &self.status.1, 11.5, Weight::W400, sg_color, 0.0);
        let (status_x, reload_x, check_x) = if wrap {
            (x, x + status_w + status_gap, x + status_w + status_gap + rw + 10.0)
        } else {
            let check_x = x + w - cw;
            let reload_x = check_x - 10.0 - rw;
            (reload_x - 10.0 - sg.size().x, reload_x, check_x)
        };
        if check_btn.show(
            ui,
            Rect::from_min_size(Pos2::new(check_x, ay), Vec2::new(cw, rh)),
            "safetycheck",
        ) {
            if let Some(doc) = &self.doc {
                let names = field_params(doc);
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
            "safetyreload",
        ) {
            self.check_card = None;
            self.load(&ctx);
        }
        ui.painter()
            .galley(Pos2::new(status_x, ay + rh / 2.0 - sg.size().y / 2.0), sg, sg_color);
        let mut y = bottom + 26.0;
        if armed {
            let g = kit::wrapped_weight(
                ui,
                "Safety configuration is read-only while armed",
                11.5,
                Weight::W400,
                style::warning(),
                w - 28.0,
                1.4,
            );
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 24.0));
            {
                card::paint(ui.painter(), r, Look::Tone(Tone::Warn));
            }
            ui.painter().galley(r.min + Vec2::new(14.0, 12.0), g, style::warning());
            y = r.bottom() + 4.0;
        }
        let Some(doc) = self.doc.clone() else { return y };
        if let Some(c) = self.check_card.clone() {
            y = check::card(ui, x, y, w, &c) + 14.0;
        }
        // the cards, in the order the web page lays them out
        let mut order: Vec<(Card, usize)> = Vec::new();
        if let Some(i) = open {
            let s = &doc.sections[i];
            let problems = s.toggle.as_ref().map_or(0, |t| t.problems.len());
            order.push((Card::SensorStatus(i), 6 + problems));
            if !s.presets.is_empty() {
                order.push((Card::SensorHardware(i), 10));
            }
            if !s.fields.is_empty() {
                order.push((Card::SensorParams(i), form_weight(s)));
            }
        } else if doc.sections.is_empty() {
            order.push((Card::Empty, 4));
            order.push((Card::Geofence(None), 14 + 2));
        } else {
            let fence = doc.sections.iter().position(|s| s.kind == "geofence");
            let mut fence_placed = false;
            for (i, s) in doc.sections.iter().enumerate() {
                if s.sensor.is_some() || s.kind == "geofence" {
                    continue;
                }
                if s.id == "rtl" {
                    if let Some(f) = fence {
                        order.push((Card::Geofence(Some(f)), 14 + form_weight(&doc.sections[f])));
                        fence_placed = true;
                    }
                }
                let weight = if s.id == "landing" { 9 } else { form_weight(s) };
                order.push((Card::Section(i), weight));
            }
            if let (Some(f), false) = (fence, fence_placed) {
                order.push((Card::Geofence(Some(f)), 14 + form_weight(&doc.sections[f])));
            }
            let sensors = doc.sections.iter().filter(|s| s.sensor.is_some()).count();
            if sensors > 0 {
                order.push((Card::Sensors, 3 + 2 * sensors));
            }
        }
        if open.is_none() {
            order.push((Card::Checklist, 8));
        }
        let weights: Vec<usize> = order.iter().map(|(_, w)| *w).collect();
        let n_stacks = stacks(w, order.len());
        let cut = if n_stacks == 1 {
            order.len()
        } else {
            split_index(&weights)
        };
        let col_w = (w - 14.0 * (n_stacks as f32 - 1.0)) / n_stacks as f32;
        let mut bottoms = [y; 2];
        let mut cards = order.into_iter().map(|(c, _)| c).collect::<Vec<_>>().into_iter();
        let mut events: Vec<(usize, Event)> = Vec::new();
        #[allow(clippy::needless_range_loop)]
        for col in 0..n_stacks {
            let count = if col == 0 { cut } else { usize::MAX };
            let cx = x + col as f32 * (col_w + 14.0);
            let mut cy = y;
            for _ in 0..count {
                let Some(card) = cards.next() else { break };
                cy = self.card(
                    ui,
                    &ctx,
                    &doc,
                    card,
                    cx,
                    cy,
                    col_w,
                    armed,
                    state,
                    checklist,
                    &mut events,
                ) + 14.0;
            }
            bottoms[col] = cy - 14.0;
        }
        for (i, e) in events {
            self.handle(&ctx, i, e, armed);
        }
        self.geofence.overlay(&ctx, state, doc.connected, armed);
        bottoms[0].max(bottoms[1]).max(y)
    }

    #[allow(clippy::too_many_arguments)]
    fn card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        card: Card,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
        state: &Value,
        checklist: &mut Checklist,
        events: &mut Vec<(usize, Event)>,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = y + 19.0;
        match card {
            Card::Empty => {
                cy = pagekit::section_title(ui, ix, cy, iw, "Safety & Sensors");
                let g = kit::wrapped_weight(ui, "Connect to a vehicle to read its safety limits, failsafe actions and sensor configuration. The page shows only what the connected firmware actually reports.", 12.0, Weight::W400, style::text_2(), iw - 28.0, 1.55);
                let r = Rect::from_min_size(Pos2::new(ix, cy - 2.0), Vec2::new(iw, g.size().y + 24.0));
                {
                    card::paint(&painter, r, Look::Note);
                }
                painter.galley(r.min + Vec2::new(14.0, 12.0), g, style::text_2());
                cy = r.bottom();
            }
            Card::Geofence(section) => {
                let section = section.map(|i| doc.sections[i].clone());
                let live = section.as_ref().and_then(|s| {
                    self.doc
                        .as_ref()
                        .and_then(|d| d.sections.iter().find(|x| x.id == s.id))
                        .map(|x| x.fields.clone())
                });
                let fields = live.unwrap_or_else(|| section.as_ref().map(|s| s.fields.clone()).unwrap_or_default());
                let title = section.as_ref().map_or("Geofence", |s| s.title.as_str());
                cy = pagekit::section_title(ui, ix, cy, iw, title) - 2.0;
                let props = Props {
                    hint: section.as_ref().map_or("", |s| s.hint.as_str()),
                    inactive: section.as_ref().map_or("", |s| s.inactive.as_str()),
                    enable_writes: section.as_ref().map_or(&[], |s| s.enable_writes.as_slice()),
                    fields: &fields,
                    connected: doc.connected,
                    armed,
                };
                let server = self.server.clone();
                cy = self
                    .geofence
                    .show_body(ui, ctx, &server, &mut self.form, &props, state, ix, cy, iw);
            }
            Card::Section(i) if doc.sections[i].id == "landing" => {
                let section = doc.sections[i].clone();
                let fields = self
                    .doc
                    .as_ref()
                    .and_then(|d| d.sections.get(i))
                    .map_or_else(|| section.fields.clone(), |s| s.fields.clone());
                let key = format!(
                    "{}:{}",
                    section.id,
                    section.actions.first().map_or("", |a| a.id.as_str())
                );
                let busy = self.action.is_some() || self.form.busy();
                let out = crate::setup::landing::show(
                    ui,
                    ctx,
                    &self.server.clone(),
                    &mut self.form,
                    &section,
                    &fields,
                    self.action_status.get(&key),
                    ix,
                    cy,
                    iw,
                    armed,
                    busy,
                );
                if out.apply {
                    if let Some(a) = section.actions.first() {
                        self.run_action(ctx, key, a);
                    }
                }
                cy = out.bottom;
            }
            Card::Section(i) => {
                let section = doc.sections[i].clone();
                cy = pagekit::section_title(ui, ix, cy, iw, &section.title) - 2.0;
                if !section.hint.is_empty() {
                    let g = kit::wrapped_weight(ui, &section.hint, 10.5, Weight::W400, style::text_3(), iw, 1.4);
                    painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
                    cy += g.size().y + 10.0;
                }
                for a in &section.actions {
                    cy = self.action_block(ui, ctx, &section, a, ix, cy, iw, armed) + 10.0;
                }
                if section.kind == "geofence" && section.fields.is_empty() {
                    let g = kit::wrapped_weight(
                        ui,
                        "The vehicle did not report a geofence action.",
                        10.5,
                        Weight::W400,
                        style::text_3(),
                        iw,
                        1.4,
                    );
                    painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
                    cy += g.size().y + 10.0;
                }
                if !section.fields.is_empty() {
                    // The form works on the live copy of the fields.
                    let mut fields = section.fields.clone();
                    if let Some(live) = self.doc.as_ref().and_then(|d| d.sections.get(i)) {
                        fields = live.fields.clone();
                    }
                    cy = self
                        .form
                        .show(ui, ctx, &self.server.clone(), &fields, ix, cy, iw, armed);
                }
                if section.kind == "geofence" {
                    let g = kit::wrapped_weight(
                        ui,
                        "Drawing the area is in the web interface for now.",
                        10.5,
                        Weight::W400,
                        style::text_3(),
                        iw,
                        1.4,
                    );
                    painter.galley(Pos2::new(ix, cy + 10.0), g.clone(), style::text_3());
                    cy += 10.0 + g.size().y;
                }
            }
            Card::Checklist => {
                cy = pagekit::section_title(ui, ix, cy, iw, "Preflight checklist") - 2.0;
                cy = checklist.card_body(ui, ctx, ix, cy, iw);
            }
            Card::SensorStatus(i) => {
                let section = &doc.sections[i];
                cy = pagekit::section_title(ui, ix, cy, iw, "Status") - 2.0;
                if let Some(t) = &section.toggle {
                    let mut local = Vec::new();
                    cy = sensor_view::status_box(ui, &mut self.sensors, &section.id, t, ix, cy, iw, armed, &mut local);
                    events.extend(local.into_iter().map(|e| (i, e)));
                }
            }
            Card::SensorHardware(i) => {
                let section = &doc.sections[i];
                cy = pagekit::section_title(ui, ix, cy, iw, "Hardware") - 2.0;
                if let Some(t) = &section.toggle {
                    let mut extra = doc.extra.clone();
                    if let Some(live) = self.doc.as_ref() {
                        extra = live.extra.clone();
                    }
                    let mut local = Vec::new();
                    let busy = self.chain.is_some() || self.form.busy();
                    cy = sensor_view::hardware_body(
                        ui,
                        ctx,
                        &self.server.clone(),
                        &mut self.sensors,
                        &mut self.form,
                        &self.extras,
                        &extra,
                        &section.id,
                        &section.title,
                        t,
                        &section.presets,
                        ix,
                        cy,
                        iw,
                        armed,
                        busy,
                        &mut local,
                    );
                    events.extend(local.into_iter().map(|e| (i, e)));
                }
            }
            Card::SensorParams(i) => {
                cy = pagekit::section_title(ui, ix, cy, iw, "Parameters") - 2.0;
                let g = kit::wrapped_weight(
                    ui,
                    "Every field writes its parameter to the vehicle as soon as you leave it. \
                     A preset fills these in; changing one afterwards is expected.",
                    10.5,
                    Weight::W400,
                    style::text_3(),
                    iw,
                    1.4,
                );
                painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
                cy += g.size().y + 10.0;
                let fields = self
                    .doc
                    .as_ref()
                    .and_then(|d| d.sections.get(i))
                    .map_or_else(|| doc.sections[i].fields.clone(), |s| s.fields.clone());
                cy = self
                    .form
                    .show(ui, ctx, &self.server.clone(), &fields, ix, cy, iw, armed);
            }
            Card::Sensors => {
                cy = pagekit::section_title(ui, ix, cy, iw, "Sensors") - 2.0;
                let g = kit::wrapped_weight(ui, "Each sensor has its own page: pick the module you actually fitted and its whole setup is written for you, or set every parameter by hand.", 10.5, Weight::W400, style::text_3(), iw, 1.4);
                painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
                cy += g.size().y + 10.0;
                let sensors: Vec<&Section> = doc.sections.iter().filter(|s| s.sensor.is_some()).collect();
                let cols = (((iw + 10.0) / (260.0 + 10.0)).floor() as usize).max(1);
                let tw = (iw - 10.0 * (cols as f32 - 1.0)) / cols as f32;
                let th = 14.0 * 2.0 + 14.0 * 1.21 + 3.0 + 3.0 * 11.5 * 1.45 + 5.0 + 18.0;
                let mut bottom = cy;
                for (k, s) in sensors.iter().enumerate() {
                    let r = Rect::from_min_size(
                        Pos2::new(
                            ix + (k % cols) as f32 * (tw + 10.0),
                            cy + (k / cols) as f32 * (th + 10.0),
                        ),
                        Vec2::new(tw, th),
                    );
                    bottom = bottom.max(r.bottom());
                    if sensor_tile(ui, r, s) {
                        self.open_sensor(&s.id);
                    }
                }
                cy = bottom;
            }
        }
        let rect = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0));
        card::fill(&painter, bg, rect, card::Look::Page);
        rect.bottom()
    }

    #[allow(clippy::too_many_arguments)]
    fn action_block(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        section: &Section,
        a: &Action,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let mut cy = y;
        if !a.hint.is_empty() {
            let g = kit::wrapped_weight(ui, &a.hint, 11.5, Weight::W400, style::text_2(), w, 1.5);
            ui.painter().galley(Pos2::new(x, cy), g.clone(), style::text_2());
            cy += g.size().y + 10.0;
        }
        // The values the preset writes, with each field's label and unit.
        for (param, value) in &a.writes {
            let f = section.fields.iter().find(|f| &f.param == param);
            let label = f.map_or(String::new(), |f| {
                format!(
                    "{}{}",
                    f.label,
                    if f.unit.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", f.unit)
                    }
                )
            });
            let line = format!("{label}   {}", schema_form::format_number(*value));
            cy += pagekit::text(ui, Pos2::new(x, cy), &line, 11.5, Weight::W400, style::text_2()) + 2.0;
        }
        if !a.writes.is_empty() {
            cy += 8.0;
        }
        let key = format!("{}:{}", section.id, a.id);
        let busy = self.action.is_some() || self.form.busy();
        let b = Button::new(&a.label, Variant::Primary)
            .icon("check-check")
            .small()
            .enabled(!armed && !busy && !a.writes.is_empty());
        let (bw, bh) = (b.width(ui), b.height());
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(x, cy), Vec2::new(bw, bh)),
            ("action", &key),
        ) {
            self.run_action(ctx, key.clone(), a);
        }
        if let Some((cls, text)) = self.action_status.get(&key) {
            let color = match *cls {
                "ok" => style::healthy(),
                "err" => style::critical(),
                _ => style::nav(),
            };
            let g = style::galley(ui, text, 10.5, Weight::W400, color, 0.0);
            ui.painter()
                .galley(Pos2::new(x + bw + 8.0, cy + (bh - g.size().y) / 2.0), g, color);
        }
        cy + bh
    }
}

/// Writes each parameter in order and stops at the first one refused.
pub(crate) fn write_all(server: &Server, writes: Vec<(String, f64)>) -> Result<(), String> {
    for (name, value) in writes {
        let d = http::post_json(server, "/api/params/set", &json!({ "name": name, "value": value }))?;
        if d.get("ok") != Some(&Value::Bool(true)) {
            return Err(format!(
                "{name}: {}",
                d.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Command rejected by vehicle")
            ));
        }
    }
    Ok(())
}

/// One sensor tile: icon, title, description and the state pill.
fn sensor_tile(ui: &mut egui::Ui, r: Rect, s: &Section) -> bool {
    let sensor = s.sensor.clone().unwrap_or(Sensor {
        icon: "radar".into(),
        short: String::new(),
        state: "off",
        detail: String::new(),
    });
    let resp = ui.interact(r, ui.scope_id().with(("sensor", &s.id)), egui::Sense::click());
    let (fill, stroke) = if resp.hovered() {
        (style::glass(0.95), style::tint(0.15))
    } else {
        (style::glass(0.82), style::tint(0.09))
    };
    let p = ui.painter().clone();
    {
        card::paint(&p, r, Look::custom(fill, stroke, style::RADIUS));
    }
    let inner = r.shrink2(Vec2::new(16.0, 14.0));
    let icon_box = Rect::from_min_size(inner.min, Vec2::splat(34.0));
    p.rect_filled(icon_box, style::RADIUS_BTN, style::accent_dark());
    icons::paint(
        ui,
        &sensor.icon,
        Rect::from_center_size(icon_box.center(), Vec2::splat(17.0)),
        style::accent_bright(),
    );
    let chevron = Rect::from_min_size(
        Pos2::new(inner.right() - 16.0, inner.top() + 9.0 - 8.0 + 8.0),
        Vec2::splat(16.0),
    );
    icons::paint(ui, "chevron-right", chevron, style::text_3());
    let tx = icon_box.right() + 12.0;
    let tw = chevron.left() - 12.0 - tx;
    let t = style::galley(ui, &s.title, 14.0, Weight::W600, style::text_1(), 0.2);
    p.galley(Pos2::new(tx, inner.top()), t.clone(), style::text_1());
    let d = kit::wrapped_weight(ui, &sensor.short, 11.5, Weight::W400, style::text_3(), tw, 1.45);
    let dy = inner.top() + t.size().y + 3.0;
    p.galley(Pos2::new(tx, dy), d.clone(), style::text_3());
    // the state row
    let (label, color) = match sensor.state {
        "on" => ("ON", style::healthy()),
        "partial" => ("INCOMPLETE", style::warning()),
        _ => ("OFF", style::text_3()),
    };
    let pg = style::galley(ui, label, 10.0, Weight::W700, color, 0.4);
    let pill = Rect::from_min_size(
        Pos2::new(tx, dy + d.size().y + 5.0),
        Vec2::new(pg.size().x + 14.0, pg.size().y + 4.0),
    );
    let (pf, ps) = match sensor.state {
        "on" | "partial" => (color.gamma_multiply(0.12), color.gamma_multiply(0.45)),
        _ => (egui::Color32::TRANSPARENT, style::border_soft()),
    };
    {
        let r = pill;
        card::paint(&p, r, Look::custom(pf, ps, 255));
    }
    p.galley(pill.min + Vec2::new(7.0, 2.0), pg, color);
    if !sensor.detail.is_empty() {
        let dg = style::galley(ui, &sensor.detail, 11.0, Weight::W400, style::text_3(), 0.0);
        let clip = Rect::from_min_max(
            Pos2::new(pill.right() + 7.0, pill.top()),
            Pos2::new(tx + tw, pill.bottom()),
        );
        ui.painter_at(clip).galley(
            Pos2::new(clip.left(), pill.center().y - dg.size().y / 2.0),
            dg,
            style::text_3(),
        );
    }
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_is_read_into_sections() {
        let v = json!({
            "connected": true, "received": 42,
            "sections": [
                {"id": "failsafe", "title": "Failsafe actions", "kind": "fields", "hint": "h",
                 "fields": [{"param": "NAV_RCL_ACT", "kind": "enum", "value": 2, "options": [{"value": 2, "label": "Return"}]}]},
                {"id": "landing", "title": "Landing", "kind": "fields",
                 "fields": [{"param": "MPC_LAND_SPEED", "kind": "number", "value": 0.7}],
                 "actions": [{"id": "fast", "label": "Fast landing", "writes": [{"param": "MPC_LAND_SPEED", "value": 1.0}, {"param": "X"}]}]},
                {"id": "rangefinder", "title": "Distance sensor", "kind": "toggle", "short": "A lidar",
                 "toggle": {"state": "partial", "detail": "driver on, not fused"}, "fields": []}
            ]
        });
        let d = parse_doc(&v);
        assert!(d.connected);
        assert_eq!((d.received, d.sections.len()), (42, 3));
        assert_eq!(d.sections[0].fields.len(), 1);
        assert_eq!(
            d.sections[1].actions[0].writes,
            vec![("MPC_LAND_SPEED".to_string(), 1.0)],
            "a write without a value is dropped"
        );
        let s = d.sections[2].sensor.clone().unwrap();
        assert_eq!(
            (s.state, s.detail.as_str(), s.icon.as_str()),
            ("partial", "driver on, not fused", "radar")
        );
        assert!(d.sections[0].sensor.is_none());
    }

    #[test]
    fn sensor_state_prefers_its_own_word() {
        assert_eq!(sensor_state(&json!({"state": "on"})), "on");
        assert_eq!(sensor_state(&json!({"state": "partial", "enabled": false})), "partial");
        assert_eq!(sensor_state(&json!({"enabled": true})), "on");
        assert_eq!(sensor_state(&json!({"state": "weird"})), "off");
    }

    #[test]
    fn the_cards_are_split_into_two_stacks_as_the_web_splits_them() {
        assert_eq!(split_index(&[8, 8, 8, 8]), 2);
        assert_eq!(split_index(&[10, 2, 2, 2]), 1);
        assert_eq!(split_index(&[5]), 1);
        assert_eq!(stacks(300.0, 5), 1);
        assert_eq!(stacks(940.0, 5), 2);
        assert_eq!(stacks(2000.0, 5), 2);
        assert_eq!(stacks(940.0, 1), 1);
    }

    #[test]
    fn a_form_card_weighs_its_fields_and_presets() {
        let f = |hint: &str| Field {
            param: "P".into(),
            label: "P".into(),
            kind: schema_form::Kind::Number,
            value: 0.0,
            unit: String::new(),
            hint: hint.into(),
            min: None,
            max: None,
            also: vec![],
        };
        let s = Section {
            id: "x".into(),
            title: "X".into(),
            hint: String::new(),
            kind: "fields".into(),
            fields: vec![f(""), f("h")],
            actions: vec![Action {
                id: "a".into(),
                label: "A".into(),
                hint: String::new(),
                writes: vec![("P".into(), 1.0), ("Q".into(), 2.0)],
            }],
            sensor: None,
            toggle: None,
            presets: vec![],
            inactive: String::new(),
            enable_writes: vec![],
        };
        assert_eq!(form_weight(&s), 2 + (3 + 2) + 1 + 2);
    }

    #[test]
    fn the_geofence_writes_of_both_stacks_are_read() {
        // ArduPilot: python/corvus/ardupilot_safety.py geofence_enable_writes, {"name", "value"}.
        let ardu = serde_json::json!({"connected": true, "sections": [{
            "id": "geofence", "title": "Geofence", "kind": "geofence", "fields": [],
            "enable_writes": [{"name": "FENCE_TYPE", "value": 7}, {"name": "FENCE_ENABLE", "value": 1}]}]});
        let doc = parse_doc(&ardu);
        assert_eq!(
            doc.sections[0].enable_writes,
            vec![("FENCE_TYPE".to_string(), 7.0), ("FENCE_ENABLE".to_string(), 1.0)]
        );
        // PX4: python/corvus/safety_config.py _geofence_section, "enable_writes": [].
        let px4 = serde_json::json!({"connected": true, "sections": [{
            "id": "geofence", "title": "Geofence", "kind": "geofence", "fields": [], "enable_writes": []}]});
        assert!(parse_doc(&px4).sections[0].enable_writes.is_empty());
        // a write that names its parameter `param`, as the sensor sections do
        let other = serde_json::json!({"connected": true, "sections": [{
            "id": "x", "title": "X", "kind": "fields", "fields": [],
            "enable_writes": [{"param": "P", "value": 2}, {"value": 3}]}]});
        assert_eq!(
            parse_doc(&other).sections[0].enable_writes,
            vec![("P".to_string(), 2.0)]
        );
    }
}
