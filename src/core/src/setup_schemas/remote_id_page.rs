//! The Setup, Remote ID page: the option lists of the form, the filing checks
//! on the identity, and the two vehicle schemas (`remote_id.py`,
//! `remote_id_config.py`, `ardupilot_remote_id.py`).
//!
//! The identity itself is the operator's and lives in `protocol::remote_id`
//! (settings, messages, broadcast); what is here is what the page shows about it.
//! The findings are advisory: nothing blocks a save, because an aircraft flown
//! under an exemption is not misconfigured.

use serde_json::{json, Value};

use super::fields::{bitmask, enum_, number, present, section, Values};
use crate::protocol::remote_id::{RemoteId, ALTITUDE_UNKNOWN, DESCRIPTION_MAX, OPERATOR_ID_MAX, UAS_ID_MAX};

const REGIONS: [(&str, &str); 2] = [
    ("faa", "United States (FAA Part 89)"),
    ("eu", "European Union (EU 2019/945, EN 4709-002)"),
];

const ID_TYPES: &[(i64, &str)] = &[
    (0, "None"),
    (1, "Serial number (ANSI/CTA-2063-A)"),
    (2, "CAA registration ID"),
    (3, "UTM-assigned UUID"),
    (4, "Specific session ID"),
];
const UA_TYPES: &[(i64, &str)] = &[
    (0, "Undeclared"),
    (1, "Aeroplane"),
    (2, "Helicopter or multirotor"),
    (3, "Gyroplane"),
    (4, "Hybrid lift (VTOL)"),
    (5, "Ornithopter"),
    (6, "Glider"),
    (7, "Kite"),
    (8, "Free balloon"),
    (9, "Captive balloon"),
    (10, "Airship"),
    (11, "Free-fall parachute"),
    (12, "Rocket"),
    (13, "Tethered powered aircraft"),
    (14, "Ground obstacle"),
    (15, "Other"),
];
const OPERATOR_ID_TYPES: &[(i64, &str)] = &[(0, "CAA-issued operator registration")];
const DESCRIPTION_TYPES: &[(i64, &str)] = &[(0, "Flight description"), (1, "Emergency"), (2, "Extended status")];
const LOCATION_TYPES: &[(i64, &str)] = &[
    (0, "Take-off location"),
    (1, "Live GNSS of the control station"),
    (2, "Fixed position"),
];
const CLASSIFICATION_TYPES: &[(i64, &str)] = &[(0, "Undeclared"), (1, "European Union")];
const CATEGORIES_EU: &[(i64, &str)] = &[(0, "Undeclared"), (1, "Open"), (2, "Specific"), (3, "Certified")];
const CLASSES_EU: &[(i64, &str)] = &[
    (0, "Undeclared"),
    (1, "Class C0 (under 250 g)"),
    (2, "Class C1 (under 900 g)"),
    (3, "Class C2 (under 4 kg)"),
    (4, "Class C3 (under 25 kg)"),
    (5, "Class C4 (under 25 kg, no automatic control)"),
    (6, "Class C5"),
    (7, "Class C6"),
];

const CTA_LENGTH_CODES: &str = "123456789ABCDEF";

fn options(table: &[(i64, &str)]) -> Vec<Value> {
    table.iter().map(|(v, l)| json!({"value": v, "label": l})).collect()
}

/// Everything the page needs to render the form without knowing the standard.
pub fn schema() -> Value {
    let mut locations = options(LOCATION_TYPES);
    if let Some(row) = locations.get_mut(1).and_then(Value::as_object_mut) {
        row.insert("disabled".into(), json!(true));
        row.insert("reason".into(), json!("Corvus has no GNSS receiver of its own"));
    }
    json!({
        "id_types": options(ID_TYPES),
        "ua_types": options(UA_TYPES),
        "operator_id_types": options(OPERATOR_ID_TYPES),
        "description_types": options(DESCRIPTION_TYPES),
        "location_types": locations,
        "classification_types": options(CLASSIFICATION_TYPES),
        "categories_eu": options(CATEGORIES_EU),
        "classes_eu": options(CLASSES_EU),
        "regions": REGIONS.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>(),
        "limits": {"uas_id": UAS_ID_MAX, "operator_id": OPERATOR_ID_MAX, "description": DESCRIPTION_MAX},
        "altitude_unknown": ALTITUDE_UNKNOWN,
    })
}

/// The MAV_ODID_UA_TYPE that matches a MAV_TYPE, or 0 when none does.
pub fn ua_type_for_vehicle(mav_type: i64) -> i64 {
    match mav_type {
        1 => 1,
        2 | 3 | 4 | 13 | 14 | 15 => 2,
        7 => 10,
        8 => 8,
        9 => 12,
        16 => 5,
        19..=25 => 4,
        _ => 0,
    }
}

/// Why `value` is not an ANSI/CTA-2063-A serial number, or "".
pub fn serial_number_problem(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return "no serial number".into();
    }
    if chars.len() < 6 {
        return "too short: a CTA-2063-A serial is a 4-character manufacturer code, a length character, then the serial itself".into();
    }
    if chars.len() > UAS_ID_MAX {
        return format!("longer than the {UAS_ID_MAX} characters the message carries");
    }
    let allowed = |c: char| (c.is_ascii_digit() || c.is_ascii_uppercase()) && c != 'I' && c != 'O';
    let mut bad: Vec<char> = chars.iter().copied().filter(|c| !allowed(*c)).collect();
    bad.sort_unstable();
    bad.dedup();
    if !bad.is_empty() {
        let shown: Vec<String> = bad.iter().map(|c| py_repr(*c)).collect();
        return format!(
            "uses {}. CTA-2063-A allows digits and capitals only, without I or O",
            shown.join(", ")
        );
    }
    let code = chars[4];
    if !CTA_LENGTH_CODES.contains(code) {
        return format!(
            "the 5th character is the length code and must be 1-9 or A-F, not {}",
            py_repr(code)
        );
    }
    let declared = code.to_digit(16).unwrap_or(0) as usize;
    let actual = chars.len() - 5;
    if actual != declared {
        return format!("the length character says {declared} characters follow it, but {actual} do");
    }
    String::new()
}

fn py_repr(c: char) -> String {
    if c == '\'' {
        "\"'\"".into()
    } else {
        format!("'{c}'")
    }
}

/// Why `value` does not look like an EU operator registration, or "".
pub fn eu_operator_id_problem(value: &str) -> String {
    if value.is_empty() {
        return "no operator registration number".into();
    }
    let stripped: Vec<char> = value.chars().filter(|c| *c != '-').collect();
    if stripped.len() > 16 {
        return "the last characters look like the secret part of your registration. Only the first 16 are broadcast, and the rest must not be".into();
    }
    if stripped.len() < 16 {
        return format!(
            "{} characters; an EU operator registration is 16, not counting the secret part",
            stripped.len()
        );
    }
    let head: Vec<char> = value.chars().take(3).collect();
    if head.len() < 3 || !head.iter().all(|c| c.is_alphabetic()) {
        return "does not start with a 3-letter country code".into();
    }
    if !stripped.iter().all(|c| c.is_alphanumeric()) {
        return "contains characters an operator registration does not use".into();
    }
    String::new()
}

fn finding(level: &str, field: &str, text: &str) -> Value {
    json!({"level": level, "field": field, "text": text})
}

/// What is missing or wrong in `identity` for its region.
pub fn findings(identity: &RemoteId) -> Vec<Value> {
    let area = if matches!(identity.region.as_str(), "faa" | "eu") {
        identity.region.as_str()
    } else {
        "eu"
    };
    let mut out: Vec<Value> = Vec::new();
    if identity.uas_id.is_empty() {
        out.push(finding(
            "error",
            "uas_id",
            "No aircraft ID. Nothing identifies the airframe.",
        ));
    } else if identity.id_type == 1 {
        let problem = serial_number_problem(&identity.uas_id);
        if !problem.is_empty() {
            out.push(finding("error", "uas_id", &format!("Serial number {problem}.")));
        }
    }
    if identity.id_type == 0 && !identity.uas_id.is_empty() {
        out.push(finding(
            "error",
            "id_type",
            "The ID type is None, so the ID is broadcast as meaning nothing.",
        ));
    }
    if identity.ua_type == 0 {
        out.push(finding("warning", "ua_type", "The aircraft type is undeclared."));
    }
    if area == "faa" {
        if identity.id_type != 1 && identity.id_type != 4 {
            out.push(finding(
                "error",
                "id_type",
                "Part 89 broadcasts either a CTA-2063-A serial number or a session ID; this is neither.",
            ));
        }
        if identity.operator_location_type == 0 {
            out.push(finding(
                "error",
                "operator_location_type",
                "Part 89 broadcasts the control station's position. The take-off point is not it.",
            ));
        }
    } else {
        if identity.operator_id.is_empty() {
            out.push(finding(
                "error",
                "operator_id",
                "No operator registration number. EU direct remote ID broadcasts the operator, not only the aircraft.",
            ));
        } else {
            let problem = eu_operator_id_problem(&identity.operator_id);
            if !problem.is_empty() {
                let level = if problem.contains("secret") { "error" } else { "warning" };
                out.push(finding(
                    level,
                    "operator_id",
                    &format!("Operator registration {problem}."),
                ));
            }
        }
        if identity.id_type != 1 {
            out.push(finding(
                "warning",
                "id_type",
                "EU direct remote ID expects the UAS serial number as the aircraft ID.",
            ));
        }
        if identity.classification_type != 1 {
            out.push(finding(
                "warning",
                "classification_type",
                "The EU classification is undeclared, so no category or class is broadcast.",
            ));
        } else {
            if identity.category_eu == 0 {
                out.push(finding(
                    "error",
                    "category_eu",
                    "EU classification is declared but the operational category is not.",
                ));
            }
            if identity.class_eu == 0 && identity.category_eu == 1 {
                out.push(finding(
                    "warning",
                    "class_eu",
                    "An Open-category aircraft normally carries a class mark (C0-C6).",
                ));
            }
        }
    }
    if identity.operator_location_type == 2 && identity.operator_latitude == 0.0 && identity.operator_longitude == 0.0 {
        out.push(finding(
            "error",
            "operator_latitude",
            "The operator position is fixed but sits at 0°N 0°E.",
        ));
    }
    if identity.operator_location_type == 1 {
        out.push(finding(
            "error",
            "operator_location_type",
            "Live GNSS means the control station's own receiver, and Corvus has none. Use a fixed position or the take-off point.",
        ));
    }
    out
}

/// `COM_ARM_ODID` is the whole PX4 schema.
pub fn px4_param_names() -> Vec<String> {
    vec!["COM_ARM_ODID".into()]
}

pub fn px4_build(values: &Values) -> Value {
    let fields = present(vec![enum_(
        "COM_ARM_ODID",
        "Remote ID arming check",
        values,
        &options(&[(0, "Disabled"), (1, "Warn if unavailable"), (2, "Required to arm")]),
    )
    .hint(
        "Whether a Remote ID system that is absent or unhealthy blocks arming. Required is what a jurisdiction that mandates Remote ID expects; Warn lets the aircraft fly and says so.",
    )]);
    let sections: Vec<Value> = section(
        "vehicle",
        "On the aircraft",
        fields,
        "PX4 keeps one Remote ID setting of its own. Everything the aircraft broadcasts about you and about itself comes from this ground station over the link.",
    )
    .map(Value::Object)
    .into_iter()
    .collect();
    json!({"sections": sections, "received": values.len()})
}

pub fn ardupilot_param_names() -> Vec<String> {
    [
        "DID_ENABLE",
        "DID_MAVPORT",
        "DID_CANDRIVER",
        "DID_OPTIONS",
        "DID_BARO_ACC",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn ardupilot_build(values: &Values) -> Value {
    let mut ports = vec![
        json!({"value": -1, "label": "Disabled"}),
        json!({"value": 0, "label": "Serial port 0 (console)"}),
    ];
    ports.extend((1..=6).map(|n| json!({"value": n, "label": format!("Serial port {n}")})));
    let fields = present(vec![
        enum_("DID_ENABLE", "Remote ID", values, &options(&[(0, "Disabled"), (1, "Enabled")])).hint(
            "Starts the Remote ID driver. It needs a reboot, and on its own it broadcasts nothing. The transmitter's port has to be set below.",
        ),
        enum_("DID_MAVPORT", "Transmitter serial port", values, &ports).hint(
            "The MAVLink serial port a serial Remote ID module is wired to. Disabled here with the driver enabled is the usual reason an aircraft broadcasts nothing.",
        ),
        enum_(
            "DID_CANDRIVER",
            "DroneCAN transmitter",
            values,
            &options(&[(0, "Disabled"), (1, "DroneCAN driver 1"), (2, "DroneCAN driver 2")]),
        )
        .hint("Use instead of the serial port when the module is on CAN."),
        bitmask(
            "DID_OPTIONS",
            "Options",
            values,
            vec![json!({"bit": 0, "label": "Enforce the arming checks"})],
        )
        .hint(
            "With the arming checks enforced, the aircraft refuses to arm while the Remote ID system is unhealthy or the ground station has stopped sending the identity.",
        ),
        number("DID_BARO_ACC", "Barometer height accuracy", values)
            .unit("m")
            .step(0.1)
            .min(0)
            .hint("What the broadcast claims for its barometric height. 0 declares the accuracy unknown."),
    ]);
    let sections: Vec<Value> = section(
        "vehicle",
        "On the aircraft",
        fields,
        "ArduPilot's Remote ID driver and the transmitter it talks to. The serial number, the operator registration and the class are not stored here. They are sent from this ground station over the link, and the aircraft will not arm once they stop arriving.",
    )
    .map(Value::Object)
    .into_iter()
    .collect();
    json!({"sections": sections, "received": values.len()})
}
