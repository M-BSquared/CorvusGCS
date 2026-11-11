//! RTK settings: the defaults, the resolver that bounds every field of a hand
//! edited config file, the redacted copy for HTTP, and the two checks and the
//! progress figure the page shows (`rtk.py`, the settings half).

use serde_json::{json, Map, Value};

use crate::protocol::common::{py_float_of, py_round};

pub const DEFAULT_SURVEY_ACCURACY_M: f64 = 2.0;
pub const DEFAULT_SURVEY_DURATION_S: i64 = 180;
const SURVEY_ACCURACY_MIN_M: f64 = 0.1;
const SURVEY_ACCURACY_MAX_M: f64 = 100.0;
const SURVEY_DURATION_MIN_S: i64 = 10;
const SURVEY_DURATION_MAX_S: i64 = 86400;
pub const DEFAULT_NTRIP_PORT: i64 = 2101;
const SOURCES: [&str; 2] = ["usb", "ntrip"];
const MODES: [&str; 2] = ["survey", "fixed"];

/// The settings a station with no config file runs with.
pub fn defaults() -> Map<String, Value> {
    let Value::Object(m) = json!({
        "enabled": false,
        "source": "usb",
        "device": "",
        "baud": 0,
        "mode": "survey",
        "survey_accuracy": DEFAULT_SURVEY_ACCURACY_M,
        "survey_duration": DEFAULT_SURVEY_DURATION_S,
        "msm7": false,
        "fixed": {"latitude": 0.0, "longitude": 0.0, "altitude": 0.0, "accuracy": 0.1},
        "ntrip": {"host": "", "port": DEFAULT_NTRIP_PORT, "mountpoint": "", "username": "", "password": ""},
    }) else {
        unreachable!()
    };
    m
}

/// `float(raw)` bounded to [lo, hi]; the default for what is not a finite number.
fn number(raw: Option<&Value>, default: f64, lo: f64, hi: f64) -> f64 {
    let Some(value) = raw.and_then(py_float_of).filter(|v| v.is_finite()) else {
        return default;
    };
    value.max(lo).min(hi)
}

/// `int(raw)` bounded to [lo, hi], for an int or a float only.
fn count(raw: Option<&Value>, default: i64, lo: i64, hi: i64) -> i64 {
    match raw {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i.clamp(lo, hi)
            } else if n.is_u64() {
                hi
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if !f.is_finite() {
                    return default;
                }
                let t = f.trunc();
                if t >= hi as f64 {
                    hi
                } else if t <= lo as f64 {
                    lo
                } else {
                    (t as i64).clamp(lo, hi)
                }
            }
        }
        _ => default,
    }
}

/// `raw.strip()[:limit]` for a string, else "".
fn text(raw: Option<&Value>, limit: usize) -> Value {
    match raw {
        Some(Value::String(s)) => Value::from(
            crate::peripherals::util::py_strip(s)
                .chars()
                .take(limit)
                .collect::<String>(),
        ),
        _ => Value::from(""),
    }
}

fn choice(raw: Option<&Value>, allowed: &[&str]) -> Option<Value> {
    match raw {
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Some(Value::from(s.clone())),
        _ => None,
    }
}

/// Stored RTK settings resolved against the defaults, every field bounded.
pub fn settings(raw: &Value) -> Map<String, Value> {
    let mut out = defaults();
    let Some(raw) = raw.as_object() else {
        return out;
    };
    if let Some(Value::Bool(b)) = raw.get("enabled") {
        out.insert("enabled".into(), Value::from(*b));
    }
    if let Some(v) = choice(raw.get("source"), &SOURCES) {
        out.insert("source".into(), v);
    }
    if let Some(v) = choice(raw.get("mode"), &MODES) {
        out.insert("mode".into(), v);
    }
    out.insert("device".into(), text(raw.get("device"), 256));
    out.insert("baud".into(), Value::from(count(raw.get("baud"), 0, 0, 4_000_000)));
    out.insert(
        "survey_accuracy".into(),
        Value::from(py_round(
            number(
                raw.get("survey_accuracy"),
                DEFAULT_SURVEY_ACCURACY_M,
                SURVEY_ACCURACY_MIN_M,
                SURVEY_ACCURACY_MAX_M,
            ),
            3,
        )),
    );
    out.insert(
        "survey_duration".into(),
        Value::from(count(
            raw.get("survey_duration"),
            DEFAULT_SURVEY_DURATION_S,
            SURVEY_DURATION_MIN_S,
            SURVEY_DURATION_MAX_S,
        )),
    );
    if let Some(Value::Bool(b)) = raw.get("msm7") {
        out.insert("msm7".into(), Value::from(*b));
    }
    if let Some(fixed) = raw.get("fixed").and_then(Value::as_object) {
        out.insert(
            "fixed".into(),
            json!({
                "latitude": py_round(number(fixed.get("latitude"), 0.0, -90.0, 90.0), 9),
                "longitude": py_round(number(fixed.get("longitude"), 0.0, -180.0, 180.0), 9),
                "altitude": py_round(number(fixed.get("altitude"), 0.0, -1000.0, 20000.0), 4),
                "accuracy": py_round(number(fixed.get("accuracy"), 0.1, 0.001, 100.0), 4),
            }),
        );
    }
    if let Some(ntrip) = raw.get("ntrip").and_then(Value::as_object) {
        out.insert(
            "ntrip".into(),
            json!({
                "host": text(ntrip.get("host"), 253),
                "port": count(ntrip.get("port"), DEFAULT_NTRIP_PORT, 1, 65535),
                "mountpoint": text(ntrip.get("mountpoint"), 100),
                "username": text(ntrip.get("username"), 128),
                "password": text(ntrip.get("password"), 128),
            }),
        );
    }
    out
}

/// A copy of `settings` safe for an HTTP response: the NTRIP password replaced
/// by `has_password`.
pub fn public_settings(settings: &Map<String, Value>) -> Map<String, Value> {
    let mut out = settings.clone();
    if let Some(Value::Object(ntrip)) = out.get_mut("ntrip") {
        let has = crate::protocol::common::py_truthy(ntrip.get("password").unwrap_or(&Value::Null));
        ntrip.insert("has_password".into(), Value::from(has));
        ntrip.insert("password".into(), Value::from(""));
    }
    out
}

fn float_or_zero(fixed: &Map<String, Value>, key: &str) -> f64 {
    match fixed.get(key) {
        Some(v) if crate::protocol::common::py_truthy(v) => py_float_of(v).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Why a fixed base position cannot be used, or "".
pub fn fixed_position_problem(fixed: &Map<String, Value>) -> &'static str {
    let (lat, lon) = (float_or_zero(fixed, "latitude"), float_or_zero(fixed, "longitude"));
    if lat.abs() < 1e-9 && lon.abs() < 1e-9 {
        return "the fixed base position is still 0, 0. Enter the surveyed mark first";
    }
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return "the fixed base position is outside the range of a coordinate";
    }
    ""
}

fn blank(ntrip: &Map<String, Value>, key: &str) -> bool {
    match ntrip.get(key) {
        Some(Value::String(s)) => crate::peripherals::util::py_strip(s).is_empty(),
        Some(Value::Null) | None => true,
        Some(other) => !crate::protocol::common::py_truthy(other) || other.to_string().trim().is_empty(),
    }
}

/// Why an NTRIP caster cannot be reached, or "".
pub fn ntrip_problem(ntrip: &Map<String, Value>) -> &'static str {
    if blank(ntrip, "host") {
        return "no NTRIP caster address is set";
    }
    if blank(ntrip, "mountpoint") {
        return "no NTRIP mountpoint is set";
    }
    ""
}

/// How far along a survey is, 0 to 100, held at 99 until the receiver says valid.
pub fn survey_progress(survey: Option<&Value>, target_duration: i64, target_accuracy: f64) -> i64 {
    let Some(s) = survey.and_then(Value::as_object).filter(|s| !s.is_empty()) else {
        return 0;
    };
    let truthy = |k: &str| crate::protocol::common::py_truthy(s.get(k).unwrap_or(&Value::Null));
    if truthy("valid") && !truthy("active") {
        return 100;
    }
    let duration = s
        .get("duration")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .trunc()
        .max(0.0);
    let accuracy = s.get("accuracy").and_then(Value::as_f64).unwrap_or(0.0);
    let target = target_duration.max(1) as f64;
    let by_time = 100.0 * duration / target;
    let by_accuracy = if accuracy > 0.0 && target_accuracy > 0.0 {
        100.0 * (target_accuracy / accuracy).min(1.0)
    } else {
        0.0
    };
    let least = by_accuracy.min(by_time);
    (least.trunc() as i64).clamp(0, 99)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_edited_block_is_bounded_field_by_field() {
        let s = settings(&json!({
            "enabled": true, "source": "tcp", "baud": 10_000_000, "survey_accuracy": 0.0,
            "survey_duration": "9", "device": "  /dev/x  ", "fixed": {"latitude": 91, "longitude": "x"},
            "ntrip": {"port": 0, "host": 5}
        }));
        assert_eq!(s["enabled"], true);
        assert_eq!(s["source"], "usb");
        assert_eq!(s["baud"], 4_000_000);
        assert_eq!(s["survey_accuracy"], 0.1);
        assert_eq!(s["survey_duration"], 180);
        assert_eq!(s["device"], "/dev/x");
        assert_eq!(s["fixed"]["latitude"], 90.0);
        assert_eq!(s["fixed"]["longitude"], 0.0);
        assert_eq!(s["ntrip"]["port"], 1);
        assert_eq!(s["ntrip"]["host"], "");
    }

    #[test]
    fn the_password_leaves_as_a_flag() {
        let s = settings(&json!({"ntrip": {"host": "h", "mountpoint": "m", "password": "secret"}}));
        let p = public_settings(&s);
        assert_eq!(p["ntrip"]["password"], "");
        assert_eq!(p["ntrip"]["has_password"], true);
        assert_eq!(public_settings(&defaults())["ntrip"]["has_password"], false);
    }

    #[test]
    fn the_checks_name_their_problem() {
        let Value::Object(f) = json!({"latitude": 0, "longitude": 0}) else {
            unreachable!()
        };
        assert!(fixed_position_problem(&f).starts_with("the fixed base position is still 0, 0"));
        let Value::Object(f) = json!({"latitude": 95.0, "longitude": 3.0}) else {
            unreachable!()
        };
        assert!(fixed_position_problem(&f).contains("outside the range"));
        let Value::Object(n) = json!({"host": " ", "mountpoint": "m"}) else {
            unreachable!()
        };
        assert_eq!(ntrip_problem(&n), "no NTRIP caster address is set");
    }

    #[test]
    fn progress_is_held_at_99_until_valid() {
        assert_eq!(survey_progress(None, 180, 2.0), 0);
        assert_eq!(
            survey_progress(Some(&json!({"valid": true, "active": false})), 180, 2.0),
            100
        );
        assert_eq!(
            survey_progress(Some(&json!({"duration": 90, "accuracy": 1.0})), 180, 2.0),
            50
        );
        assert_eq!(
            survey_progress(Some(&json!({"duration": 900, "accuracy": 0.5})), 180, 2.0),
            99
        );
        assert_eq!(
            survey_progress(Some(&json!({"duration": 90, "accuracy": 4.0})), 180, 2.0),
            50
        );
        assert_eq!(
            survey_progress(Some(&json!({"duration": 900, "accuracy": 4.0})), 180, 2.0),
            50
        );
    }
}
