//! The Rust battery estimator answers what `python/corvus/battery.py` answers
//! on a grid of settings, voltages, currents and drain series
//! (`python/tools/dump_core_cases.py battery`).

use corvus_core::battery::*;
use serde_json::{json, Value};

fn cases() -> Value {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/battery_cases.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn num(v: &Value) -> f64 {
    match v {
        Value::String(s) if s == "nan" => f64::NAN,
        Value::String(s) if s == "inf" => f64::INFINITY,
        other => other.as_f64().unwrap(),
    }
}

fn raw_of(v: &Value) -> Option<&Value> {
    (!v.is_null()).then_some(v)
}

#[test]
fn chemistries_and_their_picker() {
    let c = cases();
    for (name, label) in c["chemistry"].as_object().unwrap() {
        assert_eq!(chemistry(name).label, label.as_str().unwrap(), "{name:?}");
    }
    assert_eq!(Value::Array(chemistry_options()), c["options"]);
}

#[test]
fn settings_are_coerced_and_resolved_like_python() {
    let c = cases();
    for row in c["settings"].as_array().unwrap() {
        let raw = &row[0];
        let coerced = coerce_settings(raw).map_or(Value::Null, Value::Object);
        assert_eq!(coerced, row[1], "coerce {raw}");
        assert_eq!(settings(raw_of(raw)).to_json(), row[2], "settings {raw}");
    }
}

#[test]
fn estimates_cells_rest_and_charge_match() {
    let c = cases();
    for row in c["estimates"].as_array().unwrap() {
        let got = estimate(num(&row[1]), num(&row[2]), raw_of(&row[0])).to_json();
        assert_eq!(got, row[3], "estimate {} {} {}", row[0], row[1], row[2]);
    }
    for row in c["cells"].as_array().unwrap() {
        let got = resolve_cells(num(&row[1]), &settings(raw_of(&row[0])));
        assert_eq!(json!(got), row[2], "cells {} {}", row[0], row[1]);
    }
    for row in c["rest"].as_array().unwrap() {
        let got = rest_voltage(
            num(&row[1]),
            num(&row[2]),
            row[3].as_i64().unwrap(),
            &settings(raw_of(&row[0])),
        );
        assert_eq!(json!(got), row[4], "rest {row}");
    }
    for row in c["soc"].as_array().unwrap() {
        let got = state_of_charge(num(&row[1]), &settings(raw_of(&row[0])));
        assert_eq!(json!(got), row[2], "soc {row}");
    }
}

#[test]
fn the_drain_fit_matches() {
    let c = cases();
    for row in c["drain"].as_array().unwrap() {
        let samples: Vec<(f64, f64)> = row[1]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| {
                let pair = item.as_array()?;
                let t = pair.first().and_then(as_float)?;
                let p = pair.get(1).and_then(as_float)?;
                Some((t, p))
            })
            .collect();
        assert_eq!(json!(drain_endurance(&samples)), row[2], "drain {}", row[0]);
    }
}

fn as_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if s == "nan" => Some(f64::NAN),
        _ => None,
    }
}
