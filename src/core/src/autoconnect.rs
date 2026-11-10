//! Smart default MAVLink auto connect (`autoconnect.py`).
//!
//! The priority is USB first, then a SiK radio, then the UDP port a simulator
//! or a router publishes on: command line, direct USB flight controller, SiK
//! radio, the configured string, the UDP fallback, the built in default.
//!
//! Two rules shape everything else. **Never steal an active link**: the
//! watcher acts only while the bridge is disconnected or reconnecting with no
//! fresh heartbeat, and a device plugged in mid session is published as a
//! suggestion rather than dialled. **Manual always wins the session**: a
//! manual connect sets `manual_override` and nothing dials again until the
//! process restarts. One case is not a choice at all: the device the link is
//! on came back under another name, and the link follows it by its USB
//! identity.
//!
//! Everything that decides is a pure function over an already enumerated port
//! list. The watcher is the one moving part: a thread, a condition variable
//! for the sleep, no sockets of its own, and no serial port is ever opened.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::serial_ports::{
    classify_serial_device, is_bootloader_port, is_phantom_device, port_present, same_port, split_serial_connection,
    usb_identity, PortRow, DEFAULT_SERIAL_BAUD,
};

pub const REASON_CLI: &str = "cli";
pub const REASON_USB: &str = "usb-direct";
pub const REASON_SIK: &str = "sik";
pub const REASON_CONFIGURED: &str = "configured";
pub const REASON_UDP_FALLBACK: &str = "udp-fallback";
pub const REASON_DEFAULT_FALLBACK: &str = "default-invalid-fallback";
pub const REASON_MANUAL: &str = "manual";
pub const REASON_NO_CANDIDATE: &str = "no-candidate";
pub const REASON_BOOTLOADER_SKIP: &str = "bootloader-skip";
pub const REASON_NEW_DEVICE: &str = "new-device-while-connected";

/// 0.0.0.0 rather than 127.0.0.1: the superset bind accepts a simulator on
/// loopback and one on a bridged interface.
pub const UDP_FALLBACK_CONNECTION: &str = "udp:0.0.0.0:14550";
pub const STALE_AFTER_S: f64 = 8.0;
pub const DEFAULT_POLL_S: f64 = 2.5;

/// One enumerated port, classified.
#[derive(Debug, Clone, PartialEq)]
pub struct PortInfo {
    pub device: String,
    pub description: String,
    pub hwid: String,
    pub kind: String,
}

impl PortInfo {
    pub fn from_row(row: &PortRow) -> PortInfo {
        PortInfo {
            device: row.device.clone(),
            description: row.description.clone(),
            hwid: row.hwid.clone(),
            kind: classify_serial_device(&row.device, &row.hwid, &row.description).to_string(),
        }
    }
}

/// What the resolver chose at startup, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupDecision {
    pub connection_string: String,
    pub reason: &'static str,
    pub detail: String,
}

/// A device that appeared while the link was busy elsewhere. Display only.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub connection_string: String,
    pub kind: &'static str,
    pub device: String,
}

impl Suggestion {
    /// The shape published in the store as `link_suggestion`.
    pub fn as_value(&self, ts: f64) -> Value {
        json!({
            "connection_string": self.connection_string,
            "kind": self.kind,
            "device": self.device,
            "reason": REASON_NEW_DEVICE,
            "ts": ts,
        })
    }
}

#[derive(Debug, Clone)]
struct SessionInner {
    enabled: bool,
    usb: bool,
    sik: bool,
    udp_fallback: bool,
    manual_override: bool,
    last_suggestion_key: Option<String>,
    last_dial_key: Option<String>,
    reason: String,
    winner: String,
}

/// Auto connect state that lives for one process and is never persisted.
pub struct SessionState {
    inner: Mutex<SessionInner>,
}

impl Default for SessionState {
    fn default() -> Self {
        SessionState::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SessionState {
    pub fn new() -> SessionState {
        SessionState {
            inner: Mutex::new(SessionInner {
                enabled: true,
                usb: true,
                sik: true,
                udp_fallback: true,
                manual_override: false,
                last_suggestion_key: None,
                last_dial_key: None,
                reason: String::new(),
                winner: String::new(),
            }),
        }
    }

    /// Refreshes the toggles from a coerced `autoconnect` config block.
    pub fn apply_config(&self, block: Option<&Map<String, Value>>) {
        let flag = |key: &str| block.and_then(|b| b.get(key)).is_none_or(truthy);
        let mut s = lock(&self.inner);
        s.enabled = flag("enabled");
        s.usb = flag("usb");
        s.sik = flag("sik");
        s.udp_fallback = flag("udp_fallback");
    }

    /// A manual connect took the session: stop dialling, stop suggesting.
    pub fn note_manual_connect(&self) {
        let mut s = lock(&self.inner);
        s.manual_override = true;
        s.last_suggestion_key = None;
        s.reason = REASON_MANUAL.into();
    }

    pub fn note_decision(&self, reason: &str, winner: &str) {
        let mut s = lock(&self.inner);
        s.reason = reason.into();
        s.winner = winner.into();
    }

    pub fn enabled(&self) -> bool {
        lock(&self.inner).enabled
    }

    pub fn manual_override(&self) -> bool {
        lock(&self.inner).manual_override
    }

    pub fn usb(&self) -> bool {
        lock(&self.inner).usb
    }

    pub fn sik(&self) -> bool {
        lock(&self.inner).sik
    }

    pub fn reason(&self) -> String {
        lock(&self.inner).reason.clone()
    }

    pub fn last_dial_key(&self) -> Option<String> {
        lock(&self.inner).last_dial_key.clone()
    }

    pub fn last_suggestion_key(&self) -> Option<String> {
        lock(&self.inner).last_suggestion_key.clone()
    }

    pub fn set_last_dial_key(&self, key: Option<String>) {
        lock(&self.inner).last_dial_key = key;
    }

    /// The shape published in the store as `link_auto`.
    pub fn as_value(&self) -> Value {
        let s = lock(&self.inner);
        json!({
            "enabled": s.enabled,
            "usb": s.usb,
            "sik": s.sik,
            "udp_fallback": s.udp_fallback,
            "manual_override": s.manual_override,
            "reason": s.reason,
            "winner": if s.winner.is_empty() { Value::Null } else { json!(s.winner) },
        })
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

/// Classified rows, phantom and pseudo terminal nodes dropped.
pub fn classify_ports(rows: &[PortRow]) -> Vec<PortInfo> {
    rows.iter()
        .map(PortInfo::from_row)
        .filter(|p| !p.device.is_empty() && !is_phantom_device(&p.device))
        .collect()
}

fn usable(port: &PortInfo) -> bool {
    !is_bootloader_port(&port.device, &port.hwid, &port.description)
}

/// First port of `kind` by device name, skipping bootloaders: sorted, so two
/// flight controllers on one laptop give the same choice on every launch.
fn pick<'a>(ports: &'a [PortInfo], kind: &str) -> Option<&'a PortInfo> {
    ports
        .iter()
        .filter(|p| p.kind == kind && usable(p))
        .min_by(|a, b| a.device.cmp(&b.device))
}

/// `serial:<device>:<baud>`, the spelling the LINK tab builds.
pub fn serial_connection_string(device: &str, baud: u32) -> String {
    format!("serial:{device}:{baud}")
}

fn serial_device(conn: &str) -> String {
    if conn.starts_with("serial:") {
        split_serial_connection(conn).0
    } else {
        String::new()
    }
}

/// The baud rate already chosen for `device`, else the factory default.
fn baud_for(device: &str, known: &[&str]) -> u32 {
    for conn in known {
        let other = serial_device(conn);
        if !other.is_empty() && same_port(device, &other) {
            return split_serial_connection(conn).1;
        }
    }
    DEFAULT_SERIAL_BAUD
}

/// `(device, baud, kind)` of the best serial candidate: USB before SiK.
pub fn pick_usb_serial(
    ports: &[PortInfo],
    usb_enabled: bool,
    sik_enabled: bool,
) -> Option<(String, u32, &'static str)> {
    if usb_enabled {
        if let Some(hit) = pick(ports, "usb") {
            return Some((hit.device.clone(), DEFAULT_SERIAL_BAUD, "usb"));
        }
    }
    if sik_enabled {
        if let Some(hit) = pick(ports, "sik") {
            return Some((hit.device.clone(), DEFAULT_SERIAL_BAUD, "sik"));
        }
    }
    None
}

pub type Validator<'a> = &'a dyn Fn(&str) -> Result<(), String>;

fn is_valid(conn: Option<&str>, validator: Option<Validator>) -> bool {
    let Some(conn) = conn else { return false };
    if conn.trim().is_empty() {
        return false;
    }
    validator.is_none_or(|v| v(conn).is_ok())
}

pub struct StartupOptions<'a> {
    pub usb_enabled: bool,
    pub sik_enabled: bool,
    pub udp_fallback_enabled: bool,
    pub default_connection: &'a str,
    pub udp_fallback: &'a str,
    pub validator: Option<Validator<'a>>,
}

impl Default for StartupOptions<'_> {
    fn default() -> Self {
        StartupOptions {
            usb_enabled: true,
            sik_enabled: true,
            udp_fallback_enabled: true,
            default_connection: UDP_FALLBACK_CONNECTION,
            udp_fallback: UDP_FALLBACK_CONNECTION,
            validator: None,
        }
    }
}

/// The connection string to start on. Pure: `ports` is injected. A command
/// line argument outranks everything; below it the hardware in front of the
/// operator wins over the config file. An unusable value never fails the launch.
pub fn resolve_startup_connection(
    cli_arg: Option<&str>,
    configured: Option<&str>,
    ports: &[PortInfo],
    options: &StartupOptions,
) -> StartupDecision {
    let decision = |conn: &str, reason: &'static str, detail: String| StartupDecision {
        connection_string: conn.to_string(),
        reason,
        detail,
    };
    if is_valid(cli_arg, options.validator) {
        return decision(cli_arg.unwrap_or("").trim(), REASON_CLI, String::new());
    }
    if let Some((device, _, kind)) = pick_usb_serial(ports, options.usb_enabled, options.sik_enabled) {
        let conn = serial_connection_string(&device, baud_for(&device, &[configured.unwrap_or("")]));
        let reason = if kind == "usb" { REASON_USB } else { REASON_SIK };
        let candidates = ports.iter().filter(|p| p.kind == kind && usable(p)).count();
        let detail = if candidates < 2 {
            device
        } else {
            format!("{device} ({candidates} candidates)")
        };
        return decision(&conn, reason, detail);
    }
    if is_valid(configured, options.validator) {
        return decision(configured.unwrap_or("").trim(), REASON_CONFIGURED, String::new());
    }
    if options.udp_fallback_enabled && is_valid(Some(options.udp_fallback), options.validator) {
        return decision(options.udp_fallback, REASON_UDP_FALLBACK, String::new());
    }
    decision(options.default_connection, REASON_DEFAULT_FALLBACK, String::new())
}

/// A device worth offering the operator, or None. Only for a busy link: the
/// mid session plug, where the answer is a row in the LINK tab and never a
/// teardown.
pub fn suggest_connection(
    snapshot: &Map<String, Value>,
    ports: &[PortInfo],
    session: &SessionState,
) -> Option<Suggestion> {
    if !session.enabled() || session.manual_override() {
        return None;
    }
    let (device, _, kind) = pick_usb_serial(ports, session.usb(), session.sik())?;
    let link = text_of(snapshot.get("link_connection"));
    if same_port(&device, &serial_device(&link)) {
        return None;
    }
    Some(Suggestion {
        connection_string: serial_connection_string(&device, DEFAULT_SERIAL_BAUD),
        kind: if kind == "usb" { "usb-direct" } else { "sik" },
        device,
    })
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

// --------------------------------------------------------------- the watcher

/// What the watcher needs of the bridge.
pub trait AutoBridge: Send + Sync {
    fn is_running(&self) -> bool;
    fn connection_string(&self) -> String;
    fn validate_connection(&self, connection: &str) -> Result<(), String>;
    fn switch_connection(&self, connection: &str) -> Result<(), String>;
}

/// What the watcher needs of the state store.
pub trait AutoStore: Send + Sync {
    fn snapshot(&self) -> Map<String, Value>;
    fn is_stale(&self, timeout: f64) -> bool;
    fn update(&self, fields: Vec<(String, Value)>);
}

impl AutoBridge for crate::bridge::Bridge {
    fn is_running(&self) -> bool {
        crate::bridge::Bridge::is_running(self)
    }
    fn connection_string(&self) -> String {
        crate::bridge::Bridge::connection_string(self)
    }
    fn validate_connection(&self, connection: &str) -> Result<(), String> {
        crate::bridge::validate_connection(connection)
    }
    fn switch_connection(&self, connection: &str) -> Result<(), String> {
        crate::bridge::Bridge::switch_connection(self, connection)
    }
}

impl AutoStore for crate::store::VehicleStateStore {
    fn snapshot(&self) -> Map<String, Value> {
        (*crate::store::VehicleStateStore::snapshot(self)).clone()
    }
    fn is_stale(&self, timeout: f64) -> bool {
        crate::store::VehicleStateStore::is_stale(self, timeout)
    }
    fn update(&self, fields: Vec<(String, Value)>) {
        crate::store::VehicleStateStore::update(self, fields);
    }
}

pub type PortLister = Arc<dyn Fn() -> Vec<PortRow> + Send + Sync>;
pub type BusyDevices = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

pub struct WatcherConfig {
    pub poll_s: f64,
    pub stale_after_s: f64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        WatcherConfig {
            poll_s: DEFAULT_POLL_S,
            stale_after_s: STALE_AFTER_S,
        }
    }
}

struct Wake {
    stop: Mutex<bool>,
    cond: Condvar,
}

/// One thread that dials while disconnected and suggests otherwise.
pub struct AutoConnectWatcher {
    bridge: Arc<dyn AutoBridge>,
    store: Arc<dyn AutoStore>,
    session: Arc<SessionState>,
    list_ports: PortLister,
    busy: BusyDevices,
    cfg: WatcherConfig,
    link_identity: Mutex<Option<(String, String)>>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    wake: Arc<Wake>,
    stopped: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

const IDLE_STATES: [&str; 2] = ["disconnected", "reconnecting"];

impl AutoConnectWatcher {
    pub fn new(
        bridge: Arc<dyn AutoBridge>,
        store: Arc<dyn AutoStore>,
        session: Arc<SessionState>,
        list_ports: PortLister,
        busy: BusyDevices,
        cfg: WatcherConfig,
    ) -> AutoConnectWatcher {
        let epoch = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64())
        };
        AutoConnectWatcher {
            bridge,
            store,
            session,
            list_ports,
            busy,
            cfg: WatcherConfig {
                poll_s: cfg.poll_s.max(0.1),
                ..cfg
            },
            link_identity: Mutex::new(None),
            clock: Arc::new(epoch),
            wake: Arc::new(Wake {
                stop: Mutex::new(false),
                cond: Condvar::new(),
            }),
            stopped: AtomicBool::new(false),
            thread: Mutex::new(None),
        }
    }

    /// The wall clock the suggestion's `ts` reads (a test pins it).
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> AutoConnectWatcher {
        self.clock = clock;
        self
    }

    /// Begins watching. Idempotent; a no op when auto connect is disabled.
    pub fn start(self: &Arc<Self>) {
        if !self.session.enabled() {
            return;
        }
        let mut thread = lock(&self.thread);
        if thread.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        *lock(&self.wake.stop) = false;
        self.stopped.store(false, Ordering::SeqCst);
        let me = Arc::clone(self);
        *thread = std::thread::Builder::new()
            .name("autoconnect-watcher".into())
            .spawn(move || me.run())
            .ok();
    }

    /// Wakes the thread and joins it. Safe to call twice.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        *lock(&self.wake.stop) = true;
        self.wake.cond.notify_all();
        if let Some(handle) = lock(&self.thread).take() {
            let _ = handle.join();
        }
    }

    pub fn is_alive(&self) -> bool {
        lock(&self.thread).as_ref().is_some_and(|t| !t.is_finished())
    }

    fn run(&self) {
        loop {
            {
                let guard = lock(&self.wake.stop);
                let (guard, _) = self
                    .wake
                    .cond
                    .wait_timeout_while(guard, Duration::from_secs_f64(self.cfg.poll_s), |stop| !*stop)
                    .unwrap_or_else(|e| e.into_inner());
                if *guard {
                    return;
                }
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.tick()));
        }
    }

    /// One decision pass. Returns what it did, for logs and tests.
    pub fn tick(&self) -> String {
        if self.stopped.load(Ordering::SeqCst) || !self.session.enabled() {
            return "disabled".into();
        }
        let manual = self.session.manual_override();
        if !self.bridge.is_running() {
            return if manual {
                "noop: manual-override"
            } else {
                "noop: bridge-stopped"
            }
            .into();
        }
        let snapshot = self.store.snapshot();
        let status = text_of(snapshot.get("link_status")).to_lowercase();
        let mut ports = classify_ports(&(self.list_ports)());
        let busy: Vec<String> = (self.busy)().into_iter().filter(|d| !d.is_empty()).collect();
        if !busy.is_empty() {
            ports.retain(|p| !busy.iter().any(|d| same_port(&p.device, d)));
        }
        self.publish_bootloader(&ports, &snapshot);

        let link = self.bridge.connection_string();
        let device = serial_device(&link);
        if !device.is_empty() {
            let present: Vec<String> = ports.iter().map(|p| p.device.clone()).collect();
            if port_present(&device, &present) {
                self.remember_identity(&link, &device, &ports);
            } else if !IDLE_STATES.contains(&status.as_str()) {
                return "noop: link-device-gone".into();
            } else if let Some(moved) = self.moved_port(&link, &device, &ports) {
                return self.follow(&link, &moved);
            }
        }

        if manual {
            return "noop: manual-override".into();
        }
        let idle = IDLE_STATES.contains(&status.as_str()) && self.is_stale();

        let Some(pick) = pick_usb_serial(&ports, self.session.usb(), self.session.sik()) else {
            self.publish_suggestion(None);
            self.session.set_last_dial_key(None);
            return format!("noop: {REASON_NO_CANDIDATE}");
        };

        if !idle {
            let suggestion = suggest_connection(&snapshot, &ports, &self.session);
            let published = self.publish_suggestion(suggestion.as_ref());
            if suggestion.is_none() {
                return "noop: fresh-heartbeat".into();
            }
            return if published {
                "suggested"
            } else {
                "noop: suggestion-unchanged"
            }
            .into();
        }
        self.auto_dial(&pick)
    }

    fn is_stale(&self) -> bool {
        self.store.is_stale(self.cfg.stale_after_s)
    }

    fn auto_dial(&self, pick: &(String, u32, &'static str)) -> String {
        let (device, _, kind) = pick;
        let link = self.bridge.connection_string();
        let conn = serial_connection_string(device, baud_for(device, &[&link]));
        if self.session.last_dial_key().as_deref() == Some(conn.as_str()) {
            return "noop: already-dialled".into();
        }
        if self.bridge.validate_connection(&conn).is_err() {
            self.session.set_last_dial_key(Some(conn));
            return "noop: invalid".into();
        }
        let reason = if *kind == "usb" { REASON_USB } else { REASON_SIK };
        self.session.set_last_dial_key(Some(conn.clone()));
        if self.bridge.switch_connection(&conn).is_err() {
            return "dial-failed".into();
        }
        self.session.note_decision(reason, &conn);
        self.publish_suggestion(None);
        self.publish_auto();
        format!("dialled: {reason}")
    }

    /// Moves the link to the node its device came back on: same baud rate,
    /// same manual override flag. This is the operator's link, renamed by the
    /// operating system, not a new choice.
    fn follow(&self, link: &str, port: &PortInfo) -> String {
        let conn = serial_connection_string(&port.device, split_serial_connection(link).1);
        if self.bridge.validate_connection(&conn).is_err() {
            return "noop: invalid".into();
        }
        self.session.set_last_dial_key(Some(conn.clone()));
        if self.bridge.switch_connection(&conn).is_err() {
            return "dial-failed".into();
        }
        {
            let mut identity = lock(&self.link_identity);
            if let Some((_, id)) = identity.take() {
                *identity = Some((conn.clone(), id));
            }
        }
        let reason = if self.session.manual_override() {
            REASON_MANUAL.to_string()
        } else {
            match port.kind.as_str() {
                "usb" => REASON_USB.to_string(),
                "sik" => REASON_SIK.to_string(),
                _ => self.session.reason(),
            }
        };
        self.session.note_decision(&reason, &conn);
        self.publish_suggestion(None);
        self.publish_auto();
        "followed".into()
    }

    /// Notes the USB identity of the port the link is on, while it is there. A
    /// bootloader is never remembered.
    fn remember_identity(&self, link: &str, device: &str, ports: &[PortInfo]) {
        if let Some(port) = ports.iter().find(|p| same_port(device, &p.device)) {
            if let Some(identity) = usb_identity(&port.hwid) {
                if usable(port) {
                    *lock(&self.link_identity) = Some((link.to_string(), identity));
                }
            }
        }
    }

    /// The one present port carrying the identity the link's device had. Two
    /// candidates is no answer.
    fn moved_port(&self, link: &str, device: &str, ports: &[PortInfo]) -> Option<PortInfo> {
        let remembered = lock(&self.link_identity).clone()?;
        if remembered.0 != link {
            return None;
        }
        let matches: Vec<&PortInfo> = ports
            .iter()
            .filter(|p| {
                usb_identity(&p.hwid).as_deref() == Some(remembered.1.as_str())
                    && !same_port(&p.device, device)
                    && usable(p)
            })
            .collect();
        (matches.len() == 1).then(|| matches[0].clone())
    }

    /// Pushes (or clears) `link_suggestion`; true when the store changed.
    fn publish_suggestion(&self, suggestion: Option<&Suggestion>) -> bool {
        let key = suggestion.map(|s| s.connection_string.clone());
        {
            let mut s = lock(&self.session.inner);
            if key == s.last_suggestion_key {
                return false;
            }
            s.last_suggestion_key = key;
        }
        let value = suggestion.map_or(Value::Null, |s| s.as_value((self.clock)()));
        self.store.update(vec![("link_suggestion".into(), value)]);
        true
    }

    fn publish_bootloader(&self, ports: &[PortInfo], snapshot: &Map<String, Value>) {
        let found = ports
            .iter()
            .find(|p| is_bootloader_port(&p.device, &p.hwid, &p.description));
        let current = snapshot
            .get("link_bootloader")
            .and_then(|v| v.get("device"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if found.map(|p| p.device.clone()) == current {
            return;
        }
        let value = found.map_or(
            Value::Null,
            |p| json!({"device": p.device, "description": p.description}),
        );
        self.store.update(vec![("link_bootloader".into(), value)]);
    }

    fn publish_auto(&self) {
        self.store.update(vec![("link_auto".into(), self.session.as_value())]);
    }
}

impl Drop for AutoConnectWatcher {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        *lock(&self.wake.stop) = true;
        self.wake.cond.notify_all();
    }
}
