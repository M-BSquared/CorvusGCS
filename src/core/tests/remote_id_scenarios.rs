//! The Rust Remote ID engine plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`): the
//! coercion of the identity on a wide set of inputs, the four messages and
//! their timestamps, the arm status and what the page shows.

use std::sync::Mutex;

use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::protocol::remote_id::{arm_status, messages, settings, RemoteIdEngine, RemoteIdHost};
use serde_json::{json, Map, Value as Json};

const UNIX_BASE: f64 = 1_700_000_000.0;

struct Fake {
    sc: Json,
    inner: Mutex<(f64, Vec<Json>)>,
}

fn message_json(msg: &Message, names: &[String]) -> Map<String, Json> {
    let mut out = Map::new();
    for name in names {
        let value = match msg.get(name).unwrap() {
            Value::Int(i) => json!(*i as i64),
            Value::Float(f) => json!(*f),
            Value::Bytes(b) => json!(b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
            Value::Array(a) => json!(a
                .iter()
                .map(|x| format!("{:02x}", x.as_i64().unwrap()))
                .collect::<String>()),
        };
        out.insert(name.clone(), value);
    }
    out
}

fn fields_of(name: &str) -> Vec<String> {
    let common = ["target_system", "target_component", "id_or_mac"];
    let own: &[&str] = match name {
        "OPEN_DRONE_ID_BASIC_ID" => &["id_type", "ua_type", "uas_id"],
        "OPEN_DRONE_ID_OPERATOR_ID" => &["operator_id_type", "operator_id"],
        "OPEN_DRONE_ID_SELF_ID" => &["description_type", "description"],
        "OPEN_DRONE_ID_SYSTEM" => &[
            "operator_location_type",
            "classification_type",
            "operator_latitude",
            "operator_longitude",
            "area_count",
            "area_radius",
            "area_ceiling",
            "area_floor",
            "category_eu",
            "class_eu",
            "operator_altitude_geo",
            "timestamp",
        ],
        other => panic!("{other}"),
    };
    common.iter().chain(own).map(|s| s.to_string()).collect()
}

fn row(msg: &Message) -> Json {
    json!([msg.name().to_lowercase(), message_json(msg, &fields_of(msg.name()))])
}

impl RemoteIdHost for Fake {
    fn link_supports_remote_id(&self) -> bool {
        self.sc["v2"].as_bool().unwrap_or(true)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        if let Some(fails) = self.sc["send_fails"].as_object() {
            if fails["on"].as_str() == Some(&msg.name().to_lowercase()) {
                return Err(fails["message"].as_str().unwrap().into());
            }
        }
        let r = row(&msg);
        self.inner.lock().unwrap().1.push(json!(["send", r[0], r[1]]));
        Ok(true)
    }
    fn target(&self) -> (u8, u8) {
        let t = &self.sc["target"];
        (t[0].as_u64().unwrap_or(1) as u8, t[1].as_u64().unwrap_or(1) as u8)
    }
    fn now(&self) -> f64 {
        self.inner.lock().unwrap().0
    }
    fn unix_now(&self) -> f64 {
        UNIX_BASE + (self.now() - 1000.0)
    }
}

fn run_step(engine: &RemoteIdEngine, host: &Fake, step: &Json) -> Json {
    match step["op"].as_str().unwrap() {
        "settings" => engine.remote_id_settings(),
        "set" => engine.set_remote_id(Some(&step["raw"])),
        "send" => {
            engine.send_remote_id(host);
            Json::Null
        }
        "status" => engine.remote_id_status(host),
        "supported" => json!(engine.remote_id_supported(host)),
        "advance" => {
            host.inner.lock().unwrap().0 += step["seconds"].as_f64().unwrap();
            Json::Null
        }
        "forget" => {
            engine.forget_session();
            Json::Null
        }
        "arm" => {
            let mut error = step["error"].as_str().unwrap().as_bytes().to_vec();
            error.resize(50, 0);
            let msg = Message::named("OPEN_DRONE_ID_ARM_STATUS", Wire::V2)
                .unwrap()
                .with("status", Value::Int(step["status"].as_i64().unwrap() as i128))
                .with("error", Value::Bytes(error));
            engine.handle_arm_status(&msg);
            Json::Null
        }
        "pure_settings" => settings(Some(&step["raw"])).to_json(),
        "pure_messages" => {
            let t = &step["target"];
            let identity = settings(Some(&step["raw"]));
            let target = (t[0].as_u64().unwrap() as u8, t[1].as_u64().unwrap() as u8);
            Json::Array(
                messages(&identity, target, step["unix"].as_f64().unwrap())
                    .iter()
                    .map(row)
                    .collect(),
            )
        }
        "pure_arm" => arm_status(step["status"].as_i64(), step["error"].as_str().unwrap().as_bytes()).to_json(),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_remote_id_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/remote_id_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 18);
    let mut failures = Vec::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let host = Fake {
            sc: sc.clone(),
            inner: Mutex::new((1000.0, Vec::new())),
        };
        let engine = RemoteIdEngine::new();
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.inner.lock().unwrap().1.len();
            let result = run_step(&engine, &host, step);
            let (events, t) = {
                let s = host.inner.lock().unwrap();
                (s.1[before..].to_vec(), s.0)
            };
            let want = &case["expected"]["steps"][i];
            if result != want["result"]
                || json!(events) != want["events"]
                || (t - 1000.0 - want["t"].as_f64().unwrap()).abs() > 1e-6
            {
                failures.push(format!(
                    "{name} step {i} ({}): input {}\n  got {result} {}\n  want {} {}",
                    step["op"],
                    step.get("raw").unwrap_or(&Json::Null),
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
