//! The Rust log engine plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`) and must do the same.

use std::sync::Mutex;

use corvus_core::link::plain::{Message, Value};
use corvus_core::protocol::logs::LogEngine;
use corvus_core::protocol::RequestHost;
use serde_json::{json, Value as Json};

struct Fake {
    sc: Json,
    sent: Mutex<Vec<Json>>,
    errors: Mutex<Vec<String>>,
    console: Mutex<Vec<Json>>,
    last: Mutex<String>,
}

fn int(m: &Message, f: &str) -> i64 {
    m.get(f).and_then(Value::as_i64).unwrap()
}

impl RequestHost for Fake {
    fn connection_ready(&self) -> bool {
        self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn armed(&self) -> bool {
        self.sc["armed"].as_bool().unwrap_or(false)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        let (method, args): (&str, Vec<Json>) = match msg.name() {
            "LOG_REQUEST_LIST" => (
                "log_request_list_send",
                vec![json!(int(&msg, "start")), json!(int(&msg, "end"))],
            ),
            "LOG_REQUEST_DATA" => (
                "log_request_data_send",
                vec![
                    json!(int(&msg, "id")),
                    json!(int(&msg, "ofs")),
                    json!(int(&msg, "count")),
                ],
            ),
            "LOG_ERASE" => ("log_erase_send", vec![]),
            "LOG_REQUEST_END" => ("log_request_end_send", vec![]),
            other => panic!("{other}"),
        };
        assert_eq!((int(&msg, "target_system"), int(&msg, "target_component")), (1, 1));
        if self.sc["send_fails"].as_str() == Some(method) {
            return Err("boom".into());
        }
        self.sent.lock().unwrap().push(json!([method, args]));
        Ok(true)
    }
    fn target(&self) -> (u8, u8) {
        (1, 1)
    }
    fn set_error(&self, text: &str) {
        *self.last.lock().unwrap() = text.into();
        self.errors.lock().unwrap().push(text.into());
    }
    fn last_error(&self) -> String {
        self.last.lock().unwrap().clone()
    }
    fn console(&self, s: &str, t: &str, l: &str) {
        self.console.lock().unwrap().push(json!([s, t, l]));
    }
}

#[test]
fn every_scenario_ends_where_the_python_engine_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/logs_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 13);
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let host = Fake {
            sc: sc.clone(),
            sent: Mutex::default(),
            errors: Mutex::default(),
            console: Mutex::default(),
            last: Mutex::default(),
        };
        let engine = LogEngine::new();
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = (
                host.sent.lock().unwrap().len(),
                host.errors.lock().unwrap().len(),
                host.console.lock().unwrap().len(),
            );
            let result = match step["op"].as_str().unwrap() {
                "list" => engine.request_list(&host, step["start"].as_i64().unwrap(), step["end"].as_i64().unwrap()),
                "data" => engine.request_data(
                    &host,
                    step["id"].as_i64().unwrap(),
                    step["offset"].as_i64().unwrap(),
                    step["count"].as_i64().unwrap(),
                ),
                "erase" => engine.erase(&host),
                "end" => engine.request_end(&host),
                other => panic!("{other}"),
            };
            let want = &case["expected"]["steps"][i];
            assert_eq!(json!(result), want["result"], "{name} step {i}");
            assert_eq!(
                Json::Array(host.sent.lock().unwrap()[before.0..].to_vec()),
                want["sent"],
                "{name} step {i}: sent"
            );
            assert_eq!(
                json!(host.errors.lock().unwrap()[before.1..].to_vec()),
                want["errors"],
                "{name} step {i}: errors"
            );
            assert_eq!(
                Json::Array(host.console.lock().unwrap()[before.2..].to_vec()),
                want["console"],
                "{name} step {i}: console"
            );
        }
        assert_eq!(
            json!(host.last_error()),
            case["expected"]["final"]["last_error"],
            "{name}"
        );
    }
}
