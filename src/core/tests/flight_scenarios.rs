//! The Rust flight engine plays the scenarios the Python flight code played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`) and must
//! do the same: every command it sent, every line it published, the errors
//! it set, what it returned and how long the fake clock says it took.

use std::collections::HashSet;
use std::sync::Mutex;

use corvus_core::dialect::{dialect_for_stack, Dialect, ModeCommand, Stack};
use corvus_core::link::plain::{Message, Value};
use corvus_core::priority_lock::PriorityLock;
use corvus_core::protocol::flight::{FlightEngine, FlightHost, FlightSnapshot, MODE_CONFIRM_S};
use corvus_core::protocol::missions::{build_mission_item_spec, with_mission_home_slot, MissionItem};
use serde_json::{json, Value as Json};

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
            "-inf" => f64::NEG_INFINITY,
            other => panic!("{other}"),
        },
        other => other.as_f64().unwrap(),
    }
}

struct Inner {
    t: f64,
    armed: bool,
    mode: String,
    connected: bool,
    landed_state: Option<i64>,
    altitude_agl: f64,
    home: Option<f64>,
    pos_home: Option<f64>,
    pending: Vec<(f64, String, Json)>,
    counts: std::collections::HashMap<i64, usize>,
    aborted_on: HashSet<String>,
    aborts: u64,
    log: Vec<Json>,
    last_error: String,
}

struct Fake {
    sc: Json,
    lock: PriorityLock,
    inner: Mutex<Inner>,
}

impl Fake {
    fn new(sc: &Json) -> Fake {
        let init = &sc["initial"];
        let opt = |v: &Json| (!v.is_null()).then(|| v.as_f64().unwrap());
        Fake {
            sc: sc.clone(),
            lock: PriorityLock::new(),
            inner: Mutex::new(Inner {
                t: 1000.0,
                armed: init["armed"].as_bool().unwrap_or(false),
                mode: init["mode"].as_str().unwrap_or("").to_string(),
                connected: init["connected"].as_bool().unwrap_or(true),
                landed_state: init["landed_state"].as_i64(),
                altitude_agl: init["altitude_agl"].as_f64().unwrap_or(0.0),
                home: opt(&sc["home_amsl"]),
                pos_home: opt(&sc["position_home_amsl"]),
                pending: Vec::new(),
                counts: Default::default(),
                aborted_on: HashSet::new(),
                aborts: 0,
                log: Vec::new(),
                last_error: String::new(),
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn tick(&self) {
        let mut s = self.st();
        let now = s.t;
        let (mut due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut s.pending).into_iter().partition(|p| p.0 <= now);
        s.pending = keep;
        due.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (_, field, value) in due {
            match field.as_str() {
                "home_amsl" => s.home = value.as_f64(),
                "position_home_amsl" => s.pos_home = value.as_f64(),
                "armed" => s.armed = value.as_bool().unwrap(),
                "mode" => s.mode = value.as_str().unwrap().to_string(),
                "connected" => s.connected = value.as_bool().unwrap(),
                "landed_state" => s.landed_state = value.as_i64(),
                "altitude_agl" => s.altitude_agl = value.as_f64().unwrap(),
                other => panic!("{other}"),
            }
        }
    }

    fn mav_type(&self) -> i64 {
        self.sc["mav_type"].as_i64().unwrap_or(2)
    }

    fn stack(&self) -> Dialect {
        dialect_for_stack(Stack::from_name(self.sc["stack"].as_str().unwrap_or("px4")))
    }

    fn table(&self) -> Vec<(String, ModeCommand)> {
        if self.sc["modes"].as_str() == Some("empty") {
            return Vec::new();
        }
        self.stack().mode_table(self.mav_type(), Some(&[1, 18, 0]))
    }

    fn cost(&self) -> f64 {
        self.sc["cost"].as_f64().unwrap_or(0.1)
    }

    fn result_for(&self, command: i64, params: &[f64]) -> i64 {
        let mut s = self.st();
        s.t += self.cost();
        let plan = self.sc["results"]
            .get(command.to_string())
            .unwrap_or(&self.sc["default_result"]);
        let result = match plan {
            Json::Array(list) => {
                let n = s.counts.get(&command).copied().unwrap_or(0);
                s.counts.insert(command, n + 1);
                list[n.min(list.len() - 1)].as_i64().unwrap()
            }
            Json::Null => 0,
            other => other.as_i64().unwrap(),
        };
        if result == 0 {
            for rule in self.sc["reactions"].as_array().into_iter().flatten() {
                if rule["cmd"].as_i64() != Some(command) {
                    continue;
                }
                if let Some(p) = rule.get("param") {
                    let (index, value) = (p[0].as_u64().unwrap() as usize, p[1].as_f64().unwrap());
                    if params[index] != value {
                        continue;
                    }
                }
                for (field, value) in rule["set"].as_object().unwrap() {
                    let due = s.t + rule["after"].as_f64().unwrap_or(0.0);
                    s.pending.push((due, field.clone(), value.clone()));
                }
            }
        }
        result
    }
}

fn item_json(item: &MissionItem) -> Json {
    json!({
        "frame": item.frame,
        "command": item.command,
        "current": item.current,
        "autocontinue": item.autocontinue,
        "param1": f(item.params[0]), "param2": f(item.params[1]),
        "param3": f(item.params[2]), "param4": f(item.params[3]),
        "x_int": item.x_int,
        "y_int": item.y_int,
        "x_f": f(item.x_f), "y_f": f(item.y_f), "z": f(item.z),
    })
}

impl FlightHost for Fake {
    fn connection_ready(&self) -> bool {
        if let Some(until) = self.sc["ready_until"].as_f64() {
            if self.st().t >= until {
                return false;
            }
        }
        self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn set_error(&self, text: &str) {
        let mut s = self.st();
        s.last_error = text.into();
        s.log.push(json!(["error", text]));
    }
    fn last_error(&self) -> String {
        self.st().last_error.clone()
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        let trigger = {
            let mut s = self.st();
            s.log.push(json!(["console", source, text, level]));
            let mut hit = false;
            for prefix in self.sc["abort_on_console"].as_array().into_iter().flatten() {
                let prefix = prefix.as_str().unwrap();
                if text.starts_with(prefix) && !s.aborted_on.contains(prefix) {
                    s.aborted_on.insert(prefix.to_string());
                    s.aborts += 1;
                    hit = true;
                }
            }
            hit
        };
        if trigger {
            // An operator's Disarm, Land or RTL, from another thread.
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let _abort = self.lock.priority();
                    })
                    .join()
                    .unwrap();
            });
        }
    }
    fn log(&self, level: &str, text: &str) {
        self.st().log.push(json!(["log", level, text]));
    }
    fn store_warning(&self, text: &str, level: &str) {
        self.st().log.push(json!(["warn", text, level]));
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        self.st().log.push(json!(["failure", action, result]));
        false
    }
    fn send_command(&self, command: i64, params: &[f64], timeout: f64, retries: i64, accept_in_progress: bool) -> i64 {
        let row = json!([
            "cmd",
            command,
            params.iter().map(|x| f(*x)).collect::<Vec<_>>(),
            timeout,
            retries,
            accept_in_progress
        ]);
        self.st().log.push(row);
        self.result_for(command, params)
    }
    fn send_command_int(
        &self,
        command: i64,
        frame: i64,
        params: &[f64],
        x: i64,
        y: i64,
        z: f64,
        timeout: f64,
        retries: i64,
    ) -> i64 {
        let row = json!([
            "cmdint",
            command,
            frame,
            params.iter().map(|v| f(*v)).collect::<Vec<_>>(),
            x,
            y,
            f(z),
            timeout,
            retries
        ]);
        self.st().log.push(row);
        self.result_for(command, params)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        assert_eq!(msg.name(), "SET_POSITION_TARGET_LOCAL_NED");
        if self.sc["send_fails"].as_bool().unwrap_or(false) {
            return Err("boom".into());
        }
        if self.sc["link_lost"].as_bool().unwrap_or(false) {
            return Ok(false);
        }
        let int = |name: &str| json!(msg.get(name).and_then(Value::as_i64).unwrap());
        let float = |name: &str| match msg.get(name) {
            Some(Value::Float(v)) => f(*v),
            other => panic!("{name}: {other:?}"),
        };
        let args = vec![
            int("time_boot_ms"),
            int("target_system"),
            int("target_component"),
            int("coordinate_frame"),
            int("type_mask"),
            float("x"),
            float("y"),
            float("z"),
            float("vx"),
            float("vy"),
            float("vz"),
            float("afx"),
            float("afy"),
            float("afz"),
            float("yaw"),
            float("yaw_rate"),
        ];
        self.st()
            .log
            .push(json!(["sent", "set_position_target_local_ned_send", args]));
        Ok(true)
    }
    fn target(&self) -> (u8, u8) {
        (1, 1)
    }
    fn lock(&self) -> &PriorityLock {
        &self.lock
    }
    fn now(&self) -> f64 {
        self.st().t
    }
    fn sleep(&self, seconds: f64) {
        self.st().t += seconds;
    }
    fn stopping(&self) -> bool {
        self.sc["stop_at"].as_f64().is_some_and(|at| self.st().t >= at)
    }
    fn snapshot(&self) -> FlightSnapshot {
        self.tick();
        let s = self.st();
        FlightSnapshot {
            connected: s.connected,
            armed: s.armed,
            mode: s.mode.clone(),
            landed_state: s.landed_state,
            altitude_agl: s.altitude_agl,
        }
    }
    fn is_stale(&self, timeout: f64) -> bool {
        self.st().log.push(json!(["stale_check", timeout]));
        self.sc["stale"].as_bool().unwrap_or(false)
    }
    fn dialect(&self) -> Dialect {
        self.stack()
    }
    fn mav_type(&self) -> i64 {
        Fake::mav_type(self)
    }
    fn mode_command(&self, mode: &str) -> Option<ModeCommand> {
        self.table().into_iter().find(|(n, _)| n == mode).map(|(_, c)| c)
    }
    fn has_modes(&self) -> bool {
        !self.table().is_empty()
    }
    fn modes_unsupported(&self) -> bool {
        self.sc["modes_unsupported"].as_bool().unwrap_or(false)
    }
    fn home_alt_amsl(&self) -> Option<f64> {
        self.tick();
        self.st().home
    }
    fn position_home_alt_amsl(&self) -> Option<f64> {
        self.tick();
        self.st().pos_home
    }
    fn request_home(&self) {
        let mut s = self.st();
        s.log.push(json!(["request_home"]));
        if let Some(reply) = self.sc.get("home_reply").filter(|r| !r.is_null()) {
            let due = s.t + reply["after"].as_f64().unwrap();
            s.pending.push((due, "home_amsl".into(), reply["value"].clone()));
        }
    }
    fn validate_fly_to_points(&self, points: &Json) -> Option<Vec<(f64, f64, f64)>> {
        self.st().log.push(json!(["validate_points", points]));
        if let Some(text) = self.sc["points_error"].as_str() {
            self.set_error(text);
            return None;
        }
        Some(
            points
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap(), p[2].as_f64().unwrap()))
                .collect(),
        )
    }
    fn takeoff_item_position(&self, first: (f64, f64, f64)) -> (f64, f64) {
        (first.0, first.1)
    }
    fn build_mission_item_spec(
        &self,
        seq: i64,
        command: i64,
        lat: f64,
        lon: f64,
        alt: f64,
        p1: f64,
        p2: f64,
        p3: f64,
        p4: f64,
    ) -> MissionItem {
        build_mission_item_spec(seq, command, lat, lon, alt, p1, p2, p3, p4, None)
    }
    fn with_mission_home_slot(&self, items: Vec<MissionItem>) -> Vec<MissionItem> {
        let home: Vec<f64> = self.sc["home"]
            .as_array()
            .map(|h| h.iter().map(|v| v.as_f64().unwrap()).collect())
            .unwrap_or_else(|| vec![0.0, 0.0]);
        let home_alt = self.st().home;
        with_mission_home_slot(items, &self.stack(), &home, home_alt)
    }
    fn upload_mission(&self, items: &[MissionItem]) -> i64 {
        let mut s = self.st();
        s.log
            .push(json!(["upload", items.iter().map(item_json).collect::<Vec<_>>()]));
        s.t += self.cost();
        self.sc["upload_ack"].as_i64().unwrap_or(0)
    }
    fn mission_ack_to_result(&self, ack: i64) -> i64 {
        corvus_core::protocol::missions::mission_ack_to_result(ack)
    }
    fn note_vehicle_mission_cleared(&self) {
        self.st().log.push(json!(["note_mission", Json::Null]));
    }
}

fn run_step(engine: &FlightEngine, host: &Fake, step: &Json) -> Json {
    let op = step["op"].as_str().unwrap();
    match op {
        "set_mode" => json!(engine.set_mode(host, step["mode"].as_str().unwrap())),
        "arm" => json!(engine.arm(host, step["arm"].as_bool().unwrap())),
        "airborne" => json!(engine.is_airborne(host)),
        "state_airborne" => {
            let mut out = Vec::new();
            for landed in [None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)] {
                for agl in [0.0, 1.5, 1.6] {
                    {
                        let mut s = host.st();
                        s.landed_state = landed;
                        s.altitude_agl = agl;
                    }
                    out.push(json!(engine.is_airborne(host)));
                }
            }
            json!(out)
        }
        "amsl" => engine
            .takeoff_altitude_amsl(host, number(&step["agl"]))
            .map_or(Json::Null, f),
        "await_reference" => engine
            .await_takeoff_reference(host, number(&step["agl"]))
            .map_or(Json::Null, f),
        "wait_mode" => json!(engine.wait_for_mode(host, step["mode"].as_str().unwrap(), MODE_CONFIRM_S)),
        "pend" => {
            let mut s = host.st();
            let due = s.t + step["after"].as_f64().unwrap();
            s.pending
                .push((due, step["field"].as_str().unwrap().into(), step["value"].clone()));
            Json::Null
        }
        "takeoff" => json!(engine.takeoff(host, number(&step["agl"]))),
        "land" => json!(engine.land(host)),
        "rtl" => json!(engine.rtl(host)),
        "lat_lon" => engine
            .validate_lat_lon(host, number(&step["lat"]), number(&step["lon"]), "Check")
            .map_or(Json::Null, |(a, b)| json!([a, b])),
        "set_home" => json!(engine.set_home(host, number(&step["lat"]), number(&step["lon"]))),
        "hold" => json!(engine.hold(host)),
        "plugin" => {
            let params: Option<Vec<f64>> = step
                .get("params")
                .and_then(Json::as_array)
                .map(|a| a.iter().map(|v| v.as_f64().unwrap()).collect());
            let position = step.get("position").map(|p| {
                (
                    p[0].as_f64().unwrap(),
                    p[1].as_f64().unwrap(),
                    p[2].as_f64().unwrap(),
                    p[3].as_str().unwrap(),
                )
            });
            json!(engine.plugin_command(host, step["command"].as_i64().unwrap(), params.as_deref(), position))
        }
        "velocity" => json!(engine.velocity_setpoint(
            host,
            number(&step["vx"]),
            number(&step["vy"]),
            number(&step["vz"]),
            step.get("yaw").map(number),
            step["body"].as_bool().unwrap_or(false)
        )),
        "fly" => json!(engine.fly_to_points(host, &step["points"])),
        "start" => json!(engine.start_mission(host, step["count"].as_i64().unwrap())),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_flight_code_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/flight_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 100);
    let engine = FlightEngine::new();
    let mut failures = Vec::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let host = Fake::new(sc);
        let expected = &case["expected"];
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.st().log.len();
            let result = run_step(&engine, &host, step);
            let (events, t) = {
                let s = host.st();
                (s.log[before..].to_vec(), s.t)
            };
            let want = &expected["steps"][i];
            if result != want["result"] || json!(events) != want["events"] {
                failures.push(format!(
                    "{name} step {i}: result {result} (want {}), events:\n{}\nwant:\n{}",
                    want["result"],
                    serde_json::to_string(&events).unwrap(),
                    want["events"]
                ));
                break;
            }
            let dt = t - 1000.0;
            if (dt - want["t"].as_f64().unwrap()).abs() > 1e-5 {
                failures.push(format!("{name} step {i}: took {dt}, want {}", want["t"]));
                break;
            }
        }
        let s = host.st();
        let final_state = json!({"last_error": s.last_error, "armed": s.armed, "mode": s.mode, "aborts": s.aborts});
        if final_state != expected["final"] && failures.iter().all(|m| !m.starts_with(name)) {
            failures.push(format!("{name}: final {final_state} want {}", expected["final"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
