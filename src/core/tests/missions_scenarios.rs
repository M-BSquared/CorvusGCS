//! The Rust mission engine plays the scenarios the Python mission protocol
//! played (`python/tools/dump_protocol_cases.py`, run on `tests/reference`)
//! against the same scripted vehicle, on the same fake clock: every message
//! it sent, every line it published, the errors it set and how long it took.

use std::sync::{Arc, Mutex};

use corvus_core::dialect::{dialect_for_stack, Dialect, Stack};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::priority_lock::PriorityLock;
use corvus_core::protocol::common::Event;
use corvus_core::protocol::missions::*;
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

fn item_json(item: &MissionItem) -> Json {
    json!({
        "frame": item.frame, "command": item.command, "current": item.current, "autocontinue": item.autocontinue,
        "param1": f(item.params[0]), "param2": f(item.params[1]), "param3": f(item.params[2]), "param4": f(item.params[3]),
        "x_int": item.x_int, "y_int": item.y_int, "x_f": f(item.x_f), "y_f": f(item.y_f), "z": f(item.z),
    })
}

fn item_of(v: &Json) -> MissionItem {
    MissionItem {
        frame: v["frame"].as_i64().unwrap(),
        command: v["command"].as_i64().unwrap(),
        current: v["current"].as_i64().unwrap(),
        autocontinue: v["autocontinue"].as_i64().unwrap(),
        params: [
            number(&v["param1"]),
            number(&v["param2"]),
            number(&v["param3"]),
            number(&v["param4"]),
        ],
        x_int: v["x_int"].as_i64().unwrap(),
        y_int: v["y_int"].as_i64().unwrap(),
        x_f: number(&v["x_f"]),
        y_f: number(&v["y_f"]),
        z: number(&v["z"]),
    }
}

fn plan_of(v: &Json) -> PlanItem {
    PlanItem {
        command: v["command"].as_i64().unwrap(),
        lat: number(&v["lat"]),
        lon: number(&v["lon"]),
        alt: number(&v["alt"]),
        params: v["params"].as_array().unwrap().iter().map(number).collect(),
        item: v["item"].as_i64(),
    }
}

fn build(name: &str, fields: &Json) -> Message {
    let mut msg = Message::named(name, Wire::V2).unwrap();
    for (key, value) in fields.as_object().unwrap() {
        let float = matches!(
            msg.get(key).unwrap_or_else(|| panic!("{name} has no field {key}")),
            Value::Float(_)
        );
        let converted = if float {
            Value::Float(number(value))
        } else {
            Value::Int(value.as_i64().unwrap() as i128)
        };
        msg.set(key, converted).unwrap();
    }
    msg
}

/// MISSION_CURRENT without the mission id, which the dialect has no field for.
fn current_of(fields: &Json) -> (Message, i64) {
    let mut fields = fields.as_object().cloned().unwrap();
    let id = fields.remove("mission_id").and_then(|v| v.as_i64()).unwrap_or(0);
    (build("MISSION_CURRENT", &Json::Object(fields)), id)
}

struct Queued {
    time: f64,
    seq: u64,
    kind: &'static str,
    payload: Json,
}

#[derive(Default)]
struct SimState {
    queue: Vec<Queued>,
    n: u64,
    upload_count: i64,
    upload_type: i64,
    count_sends: i64,
    list_requests: i64,
    item_requests: std::collections::HashMap<i64, i64>,
}

struct Inner {
    t: f64,
    log: Vec<Json>,
    last_error: String,
    subs: Vec<(u64, ConsoleSub)>,
    next_sub: u64,
    swapped: bool,
    sim: SimState,
}

struct Fake {
    sc: Json,
    engine: Arc<MissionEngine>,
    lock: PriorityLock,
    inner: Mutex<Inner>,
}

impl Fake {
    fn new(sc: &Json, engine: Arc<MissionEngine>) -> Fake {
        Fake {
            sc: sc.clone(),
            engine,
            lock: PriorityLock::new(),
            inner: Mutex::new(Inner {
                t: 1000.0,
                log: Vec::new(),
                last_error: String::new(),
                subs: Vec::new(),
                next_sub: 1,
                swapped: false,
                sim: SimState::default(),
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn stack(&self) -> Dialect {
        dialect_for_stack(Stack::from_name(self.sc["stack"].as_str().unwrap_or("px4")))
    }

    fn vehicle(&self, key: &str) -> &Json {
        &self.sc["vehicle"][key]
    }

    fn schedule(&self, delay: f64, kind: &'static str, payload: Json) {
        let mut s = self.st();
        s.sim.n += 1;
        let (time, seq) = (s.t + delay, s.sim.n);
        s.sim.queue.push(Queued {
            time,
            seq,
            kind,
            payload,
        });
    }

    fn publish(&self, entry: ConsoleEntry) {
        let subs: Vec<ConsoleSub> = self.st().subs.iter().map(|(_, f)| Arc::clone(f)).collect();
        for sub in subs {
            sub(&entry);
        }
    }

    fn on_send(&self, name: &str, args: &[Json]) {
        let delay = |key: &str| self.vehicle(key).as_f64().unwrap_or(0.05);
        match name {
            "mission_count_send" => {
                let count = args[2].as_i64().unwrap();
                let kind = args.get(3).and_then(Json::as_i64).unwrap_or(0);
                let drop = {
                    let mut s = self.st();
                    s.sim.count_sends += 1;
                    s.sim.count_sends <= self.vehicle("drop_counts").as_i64().unwrap_or(0)
                };
                if drop {
                    return;
                }
                {
                    let mut s = self.st();
                    s.sim.upload_count = count;
                    s.sim.upload_type = kind;
                }
                if count == 0 {
                    self.schedule(delay("ack_delay"), "ack", json!(kind));
                } else {
                    self.schedule(delay("req_delay"), "request", json!(0));
                }
            }
            "mission_item_int_send" | "mission_item_send" => {
                let seq = args[2].as_i64().unwrap();
                let (count, kind) = {
                    let s = self.st();
                    (s.sim.upload_count, s.sim.upload_type)
                };
                if seq + 1 >= count {
                    self.schedule(delay("ack_delay"), "ack", json!(kind));
                } else if self.vehicle("stall_after").as_i64().is_some_and(|after| seq >= after) {
                } else {
                    self.schedule(delay("item_delay"), "request", json!(seq + 1));
                }
            }
            "mission_request_list_send" => {
                let requests = {
                    let mut s = self.st();
                    s.sim.list_requests += 1;
                    s.sim.list_requests
                };
                if requests <= self.vehicle("drop_lists").as_i64().unwrap_or(0) {
                    return;
                }
                let mission = self.vehicle("mission").as_array().map_or(0, Vec::len) as i64;
                let changes = self.vehicle("count_changes").as_bool().unwrap_or(false) && requests >= 2;
                let count = self
                    .vehicle("count_override")
                    .as_i64()
                    .unwrap_or(mission + i64::from(changes));
                self.schedule(delay("count_delay"), "count", json!(count));
            }
            "mission_request_int_send" => {
                let seq = args[2].as_i64().unwrap();
                let seen = {
                    let mut s = self.st();
                    let n = s.sim.item_requests.entry(seq).or_insert(0);
                    *n += 1;
                    *n
                };
                let drops = self.vehicle("drop_items")[seq.to_string()].as_i64().unwrap_or(0);
                if seen <= drops {
                    if self.vehicle("unexpected").as_bool().unwrap_or(false) {
                        self.schedule(
                            0.01,
                            "console",
                            json!({"name": "STATUSTEXT", "text": "Req. WP was unexpected", "level": "warning"}),
                        );
                    }
                    return;
                }
                self.schedule(delay("item_delay"), "item", json!(seq));
            }
            _ => {}
        }
    }

    fn deliver(&self, q: Queued) {
        let as_int = self.vehicle("request_as_int").as_bool().unwrap_or(true);
        match q.kind {
            "request" => {
                let kind = self.st().sim.upload_type;
                let name = if as_int {
                    "MISSION_REQUEST_INT"
                } else {
                    "MISSION_REQUEST"
                };
                let msg = build(name, &json!({"seq": q.payload, "mission_type": kind}));
                self.engine.handle_mission_request(self, &msg, as_int);
            }
            "ack" => {
                let ack_type = self.vehicle("ack_type").as_i64().unwrap_or(0);
                let msg = build("MISSION_ACK", &json!({"type": ack_type, "mission_type": q.payload}));
                self.engine.handle_mission_ack(self, &msg);
                if let Some(current) = self.vehicle("current_after_ack").as_object() {
                    self.schedule(current["delay"].as_f64().unwrap(), "current", current["fields"].clone());
                }
                for line in self.vehicle("status_after_ack").as_array().into_iter().flatten() {
                    self.schedule(
                        line["delay"].as_f64().unwrap(),
                        "console",
                        json!({"name": "STATUSTEXT", "text": line["text"], "level": line["level"]}),
                    );
                }
            }
            "count" => {
                let msg = build("MISSION_COUNT", &json!({"count": q.payload, "mission_type": 0}));
                self.engine.handle_mission_count(self, &msg);
            }
            "item" => {
                let seq = q.payload.as_i64().unwrap();
                let item = &self.vehicle("mission")[seq as usize];
                let mut fields = Map::new();
                fields.insert("seq".into(), json!(seq));
                fields.insert("frame".into(), item["frame"].clone());
                fields.insert("command".into(), item["command"].clone());
                fields.insert("current".into(), json!(0));
                fields.insert(
                    "autocontinue".into(),
                    item.get("autocontinue").cloned().unwrap_or(json!(1)),
                );
                fields.insert("z".into(), item.get("z").cloned().unwrap_or(json!(0.0)));
                fields.insert("mission_type".into(), json!(0));
                let params = item.get("params").cloned().unwrap_or(json!([0.0, 0.0, 0.0, 0.0]));
                for (n, value) in params.as_array().unwrap().iter().enumerate() {
                    fields.insert(format!("param{}", n + 1), value.clone());
                }
                if as_int {
                    fields.insert("x".into(), item["x"].clone());
                    fields.insert("y".into(), item["y"].clone());
                    let msg = build("MISSION_ITEM_INT", &Json::Object(fields));
                    self.engine.handle_mission_item(self, &msg, true);
                } else {
                    fields.insert("x".into(), json!(item["x"].as_f64().unwrap() / 1e7));
                    fields.insert("y".into(), json!(item["y"].as_f64().unwrap() / 1e7));
                    let msg = build("MISSION_ITEM", &Json::Object(fields));
                    self.engine.handle_mission_item(self, &msg, false);
                }
            }
            "current" => {
                let (msg, id) = current_of(&q.payload);
                self.engine.handle_mission_current_id(self, &msg, id);
            }
            "console" => self.publish(ConsoleEntry {
                name: q.payload["name"].as_str().unwrap().into(),
                text: q.payload["text"].as_str().unwrap().into(),
                level: q.payload["level"].as_str().unwrap().into(),
            }),
            other => panic!("{other}"),
        }
    }
}

impl MissionHost for Fake {
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
    fn console(&self, source: &str, text: &str, level: &str) {
        self.st().log.push(json!(["console", source, text, level]));
        self.publish(ConsoleEntry {
            name: source.into(),
            text: text.into(),
            level: level.into(),
        });
    }
    fn log(&self, level: &str, text: &str) {
        self.st().log.push(json!(["log", level, text]));
    }
    fn command_failure(&self, action: &str, result: i64) -> bool {
        self.st().log.push(json!(["failure", action, result]));
        false
    }
    fn lock(&self) -> &PriorityLock {
        &self.lock
    }
    fn abort_waiting(&self) -> bool {
        self.sc["abort_at"].as_f64().is_some_and(|at| self.st().t >= at)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        let (swap_at, swapped) = (self.sc["link_lost_at"].as_f64(), self.st().swapped);
        if let Some(at) = swap_at {
            if !swapped && self.st().t >= at {
                self.st().swapped = true;
                return Ok(false);
            }
        }
        let int = |name: &str| json!(msg.get(name).and_then(Value::as_i64).unwrap());
        let float = |name: &str| match msg.get(name) {
            Some(Value::Float(v)) => f(*v),
            other => panic!("{name}: {other:?}"),
        };
        let mut args = vec![int("target_system"), int("target_component")];
        let mission_type = msg.get("mission_type").and_then(Value::as_i64).unwrap_or(0);
        let method = match msg.name() {
            "MISSION_COUNT" => {
                args.push(int("count"));
                if mission_type != 0 {
                    args.push(json!(mission_type));
                }
                "mission_count_send"
            }
            "MISSION_ITEM_INT" | "MISSION_ITEM" => {
                for name in ["seq", "frame", "command", "current", "autocontinue"] {
                    args.push(int(name));
                }
                for name in ["param1", "param2", "param3", "param4"] {
                    args.push(float(name));
                }
                if msg.name() == "MISSION_ITEM_INT" {
                    args.push(int("x"));
                    args.push(int("y"));
                } else {
                    args.push(float("x"));
                    args.push(float("y"));
                }
                args.push(float("z"));
                if mission_type != 0 {
                    args.push(json!(mission_type));
                }
                if msg.name() == "MISSION_ITEM_INT" {
                    "mission_item_int_send"
                } else {
                    "mission_item_send"
                }
            }
            "MISSION_REQUEST_LIST" => {
                args.push(json!(mission_type));
                "mission_request_list_send"
            }
            "MISSION_REQUEST_INT" => {
                args.push(int("seq"));
                args.push(json!(mission_type));
                "mission_request_int_send"
            }
            "MISSION_ACK" => {
                args.push(int("type"));
                args.push(json!(mission_type));
                "mission_ack_send"
            }
            other => panic!("{other}"),
        };
        if self.sc["send_fails"].as_str() == Some(method) {
            return Err("boom".into());
        }
        self.st().log.push(json!(["send", method, args]));
        self.on_send(method, &args);
        Ok(true)
    }
    fn target(&self) -> (u8, u8) {
        (1, 1)
    }
    fn ack_is_for_us(&self, _: &Message) -> bool {
        self.sc["ours"].as_bool().unwrap_or(true)
    }
    fn now(&self) -> f64 {
        self.st().t
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
                    .sim
                    .queue
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| (a.time, a.seq).partial_cmp(&(b.time, b.seq)).unwrap())
                    .map(|(i, _)| i);
                match pick {
                    Some(i) if s.sim.queue[i].time <= deadline => {
                        let q = s.sim.queue.remove(i);
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
    fn stopping(&self) -> bool {
        self.sc["stop_at"].as_f64().is_some_and(|at| self.st().t >= at)
    }
    fn is_slow_link(&self) -> bool {
        self.sc["slow_link"].as_bool().unwrap_or(false)
    }
    fn dialect(&self) -> Dialect {
        self.stack()
    }
    fn mav_type(&self) -> i64 {
        self.sc["mav_type"].as_i64().unwrap_or(2)
    }
    fn store_update(&self, fields: Map<String, Json>) {
        self.st().log.push(json!(["update", Json::Object(fields)]));
    }
    fn home(&self) -> Vec<f64> {
        self.sc["home"]
            .as_array()
            .map(|h| h.iter().map(|v| v.as_f64().unwrap()).collect())
            .unwrap_or_default()
    }
    fn home_alt_amsl(&self) -> Option<f64> {
        self.sc["home_amsl"].as_f64()
    }
    fn add_console_sub(&self, sub: ConsoleSub) -> u64 {
        let mut s = self.st();
        let id = s.next_sub;
        s.next_sub += 1;
        s.subs.push((id, sub));
        id
    }
    fn remove_console_sub(&self, id: u64) {
        self.st().subs.retain(|(i, _)| *i != id);
    }
    fn lower_circles(&self, items: &[PlanItem]) -> Vec<PlanItem> {
        let mut out = Vec::new();
        for item in items {
            if item.command == 18 {
                for _ in 0..2 {
                    out.push(PlanItem {
                        command: 16,
                        ..item.clone()
                    });
                }
            } else {
                out.push(item.clone());
            }
        }
        out
    }
    fn items_to_plan(&self, items: &[DownloadedItem], home: Option<(f64, f64)>, home_alt: Option<f64>) -> Json {
        json!({
            "plan_index": (0..items.len()).collect::<Vec<_>>(),
            "home": home.map_or(Json::Null, |(lat, lon)| json!({"lat": lat, "lon": lon})),
            "home_alt": home_alt,
        })
    }
}

fn state_of(engine: &MissionEngine) -> Json {
    json!({
        "revision": engine.mission_revision(),
        "index": engine.vehicle_mission_index(),
        "id": engine.vehicle_mission_id(),
        "confirmed": engine.vehicle_mission_confirmed(),
        "fence": engine.vehicle_fence().map(|p| p.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>()),
    })
}

fn run_step(engine: &Arc<MissionEngine>, host: &Fake, step: &Json) -> Json {
    let items = |key: &str| -> Vec<Json> { step[key].as_array().cloned().unwrap_or_default() };
    match step["op"].as_str().unwrap() {
        "capabilities" => engine.mission_capabilities(host),
        "upload" => {
            let specs: Vec<MissionItem> = items("items").iter().map(item_of).collect();
            json!(engine.upload_mission(host, &specs, step["type"].as_i64().unwrap_or(0)))
        }
        "clear" => json!(engine.clear_mission(host)),
        "upload_plan" => {
            let plan: Vec<PlanItem> = items("items").iter().map(plan_of).collect();
            json!(engine.upload_mission_plan(host, &plan))
        }
        "fence" => {
            let polygon: Vec<(f64, f64)> = items("polygon")
                .iter()
                .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
                .collect();
            json!(engine.upload_fence(host, &polygon))
        }
        "clear_fence" => json!(engine.clear_fence(host)),
        "download" => engine.download_mission(host).unwrap_or(Json::Null),
        "current" => {
            let (msg, id) = current_of(&step["fields"]);
            engine.handle_mission_current_id(host, &msg, id);
            Json::Null
        }
        "reached" => {
            engine.handle_mission_item_reached(host, &build("MISSION_ITEM_REACHED", &step["fields"]));
            Json::Null
        }
        "note" => {
            let index: Option<Vec<Option<i64>>> =
                step["index"].as_array().map(|a| a.iter().map(Json::as_i64).collect());
            engine.note_vehicle_mission(host, index.as_deref());
            Json::Null
        }
        "forget" => {
            engine.forget_vehicle_mission(host, step["reason"].as_str().unwrap());
            Json::Null
        }
        "advance" => {
            host.st().t += step["seconds"].as_f64().unwrap();
            Json::Null
        }
        "serve" => {
            let specs: Vec<MissionItem> = items("items").iter().map(item_of).collect();
            engine.arm_upload_for_test(specs, step["type"].as_i64().unwrap_or(0));
            let as_int = step["as_int"].as_bool().unwrap();
            let name = if as_int {
                "MISSION_REQUEST_INT"
            } else {
                "MISSION_REQUEST"
            };
            let fields = json!({"seq": step["seq"], "mission_type": step["mission_type"].as_i64().unwrap_or(0)});
            engine.handle_mission_request(host, &build(name, &fields), as_int);
            json!(engine.last_request())
        }
        "stray_count" => {
            engine.handle_mission_count(host, &build("MISSION_COUNT", &json!({"count": step["count"]})));
            Json::Null
        }
        "stray_item" => {
            engine.handle_mission_item(host, &build("MISSION_ITEM_INT", &json!({"seq": step["seq"]})), true);
            Json::Null
        }
        "stray_ack" => {
            engine.handle_mission_ack(host, &build("MISSION_ACK", &json!({"type": step["type"]})));
            Json::Null
        }
        "spec" => {
            let a: Vec<f64> = step["args"].as_array().unwrap().iter().map(number).collect();
            item_json(&build_mission_item_spec(
                a[0] as i64,
                a[1] as i64,
                a[2],
                a[3],
                a[4],
                a[5],
                a[6],
                a[7],
                a[8],
                step["frame"].as_i64(),
            ))
        }
        "home_slot" => {
            let specs: Vec<MissionItem> = items("items").iter().map(item_of).collect();
            let out = with_mission_home_slot(specs, &host.stack(), &host.home(), host.home_alt_amsl());
            json!(out.iter().map(item_json).collect::<Vec<_>>())
        }
        "takeoff_pos" => {
            let first: Vec<f64> = step["first"].as_array().unwrap().iter().map(number).collect();
            let list = |key: &str| -> Vec<f64> {
                host.sc[key]
                    .as_array()
                    .map(|a| a.iter().map(|v| v.as_f64().unwrap()).collect())
                    .unwrap_or_default()
            };
            let (lat, lon) = takeoff_item_position((first[0], first[1], first[2]), &list("position"), &list("home"));
            json!([lat, lon])
        }
        "ack_result" => json!(mission_ack_to_result(step["ack"].as_i64().unwrap())),
        "validate" => {
            let ground = corvus_core::dialect::is_ground_vehicle(host.mav_type());
            match validate_fly_to_points(&step["points"], ground, &|text| host.set_error(text)) {
                Some(points) => json!(points.iter().map(|(a, b, c)| json!([a, b, c])).collect::<Vec<_>>()),
                None => Json::Null,
            }
        }
        "state" => state_of(engine),
        other => panic!("{other}"),
    }
}

#[test]
fn every_scenario_ends_where_the_python_mission_protocol_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/missions_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 85);
    let mut failures = Vec::new();
    for case in cases {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let engine = Arc::new(MissionEngine::new());
        let host = Fake::new(sc, Arc::clone(&engine));
        let expected = &case["expected"];
        let mut bad = false;
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
                    "{name} step {i} ({}): result {result} (want {}), events:\n{}\nwant:\n{}",
                    step["op"],
                    want["result"],
                    serde_json::to_string(&events).unwrap(),
                    want["events"]
                ));
                bad = true;
                break;
            }
            if (t - 1000.0 - want["t"].as_f64().unwrap()).abs() > 1e-5 {
                failures.push(format!(
                    "{name} step {i} ({}): took {}, want {}",
                    step["op"],
                    t - 1000.0,
                    want["t"]
                ));
                bad = true;
                break;
            }
        }
        if !bad {
            let s = host.st();
            let got = json!({"last_error": s.last_error, "subs": s.subs.len()});
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
