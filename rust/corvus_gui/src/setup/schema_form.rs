//! The schema-driven form the backend describes (`corvus/safety_config.py`,
//! `motor_config.py`) and `setup-shared.js` draws: sections of fields, each
//! naming the parameter it writes. Nothing here knows a parameter name, so a
//! firmware that lacks one sends one field fewer and the form shows one
//! fewer.
//!
//! A field is an enum (a list), a number (typed, checked against the
//! schema's min and max, written when it loses focus or Enter is pressed),
//! a sign (a parameter whose sign is a direction and whose size is kept) or a
//! bitmask (a checkbox per named bit; every toggle writes the whole recomputed
//! word, so bits the schema does not name survive). Nothing is written while
//! armed, and a refused write snaps the control back to what the vehicle
//! still holds. Layout is `.pform-*` of setup-forms.css.

use std::collections::HashMap;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::kit::{self, Choice};
use crate::ui::style::{self, Weight};

/// How a form looks: the label column, the row rhythm and where hints go.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub label_min: f32,
    pub label_max: f32,
    pub pad: f32,
    pub hint_as_info: bool,
    /// `.safety-extra-field`: the label is a parameter name, in mono.
    pub mono_label: bool,
    /// The control column's floor (`minmax(220px, 1fr)` on Battery).
    pub control_min: f32,
    /// A number input's own width and height where the page departs from
    /// `.pform-input` (`.battery-input`: 150 px, a mono line).
    pub number: Option<(f32, f32)>,
}

impl Look {
    /// `.safety-field`: labels are sentences, hints are printed.
    pub const SAFETY: Look = Look {
        label_min: 140.0,
        label_max: 230.0,
        pad: 7.0,
        hint_as_info: false,
        mono_label: false,
        control_min: 0.0,
        number: None,
    };
    /// A parameter the operator named on a sensor page.
    pub const EXTRA: Look = Look {
        label_min: 140.0,
        label_max: 230.0,
        pad: 3.0,
        hint_as_info: false,
        mono_label: true,
        control_min: 0.0,
        number: None,
    };
    /// `.motors-field`: short labels, hints behind an icon.
    pub const MOTORS: Look = Look {
        label_min: 110.0,
        label_max: 170.0,
        pad: 5.0,
        hint_as_info: true,
        mono_label: false,
        control_min: 0.0,
        number: None,
    };
    /// `.pform-field` as it is: a 130 to 220 px label column.
    pub const CALIB: Look = Look {
        label_min: 130.0,
        label_max: 220.0,
        pad: 7.0,
        hint_as_info: false,
        mono_label: false,
        control_min: 0.0,
        number: None,
    };
    /// `.battery-field`: a 220 px control column, 150 px inputs.
    pub const BATTERY: Look = Look {
        label_min: 140.0,
        label_max: 230.0,
        pad: 7.0,
        hint_as_info: false,
        mono_label: false,
        control_min: 220.0,
        number: Some((150.0, 29.5)),
    };
}

/// What a field edits.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Enum(Vec<(i64, String)>),
    Sign(Vec<(i64, String)>),
    Number,
    Bitmask(Vec<(i64, String)>),
}

/// One field of a section.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub param: String,
    pub label: String,
    pub kind: Kind,
    pub value: f64,
    pub unit: String,
    pub hint: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Further parameters that take the same value (`also`).
    pub also: Vec<String>,
}

fn options(v: &Value, key: &str, value_key: &str) -> Vec<(i64, String)> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    Some((
                        o.get(value_key)?.as_f64()?.round() as i64,
                        o.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reads the schema's fields; one that names no parameter or kind is skipped.
pub fn parse_fields(v: &Value) -> Vec<Field> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| {
                    let param = f.get("param")?.as_str().filter(|p| !p.is_empty())?.to_string();
                    let kind = match f.get("kind")?.as_str()? {
                        "enum" => Kind::Enum(options(f, "options", "value")),
                        "sign" => Kind::Sign(options(f, "options", "value")),
                        "bitmask" => Kind::Bitmask(options(f, "bits", "bit")),
                        _ => Kind::Number,
                    };
                    let s = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    Some(Field {
                        label: Some(s("label"))
                            .filter(|l| !l.is_empty())
                            .unwrap_or_else(|| param.clone()),
                        param,
                        kind,
                        value: f.get("value").and_then(Value::as_f64).unwrap_or(0.0),
                        unit: s("unit"),
                        hint: s("hint"),
                        min: f.get("min").and_then(Value::as_f64),
                        max: f.get("max").and_then(Value::as_f64),
                        also: f
                            .get("also")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(|n| n.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `formatNumber`: whole numbers as they are, others to six decimals.
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    if v.fract() == 0.0 {
        return crate::data::units::js_number(v);
    }
    crate::data::units::js_number(crate::data::units::js_round(v * 1e6) / 1e6)
}

/// `rangeProblem`: what is wrong with a typed value, or an empty text.
pub fn range_problem(f: &Field, raw: &str) -> String {
    let t = raw.trim();
    let Some(v) = t.parse::<f64>().ok().filter(|v| v.is_finite() && !t.is_empty()) else {
        return "not a number".into();
    };
    if let Some(lo) = f.min.filter(|lo| v < *lo) {
        return format!("below {}", format_number(lo));
    }
    if let Some(hi) = f.max.filter(|hi| v > *hi) {
        return format!("above {}", format_number(hi));
    }
    String::new()
}

/// The whole word with one bit set or cleared.
pub fn with_bit(word: f64, bit: i64, on: bool) -> f64 {
    let mut w = word.max(0.0) as u64 as u32;
    if on {
        w |= 1u32 << bit;
    } else {
        w &= !(1u32 << bit);
    }
    f64::from(w)
}

/// A sign field keeps its magnitude and takes the sign; `fallback` is used
/// only when the vehicle holds exactly 0, whose sign says nothing.
pub fn signed_value(current: f64, sign: i64, fallback: f64) -> f64 {
    let magnitude = [current.abs(), fallback.abs(), 1.0]
        .into_iter()
        .find(|m| *m != 0.0)
        .unwrap_or(1.0);
    if sign < 0 {
        -magnitude
    } else {
        magnitude
    }
}

type Write = (String, f64, Vec<String>, Pending<Result<(), String>>);

/// What a form remembers between frames.
#[derive(Default)]
pub struct FormState {
    /// What the operator typed, until it is written or abandoned.
    texts: HashMap<String, String>,
    /// The value a control shows while its write is on its way.
    shown: HashMap<String, f64>,
    status: HashMap<String, (&'static str, String)>,
    writes: Vec<Write>,
    /// Parameters written since the last read, so the page can re-read.
    pub written: bool,
    /// What the operator asked for, until a check confirms it (a refused
    /// field snaps back and cannot remember it).
    pub wanted: std::collections::BTreeMap<String, f64>,
    /// Where the last row drawn by `show_with` ended, for what follows it.
    pub last_end: f32,
    errors: Vec<String>,
}

impl FormState {
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    /// The status line of a parameter's last write.
    pub fn status_of(&self, param: &str) -> Option<(&'static str, String)> {
        self.status.get(param).cloned()
    }

    /// What a field holds as it is typed: the typed text when it reads as a
    /// number of zero or more, else the stored value. `None` for a blank or
    /// invalid entry (`landingValues`).
    pub fn live_value(&self, f: &Field) -> Option<f64> {
        match self.texts.get(&f.param) {
            Some(t) => t.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0),
            None => Some(self.shown.get(&f.param).copied().unwrap_or(f.value)),
        }
    }

    pub fn busy(&self) -> bool {
        !self.writes.is_empty()
    }

    /// Puts each checked value's outcome next to its field, where it was typed.
    pub fn mark_checked(&mut self, rows: &[crate::setup::check::Row]) {
        for r in rows {
            self.status.insert(
                r.name.clone(),
                if r.ok {
                    ("ok", crate::setup::check::outcome(r))
                } else {
                    (
                        "err",
                        format!(
                            "wanted {}, vehicle holds {}",
                            crate::setup::check::format_checked(r.wanted),
                            crate::setup::check::format_checked(r.after)
                        ),
                    )
                },
            );
        }
    }

    fn write(&mut self, ctx: &egui::Context, server: &Server, f: &Field, value: f64) {
        self.wanted.insert(f.param.clone(), value);
        for n in &f.also {
            self.wanted.insert(n.clone(), value);
        }
        self.status.insert(f.param.clone(), ("pending", "saving".into()));
        self.shown.insert(f.param.clone(), value);
        let (s, name, also) = (server.clone(), f.param.clone(), f.also.clone());
        let list = also.clone();
        self.writes.push((
            f.param.clone(),
            value,
            also,
            Pending::spawn(ctx, move || {
                let set = |n: &str| -> Result<(), String> {
                    let d = http::post_json(&s, "/api/params/set", &json!({ "name": n, "value": value }))?;
                    if d.get("ok") == Some(&Value::Bool(true)) {
                        Ok(())
                    } else {
                        Err(d
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("Command rejected by vehicle")
                            .to_string())
                    }
                };
                set(&name)?;
                for n in &list {
                    set(n).map_err(|e| format!("{n}: {e}"))?;
                }
                Ok(())
            }),
        ));
    }

    /// Takes in finished writes; `fields` get their new values.
    pub fn take_answers(&mut self, fields: &mut [Field]) {
        let mut done = Vec::new();
        for (i, w) in self.writes.iter().enumerate() {
            if let Some(r) = w.3.take() {
                done.push((i, r));
            }
        }
        for (i, r) in done.into_iter().rev() {
            let (param, value, _, _) = self.writes.remove(i);
            self.shown.remove(&param);
            self.texts.remove(&param);
            match r {
                Ok(()) => {
                    if let Some(f) = fields.iter_mut().find(|f| f.param == param) {
                        f.value = value;
                    }
                    self.status.insert(param, ("ok", "saved".into()));
                    self.written = true;
                }
                Err(e) => {
                    let msg = if e.is_empty() { "write failed".to_string() } else { e };
                    self.status.insert(param.clone(), ("err", msg.clone()));
                    self.errors.push(format!("Could not set {param}: {msg}"));
                }
            }
        }
    }

    /// How wide the control of a field is in a table cell: 68 px for a number
    /// and a select as wide as its widest option, as `control` draws it.
    pub fn compact_width(ui: &egui::Ui, f: &Field) -> f32 {
        match &f.kind {
            Kind::Enum(opts) | Kind::Sign(opts) => {
                let widest = opts
                    .iter()
                    .map(|(_, l)| style::galley(ui, l, 12.0, Weight::W400, style::text_1(), 0.0).size().x)
                    .fold(0.0, f32::max);
                widest + 7.0 + 24.0 + 2.0
            }
            Kind::Number => 68.0,
            Kind::Bitmask(_) => 0.0,
        }
    }

    /// Draws the control of one field at `at`, `cw` wide at most and
    /// `control_h` tall, and returns the x its right edge ends at. `compact`
    /// is the table cell: numbers 68 px wide and selects as narrow as their
    /// text.
    #[allow(clippy::too_many_arguments)]
    pub fn control(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        server: &Server,
        f: &Field,
        at: Pos2,
        cw: f32,
        control_h: f32,
        armed: bool,
        compact: bool,
    ) -> f32 {
        self.control_with(ui, ctx, server, f, at, cw, control_h, armed, compact, None, None)
    }

    /// `control` with the widths a page's own CSS gives its select
    /// (`select_w`) and its number input (`number_w`).
    #[allow(clippy::too_many_arguments)]
    pub fn control_with(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        server: &Server,
        f: &Field,
        at: Pos2,
        cw: f32,
        control_h: f32,
        armed: bool,
        compact: bool,
        select_w: Option<f32>,
        number_w: Option<f32>,
    ) -> f32 {
        let painter = ui.painter().clone();
        let (cx, top) = (at.x, at.y);
        let shown = self.shown.get(&f.param).copied().unwrap_or(f.value);
        let mut ux = cx;
        match &f.kind {
            Kind::Enum(opts) | Kind::Sign(opts) => {
                let sign = matches!(f.kind, Kind::Sign(_));
                let current = if sign {
                    if shown < 0.0 {
                        -1
                    } else {
                        1
                    }
                } else {
                    shown.round() as i64
                };
                let choices: Vec<Choice> = opts
                    .iter()
                    .map(|(v, l)| Choice::new(v.to_string(), l.clone()))
                    .collect();
                let widest = opts
                    .iter()
                    .map(|(_, l)| style::galley(ui, l, 12.0, Weight::W400, style::text_1(), 0.0).size().x)
                    .fold(0.0, f32::max);
                let narrow_full = !compact && select_w.is_none() && crate::app::responsive::Vw::of(ctx).max720;
                let sw = select_w.unwrap_or(if narrow_full {
                    cw
                } else if compact {
                    (widest + 7.0 + 24.0 + 2.0).min(cw)
                } else {
                    (widest + 11.0 + 26.0 + 2.0).clamp(150.0, 300.0).min(cw)
                });
                let r = Rect::from_min_size(Pos2::new(cx, top), Vec2::new(sw, control_h));
                if armed {
                    let label = choices
                        .iter()
                        .find(|c| c.value == current.to_string())
                        .map_or(String::new(), |c| c.label.clone());
                    {
                        painter.rect_filled(r, style::RADIUS_BTN, style::surface_2());
                        painter.rect_stroke(
                            r,
                            style::RADIUS_BTN,
                            Stroke::new(1.0, style::border()),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let g = style::galley(ui, &label, 12.0, Weight::W400, style::text_3(), 0.0);
                    ui.painter_at(r.shrink2(Vec2::new(11.0, 0.0))).galley(
                        Pos2::new(r.left() + 11.0, r.center().y - g.size().y / 2.0),
                        g,
                        style::text_3(),
                    );
                } else if let Some(v) = kit::select_padded(
                    ui,
                    r,
                    &choices,
                    &current.to_string(),
                    "",
                    false,
                    &f.param,
                    ("sf", &f.param),
                    if compact { (7.0, 24.0) } else { (11.0, 26.0) },
                ) {
                    let picked: i64 = v.parse().unwrap_or(0);
                    if picked != current && !self.busy() {
                        let next = if sign {
                            signed_value(f.value, picked, 1.0)
                        } else {
                            picked as f64
                        };
                        self.write(ctx, server, f, next);
                    }
                }
                ux = r.right() + 8.0;
            }
            Kind::Number => {
                let r = Rect::from_min_size(
                    Pos2::new(cx, top),
                    Vec2::new(number_w.unwrap_or(if compact { 68.0 } else { 110.0 }), control_h),
                );
                let mut text = self
                    .texts
                    .get(&f.param)
                    .cloned()
                    .unwrap_or_else(|| format_number(shown));
                let problem = if self.texts.contains_key(&f.param) {
                    range_problem(f, &text)
                } else {
                    String::new()
                };
                if armed {
                    {
                        painter.rect_filled(r, style::RADIUS_BTN, style::surface_2());
                        painter.rect_stroke(
                            r,
                            style::RADIUS_BTN,
                            Stroke::new(1.0, style::border()),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let g = style::galley_mono(ui, &text, 12.0, style::text_3());
                    painter.galley(
                        Pos2::new(r.left() + 9.0, r.center().y - g.size().y / 2.0),
                        g,
                        style::text_3(),
                    );
                } else {
                    let resp = kit::text_field_padded(
                        ui,
                        r,
                        &mut text,
                        "",
                        true,
                        ("sf", &f.param),
                        if compact { 6.0 } else { 11.0 },
                    );
                    if resp.changed() {
                        self.texts.insert(f.param.clone(), text.clone());
                        self.status.remove(&f.param);
                    }
                    if !problem.is_empty() {
                        painter.rect_stroke(
                            r,
                            style::RADIUS_BTN,
                            Stroke::new(1.0, style::critical()),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let committed = resp.lost_focus();
                    if committed && self.texts.contains_key(&f.param) {
                        let p = range_problem(f, &text);
                        if p.is_empty() && !self.busy() {
                            let v: f64 = text.trim().parse().unwrap_or(f.value);
                            if v == f.value {
                                self.texts.remove(&f.param);
                            } else {
                                self.write(ctx, server, f, v);
                            }
                        } else if !p.is_empty() {
                            self.status.insert(f.param.clone(), ("err", p));
                        }
                    }
                }
                ux = r.right() + 8.0;
            }
            Kind::Bitmask(bits) => {
                let word = shown.max(0.0);
                let mut bx = cx;
                let mut by = top;
                for (bit, label) in bits {
                    let lw = 14.0
                        + 6.0
                        + style::galley(ui, label, 11.5, Weight::W400, style::text_1(), 0.0)
                            .size()
                            .x;
                    if bx + lw > cx + cw && bx > cx {
                        bx = cx;
                        by += 18.0 + 4.0;
                    }
                    let mut on = (word as u64 as u32) & (1u32 << bit) != 0;
                    let was = on;
                    let resp_on = !armed;
                    if resp_on {
                        if kit::checkbox(
                            ui,
                            Pos2::new(bx, by),
                            lw + 8.0,
                            label,
                            &mut on,
                            false,
                            ("bit", &f.param, bit),
                        ) && on != was
                            && !self.busy()
                        {
                            let next = with_bit(f.value, *bit, on);
                            self.write(ctx, server, f, next);
                        }
                    } else {
                        let mut dim = on;
                        let r = Rect::from_min_size(Pos2::new(bx, by), Vec2::new(lw, 18.0));
                        {
                            let r = Rect::from_min_size(Pos2::new(bx, by + 2.0), Vec2::splat(14.0));
                            painter.rect_filled(r, 3, if dim { style::text_3() } else { style::surface_2() });
                            painter.rect_stroke(
                                r,
                                3,
                                Stroke::new(1.0, style::border_strong()),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let g = style::galley(ui, label, 11.5, Weight::W400, style::text_3(), 0.0);
                        painter.galley(
                            Pos2::new(bx + 20.0, r.center().y - g.size().y / 2.0),
                            g,
                            style::text_3(),
                        );
                        dim = false;
                        let _ = dim;
                    }
                    bx += lw + 14.0;
                }
            }
        }
        ux
    }

    /// Draws `fields` as a `.pform-grid` with the Safety page's look; see
    /// `show_with`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        server: &Server,
        fields: &[Field],
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        self.show_with(ui, ctx, server, fields, x, y, w, armed, Look::SAFETY)
    }

    /// Draws `fields` as a `.pform-grid` in `x .. x + w` from `y`; returns the
    /// bottom. `label_w` is the label column (`minmax(140px, 230px)` on the
    /// Safety page).
    #[allow(clippy::too_many_arguments)]
    pub fn show_with(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        server: &Server,
        fields: &[Field],
        x: f32,
        mut y: f32,
        w: f32,
        armed: bool,
        look: Look,
    ) -> f32 {
        let painter = ui.painter().clone();
        for (i, f) in fields.iter().enumerate() {
            // Each row is a grid of its own: the label column takes what
            // the control leaves, between the Look's floor and ceiling.
            let natural = match &f.kind {
                Kind::Number => {
                    110.0
                        + if f.unit.is_empty() {
                            0.0
                        } else {
                            8.0 + style::galley(ui, &f.unit, 11.0, Weight::W400, style::text_3(), 0.0)
                                .size()
                                .x
                        }
                }
                Kind::Enum(_) | Kind::Sign(_) | Kind::Bitmask(_) => {
                    let widest = match &f.kind {
                        Kind::Enum(o) | Kind::Sign(o) => o
                            .iter()
                            .map(|(_, l)| style::galley(ui, l, 12.0, Weight::W400, style::text_1(), 0.0).size().x)
                            .fold(0.0, f32::max),
                        _ => 90.0,
                    };
                    (widest + 11.0 + 26.0 + 2.0).clamp(150.0, 300.0)
                }
            };
            let natural = natural.max(look.control_min);
            // under 721 a row is one column: the label over its control
            let narrow = crate::app::responsive::Vw::of(ctx).max720;
            let label_w = if narrow {
                w
            } else {
                (w - 12.0 - natural)
                    .clamp(look.label_min, look.label_max)
                    .min(w * 0.5 + 40.0)
            };
            let cx = if narrow { x } else { x + label_w + 12.0 };
            let cw = if narrow { w } else { w - label_w - 12.0 };
            let pad = look.pad;
            let control_h: f32 = match (&f.kind, look.number) {
                (Kind::Number, Some((_, h))) => h,
                _ => 2.0 + 12.0 + 12.0 * 1.21,
            };
            let number_w = look.number.map(|n| n.0);
            let hint = (!f.hint.is_empty() && !look.hint_as_info)
                .then(|| kit::wrapped_weight(ui, &f.hint, 10.5, Weight::W400, style::text_3(), cw, 1.4));
            let bits_h = match &f.kind {
                Kind::Bitmask(bits) => Some(bits_height(bits.len(), cw)),
                _ => None,
            };
            let control_row = bits_h.unwrap_or(control_h).max(16.0);
            let lg = if look.mono_label {
                ui.painter()
                    .layout(f.label.clone(), style::mono(11.5), style::text_1(), label_w)
            } else {
                kit::wrapped_weight(ui, &f.label, 12.0, Weight::W400, style::text_1(), label_w, 1.2)
            };
            let lh = lg.size().y;
            let lw = lg.size().x;
            // the label's own line when it sits above the control
            let above = if narrow { lh + 6.0 } else { 0.0 };
            let row_h = control_row;
            let h = pad + above + row_h + hint.as_ref().map_or(0.0, |g| 6.0 + g.size().y) + pad;
            let top = y + pad + above;
            let label_y = if narrow { y + pad } else { top + (row_h - lh) / 2.0 };
            painter.galley(Pos2::new(x, label_y), lg, style::text_1());
            if look.hint_as_info && !f.hint.is_empty() {
                crate::ui::popover::info(
                    ui,
                    Pos2::new(
                        (x + lw + 5.0 + 8.0).min(x + label_w - 8.0),
                        if narrow { label_y + lh / 2.0 } else { top + row_h / 2.0 },
                    ),
                    &f.label,
                    &f.hint,
                    ("fi", &f.param),
                );
            }
            let status = self.status.get(&f.param).cloned();
            let ux = self.control_with(
                ui,
                ctx,
                server,
                f,
                Pos2::new(cx, top),
                cw,
                control_h,
                armed,
                false,
                None,
                number_w,
            );
            let mut ux = ux;
            if !f.unit.is_empty() && !matches!(f.kind, Kind::Bitmask(_)) {
                let g = style::galley(ui, &f.unit, 11.0, Weight::W400, style::text_3(), 0.0);
                let uw = g.size().x;
                painter.galley(Pos2::new(ux, top + (control_h - g.size().y) / 2.0), g, style::text_3());
                ux += uw + 8.0;
            }
            if let Some((cls, text)) = status {
                let color = match cls {
                    "ok" => style::healthy(),
                    "err" => style::critical(),
                    _ => style::nav(),
                };
                let g = style::galley(ui, &text, 10.5, Weight::W400, color, 0.0);
                ux += g.size().x;
                painter.galley(
                    Pos2::new(ux - g.size().x, top + (control_h - g.size().y) / 2.0),
                    g,
                    color,
                );
            }
            self.last_end = ux;
            if let Some(g) = hint {
                painter.galley(Pos2::new(cx, top + row_h + 6.0), g, style::text_3());
            }
            y += h;
            if i + 1 < fields.len() {
                painter.hline(
                    egui::Rangef::new(x, x + w),
                    y - 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
            }
        }
        y
    }
}

/// How tall a checkbox set wraps to in `width`: 18 px lines, 4 px apart.
fn bits_height(count: usize, width: f32) -> f32 {
    // An estimate from an average label; the lines are drawn where they fall.
    let per_line = (width / 120.0).floor().max(1.0) as usize;
    let lines = count.div_ceil(per_line).max(1);
    lines as f32 * 18.0 + (lines as f32 - 1.0) * 4.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(min: Option<f64>, max: Option<f64>) -> Field {
        Field {
            param: "P".into(),
            label: "P".into(),
            kind: Kind::Number,
            value: 1.0,
            unit: String::new(),
            hint: String::new(),
            min,
            max,
            also: vec![],
        }
    }

    #[test]
    fn the_schema_is_read_field_by_field() {
        let v = json!([
            {"param": "NAV_RCL_ACT", "label": "RC loss", "kind": "enum", "value": 2, "options": [{"value": 1, "label": "Hold"}, {"value": 2, "label": "Return"}]},
            {"param": "GF_MAX_HOR_DIST", "kind": "number", "value": 100.5, "unit": "m", "min": 0, "hint": "h"},
            {"param": "COM_ARM", "kind": "bitmask", "value": 5, "bits": [{"bit": 0, "label": "A"}, {"bit": 2, "label": "C"}]},
            {"kind": "number"},
            {"param": "", "kind": "number"},
        ]);
        let f = parse_fields(&v);
        assert_eq!(f.len(), 3, "one without a parameter is skipped");
        assert_eq!(f[0].kind, Kind::Enum(vec![(1, "Hold".into()), (2, "Return".into())]));
        assert_eq!(f[1].label, "GF_MAX_HOR_DIST", "no label falls back to the parameter");
        assert_eq!(
            (f[1].unit.as_str(), f[1].min, f[1].hint.as_str()),
            ("m", Some(0.0), "h")
        );
        assert_eq!(f[2].kind, Kind::Bitmask(vec![(0, "A".into()), (2, "C".into())]));
    }

    #[test]
    fn typed_numbers_are_checked_against_the_schema() {
        let f = num(Some(0.0), Some(10.0));
        assert_eq!(range_problem(&f, "5"), "");
        assert_eq!(range_problem(&f, "-1"), "below 0");
        assert_eq!(range_problem(&f, "10.5"), "above 10");
        assert_eq!(range_problem(&f, ""), "not a number");
        assert_eq!(range_problem(&f, "x"), "not a number");
        assert_eq!(range_problem(&num(None, None), "1e9"), "");
    }

    #[test]
    fn numbers_print_as_the_page_prints_them() {
        assert_eq!(format_number(100.0), "100");
        assert_eq!(format_number(0.1), "0.1");
        assert_eq!(format_number(1.23456789), "1.234568");
        assert_eq!(format_number(f64::NAN), "");
    }

    #[test]
    fn a_bit_toggle_keeps_the_bits_it_does_not_know() {
        assert_eq!(with_bit(0b1010.into(), 0, true), 11.0);
        assert_eq!(with_bit(0b1010.into(), 1, false), 8.0);
        assert_eq!(with_bit(0.0, 31, true), 2147483648.0);
    }

    #[test]
    fn a_sign_keeps_its_size() {
        assert_eq!(signed_value(-2.5, 1, 1.0), 2.5);
        assert_eq!(signed_value(2.5, -1, 1.0), -2.5);
        assert_eq!(signed_value(0.0, -1, 3.0), -3.0);
        assert_eq!(signed_value(0.0, 1, 0.0), 1.0);
    }
}
