//! The battery schemas (PX4 and ArduPilot) build what `battery_config.py` and
//! `ardupilot_battery.py` build for the same parameter values
//! (`python/tools/dump_setup_schema_cases.py battery`).

mod setup_common;

use corvus_core::setup_schemas::{ardupilot_battery, battery};
use serde_json::Value;
use setup_common::*;

#[test]
fn px4_pages_equal_the_python_pages() {
    let c = cases("setup_battery_cases.json");
    assert_eq!(battery::param_names(), strings(&c["px4_names"]));
    for (i, row) in c["px4"].as_array().unwrap().iter().enumerate() {
        assert_page(
            &battery::build(&values_of(&row[0])),
            &row[1],
            &format!("px4 case {i}: {}", row[0]),
        );
    }
}

#[test]
fn ardupilot_pages_equal_the_python_pages() {
    let c = cases("setup_battery_cases.json");
    assert_eq!(ardupilot_battery::param_names(), strings(&c["ap_names"]));
    for (i, row) in c["ardupilot"].as_array().unwrap().iter().enumerate() {
        let values = values_of(&row[0]);
        let got = match row[1].as_str() {
            Some(vehicle) => ardupilot_battery::build(&values, vehicle),
            None => ardupilot_battery::build(&values, "copter"),
        };
        assert_page(&got, &row[2], &format!("ardupilot case {i}: {}", row[0]));
    }
    let _ = Value::Null;
}
