//! The Rust field builders answer what `python/corvus/param_fields.py` answers
//! (`python/tools/dump_setup_schema_cases.py fields`).

use corvus_core::setup_schemas::fields::*;
use serde_json::Value;

fn cases() -> Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/setup_fields_cases.json"
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn values_of_case(c: &Value) -> Values {
    c["values"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
        .collect()
}

#[test]
fn builders_match() {
    let c = cases();
    let values = values_of_case(&c);
    let options: Vec<Value> = c["options"].as_array().unwrap().clone();
    for row in c["enum"].as_array().unwrap() {
        let got = enum_(row[0].as_str().unwrap(), "Label", &values, &options)
            .unit("x")
            .hint("h");
        assert_eq!(got.into_value().unwrap_or(Value::Null), row[1], "{row}");
    }
    for row in c["enum_options"].as_array().unwrap() {
        assert_eq!(
            Value::Array(enum_options(&options, row[0].as_f64().unwrap())),
            row[1],
            "{row}"
        );
    }
    for row in c["labels"].as_array().unwrap() {
        assert_eq!(
            option_label(&options, row[0].as_f64().unwrap()),
            row[1].as_str().unwrap(),
            "{row}"
        );
    }
    for row in c["number"].as_array().unwrap() {
        let got = number(row[0].as_str().unwrap(), "Label", &values)
            .unit("m")
            .step(1)
            .min(0)
            .max(1.5)
            .hint("h")
            .kv("value", 9);
        assert_eq!(got.into_value().unwrap_or(Value::Null), row[1], "{row}");
    }
    let bits = vec![
        serde_json::json!({"bit": 0, "label": "One"}),
        serde_json::json!({"bit": 1, "label": "Two"}),
    ];
    for row in c["bitmask"].as_array().unwrap() {
        let got = bitmask(row[0].as_str().unwrap(), "Label", &values, bits.clone()).hint("h");
        assert_eq!(got.into_value().unwrap_or(Value::Null), row[1], "{row}");
    }
    let present = present(vec![
        number("A", "a", &values),
        Field(None),
        number("Z", "z", &values),
        number("B", "b", &values),
    ]);
    assert_eq!(Value::Array(present), c["present"]);
}

#[test]
fn sections_and_the_flow_flag_match() {
    let c = cases();
    let values = values_of_case(&c);
    let fs = present(vec![number("A", "a", &values)]);
    let want = c["section"].as_array().unwrap();
    let wrap = |m: Option<corvus_core::json::Map>| m.map_or(Value::Null, Value::Object);
    assert_eq!(wrap(section("id", "Title", fs.clone(), "")), want[0]);
    assert_eq!(wrap(section("id", "Title", fs, "hint")), want[1]);
    assert_eq!(wrap(section("id", "T", vec![], "")), want[2]);
    for row in c["flags"].as_array().unwrap() {
        let mut sections: Vec<Value> = row[0].as_array().unwrap().clone();
        flag_flow_without_range(&mut sections);
        assert_eq!(Value::Array(sections), row[1], "{row}");
    }
}
