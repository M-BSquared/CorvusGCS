//! The Battery & Power page of Setup (src/js/setup-battery.js, the
//! `.battery-*` rules of setup-forms.css).
//!
//! Two columns. At the left the aircraft: a battery drawn with the cells it
//! has, filled to what is left, the failsafe levels marked on it, and every
//! live number under it. At the right everything that can be changed: how
//! Corvus reads the pack (its own settings, kept in the config and editable
//! while armed) and then the vehicle's parameters the backend describes. A
//! cell count typed at the right shows in the drawing at once.

use std::collections::HashMap;

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::{json, Value};

use crate::data::units::Units;
use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::battery_model::{self as model, Anchor, Doc, Flag, Live, Settings};
use crate::setup::check::{self, Check};
use crate::setup::schema_form::{self, FormState, Look};
use crate::ui::card;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::pagekit;
use crate::ui::popover;
use crate::ui::style::{self, Weight};

const X0: f32 = 8.0;
const X1: f32 = 228.0;
const Y0: f32 = 16.0;
const Y1: f32 = 86.0;
const FIGURE_MAX_W: f32 = 360.0;
/// Sans ascent over the em, to put a text's baseline where the SVG does.
const ASCENT: f32 = 0.969;

type Status = (&'static str, String);
type Save = (Vec<&'static str>, Pending<Result<Value, String>>);

/// One estimator number: its key, label, unit, range and what it says.
struct Spec {
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    max: f64,
    info: &'static str,
}

const SPECS: [Spec; 4] = [
    Spec {
        key: "cells",
        label: "Cells in series",
        unit: "S",
        max: 24.0,
        info: "0 works it out from the pack voltage when the battery is plugged in, which is the only moment that reading is unambiguous: 19.8 V is a fresh 5S and a tired 6S. Pin it if you know it.",
    },
    Spec {
        key: "full_cell",
        label: "Full cell voltage",
        unit: "V",
        max: 5.0,
        info: "Resting volts of one charged cell, at which the estimate reads 100%.",
    },
    Spec {
        key: "empty_cell",
        label: "Empty cell voltage",
        unit: "V",
        max: 5.0,
        info: "Where the estimate reads 0%. It is a landing point, not the cell's datasheet minimum: the default leaves about 5% in the pack, just before the voltage drops away. Raise it for more reserve, never lower it below the default.",
    },
    Spec {
        key: "resistance",
        label: "Internal resistance",
        unit: "m\u{3a9}/cell",
        max: 100.0,
        info: "Corrects the voltage back to rest before it is read. A 6S pack pulling 60 A through 5 m\u{3a9} a cell reads 1.8 V low: thirty points of charge, in a climb. 0 leaves the reading uncorrected, which is pessimistic rather than wrong.",
    },
];

const CHEMISTRY_INFO: &str = "Which discharge curve the cell voltage is read against. A LiFePO4 cell at 3.3 V is nearly full; a LiPo cell at 3.3 V is empty.";

const ESTIMATE_INFO: &str = "Autopilot (off): counts the charge used since power-on against the capacity you set. Precise in flight with a calibrated current sensor, but the count starts from a guess at power-on (ArduPilot assumes a full pack), so a pack flown, set aside and plugged in again can read far too high.\nCell voltage (on): reads the cell voltage against the chemistry's discharge curve. Right from the moment the pack is plugged in, whatever it went through before. Under load the voltage sags, so set the internal resistance or it reads low in a climb.\nTurn it on when packs are not always freshly charged or the current sensor is not calibrated. Leave it off when every flight starts on a full pack and the current reading is trusted.\nBoth figures stay visible on the left, whichever you choose.";

/// The page's state.
pub struct BatteryPage {
    server: Server,
    doc: Option<Doc>,
    loading: Option<Pending<Result<Value, String>>>,
    status: Status,
    form: FormState,
    check: Check,
    check_card: Option<check::Summary>,
    check_say: Option<check::Summary>,
    configured: Settings,
    typed: HashMap<&'static str, String>,
    setting_status: HashMap<&'static str, Status>,
    saves: Vec<Save>,
    errors: Vec<String>,
    /// A read was made after the link came up, so it is not repeated.
    retried: bool,
}

impl BatteryPage {
    pub fn new(server: Server, ctx: &egui::Context) -> BatteryPage {
        let mut p = BatteryPage {
            server,
            doc: None,
            loading: None,
            status: ("pending", "Reading battery configuration\u{2026}".into()),
            form: FormState::default(),
            check: Check::default(),
            check_card: None,
            check_say: None,
            configured: Settings::default(),
            typed: HashMap::new(),
            setting_status: HashMap::new(),
            saves: Vec::new(),
            errors: Vec::new(),
            retried: false,
        };
        p.load(ctx);
        p
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        let mut e = std::mem::take(&mut self.errors);
        e.extend(self.form.take_errors());
        e
    }

    /// Reads fresh, never the backend's cache: the page shows what the
    /// vehicle holds now.
    fn load(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        self.status = ("pending", "Reading battery configuration\u{2026}".into());
        let s = self.server.clone();
        self.loading = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/battery?fresh=1")));
    }

    fn check_names(doc: &Doc) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for s in &doc.sections {
            for f in &s.fields {
                if !names.contains(&f.param) {
                    names.push(f.param.clone());
                }
            }
        }
        names
    }

    fn take_answers(&mut self, ctx: &egui::Context) {
        if let Some(summary) = self.check.take(&mut self.form.wanted) {
            self.form.mark_checked(&summary.rows);
            self.check_card = Some(summary.clone());
            self.check_say = Some(summary);
            self.load(ctx);
        }
        if let Some(r) = self.loading.as_ref().and_then(Pending::take) {
            self.loading = None;
            match r {
                Ok(v) => {
                    let doc = model::parse_doc(&v);
                    self.configured = doc.configured.clone();
                    self.typed.clear();
                    self.status = if doc.connected {
                        ("ok", format!("{} parameters read", doc.received))
                    } else {
                        (
                            "err",
                            if doc.error.is_empty() {
                                "No battery configuration received".into()
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
                            "Could not read battery configuration".into()
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
        }
        let mut done = Vec::new();
        for (i, (_, p)) in self.saves.iter().enumerate() {
            if let Some(r) = p.take() {
                done.push((i, r));
            }
        }
        for (i, r) in done.into_iter().rev() {
            let (keys, _) = self.saves.remove(i);
            match r {
                Ok(v) => {
                    let saved = v
                        .get("config")
                        .and_then(|c| c.get("battery"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    self.configured = Settings::from_value(&saved);
                    for k in &keys {
                        self.typed.remove(k);
                        self.setting_status.remove(k);
                    }
                }
                Err(e) => {
                    let msg = if e.is_empty() { "save failed".to_string() } else { e };
                    for k in &keys {
                        self.setting_status.insert(k, ("err", msg.clone()));
                    }
                    self.errors.push(format!("Could not save the battery settings: {msg}"));
                }
            }
        }
    }

    /// `POST /api/config`, which merges per key, so a patch naming one
    /// setting leaves the rest alone.
    fn save(&mut self, ctx: &egui::Context, patch: Value) {
        let keys: Vec<&'static str> = [
            "estimate",
            "chemistry",
            "cells",
            "full_cell",
            "empty_cell",
            "resistance",
        ]
        .into_iter()
        .filter(|k| patch.get(*k).is_some())
        .collect();
        for k in &keys {
            self.setting_status.insert(k, ("pending", "saving".into()));
        }
        let s = self.server.clone();
        self.saves.push((
            keys,
            Pending::spawn(ctx, move || {
                http::post_json(&s, "/api/config", &json!({ "battery": patch }))
            }),
        ));
    }

    /// Draws the page from `y`; returns the bottom.
    #[allow(clippy::too_many_arguments)]
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, units: &Units) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx);
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let linked = state.get("connected") == Some(&Value::Bool(true));
        if !linked {
            self.retried = false;
        } else if !self.retried
            && self.loading.is_none()
            && !self.check.busy()
            && self.doc.as_ref().is_some_and(|d| !d.connected)
        {
            self.retried = true;
            self.load(&ctx);
        }
        let idle = self.loading.is_none() && !self.check.busy();
        let can_check = idle
            && self
                .doc
                .as_ref()
                .is_some_and(|d| d.connected && !Self::check_names(d).is_empty());
        let (mut y, pressed) = pagekit::head_actions(
            ui,
            x,
            y,
            w,
            "Battery & Power",
            "The pack, how the autopilot measures it, and how much of it is left",
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
            "batteryhead",
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
                    let names = Self::check_names(doc);
                    self.check.start(&ctx, &self.server, &wanted, names);
                }
            }
            _ => {}
        }
        if armed {
            y = pagekit::warning_banner(ui, x, y, w, "Vehicle battery parameters are read-only while armed") + 14.0;
        }
        let Some(doc) = self.doc.clone() else { return y };
        let live = model::live(state, &doc.pack, units);

        // `.battery-layout`: two columns while 300 + 14 + 420 fit.
        let two = w >= 300.0 + 14.0 + 420.0;
        let (lw, rw) = if two {
            ((w - 14.0) / 2.0, (w - 14.0) / 2.0)
        } else {
            (w, w)
        };
        let live_bottom = self.live_card(ui, &ctx, &doc, &live, x, y, lw) + 12.0;
        let (rx, ry) = if two {
            (x + lw + 14.0, y)
        } else {
            (x, live_bottom + 14.0)
        };
        let mut cy = ry;
        if let Some(c) = self.check_card.clone() {
            cy = check::card(ui, rx, cy, rw, &c) + 26.0;
        }
        cy = self.estimator_card(ui, &ctx, &doc, state, rx, cy, rw) + 26.0;
        cy = self.sections(ui, &ctx, &doc, rx, cy, rw, armed);
        let right_bottom = cy - 14.0;
        (if two {
            live_bottom.max(right_bottom)
        } else {
            right_bottom
        }) - 12.0
    }

    /// `.battery-gauge-card`: the pack drawn, and the numbers under it.
    #[allow(clippy::too_many_arguments)]
    fn live_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        live: &Live,
        x: f32,
        y: f32,
        w: f32,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Pack") + 10.0;
        cy += self.figure(ui, ctx, doc, live, ix, cy, iw) + 10.0;
        let last = live.lines.len() - 1;
        for (i, l) in live.lines.iter().enumerate() {
            let fc = i == last;
            let top = if fc { cy + 4.0 } else { cy };
            let body = top + if fc { 1.0 + 7.0 } else { 5.0 };
            let color = match l.level {
                Some("healthy") => style::healthy(),
                Some("warning") => style::warning(),
                Some("critical") => style::critical(),
                Some(_) => style::text_3(),
                None if fc => style::text_2(),
                None => style::text_1(),
            };
            let value = style::galley_mono(ui, &l.value, 12.0, color);
            let label = style::galley(ui, l.label, 11.5, Weight::W400, style::text_2(), 0.0);
            painter.galley(Pos2::new(ix + iw - value.size().x, body), value.clone(), color);
            painter.galley(
                Pos2::new(ix, body + 12.0 * 1.02 - 11.5 * ASCENT),
                label,
                style::text_2(),
            );
            let bottom = body + 15.5 + 5.0;
            if fc {
                painter.hline(
                    egui::Rangef::new(ix, ix + iw),
                    top + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
            }
            if !fc {
                painter.hline(
                    egui::Rangef::new(ix, ix + iw),
                    bottom + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
            }
            cy = bottom + if fc { 0.0 } else { 1.0 };
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    /// The battery of the card: returns its height.
    #[allow(clippy::too_many_arguments)]
    fn figure(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        live: &Live,
        x: f32,
        y: f32,
        w: f32,
    ) -> f32 {
        let painter = ui.painter().clone();
        let cells = live.cells;
        let count = if cells > 0.0 { cells as usize } else { 0 };
        // the failsafe levels, on the scale they are read against
        struct Mark {
            x: f32,
            id: String,
            text: String,
            width: f32,
        }
        let mut marks: Vec<Mark> = Vec::new();
        for t in &doc.pack.thresholds {
            let Some(p) = model::threshold_percent(t, cells, &self.configured, &doc.chemistries) else {
                continue;
            };
            let text = t.label.to_uppercase();
            let width = style::galley(ui, &text, 8.5, Weight::W400, style::text_3(), 0.3)
                .size()
                .x;
            marks.push(Mark {
                x: X0 + (X1 - X0) * (p.clamp(0.0, 100.0) as f32) / 100.0,
                id: t.id.clone(),
                text,
                width,
            });
        }
        let flags: Vec<Flag> = marks.iter().map(|m| Flag { x: m.x, width: m.width }).collect();
        let placement = model::layout_threshold_labels(&flags, 0.0, model::FIGURE_WIDTH);
        let rows = placement.iter().map(|p| p.0).max().unwrap_or(0);
        let extra = rows as f32 * model::FLAG_PITCH;

        let fig_w = w.min(FIGURE_MAX_W);
        let scale = fig_w / model::FIGURE_WIDTH;
        let fig_h = (model::FIGURE_HEIGHT + extra) * scale;
        let (ox, oy) = (x + (w - fig_w) / 2.0, y);
        let at = |sx: f32, sy: f32| Pos2::new(ox + sx * scale, oy + (sy + extra) * scale);
        let rect_of = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
        let sw = |v: f32| v * scale;

        painter.rect_filled(
            rect_of(X1, (Y0 + Y1) / 2.0 - 12.0, X1 + 14.0, (Y0 + Y1) / 2.0 + 12.0),
            4.0 * scale,
            style::text_3(),
        );
        let body = rect_of(X0, Y0, X1, Y1);
        painter.rect_filled(body, 8.0 * scale, style::surface_2());

        // the fill, clipped to the rounded body so its square end never
        // pokes out of the corner
        let target = if live.connected {
            live.percent.clamp(0.0, 100.0) as f32
        } else {
            0.0
        };
        let shown = ctx.animate_value_with_time(ui.scope_id().with("battery-fill"), target, 0.22);
        let fill_x = X0 + (X1 - X0) * shown / 100.0;
        if fill_x > X0 {
            let fill = match model::level(live.percent, live.connected) {
                "healthy" => style::healthy().gamma_multiply(0.55),
                "warning" => style::warning().gamma_multiply(0.55),
                "critical" => style::critical().gamma_multiply(0.6),
                _ => style::text_3().gamma_multiply(0.25),
            };
            let poly: Vec<Pos2> = clip_x(&rounded_rect(X0, Y0, X1, Y1, 8.0), fill_x)
                .into_iter()
                .map(|(px, py)| at(px, py))
                .collect();
            painter.add(Shape::convex_polygon(poly, fill, Stroke::NONE));
        }
        for i in 1..count.max(1) {
            let sx = X0 + (X1 - X0) * i as f32 / count as f32;
            painter.line_segment([at(sx, Y0), at(sx, Y1)], Stroke::new(sw(2.5), style::surface_1()));
        }
        painter.rect_stroke(
            body,
            8.0 * scale,
            Stroke::new(sw(5.5), style::text_3()),
            egui::StrokeKind::Middle,
        );
        let mid = (Y0 + Y1) / 2.0;
        let window = rect_of(
            X0 + (X1 - X0) / 2.0 - 31.0,
            mid - 14.0,
            X0 + (X1 - X0) / 2.0 + 31.0,
            mid + 14.0,
        );
        painter.rect_filled(window, 8.0 * scale, style::surface_1());
        painter.rect_stroke(
            window,
            8.0 * scale,
            Stroke::new(sw(1.5), style::text_3()),
            egui::StrokeKind::Middle,
        );

        let text =
            |s: &str, size: f32, weight: Weight, color: Color32, letter: f32, sx: f32, base: f32, anchor: Anchor| {
                let g = style::galley(ui, s, size * scale, weight, color, letter * scale);
                let top = at(0.0, base).y - size * scale * ASCENT;
                let px = match anchor {
                    Anchor::Start => at(sx, 0.0).x,
                    Anchor::End => at(sx, 0.0).x - g.size().x,
                };
                painter.galley(Pos2::new(px, top), g, color);
            };
        let centred = |s: &str, size: f32, weight: Weight, color: Color32, letter: f32, sx: f32, base: f32| {
            let g = style::galley(ui, s, size * scale, weight, color, letter * scale);
            let top = at(0.0, base).y - size * scale * ASCENT;
            painter.galley(Pos2::new(at(sx, 0.0).x - g.size().x / 2.0, top), g, color);
        };
        if count > 0 && count <= model::MAX_LABELLED_CELLS {
            for i in 0..count {
                let sx = X0 + (X1 - X0) * (i as f32 + 0.5) / count as f32;
                let label = model::cell_label(live.connected, i, &live.measured, live.voltage, cells);
                centred(&label, 8.5, Weight::W400, style::text_2(), 0.0, sx, Y1 + 16.0);
            }
        }
        for (m, (row, anchor)) in marks.iter().zip(&placement) {
            let base = Y0 - 9.0 - *row as f32 * model::FLAG_PITCH;
            let color = if m.id == "low" {
                style::warning()
            } else {
                style::critical()
            };
            painter.extend(Shape::dashed_line(
                &[at(m.x, base - model::FLAG_CAP), at(m.x, Y1 + 4.0)],
                Stroke::new(sw(1.5), color),
                sw(3.0),
                sw(3.0),
            ));
            let sx = if *anchor == Anchor::Start {
                m.x + model::FLAG_GAP
            } else {
                m.x - model::FLAG_GAP
            };
            text(&m.text, 8.5, Weight::W400, style::text_3(), 0.3, sx, base, *anchor);
        }
        let percent = if live.connected {
            format!(
                "{}%",
                crate::data::units::js_to_fixed(crate::data::units::js_round(live.percent), 0)
            )
        } else {
            "\u{2014}".into()
        };
        centred(
            &percent,
            18.0,
            Weight::W700,
            style::text_1(),
            0.0,
            X0 + (X1 - X0) / 2.0,
            mid + 6.5,
        );
        let source = if !live.connected {
            ""
        } else if live.using_estimate {
            "from cell voltage"
        } else {
            "from the autopilot"
        };
        centred(
            source,
            9.5,
            Weight::W400,
            style::text_3(),
            0.3,
            X0 + (X1 - X0) / 2.0,
            Y1 + 34.0,
        );
        fig_h
    }

    /// `.battery-estimator-card`: how Corvus itself reads the pack.
    #[allow(clippy::too_many_arguments)]
    fn estimator_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        state: &Value,
        x: f32,
        y: f32,
        w: f32,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "How Corvus reads the pack") + 10.0;

        // the band with the switch
        let title = style::galley(
            ui,
            "Use the cell-voltage estimate",
            13.0,
            Weight::W600,
            style::text_1(),
            0.0,
        );
        let sub_w = iw - 2.0 - 24.0 - 38.0 - 12.0;
        let sub = kit::wrapped_weight(
            ui,
            "Replaces the percentage in the top bar, the map and the logs.",
            11.5,
            Weight::W400,
            style::text_2(),
            sub_w,
            1.21,
        );
        let band_h = 2.0 + 22.0 + 16.0 + 2.0 + sub.size().y;
        let band = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, band_h));
        card::paint(&painter, band, card::Look::Inset);
        let tx = band.left() + 13.0;
        let ty = band.top() + 12.0;
        let tw = title.size().x;
        painter.galley(
            Pos2::new(tx, ty + (16.0 - title.size().y) / 2.0),
            title,
            style::text_1(),
        );
        popover::info(
            ui,
            Pos2::new(tx + tw + 6.0 + 8.0, ty + 8.0),
            "Which percentage to fly by",
            ESTIMATE_INFO,
            "battery-estimate-info",
        );
        painter.galley(Pos2::new(tx, ty + 18.0), sub, style::text_2());
        let on = self.configured.estimate;
        if kit::switch(
            ui,
            Pos2::new(band.right() - 13.0 - 38.0, band.center().y - 11.0),
            on,
            true,
            "battery-estimate",
        ) {
            self.save(ctx, json!({ "estimate": !on }));
        }
        cy = band.bottom() + 10.0;
        cy += self.status_line(ui, ix, cy, iw, "estimate") + 10.0;

        // the settings
        let cells_now = model::number_of(state, "battery_cells");
        let chem = model::chemistry(&doc.chemistries, &self.configured);
        let narrow = iw <= 380.0 || crate::app::responsive::Vw::of(ctx).max720;
        let rows = 1 + SPECS.len();
        let mut row = 0;
        cy = self.chemistry_row(ui, ctx, doc, ix, cy, iw, narrow, row == rows - 1);
        row += 1;
        for spec in &SPECS {
            let auto = match spec.key {
                "cells" => {
                    if cells_now > 0.0 {
                        format!("auto (reading {}S)", crate::data::units::js_number(cells_now))
                    } else {
                        "auto".into()
                    }
                }
                "full_cell" => format!("auto ({} V)", crate::data::units::js_number(chem.full)),
                "empty_cell" => format!("auto ({} V)", crate::data::units::js_number(chem.empty)),
                _ => "off".into(),
            };
            cy = self.number_row(ui, ctx, spec, &auto, ix, cy, iw, narrow, row == rows - 1);
            row += 1;
        }
        cy += 10.0;
        let copy = Button::new("Take these from the vehicle", Variant::Ghost)
            .icon("download")
            .small()
            .enabled(model::has_vehicle_pack(&doc.pack));
        let (cw, ch) = (copy.width(ui), copy.height());
        if copy.show(
            ui,
            Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(cw, ch)),
            "batterycopy",
        ) {
            let patch = model::copy_patch(&doc.pack);
            if patch.as_object().is_some_and(|o| !o.is_empty()) {
                self.save(ctx, patch);
            } else {
                self.setting_status
                    .insert("estimate", ("err", "the vehicle reported no pack to copy".into()));
            }
        }
        cy += ch;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    /// `.params-row-status` under the band: empty most of the time.
    fn status_line(&self, ui: &egui::Ui, x: f32, y: f32, _w: f32, key: &'static str) -> f32 {
        let Some((cls, text)) = self.setting_status.get(key) else {
            return 0.0;
        };
        let color = pagekit::status_color(cls);
        let g = style::galley(ui, text, 10.5, Weight::W400, color, 0.0);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(x, y), g, color);
        h
    }

    /// The label cell of a setting row: its text and the hint icon after it.
    #[allow(clippy::too_many_arguments)]
    fn row_label(
        ui: &mut egui::Ui,
        label: &str,
        info: &str,
        x: f32,
        y: f32,
        label_w: f32,
        center: f32,
        id: &str,
    ) -> f32 {
        let lg = kit::wrapped_weight(ui, label, 12.0, Weight::W400, style::text_1(), label_w, 1.2);
        let (lw, lh) = (lg.size().x, lg.size().y);
        ui.painter()
            .galley(Pos2::new(x, center - lh / 2.0), lg, style::text_1());
        popover::info(
            ui,
            Pos2::new((x + lw + 6.0 + 8.0).min(x + label_w - 8.0), center),
            label,
            info,
            ("battery-info", id.to_string()),
        );
        y
    }

    /// The columns of a setting row: label width, control x and width.
    fn columns(x: f32, w: f32, narrow: bool) -> (f32, f32, f32) {
        if narrow {
            (w, x, w)
        } else {
            let label_w = (w - 12.0 - 220.0).clamp(140.0, 230.0);
            (label_w, x + label_w + 12.0, w - label_w - 12.0)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn chemistry_row(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        doc: &Doc,
        x: f32,
        y: f32,
        w: f32,
        narrow: bool,
        last: bool,
    ) -> f32 {
        let (label_w, cx, cw) = Self::columns(x, w, narrow);
        let control_h = 2.0 + 12.0 + 12.0 * 1.21;
        let above = if narrow { 12.0 * 1.2 + 6.0 } else { 0.0 };
        let top = y + 7.0 + above;
        let center = if narrow { y + 7.0 + 6.0 } else { top + control_h / 2.0 };
        Self::row_label(ui, "Chemistry", CHEMISTRY_INFO, x, y, label_w, center, "chemistry");
        let choices: Vec<Choice> = if doc.chemistries.is_empty() {
            vec![Choice::new("lipo", "LiPo")]
        } else {
            doc.chemistries
                .iter()
                .map(|c| {
                    Choice::new(
                        c.value.clone(),
                        format!(
                            "{} ({} to {} V)",
                            c.label,
                            crate::data::units::js_number(c.empty),
                            crate::data::units::js_number(c.full)
                        ),
                    )
                })
                .collect()
        };
        let widest = choices
            .iter()
            .map(|c| {
                style::galley(ui, &c.label, 12.0, Weight::W400, style::text_1(), 0.0)
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        let sw = (widest + 11.0 + 26.0 + 2.0).clamp(150.0, 300.0).min(cw);
        let rect = Rect::from_min_size(Pos2::new(cx, top), Vec2::new(sw, control_h));
        let current = if self.configured.chemistry.is_empty() {
            "lipo".to_string()
        } else {
            self.configured.chemistry.clone()
        };
        if let Some(v) = kit::select(
            ui,
            rect,
            &choices,
            &current,
            "",
            false,
            "Battery chemistry",
            "battery-chem",
        ) {
            if v != current {
                self.save(ctx, json!({ "chemistry": v }));
            }
        }
        let mut ux = rect.right() + 8.0;
        if let Some((cls, t)) = self.setting_status.get("chemistry").cloned() {
            let color = pagekit::status_color(cls);
            let g = style::galley(ui, &t, 10.5, Weight::W400, color, 0.0);
            ui.painter()
                .galley(Pos2::new(ux, top + (control_h - g.size().y) / 2.0), g.clone(), color);
            ux += g.size().x;
        }
        let _ = ux;
        let bottom = top + control_h + 7.0;
        if !last {
            ui.painter().hline(
                egui::Rangef::new(x, x + w),
                bottom + 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
        }
        bottom + if last { 0.0 } else { 1.0 }
    }

    #[allow(clippy::too_many_arguments)]
    fn number_row(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        spec: &Spec,
        auto: &str,
        x: f32,
        y: f32,
        w: f32,
        narrow: bool,
        last: bool,
    ) -> f32 {
        let (label_w, cx, _cw) = Self::columns(x, w, narrow);
        let control_h = 29.5;
        let above = if narrow { 12.0 * 1.2 + 6.0 } else { 0.0 };
        let top = y + 7.0 + above;
        let center = if narrow { y + 7.0 + 6.0 } else { top + control_h / 2.0 };
        Self::row_label(ui, spec.label, spec.info, x, y, label_w, center, spec.key);
        let current = self.configured.number(spec.key);
        let original = if current > 0.0 {
            schema_form::format_number(current)
        } else {
            String::new()
        };
        let mut text = self.typed.get(spec.key).cloned().unwrap_or_else(|| original.clone());
        let rect = Rect::from_min_size(Pos2::new(cx, top), Vec2::new(150.0, control_h));
        let resp = kit::text_field_padded(ui, rect, &mut text, auto, true, ("battery-num", spec.key), 11.0);
        if resp.changed() {
            self.typed.insert(spec.key, text.clone());
            self.setting_status.remove(spec.key);
        }
        if resp.lost_focus() && self.typed.get(spec.key).is_some_and(|t| *t != original) {
            match model::parse_setting(&text, 0.0, spec.max) {
                Ok(v) => self.save(ctx, json!({ spec.key: v })),
                Err(p) => {
                    self.setting_status.insert(spec.key, ("err", p));
                }
            }
        }
        if self.setting_status.get(spec.key).is_some_and(|s| s.0 == "err") && self.typed.contains_key(spec.key) {
            ui.painter().rect_stroke(
                rect,
                style::RADIUS_BTN,
                Stroke::new(1.0, style::critical()),
                egui::StrokeKind::Inside,
            );
        }
        let mut ux = rect.right() + 8.0;
        let g = style::galley(ui, spec.unit, 11.0, Weight::W400, style::text_3(), 0.0);
        ui.painter().galley(
            Pos2::new(ux, top + (control_h - g.size().y) / 2.0),
            g.clone(),
            style::text_3(),
        );
        ux += g.size().x + 8.0;
        if let Some((cls, t)) = self.setting_status.get(spec.key).cloned() {
            let color = pagekit::status_color(cls);
            let g = style::galley(ui, &t, 10.5, Weight::W400, color, 0.0);
            ui.painter()
                .galley(Pos2::new(ux, top + (control_h - g.size().y) / 2.0), g, color);
        }
        let bottom = top + control_h + 7.0;
        if !last {
            ui.painter().hline(
                egui::Rangef::new(x, x + w),
                bottom + 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
        }
        bottom + if last { 0.0 } else { 1.0 }
    }

    /// The vehicle's own parameters, one card per section the backend sent.
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
        if doc.sections.is_empty() {
            let bg = card::reserve(&painter);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Vehicle settings") + 10.0;
            let g = kit::wrapped_weight(
                ui,
                "Connect to a vehicle to read the pack it is configured for. The settings above are Corvus's own and are saved either way.",
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
        let server = self.server.clone();
        for s in &doc.sections {
            let bg = card::reserve(&painter);
            let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, &s.title) + 10.0;
            if !s.hint.is_empty() {
                let g = kit::wrapped_weight(ui, &s.hint, 10.5, Weight::W400, style::text_3(), iw, 1.4);
                painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
                cy += g.size().y + 10.0;
            }
            if !s.fields.is_empty() {
                cy = self
                    .form
                    .show_with(ui, ctx, &server, &s.fields, ix, cy, iw, armed, Look::BATTERY);
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
}

/// The outline of a rounded rectangle, clockwise from the top left, in
/// drawing units.
fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Vec<(f32, f32)> {
    let mut pts = Vec::new();
    let corners = [
        (x1 - r, y0 + r, -90.0f32),
        (x1 - r, y1 - r, 0.0),
        (x0 + r, y1 - r, 90.0),
        (x0 + r, y0 + r, 180.0),
    ];
    for (cx, cy, start) in corners {
        for k in 0..=8 {
            let a = (start + 90.0 * k as f32 / 8.0).to_radians();
            pts.push((cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts
}

/// The part of a convex outline at or left of `x` (Sutherland and Hodgman
/// against one edge), so a fill ends square inside a rounded body.
pub fn clip_x(poly: &[(f32, f32)], x: f32) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (ain, bin) = (a.0 <= x, b.0 <= x);
        if ain {
            out.push(a);
        }
        if ain != bin {
            let t = (x - a.0) / (b.0 - a.0);
            out.push((x, a.1 + t * (b.1 - a.1)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fill_ends_square_at_its_edge_inside_the_rounded_body() {
        let body = rounded_rect(8.0, 16.0, 228.0, 86.0, 8.0);
        let half = clip_x(&body, 118.0);
        assert!(half.iter().all(|p| p.0 <= 118.0 + 1e-4));
        let right = half.iter().map(|p| p.0).fold(f32::MIN, f32::max);
        assert!((right - 118.0).abs() < 1e-4);
        let tall =
            half.iter().map(|p| p.1).fold(f32::MIN, f32::max) - half.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        assert!((tall - 70.0).abs() < 1e-3, "the fill is the body's height at its edge");
        let full = clip_x(&body, 228.0);
        assert_eq!(full.len(), body.len());
        let small = clip_x(&body, 9.0);
        let h = small.iter().map(|p| p.1).fold(f32::MIN, f32::max) - small.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        assert!(h < 70.0, "near the left edge the corner curve cuts the fill short");
    }

    #[test]
    fn the_columns_follow_the_grid_of_a_row() {
        // 405.5 px wide: the control column keeps its 220 px floor
        let (label, cx, cw) = BatteryPage::columns(586.5, 405.5, false);
        assert_eq!((label, cx, cw), (173.5, 772.0, 220.0));
        // wide: the label stops at 230
        assert_eq!(BatteryPage::columns(0.0, 600.0, false).0, 230.0);
        // narrow: one column
        assert_eq!(BatteryPage::columns(0.0, 300.0, true), (300.0, 0.0, 300.0));
    }
}
