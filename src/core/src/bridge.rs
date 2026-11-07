//! The bridge: one MAVLink link to one vehicle, its connect and reconnect
//! cycle, the receive loop, and the engines of the protocols wired to it.
//!
//! `Shared` is the bridge's state and the host every engine asks: it
//! implements `ReceiveHost`, `StatusHost`, `RequestsHost`, `TelemetryHost`,
//! `RtcmHost`, `TlogHost` and `FlightHost`, the way the Python bridge was the
//! one object all of its mixins ran on. The threads are the receive cycle, the
//! GCS heartbeat and the two deferred one shot workers, and `stop` joins every
//! one of them, so a rebooted field laptop never meets a zombie.
//!
//! Wired: the receive path and command transaction, status text and the kill
//! switch, the bridge's own requests and signing, telemetry, the RTCM
//! injection, the tlog, the flight commands, parameters, missions and the
//! geofence, FTP, logs, shell, setup and Remote ID. Not wired yet: the vehicle
//! list, the forwarder, auto connect and the planner behind `mission.py`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use corvus_link::plain::connection::{open, Connection, Options};
use corvus_link::plain::defs::constant_or;
use corvus_link::plain::frame::{Proto, Signing};
use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Map, Value};

use crate::clock;
use crate::dialect::{dialect_for_id, dialect_for_stack, Dialect, ModeCommand, Stack};
use crate::fleet::FleetTracker;
use crate::priority_lock::PriorityLock;
use crate::protocol::flight::{FlightEngine, FlightHost, FlightSnapshot};
use crate::protocol::missions::{
    build_mission_item_spec, mission_ack_to_result, takeoff_item_position, validate_fly_to_points,
    with_mission_home_slot, MissionItem,
};
use crate::protocol::receive::{is_vehicle_heartbeat, Frame, Payload, ReceiveHost, ReceivePath, StoreFlags};
use crate::protocol::requests::{RequestsEngine, RequestsHost};
use crate::protocol::rtcm::{RtcmHost, RtcmInjector};
use crate::protocol::status::{StatusEngine, StatusHost, StatusText};
use crate::protocol::targeted;
use crate::protocol::telemetry::{TelemetryEngine, TelemetryHost};

use crate::store::VehicleStateStore;
use crate::tlog::{TlogHost, TlogSession};

pub const GCS_SYSTEM_ID: u8 = 254;
pub const GCS_COMPONENT_ID: u8 = 190;
pub const HEARTBEAT_WAIT_S: f64 = 10.0;
pub const RECONNECT_BASE_S: f64 = 0.5;
pub const RECONNECT_CAP_S: f64 = 8.0;
pub const RECONNECT_JITTER: f64 = 0.25;
pub const RECONNECT_MIN_S: f64 = 0.1;
const HEARTBEAT_WARN_TIMEOUT_SERIAL: f64 = 6.0;
const HEARTBEAT_WARN_TIMEOUT_UDP: f64 = 3.0;
const HEARTBEAT_DROP_TIMEOUT_SERIAL: f64 = 15.0;
const HEARTBEAT_DROP_TIMEOUT_UDP: f64 = 8.0;
const HEARTBEAT_TIMEOUT_UDP: f64 = 5.0;
const DEFAULT_SERIAL_BAUD: u32 = 57_600;
const MAX_WARNINGS: usize = 20;
const MSG_ID_AUTOPILOT_VERSION: i64 = 148;

/// Where the bridge connects and where it keeps its tlogs.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub connection: String,
    pub log_dir: std::path::PathBuf,
    /// Names this link in the vehicle id (`link:system`).
    pub link_id: String,
}

impl BridgeConfig {
    pub fn new(connection: &str, log_dir: impl Into<std::path::PathBuf>) -> BridgeConfig {
        BridgeConfig {
            connection: connection.to_string(),
            log_dir: log_dir.into(),
            link_id: "main".into(),
        }
    }
}

/// `serial:<device>[:<baud>]` as `(device, baud)`. The baud is the last field
/// only when it is all digits, so a device name with a colon in it keeps it.
pub fn split_serial_connection(conn: &str) -> (String, u32) {
    let rest = conn.strip_prefix("serial:").unwrap_or(conn);
    if let Some((device, baud)) = rest.rsplit_once(':') {
        if !baud.is_empty() && baud.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(baud) = baud.parse() {
                return (device.to_string(), baud);
            }
        }
    }
    (rest.to_string(), DEFAULT_SERIAL_BAUD)
}

const VALID_PREFIXES: [&str; 7] = ["udp:", "udpin:", "udpout:", "udpbcast:", "tcp:", "tcpin:", "serial:"];

/// Why `connection` is not a usable connection string, or Ok. Checked before a
/// working link is torn down for it.
pub fn validate_connection(connection: &str) -> Result<(), String> {
    if connection.is_empty() {
        return Err("connection must be a non-empty string".into());
    }
    if !VALID_PREFIXES.iter().any(|p| connection.starts_with(p)) {
        return Err(format!(
            "connection must start with one of: {}",
            VALID_PREFIXES.join(", ")
        ));
    }
    if connection.starts_with("serial:") {
        let (device, baud) = split_serial_connection(connection);
        if device.is_empty() {
            return Err("serial connection requires a device path".into());
        }
        if baud == 0 {
            return Err("serial connection requires a numeric baud rate".into());
        }
    }
    Ok(())
}

impl crate::fleet::FleetBridge for Bridge {
    fn fleet_tracker(&self) -> &FleetTracker {
        Bridge::fleet_tracker(self)
    }

    fn connection_string(&self) -> String {
        Bridge::connection_string(self).to_string()
    }

    fn controlled_system(&self) -> Option<i64> {
        Bridge::controlled_system(self)
    }

    fn validate_connection(&self, connection: &str) -> Result<(), String> {
        validate_connection(connection)
    }

    fn serial_device(&self) -> String {
        Bridge::serial_device(self)
    }

    fn start(&self) -> Result<(), String> {
        Bridge::start(self);
        Ok(())
    }

    fn stop(&self) {
        Bridge::stop(self);
    }

    fn is_connected(&self) -> bool {
        Bridge::is_connected(self)
    }

    fn inject_rtcm(&self, frame: &[u8]) -> bool {
        Bridge::inject_rtcm(self, frame)
    }
}

/// Exponential backoff with jitter between reconnect cycles: base times
/// 2^(n-1), capped, plus or minus a quarter. `unit` is a random number in
/// [-1, 1]; a linear backoff hammered a 57 kbps radio while it recovered.
pub fn reconnect_delay(attempt: i64, unit: f64) -> f64 {
    let raw = (RECONNECT_BASE_S * 2f64.powi((attempt - 1).max(0) as i32)).min(RECONNECT_CAP_S);
    (raw * (1.0 + unit.clamp(-1.0, 1.0) * RECONNECT_JITTER)).max(RECONNECT_MIN_S)
}

fn jitter_unit() -> f64 {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        return 0.0;
    }
    f64::from(u32::from_le_bytes(b)) / f64::from(u32::MAX) * 2.0 - 1.0
}

#[derive(Default)]
struct Threads {
    cycle: Option<JoinHandle<()>>,
    heartbeat: Option<JoinHandle<()>>,
    version_retry: Option<JoinHandle<()>>,
    intervals: Option<JoinHandle<()>>,
}

struct State {
    target: (u8, u8),
    dialect: Dialect,
    mav_type: i64,
    firmware: Option<(i64, i64, i64)>,
    degraded_warned: bool,
    rangefinder_seen: Option<f64>,
    last_boot_ms: Option<i64>,
    kill_evidence_at: f64,
    position_home_alt: Option<f64>,
    home_alt: Option<f64>,
    board: (i64, i64),
    param_encoding_declared: String,
    request_sent: bool,
    mode_values: Vec<(String, ModeCommand)>,
    last_error: String,
    reconnect_attempt: i64,
}

/// The bridge's state and the host its engines ask.
pub struct Shared {
    me: std::sync::Weak<Shared>,
    config: BridgeConfig,
    store: Arc<VehicleStateStore>,
    conn: RwLock<Option<Arc<Connection>>>,
    generation: AtomicU64,
    running: AtomicBool,
    stop_event: AtomicBool,
    run_generation: AtomicU64,
    send_lock: Mutex<()>,
    op_lock: PriorityLock,
    state: Mutex<State>,
    threads: Mutex<Threads>,
    lifecycle: Mutex<()>,
    subs: Mutex<Vec<(u64, crate::protocol::missions::ConsoleSub)>>,
    next_sub: AtomicU64,
    receive: ReceivePath,
    status: StatusEngine,
    requests: RequestsEngine,
    telemetry: TelemetryEngine,
    rtcm: RtcmInjector,
    tlog: TlogSession,
    flight: FlightEngine,
    engines: Engines,
    fleet_tracker: FleetTracker,
    inject_refused_warned: AtomicBool,
}

/// One link to one vehicle.
#[derive(Clone)]
pub struct Bridge {
    shared: Arc<Shared>,
}

impl Bridge {
    pub fn new(store: Arc<VehicleStateStore>, config: BridgeConfig) -> Bridge {
        Bridge {
            shared: Arc::new_cyclic(|me| Shared {
                me: me.clone(),
                config,
                store,
                conn: RwLock::new(None),
                generation: AtomicU64::new(0),
                running: AtomicBool::new(false),
                stop_event: AtomicBool::new(false),
                run_generation: AtomicU64::new(0),
                send_lock: Mutex::new(()),
                op_lock: PriorityLock::new(),
                state: Mutex::new(State {
                    target: (1, 1),
                    dialect: dialect_for_stack(Stack::Px4),
                    mav_type: 0,
                    firmware: None,
                    degraded_warned: false,
                    rangefinder_seen: None,
                    last_boot_ms: None,
                    kill_evidence_at: 0.0,
                    position_home_alt: None,
                    home_alt: None,
                    board: (0, 0),
                    param_encoding_declared: String::new(),
                    request_sent: false,
                    mode_values: Vec::new(),
                    last_error: String::new(),
                    reconnect_attempt: 1,
                }),
                threads: Mutex::new(Threads::default()),
                lifecycle: Mutex::new(()),
                subs: Mutex::new(Vec::new()),
                next_sub: AtomicU64::new(1),
                receive: ReceivePath::new(),
                status: StatusEngine::new(),
                requests: RequestsEngine::new(),
                telemetry: TelemetryEngine::new(),
                rtcm: RtcmInjector::new(),
                tlog: TlogSession::new(),
                flight: FlightEngine::new(),
                engines: Engines::new(),
                fleet_tracker: FleetTracker::new(),
                inject_refused_warned: AtomicBool::new(false),
            }),
        }
    }

    pub fn store(&self) -> &Arc<VehicleStateStore> {
        &self.shared.store
    }

    /// Starts the connect and receive thread unless a healthy one runs.
    pub fn start(&self) {
        let sh = &self.shared;
        let _life = sh.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        sh.stop_event.store(false, Ordering::SeqCst);
        let mut threads = sh.threads.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = threads.cycle.as_ref() {
            if !t.is_finished() {
                if sh.running.load(Ordering::SeqCst) {
                    // A healthy thread is already running; do not spawn a second.
                    return;
                }
                // stop() was just called but the old thread has not died yet.
                join_bounded(threads.cycle.take(), Duration::from_secs(1));
            }
        }
        sh.running.store(true, Ordering::SeqCst);
        let generation = sh.run_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let me = Arc::clone(sh);
        threads.cycle = std::thread::Builder::new()
            .name("mavlink".into())
            .spawn(move || me.run(generation))
            .ok();
    }

    /// Stops everything and joins every thread, bounded. Idempotent.
    pub fn stop(&self) {
        let sh = &self.shared;
        let _life = sh.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        sh.running.store(false, Ordering::SeqCst);
        sh.stop_event.store(true, Ordering::SeqCst);
        sh.tlog.stop_tlog(sh.as_ref());
        sh.store.update([("link_status", json!("disconnected"))]);
        sh.engines.shell.stop(sh.as_ref());
        // Mark the vehicle disconnected so every in-flight command bails.
        sh.store.set_disconnected();
        ReceiveHost::cancel_mission_upload(sh.as_ref());
        sh.receive.cancel_pending_commands(sh.as_ref());
        if let Some(conn) = sh.conn.write().unwrap_or_else(|e| e.into_inner()).take() {
            conn.close();
            sh.generation.fetch_add(1, Ordering::SeqCst);
        }
        self.join_workers();
        let mut threads = sh.threads.lock().unwrap_or_else(|e| e.into_inner());
        join_bounded(threads.version_retry.take(), Duration::from_secs(2));
        join_bounded(threads.intervals.take(), Duration::from_secs(2));
        join_bounded(threads.cycle.take(), Duration::from_secs(3));
        join_bounded(threads.heartbeat.take(), Duration::from_secs(2));
    }

    pub fn is_connected(&self) -> bool {
        self.shared.link_ready()
    }

    pub fn connection_string(&self) -> &str {
        &self.shared.config.connection
    }

    /// What the vehicle on this link last said about itself, for the list.
    pub fn fleet_tracker(&self) -> &FleetTracker {
        &self.shared.fleet_tracker
    }

    /// The system this link flies, or None while it flies none.
    pub fn controlled_system(&self) -> Option<i64> {
        self.shared.link_ready().then(|| i64::from(self.shared.st().target.0))
    }

    /// The serial device the link holds, "" for a network link.
    pub fn serial_device(&self) -> String {
        let conn = &self.shared.config.connection;
        if conn.starts_with("serial:") {
            split_serial_connection(conn).0
        } else {
            String::new()
        }
    }

    pub fn last_error(&self) -> String {
        self.shared.st().last_error.clone()
    }

    /// The target the link flies, as `(system, component)`.
    pub fn target(&self) -> (u8, u8) {
        self.shared.st().target
    }

    pub fn arm(&self, arm: bool) -> bool {
        self.shared.flight.arm(self.shared.as_ref(), arm)
    }

    pub fn set_mode(&self, mode: &str) -> bool {
        self.shared.flight.set_mode(self.shared.as_ref(), mode)
    }

    pub fn takeoff(&self, altitude_agl: f64) -> bool {
        self.shared.flight.takeoff(self.shared.as_ref(), altitude_agl)
    }

    pub fn land(&self) -> bool {
        self.shared.flight.land(self.shared.as_ref())
    }

    pub fn rtl(&self) -> bool {
        self.shared.flight.rtl(self.shared.as_ref())
    }

    pub fn hold(&self) -> bool {
        self.shared.flight.hold(self.shared.as_ref())
    }

    /// Where a UDP link that listens is bound, else None. The forwarder asks,
    /// so it never mirrors into this socket.
    pub fn local_udp_address(&self) -> Option<(String, u16)> {
        let connection = &self.shared.config.connection;
        if !(connection.starts_with("udp:") || connection.starts_with("udpin:")) {
            return None;
        }
        let addr = self.shared.conn()?.local_addr()?;
        Some((addr.ip().to_string(), addr.port()))
    }

    /// Writes one already framed MAVLink message straight to the aircraft: the
    /// entry point of a second ground station. The bytes go through untouched
    /// so its own sequence numbers, system id and signature reach the vehicle.
    /// A frame under Corvus' own id or the vehicle's is refused (what a
    /// forwarding loop hands back).
    pub fn inject_raw(&self, frame: &[u8]) -> bool {
        let sh = &self.shared;
        let Some(conn) = sh.conn() else { return false };
        if frame.is_empty() {
            return false;
        }
        let source = crate::forwarder::frame_source_system(frame);
        let vehicle = sh.st().target.0;
        if source == GCS_SYSTEM_ID || source == vehicle {
            if !sh.inject_refused_warned.swap(true, Ordering::SeqCst) {
                let whose = if source == GCS_SYSTEM_ID {
                    "Corvus' own id"
                } else {
                    "the vehicle's id"
                };
                sh.log_line(
                    "warning",
                    &format!("not forwarding frames from system {source} to the vehicle: that is {whose}"),
                );
            }
            return false;
        }
        let _guard = sh.send_lock.lock().unwrap_or_else(|e| e.into_inner());
        conn.write_raw(frame).is_ok()
    }

    pub fn inject_rtcm(&self, frame: &[u8]) -> bool {
        self.shared.rtcm.inject_rtcm(self.shared.as_ref(), frame)
    }

    pub fn rtcm_stats(&self) -> Value {
        self.shared.rtcm.rtcm_stats(self.shared.as_ref())
    }

    pub fn forget_rtcm_session(&self) {
        self.shared.rtcm.forget_rtcm_session();
    }

    /// Sets a stream rate through the interval command (see `RequestsEngine`).
    pub fn set_message_interval(&self, msg_id: i64, interval_us: i64) -> bool {
        self.shared
            .requests
            .set_message_interval(self.shared.as_ref(), msg_id, interval_us, false)
            .0
    }

    /// Where the tlog being written is, if one is.
    pub fn tlog_path(&self) -> Option<std::path::PathBuf> {
        self.shared.tlog.current_path()
    }

    pub fn set_frame_sink(&self, sink: Option<crate::tlog::FrameSink>) {
        self.shared.tlog.set_frame_sink(sink);
    }

    pub fn add_console_listener(&self, f: impl Fn(&str, &str, &str) + Send + Sync + 'static) -> u64 {
        ReceiveHostConsole::add(
            self.shared.as_ref(),
            Arc::new(move |e: &crate::protocol::missions::ConsoleEntry| f(&e.name, &e.text, &e.level)),
        )
    }
}

struct ReceiveHostConsole;

impl ReceiveHostConsole {
    fn add(sh: &Shared, f: crate::protocol::missions::ConsoleSub) -> u64 {
        let id = sh.next_sub.fetch_add(1, Ordering::SeqCst);
        sh.subs.lock().unwrap_or_else(|e| e.into_inner()).push((id, f));
        id
    }
}

fn join_bounded(handle: Option<JoinHandle<()>>, limit: Duration) {
    let Some(handle) = handle else { return };
    let deadline = std::time::Instant::now() + limit;
    while !handle.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

fn int(v: i64) -> Wv {
    Wv::Int(i128::from(v))
}

impl Shared {
    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn conn(&self) -> Option<Arc<Connection>> {
        self.conn.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn is_serial(&self) -> bool {
        self.config.connection.starts_with("serial:")
    }

    fn warn_timeout(&self) -> f64 {
        if self.is_serial() {
            HEARTBEAT_WARN_TIMEOUT_SERIAL
        } else {
            HEARTBEAT_WARN_TIMEOUT_UDP
        }
    }

    fn drop_timeout(&self) -> f64 {
        if self.is_serial() {
            HEARTBEAT_DROP_TIMEOUT_SERIAL
        } else {
            HEARTBEAT_DROP_TIMEOUT_UDP
        }
    }

    /// Stale threshold for the command gate: a briefly degraded link still
    /// refuses new operator commands.
    fn heartbeat_timeout(&self) -> f64 {
        if self.is_serial() {
            10.0
        } else {
            HEARTBEAT_TIMEOUT_UDP
        }
    }

    fn link_ready(&self) -> bool {
        self.conn().is_some()
            && self.store.get("connected").is_some_and(|v| v.as_bool() == Some(true))
            && !self.store.is_stale(self.heartbeat_timeout())
    }

    fn stopping(&self) -> bool {
        self.stop_event.load(Ordering::SeqCst)
    }

    fn superseded(&self, generation: u64) -> bool {
        generation != self.run_generation.load(Ordering::SeqCst)
    }

    /// Sleep that returns early when the stop event is set.
    fn interruptible_sleep(&self, seconds: f64) {
        let mut slept = 0.0;
        while slept < seconds {
            if self.stopping() {
                return;
            }
            let chunk = (seconds - slept).min(0.1);
            std::thread::sleep(Duration::from_secs_f64(chunk));
            slept += chunk;
        }
    }

    fn do_send(&self, msg: Message) -> Result<bool, String> {
        let _guard = self.send_lock.lock().unwrap_or_else(|e| e.into_inner());
        let Some(conn) = self.conn() else { return Ok(false) };
        conn.send(&msg).map(|()| true).map_err(|e| e.to_string())
    }

    fn console_publish(&self, name: &str, text: &str, level: &str) {
        let entry = crate::protocol::missions::ConsoleEntry {
            name: name.into(),
            text: text.into(),
            level: level.into(),
        };
        let subs: Vec<_> = self
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(_, f)| Arc::clone(f))
            .collect();
        for sub in subs {
            sub(&entry);
        }
    }

    fn log_line(&self, level: &str, text: &str) {
        eprintln!("corvus.mavlink {level}: {text}");
    }

    fn set_last_error(&self, text: &str) {
        self.st().last_error = text.to_string();
    }

    /// The mode names this link can command, latched from the dialect.
    fn rebuild_mode_mapping(&self) {
        let mut st = self.st();
        let table = st.dialect.mode_table(
            st.mav_type,
            st.firmware.as_ref().map(|f| [f.0, f.1, f.2]).as_ref().map(|a| &a[..]),
        );
        st.mode_values = table;
    }

    // ------------------------------------------------------------- cycle

    fn run(self: &Arc<Self>, generation: u64) {
        while self.running.load(Ordering::SeqCst) && !self.superseded(generation) {
            let result = self.connect(generation).and_then(|()| {
                self.st().reconnect_attempt = 1;
                self.schedule_message_intervals();
                self.receive_loop(generation)
            });
            let failure = result.err();
            if let Some(e) = &failure {
                if self.running.load(Ordering::SeqCst) && !self.superseded(generation) {
                    self.log_line("error", &format!("MAVLink error: {e}"));
                }
            }
            if self.superseded(generation) {
                self.log_line(
                    "info",
                    "MAVLink cycle superseded by a restart; leaving it to the new one",
                );
                return;
            }
            ReceiveHost::cancel_mission_upload(self.as_ref());
            self.receive.cancel_pending_commands(self.as_ref());
            // Close this cycle's tlog so a reconnect opens a fresh file.
            self.tlog.stop_tlog(self.as_ref());
            if let Some(conn) = self.conn.write().unwrap_or_else(|e| e.into_inner()).take() {
                conn.close();
                self.generation.fetch_add(1, Ordering::SeqCst);
            }
            self.store.set_disconnected();
            if self.running.load(Ordering::SeqCst) {
                let attempt = self.st().reconnect_attempt;
                let delay = reconnect_delay(attempt, jitter_unit());
                self.log_line("info", &format!("Reconnecting in {delay:.1}s (attempt {attempt}) …"));
                let mut update = vec![("link_status", json!("reconnecting"))];
                if let Some(e) = &failure {
                    update.push(("link_error", json!(e)));
                }
                self.store.update(update);
                // Interruptible: the backoff reaches 8 s and stop() joins this
                // thread for 3.
                self.interruptible_sleep(delay);
                self.st().reconnect_attempt += 1;
            }
        }
    }

    /// Opens the link and finds the vehicle. `Err` ends the cycle with the
    /// reason the operator sees.
    fn connect(self: &Arc<Self>, generation: u64) -> Result<(), String> {
        let conn_str = self.config.connection.clone();
        self.log_line("info", &format!("Connecting to {conn_str} …"));
        {
            let mut st = self.st();
            st.request_sent = false;
            st.param_encoding_declared.clear();
            st.firmware = None;
            st.last_boot_ms = None;
            st.kill_evidence_at = 0.0;
            st.home_alt = None;
            st.position_home_alt = None;
        }
        self.status.forget_link();
        self.engines.params.reset();
        self.engines.shell.reset();
        self.engines.remote_id.forget_session();
        self.engines.missions.forget_link();
        self.engines.missions.forget_vehicle_mission(self.as_ref(), "");
        self.store.update([("prearm_reasons", json!([]))]);
        // Forget the displayed home with the altitude reference: a home marker
        // has to be something the aircraft said in this session or nothing.
        self.store.update([("home", json!([0.0, 0.0]))]);
        self.telemetry.forget_link();
        self.st().degraded_warned = false;
        // A new link may be a new aircraft with a different pack.
        self.telemetry.forget_battery_cells();
        self.telemetry.forget_gnss_integrity(self.as_ref());
        self.store.update([
            ("mission_state", json!("")),
            ("mission_seq", json!(-1)),
            ("mission_reached", json!(-1)),
            ("mission_total", json!(-1)),
        ]);
        self.store.update([
            ("link_status", json!("connecting")),
            ("link_connection", json!(conn_str)),
            ("link_error", json!("")),
        ]);
        let mut opts = Options {
            source_system: GCS_SYSTEM_ID,
            source_component: GCS_COMPONENT_ID,
            ..Options::default()
        };
        let device = if self.is_serial() {
            let (device, baud) = split_serial_connection(&conn_str);
            opts.baud = baud;
            device
        } else {
            conn_str.clone()
        };
        let opened = match open(&device, &opts) {
            Ok(c) => c,
            Err(e) => {
                let message = if self.is_serial() {
                    format!("{device} would not open: {e}")
                } else if matches!(&e, corvus_link::plain::connection::ConnectError::Io(io) if io.kind() == std::io::ErrorKind::AddrInUse)
                {
                    format!(
                        "{conn_str}: port already in use by another program (QGroundControl, MAVROS/MAVSDK or a second Corvus). Close it or connect on a different port"
                    )
                } else {
                    e.to_string()
                };
                self.store.update([("link_error", json!(message))]);
                return Err(message);
            }
        };
        // Opening can block long enough for stop() to give up on this thread
        // and a new start() to begin. Then this connection is nobody's.
        if self.superseded(generation) {
            opened.close();
            return Err("superseded by a restart".into());
        }
        let opened = Arc::new(opened);
        *self.conn.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&opened));
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.requests.apply_signing(self.as_ref())?;
        // Speak first, then listen: a far end that waits to hear from a station
        // and a station that waits for the vehicle would each wait for the
        // other until the heartbeat wait gave up, on every reconnect.
        self.send_gcs_heartbeat();
        self.start_gcs_heartbeat();
        self.log_line("info", "Waiting for heartbeat …");
        let Some((system, component, hb)) = self.wait_vehicle_heartbeat(&opened, HEARTBEAT_WAIT_S, generation) else {
            return Err("No heartbeat received".into());
        };
        {
            let mut st = self.st();
            st.target = (
                if system == 0 { 1 } else { system },
                if component == 0 { 1 } else { component },
            );
        }
        self.receive.reset_ignored_systems();
        let target = self.st().target;
        // The vehicle list is this one vehicle from now on; whatever the link
        // carried before (another aircraft on the same radio) is gone.
        self.fleet_tracker.bind(i64::from(target.0), i64::from(target.1));
        self.fleet_tracker.observe(
            &Frame {
                payload: Payload::Message(&hb),
                system: i64::from(system),
                component: i64::from(component),
                raw: None,
            },
            true,
            None,
        );
        let autopilot = hb.get("autopilot").and_then(Wv::as_i64).unwrap_or(0);
        let mav_type = hb.get("type").and_then(Wv::as_i64).unwrap_or(0);
        ReceiveHost::latch_dialect(self.as_ref(), autopilot, mav_type);
        let base_mode = hb.get("base_mode").and_then(Wv::as_i64).unwrap_or(0);
        let custom_mode = hb.get("custom_mode").and_then(Wv::as_i64).unwrap_or(0);
        let armed = base_mode & constant_or("MAV_MODE_FLAG_SAFETY_ARMED", 128) != 0;
        let dialect = self.st().dialect;
        let mode = dialect.decode_mode(custom_mode, base_mode, mav_type);
        let vehicle_id = format!("{}:{}", self.config.link_id, target.0);
        let previous = self
            .store
            .get("vehicle_id")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        if !previous.is_empty() && previous != vehicle_id {
            // Another aircraft than the one this store described.
            self.store.forget_vehicle();
        }
        self.log_line(
            "info",
            &format!(
                "Heartbeat: sys={} comp={} {} mode={mode}",
                target.0,
                target.1,
                dialect.label()
            ),
        );
        self.store.update([
            ("connected", json!(true)),
            ("vehicle_type", json!(crate::protocol::receive::mav_type_name(mav_type))),
            (
                "autopilot",
                json!(crate::protocol::receive::mav_autopilot_name(autopilot)),
            ),
            ("armed", json!(armed)),
            ("mode", json!(mode)),
            ("mode_label", json!(dialect.mode_label(&mode))),
            ("link_status", json!("connected")),
            ("link_error", json!("")),
            ("autopilot_stack", json!(dialect.stack().as_str())),
            ("vehicle_id", json!(vehicle_id)),
        ]);
        self.store.heartbeat();
        self.rebuild_mode_mapping();
        // A serial link asks up front for the legacy stream set: it is point to
        // point, and a 57 kbps radio must not hold the HUD empty for a command
        // round trip. On UDP it waits until the interval requests prove it is
        // needed. Per message intervals are deferred until the receive loop
        // drains the socket, or every COMMAND_ACK would time out.
        if self.is_serial() {
            self.requests.request_streams(self.as_ref());
        }
        self.requests.request_version(self.as_ref());
        self.requests.request_home(self.as_ref());
        // A fresh tlog for this flight session.
        self.tlog.start_tlog(&(Arc::clone(self) as Arc<dyn TlogHost>));
        Ok(())
    }

    /// The first HEARTBEAT from an actual autopilot, within `timeout`. A wait
    /// that matches on type alone hands back whichever heartbeat lands first,
    /// and behind mavlink-router that is QGroundControl's: every command would
    /// then be addressed to a ground station while the dot stays green.
    fn wait_vehicle_heartbeat(
        &self,
        conn: &Arc<Connection>,
        timeout: f64,
        generation: u64,
    ) -> Option<(u8, u8, Message)> {
        let deadline = clock::monotonic() + timeout;
        loop {
            if self.stopping() || self.superseded(generation) {
                return None;
            }
            let remaining = deadline - clock::monotonic();
            if remaining <= 0.0 {
                return None;
            }
            let wait_for = remaining.min(1.0);
            let from = clock::monotonic();
            let got = match conn.recv(Duration::from_secs_f64(wait_for)) {
                Ok(frames) => frames,
                Err(_) => return None,
            };
            for r in got {
                let Ok(msg) = r.message else { continue };
                if msg.name() != "HEARTBEAT" {
                    continue;
                }
                let frame = Frame {
                    payload: Payload::Message(&msg),
                    system: i64::from(r.frame.sysid),
                    component: i64::from(r.frame.compid),
                    raw: None,
                };
                if is_vehicle_heartbeat(&frame) {
                    return Some((r.frame.sysid, r.frame.compid, msg));
                }
                self.log_line(
                    "info",
                    &format!(
                        "ignoring heartbeat from sys={} comp={}: not an autopilot",
                        r.frame.sysid, r.frame.compid
                    ),
                );
            }
            // A transport that answers "nothing" the instant it is asked is
            // not going to start answering differently.
            if clock::monotonic() - from < wait_for / 2.0 {
                return None;
            }
        }
    }

    fn receive_loop(self: &Arc<Self>, generation: u64) -> Result<(), String> {
        let mut consecutive_errors = 0;
        while self.running.load(Ordering::SeqCst) && !self.superseded(generation) {
            let Some(conn) = self.conn() else { return Ok(()) };
            if self.store.is_stale(self.warn_timeout()) {
                if self.store.is_stale(self.drop_timeout()) {
                    return Err("heartbeat timeout".into());
                }
                self.receive.mark_degraded_once(self.as_ref());
            }
            match conn.recv(Duration::from_secs(1)) {
                Ok(frames) => {
                    consecutive_errors = 0;
                    self.status.cleanup_statustext_chunks(clock::monotonic());
                    for r in frames {
                        // Connection swapped under us during recv: drop it.
                        if !self.conn().is_some_and(|c| Arc::ptr_eq(&c, &conn)) {
                            break;
                        }
                        let Ok(msg) = r.message else { continue };
                        let frame = Frame {
                            payload: Payload::Message(&msg),
                            system: i64::from(r.frame.sysid),
                            component: i64::from(r.frame.compid),
                            raw: Some(&r.frame.raw),
                        };
                        *self.engines.rx_source.lock().unwrap_or_else(|e| e.into_inner()) =
                            (frame.system, frame.component);
                        self.receive.process_frame(self.as_ref(), &frame);
                    }
                }
                Err(e) => {
                    if conn.is_closed() || !self.running.load(Ordering::SeqCst) {
                        return Err(e.to_string());
                    }
                    consecutive_errors += 1;
                    self.log_line("debug", &format!("recv error ({consecutive_errors} in a row): {e}"));
                    // Unplug the USB cable and the read fails the instant it is
                    // made: count the failures, back off, and end the cycle so
                    // the reconnect path takes over instead of spinning a core.
                    if consecutive_errors >= crate::protocol::receive::RECV_ERROR_LIMIT {
                        return Err(format!("link read failed: {e}"));
                    }
                    self.interruptible_sleep(crate::protocol::receive::RECV_ERROR_BACKOFF_S);
                }
            }
            if self.store.is_stale(self.warn_timeout()) {
                if self.store.is_stale(self.drop_timeout()) {
                    return Err("heartbeat timeout".into());
                }
                self.receive.mark_degraded_once(self.as_ref());
            }
        }
        Ok(())
    }

    fn send_gcs_heartbeat(&self) -> bool {
        let msg = Message::named("HEARTBEAT", corvus_link::plain::defs::Wire::V2)
            .expect("the all dialect has HEARTBEAT")
            .with("type", int(constant_or("MAV_TYPE_GCS", 6)))
            .with("autopilot", int(constant_or("MAV_AUTOPILOT_INVALID", 8)))
            .with("base_mode", int(0))
            .with("custom_mode", int(0))
            .with("system_status", int(0))
            .with("mavlink_version", int(3));
        matches!(self.do_send(msg), Ok(true))
    }

    fn start_gcs_heartbeat(self: &Arc<Self>) {
        let mut threads = self.threads.lock().unwrap_or_else(|e| e.into_inner());
        if threads.heartbeat.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let me = Arc::clone(self);
        threads.heartbeat = std::thread::Builder::new()
            .name("gcs-hb".into())
            .spawn(move || me.gcs_hb_loop())
            .ok();
    }

    /// Runs while the bridge runs, whatever the link status: a degraded link
    /// must not stop the GCS heartbeat, or the autopilot drops us while we wait
    /// for its heartbeat to return.
    fn gcs_hb_loop(&self) {
        while self.running.load(Ordering::SeqCst) && !self.stopping() {
            if self.conn().is_none() {
                self.interruptible_sleep(1.0);
                continue;
            }
            self.send_gcs_heartbeat();
            // Remote ID rides the heartbeat: both stacks want the identity at
            // about 1 Hz and fail the pre-arm check when it stops, which is this
            // loop's cadence and lifetime, and a second worker would be a
            // second thing to join. The vehicle must be known: the identity is
            // addressed to it.
            if self.store.get("connected").is_some_and(|v| v.as_bool() == Some(true)) {
                self.engines.remote_id.send_remote_id(self);
            }
            self.interruptible_sleep(1.0);
        }
    }

    /// Defers the ACK confirmed interval requests until the receive loop is
    /// running: sent from connect, every COMMAND_ACK timed out.
    fn schedule_message_intervals(self: &Arc<Self>) {
        let mut threads = self.threads.lock().unwrap_or_else(|e| e.into_inner());
        if threads.intervals.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let me = Arc::clone(self);
        threads.intervals = std::thread::Builder::new()
            .name("mavlink-intervals".into())
            .spawn(move || {
                // Let the receive loop start draining the socket first.
                me.interruptible_sleep(0.05);
                if !me.running.load(Ordering::SeqCst) || me.conn().is_none() || me.stopping() {
                    return;
                }
                me.requests.request_message_intervals(me.as_ref());
                me.requests.check_gcs_authority(me.as_ref());
            })
            .ok();
    }

    /// One delayed AUTOPILOT_VERSION retry if the first got no reply: PX4
    /// v1.18 SITL answers the capabilities command with UNSUPPORTED.
    fn schedule_version_retry_thread(self: &Arc<Self>) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        let mut threads = self.threads.lock().unwrap_or_else(|e| e.into_inner());
        if threads.version_retry.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let me = Arc::clone(self);
        threads.version_retry = std::thread::Builder::new()
            .name("px4-ver-retry".into())
            .spawn(move || {
                me.interruptible_sleep(4.0);
                if !me.running.load(Ordering::SeqCst) || me.conn().is_none() {
                    return;
                }
                if me
                    .store
                    .get("px4_version")
                    .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                {
                    return;
                }
                let nan = f64::NAN;
                let mut msg = targeted("COMMAND_LONG", me.st().target)
                    .with("command", int(constant_or("MAV_CMD_REQUEST_MESSAGE", 512)))
                    .with("confirmation", int(0));
                let params = [MSG_ID_AUTOPILOT_VERSION as f64, nan, nan, nan, nan, nan, nan];
                for (i, p) in params.iter().enumerate() {
                    msg = msg.with(&format!("param{}", i + 1), Wv::Float(*p));
                }
                let _ = me.do_send(msg);
                let _ = me.do_send(targeted("AUTOPILOT_VERSION_REQUEST", me.st().target));
            })
            .ok();
    }
}

// ------------------------------------------------------------------ hosts

fn signing_key_from_file() -> Result<Option<Vec<u8>>, String> {
    let path = std::env::var("CORVUS_MAVLINK_SIGNING_KEY_FILE").unwrap_or_default();
    let path = path.trim();
    if path.is_empty() {
        return Ok(None);
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("MAVLink signing key path is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err("MAVLink signing key file must have mode 0600".into());
        }
        // SAFETY: getuid takes nothing and cannot fail.
        if meta.uid() != unsafe { libc::getuid() } {
            return Err("MAVLink signing key file must be owned by this user".into());
        }
    }
    let mut data = std::fs::read(path).map_err(|e| e.to_string())?;
    data.truncate(129);
    let trimmed: Vec<u8> = data.trim_ascii().to_vec();
    let key = if data.len() == 32 {
        data
    } else if trimmed.len() == 64 {
        let text = std::str::from_utf8(&trimmed)
            .map_err(|_| "MAVLink signing key must be 32 raw bytes or 64 hex characters")?;
        (0..32)
            .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| "MAVLink signing key must be 32 raw bytes or 64 hex characters".to_string())?
    } else {
        return Err("MAVLink signing key must be 32 raw bytes or 64 hex characters".into());
    };
    if key.iter().all(|b| *b == 0) {
        return Err("MAVLink signing key must not be all zero".into());
    }
    Ok(Some(key))
}

impl TlogHost for Shared {
    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    fn conn_string(&self) -> String {
        self.config.connection.clone()
    }
    fn log_dir(&self) -> std::path::PathBuf {
        self.config.log_dir.clone()
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
    fn request_version(&self) {
        self.requests.request_version(self);
    }
}

impl RtcmHost for Shared {
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
}

impl TelemetryHost for Shared {
    fn store_update(&self, fields: Map<String, Value>) {
        self.store.update(fields);
    }
    fn rc_channels(&self) -> Vec<i64> {
        self.store
            .get("rc_channels")
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()))
            .unwrap_or_default()
    }
    fn target_system(&self) -> i64 {
        i64::from(self.st().target.0)
    }
    fn lock(&self) -> &PriorityLock {
        &self.op_lock
    }
    fn set_error(&self, text: &str) {
        self.set_last_error(text);
    }
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn set_message_interval(&self, msg_id: i64, interval_us: i64) -> bool {
        self.requests.set_message_interval(self, msg_id, interval_us, false).0
    }
    fn message_interval(&self, msg_id: i64) -> i64 {
        crate::protocol::requests::message_intervals(RequestsHost::is_slow_link(self))
            .into_iter()
            .find(|(id, _)| *id == msg_id)
            .map_or(0, |(_, us)| us)
    }
    fn is_slow_link(&self) -> bool {
        RequestsHost::is_slow_link(self)
    }
    fn forget_rangefinder(&self) {
        self.st().rangefinder_seen = None;
    }
}

impl StatusHost for Shared {
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console_publish(source, text, level);
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
    fn dialect(&self) -> Dialect {
        self.st().dialect
    }
    fn store_update(&self, fields: Map<String, Value>) {
        self.store.update(fields);
    }
    fn armed(&self) -> bool {
        self.store.get("armed").is_some_and(|v| v.as_bool() == Some(true))
    }
    fn kill_switch(&self) -> bool {
        self.store.get("kill_switch").is_some_and(|v| v.as_bool() == Some(true))
    }
    fn resolve_warnings(&self, kinds: &[&str]) {
        self.store.resolve_warnings(kinds);
    }
    fn merge_warning(&self, text: &str, level: &str, event: Option<&str>) {
        self.store.merge_warning(text, level, None, MAX_WARNINGS, event);
    }
    fn kill_evidence_at(&self) -> f64 {
        self.st().kill_evidence_at
    }
    fn set_kill_evidence_at(&self, at: f64) {
        self.st().kill_evidence_at = at;
    }
    fn target(&self) -> (u8, u8) {
        self.st().target
    }
    fn has_connection(&self) -> bool {
        self.conn().is_some()
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
}

impl RequestsHost for Shared {
    fn is_slow_link(&self) -> bool {
        // A serial link is a radio until proven otherwise: guessing fast on a
        // 57 kbps SiK link saturates it.
        self.is_serial()
    }
    fn has_connection(&self) -> bool {
        self.conn().is_some()
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
    fn target(&self) -> (u8, u8) {
        self.st().target
    }
    fn request_sent(&self) -> bool {
        self.st().request_sent
    }
    fn set_request_sent(&self, sent: bool) {
        self.st().request_sent = sent;
    }
    fn dialect(&self) -> Dialect {
        self.st().dialect
    }
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn lock(&self) -> &PriorityLock {
        &self.op_lock
    }
    fn set_error(&self, text: &str) {
        self.set_last_error(text);
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console_publish(source, text, level);
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
    fn store_warning(&self, text: &str, level: &str) {
        self.store.merge_warning(text, level, None, MAX_WARNINGS, None);
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        FlightHost::command_failure(self, action, result)
    }
    fn send_command(&self, command: i64, params: &[f64], timeout: f64, retries: i64) -> i64 {
        self.receive
            .send_command_and_wait(self, command, params, timeout, retries, false, &[])
    }
    fn fetch_params(&self, names: &[&str], _timeout: f64) -> Vec<(String, f64)> {
        // The parameter engine is not wired to the bridge yet.
        let _ = names;
        Vec::new()
    }
    fn schedule_version_retry(&self) {
        if let Some(me) = self.me.upgrade() {
            me.schedule_version_retry_thread();
        }
    }
    fn store_update(&self, fields: Map<String, Value>) {
        self.store.update(fields);
    }
    fn load_signing_key(&self) -> Result<Option<Vec<u8>>, String> {
        signing_key_from_file()
    }
    fn wire_is_mavlink2(&self) -> bool {
        self.conn().is_none_or(|c| c.proto() == Proto::V2)
    }
    fn switch_to_mavlink2(&self) -> Result<(), String> {
        if let Some(c) = self.conn() {
            c.set_proto(Proto::V2);
        }
        Ok(())
    }
    fn setup_signing(&self, key: &[u8]) -> Result<(), String> {
        let key: [u8; 32] = key.try_into().map_err(|_| "a signing key is 32 bytes".to_string())?;
        // 10 microsecond units since 2015-01-01.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs_f64();
        let stamp = ((now - 1_420_070_400.0) * 100_000.0) as u64;
        if let Some(c) = self.conn() {
            c.set_signing(Some(Signing::new(key, 0, stamp)));
        }
        Ok(())
    }
    fn conn_string(&self) -> String {
        self.config.connection.clone()
    }
}

impl FlightHost for Shared {
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn set_error(&self, text: &str) {
        self.set_last_error(text);
    }
    fn last_error(&self) -> String {
        self.st().last_error.clone()
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console_publish(source, text, level);
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
    fn store_warning(&self, text: &str, level: &str) {
        self.store.merge_warning(text, level, None, MAX_WARNINGS, None);
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        let text = format!("{action} failed: {}", crate::protocol::common::result_text(result));
        self.set_last_error(&text);
        self.console_publish(&action.to_uppercase(), &text, "error");
        let level = if matches!(result, -3 | -2 | -1 | 1) {
            "warning"
        } else {
            "critical"
        };
        self.store.merge_warning(&text, level, None, MAX_WARNINGS, None);
        false
    }
    fn send_command(&self, command: i64, params: &[f64], timeout: f64, retries: i64, accept_in_progress: bool) -> i64 {
        self.receive
            .send_command_and_wait(self, command, params, timeout, retries, accept_in_progress, &[])
    }
    fn send_command_int(
        &self,
        command: i64,
        frame: i64,
        params: &[f64],
        x: i64,
        y: i64,
        z: f64,
        timeout: f64,
        retries: i64,
    ) -> i64 {
        self.receive
            .send_command_int_and_wait(self, command, frame, params, x, y, z, timeout, retries)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
    fn target(&self) -> (u8, u8) {
        self.st().target
    }
    fn lock(&self) -> &PriorityLock {
        &self.op_lock
    }
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn sleep(&self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
    }
    fn stopping(&self) -> bool {
        self.stopping()
    }
    fn snapshot(&self) -> FlightSnapshot {
        let get = |k: &str| self.store.get(k);
        FlightSnapshot {
            connected: get("connected").is_some_and(|v| v.as_bool() == Some(true)),
            armed: get("armed").is_some_and(|v| v.as_bool() == Some(true)),
            mode: get("mode")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            landed_state: get("landed_state").and_then(|v| v.as_i64()),
            altitude_agl: get("altitude_agl").and_then(|v| v.as_f64()).unwrap_or(0.0),
        }
    }
    fn is_stale(&self, timeout: f64) -> bool {
        self.store.is_stale(timeout)
    }
    fn dialect(&self) -> Dialect {
        self.st().dialect
    }
    fn mav_type(&self) -> i64 {
        self.st().mav_type
    }
    fn mode_command(&self, mode: &str) -> Option<ModeCommand> {
        self.st().mode_values.iter().find(|(n, _)| n == mode).map(|(_, c)| *c)
    }
    fn has_modes(&self) -> bool {
        !self.st().mode_values.is_empty()
    }
    fn modes_unsupported(&self) -> bool {
        self.st().dialect.stack() == Stack::Generic
    }
    fn home_alt_amsl(&self) -> Option<f64> {
        self.st().home_alt
    }
    fn position_home_alt_amsl(&self) -> Option<f64> {
        self.st().position_home_alt
    }
    fn request_home(&self) {
        self.requests.request_home(self);
    }
    fn validate_fly_to_points(&self, points: &Value) -> Option<Vec<(f64, f64, f64)>> {
        let ground = crate::dialect::is_ground_vehicle(self.st().mav_type);
        validate_fly_to_points(points, ground, &|text| self.set_last_error(text))
    }
    fn takeoff_item_position(&self, first: (f64, f64, f64)) -> (f64, f64) {
        takeoff_item_position(first, &self.store_list("position"), &self.store_list("home"))
    }
    fn build_mission_item_spec(
        &self,
        seq: i64,
        command: i64,
        lat: f64,
        lon: f64,
        alt: f64,
        p1: f64,
        p2: f64,
        p3: f64,
        p4: f64,
    ) -> MissionItem {
        build_mission_item_spec(seq, command, lat, lon, alt, p1, p2, p3, p4, None)
    }
    fn with_mission_home_slot(&self, items: Vec<MissionItem>) -> Vec<MissionItem> {
        let (dialect, home_alt) = {
            let st = self.st();
            (st.dialect, st.home_alt)
        };
        with_mission_home_slot(items, &dialect, &self.store_list("home"), home_alt)
    }
    fn upload_mission(&self, _items: &[MissionItem]) -> i64 {
        // The mission engine is not wired to the bridge yet.
        -2
    }
    fn mission_ack_to_result(&self, ack: i64) -> i64 {
        mission_ack_to_result(ack)
    }
    fn note_vehicle_mission_cleared(&self) {}
}

impl Shared {
    fn store_list(&self, key: &str) -> Vec<f64> {
        self.store
            .get(key)
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()))
            .unwrap_or_default()
    }
}

impl ReceiveHost for Shared {
    fn target(&self) -> (u8, u8) {
        self.st().target
    }
    fn link_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    fn has_connection(&self) -> bool {
        self.conn().is_some()
    }
    fn source_ids(&self) -> (i64, i64) {
        (i64::from(GCS_SYSTEM_ID), i64::from(GCS_COMPONENT_ID))
    }
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
    fn lock(&self) -> &PriorityLock {
        &self.op_lock
    }
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console_publish(source, text, level);
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
    fn store_heartbeat(&self) -> f64 {
        self.store.heartbeat()
    }
    fn store_update(&self, fields: Map<String, Value>) {
        self.store.update(fields);
    }
    fn store_flags(&self) -> StoreFlags {
        let flag = |k: &str| self.store.get(k).is_some_and(|v| v.as_bool() == Some(true));
        StoreFlags {
            armed: flag("armed"),
            kill_switch: flag("kill_switch"),
            flight_termination: flag("flight_termination"),
        }
    }
    fn resolve_warnings(&self, kinds: &[&str]) {
        self.store.resolve_warnings(kinds);
    }
    fn armed_now(&self) -> bool {
        self.store.get("armed").is_some_and(|v| v.as_bool() == Some(true))
    }
    fn dialect(&self) -> Dialect {
        self.st().dialect
    }
    fn latch_dialect(&self, autopilot: i64, mav_type: i64) -> bool {
        let mut st = self.st();
        let dialect = dialect_for_id(autopilot);
        let changed = dialect != st.dialect || mav_type != st.mav_type;
        if dialect != st.dialect {
            // Another stack's release number says nothing about this one.
            st.firmware = None;
        }
        st.dialect = dialect;
        st.mav_type = mav_type;
        changed
    }
    fn build_mode_mapping(&self) {
        self.rebuild_mode_mapping();
    }
    fn firmware_version(&self) -> Option<(i64, i64, i64)> {
        self.st().firmware
    }
    fn set_firmware_version(&self, version: (i64, i64, i64)) {
        self.st().firmware = Some(version);
    }
    fn set_board_ids(&self, vendor: i64, product: i64) {
        self.st().board = (vendor, product);
    }
    fn set_param_encoding_declared(&self, encoding: &str) {
        self.st().param_encoding_declared = encoding.into();
    }
    fn rangefinder_seen(&self) -> Option<f64> {
        self.st().rangefinder_seen
    }
    fn set_rangefinder_seen(&self, at: Option<f64>) {
        self.st().rangefinder_seen = at;
    }
    fn degraded_warned(&self) -> bool {
        self.st().degraded_warned
    }
    fn set_degraded_warned(&self, warned: bool) {
        self.st().degraded_warned = warned;
    }
    fn warn_timeout(&self) -> f64 {
        Shared::warn_timeout(self)
    }
    fn set_kill_evidence(&self, at: f64) {
        self.st().kill_evidence_at = at;
    }
    fn last_boot_ms(&self) -> Option<i64> {
        self.st().last_boot_ms
    }
    fn set_last_boot_ms(&self, ms: i64) {
        self.st().last_boot_ms = Some(ms);
    }
    fn set_position_home_alt_amsl(&self, metres: f64) {
        self.st().position_home_alt = Some(metres);
    }
    fn set_home_alt_amsl(&self, metres: f64) {
        self.st().home_alt = Some(metres);
    }
    fn observe_fleet(&self, frame: &Frame, vehicle_heartbeat: bool) {
        self.fleet_tracker.observe(frame, vehicle_heartbeat, None);
    }
    fn write_tlog(&self, frame: &Frame) {
        self.tlog.write_tlog(frame.raw);
    }
    fn record_scope(&self, _frame: &Frame) {}
    fn handle_autotune_ack(&self, result: i64, progress: i64) {
        self.engines.setup.handle_autotune_ack(self, result, progress);
    }
    fn handle_mission_request(&self, msg: &Message, as_int: bool) {
        self.engines.missions.handle_mission_request(self, msg, as_int);
    }
    fn handle_mission_ack(&self, msg: &Message) {
        self.engines.missions.handle_mission_ack(self, msg);
    }
    fn handle_mission_count(&self, msg: &Message) {
        self.engines.missions.handle_mission_count(self, msg);
    }
    fn handle_mission_item(&self, msg: &Message, as_int: bool) {
        self.engines.missions.handle_mission_item(self, msg, as_int);
    }
    fn handle_mission_current(&self, msg: &Message) {
        self.engines.missions.handle_mission_current(self, msg);
    }
    fn handle_mission_item_reached(&self, msg: &Message) {
        self.engines.missions.handle_mission_item_reached(self, msg);
    }
    fn handle_serial_control(&self, msg: &Message) {
        self.engines.shell.handle(self, msg);
    }
    fn handle_radio_status(&self, msg: &Message) {
        self.telemetry.handle_radio_status(self, msg);
    }
    fn handle_gnss_integrity(&self, frame: &Frame) {
        match &frame.payload {
            Payload::Message(m) => self.telemetry.handle_gnss_integrity(self, m, frame.system),
            Payload::Unknown { data, .. } => self.telemetry.handle_gnss_integrity_frame(self, data),
        }
    }
    fn handle_remote_id_arm_status(&self, msg: &Message) {
        self.engines.remote_id.handle_arm_status(msg);
    }
    fn handle_battery_status(&self, msg: &Message) {
        self.telemetry.handle_battery_status(self, msg);
    }
    fn handle_statustext(&self, msg: &Message) {
        // The frame's sender is not carried here; one link is one vehicle.
        let (system, component) = self.st().target;
        self.status.handle_statustext(
            self,
            &StatusText {
                msg,
                system: i64::from(system),
                component: i64::from(component),
            },
        );
    }
    fn handle_param_value(&self, msg: &Message) {
        self.engines.params.handle_param_value(self, msg, None);
    }
    fn handle_ftp_message(&self, msg: &Message) {
        let get = |k: &str| msg.get(k).and_then(Wv::as_i64).unwrap_or(0) as u8;
        let payload: Vec<u8> = match msg.get("payload") {
            Some(Wv::Array(items)) => items.iter().filter_map(Wv::as_i64).map(|v| v as u8).collect(),
            _ => Vec::new(),
        };
        self.engines
            .ftp
            .handle_message(self, get("target_system"), get("target_component"), &payload);
    }
    fn handle_rc_channels(&self, msg: &Message, raw: bool) {
        self.telemetry.handle_rc_channels(self, msg, raw);
    }
    fn handle_attitude_target(&self, msg: &Message) {
        self.telemetry.handle_attitude_target(self, msg);
    }
    fn handle_position_target(&self, msg: &Message) {
        self.telemetry.handle_position_target(self, msg);
    }
    fn log_sink(&self, _msg: &Message) -> Result<bool, String> {
        Ok(false)
    }
    fn note_heartbeat(&self, stamp: f64) {
        self.telemetry.note_heartbeat(stamp);
    }
    fn publish_heartbeat_jitter(&self) {
        self.telemetry.publish_heartbeat_jitter(self);
    }
    fn publish_link_quality(&self) {
        self.telemetry.publish_link_quality(self);
    }
    fn battery_fields(&self, voltage: f64, current: f64, reported: i64) -> Map<String, Value> {
        self.telemetry.battery_fields(voltage, current, reported)
    }
    fn battery_endurance_fields(&self, percent: Option<f64>, source: &str, armed: bool) -> Map<String, Value> {
        self.telemetry
            .battery_endurance_fields(percent, source, armed, clock::monotonic())
    }
    fn note_prearm_state(&self, prearm: Option<bool>) {
        self.status.note_prearm_state(self, prearm);
    }
    fn has_prearm_reasons(&self) -> bool {
        self.status.has_prearm_reasons()
    }
    fn clear_prearm_reasons(&self) {
        self.status.clear_prearm_reasons(self);
    }
    fn roll_tlog_on_disarm(&self, armed: bool) {
        if let Some(me) = self.me.upgrade() {
            self.tlog.roll_tlog_on_disarm(&(me as Arc<dyn TlogHost>), armed);
        }
    }
    fn reset_parameter_cache(&self) {
        self.engines.params.reset();
    }
    fn reset_param_metadata(&self) {}
    fn set_request_sent(&self, sent: bool) {
        self.st().request_sent = sent;
    }
    fn is_serial(&self) -> bool {
        Shared::is_serial(self)
    }
    fn request_streams(&self) {
        self.requests.request_streams(self);
    }
    fn request_version(&self) {
        self.requests.request_version(self);
    }
    fn request_home(&self) {
        self.requests.request_home(self);
    }
    fn schedule_message_intervals(&self) {
        if let Some(me) = self.me.upgrade() {
            me.schedule_message_intervals();
        }
    }
    fn cancel_mission_upload(&self) {
        self.engines.missions.cancel_upload();
    }
}

// ---------------------------------------------------- the other protocols

use crate::protocol::ftp::{Cancel, FtpEngine, FtpError, FtpHost, FtpLimits};
use crate::protocol::logs::LogEngine;
use crate::protocol::missions::{ConsoleSub, DownloadedItem, MissionEngine, MissionHost, PlanItem};
use crate::protocol::params::{Entry, ParamEngine, ParamHost, Status};
use crate::protocol::remote_id::{RemoteIdEngine, RemoteIdHost};
use crate::protocol::setup::{SetupEngine, SetupHost};
use crate::protocol::shell::{ShellEngine, ShellHost, ShellLimits};
use crate::protocol::RequestHost;

type ParamListener = Box<dyn Fn(&Status) + Send + Sync>;

/// The engines of the protocols that are not part of the receive path proper.
pub struct Engines {
    pub params: Arc<ParamEngine>,
    pub logs: LogEngine,
    pub shell: ShellEngine,
    pub ftp: FtpEngine,
    pub missions: MissionEngine,
    pub setup: SetupEngine,
    pub remote_id: RemoteIdEngine,
    param_listeners: Mutex<Vec<ParamListener>>,
    watchdog: Mutex<Option<JoinHandle<()>>>,
    rx_source: Mutex<(i64, i64)>,
}

impl Engines {
    fn new() -> Engines {
        Engines {
            params: Arc::new(ParamEngine::new()),
            logs: LogEngine::new(),
            shell: ShellEngine::new(ShellLimits {
                line_max_chars: 8192,
                backlog_chars: 65_536,
                write_max_chars: 4096,
            }),
            ftp: FtpEngine::new(FtpLimits::default()),
            missions: MissionEngine::new(),
            setup: SetupEngine::new(),
            remote_id: RemoteIdEngine::new(),
            param_listeners: Mutex::new(Vec::new()),
            watchdog: Mutex::new(None),
            rx_source: Mutex::new((0, 0)),
        }
    }
}

impl Shared {
    /// Is this frame addressed to us from the vehicle we talk to? The source of
    /// the frame being dispatched is the one the receive thread noted.
    fn frame_is_for_us(&self, msg: &Message) -> bool {
        if self.conn().is_none() {
            return false;
        }
        let (system, component) = *self.engines.rx_source.lock().unwrap_or_else(|e| e.into_inner());
        let target = self.st().target;
        if system != 0 && system != i64::from(target.0) {
            return false;
        }
        if component != 0 && component != i64::from(target.1) {
            return false;
        }
        let get = |k: &str| msg.get(k).and_then(Wv::as_i64).unwrap_or(0);
        let (to_system, to_component) = (get("target_system"), get("target_component"));
        let (mine_system, mine_component) = (i64::from(GCS_SYSTEM_ID), i64::from(GCS_COMPONENT_ID));
        !(to_system != 0 && to_system != mine_system) && !(to_component != 0 && to_component != mine_component)
    }
}

impl RequestHost for Shared {
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn armed(&self) -> bool {
        self.store.get("armed").is_some_and(|v| v.as_bool() == Some(true))
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
    fn target(&self) -> (u8, u8) {
        self.st().target
    }
    fn set_error(&self, text: &str) {
        self.set_last_error(text);
    }
    fn last_error(&self) -> String {
        self.st().last_error.clone()
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console_publish(source, text, level);
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
}

impl ParamHost for Shared {
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn sleep(&self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
    }
    fn interruptible_sleep(&self, seconds: f64) {
        Shared::interruptible_sleep(self, seconds);
    }
    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    fn stopping(&self) -> bool {
        Shared::stopping(self)
    }
    fn encoding(&self) -> String {
        let st = self.st();
        if st.param_encoding_declared.is_empty() {
            st.dialect.param_encoding().to_string()
        } else {
            st.param_encoding_declared.clone()
        }
    }
    fn slow_link(&self) -> bool {
        self.is_serial()
    }
    fn notify(&self, status: &Status) {
        for f in self
            .engines
            .param_listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            f(status);
        }
    }
    fn start_watchdog(&self) {
        let Some(me) = self.me.upgrade() else { return };
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        let mut slot = self.engines.watchdog.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let engine = Arc::clone(&self.engines.params);
        *slot = std::thread::Builder::new()
            .name("param-watchdog".into())
            .spawn(move || engine.run_watchdog(me.as_ref()))
            .ok();
    }
}

impl ShellHost for Shared {
    fn supports_shell(&self) -> bool {
        self.st().dialect.supports_shell()
    }
    fn label(&self) -> String {
        self.st().dialect.label().to_string()
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        FlightHost::command_failure(self, action, result)
    }
    fn ack_is_for_us(&self, msg: &Message) -> bool {
        self.frame_is_for_us(msg)
    }
}

impl FtpHost for Shared {
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn slow_link(&self) -> bool {
        self.is_serial()
    }
    fn stopping(&self) -> bool {
        Shared::stopping(self)
    }
    fn connected(&self) -> bool {
        self.conn().is_some()
    }
    fn send_ftp(&self, payload: &[u8]) -> Result<bool, String> {
        let (system, component) = self.st().target;
        let mut data: Vec<Wv> = payload.iter().map(|b| int(i64::from(*b))).collect();
        data.resize(251, int(0));
        let msg = Message::named("FILE_TRANSFER_PROTOCOL", corvus_link::plain::defs::Wire::V2)
            .ok_or("this dialect has no FILE_TRANSFER_PROTOCOL")?
            .with("target_network", int(0))
            .with("target_system", int(i64::from(system)))
            .with("target_component", int(i64::from(component)))
            .with("payload", Wv::Array(data));
        self.do_send(msg)
    }
    fn source(&self) -> (u8, u8) {
        (GCS_SYSTEM_ID, GCS_COMPONENT_ID)
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
}

impl SetupHost for Shared {
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn set_error(&self, text: &str) {
        self.set_last_error(text);
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console_publish(source, text, level);
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        FlightHost::command_failure(self, action, result)
    }
    fn lock(&self) -> &PriorityLock {
        &self.op_lock
    }
    fn snapshot(&self) -> FlightSnapshot {
        FlightHost::snapshot(self)
    }
    fn autotune_state(&self) -> String {
        self.store
            .get("autotune_state")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }
    fn dialect(&self) -> Dialect {
        self.st().dialect
    }
    fn mav_type(&self) -> i64 {
        self.st().mav_type
    }
    fn send_command(
        &self,
        command: i64,
        params: &[f64],
        timeout: f64,
        retries: i64,
        accept_in_progress: bool,
        interim: &[i64],
    ) -> i64 {
        self.receive
            .send_command_and_wait(self, command, params, timeout, retries, accept_in_progress, interim)
    }
    fn update_autotune(&self, state: &str, progress: Option<i64>) {
        let mut update = vec![("autotune_state", json!(state))];
        if let Some(p) = progress {
            update.push(("autotune_progress", json!(p)));
        }
        self.store.update(update);
    }
    fn merge_warning(&self, text: &str, level: &str) {
        self.store.merge_warning(text, level, None, MAX_WARNINGS, None);
    }
    fn set_mode(&self, mode: &str) -> bool {
        self.flight.set_mode(self, mode)
    }
    fn has_mode(&self, mode: &str) -> bool {
        self.st().mode_values.iter().any(|(n, _)| n == mode)
    }
    fn reset_parameter_cache(&self) {
        self.engines.params.reset();
    }
}

impl RemoteIdHost for Shared {
    fn link_supports_remote_id(&self) -> bool {
        self.conn().is_some_and(|c| c.proto() == Proto::V2)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
    fn target(&self) -> (u8, u8) {
        self.st().target
    }
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn unix_now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

impl MissionHost for Shared {
    fn connection_ready(&self) -> bool {
        self.link_ready()
    }
    fn set_error(&self, text: &str) {
        self.set_last_error(text);
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console_publish(source, text, level);
    }
    fn log(&self, level: &str, text: &str) {
        self.log_line(level, text);
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        FlightHost::command_failure(self, action, result)
    }
    fn lock(&self) -> &PriorityLock {
        &self.op_lock
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        self.do_send(msg)
    }
    fn target(&self) -> (u8, u8) {
        self.st().target
    }
    fn ack_is_for_us(&self, msg: &Message) -> bool {
        self.frame_is_for_us(msg)
    }
    fn now(&self) -> f64 {
        clock::monotonic()
    }
    fn stopping(&self) -> bool {
        Shared::stopping(self)
    }
    fn is_slow_link(&self) -> bool {
        self.is_serial()
    }
    fn dialect(&self) -> Dialect {
        self.st().dialect
    }
    fn mav_type(&self) -> i64 {
        self.st().mav_type
    }
    fn store_update(&self, fields: Map<String, Value>) {
        self.store.update(fields);
    }
    fn home(&self) -> Vec<f64> {
        self.store_list("home")
    }
    fn home_alt_amsl(&self) -> Option<f64> {
        self.st().home_alt
    }
    fn add_console_sub(&self, sub: ConsoleSub) -> u64 {
        ReceiveHostConsole::add(self, sub)
    }
    fn remove_console_sub(&self, id: u64) {
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(i, _)| *i != id);
    }
    fn lower_circles(&self, items: &[PlanItem]) -> Vec<PlanItem> {
        crate::mission::lower_circles(items)
    }
    fn items_to_plan(&self, items: &[DownloadedItem], home: Option<(f64, f64)>, home_alt: Option<f64>) -> Value {
        crate::mission::items_to_plan(items, home, home_alt, "Vehicle mission")
    }
}

impl Bridge {
    /// The engines of the other protocols, for the callers that drive them.
    pub fn engines(&self) -> &Engines {
        &self.shared.engines
    }

    /// Adds a listener to the parameter engine's progress and values.
    pub fn add_param_listener(&self, f: impl Fn(&Status) + Send + Sync + 'static) {
        self.shared
            .engines
            .param_listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(f));
    }

    /// Starts the full parameter download.
    pub fn request_param_list(&self) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared.engines.params.request_param_list(self.shared.as_ref())
    }

    pub fn param_status(&self) -> Status {
        self.shared.engines.params.status()
    }

    pub fn params(&self) -> Vec<Entry> {
        self.shared.engines.params.params()
    }

    /// A confirmed write of one parameter.
    pub fn set_param(&self, name: &str, value: f64) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared.engines.params.set_param(self.shared.as_ref(), name, value)
    }

    pub fn fetch_params(&self, names: &[String], timeout: f64) -> Vec<(String, f64)> {
        self.shared
            .engines
            .params
            .fetch_params(self.shared.as_ref(), names, timeout, false)
            .into_iter()
            .collect()
    }

    pub fn shell_command(&self, text: &str) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared.engines.shell.send_command(self.shared.as_ref(), text)
    }

    pub fn shell_write(&self, text: &str) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared.engines.shell.write(self.shared.as_ref(), text)
    }

    pub fn shell_stop(&self) {
        self.shared.engines.shell.stop(self.shared.as_ref());
    }

    pub fn log_request_list(&self, start: i64, end: i64) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared.engines.logs.request_list(self.shared.as_ref(), start, end)
    }

    pub fn log_request_data(&self, id: i64, offset: i64, count: i64) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared
            .engines
            .logs
            .request_data(self.shared.as_ref(), id, offset, count)
    }

    pub fn log_erase(&self) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared.engines.logs.erase(self.shared.as_ref())
    }

    pub fn log_request_end(&self) -> bool {
        let _gate = self.shared.op_lock.lock();
        self.shared.engines.logs.request_end(self.shared.as_ref())
    }

    pub fn ftp_read_file(&self, path: &str, cancel: Option<Cancel>) -> Result<Vec<u8>, FtpError> {
        self.shared
            .engines
            .ftp
            .read_file(self.shared.as_ref(), path, None, cancel)
    }

    pub fn ftp_file_crc32(&self, path: &str) -> Result<u32, FtpError> {
        self.shared.engines.ftp.file_crc32(self.shared.as_ref(), path, None)
    }

    pub fn calibrate(&self, sensor: &str) -> bool {
        self.shared.engines.setup.calibrate(self.shared.as_ref(), sensor)
    }

    pub fn cancel_calibration(&self) -> bool {
        self.shared.engines.setup.cancel_calibration(self.shared.as_ref())
    }

    pub fn motor_test(&self, motor: i64, throttle_pct: f64, duration_s: f64) -> bool {
        self.shared
            .engines
            .setup
            .motor_test(self.shared.as_ref(), motor, throttle_pct, duration_s)
    }

    pub fn stop_motor_test(&self) -> bool {
        self.shared.engines.setup.stop_motor_test(self.shared.as_ref())
    }

    pub fn autotune(&self, axis: &str, enable: bool) -> bool {
        self.shared.engines.setup.autotune(self.shared.as_ref(), axis, enable)
    }

    pub fn reboot_autopilot(&self) -> bool {
        self.shared.engines.setup.reboot_autopilot(self.shared.as_ref())
    }

    /// The plan items with every Circle flown as waypoints when the stack
    /// refuses `NAV_LOITER_TURNS` in a mission.
    pub fn lower_mission_plan(&self, items: &[PlanItem]) -> Vec<PlanItem> {
        self.shared
            .engines
            .missions
            .lower_mission_plan(self.shared.as_ref(), items)
    }

    /// The area last put on the vehicle over this link, `Some(empty)` after a
    /// clear, None when this link has not transferred one.
    pub fn vehicle_fence(&self) -> Option<Vec<(f64, f64)>> {
        self.shared.engines.missions.vehicle_fence()
    }

    /// The MAV_TYPE of the vehicle on this link, 0 before one is heard.
    pub fn vehicle_type_id(&self) -> i64 {
        self.shared.st().mav_type
    }

    pub fn upload_mission_plan(&self, items: &[PlanItem]) -> bool {
        self.shared
            .engines
            .missions
            .upload_mission_plan(self.shared.as_ref(), items)
    }

    pub fn download_mission(&self) -> Option<Value> {
        self.shared.engines.missions.download_mission(self.shared.as_ref())
    }

    pub fn clear_mission(&self) -> bool {
        self.shared.engines.missions.clear_mission(self.shared.as_ref())
    }

    pub fn start_mission(&self, count: i64) -> bool {
        self.shared.flight.start_mission(self.shared.as_ref(), count)
    }

    pub fn fly_to_points(&self, points: &Value) -> bool {
        self.shared.flight.fly_to_points(self.shared.as_ref(), points)
    }

    pub fn upload_fence(&self, polygon: &[(f64, f64)]) -> bool {
        self.shared.engines.missions.upload_fence(self.shared.as_ref(), polygon)
    }

    pub fn clear_fence(&self) -> bool {
        self.shared.engines.missions.clear_fence(self.shared.as_ref())
    }

    pub fn set_remote_id(&self, raw: Option<&Value>) -> Value {
        self.shared.engines.remote_id.set_remote_id(raw)
    }

    pub fn remote_id_status(&self) -> Value {
        self.shared.engines.remote_id.remote_id_status(self.shared.as_ref())
    }

    /// Stops the parameter watchdog with the rest; called from `stop`.
    fn join_workers(&self) {
        self.shared.engines.params.abort();
        join_bounded(
            self.shared
                .engines
                .watchdog
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
            Duration::from_secs(2),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_strings_are_validated_like_python() {
        assert_eq!(
            validate_connection(""),
            Err("connection must be a non-empty string".into())
        );
        assert_eq!(
            validate_connection("http://x"),
            Err("connection must start with one of: udp:, udpin:, udpout:, udpbcast:, tcp:, tcpin:, serial:".into())
        );
        assert_eq!(
            validate_connection("serial:"),
            Err("serial connection requires a device path".into())
        );
        assert_eq!(
            validate_connection("serial:/dev/x:0"),
            Err("serial connection requires a numeric baud rate".into())
        );
        assert!(validate_connection("serial:/dev/ttyUSB0:57600").is_ok());
        assert!(validate_connection("udpin:0.0.0.0:14550").is_ok());
    }

    #[test]
    fn serial_strings_split_like_python() {
        assert_eq!(
            split_serial_connection("serial:/dev/ttyACM0:921600"),
            ("/dev/ttyACM0".to_string(), 921_600)
        );
        assert_eq!(
            split_serial_connection("serial:/dev/ttyACM0"),
            ("/dev/ttyACM0".to_string(), DEFAULT_SERIAL_BAUD)
        );
        assert_eq!(
            split_serial_connection("serial:/dev/serial/by-path/pci-0:1.2:0"),
            ("/dev/serial/by-path/pci-0:1.2".to_string(), 0)
        );
        assert_eq!(
            split_serial_connection("serial:COM3:abc"),
            ("COM3:abc".to_string(), DEFAULT_SERIAL_BAUD)
        );
        assert_eq!(split_serial_connection("serial:"), (String::new(), DEFAULT_SERIAL_BAUD));
    }

    #[test]
    fn the_backoff_doubles_to_a_cap_with_jitter() {
        assert_eq!(reconnect_delay(1, 0.0), 0.5);
        assert_eq!(reconnect_delay(2, 0.0), 1.0);
        assert_eq!(reconnect_delay(3, 0.0), 2.0);
        assert_eq!(reconnect_delay(5, 0.0), 8.0);
        assert_eq!(reconnect_delay(9, 0.0), 8.0);
        assert_eq!(reconnect_delay(1, 1.0), 0.625);
        assert_eq!(reconnect_delay(1, -1.0), 0.375);
        assert!(reconnect_delay(0, -1.0) >= RECONNECT_MIN_S);
        for _ in 0..50 {
            let d = reconnect_delay(3, jitter_unit());
            assert!((1.5..=2.5).contains(&d), "{d}");
        }
    }
}
