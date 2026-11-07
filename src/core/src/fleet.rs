//! Several vehicles at once (`fleet.py`).
//!
//! One vehicle is flown at a time; the others are seen. **One link is one
//! vehicle**: a link flies the first autopilot it latches and nothing else.
//! The [`FleetTracker`] is the passive half (what the vehicle on one link last
//! said about itself); [`Fleet`] is the manager over every link: names and
//! colours, which vehicle is flown, notifications about the others, and the
//! snapshot the interface renders.
//!
//! Python swaps a bridge and a store proxy when another vehicle is selected.
//! Here the fleet only records which link is flown ([`Fleet::active_link`],
//! [`Fleet::active_bridge`], [`Fleet::active_store`]) and tells a switch hook
//! once; the routes read the flown link from it.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::clock::{Clock, SystemClock};
use crate::dialect::dialect_for_id;
use crate::protocol::common::py_round;
use crate::protocol::receive::{mav_type_name, Frame, Payload};
use crate::serial_ports::same_port;

pub const MAIN_LINK: &str = "main";
pub const PALETTE: [&str; 8] = [
    "#2F7DF6", "#1FA971", "#F5A623", "#9B59D0", "#00A3B4", "#E8508C", "#7DB928", "#C9712E",
];
pub const STALE_S: f64 = 5.0;
pub const FORGET_S: f64 = 600.0;
pub const PUBLISH_INTERVAL_S: f64 = 0.25;
pub const COLOR_GRACE_S: f64 = 15.0;
pub const EVENT_MAX_SEVERITY: i64 = 4;
pub const EVENT_REPEAT_S: f64 = 30.0;
pub const MAX_EVENTS: usize = 40;
pub const MAX_EXTRA_LINKS: usize = 8;
pub const MAX_NAME_CHARS: usize = 40;
pub const MAX_REMEMBERED: usize = 64;

// ------------------------------------------------------------ text helpers

fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn py_strip(text: &str) -> &str {
    text.trim_matches(py_space)
}

fn py_collapse(text: &str) -> String {
    text.split(py_space)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What `re.match(r"^...$")` sees: one trailing newline does not stop `$`.
fn dollar(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

fn digits(text: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit())
}

fn is_link_id(text: &str) -> bool {
    dollar(text).strip_prefix("link-").is_some_and(|d| digits(d, 1, 3))
}

/// The id a vehicle is known by: its link and its MAVLink system id.
pub fn vehicle_key(link_id: &str, system_id: i64) -> String {
    format!("{link_id}:{system_id}")
}

/// `"link-2:3"` is `("link-2", 3)`, anything else is None.
pub fn parse_vehicle_key(key: &str) -> Option<(String, i64)> {
    let (link, system) = dollar(key).split_once(':')?;
    if link != MAIN_LINK && !is_link_id(link) {
        return None;
    }
    if !digits(system, 1, 3) {
        return None;
    }
    let system: i64 = system.parse().ok()?;
    (1..=255).contains(&system).then(|| (link.to_string(), system))
}

fn parse_key_value(key: &Value) -> Option<(String, i64)> {
    parse_vehicle_key(key.as_str()?)
}

/// A vehicle name as typed, trimmed and bounded; "" for none.
pub fn clean_name(value: &Value) -> String {
    match value.as_str() {
        Some(text) => py_collapse(text).chars().take(MAX_NAME_CHARS).collect(),
        None => String::new(),
    }
}

/// `#RRGGBB` in upper case, or "" for "the theme's own colour".
pub fn clean_color(value: &Value) -> String {
    let Some(text) = value.as_str() else {
        return String::new();
    };
    let text = py_strip(text);
    let ok = text.len() == 7 && text.starts_with('#') && text[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        text.to_uppercase()
    } else {
        String::new()
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The `fleet` block of the config, bounded and typed. Never panics.
pub fn coerce_config(raw: &Value) -> Option<Value> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    let mut links: Vec<Value> = Vec::new();
    let mut seen_ids: Vec<String> = Vec::new();
    let mut seen_conns: Vec<String> = Vec::new();
    if let Some(Value::Array(entries)) = raw.get("links") {
        for entry in entries {
            let Some(entry) = entry.as_object() else { continue };
            let Some(link_id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !is_link_id(link_id) {
                continue;
            }
            let Some(conn) = entry.get("connection").and_then(Value::as_str) else {
                continue;
            };
            let conn = py_strip(conn);
            if conn.is_empty() {
                continue;
            }
            if seen_ids.iter().any(|x| x == link_id) || seen_conns.iter().any(|x| x == conn) {
                continue;
            }
            seen_ids.push(link_id.to_string());
            seen_conns.push(conn.to_string());
            links.push(json!({"id": link_id, "connection": conn}));
            if links.len() >= MAX_EXTRA_LINKS {
                break;
            }
        }
    }
    if !links.is_empty() {
        out.insert("links".into(), Value::Array(links));
    }
    let mut vehicles = Map::new();
    if let Some(Value::Object(raw_vehicles)) = raw.get("vehicles") {
        for (key, meta) in raw_vehicles {
            if parse_vehicle_key(key).is_none() {
                continue;
            }
            let Some(meta) = meta.as_object() else { continue };
            let mut entry = Map::new();
            let name = clean_name(meta.get("name").unwrap_or(&Value::Null));
            let color = clean_color(meta.get("color").unwrap_or(&Value::Null));
            if !name.is_empty() {
                entry.insert("name".into(), json!(name));
            }
            if !color.is_empty() {
                entry.insert("color".into(), json!(color));
            }
            if !entry.is_empty() {
                vehicles.insert(key.clone(), Value::Object(entry));
            }
            if vehicles.len() >= MAX_REMEMBERED {
                break;
            }
        }
    }
    if !vehicles.is_empty() {
        out.insert("vehicles".into(), Value::Object(vehicles));
    }
    if let Some(theme) = raw.get("theme_vehicle") {
        if parse_key_value(theme).is_some() {
            out.insert("theme_vehicle".into(), theme.clone());
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

// ------------------------------------------------------------- the tracker

#[derive(Debug, Clone)]
struct Entry {
    sysid: i64,
    component: i64,
    last_heartbeat: f64,
    type_name: String,
    type_id: i64,
    stack: String,
    mode: String,
    mode_label: String,
    armed: bool,
    system_status: i64,
    position: Option<[f64; 2]>,
    altitude_rel: Option<f64>,
    altitude_amsl: Option<f64>,
    heading: Option<f64>,
    groundspeed: Option<f64>,
    battery: Option<i64>,
    voltage: Option<f64>,
}

impl Entry {
    fn new(system: i64, component: i64, stamp: f64) -> Entry {
        Entry {
            sysid: system,
            component,
            last_heartbeat: stamp,
            type_name: String::new(),
            type_id: 0,
            stack: String::new(),
            mode: String::new(),
            mode_label: String::new(),
            armed: false,
            system_status: 0,
            position: None,
            altitude_rel: None,
            altitude_amsl: None,
            heading: None,
            groundspeed: None,
            battery: None,
            voltage: None,
        }
    }

    fn to_json(&self, stale: bool) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("sysid".into(), json!(self.sysid));
        m.insert("type".into(), json!(self.type_name));
        m.insert("type_id".into(), json!(self.type_id));
        m.insert("stack".into(), json!(self.stack));
        m.insert("mode".into(), json!(self.mode));
        m.insert("mode_label".into(), json!(self.mode_label));
        m.insert("armed".into(), json!(self.armed));
        m.insert("system_status".into(), json!(self.system_status));
        m.insert(
            "position".into(),
            self.position.map_or(Value::Null, |p| json!([p[0], p[1]])),
        );
        m.insert("altitude_rel".into(), json!(self.altitude_rel));
        m.insert("altitude_amsl".into(), json!(self.altitude_amsl));
        m.insert("heading".into(), json!(self.heading));
        m.insert("groundspeed".into(), json!(self.groundspeed));
        m.insert("battery".into(), json!(self.battery));
        m.insert("voltage".into(), json!(self.voltage));
        m.insert("stale".into(), json!(stale));
        m
    }
}

#[derive(Default)]
struct TrackerState {
    system: Option<i64>,
    component: i64,
    entry: Option<Entry>,
    events: Vec<(u64, i64, i64, String)>,
    event_seq: u64,
}

/// What the one autopilot a link flies last said about itself. Passive:
/// written by the receive thread, read by the fleet publisher.
pub struct FleetTracker {
    state: Mutex<TrackerState>,
    clock: Arc<dyn Clock>,
}

impl Default for FleetTracker {
    fn default() -> Self {
        FleetTracker::new()
    }
}

fn num(msg: &crate::link::plain::Message, name: &str) -> Option<f64> {
    msg.get(name).and_then(|v| v.as_f64()).filter(|x| x.is_finite())
}

impl FleetTracker {
    pub fn new() -> FleetTracker {
        FleetTracker::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> FleetTracker {
        FleetTracker {
            state: Mutex::new(TrackerState::default()),
            clock,
        }
    }

    fn lock(&self) -> MutexGuard<'_, TrackerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tracks `system` from now on, and only it. Another system forgets the last.
    pub fn bind(&self, system: i64, component: i64) {
        if !(1..=255).contains(&system) {
            return;
        }
        let mut st = self.lock();
        if st.system != Some(system) {
            st.entry = None;
        }
        st.system = Some(system);
        st.component = component;
        if component != 0 {
            if let Some(entry) = st.entry.as_mut() {
                entry.component = component;
            }
        }
    }

    pub fn bound_system(&self) -> Option<i64> {
        self.lock().system
    }

    /// Takes one received frame. `vehicle_heartbeat` is the bridge's verdict
    /// that a HEARTBEAT is an autopilot's and not a gimbal's or a station's.
    pub fn observe(&self, frame: &Frame, vehicle_heartbeat: bool, now: Option<f64>) {
        let Payload::Message(msg) = &frame.payload else { return };
        let name = msg.name();
        let stamp = now.unwrap_or_else(|| self.clock.monotonic());
        let (system, component) = (frame.system, frame.component);
        let mut st = self.lock();
        if st.system != Some(system) {
            return;
        }
        if name == "HEARTBEAT" {
            if !vehicle_heartbeat {
                return;
            }
            if st.component != 0 && component != 0 && component != st.component {
                return;
            }
            let bound_component = st.component;
            let entry = st.entry.get_or_insert_with(|| {
                Entry::new(system, if component != 0 { component } else { bound_component }, stamp)
            });
            take_heartbeat(entry, msg, stamp);
            return;
        }
        let Some(entry) = st.entry.as_mut() else { return };
        if entry.component != 0 && component != entry.component {
            return;
        }
        match name {
            "GLOBAL_POSITION_INT" => {
                let (Some(lat), Some(lon)) = (num(msg, "lat"), num(msg, "lon")) else {
                    return;
                };
                if lat == 0.0 && lon == 0.0 {
                    return;
                }
                if !((-90e7..=90e7).contains(&lat) && (-180e7..=180e7).contains(&lon)) {
                    return;
                }
                entry.position = Some([py_round(lon / 1e7, 7), py_round(lat / 1e7, 7)]);
                entry.altitude_rel = num(msg, "relative_alt").map(|x| py_round(x / 1000.0, 1));
                entry.altitude_amsl = num(msg, "alt").map(|x| py_round(x / 1000.0, 1));
                if let Some(hdg) = msg.get("hdg").and_then(|v| v.as_f64()) {
                    if hdg != 65535.0 {
                        entry.heading = Some(py_round(hdg / 100.0, 1));
                    }
                }
                let vx = num(msg, "vx").unwrap_or(0.0);
                let vy = num(msg, "vy").unwrap_or(0.0);
                entry.groundspeed = Some(py_round(vx.hypot(vy) / 100.0, 1));
            }
            "VFR_HUD" => {
                if let Some(heading) = num(msg, "heading") {
                    entry.heading = Some(heading);
                }
                if let Some(speed) = num(msg, "groundspeed") {
                    entry.groundspeed = Some(py_round(speed, 1));
                }
            }
            "ATTITUDE" if entry.heading.is_none() => {
                if let Some(yaw) = num(msg, "yaw") {
                    entry.heading = Some(py_round(yaw.to_degrees().rem_euclid(360.0), 1));
                }
            }
            "STATUSTEXT" => {
                let sysid = entry.sysid;
                take_statustext(&mut st, sysid, msg);
            }
            "SYS_STATUS" => {
                if let Some(remaining) = msg.get("battery_remaining").and_then(|v| v.as_i64()) {
                    if (0..=100).contains(&remaining) {
                        entry.battery = Some(remaining);
                    }
                }
                if let Some(voltage) = msg.get("voltage_battery").and_then(|v| v.as_i64()) {
                    if voltage > 0 && voltage != 65535 {
                        entry.voltage = Some(py_round(voltage as f64 / 1000.0, 1));
                    }
                }
            }
            _ => {}
        }
    }

    /// `(last seq, [(system, severity, text), ...])` after `seq`.
    pub fn events_since(&self, seq: u64) -> (u64, Vec<(i64, i64, String)>) {
        let st = self.lock();
        let fresh = st
            .events
            .iter()
            .filter(|e| e.0 > seq)
            .map(|e| (e.1, e.2, e.3.clone()))
            .collect();
        (st.event_seq, fresh)
    }

    /// The vehicle, as a list of none or one plain object.
    pub fn systems(&self, now: Option<f64>) -> Vec<Map<String, Value>> {
        let stamp = now.unwrap_or_else(|| self.clock.monotonic());
        let mut st = self.lock();
        let Some(entry) = st.entry.as_ref() else {
            return Vec::new();
        };
        let age = stamp - entry.last_heartbeat;
        if age > FORGET_S {
            st.entry = None;
            return Vec::new();
        }
        vec![entry.to_json(age > STALE_S)]
    }

    /// Drops the vehicle and the binding: the link flies nothing now.
    pub fn forget(&self) {
        let mut st = self.lock();
        st.entry = None;
        st.system = None;
        st.component = 0;
    }
}

fn take_heartbeat(entry: &mut Entry, msg: &crate::link::plain::Message, stamp: f64) {
    entry.last_heartbeat = stamp;
    let type_id = msg.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
    entry.type_id = type_id;
    entry.type_name = mav_type_name(type_id);
    let autopilot = msg.get("autopilot").and_then(|v| v.as_i64()).unwrap_or(0);
    let base_mode = msg.get("base_mode").and_then(|v| v.as_i64()).unwrap_or(0);
    entry.armed = base_mode & 128 != 0;
    entry.system_status = msg.get("system_status").and_then(|v| v.as_i64()).unwrap_or(0);
    let dialect = dialect_for_id(autopilot);
    let custom_mode = msg.get("custom_mode").and_then(|v| v.as_i64()).unwrap_or(0);
    let mode = dialect.decode_mode(custom_mode, base_mode, type_id);
    entry.stack = dialect.stack().as_str().to_string();
    entry.mode_label = if mode.is_empty() {
        String::new()
    } else {
        dialect.mode_label(&mode)
    };
    entry.mode = mode;
}

fn take_statustext(st: &mut TrackerState, sysid: i64, msg: &crate::link::plain::Message) {
    let severity = msg.get("severity").and_then(|v| v.as_i64()).unwrap_or(6);
    if severity > EVENT_MAX_SEVERITY {
        return;
    }
    let raw = msg.get("text").and_then(|v| v.as_text()).unwrap_or_default();
    let text = py_collapse(&raw);
    if text.is_empty() {
        return;
    }
    st.event_seq += 1;
    let seq = st.event_seq;
    st.events.push((seq, sysid, severity, text));
    if st.events.len() > MAX_EVENTS {
        let extra = st.events.len() - MAX_EVENTS;
        st.events.drain(..extra);
    }
}

// ---------------------------------------------------------------- the fleet

/// What the fleet needs of one link's bridge.
pub trait FleetBridge: Send + Sync {
    fn fleet_tracker(&self) -> &FleetTracker;
    fn connection_string(&self) -> String;
    /// The system this link flies, once latched.
    fn controlled_system(&self) -> Option<i64>;
    /// Refuses a connection string this machine cannot open (`Err(why)`).
    fn validate_connection(&self, connection: &str) -> Result<(), String>;
    /// The serial device of the link, "" for a network link.
    fn serial_device(&self) -> String;
    fn start(&self) -> Result<(), String>;
    fn stop(&self);
    fn is_connected(&self) -> bool;
    fn inject_rtcm(&self, frame: &[u8]) -> bool;
}

/// What the fleet needs of one link's state store.
pub trait FleetStore: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn update(&self, fields: Vec<(String, Value)>);
    fn forget_vehicle(&self);
    fn shutdown(&self);
}

impl FleetStore for crate::store::VehicleStateStore {
    fn get(&self, key: &str) -> Option<Value> {
        crate::store::VehicleStateStore::get(self, key)
    }

    fn update(&self, fields: Vec<(String, Value)>) {
        crate::store::VehicleStateStore::update(self, fields);
    }

    fn forget_vehicle(&self) {
        crate::store::VehicleStateStore::forget_vehicle(self);
    }

    fn shutdown(&self) {
        crate::store::VehicleStateStore::shutdown(self);
    }
}

/// One MAVLink link: its bridge, its store, and what the operator calls it.
pub struct FleetLink {
    pub id: String,
    pub bridge: Arc<dyn FleetBridge>,
    pub store: Arc<dyn FleetStore>,
    pub primary: bool,
}

pub type BridgeFactory =
    Box<dyn Fn(&str, Arc<dyn FleetStore>, &str) -> Result<Arc<dyn FleetBridge>, String> + Send + Sync>;
pub type StoreFactory = Box<dyn Fn() -> Arc<dyn FleetStore> + Send + Sync>;
pub type Persist = Box<dyn Fn(Option<Value>) + Send + Sync>;
pub type FleetListener = Arc<dyn Fn(&Value) + Send + Sync>;
pub type SwitchHook = Box<dyn Fn(&Arc<FleetLink>) + Send + Sync>;

#[derive(Debug, Clone, Default, PartialEq)]
struct Meta {
    name: Option<String>,
    color: Option<String>,
}

impl Meta {
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.color.is_none()
    }

    fn to_json(&self) -> Value {
        let mut m = Map::new();
        if let Some(name) = &self.name {
            m.insert("name".into(), json!(name));
        }
        if let Some(color) = &self.color {
            m.insert("color".into(), json!(color));
        }
        Value::Object(m)
    }
}

#[derive(Default)]
struct State {
    links: Vec<Arc<FleetLink>>,
    active: String,
    meta: Vec<(String, Meta)>,
    theme_key: String,
    pending_links: Vec<(String, String)>,
}

impl State {
    fn meta_get(&self, key: &str) -> Option<&Meta> {
        self.meta.iter().find(|(k, _)| k == key).map(|(_, m)| m)
    }

    fn meta_pop(&mut self, key: &str) {
        self.meta.retain(|(k, _)| k != key);
    }

    fn meta_entry(&mut self, key: &str) -> &mut Meta {
        if let Some(i) = self.meta.iter().position(|(k, _)| k == key) {
            return &mut self.meta[i].1;
        }
        self.meta.push((key.to_string(), Meta::default()));
        &mut self.meta.last_mut().expect("just pushed").1
    }

    fn trim_meta(&mut self) {
        while self.meta.len() > MAX_REMEMBERED {
            let victim = self.meta.iter().position(|(_, m)| m.name.is_none()).unwrap_or(0);
            self.meta.remove(victim);
        }
    }
}

#[derive(Default)]
struct Notices {
    list: Vec<Value>,
    seq: u64,
    cursor: HashMap<String, u64>,
    last: HashMap<(String, String), f64>,
    was_live: HashMap<String, bool>,
    last_published: String,
}

/// Every link, every vehicle on them, and which one is being flown.
pub struct Fleet {
    state: Mutex<State>,
    add_lock: Mutex<()>,
    notices: Mutex<Notices>,
    publish_lock: Mutex<()>,
    listeners: Mutex<Vec<(u64, FleetListener)>>,
    next_listener: Mutex<u64>,
    clock: Arc<dyn Clock>,
    created: f64,
    bridge_factory: Option<BridgeFactory>,
    store_factory: Option<StoreFactory>,
    persist: Option<Persist>,
    switch_hook: Mutex<Option<SwitchHook>>,
    stopping: Mutex<bool>,
    wake: Condvar,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn link_label(link_id: &str) -> String {
    if link_id == MAIN_LINK {
        return "Main link".into();
    }
    format!("Link {}", link_id.split_once('-').map_or("", |(_, n)| n))
}

impl Fleet {
    pub fn new(main_bridge: Arc<dyn FleetBridge>, main_store: Arc<dyn FleetStore>) -> Fleet {
        Fleet::with_clock(main_bridge, main_store, Arc::new(SystemClock))
    }

    pub fn with_clock(
        main_bridge: Arc<dyn FleetBridge>,
        main_store: Arc<dyn FleetStore>,
        clock: Arc<dyn Clock>,
    ) -> Fleet {
        let main = Arc::new(FleetLink {
            id: MAIN_LINK.into(),
            bridge: main_bridge,
            store: main_store,
            primary: true,
        });
        let created = clock.monotonic();
        Fleet {
            state: Mutex::new(State {
                links: vec![main],
                active: MAIN_LINK.into(),
                ..State::default()
            }),
            add_lock: Mutex::new(()),
            notices: Mutex::new(Notices::default()),
            publish_lock: Mutex::new(()),
            listeners: Mutex::new(Vec::new()),
            next_listener: Mutex::new(0),
            clock,
            created,
            bridge_factory: None,
            store_factory: None,
            persist: None,
            switch_hook: Mutex::new(None),
            stopping: Mutex::new(false),
            wake: Condvar::new(),
            thread: Mutex::new(None),
        }
    }

    /// How extra links are made. Without it `add_link` answers "could not open".
    pub fn with_factories(mut self, bridge: BridgeFactory, store: StoreFactory) -> Fleet {
        self.bridge_factory = Some(bridge);
        self.store_factory = Some(store);
        self
    }

    pub fn with_persist(mut self, persist: Persist) -> Fleet {
        self.persist = Some(persist);
        self
    }

    /// Called once with the new link whenever another link is flown.
    pub fn set_switch_hook(&self, hook: SwitchHook) {
        *lock(&self.switch_hook) = Some(hook);
    }

    // ---- configuration --------------------------------------------------

    /// Takes the `fleet` config block: names, colours, and extra links. The
    /// links are only recorded here; `start` opens them.
    pub fn load(&self, block: &Value) {
        let clean = coerce_config(block).unwrap_or_else(|| json!({}));
        let mut st = lock(&self.state);
        st.meta = clean
            .get("vehicles")
            .and_then(Value::as_object)
            .map(|vehicles| {
                vehicles
                    .iter()
                    .map(|(k, v)| {
                        let meta = Meta {
                            name: v.get("name").and_then(Value::as_str).map(String::from),
                            color: v.get("color").and_then(Value::as_str).map(String::from),
                        };
                        (k.clone(), meta)
                    })
                    .collect()
            })
            .unwrap_or_default();
        st.theme_key = clean
            .get("theme_vehicle")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        st.pending_links = clean
            .get("links")
            .and_then(Value::as_array)
            .map(|links| {
                links
                    .iter()
                    .map(|l| {
                        (
                            l["id"].as_str().unwrap_or("").to_string(),
                            l["connection"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    }

    /// The `fleet` config block as it stands now, or None for nothing.
    pub fn to_config(&self) -> Option<Value> {
        let (links, meta, theme) = {
            let st = lock(&self.state);
            let links: Vec<Value> = st
                .links
                .iter()
                .filter(|l| !l.primary)
                .map(|l| json!({"id": l.id, "connection": l.bridge.connection_string()}))
                .collect();
            let mut meta = Map::new();
            for (k, v) in &st.meta {
                if !v.is_empty() {
                    meta.insert(k.clone(), v.to_json());
                }
            }
            (links, meta, st.theme_key.clone())
        };
        coerce_config(&json!({"links": links, "vehicles": meta, "theme_vehicle": theme}))
    }

    fn save(&self) {
        if let Some(persist) = &self.persist {
            let config = self.to_config();
            let _ = catch_unwind(AssertUnwindSafe(|| persist(config)));
        }
    }

    // ---- lifecycle ------------------------------------------------------

    /// Opens the configured extra links and starts publishing.
    pub fn start(self: &Arc<Self>) {
        let pending = std::mem::take(&mut lock(&self.state).pending_links);
        for (id, connection) in pending {
            let _ = self.open_link(&id, &connection);
        }
        *lock(&self.stopping) = false;
        let mut thread = lock(&self.thread);
        if thread.as_ref().is_none_or(|h| h.is_finished()) {
            let fleet = Arc::clone(self);
            *thread = std::thread::Builder::new()
                .name("fleet-publish".into())
                .spawn(move || fleet.publish_loop())
                .ok();
        }
    }

    /// Stops publishing and closes every extra link. The main one is not ours.
    pub fn stop(&self) {
        *lock(&self.stopping) = true;
        self.wake.notify_all();
        if let Some(handle) = lock(&self.thread).take() {
            let _ = handle.join();
        }
        let extras: Vec<Arc<FleetLink>> = lock(&self.state).links.iter().filter(|l| !l.primary).cloned().collect();
        for link in extras {
            Self::close(&link);
        }
    }

    fn close(link: &FleetLink) {
        let _ = catch_unwind(AssertUnwindSafe(|| link.bridge.stop()));
        let _ = catch_unwind(AssertUnwindSafe(|| link.store.shutdown()));
    }

    fn publish_loop(&self) {
        loop {
            {
                let guard = lock(&self.stopping);
                let (guard, _) = self
                    .wake
                    .wait_timeout_while(guard, Duration::from_secs_f64(PUBLISH_INTERVAL_S), |stop| !*stop)
                    .unwrap_or_else(|e| e.into_inner());
                if *guard {
                    return;
                }
            }
            self.publish(false);
        }
    }

    // ---- links ----------------------------------------------------------

    pub fn extra_bridges(&self) -> Vec<Arc<dyn FleetBridge>> {
        lock(&self.state)
            .links
            .iter()
            .filter(|l| !l.primary)
            .map(|l| Arc::clone(&l.bridge))
            .collect()
    }

    pub fn link(&self, link_id: &str) -> Option<Arc<FleetLink>> {
        lock(&self.state).links.iter().find(|l| l.id == link_id).cloned()
    }

    pub fn active_link(&self) -> Arc<FleetLink> {
        let st = lock(&self.state);
        st.links
            .iter()
            .find(|l| l.id == st.active)
            .cloned()
            .expect("the active link exists")
    }

    pub fn active_bridge(&self) -> Arc<dyn FleetBridge> {
        Arc::clone(&self.active_link().bridge)
    }

    pub fn active_store(&self) -> Arc<dyn FleetStore> {
        Arc::clone(&self.active_link().store)
    }

    pub fn active_is_main(&self) -> bool {
        lock(&self.state).active == MAIN_LINK
    }

    fn next_link_id(st: &State) -> String {
        let mut n = 2;
        while st.links.iter().any(|l| l.id == format!("link-{n}")) {
            n += 1;
        }
        format!("link-{n}")
    }

    fn open_link(&self, link_id: &str, connection: &str) -> Result<Arc<FleetLink>, String> {
        let (Some(bridge_factory), Some(store_factory)) = (&self.bridge_factory, &self.store_factory) else {
            return Err("extra links are not available".into());
        };
        let store = store_factory();
        let bridge = bridge_factory(link_id, Arc::clone(&store), connection)?;
        let link = Arc::new(FleetLink {
            id: link_id.into(),
            bridge,
            store,
            primary: false,
        });
        {
            let mut st = lock(&self.state);
            st.links.retain(|l| l.id != link_id);
            st.links.push(Arc::clone(&link));
        }
        link.bridge.start()?;
        Ok(link)
    }

    /// Opens another link. Returns `(Some(link id), "")` or `(None, why)`.
    pub fn add_link(&self, connection: &str) -> (Option<String>, String) {
        let connection = py_strip(connection);
        if connection.is_empty() {
            return (None, "connection must be a non-empty string".into());
        }
        let _one_at_a_time = lock(&self.add_lock);
        let link_id = {
            let (extra, main) = {
                let st = lock(&self.state);
                (st.links.len() - 1, st.links.iter().find(|l| l.id == MAIN_LINK).cloned())
            };
            if extra >= MAX_EXTRA_LINKS {
                return (None, format!("at most {MAX_EXTRA_LINKS} extra links"));
            }
            let in_use = self.connection_in_use(connection, "");
            if !in_use.is_empty() {
                return (None, format!("{connection} is already open as {in_use}"));
            }
            if let Some(main) = main {
                if let Err(why) = main.bridge.validate_connection(connection) {
                    return (None, why);
                }
            }
            Self::next_link_id(&lock(&self.state))
        };
        if let Err(why) = self.open_link(&link_id, connection) {
            return (None, format!("could not open the link: {why}"));
        }
        self.withdraw_suggestion(connection);
        self.save();
        self.publish_now();
        (Some(link_id), String::new())
    }

    /// The device auto-connect offered is now a link of its own: stop offering it.
    fn withdraw_suggestion(&self, connection: &str) {
        if !connection.starts_with("serial:") {
            return;
        }
        let Some(main) = self.link(MAIN_LINK) else { return };
        let offer = main.store.get("link_suggestion").unwrap_or(Value::Null);
        let offered = offer.get("connection_string").and_then(Value::as_str).unwrap_or("");
        if offered.starts_with("serial:")
            && same_port(
                &crate::bridge::split_serial_connection(offered).0,
                &crate::bridge::split_serial_connection(connection).0,
            )
            && offer.is_object()
        {
            main.store.update(vec![("link_suggestion".into(), Value::Null)]);
        }
    }

    /// Closes an extra link and forgets the vehicle on it. Returns "" or why not.
    pub fn remove_link(&self, link_id: &str) -> String {
        let (link, was_active) = {
            let mut st = lock(&self.state);
            let Some(index) = st.links.iter().position(|l| l.id == link_id) else {
                return "no such link".into();
            };
            if st.links[index].primary {
                return "the main link is closed from the LINK tab".into();
            }
            let link = st.links.remove(index);
            Self::forget_link_meta(&mut st, link_id);
            (link, st.active == link_id)
        };
        lock(&self.notices).cursor.remove(link_id);
        {
            let mut notices = lock(&self.notices);
            let prefix = format!("{link_id}:");
            notices.was_live.retain(|k, _| !k.starts_with(&prefix));
        }
        if was_active {
            let fallback = self.fallback_link("");
            self.switch_link(&fallback);
        }
        Self::close(&link);
        self.save();
        self.publish_now();
        String::new()
    }

    /// Takes a vehicle off the list: closes its link, so nothing of it is
    /// shown. The main link is disconnected and left closed. An armed vehicle
    /// that is still heard is only removed with `confirm_armed`. Returns
    /// `("", 200)` or `(why not, HTTP status)`.
    pub fn remove_vehicle(&self, key: &str, confirm_armed: bool) -> (String, u16) {
        let Some((link_id, system_id)) = parse_vehicle_key(key) else {
            return ("unknown vehicle".into(), 400);
        };
        let Some(link) = self.link(&link_id) else {
            return ("that vehicle's link is not open".into(), 404);
        };
        let Some(entry) = Self::entry_of(&link, system_id) else {
            return ("that vehicle is not on the list".into(), 404);
        };
        let armed = entry.get("armed").and_then(Value::as_bool).unwrap_or(false);
        let stale = entry.get("stale").and_then(Value::as_bool).unwrap_or(false);
        if armed && !stale && !confirm_armed {
            return (
                "The vehicle is armed. Removing it closes its link: it keeps flying and the station no longer sees it."
                    .into(),
                409,
            );
        }
        if !link.primary {
            let error = self.remove_link(&link_id);
            return if error.is_empty() {
                (String::new(), 200)
            } else {
                (error, 400)
            };
        }
        let _ = catch_unwind(AssertUnwindSafe(|| link.bridge.stop()));
        link.bridge.fleet_tracker().forget();
        link.store.forget_vehicle();
        link.store.update(vec![("vehicle_id".into(), json!(""))]);
        let was_active = {
            let mut st = lock(&self.state);
            Self::forget_key(&mut st, &vehicle_key(&link_id, system_id));
            st.active == link_id
        };
        lock(&self.notices).was_live.remove(&vehicle_key(&link_id, system_id));
        if was_active {
            let fallback = self.fallback_link(&link_id);
            self.switch_link(&fallback);
        }
        self.save();
        self.publish_now();
        (String::new(), 200)
    }

    fn entry_of(link: &FleetLink, system_id: i64) -> Option<Map<String, Value>> {
        link.bridge
            .fleet_tracker()
            .systems(None)
            .into_iter()
            .find(|e| e.get("sysid").and_then(Value::as_i64) == Some(system_id))
    }

    /// The link to fly once the flown one is gone: one with a live vehicle.
    fn fallback_link(&self, exclude: &str) -> String {
        let candidates: Vec<Arc<FleetLink>> = lock(&self.state)
            .links
            .iter()
            .filter(|l| l.id != exclude)
            .cloned()
            .collect();
        for link in candidates {
            if link.bridge.controlled_system().is_some() {
                return link.id.clone();
            }
        }
        MAIN_LINK.into()
    }

    fn forget_key(st: &mut State, key: &str) {
        st.meta_pop(key);
        if st.theme_key == key {
            st.theme_key.clear();
        }
    }

    /// Names and colours of a closed link's vehicles: the next link of that id
    /// may carry another aircraft, which must not inherit them.
    fn forget_link_meta(st: &mut State, link_id: &str) {
        let prefix = format!("{link_id}:");
        let keys: Vec<String> = st
            .meta
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            Self::forget_key(st, &key);
        }
        if st.theme_key.starts_with(&prefix) {
            st.theme_key.clear();
        }
    }

    /// The label of another link already open on `connection`, or "". A serial
    /// port is the same port at any baud rate.
    pub fn connection_in_use(&self, connection: &str, except_link: &str) -> String {
        let device = if connection.starts_with("serial:") {
            crate::bridge::split_serial_connection(connection).0
        } else {
            String::new()
        };
        let links: Vec<Arc<FleetLink>> = lock(&self.state).links.clone();
        for link in links {
            if link.id == except_link {
                continue;
            }
            let other = link.bridge.connection_string();
            if other == connection {
                return link_label(&link.id);
            }
            if !device.is_empty()
                && other.starts_with("serial:")
                && same_port(&device, &crate::bridge::split_serial_connection(&other).0)
            {
                return link_label(&link.id);
            }
        }
        String::new()
    }

    /// An extra link was pointed somewhere else from the LINK tab: save it.
    pub fn link_changed(&self) {
        self.save();
        self.publish_now();
    }

    /// The serial ports the extra links hold, for auto-connect to leave alone.
    pub fn extra_serial_devices(&self) -> Vec<String> {
        self.extra_bridges()
            .iter()
            .map(|b| b.serial_device())
            .filter(|d| !d.is_empty())
            .collect()
    }

    /// RTK corrections are for every aircraft in range: the main bridge's
    /// answer, and a copy to every connected extra link.
    pub fn inject_rtcm(&self, frame: &[u8]) -> bool {
        let ok = self.link(MAIN_LINK).is_some_and(|l| l.bridge.inject_rtcm(frame));
        for bridge in self.extra_bridges() {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if bridge.is_connected() {
                    bridge.inject_rtcm(frame);
                }
            }));
        }
        ok
    }

    /// Whether any link is connected.
    pub fn is_connected(&self) -> bool {
        self.link(MAIN_LINK).is_some_and(|l| l.bridge.is_connected())
            || self.extra_bridges().iter().any(|b| b.is_connected())
    }

    // ---- vehicles -------------------------------------------------------

    /// Names or colours a vehicle. `None` leaves a field as it is.
    pub fn set_meta(&self, key: &str, name: Option<&Value>, color: Option<&Value>) -> String {
        if parse_vehicle_key(key).is_none() {
            return "unknown vehicle".into();
        }
        let (name, color) = (name.filter(|v| !v.is_null()), color.filter(|v| !v.is_null()));
        {
            let mut st = lock(&self.state);
            let mut entry = st.meta_get(key).cloned().unwrap_or_default();
            if let Some(name) = name {
                let cleaned = clean_name(name);
                entry.name = (!cleaned.is_empty()).then_some(cleaned);
            }
            if let Some(color) = color {
                let cleaned = clean_color(color);
                if truthy(color) && cleaned.is_empty() {
                    return "colour must be #RRGGBB".into();
                }
                entry.color = (!cleaned.is_empty()).then_some(cleaned);
            }
            if entry.is_empty() {
                st.meta_pop(key);
            } else {
                *st.meta_entry(key) = entry;
            }
            st.trim_meta();
        }
        self.save();
        self.publish_now();
        String::new()
    }

    /// Flies the vehicle `key` from now on. Returns "" or why not.
    pub fn select(&self, key: &str) -> String {
        let Some((link_id, system_id)) = parse_vehicle_key(key) else {
            return "unknown vehicle".into();
        };
        let Some(link) = self.link(&link_id) else {
            return "that vehicle's link is not open".into();
        };
        if Self::entry_of(&link, system_id).is_none() {
            return "that vehicle is not on the list".into();
        }
        if let Some(current) = link.bridge.controlled_system() {
            if current != system_id {
                return format!("this link flies system {current}");
            }
        }
        self.switch_link(&link_id);
        self.publish_now();
        String::new()
    }

    fn switch_link(&self, link_id: &str) {
        let link = {
            let mut st = lock(&self.state);
            if st.active == link_id {
                return;
            }
            let Some(link) = st.links.iter().find(|l| l.id == link_id).cloned() else {
                return;
            };
            st.active = link_id.to_string();
            link
        };
        if let Some(hook) = lock(&self.switch_hook).as_ref() {
            hook(&link);
        }
    }

    pub fn default_name(link_id: &str, system_id: i64) -> String {
        if link_id == MAIN_LINK {
            format!("Vehicle {system_id}")
        } else {
            format!("Vehicle {system_id} ({})", link_label(link_id))
        }
    }

    /// The vehicle list and the links, as the interface receives them.
    pub fn snapshot(&self) -> Value {
        let (links, active_link) = {
            let st = lock(&self.state);
            (st.links.clone(), st.active.clone())
        };
        let mut vehicles: Vec<Map<String, Value>> = Vec::new();
        let mut link_rows: Vec<Value> = Vec::new();
        let mut active_key = String::new();
        for link in &links {
            let controlled = link.bridge.controlled_system();
            let systems = link.bridge.fleet_tracker().systems(None);
            if link.id == active_link {
                let chosen = controlled.or_else(|| systems.first().and_then(|s| s["sysid"].as_i64()));
                if let Some(chosen) = chosen.filter(|c| *c != 0) {
                    active_key = vehicle_key(&link.id, chosen);
                }
            }
            link_rows.push(json!({
                "id": link.id,
                "label": link_label(&link.id),
                "primary": link.primary,
                "connection": link.bridge.connection_string(),
                "status": link.store.get("link_status").unwrap_or_else(|| json!("disconnected")),
                "error": link.store.get("link_error").unwrap_or_else(|| json!("")),
                "vehicles": systems.len(),
            }));
            for entry in systems {
                let sysid = entry["sysid"].as_i64().unwrap_or(0);
                let mut row = entry;
                row.insert("id".into(), json!(vehicle_key(&link.id, sysid)));
                row.insert("link".into(), json!(link.id));
                row.insert("link_label".into(), json!(link_label(&link.id)));
                row.insert("controlled".into(), json!(controlled == Some(sysid)));
                vehicles.push(row);
            }
        }
        self.assign_colors(&vehicles, &active_key);
        let meta: HashMap<String, Meta> = lock(&self.state).meta.iter().cloned().collect();
        for row in &mut vehicles {
            let id = row["id"].as_str().unwrap_or("").to_string();
            let entry = meta.get(&id).cloned().unwrap_or_default();
            let name = entry.name.clone().unwrap_or_else(|| {
                Self::default_name(row["link"].as_str().unwrap_or(""), row["sysid"].as_i64().unwrap_or(0))
            });
            row.insert("name".into(), json!(name));
            row.insert("custom_name".into(), json!(entry.name.is_some()));
            row.insert("color".into(), json!(entry.color.unwrap_or_default()));
            row.insert("active".into(), json!(id == active_key));
        }
        let (events, event_seq) = {
            let notices = lock(&self.notices);
            let start = notices.list.len().saturating_sub(20);
            (notices.list[start..].to_vec(), notices.seq)
        };
        let suggestion = self
            .link(MAIN_LINK)
            .and_then(|l| l.store.get("link_suggestion"))
            .filter(Value::is_object)
            .unwrap_or(Value::Null);
        json!({
            "active": active_key,
            "active_link": active_link,
            "links": link_rows,
            "vehicles": vehicles,
            "palette": PALETTE,
            "suggestion": suggestion,
            "events": events,
            "event_seq": event_seq,
        })
    }

    /// Gives each vehicle that has none a colour of its own, once. Only when
    /// there is more than one to tell apart: a single aircraft keeps the
    /// theme's colour and nothing is written.
    fn assign_colors(&self, vehicles: &[Map<String, Value>], active_key: &str) {
        let live: Vec<&Map<String, Value>> = vehicles
            .iter()
            .filter(|v| !v["stale"].as_bool().unwrap_or(false))
            .collect();
        if live.len() < 2 {
            return;
        }
        if active_key.is_empty() && self.clock.monotonic() - self.created < COLOR_GRACE_S {
            return;
        }
        let mut changed = false;
        {
            let mut st = lock(&self.state);
            let keys: Vec<String> = live
                .iter()
                .map(|r| r["id"].as_str().unwrap_or("").to_string())
                .collect();
            if st.theme_key.is_empty() {
                st.theme_key = if keys.iter().any(|k| k == active_key) {
                    active_key.to_string()
                } else {
                    keys[0].clone()
                };
                changed = true;
            }
            let mut used: Vec<String> = Vec::new();
            for (_, m) in &st.meta {
                if let Some(c) = &m.color {
                    if !used.contains(c) {
                        used.push(c.clone());
                    }
                }
            }
            for key in &keys {
                if *key == st.theme_key || st.meta_get(key).is_some_and(|m| m.color.is_some()) {
                    continue;
                }
                let color = PALETTE
                    .iter()
                    .find(|c| !used.iter().any(|u| u == *c))
                    .unwrap_or(&PALETTE[used.len() % PALETTE.len()]);
                st.meta_entry(key).color = Some((*color).to_string());
                used.push((*color).to_string());
                changed = true;
            }
            if changed {
                st.trim_meta();
            }
        }
        if changed {
            self.save();
        }
    }

    // ---- publishing -----------------------------------------------------

    pub fn add_listener(&self, listener: FleetListener) -> u64 {
        let mut next = lock(&self.next_listener);
        *next += 1;
        lock(&self.listeners).push((*next, listener));
        *next
    }

    pub fn remove_listener(&self, id: u64) {
        lock(&self.listeners).retain(|(i, _)| *i != id);
    }

    pub fn publish_now(&self) {
        self.publish(true);
    }

    fn flown(&self) -> (String, Option<i64>) {
        let link = self.active_link();
        (link.id.clone(), link.bridge.controlled_system())
    }

    fn add_event(&self, key: &str, level: &str, text: &str, now: f64) {
        let mut notices = lock(&self.notices);
        let id = (key.to_string(), text.to_string());
        if let Some(last) = notices.last.get(&id) {
            if now - last < EVENT_REPEAT_S {
                return;
            }
        }
        notices.last.insert(id, now);
        if notices.last.len() > 4 * MAX_EVENTS {
            let cutoff = now - EVENT_REPEAT_S;
            notices.last.retain(|_, t| *t >= cutoff);
        }
        notices.seq += 1;
        let seq = notices.seq;
        notices
            .list
            .push(json!({"seq": seq, "vehicle": key, "level": level, "text": text}));
        if notices.list.len() > MAX_EVENTS {
            let extra = notices.list.len() - MAX_EVENTS;
            notices.list.drain(..extra);
        }
    }

    /// Turns what the vehicles not being flown said into notifications: a
    /// STATUSTEXT at warning level or worse, and a vehicle that stopped
    /// heartbeating or came back. The vehicle being flown is left out.
    pub fn collect_events(&self, now: Option<f64>) {
        let stamp = now.unwrap_or_else(|| self.clock.monotonic());
        let (flown_link, flown_system) = self.flown();
        let links: Vec<Arc<FleetLink>> = lock(&self.state).links.clone();
        for link in links {
            let tracker = link.bridge.fleet_tracker();
            let cursor = lock(&self.notices).cursor.get(&link.id).copied();
            let (last, mut fresh) = tracker.events_since(cursor.unwrap_or(1 << 62));
            lock(&self.notices).cursor.insert(link.id.clone(), last);
            if cursor.is_none() {
                fresh.clear();
            }
            for (system, severity, text) in fresh {
                if link.id == flown_link && Some(system) == flown_system {
                    continue;
                }
                let level = if severity <= 3 { "critical" } else { "warning" };
                self.add_event(&vehicle_key(&link.id, system), level, &text, stamp);
            }
            for entry in tracker.systems(None) {
                let sysid = entry["sysid"].as_i64().unwrap_or(0);
                let key = vehicle_key(&link.id, sysid);
                let live = !entry["stale"].as_bool().unwrap_or(false);
                let was = lock(&self.notices).was_live.insert(key.clone(), live);
                if was.is_none() || was == Some(live) {
                    continue;
                }
                if link.id == flown_link && Some(sysid) == flown_system {
                    continue;
                }
                if live {
                    self.add_event(&key, "info", "heard again", stamp);
                } else {
                    self.add_event(&key, "critical", "no heartbeat for 5 s, link lost", stamp);
                }
            }
        }
    }

    /// One publishing round: collect notifications, then tell the listeners
    /// when the list changed (or always, with `force`).
    pub fn publish(&self, force: bool) {
        let _one_at_a_time = lock(&self.publish_lock);
        let _ = catch_unwind(AssertUnwindSafe(|| self.collect_events(None)));
        let listeners: Vec<FleetListener> = lock(&self.listeners).iter().map(|(_, f)| Arc::clone(f)).collect();
        if listeners.is_empty() && !force {
            return;
        }
        let snapshot = self.snapshot();
        let text = snapshot.to_string();
        {
            let mut notices = lock(&self.notices);
            if !force && text == notices.last_published {
                return;
            }
            notices.last_published = text;
        }
        for listener in listeners {
            let _ = catch_unwind(AssertUnwindSafe(|| listener(&snapshot)));
        }
    }
}
