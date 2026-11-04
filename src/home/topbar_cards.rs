//! The cards behind the GPS, Battery, Altitude, Groundspeed and Vertical
//! speed blocks of the top bar (`gpsDetail` and its siblings in
//! python/src/js/topbar.js).
//!
//! The bar has room for one number per block. A card holds the rest of what
//! an operator asks before trusting that number. Each is a list of rows; a
//! row the vehicle has no answer for is left out rather than printed as a
//! zero, and nothing shows while disconnected.
//!
//! Not yet: the AGL row of the altitude card (it needs the elevation tiles)
//! and the distance sensor row Settings can ask for.

use std::collections::HashMap;

use serde_json::Value;

use crate::data::units::{js_to_fixed, Units};
use crate::home::topbar::{has_fix, num, reception, s, truthy, Tone};
use crate::ui::popover::{Content, Row};

fn row(key: &str, value: impl Into<String>, tone: &'static str) -> Row {
    Row {
        key: key.into(),
        value: value.into(),
        tone,
    }
}

fn tone_word(t: Tone) -> &'static str {
    match t {
        Tone::Healthy => "healthy",
        Tone::Warning | Tone::Armed => "warning",
        Tone::Critical => "critical",
        _ => "",
    }
}

fn fix_label(fix: &str) -> String {
    match fix {
        "NO_GPS" => "No receiver",
        "NO_FIX" => "No fix",
        "2D_FIX" => "2D fix",
        "3D_FIX" => "3D fix",
        "DGPS" => "DGPS",
        "RTK_FLOAT" => "RTK float",
        "RTK_FIXED" => "RTK fixed",
        "STATIC" => "Static",
        "PPP" => "PPP",
        other => other,
    }
    .to_string()
}

/// `dopWord`: HDOP in words.
pub fn dop_word(dop: f64) -> &'static str {
    if dop.is_nan() || dop <= 0.0 || dop >= 99.0 {
        ""
    } else if dop <= 1.0 {
        "excellent"
    } else if dop <= 2.0 {
        "good"
    } else if dop <= 5.0 {
        "moderate"
    } else {
        "poor"
    }
}

fn integrity(v: &str) -> Option<(&'static str, &'static str)> {
    match v {
        "ok" => Some(("None detected", "healthy")),
        "mitigated" => Some(("Detected, mitigated", "warning")),
        "detected" => Some(("Detected", "critical")),
        _ => None,
    }
}

/// `metres`: two decimals under 10 in the length unit, none above.
fn metres(units: &Units, v: f64) -> Option<String> {
    (v >= 0.0).then(|| {
        let shown = v * units.length.per_si;
        units.format_length(v, if shown < 10.0 { 2 } else { 0 }, false, false)
    })
}

/// `flightTime`: rounded to the minute, because seconds would claim a
/// precision no estimate here has.
pub fn flight_time(seconds: f64) -> String {
    let sec = if seconds.is_nan() { 0.0 } else { seconds };
    if sec < 60.0 {
        return "under 1 min".into();
    }
    let total = (sec / 60.0 + 0.5).floor() as i64;
    if total < 60 {
        return format!("about {total} min");
    }
    format!("about {} h {:02} min", total / 60, total % 60)
}

fn card(title: &str, rows: Vec<Row>) -> Content {
    Content {
        title: title.into(),
        rows,
        ..Default::default()
    }
}

/// `gpsDetail`.
pub fn gps(state: &Value, units: &Units) -> Option<Content> {
    if !truthy(state, "connected") {
        return None;
    }
    let mut rows = Vec::new();
    let fix = match s(state, "gps_fix") {
        "" => "NO_GPS",
        f => f,
    };
    rows.push(row(
        "Fix",
        fix_label(fix),
        if has_fix(state) { "healthy" } else { "critical" },
    ));
    let sats = num(state, "gps_satellites");
    rows.push(row(
        "Satellites",
        crate::data::units::js_number(if sats.is_nan() { 0.0 } else { sats }),
        "",
    ));
    let (recv, tone) = reception(state);
    rows.push(row("Reception", recv, tone_word(tone)));
    let signal = num(state, "gps_signal_quality");
    if signal >= 0.0 && state.get("gps_signal_quality").is_some_and(|v| !v.is_null()) {
        rows.push(row(
            "Signal quality",
            format!("{} of 10", crate::data::units::js_number(signal)),
            "",
        ));
    }
    let hdop = num(state, "gps_hdop");
    if hdop > 0.0 && hdop < 99.0 {
        rows.push(row(
            "HDOP",
            format!("{} ({})", js_to_fixed(hdop, 1), dop_word(hdop)),
            "",
        ));
    }
    let vdop = num(state, "gps_vdop");
    if vdop > 0.0 && vdop < 99.0 {
        rows.push(row("VDOP", js_to_fixed(vdop, 1), ""));
    }
    if state.get("gps_h_acc").is_some_and(|v| !v.is_null()) {
        if let Some(v) = metres(units, num(state, "gps_h_acc")) {
            rows.push(row("Horizontal accuracy", v, ""));
        }
    }
    if state.get("gps_v_acc").is_some_and(|v| !v.is_null()) {
        if let Some(v) = metres(units, num(state, "gps_v_acc")) {
            rows.push(row("Vertical accuracy", v, ""));
        }
    }
    if let Some((t, c)) = integrity(s(state, "gps_jamming")) {
        rows.push(row("Jamming", t, c));
    }
    if let Some((t, c)) = integrity(s(state, "gps_spoofing")) {
        rows.push(row("Spoofing", t, c));
    }
    match s(state, "gps_health") {
        "ok" => rows.push(row("Receiver health", "OK", "healthy")),
        "fault" => rows.push(row("Receiver health", "Fault", "critical")),
        _ => {}
    }
    Some(card("GPS", rows))
}

/// `endurance`: time left, only when there is an honest answer: the
/// autopilot's own figure first, the drain estimate second, both only armed.
fn endurance(state: &Value) -> Option<String> {
    if !truthy(state, "armed") {
        return None;
    }
    let fc = num(state, "battery_time_remaining");
    if fc > 0.0 {
        return Some(flight_time(fc));
    }
    let est = num(state, "battery_endurance_est");
    if state.get("battery_endurance_est").is_some_and(|v| !v.is_null()) && est >= 0.0 {
        return Some(flight_time(est));
    }
    None
}

/// `batteryDetail`.
pub fn battery(state: &Value, units: &Units) -> Option<Content> {
    if !truthy(state, "connected") {
        return None;
    }
    let n = |k: &str| {
        let v = num(state, k);
        if v.is_nan() {
            0.0
        } else {
            v
        }
    };
    let (voltage, current, pct) = (n("battery_voltage"), n("battery_current"), n("battery_percent"));
    let tone = if pct > 25.0 {
        "healthy"
    } else if pct > 12.0 {
        "warning"
    } else {
        "critical"
    };
    let estimate = s(state, "battery_source") == "estimate";
    let mut rows = vec![row(
        "Remaining",
        format!("{}%", crate::data::units::js_round(pct)),
        tone,
    )];
    if let Some(t) = endurance(state) {
        rows.push(row("Flight time left", t, tone));
    }
    if voltage > 0.0 {
        rows.push(row("Voltage", format!("{} V", js_to_fixed(voltage, 1)), ""));
    }
    let cells = n("battery_cells");
    let measured: Vec<f64> = state
        .get("battery_cell_voltages")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    if measured.len() > 1 {
        let low = measured.iter().cloned().fold(f64::INFINITY, f64::min);
        let high = measured.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        rows.push(row(
            "Cells",
            format!("{} to {} V", js_to_fixed(low, 2), js_to_fixed(high, 2)),
            "",
        ));
    } else if cells > 0.0 && voltage > 0.0 {
        rows.push(row(
            "Per cell",
            format!(
                "{} V ({}S)",
                js_to_fixed(voltage / cells, 2),
                crate::data::units::js_number(cells)
            ),
            "",
        ));
    }
    rows.push(row("Current", format!("{} A", js_to_fixed(current, 1)), ""));
    if voltage > 0.0 {
        rows.push(row(
            "Power",
            format!("{} W", crate::data::units::js_round(voltage * current)),
            "",
        ));
    }
    let consumed = n("battery_consumed_mah");
    if consumed > 0.0 {
        rows.push(row(
            "Consumed",
            format!("{} mAh", crate::data::units::js_round(consumed)),
            "",
        ));
    }
    if let Some(t) = state.get("battery_temperature").and_then(Value::as_f64) {
        rows.push(row("Temperature", units.format_temperature(t, 1), ""));
    }
    let other_key = if estimate {
        "battery_percent_fc"
    } else {
        "battery_percent_est"
    };
    let other = num(state, other_key);
    let known = state.get(other_key).is_some_and(|v| !v.is_null()) && other >= 0.0;
    rows.push(row(
        if estimate {
            "Autopilot says"
        } else {
            "Cell voltage says"
        },
        if known {
            format!("{}%", crate::data::units::js_round(other))
        } else {
            "no reading".into()
        },
        "muted",
    ));
    Some(card("Battery", rows))
}

/// `altitudeDetail`, without the ground height row.
pub fn altitude(state: &Value, units: &Units) -> Option<Content> {
    if !truthy(state, "connected") {
        return None;
    }
    let mut rows = vec![row(
        "AMSL",
        units.format_length(num(state, "altitude_amsl"), 0, false, false),
        "",
    )];
    rows.push(row(
        "Above home",
        units.format_length(num(state, "altitude_agl"), 0, false, false),
        "",
    ));
    if let Some(g) = state.get("gps_altitude").and_then(Value::as_f64) {
        rows.push(row("GPS", units.format_length(g, 0, false, false), ""));
    }
    Some(card("Altitude", rows))
}

/// The highest groundspeed, climb rate and sink rate of the flight, per
/// vehicle. A flight starts when the vehicle arms and the figure stays after
/// the landing. A fresh link on a disarmed vehicle, or an autopilot reboot,
/// starts over; a link that drops and returns mid-flight does not.
#[derive(Default)]
pub struct TopSpeeds(HashMap<i64, Rec>);

#[derive(Default, Clone, Copy)]
struct Rec {
    max: f64,
    climb: f64,
    sink: f64,
    connected: bool,
    armed: bool,
    boot_ms: f64,
}

impl TopSpeeds {
    /// `trackTopSpeed`.
    pub fn track(&mut self, state: &Value) {
        let key = state.get("vehicle_id").and_then(Value::as_i64).unwrap_or(0);
        let rec = self.0.entry(key).or_default();
        let connected = truthy(state, "connected");
        let armed = truthy(state, "armed");
        let boot = num(state, "boot_ms");
        let boot = if boot.is_nan() { 0.0 } else { boot };
        let rebooted = rec.boot_ms > 0.0 && boot > 0.0 && boot < rec.boot_ms;
        if connected {
            let fresh_link = !rec.connected && !armed;
            let just_armed = rec.connected && !rec.armed && armed;
            if fresh_link || just_armed || rebooted {
                rec.max = 0.0;
                rec.climb = 0.0;
                rec.sink = 0.0;
            }
            let v = num(state, "groundspeed");
            if v.is_finite() && v > rec.max {
                rec.max = v;
            }
            let vs = num(state, "vspeed");
            if vs.is_finite() {
                rec.climb = rec.climb.max(vs);
                rec.sink = rec.sink.max(-vs);
            }
            if boot > 0.0 {
                rec.boot_ms = boot;
            }
        }
        rec.connected = connected;
        rec.armed = armed;
    }

    fn rec(&self, state: &Value) -> Rec {
        let key = state.get("vehicle_id").and_then(Value::as_i64).unwrap_or(0);
        self.0.get(&key).copied().unwrap_or_default()
    }

    /// `groundspeedDetail`.
    pub fn groundspeed(&self, state: &Value, units: &Units) -> Option<Content> {
        if !truthy(state, "connected") {
            return None;
        }
        let r = self.rec(state);
        Some(card(
            "Groundspeed",
            vec![row("Top speed", units.format_speed(r.max, 1, false, false), "")],
        ))
    }

    /// `vspeedDetail`.
    pub fn vspeed(&self, state: &Value, units: &Units) -> Option<Content> {
        if !truthy(state, "connected") {
            return None;
        }
        let r = self.rec(state);
        Some(card(
            "Vertical speed",
            vec![
                row("Top climb rate", units.format_speed(r.climb, 1, false, false), ""),
                row("Top sink rate", units.format_speed(r.sink, 1, false, false), ""),
            ],
        ))
    }
}

/// The cards of the bar's blocks, in block order: `None` for a block with no
/// card.
pub fn all(state: &Value, units: &Units, tops: &mut TopSpeeds) -> Vec<Option<Content>> {
    tops.track(state);
    vec![
        None,
        None,
        None,
        gps(state, units),
        battery(state, units),
        altitude(state, units),
        tops.groundspeed(state, units),
        tops.vspeed(state, units),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows(c: &Content) -> Vec<(String, String, &'static str)> {
        c.rows
            .iter()
            .map(|r| (r.key.clone(), r.value.clone(), r.tone))
            .collect()
    }

    #[test]
    fn the_time_left_is_rounded_to_the_minute() {
        assert_eq!(flight_time(30.0), "under 1 min");
        assert_eq!(flight_time(725.0), "about 12 min");
        assert_eq!(flight_time(3900.0), "about 1 h 05 min");
    }

    #[test]
    fn the_geometry_is_said_in_words() {
        assert_eq!(dop_word(0.7), "excellent");
        assert_eq!(dop_word(1.5), "good");
        assert_eq!(dop_word(4.0), "moderate");
        assert_eq!(dop_word(9.0), "poor");
        assert_eq!(dop_word(99.0), "");
    }

    #[test]
    fn nothing_is_said_without_a_link() {
        let u = Units::default();
        assert!(gps(&json!({"connected": false}), &u).is_none());
        assert!(battery(&json!({}), &u).is_none());
        assert!(altitude(&json!({}), &u).is_none());
    }

    #[test]
    fn the_gps_card_leaves_out_what_the_receiver_does_not_report() {
        let st = json!({"connected": true, "gps_fix": "RTK_FIXED", "gps_satellites": 18, "gps_hdop": 0.7,
            "gps_jamming": "ok"});
        let c = gps(&st, &Units::default()).unwrap();
        assert_eq!(c.title, "GPS");
        assert_eq!(
            rows(&c),
            vec![
                ("Fix".into(), "RTK fixed".into(), "healthy"),
                ("Satellites".into(), "18".into(), ""),
                ("Reception".into(), "Good".into(), "healthy"),
                ("HDOP".into(), "0.7 (excellent)".into(), ""),
                ("Jamming".into(), "None detected".into(), "healthy"),
            ]
        );
        let none = gps(&json!({"connected": true}), &Units::default()).unwrap();
        assert_eq!(none.rows[0].value, "No receiver");
        assert_eq!(none.rows[0].tone, "critical");
    }

    #[test]
    fn the_battery_card_names_where_the_percentage_came_from() {
        let st = json!({"connected": true, "armed": true, "battery_voltage": 24.94, "battery_current": 12.3,
            "battery_percent": 99, "battery_cells": 6, "battery_time_remaining": 725,
            "battery_percent_est": 97, "battery_consumed_mah": 410.4});
        let c = battery(&st, &Units::default()).unwrap();
        assert_eq!(
            rows(&c),
            vec![
                ("Remaining".into(), "99%".into(), "healthy"),
                ("Flight time left".into(), "about 12 min".into(), "healthy"),
                ("Voltage".into(), "24.9 V".into(), ""),
                ("Per cell".into(), "4.16 V (6S)".into(), ""),
                ("Current".into(), "12.3 A".into(), ""),
                ("Power".into(), "307 W".into(), ""),
                ("Consumed".into(), "410 mAh".into(), ""),
                ("Cell voltage says".into(), "97%".into(), "muted"),
            ]
        );
        let est = battery(
            &json!({"connected": true, "battery_source": "estimate", "battery_percent": 8}),
            &Units::default(),
        )
        .unwrap();
        assert_eq!(est.rows[0].tone, "critical");
        assert_eq!(est.rows.last().unwrap().key, "Autopilot says");
        assert_eq!(est.rows.last().unwrap().value, "no reading");
    }

    #[test]
    fn the_top_speeds_belong_to_one_flight() {
        let u = Units::default();
        let mut t = TopSpeeds::default();
        let st =
            |armed: bool, gs: f64, vs: f64| json!({"connected": true, "armed": armed, "groundspeed": gs, "vspeed": vs});
        t.track(&st(false, 0.0, 0.0));
        t.track(&st(true, 3.0, 1.0));
        t.track(&st(true, 8.0, -2.5));
        t.track(&st(true, 5.0, 0.5));
        assert_eq!(t.groundspeed(&st(true, 0.0, 0.0), &u).unwrap().rows[0].value, "8.0 m/s");
        let v = t.vspeed(&st(true, 0.0, 0.0), &u).unwrap();
        assert_eq!(
            (v.rows[0].value.as_str(), v.rows[1].value.as_str()),
            ("1.0 m/s", "2.5 m/s")
        );
        t.track(&st(false, 0.0, 0.0));
        assert_eq!(
            t.groundspeed(&st(false, 0.0, 0.0), &u).unwrap().rows[0].value,
            "8.0 m/s",
            "stays after landing"
        );
        t.track(&st(true, 1.0, 0.0));
        assert_eq!(
            t.groundspeed(&st(true, 0.0, 0.0), &u).unwrap().rows[0].value,
            "1.0 m/s",
            "a new arming starts over"
        );
    }

    #[test]
    fn the_altitude_card_has_the_references() {
        let st = json!({"connected": true, "altitude_amsl": 610.4, "altitude_agl": 12.2, "gps_altitude": 611.0});
        let c = altitude(&st, &Units::default()).unwrap();
        assert_eq!(
            rows(&c),
            vec![
                ("AMSL".into(), "610 m".into(), ""),
                ("Above home".into(), "12 m".into(), ""),
                ("GPS".into(), "611 m".into(), ""),
            ]
        );
    }
}
