//! The PX4 safety schema builds what `python/corvus/safety_config.py` builds, for
//! the same parameter values (`python/tools/dump_setup_schema_cases.py safety`):
//! nothing answered, everything answered, and random subsets of what a firmware
//! may carry, each with operator added names.

use corvus_core::setup_schemas::fields::Values;
use corvus_core::setup_schemas::safety::*;
use serde_json::Value;

fn cases() -> Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/setup_safety_cases.json"
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn values_of(v: &Value) -> Values {
    v.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
        .collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_page_reads_the_same_parameters() {
    let c = cases();
    assert_eq!(param_names(), strings(&c["names"]));
    assert_eq!(return_param_names(), strings(&c["return_names"]));
}

#[test]
fn pages_equal_the_python_pages() {
    let c = cases();
    for (i, row) in c["build"].as_array().unwrap().iter().enumerate() {
        let values = values_of(&row[0]);
        let extra = if row[1].is_null() { vec![] } else { strings(&row[1]) };
        let got = build(&values, &extra);
        if got != row[2] {
            let want = &row[2];
            let sections = |v: &Value| v["sections"].as_array().unwrap().clone();
            for (a, b) in sections(&got).iter().zip(sections(want).iter()) {
                assert_eq!(a, b, "case {i}: section {}\nvalues {}", b["id"], row[0]);
            }
            assert_eq!(got, *want, "case {i}: values {}", row[0]);
        }
    }
}

#[test]
fn the_return_profile_and_extra_names_match() {
    let c = cases();
    for row in c["profiles"].as_array().unwrap() {
        assert_eq!(return_profile(&values_of(&row[0])), row[1], "{row}");
    }
    for row in c["normalise"].as_array().unwrap() {
        assert_eq!(normalise_extra(&strings(&row[0])), strings(&row[1]), "{row}");
    }
}

#[test]
fn a_height_that_is_not_a_number_is_none() {
    let values: Values = [("RTL_RETURN_ALT".to_string(), f64::NAN)].into_iter().collect();
    assert_eq!(return_profile(&values)["climb_to"], Value::Null);
}
