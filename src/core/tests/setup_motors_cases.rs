//! The motor schemas (PX4 and ArduPilot) build what `motor_config.py` and
//! `ardupilot_motors.py` build for the same parameter values
//! (`python/tools/dump_setup_schema_cases.py motors`).

mod setup_common;

use corvus_core::setup_schemas::{ardupilot_motors as ap, motors as px4};
use serde_json::Value;
use setup_common::*;

#[test]
fn px4_pages_equal_the_python_pages() {
    let c = &cases("setup_motors_cases.json")["px4"];
    assert_eq!(px4::param_names(), strings(&c["names"]));
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        let values = values_of(&row[0]);
        assert_page(&px4::build(&values), &row[1], &format!("px4 case {i}: {}", row[0]));
        assert_eq!(px4::output_param_names(&values), strings(&row[2]), "output names {i}");
    }
}

#[test]
fn px4_output_helpers_match() {
    let c = &cases("setup_motors_cases.json")["px4"];
    for row in c["labels"].as_array().unwrap() {
        let v = row[0].as_f64().unwrap();
        assert_eq!(px4::function_label(v), row[1].as_str().unwrap(), "{row}");
        assert_eq!(px4::motor_of(v).map_or(Value::Null, |m| m.into()), row[2], "{row}");
        assert_eq!(
            px4::motor_function_value(v as i64).map_or(Value::Null, Value::from),
            row[3],
            "{row}"
        );
    }
    for row in c["function_params"].as_array().unwrap() {
        let got = px4::function_param(row[0].as_str().unwrap(), row[1].as_i64().unwrap());
        assert_eq!(got.map_or(Value::Null, Value::from), row[2], "{row}");
    }
    for row in c["motor_values"].as_array().unwrap() {
        let got = px4::motor_function_value(row[0].as_i64().unwrap());
        assert_eq!(got.map_or(Value::Null, Value::from), row[1], "{row}");
    }
}

#[test]
fn ardupilot_pages_equal_the_python_pages() {
    let c = &cases("setup_motors_cases.json")["ardupilot"];
    assert_eq!(ap::param_names(), strings(&c["names"]));
    for (i, row) in c["cases"].as_array().unwrap().iter().enumerate() {
        assert_page(
            &ap::build(&values_of(&row[0])),
            &row[1],
            &format!("ardupilot case {i}: {}", row[0]),
        );
    }
    for row in c["labels"].as_array().unwrap() {
        let v = row[0].as_f64().unwrap();
        assert_eq!(ap::function_label(v), row[1].as_str().unwrap(), "{row}");
        assert_eq!(ap::motor_of(v).map_or(Value::Null, |m| m.into()), row[2], "{row}");
    }
    for row in c["function_params"].as_array().unwrap() {
        let got = ap::function_param(row[0].as_str().unwrap(), row[1].as_i64().unwrap());
        assert_eq!(got.map_or(Value::Null, Value::from), row[2], "{row}");
    }
    for row in c["motor_values"].as_array().unwrap() {
        let got = ap::motor_function_value(row[0].as_i64().unwrap());
        assert_eq!(got.map_or(Value::Null, Value::from), row[1], "{row}");
    }
}
