//! Helpers the log and firmware tests share.
#![allow(dead_code)]

use serde_json::Value;

pub fn data_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

pub fn load(name: &str) -> Value {
    let text = std::fs::read_to_string(data_path(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// Differences between the Python answer and the Rust one as parsed JSON.
/// Numbers are equal when the ints are, or the floats are within `tol`
/// (relative above 1, absolute below); `tol` 0 means bit for bit.
pub fn diff(py: &Value, rs: &Value, tol: f64) -> Vec<String> {
    let mut out = Vec::new();
    walk(py, rs, tol, String::new(), &mut out);
    out
}

fn walk(py: &Value, rs: &Value, tol: f64, path: String, out: &mut Vec<String>) {
    if out.len() > 40 {
        return;
    }
    match (py, rs) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in a {
                match b.get(k) {
                    Some(w) => walk(v, w, tol, format!("{path}.{k}"), out),
                    None => out.push(format!("{path}.{k}: missing in Rust")),
                }
            }
            for k in b.keys() {
                if !a.contains_key(k) {
                    out.push(format!("{path}.{k}: only in Rust"));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!("{path}: length {} != {}", a.len(), b.len()));
                return;
            }
            for (i, (v, w)) in a.iter().zip(b).enumerate() {
                walk(v, w, tol, format!("{path}[{i}]"), out);
            }
        }
        (Value::Number(a), Value::Number(b)) => {
            let same = if a.is_f64() || b.is_f64() {
                let (x, y) = (a.as_f64().unwrap(), b.as_f64().unwrap());
                x == y || (x - y).abs() <= tol * x.abs().max(1.0)
            } else {
                a == b
            };
            if !same {
                out.push(format!("{path}: {a} != {b}"));
            }
        }
        _ => {
            if py != rs {
                out.push(format!("{path}: {py} != {rs}"));
            }
        }
    }
}

pub fn assert_same(what: &str, py: &Value, rs: &Value, tol: f64) {
    let d = diff(py, rs, tol);
    assert!(d.is_empty(), "{what}: {} differences:\n{}", d.len(), d.join("\n"));
}
