//! The Remote ID page of Setup (python/src/js/setup-remoteid.js, the `.rid-*` rules
//! of setup-forms.css). The one page whose subject is not the aircraft: the
//! serial number, the operator registration and the class mark are held by
//! this station and pushed down the link once a second, so the page edits a
//! document that is saved to the config file, with a switch that starts off,
//! advisory checks (the page always saves) and the vehicle's own parameters
//! below.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::check::{self, Check};
use crate::setup::remoteid_model::{self as model, Commit, Opt, Range, VehicleSection};
use crate::setup::schema_form::{FormState, Look};
use crate::ui::card;
use crate::ui::field::TextField;
use crate::ui::formfield as ff;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::pagekit;
use crate::ui::popover;
use crate::ui::style::{self, Weight};

type Status = (&'static str, String);
type Answer = Pending<Result<Value, String>>;

/// A save of part of the identity on its way.
struct Save {
    key: String,
    /// The rows are rebuilt afterwards: the value decides which fields exist.
    repaint: bool,
    answer: Answer,
}

/// What a row of the identity is.
enum Row<'a> {
    Select {
        path: &'a [&'a str],
        key: &'a str,
        label: &'a str,
        options: Vec<Opt>,
        info: &'a str,
        string_value: bool,
        suggestion: Option<(i64, String)>,
    },
    Text {
        path: &'a [&'a str],
        key: &'a str,
        label: &'a str,
        max: usize,
        placeholder: &'a str,
        info: &'a str,
        uppercase: bool,
        serial: bool,
    },
    Number {
        path: &'a [&'a str],
        key: &'a str,
        label: &'a str,
        unit: &'a str,
        range: Range,
        placeholder: &'a str,
        info: &'a str,
    },
}

/// The page's state.
pub struct RemoteIdPage {
    server: Server,
    doc: Option<Value>,
    identity: Value,
    sections: Vec<VehicleSection>,
    loading: Option<Answer>,
    poll: Option<Answer>,
    last_poll: f64,
    status: Status,
    form: FormState,
    check: Check,
    check_card: Option<check::Summary>,
    check_say: Option<check::Summary>,
    saves: Vec<Save>,
    texts: HashMap<String, String>,
    row_status: HashMap<String, Status>,
    invalid: HashSet<String>,
    errors: Vec<String>,
}

impl RemoteIdPage {
    pub fn new(server: Server, ctx: &egui::Context) -> RemoteIdPage {
        let mut p = RemoteIdPage {
            server,
            doc: None,
            identity: model::clone_identity(&model::empty_identity()),
            sections: Vec::new(),
            loading: None,
            poll: None,
            last_poll: f64::MIN,
            status: ("pending", "Reading Remote ID configuration\u{2026}".into()),
            form: FormState::default(),
            check: Check::default(),
            check_card: None,
            check_say: None,
            saves: Vec::new(),
            texts: HashMap::new(),
            row_status: HashMap::new(),
            invalid: HashSet::new(),
            errors: Vec::new(),
        };
        p.load(ctx, false);
        p
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        e.extend(self.form.take_errors());
        e
    }

    /// `fresh` reads the vehicle's parameters from it rather than the cache.
    fn load(&mut self, ctx: &egui::Context, fresh: bool) {
        if self.loading.is_some() {
            return;
        }
        self.status = ("pending", "Reading Remote ID configuration\u{2026}".into());
        let s = self.server.clone();
        let path = if fresh {
            "/api/remoteid?fresh=1"
        } else {
            "/api/remoteid"
        };
        self.loading = Some(Pending::spawn(ctx, move || http::get_json(&s, path)));
    }

    fn take_answers(&mut self, ctx: &egui::Context) {
        if let Some(summary) = self.check.take(&mut self.form.wanted) {
            self.form.mark_checked(&summary.rows);
            self.check_card = Some(summary.clone());
            self.check_say = Some(summary);
            self.load(ctx, true);
        }
        if let Some(r) = self.loading.as_ref().and_then(Pending::take) {
            self.loading = None;
            match r {
                Ok(doc) => {
                    self.identity = model::clone_identity(doc.get("identity").unwrap_or(&Value::Null));
                    self.sections = model::sections(&doc);
                    let (cls, text) = model::loaded_status(&doc);
                    self.status = (cls, text);
                    self.doc = Some(doc);
                    self.texts.clear();
                    self.invalid.clear();
                    if let Some(c) = self.check_say.take() {
                        self.status = (c.cls, c.text);
                    }
                }
                Err(e) => {
                    self.doc = Some(json!({}));
                    self.sections.clear();
                    self.status = (
                        "err",
                        if e.is_empty() {
                            "Could not read the Remote ID configuration".into()
                        } else {
                            e
                        },
                    );
                }
            }
        }
        if let Some(r) = self.poll.as_ref().and_then(Pending::take) {
            self.poll = None;
            if let (Ok(v), Some(doc), None) = (r, self.doc.as_mut(), self.loading.as_ref()) {
                for k in ["status", "findings"] {
                    if let Some(x) = v.get(k) {
                        doc[k] = x.clone();
                    }
                }
            }
        }
        for s in &mut self.sections {
            self.form.take_answers(&mut s.fields);
        }
        let mut done = Vec::new();
        for (i, s) in self.saves.iter().enumerate() {
            if let Some(r) = s.answer.take() {
                done.push((i, r));
            }
        }
        let mut repaint = false;
        for (i, r) in done.into_iter().rev() {
            let s = self.saves.remove(i);
            match r {
                Ok(v) => {
                    if let Some(saved) = v
                        .get("config")
                        .and_then(|c| c.get("remote_id"))
                        .filter(|s| s.is_object())
                    {
                        self.identity = model::clone_identity(saved);
                    }
                    self.row_status.insert(s.key.clone(), ("ok", "saved".into()));
                    repaint |= s.repaint;
                    self.last_poll = f64::MIN;
                }
                Err(e) => {
                    let msg = if e.is_empty() { "save failed".to_string() } else { e };
                    self.row_status.insert(s.key.clone(), ("err", msg.clone()));
                    self.errors
                        .push(format!("Could not save the Remote ID identity: {msg}"));
                }
            }
        }
        if repaint {
            self.row_status.clear();
            self.texts.clear();
        }
    }

    /// `saveIdentity`: persists part of the identity; the backend merges the
    /// patch one level deep and hands the result to the bridge.
    fn save(&mut self, ctx: &egui::Context, key: &str, patch: Value, repaint: bool) {
        self.row_status.insert(key.to_string(), ("pending", "saving".into()));
        let s = self.server.clone();
        let body = model::save_body(&patch);
        self.saves.push(Save {
            key: key.to_string(),
            repaint,
            answer: Pending::spawn(ctx, move || http::post_json(&s, "/api/config", &body)),
        });
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx);
        if self.doc.is_some() && self.loading.is_none() && self.poll.is_none() && now - self.last_poll >= model::POLL_MS
        {
            self.last_poll = now;
            let s = self.server.clone();
            self.poll = Some(Pending::spawn(&ctx, move || http::get_json(&s, "/api/remoteid/status")));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let idle = self.loading.is_none() && !self.check.busy();
        let can_check = idle
            && self
                .doc
                .as_ref()
                .is_some_and(|d| d.get("connected") == Some(&Value::Bool(true)))
            && !model::check_names(&self.sections).is_empty();
        let (mut y, pressed) = pagekit::head_actions(
            ui,
            x,
            y,
            w,
            "Remote ID",
            "The identity this station broadcasts for the aircraft",
            &self.status,
            vec![
                Button::new("Reload", Variant::Primary)
                    .icon("refresh-cw")
                    .small()
                    .actions()
                    .enabled(idle),
                Button::new("Check values", Variant::Secondary)
                    .icon("list-checks")
                    .small()
                    .actions()
                    .title("Read every value back from the vehicle, write again what did not stick, and show what the vehicle holds now")
                    .enabled(can_check),
            ],
            "ridhead",
        );
        match pressed {
            Some(0) => {
                self.check_card = None;
                self.load(&ctx, false);
            }
            Some(1) => {
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
                let names = model::check_names(&self.sections);
                self.check.start(&ctx, &self.server, &wanted, names);
            }
            _ => {}
        }
        // Only the vehicle's own parameters are refused while armed: the
        // identity is this station's and stays live.
        if armed {
            y = pagekit::warning_banner(ui, x, y, w, "Vehicle Remote ID parameters are read-only while armed") + 14.0;
        }
        let Some(doc) = self.doc.clone() else {
            return y;
        };
        let schema = doc.get("schema").cloned().unwrap_or(Value::Null);
        y = self.broadcast_card(ui, &ctx, x, y, w, &schema) + 26.0;
        y = self.status_card(ui, x, y, w, &doc) + 26.0;
        y = self.checks_card(ui, x, y, w, &doc, &schema) + 26.0;
        y = self.identity_grid(ui, &ctx, x, y, w, &doc, &schema, state) + 26.0;
        y = self.classification_card(ui, &ctx, x, y, w, &schema) + 26.0;
        if let Some(c) = self.check_card.clone() {
            y = check::card(ui, x, y, w, &c) + 26.0;
        }
        y = self.vehicle_sections(ui, &ctx, x, y, w, armed);
        y - 26.0
    }

    fn broadcast_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        schema: &Value,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Broadcast") + 10.0;
        cy += ff::hint(
            ui,
            ix,
            cy,
            iw,
            "With this on, Corvus sends the identity below to the aircraft once a second for as long as it is connected. The aircraft passes it to its Remote ID transmitter, and stops arming when the stream stops. Off, nothing is sent at all, which is the right setting until the details below are the ones on your registration.",
        ) + 10.0;
        // the switch
        let row = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, 55.5));
        card::paint(&painter, row, card::Look::Inset);
        let title = style::galley(ui, "Broadcast this identity", 13.0, Weight::W600, style::text_1(), 0.0);
        let sub = style::galley(
            ui,
            "Sent to the connected aircraft, not over the air by this laptop.",
            11.5,
            Weight::W400,
            style::text_2(),
            0.0,
        );
        let th = title.size().y;
        painter.galley(Pos2::new(ix + 13.0, cy + 12.0), title, style::text_1());
        painter.galley(Pos2::new(ix + 13.0, cy + 12.0 + th + 2.0), sub, style::text_2());
        let on = self.identity.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        if kit::switch(
            ui,
            Pos2::new(ix + iw - 13.0 - 38.0, cy + (55.5 - 22.0) / 2.0),
            on,
            true,
            "ridenable",
        ) {
            model::write(&mut self.identity, &[], "enabled", json!(!on));
            self.save(ctx, "enabled", model::patch_for(&[], "enabled", json!(!on)), false);
        }
        cy += 55.5 + 10.0;
        if let Some((cls, text)) = self.row_status.get("enabled").cloned() {
            let col = pagekit::status_color(cls);
            let g = style::galley(ui, &text, 10.5, Weight::W400, col, 0.0);
            let h = g.size().y;
            painter.galley(Pos2::new(ix, cy), g, col);
            cy += h;
        }
        cy += 10.0;
        let regions = model::options(schema, "regions", &[("eu", "European Union")]);
        cy = self.row(
            ui,
            ctx,
            ix,
            cy,
            iw,
            Row::Select {
                path: &[],
                key: "region",
                label: "Check against",
                options: regions,
                info: "Which published broadcast format the list below is measured against. It changes what Corvus checks, never what it sends.",
                string_value: true,
                suggestion: None,
            },
            true,
        );
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    fn status_card(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, doc: &Value) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Right now") + 10.0;
        let narrow = crate::app::responsive::Vw::of(ui.ctx()).max720;
        let rows = model::status_rows(doc.get("status").unwrap_or(&Value::Null));
        for (i, r) in rows.iter().enumerate() {
            let color = match r.level {
                "healthy" => style::healthy(),
                "warning" => style::warning(),
                "critical" => style::critical(),
                _ => style::text_disabled(),
            };
            kit::dot(&painter, Pos2::new(ix + 3.5, cy + 7.5), color, r.level == "healthy");
            let mut tx = ix + 7.0 + 9.0;
            if !narrow {
                let g = style::galley(ui, r.label, 12.0, Weight::W400, style::text_2(), 0.0);
                painter.galley(Pos2::new(tx, cy + 7.5 - g.size().y / 2.0), g, style::text_2());
                tx += 74.0 + 9.0;
            }
            let g = kit::wrapped_weight(
                ui,
                &r.text,
                12.5,
                Weight::W400,
                style::text_1(),
                (ix + iw - tx).max(60.0),
                1.21,
            );
            let h = g.size().y.max(15.0);
            painter.galley(Pos2::new(tx, cy), g, style::text_1());
            cy += h;
            if i + 1 < rows.len() {
                cy += 7.0;
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

    fn checks_card(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, doc: &Value, schema: &Value) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "What the rules ask for") + 10.0;
        let hint = format!(
            "Measured against {}. This is a reading of the published broadcast format, not legal advice and not a certification. The filing is yours.",
            model::region_label(schema, &self.identity)
        );
        cy += ff::hint(ui, ix, cy, iw, &hint) + 10.0;
        let findings = model::findings(doc);
        let rows: Vec<(String, &str)> = if findings.is_empty() {
            vec![(
                "Everything this region's broadcast format asks for is filled in.".into(),
                "ok",
            )]
        } else {
            findings
                .iter()
                .map(|f| (f.text.clone(), if f.level == "error" { "error" } else { "warning" }))
                .collect::<Vec<_>>()
        };
        let rows: Vec<(String, String)> = rows.into_iter().map(|(t, l)| (t, l.to_string())).collect();
        for (i, (text, level)) in rows.iter().enumerate() {
            let (icon, color) = match level.as_str() {
                "ok" => ("check", style::healthy()),
                "error" => ("triangle-alert", style::critical()),
                _ => ("info", style::warning()),
            };
            crate::ui::icons::paint(
                ui,
                icon,
                Rect::from_min_size(Pos2::new(ix, cy + 2.0), Vec2::splat(14.0)),
                color,
            );
            let g = kit::wrapped_weight(ui, text, 12.5, Weight::W400, style::text_1(), iw - 22.0, 1.45);
            let h = g.size().y;
            painter.galley(Pos2::new(ix + 22.0, cy), g, style::text_1());
            cy += h;
            if i + 1 < rows.len() {
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

    /// `.rid-grid`: four cards, two columns from 894 px (`minmax(440px, 1fr)`
    /// and a 14 px gap), else one.
    #[allow(clippy::too_many_arguments)]
    fn identity_grid(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        doc: &Value,
        schema: &Value,
        state: &Value,
    ) -> f32 {
        let two = w >= 2.0 * 440.0 + 14.0;
        let cw = if two { (w - 14.0) / 2.0 } else { w };
        let mut bottoms = [y; 4];
        let mut tops = [y; 4];
        let mut row_bottom = y;
        for i in 0..4 {
            let top = if two {
                if i % 2 == 0 && i > 0 {
                    row_bottom + 26.0
                } else if i % 2 == 1 {
                    tops[i - 1]
                } else {
                    y
                }
            } else if i == 0 {
                y
            } else {
                bottoms[i - 1] + 26.0
            };
            tops[i] = top;
            let cx = if two && i % 2 == 1 { x + cw + 14.0 } else { x };
            bottoms[i] = match i {
                0 => self.basic_card(ui, ctx, cx, top, cw, doc, schema),
                1 => self.operator_card(ui, ctx, cx, top, cw, schema),
                2 => self.self_card(ui, ctx, cx, top, cw, schema),
                _ => self.location_card(ui, ctx, cx, top, cw, schema, state),
            };
            row_bottom = if two && i % 2 == 1 {
                bottoms[i].max(bottoms[i - 1])
            } else {
                bottoms[i]
            };
        }
        row_bottom
    }

    /// One identity card: a title, an optional hint and its rows.
    #[allow(clippy::too_many_arguments)]
    fn card(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        title: &str,
        hint: &str,
        body: impl FnOnce(&mut Self, &mut egui::Ui, f32, f32, f32) -> f32,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, title) + 10.0;
        if !hint.is_empty() {
            cy += ff::hint(ui, ix, cy, iw, hint) + 10.0;
        }
        cy = body(self, ui, ix, cy, iw);
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    #[allow(clippy::too_many_arguments)]
    fn basic_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        doc: &Value,
        schema: &Value,
    ) -> f32 {
        let ua_types = model::options(schema, "ua_types", &[]);
        let suggestion = model::suggestion(doc, &self.identity, &ua_types);
        let ctx = ctx.clone();
        let schema = schema.clone();
        self.card(
            ui,
            x,
            y,
            w,
            "Basic ID: the aircraft",
            "What identifies the airframe. Under both the FAA's and the EU's rules this is normally the manufacturer's serial number in ANSI/CTA-2063-A form: four characters of manufacturer code, one character saying how long the rest is, then the serial. It is on the airframe's label, not in its parameters.",
            |p, ui, ix, cy, iw| {
                let mut cy = p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Select {
                        path: &["basic_id"],
                        key: "id_type",
                        label: "ID type",
                        options: model::options(&schema, "id_types", &[]),
                        info: "What kind of identifier the field below is. Getting this wrong broadcasts a correct number under the wrong heading.",
                        string_value: false,
                        suggestion: None,
                    },
                    false,
                );
                cy = p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Text {
                        path: &["basic_id"],
                        key: "uas_id",
                        label: "Aircraft ID",
                        max: model::limit(&schema, "uas_id", 20),
                        placeholder: "e.g. 1596F483658SK8U6PNJ1",
                        info: "Exactly as it appears on the aircraft. Capitals and digits only, without the letters I and O.",
                        uppercase: true,
                        serial: true,
                    },
                    false,
                );
                p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Select {
                        path: &["basic_id"],
                        key: "ua_type",
                        label: "Aircraft type",
                        options: ua_types.clone(),
                        info: "What the aircraft is, as it was registered.",
                        string_value: false,
                        suggestion: suggestion.clone(),
                    },
                    true,
                )
            },
        )
    }

    fn operator_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, schema: &Value) -> f32 {
        let ctx = ctx.clone();
        let schema = schema.clone();
        self.card(
            ui,
            x,
            y,
            w,
            "Operator ID: you",
            "The registration number your authority issued to you as an operator, which is not the same as the aircraft's. In the EU it is the 16 characters that start with your country code. The three characters after them are the secret half of your registration and are never broadcast. Do not type them here.",
            |p, ui, ix, cy, iw| {
                let cy = p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Select {
                        path: &["operator_id"],
                        key: "operator_id_type",
                        label: "Type",
                        options: model::options(&schema, "operator_id_types", &[]),
                        info: "",
                        string_value: false,
                        suggestion: None,
                    },
                    false,
                );
                p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Text {
                        path: &["operator_id"],
                        key: "operator_id",
                        label: "Operator registration",
                        max: model::limit(&schema, "operator_id", 20),
                        placeholder: "e.g. FIN87astrdge12k8",
                        info: "Under the FAA's rule this field is optional; under the EU's it is what the broadcast is for.",
                        uppercase: false,
                        serial: false,
                    },
                    true,
                )
            },
        )
    }

    fn self_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, schema: &Value) -> f32 {
        let ctx = ctx.clone();
        let schema = schema.clone();
        self.card(
            ui,
            x,
            y,
            w,
            "Self ID: the flight",
            "A free line about what this flight is, broadcast alongside the two IDs. It is what somebody on the ground reads when they wonder why an aircraft is over their field, so it is worth filling in even where nothing requires it. Switching the type to Emergency is how an operator says so while the aircraft is still in the air.",
            |p, ui, ix, cy, iw| {
                let cy = p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Select {
                        path: &["self_id"],
                        key: "description_type",
                        label: "Type",
                        options: model::options(&schema, "description_types", &[]),
                        info: "",
                        string_value: false,
                        suggestion: None,
                    },
                    false,
                );
                p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Text {
                        path: &["self_id"],
                        key: "description",
                        label: "Description",
                        max: model::limit(&schema, "description", 23),
                        placeholder: "e.g. Survey flight, Hall 7",
                        info: "Short enough to read off a phone screen.",
                        uppercase: false,
                        serial: false,
                    },
                    true,
                )
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn location_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        schema: &Value,
        state: &Value,
    ) -> f32 {
        let ctx = ctx.clone();
        let schema = schema.clone();
        let fixed = model::read(&self.identity, &["system"], "operator_location_type").as_i64() == Some(2);
        let position = model::vehicle_position(state);
        let altitude = Range {
            min: -1000.0,
            max: 31767.0,
            unknown: Some(model::ALTITUDE_UNKNOWN),
        };
        self.card(
            ui,
            x,
            y,
            w,
            "Operator position",
            "Where the broadcast says the person flying is standing. The take-off point is what the aircraft fills in for itself; a fixed position is one you type. The FAA's rule wants the control station's own position, so the take-off point does not satisfy it.",
            |p, ui, ix, cy, iw| {
                let mut cy = p.row(
                    ui,
                    &ctx,
                    ix,
                    cy,
                    iw,
                    Row::Select {
                        path: &["system"],
                        key: "operator_location_type",
                        label: "Position source",
                        options: model::options(&schema, "location_types", &[]),
                        info: "",
                        string_value: false,
                        suggestion: None,
                    },
                    !fixed,
                );
                if fixed {
                    cy = p.row(
                        ui,
                        &ctx,
                        ix,
                        cy,
                        iw,
                        Row::Number {
                            path: &["system"],
                            key: "operator_latitude",
                            label: "Latitude",
                            unit: "\u{b0}",
                            range: Range { min: -90.0, max: 90.0, unknown: None },
                            placeholder: "",
                            info: "",
                        },
                        false,
                    );
                    cy = p.row(
                        ui,
                        &ctx,
                        ix,
                        cy,
                        iw,
                        Row::Number {
                            path: &["system"],
                            key: "operator_longitude",
                            label: "Longitude",
                            unit: "\u{b0}",
                            range: Range { min: -180.0, max: 180.0, unknown: None },
                            placeholder: "",
                            info: "",
                        },
                        false,
                    );
                    cy = p.row(
                        ui,
                        &ctx,
                        ix,
                        cy,
                        iw,
                        Row::Number {
                            path: &["system"],
                            key: "operator_altitude_geo",
                            label: "Height above the ellipsoid",
                            unit: "m",
                            range: altitude,
                            placeholder: "not declared",
                            info: "WGS-84 height, not height above ground. Leave it empty when you do not know it. The broadcast has a value that means exactly that, and a guessed number does not.",
                        },
                        true,
                    );
                    cy += 10.0;
                    let b = Button::new("Take the aircraft's position", Variant::Ghost)
                        .small()
                        .icon("map-pin")
                        .enabled(position.is_some());
                    let (bw, bh) = (b.width(ui), b.height());
                    if b.show(ui, Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(bw, bh)), "ridtake") {
                        if let Some((lat, lon)) = position {
                            for (k, v) in [("operator_latitude", lat), ("operator_longitude", lon)] {
                                model::write(&mut p.identity, &["system"], k, json!(v));
                            }
                            p.save(
                                &ctx,
                                "take-position",
                                json!({"system": {"operator_latitude": lat, "operator_longitude": lon}}),
                                true,
                            );
                        }
                    }
                    let mut tx = ix + bw + 8.0;
                    if let Some((cls, text)) = p.row_status.get("take-position").cloned() {
                        let col = pagekit::status_color(cls);
                        let g = style::galley(ui, &text, 10.5, Weight::W400, col, 0.0);
                        let gw = g.size().x;
                        let gh = g.size().y;
                        ui.painter().galley(Pos2::new(tx, cy + (bh - gh) / 2.0), g, col);
                        tx += gw + 8.0;
                    }
                    let g = kit::wrapped_weight(
                        ui,
                        "An operator normally stands within a few metres of the aircraft before take-off, which makes its position the fastest honest way to fill these in.",
                        10.5,
                        Weight::W400,
                        style::text_3(),
                        (ix + iw - tx).max(120.0),
                        1.4,
                    );
                    let gh = g.size().y;
                    ui.painter().galley(Pos2::new(tx, cy + (bh - gh).max(0.0) / 2.0), g, style::text_3());
                    cy += bh.max(gh);
                }
                cy
            },
        )
    }

    fn classification_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        schema: &Value,
    ) -> f32 {
        let ctx = ctx.clone();
        let schema = schema.clone();
        let declared = model::read(&self.identity, &["system"], "classification_type").as_i64() == Some(1);
        self.card(
            ui,
            x,
            y,
            w,
            "EU vehicle info",
            "The European classification: which operational category the flight is in, and which class mark the airframe carries. Declare it and both are broadcast; leave it undeclared and neither is sent, which is the right setting outside the EU. The operational volume underneath is for the Specific category, where the flight is flown inside an approved area rather than within sight.",
            |p, ui, ix, cy, iw| {
                let sel = |key: &'static str, label: &'static str, list: &str, info: &'static str| Row::Select {
                    path: &["system"],
                    key,
                    label,
                    options: model::options(&schema, list, &[]),
                    info,
                    string_value: false,
                    suggestion: None,
                };
                let mut cy = p.row(ui, &ctx, ix, cy, iw, sel("classification_type", "Classification", "classification_types", ""), !declared);
                if declared {
                    cy = p.row(
                        ui,
                        &ctx,
                        ix,
                        cy,
                        iw,
                        sel(
                            "category_eu",
                            "Operational category",
                            "categories_eu",
                            "Open, Specific or Certified: the category the flight is authorised under, not the aircraft's own class.",
                        ),
                        false,
                    );
                    cy = p.row(
                        ui,
                        &ctx,
                        ix,
                        cy,
                        iw,
                        sel(
                            "class_eu",
                            "Class mark",
                            "classes_eu",
                            "The C0-C6 label on the airframe or in its declaration of conformity.",
                        ),
                        false,
                    );
                    let unknown = Some(model::ALTITUDE_UNKNOWN);
                    let numbers: [(&str, &str, &str, Range, &str, &str); 4] = [
                        (
                            "area_count",
                            "Operational areas",
                            "",
                            Range { min: 1.0, max: 65535.0, unknown: None },
                            "",
                            "How many volumes the flight is authorised in. 1 unless the authorisation says otherwise.",
                        ),
                        (
                            "area_radius",
                            "Area radius",
                            "m",
                            Range { min: 0.0, max: 65535.0, unknown: None },
                            "",
                            "Radius of the authorised volume around the aircraft. 0 declares no radius.",
                        ),
                        (
                            "area_ceiling",
                            "Area ceiling",
                            "m",
                            Range { min: -1000.0, max: 31767.0, unknown },
                            "not declared",
                            "Upper limit of the authorised volume, above the ellipsoid.",
                        ),
                        (
                            "area_floor",
                            "Area floor",
                            "m",
                            Range { min: -1000.0, max: 31767.0, unknown },
                            "not declared",
                            "Lower limit of the authorised volume, above the ellipsoid.",
                        ),
                    ];
                    for (i, (key, label, unit, range, placeholder, info)) in numbers.into_iter().enumerate() {
                        cy = p.row(
                            ui,
                            &ctx,
                            ix,
                            cy,
                            iw,
                            Row::Number {
                                path: &["system"],
                                key,
                                label,
                                unit,
                                range,
                                placeholder,
                                info,
                            },
                            i == 3,
                        );
                    }
                }
                cy
            },
        )
    }

    fn vehicle_sections(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        mut y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let painter = ui.painter().clone();
        let (ix, iw) = (x + 19.0, w - 38.0);
        let server = self.server.clone();
        let sections = self.sections.clone();
        for s in &sections {
            let bg = card::reserve(&painter);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, &s.title) + 10.0;
            if !s.hint.is_empty() {
                cy += ff::hint(ui, ix, cy, iw, &s.hint) + 10.0;
            }
            if !s.fields.is_empty() {
                cy = self
                    .form
                    .show_with(ui, ctx, &server, &s.fields, ix, cy, iw, armed, Look::CALIB);
            }
            card::fill(
                &painter,
                bg,
                Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
                card::Look::Page,
            );
            y = cy + 19.0 + 26.0;
        }
        y
    }

    /// One `.pform-field.rid-field` row: a 110 to 175 px label column, the
    /// control, its counter or unit and its status. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    fn row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, row: Row<'_>, last: bool) -> f32 {
        let painter = ui.painter().clone();
        let narrow = crate::app::responsive::Vw::of(ctx).max720;
        let (path, key, label, info) = match &row {
            Row::Select {
                path, key, label, info, ..
            }
            | Row::Text {
                path, key, label, info, ..
            }
            | Row::Number {
                path, key, label, info, ..
            } => (*path, *key, *label, *info),
        };
        let id = format!("{}.{key}", path.join("."));
        let label_w = if narrow {
            w
        } else {
            (w - 12.0 - 150.0).clamp(110.0, 175.0)
        };
        let cx = if narrow { x } else { x + label_w + 12.0 };
        let cw = if narrow { w } else { w - label_w - 12.0 };
        let lg = style::galley(ui, label, 12.0, Weight::W400, style::text_1(), 0.0);
        let lh = if info.is_empty() { lg.size().y } else { 16.0 };
        let above = if narrow { lh + 6.0 } else { 0.0 };
        let control_h: f32 = match &row {
            Row::Select { .. } => 28.5,
            _ => 29.5,
        };
        let top = y + 7.0 + above;
        let mid = top
            + if narrow {
                control_h / 2.0
            } else {
                control_h.max(lh) / 2.0
            };
        let (label_y, label_mid) = if narrow {
            (y + 7.0, y + 7.0 + lh / 2.0)
        } else {
            (mid - lg.size().y / 2.0, mid)
        };
        let lw = lg.size().x;
        painter.galley(Pos2::new(x, label_y), lg, style::text_1());
        if !info.is_empty() {
            popover::info(
                ui,
                Pos2::new(x + lw + 6.0 + 8.0, label_mid),
                label,
                info,
                ("ridinfo", &id),
            );
        }
        let status = self.row_status.get(&id).cloned();
        let status_g = status
            .as_ref()
            .map(|(cls, t)| style::galley(ui, t, 10.5, Weight::W400, pagekit::status_color(cls), 0.0));
        let status_w = status_g.as_ref().map_or(0.0, |g| g.size().x);
        let mut end_x: f32;
        let mut extra_below = 0.0;
        match row {
            Row::Select {
                options,
                string_value,
                suggestion,
                ..
            } => {
                let current = model::read(&self.identity, path, key).clone();
                let selected = opt_key(&current);
                let choices: Vec<Choice> = options
                    .iter()
                    .map(|o| Choice {
                        value: opt_key(&o.value),
                        label: o.label.clone(),
                        enabled: !o.disabled,
                    })
                    .collect();
                let widest = options
                    .iter()
                    .map(|o| {
                        style::galley(ui, &o.label, 12.0, Weight::W400, style::text_1(), 0.0)
                            .size()
                            .x
                    })
                    .fold(0.0, f32::max);
                let sw = (widest + 9.0 + 26.0 + 2.0)
                    .clamp(150.0, 320.0)
                    .min((cw - 8.0 - status_w).max(100.0));
                let rect = Rect::from_min_size(Pos2::new(cx, top), Vec2::new(sw, control_h));
                if let Some(p) = kit::select_padded(
                    ui,
                    rect,
                    &choices,
                    &selected,
                    "",
                    false,
                    "",
                    ("ridsel", &id),
                    (9.0, 26.0),
                ) {
                    let next = if string_value {
                        json!(p)
                    } else {
                        json!(p.parse::<f64>().map(|v| v as i64).unwrap_or(0))
                    };
                    model::write(&mut self.identity, path, key, next.clone());
                    self.save(ctx, &id, model::patch_for(path, key, next), true);
                }
                end_x = cx + sw;
                if let Some((value, text)) = suggestion {
                    let b = Button::new(&text, Variant::Ghost).small();
                    let (bw, bh) = (b.width(ui), b.height());
                    let by = top + control_h + 6.0;
                    if b.show(
                        ui,
                        Rect::from_min_size(Pos2::new(cx, by), Vec2::new(bw, bh)),
                        ("ridsuggest", &id),
                    ) {
                        model::write(&mut self.identity, path, key, json!(value));
                        self.save(ctx, &id, model::patch_for(path, key, json!(value)), true);
                    }
                    extra_below = 6.0 + bh;
                }
            }
            Row::Text {
                max,
                placeholder,
                uppercase,
                serial,
                ..
            } => {
                let stored = model::read(&self.identity, path, key)
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let mut text = self.texts.get(&id).cloned().unwrap_or(stored);
                let counter_g = style::galley(
                    ui,
                    &model::counter(&text, max),
                    11.0,
                    Weight::W400,
                    style::text_3(),
                    0.0,
                );
                let cwid = counter_g.size().x;
                let input_w = (cw - (cwid + 8.0) - (status_w + 8.0)).clamp(40.0, 260.0);
                let rect = Rect::from_min_size(Pos2::new(cx, top), Vec2::new(input_w, control_h));
                let before = text.clone();
                let bad = self.invalid.contains(&id);
                let resp = TextField::new(&mut text)
                    .hint(placeholder)
                    .mono(true)
                    .pad(9.0, 9.0)
                    .focus_color(if bad { style::critical() } else { style::accent() })
                    .show(ui, rect, ("ridtext", &id));
                if resp.changed() || text != before {
                    text = model::typed_text(&text, uppercase, max);
                    let problem = if serial {
                        let t = model::read(&self.identity, &["basic_id"], "id_type")
                            .as_i64()
                            .unwrap_or(1);
                        model::serial_problem(t, &text)
                    } else {
                        String::new()
                    };
                    if problem.is_empty() {
                        self.invalid.remove(&id);
                        self.row_status.remove(&id);
                    } else {
                        self.invalid.insert(id.clone());
                        self.row_status.insert(id.clone(), ("err", problem));
                    }
                    self.texts.insert(id.clone(), text.clone());
                }
                if bad {
                    painter.rect_stroke(
                        rect,
                        style::RADIUS_BTN,
                        Stroke::new(1.0, style::critical()),
                        egui::StrokeKind::Inside,
                    );
                }
                let cg = style::galley(
                    ui,
                    &model::counter(&text, max),
                    11.0,
                    Weight::W400,
                    style::text_3(),
                    0.0,
                );
                let cgh = cg.size().y;
                painter.galley(
                    Pos2::new(cx + input_w + 8.0, top + (control_h - cgh) / 2.0),
                    cg,
                    style::text_3(),
                );
                end_x = cx + input_w + 8.0 + cwid;
                // 'change': committed when the box is left with an edit
                if resp.lost_focus() && self.texts.contains_key(&id) {
                    let value = text.trim().to_string();
                    self.texts.remove(&id);
                    model::write(&mut self.identity, path, key, json!(value));
                    self.save(ctx, &id, model::patch_for(path, key, json!(value)), false);
                    // the problem stays on screen while it is true
                    if self.invalid.contains(&id) {
                        self.row_status.entry(id.clone()).or_insert(("err", String::new()));
                    }
                }
            }
            Row::Number {
                unit,
                range,
                placeholder,
                ..
            } => {
                let raw = model::read(&self.identity, path, key).as_f64().unwrap_or(0.0);
                let mut text = self
                    .texts
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| model::number_text(raw, range));
                let unit_g =
                    (!unit.is_empty()).then(|| style::galley(ui, unit, 11.0, Weight::W400, style::text_3(), 0.0));
                let unit_w = unit_g.as_ref().map_or(0.0, |g| g.size().x + 8.0);
                let input_w = (cw - unit_w - (status_w + 8.0)).clamp(40.0, 260.0);
                let rect = Rect::from_min_size(Pos2::new(cx, top), Vec2::new(input_w, control_h));
                let bad = self.invalid.contains(&id);
                let resp = TextField::new(&mut text)
                    .hint(placeholder)
                    .mono(true)
                    .pad(9.0, 9.0)
                    .focus_color(if bad { style::critical() } else { style::accent() })
                    .show(ui, rect, ("ridnum", &id));
                if resp.changed() {
                    self.texts.insert(id.clone(), text.clone());
                }
                if bad {
                    painter.rect_stroke(
                        rect,
                        style::RADIUS_BTN,
                        Stroke::new(1.0, style::critical()),
                        egui::StrokeKind::Inside,
                    );
                }
                end_x = cx + input_w;
                if let Some(g) = unit_g {
                    let gh = g.size().y;
                    painter.galley(
                        Pos2::new(end_x + 8.0, top + (control_h - gh) / 2.0),
                        g.clone(),
                        style::text_3(),
                    );
                    end_x += 8.0 + g.size().x;
                }
                if resp.lost_focus() && self.texts.contains_key(&id) {
                    let typed = text.clone();
                    match model::commit_number(&typed, range) {
                        Commit::Unknown => {
                            self.invalid.remove(&id);
                            self.texts.remove(&id);
                            self.row_status.remove(&id);
                            let v = json!(model::ALTITUDE_UNKNOWN);
                            model::write(&mut self.identity, path, key, v.clone());
                            self.save(ctx, &id, model::patch_for(path, key, v), false);
                        }
                        Commit::Value(n) => {
                            self.invalid.remove(&id);
                            self.texts.remove(&id);
                            self.row_status.remove(&id);
                            model::write(&mut self.identity, path, key, json!(n));
                            self.save(ctx, &id, model::patch_for(path, key, json!(n)), false);
                        }
                        Commit::Problem(p) => {
                            self.invalid.insert(id.clone());
                            self.row_status.insert(id.clone(), ("err", p));
                        }
                    }
                }
            }
        }
        if let (Some(g), Some((cls, _))) = (status_g, status) {
            let col = pagekit::status_color(cls);
            let gh = g.size().y;
            let sx = end_x + 8.0;
            painter.galley(Pos2::new(sx, top + (control_h - gh) / 2.0), g, col);
        }
        let h = 7.0 + above + control_h.max(if narrow { 0.0 } else { lh }) + extra_below + 7.0;
        let bottom = y + h;
        if !last {
            painter.hline(
                egui::Rangef::new(x, x + w),
                bottom + 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
            return bottom + 1.0;
        }
        bottom
    }
}

/// The select value of an option: a string as it is, a number as an integer.
fn opt_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(|f| (f.round() as i64).to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> (RemoteIdPage, egui::Context) {
        let ctx = egui::Context::default();
        let mut p = RemoteIdPage::new(Server::local(1), &ctx);
        p.loading = None;
        (p, ctx)
    }

    #[test]
    fn a_saved_identity_replaces_the_local_one_and_asks_for_the_checks_again() {
        let (mut p, ctx) = page();
        p.last_poll = 5.0;
        p.saves.push(Save {
            key: "basic_id.uas_id".into(),
            repaint: false,
            answer: Pending::ready(Ok(
                json!({"ok": true, "config": {"remote_id": {"enabled": true, "basic_id": {"uas_id": "ABCD3XYZ"}}}}),
            )),
        });
        p.take_answers(&ctx);
        assert_eq!(p.identity["enabled"], true);
        assert_eq!(p.identity["basic_id"]["uas_id"], "ABCD3XYZ");
        assert_eq!(
            p.identity["basic_id"]["id_type"], 1,
            "what was not saved keeps its default"
        );
        assert_eq!(p.row_status["basic_id.uas_id"], ("ok", "saved".into()));
        assert_eq!(p.last_poll, f64::MIN);
    }

    #[test]
    fn a_refused_save_is_said_on_its_row_and_on_the_board() {
        let (mut p, ctx) = page();
        p.saves.push(Save {
            key: "enabled".into(),
            repaint: false,
            answer: Pending::ready(Err("disk full".into())),
        });
        p.take_answers(&ctx);
        assert_eq!(p.row_status["enabled"], ("err", "disk full".into()));
        assert_eq!(
            p.take_errors(),
            vec!["Could not save the Remote ID identity: disk full".to_string()]
        );
    }

    #[test]
    fn a_save_that_changes_which_fields_exist_clears_the_rows_state() {
        let (mut p, ctx) = page();
        p.texts.insert("system.operator_latitude".into(), "4".into());
        p.saves.push(Save {
            key: "system.operator_location_type".into(),
            repaint: true,
            answer: Pending::ready(Ok(json!({"ok": true}))),
        });
        p.take_answers(&ctx);
        assert!(p.texts.is_empty() && p.row_status.is_empty());
    }

    #[test]
    fn the_poll_moves_the_status_and_the_findings_but_not_the_form() {
        let (mut p, ctx) = page();
        p.doc = Some(
            json!({"identity": {"enabled": false}, "schema": {"x": 1}, "status": {"enabled": false}, "findings": []}),
        );
        p.poll = Some(Pending::ready(Ok(
            json!({"status": {"enabled": true}, "findings": [{"level": "error", "text": "t"}], "identity": {"enabled": true}}),
        )));
        p.take_answers(&ctx);
        let doc = p.doc.unwrap();
        assert_eq!(doc["status"]["enabled"], true);
        assert_eq!(doc["findings"][0]["text"], "t");
        assert_eq!(doc["identity"]["enabled"], false);
        assert_eq!(doc["schema"]["x"], 1);
    }

    #[test]
    fn a_read_without_a_vehicle_says_the_identity_is_still_editable() {
        let (mut p, ctx) = page();
        p.loading = Some(Pending::ready(Ok(
            json!({"connected": false, "error": "", "identity": {"region": "eu"}, "sections": []}),
        )));
        p.take_answers(&ctx);
        assert_eq!(p.status.1, "No vehicle connected. The identity below is still editable");
        assert!(p.sections.is_empty());
        p.loading = Some(Pending::ready(Err(String::new())));
        p.take_answers(&ctx);
        assert_eq!(p.status, ("err", "Could not read the Remote ID configuration".into()));
    }

    #[test]
    fn option_values_become_select_keys() {
        assert_eq!(opt_key(&json!(2)), "2");
        assert_eq!(opt_key(&json!(2.0)), "2");
        assert_eq!(opt_key(&json!("eu")), "eu");
        assert_eq!(opt_key(&Value::Null), "");
    }
}
