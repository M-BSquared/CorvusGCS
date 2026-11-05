//! Every vehicle the station hears, and which one is flown
//! (`python/src/js/fleet.js`, `map-fleet.js`).
//!
//! The backend (`corvus/fleet.py`) pushes the whole list on the `fleet` topic
//! of `/api/events`, a few times a second while anything in it changes. This
//! module keeps the latest copy, turns what the vehicles not flown said into
//! board notes, and posts the operator's changes: which vehicle is flown,
//! names and colours, extra links, removing a vehicle.

pub mod dialog;
pub mod tracks;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32};
use serde_json::{json, Value};

use crate::net::http::{self, Server};
use crate::net::stream::Stream;

/// A snapshot published just before a removal can still be on its way; the
/// removed vehicle is held off the list this long (`REMOVED_HOLD_MS`).
const REMOVED_HOLD: Duration = Duration::from_millis(3000);

/// One vehicle of the list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vehicle {
    pub id: String,
    pub name: String,
    /// The operator named it; otherwise `name` is the default.
    pub custom_name: bool,
    /// `#RRGGBB`, or empty for the theme's own colour.
    pub color: String,
    pub sysid: i64,
    pub link: String,
    pub link_label: String,
    pub stale: bool,
    pub armed: bool,
    pub active: bool,
    pub kind: String,
    pub stack: String,
    pub mode_label: String,
    pub battery: Option<f64>,
    pub voltage: Option<f64>,
    pub altitude_rel: Option<f64>,
    /// Longitude and latitude.
    pub position: Option<[f64; 2]>,
    pub heading: Option<f64>,
}

/// One link of the list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Link {
    pub id: String,
    pub label: String,
    pub primary: bool,
    pub connection: String,
    pub status: String,
    pub error: String,
    pub vehicles: i64,
}

/// What `GET /api/fleet` answers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub active: String,
    pub active_link: String,
    pub links: Vec<Link>,
    pub vehicles: Vec<Vehicle>,
    pub palette: Vec<String>,
    pub suggestion: Option<Value>,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

impl Snapshot {
    /// `normalise`: a vehicle the operator just removed is held off the list,
    /// and so is the active id when it is that vehicle.
    pub fn parse(data: &Value, removed: &dyn Fn(&str) -> bool) -> Snapshot {
        let vehicles = data
            .get("vehicles")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter(|v| v.get("id").and_then(Value::as_str).is_some_and(|id| !removed(id)))
                    .map(|v| Vehicle {
                        id: text(v, "id"),
                        name: text(v, "name"),
                        custom_name: flag(v, "custom_name"),
                        color: text(v, "color"),
                        sysid: v.get("sysid").and_then(Value::as_i64).unwrap_or(0),
                        link: text(v, "link"),
                        link_label: text(v, "link_label"),
                        stale: flag(v, "stale"),
                        armed: flag(v, "armed"),
                        active: flag(v, "active"),
                        kind: text(v, "type"),
                        stack: text(v, "stack"),
                        mode_label: text(v, "mode_label"),
                        battery: number(v, "battery"),
                        voltage: number(v, "voltage"),
                        altitude_rel: number(v, "altitude_rel"),
                        position: v
                            .get("position")
                            .and_then(Value::as_array)
                            .and_then(|p| Some([p.first()?.as_f64()?, p.get(1)?.as_f64()?])),
                        heading: number(v, "heading"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let active = text(data, "active");
        Snapshot {
            active: if removed(&active) { String::new() } else { active },
            active_link: data
                .get("active_link")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            links: data
                .get("links")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .map(|l| Link {
                            id: text(l, "id"),
                            label: text(l, "label"),
                            primary: flag(l, "primary"),
                            connection: text(l, "connection"),
                            status: text(l, "status"),
                            error: text(l, "error"),
                            vehicles: l.get("vehicles").and_then(Value::as_i64).unwrap_or(0),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            vehicles,
            palette: data
                .get("palette")
                .and_then(Value::as_array)
                .map(|p| p.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default(),
            suggestion: data.get("suggestion").filter(|s| s.is_object()).cloned(),
        }
    }

    pub fn vehicle(&self, id: &str) -> Option<&Vehicle> {
        if id.is_empty() {
            return None;
        }
        self.vehicles.iter().find(|v| v.id == id)
    }

    /// The vehicle being flown, while there is one.
    pub fn active_vehicle(&self) -> Option<&Vehicle> {
        self.vehicle(&self.active)
    }

    /// More than one thing to choose between: vehicles or links.
    pub fn is_multi(&self) -> bool {
        self.vehicles.len() > 1 || self.links.len() > 1
    }

    /// The colour chosen for `id`, or `""` for the theme's own.
    pub fn color_of(&self, id: &str) -> &str {
        self.vehicle(id).map_or("", |v| v.color.as_str())
    }

    /// The name the operator reads for `id`, or `""`.
    pub fn name_of(&self, id: &str) -> &str {
        self.vehicle(id).map_or("", |v| v.name.as_str())
    }

    /// The link a vehicle is heard on.
    pub fn link_of(&self, v: &Vehicle) -> Option<&Link> {
        self.links.iter().find(|l| l.id == v.link)
    }
}

/// `"#RRGGBB"` as a colour.
pub fn parse_hex(hex: &str) -> Option<Color32> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color32::from_rgb(c(0)?, c(2)?, c(4)?))
}

/// `hex` mixed toward white by `amount`, as `Corvus.fleet.tint` does.
pub fn tint(hex: &str, amount: f64) -> String {
    let Some(c) = parse_hex(hex) else {
        return hex.to_string();
    };
    let k = amount.clamp(0.0, 1.0);
    let mix = |v: u8| (f64::from(v) + (255.0 - f64::from(v)) * k).round() as u8;
    format!("#{:02X}{:02X}{:02X}", mix(c.r()), mix(c.g()), mix(c.b()))
}

/// `#RRGGBB` for a hue in degrees, saturation and lightness in 0 to 1.
pub fn hsl_hex(hue: f64, sat: f64, light: f64) -> String {
    let a = sat * light.min(1.0 - light);
    let channel = |n: f64| {
        let k = (n + hue / 30.0) % 12.0;
        let v = light - a * (-1.0f64).max((k - 3.0).min(9.0 - k).min(1.0));
        format!("{:02X}", (v * 255.0).round() as u8)
    };
    format!("#{}{}{}", channel(0.0), channel(8.0), channel(4.0))
}

/// The tone of the earlier flight at `index`, oldest first: neighbours step
/// round the wheel by the golden angle, pulled toward white so they stay
/// quieter than the flight in progress.
pub fn rainbow_tone(index: usize) -> String {
    let hue = (200.0 + index as f64 * 137.508) % 360.0;
    tint(&hsl_hex(hue, 0.8, 0.55), 0.3)
}

/// The words under a vehicle's name (`detailText`).
pub fn detail_text(v: &Vehicle, snap: &Snapshot) -> String {
    let link = snap.link_of(v);
    let mut parts = vec![format!("System {}", v.sysid), v.link_label.clone()];
    parts.push(
        link.filter(|l| !l.primary)
            .map(|l| l.connection.clone())
            .unwrap_or_default(),
    );
    if !v.kind.is_empty() {
        parts.push(v.kind.replace('_', " ").to_lowercase());
    }
    if !v.stack.is_empty() {
        parts.push(
            match v.stack.as_str() {
                "px4" => "PX4",
                "ardupilot" => "ArduPilot",
                other => other,
            }
            .to_string(),
        );
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" \u{b7} ")
}

/// The live line (`readoutText`): mode, battery and height above home.
pub fn readout_text(v: &Vehicle, units: &crate::data::units::Units) -> String {
    let mut parts = Vec::new();
    if !v.mode_label.is_empty() {
        parts.push(v.mode_label.clone());
    }
    if let Some(b) = v.battery {
        parts.push(format!("{b}%"));
    } else if let Some(volts) = v.voltage {
        parts.push(format!("{} V", crate::data::units::js_to_fixed(volts, 1)));
    }
    if let Some(a) = v.altitude_rel {
        parts.push(format!("{} rel", units.format_length(a, 0, false, false)));
    }
    parts.join(" \u{b7} ")
}

/// "Lost", "Armed" or "Disarmed".
pub fn status_text(v: &Vehicle) -> &'static str {
    if v.stale {
        "Lost"
    } else if v.armed {
        "Armed"
    } else {
        "Disarmed"
    }
}

/// The Vehicle block's words (`fleetVehicleLabel`): the vehicle's own name
/// once there is more than one, or one the operator named.
pub fn vehicle_label(state: &Value, snap: &Snapshot, base: &str) -> String {
    if state.get("connected") != Some(&Value::Bool(true)) {
        return base.to_string();
    }
    let id = state.get("vehicle_id").and_then(Value::as_str).unwrap_or("");
    match snap.vehicle(id) {
        Some(v) if snap.is_multi() || v.custom_name => v.name.clone(),
        _ => base.to_string(),
    }
}

/// The server-sent part of a vehicle's notes: warnings and worse, and a
/// vehicle that went quiet or came back. The first list after start is the
/// baseline, so a restart does not replay the session.
pub fn new_events(data: &Value, seen: &mut Option<i64>, snap: &Snapshot) -> Vec<(String, String)> {
    let Some(list) = data.get("events").and_then(Value::as_array) else {
        return Vec::new();
    };
    let top = list
        .iter()
        .map(|e| e.get("seq").and_then(Value::as_i64).unwrap_or(0))
        .max()
        .unwrap_or(0);
    let Some(prev) = *seen else {
        *seen = Some(top);
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        let seq = e.get("seq").and_then(Value::as_i64).unwrap_or(0);
        if seq <= prev {
            continue;
        }
        let vehicle = e.get("vehicle").and_then(Value::as_str).unwrap_or("");
        let who = match snap.name_of(vehicle) {
            "" if vehicle.is_empty() => "Another vehicle".to_string(),
            "" => vehicle.to_string(),
            n => n.to_string(),
        };
        let level = e
            .get("level")
            .and_then(Value::as_str)
            .filter(|l| matches!(*l, "critical" | "warning"))
            .unwrap_or("info");
        out.push((level.to_string(), format!("{who}: {}", text(e, "text"))));
    }
    *seen = Some(prev.max(top));
    out
}

#[derive(Default)]
struct Shared {
    snapshot: Snapshot,
    removed_at: HashMap<String, Instant>,
    seen_event: Option<i64>,
    notes: Vec<(String, String)>,
    errors: Vec<String>,
}

impl Shared {
    fn recently_removed(&mut self, id: &str) -> bool {
        match self.removed_at.get(id) {
            Some(at) if at.elapsed() < REMOVED_HOLD => true,
            Some(_) => {
                self.removed_at.remove(id);
                false
            }
            None => false,
        }
    }

    fn take(&mut self, data: &Value) {
        let held: Vec<String> = self.removed_at.keys().cloned().collect();
        let held: Vec<String> = held.into_iter().filter(|id| self.recently_removed(id)).collect();
        let snap = Snapshot::parse(data, &|id| held.iter().any(|h| h == id));
        let notes = new_events(data, &mut self.seen_event, &snap);
        self.notes.extend(notes);
        self.snapshot = snap;
    }
}

/// The fleet as the window sees it.
pub struct Fleet {
    server: Server,
    shared: Arc<Mutex<Shared>>,
    stream: Option<Stream>,
}

impl Fleet {
    /// Follows the `fleet` topic and reads the list once.
    pub fn start(server: Server, ctx: &egui::Context) -> Fleet {
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let (s, c) = (shared.clone(), ctx.clone());
        let stream = Stream::start(server.clone(), "/api/events?topics=fleet".into(), move |ev| {
            if ev.name == "fleet" {
                if let Ok(data) = serde_json::from_str::<Value>(&ev.data) {
                    if let Ok(mut g) = s.lock() {
                        g.take(&data);
                    }
                    c.request_repaint();
                }
            }
        });
        let (s, c, srv) = (shared.clone(), ctx.clone(), server.clone());
        std::thread::spawn(move || {
            if let Ok(data) = http::get_json(&srv, "/api/fleet") {
                if let Ok(mut g) = s.lock() {
                    g.take(&data);
                }
                c.request_repaint();
            }
        });
        Fleet {
            server,
            shared,
            stream: Some(stream),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().map(|g| g.snapshot.clone()).unwrap_or_default()
    }

    /// What the other vehicles said, for the board.
    pub fn take_notes(&self) -> Vec<(String, String)> {
        self.shared
            .lock()
            .map(|mut g| std::mem::take(&mut g.notes))
            .unwrap_or_default()
    }

    /// What the backend refused, for toasts.
    pub fn take_errors(&self) -> Vec<String> {
        self.shared
            .lock()
            .map(|mut g| std::mem::take(&mut g.errors))
            .unwrap_or_default()
    }

    /// POSTs a change on a thread of its own. The answer carries the new
    /// list (`fleet`); a refusal goes to `take_errors`. `removed` is the
    /// vehicle to hold off the list meanwhile.
    fn post(&self, ctx: &egui::Context, path: &'static str, body: Value, removed: Option<String>) {
        if let Some(id) = &removed {
            if let Ok(mut g) = self.shared.lock() {
                g.removed_at.insert(id.clone(), Instant::now());
            }
        }
        let (server, shared, ctx) = (self.server.clone(), self.shared.clone(), ctx.clone());
        std::thread::spawn(move || {
            let answer = http::post_json(&server, path, &body);
            if let Ok(mut g) = shared.lock() {
                match answer {
                    Ok(d) if d.get("ok") != Some(&Value::Bool(false)) => {
                        if let Some(f) = d.get("fleet") {
                            g.take(f);
                        }
                    }
                    Ok(d) => {
                        if let Some(id) = &removed {
                            g.removed_at.remove(id);
                        }
                        g.errors.push(
                            d.get("error")
                                .and_then(Value::as_str)
                                .filter(|e| !e.is_empty())
                                .unwrap_or("The vehicle list could not be changed.")
                                .to_string(),
                        );
                    }
                    Err(e) => {
                        if let Some(id) = &removed {
                            g.removed_at.remove(id);
                        }
                        g.errors.push(e);
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    /// Fly this vehicle from now on.
    pub fn select(&self, ctx: &egui::Context, id: &str) {
        self.post(ctx, "/api/fleet/select", json!({"id": id}), None);
    }

    /// Name a vehicle; an empty name goes back to the default.
    pub fn rename(&self, ctx: &egui::Context, id: &str, name: &str) {
        self.post(ctx, "/api/fleet/vehicle", json!({"id": id, "name": name}), None);
    }

    /// Give a vehicle a map colour; empty is the theme's.
    pub fn set_color(&self, ctx: &egui::Context, id: &str, color: &str) {
        self.post(ctx, "/api/fleet/vehicle", json!({"id": id, "color": color}), None);
    }

    /// Open another link, for a vehicle on a radio of its own.
    pub fn add_link(&self, ctx: &egui::Context, connection: &str) {
        self.post(ctx, "/api/fleet/links/add", json!({"connection": connection}), None);
    }

    /// Close an extra link.
    pub fn remove_link(&self, ctx: &egui::Context, id: &str) {
        self.post(ctx, "/api/fleet/links/remove", json!({"id": id}), None);
    }

    /// Take a vehicle off the list: its link is closed and the map forgets it.
    pub fn remove_vehicle(&self, ctx: &egui::Context, id: &str, confirm_armed: bool) {
        self.post(
            ctx,
            "/api/fleet/vehicle/remove",
            json!({"id": id, "confirm_armed": confirm_armed}),
            Some(id.to_string()),
        );
    }

    pub fn shut_down(&mut self) {
        if let Some(mut s) = self.stream.take() {
            s.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::units::Units;

    fn data() -> Value {
        json!({
            "active": "main:1",
            "active_link": "main",
            "links": [
                {"id": "main", "label": "Main link", "primary": true, "connection": "udpin:127.0.0.1:14550", "status": "connected", "vehicles": 1},
                {"id": "link-2", "label": "Radio 2", "primary": false, "connection": "serial:/dev/ttyUSB1:57600", "status": "connecting", "vehicles": 1}
            ],
            "vehicles": [
                {"id": "main:1", "name": "Vehicle 1", "custom_name": false, "color": "", "sysid": 1, "link": "main", "link_label": "Main link",
                 "stale": false, "armed": true, "active": true, "type": "QUADROTOR", "stack": "px4", "mode_label": "MISSION",
                 "battery": 71, "altitude_rel": 12.0, "position": [11.64, 48.07], "heading": 90.0},
                {"id": "link-2:2", "name": "Rover", "custom_name": true, "color": "#2BC4E4", "sysid": 2, "link": "link-2", "link_label": "Radio 2",
                 "stale": true, "armed": false, "active": false, "type": "GROUND_ROVER", "stack": "ardupilot", "mode_label": "",
                 "voltage": 12.34, "position": null}
            ],
            "palette": ["#2BC4E4", "#F5A524"],
            "suggestion": null,
            "events": [
                {"seq": 3, "vehicle": "link-2:2", "level": "warning", "text": "went quiet"},
                {"seq": 4, "vehicle": "link-2:2", "level": "debug", "text": "back"}
            ]
        })
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn the_list_is_read_as_the_web_reads_it() {
        let s = Snapshot::parse(&data(), &none);
        assert_eq!(s.active, "main:1");
        assert_eq!(s.vehicles.len(), 2);
        assert!(s.is_multi());
        assert_eq!(s.color_of("link-2:2"), "#2BC4E4");
        assert_eq!(s.name_of("link-2:2"), "Rover");
        assert_eq!(s.active_vehicle().map(|v| v.sysid), Some(1));
        assert_eq!(s.vehicles[1].position, None);
        assert_eq!(s.palette.len(), 2);
    }

    #[test]
    fn a_single_vehicle_on_one_link_is_not_multi() {
        let s = Snapshot::parse(
            &json!({"active": "main:1", "links": [{"id": "main"}], "vehicles": [{"id": "main:1"}]}),
            &none,
        );
        assert!(!s.is_multi());
    }

    #[test]
    fn a_removed_vehicle_stays_off_the_list() {
        let s = Snapshot::parse(&data(), &|id| id == "main:1");
        assert_eq!(s.vehicles.len(), 1);
        assert_eq!(s.active, "");
        assert!(s.active_vehicle().is_none());
    }

    #[test]
    fn the_words_under_a_name() {
        let s = Snapshot::parse(&data(), &none);
        assert_eq!(
            detail_text(&s.vehicles[0], &s),
            "System 1 \u{b7} Main link \u{b7} quadrotor \u{b7} PX4"
        );
        assert_eq!(
            detail_text(&s.vehicles[1], &s),
            "System 2 \u{b7} Radio 2 \u{b7} serial:/dev/ttyUSB1:57600 \u{b7} ground rover \u{b7} ArduPilot"
        );
    }

    #[test]
    fn the_readout_is_mode_battery_and_height() {
        let s = Snapshot::parse(&data(), &none);
        let u = Units::default();
        assert_eq!(readout_text(&s.vehicles[0], &u), "MISSION \u{b7} 71% \u{b7} 12 m rel");
        assert_eq!(readout_text(&s.vehicles[1], &u), "12.3 V");
    }

    #[test]
    fn the_status_word() {
        let s = Snapshot::parse(&data(), &none);
        assert_eq!(status_text(&s.vehicles[0]), "Armed");
        assert_eq!(status_text(&s.vehicles[1]), "Lost");
        assert_eq!(status_text(&Vehicle::default()), "Disarmed");
    }

    #[test]
    fn the_top_bar_names_the_vehicle_once_there_are_two() {
        let s = Snapshot::parse(&data(), &none);
        let st = json!({"connected": true, "vehicle_id": "main:1"});
        assert_eq!(vehicle_label(&st, &s, "PX4 Quadcopter"), "Vehicle 1");
        assert_eq!(
            vehicle_label(&json!({"connected": false}), &s, "DISCONNECTED"),
            "DISCONNECTED"
        );
        let one = Snapshot::parse(
            &json!({"links": [{"id": "main"}], "vehicles": [{"id": "main:1", "name": "Vehicle 1"}]}),
            &none,
        );
        assert_eq!(vehicle_label(&st, &one, "PX4 Quadcopter"), "PX4 Quadcopter");
        let named = Snapshot::parse(
            &json!({"links": [{"id": "main"}], "vehicles": [{"id": "main:1", "name": "Scout", "custom_name": true}]}),
            &none,
        );
        assert_eq!(vehicle_label(&st, &named, "PX4 Quadcopter"), "Scout");
    }

    #[test]
    fn the_first_list_is_the_baseline_and_later_events_are_notes() {
        let s = Snapshot::parse(&data(), &none);
        let mut seen = None;
        assert!(new_events(&data(), &mut seen, &s).is_empty());
        assert_eq!(seen, Some(4));
        let mut later = data();
        later["events"] = json!([
            {"seq": 4, "vehicle": "link-2:2", "level": "debug", "text": "old"},
            {"seq": 5, "vehicle": "link-2:2", "level": "critical", "text": "battery low"},
            {"seq": 6, "vehicle": "", "level": "info", "text": "hello"}
        ]);
        let notes = new_events(&later, &mut seen, &s);
        assert_eq!(
            notes,
            vec![
                ("critical".to_string(), "Rover: battery low".to_string()),
                ("info".to_string(), "Another vehicle: hello".to_string())
            ]
        );
        assert_eq!(seen, Some(6));
    }

    #[test]
    fn colours_mix_toward_white_as_the_web_mixes_them() {
        assert_eq!(tint("#000000", 0.5), "#808080");
        assert_eq!(tint("#2BC4E4", 0.0), "#2BC4E4");
        assert_eq!(tint("#2BC4E4", 1.0), "#FFFFFF");
        assert_eq!(tint("nonsense", 0.5), "nonsense");
        assert_eq!(parse_hex("#2BC4E4"), Some(Color32::from_rgb(0x2B, 0xC4, 0xE4)));
        assert_eq!(parse_hex("2BC4E4"), None);
    }

    #[test]
    fn the_rainbow_steps_by_the_golden_angle() {
        assert_eq!(hsl_hex(0.0, 1.0, 0.5), "#FF0000");
        assert_eq!(hsl_hex(120.0, 1.0, 0.5), "#00FF00");
        assert_eq!(hsl_hex(240.0, 1.0, 0.5), "#0000FF");
        // 200 degrees, 80 percent, 55 percent, then 30 percent toward white.
        assert_eq!(rainbow_tone(0), tint(&hsl_hex(200.0, 0.8, 0.55), 0.3));
        assert_ne!(rainbow_tone(0), rainbow_tone(1));
    }
}
