//! The Vehicles dialog the top bar's Vehicle block opens (`openDialog` of
//! `python/src/js/fleet.js`): one row per vehicle with its colour, name,
//! status, readout, locate and take control, then the links, the radio
//! auto-connect just saw, the serial ports no link holds and a field for any
//! other connection.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use super::{detail_text, parse_hex, readout_text, status_text, Fleet, Snapshot, Vehicle};
use crate::data::units::Units;
use crate::map::Map;
use crate::net::bg::Pending;
use crate::net::http;
use crate::ui::card::{self, Look};
use crate::ui::field::{self, TextField};
use crate::ui::icons;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::modal::{self, Size};
use crate::ui::style::{self, Weight};

const ROW_PAD_V: f32 = 10.0;
const ROW_PAD_H: f32 = 12.0;
const SWATCH: f32 = 20.0;
const CHIP: f32 = 18.0;
const NAME_H: f32 = 24.0;
const LINK_H: f32 = 35.0;
const FIELD_H: f32 = 32.5;

/// A serial port the machine has.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub device: String,
    pub description: String,
}

/// The serial device of a `serial:<device>:<baud>` connection, or "".
pub fn serial_device(conn: &str) -> String {
    let Some(rest) = conn.strip_prefix("serial:") else {
        return String::new();
    };
    match rest.rsplit_once(':') {
        Some((dev, baud)) if !baud.is_empty() && baud.bytes().all(|b| b.is_ascii_digit()) => dev.to_string(),
        _ => rest.to_string(),
    }
}

/// The same device whatever the case or the macOS prefix (`/dev/tty.` and
/// `/dev/cu.` are one port).
pub fn same_device(a: &str, b: &str) -> bool {
    let norm = |d: &str| d.to_lowercase().replace("/dev/tty.", "/dev/cu.");
    !a.is_empty() && norm(a) == norm(b)
}

/// Whether a link already holds `device`.
pub fn in_use(snap: &Snapshot, device: &str) -> bool {
    snap.links
        .iter()
        .any(|l| same_device(&serial_device(&l.connection), device))
}

/// The ports no link uses.
pub fn free_ports<'a>(snap: &Snapshot, ports: &'a [Port]) -> Vec<&'a Port> {
    ports.iter().filter(|p| !in_use(snap, &p.device)).collect()
}

/// The offer of the radio auto-connect just saw plugged in: its connection
/// string and the sentence, unless a link holds that port already.
pub fn offer(snap: &Snapshot) -> Option<(String, String)> {
    let s = snap.suggestion.as_ref()?;
    let conn = s
        .get("connection_string")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())?;
    let device = serial_device(conn);
    if in_use(snap, &device) {
        return None;
    }
    let kind = if s.get("kind").and_then(Value::as_str) == Some("sik") {
        "Telemetry radio"
    } else {
        "Flight controller"
    };
    let on = s
        .get("device")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map_or(device, String::from);
    Some((conn.to_string(), format!("{kind} on {on} plugged in just now.")))
}

/// What asking to take control or to remove says (`confirmSwitch`,
/// `confirmRemove`): paragraphs, the last one in the warning colour when
/// `warn` is set.
pub fn switch_text(current: &Vehicle, target: &Vehicle) -> (String, Vec<String>) {
    let mode = if current.mode_label.is_empty() {
        "its current mode"
    } else {
        current.mode_label.as_str()
    };
    (
        format!("Take control of {}?", target.name),
        vec![
            format!(
                "{} is armed. It keeps flying in {mode}, and the station stops sending it commands.",
                current.name
            ),
            format!(
                "From now on the flight bar, the joystick and every page control {}.",
                target.name
            ),
        ],
    )
}

/// The words of the removal question and its warning for an armed vehicle.
pub fn remove_text(v: &Vehicle, snap: &Snapshot) -> (String, String, Option<String>) {
    let link = snap.link_of(v);
    let body = match link {
        Some(l) if l.primary => "The main link is disconnected and stays closed. It leaves the list and the map. Connect it again on the LINK tab.".to_string(),
        l => {
            let conn = l.map(|l| format!(" ({})", l.connection)).unwrap_or_default();
            format!("Its link{conn} is closed. It leaves the list and the map. To bring it back, open the link again.")
        }
    };
    let warn = (v.armed && !v.stale).then(|| {
        let mode = if v.mode_label.is_empty() {
            "its current mode"
        } else {
            v.mode_label.as_str()
        };
        format!(
            "{} is armed. It keeps flying in {mode}, and the station no longer sees it or sends it anything.",
            v.name
        )
    });
    (format!("Remove {}?", v.name), body, warn)
}

/// The dialog's state.
#[derive(Default)]
pub struct Dialog {
    pub open: bool,
    focus: Option<String>,
    palette_for: Option<String>,
    conn: String,
    ports: Vec<Port>,
    ports_load: Option<Pending<Result<Value, String>>>,
    take: Option<String>,
    remove: Option<String>,
    error: Option<String>,
    offered: String,
    removed: Vec<String>,
}

/// The link's state colour: connected is healthy, a link on its way a
/// warning, anything else off.
fn link_level(status: &str) -> &'static str {
    match status {
        "connected" => "healthy",
        "degraded" | "connecting" | "reconnecting" => "warning",
        _ => "off",
    }
}

fn caption(ui: &egui::Ui, text: &str) -> std::sync::Arc<egui::Galley> {
    style::galley(ui, &text.to_uppercase(), 11.0, Weight::W600, style::text_3(), 0.66)
}

fn circle(ui: &egui::Ui, c: Pos2, size: f32, fill: Color32) {
    let p = ui.painter();
    p.circle_filled(c, size / 2.0, fill);
    p.circle_stroke(c, size / 2.0 - 1.0, Stroke::new(2.0, Color32::from_white_alpha(235)));
}

impl Dialog {
    /// The vehicles the operator removed since the last call.
    pub fn take_removed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.removed)
    }

    /// Opens the dialog, scrolled to one vehicle when `focus` is given.
    pub fn open(&mut self, ctx: &egui::Context, server: &http::Server, focus: Option<String>) {
        self.open = true;
        self.focus = focus;
        self.palette_for = None;
        self.error = None;
        self.refresh_ports(ctx, server);
    }

    fn refresh_ports(&mut self, ctx: &egui::Context, server: &http::Server) {
        let server = server.clone();
        self.ports_load = Some(Pending::spawn(ctx, move || {
            http::get_json(&server, "/api/mavlink/serial-ports")
        }));
    }

    fn poll(&mut self) {
        if let Some(Ok(v)) = self.ports_load.as_ref().and_then(Pending::take) {
            self.ports = v
                .get("ports")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|p| {
                            Some(Port {
                                device: p.get("device")?.as_str()?.to_string(),
                                description: p.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
    }

    /// Draws the dialog and its two questions.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        fleet: &Fleet,
        server: &http::Server,
        state: &Value,
        units: &Units,
        map: &mut Map,
    ) {
        if !self.open {
            return;
        }
        self.poll();
        let snap = fleet.snapshot();
        if self.take.is_some() || self.remove.is_some() {
            self.questions(ctx, fleet, &snap, state);
            return;
        }
        let mut close = false;
        let mut select: Option<String> = None;
        let mut locate: Option<[f64; 2]> = None;
        let mut rename: Option<(String, String)> = None;
        let mut color: Option<(String, String)> = None;
        let mut ask_remove: Option<String> = None;
        let mut close_link: Option<String> = None;
        let mut open_conn: Option<String> = None;
        let mut refresh = false;
        let armed_now = state.get("armed") == Some(&Value::Bool(true));
        let this = &mut *self;
        close |= modal::show(ctx, "fleet", Size::Lg, "Vehicles", |ui, x, y, w| {
            let mut y = y;
            let head = caption(ui, "Vehicles");
            ui.painter()
                .galley(Pos2::new(x, y + 4.0), head.clone(), style::text_3());
            y += 4.0 + head.size().y + 8.0;
            if snap.vehicles.is_empty() {
                let g = kit::wrapped(
                    ui,
                    "No vehicle heard yet. Connect one on the LINK tab, or open another link below.",
                    12.0,
                    style::text_3(),
                    w,
                );
                ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_3());
                y += g.size().y + 8.0;
            }
            for v in &snap.vehicles {
                let open_palette = this.palette_for.as_deref() == Some(v.id.as_str());
                let chips = snap.palette.len() + 1;
                let per_line = (((w - 2.0 * ROW_PAD_H + 8.0) / (CHIP + 8.0)).floor() as usize).max(1);
                let lines = chips.div_ceil(per_line);
                let palette_h = if open_palette {
                    6.0 + 1.0 + 10.0 + lines as f32 * CHIP + (lines.saturating_sub(1)) as f32 * 8.0
                } else {
                    0.0
                };
                let main_h = NAME_H + 1.0 + 13.5 + 1.0 + 13.5;
                let h = ROW_PAD_V * 2.0 + main_h + palette_h;
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
                let ring = v.active || this.focus.as_deref() == Some(v.id.as_str());
                card::paint(ui.painter(), r, Look::Page);
                if ring {
                    let stroke = Stroke::new(
                        2.0,
                        if v.active {
                            style::accent()
                        } else {
                            style::accent_bright()
                        },
                    );
                    ui.painter()
                        .rect_stroke(r, style::RADIUS, stroke, egui::StrokeKind::Inside);
                }
                let dim = if v.stale { 0.55 } else { 1.0 };
                let paint_color = parse_hex(&v.color).unwrap_or_else(style::vehicle);
                let sw = Rect::from_center_size(
                    Pos2::new(r.left() + ROW_PAD_H + SWATCH / 2.0, r.top() + ROW_PAD_V + main_h / 2.0),
                    Vec2::splat(SWATCH),
                );
                circle(ui, sw.center(), SWATCH, paint_color.gamma_multiply(dim));
                let sresp = ui
                    .interact(sw, ui.scope_id().with(("swatch", &v.id)), egui::Sense::click())
                    .on_hover_text("Colour on the map");
                if sresp.clicked() {
                    this.palette_for = if open_palette { None } else { Some(v.id.clone()) };
                }

                // The right side, from the edge in.
                let mid_y = r.top() + ROW_PAD_V + main_h / 2.0;
                let mut right = r.right() - ROW_PAD_H;
                let trash = Rect::from_center_size(Pos2::new(right - 12.0, mid_y), Vec2::splat(24.0));
                if kit::icon_btn(ui, trash, "trash-2", "Remove this vehicle", false, ("rm", &v.id)) {
                    ask_remove = Some(v.id.clone());
                }
                right = trash.left() - 8.0;
                if v.active {
                    let g = style::galley(ui, "In control", 11.0, Weight::W600, style::accent_bright(), 0.0);
                    let pill = Rect::from_center_size(
                        Pos2::new(right - (g.size().x + 18.0) / 2.0, mid_y),
                        Vec2::new(g.size().x + 18.0, g.size().y + 6.0),
                    );
                    ui.painter().rect_filled(pill, 255, style::tint(0.06));
                    ui.painter()
                        .rect_stroke(pill, 255, Stroke::new(1.0, style::tint(0.09)), egui::StrokeKind::Inside);
                    ui.painter().galley(
                        Pos2::new(pill.left() + 9.0, pill.center().y - g.size().y / 2.0),
                        g,
                        style::accent_bright(),
                    );
                    right = pill.left() - 8.0;
                } else {
                    let b = Button::new("Take control", Variant::Primary).small().enabled(!v.stale);
                    let (bw, bh) = (b.width(ui), b.height());
                    let br = Rect::from_min_size(Pos2::new(right - bw, mid_y - bh / 2.0), Vec2::new(bw, bh));
                    if b.show(ui, br, ("take", &v.id)) {
                        if armed_now && snap.active_vehicle().is_some_and(|c| !c.stale) {
                            this.take = Some(v.id.clone());
                        } else {
                            select = Some(v.id.clone());
                        }
                    }
                    right = br.left() - 8.0;
                }
                let loc = Rect::from_center_size(Pos2::new(right - 12.0, mid_y), Vec2::splat(24.0));
                if v.position.is_some() {
                    if kit::icon_btn(ui, loc, "crosshair", "Show on the map", false, ("loc", &v.id)) {
                        locate = v.position;
                    }
                } else {
                    icons::paint(ui, "crosshair", loc.shrink(5.0), style::text_disabled());
                }
                right = loc.left() - 8.0;
                let st_color = if v.stale {
                    style::critical()
                } else if v.armed {
                    style::warning()
                } else {
                    style::text_3()
                };
                let st = style::galley(ui, &status_text(v).to_uppercase(), 10.0, Weight::W600, st_color, 0.5);
                ui.painter().galley(
                    Pos2::new(right - st.size().x, mid_y - st.size().y / 2.0),
                    st.clone(),
                    st_color,
                );
                right -= st.size().x + 12.0;

                // The name, the detail and the readout.
                let left = sw.right() + 12.0;
                let top = r.top() + ROW_PAD_V;
                let name_rect = Rect::from_min_size(
                    Pos2::new(left - 7.0, top),
                    Vec2::new((right - left).max(80.0) + 7.0, NAME_H),
                );
                let shown = if v.custom_name { v.name.as_str() } else { "" };
                if let Some(t) = field::committed(ui, name_rect, ("name", &v.id), shown, &v.name, false) {
                    rename = Some((v.id.clone(), t.trim().chars().take(40).collect()));
                }
                let clip = Rect::from_min_max(Pos2::new(left, top), Pos2::new(right, r.bottom()));
                let dg = style::galley(ui, &detail_text(v, &snap), 11.0, Weight::W400, style::text_3(), 0.0);
                ui.painter_at(clip).galley(
                    Pos2::new(left, top + NAME_H + 1.0),
                    dg,
                    style::text_3().gamma_multiply(dim),
                );
                let rg = style::galley(ui, &readout_text(v, units), 11.0, Weight::W400, style::text_2(), 0.0);
                ui.painter_at(clip).galley(
                    Pos2::new(left, top + NAME_H + 1.0 + 13.5 + 1.0),
                    rg,
                    style::text_2().gamma_multiply(dim),
                );

                if open_palette {
                    let line_y = top + main_h + 6.0;
                    ui.painter().hline(
                        (r.left() + ROW_PAD_H)..=(r.right() - ROW_PAD_H),
                        line_y,
                        Stroke::new(1.0, style::tint(0.09)),
                    );
                    let mut choices: Vec<String> = vec![String::new()];
                    choices.extend(snap.palette.iter().cloned());
                    for (i, c) in choices.iter().enumerate() {
                        let (row, col) = (i / per_line, i % per_line);
                        let center = Pos2::new(
                            r.left() + ROW_PAD_H + CHIP / 2.0 + col as f32 * (CHIP + 8.0),
                            line_y + 1.0 + 10.0 + CHIP / 2.0 + row as f32 * (CHIP + 8.0),
                        );
                        let fill = parse_hex(c).unwrap_or_else(style::vehicle);
                        let selected = c.eq_ignore_ascii_case(&v.color);
                        if selected {
                            ui.painter().circle_stroke(
                                center,
                                CHIP / 2.0 + 2.0,
                                Stroke::new(2.0, style::accent_bright()),
                            );
                        }
                        circle(ui, center, CHIP, fill);
                        let resp = ui
                            .interact(
                                Rect::from_center_size(center, Vec2::splat(CHIP)),
                                ui.scope_id().with(("chip", &v.id, i)),
                                egui::Sense::click(),
                            )
                            .on_hover_text(if c.is_empty() { "Theme colour" } else { c.as_str() });
                        if resp.clicked() {
                            color = Some((v.id.clone(), c.clone()));
                            this.palette_for = None;
                        }
                    }
                }
                y = r.bottom() + 6.0;
            }

            // Links
            y += 4.0;
            let head = caption(ui, "Links");
            ui.painter().galley(Pos2::new(x, y), head.clone(), style::text_3());
            y += head.size().y + 8.0;
            for l in &snap.links {
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, LINK_H));
                card::paint(ui.painter(), r, Look::Page);
                if l.id == snap.active_link {
                    ui.painter().rect_stroke(
                        r,
                        style::RADIUS,
                        Stroke::new(1.0, style::accent()),
                        egui::StrokeKind::Inside,
                    );
                }
                let level = link_level(&l.status);
                let dot = match level {
                    "healthy" => style::healthy(),
                    "warning" => style::warning(),
                    _ => style::text_3(),
                };
                kit::dot(
                    ui.painter(),
                    Pos2::new(r.left() + 12.0 + 3.5, r.center().y),
                    dot,
                    level != "off",
                );
                let label = style::galley(ui, &l.label, 12.0, Weight::W600, style::text_1(), 0.0);
                let lx = r.left() + 12.0 + 7.0 + 8.0;
                ui.painter().galley(
                    Pos2::new(lx, r.center().y - label.size().y / 2.0),
                    label.clone(),
                    style::text_1(),
                );
                let count = if l.vehicles > 0 { "1 vehicle" } else { "No vehicle yet" };
                let c = style::galley(ui, count, 11.0, Weight::W400, style::text_3(), 0.0);
                let right = r.right() - 12.0 - if l.primary { 0.0 } else { 20.0 + 8.0 };
                ui.painter().galley(
                    Pos2::new(right - c.size().x, r.center().y - c.size().y / 2.0),
                    c.clone(),
                    style::text_3(),
                );
                let conn_x = lx + label.size().x + 8.0;
                let conn = style::galley_mono(ui, &l.connection, 12.0, style::text_2());
                let clip = Rect::from_min_max(
                    Pos2::new(conn_x, r.top()),
                    Pos2::new(right - c.size().x - 8.0, r.bottom()),
                );
                ui.painter_at(clip).galley(
                    Pos2::new(conn_x, r.center().y - conn.size().y / 2.0),
                    conn,
                    style::text_2(),
                );
                if !l.primary
                    && kit::icon_btn(
                        ui,
                        Rect::from_center_size(Pos2::new(r.right() - 12.0 - 10.0, r.center().y), Vec2::splat(20.0)),
                        "x",
                        "Close this link",
                        false,
                        ("closelink", &l.id),
                    )
                {
                    close_link = Some(l.id.clone());
                }
                y = r.bottom() + 6.0;
            }

            if let Some((conn, text)) = offer(&snap) {
                if this.offered != conn {
                    this.offered = conn.clone();
                }
                let g = kit::wrapped(ui, &text, 12.0, style::text_1(), w - 24.0 - 150.0);
                let b = Button::new("Open as another vehicle", Variant::Primary).small();
                let (bw, bh) = (b.width(ui), b.height());
                let h = (g.size().y + 16.0).max(bh + 16.0);
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
                card::paint(ui.painter(), r, Look::Page);
                ui.painter().rect_stroke(
                    r,
                    style::RADIUS,
                    Stroke::new(1.0, style::accent()),
                    egui::StrokeKind::Inside,
                );
                ui.painter().galley(
                    Pos2::new(r.left() + 12.0, r.center().y - g.size().y / 2.0),
                    g,
                    style::text_1(),
                );
                if b.show(
                    ui,
                    Rect::from_min_size(
                        Pos2::new(r.right() - 12.0 - bw, r.center().y - bh / 2.0),
                        Vec2::new(bw, bh),
                    ),
                    "offer",
                ) {
                    open_conn = Some(conn);
                }
                y = r.bottom() + 8.0;
            }

            let free = free_ports(&snap, &this.ports);
            if !free.is_empty() {
                let g = style::galley(
                    ui,
                    "Serial ports no link uses",
                    11.0,
                    Weight::W400,
                    style::text_3(),
                    0.0,
                );
                ui.painter().galley(Pos2::new(x, y + 2.0), g, style::text_3());
                if kit::icon_btn(
                    ui,
                    Rect::from_center_size(Pos2::new(x + w - 11.0, y + 9.0), Vec2::splat(22.0)),
                    "refresh-cw",
                    "Look for ports again",
                    false,
                    "freshports",
                ) {
                    refresh = true;
                }
                y += 22.0;
                for p in free {
                    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 30.0));
                    ui.painter().add(egui::Shape::dashed_line(
                        &[
                            r.left_top(),
                            r.right_top(),
                            r.right_bottom(),
                            r.left_bottom(),
                            r.left_top(),
                        ],
                        Stroke::new(1.0, style::border_strong()),
                        4.0,
                        3.0,
                    ));
                    let dg = style::galley_mono(ui, &p.device, 12.0, style::text_2());
                    ui.painter().galley(
                        Pos2::new(r.left() + 10.0, r.center().y - dg.size().y / 2.0),
                        dg.clone(),
                        style::text_2(),
                    );
                    let desc = style::galley(ui, &p.description, 11.0, Weight::W400, style::text_3(), 0.0);
                    ui.painter_at(Rect::from_min_max(
                        Pos2::new(r.left() + 20.0 + dg.size().x, r.top()),
                        Pos2::new(r.right() - 70.0, r.bottom()),
                    ))
                    .galley(
                        Pos2::new(r.left() + 20.0 + dg.size().x, r.center().y - desc.size().y / 2.0),
                        desc,
                        style::text_3(),
                    );
                    let b = Button::new("Open", Variant::Secondary).small();
                    let (bw, bh) = (b.width(ui), b.height());
                    if b.show(
                        ui,
                        Rect::from_min_size(
                            Pos2::new(r.right() - 4.0 - bw, r.center().y - bh / 2.0),
                            Vec2::new(bw, bh),
                        ),
                        ("openport", &p.device),
                    ) {
                        open_conn = Some(format!("serial:{}:57600", p.device));
                    }
                    y = r.bottom() + 4.0;
                }
                y += 4.0;
            }

            let hint = kit::wrapped(
                ui,
                "One link is one vehicle. A second vehicle needs a radio of its own and a link of its own. Another autopilot heard on a link is ignored.",
                12.0,
                style::text_3(),
                w,
            );
            ui.painter().galley(Pos2::new(x, y), hint.clone(), style::text_3());
            y += hint.size().y + 8.0;

            let b = Button::new("Open link", Variant::Primary).icon("plus").small();
            let (bw, bh) = (b.width(ui), b.height());
            let fr = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w - bw - 6.0, FIELD_H));
            let resp = TextField::new(&mut this.conn)
                .hint("serial:/dev/ttyUSB1:57600 or udpin:0.0.0.0:14560")
                .mono(true)
                .pad(11.0, 11.0)
                .show(ui, fr, "fleetnew");
            let go = b.show(
                ui,
                Rect::from_min_size(Pos2::new(fr.right() + 6.0, y + (FIELD_H - bh) / 2.0), Vec2::new(bw, bh)),
                "openlink",
            );
            if (go || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                && !this.conn.trim().is_empty()
            {
                open_conn = Some(this.conn.trim().to_string());
                this.conn.clear();
            }
            y += FIELD_H;
            if let Some(e) = &this.error {
                y += 8.0;
                let g = kit::wrapped(ui, e, 12.0, style::critical(), w);
                ui.painter().galley(Pos2::new(x, y), g.clone(), style::critical());
                y += g.size().y;
            }
            y
        });
        if close {
            self.open = false;
            self.palette_for = None;
        }
        if refresh {
            self.refresh_ports(ctx, server);
        }
        for e in fleet.take_errors() {
            self.error = Some(e);
        }
        if let Some(id) = select {
            fleet.select(ctx, &id);
        }
        if let Some(p) = locate {
            let zoom = map.camera().map_or(16.0, |c| c.2);
            map.go_to(p[0], p[1], zoom);
        }
        if let Some((id, name)) = rename {
            fleet.rename(ctx, &id, &name);
        }
        if let Some((id, c)) = color {
            fleet.set_color(ctx, &id, &c);
        }
        if let Some(id) = ask_remove {
            self.remove = Some(id);
        }
        if let Some(id) = close_link {
            fleet.remove_link(ctx, &id);
        }
        if let Some(c) = open_conn {
            self.error = None;
            fleet.add_link(ctx, &c);
        }
    }

    /// The two questions: take control while armed, and remove.
    fn questions(&mut self, ctx: &egui::Context, fleet: &Fleet, snap: &Snapshot, _state: &Value) {
        if let Some(id) = self.take.clone() {
            let (Some(target), Some(current)) = (snap.vehicle(&id), snap.active_vehicle()) else {
                self.take = None;
                return;
            };
            let (title, paras) = switch_text(current, target);
            let mut hit = None;
            let close = modal::show(ctx, "fleet-take", Size::Sm, &title, |ui, x, y, w| {
                let mut y = y;
                for p in &paras {
                    let g = kit::wrapped(ui, p, 13.0, style::text_1(), w);
                    ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_1());
                    y += g.size().y + 8.0;
                }
                let (h, bottom) = modal::actions(
                    ui,
                    x,
                    y + 8.0,
                    w,
                    vec![
                        Button::new("Cancel", Variant::Ghost),
                        Button::new("Take control", Variant::Primary),
                    ],
                );
                hit = h;
                bottom
            });
            match hit {
                Some(1) => {
                    fleet.select(ctx, &id);
                    self.take = None;
                }
                Some(_) => self.take = None,
                None if close => self.take = None,
                None => {}
            }
        } else if let Some(id) = self.remove.clone() {
            let Some(v) = snap.vehicle(&id) else {
                self.remove = None;
                return;
            };
            let (title, body, warn) = remove_text(v, snap);
            let armed = warn.is_some();
            let mut hit = None;
            let close = modal::show(ctx, "fleet-remove", Size::Sm, &title, |ui, x, y, w| {
                let mut y = y;
                let g = kit::wrapped(ui, &body, 13.0, style::text_1(), w);
                ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_1());
                y += g.size().y + 8.0;
                if let Some(wtext) = &warn {
                    let g = kit::wrapped_weight(ui, wtext, 13.0, Weight::W600, style::warning(), w, 1.45);
                    ui.painter().galley(Pos2::new(x, y), g.clone(), style::warning());
                    y += g.size().y + 8.0;
                }
                let (h, bottom) = modal::actions(
                    ui,
                    x,
                    y + 8.0,
                    w,
                    vec![
                        Button::new("Cancel", Variant::Ghost),
                        Button::new("Remove", Variant::Danger).icon("trash-2"),
                    ],
                );
                hit = h;
                bottom
            });
            match hit {
                Some(1) => {
                    fleet.remove_vehicle(ctx, &id, armed);
                    self.removed.push(id.clone());
                    self.remove = None;
                }
                Some(_) => self.remove = None,
                None if close => self.remove = None,
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(links: Value, vehicles: Value, suggestion: Value) -> Snapshot {
        Snapshot::parse(
            &json!({"active": "main:1", "links": links, "vehicles": vehicles, "suggestion": suggestion}),
            &|_| false,
        )
    }

    #[test]
    fn the_device_of_a_serial_connection() {
        assert_eq!(serial_device("serial:/dev/ttyUSB1:57600"), "/dev/ttyUSB1");
        assert_eq!(serial_device("serial:COM3:115200"), "COM3");
        assert_eq!(serial_device("serial:/dev/ttyACM0"), "/dev/ttyACM0");
        assert_eq!(serial_device("udpin:0.0.0.0:14550"), "");
    }

    #[test]
    fn a_macos_port_is_one_port_under_both_names() {
        assert!(same_device("/dev/tty.usbserial-1", "/dev/cu.usbserial-1"));
        assert!(same_device("COM3", "com3"));
        assert!(!same_device("", ""));
        assert!(!same_device("/dev/ttyUSB0", "/dev/ttyUSB1"));
    }

    #[test]
    fn only_ports_no_link_holds_are_offered() {
        let s = snap(
            json!([{"id": "l", "connection": "serial:/dev/ttyUSB0:57600"}]),
            json!([]),
            Value::Null,
        );
        let ports = vec![
            Port {
                device: "/dev/ttyUSB0".into(),
                description: "SiK".into(),
            },
            Port {
                device: "/dev/ttyUSB1".into(),
                description: "".into(),
            },
        ];
        let free = free_ports(&s, &ports);
        assert_eq!(free.len(), 1);
        assert_eq!(free[0].device, "/dev/ttyUSB1");
    }

    #[test]
    fn the_plugged_in_radio_is_offered_unless_a_link_holds_it() {
        let sug = json!({"connection_string": "serial:/dev/ttyUSB2:57600", "kind": "sik", "device": "/dev/ttyUSB2"});
        let s = snap(json!([]), json!([]), sug.clone());
        assert_eq!(
            offer(&s),
            Some((
                "serial:/dev/ttyUSB2:57600".to_string(),
                "Telemetry radio on /dev/ttyUSB2 plugged in just now.".to_string()
            ))
        );
        let held = snap(
            json!([{"id": "l", "connection": "serial:/dev/ttyUSB2:57600"}]),
            json!([]),
            sug,
        );
        assert_eq!(offer(&held), None);
        let fc = snap(
            json!([]),
            json!([]),
            json!({"connection_string": "serial:/dev/ttyACM0:115200", "kind": "px4"}),
        );
        assert_eq!(
            offer(&fc).map(|o| o.1),
            Some("Flight controller on /dev/ttyACM0 plugged in just now.".to_string())
        );
    }

    #[test]
    fn taking_control_says_what_happens_to_the_armed_one() {
        let s = snap(
            json!([{"id": "main", "primary": true, "connection": "udpin:127.0.0.1:14550"}]),
            json!([
                {"id": "main:1", "name": "Scout", "armed": true, "mode_label": "MISSION", "link": "main"},
                {"id": "main:2", "name": "Rover", "link": "main"}
            ]),
            Value::Null,
        );
        let (title, paras) = switch_text(&s.vehicles[0], &s.vehicles[1]);
        assert_eq!(title, "Take control of Rover?");
        assert_eq!(
            paras[0],
            "Scout is armed. It keeps flying in MISSION, and the station stops sending it commands."
        );
        assert_eq!(
            paras[1],
            "From now on the flight bar, the joystick and every page control Rover."
        );
    }

    #[test]
    fn removing_names_the_link_and_warns_for_an_armed_vehicle() {
        let s = snap(
            json!([
                {"id": "main", "primary": true, "connection": "udpin:127.0.0.1:14550"},
                {"id": "link-2", "primary": false, "connection": "serial:/dev/ttyUSB1:57600"}
            ]),
            json!([
                {"id": "main:1", "name": "Scout", "link": "main"},
                {"id": "link-2:2", "name": "Rover", "link": "link-2", "armed": true}
            ]),
            Value::Null,
        );
        let (t, b, w) = remove_text(&s.vehicles[0], &s);
        assert_eq!(t, "Remove Scout?");
        assert_eq!(
            b,
            "The main link is disconnected and stays closed. It leaves the list and the map. Connect it again on the LINK tab."
        );
        assert!(w.is_none());
        let (_, b, w) = remove_text(&s.vehicles[1], &s);
        assert_eq!(
            b,
            "Its link (serial:/dev/ttyUSB1:57600) is closed. It leaves the list and the map. To bring it back, open the link again."
        );
        assert_eq!(
            w.as_deref(),
            Some("Rover is armed. It keeps flying in its current mode, and the station no longer sees it or sends it anything.")
        );
    }
}
