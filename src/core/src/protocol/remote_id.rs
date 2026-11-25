//! Remote ID: the identity a ground station broadcasts for its aircraft, as
//! the four OPEN_DRONE_ID messages it sends at 1 Hz, and the aircraft's
//! OPEN_DRONE_ID_ARM_STATUS verdict on it.
//!
//! The twin of `corvus_link`'s Remote ID engine (`protocol/remote_id.rs`) and
//! of the model it calls. Every field arrives from a browser and leaves on a
//! radio that a regulator is listening to, so the stored identity is coerced
//! on the way in: an out of range enum falls back to its default, text is cut
//! to printable ASCII and the field width, numbers are clamped. The coercion
//! follows Python's own `int()`, `float()` and `str()`, because the identity
//! was a Python dict and a config file written by it must read the same.
//! The filing checks and option lists the Remote ID page shows are the
//! page's, not the broadcast's.

use std::sync::Mutex;

use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Map, Value};

use super::common::{py_float_of, py_int_of, py_round, py_str, py_truthy};
use super::targeted;

/// Field widths from the MAVLink message definitions: the hard cap on what can
/// be sent, since a longer value shifts the field rather than truncating.
pub const UAS_ID_MAX: usize = 20;
pub const OPERATOR_ID_MAX: usize = 20;
pub const DESCRIPTION_MAX: usize = 23;
pub const ARM_ERROR_MAX: usize = 50;
/// The ODID "unknown" sentinel for altitudes and the operational volume.
pub const ALTITUDE_UNKNOWN: f64 = -1000.0;

pub const ARM_STATUS_GOOD: i64 = 0;
pub const ARM_STATUS_FAIL: i64 = 1;
const ID_TYPE_SERIAL: i64 = 1;
const LOCATION_TAKEOFF: i64 = 0;
const LOCATION_FIXED: i64 = 2;
const CLASSIFICATION_UNDECLARED: i64 = 0;
const CLASSIFICATION_EU: i64 = 1;
const REGIONS: [&str; 2] = ["faa", "eu"];
/// How recent a send must be for the broadcast to count as running.
pub const BROADCAST_FRESH_S: f64 = 5.0;

/// The accepted enum codes and the ODID epoch, as a frozen copy of the standard.
/// This is what the broadcast uses unless the `remoteid-model` feature is on.
#[cfg_attr(feature = "remoteid-model", cfg(test))]
mod frozen {
    use std::ops::RangeInclusive;

    pub const ODID_EPOCH_UNIX: f64 = 1_546_300_800.0;

    const ID_TYPES: RangeInclusive<i64> = 0..=4;
    const UA_TYPES: RangeInclusive<i64> = 0..=15;
    const OPERATOR_ID_TYPES: RangeInclusive<i64> = 0..=0;
    const DESCRIPTION_TYPES: RangeInclusive<i64> = 0..=2;
    const LOCATION_TYPES: RangeInclusive<i64> = 0..=2;
    const CLASSIFICATION_TYPES: RangeInclusive<i64> = 0..=1;
    const CATEGORIES_EU: RangeInclusive<i64> = 0..=3;
    const CLASSES_EU: RangeInclusive<i64> = 0..=7;

    pub fn id_type(v: i64) -> bool {
        ID_TYPES.contains(&v)
    }
    pub fn ua_type(v: i64) -> bool {
        UA_TYPES.contains(&v)
    }
    pub fn operator_id_type(v: i64) -> bool {
        OPERATOR_ID_TYPES.contains(&v)
    }
    pub fn description_type(v: i64) -> bool {
        DESCRIPTION_TYPES.contains(&v)
    }
    pub fn location_type(v: i64) -> bool {
        LOCATION_TYPES.contains(&v)
    }
    pub fn classification_type(v: i64) -> bool {
        CLASSIFICATION_TYPES.contains(&v)
    }
    pub fn category_eu(v: i64) -> bool {
        CATEGORIES_EU.contains(&v)
    }
    pub fn class_eu(v: i64) -> bool {
        CLASSES_EU.contains(&v)
    }
}

/// The same spans as the `remoteid` crate names them (an opt in build only).
#[cfg(feature = "remoteid-model")]
mod model {
    use remoteid::{CategoryEu, ClassEu, ClassificationType, DescriptionType, IdType, OperatorLocationType, UaType};

    pub const ODID_EPOCH_UNIX: f64 = remoteid::EPOCH_2019_UNIX as f64;

    pub fn id_type(v: i64) -> bool {
        u8::try_from(v).is_ok_and(|b| !matches!(IdType::from(b), IdType::Other(_)))
    }
    pub fn ua_type(v: i64) -> bool {
        u8::try_from(v).is_ok_and(|b| !matches!(UaType::from(b), UaType::Other(_)))
    }
    pub fn description_type(v: i64) -> bool {
        u8::try_from(v).is_ok_and(|b| !matches!(DescriptionType::from(b), DescriptionType::Other(_)))
    }
    pub fn location_type(v: i64) -> bool {
        u8::try_from(v).is_ok_and(|b| !matches!(OperatorLocationType::from(b), OperatorLocationType::Other(_)))
    }
    pub fn classification_type(v: i64) -> bool {
        u8::try_from(v).is_ok_and(|b| !matches!(ClassificationType::from(b), ClassificationType::Other(_)))
    }
    pub fn category_eu(v: i64) -> bool {
        u8::try_from(v).is_ok_and(|b| !matches!(CategoryEu::from(b), CategoryEu::Other(_)))
    }
    pub fn class_eu(v: i64) -> bool {
        u8::try_from(v).is_ok_and(|b| !matches!(ClassEu::from(b), ClassEu::Other(_)))
    }
    /// The Operator ID message has no sub type field: 0 is its only valid code.
    pub fn operator_id_type(v: i64) -> bool {
        v == 0
    }
}

#[cfg(not(feature = "remoteid-model"))]
use frozen as spans;
#[cfg(feature = "remoteid-model")]
use model as spans;

use spans::ODID_EPOCH_UNIX;

fn enum_value(raw: Option<&Value>, known: impl Fn(i64) -> bool, default: i64) -> i64 {
    match raw.and_then(py_int_of) {
        Some(v) if known(v) => v,
        _ => default,
    }
}

/// `min(max(value, lo), hi)` as Python orders it.
fn clamp(value: f64, lo: f64, hi: f64) -> f64 {
    let low = if lo > value { lo } else { value };
    if hi < low {
        hi
    } else {
        low
    }
}

fn number(raw: Option<&Value>, default: f64, lo: f64, hi: f64) -> f64 {
    match raw.and_then(py_float_of) {
        Some(v) if v.is_finite() => clamp(v, lo, hi),
        _ => default,
    }
}

fn count(raw: Option<&Value>, default: i64, lo: i64, hi: i64) -> i64 {
    let Some(raw) = raw else { return default };
    if raw.is_boolean() {
        return default;
    }
    match py_float_of(raw) {
        Some(v) if v.is_finite() => clamp(v.trunc(), lo as f64, hi as f64) as i64,
        _ => default,
    }
}

/// Python's `str.isspace()`, which also counts the separators 0x1C to 0x1F.
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Printable ASCII of `text`, at most `limit` characters.
fn clean_text(text: &str, limit: usize) -> String {
    let without_nul: String = text.chars().filter(|c| *c != '\0').collect();
    without_nul
        .trim_matches(python_space)
        .chars()
        .filter(|c| (0x20..=0x7E).contains(&(*c as u32)))
        .take(limit)
        .collect()
}

fn text(raw: Option<&Value>, limit: usize) -> String {
    match raw {
        None | Some(Value::Null) => String::new(),
        Some(v) => clean_text(&py_str(v), limit),
    }
}

/// A fixed width char field read as bytes: up to its first NUL, ASCII only.
fn text_of_bytes(bytes: &[u8], limit: usize) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let ascii: String = bytes[..end]
        .iter()
        .filter(|b| b.is_ascii())
        .map(|b| *b as char)
        .collect();
    clean_text(&ascii, limit)
}

/// ASCII, zero padded to exactly `length` bytes.
fn fixed_bytes(text: &str, length: usize) -> Vec<u8> {
    let mut raw: Vec<u8> = text.chars().filter(char::is_ascii).map(|c| c as u8).collect();
    raw.truncate(length);
    raw.resize(length, 0);
    raw
}

/// The stored identity, every field bounded and typed.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteId {
    /// Off unless the operator turned it on: broadcasting somebody else's
    /// serial number, or a half filled form, is worse than broadcasting nothing.
    pub enabled: bool,
    pub region: String,
    pub id_type: i64,
    pub ua_type: i64,
    pub uas_id: String,
    pub operator_id_type: i64,
    pub operator_id: String,
    pub description_type: i64,
    pub description: String,
    pub operator_location_type: i64,
    pub operator_latitude: f64,
    pub operator_longitude: f64,
    pub operator_altitude_geo: f64,
    pub classification_type: i64,
    pub category_eu: i64,
    pub class_eu: i64,
    pub area_count: i64,
    pub area_radius: i64,
    pub area_ceiling: f64,
    pub area_floor: f64,
}

impl RemoteId {
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "region": self.region,
            "basic_id": {"id_type": self.id_type, "ua_type": self.ua_type, "uas_id": self.uas_id},
            "operator_id": {"operator_id_type": self.operator_id_type, "operator_id": self.operator_id},
            "self_id": {"description_type": self.description_type, "description": self.description},
            "system": {
                "operator_location_type": self.operator_location_type,
                "operator_latitude": self.operator_latitude,
                "operator_longitude": self.operator_longitude,
                "operator_altitude_geo": self.operator_altitude_geo,
                "classification_type": self.classification_type,
                "category_eu": self.category_eu,
                "class_eu": self.class_eu,
                "area_count": self.area_count,
                "area_radius": self.area_radius,
                "area_ceiling": self.area_ceiling,
                "area_floor": self.area_floor,
            },
        })
    }
}

/// The identity of `raw`, every field bounded and typed. Called on the way out
/// of the config file and again on the way in from the page, so the two can
/// never disagree about what a stored value means.
pub fn settings(raw: Option<&Value>) -> RemoteId {
    let empty = Map::new();
    let data = raw.and_then(Value::as_object).unwrap_or(&empty);
    let section = |key: &str| data.get(key).and_then(Value::as_object).unwrap_or(&empty);
    let (basic, operator, self_id, system) = (
        section("basic_id"),
        section("operator_id"),
        section("self_id"),
        section("system"),
    );
    let region = match data.get("region").filter(|r| py_truthy(r)) {
        Some(r) => py_str(r).to_lowercase(),
        None => "eu".to_string(),
    };
    let area = |key: &str| number(system.get(key), ALTITUDE_UNKNOWN, -1000.0, 31767.0);
    RemoteId {
        enabled: data.get("enabled").is_some_and(py_truthy),
        region: if REGIONS.contains(&region.as_str()) {
            region
        } else {
            "eu".into()
        },
        id_type: enum_value(basic.get("id_type"), spans::id_type, ID_TYPE_SERIAL),
        ua_type: enum_value(basic.get("ua_type"), spans::ua_type, 0),
        uas_id: text(basic.get("uas_id"), UAS_ID_MAX),
        operator_id_type: enum_value(operator.get("operator_id_type"), spans::operator_id_type, 0),
        operator_id: text(operator.get("operator_id"), OPERATOR_ID_MAX),
        description_type: enum_value(self_id.get("description_type"), spans::description_type, 0),
        description: text(self_id.get("description"), DESCRIPTION_MAX),
        operator_location_type: enum_value(
            system.get("operator_location_type"),
            spans::location_type,
            LOCATION_TAKEOFF,
        ),
        operator_latitude: number(system.get("operator_latitude"), 0.0, -90.0, 90.0),
        operator_longitude: number(system.get("operator_longitude"), 0.0, -180.0, 180.0),
        operator_altitude_geo: area("operator_altitude_geo"),
        classification_type: enum_value(
            system.get("classification_type"),
            spans::classification_type,
            CLASSIFICATION_UNDECLARED,
        ),
        category_eu: enum_value(system.get("category_eu"), spans::category_eu, 0),
        class_eu: enum_value(system.get("class_eu"), spans::class_eu, 0),
        area_count: count(system.get("area_count"), 1, 1, 65535),
        area_radius: count(system.get("area_radius"), 0, 0, 65535),
        area_ceiling: area("area_ceiling"),
        area_floor: area("area_floor"),
    }
}

/// The identity a station that has never been configured holds.
pub fn defaults() -> RemoteId {
    settings(None)
}

/// Seconds since the ODID epoch (2019-01-01 UTC), clamped to uint32, from Unix
/// time in seconds.
pub fn system_timestamp(unix_seconds: f64) -> u32 {
    let whole = (unix_seconds - ODID_EPOCH_UNIX).trunc();
    if whole <= 0.0 {
        0
    } else if whole >= u32::MAX as f64 {
        u32::MAX
    } else {
        whole as u32
    }
}

fn int(v: i64) -> Wv {
    Wv::Int(v as i128)
}

fn bytes(text: &str, length: usize) -> Wv {
    Wv::Bytes(fixed_bytes(text, length))
}

/// The four Remote ID messages for `identity`, addressed to the vehicle. All
/// four go every cycle rather than only the ones that changed: they are not a
/// configuration write the vehicle stores, both stacks time the last one out
/// and fail the pre-arm check when the stream stops. `id_or_mac` is 20
/// reserved zero bytes in every one of them.
pub fn messages(identity: &RemoteId, target: (u8, u8), unix_seconds: f64) -> Vec<Message> {
    let addressed = |name: &str| targeted(name, target).with("id_or_mac", Wv::Array(vec![int(0); 20]));
    // A fixed position is the only one whose coordinates are the operator's. On
    // the take off setting the vehicle supplies them, so a stale pair of
    // numbers alongside that flag would be a position nobody claimed.
    let (latitude, longitude, altitude) = if identity.operator_location_type == LOCATION_FIXED {
        (
            (identity.operator_latitude * 1e7).round_ties_even() as i64,
            (identity.operator_longitude * 1e7).round_ties_even() as i64,
            identity.operator_altitude_geo,
        )
    } else {
        (0, 0, ALTITUDE_UNKNOWN)
    };
    // The category and class only mean anything under the EU classification.
    let (category_eu, class_eu) = if identity.classification_type == CLASSIFICATION_EU {
        (identity.category_eu, identity.class_eu)
    } else {
        (0, 0)
    };
    vec![
        addressed("OPEN_DRONE_ID_BASIC_ID")
            .with("id_type", int(identity.id_type))
            .with("ua_type", int(identity.ua_type))
            // uint8_t[20] in the dialect, unlike the two char fields below.
            .with(
                "uas_id",
                Wv::Array(
                    fixed_bytes(&identity.uas_id, UAS_ID_MAX)
                        .into_iter()
                        .map(|b| int(i64::from(b)))
                        .collect(),
                ),
            ),
        addressed("OPEN_DRONE_ID_OPERATOR_ID")
            .with("operator_id_type", int(identity.operator_id_type))
            .with("operator_id", bytes(&identity.operator_id, OPERATOR_ID_MAX)),
        addressed("OPEN_DRONE_ID_SELF_ID")
            .with("description_type", int(identity.description_type))
            .with("description", bytes(&identity.description, DESCRIPTION_MAX)),
        addressed("OPEN_DRONE_ID_SYSTEM")
            .with("operator_location_type", int(identity.operator_location_type))
            .with("classification_type", int(identity.classification_type))
            .with("operator_latitude", int(latitude))
            .with("operator_longitude", int(longitude))
            .with("area_count", int(identity.area_count))
            .with("area_radius", int(identity.area_radius))
            .with("area_ceiling", Wv::Float(identity.area_ceiling))
            .with("area_floor", Wv::Float(identity.area_floor))
            .with("category_eu", int(category_eu))
            .with("class_eu", int(class_eu))
            .with("operator_altitude_geo", Wv::Float(altitude))
            .with("timestamp", int(i64::from(system_timestamp(unix_seconds)))),
    ]
}

/// The vehicle's verdict on the identity it is sent.
#[derive(Debug, Clone, PartialEq)]
pub struct ArmStatus {
    pub ok: bool,
    pub status: i64,
    pub error: String,
}

impl ArmStatus {
    pub fn to_json(&self) -> Value {
        json!({"ok": self.ok, "status": self.status, "error": self.error})
    }
}

/// An OPEN_DRONE_ID_ARM_STATUS as something renderable. `status` that does not
/// read as an integer is a failure.
pub fn arm_status(status: Option<i64>, error: &[u8]) -> ArmStatus {
    let code = status.unwrap_or(ARM_STATUS_FAIL);
    ArmStatus {
        ok: code == ARM_STATUS_GOOD,
        status: code,
        error: text_of_bytes(error, ARM_ERROR_MAX),
    }
}

/// What the Remote ID engine asks of the bridge it runs on.
pub trait RemoteIdHost: Send + Sync {
    /// Can the live link carry the Remote ID messages at all? They are
    /// MAVLink 2 only, so a MAVLink 1 only autopilot cannot be told an identity.
    fn link_supports_remote_id(&self) -> bool;
    /// Sends one message on the link. `Ok(false)` is "the link is gone".
    fn send(&self, msg: Message) -> Result<bool, String>;
    fn target(&self) -> (u8, u8);
    /// The monotonic clock, in seconds.
    fn now(&self) -> f64;
    /// UTC as Unix seconds.
    fn unix_now(&self) -> f64;
}

#[derive(Default)]
struct State {
    identity: Option<RemoteId>,
    sent_at: f64,
    error: String,
    arm: Option<ArmStatus>,
}

/// The Remote ID state of one bridge.
pub struct RemoteIdEngine {
    state: Mutex<State>,
}

impl Default for RemoteIdEngine {
    fn default() -> Self {
        RemoteIdEngine::new()
    }
}

impl RemoteIdEngine {
    pub fn new() -> RemoteIdEngine {
        RemoteIdEngine {
            state: Mutex::new(State {
                identity: Some(defaults()),
                ..State::default()
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn identity(&self) -> RemoteId {
        self.st().identity.clone().unwrap_or_else(defaults)
    }

    /// Installs the identity and returns it resolved. Takes effect on the next
    /// 1 Hz cycle rather than the next connection: an operator who has just
    /// corrected a mistyped serial number is standing beside an aircraft that
    /// is still broadcasting the old one.
    pub fn set_remote_id(&self, raw: Option<&Value>) -> Value {
        let resolved = settings(raw);
        let mut st = self.st();
        st.identity = Some(resolved.clone());
        // The old identity's send history says nothing about the new one, and
        // the vehicle has not judged it yet.
        st.sent_at = 0.0;
        st.error.clear();
        st.arm = None;
        resolved.to_json()
    }

    /// The identity currently in force, resolved against the defaults.
    pub fn remote_id_settings(&self) -> Value {
        settings(Some(&self.identity().to_json())).to_json()
    }

    /// Drops everything Remote ID learned from the link that just ended.
    pub fn forget_session(&self) {
        let mut st = self.st();
        st.sent_at = 0.0;
        st.error.clear();
        st.arm = None;
    }

    pub fn remote_id_supported(&self, host: &dyn RemoteIdHost) -> bool {
        host.link_supports_remote_id()
    }

    /// Sends one round of the identity. Never raises: it is called from the
    /// heartbeat loop, where a throw would turn a link that cannot carry ODID
    /// into a 10 Hz heartbeat spin. The reason is recorded for the page.
    pub fn send_remote_id(&self, host: &dyn RemoteIdHost) {
        let identity = self.identity();
        if !identity.enabled {
            return;
        }
        if !host.link_supports_remote_id() {
            self.st().error = "this link is MAVLink 1, and the Remote ID messages are MAVLink 2 only".into();
            return;
        }
        for msg in messages(&identity, host.target(), host.unix_now()) {
            match host.send(msg) {
                Ok(true) => {}
                Ok(false) => return,
                Err(e) => {
                    self.st().error = if e.is_empty() { "error".into() } else { e };
                    return;
                }
            }
        }
        let mut st = self.st();
        st.sent_at = host.now();
        st.error.clear();
    }

    /// Latches the vehicle's verdict on the identity it is being sent: the
    /// only feedback a ground station gets, and the reason in plain text.
    pub fn handle_arm_status(&self, msg: &Message) {
        let status = msg.get("status").and_then(Wv::as_i64);
        let error = match msg.get("error") {
            Some(Wv::Bytes(b)) => b.clone(),
            _ => Vec::new(),
        };
        self.st().arm = Some(arm_status(status, &error));
    }

    /// What the broadcast is doing right now. `broadcasting` is "a send landed
    /// in the last few seconds", not "enabled is true": the switch is the
    /// operator's intention and this is what the aircraft is being told.
    pub fn remote_id_status(&self, host: &dyn RemoteIdHost) -> Value {
        let (enabled, sent_at, error, arm) = {
            let st = self.st();
            (
                st.identity.as_ref().is_some_and(|i| i.enabled),
                st.sent_at,
                st.error.clone(),
                st.arm.clone(),
            )
        };
        let age = (sent_at != 0.0).then(|| host.now() - sent_at);
        json!({
            "enabled": enabled,
            "supported": self.remote_id_supported(host),
            "broadcasting": enabled && age.is_some_and(|a| a < BROADCAST_FRESH_S),
            "last_sent_age": age.map(|a| py_round(a, 1)),
            "error": error,
            "arm_status": arm.map(|a| a.to_json()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_enums_accept_exactly_the_standard_spans() {
        assert!((0..=4).all(spans::id_type) && !spans::id_type(5));
        assert!((0..=15).all(spans::ua_type) && !spans::ua_type(16));
        assert!(spans::operator_id_type(0) && !spans::operator_id_type(1));
        assert!((0..=2).all(spans::description_type) && !spans::description_type(3));
        assert!((0..=2).all(spans::location_type) && !spans::location_type(3));
        assert!((0..=1).all(spans::classification_type) && !spans::classification_type(2));
        assert!((0..=3).all(spans::category_eu) && !spans::category_eu(4));
        assert!((0..=7).all(spans::class_eu) && !spans::class_eu(8));
        assert!(!spans::id_type(-1) && !spans::class_eu(-1));
    }

    #[test]
    fn system_timestamp_counts_from_the_odid_epoch() {
        assert_eq!(frozen::ODID_EPOCH_UNIX, 1_546_300_800.0);
        assert_eq!(system_timestamp(ODID_EPOCH_UNIX), 0);
        assert_eq!(system_timestamp(ODID_EPOCH_UNIX + 10.0), 10);
    }

    /// The guard for the opt in build: the crate and the frozen copy must agree on
    /// every code. A crate release that moves a span fails here, not on the air.
    #[cfg(feature = "remoteid-model")]
    #[test]
    fn remoteid_model_matches_the_frozen_spans() {
        assert_eq!(model::ODID_EPOCH_UNIX, frozen::ODID_EPOCH_UNIX);
        for v in -300..=300_i64 {
            assert_eq!(model::id_type(v), frozen::id_type(v), "id_type {v}");
            assert_eq!(model::ua_type(v), frozen::ua_type(v), "ua_type {v}");
            assert_eq!(
                model::operator_id_type(v),
                frozen::operator_id_type(v),
                "operator_id_type {v}"
            );
            assert_eq!(
                model::description_type(v),
                frozen::description_type(v),
                "description_type {v}"
            );
            assert_eq!(model::location_type(v), frozen::location_type(v), "location_type {v}");
            assert_eq!(
                model::classification_type(v),
                frozen::classification_type(v),
                "classification_type {v}"
            );
            assert_eq!(model::category_eu(v), frozen::category_eu(v), "category_eu {v}");
            assert_eq!(model::class_eu(v), frozen::class_eu(v), "class_eu {v}");
        }
    }
}
