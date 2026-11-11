//! First setup answers what `python/corvus/first_setup.py` answers
//! (`python/tools/dump_setup_schema_cases.py first_setup`).

mod setup_common;

use corvus_core::setup_schemas::first_setup::*;
use serde_json::{json, Value};
use setup_common::*;

#[test]
fn assessments_match() {
    let c = cases("setup_first_cases.json");
    for (stack, names) in c["names"].as_object().unwrap() {
        assert_eq!(param_names(stack), strings(names), "names {stack:?}");
    }
    for row in c["cases"].as_array().unwrap() {
        let got = assess(
            row[0].as_str().unwrap(),
            row[1].as_str().unwrap(),
            row[2].as_str().unwrap(),
            &values_of(&row[3]),
        );
        assert_page(&got, &row[4], &format!("{} {} {}: {}", row[0], row[1], row[2], row[3]));
    }
}

#[test]
fn presets_and_writes_match() {
    let c = cases("setup_first_cases.json");
    for row in c["presets"].as_array().unwrap() {
        assert_eq!(
            Value::Array(presets(row[0].as_str().unwrap(), row[1].as_str().unwrap())),
            row[2],
            "{row}"
        );
    }
    for row in c["writes"].as_array().unwrap() {
        let got = writes_for(
            row[0].as_str().unwrap(),
            row[1].as_str().unwrap(),
            row[2].as_i64().unwrap(),
        )
        .map_or(Value::Null, |w| {
            Value::Array(w.into_iter().map(|(n, v)| json!([n, v])).collect())
        });
        assert_eq!(got, row[3], "{row}");
    }
    for row in c["types"].as_array().unwrap() {
        assert_eq!(
            vehicle_for_mav_type(row[0].as_i64().unwrap()),
            row[1].as_str().unwrap(),
            "{row}"
        );
    }
}
