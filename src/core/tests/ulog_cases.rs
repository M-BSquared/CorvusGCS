//! The ULog reader against what `ulog.py` reads from the same bytes.

mod logs_util;

use corvus_core::logs::ulog::{self, Col, Scalar, ULog};
use logs_util::{assert_same, load, unhex};
use serde_json::{json, Map, Value};

fn wire_f64(f: f64) -> Value {
    if f.is_nan() {
        json!("nan")
    } else if f.is_infinite() {
        json!(if f > 0.0 { "inf" } else { "-inf" })
    } else {
        json!(f)
    }
}

fn col_json(c: &Col) -> Value {
    match c {
        Col::Int(v) => Value::Array(v.iter().map(|i| scalar_json(&Scalar::Int(*i))).collect()),
        Col::Bool(v) => json!(v),
        Col::Float(v) => Value::Array(v.iter().map(|f| wire_f64(*f)).collect()),
        Col::Str(v) => json!(v),
        Col::Arr(v) => Value::Array(
            v.iter()
                .map(|r| Value::Array(r.iter().map(|f| wire_f64(*f)).collect()))
                .collect(),
        ),
    }
}

fn scalar_json(s: &Scalar) -> Value {
    match s {
        Scalar::Float(f) => wire_f64(*f),
        other => other.to_json(),
    }
}

fn dump(log: &ULog) -> Value {
    let map = |m: &std::collections::HashMap<String, Scalar>| {
        let mut sorted: Vec<_> = m.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        Value::Object(
            sorted
                .into_iter()
                .map(|(k, v)| (k.clone(), scalar_json(v)))
                .collect::<Map<_, _>>(),
        )
    };
    let mut data = Map::new();
    for (topic, inst) in &log.data {
        let mut i = Map::new();
        for (multi, cols) in inst {
            let mut c = Map::new();
            let mut sorted: Vec<_> = cols.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            for (k, v) in sorted {
                c.insert(k.clone(), col_json(v));
            }
            i.insert(multi.to_string(), Value::Object(c));
        }
        data.insert(topic.clone(), Value::Object(i));
    }
    json!({
        "start_timestamp": log.start_timestamp,
        "truncated": log.truncated,
        "info": map(&log.info),
        "params": map(&log.params),
        "messages": log.messages.iter().map(|m| json!({"t": m.t, "level": m.level, "text": m.text})).collect::<Vec<_>>(),
        "dropouts": log.dropout_ms,
        "data": data,
    })
}

fn sorted_keys(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<_> = m.keys().cloned().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted_keys(&m[&k]))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted_keys).collect()),
        other => other.clone(),
    }
}

#[test]
fn the_reader_agrees_with_the_python_reader() {
    let cases = load("ulog_cases.json");
    let cases = cases.as_array().unwrap();
    assert!(cases.len() > 25);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let blob = unhex(case["blob"].as_str().unwrap());
        let topics: Option<Vec<&str>> = case["topics"]
            .as_array()
            .map(|a| a.iter().map(|t| t.as_str().unwrap()).collect());
        if let Some(max) = case.get("max_bytes") {
            assert!(blob.len() > max.as_u64().unwrap() as usize);
            // The cap is a constant of the module; the case only records the message.
            assert_eq!(case["error"], "log is too large to review");
            continue;
        }
        let got = ulog::read(&blob, topics.as_deref());
        match (&case.get("error"), got) {
            (Some(want), Err(e)) => assert_eq!(want.as_str().unwrap(), e.0, "{name}"),
            (Some(want), Ok(_)) => panic!("{name}: Python refused ({want}), Rust read it"),
            (None, Err(e)) => panic!("{name}: Rust refused: {e}"),
            (None, Ok(log)) => assert_same(name, &sorted_keys(&case["log"]), &sorted_keys(&dump(&log)), 0.0),
        }
    }
}
