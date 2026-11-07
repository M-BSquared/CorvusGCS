//! The Rust receive path plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`): every
//! store update, console line and hand over to another protocol for each kind
//! of frame, who a frame may come from, the duplicate window, and the command
//! transaction against a scripted vehicle on a fake clock.

use std::collections::HashSet;
use std::sync::Mutex;

use corvus_core::dialect::{dialect_for_id, Dialect};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::priority_lock::PriorityLock;
use corvus_core::protocol::common::Event;
use corvus_core::protocol::receive::*;
use corvus_core::protocol::telemetry::{TelemetryEngine, TelemetryHost};
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
        Json::String(s) if s == "nan" => f64::NAN,
        other => other.as_f64().unwrap(),
    }
}

/// Python's equality: 1 and 1.0 are the same number.
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn build(name: &str, fields: &Json) -> Message {
    let mut msg = Message::named(name, Wire::V2).unwrap();
    for (key, value) in fields.as_object().unwrap() {
        let float = match msg.get(key).unwrap_or_else(|| panic!("{name} has no field {key}")) {
            Value::Float(_) => true,
            Value::Array(items) => matches!(items.first(), Some(Value::Float(_))),
            _ => false,
        };
        let one = |v: &Json| {
            if float {
                Value::Float(number(v))
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

struct Spec {
    message: Option<Message>,
    unknown: Option<(u32, Vec<u8>)>,
    system: i64,
    component: i64,
    raw: Option<Vec<u8>>,
}

impl Spec {
    fn of(v: &Json) -> Spec {
        let system = v["system"].as_i64().unwrap_or(1);
        let component = v["component"].as_i64().unwrap_or(1);
        if let Some(id) = v["unknown"].as_u64() {
            return Spec {
                message: None,
                unknown: Some((id as u32, hex(v["data"].as_str().unwrap_or("")))),
                system: v["system"].as_i64().unwrap_or(0),
                component: v["component"].as_i64().unwrap_or(0),
                raw: None,
            };
        }
        let empty = json!({});
        Spec {
            message: Some(build(v["name"].as_str().unwrap(), v.get("fields").unwrap_or(&empty))),
            unknown: None,
            system,
            component,
            raw: v["raw"].as_str().map(hex),
        }
    }

    fn frame(&self) -> Frame<'_> {
        Frame {
            payload: match (&self.message, &self.unknown) {
                (Some(m), _) => Payload::Message(m),
                (None, Some((id, data))) => Payload::Unknown { id: *id, data },
                _ => unreachable!(),
            },
            system: self.system,
            component: self.component,
            raw: self.raw.as_deref(),
        }
    }
}

struct Queued {
    time: f64,
    seq: u64,
    ack: Json,
}

struct Inner {
    t: f64,
    log: Vec<Json>,
    snap: Map<String, Json>,
    queue: Vec<Queued>,
    n: u64,
    sends: std::collections::HashMap<i64, usize>,
    dialect: Dialect,
    mav_type: i64,
    degraded: bool,
    rangefinder_seen: Option<f64>,
    last_boot_ms: Option<i64>,
    kill_evidence: Option<f64>,
    firmware: Option<(i64, i64, i64)>,
    board: (i64, i64),
    encoding: String,
    request_sent: bool,
}

struct Fake {
    sc: Json,
    lock: PriorityLock,
    telemetry: TelemetryEngine,
    path: ReceivePath,
    inner: Mutex<Inner>,
}

struct Tele<'a>(&'a Fake);

impl TelemetryHost for Tele<'_> {
    fn store_update(&self, fields: Map<String, Json>) {
        ReceiveHost::store_update(self.0, fields);
    }
    fn rc_channels(&self) -> Vec<i64> {
        self.0
            .st()
            .snap
            .get("rc_channels")
            .and_then(Json::as_array)
            .map_or(Vec::new(), |a| a.iter().map(|v| v.as_i64().unwrap()).collect())
    }
    fn target_system(&self) -> i64 {
        i64::from(ReceiveHost::target(self.0).0)
    }
    fn lock(&self) -> &PriorityLock {
        &self.0.lock
    }
    fn set_error(&self, text: &str) {
        self.0.st().log.push(json!(["error", text]));
    }
    fn connection_ready(&self) -> bool {
        ReceiveHost::connection_ready(self.0)
    }
    fn set_message_interval(&self, _: i64, _: i64) -> bool {
        true
    }
    fn message_interval(&self, _: i64) -> i64 {
        0
    }
    fn is_slow_link(&self) -> bool {
        false
    }
    fn forget_rangefinder(&self) {}
}

impl Fake {
    fn new(sc: &Json) -> Fake {
        let mut snap = Map::new();
        for (k, v) in sc["snapshot"].as_object().into_iter().flatten() {
            snap.insert(k.clone(), v.clone());
        }
        Fake {
            sc: sc.clone(),
            lock: PriorityLock::new(),
            telemetry: TelemetryEngine::new(),
            path: ReceivePath::new(),
            inner: Mutex::new(Inner {
                t: 1000.0,
                log: Vec::new(),
                snap,
                queue: Vec::new(),
                n: 0,
                sends: Default::default(),
                dialect: dialect_for_id(12),
                mav_type: sc["mav_type"].as_i64().unwrap_or(2),
                degraded: sc["degraded"].as_bool().unwrap_or(false),
                rangefinder_seen: sc["rangefinder_seen"].as_f64(),
                last_boot_ms: sc["last_boot_ms"].as_i64(),
                kill_evidence: None,
                firmware: None,
                board: (0, 0),
                encoding: "unset".into(),
                request_sent: true,
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn call(&self, name: &str, args: Vec<Json>) {
        self.st().log.push(json!(["call", name, args]));
    }

    fn on_command(&self, command: i64) {
        let mut s = self.st();
        let attempt = {
            let n = s.sends.entry(command).or_insert(0);
            *n += 1;
            *n - 1
        };
        let script = self.sc["acks"][command.to_string()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(acks) = script.get(attempt).and_then(Json::as_array) {
            for ack in acks {
                s.n += 1;
                let (time, seq) = (s.t + ack["after"].as_f64().unwrap(), s.n);
                s.queue.push(Queued {
                    time,
                    seq,
                    ack: ack.clone(),
                });
            }
        }
    }

    fn deliver(&self, q: Queued) {
        let ack = &q.ack;
        let fields = json!({
            "command": ack["command"].as_i64().unwrap_or(0),
            "result": ack["result"],
            "progress": ack["progress"].as_i64().unwrap_or(0),
            "target_system": ack["target_system"].as_i64().unwrap_or(0),
            "target_component": ack["target_component"].as_i64().unwrap_or(0),
        });
        let spec = Spec {
            message: Some(build("COMMAND_ACK", &fields)),
            unknown: None,
            system: ack["system"].as_i64().unwrap_or(1),
            component: ack["component"].as_i64().unwrap_or(1),
            raw: None,
        };
        self.path.dispatch(self, &spec.frame());
    }

    fn no_connection(&self) -> bool {
        self.sc["no_connection"].as_bool().unwrap_or(false)
    }
}

impl ReceiveHost for Fake {
    fn target(&self) -> (u8, u8) {
        let t = &self.sc["target"];
        (t[0].as_u64().unwrap_or(1) as u8, t[1].as_u64().unwrap_or(1) as u8)
    }
    fn link_generation(&self) -> u64 {
        1
    }
    fn has_connection(&self) -> bool {
        !self.no_connection()
    }
    fn source_ids(&self) -> (i64, i64) {
        (
            self.sc["source_system"].as_i64().unwrap_or(255),
            self.sc["source_component"].as_i64().unwrap_or(190),
        )
    }
    fn connection_ready(&self) -> bool {
        !self.no_connection() && self.sc["ready"].as_bool().unwrap_or(true)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        if self.sc["send_fails"].as_bool().unwrap_or(false) {
            return Err("boom".into());
        }
        let int = |name: &str| json!(msg.get(name).and_then(Value::as_i64).unwrap());
        let float = |name: &str| match msg.get(name) {
            Some(Value::Float(v)) => f(*v),
            other => panic!("{name}: {other:?}"),
        };
        let mut args = vec![int("target_system"), int("target_component")];
        let (method, command) = match msg.name() {
            "COMMAND_LONG" => {
                args.push(int("command"));
                args.push(int("confirmation"));
                for n in 1..=7 {
                    args.push(float(&format!("param{n}")));
                }
                ("command_long_send", msg.get("command").and_then(Value::as_i64).unwrap())
            }
            "COMMAND_INT" => {
                for name in ["frame", "command", "current", "autocontinue"] {
                    args.push(int(name));
                }
                for n in 1..=4 {
                    args.push(float(&format!("param{n}")));
                }
                args.push(int("x"));
                args.push(int("y"));
                args.push(float("z"));
                ("command_int_send", msg.get("command").and_then(Value::as_i64).unwrap())
            }
            other => panic!("{other}"),
        };
        self.st().log.push(json!(["send", method, args]));
        self.on_command(command);
        Ok(true)
    }
    fn lock(&self) -> &PriorityLock {
        &self.lock
    }
    fn wait_event(&self, event: &Event, timeout: f64) -> bool {
        let deadline = self.st().t + timeout;
        loop {
            if event.is_set() {
                return true;
            }
            let next = {
                let mut s = self.st();
                let pick = s
                    .queue
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| (a.time, a.seq).partial_cmp(&(b.time, b.seq)).unwrap())
                    .map(|(i, _)| i);
                match pick {
                    Some(i) if s.queue[i].time <= deadline => {
                        let q = s.queue.remove(i);
                        if q.time > s.t {
                            s.t = q.time;
                        }
                        Some(q)
                    }
                    _ => {
                        s.t = deadline;
                        None
                    }
                }
            };
            match next {
                Some(q) => self.deliver(q),
                None => return event.is_set(),
            }
        }
    }
    fn now(&self) -> f64 {
        self.st().t
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.st().log.push(json!(["console", source, text, level]));
    }
    fn log(&self, level: &str, text: &str) {
        self.st().log.push(json!(["log", level, text]));
    }
    fn store_heartbeat(&self) -> f64 {
        let mut s = self.st();
        s.log.push(json!(["store_heartbeat"]));
        s.t
    }
    fn store_update(&self, fields: Map<String, Json>) {
        let mut s = self.st();
        s.log.push(json!(["update", Json::Object(fields.clone())]));
        for (k, v) in fields {
            s.snap.insert(k, v);
        }
    }
    fn store_flags(&self) -> StoreFlags {
        let s = self.st();
        let flag = |k: &str| s.snap.get(k).and_then(Json::as_bool).unwrap_or(false);
        StoreFlags {
            armed: flag("armed"),
            kill_switch: flag("kill_switch"),
            flight_termination: flag("flight_termination"),
        }
    }
    fn resolve_warnings(&self, kinds: &[&str]) {
        let mut sorted: Vec<&str> = kinds.to_vec();
        sorted.sort_unstable();
        self.st().log.push(json!(["resolve_warnings", sorted]));
    }
    fn armed_now(&self) -> bool {
        self.st().snap.get("armed").and_then(Json::as_bool).unwrap_or(false)
    }
    fn dialect(&self) -> Dialect {
        self.st().dialect
    }
    fn latch_dialect(&self, autopilot: i64, mav_type: i64) -> bool {
        let mut s = self.st();
        let dialect = dialect_for_id(autopilot);
        let changed = dialect != s.dialect || mav_type != s.mav_type;
        if dialect != s.dialect {
            s.firmware = None;
        }
        s.dialect = dialect;
        s.mav_type = mav_type;
        changed
    }
    fn build_mode_mapping(&self) {
        self.call("build_mode_mapping", vec![]);
    }
    fn firmware_version(&self) -> Option<(i64, i64, i64)> {
        self.st().firmware
    }
    fn set_firmware_version(&self, version: (i64, i64, i64)) {
        self.st().firmware = Some(version);
    }
    fn set_board_ids(&self, vendor: i64, product: i64) {
        self.st().board = (vendor, product);
    }
    fn set_param_encoding_declared(&self, encoding: &str) {
        self.st().encoding = encoding.into();
    }
    fn rangefinder_seen(&self) -> Option<f64> {
        self.st().rangefinder_seen
    }
    fn set_rangefinder_seen(&self, at: Option<f64>) {
        self.st().rangefinder_seen = at;
    }
    fn degraded_warned(&self) -> bool {
        self.st().degraded
    }
    fn set_degraded_warned(&self, warned: bool) {
        self.st().degraded = warned;
    }
    fn warn_timeout(&self) -> f64 {
        5.0
    }
    fn set_kill_evidence(&self, at: f64) {
        self.st().kill_evidence = Some(at);
    }
    fn last_boot_ms(&self) -> Option<i64> {
        self.st().last_boot_ms
    }
    fn set_last_boot_ms(&self, ms: i64) {
        self.st().last_boot_ms = Some(ms);
    }
    fn set_position_home_alt_amsl(&self, _: f64) {}
    fn set_home_alt_amsl(&self, _: f64) {}
    fn observe_fleet(&self, frame: &Frame, vehicle_heartbeat: bool) {
        self.call("fleet", vec![json!(frame.name()), json!(vehicle_heartbeat)]);
    }
    fn write_tlog(&self, frame: &Frame) {
        self.call("tlog", vec![json!(frame.name())]);
    }
    fn record_scope(&self, frame: &Frame) {
        self.call("scope", vec![json!(frame.name())]);
    }
    fn handle_autotune_ack(&self, result: i64, progress: i64) {
        self.call("autotune_ack", vec![json!(result), json!(progress)]);
    }
    fn handle_mission_request(&self, msg: &Message, as_int: bool) {
        self.call("handle_mission_request", vec![json!(msg.name()), json!(as_int)]);
    }
    fn handle_mission_ack(&self, msg: &Message) {
        self.call("handle_mission_ack", vec![json!(msg.name())]);
    }
    fn handle_mission_count(&self, msg: &Message) {
        self.call("handle_mission_count", vec![json!(msg.name())]);
    }
    fn handle_mission_item(&self, msg: &Message, as_int: bool) {
        self.call("handle_mission_item", vec![json!(msg.name()), json!(as_int)]);
    }
    fn handle_mission_current(&self, msg: &Message) {
        self.call("handle_mission_current", vec![json!(msg.name())]);
    }
    fn handle_mission_item_reached(&self, msg: &Message) {
        self.call("handle_mission_item_reached", vec![json!(msg.name())]);
    }
    fn handle_serial_control(&self, msg: &Message) {
        self.call("handle_serial_control", vec![json!(msg.name())]);
    }
    fn handle_radio_status(&self, msg: &Message) {
        self.telemetry.handle_radio_status(&Tele(self), msg);
    }
    fn handle_gnss_integrity(&self, frame: &Frame) {
        match &frame.payload {
            Payload::Message(m) => self.telemetry.handle_gnss_integrity(&Tele(self), m, frame.system),
            Payload::Unknown { data, .. } => self.telemetry.handle_gnss_integrity_frame(&Tele(self), data),
        }
    }
    fn handle_remote_id_arm_status(&self, msg: &Message) {
        self.call("handle_remote_id_arm_status", vec![json!(msg.name())]);
    }
    fn handle_battery_status(&self, msg: &Message) {
        self.telemetry.handle_battery_status(&Tele(self), msg);
    }
    fn handle_statustext(&self, msg: &Message) {
        self.call("handle_statustext", vec![json!(msg.name())]);
    }
    fn handle_param_value(&self, msg: &Message) {
        self.call("handle_param_value", vec![json!(msg.name())]);
    }
    fn handle_ftp_message(&self, msg: &Message) {
        self.call("handle_ftp_message", vec![json!(msg.name())]);
    }
    fn handle_rc_channels(&self, msg: &Message, raw: bool) {
        self.telemetry.handle_rc_channels(&Tele(self), msg, raw);
    }
    fn handle_attitude_target(&self, msg: &Message) {
        self.telemetry.handle_attitude_target(&Tele(self), msg);
    }
    fn handle_position_target(&self, msg: &Message) {
        self.telemetry.handle_position_target(&Tele(self), msg);
    }
    fn log_sink(&self, msg: &Message) -> Result<bool, String> {
        if self.sc["log_sink_raises"].as_bool().unwrap_or(false) {
            return Err("sink down".into());
        }
        if self.sc["log_sink"].as_bool().unwrap_or(false) {
            self.call("log_sink", vec![json!(msg.name())]);
            return Ok(true);
        }
        Ok(false)
    }
    fn note_heartbeat(&self, stamp: f64) {
        self.telemetry.note_heartbeat(stamp);
    }
    fn publish_heartbeat_jitter(&self) {
        self.telemetry.publish_heartbeat_jitter(&Tele(self));
    }
    fn publish_link_quality(&self) {
        self.telemetry.publish_link_quality(&Tele(self));
    }
    fn battery_fields(&self, voltage: f64, current: f64, reported: i64) -> Map<String, Json> {
        self.telemetry.battery_fields(voltage, current, reported)
    }
    fn battery_endurance_fields(&self, percent: Option<f64>, source: &str, armed: bool) -> Map<String, Json> {
        self.telemetry
            .battery_endurance_fields(percent, source, armed, self.now())
    }
    fn note_prearm_state(&self, prearm: Option<bool>) {
        self.call("note_prearm_state", vec![prearm.map_or(Json::Null, |p| json!(p))]);
    }
    fn has_prearm_reasons(&self) -> bool {
        self.sc["prearm_reasons"].as_bool().unwrap_or(false)
    }
    fn clear_prearm_reasons(&self) {
        self.call("clear_prearm_reasons", vec![]);
    }
    fn roll_tlog_on_disarm(&self, armed: bool) {
        self.call("roll_tlog_on_disarm", vec![json!(armed)]);
    }
    fn reset_parameter_cache(&self) {
        self.call("reset_parameter_cache", vec![]);
    }
    fn reset_param_metadata(&self) {
        self.call("reset_param_metadata", vec![]);
    }
    fn set_request_sent(&self, sent: bool) {
        self.st().request_sent = sent;
    }
    fn is_serial(&self) -> bool {
        self.sc["serial"].as_bool().unwrap_or(false)
    }
    fn request_streams(&self) {
        self.call("request_streams", vec![]);
    }
    fn request_version(&self) {
        self.call("request_version", vec![]);
    }
    fn request_home(&self) {
        self.call("request_home", vec![]);
    }
    fn schedule_message_intervals(&self) {
        self.call("schedule_message_intervals", vec![]);
    }
    fn cancel_mission_upload(&self) {
        self.call("cancel_upload", vec![]);
    }
}

fn state_of(host: &Fake) -> Json {
    let s = host.st();
    json!({
        "snapshot": Json::Object(s.snap.clone()),
        "degraded": s.degraded,
        "ignored": host.path.ignored_systems(),
        "duplicates": host.path.duplicate_frames(),
        "rangefinder_seen": s.rangefinder_seen,
        "last_boot_ms": s.last_boot_ms,
        "kill_evidence": s.kill_evidence,
        "firmware": s.firmware.map(|(a, b, c)| json!([a, b, c])),
        "board": [s.board.0, s.board.1],
        "encoding": s.encoding,
        "request_sent": s.request_sent,
        "stack": s.dialect.stack().as_str(),
        "mav_type": s.mav_type,
        "hb": host.telemetry.hb_times(),
    })
}

fn run_step(host: &Fake, step: &Json) -> Json {
    let path = &host.path;
    let op = step["op"].as_str().unwrap();
    let frame_of = || Spec::of(&step["frame"]);
    match op {
        "plausible" => json!(is_plausible_heartbeat(frame_of().message.as_ref().unwrap())),
        "vehicle_heartbeat" => json!(is_vehicle_heartbeat(&frame_of().frame())),
        "sys_helpers" => {
            let msg = build("SYS_STATUS", &step["fields"]);
            json!([sensor_health(&msg, 32), prearm_ok(&msg)])
        }
        "range" => rangefinder_distance_m(&build("DISTANCE_SENSOR", &step["fields"])).map_or(Json::Null, |d| json!(d)),
        "gps_helpers" => {
            let msg = build("GPS_RAW_INT", &step["fields"]);
            let acc = |name: &str| gps_accuracy_m(msg.get(name).and_then(Value::as_i64).unwrap());
            json!([gps_altitude_m(&msg), acc("h_acc"), acc("v_acc")])
        }
        "git_hash" => {
            let raw: Vec<u8> = step["raw"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap() as u8)
                .collect();
            json!(decode_git_hash(&raw))
        }
        "who" => {
            let spec = frame_of();
            json!([
                path.is_from_vehicle(host, &spec.frame()),
                path.is_from_autopilot(host, &spec.frame())
            ])
        }
        "ack_for" => json!(path.ack_is_for_us(host, &frame_of().frame())),
        "dispatch" => {
            path.dispatch(host, &frame_of().frame());
            Json::Null
        }
        "process" => {
            path.process_frame(host, &frame_of().frame());
            Json::Null
        }
        "mark_degraded" => {
            path.mark_degraded_once(host);
            Json::Null
        }
        "set_battery" => {
            host.telemetry.set_battery_settings(Some(&step["raw"]));
            Json::Null
        }
        "advance" => {
            host.st().t += step["seconds"].as_f64().unwrap();
            Json::Null
        }
        "command" => {
            let params: Vec<f64> = step["params"].as_array().unwrap().iter().map(number).collect();
            let interim: Vec<i64> = step["interim"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| v.as_i64().unwrap())
                .collect();
            json!(path.send_command_and_wait(
                host,
                step["command"].as_i64().unwrap(),
                &params,
                step["timeout"].as_f64().unwrap(),
                step["retries"].as_i64().unwrap(),
                step["accept_in_progress"].as_bool().unwrap_or(false),
                &interim
            ))
        }
        "command_int" => {
            let params: Vec<f64> = step["params"].as_array().unwrap().iter().map(number).collect();
            json!(path.send_command_int_and_wait(
                host,
                step["command"].as_i64().unwrap(),
                step["frame"].as_i64().unwrap(),
                &params,
                step["x"].as_i64().unwrap(),
                step["y"].as_i64().unwrap(),
                number(&step["z"]),
                step["timeout"].as_f64().unwrap(),
                step["retries"].as_i64().unwrap()
            ))
        }
        "cancel" => {
            path.cancel_pending_commands(host);
            json!(-2)
        }
        "pending" => json!(path.pending_commands()),
        "state" => state_of(host),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_receive_path_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/receive_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 60);
    let mut failures = Vec::new();
    let mut seen = HashSet::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        seen.insert(name.to_string());
        let host = Fake::new(sc);
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.st().log.len();
            let result = run_step(&host, step);
            let (events, t) = {
                let s = host.st();
                (s.log[before..].to_vec(), s.t)
            };
            let want = &case["expected"]["steps"][i];
            if !same(&result, &want["result"])
                || !same(&json!(events), &want["events"])
                || (t - 1000.0 - want["t"].as_f64().unwrap()).abs() > 1e-5
            {
                failures.push(format!(
                    "{name} step {i} ({}): result {result} (want {}), events:\n{}\nwant:\n{}",
                    step["op"],
                    want["result"],
                    serde_json::to_string(&events).unwrap(),
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
