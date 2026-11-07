//! The MAVLink mission protocol: uploading a mission or a geofence, reading
//! the mission off the vehicle, serving the vehicle's item requests, and
//! following which item the vehicle flies to.
//!
//! The twin of `corvus_link`'s mission engine (`protocol/missions.rs`). An
//! upload is a handshake: MISSION_COUNT, then the vehicle asks for each item
//! and ends with a MISSION_ACK. The requests and the ACK arrive on the
//! receive thread (`handle_*`) and wake the thread that is waiting
//! (`upload_mission`, `download_mission`). Waiting goes through
//! `MissionHost::wait_event`, so a test can drive the whole handshake on a
//! fake clock. The engine never holds its lock across a call into the host.
//!
//! Planning (turning a plan into items, circles into waypoints, items back
//! into a plan) is the planner's; the host answers those few calls.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use corvus_link::plain::defs::constant_or;
use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Map, Value};

use super::common::{fixed, py_capitalize, Event, TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M};
use super::targeted;
use crate::dialect::Dialect;
use crate::priority_lock::PriorityLock;

/// Waypoints one request may carry: the upload waits two seconds per item
/// while holding the lock every other command takes, so there must be a bound.
pub const FLY_TO_MAX_POINTS: usize = 255;
/// Items the planner holds.
pub const MISSION_MAX_ITEMS: i64 = 255;
/// MAV_DO_REPOSITION_FLAGS_CHANGE_MODE.
pub const DO_REPOSITION_CHANGE_MODE: f64 = 1.0;
/// How long the upload waits with no request and no ACK before calling it lost.
pub const MISSION_UPLOAD_QUIET_S: f64 = 10.0;
/// MISSION_COUNT is the one frame of an upload the vehicle does not ask for
/// again: it is sent again after this long without a request, a few times.
pub const MISSION_COUNT_RETRY_S: f64 = 1.5;
pub const MISSION_COUNT_RETRY_S_SLOW: f64 = 3.0;
pub const MISSION_COUNT_TRIES: i64 = 3;
/// Absolute ceiling on one upload, whatever the item count.
pub const MISSION_UPLOAD_MAX_S: f64 = 120.0;
/// Download: how long one request waits for its answer, and how often it is asked again.
pub const MISSION_DOWNLOAD_WAIT_S: f64 = 1.5;
pub const MISSION_DOWNLOAD_WAIT_S_SLOW: f64 = 3.0;
pub const MISSION_DOWNLOAD_TRIES: i64 = 4;
/// An item that does not arrive is not asked for again as it stands: the
/// vehicle may have dropped the transfer, so the whole handshake starts again.
pub const MISSION_DOWNLOAD_RESTARTS: i64 = 3;
const UNEXPECTED_REQUEST: &str = "req. wp was unexpected";
/// MAV_MISSION_RESULT a GCS sends to end a transfer it is abandoning.
pub const MAV_MISSION_OPERATION_CANCELLED: i64 = 15;
/// MISSION_CURRENT.total when the vehicle holds no mission at all.
pub const MISSION_TOTAL_NONE: i64 = 65535;
/// How long after Corvus's own transfer a MISSION_CURRENT still counting the
/// old mission is taken for one sent before the transfer.
pub const MISSION_CHANGE_GRACE_S: f64 = 5.0;
/// After an accepted upload, how long a stack that reports a verdict is given
/// to say whether it will fly the mission, and how soon after the ACK a report
/// may be taken for that verdict.
pub const MISSION_VERDICT_WAIT_S: f64 = 2.5;
pub const MISSION_VERDICT_SETTLE_S: f64 = 0.3;

const MAV_CMD_NAV_WAYPOINT: i64 = 16;
const MAV_CMD_NAV_LOITER_TURNS: i64 = 18;
const MAV_CMD_NAV_LOITER_TIME: i64 = 19;
const MAV_CMD_NAV_RETURN_TO_LAUNCH: i64 = 20;
const MAV_CMD_NAV_LAND: i64 = 21;
const MAV_CMD_NAV_TAKEOFF: i64 = 22;
const MAV_CMD_DO_CHANGE_SPEED: i64 = 178;
const MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION: i64 = 5001;
const MAV_MISSION_TYPE_FENCE: i64 = 1;
pub const MIN_FENCE_VERTICES: usize = 3;

/// The planner's item types: name, command and the word the page shows.
const ITEM_TYPES: &[(&str, i64, &str)] = &[
    ("takeoff", MAV_CMD_NAV_TAKEOFF, "Takeoff"),
    ("waypoint", MAV_CMD_NAV_WAYPOINT, "Waypoint"),
    ("loiter_turns", MAV_CMD_NAV_LOITER_TURNS, "Circle"),
    ("loiter_time", MAV_CMD_NAV_LOITER_TIME, "Hold"),
    ("land", MAV_CMD_NAV_LAND, "Land"),
    ("rtl", MAV_CMD_NAV_RETURN_TO_LAUNCH, "Return"),
];

/// The commands that name no place: a Return and a speed change. They travel in
/// MAV_FRAME_MISSION, since PX4 parses a Return only there.
pub fn is_positionless_command(command: i64) -> bool {
    command == MAV_CMD_DO_CHANGE_SPEED || command == MAV_CMD_NAV_RETURN_TO_LAUNCH
}

/// The plan item types a flight stack will not fly, with the reason, keyed by
/// the names the planner uses.
pub fn refused_item_types(refusals: &[(i64, &str)]) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, command, _) in ITEM_TYPES {
        if let Some((_, reason)) = refusals.iter().find(|(c, _)| c == command) {
            out.insert((*name).to_string(), json!(reason));
        }
    }
    out
}

/// The planner's name for a MAV_CMD, or "command N" for one it has none for.
pub fn label_of_command(command: i64) -> String {
    ITEM_TYPES
        .iter()
        .find(|(_, c, _)| *c == command)
        .map_or_else(|| format!("command {command}"), |(_, _, label)| (*label).to_string())
}

/// One mission item as it goes on the wire, in both its integer and its
/// floating point coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionItem {
    pub frame: i64,
    pub command: i64,
    pub current: i64,
    pub autocontinue: i64,
    pub params: [f64; 4],
    pub x_int: i64,
    pub y_int: i64,
    pub x_f: f64,
    pub y_f: f64,
    pub z: f64,
}

/// One item of a plan, as `plan_to_items` lowers it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    pub command: i64,
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub params: Vec<f64>,
    /// The plan item it came from, when the caller lowered a plan.
    pub item: Option<i64>,
}

/// One item of a mission read off the vehicle.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadedItem {
    pub command: i64,
    pub frame: i64,
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    /// NaN is how a yaw that was never asked for travels; None means the same.
    pub params: [Option<f64>; 4],
    pub autocontinue: i64,
}

impl DownloadedItem {
    pub fn to_json(&self) -> Value {
        json!({
            "command": self.command,
            "frame": self.frame,
            "lat": self.lat,
            "lon": self.lon,
            "alt": self.alt,
            "params": self.params.iter().map(|p| p.map_or(Value::Null, |v| json!(v))).collect::<Vec<_>>(),
            "autocontinue": self.autocontinue,
        })
    }
}

/// One line of the console, as the subscribers see it.
#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub name: String,
    pub text: String,
    pub level: String,
}

pub type ConsoleSub = Arc<dyn Fn(&ConsoleEntry) + Send + Sync>;

/// What the mission engine asks of the bridge it runs on.
pub trait MissionHost: Send + Sync {
    fn connection_ready(&self) -> bool;
    fn set_error(&self, text: &str);
    fn console(&self, source: &str, text: &str, level: &str);
    fn log(&self, level: &str, text: &str);
    /// Records "`action` failed: `result`" for the operator; always false.
    fn command_failure(&self, action: &str, result: i64) -> bool;
    fn lock(&self) -> &PriorityLock;
    /// Is an abort command queued or running? A long transfer asks between
    /// steps and gives way.
    fn abort_waiting(&self) -> bool {
        self.lock().abort_waiting()
    }
    /// Sends one message under the link's send lock. `Ok(false)` is "the link
    /// is no longer this bridge's", `Err` a failed write.
    fn send(&self, msg: Message) -> Result<bool, String>;
    fn target(&self) -> (u8, u8);
    /// Does this frame answer us (the vehicle we talk to)?
    fn ack_is_for_us(&self, msg: &Message) -> bool;
    fn now(&self) -> f64;
    /// Waits for `event` up to `timeout` seconds; true when it is set.
    fn wait_event(&self, event: &Event, timeout: f64) -> bool {
        event.wait(timeout)
    }
    fn stopping(&self) -> bool;
    fn is_slow_link(&self) -> bool;
    fn dialect(&self) -> Dialect;
    fn mav_type(&self) -> i64;
    fn store_update(&self, fields: Map<String, Value>);
    /// The home the store holds as `[lon, lat, ...]`; empty when it has none.
    fn home(&self) -> Vec<f64>;
    fn home_alt_amsl(&self) -> Option<f64>;
    fn add_console_sub(&self, sub: ConsoleSub) -> u64;
    fn remove_console_sub(&self, id: u64);
    // The planner's side.
    /// A plan's items with every Circle flown as the waypoints around its rim.
    fn lower_circles(&self, items: &[PlanItem]) -> Vec<PlanItem>;
    /// A downloaded mission read back as a plan (`plan_index` among its keys).
    fn items_to_plan(&self, items: &[DownloadedItem], home: Option<(f64, f64)>, home_alt: Option<f64>) -> Value;
    /// The inclusion polygon vertices of a fence, as plan items.
    fn fence_items(&self, polygon: &[(f64, f64)]) -> Vec<PlanItem> {
        fence_items(polygon)
    }
}

/// The fence transfer for `polygon` (`[lon, lat]` corners): one inclusion
/// vertex per corner, the corner count in param1.
pub fn fence_items(polygon: &[(f64, f64)]) -> Vec<PlanItem> {
    let count = polygon.len() as f64;
    polygon
        .iter()
        .map(|(lon, lat)| PlanItem {
            command: MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION,
            lat: *lat,
            lon: *lon,
            alt: 0.0,
            params: vec![count, 0.0, 0.0, 0.0],
            item: None,
        })
        .collect()
}

// ----------------------------------------------------- pure mission helpers

/// One MISSION_ITEM_INT / MISSION_ITEM send spec. `frame` defaults to
/// GLOBAL_RELATIVE_ALT (relative to home, which is AGL); a planner item that
/// names no place passes MAV_FRAME_MISSION instead.
#[allow(clippy::too_many_arguments)]
pub fn build_mission_item_spec(
    seq: i64,
    command: i64,
    lat: f64,
    lon: f64,
    alt: f64,
    p1: f64,
    p2: f64,
    p3: f64,
    p4: f64,
    frame: Option<i64>,
) -> MissionItem {
    MissionItem {
        frame: frame.unwrap_or_else(|| constant_or("MAV_FRAME_GLOBAL_RELATIVE_ALT", 3)),
        command,
        current: if seq == 0 { 1 } else { 0 },
        autocontinue: 1,
        params: [p1, p2, p3, p4],
        x_int: (lat * 1e7).round_ties_even() as i64,
        y_int: (lon * 1e7).round_ties_even() as i64,
        x_f: lat,
        y_f: lon,
        z: alt,
    }
}

/// Prepends the home slot ArduPilot reserves at mission sequence 0. PX4
/// numbers items from 0. An upload that does not account for that does not
/// fail: the vehicle stores the first item as a home position and flies a
/// mission with its takeoff missing. A zero item upload (the protocol's
/// "clear") passes through untouched. `home` is the store's `[lon, lat]`.
pub fn with_mission_home_slot(
    items: Vec<MissionItem>,
    dialect: &Dialect,
    home: &[f64],
    home_alt_amsl: Option<f64>,
) -> Vec<MissionItem> {
    if items.is_empty() || !dialect.mission_seq0_is_home() {
        return items;
    }
    let (lon, lat) = match home {
        [lon, lat, ..] => (*lon, *lat),
        _ => (0.0, 0.0),
    };
    let home_alt = home_alt_amsl.filter(|a| *a != 0.0).unwrap_or(0.0);
    let placeholder = build_mission_item_spec(
        0,
        MAV_CMD_NAV_WAYPOINT,
        lat,
        lon,
        home_alt,
        0.0,
        0.0,
        0.0,
        0.0,
        Some(constant_or("MAV_FRAME_GLOBAL", 0)),
    );
    let mut out = vec![placeholder];
    out.extend(items.into_iter().map(|mut item| {
        item.current = 0;
        item
    }));
    out
}

/// Where a prepended takeoff item is placed: the vehicle, else home, else the
/// first point. Never 0/0, which PX4's feasibility check would count as the
/// first waypoint. `position` and `home` are the store's `[lon, lat]`.
pub fn takeoff_item_position(first: (f64, f64, f64), position: &[f64], home: &[f64]) -> (f64, f64) {
    for value in [position, home] {
        let [lon, lat, ..] = value else { continue };
        let (lat, lon) = (*lat, *lon);
        if (lat, lon) == (0.0, 0.0) {
            continue;
        }
        if lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            return (lat, lon);
        }
    }
    (first.0, first.1)
}

/// MAV_MISSION_RESULT to MAV_RESULT for "Fly to points failed: ...".
pub fn mission_ack_to_result(ack_type: i64) -> i64 {
    if ack_type == constant_or("MAV_MISSION_ACCEPTED", 0) {
        constant_or("MAV_RESULT_ACCEPTED", 0)
    } else if ack_type == constant_or("MAV_MISSION_DENIED", 14) {
        constant_or("MAV_RESULT_DENIED", 2)
    } else if ack_type == 2 || ack_type == 3 {
        constant_or("MAV_RESULT_UNSUPPORTED", 3)
    } else {
        constant_or("MAV_RESULT_FAILED", 4)
    }
}

/// A point field as a float; a bool or anything not a number is None.
fn point_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// The operator's "fly to points" payload cleaned, or None with the reason
/// given to `fail`.
pub fn validate_fly_to_points(points: &Value, ground: bool, fail: &dyn Fn(&str)) -> Option<Vec<(f64, f64, f64)>> {
    let Some(list) = points.as_array().filter(|l| !l.is_empty()) else {
        fail("points must be a non-empty list");
        return None;
    };
    if list.len() > FLY_TO_MAX_POINTS {
        fail(&format!(
            "too many points ({}); the maximum is {FLY_TO_MAX_POINTS}",
            list.len()
        ));
        return None;
    }
    let mut cleaned = Vec::new();
    for (idx, entry) in list.iter().enumerate() {
        let Some(entry) = entry.as_object() else {
            fail(&format!("point {idx} must be an object"));
            return None;
        };
        let lat = point_number(entry.get("lat"));
        let lon = point_number(entry.get("lon"));
        let mut alt = point_number(entry.get("alt_agl"));
        if ground && entry.get("alt_agl").is_none_or(Value::is_null) {
            alt = Some(0.0);
        }
        let Some(lat) = lat else {
            fail(&format!("point {idx} lat must be a number"));
            return None;
        };
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            fail(&format!("point {idx} lat out of range"));
            return None;
        }
        let Some(lon) = lon else {
            fail(&format!("point {idx} lon must be a number"));
            return None;
        };
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            fail(&format!("point {idx} lon out of range"));
            return None;
        }
        let Some(alt) = alt else {
            fail(&format!("point {idx} alt_agl must be a number"));
            return None;
        };
        let alt = if ground {
            0.0
        } else if !alt.is_finite() || !(TAKEOFF_ALTITUDE_MIN_M..=TAKEOFF_ALTITUDE_MAX_M).contains(&alt) {
            fail(&format!(
                "point {idx} alt_agl must be between {} and {} m",
                fixed(TAKEOFF_ALTITUDE_MIN_M, 0),
                fixed(TAKEOFF_ALTITUDE_MAX_M, 0)
            ));
            return None;
        } else {
            alt
        };
        cleaned.push((lat, lon, alt));
    }
    Some(cleaned)
}

// --------------------------------------------------------------- message io

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

fn float_field(msg: &Message, name: &str) -> Option<f64> {
    match msg.get(name)? {
        Wv::Int(i) => Some(*i as f64),
        Wv::Float(f) => Some(*f),
        _ => None,
    }
}

fn finite_or_none(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

fn put(msg: Message, fields: &[(&str, i64)]) -> Message {
    fields
        .iter()
        .fold(msg, |m, (name, v)| m.with(name, Wv::Int(*v as i128)))
}

struct PendingAck {
    event: Event,
    result: Mutex<Option<i64>>,
}

/// What the vehicle says about a mission it has just accepted. `total` is the
/// item count it was sent, so a MISSION_CURRENT still about the previous
/// mission is not read as the answer.
struct Verdict {
    total: i64,
    event: Event,
    inner: Mutex<VerdictInner>,
}

struct VerdictInner {
    after: f64,
    state: String,
    reasons: Vec<String>,
}

#[derive(Default)]
struct DownloadInner {
    count: Option<i64>,
    waiting_for: i64,
    items: BTreeMap<i64, DownloadedItem>,
    drops: i64,
}

/// One download in flight: what the receive thread fills in, and the wake up.
struct Download {
    event: Event,
    inner: Mutex<DownloadInner>,
}

impl Download {
    fn inner(&self) -> std::sync::MutexGuard<'_, DownloadInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Default)]
struct State {
    items: Option<Vec<MissionItem>>,
    upload_type: i64,
    vehicle_fence: Option<Vec<(f64, f64)>>,
    pending_ack: Option<Arc<PendingAck>>,
    last_request: f64,
    download: Option<Arc<Download>>,
    vehicle_mission_index: Option<Vec<Option<i64>>>,
    vehicle_mission_id: Option<i64>,
    vehicle_mission_id_pending: bool,
    vehicle_mission_confirmed: bool,
    vehicle_mission_noted_at: f64,
    mission_revision: i64,
    verdict: Option<Arc<Verdict>>,
}

/// The mission protocol of one bridge.
#[derive(Default)]
pub struct MissionEngine {
    state: Mutex<State>,
}

type Plan<'a> = &'a [PlanItem];

impl MissionEngine {
    pub fn new() -> MissionEngine {
        MissionEngine::default()
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Arms an upload the way `upload_mission` does, without running the
    /// handshake, so the receive side can be exercised on its own.
    #[doc(hidden)]
    pub fn arm_upload_for_test(&self, items: Vec<MissionItem>, upload_type: i64) {
        let mut st = self.st();
        st.items = Some(items);
        st.upload_type = upload_type;
    }

    /// When the vehicle last asked for an item.
    pub fn last_request(&self) -> f64 {
        self.st().last_request
    }

    pub fn mission_revision(&self) -> i64 {
        self.st().mission_revision
    }

    pub fn vehicle_mission_index(&self) -> Option<Vec<Option<i64>>> {
        self.st().vehicle_mission_index.clone()
    }

    pub fn vehicle_mission_id(&self) -> Option<i64> {
        self.st().vehicle_mission_id
    }

    pub fn vehicle_mission_confirmed(&self) -> bool {
        self.st().vehicle_mission_confirmed
    }

    /// The area last put on the vehicle over this link (`[]` after a clear),
    /// None when this link has not transferred one.
    pub fn vehicle_fence(&self) -> Option<Vec<(f64, f64)>> {
        self.st().vehicle_fence.clone()
    }

    /// Start of a new link: nothing is known of what the vehicle holds.
    pub fn forget_link(&self) {
        let mut st = self.st();
        st.vehicle_fence = None;
        st.vehicle_mission_index = None;
        st.vehicle_mission_id = None;
        st.vehicle_mission_id_pending = false;
        st.vehicle_mission_confirmed = false;
    }

    /// The planner's part of the capabilities: which item types this stack
    /// will not fly, with the reason, so the page greys the tool with it.
    pub fn mission_capabilities(&self, host: &dyn MissionHost) -> Value {
        let refusals = host.dialect().mission_refusals(host.mav_type());
        json!({"mission_unsupported": Value::Object(refused_item_types(&refusals))})
    }

    /// Vehicle sequence numbers minus this is the plan's numbering.
    fn mission_offset(host: &dyn MissionHost) -> i64 {
        i64::from(host.dialect().mission_seq0_is_home())
    }

    fn plan_item_of(&self, seq: i64) -> i64 {
        let st = self.st();
        match &st.vehicle_mission_index {
            Some(index) if (0..index.len() as i64).contains(&seq) => index[seq as usize].unwrap_or(-1),
            _ => -1,
        }
    }

    // ------------------------------------------------------ receive side

    /// Serves one MISSION_REQUEST(_INT) during an upload we initiated.
    pub fn handle_mission_request(&self, host: &dyn MissionHost, msg: &Message, as_int: bool) {
        if !host.ack_is_for_us(msg) {
            return;
        }
        let seq = int_attr(msg, "seq", -1);
        let kind = int_attr(msg, "mission_type", 0);
        let (item, upload_type) = {
            let st = self.st();
            match &st.items {
                Some(items) if (0..items.len() as i64).contains(&seq) && kind == st.upload_type => {
                    (items[seq as usize].clone(), st.upload_type)
                }
                _ => return,
            }
        };
        self.st().last_request = host.now();
        let name = if as_int { "MISSION_ITEM_INT" } else { "MISSION_ITEM" };
        let mut out = put(
            targeted(name, host.target()),
            &[
                ("seq", seq),
                ("frame", item.frame),
                ("command", item.command),
                ("current", item.current),
                ("autocontinue", item.autocontinue),
            ],
        );
        for (i, p) in item.params.iter().enumerate() {
            out = out.with(&format!("param{}", i + 1), Wv::Float(*p));
        }
        out = if as_int {
            put(out, &[("x", item.x_int), ("y", item.y_int)]).with("z", Wv::Float(item.z))
        } else {
            out.with("x", Wv::Float(item.x_f))
                .with("y", Wv::Float(item.y_f))
                .with("z", Wv::Float(item.z))
        };
        if upload_type != 0 {
            out = put(out, &[("mission_type", upload_type)]);
        }
        // A link swapped under us drops the request; a failed write is only logged.
        if let Err(e) = host.send(out) {
            host.log("debug", &format!("mission item send failed (seq={seq}): {e}"));
        }
    }

    /// The vehicle's answer to MISSION_REQUEST_LIST: how many items.
    pub fn handle_mission_count(&self, host: &dyn MissionHost, msg: &Message) {
        if !host.ack_is_for_us(msg) || int_attr(msg, "mission_type", 0) != 0 {
            return;
        }
        let download = self.st().download.clone();
        let Some(download) = download else { return };
        let mut inner = download.inner();
        if inner.count.is_some() {
            return;
        }
        let Some(count) = int_field(msg, "count") else { return };
        inner.count = Some(count.max(0));
        drop(inner);
        download.event.set();
    }

    /// One item of a download, kept only if it is the one asked for.
    pub fn handle_mission_item(&self, host: &dyn MissionHost, msg: &Message, as_int: bool) {
        if !host.ack_is_for_us(msg) || int_attr(msg, "mission_type", 0) != 0 {
            return;
        }
        let (Some(seq), Some(command), Some(frame)) = (
            int_field(msg, "seq"),
            int_field(msg, "command"),
            int_field(msg, "frame"),
        ) else {
            host.log("debug", "discarding malformed mission item");
            return;
        };
        let coordinate = |name: &str| -> Option<f64> {
            if as_int {
                int_field(msg, name).map(|v| v as f64 / 1e7)
            } else {
                float_field(msg, name)
            }
        };
        let (Some(lat), Some(lon)) = (coordinate("x"), coordinate("y")) else {
            host.log("debug", "discarding malformed mission item");
            return;
        };
        let item = DownloadedItem {
            command,
            frame,
            lat,
            lon,
            alt: finite_or_none(float_field(msg, "z"))
                .filter(|a| *a != 0.0)
                .unwrap_or(0.0),
            params: [1, 2, 3, 4].map(|n| finite_or_none(float_field(msg, &format!("param{n}")))),
            autocontinue: int_attr(msg, "autocontinue", 1),
        };
        let download = self.st().download.clone();
        let Some(download) = download else { return };
        let mut inner = download.inner();
        if seq != inner.waiting_for {
            return;
        }
        inner.items.insert(seq, item);
        drop(inner);
        download.event.set();
    }

    /// Resolves the pending upload waiter with the MISSION_ACK result.
    pub fn handle_mission_ack(&self, host: &dyn MissionHost, msg: &Message) {
        if !host.ack_is_for_us(msg) {
            return;
        }
        let ack_type = int_attr(msg, "type", -1);
        let level = if ack_type == constant_or("MAV_MISSION_ACCEPTED", 0) {
            "success"
        } else {
            "error"
        };
        host.console("GOTOPOINTS", &format!("MISSION_ACK: type={ack_type}"), level);
        let pending = self.st().pending_ack.clone();
        if let Some(pending) = pending {
            let mut result = pending.result.lock().unwrap_or_else(|e| e.into_inner());
            if result.is_none() {
                *result = Some(ack_type);
                drop(result);
                pending.event.set();
            }
        }
    }

    /// Which item the vehicle is flying to, and whether its mission changed.
    pub fn handle_mission_current(&self, host: &dyn MissionHost, msg: &Message) {
        self.handle_mission_current_id(host, msg, int_attr(msg, "mission_id", 0));
    }

    /// The same, with the mission id PX4 v1.14+ sends when the dialect in use
    /// carries it (0 when it does not).
    pub fn handle_mission_current_id(&self, host: &dyn MissionHost, msg: &Message, mission_id: i64) {
        let Some(raw_seq) = int_field(msg, "seq") else { return };
        let offset = Self::mission_offset(host);
        let seq = raw_seq - offset;
        let mut fields = Map::new();
        fields.insert("mission_seq".into(), json!(if seq >= 0 { seq } else { -1 }));
        fields.insert("mission_item".into(), json!(self.plan_item_of(seq)));
        let total = int_field(msg, "total");
        if let Some(total) = total.filter(|t| *t != 0) {
            let shown = if total == MISSION_TOTAL_NONE {
                0
            } else {
                (total - offset).max(0)
            };
            fields.insert("mission_total".into(), json!(shown));
        }
        let state = match int_attr(msg, "mission_state", 0) {
            1 => "no_mission",
            2 => "not_started",
            3 => "active",
            4 => "paused",
            5 => "complete",
            _ => "",
        };
        if !state.is_empty() {
            fields.insert("mission_state".into(), json!(state));
        }
        let verdict = self.st().verdict.clone();
        if let Some(verdict) = verdict {
            let mut inner = verdict.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !state.is_empty() && total == Some(verdict.total) && host.now() >= inner.after {
                inner.state = state.to_string();
                drop(inner);
                verdict.event.set();
            }
        }
        // Somebody else may have put a mission on the vehicle: QGroundControl,
        // a companion, a second Corvus. Two signals: the mission id PX4 v1.14+
        // sends, and the item count, which every stack sends.
        let mut changed = false;
        let index_len = {
            let mut st = self.st();
            if mission_id != 0 {
                if st.vehicle_mission_id_pending || st.vehicle_mission_id.is_none() {
                    st.vehicle_mission_id = Some(mission_id);
                    st.vehicle_mission_id_pending = false;
                } else if Some(mission_id) != st.vehicle_mission_id {
                    st.vehicle_mission_id = Some(mission_id);
                    changed = true;
                }
            }
            st.vehicle_mission_index.as_ref().map(Vec::len)
        };
        if let (Some(len), Some(total)) = (index_len, fields.get("mission_total").and_then(Value::as_i64)) {
            let mut st = self.st();
            if total == len as i64 {
                st.vehicle_mission_confirmed = true;
            } else if st.vehicle_mission_confirmed || host.now() - st.vehicle_mission_noted_at >= MISSION_CHANGE_GRACE_S
            {
                changed = true;
            }
        }
        if changed && index_len.is_some() {
            self.forget_vehicle_mission(host, "the vehicle holds a different mission now");
            fields.insert("mission_item".into(), json!(-1));
        }
        host.store_update(fields);
    }

    pub fn handle_mission_item_reached(&self, host: &dyn MissionHost, msg: &Message) {
        let Some(raw_seq) = int_field(msg, "seq") else { return };
        let seq = raw_seq - Self::mission_offset(host);
        if seq >= 0 {
            let mut fields = Map::new();
            fields.insert("mission_reached".into(), json!(seq));
            fields.insert("mission_reached_item".into(), json!(self.plan_item_of(seq)));
            host.store_update(fields);
        }
    }

    // --------------------------------------------------------- what is on board

    /// Records what Corvus just put on (or read off) the vehicle.
    pub fn note_vehicle_mission(&self, host: &dyn MissionHost, plan_index: Option<&[Option<i64>]>) {
        let revision = {
            let mut st = self.st();
            st.vehicle_mission_index = plan_index.map(<[Option<i64>]>::to_vec);
            st.vehicle_mission_id_pending = true;
            st.vehicle_mission_confirmed = false;
            st.vehicle_mission_noted_at = host.now();
            st.mission_revision += 1;
            st.mission_revision
        };
        let mut fields = Map::new();
        fields.insert("mission_known".into(), json!(plan_index.is_some()));
        fields.insert("mission_revision".into(), json!(revision));
        fields.insert("mission_total".into(), json!(plan_index.map_or(-1, |p| p.len() as i64)));
        fields.insert("mission_item".into(), json!(-1));
        fields.insert("mission_reached_item".into(), json!(-1));
        host.store_update(fields);
    }

    /// Corvus no longer knows what is on the vehicle (a new link, another station).
    pub fn forget_vehicle_mission(&self, host: &dyn MissionHost, reason: &str) {
        let (had, revision) = {
            let mut st = self.st();
            let had = st.vehicle_mission_index.is_some();
            st.vehicle_mission_index = None;
            st.mission_revision += 1;
            (had, st.mission_revision)
        };
        let mut fields = Map::new();
        fields.insert("mission_known".into(), json!(false));
        fields.insert("mission_revision".into(), json!(revision));
        fields.insert("mission_item".into(), json!(-1));
        fields.insert("mission_reached_item".into(), json!(-1));
        host.store_update(fields);
        if had && !reason.is_empty() {
            host.console("MISSION", &py_capitalize(reason), "info");
        }
    }

    fn home_lat_lon(host: &dyn MissionHost) -> Option<(f64, f64)> {
        let [lon, lat, ..] = host.home()[..] else { return None };
        if (lat, lon) == (0.0, 0.0) || !(lat.is_finite() && lon.is_finite()) {
            return None;
        }
        Some((lat, lon))
    }

    // ----------------------------------------------------------- sending

    fn send_mission_ack(&self, host: &dyn MissionHost, result: i64, mission_type: i64) {
        let msg = put(
            targeted("MISSION_ACK", host.target()),
            &[("type", result), ("mission_type", mission_type)],
        );
        // The vehicle times the transfer out anyway.
        if let Err(e) = host.send(msg) {
            host.log("debug", &format!("mission ack send failed: {e}"));
        }
    }

    fn cancel_mission_download(&self, host: &dyn MissionHost) {
        self.send_mission_ack(host, MAV_MISSION_OPERATION_CANCELLED, 0);
    }

    // ------------------------------------------------------------ download

    /// Reads the mission stored on the vehicle, and reads it back as a plan:
    /// MISSION_REQUEST_LIST, one MISSION_REQUEST_INT per item (each asked again
    /// until it arrives), then MISSION_ACK. ArduPilot's home slot is left out.
    /// Holds the command lock like an upload does, but gives it up between
    /// items if an abort is waiting. None with the reason as the last error.
    pub fn download_mission(&self, host: &dyn MissionHost) -> Option<Value> {
        let _gate = host.lock().lock();
        host.set_error("");
        if !host.connection_ready() {
            host.command_failure("Mission download", -2);
            return None;
        }
        let download = Arc::new(Download {
            event: Event::new(),
            inner: Mutex::new(DownloadInner {
                waiting_for: -1,
                ..Default::default()
            }),
        });
        let watched = Arc::clone(&download);
        let sub_id = host.add_console_sub(Arc::new(move |entry: &ConsoleEntry| {
            if entry.name == "STATUSTEXT" && entry.text.to_lowercase().contains(UNEXPECTED_REQUEST) {
                watched.inner().drops += 1;
                watched.event.set();
            }
        }));
        self.st().download = Some(Arc::clone(&download));
        let items = self.run_mission_download(host, &download);
        {
            let mut st = self.st();
            if st.download.as_ref().is_some_and(|d| Arc::ptr_eq(d, &download)) {
                st.download = None;
            }
        }
        host.remove_console_sub(sub_id);
        let items = items?;
        let home = Self::home_lat_lon(host);
        let converted = host.items_to_plan(&items, home, host.home_alt_amsl());
        let plan_index: Vec<Option<i64>> = converted
            .get("plan_index")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(Value::as_i64).collect())
            .unwrap_or_default();
        self.note_vehicle_mission(host, Some(&plan_index));
        host.console(
            "MISSION",
            &format!("Read {} mission item(s) from the vehicle", items.len()),
            "success",
        );
        let mut out = converted.as_object().cloned().unwrap_or_default();
        out.insert(
            "items".into(),
            Value::Array(items.iter().map(DownloadedItem::to_json).collect()),
        );
        out.insert("count".into(), json!(items.len()));
        out.insert("revision".into(), json!(self.mission_revision()));
        Some(Value::Object(out))
    }

    fn mission_wait_s(host: &dyn MissionHost) -> f64 {
        if host.is_slow_link() {
            MISSION_DOWNLOAD_WAIT_S_SLOW
        } else {
            MISSION_DOWNLOAD_WAIT_S
        }
    }

    /// Send, wait, repeat. Some(true) answered, Some(false) lost, None interrupted.
    fn ask(
        &self,
        host: &dyn MissionHost,
        download: &Download,
        send: &dyn Fn() -> Message,
        answered: &dyn Fn() -> bool,
        tries: i64,
    ) -> Option<bool> {
        for _attempt in 0..tries {
            if host.stopping() || !host.connection_ready() {
                return None;
            }
            download.event.clear();
            let drops = download.inner().drops;
            match host.send(send()) {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => {
                    host.log("debug", &format!("mission download send failed: {e}"));
                    return None;
                }
            }
            let deadline = host.now() + Self::mission_wait_s(host);
            while host.now() < deadline {
                if host.wait_event(&download.event, 0.1) && answered() {
                    return Some(true);
                }
                if host.abort_waiting() {
                    return None;
                }
                if download.inner().drops != drops {
                    break;
                }
            }
            if answered() {
                return Some(true);
            }
        }
        Some(false)
    }

    fn run_mission_download(&self, host: &dyn MissionHost, download: &Arc<Download>) -> Option<Vec<DownloadedItem>> {
        let target = host.target();
        let ask_list = |this: &MissionEngine| -> Option<bool> {
            download.inner().count = None;
            this.ask(
                host,
                download,
                &|| put(targeted("MISSION_REQUEST_LIST", target), &[("mission_type", 0)]),
                &|| download.inner().count.is_some(),
                MISSION_DOWNLOAD_TRIES,
            )
        };
        let got = ask_list(self);
        if got.is_none() {
            self.cancel_mission_download(host);
            host.set_error("mission download interrupted");
            return None;
        }
        if got != Some(true) {
            host.set_error("the vehicle did not answer the mission request");
            return None;
        }
        let total = download.inner().count.unwrap_or(0);
        let offset = if total != 0 { Self::mission_offset(host) } else { 0 };
        if total - offset > MISSION_MAX_ITEMS {
            self.cancel_mission_download(host);
            host.set_error(&format!(
                "the vehicle's mission has {} items, more than the {MISSION_MAX_ITEMS} the planner holds",
                total - offset
            ));
            return None;
        }
        for seq in 0..total {
            download.inner().waiting_for = seq;
            let mut restarts = 0;
            loop {
                let got = self.ask(
                    host,
                    download,
                    &|| {
                        put(
                            targeted("MISSION_REQUEST_INT", target),
                            &[("seq", seq), ("mission_type", 0)],
                        )
                    },
                    &|| download.inner().items.contains_key(&seq),
                    1,
                );
                if got == Some(true) {
                    break;
                }
                if got.is_none() {
                    self.cancel_mission_download(host);
                    host.set_error("mission download interrupted");
                    return None;
                }
                let drops = download.inner().drops;
                if restarts >= MISSION_DOWNLOAD_RESTARTS {
                    self.cancel_mission_download(host);
                    let reason = if drops != 0 {
                        ". The vehicle keeps reporting the request as unexpected, so another station (a companion computer or a second ground station) is probably reading the mission at the same time"
                    } else {
                        ""
                    };
                    host.set_error(&format!("mission item {} of {total} never arrived{reason}", seq + 1));
                    return None;
                }
                restarts += 1;
                let lead = if drops != 0 {
                    "The vehicle ended the transfer and reported the request as unexpected. ".to_string()
                } else {
                    format!("Mission item {} did not arrive. ", seq + 1)
                };
                host.console(
                    "MISSION",
                    &format!("{lead}Reading the mission again from the start"),
                    "info",
                );
                download.inner().waiting_for = -1;
                let got = ask_list(self);
                if got != Some(true) {
                    self.cancel_mission_download(host);
                    host.set_error(if got.is_none() {
                        "mission download interrupted"
                    } else {
                        "the vehicle did not answer the mission request"
                    });
                    return None;
                }
                if download.inner().count.unwrap_or(0) != total {
                    self.cancel_mission_download(host);
                    host.set_error("the mission on the vehicle changed while it was being read");
                    return None;
                }
                download.inner().waiting_for = seq;
            }
        }
        if total != 0 {
            self.send_mission_ack(host, constant_or("MAV_MISSION_ACCEPTED", 0), 0);
        }
        let inner = download.inner();
        Some(
            (offset..total)
                .filter_map(|seq| inner.items.get(&seq).cloned())
                .collect(),
        )
    }

    // ------------------------------------------------------------- upload

    /// Uploads mission items and waits for MISSION_ACK. `mission_type` is
    /// MAV_MISSION_TYPE: 0 for the mission, FENCE for the geofence. The
    /// MAV_MISSION_ACCEPTED on success, the ACK type on rejection, -1 on
    /// timeout, -2 on disconnect, -3 when an abort command is waiting: the
    /// transfer is cancelled on the vehicle, which keeps the mission it had.
    pub fn upload_mission(&self, host: &dyn MissionHost, items: &[MissionItem], mission_type: i64) -> i64 {
        let pending = Arc::new(PendingAck {
            event: Event::new(),
            result: Mutex::new(None),
        });
        let started = host.now();
        {
            let mut st = self.st();
            st.upload_type = mission_type;
            st.items = Some(items.to_vec());
            st.pending_ack = Some(Arc::clone(&pending));
            st.last_request = started;
        }
        let count_retry_s = if host.is_slow_link() {
            MISSION_COUNT_RETRY_S_SLOW
        } else {
            MISSION_COUNT_RETRY_S
        };
        let send_count = || -> bool {
            let mut msg = put(
                targeted("MISSION_COUNT", host.target()),
                &[("count", items.len() as i64)],
            );
            if mission_type != 0 {
                msg = put(msg, &[("mission_type", mission_type)]);
            }
            match host.send(msg) {
                Ok(sent) => sent,
                Err(e) => {
                    host.log("error", &format!("mission_count_send failed: {e}"));
                    false
                }
            }
        };
        let outcome = (|| -> i64 {
            if !send_count() {
                return -2;
            }
            let mut count_sent_at = started;
            let mut count_sends = 1;
            // Bounded by progress, not by item count: PX4 requests the items
            // back to back, so "the vehicle has stopped asking" is known within
            // MISSION_UPLOAD_QUIET_S.
            let deadline = host.now() + MISSION_UPLOAD_MAX_S.min(5.0 + 2.0 * items.len() as f64);
            loop {
                if host.wait_event(&pending.event, 0.2) {
                    return pending.result.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or(-1);
                }
                if host.abort_waiting() {
                    self.send_mission_ack(host, MAV_MISSION_OPERATION_CANCELLED, mission_type);
                    host.log("info", "mission upload cancelled: an abort command is waiting");
                    return -3;
                }
                let now = host.now();
                if now > deadline {
                    return -1;
                }
                let last = self.st().last_request;
                if last <= started && count_sends < MISSION_COUNT_TRIES && now - count_sent_at > count_retry_s {
                    host.log("info", "no item requested yet, sending MISSION_COUNT again");
                    if !send_count() {
                        return -2;
                    }
                    count_sent_at = now;
                    count_sends += 1;
                    continue;
                }
                // No item has been asked for in a while and no ACK has landed:
                // the handshake is not in progress, it is over.
                if now - last > MISSION_UPLOAD_QUIET_S {
                    return -1;
                }
            }
        })();
        let mut st = self.st();
        if st.pending_ack.as_ref().is_some_and(|p| Arc::ptr_eq(p, &pending)) {
            st.pending_ack = None;
        }
        st.items = None;
        st.upload_type = 0;
        outcome
    }

    // ------------------------------------------------------- the plan

    /// `items` as this autopilot flies them: a Circle as the waypoints around
    /// its rim where the stack has no NAV_LOITER_TURNS in a mission.
    pub fn lower_mission_plan(&self, host: &dyn MissionHost, items: Plan<'_>) -> Vec<PlanItem> {
        if host.dialect().mission_circles_as_waypoints() {
            host.lower_circles(items)
        } else {
            items.to_vec()
        }
    }

    /// Why this stack will not take `items`, or "" when it will. Checked
    /// before anything is sent: an item the firmware does not know fails the
    /// whole transfer with a bare UNSUPPORTED.
    fn mission_refusal(&self, host: &dyn MissionHost, items: Plan<'_>) -> String {
        let refusals = host.dialect().mission_refusals(host.mav_type());
        for (number, entry) in items.iter().enumerate() {
            let Some((_, reason)) = refusals.iter().find(|(c, _)| *c == entry.command) else {
                continue;
            };
            let where_ = match entry.item {
                Some(item) => format!("item {}", item + 1),
                None => format!("item {}", number + 1),
            };
            return format!(
                "Mission upload refused: {} ({where_}). {reason}.",
                label_of_command(entry.command)
            );
        }
        String::new()
    }

    /// Uploads a planned mission. Does not start it and does not arm. True
    /// only on MAV_MISSION_ACCEPTED, and, for a stack that reports a verdict,
    /// only when the vehicle does not turn round and reject it.
    pub fn upload_mission_plan(&self, host: &dyn MissionHost, items: Plan<'_>) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if items.is_empty() {
            host.set_error("mission must carry at least one item");
            return false;
        }
        let drawn = items.len();
        let items = self.lower_mission_plan(host, items);
        if items.len() > FLY_TO_MAX_POINTS {
            let circles = if items.len() > drawn {
                " This autopilot flies a circle as waypoints around it: give the circles fewer turns."
            } else {
                ""
            };
            host.set_error(&format!(
                "too many mission items ({}); the maximum is {FLY_TO_MAX_POINTS}.{circles}",
                items.len()
            ));
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Mission upload", -2);
        }
        let refusal = self.mission_refusal(host, &items);
        if !refusal.is_empty() {
            host.set_error(&refusal);
            host.console("MISSION", &refusal, "error");
            return false;
        }
        let specs: Vec<MissionItem> = items
            .iter()
            .enumerate()
            .map(|(seq, entry)| {
                let mut params = entry.params.clone();
                if params.is_empty() {
                    params = vec![0.0; 4];
                }
                params.resize(params.len().max(4), 0.0);
                // A command that names no place carries no altitude frame
                // either; everything that navigates stays in the relative alt
                // frame the plan's altitudes are written in.
                let frame = is_positionless_command(entry.command).then(|| constant_or("MAV_FRAME_MISSION", 2));
                build_mission_item_spec(
                    seq as i64,
                    entry.command,
                    entry.lat,
                    entry.lon,
                    entry.alt,
                    params[0],
                    params[1],
                    params[2],
                    params[3],
                    frame,
                )
            })
            .collect();
        host.console(
            "MISSION",
            &format!("Uploading {} mission item(s) …", specs.len()),
            "info",
        );
        let dialect = host.dialect();
        let wire = with_mission_home_slot(specs, &dialect, &host.home(), host.home_alt_amsl());
        let verdict = dialect.mission_validity_reported().then(|| {
            Arc::new(Verdict {
                total: wire.len() as i64,
                event: Event::new(),
                inner: Mutex::new(VerdictInner {
                    after: f64::INFINITY,
                    state: String::new(),
                    reasons: Vec::new(),
                }),
            })
        });
        let listening = self.listen_for_mission_verdict(host, verdict.as_ref());
        let outcome = (|| -> Option<String> {
            let ack = self.upload_mission(host, &wire, 0);
            if ack != constant_or("MAV_MISSION_ACCEPTED", 0) {
                let result = if ack < 0 { ack } else { mission_ack_to_result(ack) };
                host.command_failure("Mission upload", result);
                return None;
            }
            // Which plan item each uploaded item came from, when the caller
            // lowered a plan. Noted before the verdict is waited for: the
            // mission is on the vehicle whatever it thinks of it.
            let index: Vec<Option<i64>> = items.iter().map(|e| e.item).collect();
            let all = index.iter().all(Option::is_some);
            self.note_vehicle_mission(host, all.then_some(index.as_slice()));
            Some(self.await_mission_verdict(host, verdict.as_ref()))
        })();
        listening(self, host);
        let Some(rejection) = outcome else { return false };
        if !rejection.is_empty() {
            host.set_error(&rejection);
            host.console("MISSION", &rejection, "error");
            return false;
        }
        host.console("MISSION", "Mission accepted by the vehicle", "success");
        true
    }

    /// Arms `verdict` for the receive thread; returns the call that disarms
    /// it. The critical STATUSTEXT lines seen meanwhile are kept, since the
    /// reason for a rejection arrives as one of them.
    #[allow(clippy::type_complexity)]
    fn listen_for_mission_verdict(
        &self,
        host: &dyn MissionHost,
        verdict: Option<&Arc<Verdict>>,
    ) -> Box<dyn FnOnce(&MissionEngine, &dyn MissionHost) + '_> {
        let Some(verdict) = verdict else {
            return Box::new(|_, _| {});
        };
        let collecting = Arc::clone(verdict);
        let sub_id = host.add_console_sub(Arc::new(move |entry: &ConsoleEntry| {
            if entry.name == "STATUSTEXT" && entry.level == "critical" {
                let text = entry.text.trim();
                if !text.is_empty() {
                    collecting
                        .inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .reasons
                        .push(text.to_string());
                }
            }
        }));
        self.st().verdict = Some(Arc::clone(verdict));
        let armed = Arc::clone(verdict);
        Box::new(move |engine, host| {
            let mut st = engine.st();
            if st.verdict.as_ref().is_some_and(|v| Arc::ptr_eq(v, &armed)) {
                st.verdict = None;
            }
            drop(st);
            host.remove_console_sub(sub_id);
        })
    }

    /// The vehicle's reason for refusing a mission it has just accepted, or
    /// "". Also "" when it said nothing in time (an older firmware, a lost
    /// MISSION_CURRENT): the upload's own ACK is then all there is.
    fn await_mission_verdict(&self, host: &dyn MissionHost, verdict: Option<&Arc<Verdict>>) -> String {
        let Some(verdict) = verdict else { return String::new() };
        verdict.inner.lock().unwrap_or_else(|e| e.into_inner()).after = host.now() + MISSION_VERDICT_SETTLE_S;
        let deadline = host.now() + MISSION_VERDICT_WAIT_S;
        while !host.wait_event(&verdict.event, 0.1) {
            if host.now() >= deadline {
                return String::new();
            }
            if host.stopping() || host.abort_waiting() {
                return String::new();
            }
        }
        let inner = verdict.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.state != "no_mission" {
            return String::new();
        }
        // The line that names the mission, else the last critical one.
        let named: Vec<&String> = inner
            .reasons
            .iter()
            .filter(|t| t.to_lowercase().contains("mission"))
            .collect();
        let reasons: Vec<&String> = if named.is_empty() {
            inner.reasons.iter().collect()
        } else {
            named
        };
        if let Some(last) = reasons.last() {
            let lower = last.to_lowercase();
            let text = if lower.starts_with("mission rejected:") {
                last["mission rejected:".len()..].trim_start()
            } else {
                last.as_str()
            };
            return format!("The vehicle rejected the mission: {text}");
        }
        if Self::home_lat_lon(host).is_none() {
            // The one reason the station can see for itself, and the usual one
            // on a bench: every height in the plan is relative to home.
            return "The vehicle rejected the mission: it has no home position yet. Every height in the plan is measured from home, so wait for a GPS fix and upload again.".to_string();
        }
        let hint = host.dialect().mission_rejection_hint();
        format!(
            "The vehicle stored the mission but will not fly it.{}",
            if hint.is_empty() {
                String::new()
            } else {
                format!(" {hint}")
            }
        )
    }

    /// Wipes the mission stored on the vehicle: a zero item mission is the
    /// protocol's own way of saying so, through the same handshake.
    pub fn clear_mission(&self, host: &dyn MissionHost) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if !host.connection_ready() {
            return host.command_failure("Mission clear", -2);
        }
        let ack = self.upload_mission(host, &[], 0);
        if ack != constant_or("MAV_MISSION_ACCEPTED", 0) {
            let result = if ack < 0 { ack } else { mission_ack_to_result(ack) };
            return host.command_failure("Mission clear", result);
        }
        self.note_vehicle_mission(host, Some(&[]));
        host.console("MISSION", "Mission cleared", "success");
        true
    }

    // ------------------------------------------------------------ geofence

    /// Puts `polygon` (`[lon, lat]` corners) on the vehicle as its inclusion
    /// fence, replacing whatever fence it held. Whether the fence acts is a
    /// parameter, written by the caller from the safety schema.
    pub fn upload_fence(&self, host: &dyn MissionHost, polygon: &[(f64, f64)]) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if polygon.len() < MIN_FENCE_VERTICES {
            host.set_error(&format!("an area needs at least {MIN_FENCE_VERTICES} corners"));
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Geofence upload", -2);
        }
        let specs: Vec<MissionItem> = host
            .fence_items(polygon)
            .iter()
            .enumerate()
            .map(|(seq, e)| {
                let mut spec = build_mission_item_spec(
                    seq as i64,
                    e.command,
                    e.lat,
                    e.lon,
                    0.0,
                    e.params[0],
                    e.params[1],
                    e.params[2],
                    e.params[3],
                    Some(constant_or("MAV_FRAME_GLOBAL", 0)),
                );
                spec.current = 0;
                spec
            })
            .collect();
        host.console(
            "GEOFENCE",
            &format!("Uploading a geofence with {} corners …", specs.len()),
            "info",
        );
        let ack = self.upload_mission(host, &specs, MAV_MISSION_TYPE_FENCE);
        if ack != constant_or("MAV_MISSION_ACCEPTED", 0) {
            let result = if ack < 0 { ack } else { mission_ack_to_result(ack) };
            return host.command_failure("Geofence upload", result);
        }
        self.st().vehicle_fence = Some(polygon.to_vec());
        host.console("GEOFENCE", "Geofence accepted by the vehicle", "success");
        true
    }

    /// Removes every fence the vehicle holds: a zero item FENCE transfer.
    pub fn clear_fence(&self, host: &dyn MissionHost) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if !host.connection_ready() {
            return host.command_failure("Geofence clear", -2);
        }
        let ack = self.upload_mission(host, &[], MAV_MISSION_TYPE_FENCE);
        if ack != constant_or("MAV_MISSION_ACCEPTED", 0) {
            let result = if ack < 0 { ack } else { mission_ack_to_result(ack) };
            return host.command_failure("Geofence clear", result);
        }
        self.st().vehicle_fence = Some(Vec::new());
        host.console("GEOFENCE", "Geofence removed from the vehicle", "success");
        true
    }
}
