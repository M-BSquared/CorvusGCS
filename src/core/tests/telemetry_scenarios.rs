//! The Rust telemetry engine plays the scenarios the Python telemetry code
//! played (`python/tools/dump_protocol_cases.py`, run on `tests/reference`)
//! and must do the same: every store update, error and stream request, and
//! what the engine remembers afterwards.

use std::sync::Mutex;

use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::priority_lock::PriorityLock;
use corvus_core::protocol::telemetry::{TelemetryEngine, TelemetryHost};
use serde_json::{json, Map, Value as Json};

struct Inner {
    log: Vec<Json>,
    rc: Vec<i64>,
    last_error: String,
}

struct Fake {
    sc: Json,
    lock: PriorityLock,
    inner: Mutex<Inner>,
}

impl Fake {
    fn new(sc: &Json) -> Fake {
        Fake {
            sc: sc.clone(),
            lock: PriorityLock::new(),
            inner: Mutex::new(Inner {
                log: Vec::new(),
                rc: sc["rc"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| v.as_i64().unwrap())
                    .collect(),
                last_error: String::new(),
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }
}

impl TelemetryHost for Fake {
    fn store_update(&self, fields: Map<String, Json>) {
        let mut s = self.st();
        if let Some(channels) = fields.get("rc_channels") {
            s.rc = channels
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect();
        }
        s.log.push(json!(["update", Json::Object(fields)]));
    }
    fn rc_channels(&self) -> Vec<i64> {
        self.st().rc.clone()
    }
    fn target_system(&self) -> i64 {
        self.sc["target_system"].as_i64().unwrap_or(1)
    }
    fn lock(&self) -> &PriorityLock {
        &self.lock
    }
    fn set_error(&self, text: &str) {
        let mut s = self.st();
        s.last_error = text.into();
        s.log.push(json!(["error", text]));
    }
    fn connection_ready(&self) -> bool {
        self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn set_message_interval(&self, msg_id: i64, interval_us: i64) -> bool {
        self.st().log.push(json!(["interval", msg_id, interval_us]));
        self.sc["interval_results"]
            .get(msg_id.to_string())
            .and_then(Json::as_bool)
            .unwrap_or(true)
    }
    fn message_interval(&self, msg_id: i64) -> i64 {
        self.sc["intervals"]
            .get(msg_id.to_string())
            .and_then(Json::as_i64)
            .unwrap_or(0)
    }
    fn is_slow_link(&self) -> bool {
        self.sc["slow_link"].as_bool().unwrap_or(false)
    }
    fn forget_rangefinder(&self) {
        self.st().log.push(json!(["forget_rangefinder", Json::Null]));
    }
}

fn build(name: &str, fields: &Json) -> Message {
    let mut msg = Message::named(name, Wire::V2).unwrap();
    for (key, value) in fields.as_object().unwrap() {
        let float = match msg.get(key).unwrap() {
            Value::Float(_) => true,
            Value::Array(items) => matches!(items.first(), Some(Value::Float(_))),
            _ => false,
        };
        let one = |v: &Json| {
            if float {
                Value::Float(v.as_f64().unwrap())
            } else {
                Value::Int(v.as_i64().unwrap() as i128)
            }
        };
        let converted = match value {
            Json::Array(items) => Value::Array(items.iter().map(one).collect()),
            single => one(single),
        };
        msg.set(key, converted).unwrap();
    }
    msg
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn state_of(engine: &TelemetryEngine) -> Json {
    json!({
        "hb": engine.hb_times(),
        "radio_seen": engine.radio_status_seen(),
        "uplink": engine.uplink_score(),
        "cells": engine.battery_cells(),
        "drain": engine.drain_samples().iter().map(|(t, p)| json!([t, p])).collect::<Vec<_>>(),
        "source": engine.drain_source(),
    })
}

fn run_step(engine: &TelemetryEngine, host: &Fake, step: &Json) -> Json {
    let num = |k: &str| step[k].as_f64().unwrap();
    let flag = |k: &str| step[k].as_bool().unwrap();
    match step["op"].as_str().unwrap() {
        "battery" => engine.battery_settings(),
        "set_battery" => engine.set_battery_settings((!step["raw"].is_null()).then_some(&step["raw"])),
        "forget_cells" => {
            engine.forget_battery_cells();
            Json::Null
        }
        "fields" => Json::Object(engine.battery_fields(num("v"), num("a"), step["reported"].as_i64().unwrap())),
        "endurance" => Json::Object(engine.battery_endurance_fields(
            step["percent"].as_f64(),
            step["source"].as_str().unwrap(),
            flag("armed"),
            num("now"),
        )),
        "battery_status" => {
            engine.handle_battery_status(host, &build("BATTERY_STATUS", &step["fields"]));
            Json::Null
        }
        "gnss" => {
            engine.handle_gnss_integrity(
                host,
                &build("GNSS_INTEGRITY", &step["fields"]),
                step["system"].as_i64().unwrap(),
            );
            Json::Null
        }
        "gnss_frame" => {
            engine.handle_gnss_integrity_frame(host, &hex(step["frame"].as_str().unwrap()));
            Json::Null
        }
        "forget_gnss" => {
            engine.forget_gnss_integrity(host);
            Json::Null
        }
        "att" => {
            engine.handle_attitude_target(host, &build("ATTITUDE_TARGET", &step["fields"]));
            Json::Null
        }
        "pos" => {
            engine.handle_position_target(host, &build("POSITION_TARGET_LOCAL_NED", &step["fields"]));
            Json::Null
        }
        "rc" => {
            engine.handle_rc_channels(host, &build("RC_CHANNELS", &step["fields"]), false);
            Json::Null
        }
        "rc_raw" => {
            engine.handle_rc_channels(host, &build("RC_CHANNELS_RAW", &step["fields"]), true);
            Json::Null
        }
        "radio" => {
            engine.handle_radio_status(host, &build("RADIO_STATUS", &step["fields"]));
            Json::Null
        }
        "hb" => {
            engine.note_heartbeat(num("t"));
            Json::Null
        }
        "forget_link" => {
            engine.forget_link();
            Json::Null
        }
        "jitter" => {
            engine.publish_heartbeat_jitter(host);
            Json::Null
        }
        "current_jitter" => json!(engine.current_jitter_ms()),
        "link_quality" => {
            engine.publish_link_quality(host);
            Json::Null
        }
        "vibration" => json!(engine.vibration_stream(host, flag("enabled"), step["rate"].as_i64().unwrap())),
        "rangefinder" => json!(engine.rangefinder_stream(host, flag("enabled"))),
        "rc_stream" => json!(engine.rc_stream(host, flag("enabled"), step["rate"].as_i64().unwrap())),
        "tuning" => json!(engine.tuning_stream(host, flag("enabled"), step["rate"].as_i64().unwrap())),
        "interval" => json!(engine.stream_interval_us(host, step["id"].as_i64().unwrap(), step["requested"].as_i64())),
        "state" => state_of(engine),
        "state_len" => json!(engine.drain_samples().len()),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_telemetry_code_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/telemetry_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 30);
    let mut failures = Vec::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let host = Fake::new(sc);
        let engine = TelemetryEngine::new();
        let expected = &case["expected"];
        let mut bad = false;
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.st().log.len();
            let result = run_step(&engine, &host, step);
            let events = host.st().log[before..].to_vec();
            let want = &expected["steps"][i];
            if result != want["result"] || json!(events) != want["events"] {
                failures.push(format!(
                    "{name} step {i} ({}): result {result} (want {}), events:\n{}\nwant:\n{}",
                    step["op"],
                    want["result"],
                    serde_json::to_string(&events).unwrap(),
                    want["events"]
                ));
                bad = true;
                break;
            }
        }
        if !bad {
            let s = host.st();
            let got = json!({"last_error": s.last_error, "rc": s.rc});
            if got != expected["final"] {
                failures.push(format!("{name}: final {got} want {}", expected["final"]));
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
