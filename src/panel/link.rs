//! The LINK tab (index.html `[data-panel="link"]`, python/src/js/link.js,
//! panel.css `.link-*`): the status card, the one New connection card with
//! its serial and UDP/TCP branches, and Share with another station.
//!
//! The words and rules are link.js's: statusInfo, qualityInfo and autoInfo
//! name the state, the Connect button reads Connecting… and Connected, a
//! port that went missing is offered as "(not connected)" and refused with
//! the same sentence, and the forwarding hint is forwardingHint word for
//! word. Recent links and the plug-in suggestion row come later.

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::card;
use crate::ui::icons;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::segmented::{self, Segment};
use crate::ui::style::{self, Weight};

/// The common endpoints offered under the connection string.
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("SITL / GCS UDP", "udp:0.0.0.0:14550", "Waits on the usual ground-station port. Start here. SITL and most telemetry radios send to it by default."),
    ("PX4 onboard UDP", "udp:0.0.0.0:14540", "PX4's companion-computer port. MAVROS and MAVSDK bind it too, and taking it can leave them without telemetry."),
    ("mavlink-router", "udpout:127.0.0.1:14550", "Dials out instead of waiting, because a router in server mode holds the port and expects the station to speak first."),
    ("ArduPilot SITL", "tcp:127.0.0.1:5760", "ArduPilot's simulator on this machine. PX4 SITL serves no TCP. Use the UDP rows above for it."),
];

const BAUDS: &[(&str, &str)] = &[
    ("9600", "9600"),
    ("19200", "19200"),
    ("38400", "38400"),
    ("57600", "57600 (SiK Radio v3)"),
    ("115200", "115200"),
    ("230400", "230400"),
];

/// `serial:<device>:<baud>`.
pub fn build_connection_string(device: &str, baud: &str) -> String {
    if device.is_empty() {
        String::new()
    } else {
        format!("serial:{device}:{baud}")
    }
}

/// The status word and its dot.
pub fn status_info(link_status: &str) -> (&'static str, &'static str) {
    match link_status.to_lowercase().as_str() {
        "connected" => ("CONNECTED", "healthy"),
        "connecting" => ("CONNECTING", "warning"),
        "reconnecting" => ("RECONNECTING", "warning"),
        _ => ("DISCONNECTED", "off"),
    }
}

/// The quality pill: its word and level, or an empty word for none.
pub fn quality_info(q: &str) -> (&'static str, &'static str) {
    match q.to_lowercase().as_str() {
        "good" => ("GOOD", "healthy"),
        "fair" => ("FAIR", "warning"),
        "poor" => ("POOR", "critical"),
        "lost" => ("LOST", "critical"),
        _ => ("", "off"),
    }
}

/// Which rule picked this link, or that one is still being looked for.
pub fn auto_info(state: &Value, link_status: &str) -> Option<(String, &'static str)> {
    let auto = state.get("link_auto")?;
    if auto.get("enabled") != Some(&Value::Bool(true))
        || auto.get("manual_override").is_some_and(|v| v.as_bool() == Some(true))
    {
        return None;
    }
    if link_status.to_lowercase() != "connected" {
        return Some((
            "AUTO \u{00b7} SCANNING".into(),
            "Looking for a flight controller on USB, then a telemetry radio, then the ground-station UDP port.",
        ));
    }
    let (short, title) = match auto.get("reason").and_then(Value::as_str).unwrap_or("") {
        "usb-direct" => ("USB", "Connected automatically to the flight controller on USB."),
        "sik" => ("RADIO", "Connected automatically to the SiK telemetry radio."),
        "udp-fallback" => (
            "UDP",
            "No flight controller or radio found. Listening on the ground-station UDP port, which is where a simulator publishes.",
        ),
        "configured" => ("SAVED", "No flight controller or radio found. Using the connection saved in the config file."),
        "cli" => ("ARGUMENT", "Using the connection given on the command line."),
        "default-invalid-fallback" => ("DEFAULT", "The configured connection could not be used. Started on the built-in default instead."),
        _ => return None,
    };
    Some((format!("AUTO \u{00b7} {short}"), title))
}

/// The sentence under the forwarding switch.
pub fn forwarding_hint(s: &Value) -> String {
    let get = |k: &str| s.get(k);
    let host = get("host")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .unwrap_or("127.0.0.1");
    let port = get("port").and_then(Value::as_u64).filter(|p| *p > 0).unwrap_or(14550);
    let where_ = format!("{host}:{port}");
    if get("running") != Some(&Value::Bool(true)) {
        return format!("Mirrors this link to {where_}, the UDP link QGroundControl opens on its own.");
    }
    if let Some(blocked) = get("loop_blocked").filter(|v| truthy(v)) {
        return format!(
            "Not mirroring to {}: that is where Corvus receives the vehicle. Choose another port, for example 14560.",
            plain(blocked)
        );
    }
    if get("sysid_conflict").is_some_and(truthy) {
        return "The other station is using Corvus' MAVLink system ID (254). Give it its own. In QGroundControl: Application Settings → MAVLink → Ground Station system ID.".into();
    }
    let refused = get("commands_refused").and_then(Value::as_u64).unwrap_or(0);
    if refused > 0 {
        let who = get("commands_refused_from")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .map(plain);
        let from = who.map(|w| format!(" from {w}")).unwrap_or_default();
        return format!(
            "{refused} command(s) refused{from}. That address is not in the endpoint list, so it receives telemetry but cannot command. Add it to forwarding.endpoints to let it."
        );
    }
    let peers = get("peers").and_then(Value::as_array).map_or(0, Vec::len);
    let back = match get("listen_port").filter(|v| truthy(v)) {
        Some(lp) => format!(
            " Corvus answers on {}:{}.",
            get("listen_host")
                .and_then(Value::as_str)
                .filter(|h| !h.is_empty())
                .unwrap_or("127.0.0.1"),
            plain(lp)
        ),
        None => String::new(),
    };
    if peers == 0 {
        return format!("Mirroring to {where_}. Nothing has answered yet.{back}");
    }
    format!("{peers} station(s) connected; mirroring to {where_}.{back}")
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn plain(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

fn missing_port_hint(device: &str) -> String {
    format!("{device} is not connected. Plug it in, then press refresh.")
}

/// The port dropdown's rows: the placeholder, the ports, and a remembered
/// device that is not enumerated now, marked so.
pub fn port_choices(ports: &Value, current: &str) -> (Vec<Choice>, bool) {
    let mut out = vec![Choice::new("", "Select serial port\u{2026}")];
    let list = ports
        .get("ports")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for p in &list {
        let device = p.get("device").and_then(Value::as_str).unwrap_or("");
        let desc = p.get("description").and_then(Value::as_str).unwrap_or("");
        let text = if desc.is_empty() {
            device.to_string()
        } else {
            format!("{device} ({desc})")
        };
        out.push(Choice::new(device, text));
    }
    let missing = !current.is_empty()
        && !list
            .iter()
            .any(|p| p.get("device").and_then(Value::as_str) == Some(current));
    if missing {
        out.push(Choice::new(current, format!("{current} (not connected)")));
    }
    (out, missing)
}

/// The tab's state.
pub struct LinkTab {
    server: Server,
    net: bool,
    ports: Vec<Choice>,
    port: String,
    port_missing: bool,
    baud: String,
    custom: String,
    new_open: bool,
    share_open: bool,
    error: String,
    connecting: Option<Pending<Result<(), String>>>,
    listing: Option<Pending<Result<Value, String>>>,
    forwarding: Value,
    fwd_port: String,
    fwd_pending: Option<Pending<Result<Value, String>>>,
    fwd_loading: Option<Pending<Result<Value, String>>>,
    last_sent: String,
}

impl LinkTab {
    pub fn new(server: Server, ctx: &egui::Context) -> LinkTab {
        let mut t = LinkTab {
            server,
            net: false,
            ports: vec![Choice::new("", "Select serial port\u{2026}")],
            port: String::new(),
            port_missing: false,
            baud: "57600".into(),
            custom: String::new(),
            new_open: true,
            share_open: false,
            error: String::new(),
            connecting: None,
            listing: None,
            forwarding: Value::Null,
            fwd_port: "14550".into(),
            fwd_pending: None,
            fwd_loading: None,
            last_sent: String::new(),
        };
        t.refresh_ports(ctx);
        t.load_forwarding(ctx);
        t
    }

    fn refresh_ports(&mut self, ctx: &egui::Context) {
        let server = self.server.clone();
        self.listing = Some(Pending::spawn(ctx, move || {
            http::get_json(&server, "/api/mavlink/serial-ports")
        }));
    }

    fn load_forwarding(&mut self, ctx: &egui::Context) {
        let server = self.server.clone();
        self.fwd_loading = Some(Pending::spawn(ctx, move || http::get_json(&server, "/api/forwarding")));
    }

    fn post(&mut self, ctx: &egui::Context, path: &'static str, body: Value) {
        let server = self.server.clone();
        self.connecting = Some(Pending::spawn(ctx, move || {
            let data = http::post_json(&server, path, &body)?;
            if data.get("ok") == Some(&Value::Bool(true)) {
                Ok(())
            } else {
                Err(data
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Command rejected by vehicle")
                    .to_string())
            }
        }));
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let conn = if self.net {
            let c = self.custom.trim().to_string();
            if c.is_empty() {
                self.error = "Enter a connection string (e.g. udp:0.0.0.0:14540).".into();
                return;
            }
            c
        } else {
            if self.port.is_empty() {
                self.error = "Select a serial port first.".into();
                return;
            }
            if self.port_missing {
                self.error = missing_port_hint(&self.port);
                return;
            }
            build_connection_string(&self.port, &self.baud)
        };
        self.error.clear();
        self.last_sent = conn.clone();
        self.post(ctx, "/api/mavlink/connect", json!({ "connection": conn }));
    }

    fn take_answers(&mut self) {
        if let Some(r) = self.connecting.as_ref().and_then(Pending::take) {
            self.connecting = None;
            match r {
                Ok(()) => self.error.clear(),
                Err(e) => self.error = if e.is_empty() { "Connection failed".into() } else { e },
            }
        }
        if let Some(r) = self.listing.as_ref().and_then(Pending::take) {
            self.listing = None;
            match r {
                Ok(data) => {
                    let (choices, missing) = port_choices(&data, &self.port);
                    let only = data
                        .get("ports")
                        .and_then(Value::as_array)
                        .filter(|a| a.len() == 1)
                        .and_then(|a| a[0].get("device"))
                        .and_then(Value::as_str);
                    if self.port.is_empty() {
                        if let Some(d) = only {
                            self.port = d.to_string();
                        }
                    }
                    self.ports = choices;
                    self.port_missing = missing;
                    self.error = data.get("error").and_then(Value::as_str).unwrap_or("").to_string();
                }
                Err(e) => {
                    self.ports = vec![Choice::new("", "Select serial port\u{2026}")];
                    self.port_missing = false;
                    self.error = if e.is_empty() {
                        "Could not list serial ports".into()
                    } else {
                        e
                    };
                }
            }
        }
        for slot in [&mut self.fwd_loading, &mut self.fwd_pending] {
            if let Some(r) = slot.as_ref().and_then(Pending::take) {
                *slot = None;
                match r {
                    Ok(v) => {
                        let status = v.get("status").cloned().unwrap_or(v);
                        if let Some(p) = status.get("port").and_then(Value::as_u64) {
                            self.fwd_port = p.to_string();
                        }
                        self.forwarding = status;
                    }
                    Err(e) => {
                        if let Value::Object(m) = &mut self.forwarding {
                            m.insert("error".into(), Value::String(e));
                        }
                    }
                }
            }
        }
    }

    fn save_forwarding(&mut self, ctx: &egui::Context, enabled: bool, allow: bool) {
        let mut payload = json!({ "enabled": enabled, "allow_commands": allow });
        if let Ok(p) = self.fwd_port.trim().parse::<u32>() {
            if p > 0 && p < 65536 {
                payload["port"] = json!(p);
            }
        }
        let server = self.server.clone();
        self.fwd_pending = Some(Pending::spawn(ctx, move || {
            let data = http::post_json(&server, "/api/forwarding", &payload)?;
            let _ = data.get("ok");
            http::get_json(&server, "/api/forwarding")
        }));
    }

    /// Draws the tab into `rect`.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, state: &Value) {
        let ctx = ui.ctx().clone();
        self.take_answers();
        let header = section_header(ui, rect, "Connection", 0.0);
        let content = Rect::from_min_max(Pos2::new(rect.left(), header.bottom()), rect.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content));
        egui::ScrollArea::vertical()
            .id_salt("linkscroll")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let top = ui.max_rect().top();
                let left = content.left() + 12.0;
                let w = content.width() - 24.0;
                let mut y = top + 12.0;
                // the empty list of recent links is a flex item too: one more gap
                y = self.status_card(ui, &ctx, left, y, w, state) + 24.0;
                y = self.new_card(ui, &ctx, left, y, w, state) + 12.0;
                y = self.share_card(ui, &ctx, left, y, w) + 12.0;
                ui.allocate_space(Vec2::new(w, y - top));
            });
    }

    fn status_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, state: &Value) -> f32 {
        let status = state
            .get("link_status")
            .and_then(Value::as_str)
            .unwrap_or("disconnected")
            .to_lowercase();
        let (label, dot) = status_info(&status);
        let in_flight = self.connecting.is_some();
        let conn = state
            .get("link_connection")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let error = match state
            .get("link_error")
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty())
        {
            Some(e) => e.to_string(),
            None => self.error.clone(),
        };
        let auto = auto_info(state, &status);
        let row_h = 26.0;
        let why_h = if conn.is_empty() && auto.is_none() { 0.0 } else { 17.0 };
        let err_h = if error.is_empty() { 0.0 } else { 8.0 + 1.0 + 15.4 };
        let h = 12.0 * 2.0
            + row_h
            + if why_h > 0.0 { 8.0 + why_h } else { 0.0 }
            + if err_h > 0.0 { 8.0 + err_h } else { 0.0 }
            + 2.0;
        let card = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        let painter = ui.painter().clone();
        card::paint(&painter, card, card::Look::Panel);
        let inner = card.shrink(13.0);
        let cy = inner.top() + row_h / 2.0;
        let dot_c = match dot {
            "healthy" => style::healthy(),
            "warning" => style::warning(),
            _ => style::text_disabled(),
        };
        kit::dot(&painter, Pos2::new(inner.left() + 3.5, cy), dot_c, dot != "off");
        let g = style::galley(ui, label, 12.0, Weight::W700, style::text_1(), 0.4);
        painter.galley(
            Pos2::new(inner.left() + 7.0 + 6.0, cy - g.size().y / 2.0),
            g.clone(),
            style::text_1(),
        );
        let mut px = inner.left() + 13.0 + g.size().x + 6.0;
        if status == "connected" {
            let (q, level) = quality_info(state.get("link_quality").and_then(Value::as_str).unwrap_or(""));
            if !q.is_empty() {
                let color = match level {
                    "healthy" => style::healthy(),
                    "warning" => style::warning(),
                    _ => style::critical(),
                };
                let pg = style::galley(ui, q, 9.5, Weight::W700, color, 0.6);
                let pill = Rect::from_min_size(
                    Pos2::new(px, cy - (pg.size().y + 6.0) / 2.0),
                    pg.size() + Vec2::new(12.0, 6.0),
                );
                painter.rect_stroke(pill, 255, Stroke::new(1.0, color), egui::StrokeKind::Inside);
                painter.galley(pill.min + Vec2::new(6.0, 3.0), pg, color);
                px = pill.right();
            }
        }
        let _ = px;
        // The actions, right aligned: Reconnect (icon), Disconnect.
        let mut rx = inner.right();
        if status != "disconnected" && !in_flight {
            let b = Button::new("Disconnect", Variant::DangerOutline)
                .small()
                .title("Close this link");
            let bw = style::galley(ui, "Disconnect", 11.0, Weight::W600, style::critical(), 0.1)
                .size()
                .x
                + 22.0;
            let r = Rect::from_min_size(Pos2::new(rx - bw, cy - 13.0), Vec2::new(bw, 26.0));
            if b.show(ui, r, "disconnect") {
                self.post(ctx, "/api/mavlink/disconnect", json!({}));
            }
            rx = r.left() - 3.0;
        }
        if status == "connected" && !in_flight {
            let r = Rect::from_min_size(Pos2::new(rx - 25.0, cy - 12.5), Vec2::splat(25.0));
            let resp = ui
                .interact(r, ui.scope_id().with("reconnect"), egui::Sense::click())
                .on_hover_text("Reconnect this link");
            if resp.hovered() {
                painter.rect_filled(r, style::RADIUS_BTN, style::tint(0.04));
            }
            icons::paint(
                ui,
                "refresh-cw",
                Rect::from_center_size(r.center(), Vec2::splat(13.0)),
                if resp.hovered() {
                    style::text_1()
                } else {
                    style::text_3()
                },
            );
            if resp.clicked() {
                let c = if conn.is_empty() {
                    self.last_sent.clone()
                } else {
                    conn.clone()
                };
                if !c.is_empty() {
                    self.last_sent = c.clone();
                    self.post(ctx, "/api/mavlink/connect", json!({ "connection": c }));
                }
            }
        }
        let mut ly = inner.top() + row_h;
        if why_h > 0.0 {
            ly += 8.0;
            let mut bx = inner.right();
            if let Some((text, title)) = &auto {
                let bg = style::galley(ui, text, 9.5, Weight::W700, style::text_3(), 0.6);
                let pill = Rect::from_min_size(
                    Pos2::new(bx - bg.size().x - 12.0, ly + (why_h - bg.size().y - 6.0) / 2.0),
                    bg.size() + Vec2::new(12.0, 6.0),
                );
                dashed_pill(&painter, pill, style::border_strong());
                painter.galley(pill.min + Vec2::new(6.0, 3.0), bg, style::text_3());
                ui.interact(pill, ui.scope_id().with("autobadge"), egui::Sense::hover())
                    .on_hover_text(*title);
                bx = pill.left() - 8.0;
            }
            if !conn.is_empty() {
                let cg = style::galley_mono(ui, &conn, 10.5, style::text_2());
                let clip = Rect::from_min_max(Pos2::new(inner.left(), ly), Pos2::new(bx, ly + why_h));
                ui.painter_at(clip).galley(
                    Pos2::new(inner.left(), ly + (why_h - cg.size().y) / 2.0),
                    cg,
                    style::text_2(),
                );
                ui.interact(clip, ui.scope_id().with("conn"), egui::Sense::hover())
                    .on_hover_text(conn.as_str());
            }
            ly += why_h;
        }
        if err_h > 0.0 {
            ly += 8.0;
            painter.hline(inner.x_range(), ly + 0.5, Stroke::new(1.0, style::border()));
            let eg = style::galley(ui, &error, 11.0, Weight::W400, style::critical(), 0.0);
            let clip = Rect::from_min_size(Pos2::new(inner.left(), ly + 9.0), Vec2::new(inner.width(), 15.4));
            ui.painter_at(clip).galley(clip.min, eg, style::critical());
            ui.interact(clip, ui.scope_id().with("err"), egui::Sense::hover())
                .on_hover_text(error.as_str());
        }
        card.bottom()
    }

    /// A card's title row: icon, title, caret; true when clicked.
    fn card_title(
        ui: &mut egui::Ui,
        at: Pos2,
        w: f32,
        icon: &'static str,
        title: &str,
        open: bool,
        id: &str,
    ) -> (f32, bool) {
        let g = style::galley(ui, title, 12.0, Weight::W600, style::text_1(), 0.4);
        let h = g.size().y.max(15.0);
        let r = Rect::from_min_size(at, Vec2::new(w, h));
        let resp = ui.interact(r, ui.scope_id().with(id), egui::Sense::click());
        icons::paint(
            ui,
            icon,
            Rect::from_min_size(Pos2::new(at.x, at.y + (h - 15.0) / 2.0), Vec2::splat(15.0)),
            style::accent(),
        );
        ui.painter().galley(
            Pos2::new(at.x + 23.0, at.y + (h - g.size().y) / 2.0),
            g,
            style::text_1(),
        );
        let caret = if open { "chevron-down" } else { "chevron-right" };
        let cc = if resp.hovered() {
            style::text_1()
        } else {
            style::text_3()
        };
        icons::paint(
            ui,
            caret,
            Rect::from_min_size(Pos2::new(r.right() - 14.0, at.y + (h - 14.0) / 2.0), Vec2::splat(14.0)),
            cc,
        );
        (h, resp.clicked())
    }

    fn new_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, state: &Value) -> f32 {
        let status = state
            .get("link_status")
            .and_then(Value::as_str)
            .unwrap_or("disconnected")
            .to_lowercase();
        let painter = ui.painter().clone();
        let start_shape = card::reserve(&painter);
        let inner_x = x + 14.0;
        let iw = w - 28.0;
        let mut cy = y + 14.0;
        let (th, clicked) = Self::card_title(
            ui,
            Pos2::new(inner_x, cy),
            iw,
            "plug",
            "New connection",
            self.new_open,
            "newtoggle",
        );
        if clicked {
            self.new_open = !self.new_open;
        }
        cy += th;
        if self.new_open {
            cy += 10.0;
            // .link-kinds: a segmented switch
            let kinds = [
                Segment::new("Serial").icon("cable"),
                Segment::new("UDP / TCP").icon("network"),
            ];
            let (seg, picked) = segmented::show(
                ui,
                Pos2::new(inner_x, cy),
                &kinds,
                usize::from(self.net),
                "kind",
                segmented::Opts::stretched(iw),
            );
            if let Some(i) = picked {
                self.net = i == 1;
            }
            cy = seg.bottom() + 10.0;
            let field_h = 2.0 + 18.0 + 12.0 * 1.32;
            if !self.net {
                cy = field_label(ui, inner_x, cy, "Port") + 5.0;
                let sel = Rect::from_min_size(Pos2::new(inner_x, cy), Vec2::new(iw - 28.0 - 6.0, field_h));
                if let Some(v) = kit::select(
                    ui,
                    sel,
                    &self.ports,
                    &self.port,
                    "Select serial port\u{2026}",
                    true,
                    "Serial port",
                    "port",
                ) {
                    self.port = v;
                    self.port_missing = self
                        .ports
                        .iter()
                        .any(|c| c.value == self.port && c.label.ends_with("(not connected)"));
                    self.error.clear();
                }
                let refresh = Rect::from_min_size(Pos2::new(sel.right() + 6.0, cy), Vec2::new(28.0, field_h));
                if kit::icon_btn(ui, refresh, "refresh-cw", "Refresh serial ports", false, "refreshports") {
                    self.refresh_ports(ctx);
                }
                cy += field_h;
                if self.port_missing {
                    let g = kit::wrapped(ui, &missing_port_hint(&self.port), 11.0, style::warning(), iw);
                    painter.galley(Pos2::new(inner_x, cy + 5.0), g.clone(), style::warning());
                    cy += 5.0 + g.size().y;
                }
                cy += 10.0;
                cy = field_label(ui, inner_x, cy, "Baud") + 5.0;
                let bauds: Vec<Choice> = BAUDS.iter().map(|(v, l)| Choice::new(*v, *l)).collect();
                let sel = Rect::from_min_size(Pos2::new(inner_x, cy), Vec2::new(iw, field_h));
                if let Some(v) = kit::select(ui, sel, &bauds, &self.baud, "", true, "Baud rate", "baud") {
                    self.baud = v;
                }
                cy += field_h;
            } else {
                cy = field_label(ui, inner_x, cy, "Connection string") + 5.0;
                let input = Rect::from_min_size(Pos2::new(inner_x, cy), Vec2::new(iw, field_h));
                let resp = kit::text_field(ui, input, &mut self.custom, "udp:0.0.0.0:14550", true, "custom");
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.connect(ctx);
                }
                cy += field_h + 9.0;
                let lg = style::galley(ui, "COMMON ENDPOINTS", 10.0, Weight::W600, style::text_3(), 0.8);
                painter.galley(Pos2::new(inner_x, cy), lg.clone(), style::text_3());
                cy += lg.size().y + 5.0;
                for (i, (name, conn, note)) in PRESETS.iter().enumerate() {
                    let row =
                        Rect::from_min_size(Pos2::new(inner_x, cy), Vec2::new(iw, 2.0 + 12.0 + 2.0 + 11.0 * 1.25));
                    let resp = ui
                        .interact(row, ui.scope_id().with(("preset", i)), egui::Sense::click())
                        .on_hover_text(*conn);
                    let (fill, stroke) = if resp.hovered() {
                        (style::glass(0.95), style::accent_a(0.45))
                    } else {
                        (style::glass(0.82), style::tint(0.09))
                    };
                    {
                        let r = row;
                        painter.rect_filled(r, style::RADIUS_BTN, fill);
                        painter.rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
                    }
                    let ng = style::galley(
                        ui,
                        name,
                        11.0,
                        Weight::W600,
                        if resp.hovered() {
                            style::accent()
                        } else {
                            style::text_1()
                        },
                        0.1,
                    );
                    painter.galley(
                        Pos2::new(row.left() + 8.0, row.center().y - ng.size().y / 2.0),
                        ng,
                        style::text_1(),
                    );
                    let cg = style::galley_mono(ui, conn, 10.0, style::text_3());
                    let info =
                        Rect::from_center_size(Pos2::new(row.right() - 8.0 - 8.0, row.center().y), Vec2::splat(16.0));
                    painter.galley(
                        Pos2::new(
                            info.left() - 6.0 - cg.size().x,
                            row.center().y - cg.size().y / 2.0 + 1.0,
                        ),
                        cg,
                        style::text_3(),
                    );
                    crate::ui::popover::info(ui, info.center(), name, note, ("presetinfo", i));
                    if resp.clicked() {
                        self.custom = (*conn).to_string();
                    }
                    cy = row.bottom() + 4.0;
                }
                cy -= 4.0;
            }
            cy += 10.0;
            let busy = self.connecting.is_some() || status == "connecting";
            let label = match status.as_str() {
                "connecting" => "Connecting\u{2026}",
                "connected" => "Connected",
                _ => "Connect",
            };
            let b = Button::new(label, Variant::Primary)
                .icon("plug")
                .bold()
                .enabled(!busy && status != "connected");
            let r = Rect::from_min_size(Pos2::new(inner_x, cy), Vec2::new(iw, b.height()));
            cy += b.height();
            if b.show(ui, r, "connect") {
                self.connect(ctx);
            }
        }
        let card = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 14.0));
        card::fill(&painter, start_shape, card, card::Look::Panel);
        card.bottom()
    }

    fn share_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let start_shape = card::reserve(&painter);
        let inner_x = x + 14.0;
        let iw = w - 28.0;
        let mut cy = y + 14.0;
        let (th, clicked) = Self::card_title(
            ui,
            Pos2::new(inner_x, cy),
            iw,
            "share-2",
            "Share with another station",
            self.share_open,
            "sharetoggle",
        );
        if clicked {
            self.share_open = !self.share_open;
        }
        cy += th;
        if self.share_open {
            let s = self.forwarding.clone();
            let mut enabled = s.get("running") == Some(&Value::Bool(true));
            let mut allow = s.get("allow_commands") == Some(&Value::Bool(true));
            cy += 10.0;
            if kit::checkbox(
                ui,
                Pos2::new(inner_x, cy),
                iw,
                "Mirror this link over UDP",
                &mut enabled,
                false,
                "fwdon",
            ) {
                self.save_forwarding(ctx, enabled, allow);
            }
            cy += 15.0 + 10.0;
            cy = field_label(ui, inner_x, cy, "Port the other station listens on") + 5.0;
            let field_h = 2.0 + 18.0 + 12.0 * 1.21;
            let input = Rect::from_min_size(Pos2::new(inner_x, cy), Vec2::new(iw, field_h));
            let resp = kit::text_field(ui, input, &mut self.fwd_port, "", true, "fwdport");
            if resp.lost_focus() {
                self.save_forwarding(ctx, enabled, allow);
            }
            cy += field_h + 5.0;
            let hint = kit::wrapped(ui, &forwarding_hint(&s), 11.0, style::text_3(), iw);
            painter.galley(Pos2::new(inner_x, cy), hint.clone(), style::text_3());
            cy += hint.size().y + 10.0;
            if kit::checkbox(
                ui,
                Pos2::new(inner_x, cy),
                iw,
                "Let it command the aircraft",
                &mut allow,
                true,
                "fwdallow",
            ) {
                self.save_forwarding(ctx, enabled, allow);
            }
            cy += 15.0;
            if allow {
                let warn = "Two stations will be able to arm and change the mode of this aircraft. Leave off to share telemetry only. Corvus transmits as MAVLink system 254 and QGroundControl as 255; if the other station is on 254 too, give it its own. PX4 counts sequence numbers per ID and reports loss that is not happening.";
                let g = kit::wrapped(ui, warn, 11.0, style::warning(), iw);
                painter.galley(Pos2::new(inner_x, cy + 10.0), g.clone(), style::warning());
                cy += 10.0 + g.size().y;
            }
            let err = s.get("error").and_then(Value::as_str).filter(|e| !e.is_empty());
            let notice = s.get("notice").and_then(Value::as_str).filter(|e| !e.is_empty());
            if let Some(text) = err.or(notice) {
                let color = if err.is_some() {
                    style::critical()
                } else {
                    style::warning()
                };
                let g = kit::wrapped(ui, text, 10.5, color, iw);
                painter.galley(Pos2::new(inner_x, cy + 10.0), g.clone(), color);
                cy += 10.0 + g.size().y;
            }
            let sent = s.get("frames_sent").and_then(Value::as_u64).unwrap_or(0);
            let injected = s.get("frames_injected").and_then(Value::as_u64).unwrap_or(0);
            if enabled && (sent > 0 || injected > 0) {
                let mut t = format!("{sent} frame(s) mirrored");
                if allow {
                    t.push_str(&format!(", {injected} received from the other station"));
                }
                let g = kit::wrapped(ui, &t, 11.0, style::text_3(), iw);
                painter.galley(Pos2::new(inner_x, cy + 10.0), g.clone(), style::text_3());
                cy += 10.0 + g.size().y;
            }
        }
        let card = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 14.0));
        card::fill(&painter, start_shape, card, card::Look::Panel);
        card.bottom()
    }
}

/// `.link-field-label`: 10 px, uppercase. Returns the y under it.
fn field_label(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x, y), g, style::text_3());
    y + h
}

/// `.panel-section-header` with its title; returns its rect.
/// `.panel-section-header`: 11 px above and below the title or the tallest
/// of its actions (`actions_h`, 0 when it has none), a hairline under it.
pub fn section_header(ui: &egui::Ui, rect: Rect, title: &str, actions_h: f32) -> Rect {
    let g = style::galley(ui, title, 12.0, Weight::W600, style::text_1(), 0.4);
    let h = 11.0 * 2.0 + g.size().y.max(15.0).max(actions_h) + 1.0;
    let r = Rect::from_min_size(rect.min, Vec2::new(rect.width(), h));
    let painter = ui.painter();
    painter.galley(
        Pos2::new(r.left() + 12.0, r.center().y - g.size().y / 2.0),
        g,
        style::text_1(),
    );
    painter.hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, style::border_soft()));
    r
}

/// A pill outlined in dashes (`border: 1px dashed`).
fn dashed_pill(painter: &egui::Painter, r: Rect, color: Color32) {
    let rad = r.height() / 2.0;
    let mut pts = Vec::new();
    let arc = |c: Pos2, a0: f32, pts: &mut Vec<Pos2>| {
        for i in 0..=8 {
            let a = a0 + std::f32::consts::PI * i as f32 / 8.0;
            pts.push(c + Vec2::new(a.cos(), a.sin()) * rad);
        }
    };
    arc(
        Pos2::new(r.right() - rad, r.center().y),
        -std::f32::consts::FRAC_PI_2,
        &mut pts,
    );
    arc(
        Pos2::new(r.left() + rad, r.center().y),
        std::f32::consts::FRAC_PI_2,
        &mut pts,
    );
    pts.push(pts[0]);
    painter.extend(Shape::dashed_line(&pts, Stroke::new(1.0, color), 3.0, 2.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_as_link_js_writes_them() {
        assert_eq!(
            build_connection_string("/dev/ttyACM0", "57600"),
            "serial:/dev/ttyACM0:57600"
        );
        assert_eq!(build_connection_string("", "57600"), "");
        assert_eq!(status_info("Connected"), ("CONNECTED", "healthy"));
        assert_eq!(status_info("weird"), ("DISCONNECTED", "off"));
        assert_eq!(quality_info("lost"), ("LOST", "critical"));
        assert_eq!(quality_info(""), ("", "off"));
    }

    #[test]
    fn the_auto_badge() {
        let s = json!({"link_auto": {"enabled": true, "reason": "sik"}});
        assert_eq!(auto_info(&s, "connected").unwrap().0, "AUTO \u{00b7} RADIO");
        assert_eq!(auto_info(&s, "connecting").unwrap().0, "AUTO \u{00b7} SCANNING");
        let manual = json!({"link_auto": {"enabled": true, "manual_override": true}});
        assert!(auto_info(&manual, "connected").is_none());
        assert!(auto_info(&json!({}), "connected").is_none());
    }

    #[test]
    fn ports_with_a_missing_device() {
        let data = json!({"ports": [{"device": "/dev/ttyACM0", "description": "PX4 FMU"}]});
        let (c, missing) = port_choices(&data, "/dev/ttyUSB0");
        assert!(missing);
        assert_eq!(c[1].label, "/dev/ttyACM0 (PX4 FMU)");
        assert_eq!(c[2].label, "/dev/ttyUSB0 (not connected)");
        assert!(!port_choices(&data, "/dev/ttyACM0").1);
    }

    #[test]
    fn the_forwarding_hint() {
        assert_eq!(
            forwarding_hint(&json!({})),
            "Mirrors this link to 127.0.0.1:14550, the UDP link QGroundControl opens on its own."
        );
        assert_eq!(
            forwarding_hint(&json!({"running": true, "port": 14551})),
            "Mirroring to 127.0.0.1:14551. Nothing has answered yet."
        );
        assert_eq!(
            forwarding_hint(&json!({"running": true, "peers": [1, 2], "listen_port": 14600})),
            "2 station(s) connected; mirroring to 127.0.0.1:14550. Corvus answers on 127.0.0.1:14600."
        );
        assert!(forwarding_hint(
            &json!({"running": true, "commands_refused": 3, "commands_refused_from": ["10.0.0.9:1"]})
        )
        .starts_with("3 command(s) refused from 10.0.0.9:1."));
    }
}
