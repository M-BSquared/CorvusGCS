//! The Rust requests play the scenarios the Python ones played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`): the
//! stream and interval tables, the fallback decision read from the ACKs, the
//! home and version requests, the GCS authority check and applying a key.

use std::sync::Mutex;

use corvus_core::dialect::{dialect_for_stack, Dialect, Stack};
use corvus_core::link::plain::{Message, Value};
use corvus_core::priority_lock::PriorityLock;
use corvus_core::protocol::requests::{message_intervals, stream_rates, RequestsEngine, RequestsHost};
use serde_json::{json, Map, Value as Json};

struct Inner {
    log: Vec<Json>,
    slow: bool,
    request_sent: bool,
    wire: String,
    counts: std::collections::HashMap<String, usize>,
}

struct Fake {
    sc: Json,
    lock: PriorityLock,
    inner: Mutex<Inner>,
}

impl Fake {
    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }
}

impl RequestsHost for Fake {
    fn is_slow_link(&self) -> bool {
        self.st().slow
    }
    fn has_connection(&self) -> bool {
        !self.sc["no_connection"].as_bool().unwrap_or(false)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        let int = |name: &str| json!(msg.get(name).and_then(Value::as_i64).unwrap());
        let mut args = vec![int("target_system"), int("target_component")];
        let method = match msg.name() {
            "REQUEST_DATA_STREAM" => {
                for name in ["req_stream_id", "req_message_rate", "start_stop"] {
                    args.push(int(name));
                }
                "request_data_stream_send"
            }
            "COMMAND_LONG" => {
                args.push(int("command"));
                args.push(int("confirmation"));
                for n in 1..=7 {
                    args.push(match msg.get(&format!("param{n}")) {
                        Some(Value::Float(v)) if v.is_nan() => json!("nan"),
                        Some(Value::Float(v)) => json!(v),
                        other => panic!("{other:?}"),
                    });
                }
                "command_long_send"
            }
            "AUTOPILOT_VERSION_REQUEST" => "autopilot_version_request_send",
            other => panic!("{other}"),
        };
        let fails = &self.sc["send_fails"];
        if fails.as_str() == Some(method) || fails.as_bool() == Some(true) {
            return Err("boom".into());
        }
        self.st().log.push(json!(["send", method, args]));
        Ok(true)
    }
    fn target(&self) -> (u8, u8) {
        (1, 1)
    }
    fn request_sent(&self) -> bool {
        self.st().request_sent
    }
    fn set_request_sent(&self, sent: bool) {
        self.st().request_sent = sent;
    }
    fn dialect(&self) -> Dialect {
        dialect_for_stack(Stack::from_name(self.sc["stack"].as_str().unwrap_or("px4")))
    }
    fn connection_ready(&self) -> bool {
        self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn lock(&self) -> &PriorityLock {
        &self.lock
    }
    fn set_error(&self, text: &str) {
        self.st().log.push(json!(["error", text]));
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.st().log.push(json!(["console", source, text, level]));
    }
    fn log(&self, level: &str, text: &str) {
        self.st().log.push(json!(["log", level, text]));
    }
    fn store_warning(&self, text: &str, level: &str) {
        self.st().log.push(json!(["warning", text, level]));
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        self.st().log.push(json!(["failure", action, result]));
        false
    }
    fn send_command(&self, command: i64, params: &[f64], timeout: f64, retries: i64) -> i64 {
        let mut s = self.st();
        s.log.push(json!([
            "cmd",
            command,
            params
                .iter()
                .map(|x| if x.is_nan() { json!("nan") } else { json!(x) })
                .collect::<Vec<_>>(),
            timeout,
            retries
        ]));
        let key = if command == 511 {
            (params[0] as i64).to_string()
        } else {
            command.to_string()
        };
        let plan = self.sc["results"].get(&key).unwrap_or(&self.sc["default_result"]);
        match plan {
            Json::Array(list) => {
                let n = s.counts.get(&key).copied().unwrap_or(0);
                s.counts.insert(key, n + 1);
                list[n.min(list.len() - 1)].as_i64().unwrap()
            }
            Json::Null => 0,
            other => other.as_i64().unwrap(),
        }
    }
    fn fetch_params(&self, names: &[&str], timeout: f64) -> Vec<(String, f64)> {
        self.st().log.push(json!(["fetch", names, timeout]));
        self.sc["params"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| names.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
            .collect()
    }
    fn schedule_version_retry(&self) {
        self.st().log.push(json!(["call", "schedule_version_retry"]));
    }
    fn store_update(&self, fields: Map<String, Json>) {
        self.st().log.push(json!(["update", Json::Object(fields)]));
    }
    fn load_signing_key(&self) -> Result<Option<Vec<u8>>, String> {
        let spec = &self.sc["signing"];
        if let Some(message) = spec["message"].as_str() {
            return Err(message.into());
        }
        Ok(spec["key"].as_str().map(|k| {
            (0..k.len() / 2)
                .map(|i| u8::from_str_radix(&k[2 * i..2 * i + 2], 16).unwrap())
                .collect()
        }))
    }
    fn wire_is_mavlink2(&self) -> bool {
        self.st().wire == "2.0"
    }
    fn switch_to_mavlink2(&self) -> Result<(), String> {
        if self.sc["switch_fails"].as_bool().unwrap_or(false) {
            return Err("no v2 dialect".into());
        }
        let mut s = self.st();
        s.log.push(json!(["switch_to_mavlink2", "fd"]));
        s.wire = "2.0".into();
        Ok(())
    }
    fn setup_signing(&self, key: &[u8]) -> Result<(), String> {
        if self.sc["setup_fails"].as_bool().unwrap_or(false) {
            return Err("signing unsupported".into());
        }
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        self.st().log.push(json!(["setup_signing", hex, true, true]));
        Ok(())
    }
    fn conn_string(&self) -> String {
        self.sc["conn_str"].as_str().unwrap_or("udp:127.0.0.1:14550").into()
    }
}

fn pairs(table: Vec<(i64, i64)>) -> Json {
    json!(table.into_iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>())
}

fn run_step(engine: &RequestsEngine, host: &Fake, step: &Json) -> Json {
    match step["op"].as_str().unwrap() {
        "tables" => {
            let slow = step["slow"].as_bool().unwrap();
            host.st().slow = slow;
            json!({"streams": pairs(stream_rates(slow)), "intervals": pairs(message_intervals(slow))})
        }
        "streams" => {
            engine.request_streams(host);
            Json::Null
        }
        "intervals" => {
            engine.request_message_intervals(host);
            Json::Null
        }
        "set_interval" => {
            let (ok, code) = engine.set_message_interval(
                host,
                step["id"].as_i64().unwrap(),
                step["us"].as_i64().unwrap(),
                step["quiet"].as_bool().unwrap(),
            );
            json!([ok, code])
        }
        "home" => {
            engine.request_home(host);
            Json::Null
        }
        "version" => {
            engine.request_version(host);
            Json::Null
        }
        "authority" => {
            engine.check_gcs_authority(host);
            Json::Null
        }
        "signing" => match engine.apply_signing(host) {
            Ok(()) => json!("ok"),
            Err(message) => json!(format!("connection error: {message}")),
        },
        "state" => json!({"request_sent": host.request_sent()}),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_requests_end() {
    let doc: Json = serde_json::from_str(include_str!("data/requests_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 35);
    let mut failures = Vec::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let host = Fake {
            sc: sc.clone(),
            lock: PriorityLock::new(),
            inner: Mutex::new(Inner {
                log: Vec::new(),
                slow: sc["slow"].as_bool().unwrap_or(false),
                request_sent: sc["request_sent"].as_bool().unwrap_or(false),
                wire: sc["wire"].as_str().unwrap_or("2.0").into(),
                counts: Default::default(),
            }),
        };
        let engine = RequestsEngine::new();
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.st().log.len();
            let result = run_step(&engine, &host, step);
            let events = host.st().log[before..].to_vec();
            let want = &case["expected"]["steps"][i];
            if result != want["result"] || json!(events) != want["events"] {
                failures.push(format!(
                    "{name} step {i} ({}): got {result} {}\n  want {} {}",
                    step["op"],
                    serde_json::to_string(&events).unwrap(),
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
