//! The Rust vehicle list plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`): what the
//! tracker keeps of a vehicle, the snapshot, colours, names, links coming and
//! going, notifications about the vehicles not flown, and the config block.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use corvus_core::clock::Clock;
use corvus_core::fleet::*;
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::protocol::receive::{Frame, Payload};
use serde_json::{json, Map, Value as Json};

struct TestClock(Mutex<f64>);

impl Clock for TestClock {
    fn monotonic(&self) -> f64 {
        *self.0.lock().unwrap()
    }

    fn local_hms(&self) -> String {
        "00:00:00".into()
    }
}

type Log = Arc<Mutex<Vec<Json>>>;

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

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn build(name: &str, fields: &Json) -> Message {
    let mut msg = Message::named(name, Wire::V2).unwrap();
    for (key, value) in fields.as_object().unwrap() {
        let current = msg
            .get(key)
            .unwrap_or_else(|| panic!("{name} has no field {key}"))
            .clone();
        let converted = match (&current, value) {
            (Value::Bytes(old), Json::String(text)) => {
                let mut bytes = text.as_bytes().to_vec();
                bytes.resize(old.len(), 0);
                Value::Bytes(bytes)
            }
            (Value::Float(_), v) => Value::Float(v.as_f64().unwrap()),
            (_, v) => Value::Int(v.as_i64().unwrap() as i128),
        };
        msg.set(key, converted).unwrap();
    }
    msg
}

struct FakeStore {
    link: Mutex<String>,
    snap: Mutex<Map<String, Json>>,
    log: Log,
}

impl FakeStore {
    fn new(log: &Log, link: &str) -> Arc<FakeStore> {
        Arc::new(FakeStore {
            link: Mutex::new(link.into()),
            snap: Mutex::new(Map::new()),
            log: Arc::clone(log),
        })
    }

    fn note(&self, parts: Vec<Json>) {
        let mut entry = vec![json!("store"), json!(self.link.lock().unwrap().clone())];
        entry.extend(parts);
        self.log.lock().unwrap().push(Json::Array(entry));
    }
}

impl FleetStore for FakeStore {
    fn get(&self, key: &str) -> Option<Json> {
        self.snap.lock().unwrap().get(key).cloned()
    }

    fn update(&self, fields: Vec<(String, Json)>) {
        let mut map = Map::new();
        for (k, v) in fields {
            self.snap.lock().unwrap().insert(k.clone(), v.clone());
            map.insert(k, v);
        }
        self.note(vec![json!("update"), Json::Object(map)]);
    }

    fn forget_vehicle(&self) {
        self.note(vec![json!("forget_vehicle")]);
    }

    fn shutdown(&self) {
        self.note(vec![json!("shutdown")]);
    }
}

struct FakeBridge {
    link: String,
    conn: String,
    tracker: FleetTracker,
    controlled: Mutex<Option<i64>>,
    connected: Mutex<bool>,
    accept: Mutex<bool>,
    log: Log,
}

impl FakeBridge {
    fn new(log: &Log, clock: &Arc<TestClock>, link: &str, conn: &str) -> Arc<FakeBridge> {
        Arc::new(FakeBridge {
            link: link.into(),
            conn: conn.into(),
            tracker: FleetTracker::with_clock(clock.clone()),
            controlled: Mutex::new(None),
            connected: Mutex::new(true),
            accept: Mutex::new(true),
            log: Arc::clone(log),
        })
    }

    fn note(&self, what: &str, extra: Option<String>) {
        let mut entry = vec![json!("bridge"), json!(self.link), json!(what)];
        entry.extend(extra.map(Json::from));
        self.log.lock().unwrap().push(Json::Array(entry));
    }
}

impl FleetBridge for FakeBridge {
    fn fleet_tracker(&self) -> &FleetTracker {
        &self.tracker
    }

    fn connection_string(&self) -> String {
        self.conn.clone()
    }

    fn controlled_system(&self) -> Option<i64> {
        *self.controlled.lock().unwrap()
    }

    fn validate_connection(&self, connection: &str) -> Result<(), String> {
        if connection.starts_with("bad:") {
            Err(format!("{connection} is not a connection string"))
        } else {
            Ok(())
        }
    }

    fn serial_device(&self) -> String {
        match self.conn.strip_prefix("serial:") {
            Some(_) => corvus_core::bridge::split_serial_connection(&self.conn).0,
            None => String::new(),
        }
    }

    fn start(&self) -> Result<(), String> {
        self.note("start", None);
        Ok(())
    }

    fn stop(&self) {
        self.note("stop", None);
    }

    fn is_connected(&self) -> bool {
        *self.connected.lock().unwrap()
    }

    fn inject_rtcm(&self, frame: &[u8]) -> bool {
        self.note("rtcm", Some(to_hex(frame)));
        *self.accept.lock().unwrap()
    }
}

struct World {
    clock: Arc<TestClock>,
    log: Log,
    bridges: Arc<Mutex<HashMap<String, Arc<FakeBridge>>>>,
    stores: Arc<Mutex<HashMap<String, Arc<FakeStore>>>>,
    fleet: Arc<Fleet>,
    listeners: HashMap<String, u64>,
}

fn world() -> World {
    let clock = Arc::new(TestClock(Mutex::new(1000.0)));
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let bridges: Arc<Mutex<HashMap<String, Arc<FakeBridge>>>> = Arc::default();
    let stores: Arc<Mutex<HashMap<String, Arc<FakeStore>>>> = Arc::default();
    let main_bridge = FakeBridge::new(&log, &clock, "main", "udp:127.0.0.1:14550");
    let main_store = FakeStore::new(&log, "main");
    bridges.lock().unwrap().insert("main".into(), main_bridge.clone());
    stores.lock().unwrap().insert("main".into(), main_store.clone());

    let (factory_log, factory_clock, factory_bridges, factory_stores) = (
        Arc::clone(&log),
        clock.clone(),
        Arc::clone(&bridges),
        Arc::clone(&stores),
    );
    let bridge_factory: BridgeFactory = Box::new(move |link, store, connection| {
        if connection.starts_with("fail:") {
            return Err("no such device".into());
        }
        let _ = store;
        let bridge = FakeBridge::new(&factory_log, &factory_clock, link, connection);
        let pending = factory_stores.lock().unwrap().remove("pending");
        if let Some(store) = pending {
            *store.link.lock().unwrap() = link.to_string();
            factory_stores.lock().unwrap().insert(link.to_string(), store);
        }
        factory_bridges.lock().unwrap().insert(link.to_string(), bridge.clone());
        Ok(bridge as Arc<dyn FleetBridge>)
    });
    let (store_log, store_stores) = (Arc::clone(&log), Arc::clone(&stores));
    let store_factory: StoreFactory = Box::new(move || {
        let store = FakeStore::new(&store_log, "pending");
        store_stores.lock().unwrap().insert("pending".into(), store.clone());
        store as Arc<dyn FleetStore>
    });
    let persist_log = Arc::clone(&log);
    let persist: Persist = Box::new(move |config| {
        persist_log.lock().unwrap().push(json!(["persist", config]));
    });
    let fleet = Arc::new(
        Fleet::with_clock(main_bridge, main_store, clock.clone())
            .with_factories(bridge_factory, store_factory)
            .with_persist(persist),
    );
    World {
        clock,
        log,
        bridges,
        stores,
        fleet,
        listeners: HashMap::new(),
    }
}

impl World {
    fn bridge(&self, link: &str) -> Arc<FakeBridge> {
        self.bridges
            .lock()
            .unwrap()
            .get(link)
            .unwrap_or_else(|| panic!("no bridge {link}"))
            .clone()
    }

    fn observe(&self, link: &str, spec: &Json) {
        let message = build(spec["name"].as_str().unwrap(), &spec["fields"]);
        let frame = Frame {
            payload: Payload::Message(&message),
            system: spec["system"].as_i64().unwrap_or(1),
            component: spec["component"].as_i64().unwrap_or(1),
            raw: None,
        };
        self.bridge(link)
            .tracker
            .observe(&frame, spec["vh"].as_bool().unwrap_or(false), None);
    }

    fn step(&mut self, step: &Json) -> Json {
        let op = step["op"].as_str().unwrap();
        let f = Arc::clone(&self.fleet);
        match op {
            "advance" => {
                *self.clock.0.lock().unwrap() += step["seconds"].as_f64().unwrap();
                Json::Null
            }
            "bind" | "bind_extra" => {
                let link = step.get("link").or_else(|| step.get("id")).unwrap().as_str().unwrap();
                self.bridge(link)
                    .tracker
                    .bind(step["system"].as_i64().unwrap(), step["component"].as_i64().unwrap());
                Json::Null
            }
            "frame" => {
                self.observe(step["link"].as_str().unwrap(), step);
                Json::Null
            }
            "frame_extra" => {
                self.observe(step["id"].as_str().unwrap(), &step["spec"]);
                Json::Null
            }
            "forget" => {
                self.bridge(step["link"].as_str().unwrap()).tracker.forget();
                Json::Null
            }
            "systems" => {
                let systems = self.bridge(step["link"].as_str().unwrap()).tracker.systems(None);
                Json::Array(systems.into_iter().map(Json::Object).collect())
            }
            "events" => {
                let (last, fresh) = self
                    .bridge(step["link"].as_str().unwrap())
                    .tracker
                    .events_since(step["since"].as_u64().unwrap());
                json!({"last": last, "fresh": fresh.iter().map(|(s, v, t)| json!([s, v, t])).collect::<Vec<_>>()})
            }
            "controlled" => {
                *self.bridge(step["link"].as_str().unwrap()).controlled.lock().unwrap() = step["system"].as_i64();
                Json::Null
            }
            "store" => {
                let store = self
                    .stores
                    .lock()
                    .unwrap()
                    .get(step["link"].as_str().unwrap())
                    .unwrap()
                    .clone();
                for (k, v) in step["fields"].as_object().unwrap() {
                    store.snap.lock().unwrap().insert(k.clone(), v.clone());
                }
                Json::Null
            }
            "add_link" => {
                let (id, error) = f.add_link(step["connection"].as_str().unwrap());
                json!([id, error])
            }
            "remove_link" => json!(f.remove_link(step["id"].as_str().unwrap())),
            "remove_vehicle" => {
                let (why, status) = f.remove_vehicle(
                    step["key"].as_str().unwrap(),
                    step["confirm"].as_bool().unwrap_or(false),
                );
                json!([why, status])
            }
            "select" => json!(f.select(step["key"].as_str().unwrap())),
            "set_meta" => json!(f.set_meta(step["key"].as_str().unwrap(), step.get("name"), step.get("color"))),
            "snapshot" => f.snapshot(),
            "config" => f.to_config().unwrap_or(Json::Null),
            "load" => {
                f.load(&step["block"]);
                Json::Null
            }
            "collect" => {
                f.collect_events(None);
                Json::Null
            }
            "publish" => {
                f.publish(step["force"].as_bool().unwrap());
                Json::Null
            }
            "listener" => {
                let name = step["name"].as_str().unwrap().to_string();
                let log = Arc::clone(&self.log);
                let tag = name.clone();
                let id = f.add_listener(Arc::new(move |snapshot: &Json| {
                    log.lock().unwrap().push(json!(["listener", tag, snapshot]));
                }));
                self.listeners.insert(name, id);
                Json::Null
            }
            "remove_listener" => {
                f.remove_listener(self.listeners[step["name"].as_str().unwrap()]);
                Json::Null
            }
            "in_use" => {
                json!(f.connection_in_use(step["connection"].as_str().unwrap(), step["except"].as_str().unwrap()))
            }
            "serial_devices" => json!(f.extra_serial_devices()),
            "start" => {
                f.start();
                f.stop();
                Json::Null
            }
            "stop" => Json::Null,
            "coerce" => coerce_config(&step["raw"]).unwrap_or(Json::Null),
            "clean" => {
                let value = &step["value"];
                match step["fn"].as_str().unwrap() {
                    "name" => json!(clean_name(value)),
                    "color" => json!(clean_color(value)),
                    _ => value
                        .as_str()
                        .and_then(parse_vehicle_key)
                        .map_or(Json::Null, |(link, system)| json!([link, system])),
                }
            }
            "inject" => json!(f.inject_rtcm(&hex(step["frame"].as_str().unwrap()))),
            "connected" => json!(f.is_connected()),
            "link_connected" => {
                *self.bridge(step["link"].as_str().unwrap()).connected.lock().unwrap() =
                    step["value"].as_bool().unwrap();
                Json::Null
            }
            "link_accept" => {
                *self.bridge(step["link"].as_str().unwrap()).accept.lock().unwrap() = step["value"].as_bool().unwrap();
                Json::Null
            }
            other => panic!("{other}"),
        }
    }
}

#[test]
fn the_vehicle_list_answers_like_python() {
    let data: Json = serde_json::from_str(include_str!("data/fleet_cases.json")).unwrap();
    let mut failures = Vec::new();
    for case in data["cases"].as_array().unwrap() {
        let name = case["scenario"]["name"].as_str().unwrap();
        let mut w = world();
        for (i, step) in case["scenario"]["steps"].as_array().unwrap().iter().enumerate() {
            let before = w.log.lock().unwrap().len();
            let result = w.step(step);
            let log = w.log.lock().unwrap()[before..].to_vec();
            let want = &case["expected"]["steps"][i];
            let active = w.fleet.active_link().id.clone();
            let t = ((*w.clock.0.lock().unwrap() - 1000.0) * 1e6).round() / 1e6;
            let got = json!({"result": result, "log": log, "active": active, "t": t});
            if !same(&got, want) {
                failures.push(format!(
                    "{name} step {i} ({}):\n  got  {got}\n  want {want}",
                    step["op"]
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
