//! The Betaflight page of Setup (`setup-betaflight.js`): read a flight
//! controller over MSP and show what it says, firmware, board, sensors,
//! battery, PIDs and rates.
//!
//! Read only, like the route behind it (`peripherals/betaflight.rs`): nothing
//! on this page changes the controller. A read is one short session on a
//! serial port the MAVLink link does not hold, so the page lists the ports,
//! flags the link's own and lets the operator pick the baud rate.

use std::time::Duration;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::card;
use crate::ui::formfield as ff;
use crate::ui::kit::{Button, Choice, Variant};
use crate::ui::message::{self, Tone as Msg};
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

type Answer = Pending<Result<Value, String>>;

const INTRO: &str = "Reads a Betaflight flight controller over MSP: firmware, board, sensors, battery, PIDs and rates. Nothing here changes the controller. Use the controller's own USB port, not the one the live link runs on.";

/// A short session: a silent port costs a second, a slow board a few.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// One serial port of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub device: String,
    pub label: String,
    pub link: bool,
}

/// The ports the status lists, the live link's own marked.
pub fn ports(status: &Value) -> Vec<Port> {
    status
        .get("ports")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|p| {
                    let device = p.get("device").and_then(Value::as_str)?.to_string();
                    let description = p.get("description").and_then(Value::as_str).unwrap_or("");
                    let link = p.get("link").and_then(Value::as_bool).unwrap_or(false);
                    let mut label = device.clone();
                    if !description.is_empty() && description != "n/a" {
                        label = format!("{device} ({description})");
                    }
                    if link {
                        label.push_str(" [live link]");
                    }
                    Some(Port { device, label, link })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The port to preselect: the one already chosen, else the first that is not
/// the live link.
pub fn preferred_port(ports: &[Port], previous: &str) -> String {
    if !previous.is_empty() && ports.iter().any(|p| p.device == previous) {
        return previous.to_string();
    }
    ports
        .iter()
        .find(|p| !p.link)
        .or_else(|| ports.first())
        .map(|p| p.device.clone())
        .unwrap_or_default()
}

/// The baud rates on offer.
pub fn bauds(status: &Value) -> Vec<i64> {
    let list: Vec<i64> = status
        .get("bauds")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if list.is_empty() {
        vec![115_200]
    } else {
        list
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn plain(n: f64) -> String {
    if (n - n.round()).abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        format!("{n:.2}")
    }
}

const DASH: &str = "\u{2014}";

/// The controller card: what it is and how it runs.
pub fn controller_rows(r: &Value) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    let variant = {
        let name = text(r, "variant_name");
        if name.is_empty() {
            text(r, "variant")
        } else {
            name
        }
    };
    let version = text(r, "version");
    rows.push((
        "Firmware",
        match (variant.is_empty(), version.is_empty()) {
            (false, false) => format!("{variant} {version}"),
            (false, true) => variant,
            (true, false) => version,
            (true, true) => DASH.to_string(),
        },
    ));
    let board = text(r, "board");
    let target = text(r, "target");
    rows.push((
        "Board",
        match (board.is_empty(), target.is_empty()) {
            (false, false) if board != target => format!("{board} ({target})"),
            (false, _) => board,
            (true, false) => target,
            (true, true) => DASH.to_string(),
        },
    ));
    if let Some(b) = r.get("build").filter(|b| b.is_object()) {
        let parts: Vec<String> = ["date", "time", "revision"]
            .iter()
            .map(|k| text(b, k))
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            rows.push(("Build", parts.join(" ")));
        }
    }
    let name = text(r, "name");
    if !name.is_empty() {
        rows.push(("Craft name", name));
    }
    let api = text(r, "api");
    if !api.is_empty() {
        rows.push(("MSP API", api));
    }
    if let Some(us) = number(r, "cycle_time_us") {
        rows.push(("Cycle time", format!("{} \u{00B5}s", plain(us))));
    }
    if let Some(load) = number(r, "cpu_load") {
        rows.push(("CPU load", format!("{} %", plain(load))));
    }
    if let Some(e) = number(r, "i2c_errors") {
        rows.push(("I2C errors", plain(e)));
    }
    if let Some(p) = number(r, "pid_profile") {
        let of = number(r, "pid_profile_count")
            .map(|n| format!(" of {}", plain(n)))
            .unwrap_or_default();
        rows.push(("PID profile", format!("{}{of}", plain(p))));
    }
    if let Some(p) = number(r, "rate_profile") {
        rows.push(("Rate profile", plain(p)));
    }
    let sensors: Vec<&str> = r
        .get("sensors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    rows.push((
        "Sensors",
        if sensors.is_empty() {
            "none".to_string()
        } else {
            sensors.join(", ")
        },
    ));
    rows
}

/// The battery card, empty when the controller did not answer for it.
pub fn battery_rows(r: &Value) -> Vec<(&'static str, String)> {
    let Some(b) = r.get("battery").filter(|b| b.is_object()) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if let Some(c) = number(b, "cells") {
        rows.push(("Cells", plain(c)));
    }
    if let Some(v) = number(b, "voltage") {
        rows.push(("Voltage", format!("{v:.2} V")));
    }
    if let Some(a) = number(b, "current") {
        rows.push(("Current", format!("{a:.2} A")));
    }
    if let Some(m) = number(b, "capacity_mah") {
        rows.push(("Capacity", format!("{} mAh", plain(m))));
    }
    if let Some(m) = number(b, "used_mah") {
        rows.push(("Used", format!("{} mAh", plain(m))));
    }
    rows
}

/// The PID table: axis, P, I and D, in the order the controller lists them.
pub fn pid_table(r: &Value) -> Vec<[String; 4]> {
    let Some(p) = r.get("pids").and_then(Value::as_object) else {
        return Vec::new();
    };
    p.iter()
        .map(|(axis, v)| {
            let cell = |k: &str| number(v, k).map_or_else(|| DASH.to_string(), plain);
            [title_case(axis), cell("p"), cell("i"), cell("d")]
        })
        .collect()
}

/// The rates card.
pub fn rate_rows(r: &Value) -> Vec<(String, String)> {
    let Some(rates) = r.get("rates").and_then(Value::as_object) else {
        return Vec::new();
    };
    rates
        .iter()
        .filter_map(|(k, v)| v.as_f64().map(|n| (title_case(&k.replace('_', " ")), plain(n))))
        .collect()
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// What the page says under the Read button for an answer.
pub fn summary(r: &Value) -> (Msg, String) {
    let ok = r.get("is_betaflight") == Some(&Value::Bool(true));
    let unanswered = r.get("unanswered").and_then(Value::as_array).map_or(0, Vec::len);
    if !ok {
        let name = text(r, "variant_name");
        let what = if name.is_empty() { text(r, "variant") } else { name };
        return (
            Msg::Warn,
            format!(
                "The controller answers, but it is not Betaflight{}. What it reported is below.",
                if what.is_empty() {
                    String::new()
                } else {
                    format!(" ({what})")
                }
            ),
        );
    }
    if unanswered > 0 {
        return (
            Msg::Warn,
            "Read the controller. Some parts were not answered by this firmware and are left out.".into(),
        );
    }
    (Msg::Ok, "Read the controller.".into())
}

/// The page's state.
pub struct BetaflightPage {
    server: Server,
    status: Value,
    status_poll: Option<Answer>,
    status_asked: bool,
    port: String,
    baud: i64,
    result: Option<Value>,
    busy: Option<Answer>,
    message: Option<(Msg, String)>,
    errors: Vec<String>,
}

impl BetaflightPage {
    pub fn new(server: Server, _ctx: &egui::Context) -> BetaflightPage {
        BetaflightPage {
            server,
            status: Value::Null,
            status_poll: None,
            status_asked: false,
            port: String::new(),
            baud: 115_200,
            result: None,
            busy: None,
            message: None,
            errors: Vec::new(),
        }
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn take_answers(&mut self) {
        if let Some(r) = self.status_poll.as_ref().and_then(Pending::take) {
            self.status_poll = None;
            match r {
                Ok(v) => {
                    self.status = v;
                    self.port = preferred_port(&ports(&self.status), &self.port);
                    if let Some(b) = self.status.get("default_baud").and_then(Value::as_i64) {
                        if self.result.is_none() {
                            self.baud = b;
                        }
                    }
                }
                Err(_) => self.message = Some((Msg::Err, "Could not read the serial port list.".into())),
            }
        }
        let Some(r) = self.busy.as_ref().and_then(Pending::take) else {
            return;
        };
        self.busy = None;
        match r {
            Ok(v) => {
                self.message = Some(summary(&v));
                self.result = Some(v);
            }
            Err(e) => {
                let e = if e.is_empty() {
                    "Could not read the controller.".to_string()
                } else {
                    e
                };
                self.errors.push(e.clone());
                self.message = Some((Msg::Err, e));
            }
        }
    }

    fn start_read(&mut self, ctx: &egui::Context) {
        if self.busy.is_some() || self.port.is_empty() {
            return;
        }
        self.message = Some((Msg::Plain, "Reading the controller\u{2026}".into()));
        let s = self.server.clone();
        let body = serde_json::json!({"device": self.port, "baud": self.baud});
        self.busy = Some(Pending::spawn(ctx, move || post(&s, "/api/betaflight/read", &body)));
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32) -> f32 {
        let ctx = ui.ctx().clone();
        if !self.status_asked {
            self.status_asked = true;
            let s = self.server.clone();
            self.status_poll = Some(Pending::spawn(&ctx, move || {
                http::get_json(&s, "/api/betaflight/status")
            }));
        }
        self.take_answers();
        let y = pagekit::header(
            ui,
            x,
            y,
            "Betaflight",
            "Read a Betaflight flight controller over MSP: firmware, board, sensors, PIDs and rates",
        ) + 32.0;
        let mut y = self.port_card(ui, &ctx, x, y, w) + 28.0;
        if let Some(r) = self.result.clone() {
            y = rows_card(ui, x, y, w, "Controller", &controller_rows(&r)) + 26.0;
            let battery = battery_rows(&r);
            if !battery.is_empty() {
                y = rows_card(ui, x, y, w, "Battery", &battery) + 26.0;
            }
            let pids = pid_table(&r);
            if !pids.is_empty() {
                y = pid_card(ui, x, y, w, &pids) + 26.0;
            }
            let rates = rate_rows(&r);
            if !rates.is_empty() {
                y = owned_rows_card(ui, x, y, w, "Rates", &rates);
            }
        }
        y
    }

    fn port_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Controller port") + 2.0;
        cy = message::boxed(ui, ix, cy, iw, INTRO, Msg::Plain) + 14.0;
        let list = ports(&self.status);
        let read = !self.status.is_null();
        let on_link = list.iter().any(|p| p.device == self.port && p.link);
        if on_link {
            cy = pagekit::warning_banner(
                ui,
                ix,
                cy,
                iw,
                "This port carries the live link. Pick the controller's own USB port.",
            ) + 14.0;
        }
        let narrow = crate::app::responsive::Vw::of(ctx).max900;
        let (pw, bw) = if narrow {
            (iw, iw)
        } else {
            (iw * 0.62, iw * 0.38 - 12.0)
        };
        let (bx, by) = if narrow {
            (ix, cy + ff::control_h() + 24.0)
        } else {
            (ix + pw + 12.0, cy)
        };
        let choices: Vec<Choice> = if list.is_empty() {
            vec![Choice::new(
                "",
                if read {
                    "No serial ports found"
                } else {
                    "Looking for serial ports\u{2026}"
                },
            )]
        } else {
            list.iter()
                .map(|p| Choice::new(p.device.clone(), p.label.clone()))
                .collect()
        };
        let mut picked_port = None;
        let port = self.port.clone();
        ff::field_info(
            ui,
            ix,
            cy,
            "Serial port",
            "The flight controller on its USB cable.",
            "bfport-info",
            |ui, top| {
                ui.add_enabled_ui(!list.is_empty(), |ui| {
                    picked_port = ff::select(ui, ix, top, pw, &choices, &port, "bfport");
                });
                ff::control_h()
            },
        );
        let baud_choices: Vec<Choice> = bauds(&self.status)
            .iter()
            .map(|b| Choice::new(b.to_string(), format!("{b} baud")))
            .collect();
        let mut picked_baud = None;
        let baud = self.baud.to_string();
        ff::field_info(
            ui,
            bx,
            by,
            "Baud rate",
            "The speed of this cable. Betaflight on USB ignores it; a UART port needs the rate set in the controller.",
            "bfbaud-info",
            |ui, top| {
                ui.add_enabled_ui(read, |ui| {
                    picked_baud = ff::select(ui, bx, top, bw, &baud_choices, &baud, "bfbaud");
                });
                ff::control_h()
            },
        );
        if let Some(p) = picked_port {
            self.port = p;
        }
        if let Some(b) = picked_baud.and_then(|b| b.parse().ok()) {
            self.baud = b;
        }
        cy = if narrow { by } else { cy } + 53.5 + 14.0;
        let button = Button::new("Read controller", Variant::Primary)
            .icon("download")
            .enabled(self.busy.is_none() && !self.port.is_empty() && !on_link);
        let (lw, lh) = (button.width(ui), button.height());
        if button.show(ui, Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(lw, lh)), "bfread") {
            self.start_read(ctx);
        }
        cy += lh;
        if let Some((kind, text)) = &self.message {
            cy = message::show(ui, ix, cy + 10.0, iw, text, *kind);
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 31.0)),
            card::Look::Page,
        );
        cy + 31.0
    }
}

fn rows_card(ui: &egui::Ui, x: f32, y: f32, w: f32, title: &str, rows: &[(&str, String)]) -> f32 {
    let painter = ui.painter().clone();
    let bg = card::reserve(&painter);
    let (ix, iw) = (x + 19.0, w - 38.0);
    let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, title) + 6.0;
    for (i, (label, value)) in rows.iter().enumerate() {
        cy = info_row(ui, ix, iw, cy, label, value, i + 1 == rows.len());
    }
    card::fill(
        &painter,
        bg,
        Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
        card::Look::Page,
    );
    cy + 19.0
}

fn owned_rows_card(ui: &egui::Ui, x: f32, y: f32, w: f32, title: &str, rows: &[(String, String)]) -> f32 {
    let borrowed: Vec<(&str, String)> = rows.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    rows_card(ui, x, y, w, title, &borrowed)
}

fn pid_card(ui: &egui::Ui, x: f32, y: f32, w: f32, rows: &[[String; 4]]) -> f32 {
    let painter = ui.painter().clone();
    let bg = card::reserve(&painter);
    let (ix, iw) = (x + 19.0, w - 38.0);
    let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "PIDs") + 6.0;
    let first = iw * 0.4;
    let col = (iw - first) / 3.0;
    let head = ["Axis", "P", "I", "D"];
    let cell = |ui: &egui::Ui, cx: f32, cy: f32, t: &str, head: bool, right: bool| {
        let g = if head {
            style::galley(ui, &t.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7)
        } else {
            style::galley_mono(ui, t, 13.0, style::text_1())
        };
        let px = if right { cx - g.size().x } else { cx };
        ui.painter().galley(Pos2::new(px, cy), g, style::text_1());
    };
    for (i, h) in head.iter().enumerate() {
        let cx = if i == 0 { ix } else { ix + first + col * i as f32 };
        cell(ui, cx, cy, h, true, i != 0);
    }
    cy += 22.0;
    painter.hline(
        egui::Rangef::new(ix, ix + iw),
        cy - 4.5,
        Stroke::new(1.0, style::border_soft()),
    );
    for (n, r) in rows.iter().enumerate() {
        for (i, t) in r.iter().enumerate() {
            let cx = if i == 0 { ix } else { ix + first + col * i as f32 };
            if i == 0 {
                let g = style::galley(ui, t, 13.0, Weight::W400, style::text_1(), 0.0);
                ui.painter().galley(Pos2::new(ix, cy), g, style::text_1());
            } else {
                cell(ui, cx, cy, t, false, true);
            }
        }
        cy += 28.0;
        if n + 1 < rows.len() {
            painter.hline(
                egui::Rangef::new(ix, ix + iw),
                cy - 5.5,
                Stroke::new(1.0, style::border_soft()),
            );
        }
    }
    card::fill(
        &painter,
        bg,
        Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 14.0)),
        card::Look::Page,
    );
    cy + 14.0
}

/// `.page-row`: a label at the left and a mono value at the right.
fn info_row(ui: &egui::Ui, x: f32, w: f32, y: f32, label: &str, value: &str, last: bool) -> f32 {
    let painter = ui.painter();
    let lg = style::galley(ui, label, 13.0, Weight::W400, style::text_1(), 0.0);
    let vg = style::galley_mono(ui, value, 13.0, style::text_2());
    let h = if last { 37.5 } else { 38.5 };
    let mid = y + 10.0 + 8.75;
    painter.galley(Pos2::new(x, mid - lg.size().y / 2.0), lg, style::text_1());
    painter.galley(
        Pos2::new(x + w - vg.size().x, mid - vg.size().y / 2.0),
        vg,
        style::text_2(),
    );
    if !last {
        painter.hline(
            egui::Rangef::new(x, x + w),
            y + h - 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
    }
    y + h
}

fn post(s: &Server, path: &str, body: &Value) -> Result<Value, String> {
    let text = body.to_string();
    let r = http::request(s, "POST", path, Some(&text), "application/json", READ_TIMEOUT)
        .map_err(|e| format!("{}{path}: {e}", s.url()))?;
    let status = r.status;
    let raw = r.text().map_err(|e| format!("{}{path}: {e}", s.url()))?;
    let v: Value = serde_json::from_str(&raw).map_err(|_| format!("Invalid response from {path}"))?;
    if !(200..300).contains(&status) || v.get("ok") != Some(&Value::Bool(true)) {
        return Err(v.get("error").and_then(Value::as_str).unwrap_or("").to_string());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read() -> Value {
        json!({
            "ok": true, "is_betaflight": true, "unanswered": [],
            "variant": "BTFL", "variant_name": "Betaflight", "version": "4.5.1",
            "api": "1.46", "board": "S405", "target": "STM32F405", "name": "quad",
            "build": {"date": "Jan 10 2025", "time": "12:00:00", "revision": "abc1234"},
            "cycle_time_us": 125, "cpu_load": 7, "i2c_errors": 0,
            "sensors": ["gyro", "accelerometer"], "pid_profile": 2, "pid_profile_count": 4, "rate_profile": 1,
            "pids": {"roll": {"p": 45, "i": 80, "d": 30}, "pitch": {"p": 47, "i": 84, "d": 34}},
            "rates": {"rc_rate": 120, "tpa_breakpoint": 1350},
            "battery": {"cells": 4, "capacity_mah": 1500, "voltage": 16.4, "used_mah": 120, "current": 1.25}
        })
    }

    #[test]
    fn the_ports_mark_the_live_link_and_prefer_another() {
        let status = json!({"ports": [
            {"device": "/dev/ttyACM0", "description": "Pixhawk", "link": true},
            {"device": "/dev/ttyUSB0", "description": "n/a", "link": false}
        ]});
        let list = ports(&status);
        assert_eq!(list[0].label, "/dev/ttyACM0 (Pixhawk) [live link]");
        assert_eq!(list[1].label, "/dev/ttyUSB0");
        assert_eq!(preferred_port(&list, ""), "/dev/ttyUSB0");
        assert_eq!(preferred_port(&list, "/dev/ttyACM0"), "/dev/ttyACM0", "a choice stays");
        assert_eq!(
            preferred_port(&list, "/dev/gone"),
            "/dev/ttyUSB0",
            "a vanished port is dropped"
        );
        assert_eq!(preferred_port(&[], ""), "");
        assert_eq!(bauds(&Value::Null), [115_200]);
    }

    #[test]
    fn a_read_becomes_labelled_rows() {
        let rows = controller_rows(&read());
        let get = |k: &str| rows.iter().find(|r| r.0 == k).map(|r| r.1.clone());
        assert_eq!(get("Firmware").as_deref(), Some("Betaflight 4.5.1"));
        assert_eq!(get("Board").as_deref(), Some("S405 (STM32F405)"));
        assert_eq!(get("Cycle time").as_deref(), Some("125 \u{00B5}s"));
        assert_eq!(get("PID profile").as_deref(), Some("2 of 4"));
        assert_eq!(get("Sensors").as_deref(), Some("gyro, accelerometer"));
        let battery = battery_rows(&read());
        assert_eq!(battery[1], ("Voltage", "16.40 V".to_string()));
        assert!(battery_rows(&json!({})).is_empty());
    }

    #[test]
    fn the_pid_and_rate_tables_follow_the_controller() {
        let t = pid_table(&read());
        assert_eq!(t[0], ["Roll", "45", "80", "30"]);
        assert_eq!(t.len(), 2);
        assert!(pid_table(&json!({})).is_empty());
        let r = rate_rows(&read());
        assert!(r.contains(&("Rc rate".to_string(), "120".to_string())));
        assert!(r.contains(&("Tpa breakpoint".to_string(), "1350".to_string())));
    }

    #[test]
    fn the_answer_says_whether_it_was_betaflight_and_whole() {
        assert_eq!(summary(&read()).0, Msg::Ok);
        let mut partial = read();
        partial["unanswered"] = json!([1]);
        assert_eq!(summary(&partial).0, Msg::Warn);
        let other = json!({"is_betaflight": false, "variant": "INAV", "unanswered": []});
        let (kind, said) = summary(&other);
        assert_eq!(kind, Msg::Warn);
        assert!(said.contains("not Betaflight (INAV)"));
    }

    #[test]
    fn an_empty_answer_still_has_a_dash_for_the_firmware() {
        let rows = controller_rows(&json!({}));
        assert_eq!(rows[0], ("Firmware", DASH.to_string()));
    }
}
