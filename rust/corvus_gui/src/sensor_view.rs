//! The sensor page drawn: the Status card with the switch, the pickers and
//! the wiring line, the Hardware card with presets and the parameters the
//! operator adds, after the `.safety-sensor*`, `.safety-preset*` and
//! `.safety-extra*` rules of setup-forms.css.
//!
//! Nothing here sends a request. A press is reported as an `Event` and
//! `safety.rs` runs it, because it owns the reads that must follow a write.

use std::collections::HashMap;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::http::Server;
use crate::icons;
use crate::kit::{self, Button, Choice, Variant};
use crate::schema_form::{Field, FormState, Look};
use crate::sensors::{self, Extras, Preset, Toggle, CUSTOM};
use crate::style::{self, Weight};

/// A status line: its class (`ok`, `err`, `pending` or empty) and its text.
pub type Status = (&'static str, String);

/// What the operator did this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The sensor's switch was pressed; the value is where it should go.
    Switch(bool),
    /// The model or the port picker changed.
    Repick,
    /// Apply this preset.
    Apply(String),
    /// Add this parameter name (not yet checked).
    AddExtra(String),
    RemoveExtra(String),
}

/// What the sensor pages remember between frames.
#[derive(Default)]
pub struct SensorUi {
    driver: HashMap<String, String>,
    port: HashMap<String, String>,
    preset: HashMap<String, String>,
    preset_port: HashMap<String, String>,
    /// The switch where it was last put, until the page has been read again.
    pub shown_on: Option<bool>,
    pub status: Status,
    pub preset_status: Status,
    pub extra_text: String,
    pub extra_status: Status,
}

impl SensorUi {
    /// The model the picker names: the operator's choice, else the backend's.
    pub fn driver_id(&self, sid: &str, t: &Toggle) -> String {
        self.driver
            .get(sid)
            .cloned()
            .filter(|id| t.drivers.iter().any(|d| &d.id == id))
            .or_else(|| Some(t.selected.clone()).filter(|s| t.drivers.iter().any(|d| &d.id == s)))
            .or_else(|| t.drivers.first().map(|d| d.id.clone()))
            .unwrap_or_default()
    }

    /// The serial port the switch's picker names, `""` when there is none.
    pub fn port_text(&self, sid: &str, t: &Toggle) -> String {
        self.port
            .get(sid)
            .cloned()
            .filter(|v| t.ports.iter().any(|p| &p.value == v))
            .or_else(|| t.port.clone().filter(|v| t.ports.iter().any(|p| &p.value == v)))
            .or_else(|| t.ports.first().map(|p| p.value.clone()))
            .unwrap_or_default()
    }

    /// The serial port a preset's picker names.
    pub fn preset_port_text(&self, sid: &str, t: &Toggle) -> String {
        self.preset_port
            .get(sid)
            .cloned()
            .filter(|v| t.ports.iter().any(|p| &p.value == v))
            .unwrap_or_else(|| self.port_text(sid, t))
    }

    /// `presetChoice`: Custom unless the remembered preset is still offered.
    pub fn preset_choice(&self, sid: &str, presets: &[Preset]) -> String {
        match self.preset.get(sid) {
            Some(want) if want != CUSTOM && presets.iter().any(|p| &p.id == want) => want.clone(),
            _ => CUSTOM.to_string(),
        }
    }
}

fn status_color(cls: &str) -> Color32 {
    match cls {
        "ok" => style::healthy(),
        "err" => style::critical(),
        _ => style::nav(),
    }
}

/// `.field-select`'s height: 2 + 12 + 12 * 1.21.
const SELECT_H: f32 = 28.52;

fn select_width(ui: &egui::Ui, choices: &[Choice], min: f32, max: f32, avail: f32) -> f32 {
    let widest = choices
        .iter()
        .map(|c| {
            style::galley(ui, &c.label, 12.0, Weight::W400, style::text_1(), 0.0)
                .size()
                .x
        })
        .fold(0.0, f32::max);
    (widest + 11.0 + 26.0 + 2.0).clamp(min, max).min(avail)
}

/// A select, or its disabled box while armed. Returns the value picked.
#[allow(clippy::too_many_arguments)]
fn pick(
    ui: &mut egui::Ui,
    rect: Rect,
    choices: &[Choice],
    current: &str,
    armed: bool,
    title: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> Option<String> {
    if !armed {
        return kit::select(ui, rect, choices, current, "", false, title, id);
    }
    {
        let r = rect;
        ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
        ui.painter().rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::border()),
            egui::StrokeKind::Inside,
        );
    }
    let label = choices
        .iter()
        .find(|c| c.value == current)
        .map_or(String::new(), |c| c.label.clone());
    let g = style::galley(ui, &label, 12.0, Weight::W400, style::text_3(), 0.0);
    ui.painter_at(rect.shrink2(Vec2::new(11.0, 0.0))).galley(
        Pos2::new(rect.left() + 11.0, rect.center().y - g.size().y / 2.0),
        g,
        style::text_3(),
    );
    None
}

/// `labelled`: a small uppercase caption above a control; returns the
/// caption's height and the y the control starts at.
fn caption(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 10.5, Weight::W700, style::text_3(), 0.4);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x, y), g, style::text_3());
    y + h + 4.0
}

fn caption_h(ui: &egui::Ui) -> f32 {
    style::galley(ui, "X", 10.5, Weight::W700, style::text_3(), 0.4)
        .size()
        .y
        + 4.0
}

/// `wiringLine`: a plug and one sentence; returns the bottom.
fn wiring(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    icons::paint(
        ui,
        "plug",
        Rect::from_min_size(Pos2::new(x, y + 2.0), Vec2::splat(14.0)),
        style::text_3(),
    );
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, style::text_2(), w - 21.0, 1.5);
    ui.painter().galley(Pos2::new(x + 21.0, y), g.clone(), style::text_2());
    y + g.size().y
}

fn warning_line(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str, icon_gap: f32) -> f32 {
    icons::paint(
        ui,
        "triangle-alert",
        Rect::from_min_size(Pos2::new(x, y + 2.0), Vec2::splat(14.0)),
        style::warning(),
    );
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, style::warning(), w - 14.0 - icon_gap, 1.5);
    ui.painter()
        .galley(Pos2::new(x + 14.0 + icon_gap, y), g.clone(), style::warning());
    y + g.size().y
}

/// The raised box the status and the preset detail sit in.
fn box_bg(ui: &egui::Ui, bg: egui::layers::ShapeIdx, rect: Rect) {
    let p = ui.painter();
    p.set(bg, egui::Shape::rect_filled(rect, 10, style::surface_2()));
    p.rect_stroke(
        rect,
        10,
        Stroke::new(1.0, style::border_soft()),
        egui::StrokeKind::Inside,
    );
}

fn note(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, style::text_2(), w, 1.5);
    ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
    y + g.size().y
}

/// `sensorHeader`: the switch, what is missing, the pickers and the wiring.
/// Returns the bottom of the box.
#[allow(clippy::too_many_arguments)]
pub fn status_box(
    ui: &mut egui::Ui,
    st: &mut SensorUi,
    sid: &str,
    t: &Toggle,
    x: f32,
    y: f32,
    w: f32,
    armed: bool,
    events: &mut Vec<Event>,
) -> f32 {
    let painter = ui.painter().clone();
    let bg = painter.add(egui::Shape::Noop);
    let (ix, iw) = (x + 13.0, w - 26.0);
    let mut cy = y + 12.0;
    let on = st.shown_on.unwrap_or(t.enabled);

    let label = style::galley(ui, &t.label, 13.0, Weight::W600, style::text_1(), 0.0);
    let detail = kit::wrapped_weight(
        ui,
        &t.detail,
        11.5,
        Weight::W400,
        style::text_2(),
        iw - 38.0 - 12.0,
        1.3,
    );
    let text_h = label.size().y + 2.0 + detail.size().y;
    let main_h = text_h.max(22.0);
    painter.galley(
        Pos2::new(ix, cy + (main_h - text_h) / 2.0),
        label.clone(),
        style::text_1(),
    );
    painter.galley(
        Pos2::new(ix, cy + (main_h - text_h) / 2.0 + label.size().y + 2.0),
        detail,
        style::text_2(),
    );
    if kit::switch(
        ui,
        Pos2::new(ix + iw - 38.0, cy + (main_h - 22.0) / 2.0),
        on,
        !armed,
        ("sensorswitch", sid),
    ) {
        st.shown_on = Some(!on);
        events.push(Event::Switch(!on));
    }
    cy += main_h;

    for p in &t.problems {
        cy += if std::ptr::eq(p, &t.problems[0]) { 18.0 } else { 4.0 };
        cy = warning_line(ui, ix, cy, iw, p, 7.0);
    }
    if !t.note.is_empty() {
        cy = note(ui, ix, cy + 18.0, iw, &t.note);
    }

    // the pickers
    let driver_id = st.driver_id(sid, t);
    let picked = t.drivers.iter().find(|d| d.id == driver_id).cloned();
    cy += 10.0;
    let cap = caption_h(ui);
    let row_top = cy;
    let mut px = ix;
    // under 721 the pickers stack, each as wide as the box
    let narrow = crate::responsive::Vw::of(ui.ctx()).max720;
    let mut fy = row_top;
    let mut repick = false;
    if !t.drivers.is_empty() {
        let choices: Vec<Choice> = t
            .drivers
            .iter()
            .map(|d| {
                Choice::new(
                    d.id.clone(),
                    if d.blocked.is_empty() {
                        d.label.clone()
                    } else {
                        format!("{}  (cannot be set up here)", d.label)
                    },
                )
            })
            .collect();
        let sw = if narrow {
            iw
        } else {
            select_width(ui, &choices, 220.0, 340.0, iw)
        };
        let (fx, top) = if narrow { (ix, fy) } else { (px, row_top) };
        caption(ui, fx, top, "Sensor");
        let r = Rect::from_min_size(Pos2::new(fx, top + cap), Vec2::new(sw, SELECT_H));
        if let Some(v) = pick(
            ui,
            r,
            &choices,
            &driver_id,
            armed,
            &format!("{} model", t.label),
            ("driver", sid),
        ) {
            if v != driver_id {
                st.driver.insert(sid.to_string(), v);
                repick = true;
            }
        }
        px += sw + 12.0;
        fy += cap + SELECT_H + 12.0;
    }
    if !t.ports.is_empty() && picked.as_ref().is_some_and(|d| d.serial) {
        let choices: Vec<Choice> = t
            .ports
            .iter()
            .map(|p| Choice::new(p.value.clone(), p.label.clone()))
            .collect();
        let sw = if narrow {
            iw
        } else {
            select_width(ui, &choices, 150.0, 300.0, iw)
        };
        let port = st.port_text(sid, t);
        let (fx, top) = if narrow { (ix, fy) } else { (px, row_top) };
        caption(ui, fx, top, "Serial port");
        let r = Rect::from_min_size(Pos2::new(fx, top + cap), Vec2::new(sw, SELECT_H));
        if let Some(v) = pick(
            ui,
            r,
            &choices,
            &port,
            armed,
            &format!("{} serial port", t.label),
            ("port", sid),
        ) {
            if v != port {
                st.port.insert(sid.to_string(), v);
                repick = true;
            }
        }
        px += sw + 12.0;
        fy += cap + SELECT_H + 12.0;
    }
    if repick {
        // A driver that cannot be set up from here, or a port something else
        // already runs on, says why the moment it is picked.
        let driver = t.drivers.iter().find(|d| d.id == st.driver_id(sid, t));
        let problem = driver
            .map(|d| {
                if !d.blocked.is_empty() {
                    d.blocked.clone()
                } else if d.serial {
                    sensors::port_problem(t, &st.port_text(sid, t))
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        st.status = if problem.is_empty() {
            ("", String::new())
        } else {
            ("err", problem)
        };
        events.push(Event::Repick);
    }
    if !st.status.1.is_empty() {
        let g = style::galley(ui, &st.status.1, 10.5, Weight::W400, status_color(st.status.0), 0.0);
        let at = if narrow {
            Pos2::new(ix, fy)
        } else {
            Pos2::new(px, row_top + cap + (SELECT_H - g.size().y) / 2.0)
        };
        painter.galley(at, g, status_color(st.status.0));
    }
    cy = if narrow {
        fy - 12.0 + if st.status.1.is_empty() { 0.0 } else { 12.0 + 13.0 }
    } else {
        row_top + cap + SELECT_H
    };

    if let Some(d) = &picked {
        if !d.wiring.is_empty() {
            cy = wiring(ui, ix, cy + 18.0, iw, &d.wiring);
        }
    }
    let rect = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 12.0));
    box_bg(ui, bg, rect);
    rect.bottom()
}

/// The write list of a preset: every parameter, its value and why, with what
/// the preset considered and left alone below. Returns the bottom.
fn write_table(ui: &egui::Ui, x: f32, y: f32, w: f32, p: &Preset, port_text: &str) -> f32 {
    let painter = ui.painter().clone();
    let c1 = (w * 0.34).clamp(120.0, 175.0);
    let c2 = (w * 0.2).clamp(70.0, 108.0);
    let c3 = (w - c1 - c2 - 20.0).max(60.0);
    let mut cy = y;
    let mut first = true;
    let mut row = |cy: &mut f32, param: String, value: String, pending: bool, why: &str, dim: bool| {
        if !first {
            painter.hline(
                egui::Rangef::new(x, x + w),
                *cy + 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
        }
        first = false;
        let fade = if dim { 0.7 } else { 1.0 };
        let top = *cy + 3.0;
        let name = ui
            .painter()
            .layout(param, style::mono(11.0), style::text_2().gamma_multiply(fade), c1);
        let val = ui.painter().layout(
            value,
            style::mono(11.0),
            if pending { style::text_3() } else { style::text_1() }.gamma_multiply(fade),
            c2,
        );
        let why = kit::wrapped_weight(
            ui,
            why,
            11.0,
            Weight::W400,
            style::text_3().gamma_multiply(fade),
            c3,
            1.4,
        );
        let h = name.size().y.max(val.size().y).max(why.size().y);
        painter.galley(Pos2::new(x, top), name, style::text_2());
        painter.galley(Pos2::new(x + c1 + 10.0, top), val, style::text_1());
        painter.galley(Pos2::new(x + c1 + c2 + 20.0, top), why, style::text_3());
        *cy += 3.0 + h + 3.0 + 1.0;
    };
    for wr in &p.writes {
        let name = if wr.port_param {
            sensors::port_param(&wr.param, port_text)
        } else {
            wr.param.clone()
        };
        let value = if wr.port {
            "the port below".to_string()
        } else {
            crate::schema_form::format_number(wr.value.unwrap_or(0.0))
        };
        row(&mut cy, name, value, wr.port, &wr.label, false);
    }
    for (param, value, why) in &p.kept {
        row(
            &mut cy,
            param.clone(),
            format!("stays {}", crate::schema_form::format_number(*value)),
            false,
            why,
            true,
        );
    }
    cy
}

/// The body of the Hardware card under its title: the hint, the module
/// picker and the detail box. Returns the bottom.
#[allow(clippy::too_many_arguments)]
pub fn hardware_body(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    server: &Server,
    st: &mut SensorUi,
    form: &mut FormState,
    extras: &Extras,
    extra_fields: &[(Field, bool)],
    sid: &str,
    title: &str,
    t: &Toggle,
    presets: &[Preset],
    x: f32,
    y: f32,
    w: f32,
    armed: bool,
    busy: bool,
    events: &mut Vec<Event>,
) -> f32 {
    let painter = ui.painter().clone();
    let mut cy = y;
    let g = kit::wrapped_weight(
        ui,
        "Pick the module you actually fitted and Corvus writes its whole setup: driver, \
         estimator, and the numbers from its datasheet. Custom writes nothing: the \
         parameters are yours, and you can add any others this airframe needs.",
        10.5,
        Weight::W400,
        style::text_3(),
        w,
        1.4,
    );
    painter.galley(Pos2::new(x, cy), g.clone(), style::text_3());
    cy += g.size().y + 10.0;

    let mut choices = vec![Choice::new(CUSTOM, "Custom: set the parameters yourself")];
    choices.extend(
        presets
            .iter()
            .map(|p| Choice::new(p.id.clone(), sensors::preset_option_label(p))),
    );
    let chosen = st.preset_choice(sid, presets);
    let cap = caption_h(ui);
    caption(ui, x, cy, "Module");
    let sw = if crate::responsive::Vw::of(ui.ctx()).max720 {
        w
    } else {
        select_width(ui, &choices, 280.0, 460.0, w)
    };
    let r = Rect::from_min_size(Pos2::new(x, cy + cap), Vec2::new(sw, SELECT_H));
    if let Some(v) = pick(
        ui,
        r,
        &choices,
        &chosen,
        armed,
        &format!("{title} hardware"),
        ("preset", sid),
    ) {
        if v != chosen {
            st.preset.insert(sid.to_string(), v);
            st.preset_status = ("", String::new());
        }
    }
    cy += cap + SELECT_H + 10.0;

    // the detail box
    let bg = painter.add(egui::Shape::Noop);
    let (ix, iw) = (x + 13.0, w - 26.0);
    let mut dy = cy + 12.0;
    let chosen = st.preset_choice(sid, presets);
    if chosen == CUSTOM {
        dy = extras_body(
            ui,
            ctx,
            server,
            st,
            form,
            extras,
            extra_fields,
            sid,
            ix,
            dy,
            iw,
            armed,
            events,
        );
    } else if let Some(p) = presets.iter().find(|p| p.id == chosen) {
        let mut first = true;
        let mut gap = |dy: &mut f32| {
            if !first {
                *dy += 10.0;
            }
            first = false;
        };
        if !p.summary.is_empty() {
            gap(&mut dy);
            let g = kit::wrapped_weight(ui, &p.summary, 12.5, Weight::W400, style::text_1(), iw, 1.4);
            painter.galley(Pos2::new(ix, dy), g.clone(), style::text_1());
            dy += g.size().y;
        }
        if !p.wiring.is_empty() {
            gap(&mut dy);
            dy = wiring(ui, ix, dy, iw, &p.wiring);
        }
        if !p.note.is_empty() {
            gap(&mut dy);
            dy = note(ui, ix, dy, iw, &p.note);
        }
        if !p.supported {
            gap(&mut dy);
            let text = if p.unsupported.is_empty() {
                "This module cannot be configured from here."
            } else {
                &p.unsupported
            };
            dy = warning_line(ui, ix, dy, iw, text, 8.0);
        } else {
            let port_text = if p.serial {
                st.preset_port_text(sid, t)
            } else {
                String::new()
            };
            if p.serial {
                gap(&mut dy);
                let choices: Vec<Choice> = t
                    .ports
                    .iter()
                    .map(|q| Choice::new(q.value.clone(), q.label.clone()))
                    .collect();
                let sw = select_width(ui, &choices, 150.0, 300.0, iw);
                caption(ui, ix, dy, "Serial port");
                let r = Rect::from_min_size(Pos2::new(ix, dy + cap), Vec2::new(sw, SELECT_H));
                if let Some(v) = pick(
                    ui,
                    r,
                    &choices,
                    &port_text,
                    armed,
                    &format!("{} serial port", p.label),
                    ("presetport", sid),
                ) {
                    st.preset_port.insert(sid.to_string(), v);
                }
                dy += cap + SELECT_H;
            }
            gap(&mut dy);
            dy = write_table(ui, ix, dy, iw, p, &port_text);
            if !p.missing.is_empty() {
                gap(&mut dy);
                let g = kit::wrapped_weight(
                    ui,
                    &format!(
                        "This firmware does not carry {}. Those are skipped, the rest is written.",
                        p.missing.join(", ")
                    ),
                    11.0,
                    Weight::W400,
                    style::warning(),
                    iw,
                    1.4,
                );
                painter.galley(Pos2::new(ix, dy), g.clone(), style::warning());
                dy += g.size().y;
            }
            gap(&mut dy);
            let label = format!("Apply {}", p.label);
            let b = Button::new(&label, Variant::Primary)
                .icon("check-check")
                .small()
                .enabled(!armed && !busy);
            let (bw, bh) = (b.width(ui), b.height());
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(ix, dy), Vec2::new(bw, bh)),
                ("applypreset", &p.id),
            ) {
                events.push(Event::Apply(p.id.clone()));
            }
            if !st.preset_status.1.is_empty() {
                let c = status_color(st.preset_status.0);
                let g = style::galley(ui, &st.preset_status.1, 10.5, Weight::W400, c, 0.0);
                painter.galley(Pos2::new(ix + bw + 10.0, dy + (bh - g.size().y) / 2.0), g, c);
            }
            dy += bh;
        }
    }
    let rect = Rect::from_min_max(Pos2::new(x, cy), Pos2::new(x + w, dy + 12.0));
    box_bg(ui, bg, rect);
    rect.bottom()
}

/// `renderExtraParams`: under Custom, the parameters the operator named.
#[allow(clippy::too_many_arguments)]
fn extras_body(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    server: &Server,
    st: &mut SensorUi,
    form: &mut FormState,
    extras: &Extras,
    extra_fields: &[(Field, bool)],
    sid: &str,
    x: f32,
    y: f32,
    w: f32,
    armed: bool,
    events: &mut Vec<Event>,
) -> f32 {
    let painter = ui.painter().clone();
    let mut cy = note(
        ui,
        x,
        y,
        w,
        "Nothing is written until you change a field. If this airframe needs a parameter \
         the page does not show, name it below and it joins the form, read with \
         everything else, written the same way.",
    );
    cy += 10.0;
    let names = extras.names(sid);
    if names.is_empty() {
        let g = style::galley(ui, "No extra parameters yet.", 11.5, Weight::W400, style::text_3(), 0.0);
        painter.galley(Pos2::new(x, cy), g.clone(), style::text_3());
        cy += g.size().y;
    }
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            painter.hline(
                egui::Rangef::new(x, x + w),
                cy + 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
        }
        let found = extra_fields.iter().find(|(f, _)| &f.param == name);
        let row_top = cy;
        let end;
        match found {
            Some((f, true)) => {
                cy = form.show_with(ui, ctx, server, std::slice::from_ref(f), x, cy, w, armed, Look::EXTRA);
                end = form.last_end;
            }
            _ => {
                let label_w = 230f32.min((w * 0.5).max(140.0));
                let lg = ui
                    .painter()
                    .layout(name.clone(), style::mono(11.5), style::text_1(), label_w);
                let g = style::galley(
                    ui,
                    "this firmware does not have it",
                    11.5,
                    Weight::W400,
                    style::warning(),
                    0.0,
                );
                let rh = 28.52f32;
                painter.galley(
                    Pos2::new(x, row_top + 3.0 + (rh - lg.size().y) / 2.0),
                    lg,
                    style::text_1(),
                );
                let gx = x + label_w + 12.0;
                painter.galley(
                    Pos2::new(gx, row_top + 3.0 + (rh - g.size().y) / 2.0),
                    g.clone(),
                    style::warning(),
                );
                end = gx + g.size().x;
                cy = row_top + 3.0 + rh + 3.0;
            }
        }
        let rh = cy - row_top;
        let r = Rect::from_min_size(
            Pos2::new((end + 8.0).min(x + w - 26.0), row_top + (rh - 26.0) / 2.0),
            Vec2::splat(26.0),
        );
        if kit::icon_btn(ui, r, "x", &format!("Remove {name}"), false, ("rmextra", name)) && !armed {
            events.push(Event::RemoveExtra(name.clone()));
        }
        cy = cy.max(r.bottom());
    }
    cy += 10.0;
    // the add row
    let cap = caption_h(ui);
    caption(ui, x, cy, "Add a parameter");
    let field = Rect::from_min_size(Pos2::new(x, cy + cap), Vec2::new(200.0, SELECT_H));
    let resp = kit::text_field(
        ui,
        field,
        &mut st.extra_text,
        "EKF2_RNG_QLTY_T",
        true,
        ("extrainput", sid),
    );
    let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let b = Button::new("Add", Variant::Secondary).icon("plus").small();
    let (bw, bh) = (b.width(ui), b.height());
    let bx = field.right() + 10.0;
    let clicked = b.show(
        ui,
        Rect::from_min_size(Pos2::new(bx, cy + cap + SELECT_H - bh), Vec2::new(bw, bh)),
        ("addextra", sid),
    );
    if !st.extra_status.1.is_empty() {
        let c = status_color(st.extra_status.0);
        let g = style::galley(ui, &st.extra_status.1, 10.5, Weight::W400, c, 0.0);
        painter.galley(
            Pos2::new(bx + bw + 10.0, cy + cap + (SELECT_H - g.size().y) / 2.0),
            g,
            c,
        );
    }
    if clicked || entered {
        events.push(Event::AddExtra(st.extra_text.trim().to_uppercase()));
    }
    cy + cap + SELECT_H
}
