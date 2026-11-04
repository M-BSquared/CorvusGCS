//! What the Remote ID page says and sends (`python/src/js/setup-remoteid.js`). The
//! identity (serial number, operator registration, class mark) belongs to the
//! ground station, not to the vehicle: it is saved to the config file and
//! pushed down the link once a second, so the page edits a document and keeps
//! a few rules of its own: the nested patch a row saves, the serial number
//! check as the operator types, "not known" as the -1000 m sentinel, and the
//! words for the three status rows.

use serde_json::{json, Map, Value};

use crate::data::units::js_number;
use crate::setup::schema_form::{self, Field};

/// How often the broadcast status is re-read while the page is open.
pub const POLL_MS: f64 = 3000.0;

/// The Open Drone ID sentinel for "not known" on every altitude field. It is
/// a real number on the wire, so an empty box has to become this, not 0.
pub const ALTITUDE_UNKNOWN: f64 = -1000.0;

/// What a page that has never been saved holds, so the form can draw first.
pub fn empty_identity() -> Value {
    json!({
        "enabled": false, "region": "eu",
        "basic_id": {"id_type": 1, "ua_type": 0, "uas_id": ""},
        "operator_id": {"operator_id_type": 0, "operator_id": ""},
        "self_id": {"description_type": 0, "description": ""},
        "system": {
            "operator_location_type": 0, "operator_latitude": 0, "operator_longitude": 0,
            "operator_altitude_geo": ALTITUDE_UNKNOWN, "classification_type": 0,
            "category_eu": 0, "class_eu": 0, "area_count": 1, "area_radius": 0,
            "area_ceiling": ALTITUDE_UNKNOWN, "area_floor": ALTITUDE_UNKNOWN,
        },
    })
}

/// `cloneIdentity`: the empty identity with what `src` holds laid over it.
pub fn clone_identity(src: &Value) -> Value {
    let base = empty_identity();
    let mut out = Map::new();
    out.insert(
        "enabled".into(),
        json!(src.get("enabled").and_then(Value::as_bool).unwrap_or(false)),
    );
    out.insert(
        "region".into(),
        json!(src
            .get("region")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .unwrap_or("eu")),
    );
    for key in ["basic_id", "operator_id", "self_id", "system"] {
        let mut m = base[key].as_object().cloned().unwrap_or_default();
        if let Some(over) = src.get(key).and_then(Value::as_object) {
            for (k, v) in over {
                m.insert(k.clone(), v.clone());
            }
        }
        out.insert(key.into(), Value::Object(m));
    }
    Value::Object(out)
}

/// `readValue`: `identity[...path][key]`.
pub fn read<'a>(identity: &'a Value, path: &[&str], key: &str) -> &'a Value {
    let mut node = identity;
    for step in path {
        node = node.get(*step).unwrap_or(&Value::Null);
    }
    node.get(key).unwrap_or(&Value::Null)
}

/// `writeLocal`.
pub fn write(identity: &mut Value, path: &[&str], key: &str, value: Value) {
    let mut node = identity;
    for step in path {
        if !node.get(*step).is_some_and(Value::is_object) {
            node[*step] = json!({});
        }
        node = &mut node[*step];
    }
    node[key] = value;
}

/// `patchFor`: the nested patch a row saves, `{system: {area_floor: 12}}`.
pub fn patch_for(path: &[&str], key: &str, value: Value) -> Value {
    let mut out = json!({ key: value });
    for step in path.iter().rev() {
        out = json!({ *step: out });
    }
    out
}

/// The body of POST /api/config for a patch of the identity.
pub fn save_body(patch: &Value) -> Value {
    json!({ "remote_id": patch })
}

/// One entry of a select the backend described.
#[derive(Debug, Clone, PartialEq)]
pub struct Opt {
    pub value: Value,
    pub label: String,
    pub disabled: bool,
}

/// `options(state, key, fallback)`: the list the schema names, else `fallback`.
pub fn options(schema: &Value, key: &str, fallback: &[(&str, &str)]) -> Vec<Opt> {
    let list: Vec<Opt> = schema
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|o| {
                    let label = o.get("label").and_then(Value::as_str).unwrap_or("");
                    let disabled = o.get("disabled").and_then(Value::as_bool).unwrap_or(false);
                    let reason = o.get("reason").and_then(Value::as_str).unwrap_or("");
                    Opt {
                        value: o.get("value").cloned().unwrap_or(Value::Null),
                        label: if disabled && !reason.is_empty() {
                            format!("{label} ({reason})")
                        } else {
                            label.to_string()
                        },
                        disabled,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if list.is_empty() {
        fallback
            .iter()
            .map(|(v, l)| Opt {
                value: json!(v),
                label: (*l).into(),
                disabled: false,
            })
            .collect()
    } else {
        list
    }
}

/// A character cap of the message format (`limits`).
pub fn limit(schema: &Value, key: &str, default: usize) -> usize {
    schema
        .get("limits")
        .and_then(|l| l.get(key))
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .map_or(default, |n| n as usize)
}

/// `serialProblem`: whether a serial number fits the CTA-2063-A shape, said
/// while the character is still under the cursor. The value is saved either
/// way: the check is about the shape of a filing, and a page that refused to
/// remember a half typed serial number would lose it.
pub fn serial_problem(id_type: i64, value: &str) -> String {
    if id_type != 1 {
        return String::new();
    }
    let text = value.trim();
    if text.is_empty() {
        return String::new();
    }
    if text.chars().any(|c| !(c.is_ascii_digit() || c.is_ascii_uppercase())) {
        return "capitals and digits only".into();
    }
    if text.contains(['I', 'O']) {
        return "the letters I and O are not allowed".into();
    }
    let n = text.chars().count();
    if n < 6 {
        return "too short for a CTA-2063-A serial".into();
    }
    let declared = text.chars().nth(4).and_then(|c| c.to_digit(16)).unwrap_or(0) as usize;
    if !(1..=15).contains(&declared) {
        return "the 5th character is the length code and must be 1-9 or A-F".into();
    }
    if n - 5 != declared {
        return format!(
            "the length character says {declared} characters follow it, but {} do",
            n - 5
        );
    }
    String::new()
}

/// What typing into a capped text box leaves: capitals when asked, at most
/// `max` characters.
pub fn typed_text(text: &str, uppercase: bool, max: usize) -> String {
    let t: String = if uppercase {
        text.to_uppercase()
    } else {
        text.to_string()
    };
    t.chars().take(max).collect()
}

/// The counter beside a capped text box.
pub fn counter(text: &str, max: usize) -> String {
    format!("{}/{max}", text.chars().count())
}

/// The aircraft type the connected vehicle looks like, offered as a button
/// worded as what the airframe is, never as what to file it as.
pub fn suggestion(doc: &Value, identity: &Value, ua_types: &[Opt]) -> Option<(i64, String)> {
    let suggested = doc.get("suggested_ua_type").and_then(Value::as_i64).unwrap_or(0);
    let current = read(identity, &["basic_id"], "ua_type").as_i64().unwrap_or(0);
    if suggested == 0 || suggested == current {
        return None;
    }
    let m = ua_types.iter().find(|o| o.value.as_i64() == Some(suggested))?;
    Some((
        suggested,
        format!("The connected aircraft is a {}", m.label.to_lowercase()),
    ))
}

/// What committing a number box means.
#[derive(Debug, Clone, PartialEq)]
pub enum Commit {
    /// Save this number.
    Value(f64),
    /// An empty box on a field that has a "not known" value.
    Unknown,
    /// Not saved: this is wrong with it.
    Problem(String),
}

/// A number row's limits and "not known" value.
#[derive(Debug, Clone, Copy)]
pub struct Range {
    pub min: f64,
    pub max: f64,
    pub unknown: Option<f64>,
}

/// The text a number box starts with: empty for "not known".
pub fn number_text(raw: f64, range: Range) -> String {
    if range.unknown == Some(raw) {
        String::new()
    } else {
        schema_form::format_number(raw)
    }
}

/// What a number box holds when it is left (`numberRow`'s change handler).
pub fn commit_number(text: &str, range: Range) -> Commit {
    let t = text.trim();
    if t.is_empty() && range.unknown.is_some() {
        return Commit::Unknown;
    }
    let f = Field {
        param: String::new(),
        label: String::new(),
        kind: schema_form::Kind::Number,
        value: 0.0,
        unit: String::new(),
        hint: String::new(),
        min: Some(range.min),
        max: Some(range.max),
        also: vec![],
    };
    let problem = schema_form::range_problem(&f, t);
    if problem.is_empty() {
        Commit::Value(t.parse().unwrap_or(0.0))
    } else {
        Commit::Problem(problem)
    }
}

/// One of the three rows of "Right now".
#[derive(Debug, Clone, PartialEq)]
pub struct StatusRow {
    pub label: &'static str,
    pub text: String,
    pub level: &'static str,
}

/// `statusCard`: the link, the broadcast and the aircraft's own verdict.
pub fn status_rows(live: &Value) -> [StatusRow; 3] {
    let flag = |k: &str| live.get(k).and_then(Value::as_bool).unwrap_or(false);
    let link = if flag("supported") {
        (
            "MAVLink 2: the Remote ID messages can be carried".to_string(),
            "healthy",
        )
    } else {
        ("MAVLink 1, or no vehicle. Remote ID needs MAVLink 2".to_string(), "off")
    };
    let sending = if !flag("enabled") {
        ("Off, nothing is being sent".to_string(), "off")
    } else if flag("broadcasting") {
        (
            match live.get("last_sent_age").filter(|v| !v.is_null()) {
                Some(age) => format!("Sending, last message {} s ago", show(age)),
                None => "Sending".to_string(),
            },
            "healthy",
        )
    } else {
        let err = live.get("error").and_then(Value::as_str).unwrap_or("");
        (
            if err.is_empty() {
                "On, but nothing has gone out yet".to_string()
            } else {
                err.to_string()
            },
            "warning",
        )
    };
    let arm = match live.get("arm_status").filter(|a| a.is_object()) {
        // not a fault: PX4 does not send this message at all
        None => (
            "The aircraft has not reported a Remote ID arm status".to_string(),
            "off",
        ),
        Some(a) if a.get("ok").and_then(Value::as_bool).unwrap_or(false) => {
            ("The aircraft's Remote ID system is ready to arm".to_string(), "healthy")
        }
        Some(a) => {
            let err = a.get("error").and_then(Value::as_str).unwrap_or("");
            (
                if err.is_empty() {
                    "The aircraft's Remote ID system would refuse the arm".to_string()
                } else {
                    err.to_string()
                },
                "critical",
            )
        }
    };
    [
        StatusRow {
            label: "Link",
            text: link.0,
            level: link.1,
        },
        StatusRow {
            label: "Broadcast",
            text: sending.0,
            level: sending.1,
        },
        StatusRow {
            label: "Aircraft",
            text: arm.0,
            level: arm.1,
        },
    ]
}

fn show(v: &Value) -> String {
    match v {
        Value::Number(n) => js_number(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// One entry of "What the rules ask for".
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub level: String,
    pub text: String,
}

pub fn findings(doc: &Value) -> Vec<Finding> {
    doc.get("findings")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|f| Finding {
                    level: f
                        .get("level")
                        .and_then(Value::as_str)
                        .filter(|l| !l.is_empty())
                        .unwrap_or("warning")
                        .into(),
                    text: f.get("text").and_then(Value::as_str).unwrap_or("").into(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The line of the "Right now" card's region: the label of the picked region.
pub fn region_label(schema: &Value, identity: &Value) -> String {
    let regions = options(schema, "regions", &[]);
    let region = read(identity, &[], "region").as_str().unwrap_or("");
    regions
        .iter()
        .find(|r| r.value.as_str() == Some(region))
        .map(|r| r.label.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "the selected region".into())
}

/// The vehicle's position as `(latitude, longitude)`, or none when nothing
/// real has arrived. The state holds it as `[longitude, latitude]`, like
/// GeoJSON; 0, 0 is the store's "nothing yet" and also a point in the
/// Atlantic: not a position.
pub fn vehicle_position(state: &Value) -> Option<(f64, f64)> {
    let pos = state.get("position")?.as_array()?;
    if pos.len() < 2 {
        return None;
    }
    let (lon, lat) = (pos[0].as_f64()?, pos[1].as_f64()?);
    if !lat.is_finite() || !lon.is_finite() || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    Some((lat, lon))
}

/// The status line after a read of the whole document.
pub fn loaded_status(doc: &Value) -> (&'static str, String) {
    if doc.get("connected").and_then(Value::as_bool).unwrap_or(false) {
        (
            "ok",
            format!(
                "{} vehicle parameters read",
                doc.get("received").and_then(Value::as_i64).unwrap_or(0)
            ),
        )
    } else {
        let err = doc.get("error").and_then(Value::as_str).unwrap_or("");
        (
            "",
            if err.is_empty() {
                "No vehicle connected. The identity below is still editable".to_string()
            } else {
                format!("Vehicle settings unavailable: {err}")
            },
        )
    }
}

/// The name of the check on the vehicle's own sections: every parameter once.
pub fn check_names(sections: &[VehicleSection]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for s in sections {
        for f in &s.fields {
            if !names.contains(&f.param) {
                names.push(f.param.clone());
            }
        }
    }
    names
}

/// One card of the vehicle's own Remote ID parameters.
#[derive(Debug, Clone)]
pub struct VehicleSection {
    pub title: String,
    pub hint: String,
    pub fields: Vec<Field>,
}

pub fn sections(doc: &Value) -> Vec<VehicleSection> {
    doc.get("sections")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|s| VehicleSection {
                    title: s.get("title").and_then(Value::as_str).unwrap_or("").into(),
                    hint: s.get("hint").and_then(Value::as_str).unwrap_or("").into(),
                    fields: schema_form::parse_fields(s.get("fields").unwrap_or(&Value::Null)),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identity_is_the_empty_one_with_what_was_saved_over_it() {
        let saved = json!({"enabled": true, "region": "", "basic_id": {"uas_id": "ABC"}, "system": {"area_count": 3}});
        let id = clone_identity(&saved);
        assert_eq!(id["enabled"], true);
        assert_eq!(id["region"], "eu");
        assert_eq!(id["basic_id"], json!({"id_type": 1, "ua_type": 0, "uas_id": "ABC"}));
        assert_eq!(id["system"]["area_count"], 3);
        assert_eq!(id["system"]["area_floor"], -1000.0);
        assert_eq!(
            clone_identity(&Value::Null),
            empty_identity()
                .as_object()
                .map(|_| clone_identity(&json!({})))
                .unwrap()
        );
    }

    #[test]
    fn rows_read_write_and_patch_by_path() {
        let mut id = clone_identity(&json!({}));
        assert_eq!(read(&id, &["basic_id"], "id_type"), &json!(1));
        assert_eq!(read(&id, &[], "region"), &json!("eu"));
        assert_eq!(read(&id, &["nowhere"], "x"), &Value::Null);
        write(&mut id, &["system"], "area_floor", json!(12));
        assert_eq!(id["system"]["area_floor"], 12);
        write(&mut id, &["extra", "deep"], "k", json!(1));
        assert_eq!(id["extra"]["deep"]["k"], 1);
        assert_eq!(
            patch_for(&["system"], "area_floor", json!(12)),
            json!({"system": {"area_floor": 12}})
        );
        assert_eq!(patch_for(&[], "enabled", json!(true)), json!({"enabled": true}));
        assert_eq!(
            save_body(&patch_for(&["basic_id"], "uas_id", json!("X"))),
            json!({"remote_id": {"basic_id": {"uas_id": "X"}}})
        );
    }

    #[test]
    fn the_serial_number_check() {
        assert_eq!(serial_problem(2, "anything"), "");
        assert_eq!(serial_problem(1, ""), "");
        assert_eq!(serial_problem(1, "ab1234"), "capitals and digits only");
        assert_eq!(serial_problem(1, "1596I483658"), "the letters I and O are not allowed");
        assert_eq!(serial_problem(1, "AB12"), "too short for a CTA-2063-A serial");
        assert_eq!(
            serial_problem(1, "ABCDGXYZ"),
            "the 5th character is the length code and must be 1-9 or A-F"
        );
        assert_eq!(
            serial_problem(1, "ABCD5XYZ"),
            "the length character says 5 characters follow it, but 3 do"
        );
        assert_eq!(serial_problem(1, "ABCD3XYZ"), "");
        assert_eq!(serial_problem(1, "  1596F483658SK8U6PNJ1  "), "");
    }

    #[test]
    fn typing_is_capped_and_counted() {
        assert_eq!(typed_text("abcd3xyz", true, 6), "ABCD3X");
        assert_eq!(typed_text("abc", false, 6), "abc");
        assert_eq!(counter("ABC", 20), "3/20");
        assert_eq!(limit(&json!({"limits": {"uas_id": 20}}), "uas_id", 9), 20);
        assert_eq!(limit(&json!({}), "description", 23), 23);
    }

    #[test]
    fn a_number_box_commits_or_says_why_not() {
        let alt = Range {
            min: -1000.0,
            max: 31767.0,
            unknown: Some(ALTITUDE_UNKNOWN),
        };
        let lat = Range {
            min: -90.0,
            max: 90.0,
            unknown: None,
        };
        assert_eq!(commit_number("", alt), Commit::Unknown);
        assert_eq!(commit_number("  ", alt), Commit::Unknown);
        assert_eq!(commit_number("120.5", alt), Commit::Value(120.5));
        assert_eq!(commit_number("40000", alt), Commit::Problem("above 31767".into()));
        assert_eq!(commit_number("", lat), Commit::Problem("not a number".into()));
        assert_eq!(commit_number("91", lat), Commit::Problem("above 90".into()));
        assert_eq!(number_text(-1000.0, alt), "");
        assert_eq!(number_text(12.0, alt), "12");
        assert_eq!(number_text(48.1234567, lat), "48.123457");
        assert_eq!(number_text(-1000.0, lat), "-1000");
    }

    #[test]
    fn the_select_lists() {
        let schema = json!({"location_types": [
            {"value": 0, "label": "Take-off location"},
            {"value": 1, "label": "Live GNSS", "disabled": true, "reason": "no receiver"}]});
        let o = options(&schema, "location_types", &[]);
        assert_eq!(o[1].label, "Live GNSS (no receiver)");
        assert!(o[1].disabled && !o[0].disabled);
        let f = options(&json!({}), "regions", &[("eu", "European Union")]);
        assert_eq!(f[0].value, json!("eu"));
        assert!(options(&json!({}), "regions", &[]).is_empty());
    }

    #[test]
    fn an_aircraft_type_is_offered_as_what_it_is() {
        let types = options(
            &json!({"ua_types": [{"value": 0, "label": "Undeclared"}, {"value": 2, "label": "Helicopter or multirotor"}]}),
            "ua_types",
            &[],
        );
        let id = clone_identity(&json!({}));
        assert_eq!(
            suggestion(&json!({"suggested_ua_type": 2}), &id, &types),
            Some((2, "The connected aircraft is a helicopter or multirotor".into()))
        );
        assert_eq!(suggestion(&json!({"suggested_ua_type": 0}), &id, &types), None);
        assert_eq!(suggestion(&json!({"suggested_ua_type": 9}), &id, &types), None);
        let same = clone_identity(&json!({"basic_id": {"ua_type": 2}}));
        assert_eq!(suggestion(&json!({"suggested_ua_type": 2}), &same, &types), None);
    }

    #[test]
    fn the_three_status_rows() {
        let off = status_rows(&json!({"enabled": false, "supported": false}));
        assert_eq!(off[0].level, "off");
        assert_eq!(off[1].text, "Off, nothing is being sent");
        assert_eq!(off[2].text, "The aircraft has not reported a Remote ID arm status");
        let on = status_rows(&json!({"enabled": true, "supported": true, "broadcasting": true,
            "last_sent_age": 1.5, "arm_status": {"ok": true}}));
        assert_eq!(on[0].level, "healthy");
        assert_eq!(on[1].text, "Sending, last message 1.5 s ago");
        assert_eq!(
            on[2],
            StatusRow {
                label: "Aircraft",
                text: "The aircraft's Remote ID system is ready to arm".into(),
                level: "healthy"
            }
        );
        let waiting = status_rows(&json!({"enabled": true, "broadcasting": false, "error": ""}));
        assert_eq!(waiting[1].text, "On, but nothing has gone out yet");
        assert_eq!(waiting[1].level, "warning");
        let refused = status_rows(
            &json!({"enabled": true, "broadcasting": true, "arm_status": {"ok": false, "error": "no GPS"}}),
        );
        assert_eq!(
            (refused[1].text.as_str(), refused[2].text.as_str(), refused[2].level),
            ("Sending", "no GPS", "critical")
        );
    }

    #[test]
    fn findings_and_the_status_line() {
        let f = findings(&json!({"findings": [{"level": "error", "text": "No ID"}, {"text": "x"}]}));
        assert_eq!((f[0].level.as_str(), f[1].level.as_str()), ("error", "warning"));
        assert!(findings(&json!({})).is_empty());
        assert_eq!(
            loaded_status(&json!({"connected": true, "received": 7})),
            ("ok", "7 vehicle parameters read".into())
        );
        assert_eq!(
            loaded_status(&json!({"connected": false, "error": "not connected"})).1,
            "Vehicle settings unavailable: not connected"
        );
        assert_eq!(
            loaded_status(&json!({"connected": false})).1,
            "No vehicle connected. The identity below is still editable"
        );
        let schema = json!({"regions": [{"value": "eu", "label": "European Union"}]});
        assert_eq!(region_label(&schema, &clone_identity(&json!({}))), "European Union");
        assert_eq!(
            region_label(&json!({}), &clone_identity(&json!({}))),
            "the selected region"
        );
    }

    #[test]
    fn the_position_is_a_real_one() {
        assert_eq!(vehicle_position(&json!({"position": [11.5, 48.1]})), Some((48.1, 11.5)));
        assert_eq!(vehicle_position(&json!({"position": [0.0, 0.0]})), None);
        assert_eq!(vehicle_position(&json!({"position": [48.1]})), None);
        assert_eq!(vehicle_position(&json!({})), None);
    }

    #[test]
    fn the_vehicle_sections() {
        let doc = json!({"sections": [{"id": "odid", "title": "Remote ID", "hint": "h", "fields": [
            {"param": "COM_ARM_ODID", "kind": "enum", "value": 0, "options": [{"value": 0, "label": "Off"}]},
            {"param": "COM_ARM_ODID", "kind": "number", "value": 0}]}]});
        let s = sections(&doc);
        assert_eq!(s[0].title, "Remote ID");
        assert_eq!(check_names(&s), vec!["COM_ARM_ODID".to_string()]);
    }
}
