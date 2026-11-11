//! The ArduPilot safety schema builds what `python/corvus/ardupilot_safety.py`
//! builds for the same parameter values and vehicle class
//! (`python/tools/dump_setup_schema_cases.py ardupilot_safety`).

mod setup_common;

use corvus_core::setup_schemas::ardupilot_safety::*;
use serde_json::Value;
use setup_common::*;

#[test]
fn the_page_reads_the_same_parameters() {
    let c = cases("setup_ardupilot_safety_cases.json");
    assert_eq!(param_names(), strings(&c["names"]));
    for (vehicle, names) in c["return_names"].as_object().unwrap() {
        assert_eq!(return_param_names(vehicle), strings(names), "{vehicle}");
    }
}

#[test]
fn pages_equal_the_python_pages() {
    let c = cases("setup_ardupilot_safety_cases.json");
    for (i, row) in c["build"].as_array().unwrap().iter().enumerate() {
        let got = build(
            &values_of(&row[0]),
            &strings(&row[1]),
            row[2].as_str().unwrap_or("copter"),
        );
        assert_page(&got, &row[3], &format!("case {i} ({}): {}", row[2], row[0]));
    }
}

#[test]
fn the_return_profile_and_fence_writes_match() {
    let c = cases("setup_ardupilot_safety_cases.json");
    for row in c["profiles"].as_array().unwrap() {
        assert_eq!(
            return_profile(&values_of(&row[0]), row[1].as_str().unwrap()),
            row[2],
            "{row}"
        );
    }
    for row in c["enable_writes"].as_array().unwrap() {
        assert_eq!(
            Value::Array(geofence_enable_writes(&values_of(&row[0]))),
            row[1],
            "{row}"
        );
    }
}
