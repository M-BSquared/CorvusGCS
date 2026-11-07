//! The Rust shell engine plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`) and must do the same.

use std::sync::{Arc, Mutex};

use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::protocol::shell::{ShellEngine, ShellHost, ShellLimits, SubId};
use corvus_core::protocol::RequestHost;
use serde_json::{json, Value as Json};

struct Fake {
    sc: Json,
    sent: Mutex<Vec<Json>>,
    errors: Mutex<Vec<String>>,
    console: Mutex<Vec<Json>>,
    failures: Mutex<Vec<Json>>,
    entries: Arc<Mutex<Vec<Json>>>,
    last: Mutex<String>,
}

impl RequestHost for Fake {
    fn connection_ready(&self) -> bool {
        self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn armed(&self) -> bool {
        false
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        assert_eq!(msg.name(), "SERIAL_CONTROL");
        if self.sc["send_fails"].as_str() == Some("serial_control_send") {
            return Err("boom".into());
        }
        let int = |f: &str| msg.get(f).and_then(Value::as_i64).unwrap();
        let data: String = match msg.get("data") {
            Some(Value::Array(a)) => a.iter().map(|v| format!("{:02x}", v.as_i64().unwrap())).collect(),
            _ => panic!(),
        };
        let mut args = vec![
            json!(int("device")),
            json!(int("flags")),
            json!(int("timeout")),
            json!(int("baudrate")),
            json!(int("count")),
            json!(data),
        ];
        if msg.wire() == Wire::V2 {
            args.push(json!(int("target_system")));
            args.push(json!(int("target_component")));
        }
        self.sent.lock().unwrap().push(json!(["serial_control_send", args]));
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

impl ShellHost for Fake {
    fn supports_shell(&self) -> bool {
        self.sc["supports_shell"].as_bool().unwrap_or(true)
    }
    fn label(&self) -> String {
        self.sc["label"].as_str().unwrap_or("PX4").into()
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        self.failures.lock().unwrap().push(json!([action, result]));
        false
    }
    fn ack_is_for_us(&self, _: &Message) -> bool {
        self.sc["ack_ours"].as_bool().unwrap_or(true)
    }
    fn wire(&self) -> Wire {
        Wire::V1
    }
}

fn serial(device: i64, count: i64, data: &[u8]) -> Message {
    let mut padded = data.to_vec();
    padded.resize(70, 0);
    Message::named("SERIAL_CONTROL", Wire::V1)
        .unwrap()
        .with("device", Value::Int(device as i128))
        .with("count", Value::Int(count as i128))
        .with(
            "data",
            Value::Array(padded.iter().map(|b| Value::Int(*b as i128)).collect()),
        )
}

#[test]
fn every_scenario_ends_where_the_python_engine_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/shell_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 19);
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let lim = &sc["limits"];
        let engine = ShellEngine::new(ShellLimits {
            line_max_chars: lim["line_max"].as_u64().unwrap_or(8192) as usize,
            backlog_chars: lim["backlog"].as_u64().unwrap_or(65536) as usize,
            write_max_chars: lim["write_max"].as_u64().unwrap_or(4096) as usize,
        });
        let host = Fake {
            sc: sc.clone(),
            sent: Mutex::default(),
            errors: Mutex::default(),
            console: Mutex::default(),
            failures: Mutex::default(),
            entries: Arc::default(),
            last: Mutex::default(),
        };
        let mut subs: Vec<SubId> = Vec::new();
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = (
                host.sent.lock().unwrap().len(),
                host.errors.lock().unwrap().len(),
                host.console.lock().unwrap().len(),
                host.failures.lock().unwrap().len(),
                host.entries.lock().unwrap().len(),
            );
            let result = match step["op"].as_str().unwrap() {
                "command" => json!(engine.send_command(&host, step["text"].as_str().unwrap())),
                "write" => json!(engine.write(&host, step["text"].as_str().unwrap())),
                "recv" => {
                    let b = step["data"].as_str().unwrap().as_bytes();
                    engine.handle(&host, &serial(step["device"].as_i64().unwrap(), b.len() as i64, b));
                    Json::Null
                }
                "recv_bytes" => {
                    let b: Vec<u8> = step["data"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap() as u8)
                        .collect();
                    engine.handle(&host, &serial(step["device"].as_i64().unwrap(), b.len() as i64, &b));
                    Json::Null
                }
                "recv_count" => {
                    engine.handle(
                        &host,
                        &serial(
                            step["device"].as_i64().unwrap(),
                            step["count"].as_i64().unwrap(),
                            b"abc",
                        ),
                    );
                    Json::Null
                }
                "add_sub" => {
                    let idx = subs.len();
                    let entries = host.entries.clone();
                    let (id, backlog) = engine.add_sub(move |t| entries.lock().unwrap().push(json!([idx, t])));
                    subs.push(id);
                    json!(backlog)
                }
                "remove_sub" => {
                    engine.remove_sub(subs[step["which"].as_u64().unwrap() as usize]);
                    Json::Null
                }
                "stop" => {
                    engine.stop(&host);
                    Json::Null
                }
                "reset" => {
                    engine.reset();
                    Json::Null
                }
                "state" => {
                    json!({"active": engine.active(), "raw": engine.raw(), "buffer": engine.buffer(), "backlog": engine.backlog()})
                }
                other => panic!("{other}"),
            };
            let want = &case["expected"]["steps"][i];
            let tail = |v: &Mutex<Vec<Json>>, from: usize| Json::Array(v.lock().unwrap()[from..].to_vec());
            assert_eq!(result, want["result"], "{name} step {i}: result");
            assert_eq!(tail(&host.sent, before.0), want["sent"], "{name} step {i}: sent");
            assert_eq!(
                json!(host.errors.lock().unwrap()[before.1..].to_vec()),
                want["errors"],
                "{name} step {i}: errors"
            );
            assert_eq!(
                tail(&host.console, before.2),
                want["console"],
                "{name} step {i}: console"
            );
            assert_eq!(
                tail(&host.failures, before.3),
                want["failures"],
                "{name} step {i}: failures"
            );
            assert_eq!(
                tail(&host.entries.lock().unwrap().clone().into(), 0)
                    .as_array()
                    .unwrap()[before.4..],
                *want["entries"].as_array().unwrap(),
                "{name} step {i}: entries"
            );
        }
        let fin = &case["expected"]["final"];
        assert_eq!(json!(engine.active()), fin["active"], "{name}");
        assert_eq!(json!(engine.raw()), fin["raw"], "{name}");
        assert_eq!(json!(engine.buffer()), fin["buffer"], "{name}");
        assert_eq!(json!(engine.backlog()), fin["backlog"], "{name}");
        assert_eq!(json!(host.last_error()), fin["last_error"], "{name}");
    }
}

#[test]
fn a_mavlink_2_link_addresses_the_vehicle_where_the_dialect_can() {
    struct V2(Fake);
    impl RequestHost for V2 {
        fn connection_ready(&self) -> bool {
            true
        }
        fn armed(&self) -> bool {
            false
        }
        fn send(&self, msg: Message) -> Result<bool, String> {
            if let Some(t) = msg.get("target_system") {
                assert_eq!(t, &Value::Int(4));
                assert_eq!(msg.get("target_component"), Some(&Value::Int(5)));
            }
            self.0.sent.lock().unwrap().push(json!(msg.name()));
            Ok(true)
        }
        fn target(&self) -> (u8, u8) {
            (4, 5)
        }
        fn set_error(&self, t: &str) {
            if !t.is_empty() {
                eprintln!("ERR {t}");
            }
        }
        fn last_error(&self) -> String {
            String::new()
        }
        fn console(&self, _: &str, _: &str, _: &str) {}
    }
    impl ShellHost for V2 {
        fn supports_shell(&self) -> bool {
            true
        }
        fn label(&self) -> String {
            "PX4".into()
        }
        fn command_failure(&self, _: &str, _: i64) -> bool {
            false
        }
        fn ack_is_for_us(&self, _: &Message) -> bool {
            true
        }
    }
    let host = V2(Fake {
        sc: json!({}),
        sent: Mutex::default(),
        errors: Mutex::default(),
        console: Mutex::default(),
        failures: Mutex::default(),
        entries: Arc::default(),
        last: Mutex::default(),
    });
    assert!(ShellEngine::new(ShellLimits::default()).send_command(&host, "ls"));
    assert_eq!(host.0.sent.lock().unwrap().len(), 1);
}
