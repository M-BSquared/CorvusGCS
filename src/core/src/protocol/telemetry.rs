//! Telemetry the bridge turns into state: battery detail and Corvus's own
//! battery estimate, GNSS jamming and spoofing, the controller setpoints the
//! tuning page plots, the transmitter channels, the radio's link quality and
//! heartbeat jitter, and the stream rates the pages ask for while open.
//!
//! The twin of `TelemetryEngine` in `corvus_link` (`protocol/telemetry.rs`).
//! The numbers end up on screen and in tests, so they are computed the way
//! Python computed them: `round()` rounds exact ties to even and `sum()` is
//! the compensated sum of 3.12. The engine owns the heartbeat window, the
//! radio view, the latched cell count and the drain series; what belongs to
//! the bridge (the store, the link, the message intervals) is `TelemetryHost`.

use std::collections::VecDeque;
use std::sync::Mutex;

use corvus_link::plain::crc;
use corvus_link::plain::defs::{message_by_name, Wire};
use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Map, Value};

use super::common::{py_max, py_min, py_round, py_sum};
use crate::battery;
use crate::priority_lock::PriorityLock;

const HEARTBEAT_WINDOW: usize = 30;
const DRAIN_WINDOW: usize = 400;
/// The channels PX4 defines (`rc_config.MAX_CHANNELS`).
pub const MAX_CHANNELS: i64 = 18;

/// SiK RSSI is a 0 to 255 relative scale: about 150 is strong, 40 weak.
const SIK_RSSI_STRONG: f64 = 150.0;
const SIK_RSSI_WEAK: f64 = 40.0;

/// GNSS_INTEGRITY (id 441) from development.xml: its id, CRC seed and payload
/// length ("<IHHBBBBBBBBB").
const GNSS_INTEGRITY_ID: u32 = 441;
const GNSS_INTEGRITY_CRC_EXTRA: u8 = 169;
const GNSS_INTEGRITY_LEN: usize = 17;

/// What the telemetry engine asks of the bridge it runs on.
pub trait TelemetryHost: Send + Sync {
    /// `store.update(**fields)`.
    fn store_update(&self, fields: Map<String, Value>);
    /// The `rc_channels` the store holds, empty when none.
    fn rc_channels(&self) -> Vec<i64>;
    /// The aircraft's system id, 0 when not known yet.
    fn target_system(&self) -> i64;
    fn lock(&self) -> &PriorityLock;
    fn set_error(&self, text: &str);
    fn connection_ready(&self) -> bool;
    /// Asks the vehicle to stream a message at an interval (0 hands the rate
    /// back to the firmware).
    fn set_message_interval(&self, msg_id: i64, interval_us: i64) -> bool;
    /// The interval the session asked for itself at connect, 0 for none.
    fn message_interval(&self, msg_id: i64) -> i64;
    fn is_slow_link(&self) -> bool;
    /// The DISTANCE_SENSOR reading is no longer fresh.
    fn forget_rangefinder(&self);
}

/// RC_CHANNELS.rssi as 0 to 100, or -1 when the receiver does not report it.
/// 255 is "unknown"; above 100 can only be the raw 0 to 254 scale, so it is
/// scaled; at or below 100 it is already the percentage it claims to be.
pub fn rc_rssi_percent(rssi: i64) -> i64 {
    if !(0..255).contains(&rssi) {
        return -1;
    }
    if rssi <= 100 {
        return rssi;
    }
    ((rssi as f64 * 100.0 / 254.0).round_ties_even() as i64).clamp(0, 100)
}

/// A MAVLink attitude quaternion as roll, pitch and yaw in degrees, the
/// convention of ATTITUDE. Pitch is clamped before `asin`, since a quaternion
/// a hair off unit length would otherwise land outside its domain.
pub fn quaternion_to_euler_deg(w: f64, x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
    let pitch = py_max(-1.0, py_min(1.0, 2.0 * (w * y - z * x))).asin();
    let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
    (roll.to_degrees(), pitch.to_degrees(), yaw.to_degrees())
}

/// The fields of an intact GNSS_INTEGRITY frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnssIntegrity {
    pub system: i64,
    pub id: i64,
    pub jamming_state: i64,
    pub spoofing_state: i64,
    pub gnss_signal_quality: i64,
}

/// A raw MAVLink 2 GNSS_INTEGRITY frame as its fields, or None for anything
/// that is not an intact frame of that message (MAVLink 1, short, bad CRC).
/// The trimmed trailing zeros of a v2 payload are put back before reading.
pub fn decode_gnss_integrity(buf: &[u8]) -> Option<GnssIntegrity> {
    if buf.len() < 12 || buf[0] != 0xFD {
        return None;
    }
    let length = buf[1] as usize;
    if u32::from_le_bytes([buf[7], buf[8], buf[9], 0]) != GNSS_INTEGRITY_ID {
        return None;
    }
    let end = 10 + length;
    if buf.len() < end + 2 || length > GNSS_INTEGRITY_LEN {
        return None;
    }
    let sum = crc::accumulate(crc::x25(&buf[1..end]), &[GNSS_INTEGRITY_CRC_EXTRA]);
    if sum != u16::from_le_bytes([buf[end], buf[end + 1]]) {
        return None;
    }
    let mut payload = buf[10..end].to_vec();
    payload.resize(GNSS_INTEGRITY_LEN, 0);
    Some(GnssIntegrity {
        system: buf[5] as i64,
        id: payload[8] as i64,
        jamming_state: payload[10] as i64,
        spoofing_state: payload[11] as i64,
        gnss_signal_quality: payload[15] as i64,
    })
}

fn integrity_state(code: i64) -> &'static str {
    match code {
        1 => "ok",
        2 => "mitigated",
        3 => "detected",
        _ => "",
    }
}

/// Population standard deviation of the intervals, in ms, rounded to 0.1.
fn jitter_ms(times: &VecDeque<f64>) -> f64 {
    if times.len() < 2 {
        return 0.0;
    }
    let intervals: Vec<f64> = times
        .iter()
        .zip(times.iter().skip(1))
        .map(|(a, b)| (b - a) * 1000.0)
        .collect();
    let n = intervals.len() as f64;
    let mean = py_sum(&intervals) / n;
    let squares: Vec<f64> = intervals.iter().map(|v| (v - mean).powi(2)).collect();
    py_round((py_sum(&squares) / n).sqrt(), 1)
}

// ------------------------------------------------------------ message access

fn int_field(msg: &Message, name: &str) -> Option<i64> {
    match msg.get(name)? {
        Wv::Int(i) => Some(*i as i64),
        Wv::Float(f) if f.is_finite() => Some(f.trunc() as i64),
        _ => None,
    }
}

/// `int(getattr(msg, name, fallback) or 0)`.
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

fn int_list(msg: &Message, name: &str) -> Vec<i64> {
    match msg.get(name) {
        Some(Wv::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Wv::Int(i) => Some(*i as i64),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn update(host: &dyn TelemetryHost, pairs: Vec<(&str, Value)>) {
    host.store_update(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
}

fn msg_id(name: &str) -> i64 {
    message_by_name(name, Wire::V2).map_or(0, |m| m.id as i64)
}

struct State {
    hb_times: VecDeque<f64>,
    radio_status_seen: bool,
    uplink_score: i64,
    battery_settings: battery::Settings,
    battery_cells: i64,
    drain_samples: VecDeque<(f64, f64)>,
    drain_source: String,
}

/// The telemetry state of one bridge.
pub struct TelemetryEngine {
    st: Mutex<State>,
}

impl Default for TelemetryEngine {
    fn default() -> Self {
        TelemetryEngine::new()
    }
}

impl TelemetryEngine {
    pub fn new() -> TelemetryEngine {
        TelemetryEngine {
            st: Mutex::new(State {
                hb_times: VecDeque::with_capacity(HEARTBEAT_WINDOW),
                radio_status_seen: false,
                uplink_score: 0,
                battery_settings: battery::settings(None),
                battery_cells: 0,
                drain_samples: VecDeque::with_capacity(DRAIN_WINDOW),
                drain_source: String::new(),
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn hb_times(&self) -> Vec<f64> {
        self.st().hb_times.iter().copied().collect()
    }
    pub fn radio_status_seen(&self) -> bool {
        self.st().radio_status_seen
    }
    pub fn uplink_score(&self) -> i64 {
        self.st().uplink_score
    }
    pub fn battery_cells(&self) -> i64 {
        self.st().battery_cells
    }
    pub fn drain_samples(&self) -> Vec<(f64, f64)> {
        self.st().drain_samples.iter().copied().collect()
    }
    pub fn drain_source(&self) -> String {
        self.st().drain_source.clone()
    }

    /// A heartbeat landed at `stamp` (the store's clock).
    pub fn note_heartbeat(&self, stamp: f64) {
        let mut st = self.st();
        if st.hb_times.len() == HEARTBEAT_WINDOW {
            st.hb_times.pop_front();
        }
        st.hb_times.push_back(stamp);
    }

    /// Start a fresh jitter window and radio view for a new link.
    pub fn forget_link(&self) {
        let mut st = self.st();
        st.hb_times.clear();
        st.radio_status_seen = false;
        st.uplink_score = 0;
    }

    // ---------------------------------------------------------- battery

    /// Install the operator's estimator settings and return them resolved; the
    /// latched cell count goes with the old ones, so a 6S typed in place of an
    /// auto detected 5S takes effect on the next frame.
    pub fn set_battery_settings(&self, raw: Option<&Value>) -> Value {
        let resolved = battery::settings(raw);
        let mut st = self.st();
        st.battery_settings = resolved.clone();
        st.battery_cells = 0;
        resolved.to_json()
    }

    pub fn battery_settings(&self) -> Value {
        self.st().battery_settings.to_json()
    }

    /// Drop the latched cell count and the drain series (a new link may be a
    /// new pack).
    pub fn forget_battery_cells(&self) {
        let mut st = self.st();
        st.battery_cells = 0;
        st.drain_samples.clear();
    }

    /// The battery half of a SYS_STATUS update, the autopilot's percentage and
    /// Corvus's estimate side by side. The cell count is latched on the first
    /// reading of a connection, where the voltage is unambiguous: recomputing
    /// it per frame would let it fall by one somewhere over the field, thirty
    /// points of percentage in a single frame while flying.
    pub fn battery_fields(&self, voltage: f64, current: f64, reported: i64) -> Map<String, Value> {
        let (mut resolved, latched) = {
            let st = self.st();
            (st.battery_settings.clone(), st.battery_cells)
        };
        let configured = resolved.cells;
        if latched != 0 && configured == 0 {
            resolved.cells = latched;
        }
        let est = battery::estimate(voltage, current, Some(&resolved.to_json()));
        let cells = est.cells;
        if cells != 0 && latched == 0 && configured == 0 {
            let mut st = self.st();
            // Only if nothing was latched meanwhile: two frames can race here,
            // and the first to answer is the one closest to plug in.
            if st.battery_cells == 0 {
                st.battery_cells = cells;
            }
        }
        let estimated = est.percent;
        let use_estimate = resolved.estimate && estimated >= 0.0;
        let shown = if use_estimate {
            estimated.round_ties_even() as i64
        } else if reported >= 0 {
            reported
        } else {
            0
        };
        let mut out = Map::new();
        out.insert("battery_voltage".into(), json!(py_round(voltage, 1)));
        out.insert("battery_current".into(), json!(py_round(current, 1)));
        out.insert("battery_percent".into(), json!(shown));
        out.insert("battery_percent_fc".into(), json!(reported));
        out.insert("battery_percent_est".into(), json!(estimated));
        out.insert(
            "battery_source".into(),
            json!(if use_estimate { "estimate" } else { "autopilot" }),
        );
        out.insert("battery_cells".into(), json!(cells));
        out.insert("battery_cell_voltage".into(), json!(est.cell_voltage));
        out
    }

    /// Corvus's own time to empty from the drain rate of the percentage being
    /// flown by: kept only while armed, restarted when that percentage's source
    /// changes. `percent` is None when neither source has one.
    pub fn battery_endurance_fields(
        &self,
        percent: Option<f64>,
        source: &str,
        armed: bool,
        now: f64,
    ) -> Map<String, Value> {
        let mut out = Map::new();
        let samples = {
            let mut st = self.st();
            if !armed || percent.is_none() || source != st.drain_source {
                st.drain_samples.clear();
                st.drain_source = source.to_string();
            }
            let Some(percent) = percent.filter(|_| armed) else {
                out.insert("battery_endurance_est".into(), json!(-1));
                return out;
            };
            if st.drain_samples.len() == DRAIN_WINDOW {
                st.drain_samples.pop_front();
            }
            st.drain_samples.push_back((now, percent));
            st.drain_samples.iter().copied().collect::<Vec<_>>()
        };
        let seconds = battery::drain_endurance(&samples);
        out.insert(
            "battery_endurance_est".into(),
            json!(if seconds >= 0.0 { seconds.trunc() as i64 } else { -1 }),
        );
        out
    }

    /// BATTERY_STATUS detail, first battery only, each field only when the
    /// message carries a real value. A pack voltage in slot 0 (or carried into
    /// slot 1 above 65.534 V) is not a cell reading.
    pub fn handle_battery_status(&self, host: &dyn TelemetryHost, msg: &Message) {
        if int_attr(msg, "id", 0) != 0 {
            return;
        }
        let mut slots = int_list(msg, "voltages");
        slots.extend(int_list(msg, "voltages_ext"));
        let used: Vec<i64> = slots.iter().copied().filter(|v| *v != 0 && *v != 65535).collect();
        let pack_only = !slots.is_empty()
            && ((used.len() == 1 && slots[0] == used[0])
                || (slots[0] == 65534 && used.len() == 2 && slots.get(1) == used.get(1)));
        let cells: Vec<f64> = if pack_only {
            Vec::new()
        } else {
            used.iter().map(|v| py_round(*v as f64 / 1000.0, 3)).collect()
        };
        let mut fields = vec![("battery_cell_voltages", json!(cells))];
        let consumed = int_attr(msg, "current_consumed", -1);
        if consumed >= 0 {
            fields.push(("battery_consumed_mah", json!(consumed as f64)));
        }
        let temperature = int_attr(msg, "temperature", 32767);
        fields.push((
            "battery_temperature",
            if temperature == 32767 {
                Value::Null
            } else {
                json!(py_round(temperature as f64 / 100.0, 1))
            },
        ));
        fields.push((
            "battery_time_remaining",
            json!(int_attr(msg, "time_remaining", 0).max(0)),
        ));
        update(host, fields);
    }

    // ---------------------------------------------------- GNSS integrity

    /// The receiver's own jamming and spoofing verdicts, first receiver of the
    /// aircraft only, from a decoded GNSS_INTEGRITY. `source_system` is the
    /// frame's sender.
    pub fn handle_gnss_integrity(&self, host: &dyn TelemetryHost, msg: &Message, source_system: i64) {
        let fields = GnssIntegrity {
            system: source_system,
            id: int_attr(msg, "id", 0),
            jamming_state: int_attr(msg, "jamming_state", 0),
            spoofing_state: int_attr(msg, "spoofing_state", 0),
            gnss_signal_quality: int_field(msg, "gnss_signal_quality").unwrap_or(255),
        };
        self.publish_gnss(host, &fields);
    }

    /// The same from the raw bytes of an unknown frame (a link still on the
    /// MAVLink 1 view hands GNSS_INTEGRITY over as UNKNOWN_441), which is
    /// decoded and checked here.
    pub fn handle_gnss_integrity_frame(&self, host: &dyn TelemetryHost, frame: &[u8]) {
        if let Some(fields) = decode_gnss_integrity(frame) {
            self.publish_gnss(host, &fields);
        }
    }

    fn publish_gnss(&self, host: &dyn TelemetryHost, f: &GnssIntegrity) {
        let target = host.target_system();
        if f.system != 0 && target != 0 && f.system != target {
            return;
        }
        if f.id != 0 {
            return;
        }
        let signal = f.gnss_signal_quality;
        update(
            host,
            vec![
                ("gps_jamming", json!(integrity_state(f.jamming_state))),
                ("gps_spoofing", json!(integrity_state(f.spoofing_state))),
                (
                    "gps_signal_quality",
                    json!(if (0..=10).contains(&signal) { signal } else { -1 }),
                ),
            ],
        );
    }

    /// Clear the integrity verdicts (a new link may be a new receiver).
    pub fn forget_gnss_integrity(&self, host: &dyn TelemetryHost) {
        update(
            host,
            vec![
                ("gps_jamming", json!("")),
                ("gps_spoofing", json!("")),
                ("gps_signal_quality", json!(-1)),
            ],
        );
    }

    // -------------------------------------------------------- setpoints

    /// ATTITUDE_TARGET: the attitude and body rate setpoints, in degrees. A
    /// short quaternion leaves the attitude out: an invented setpoint on a
    /// tuning graph is worse than none.
    pub fn handle_attitude_target(&self, host: &dyn TelemetryHost, msg: &Message) {
        let rate = |name: &str| float_field(msg, name).map(f64::to_degrees);
        let (Some(roll_rate), Some(pitch_rate), Some(yaw_rate)) =
            (rate("body_roll_rate"), rate("body_pitch_rate"), rate("body_yaw_rate"))
        else {
            return;
        };
        let quat: Vec<f64> = match msg.get("q") {
            Some(Wv::Array(items)) => items
                .iter()
                .filter_map(|v| match v {
                    Wv::Float(f) => Some(*f),
                    Wv::Int(i) => Some(*i as f64),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut fields = vec![
            ("rollspeed_sp", json!(py_round(roll_rate, 1))),
            ("pitchspeed_sp", json!(py_round(pitch_rate, 1))),
            ("yawspeed_sp", json!(py_round(yaw_rate, 1))),
            ("setpoints_live", json!(true)),
        ];
        if quat.len() >= 4 {
            let (roll, pitch, yaw) = quaternion_to_euler_deg(quat[0], quat[1], quat[2], quat[3]);
            fields.push(("roll_sp", json!(py_round(roll, 1))));
            fields.push(("pitch_sp", json!(py_round(pitch, 1))));
            fields.push(("yaw_sp", json!(py_round(yaw, 1))));
        }
        update(host, fields);
    }

    /// POSITION_TARGET_LOCAL_NED: the velocity setpoint only, whatever the type
    /// mask says (PX4 fills it with the controller's own setpoint).
    pub fn handle_position_target(&self, host: &dyn TelemetryHost, msg: &Message) {
        let v = |name: &str| float_field(msg, name).map(|x| json!(py_round(x, 2)));
        let (Some(vx), Some(vy), Some(vz)) = (v("vx"), v("vy"), v("vz")) else {
            return;
        };
        update(
            host,
            vec![
                ("vx_sp", vx),
                ("vy_sp", vy),
                ("vz_sp", vz),
                ("setpoints_live", json!(true)),
            ],
        );
    }

    // ------------------------------------------------------- RC channels

    /// The transmitter channels in microseconds. RC_CHANNELS carries all of
    /// them; RC_CHANNELS_RAW (`raw`) one bank of eight named by `port`, merged
    /// into what the other bank published. UINT16_MAX ("not delivered") is 0.
    pub fn handle_rc_channels(&self, host: &dyn TelemetryHost, msg: &Message, raw: bool) {
        let offset = if raw { int_attr(msg, "port", 0) * 8 } else { 0 };
        let width = if raw { 8 } else { MAX_CHANNELS };
        if !(0..MAX_CHANNELS).contains(&offset) {
            return;
        }
        let mut values: Vec<i64> = Vec::new();
        for index in 1..=width {
            let Some(pulse) = int_field(msg, &format!("chan{index}_raw")) else {
                break;
            };
            values.push(if pulse == 65535 { 0 } else { pulse.clamp(0, 65534) });
        }
        let (channels, count) = if raw {
            // Merge the bank into whatever the other bank already published,
            // so a receiver split across two messages does not flip between
            // eight channels and the other eight on every frame.
            let room = (MAX_CHANNELS - offset) as usize;
            values.truncate(room);
            if values.is_empty() {
                return;
            }
            let mut existing = host.rc_channels();
            let start = offset as usize;
            let needed = start + values.len();
            if existing.len() < needed {
                existing.resize(needed, 0);
            }
            existing[start..start + values.len()].copy_from_slice(&values);
            let count = existing.len() as i64;
            (existing, count)
        } else {
            // chancount is what the receiver actually delivers; trust it over
            // the fixed 18 slots, but never let a bad value grow the list.
            let claimed = int_attr(msg, "chancount", 0);
            let count = if claimed > 0 && claimed as usize <= values.len() {
                values.truncate(claimed as usize);
                claimed
            } else {
                values.len() as i64
            };
            (values, count)
        };
        let rssi = rc_rssi_percent(int_attr(msg, "rssi", 255));
        update(
            host,
            vec![
                ("rc_channels", json!(channels)),
                ("rc_channel_count", json!(count)),
                ("rc_rssi", json!(rssi)),
                ("rc_live", json!(true)),
            ],
        );
    }

    // ------------------------------------------------------ link quality

    /// RADIO_STATUS: the uplink score from the drone's signal as the ground
    /// radio sees it (remrssi) and the radio's buffer. remrssi 0 or 255 is no
    /// reading, which keeps the last score rather than flapping to poor.
    pub fn handle_radio_status(&self, host: &dyn TelemetryHost, msg: &Message) {
        let remrssi = float_field(msg, "remrssi").unwrap_or(0.0);
        let txbuf = float_field(msg, "txbuf").unwrap_or(0.0);
        let rxerrors = int_attr(msg, "rxerrors", 0);
        let fixed = int_attr(msg, "fixed", 0);
        let score = {
            let mut st = self.st();
            st.radio_status_seen = true;
            if remrssi != 0.0 && remrssi != 255.0 {
                let remrssi_pct = py_max(
                    0.0,
                    py_min(
                        100.0,
                        (remrssi - SIK_RSSI_WEAK) / (SIK_RSSI_STRONG - SIK_RSSI_WEAK) * 100.0,
                    ),
                );
                let txbuf_pct = py_max(0.0, py_min(100.0, txbuf));
                st.uplink_score = ((0.7 * remrssi_pct + 0.3 * txbuf_pct).round_ties_even() as i64).clamp(0, 100);
            }
            st.uplink_score
        };
        update(
            host,
            vec![
                ("uplink", json!(score)),
                ("uplink_rssi", json!(remrssi)),
                ("uplink_rxerrors", json!(rxerrors)),
                ("uplink_fixed", json!(fixed)),
            ],
        );
        self.publish_link_quality(host);
    }

    /// The rolling standard deviation of heartbeat intervals, in ms; 0 before
    /// there are two heartbeats.
    pub fn publish_heartbeat_jitter(&self, host: &dyn TelemetryHost) {
        let jitter = jitter_ms(&self.st().hb_times);
        update(host, vec![("heartbeat_jitter_ms", json!(jitter))]);
    }

    pub fn current_jitter_ms(&self) -> f64 {
        jitter_ms(&self.st().hb_times)
    }

    /// good, fair or poor from the uplink score and the jitter; without a radio
    /// (UDP SITL) a fresh heartbeat is good, and staleness is the drop path's
    /// to judge.
    pub fn publish_link_quality(&self, host: &dyn TelemetryHost) {
        let (jitter, seen, uplink) = {
            let st = self.st();
            (jitter_ms(&st.hb_times), st.radio_status_seen, st.uplink_score)
        };
        let quality = if !seen || (uplink >= 70 && jitter < 50.0) {
            "good"
        } else if uplink >= 40 {
            "fair"
        } else {
            "poor"
        };
        update(host, vec![("link_quality", json!(quality))]);
    }

    // ------------------------------------------------------ stream rates

    /// The interval a page's stream request should really send for `msg_id`:
    /// never slower than the session's own rate for that message, and that
    /// rate back (or 0, the firmware's default) when the page is done.
    pub fn stream_interval_us(&self, host: &dyn TelemetryHost, msg_id: i64, requested_us: Option<i64>) -> i64 {
        let baseline = host.message_interval(msg_id);
        match requested_us {
            None => baseline,
            Some(r) if baseline > 0 => r.min(baseline),
            Some(r) => r,
        }
    }

    fn rate_interval(host: &dyn TelemetryHost, rate_hz: i64, what: &str) -> Option<i64> {
        if (1..=50).contains(&rate_hz) {
            Some(((1_000_000.0 / rate_hz as f64) as i64).max(1))
        } else {
            host.set_error(&format!("{what} rate must be between 1 and 50 Hz"));
            None
        }
    }

    /// VIBRATION at `rate_hz` while the vibration page is open, the firmware's
    /// default (0) after.
    pub fn vibration_stream(&self, host: &dyn TelemetryHost, enabled: bool, rate_hz: i64) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        let interval = if enabled {
            if !(1..=50).contains(&rate_hz) {
                host.set_error("vibration rate must be between 1 and 50 Hz");
                return false;
            }
            ((1_000_000.0 / rate_hz as f64) as i64).max(1)
        } else {
            0
        };
        host.set_message_interval(msg_id("VIBRATION"), interval)
    }

    /// DISTANCE_SENSOR for the top bar while the operator has it on.
    pub fn rangefinder_stream(&self, host: &dyn TelemetryHost, enabled: bool) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if !host.connection_ready() {
            host.set_error("not connected");
            update(host, vec![("rangefinder_distance", Value::Null)]);
            return false;
        }
        let requested = enabled.then(|| if host.is_slow_link() { 1_000_000 } else { 500_000 });
        let id = msg_id("DISTANCE_SENSOR");
        let interval = self.stream_interval_us(host, id, requested);
        let ok = host.set_message_interval(id, interval);
        if !enabled {
            host.forget_rangefinder();
            update(host, vec![("rangefinder_distance", Value::Null)]);
        }
        ok
    }

    /// RC_CHANNELS at a calibration rate while the Radio Control page is open,
    /// the session's own rate after. Read only telemetry, safe while armed.
    pub fn rc_stream(&self, host: &dyn TelemetryHost, enabled: bool, rate_hz: i64) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        let requested = if enabled {
            match Self::rate_interval(host, rate_hz, "RC") {
                Some(i) => Some(i),
                None => return false,
            }
        } else {
            None
        };
        let id = msg_id("RC_CHANNELS");
        let interval = self.stream_interval_us(host, id, requested);
        if !host.connection_ready() {
            // Without a reason the page opened over a dead link would reach the
            // browser as a conflict rather than as "there is no vehicle".
            host.set_error("not connected");
            update(host, vec![("rc_live", json!(false))]);
            return false;
        }
        let ok = host.set_message_interval(id, interval);
        if !enabled {
            update(host, vec![("rc_live", json!(false))]);
        }
        ok
    }

    /// The tuning page's commanded and achieved pairs at `rate_hz`, each handed
    /// back after. True only if every message was accepted: a firmware that
    /// refuses one still gets the others.
    pub fn tuning_stream(&self, host: &dyn TelemetryHost, enabled: bool, rate_hz: i64) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        let requested = if enabled {
            match Self::rate_interval(host, rate_hz, "tuning") {
                Some(i) => Some(i),
                None => return false,
            }
        } else {
            None
        };
        if !host.connection_ready() {
            host.set_error("not connected");
            update(host, vec![("setpoints_live", json!(false))]);
            return false;
        }
        let mut ok = true;
        for name in ["ATTITUDE", "ATTITUDE_TARGET", "POSITION_TARGET_LOCAL_NED"] {
            let id = msg_id(name);
            let interval = self.stream_interval_us(host, id, requested);
            if !host.set_message_interval(id, interval) {
                ok = false;
            }
        }
        if !enabled {
            update(host, vec![("setpoints_live", json!(false))]);
        }
        ok
    }
}
