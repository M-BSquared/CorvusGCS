//! The mounting schemas (PX4 and ArduPilot) build what `mounting_config.py` and
//! `ardupilot_mounting.py` build for the same parameter values
//! (`python/tools/dump_setup_schema_cases.py mounting`).

mod setup_common;

use corvus_core::setup_schemas::{ardupilot_mounting as ap, mounting as px4};
use serde_json::Value;
use setup_common::*;

#[test]
fn px4_pages_equal_the_python_pages() {
    let c = &cases("setup_mounting_cases.json")["px4"];
    assert_eq!(px4::param_names(), strings(&c["names"]));
    assert_eq!(px4::orientation_param_names(), strings(&c["orientation_names"]));
    assert_eq!(px4::position_param_names(), strings(&c["position_names"]));
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        let values = values_of(&row[0]);
        let got = px4::build(&values);
        assert_page(&got, &row[1], &format!("px4 case {i}: {}", row[0]));
        assert_eq!(
            Value::Array(px4::positions(&values)),
            row[1]["positions"],
            "positions {i}"
        );
    }
}

#[test]
fn ardupilot_pages_equal_the_python_pages() {
    let c = &cases("setup_mounting_cases.json")["ardupilot"];
    assert_eq!(ap::param_names(), strings(&c["names"]));
    assert_eq!(ap::orientation_param_names(), strings(&c["orientation_names"]));
    assert_eq!(ap::position_param_names(), strings(&c["position_names"]));
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        let got = ap::build(&values_of(&row[0]));
        assert_page(&got, &row[1], &format!("ardupilot case {i}: {}", row[0]));
    }
}

#[test]
fn each_px4_release_is_built_as_python_builds_it() {
    let versions = cases("setup_mounting_cases.json")["versions"].clone();
    let names: Vec<&str> = versions.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(names, ["PX4 v1.16", "PX4 v1.17", "PX4 v1.18", "PX4 v1.18 migrated"]);
    for (name, row) in versions.as_object().unwrap() {
        let values = values_of(&row[0]);
        assert_page(&px4::build(&values), &row[1], name);
        assert_eq!(
            Value::Array(px4::positions(&values)),
            row[1]["positions"],
            "{name} positions"
        );
    }
}
