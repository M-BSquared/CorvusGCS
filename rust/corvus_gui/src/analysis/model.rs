//! What the Analysis page knows without drawing: how sizes, times, rates and
//! field values read, how the two log lists are ordered, what the tiles say,
//! how a finding ranks, and the colour of a flight mode. Every rule is
//! `src/js/analysis.js` with its expected text in a test.

use serde_json::Value;

use crate::data::clock;
use crate::data::units::{js_number, js_round, js_to_fixed, Units};

/// `formatSize`.
pub fn format_size(bytes: f64) -> String {
    let n = if bytes.is_finite() { bytes } else { 0.0 };
    if n >= 1024.0 * 1024.0 {
        format!("{} MB", js_to_fixed(n / (1024.0 * 1024.0), 1))
    } else if n >= 1024.0 {
        format!("{} KB", js_round(n / 1024.0))
    } else {
        format!("{} B", js_number(n))
    }
}

/// `formatStamp`: an epoch as this computer's wall clock, year first.
pub fn format_stamp(secs: f64, with_seconds: bool, offset: i64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "no timestamp".into();
    }
    clock::stamp(secs.floor() as i64, offset, with_seconds)
}

/// `formatRate`.
pub fn format_rate(rate: f64) -> String {
    if rate.is_nan() || rate <= 0.0 {
        "0".into()
    } else if rate >= 10.0 {
        js_number(js_round(rate))
    } else {
        js_to_fixed(rate, 1)
    }
}

/// `formatAge`.
pub fn format_age(age: f64) -> String {
    let n = if age.is_finite() { age } else { 0.0 };
    if n < 10.0 {
        format!("{} s", js_to_fixed(n, 1))
    } else {
        format!("{} s", js_number(js_round(n)))
    }
}

/// `+value.toPrecision(7)`: seven significant digits, trailing zeros gone.
pub fn precision7(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return js_number(v);
    }
    let rounded: f64 = format!("{:.6e}", v).parse().unwrap_or(v);
    js_number(rounded)
}

/// `formatFieldValue` of the Live Messages fields.
pub fn format_field(v: &Value) -> String {
    match v {
        Value::Null => "n/a".into(),
        Value::Array(a) => a.iter().map(format_field).collect::<Vec<_>>().join(", "),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() != 0.0 => precision7(f),
            Some(f) if n.is_f64() => js_number(f),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `clock(seconds)`: m:ss.
pub fn clock_text(seconds: f64) -> String {
    let total = js_round(if seconds.is_finite() { seconds.max(0.0) } else { 0.0 }) as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// The colour of a flight mode, stable across the plots and the strip.
pub fn mode_color(name: &str) -> u32 {
    match name {
        "Manual" => 0xF5A742,
        "Stabilized" | "Rattitude" => 0xF5C842,
        "Acro" => 0xFF8A5B,
        "Altitude" => 0x7DDEFF,
        "Position" => 0x4CC9FF,
        "Position slow" => 0x63B4E8,
        "Offboard" => 0xB98CFF,
        "Mission" => 0x45D483,
        "Loiter" => 0x63E8A0,
        "Return" => 0xFF726E,
        "Takeoff" | "VTOL takeoff" => 0x8CE0B4,
        "Land" | "Precision land" => 0x8CD0E0,
        "Descend" => 0xFFB36E,
        "Termination" => 0xFF514D,
        "Follow target" => 0x9ED483,
        "Orbit" => 0xC9A0FF,
        _ => 0x8A94A6,
    }
}

/// How the two lists can be ordered.
pub const ULOG_SORTS: [(&str, &str); 4] = [
    ("time-desc", "Newest first"),
    ("time-asc", "Oldest first"),
    ("size-desc", "Largest first"),
    ("id-desc", "Log number"),
];
pub const TLOG_SORTS: [(&str, &str); 4] = [
    ("time-desc", "Newest first"),
    ("time-asc", "Oldest first"),
    ("size-desc", "Largest first"),
    ("name-asc", "Name (A to Z)"),
];

fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// `sortRows`: `time` and `tie` make it shared between the two lists. A log the
/// vehicle never stamped lands after everything that has a time, in either
/// direction.
pub fn sort_rows(
    items: &[Value],
    mode: &str,
    time: impl Fn(&Value) -> f64,
    tie: impl Fn(&Value, &Value) -> std::cmp::Ordering,
) -> Vec<Value> {
    use std::cmp::Ordering;
    let mut rows = items.to_vec();
    match mode {
        "size-desc" => rows.sort_by(|a, b| {
            num(b, "size")
                .partial_cmp(&num(a, "size"))
                .unwrap_or(Ordering::Equal)
                .then_with(|| tie(a, b))
        }),
        "id-desc" | "name-asc" => rows.sort_by(|a, b| tie(a, b)),
        _ => {
            let ascending = mode == "time-asc";
            rows.sort_by(|a, b| {
                let (ta, tb) = (time(a), time(b));
                if (ta == 0.0) != (tb == 0.0) {
                    return if ta != 0.0 { Ordering::Less } else { Ordering::Greater };
                }
                if ta != 0.0 && tb != 0.0 && ta != tb {
                    let c = tb.partial_cmp(&ta).unwrap_or(Ordering::Equal);
                    return if ascending { c.reverse() } else { c };
                }
                tie(a, b)
            });
        }
    }
    rows
}

/// The vehicle's logs in the order asked for (log number breaks a tie).
pub fn sorted_logs(logs: &[Value], mode: &str) -> Vec<Value> {
    sort_rows(
        logs,
        mode,
        |l| num(l, "utc"),
        |a, b| {
            num(b, "id")
                .partial_cmp(&num(a, "id"))
                .unwrap_or(std::cmp::Ordering::Equal)
        },
    )
}

/// The recorded tlogs in the order asked for (the name breaks a tie).
pub fn sorted_tlogs(logs: &[Value], mode: &str) -> Vec<Value> {
    sort_rows(
        logs,
        mode,
        |t| {
            let s = num(t, "started");
            if s != 0.0 {
                s
            } else {
                num(t, "mtime")
            }
        },
        |a, b| {
            let name = |v: &Value| v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            name(a).cmp(&name(b))
        },
    )
}

/// What each of the nine live telemetry tiles shows: label, icon, value,
/// unit, detail (`TELEMETRY_TILES`).
pub struct Tile {
    pub label: &'static str,
    pub icon: &'static str,
    pub value: String,
    pub unit: String,
    pub detail: String,
}

/// The nine tiles of a telemetry frame; dashes and no unit while not connected.
pub fn telemetry_tiles(s: &Value, units: &Units) -> Vec<Tile> {
    let connected = s.get("connected").and_then(Value::as_bool).unwrap_or(false);
    let f = |k: &str| s.get(k).and_then(Value::as_f64);
    let tile = |label, icon, unit: String, value: Option<String>, detail: Option<String>| {
        let live = connected && value.is_some();
        Tile {
            label,
            icon,
            value: if live {
                value.unwrap_or_default()
            } else {
                "\u{2014}".into()
            },
            unit: if live { unit } else { String::new() },
            detail: if connected {
                detail.unwrap_or_default()
            } else {
                String::new()
            },
        }
    };
    vec![
        tile(
            "Altitude AMSL",
            "mountain",
            units.length.symbol.into(),
            f("altitude_amsl").map(|v| units.format_length(v, 0, false, true)),
            None,
        ),
        tile(
            "Altitude AGL",
            "arrow-up-from-line",
            units.length.symbol.into(),
            f("altitude_agl").map(|v| units.format_length(v, 0, false, true)),
            None,
        ),
        tile(
            "Groundspeed",
            "gauge",
            units.speed.symbol.into(),
            f("groundspeed").map(|v| units.format_speed(v, 1, false, true)),
            None,
        ),
        tile(
            "Vertical speed",
            "move-vertical",
            units.speed.symbol.into(),
            f("vspeed").map(|v| units.format_speed(v, 1, false, true)),
            None,
        ),
        tile(
            "Heading",
            "compass",
            "\u{b0}".into(),
            f("heading").map(|v| js_number(js_round(v))),
            None,
        ),
        tile(
            "Pitch",
            "plane",
            "\u{b0}".into(),
            f("pitch").map(|v| js_to_fixed(v, 1)),
            None,
        ),
        tile(
            "Roll",
            "rotate-3d",
            "\u{b0}".into(),
            f("roll").map(|v| js_to_fixed(v, 1)),
            None,
        ),
        tile(
            "Battery",
            "battery-medium",
            "V".into(),
            f("battery_voltage").map(|v| js_to_fixed(v, 1)),
            f("battery_percent").map(|p| format!("{}%", js_number(p))),
        ),
        tile(
            "GPS Fix",
            "satellite",
            String::new(),
            f("gps_fix").map(js_number),
            f("gps_satellites").map(|n| format!("{} sats", js_number(n))),
        ),
    ]
}

/// The tile subtitles of the landing view (`paintTiles`).
pub fn ulog_tile_desc(status: &Value) -> String {
    let logs = status.get("logs").and_then(Value::as_array).map_or(0, Vec::len);
    let have = status.get("logs").and_then(Value::as_array).map_or(0, |a| {
        a.iter()
            .filter(|l| l.get("downloaded").and_then(Value::as_bool) == Some(true))
            .count()
    });
    let state = status.get("state").and_then(Value::as_str).unwrap_or("");
    let busy = matches!(state, "listing" | "downloading" | "erasing");
    if status.get("connected").and_then(Value::as_bool) != Some(true) {
        "Connect to the vehicle to read its logs.".into()
    } else if logs > 0 {
        format!(
            "{logs} log(s) on the vehicle{}{}",
            if have > 0 {
                format!(", {have} already in the folder")
            } else {
                String::new()
            },
            if busy { ", download running" } else { "" }
        )
    } else {
        "Not read yet. Open to ask the vehicle.".into()
    }
}

pub fn tlog_tile_desc(status: &Value) -> String {
    let t = status.get("tlogs").and_then(Value::as_array).map_or(0, Vec::len);
    if t > 0 {
        format!("{t} session(s) recorded on this laptop")
    } else {
        "Nothing recorded yet.".into()
    }
}

pub fn review_tile_desc(status: &Value) -> String {
    let n = status.get("saved").and_then(Value::as_array).map_or(0, Vec::len);
    if n > 0 {
        format!("{n} downloaded log(s) ready to review")
    } else {
        "Download a ULog first, then review it here.".into()
    }
}

/// `busy(s)`: a job is running on the vehicle's log protocol.
pub fn busy(status: &Value) -> bool {
    matches!(
        status.get("state").and_then(Value::as_str),
        Some("listing" | "downloading" | "erasing")
    )
}

/// How a finding ranks: critical first, notes last, anything else after.
pub fn finding_rank(level: &str) -> u8 {
    match level {
        "critical" => 0,
        "warning" => 1,
        "ok" => 2,
        "note" => 3,
        _ => 9,
    }
}

/// The message severities in order (`RANK` of `appendMessages`).
pub fn message_rank(level: &str) -> u8 {
    match level {
        "emergency" => 0,
        "alert" => 1,
        "critical" => 2,
        "error" => 3,
        "warning" => 4,
        "notice" => 5,
        "info" => 6,
        "debug" => 7,
        _ => 99,
    }
}

/// The lowest rank a message filter lets through.
pub fn message_limit(filter: &str) -> u8 {
    match filter {
        "error" => 3,
        "warning" => 4,
        _ => 99,
    }
}

/// The five facts a ULog review states when the backend names none.
pub fn default_facts(summary: &Value) -> Vec<(String, String)> {
    let s = |k: &str| summary.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let dash = |t: String| if t.is_empty() { "\u{2014}".to_string() } else { t };
    let dur = summary.get("duration_s").and_then(Value::as_f64).unwrap_or(0.0);
    let dropouts = summary.get("dropouts").and_then(Value::as_f64).unwrap_or(0.0);
    vec![
        (
            "Duration".into(),
            if dur != 0.0 {
                format!("{} s", js_number(dur))
            } else {
                "\u{2014}".into()
            },
        ),
        ("Airframe".into(), dash(s("airframe"))),
        ("Firmware".into(), dash(s("sw").chars().take(10).collect())),
        ("Board".into(), dash(s("hw"))),
        (
            "Dropouts".into(),
            if dropouts != 0.0 {
                format!(
                    "{} ({} ms)",
                    js_number(dropouts),
                    js_number(summary.get("dropout_ms").and_then(Value::as_f64).unwrap_or(0.0))
                )
            } else {
                "none".into()
            },
        ),
    ]
}

/// The facts the review shows: the backend's own when it sent some.
pub fn facts(summary: &Value) -> Vec<(String, String)> {
    let named: Vec<(String, String)> = summary
        .get("facts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    let text = |v: &Value| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    Some((text(p.first()?), text(p.get(1)?)))
                })
                .collect()
        })
        .unwrap_or_default();
    if named.is_empty() {
        default_facts(summary)
    } else {
        named
    }
}

/// "Findings at Normal sensitivity. Change it under Settings, Analysis."
pub fn sensitivity_line(summary: &Value) -> String {
    let level = summary.get("sensitivity").and_then(Value::as_str).unwrap_or("");
    if level.is_empty() {
        return String::new();
    }
    let mut c = level.chars();
    let cap: String = c
        .next()
        .map(|f| f.to_uppercase().collect::<String>())
        .unwrap_or_default()
        + c.as_str();
    format!("Findings at {cap} sensitivity. Change it under Settings, Analysis.")
}

/// Time on each mode, in the order the modes first appear.
pub fn mode_totals(modes: &[Value]) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    for m in modes {
        let name = m.get("mode").and_then(Value::as_str).unwrap_or("").to_string();
        let len = num(m, "end") - num(m, "start");
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some(e) => e.1 += len,
            None => out.push((name, len)),
        }
    }
    out
}

/// "Showing 60 of 75 recordings." when the list is cut.
pub fn tlog_note(n: usize, cap: usize) -> String {
    if n > cap {
        format!("Showing {cap} of {n} recordings.")
    } else {
        String::new()
    }
}

/// The erase dialog's lines (`confirmErase`).
pub fn erase_lines(all: usize, missing: usize) -> Vec<String> {
    vec![
        format!(
            "This erases ALL {all} log(s) from the flight controller. MAVLink has no way to delete a single log."
        ),
        if missing > 0 {
            format!("{missing} of them are NOT in your download folder yet and cannot be recovered afterwards.")
        } else {
            "All of them are already in your download folder.".into()
        },
        "The vehicle is asked for its log list again afterwards, so you will see the result rather than have to trust it.".into(),
    ]
}

/// `1 plot` or `N plots`.
pub fn plot_count(n: usize) -> String {
    format!("{n} {}", if n == 1 { "plot" } else { "plots" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sizes_read_as_the_web_writes_them() {
        assert_eq!(format_size(512.0), "512 B");
        assert_eq!(format_size(1024.0), "1 KB");
        assert_eq!(format_size(1536.0), "2 KB");
        assert_eq!(format_size(1024.0 * 1024.0), "1.0 MB");
        assert_eq!(format_size(5.55 * 1024.0 * 1024.0), "5.5 MB");
        assert_eq!(format_size(f64::NAN), "0 B");
    }

    #[test]
    fn times_are_the_local_clock_or_say_there_is_none() {
        assert_eq!(format_stamp(0.0, false, 0), "no timestamp");
        assert_eq!(format_stamp(-5.0, false, 0), "no timestamp");
        assert_eq!(format_stamp(1_000_000_000.0, false, 3600), "2001-09-09 02:46");
        assert_eq!(format_stamp(1_000_000_000.0, true, 0), "2001-09-09 01:46:40");
        assert_eq!(clock_text(0.0), "0:00");
        assert_eq!(clock_text(7.4), "0:07");
        assert_eq!(clock_text(425.5), "7:06");
        assert_eq!(clock_text(-3.0), "0:00");
    }

    #[test]
    fn rates_and_ages() {
        assert_eq!(format_rate(0.0), "0");
        assert_eq!(format_rate(-1.0), "0");
        assert_eq!(format_rate(4.26), "4.3");
        assert_eq!(format_rate(10.0), "10");
        assert_eq!(format_rate(49.6), "50");
        assert_eq!(format_age(0.04), "0.0 s");
        assert_eq!(format_age(9.96), "10.0 s");
        assert_eq!(format_age(12.4), "12 s");
    }

    #[test]
    fn field_values_keep_seven_digits() {
        assert_eq!(format_field(&json!(null)), "n/a");
        assert_eq!(format_field(&json!(42)), "42");
        assert_eq!(format_field(&json!(1.5)), "1.5");
        assert_eq!(format_field(&json!(0.123456789)), "0.1234568");
        assert_eq!(format_field(&json!(47.3977419)), "47.39774");
        assert_eq!(format_field(&json!([1, 2.5, null])), "1, 2.5, n/a");
        assert_eq!(format_field(&json!("OK")), "OK");
        assert_eq!(format_field(&json!(true)), "true");
    }

    #[test]
    fn the_logs_are_ordered_and_the_unstamped_go_last() {
        let logs = vec![
            json!({"id": 1, "utc": 100, "size": 10}),
            json!({"id": 2, "utc": 0, "size": 90}),
            json!({"id": 3, "utc": 300, "size": 50}),
            json!({"id": 4, "utc": 200, "size": 50}),
        ];
        let ids = |v: Vec<Value>| v.iter().map(|l| l["id"].as_i64().unwrap()).collect::<Vec<_>>();
        assert_eq!(ids(sorted_logs(&logs, "time-desc")), [3, 4, 1, 2]);
        assert_eq!(ids(sorted_logs(&logs, "time-asc")), [1, 4, 3, 2]);
        assert_eq!(ids(sorted_logs(&logs, "size-desc")), [2, 4, 3, 1]);
        assert_eq!(ids(sorted_logs(&logs, "id-desc")), [4, 3, 2, 1]);
        let tlogs = vec![
            json!({"name": "b.tlog", "started": 5, "size": 1}),
            json!({"name": "a.tlog", "mtime": 9, "size": 2}),
            json!({"name": "c.tlog", "size": 3}),
        ];
        let names = |v: Vec<Value>| {
            v.iter()
                .map(|l| l["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(sorted_tlogs(&tlogs, "time-desc")), ["a.tlog", "b.tlog", "c.tlog"]);
        assert_eq!(names(sorted_tlogs(&tlogs, "name-asc")), ["a.tlog", "b.tlog", "c.tlog"]);
        assert_eq!(names(sorted_tlogs(&tlogs, "size-desc")), ["c.tlog", "a.tlog", "b.tlog"]);
    }

    #[test]
    fn the_telemetry_tiles_show_dashes_until_connected() {
        let u = Units::default();
        let off = telemetry_tiles(&json!({"connected": false, "altitude_amsl": 12.0}), &u);
        assert_eq!(off.len(), 9);
        assert!(off
            .iter()
            .all(|t| t.value == "\u{2014}" && t.unit.is_empty() && t.detail.is_empty()));
        let s = json!({"connected": true, "altitude_amsl": 120.4, "altitude_agl": 99.6, "groundspeed": 7.26,
            "vspeed": -1.04, "heading": 359.6, "pitch": -2.44, "roll": 10.0, "battery_voltage": 15.84,
            "battery_percent": 87, "gps_fix": 3, "gps_satellites": 14});
        let on = telemetry_tiles(&s, &u);
        let v: Vec<(&str, &str, &str)> = on
            .iter()
            .map(|t| (t.label, t.value.as_str(), t.unit.as_str()))
            .collect();
        assert_eq!(v[0], ("Altitude AMSL", "120", "m"));
        assert_eq!(v[1].1, "100");
        assert_eq!(v[2], ("Groundspeed", "7.3", "m/s"));
        assert_eq!(v[3].1, "-1.0");
        assert_eq!(v[4], ("Heading", "360", "\u{b0}"));
        assert_eq!(v[5].1, "-2.4");
        assert_eq!(v[6].1, "10.0");
        assert_eq!((on[7].value.as_str(), on[7].detail.as_str()), ("15.8", "87%"));
        assert_eq!((on[8].value.as_str(), on[8].detail.as_str()), ("3", "14 sats"));
        let imperial = Units::from_value(&json!({"length": "ft", "speed": "kn"}));
        assert_eq!(telemetry_tiles(&s, &imperial)[0].value, "395");
        assert_eq!(telemetry_tiles(&s, &imperial)[0].unit, "ft");
    }

    #[test]
    fn the_tile_subtitles_say_what_is_there() {
        assert_eq!(ulog_tile_desc(&json!({})), "Connect to the vehicle to read its logs.");
        assert_eq!(
            ulog_tile_desc(&json!({"connected": true})),
            "Not read yet. Open to ask the vehicle."
        );
        let s = json!({"connected": true, "state": "downloading",
            "logs": [{"downloaded": true}, {"downloaded": false}]});
        assert_eq!(
            ulog_tile_desc(&s),
            "2 log(s) on the vehicle, 1 already in the folder, download running"
        );
        assert_eq!(tlog_tile_desc(&json!({})), "Nothing recorded yet.");
        assert_eq!(
            tlog_tile_desc(&json!({"tlogs": [1, 2]})),
            "2 session(s) recorded on this laptop"
        );
        assert_eq!(
            review_tile_desc(&json!({})),
            "Download a ULog first, then review it here."
        );
        assert_eq!(
            review_tile_desc(&json!({"saved": [1]})),
            "1 downloaded log(s) ready to review"
        );
        assert!(busy(&json!({"state": "erasing"})) && !busy(&json!({"state": "idle"})));
    }

    #[test]
    fn findings_and_messages_rank_as_the_review_does() {
        let mut levels = vec!["note", "ok", "critical", "odd", "warning"];
        levels.sort_by_key(|l| finding_rank(l));
        assert_eq!(levels, ["critical", "warning", "ok", "note", "odd"]);
        assert_eq!(message_limit("error"), 3);
        assert_eq!(message_limit("warning"), 4);
        assert_eq!(message_limit("all"), 99);
        assert!(message_rank("error") <= message_limit("error"));
        assert!(message_rank("warning") > message_limit("error"));
        assert_eq!(message_rank("strange"), 99);
    }

    #[test]
    fn the_summary_names_its_facts() {
        let s = json!({"duration_s": 412, "airframe": "Quad X", "sw": "v1.16.0-abcdef123456", "hw": "PX4_FMU_V6X",
            "dropouts": 2, "dropout_ms": 340, "sensitivity": "strict"});
        let f = facts(&s);
        assert_eq!(f[0], ("Duration".into(), "412 s".into()));
        assert_eq!(f[2].1, "v1.16.0-ab");
        assert_eq!(f[4].1, "2 (340 ms)");
        assert_eq!(facts(&json!({}))[4].1, "none");
        assert_eq!(facts(&json!({}))[0].1, "\u{2014}");
        let own = json!({"facts": [["Frames", 1200], ["Armed", "6 min"]]});
        assert_eq!(
            facts(&own),
            [("Frames".into(), "1200".into()), ("Armed".into(), "6 min".into())]
        );
        assert_eq!(
            sensitivity_line(&s),
            "Findings at Strict sensitivity. Change it under Settings, Analysis."
        );
        assert_eq!(sensitivity_line(&json!({})), "");
    }

    #[test]
    fn the_modes_are_totalled_in_order_of_appearance() {
        let modes = vec![
            json!({"mode": "Manual", "start": 0, "end": 10}),
            json!({"mode": "Mission", "start": 10, "end": 100}),
            json!({"mode": "Manual", "start": 100, "end": 105}),
        ];
        assert_eq!(
            mode_totals(&modes),
            [("Manual".to_string(), 15.0), ("Mission".to_string(), 90.0)]
        );
        assert_eq!(mode_color("Mission"), 0x45D483);
        assert_eq!(mode_color("Unheard of"), 0x8A94A6);
        assert_eq!(plot_count(1), "1 plot");
        assert_eq!(plot_count(3), "3 plots");
        assert_eq!(tlog_note(75, 60), "Showing 60 of 75 recordings.");
        assert_eq!(tlog_note(60, 60), "");
        let lines = erase_lines(5, 2);
        assert_eq!(
            lines[1],
            "2 of them are NOT in your download folder yet and cannot be recovered afterwards."
        );
        assert_eq!(erase_lines(5, 0)[1], "All of them are already in your download folder.");
    }
}
