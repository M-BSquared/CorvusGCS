//! Helpers shared by the setup schema tests: fixtures dumped from the Python
//! (`python/tools/dump_setup_schema_cases.py`) and their inputs.
#![allow(dead_code)]

use corvus_core::setup_schemas::fields::Values;
use serde_json::Value;

pub fn cases(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

pub fn values_of(v: &Value) -> Values {
    v.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
        .collect()
}

pub fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// The path and the two values where `a` and `b` first differ.
pub fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x {
                match y.get(k) {
                    None => return Some(format!("{path}/{k}: only here: {v}")),
                    Some(w) => {
                        if let Some(d) = first_diff(v, w, &format!("{path}/{k}")) {
                            return Some(d);
                        }
                    }
                }
            }
            y.keys()
                .find(|k| !x.contains_key(*k))
                .map(|k| format!("{path}/{k}: only in Python: {}", y[k]))
        }
        (Value::Array(x), Value::Array(y)) => {
            for (i, (v, w)) in x.iter().zip(y.iter()).enumerate() {
                if let Some(d) = first_diff(v, w, &format!("{path}[{i}]")) {
                    return Some(d);
                }
            }
            (x.len() != y.len()).then(|| format!("{path}: {} items here, {} in Python", x.len(), y.len()))
        }
        _ => (a != b).then(|| format!("{path}: {a} here, {b} in Python")),
    }
}

/// Fails with the first difference between a built page and the Python one.
pub fn assert_page(got: &Value, want: &Value, what: &str) {
    if let Some(d) = first_diff(got, want, "") {
        panic!("{what}\n  {d}");
    }
}
