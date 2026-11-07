//! The Rust status engine plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`):
//! STATUSTEXT reassembly and publishing, the kill switch from its own line, a
//! failing check or a pass, and the preflight report requests.

use std::sync::Mutex;

use corvus_core::dialect::{dialect_for_stack, Dialect, Stack};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::protocol::status::{StatusEngine, StatusHost, StatusText};
use serde_json::{json, Map, Value as Json};

struct Inner {
    t: f64,
    log: Vec<Json>,
    snap: Map<String, Json>,
    kill_evidence: f64,
}

struct Fake {
    sc: Json,
    inner: Mutex<Inner>,
}

impl Fake {
    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn flag(&self, key: &str) -> bool {
        self.st().snap.get(key).and_then(Json::as_bool).unwrap_or(false)
    }
}

impl StatusHost for Fake {
    fn now(&self) -> f64 {
        self.st().t
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.st().log.push(json!(["console", source, text, level]));
    }
    fn log(&self, level: &str, text: &str) {
        self.st().log.push(json!(["log", level, text]));
    }
    fn dialect(&self) -> Dialect {
        dialect_for_stack(Stack::from_name(self.sc["stack"].as_str().unwrap_or("px4")))
    }
    fn store_update(&self, fields: Map<String, Json>) {
        let mut s = self.st();
        s.log.push(json!(["update", Json::Object(fields.clone())]));
        for (k, v) in fields {
            s.snap.insert(k, v);
        }
    }
    fn armed(&self) -> bool {
        self.flag("armed")
    }
    fn kill_switch(&self) -> bool {
        self.flag("kill_switch")
    }
    fn resolve_warnings(&self, kinds: &[&str]) {
        let mut sorted = kinds.to_vec();
        sorted.sort_unstable();
        self.st().log.push(json!(["resolve_warnings", sorted]));
    }
    fn merge_warning(&self, text: &str, level: &str, event: Option<&str>) {
        self.st().log.push(json!(["warning", text, level, event]));
    }
    fn kill_evidence_at(&self) -> f64 {
        self.st().kill_evidence
    }
    fn set_kill_evidence_at(&self, at: f64) {
        self.st().kill_evidence = at;
    }
    fn target(&self) -> (u8, u8) {
        (1, 1)
    }
    fn has_connection(&self) -> bool {
        !self.sc["no_connection"].as_bool().unwrap_or(false)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        if self.sc["send_fails"].as_bool().unwrap_or(false) {
            return Err("boom".into());
        }
        let int = |name: &str| json!(msg.get(name).and_then(Value::as_i64).unwrap());
        let mut args = vec![
            int("target_system"),
            int("target_component"),
            int("command"),
            int("confirmation"),
        ];
        for n in 1..=7 {
            args.push(match msg.get(&format!("param{n}")) {
                Some(Value::Float(v)) if v.is_nan() => json!("nan"),
                Some(Value::Float(v)) => json!(v),
                other => panic!("{other:?}"),
            });
        }
        self.st().log.push(json!(["send", "command_long_send", args]));
        Ok(true)
    }
}

fn run_step(engine: &StatusEngine, host: &Fake, step: &Json) -> Json {
    match step["op"].as_str().unwrap() {
        "text" => {
            let mut bytes = step["text"].as_str().unwrap().as_bytes().to_vec();
            bytes.resize(50, 0);
            let mut msg = Message::named("STATUSTEXT", Wire::V2)
                .unwrap()
                .with("severity", Value::Int(step["severity"].as_i64().unwrap() as i128))
                .with("text", Value::Bytes(bytes));
            if let Some(id) = step["id"].as_i64() {
                msg = msg
                    .with("id", Value::Int(id as i128))
                    .with("chunk_seq", Value::Int(step["seq"].as_i64().unwrap() as i128));
            }
            let frame = StatusText {
                msg: &msg,
                system: step["system"].as_i64().unwrap_or(1),
                component: step["component"].as_i64().unwrap_or(1),
            };
            engine.handle_statustext(host, &frame);
            Json::Null
        }
        "cleanup" => {
            engine.cleanup_statustext_chunks(host.now());
            Json::Null
        }
        "advance" => {
            host.st().t += step["seconds"].as_f64().unwrap();
            Json::Null
        }
        "prearm" => {
            engine.note_prearm_state(host, step["value"].as_bool());
            Json::Null
        }
        "kill_evidence" => {
            host.st().kill_evidence = step["at"].as_f64().unwrap();
            Json::Null
        }
        "request" => {
            engine.request_prearm_report(host);
            Json::Null
        }
        "clear" => {
            engine.clear_prearm_reasons(host);
            Json::Null
        }
        "state" => json!({
            "reasons": engine.prearm_reasons(),
            "reason_at": engine.prearm_reason_at(),
            "requested_at": engine.prearm_requested_at(),
            "kill_evidence": host.kill_evidence_at(),
            "partials": engine.partial_count(),
            "report_kill": engine.prearm_report_kill(),
            "report_started": engine.prearm_report_started_at(),
            "last": engine.prearm_last(),
            "snapshot": Json::Object(host.st().snap.clone()),
        }),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_status_code_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/status_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 40);
    let mut failures = Vec::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let mut snap = Map::new();
        for (k, v) in sc["snapshot"].as_object().into_iter().flatten() {
            snap.insert(k.clone(), v.clone());
        }
        let host = Fake {
            sc: sc.clone(),
            inner: Mutex::new(Inner {
                t: 1000.0,
                log: Vec::new(),
                snap,
                kill_evidence: 0.0,
            }),
        };
        let engine = StatusEngine::new();
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.st().log.len();
            let result = run_step(&engine, &host, step);
            let (events, t) = {
                let s = host.st();
                (s.log[before..].to_vec(), s.t)
            };
            let want = &case["expected"]["steps"][i];
            if result != want["result"]
                || json!(events) != want["events"]
                || (t - 1000.0 - want["t"].as_f64().unwrap()).abs() > 1e-6
            {
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
