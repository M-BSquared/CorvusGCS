//! The radio control schemas (PX4 and ArduPilot) and their calibration
//! validation answer what `rc_config.py` and `ardupilot_rc.py` answer
//! (`python/tools/dump_setup_schema_cases.py rc`).

mod setup_common;

use std::collections::HashSet;

use corvus_core::setup_schemas::{ardupilot_rc as ap, rc as px4};
use serde_json::{json, Value};
use setup_common::*;

fn run(
    entry: &Value,
    f: impl Fn(
        &Value,
        Option<&Value>,
        Option<&HashSet<String>>,
        Option<&Value>,
    ) -> Result<Vec<Value>, px4::CalibrationError>,
) -> Value {
    let known: Option<HashSet<String>> = (!entry[2].is_null()).then(|| strings(&entry[2]).into_iter().collect());
    let count = (!entry[1].is_null()).then_some(&entry[1]);
    let mapping = (!entry[3].is_null()).then_some(&entry[3]);
    match f(&entry[0], count, known.as_ref(), mapping) {
        Ok(w) => json!({"ok": w}),
        Err(e) => json!({"error": e.0}),
    }
}

#[test]
fn px4_pages_and_names_match() {
    let c = &cases("setup_rc_cases.json")["px4"];
    for (n, names) in c["names"].as_object().unwrap() {
        assert_eq!(px4::param_names(n.parse().unwrap()), strings(names), "names {n}");
    }
    for (n, options) in c["options"].as_object().unwrap() {
        assert_eq!(
            Value::Array(px4::channel_options(n.parse().unwrap())),
            *options,
            "options {n}"
        );
    }
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        let got = px4::build(&values_of(&row[0]), row[1].as_i64().unwrap());
        assert_page(&got, &row[2], &format!("px4 case {i}: {}", row[0]));
    }
}

#[test]
fn px4_calibration_matches() {
    let c = &cases("setup_rc_cases.json")["px4"];
    for row in c["calibration"].as_array().unwrap() {
        assert_eq!(run(&row[0], px4::calibration_writes), row[1], "{}", row[0]);
    }
}

#[test]
fn ardupilot_pages_and_names_match() {
    let c = &cases("setup_rc_cases.json")["ardupilot"];
    for (n, names) in c["names"].as_object().unwrap() {
        assert_eq!(ap::param_names(n.parse().unwrap()), strings(names), "names {n}");
    }
    for (t, modes) in c["modes"].as_object().unwrap() {
        assert_eq!(
            Value::Array(ap::flight_mode_options(t.parse().unwrap())),
            *modes,
            "modes {t}"
        );
    }
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        let got = ap::build(&values_of(&row[0]), row[1].as_i64().unwrap(), row[2].as_i64().unwrap());
        assert_page(&got, &row[3], &format!("ardupilot case {i}: {}", row[0]));
    }
}

#[test]
fn ardupilot_calibration_matches() {
    let c = &cases("setup_rc_cases.json")["ardupilot"];
    for row in c["calibration"].as_array().unwrap() {
        assert_eq!(run(&row[0], ap::calibration_writes), row[1], "{}", row[0]);
    }
}
