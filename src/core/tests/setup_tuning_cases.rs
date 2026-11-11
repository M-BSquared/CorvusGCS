//! The tuning schemas (PX4 and ArduPilot) build what `tuning_config.py` and
//! `ardupilot_tuning.py` build for the same parameter values
//! (`python/tools/dump_setup_schema_cases.py tuning`).

mod setup_common;

use corvus_core::setup_schemas::{ardupilot_tuning as ap, tuning as px4};
use setup_common::*;

#[test]
fn px4_pages_equal_the_python_pages() {
    let c = &cases("setup_tuning_cases.json")["px4"];
    assert_eq!(px4::param_names(), strings(&c["names"]));
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        assert_page(
            &px4::build(&values_of(&row[0])),
            &row[1],
            &format!("px4 case {i}: {}", row[0]),
        );
    }
}

#[test]
fn ardupilot_pages_equal_the_python_pages() {
    let c = &cases("setup_tuning_cases.json")["ardupilot"];
    assert_eq!(ap::param_names(), strings(&c["names"]));
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        let got = ap::build(&values_of(&row[0]), row[1].as_str().unwrap_or("copter"));
        assert_page(&got, &row[2], &format!("ardupilot case {i} ({}): {}", row[1], row[0]));
    }
}
