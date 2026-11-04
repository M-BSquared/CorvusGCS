//! What the RTK GPS page says (`python/src/js/setup-rtk.js`): the words for a state,
//! the sentence under the survey bar, what the corrections and the aircraft's
//! fix are called, and the form's request body. The page is mostly a status
//! display: three parties are on the route (a base that works out how wrong it
//! is, the station that carries the corrections down the link, the aircraft's
//! receiver that subtracts them), and every failure is somewhere along it, so
//! "RTK" is never a single light.

use serde_json::{json, Value};

use crate::data::units::{js_number, js_round, js_to_fixed};

/// How often the status is re-read while the page is open: a survey moves
/// once a second and nothing else here moves faster.
pub const POLL_MS: f64 = 1000.0;

/// What each state means in one line.
pub fn state_label(state: &str) -> String {
    match state {
        "off" => "Off",
        "searching" => "Looking for a base",
        "connecting" => "Connecting",
        "surveying" => "Surveying",
        "active" => "Correcting",
        "error" => "Problem",
        other => return other.into(),
    }
    .into()
}

/// The badge's tone: `ok`, `err`, `pending` or none.
pub fn state_tone(state: &str) -> &'static str {
    match state {
        "searching" | "connecting" | "surveying" => "pending",
        "active" => "ok",
        "error" => "err",
        _ => "",
    }
}

/// `fmt`: trim float noise without losing a centimetre; a dash for no number.
pub fn fmt(v: Option<&Value>) -> String {
    let n = match v {
        None => return "\u{2014}".into(),
        Some(Value::Null) => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(_) => f64::NAN,
    };
    if n.is_finite() {
        js_number(js_round(n * 100.0) / 100.0)
    } else {
        "\u{2014}".into()
    }
}

/// `formatBytes`.
pub fn format_bytes(v: Option<&Value>) -> String {
    let n = match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    };
    if n < 1024.0 {
        format!("{} B", js_number(n))
    } else if n < 1024.0 * 1024.0 {
        format!("{} kB", js_to_fixed(n / 1024.0, 1))
    } else {
        format!("{} MB", js_to_fixed(n / (1024.0 * 1024.0), 1))
    }
}

/// The autopilot's fix name, in words. RTK fixed and RTK float are the two
/// that matter, and the difference between them is the point of the page:
/// float has the corrections and has not resolved the carrier ambiguity, which
/// is decimetres; fixed is the centimetre answer.
pub fn friendly_fix(fix: &str) -> String {
    match fix {
        "RTK_FIXED" => "RTK fixed (centimetres)",
        "RTK_FLOAT" => "RTK float (decimetres)",
        "3D_FIX" => "3D fix",
        "DGPS" => "DGPS",
        "NO_FIX" => "no fix",
        "NO_GPS" => "no GPS",
        other => return other.into(),
    }
    .into()
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The progress bar's share and the sentence under it: what is still holding
/// the survey up. A survey ends when both its minimum duration has passed and
/// its accuracy limit is met, so an operator watching a bar stuck at 30 percent
/// for four minutes learns that it is the sky holding them up and not the clock.
pub fn survey_sentence(data: &Value, pct: f64) -> String {
    let survey = data.get("survey").cloned().unwrap_or(Value::Null);
    let target = data.get("survey_target").cloned().unwrap_or(Value::Null);
    let truthy = |k: &str| survey.get(k).is_some_and(|v| v == &Value::Bool(true));
    if truthy("valid") && !truthy("active") {
        return format!(
            "Survey complete: the base knows its position to {} m.",
            fmt(survey.get("accuracy"))
        );
    }
    let mut parts = vec![format!("{}%", js_number(pct))];
    parts.push(format!(
        "{} s of {} s",
        js_number(js_round(num(&survey, "duration"))),
        js_number(num(&target, "duration"))
    ));
    if num(&survey, "accuracy") != 0.0 {
        parts.push(format!(
            "accurate to {} m, needs {} m",
            fmt(survey.get("accuracy")),
            fmt(target.get("accuracy"))
        ));
    }
    parts.join(" \u{b7} ")
}

/// What the base has produced.
pub fn describe_corrections(data: &Value) -> String {
    let frames = num(data, "frames");
    if frames == 0.0 {
        return "none yet".into();
    }
    let mut types: Vec<&str> = data
        .get("messages")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    types.sort_unstable();
    let types = types.join(", ");
    let stale = match data.get("source_age").and_then(Value::as_f64) {
        Some(age) if age > 5.0 => format!(" \u{b7} last {} s ago", js_number(js_round(age))),
        _ => String::new(),
    };
    format!(
        "{} messages{}{}",
        js_number(frames),
        if types.is_empty() {
            String::new()
        } else {
            format!(" ({types})")
        },
        stale
    )
}

/// What reached the aircraft.
pub fn describe_injected(data: &Value) -> String {
    let injected = data.get("injected").cloned().unwrap_or(Value::Null);
    if data
        .get("link_ready")
        .is_none_or(|v| v == &Value::Bool(false) || v == &Value::Null)
    {
        return "waiting for the aircraft link".into();
    }
    let messages = num(&injected, "messages");
    if messages == 0.0 {
        return "none yet".into();
    }
    let dropped = num(&injected, "dropped");
    format!(
        "{} messages, {}{}",
        js_number(messages),
        format_bytes(injected.get("bytes")),
        if dropped != 0.0 {
            format!(" \u{b7} {} could not be sent", js_number(dropped))
        } else {
            String::new()
        }
    )
}

/// The receiver row.
pub fn receiver_text(data: &Value) -> String {
    let label = data.get("receiver_label").and_then(Value::as_str).unwrap_or("");
    if !label.is_empty() {
        return label.into();
    }
    let state = data.get("state").and_then(Value::as_str).unwrap_or("");
    let receiver = data.get("receiver").filter(|v| !v.is_null());
    if state == "active" && receiver.is_none() {
        "a base that configured itself".into()
    } else {
        "\u{2014}".into()
    }
}

/// The connection row.
pub fn port_text(data: &Value) -> String {
    let device = data.get("device").and_then(Value::as_str).unwrap_or("");
    if device.is_empty() {
        return "\u{2014}".into();
    }
    let baud = num(data, "baud");
    if baud != 0.0 {
        format!("{device} at {} baud", js_number(baud))
    } else {
        device.into()
    }
}

/// The aircraft's GPS row.
pub fn fix_text(data: &Value) -> String {
    let v = data.get("vehicle").cloned().unwrap_or(Value::Null);
    match v.get("fix").and_then(Value::as_str).filter(|f| !f.is_empty()) {
        Some(fix) => format!(
            "{} \u{b7} {} satellites",
            friendly_fix(fix),
            js_number(num(&v, "satellites"))
        ),
        None => "\u{2014}".into(),
    }
}

/// Whether "Survey again" is there: only a base on a cable that is working.
pub fn can_restart(data: &Value) -> bool {
    let state = data.get("state").and_then(Value::as_str).unwrap_or("");
    data.get("source").and_then(Value::as_str) == Some("usb")
        && data
            .get("receiver")
            .is_some_and(|r| !r.is_null() && r != &Value::Bool(false))
        && matches!(state, "active" | "surveying")
}

/// The form as the page holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub enabled: bool,
    pub source: String,
    pub mode: String,
    pub accuracy: String,
    pub duration: String,
    pub fixed: [String; 4],
    pub ntrip: [String; 5],
    pub has_password: bool,
}

pub const FIXED_KEYS: [&str; 4] = ["latitude", "longitude", "altitude", "accuracy"];
pub const NTRIP_KEYS: [&str; 5] = ["host", "port", "mountpoint", "username", "password"];

impl Default for Form {
    fn default() -> Form {
        Form {
            enabled: false,
            source: "usb".into(),
            mode: "survey".into(),
            accuracy: "2".into(),
            duration: "180".into(),
            fixed: ["0".into(), "0".into(), "0".into(), "0".into()],
            ntrip: Default::default(),
            has_password: false,
        }
    }
}

impl Form {
    /// Fills the form from the stored settings. The real password never
    /// reaches the page: an empty box with "stored" says one is kept, and an
    /// empty box on save means keep it.
    pub fn from_settings(s: &Value) -> Form {
        let text = |v: Option<&Value>, empty: &str| match v {
            None | Some(Value::Null) => empty.to_string(),
            Some(Value::String(t)) => t.clone(),
            Some(other) => other.to_string(),
        };
        let fixed = s.get("fixed").cloned().unwrap_or(Value::Null);
        let ntrip = s.get("ntrip").cloned().unwrap_or(Value::Null);
        let num_text = |v: Option<&Value>, empty: &str| match v.and_then(Value::as_f64) {
            Some(n) => js_number(n),
            None => text(v, empty),
        };
        Form {
            enabled: s.get("enabled") == Some(&Value::Bool(true)),
            source: s
                .get("source")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or("usb")
                .into(),
            mode: s
                .get("mode")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or("survey")
                .into(),
            accuracy: num_text(s.get("survey_accuracy"), ""),
            duration: num_text(s.get("survey_duration"), ""),
            fixed: FIXED_KEYS.map(|k| num_text(fixed.get(k), "0")),
            ntrip: NTRIP_KEYS.map(|k| {
                if k == "password" {
                    String::new()
                } else if k == "port" {
                    num_text(ntrip.get(k), "")
                } else {
                    text(ntrip.get(k), "")
                }
            }),
            has_password: ntrip.get("has_password") == Some(&Value::Bool(true)),
        }
    }

    /// The body of `POST /api/rtk/settings`: the whole form, which the backend
    /// replaces wholesale.
    pub fn body(&self) -> Value {
        let n = |t: &str| t.trim().parse::<f64>().unwrap_or(0.0);
        let port = if n(&self.ntrip[1]) == 0.0 {
            2101.0
        } else {
            n(&self.ntrip[1])
        };
        json!({
            "enabled": self.enabled,
            "source": self.source,
            "mode": self.mode,
            "survey_accuracy": n(&self.accuracy),
            "survey_duration": n(&self.duration),
            "fixed": {
                "latitude": n(&self.fixed[0]), "longitude": n(&self.fixed[1]),
                "altitude": n(&self.fixed[2]), "accuracy": n(&self.fixed[3]),
            },
            "ntrip": {
                "host": self.ntrip[0].trim(),
                "port": port,
                "mountpoint": self.ntrip[2].trim(),
                "username": self.ntrip[3].trim(),
                // empty means keep the stored one
                "password": self.ntrip[4],
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &str = include_str!("rtk_cases.json");

    fn arg(c: &Value) -> Option<Value> {
        if c["a"] == "__undef" {
            None
        } else {
            Some(c["a"].clone())
        }
    }

    #[test]
    fn numbers_bytes_and_fixes_read_as_the_web_page_reads_them() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        for c in cases["fmt"].as_array().unwrap() {
            // the NaN case is not JSON: it came out as null, and its answer is a dash
            let a = arg(c);
            let got = if c["a"] == Value::Null && c["r"] == "\u{2014}" {
                "\u{2014}".to_string()
            } else {
                fmt(a.as_ref())
            };
            assert_eq!(got, c["r"].as_str().unwrap(), "fmt {}", c["a"]);
        }
        for c in cases["bytes"].as_array().unwrap() {
            assert_eq!(
                format_bytes(Some(&c["a"])),
                c["r"].as_str().unwrap(),
                "bytes {}",
                c["a"]
            );
        }
        for c in cases["fix"].as_array().unwrap() {
            assert_eq!(friendly_fix(c["a"].as_str().unwrap()), c["r"].as_str().unwrap());
        }
    }

    #[test]
    fn the_state_is_a_word_and_a_tone() {
        assert_eq!(state_label("surveying"), "Surveying");
        assert_eq!(state_label("active"), "Correcting");
        assert_eq!(state_label("weird"), "weird");
        assert_eq!(state_tone("active"), "ok");
        assert_eq!(state_tone("error"), "err");
        assert_eq!(state_tone("searching"), "pending");
        assert_eq!(state_tone("off"), "");
    }

    #[test]
    fn the_survey_says_what_is_holding_it_up() {
        let running = json!({"survey": {"active": true, "valid": false, "duration": 62.4, "accuracy": 3.456},
            "survey_target": {"duration": 180, "accuracy": 2.0}});
        assert_eq!(
            survey_sentence(&running, 35.0),
            "35% \u{b7} 62 s of 180 s \u{b7} accurate to 3.46 m, needs 2 m"
        );
        let early =
            json!({"survey": {"active": true, "duration": 3.0}, "survey_target": {"duration": 180, "accuracy": 2.0}});
        assert_eq!(survey_sentence(&early, 2.0), "2% \u{b7} 3 s of 180 s");
        let done = json!({"survey": {"active": false, "valid": true, "accuracy": 1.234}});
        assert_eq!(
            survey_sentence(&done, 100.0),
            "Survey complete: the base knows its position to 1.23 m."
        );
    }

    #[test]
    fn the_rows_describe_the_route_party_by_party() {
        assert_eq!(describe_corrections(&json!({"frames": 0})), "none yet");
        let d = json!({"frames": 1200, "messages": {"1077": 600, "1005": 120}, "source_age": 2.0});
        assert_eq!(describe_corrections(&d), "1200 messages (1005, 1077)");
        let stale = json!({"frames": 10, "messages": {}, "source_age": 8.6});
        assert_eq!(describe_corrections(&stale), "10 messages \u{b7} last 9 s ago");
        assert_eq!(
            describe_injected(&json!({"link_ready": false})),
            "waiting for the aircraft link"
        );
        assert_eq!(
            describe_injected(&json!({"link_ready": true, "injected": {"messages": 0}})),
            "none yet"
        );
        let sent = json!({"link_ready": true, "injected": {"messages": 40, "bytes": 5120, "dropped": 2}});
        assert_eq!(
            describe_injected(&sent),
            "40 messages, 5.0 kB \u{b7} 2 could not be sent"
        );
        assert_eq!(receiver_text(&json!({"receiver_label": "u-blox F9P"})), "u-blox F9P");
        assert_eq!(
            receiver_text(&json!({"state": "active", "receiver": null})),
            "a base that configured itself"
        );
        assert_eq!(receiver_text(&json!({"state": "off", "receiver": null})), "\u{2014}");
        assert_eq!(
            port_text(&json!({"device": "/dev/ttyACM0", "baud": 115200})),
            "/dev/ttyACM0 at 115200 baud"
        );
        assert_eq!(port_text(&json!({"device": "/dev/ttyACM0"})), "/dev/ttyACM0");
        assert_eq!(port_text(&json!({"device": ""})), "\u{2014}");
        assert_eq!(
            fix_text(&json!({"vehicle": {"fix": "RTK_FIXED", "satellites": 18}})),
            "RTK fixed (centimetres) \u{b7} 18 satellites"
        );
        assert_eq!(fix_text(&json!({"vehicle": {}})), "\u{2014}");
    }

    #[test]
    fn survey_again_is_for_a_working_base_on_a_cable() {
        assert!(can_restart(
            &json!({"source": "usb", "receiver": {"x": 1}, "state": "active"})
        ));
        assert!(can_restart(
            &json!({"source": "usb", "receiver": "f9p", "state": "surveying"})
        ));
        assert!(!can_restart(
            &json!({"source": "ntrip", "receiver": "f9p", "state": "active"})
        ));
        assert!(!can_restart(
            &json!({"source": "usb", "receiver": null, "state": "active"})
        ));
        assert!(!can_restart(
            &json!({"source": "usb", "receiver": "f9p", "state": "off"})
        ));
    }

    #[test]
    fn the_form_is_filled_from_the_settings_and_sent_whole() {
        let f = Form::from_settings(&json!({
            "enabled": true, "source": "ntrip", "mode": "fixed", "survey_accuracy": 2.0, "survey_duration": 180,
            "fixed": {"latitude": 48.07, "longitude": 11.64, "altitude": 570.5, "accuracy": 0.1},
            "ntrip": {"host": "caster.example", "port": 2101, "mountpoint": "NEAR", "username": "u", "password": "", "has_password": true}
        }));
        assert!(f.enabled && f.has_password);
        assert_eq!((f.accuracy.as_str(), f.duration.as_str()), ("2", "180"));
        assert_eq!(f.fixed, ["48.07", "11.64", "570.5", "0.1"]);
        assert_eq!(f.ntrip[4], "", "the stored password never reaches the box");
        let b = f.body();
        assert_eq!(b["source"], "ntrip");
        assert_eq!(b["fixed"]["latitude"], 48.07);
        assert_eq!(b["ntrip"]["port"], 2101.0);
        assert_eq!(b["ntrip"]["password"], "");
        // a blank or zero port is the usual one, and typed text is trimmed
        let g = Form {
            ntrip: [" host ".into(), "".into(), " m ".into(), " u ".into(), "pw".into()],
            ..Form::default()
        };
        let b = g.body();
        assert_eq!(
            (b["ntrip"]["host"].as_str(), b["ntrip"]["port"].as_f64()),
            (Some("host"), Some(2101.0))
        );
        assert_eq!(
            (b["ntrip"]["mountpoint"].as_str(), b["ntrip"]["password"].as_str()),
            (Some("m"), Some("pw"))
        );
    }
}
