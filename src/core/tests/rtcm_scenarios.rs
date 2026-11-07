//! The Rust RTCM injection plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`): the
//! fragmenting, the sequence, what is dropped and what the page counts.

use std::sync::Mutex;

use corvus_core::link::plain::{Message, Value};
use corvus_core::protocol::rtcm::{fragments, RtcmHost, RtcmInjector};
use serde_json::{json, Value as Json};

struct Fake {
    sc: Json,
    inner: Mutex<(f64, Vec<Json>)>,
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

impl RtcmHost for Fake {
    fn connection_ready(&self) -> bool {
        self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        if self.sc["no_connection"].as_bool().unwrap_or(false) {
            return Ok(false);
        }
        let sent = self.inner.lock().unwrap().1.iter().filter(|e| e[0] == "send").count();
        if self.sc["fail_on"].as_u64().is_some_and(|n| n as usize == sent) {
            return Err("boom".into());
        }
        let int = |name: &str| msg.get(name).and_then(Value::as_i64).unwrap();
        let data: Vec<u8> = match msg.get("data") {
            Some(Value::Array(a)) => a.iter().map(|v| v.as_i64().unwrap() as u8).collect(),
            other => panic!("{other:?}"),
        };
        self.inner
            .lock()
            .unwrap()
            .1
            .push(json!(["send", int("flags"), int("len"), hex(&data)]));
        Ok(true)
    }
    fn now(&self) -> f64 {
        self.inner.lock().unwrap().0
    }
    fn log(&self, level: &str, text: &str) {
        self.inner.lock().unwrap().1.push(json!(["log", level, text]));
    }
}

#[test]
fn every_scenario_ends_where_the_python_injection_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/rtcm_cases.json")).unwrap();
    let mut failures = Vec::new();
    for case in doc["cases"].as_array().unwrap() {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let host = Fake {
            sc: sc.clone(),
            inner: Mutex::new((1000.0, Vec::new())),
        };
        let injector = RtcmInjector::new();
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.inner.lock().unwrap().1.len();
            let result = match step["op"].as_str().unwrap() {
                "fragments" => json!(fragments(
                    &unhex(step["frame"].as_str().unwrap()),
                    step["sequence"].as_u64().unwrap() as u32
                )
                .iter()
                .map(|(f, d)| json!([f, hex(d)]))
                .collect::<Vec<_>>()),
                "inject" => json!(injector.inject_rtcm(&host, &unhex(step["frame"].as_str().unwrap()))),
                "stats" => injector.rtcm_stats(&host),
                "advance" => {
                    host.inner.lock().unwrap().0 += step["seconds"].as_f64().unwrap();
                    Json::Null
                }
                "forget" => {
                    injector.forget_rtcm_session();
                    Json::Null
                }
                other => panic!("{other}"),
            };
            let events = host.inner.lock().unwrap().1[before..].to_vec();
            let want = &case["expected"]["steps"][i];
            if result != want["result"] || json!(events) != want["events"] {
                failures.push(format!(
                    "{name} step {i}: got {result} {}\n  want {} {}",
                    json!(events),
                    want["result"],
                    want["events"]
                ));
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
