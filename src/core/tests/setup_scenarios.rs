//! The Rust setup engine plays the scenarios the Python setup code played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`) and must
//! do the same: every command, line, error and store update, and what the
//! engine remembers afterwards.

use std::sync::Mutex;

use corvus_core::dialect::{dialect_for_stack, Dialect, Stack};
use corvus_core::priority_lock::PriorityLock;
use corvus_core::protocol::flight::FlightSnapshot;
use corvus_core::protocol::setup::{SetupEngine, SetupHost};
use serde_json::{json, Map, Value as Json};

fn f(x: f64) -> Json {
    if x.is_nan() {
        json!("nan")
    } else {
        json!(x)
    }
}

fn number(v: &Json) -> f64 {
    match v {
        Json::String(s) => match s.as_str() {
            "nan" => f64::NAN,
            "inf" => f64::INFINITY,
            other => panic!("{other}"),
        },
        other => other.as_f64().unwrap(),
    }
}

struct Inner {
    log: Vec<Json>,
    snap: Map<String, Json>,
    counts: std::collections::HashMap<i64, usize>,
    last_error: String,
}

struct Fake {
    sc: Json,
    lock: PriorityLock,
    inner: Mutex<Inner>,
}

impl Fake {
    fn new(sc: &Json) -> Fake {
        let mut snap = Map::new();
        snap.insert("armed".into(), json!(false));
        snap.insert("mode".into(), json!(""));
        snap.insert("landed_state".into(), Json::Null);
        snap.insert("autotune_state".into(), Json::Null);
        for (k, v) in sc["snapshot"].as_object().into_iter().flatten() {
            snap.insert(k.clone(), v.clone());
        }
        Fake {
            sc: sc.clone(),
            lock: PriorityLock::new(),
            inner: Mutex::new(Inner {
                log: Vec::new(),
                snap,
                counts: Default::default(),
                last_error: String::new(),
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn stack(&self) -> Dialect {
        dialect_for_stack(Stack::from_name(self.sc["stack"].as_str().unwrap_or("px4")))
    }

    fn mav(&self) -> i64 {
        self.sc["mav_type"].as_i64().unwrap_or(2)
    }
}

impl SetupHost for Fake {
    fn connection_ready(&self) -> bool {
        self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn set_error(&self, text: &str) {
        let mut s = self.st();
        s.last_error = text.into();
        s.log.push(json!(["error", text]));
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.st().log.push(json!(["console", source, text, level]));
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        self.st().log.push(json!(["failure", action, result]));
        false
    }
    fn lock(&self) -> &PriorityLock {
        &self.lock
    }
    fn snapshot(&self) -> FlightSnapshot {
        let s = self.st();
        FlightSnapshot {
            connected: true,
            armed: s.snap["armed"].as_bool().unwrap_or(false),
            mode: s.snap["mode"].as_str().unwrap_or("").to_string(),
            landed_state: s.snap["landed_state"].as_i64(),
            altitude_agl: 0.0,
        }
    }
    fn autotune_state(&self) -> String {
        self.st().snap["autotune_state"].as_str().unwrap_or("").to_string()
    }
    fn dialect(&self) -> Dialect {
        self.stack()
    }
    fn mav_type(&self) -> i64 {
        self.mav()
    }
    fn send_command(
        &self,
        command: i64,
        params: &[f64],
        timeout: f64,
        retries: i64,
        accept_in_progress: bool,
        interim: &[i64],
    ) -> i64 {
        let mut sorted = interim.to_vec();
        sorted.sort_unstable();
        let mut s = self.st();
        s.log.push(json!([
            "cmd",
            command,
            params.iter().map(|x| f(*x)).collect::<Vec<_>>(),
            timeout,
            retries,
            accept_in_progress,
            sorted
        ]));
        let plan = self.sc["results"]
            .get(command.to_string())
            .unwrap_or(&self.sc["default_result"]);
        match plan {
            Json::Array(list) => {
                let n = s.counts.get(&command).copied().unwrap_or(0);
                s.counts.insert(command, n + 1);
                list[n.min(list.len() - 1)].as_i64().unwrap()
            }
            Json::Null => 0,
            other => other.as_i64().unwrap(),
        }
    }
    fn update_autotune(&self, state: &str, progress: Option<i64>) {
        let mut fields = Map::new();
        fields.insert("autotune_state".into(), json!(state));
        if let Some(p) = progress {
            fields.insert("autotune_progress".into(), json!(p));
        }
        let mut s = self.st();
        s.log.push(json!(["update", Json::Object(fields.clone())]));
        for (k, v) in fields {
            s.snap.insert(k, v);
        }
    }
    fn merge_warning(&self, text: &str, level: &str) {
        self.st().log.push(json!(["merge_warning", text, level]));
    }
    fn set_mode(&self, mode: &str) -> bool {
        self.st().log.push(json!(["set_mode", mode]));
        self.sc["set_mode_results"]
            .get(mode)
            .and_then(Json::as_bool)
            .unwrap_or_else(|| self.sc["set_mode_ok"].as_bool().unwrap_or(true))
    }
    fn has_mode(&self, mode: &str) -> bool {
        match self.sc["mode_values"].as_array() {
            Some(list) => list.iter().any(|m| m.as_str() == Some(mode)),
            None => self
                .stack()
                .mode_table(self.mav(), Some(&[1, 18, 0]))
                .iter()
                .any(|(n, _)| n == mode),
        }
    }
    fn reset_parameter_cache(&self) {
        self.st().log.push(json!(["reset_cache"]));
    }
}

fn state_of(engine: &SetupEngine) -> Json {
    json!({
        "active": engine.active_calibration(),
        "return_mode": engine.autotune_return_mode(),
        "tested": engine.motors_tested(),
    })
}

fn run_step(engine: &SetupEngine, host: &Fake, step: &Json) -> Json {
    match step["op"].as_str().unwrap() {
        "available" => json!(engine.available_calibrations(host)),
        "calibrate" => json!(engine.calibrate(host, step["sensor"].as_str().unwrap())),
        "cancel" => json!(engine.cancel_calibration(host)),
        "accel_position" => json!(engine.accel_calibration_position(host, step["position"].as_str().unwrap())),
        "motor_test" => json!(engine.motor_test(
            host,
            step["motor"].as_i64().unwrap(),
            number(&step["throttle"]),
            number(&step["duration"])
        )),
        "stop_motor" => json!(engine.stop_motor_test(host)),
        "autotune" => json!(engine.autotune(host, step["axis"].as_str().unwrap(), step["enable"].as_bool().unwrap())),
        "ack" => {
            engine.handle_autotune_ack(
                host,
                step["result"].as_i64().unwrap(),
                step["progress"].as_i64().unwrap(),
            );
            Json::Null
        }
        "reboot" => json!(engine.reboot_autopilot(host)),
        "bootloader" => json!(engine.reboot_to_bootloader(host)),
        "set_snapshot" => {
            let mut s = host.st();
            for (k, v) in step["snapshot"].as_object().unwrap() {
                s.snap.insert(k.clone(), v.clone());
            }
            Json::Null
        }
        "state" => state_of(engine),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_setup_code_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/setup_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 50);
    let mut failures = Vec::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let host = Fake::new(sc);
        let engine = SetupEngine::new();
        let expected = &case["expected"];
        let mut bad = false;
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.st().log.len();
            let result = run_step(&engine, &host, step);
            let events = host.st().log[before..].to_vec();
            let want = &expected["steps"][i];
            if result != want["result"] || json!(events) != want["events"] {
                failures.push(format!(
                    "{name} step {i}: result {result} (want {}), events:\n{}\nwant:\n{}",
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
            let mut last = state_of(&engine);
            last["last_error"] = json!(s.last_error);
            last["snapshot"] = Json::Object(s.snap.clone());
            let want = &expected["final"];
            let got = json!({
                "last_error": last["last_error"], "active": last["active"], "return_mode": last["return_mode"],
                "tested": last["tested"], "snapshot": last["snapshot"],
            });
            if &got != want {
                failures.push(format!("{name}: final {got} want {want}"));
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
