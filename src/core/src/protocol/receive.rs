//! The bridge's receive path and command transaction: the duplicate frame
//! filter, the dispatcher that routes every message to its handler and writes
//! the aircraft's own state, the check that a frame comes from the vehicle this
//! link flies, and the COMMAND_LONG / COMMAND_INT waits for their COMMAND_ACK.
//!
//! The twin of `corvus_link`'s receive engine (`bridge/receive.rs`). The state
//! it decides on stays with the bridge (the store, the dialect, the engines of
//! the other protocols); `ReceiveHost` is the bridge's side of that, named
//! after what Python called on it. The engine owns what only it reads: the
//! duplicate window, the ACK registry and the systems it already told the
//! console about.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use corvus_link::plain::defs::constant_or;
use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Map, Value};

use super::common::{known_result_text, py_round, Event};
use super::targeted;
use crate::dialect::Dialect;
use crate::priority_lock::PriorityLock;

/// A copy of a frame this soon after the first is dropped before anything reads it.
pub const DUPLICATE_FRAME_WINDOW_S: f64 = 0.3;
/// The ground station's own MAVLink system id.
pub const GCS_SYSTEM_ID: i64 = 254;
pub const RECV_ERROR_BACKOFF_S: f64 = 0.05;
pub const RECV_ERROR_LIMIT: i64 = 40;
/// An uptime that steps back by more than this is a reboot, not jitter.
pub const VEHICLE_REBOOT_BACKSTEP_MS: i64 = 5000;
const CAP_PARAM_ENCODE_BYTEWISE: i64 = 16;
const CAP_PARAM_ENCODE_C_CAST: i64 = 131_072;
const DOWNWARD_ORIENTATION: i64 = 25;
const GPS_SENSOR_BIT: i64 = 32;
const HEARTBEAT_LIMITS: [(&str, i64); 3] = [("type", 128), ("autopilot", 64), ("system_status", 32)];
const NON_AUTOPILOT_COMPONENTS: [i64; 4] = [240, 154, 68, 191];
const NON_VEHICLE_TYPES: [i64; 13] = [33, 36, 37, 6, 38, 39, 40, 41, 42, 18, 26, 27, 30];
const PREARM_BIT: i64 = 268_435_456;
const RANGEFINDER_STALE_S: f64 = 3.0;
const IGNORED_SYSTEMS_MAX: usize = 16;

const GPS_FIX: [&str; 9] = [
    "NO_GPS",
    "NO_FIX",
    "2D_FIX",
    "3D_FIX",
    "DGPS",
    "RTK_FLOAT",
    "RTK_FIXED",
    "STATIC",
    "PPP",
];
const MAV_AUTOPILOT: &[(i64, &str)] = &[
    (0, "GENERIC"),
    (1, "RESERVED"),
    (2, "SLUGS"),
    (3, "ARDUPILOTMEGA"),
    (4, "OPENPILOT"),
    (8, "INVALID"),
    (9, "PPZ"),
    (10, "UDB"),
    (11, "FP"),
    (12, "PX4"),
    (13, "SMACCMPILOT"),
    (14, "AUTOQUAD"),
    (15, "ARMAZILA"),
    (16, "AEROB"),
    (17, "ASLUAV"),
    (18, "SMARTAP"),
    (19, "AIRRAILS"),
    (20, "REFLEX"),
];
const MAV_TYPE: &[(i64, &str)] = &[
    (0, "GENERIC"),
    (1, "FIXED_WING"),
    (2, "QUADROTOR"),
    (3, "COAXIAL"),
    (4, "HELICOPTER"),
    (7, "AIRSHIP"),
    (8, "FREE_BALLOON"),
    (9, "ROCKET"),
    (10, "GROUNDED_ROVER"),
    (11, "SURFACE_BOAT"),
    (12, "SUBMARINE"),
    (13, "HEXAROTOR"),
    (14, "OCTOROTOR"),
    (15, "TRIROTOR"),
    (19, "VTOL_DUOROTOR"),
    (20, "VTOL_QUADROTOR"),
    (21, "VTOL_TILTROTOR"),
];

/// One received frame: the message (or the bytes of one this dialect does not
/// know) and who sent it. Source ids of 0 mean the frame carries none.
pub enum Payload<'a> {
    Message(&'a Message),
    Unknown { id: u32, data: &'a [u8] },
}

pub struct Frame<'a> {
    pub payload: Payload<'a>,
    pub system: i64,
    pub component: i64,
    /// The frame as it came off the wire, for the duplicate filter.
    pub raw: Option<&'a [u8]>,
}

impl Frame<'_> {
    pub fn name(&self) -> String {
        match &self.payload {
            Payload::Message(m) => m.name().to_string(),
            Payload::Unknown { id, .. } => format!("UNKNOWN_{id}"),
        }
    }

    fn message(&self) -> Option<&Message> {
        match &self.payload {
            Payload::Message(m) => Some(m),
            Payload::Unknown { .. } => None,
        }
    }
}

/// The store flags the heartbeat branch reads back.
#[derive(Debug, Clone, Copy, Default)]
pub struct StoreFlags {
    pub armed: bool,
    pub kill_switch: bool,
    pub flight_termination: bool,
}

/// What the receive path asks of the bridge it runs on.
pub trait ReceiveHost: Send + Sync {
    // The link and its identity.
    fn target(&self) -> (u8, u8);
    /// The link as it was when asked: a different number means it was swapped.
    fn link_generation(&self) -> u64;
    fn has_connection(&self) -> bool;
    /// The link's own system and component ids, as the ACK check wants them.
    fn source_ids(&self) -> (i64, i64);
    fn connection_ready(&self) -> bool;
    fn send(&self, msg: Message) -> Result<bool, String>;
    fn lock(&self) -> &PriorityLock;
    fn wait_event(&self, event: &Event, timeout: f64) -> bool {
        event.wait(timeout)
    }
    fn now(&self) -> f64;
    fn console(&self, source: &str, text: &str, level: &str);
    fn log(&self, level: &str, text: &str);
    // The store.
    fn store_heartbeat(&self) -> f64;
    fn store_update(&self, fields: Map<String, Value>);
    fn store_flags(&self) -> StoreFlags;
    fn resolve_warnings(&self, kinds: &[&str]);
    fn armed_now(&self) -> bool;
    // The dialect and what hangs on it.
    fn dialect(&self) -> Dialect;
    /// Re-reads the stack from the heartbeat; true when it changed.
    fn latch_dialect(&self, autopilot: i64, mav_type: i64) -> bool;
    fn build_mode_mapping(&self);
    fn firmware_version(&self) -> Option<(i64, i64, i64)>;
    fn set_firmware_version(&self, version: (i64, i64, i64));
    fn set_board_ids(&self, vendor: i64, product: i64);
    fn set_param_encoding_declared(&self, encoding: &str);
    // Timers and flags the bridge keeps.
    fn rangefinder_seen(&self) -> Option<f64>;
    fn set_rangefinder_seen(&self, at: Option<f64>);
    fn degraded_warned(&self) -> bool;
    fn set_degraded_warned(&self, warned: bool);
    fn warn_timeout(&self) -> f64;
    fn set_kill_evidence(&self, at: f64);
    fn last_boot_ms(&self) -> Option<i64>;
    fn set_last_boot_ms(&self, ms: i64);
    fn set_position_home_alt_amsl(&self, metres: f64);
    fn set_home_alt_amsl(&self, metres: f64);
    // Things the other protocols and services do with a frame.
    fn observe_fleet(&self, frame: &Frame, vehicle_heartbeat: bool);
    fn write_tlog(&self, frame: &Frame);
    fn record_scope(&self, frame: &Frame);
    fn handle_autotune_ack(&self, result: i64, progress: i64);
    fn handle_mission_request(&self, msg: &Message, as_int: bool);
    fn handle_mission_ack(&self, msg: &Message);
    fn handle_mission_count(&self, msg: &Message);
    fn handle_mission_item(&self, msg: &Message, as_int: bool);
    fn handle_mission_current(&self, msg: &Message);
    fn handle_mission_item_reached(&self, msg: &Message);
    fn handle_serial_control(&self, msg: &Message);
    fn handle_radio_status(&self, msg: &Message);
    fn handle_gnss_integrity(&self, frame: &Frame);
    fn handle_remote_id_arm_status(&self, msg: &Message);
    fn handle_battery_status(&self, msg: &Message);
    fn handle_statustext(&self, msg: &Message);
    fn handle_param_value(&self, msg: &Message);
    fn handle_ftp_message(&self, msg: &Message);
    fn handle_rc_channels(&self, msg: &Message, raw: bool);
    fn handle_attitude_target(&self, msg: &Message);
    fn handle_position_target(&self, msg: &Message);
    /// The log service's sink; Ok(false) when none is attached.
    fn log_sink(&self, msg: &Message) -> Result<bool, String>;
    fn note_heartbeat(&self, stamp: f64);
    fn publish_heartbeat_jitter(&self);
    fn publish_link_quality(&self);
    fn battery_fields(&self, voltage: f64, current: f64, reported: i64) -> Map<String, Value>;
    fn battery_endurance_fields(&self, percent: Option<f64>, source: &str, armed: bool) -> Map<String, Value>;
    fn note_prearm_state(&self, prearm: Option<bool>);
    fn has_prearm_reasons(&self) -> bool;
    fn clear_prearm_reasons(&self);
    fn roll_tlog_on_disarm(&self, armed: bool);
    // A vehicle that rebooted under a live link.
    fn reset_parameter_cache(&self);
    fn reset_param_metadata(&self);
    fn set_request_sent(&self, sent: bool);
    fn is_serial(&self) -> bool;
    fn request_streams(&self);
    fn request_version(&self);
    fn request_home(&self);
    fn schedule_message_intervals(&self);
    /// Wakes a blocked mission uploader so shutdown cannot hang.
    fn cancel_mission_upload(&self);
}

// ---------------------------------------------------------------- helpers

fn int_field(msg: &Message, name: &str) -> Option<i64> {
    match msg.get(name)? {
        Wv::Int(i) => Some(*i as i64),
        Wv::Float(f) if f.is_finite() => Some(f.trunc() as i64),
        _ => None,
    }
}

fn int_attr(msg: &Message, name: &str, fallback: i64) -> i64 {
    int_field(msg, name).unwrap_or(fallback)
}

fn float_attr(msg: &Message, name: &str) -> f64 {
    match msg.get(name) {
        Some(Wv::Int(i)) => *i as f64,
        Some(Wv::Float(f)) => *f,
        _ => 0.0,
    }
}

/// Could this HEARTBEAT have been sent by a real MAVLink node? A noisy radio
/// or a serial port at the wrong baud rate produces bytes that now and then
/// pass the checksum as a HEARTBEAT with a random system id. Every real node
/// sends `mavlink_version` 3 and small enum values.
pub fn is_plausible_heartbeat(hb: &Message) -> bool {
    if let Some(version) = int_field(hb, "mavlink_version") {
        if version != 3 {
            return false;
        }
    }
    for (field, limit) in HEARTBEAT_LIMITS {
        if let Some(value) = int_field(hb, field) {
            if !(0..limit).contains(&value) {
                return false;
            }
        }
    }
    true
}

/// Is this HEARTBEAT the flight controller's, or some other node's? Behind
/// mavlink-router the link also carries QGroundControl's own heartbeat, a
/// companion's, a gimbal's. The spec's answer is the `autopilot` field, but
/// plenty of nodes do not follow it, so the component id and the MAV_TYPE are
/// checked against the peripherals that are never a flight controller.
pub fn is_vehicle_heartbeat(frame: &Frame) -> bool {
    let Some(hb) = frame.message() else { return false };
    if !is_plausible_heartbeat(hb) {
        return false;
    }
    if frame.component != 0 && NON_AUTOPILOT_COMPONENTS.contains(&frame.component) {
        return false;
    }
    if let Some(vehicle_type) = int_field(hb, "type") {
        if NON_VEHICLE_TYPES.contains(&vehicle_type) {
            return false;
        }
    }
    match int_field(hb, "autopilot") {
        None => true,
        Some(a) => a != constant_or("MAV_AUTOPILOT_INVALID", 8),
    }
}

/// SYS_STATUS to "ok" or "fault" for one sensor, "" when it is not reported.
pub fn sensor_health(msg: &Message, bit: i64) -> &'static str {
    let enabled = int_attr(msg, "onboard_control_sensors_enabled", 0);
    let present = int_attr(msg, "onboard_control_sensors_present", 0);
    if (enabled | present) & bit == 0 {
        return "";
    }
    if int_attr(msg, "onboard_control_sensors_health", 0) & bit != 0 {
        "ok"
    } else {
        "fault"
    }
}

/// DISTANCE_SENSOR current_distance (cm) in metres, None unless usable. Only
/// the downward sensor counts, and only inside its own min and max: a reading
/// at or beyond max_distance is "nothing in range", not a distance.
pub fn rangefinder_distance_m(msg: &Message) -> Option<f64> {
    if int_field(msg, "orientation")? != DOWNWARD_ORIENTATION {
        return None;
    }
    let current = int_field(msg, "current_distance")?;
    let low = int_attr(msg, "min_distance", 0);
    let high = int_attr(msg, "max_distance", 0);
    if current < low || (high > 0 && current >= high) {
        return None;
    }
    Some(py_round(current as f64 / 100.0, 2))
}

/// GPS_RAW_INT alt (mm AMSL) in metres, None without a 3D fix: the field is
/// meaningless below one.
pub fn gps_altitude_m(msg: &Message) -> Option<f64> {
    if int_attr(msg, "fix_type", 0) < 3 {
        return None;
    }
    Some(py_round(int_field(msg, "alt")? as f64 / 1000.0, 1))
}

/// GPS_RAW_INT h_acc or v_acc (mm) in metres, -1 when not reported. Both are
/// MAVLink 2 extensions, so 0 is "unknown" and not a perfect fix.
pub fn gps_accuracy_m(raw: i64) -> f64 {
    if raw > 0 {
        py_round(raw as f64 / 1000.0, 2)
    } else {
        -1.0
    }
}

/// SYS_STATUS: ready to arm, refused, or not reported.
pub fn prearm_ok(msg: &Message) -> Option<bool> {
    let enabled = int_attr(msg, "onboard_control_sensors_enabled", 0);
    let present = int_attr(msg, "onboard_control_sensors_present", 0);
    if (enabled | present) & PREARM_BIT == 0 {
        return None;
    }
    Some(int_attr(msg, "onboard_control_sensors_health", 0) & PREARM_BIT != 0)
}

/// AUTOPILOT_VERSION.flight_custom_version as a git hash. The two stacks fill
/// the same eight bytes with two different things: PX4 packs the first five
/// bytes of the SHA-1 into the high bytes of a little endian uint64, ArduPilot
/// writes the ASCII of the abbreviated SHA. The format is sniffed: a field
/// that is already hex digits is the text, anything else the packed integer.
/// "" when the field is empty or all zero.
pub fn decode_git_hash(raw: &[u8]) -> String {
    let start = raw.iter().position(|b| *b != 0);
    let Some(start) = start else { return String::new() };
    let end = raw.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    let trimmed = &raw[start..end];
    if trimmed.len() >= 6 && trimmed.iter().all(u8::is_ascii_hexdigit) {
        return String::from_utf8_lossy(trimmed).to_lowercase();
    }
    let mut bytes = raw.to_vec();
    if bytes.first() == Some(&0) {
        bytes.reverse();
    }
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `HH:MM:SS UTC` of Unix microseconds, None where Python's
/// `datetime.fromtimestamp` would raise (outside years 1 to 9999).
fn utc_clock(usec: i64) -> Option<String> {
    let ts = usec as f64 / 1e6;
    let whole = ts.trunc();
    let frac = ts - whole;
    let mut seconds = whole as i64;
    let micro = (frac * 1e6).round_ties_even();
    if micro >= 1e6 {
        seconds += 1;
    } else if micro < 0.0 {
        seconds -= 1;
    }
    // 0001-01-01 and 9999-12-31 23:59:59 as Unix seconds.
    if !(-62_135_596_800..=253_402_300_799).contains(&seconds) {
        return None;
    }
    let of_day = seconds.rem_euclid(86_400);
    Some(format!(
        "{:02}:{:02}:{:02} UTC",
        of_day / 3600,
        (of_day % 3600) / 60,
        of_day % 60
    ))
}

fn name_of(table: &[(i64, &str)], key: i64, prefix: &str) -> String {
    table
        .iter()
        .find(|(k, _)| *k == key)
        .map_or_else(|| format!("{prefix}{key}"), |(_, v)| (*v).to_string())
}

fn update(host: &dyn ReceiveHost, pairs: Vec<(&str, Value)>) {
    host.store_update(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
}

/// A command waiting for its COMMAND_ACK.
struct PendingAck {
    event: Event,
    accept_in_progress: bool,
    interim: Vec<i64>,
    inner: Mutex<PendingInner>,
}

#[derive(Default)]
struct PendingInner {
    result: Option<i64>,
    interim_result: Option<i64>,
    in_progress: bool,
}

impl PendingAck {
    fn new(accept_in_progress: bool, interim: &[i64]) -> Arc<PendingAck> {
        Arc::new(PendingAck {
            event: Event::new(),
            accept_in_progress,
            interim: interim.to_vec(),
            inner: Mutex::new(PendingInner::default()),
        })
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, PendingInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Default)]
struct State {
    pending_acks: HashMap<i64, Arc<PendingAck>>,
    recent_frames: HashMap<Vec<u8>, f64>,
    recent_swept: f64,
    duplicate_frames: i64,
    ignored_systems: HashSet<i64>,
}

/// The receive path and command transaction of one bridge.
#[derive(Default)]
pub struct ReceivePath {
    state: Mutex<State>,
}

impl ReceivePath {
    pub fn new() -> ReceivePath {
        ReceivePath::default()
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn duplicate_frames(&self) -> i64 {
        self.st().duplicate_frames
    }

    pub fn ignored_systems(&self) -> Vec<i64> {
        let mut systems: Vec<i64> = self.st().ignored_systems.iter().copied().collect();
        systems.sort_unstable();
        systems
    }

    pub fn pending_commands(&self) -> usize {
        self.st().pending_acks.len()
    }

    // --------------------------------------------------------- identity

    /// Did `frame` come from the aircraft this bridge is flying? Source system
    /// 0 means the frame carries no sender id, and is accepted: this rejects an
    /// identified other node, it does not demand provenance a frame never had.
    pub fn is_from_vehicle(&self, host: &dyn ReceiveHost, frame: &Frame) -> bool {
        frame.system == 0 || frame.system == i64::from(host.target().0)
    }

    /// Tighter: the flight controller itself. A gimbal, a camera and a
    /// companion computer all heartbeat under the vehicle's system id.
    pub fn is_from_autopilot(&self, host: &dyn ReceiveHost, frame: &Frame) -> bool {
        self.is_from_vehicle(host, frame) && (frame.component == 0 || frame.component == i64::from(host.target().1))
    }

    /// Is this frame addressed to us, from the vehicle we talk to?
    pub fn ack_is_for_us(&self, host: &dyn ReceiveHost, frame: &Frame) -> bool {
        if !host.has_connection() {
            return false;
        }
        let (target_system, target_component) = host.target();
        if frame.system != 0 && frame.system != i64::from(target_system) {
            return false;
        }
        if frame.component != 0 && frame.component != i64::from(target_component) {
            return false;
        }
        let Some(msg) = frame.message() else { return true };
        let (expected_system, expected_component) = host.source_ids();
        let to_system = int_attr(msg, "target_system", 0);
        if to_system != 0 && expected_system != 0 && to_system != expected_system {
            return false;
        }
        let to_component = int_attr(msg, "target_component", 0);
        !(to_component != 0 && expected_component != 0 && to_component != expected_component)
    }

    // ----------------------------------------------------------- frames

    /// Has this exact frame already arrived in the last few hundred ms? A copy
    /// is dropped before anything reads it: when a link loops, an old HEARTBEAT
    /// decoded again flips the mode back, and an old COMMAND_ACK confirms a
    /// command the vehicle never acknowledged.
    pub fn is_duplicate_frame(&self, host: &dyn ReceiveHost, frame: &Frame) -> bool {
        let Some(raw) = frame.raw.filter(|r| !r.is_empty()) else {
            return false;
        };
        let now = host.now();
        let mut st = self.st();
        if now - st.recent_swept > DUPLICATE_FRAME_WINDOW_S {
            st.recent_frames
                .retain(|_, seen| now - *seen <= DUPLICATE_FRAME_WINDOW_S);
            st.recent_swept = now;
        }
        let seen = st.recent_frames.insert(raw.to_vec(), now);
        if seen.is_some_and(|s| now - s <= DUPLICATE_FRAME_WINDOW_S) {
            st.duplicate_frames += 1;
            return true;
        }
        false
    }

    /// Logs, counts and dispatches one received frame.
    pub fn process_frame(&self, host: &dyn ReceiveHost, frame: &Frame) {
        if self.is_duplicate_frame(host, frame) {
            return;
        }
        host.write_tlog(frame);
        host.record_scope(frame);
        self.dispatch(host, frame);
    }

    /// Transition to degraded link quality once per connection cycle, so the
    /// receive loop does not spam the store while heartbeats are late.
    pub fn mark_degraded_once(&self, host: &dyn ReceiveHost) {
        if host.degraded_warned() {
            return;
        }
        host.set_degraded_warned(true);
        update(
            host,
            vec![("link_status", json!("degraded")), ("link_quality", json!("poor"))],
        );
        host.log(
            "warning",
            &format!(
                "Link degraded: heartbeat late (>{}s)",
                super::common::fixed(host.warn_timeout(), 0)
            ),
        );
    }

    /// Notices a vehicle that rebooted while the link stayed up: the
    /// parameter cache, a download marked complete, the declared encoding and
    /// the stream rates all belong to the old boot.
    pub fn note_boot_time(&self, host: &dyn ReceiveHost, boot_ms: i64) {
        let last = host.last_boot_ms();
        host.set_last_boot_ms(boot_ms);
        let Some(last) = last else { return };
        if boot_ms + VEHICLE_REBOOT_BACKSTEP_MS >= last {
            return;
        }
        let text = "The vehicle rebooted. Parameters are read again when next needed";
        host.log("info", &format!("{text} (uptime {boot_ms} ms after {last} ms)"));
        host.console("LINK", text, "info");
        host.reset_parameter_cache();
        host.reset_param_metadata();
        host.set_param_encoding_declared("");
        host.set_request_sent(false);
        if host.is_serial() {
            host.request_streams();
        }
        host.request_version();
        host.request_home();
        host.schedule_message_intervals();
    }

    /// Hands a frame of the vehicle this link flies to the vehicle list. Frames
    /// of any other system are not the list's: one link is one vehicle.
    fn observe_fleet(&self, host: &dyn ReceiveHost, frame: &Frame, name: &str) {
        let heartbeat = name == "HEARTBEAT";
        if frame.system != i64::from(host.target().0) {
            if heartbeat && frame.system != 0 {
                self.note_ignored_system(host, frame, frame.system);
            }
            return;
        }
        host.observe_fleet(frame, heartbeat && is_vehicle_heartbeat(frame));
    }

    /// Tells the console, once per system, that another autopilot is ignored.
    fn note_ignored_system(&self, host: &dyn ReceiveHost, frame: &Frame, system: i64) {
        {
            let st = self.st();
            if st.ignored_systems.contains(&system) || st.ignored_systems.len() >= IGNORED_SYSTEMS_MAX {
                return;
            }
        }
        if system == GCS_SYSTEM_ID || !is_vehicle_heartbeat(frame) {
            return;
        }
        self.st().ignored_systems.insert(system);
        let this = host.target().0;
        host.log(
            "info",
            &format!("ignoring autopilot sys={system}: this link flies sys={this}"),
        );
        host.console(
            "LINK",
            &format!(
                "Another autopilot (system {system}) is on this link and is ignored. This link flies system {this}; a second vehicle needs a radio and a link of its own."
            ),
            "warning",
        );
    }

    // --------------------------------------------------------- dispatch

    pub fn dispatch(&self, host: &dyn ReceiveHost, frame: &Frame) {
        let name = frame.name();
        self.observe_fleet(host, frame, &name);

        if let Some(msg) = frame.message() {
            if name == "COMMAND_ACK" && self.is_from_vehicle(host, frame) {
                self.handle_command_ack(host, frame, msg);
            }
            match name.as_str() {
                "MISSION_REQUEST_INT" | "MISSION_REQUEST" => {
                    host.handle_mission_request(msg, name == "MISSION_REQUEST_INT");
                }
                "MISSION_ACK" => host.handle_mission_ack(msg),
                "MISSION_COUNT" => host.handle_mission_count(msg),
                "MISSION_ITEM_INT" | "MISSION_ITEM" => host.handle_mission_item(msg, name == "MISSION_ITEM_INT"),
                _ => {}
            }
            // NSH debug shell reply: a standalone branch, so a chatty shell
            // never starves the telemetry chain.
            if name == "SERIAL_CONTROL" {
                host.handle_serial_control(msg);
            }
            // The SiK radio speaks for itself, not for the aircraft: it carries
            // the radio's own system id, so it is served before the guard below.
            if name == "RADIO_STATUS" {
                host.handle_radio_status(msg);
                return;
            }
        }
        // Not in the dialect, so it arrives without a source id and checks the
        // aircraft's own itself.
        if name == "UNKNOWN_441" || name == "GNSS_INTEGRITY" {
            host.handle_gnss_integrity(frame);
            return;
        }
        // Everything past here writes the aircraft's own state. On a link that
        // carries more than one node the receive loop also sees a ground
        // station's heartbeats and a second aircraft's telemetry, and taking
        // those as this vehicle's means an armed indicator that blinks off
        // once a second, and a lost aircraft that still looks connected.
        if !self.is_from_vehicle(host, frame) {
            return;
        }
        let Some(msg) = frame.message() else { return };
        // The Remote ID verdict: guarded by the vehicle check but not by the
        // autopilot one, since a Remote ID module is a component of its own.
        if name == "OPEN_DRONE_ID_ARM_STATUS" {
            host.handle_remote_id_arm_status(msg);
            return;
        }
        self.dispatch_state(host, frame, msg, &name);
    }

    fn handle_command_ack(&self, host: &dyn ReceiveHost, frame: &Frame, msg: &Message) {
        let result = int_attr(msg, "result", 0);
        let command = int_attr(msg, "command", 0);
        // The console is this aircraft's console: through a router it would
        // otherwise fill with acknowledgements for another vehicle.
        let text = known_result_text(result).map_or_else(|| format!("result={result}"), str::to_string);
        let level = if result == 0 {
            "success"
        } else if result == 5 {
            "info"
        } else {
            "error"
        };
        host.console("COMMAND_ACK", &format!("COMMAND_ACK: cmd={command} {text}"), level);
        if command == constant_or("MAV_CMD_DO_AUTOTUNE_ENABLE", 212) {
            host.handle_autotune_ack(result, int_attr(msg, "progress", 0));
        }
        if !self.ack_is_for_us(host, frame) {
            return;
        }
        let pending = self.st().pending_acks.get(&command).cloned();
        let Some(pending) = pending else { return };
        let in_progress = constant_or("MAV_RESULT_IN_PROGRESS", 5);
        let mut inner = pending.inner();
        if result == in_progress && !pending.accept_in_progress {
            // Heard and being worked on: the final word follows.
            inner.in_progress = true;
        } else if pending.interim.contains(&result) {
            inner.interim_result = Some(result);
        } else {
            inner.result = Some(result);
            drop(inner);
            pending.event.set();
        }
    }

    fn dispatch_state(&self, host: &dyn ReceiveHost, frame: &Frame, msg: &Message, name: &str) {
        match name {
            "HEARTBEAT" => self.handle_heartbeat(host, frame, msg),
            "GLOBAL_POSITION_INT" => {
                let altitude_amsl = int_attr(msg, "alt", 0) as f64 / 1000.0;
                let altitude_agl = int_attr(msg, "relative_alt", 0) as f64 / 1000.0;
                host.set_position_home_alt_amsl(altitude_amsl - altitude_agl);
                let hdg = int_attr(msg, "hdg", 0);
                let velocity = |name: &str| py_round(float_attr(msg, name) / 100.0, 2);
                update(
                    host,
                    vec![
                        (
                            "position",
                            json!([
                                int_attr(msg, "lon", 0) as f64 / 1e7,
                                int_attr(msg, "lat", 0) as f64 / 1e7
                            ]),
                        ),
                        ("altitude_amsl", json!(py_round(altitude_amsl, 1))),
                        ("altitude_agl", json!(py_round(altitude_agl, 1))),
                        (
                            "heading",
                            if hdg != 65535 {
                                json!(hdg as f64 / 100.0)
                            } else {
                                json!(0)
                            },
                        ),
                        ("vx", json!(velocity("vx"))),
                        ("vy", json!(velocity("vy"))),
                        ("vz", json!(velocity("vz"))),
                    ],
                );
            }
            "DISTANCE_SENSOR" => {
                if int_attr(msg, "orientation", -1) == DOWNWARD_ORIENTATION {
                    host.set_rangefinder_seen(Some(host.now()));
                    update(
                        host,
                        vec![(
                            "rangefinder_distance",
                            rangefinder_distance_m(msg).map_or(Value::Null, |d| json!(d)),
                        )],
                    );
                }
            }
            "ATTITUDE" => {
                let deg = |name: &str| json!(py_round(float_attr(msg, name).to_degrees(), 1));
                update(
                    host,
                    vec![
                        ("roll", deg("roll")),
                        ("pitch", deg("pitch")),
                        ("yaw", deg("yaw")),
                        ("rollspeed", deg("rollspeed")),
                        ("pitchspeed", deg("pitchspeed")),
                        ("yawspeed", deg("yawspeed")),
                    ],
                );
            }
            "VFR_HUD" => {
                let airspeed = float_attr(msg, "airspeed");
                update(
                    host,
                    vec![
                        ("groundspeed", json!(py_round(float_attr(msg, "groundspeed"), 1))),
                        (
                            "airspeed",
                            if !airspeed.is_nan() {
                                json!(py_round(airspeed, 1))
                            } else {
                                json!(0)
                            },
                        ),
                        ("heading", json!(int_attr(msg, "heading", 0))),
                        ("vspeed", json!(py_round(float_attr(msg, "climb"), 1))),
                    ],
                );
            }
            "GPS_RAW_INT" => {
                let fix_type = int_attr(msg, "fix_type", 0);
                let fix = usize::try_from(fix_type)
                    .ok()
                    .and_then(|i| GPS_FIX.get(i))
                    .map_or_else(|| format!("FIX_{fix_type}"), |f| (*f).to_string());
                let eph = int_attr(msg, "eph", 0);
                let hdop = if eph != 65535 && eph > 0 {
                    eph as f64 / 100.0
                } else {
                    99.0
                };
                let epv = int_attr(msg, "epv", 65535);
                let vdop = if epv != 65535 && epv > 0 {
                    epv as f64 / 100.0
                } else {
                    99.0
                };
                update(
                    host,
                    vec![
                        ("gps_fix", json!(fix)),
                        ("gps_satellites", json!(int_attr(msg, "satellites_visible", 0))),
                        ("gps_hdop", json!(py_round(hdop, 1))),
                        ("gps_vdop", json!(py_round(vdop, 1))),
                        ("gps_h_acc", json!(gps_accuracy_m(int_attr(msg, "h_acc", 0)))),
                        ("gps_v_acc", json!(gps_accuracy_m(int_attr(msg, "v_acc", 0)))),
                        ("gps_altitude", gps_altitude_m(msg).map_or(Value::Null, |a| json!(a))),
                    ],
                );
            }
            "SYS_STATUS" => {
                let raw_voltage = int_attr(msg, "voltage_battery", 0);
                let raw_current = int_attr(msg, "current_battery", 0);
                let remaining = int_attr(msg, "battery_remaining", 0);
                let voltage = if raw_voltage != 65535 {
                    raw_voltage as f64 / 1000.0
                } else {
                    0.0
                };
                let current = if raw_current != -1 {
                    raw_current as f64 / 100.0
                } else {
                    0.0
                };
                let reported = if remaining != -1 { remaining } else { -1 };
                let prearm = prearm_ok(msg);
                let mut battery = host.battery_fields(voltage, current, reported);
                let source = battery
                    .get("battery_source")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let known = source == "estimate" || reported >= 0;
                let percent = battery.get("battery_percent").and_then(Value::as_f64).filter(|_| known);
                let endurance = host.battery_endurance_fields(percent, &source, host.armed_now());
                battery.extend(endurance);
                let mut fields = Map::new();
                fields.insert("prearm_ok".into(), prearm.map_or(Value::Null, |p| json!(p)));
                fields.insert("gps_health".into(), json!(sensor_health(msg, GPS_SENSOR_BIT)));
                fields.extend(battery);
                host.store_update(fields);
                host.note_prearm_state(prearm);
            }
            "BATTERY_STATUS" => host.handle_battery_status(msg),
            "SYSTEM_TIME" => {
                // The autopilot's own clock only: a companion computer sends
                // this message too under the vehicle's system id, and its uptime
                // interleaved with the autopilot's reads as a reboot every second.
                if !self.is_from_autopilot(host, frame) {
                    return;
                }
                let usec = int_attr(msg, "time_unix_usec", 0);
                if usec != 0 {
                    match utc_clock(usec) {
                        Some(clock) => update(host, vec![("time", json!(clock))]),
                        None => host.log("debug", &format!("SYSTEM_TIME out of range: {usec}")),
                    }
                }
                // The autopilot's uptime: the only honest signal that the
                // vehicle REBOOTED as opposed to the link having dropped.
                if let Some(boot_ms) = int_field(msg, "time_boot_ms").filter(|b| *b >= 0) {
                    self.note_boot_time(host, boot_ms);
                    update(host, vec![("boot_ms", json!(boot_ms))]);
                }
            }
            "STATUSTEXT" => host.handle_statustext(msg),
            "AUTOPILOT_VERSION" => self.handle_autopilot_version(host, frame, msg),
            "LOG_ENTRY" | "LOG_DATA" => {
                if !self.is_from_autopilot(host, frame) {
                    return;
                }
                // Routed to the log service rather than handled here: the
                // download is a stateful protocol the receive loop must not own.
                if host.log_sink(msg).is_err() {
                    host.log("debug", "log sink raised");
                }
            }
            "PARAM_VALUE" => {
                if self.is_from_autopilot(host, frame) {
                    host.handle_param_value(msg);
                }
            }
            "FILE_TRANSFER_PROTOCOL" => {
                if self.is_from_autopilot(host, frame) {
                    host.handle_ftp_message(msg);
                }
            }
            "VIBRATION" => {
                let axis = |name: &str| json!(py_round(float_attr(msg, name), 4));
                update(
                    host,
                    vec![
                        ("vibration_x", axis("vibration_x")),
                        ("vibration_y", axis("vibration_y")),
                        ("vibration_z", axis("vibration_z")),
                        ("clipping_0", json!(int_attr(msg, "clipping_0", 0))),
                        ("clipping_1", json!(int_attr(msg, "clipping_1", 0))),
                        ("clipping_2", json!(int_attr(msg, "clipping_2", 0))),
                    ],
                );
            }
            "EXTENDED_SYS_STATE" => {
                // The autopilot's own on ground or in air verdict: armed does
                // not mean flying, and altitude cannot tell a low hover from a
                // bad home reference.
                let landed = int_attr(msg, "landed_state", 0);
                if (0..=4).contains(&landed) {
                    update(host, vec![("landed_state", json!(landed))]);
                }
            }
            "RC_CHANNELS" | "RC_CHANNELS_RAW" => host.handle_rc_channels(msg, name == "RC_CHANNELS_RAW"),
            "ATTITUDE_TARGET" => host.handle_attitude_target(msg),
            "POSITION_TARGET_LOCAL_NED" => host.handle_position_target(msg),
            "MISSION_CURRENT" | "MISSION_ITEM_REACHED" => {
                if !self.is_from_autopilot(host, frame) {
                    return;
                }
                if name == "MISSION_CURRENT" {
                    host.handle_mission_current(msg);
                } else {
                    host.handle_mission_item_reached(msg);
                }
            }
            "HOME_POSITION" => {
                if !self.is_from_autopilot(host, frame) {
                    return;
                }
                let lat = int_attr(msg, "latitude", 0) as f64 / 1e7;
                let lon = int_attr(msg, "longitude", 0) as f64 / 1e7;
                host.set_home_alt_amsl(int_attr(msg, "altitude", 0) as f64 / 1000.0);
                update(host, vec![("home", json!([lon, lat]))]);
            }
            _ => {}
        }
    }

    fn handle_heartbeat(&self, host: &dyn ReceiveHost, frame: &Frame, msg: &Message) {
        // Tighter than the vehicle guard: every component on the airframe
        // heartbeats under the vehicle's system id with its own type,
        // autopilot and base_mode, so a gimbal would otherwise overwrite the
        // vehicle type, the flight mode and the armed flag.
        if !self.is_from_autopilot(host, frame) {
            return;
        }
        // One clock read per heartbeat, shared by the staleness timer and the
        // jitter window, so the two cannot disagree about when it landed.
        host.note_heartbeat(host.store_heartbeat());
        host.publish_heartbeat_jitter();
        if let Some(seen) = host.rangefinder_seen() {
            if host.now() - seen > RANGEFINDER_STALE_S {
                host.set_rangefinder_seen(None);
                update(host, vec![("rangefinder_distance", Value::Null)]);
            }
        }
        if host.degraded_warned() {
            // The heartbeat is back before the drop timeout. Without this the
            // link read "degraded" until the next reconnect, and a second
            // dropout in the same session was never reported.
            host.set_degraded_warned(false);
            update(
                host,
                vec![("link_status", json!("connected")), ("link_error", json!(""))],
            );
            host.log("info", "Link recovered: heartbeat back");
        }
        let mav_type = int_attr(msg, "type", 0);
        let autopilot = int_attr(msg, "autopilot", 0);
        let vtype = name_of(MAV_TYPE, mav_type, "TYPE_");
        let autopilot_name = name_of(MAV_AUTOPILOT, autopilot, "AP_");
        // Behind a router the aircraft on the far end can be swapped without
        // the socket closing, so the stack is re-read from every heartbeat.
        // The mode table goes with it: mode 4 is GUIDED on a copter, ACRO on a plane.
        if host.latch_dialect(autopilot, mav_type) && host.has_connection() {
            host.build_mode_mapping();
        }
        let base_mode = int_attr(msg, "base_mode", 0);
        let armed = base_mode & constant_or("MAV_MODE_FLAG_SAFETY_ARMED", 128) != 0;
        let dialect = host.dialect();
        let mode = dialect.decode_mode(int_attr(msg, "custom_mode", 0), base_mode, mav_type);
        let terminated = int_attr(msg, "system_status", 0) == constant_or("MAV_STATE_FLIGHT_TERMINATION", 8);
        let before = host.store_flags();
        if terminated && before.kill_switch {
            host.set_kill_evidence(host.now());
        }
        let lifted = !terminated && before.flight_termination;
        let mut fields = Map::new();
        fields.insert("vehicle_type".into(), json!(vtype));
        fields.insert("autopilot".into(), json!(autopilot_name));
        fields.insert("armed".into(), json!(armed));
        fields.insert("mode".into(), json!(mode));
        fields.insert("mode_label".into(), json!(dialect.mode_label(&mode)));
        fields.insert("autopilot_stack".into(), json!(dialect.stack().as_str()));
        fields.insert("flight_termination".into(), json!(terminated));
        if lifted {
            // The lockdown the heartbeat reported has lifted. On PX4 that is
            // the kill switch coming back, even if its line was lost.
            fields.insert("kill_switch".into(), json!(false));
        }
        host.store_update(fields);
        if lifted {
            host.resolve_warnings(&["kill"]);
        }
        if armed {
            // It armed, so whatever held it back no longer does.
            if host.has_prearm_reasons() {
                host.clear_prearm_reasons();
            }
            host.resolve_warnings(&["prearm"]);
        }
        host.roll_tlog_on_disarm(armed);
        // A heartbeat recovers a degraded link; recompute quality. When no
        // RADIO_STATUS arrives (UDP SITL), quality is heartbeat driven.
        host.publish_link_quality();
    }

    fn handle_autopilot_version(&self, host: &dyn ReceiveHost, frame: &Frame, msg: &Message) {
        if !self.is_from_autopilot(host, frame) {
            return;
        }
        let sw = int_attr(msg, "flight_sw_version", 0);
        if sw != 0 {
            let (major, minor, patch) = ((sw >> 24) & 0xFF, (sw >> 16) & 0xFF, (sw >> 8) & 0xFF);
            let custom: Vec<u8> = match msg.get("flight_custom_version") {
                Some(Wv::Array(items)) => items
                    .iter()
                    .filter_map(|v| v.as_i64())
                    .map(|v| (v & 0xFF) as u8)
                    .collect(),
                Some(Wv::Bytes(b)) => b.clone(),
                _ => Vec::new(),
            };
            update(
                host,
                vec![
                    ("px4_version", json!(format!("v{major}.{minor}.{patch}"))),
                    ("px4_version_detail", json!(decode_git_hash(&custom))),
                ],
            );
            let version = (major, minor, patch);
            if Some(version) != host.firmware_version() {
                // The mode table was built from the heartbeat, before this
                // arrived, so it holds only what every release accepts.
                host.set_firmware_version(version);
                host.build_mode_mapping();
            }
        }
        // Kept on the bridge rather than in the telemetry state: these two
        // identify the board for the firmware picker and are read once.
        host.set_board_ids(int_attr(msg, "vendor_id", 0), int_attr(msg, "product_id", 0));
        let capabilities = int_attr(msg, "capabilities", 0);
        if capabilities & CAP_PARAM_ENCODE_BYTEWISE != 0 {
            host.set_param_encoding_declared(crate::dialect::PARAM_ENCODING_BYTEWISE);
        } else if capabilities & CAP_PARAM_ENCODE_C_CAST != 0 {
            host.set_param_encoding_declared(crate::dialect::PARAM_ENCODING_C_CAST);
        }
    }

    // ------------------------------------------------ command transaction

    /// Wakes every command waiting for an ACK with DISCONNECTED, and the
    /// mission uploader, so shutdown cannot hang.
    pub fn cancel_pending_commands(&self, host: &dyn ReceiveHost) {
        let pending: Vec<Arc<PendingAck>> = self.st().pending_acks.values().cloned().collect();
        for p in pending {
            p.inner().result = Some(-2);
            p.event.set();
        }
        host.cancel_mission_upload();
    }

    /// The vehicle said IN_PROGRESS: wait for its verdict, never resend. A
    /// resend of a command the vehicle is already carrying out can start it
    /// over (a second calibration, a second takeoff target).
    fn await_final_ack(&self, host: &dyn ReceiveHost, pending: &PendingAck, timeout: f64) -> i64 {
        if host.wait_event(&pending.event, timeout) {
            return pending.inner().result.unwrap_or(-1);
        }
        constant_or("MAV_RESULT_IN_PROGRESS", 5)
    }

    #[allow(clippy::too_many_arguments)]
    /// Sends a command and waits for its COMMAND_ACK: the MAV_RESULT, -1 for
    /// no ack, -2 for a link that is gone. `interim` names results that may be
    /// followed by the real answer: they do not end the wait, one is returned
    /// only when nothing else arrives, and it stops the retries.
    pub fn send_command_and_wait(
        &self,
        host: &dyn ReceiveHost,
        command: i64,
        params: &[f64],
        timeout: f64,
        retries: i64,
        accept_in_progress: bool,
        interim: &[i64],
    ) -> i64 {
        let mut p: Vec<f64> = params.iter().copied().take(7).collect();
        p.resize(7, f64::NAN);
        self.transact(
            host,
            command,
            timeout,
            retries,
            accept_in_progress,
            interim,
            |attempt| {
                let int = |v: i64| Wv::Int(v as i128);
                let mut msg = targeted("COMMAND_LONG", host.target())
                    .with("command", int(command))
                    .with("confirmation", int(if attempt == 0 { 0 } else { 1 }));
                for (i, value) in p.iter().enumerate() {
                    msg = msg.with(&format!("param{}", i + 1), Wv::Float(*value));
                }
                ("send command", msg)
            },
        )
    }

    /// COMMAND_INT and its ACK. The COMMAND_LONG twin cannot carry a position:
    /// its params are float32, enough for 48.081 but not for 48.0812345, so a
    /// latitude sent that way lands tens of metres from where the operator
    /// pointed. COMMAND_INT carries x and y as int32 degrees times 1e7.
    #[allow(clippy::too_many_arguments)]
    pub fn send_command_int_and_wait(
        &self,
        host: &dyn ReceiveHost,
        command: i64,
        frame: i64,
        params: &[f64],
        x: i64,
        y: i64,
        z: f64,
        timeout: f64,
        retries: i64,
    ) -> i64 {
        let mut p: Vec<f64> = params.iter().copied().take(4).collect();
        p.resize(4, f64::NAN);
        self.transact(host, command, timeout, retries, false, &[], |_| {
            let int = |v: i64| Wv::Int(v as i128);
            let mut msg = targeted("COMMAND_INT", host.target())
                .with("frame", int(frame))
                .with("command", int(command))
                .with("current", int(0))
                .with("autocontinue", int(0))
                .with("x", int(x))
                .with("y", int(y))
                .with("z", Wv::Float(z));
            for (i, value) in p.iter().enumerate() {
                msg = msg.with(&format!("param{}", i + 1), Wv::Float(*value));
            }
            ("send command_int", msg)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn transact(
        &self,
        host: &dyn ReceiveHost,
        command: i64,
        timeout: f64,
        retries: i64,
        accept_in_progress: bool,
        interim: &[i64],
        build: impl Fn(i64) -> (&'static str, Message),
    ) -> i64 {
        let _gate = host.lock().lock();
        if !host.connection_ready() {
            return -2;
        }
        // The link as it is now: a stop or a reconnect that swaps it makes the
        // rest of this bail out with DISCONNECTED instead of sending on a closed socket.
        let link = host.link_generation();
        let pending = PendingAck::new(accept_in_progress, interim);
        self.st().pending_acks.insert(command, Arc::clone(&pending));
        let outcome = (|| -> i64 {
            for attempt in 0..=retries {
                if link != host.link_generation() || !host.connection_ready() {
                    return -2;
                }
                let (what, msg) = build(attempt);
                match host.send(msg) {
                    Ok(true) => {}
                    Ok(false) => return -2,
                    Err(e) => {
                        host.log("error", &format!("{what} {command} failed: {e}"));
                        return -2;
                    }
                }
                // Connection swapped during or after the send: do not wait for
                // an ACK on a torn down link.
                if link != host.link_generation() {
                    return -2;
                }
                if host.wait_event(&pending.event, timeout) {
                    return pending.inner().result.unwrap_or(-1);
                }
                let (interim_result, in_progress) = {
                    let inner = pending.inner();
                    (inner.interim_result, inner.in_progress)
                };
                if let Some(result) = interim_result.filter(|_| !interim.is_empty()) {
                    return result;
                }
                if in_progress {
                    return self.await_final_ack(host, &pending, timeout);
                }
                host.log(
                    "warning",
                    &format!("Command {command} ACK timeout (attempt {})", attempt + 1),
                );
            }
            -1
        })();
        let mut st = self.st();
        if st.pending_acks.get(&command).is_some_and(|p| Arc::ptr_eq(p, &pending)) {
            st.pending_acks.remove(&command);
        }
        outcome
    }
}
