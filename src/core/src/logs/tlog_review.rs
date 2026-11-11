//! Telemetry Review (`tlog_review.py`): a recorded tlog reduced to the same
//! plots a ULog gets, from what a telemetry link carries.
//!
//! The payload has the shape `flight_review::review` returns, so the Analysis
//! page draws a tlog with the renderer it already has.

#![allow(clippy::needless_range_loop)]

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::flight_review::{
    align, capitalize, correlate, decimate, during, f, finding, held_floor, limits_for, normalize_sensitivity_str,
    percentile, plot, rise, round_to, series_value, time_above, within, Limits, Mode, Span, GROUP_ORDER,
    MESSAGE_GRACE_S, MIN_MODE_S, RECOVERY_MODES,
};
use crate::dialect::{dialect_for_id, Stack};
use crate::json::num;
use crate::link::plain::frame::{Event, Parser};
use crate::link::plain::{Message, Value as Mv};
use crate::protocol::common::{py_max, py_min, py_sum};

pub const MAX_TLOG_BYTES: usize = 512 * 1024 * 1024;
const MAGIC: &[u8] = b"#CORVUS-TLOG ";
const CHUNK_BYTES: usize = 1 << 20;
const ARMED_FLAG: i128 = 128;
const REBOOT_GAP_S: f64 = 5.0;
const GAP_FLOOR_S: f64 = 2.0;
const GAP_SHARE: f64 = 0.2;
const MAX_GAPS: usize = 200;
const PREARM_BIT: i128 = 1 << 28;
const TARGET_MAX_AGE_S: f64 = 2.0;
const EARTH_RADIUS_M: f64 = 6371000.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlogError(pub String);

const SENSOR_BITS: [(i128, &str); 27] = [
    (1 << 0, "gyroscope"),
    (1 << 1, "accelerometer"),
    (1 << 2, "magnetometer"),
    (1 << 3, "barometer"),
    (1 << 4, "airspeed sensor"),
    (1 << 5, "GPS"),
    (1 << 6, "optical flow"),
    (1 << 7, "vision positioning"),
    (1 << 8, "rangefinder"),
    (1 << 10, "rate control"),
    (1 << 11, "attitude stabilisation"),
    (1 << 12, "yaw control"),
    (1 << 13, "altitude control"),
    (1 << 14, "position control"),
    (1 << 15, "motor outputs"),
    (1 << 16, "RC receiver"),
    (1 << 17, "second gyroscope"),
    (1 << 18, "second accelerometer"),
    (1 << 19, "second magnetometer"),
    (1 << 20, "geofence"),
    (1 << 21, "AHRS"),
    (1 << 22, "terrain database"),
    (1 << 24, "logging"),
    (1 << 25, "battery monitor"),
    (1 << 26, "proximity sensor"),
    (1 << 29, "obstacle avoidance"),
    (1 << 30, "propulsion"),
];

const EKF_FLAGS: [(i128, &str); 2] = [(1 << 10, "a GPS glitch"), (1 << 11, "an accelerometer error")];

fn px4_word(name: &str) -> Option<String> {
    Some(
        match name {
            "MANUAL" => "Manual",
            "ALTCTL" => "Altitude",
            "ALTITUDE_CRUISE" => "Altitude cruise",
            "POSCTL" => "Position",
            "POSITION_SLOW" => "Position slow",
            "ORBIT" => "Orbit",
            "STABILIZED" => "Stabilized",
            "ACRO" => "Acro",
            "RATTITUDE" => "Rattitude",
            "OFFBOARD" => "Offboard",
            "TERMINATION" => "Termination",
            "READY" => "Ready",
            "TAKEOFF" => "Takeoff",
            "LOITER" => "Loiter",
            "MISSION" => "Mission",
            "RTL" => "Return",
            "RTGS" => "Return",
            "LAND" => "Land",
            "FOLLOWME" => "Follow target",
            "PRECLAND" => "Precision land",
            "VTOL_TAKEOFF" => "VTOL takeoff",
            "GUIDED_COURSE" => "Guided course",
            other => {
                if let Some(n) = other.strip_prefix("EXTERNAL") {
                    if let Ok(n @ 1..=8) = n.parse::<u32>() {
                        return Some(format!("External {n}"));
                    }
                }
                return None;
            }
        }
        .to_string(),
    )
}

fn mav_type(kind: i128) -> &'static str {
    match kind {
        1 => "Fixed wing",
        2 => "Quadrotor",
        3 => "Coaxial helicopter",
        4 => "Helicopter",
        6 => "Ground control station",
        7 => "Airship",
        10 => "Ground rover",
        11 => "Surface boat",
        12 => "Submarine",
        13 => "Hexarotor",
        14 => "Octorotor",
        15 => "Tricopter",
        19 => "VTOL (tailsitter duo)",
        20 => "VTOL (tailsitter quad)",
        21 => "VTOL (tiltrotor)",
        22..=25 => "VTOL",
        _ => "",
    }
}

fn severity(n: i128) -> &'static str {
    match n {
        0 => "emergency",
        1 => "alert",
        2 => "critical",
        3 => "error",
        4 => "warning",
        5 => "notice",
        6 => "info",
        7 => "debug",
        _ => "info",
    }
}

// ---------------------------------------------------------------------------
// Reading the file
// ---------------------------------------------------------------------------

/// Corvus's metadata line and the offset the frames begin at.
fn parse_header(blob: &[u8]) -> (Map<String, Value>, usize) {
    if !blob.starts_with(MAGIC) {
        return (Map::new(), 0);
    }
    let Some(end) = blob.iter().position(|b| *b == b'\n') else {
        return (Map::new(), blob.len());
    };
    let text = String::from_utf8_lossy(&blob[MAGIC.len()..end]).into_owned();
    let meta = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    (meta, end + 1)
}

struct Msg<'a> {
    m: &'a Message,
    system: u8,
}

fn as_int(v: &Mv) -> Option<i128> {
    match v {
        Mv::Int(i) => Some(*i),
        Mv::Float(f) if f.is_finite() => Some(f.trunc() as i128),
        _ => None,
    }
}

impl Msg<'_> {
    /// `int(getattr(msg, name, default))`.
    fn int(&self, name: &str, default: i128) -> i128 {
        self.m.get(name).and_then(as_int).unwrap_or(default)
    }

    /// `int(getattr(msg, name, default) or other)`.
    fn int_or(&self, name: &str, default: i128, other: i128) -> i128 {
        let v = self.int(name, default);
        if v == 0 {
            other
        } else {
            v
        }
    }

    /// `float(getattr(msg, name, default))`.
    fn float(&self, name: &str, default: f64) -> f64 {
        self.m.get(name).and_then(Mv::as_f64).unwrap_or(default)
    }

    fn has(&self, name: &str) -> bool {
        self.m.get(name).is_some()
    }
}

// ---------------------------------------------------------------------------
// Reducing it
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Series {
    xs: Vec<f64>,
    ys: Vec<f64>,
}

struct Message2 {
    t: f64,
    level: &'static str,
    text: String,
}

#[derive(Default)]
struct Recording {
    series: Vec<(String, Series)>,
    modes: Vec<Mode>,
    armed: Vec<Span>,
    messages: Vec<Message2>,
    fixes: Vec<(f64, f64)>,
    gps_fixes: Vec<(f64, f64)>,
    meta_sw: Option<String>,
    meta_hw: Option<String>,
    meta_frame: Option<String>,
    meta_start_unix: Option<f64>,
    duration: f64,
    frames: i64,
    types: Vec<(String, i64)>,
    gaps: Vec<(f64, f64)>,
    intervals: i64,
    cadence_sum: f64,
    cadence_n: i64,
    unhealthy: Vec<(&'static str, f64)>,
    ekf_events: Vec<(&'static str, Vec<f64>)>,
    reboots: Vec<f64>,
    armed_reboots: Vec<f64>,
    ended_armed: Option<f64>,
}

impl Recording {
    fn slot(&mut self, name: &str) -> &mut Series {
        if let Some(i) = self.series.iter().position(|(n, _)| n == name) {
            return &mut self.series[i].1;
        }
        self.series.push((name.to_string(), Series::default()));
        &mut self.series.last_mut().expect("pushed").1
    }

    fn add(&mut self, name: &str, when: f64, value: f64) {
        if !value.is_finite() {
            return;
        }
        let s = self.slot(name);
        s.xs.push(when);
        s.ys.push(value);
    }

    fn get(&self, name: &str) -> Option<&Series> {
        self.series
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s)
            .filter(|s| !s.ys.is_empty())
    }

    fn xs(&self, name: &str) -> Vec<f64> {
        self.get(name).map(|s| s.xs.clone()).unwrap_or_default()
    }

    fn ys(&self, name: &str) -> Vec<f64> {
        self.get(name).map(|s| s.ys.clone()).unwrap_or_default()
    }

    fn series_json(&self, name: &str, label: &str) -> Option<Value> {
        let s = self.get(name)?;
        let (xs, ys) = decimate(&s.xs, &s.ys);
        let label = if label.is_empty() { name } else { label };
        Some(series_value(label, &xs, &ys, 5))
    }

    fn count_type(&mut self, kind: &str) {
        if let Some(e) = self.types.iter_mut().find(|(n, _)| n == kind) {
            e.1 += 1;
        } else {
            self.types.push((kind.to_string(), 1));
        }
    }
}

fn clock_of(msg: &Msg) -> Option<f64> {
    let raw = msg.m.get("time_boot_ms")?;
    let value = as_int(raw)?;
    if value > 0 {
        Some(value as f64 / 1000.0)
    } else {
        None
    }
}

fn euler_from(q: Option<&Mv>) -> Option<(f64, f64, f64)> {
    let Some(Mv::Array(values)) = q else {
        return None;
    };
    if values.len() < 4 {
        return None;
    }
    let (w, x, y, z) = (
        values[0].as_f64()?,
        values[1].as_f64()?,
        values[2].as_f64()?,
        values[3].as_f64()?,
    );
    Some((
        (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y)).to_degrees(),
        py_max(-1.0, py_min(1.0, 2.0 * (w * y - z * x))).asin().to_degrees(),
        (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees(),
    ))
}

fn statustext(msg: &Msg, when: f64) -> Message2 {
    // corvus_link decodes a char array as ASCII, one U+FFFD for each other byte.
    let text = match msg.m.get("text") {
        Some(Mv::Bytes(b)) => {
            let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
            let t: String = b[..end]
                .iter()
                .map(|c| if *c < 128 { *c as char } else { '\u{FFFD}' })
                .collect();
            t.trim_end_matches('\0').trim().to_string()
        }
        _ => String::new(),
    };
    Message2 {
        t: round_to(when, 2),
        level: severity(msg.int_or("severity", 6, 6)),
        text,
    }
}

fn mode_name(msg: &Msg) -> String {
    let custom = msg.int_or("custom_mode", 0, 0);
    let autopilot = msg.int_or("autopilot", 0, 0);
    let dialect = dialect_for_id(autopilot as i64);
    let name = dialect.decode_mode(custom as i64, msg.int("base_mode", 0) as i64, msg.int("type", 0) as i64);
    if dialect.stack() == Stack::Px4 {
        return px4_word(&name).unwrap_or_else(|| format!("Mode {custom}"));
    }
    if !name.is_empty() && !name.starts_with("MODE_") {
        return capitalize(&name.replace('_', " "));
    }
    format!("Mode {custom}")
}

fn version_text(raw: i128) -> String {
    if raw == 0 {
        return String::new();
    }
    format!("{}.{}.{}", (raw >> 24) & 0xFF, (raw >> 16) & 0xFF, (raw >> 8) & 0xFF)
}

fn note_interval(rec: &mut Recording, when: f64, length: f64) {
    rec.intervals += 1;
    if length < 0.5 {
        rec.cadence_sum += length;
        rec.cadence_n += 1;
    } else if length >= GAP_FLOOR_S && rec.gaps.len() < MAX_GAPS {
        rec.gaps.push((when, length));
    }
}

fn dominant_system(blob: &[u8], start: usize) -> (Option<u8>, i64) {
    let mut beats: Vec<(u8, i64)> = Vec::new();
    let mut ids: Vec<(u8, i64)> = Vec::new();
    let mut frames = 0;
    let bump = |v: &mut Vec<(u8, i64)>, k: u8| {
        if let Some(e) = v.iter_mut().find(|(n, _)| *n == k) {
            e.1 += 1;
        } else {
            v.push((k, 1));
        }
    };
    let mut parser = Parser::new();
    let mut offset = start;
    while offset < blob.len() {
        let end = (offset + CHUNK_BYTES).min(blob.len());
        for event in parser.feed(&blob[offset..end]) {
            let Event::Frame(frame) = event else {
                continue;
            };
            let Ok(message) = frame.message() else {
                continue;
            };
            frames += 1;
            bump(&mut ids, frame.sysid);
            if message.name() != "HEARTBEAT" {
                continue;
            }
            let kind = message.get("type").and_then(as_int).unwrap_or(0);
            if kind == 6 {
                continue;
            }
            bump(&mut beats, frame.sysid);
        }
        offset = end;
    }
    let first_max = |v: &Vec<(u8, i64)>| {
        let mut best: Option<(u8, i64)> = None;
        for &(k, n) in v {
            if best.is_none_or(|b| n > b.1) {
                best = Some((k, n));
            }
        }
        best.map(|b| b.0)
    };
    if !beats.is_empty() {
        return (first_max(&beats), frames);
    }
    (first_max(&ids), frames)
}

fn walk(blob: &[u8], start: usize, system: Option<u8>) -> Recording {
    let mut rec = Recording::default();
    let mut epoch: Option<f64> = None;
    let mut clock: Option<f64> = None;
    let mut last_boot: Option<f64> = None;
    let mut mode_name_now: Option<String> = None;
    let mut mode_start = 0.0f64;
    let mut armed_since: Option<f64> = None;
    let mut unix_at: Option<(f64, f64)> = None;
    let mut ekf_flags: i128 = 0;

    let mut parser = Parser::new();
    let mut offset = start;
    while offset < blob.len() {
        let end = (offset + CHUNK_BYTES).min(blob.len());
        for event in parser.feed(&blob[offset..end]) {
            let Event::Frame(frame) = event else {
                continue;
            };
            let Ok(message) = frame.message() else {
                continue;
            };
            let msg = Msg {
                m: &message,
                system: frame.sysid,
            };
            if system.is_some_and(|s| msg.system != s) {
                continue;
            }
            let kind = message.name();
            rec.frames += 1;
            rec.count_type(kind);

            if let Some(boot) = clock_of(&msg) {
                if epoch.is_none() {
                    epoch = Some(boot);
                } else if last_boot.is_some_and(|l| boot < l - REBOOT_GAP_S) {
                    epoch = Some(boot - clock.filter(|c| *c != 0.0).unwrap_or(0.0));
                    let at = clock.filter(|c| *c != 0.0).unwrap_or(0.0);
                    rec.reboots.push(at);
                    if armed_since.is_some() {
                        rec.armed_reboots.push(at);
                    }
                }
                last_boot = Some(boot);
                let previous = clock;
                let now = py_max(0.0, boot - epoch.unwrap_or(boot));
                clock = Some(now);
                if let Some(p) = previous {
                    if now > p {
                        note_interval(&mut rec, p, now - p);
                    }
                }
                rec.duration = py_max(rec.duration, now);
            }
            let Some(now) = clock else {
                if kind == "STATUSTEXT" {
                    let m = statustext(&msg, 0.0);
                    rec.messages.push(m);
                }
                continue;
            };

            match kind {
                "GLOBAL_POSITION_INT" => {
                    let lat = msg.int("lat", 0) as f64 / 1e7;
                    let lon = msg.int("lon", 0) as f64 / 1e7;
                    if lat != 0.0 || lon != 0.0 {
                        rec.fixes.push((lat, lon));
                    }
                    rec.add("alt_amsl", now, msg.int("alt", 0) as f64 / 1000.0);
                    rec.add("alt_rel", now, msg.int("relative_alt", 0) as f64 / 1000.0);
                    let vx = msg.int("vx", 0) as f64 / 100.0;
                    let vy = msg.int("vy", 0) as f64 / 100.0;
                    rec.add("vel_n", now, vx);
                    rec.add("vel_e", now, vy);
                    rec.add("vel_d", now, msg.int("vz", 0) as f64 / 100.0);
                    rec.add("groundspeed", now, vx.hypot(vy));
                }
                "ATTITUDE" => {
                    rec.add("roll", now, msg.float("roll", f64::NAN).to_degrees());
                    rec.add("pitch", now, msg.float("pitch", f64::NAN).to_degrees());
                    rec.add("yaw", now, msg.float("yaw", f64::NAN).to_degrees());
                    rec.add("rate_roll", now, msg.float("rollspeed", f64::NAN).to_degrees());
                    rec.add("rate_pitch", now, msg.float("pitchspeed", f64::NAN).to_degrees());
                    rec.add("rate_yaw", now, msg.float("yawspeed", f64::NAN).to_degrees());
                }
                "VFR_HUD" => {
                    rec.add("hud_airspeed", now, msg.float("airspeed", f64::NAN));
                    rec.add("hud_groundspeed", now, msg.float("groundspeed", f64::NAN));
                    rec.add("climb", now, msg.float("climb", f64::NAN));
                    rec.add("throttle", now, msg.float("throttle", f64::NAN));
                }
                "GPS_RAW_INT" => {
                    rec.add("gps_sats", now, msg.float("satellites_visible", 0.0));
                    rec.add("gps_fix", now, msg.float("fix_type", 0.0));
                    let eph = msg.int_or("eph", 65535, 65535);
                    let epv = msg.int_or("epv", 65535, 65535);
                    if eph != 65535 {
                        rec.add("gps_eph", now, eph as f64 / 100.0);
                    }
                    if epv != 65535 {
                        rec.add("gps_epv", now, epv as f64 / 100.0);
                    }
                    let lat = msg.int("lat", 0) as f64 / 1e7;
                    let lon = msg.int("lon", 0) as f64 / 1e7;
                    if lat != 0.0 || lon != 0.0 {
                        rec.gps_fixes.push((lat, lon));
                    }
                }
                "SYS_STATUS" => {
                    let voltage = msg.int_or("voltage_battery", 0, 0);
                    if 0 < voltage && voltage < 65535 {
                        rec.add("batt_v", now, voltage as f64 / 1000.0);
                    }
                    let current = msg.int("current_battery", -1);
                    if current >= 0 {
                        rec.add("batt_a", now, current as f64 / 100.0);
                    }
                    let remaining = msg.int("battery_remaining", -1);
                    if remaining >= 0 {
                        rec.add("batt_pct", now, remaining as f64);
                    }
                    rec.add("cpu", now, msg.int_or("load", 0, 0) as f64 / 10.0);
                    rec.add("drop_rate", now, msg.int_or("drop_rate_comm", 0, 0) as f64 / 100.0);
                    rec.add("comm_errors", now, msg.float("errors_comm", 0.0));
                    let enabled = msg.int_or("onboard_control_sensors_enabled", 0, 0) & 0xFFFF_FFFF;
                    let health = msg.int_or("onboard_control_sensors_health", 0, 0) & 0xFFFF_FFFF;
                    let broken = enabled & !health & !PREARM_BIT & 0xFFFF_FFFF;
                    rec.add("unhealthy", now, broken.count_ones() as f64);
                    rec.add("broken", now, broken as f64);
                    for (bit, label) in SENSOR_BITS {
                        if broken & bit != 0 && !rec.unhealthy.iter().any(|(l, _)| *l == label) {
                            rec.unhealthy.push((label, now));
                        }
                    }
                }
                "BATTERY_STATUS" => {
                    let cells: Vec<i128> = match msg.m.get("voltages") {
                        Some(Mv::Array(vs)) => vs.iter().filter_map(as_int).filter(|v| 0 < *v && *v < 65535).collect(),
                        _ => Vec::new(),
                    };
                    if !cells.is_empty() {
                        rec.add("batt_pack_v", now, cells.iter().sum::<i128>() as f64 / 1000.0);
                    }
                    let consumed = msg.int("current_consumed", -1);
                    if consumed >= 0 {
                        rec.add("batt_mah", now, consumed as f64);
                    }
                }
                "VIBRATION" => {
                    rec.add("vibe_x", now, msg.float("vibration_x", f64::NAN));
                    rec.add("vibe_y", now, msg.float("vibration_y", f64::NAN));
                    rec.add("vibe_z", now, msg.float("vibration_z", f64::NAN));
                    rec.add("clip_0", now, msg.float("clipping_0", f64::NAN));
                    rec.add("clip_1", now, msg.float("clipping_1", f64::NAN));
                    rec.add("clip_2", now, msg.float("clipping_2", f64::NAN));
                }
                "RC_CHANNELS" => {
                    let rssi = msg.int_or("rssi", 255, 255);
                    if rssi != 255 {
                        rec.add("rc_rssi", now, rssi as f64);
                    }
                    for channel in 1..=4 {
                        let value = msg.int_or(&format!("chan{channel}_raw"), 0, 0);
                        if 0 < value && value < 65535 {
                            rec.add(&format!("rc_{channel}"), now, value as f64);
                        }
                    }
                }
                "RADIO_STATUS" => {
                    rec.add("radio_rssi", now, msg.float("rssi", 0.0));
                    rec.add("radio_remrssi", now, msg.float("remrssi", 0.0));
                    rec.add("radio_noise", now, msg.float("noise", 0.0));
                    rec.add("radio_rxerrors", now, msg.float("rxerrors", 0.0));
                }
                "ESTIMATOR_STATUS" => {
                    rec.add("ekf_vel", now, msg.float("vel_ratio", 0.0));
                    rec.add("ekf_pos", now, msg.float("pos_horiz_ratio", 0.0));
                    rec.add("ekf_hgt", now, msg.float("hgt_ratio", 0.0));
                    rec.add("ekf_mag", now, msg.float("mag_ratio", 0.0));
                    let flags = msg.int_or("flags", 0, 0);
                    for (bit, label) in EKF_FLAGS {
                        if flags & bit != 0 && ekf_flags & bit == 0 {
                            if let Some(e) = rec.ekf_events.iter_mut().find(|(l, _)| *l == label) {
                                e.1.push(now);
                            } else {
                                rec.ekf_events.push((label, vec![now]));
                            }
                        }
                    }
                    ekf_flags = flags;
                }
                "ATTITUDE_TARGET" => {
                    if let Some(angles) = euler_from(message.get("q")) {
                        rec.add("sp_roll", now, angles.0);
                        rec.add("sp_pitch", now, angles.1);
                        rec.add("sp_yaw", now, angles.2);
                    }
                    if msg.has("thrust") {
                        rec.add("sp_thrust", now, msg.float("thrust", 0.0) * 100.0);
                    }
                }
                "NAV_CONTROLLER_OUTPUT" => {
                    rec.add("nav_alt_error", now, msg.float("alt_error", 0.0));
                    rec.add("nav_xtrack", now, msg.float("xtrack_error", 0.0));
                }
                "SYSTEM_TIME" => {
                    let unix = msg.int_or("time_unix_usec", 0, 0);
                    if unix > 0 && unix_at.is_none() {
                        unix_at = Some((now, unix as f64 / 1e6));
                    }
                }
                "STATUSTEXT" => {
                    let m = statustext(&msg, now);
                    rec.messages.push(m);
                }
                "AUTOPILOT_VERSION" => {
                    if rec.meta_sw.is_none() {
                        rec.meta_sw = Some(version_text(msg.int_or("flight_sw_version", 0, 0)));
                    }
                    let board = msg.int_or("board_version", 0, 0);
                    if board != 0 && rec.meta_hw.is_none() {
                        rec.meta_hw = Some(format!("board {board}"));
                    }
                }
                "HEARTBEAT" => {
                    if rec.meta_frame.is_none() {
                        rec.meta_frame = Some(mav_type(msg.int_or("type", 0, 0)).to_string());
                    }
                    let name = mode_name(&msg);
                    if mode_name_now.as_deref() != Some(name.as_str()) {
                        if let Some(old) = mode_name_now.take() {
                            rec.modes.push(Mode {
                                mode: old,
                                state: 0,
                                start: round_to(mode_start, 2),
                                end: round_to(now, 2),
                            });
                        }
                        mode_name_now = Some(name);
                        mode_start = now;
                    }
                    let is_armed = msg.int_or("base_mode", 0, 0) & ARMED_FLAG != 0;
                    if is_armed && armed_since.is_none() {
                        armed_since = Some(now);
                    } else if !is_armed {
                        if let Some(since) = armed_since.take() {
                            rec.armed.push(Span {
                                start: round_to(since, 2),
                                end: round_to(now, 2),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        offset = end;
    }

    if let Some(name) = mode_name_now {
        if rec.duration > mode_start {
            rec.modes.push(Mode {
                mode: name,
                state: 0,
                start: round_to(mode_start, 2),
                end: round_to(rec.duration, 2),
            });
        }
    }
    if let Some(since) = armed_since {
        rec.ended_armed = Some(since);
        if rec.duration > since {
            rec.armed.push(Span {
                start: round_to(since, 2),
                end: round_to(rec.duration, 2),
            });
        }
    }
    rec.modes.retain(|m| m.end - m.start >= MIN_MODE_S);
    rec.armed.retain(|a| a.end - a.start >= MIN_MODE_S);
    if let Some((at, unix)) = unix_at {
        rec.meta_start_unix = Some(unix - at);
    }
    rec
}

fn silences(rec: &Recording) -> Vec<(f64, f64)> {
    if rec.gaps.is_empty() {
        return Vec::new();
    }
    let cadence = if rec.cadence_n != 0 {
        rec.cadence_sum / rec.cadence_n as f64
    } else {
        0.1
    };
    let threshold = py_max(GAP_FLOOR_S, cadence * 20.0);
    let found: Vec<(f64, f64)> = rec.gaps.iter().copied().filter(|g| g.1 >= threshold).collect();
    if rec.intervals != 0 && found.len() as f64 > rec.intervals as f64 * GAP_SHARE {
        return Vec::new();
    }
    found
}

// ---------------------------------------------------------------------------
// Plots
// ---------------------------------------------------------------------------

fn project_points(points: &[(f64, f64)], name: &str, sin_lat0: f64, cos_lat0: f64, lon0: f64) -> Option<Value> {
    if points.is_empty() {
        return None;
    }
    let (mut east, mut north): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for &(lat_deg, lon_deg) in points {
        if !((-90.0..=90.0).contains(&lat_deg) && (-180.0..=180.0).contains(&lon_deg)) {
            continue;
        }
        let lat = lat_deg.to_radians();
        let d_lon = lon_deg.to_radians() - lon0;
        let sin_lat = lat.sin();
        let cos_lat = lat.cos();
        let arg = py_max(-1.0, py_min(1.0, sin_lat0 * sin_lat + cos_lat0 * cos_lat * d_lon.cos()));
        let angle = arg.acos();
        let scale = if angle.abs() > 1e-12 { angle / angle.sin() } else { 1.0 };
        north.push(scale * (cos_lat0 * sin_lat - sin_lat0 * cos_lat * d_lon.cos()) * EARTH_RADIUS_M);
        east.push(scale * cos_lat * d_lon.sin() * EARTH_RADIUS_M);
    }
    if east.is_empty() {
        return None;
    }
    if east.len() > super::flight_review::MAX_POINTS {
        let step = east.len() as f64 / super::flight_review::MAX_POINTS as f64;
        let picks: Vec<usize> = (0..super::flight_review::MAX_POINTS)
            .map(|i| (east.len() - 1).min((i as f64 * step) as usize))
            .collect();
        east = picks.iter().map(|i| east[*i]).collect();
        north = picks.iter().map(|i| north[*i]).collect();
    }
    Some(json!({
        "name": name,
        "x": east.iter().map(|v| num(round_to(*v, 2))).collect::<Vec<_>>(),
        "y": north.iter().map(|v| num(round_to(*v, 2))).collect::<Vec<_>>(),
    }))
}

fn track(rec: &Recording) -> Option<Value> {
    if rec.fixes.is_empty() && rec.gps_fixes.is_empty() {
        return None;
    }
    let origin = if rec.fixes.is_empty() {
        rec.gps_fixes[0]
    } else {
        rec.fixes[0]
    };
    let sin_lat0 = origin.0.to_radians().sin();
    let cos_lat0 = origin.0.to_radians().cos();
    let lon0 = origin.1.to_radians();
    let series = vec![
        project_points(&rec.fixes, "Estimated position", sin_lat0, cos_lat0, lon0),
        project_points(&rec.gps_fixes, "GPS", sin_lat0, cos_lat0, lon0),
    ];
    plot(
        "track",
        "Ground track",
        "m",
        series,
        "Flight",
        vec![
            ("equal", json!(true)),
            ("xlabel", json!("East (m)")),
            ("ylabel", json!("North (m)")),
            ("origin", json!({"lat": num(round_to(origin.0, 7)), "lon": num(round_to(origin.1, 7))})),
            ("note", json!("Metres east and north of the first fix. Telemetry rate, so the line is the path flown sampled a few times a second, not every metre of it.")),
        ],
    )
}

fn p(
    id: &str,
    title: &str,
    unit: &str,
    series: Vec<Option<Value>>,
    group: &str,
    extra: Vec<(&'static str, Value)>,
) -> Option<Value> {
    plot(id, title, unit, series, group, extra)
}

fn n(text: &str) -> Vec<(&'static str, Value)> {
    vec![("note", json!(text))]
}

fn build_plots(rec: &Recording) -> Vec<Value> {
    let s = |name: &str, label: &str| rec.series_json(name, label);
    let out = vec![
        track(rec),
        p("altitude", "Altitude", "m", vec![s("alt_rel", "Above home"), s("alt_amsl", "AMSL")], "Flight", vec![]),
        p(
            "speed",
            "Speed",
            "m/s",
            vec![s("groundspeed", "Groundspeed"), s("hud_airspeed", "Airspeed")],
            "Flight",
            vec![],
        ),
        p("climb", "Vertical speed", "m/s", vec![s("climb", "Climb rate")], "Flight", vec![]),
        p(
            "velocity",
            "Velocity NED",
            "m/s",
            vec![s("vel_n", "North"), s("vel_e", "East"), s("vel_d", "Down")],
            "Flight",
            vec![],
        ),
        p(
            "attitude",
            "Attitude",
            "deg",
            vec![s("roll", "Roll"), s("pitch", "Pitch"), s("sp_roll", "Roll setpoint"), s("sp_pitch", "Pitch setpoint")],
            "Control",
            n("Where the setpoints are present the link was carrying ATTITUDE_TARGET: the gap between an angle and its setpoint is the aircraft failing to do what it was told, which no single trace can show."),
        ),
        p("heading", "Heading", "deg", vec![s("yaw", "Yaw"), s("sp_yaw", "Yaw setpoint")], "Control", vec![]),
        p(
            "rates",
            "Angular rates",
            "deg/s",
            vec![s("rate_roll", "Roll rate"), s("rate_pitch", "Pitch rate"), s("rate_yaw", "Yaw rate")],
            "Control",
            n("Telemetry rate. An oscillation shows here; the shape of a single cycle needs the ULog."),
        ),
        p(
            "throttle",
            "Throttle",
            "%",
            vec![s("throttle", "Throttle"), s("sp_thrust", "Thrust setpoint")],
            "Control",
            vec![],
        ),
        p(
            "rc",
            "RC input",
            "µs",
            vec![s("rc_1", "Channel 1"), s("rc_2", "Channel 2"), s("rc_3", "Channel 3"), s("rc_4", "Channel 4")],
            "Control",
            vec![],
        ),
        p(
            "nav_error",
            "Navigation error",
            "m",
            vec![s("nav_xtrack", "Cross-track error"), s("nav_alt_error", "Altitude error")],
            "Flight",
            n("Reported by the navigator itself. Cross-track is how far off the line between waypoints the aircraft was; a steady offset in wind is normal, a growing one is not."),
        ),
        p("vibration", "Vibration", "m/s²", vec![s("vibe_x", "X"), s("vibe_y", "Y"), s("vibe_z", "Z")], "Airframe", vec![]),
        p(
            "clipping",
            "Accelerometer clipping",
            "count",
            vec![s("clip_0", "IMU 0"), s("clip_1", "IMU 1"), s("clip_2", "IMU 2")],
            "Airframe",
            n("A running total. Any climb at all means the accelerometer saturated during the flight."),
        ),
        p(
            "ekf",
            "EKF innovation test ratios",
            "ratio",
            vec![s("ekf_vel", "Velocity"), s("ekf_pos", "Position"), s("ekf_hgt", "Height"), s("ekf_mag", "Magnetometer")],
            "Estimator",
            vec![("threshold", json!(1.0))],
        ),
        p(
            "gps_sats",
            "GPS satellites and fix",
            "count",
            vec![s("gps_sats", "Satellites"), s("gps_fix", "Fix type")],
            "Sensors",
            vec![],
        ),
        p(
            "gps_accuracy",
            "GPS accuracy estimate",
            "m",
            vec![s("gps_eph", "Horizontal"), s("gps_epv", "Vertical")],
            "Sensors",
            vec![],
        ),
        p("battery", "Battery", "V", vec![s("batt_v", "Voltage"), s("batt_pack_v", "Pack voltage")], "System", vec![]),
        p("battery_draw", "Battery draw", "A", vec![s("batt_a", "Current")], "System", vec![]),
        p(
            "battery_remaining",
            "Battery remaining",
            "%",
            vec![s("batt_pct", "Remaining"), s("batt_mah", "Consumed (mAh)")],
            "System",
            vec![],
        ),
        p("cpu", "CPU load", "%", vec![s("cpu", "Load")], "System", vec![]),
        p(
            "health",
            "Unhealthy subsystems",
            "count",
            if rec.unhealthy.is_empty() {
                vec![]
            } else {
                vec![s("unhealthy", "Configured but not working")]
            },
            "System",
            n("The autopilot's own count of subsystems it has configured and cannot get a healthy answer from. What stayed down while armed is in the findings above; a count on the ground is usually a vehicle still starting up."),
        ),
        p(
            "radio",
            "Radio signal",
            "dB",
            vec![s("radio_rssi", "Local RSSI"), s("radio_remrssi", "Remote RSSI"), s("radio_noise", "Noise floor")],
            "Link",
            n("Reported by the radio modem, not the autopilot. this is the link between the two ends of the flight."),
        ),
        p("link_loss", "Dropped telemetry", "%", vec![s("drop_rate", "Drop rate")], "Link", vec![]),
        p(
            "link_errors",
            "Link error counters",
            "count",
            vec![s("comm_errors", "Comm errors"), s("radio_rxerrors", "Radio RX errors")],
            "Link",
            vec![],
        ),
        p("rc_rssi", "RC link strength", "", vec![s("rc_rssi", "RC RSSI")], "Link", vec![]),
    ];
    out.into_iter().flatten().collect()
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

fn extreme(rec: &Recording, name: &str, lowest: bool, spans: Option<&[Span]>) -> Option<(f64, f64)> {
    let mut times = rec.xs(name);
    let mut values = rec.ys(name);
    if let Some(spans) = spans {
        (times, values) = within(&times, &values, spans);
    }
    if values.is_empty() {
        return None;
    }
    let mut index = 0;
    for i in 1..values.len() {
        let better = if lowest {
            values[i] < values[index]
        } else {
            values[i] > values[index]
        };
        if better {
            index = i;
        }
    }
    Some((values[index], times[index]))
}

fn tracks_throttle(rec: &Recording, name: &str) -> Option<f64> {
    let throttle_t = rec.xs("throttle");
    let throttle_v = rec.ys("throttle");
    let times = rec.xs(name);
    let values = rec.ys(name);
    if throttle_v.len() < 50 || values.len() < 50 {
        return None;
    }
    let (times, values) = within(&times, &values, &rec.armed);
    if times.len() < 50 {
        return None;
    }
    let held = align(&times, &throttle_t, &throttle_v, None);
    let finite: Vec<f64> = held.iter().copied().filter(|v| !v.is_nan()).collect();
    if finite.len() < 50 {
        return None;
    }
    let (mut hi, mut lo) = (finite[0], finite[0]);
    for &v in &finite[1..] {
        if v > hi {
            hi = v;
        }
        if v < lo {
            lo = v;
        }
    }
    if hi - lo < 20.0 {
        return None;
    }
    correlate(&values, &held)
}

fn attitude_error(rec: &Recording, axis: &str) -> Option<(f64, f64, f64)> {
    let times = rec.xs(axis);
    let values = rec.ys(axis);
    let setpoint_t = rec.xs(&format!("sp_{axis}"));
    let setpoint_v = rec.ys(&format!("sp_{axis}"));
    if values.len() < 50 || setpoint_v.len() < 10 {
        return None;
    }
    let (times, values) = within(&times, &values, &rec.armed);
    if times.len() < 50 {
        return None;
    }
    let held = align(&times, &setpoint_t, &setpoint_v, Some(TARGET_MAX_AGE_S));
    let mut errors = Vec::new();
    let (mut peak, mut peak_at) = (0.0, 0.0);
    for (index, &value) in values.iter().enumerate() {
        let target = held[index];
        if target.is_nan() {
            continue;
        }
        let error = (value - target).abs();
        errors.push(error);
        if error > peak {
            peak = error;
            peak_at = times[index];
        }
    }
    if errors.len() < 50 {
        return None;
    }
    Some((percentile(&errors, 0.95), peak, peak_at))
}

fn unhealthy_in_flight(rec: &Recording, hold_s: f64) -> Vec<(&'static str, f64)> {
    let (times, masks) = within(&rec.xs("broken"), &rec.ys("broken"), &rec.armed);
    let mut out = Vec::new();
    for (bit, label) in SENSOR_BITS {
        let column: Vec<f64> = masks
            .iter()
            .map(|m| if (*m as i128) & bit != 0 { 1.0 } else { 0.0 })
            .collect();
        let (seconds, first, _) = time_above(&times, &column, 0.5, false);
        if let Some(first) = first {
            if seconds >= hold_s {
                out.push((label, first));
            }
        }
    }
    out
}

fn overlaps(start: f64, length: f64, spans: &[Span]) -> bool {
    if spans.is_empty() {
        return true;
    }
    spans.iter().any(|s| start <= s.end && start + length >= s.start)
}

fn thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

struct Add<'m> {
    out: Vec<Value>,
    modes: &'m [Mode],
}

impl Add<'_> {
    fn add(&mut self, level: &str, text: &str, detail: &str, when: Option<f64>) {
        self.out.push(finding(level, text, when, self.modes, detail));
    }
}

fn findings(rec: &Recording, sensitivity: &str) -> Vec<Value> {
    let level_name = normalize_sensitivity_str(sensitivity);
    let limits = limits_for(level_name);
    let mut add = Add {
        out: Vec::new(),
        modes: &rec.modes,
    };

    if rec.series.iter().all(|(_, s)| s.ys.is_empty()) {
        let mut types: Vec<&(String, i64)> = rec.types.iter().collect();
        types.sort_by_key(|t| std::cmp::Reverse(t.1));
        let kinds: Vec<&str> = types.iter().take(4).map(|t| t.0.as_str()).collect();
        let kinds = kinds.join(", ");
        add.add(
            "warning",
            "This recording carries no timeline",
            &format!(
                "Nothing in it is timestamped, so none of it can be plotted. It holds {} frames{}. A session that only ever exchanged heartbeats looks like this.",
                thousands(rec.frames),
                if kinds.is_empty() { String::new() } else { format!(" ({kinds})") }
            ),
            None,
        );
        return add.out;
    }

    if let Some(_armed) = rec.ended_armed {
        add.add(
            "warning",
            "The recording ends with the aircraft still armed",
            "Either the session was closed in flight, or the link was lost and never came back. If the aircraft did not come home, everything that explains why is in the last seconds of these plots.",
            Some(rec.duration),
        );
    }

    let failed = unhealthy_in_flight(rec, limits.unhealthy_s);
    if !failed.is_empty() {
        let labels: Vec<&str> = failed.iter().map(|f| f.0).collect();
        let mut named = labels[..labels.len().min(3)].join(", ");
        if labels.len() > 3 {
            named.push_str(&format!(" and {} more", labels.len() - 3));
        }
        let all = if labels.len() > 3 {
            format!(" All of them: {}.", labels.join(", "))
        } else {
            String::new()
        };
        let when = failed
            .iter()
            .map(|f| f.1)
            .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
        add.add(
            "critical",
            &format!("The autopilot reported {named} as unhealthy"),
            &format!("Configured but not answering while armed: that is the vehicle's own verdict on its hardware, reached from more than this recording holds. It is the first thing to fix, whatever else is on this page.{all}"),
            Some(when),
        );
    }

    let (mut clipped, mut clipped_at) = (0.0, None);
    for name in ["clip_0", "clip_1", "clip_2"] {
        let (grown, when) = rise(&rec.xs(name), &rec.ys(name), &rec.armed);
        if grown > clipped {
            clipped = grown;
            clipped_at = when;
        }
    }
    if clipped >= limits.clipping_crit {
        add.add(
            "critical",
            &format!("Accelerometer clipping: {} samples", clipped.trunc() as i64),
            "The IMU saturated while armed, so the estimator was working from readings that were a limit rather than a measurement.",
            clipped_at,
        );
    } else if clipped >= limits.clipping_warn {
        add.add(
            "warning",
            &format!("Accelerometer clipping: {} samples", clipped.trunc() as i64),
            "The IMU saturated for a moment while armed. A few at touchdown are a firm landing; more than that in the air is vibration reaching the flight controller. Only armed time is counted, because the counter itself runs from boot.",
            clipped_at,
        );
    }

    let (mut vibe, mut vibe_at) = (0.0, None);
    for name in ["vibe_x", "vibe_y", "vibe_z"] {
        let (t, v) = within(&rec.xs(name), &rec.ys(name), &rec.armed);
        for (moment, value) in t.iter().zip(&v) {
            if *value > vibe {
                vibe = *value;
                vibe_at = Some(*moment);
            }
        }
    }
    if vibe >= limits.vibration_warn {
        let mut cause = String::new();
        let follows = tracks_throttle(rec, "vibe_z");
        match follows {
            Some(c) if c >= 0.5 => {
                cause = format!(" It follows the throttle (correlation {}), which points at the propellers and motors rather than at how the flight controller is mounted.", f(c, 2));
            }
            Some(c) if c < 0.2 => {
                cause = " It does not follow the throttle, so the mounting or a loose airframe is a likelier cause than the rotating parts.".into();
            }
            _ => {}
        }
        if vibe >= limits.vibration_crit {
            add.add(
                "critical",
                &format!("Vibration peaked at {} m/s²", f(vibe, 0)),
                &format!("High enough that position and altitude hold suffer. Check propellers, motor bearings and the controller mounting.{cause}"),
                vibe_at,
            );
        } else {
            add.add(
                "warning",
                &format!("Vibration peaked at {} m/s²", f(vibe, 0)),
                &format!(
                    "Usable, but high enough to act on. Worth watching rather than grounding the aircraft for.{cause}"
                ),
                vibe_at,
            );
        }
    }

    let quiet: Vec<(f64, f64)> = silences(rec)
        .into_iter()
        .filter(|g| overlaps(g.0, g.1, &rec.armed))
        .collect();
    if !quiet.is_empty() {
        let mut longest = quiet[0];
        for g in &quiet[1..] {
            if g.1 > longest.1 {
                longest = *g;
            }
        }
        let total = py_sum(&quiet.iter().map(|g| g.1).collect::<Vec<_>>());
        add.add(
            "warning",
            &format!("The link went quiet {} time(s), the longest {} s", quiet.len(), f(longest.1, 0)),
            &format!("{} s of silence in total. Every plot runs straight across those stretches because nothing arrived, not because nothing happened. This is the one thing a log written on the aircraft cannot tell you.", f(total, 0)),
            Some(longest.0),
        );
    }

    if !rec.armed_reboots.is_empty() {
        add.add(
            "warning",
            &format!("The autopilot rebooted {} time(s) mid-recording", rec.reboots.len()),
            "At least once while armed. The timeline is stitched back together so the plots keep running forwards, but everything the vehicle held in memory (estimator state, mission progress, armed state) started again there.",
            Some(rec.armed_reboots[0]),
        );
    } else if !rec.reboots.is_empty() {
        add.add(
            "note",
            &format!("The autopilot rebooted {} time(s) mid-recording", rec.reboots.len()),
            "Never while armed, so this is most likely a reboot between flights after a parameter change or a calibration. The timeline is stitched back together so the plots keep running forwards.",
            Some(rec.reboots[0]),
        );
    }

    let mut events: Vec<&(&str, Vec<f64>)> = rec.ekf_events.iter().collect();
    events.sort_by(|a, b| a.1[0].partial_cmp(&b.1[0]).unwrap_or(std::cmp::Ordering::Equal));
    for (label, moments) in events {
        let flying: Vec<f64> = moments
            .iter()
            .copied()
            .filter(|m| during(Some(*m), &rec.armed, 0.0))
            .collect();
        if let Some(first) = flying.first() {
            add.add(
                "warning",
                &format!("The estimator flagged {label}"),
                "Position and velocity either side of that point come from different measurements, so a step in the track there is the estimate moving rather than the aircraft.",
                Some(*first),
            );
        }
    }

    for axis in ["roll", "pitch"] {
        let Some((typical, peak, when)) = attitude_error(rec, axis) else {
            continue;
        };
        let cap = capitalize(axis);
        if typical >= limits.attitude_crit_deg {
            add.add(
                "critical",
                &format!("{cap} missed its setpoint by {}° or more for most of the armed time", f(typical, 0)),
                &format!("Peak {}°. An aircraft that cannot hold the angle it is given is short of control authority, not short of tuning.", f(peak, 0)),
                Some(when),
            );
        } else if typical >= limits.attitude_warn_deg {
            add.add(
                "warning",
                &format!("{cap} tracked its setpoint to about {}°", f(typical, 0)),
                &format!(
                    "Peak {}°. Worth a look at the attitude plot before the next flight.",
                    f(peak, 0)
                ),
                Some(when),
            );
        }
    }

    if let Some(drop) = extreme(rec, "drop_rate", false, Some(&rec.armed)) {
        if drop.0 >= 1.0 {
            add.add(
                if drop.0 >= limits.link_drop_pct {
                    "warning"
                } else {
                    "note"
                },
                &format!("The vehicle reported dropping up to {}% of telemetry", f(drop.0, 1)),
                "Gaps in these plots across that stretch are the link, not the flight.",
                Some(drop.1),
            );
        }
    }

    if let Some(rssi) = extreme(rec, "radio_remrssi", true, Some(&rec.armed)) {
        if rssi.0 < limits.remote_rssi {
            add.add(
                "warning",
                &format!("Remote radio RSSI fell to {}", f(rssi.0, 0)),
                "The lower this goes, the closer the modem is to losing the link. Read it against the ground track. It is usually distance or an antenna pointing the wrong way.",
                Some(rssi.1),
            );
        }
    }

    let fix_t = rec.xs("gps_fix");
    let fix_v = rec.ys("gps_fix");
    let fixed_at = fix_t.iter().zip(&fix_v).find(|(_, v)| **v >= 3.0).map(|(t, _)| *t);
    if let Some(fixed_at) = fixed_at.filter(|_| !rec.armed.is_empty()) {
        let (wt, wv) = within(&fix_t, &fix_v, &rec.armed);
        let air: Vec<(f64, f64)> = wt.into_iter().zip(wv).filter(|(t, _)| *t >= fixed_at).collect();
        let (lost, lost_at, _) = time_above(
            &air.iter().map(|a| a.0).collect::<Vec<_>>(),
            &air.iter().map(|a| a.1).collect::<Vec<_>>(),
            3.0,
            true,
        );
        if let Some(lost_at) = lost_at {
            if lost >= limits.gps_lost_s {
                let span = if lost >= 1.0 {
                    format!(" for {} s", f(lost, 0))
                } else {
                    String::new()
                };
                add.add(
                    "warning",
                    &format!("GPS dropped below a 3-D fix{span} while armed"),
                    "Any position held on the plots across that stretch was dead reckoning.",
                    Some(lost_at),
                );
            }
        }
    }

    battery_findings(rec, &mut add, &limits);

    let errors: Vec<&Message2> = rec
        .messages
        .iter()
        .filter(|m| matches!(m.level, "emergency" | "alert" | "critical" | "error"))
        .collect();
    let (flying_errors, grounded): (Vec<&Message2>, Vec<&Message2>) = errors
        .into_iter()
        .partition(|m| during(Some(m.t), &rec.armed, MESSAGE_GRACE_S));
    if let Some(first) = flying_errors.first() {
        add.add(
            "warning",
            &format!("The aircraft printed {} error-level message(s)", flying_errors.len()),
            &format!("The first was “{}”. The rest are at the bottom of this page, and they are usually the shortest route to why a flight went the way it did.", first.text),
            Some(first.t),
        );
    }
    if let Some(first) = grounded.first() {
        add.add(
            "note",
            &format!("{} error-level message(s) while disarmed", grounded.len()),
            &format!("The first was “{}”. Printed on the ground, these are usually the pre-arm checks refusing to arm, which is the system doing its job. They are listed at the bottom of this page.", first.text),
            Some(first.t),
        );
    }

    if let Some(recovery) = rec.modes.iter().find(|m| RECOVERY_MODES.contains(&m.mode.as_str())) {
        add.add(
            "note",
            &format!("The aircraft flew {}", recovery.mode),
            "PX4 also selects that for itself on a failsafe, so if the pilot did not ask for it, the reason is in the messages below.",
            Some(recovery.start),
        );
    }

    let mut out = add.out;
    if !out
        .iter()
        .any(|v| matches!(v["level"].as_str(), Some("critical" | "warning")))
    {
        out.insert(
            0,
            finding(
                "ok",
                "Nothing in this recording stands out",
                None,
                &rec.modes,
                &format!("Checked at the {level_name} sensitivity set in Settings. It is telemetry, though, and a clean tlog is not a clean flight: motor outputs, per-IMU data and estimator innovations never leave the aircraft. For those, read the ULog."),
            ),
        );
    }
    out
}

fn battery_findings(rec: &Recording, add: &mut Add<'_>, limits: &Limits) {
    let kept: Vec<(f64, f64)> = rec
        .xs("batt_v")
        .into_iter()
        .zip(rec.ys("batt_v"))
        .filter(|(_, v)| *v > 1.0)
        .collect();
    if kept.is_empty() {
        return;
    }
    let top = kept
        .iter()
        .map(|k| k.1)
        .fold(kept[0].1, |a, b| if b > a { b } else { a });
    let cells = py_max(1.0, (top / 4.25).ceil());
    let (air_t, air_v) = within(
        &kept.iter().map(|k| k.0).collect::<Vec<_>>(),
        &kept.iter().map(|k| k.1).collect::<Vec<_>>(),
        &rec.armed,
    );
    let per: Vec<f64> = air_v.iter().map(|v| v / cells).collect();
    let Some((per_cell, when)) = held_floor(&air_t, &per, limits.cell_hold_s) else {
        return;
    };
    let volts = per_cell * cells;
    if per_cell < limits.cell_crit_v {
        add.add(
            "critical",
            &format!("Battery reached {} V ({} V per cell across {})", f(volts, 1), f(per_cell, 2), cells as i64),
            &format!("It stayed at or under that for {} s or more while armed, which is into the range that permanently costs a lithium pack capacity. The next flight should be shorter.", f(limits.cell_hold_s, 0)),
            Some(when),
        );
    } else if per_cell < limits.cell_warn_v {
        add.add(
            "warning",
            &format!("Battery reached {} V ({} V per cell)", f(volts, 1), f(per_cell, 2)),
            "Held there under load rather than touched in one punch. The number that decides the next flight is where the pack settles after landing.",
            Some(when),
        );
    }
}

// ---------------------------------------------------------------------------
// The review
// ---------------------------------------------------------------------------

fn group_order() -> Vec<&'static str> {
    let mut g: Vec<&'static str> = GROUP_ORDER[..GROUP_ORDER.len() - 1].to_vec();
    g.push("Link");
    g.push(GROUP_ORDER[GROUP_ORDER.len() - 1]);
    g
}

/// Reduces one tlog's bytes to the payload the Analysis page renders.
pub fn review_bytes_uncached(blob: &[u8], name: &str, sensitivity: &str) -> Result<Value, TlogError> {
    let sensitivity = normalize_sensitivity_str(sensitivity);
    if blob.len() > MAX_TLOG_BYTES {
        return Err(TlogError("that recording is too large to review".into()));
    }
    let (meta, start) = parse_header(blob);
    if start >= blob.len() {
        return Err(TlogError("that recording is empty".into()));
    }
    let (system, frames) = dominant_system(blob, start);
    if frames == 0 {
        return Err(TlogError("no MAVLink frames in that recording".into()));
    }
    let rec = walk(blob, start, system);

    let plots = build_plots(&rec);
    let mut duration = rec.duration;
    for span in &rec.modes {
        duration = py_max(duration, span.end);
    }
    let armed_total = py_sum(&rec.armed.iter().map(|a| a.end - a.start).collect::<Vec<_>>());
    let frame = rec.meta_frame.clone().unwrap_or_default();
    let sw = rec.meta_sw.clone().unwrap_or_default();
    let mut flown: Vec<String> = rec.modes.iter().map(|m| m.mode.clone()).collect();
    flown.sort();
    flown.dedup();
    let mut topics: Vec<&str> = rec.types.iter().map(|t| t.0.as_str()).collect();
    topics.sort();
    let meta_get = |k: &str| meta.get(k).cloned().unwrap_or_else(|| json!(""));
    let dash = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
    let summary = json!({
        "name": name,
        "duration_s": num(round_to(duration, 1)),
        "airframe": frame,
        "sw": sw,
        "hw": rec.meta_hw.clone().unwrap_or_default(),
        "sys_name": system.map_or(String::new(), |s| format!("System {s}")),
        "dropouts": 0,
        "dropout_ms": 0,
        "truncated": false,
        "armed_s": num(round_to(armed_total, 1)),
        "modes_flown": flown,
        "frames": rec.frames,
        "start_unix": num(rec.meta_start_unix.unwrap_or(0.0)),
        "recorded_by": meta_get("product"),
        "recorder_version": meta_get("version"),
        "conn": meta_get("conn"),
        "started_at": meta_get("started_at"),
        "topics": topics,
        "sensitivity": sensitivity,
        "facts": [
            ["Duration", if duration != 0.0 { format!("{} s", f(duration, 0)) } else { "—".into() }],
            ["Armed", if rec.armed.is_empty() { "not armed".to_string() } else { format!("{} s", f(armed_total, 0)) }],
            ["Airframe", dash(&frame)],
            ["Firmware", dash(&sw)],
            ["Frames", thousands(rec.frames)],
        ],
    });
    let order = group_order();
    let groups: Vec<&str> = order
        .iter()
        .copied()
        .filter(|g| plots.iter().any(|p| p["group"] == *g))
        .collect();
    let total = rec.messages.len();
    Ok(json!({
        "summary": summary,
        "groups": groups,
        "modes": rec.modes.iter().map(|m| json!({
            "mode": m.mode, "state": 0, "start": num(m.start), "end": num(m.end)})).collect::<Vec<_>>(),
        "armed": rec.armed.iter().map(|s| json!({"start": num(s.start), "end": num(s.end)})).collect::<Vec<_>>(),
        "findings": findings(&rec, sensitivity),
        "plots": plots,
        "messages": rec.messages[total.saturating_sub(300)..].iter()
            .map(|m| json!({"t": num(m.t), "level": m.level, "text": m.text})).collect::<Vec<_>>(),
        "kind": "tlog",
    }))
}

const CACHE_ENTRIES: usize = 2;
type CacheKey = (String, u64, i128, String);
static CACHE: Mutex<Vec<(CacheKey, Arc<Value>)>> = Mutex::new(Vec::new());

pub fn clear_cache() {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Reviews the tlog at `path`, reusing a recent result for the same file.
pub fn review_file(path: &std::path::Path, name: &str, sensitivity: &str) -> Result<Arc<Value>, TlogError> {
    let label = if name.is_empty() {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        name.to_string()
    };
    let sensitivity = normalize_sensitivity_str(sensitivity);
    let meta = std::fs::metadata(path).map_err(|e| {
        let text = super::pytext::os_error_text(&e, None);
        let strerror = text.split_once("] ").map_or(text.as_str(), |t| t.1);
        TlogError(format!("could not read that recording ({strerror})"))
    })?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let key: CacheKey = (
        real.to_string_lossy().into_owned(),
        meta.len(),
        mtime,
        sensitivity.to_string(),
    );
    if meta.len() > MAX_TLOG_BYTES as u64 {
        return Err(TlogError("that recording is too large to review".into()));
    }
    {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = cache.iter().position(|(k, _)| *k == key) {
            let entry = cache.remove(at);
            let hit = entry.1.clone();
            cache.push(entry);
            return Ok(hit);
        }
    }
    let blob = std::fs::read(path).map_err(|e| TlogError(format!("could not read that recording ({e})")))?;
    let data = Arc::new(review_bytes_uncached(&blob, &label, sensitivity)?);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _)| *k != key);
    cache.push((key, data.clone()));
    while cache.len() > CACHE_ENTRIES {
        cache.remove(0);
    }
    Ok(data)
}

/// Reviews a tlog handed over as bytes (no cache: Python keeps none for bytes either).
pub fn review_bytes(blob: &[u8], name: &str, sensitivity: &str) -> Result<Value, TlogError> {
    review_bytes_uncached(blob, name, sensitivity)
}
