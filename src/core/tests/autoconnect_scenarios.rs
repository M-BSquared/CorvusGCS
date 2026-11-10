//! The Rust auto connect decides what the Python one decided
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`): which
//! port is picked, the connection string at startup, what is suggested, and
//! the watcher's passes on a scripted bridge and store.

use std::sync::{Arc, Mutex};

use corvus_core::autoconnect::*;
use corvus_core::serial_ports::PortRow;
use serde_json::{json, Map, Value as Json};

fn rows(v: &Json) -> Vec<PortRow> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| PortRow {
            device: r["device"].as_str().unwrap().into(),
            description: r["description"].as_str().unwrap().into(),
            hwid: r["hwid"].as_str().unwrap().into(),
        })
        .collect()
}

fn named_rows(sets: &Json, name: &str) -> Vec<PortRow> {
    rows(&sets[name])
}

fn info_json(p: &PortInfo) -> Json {
    json!({"device": p.device, "description": p.description, "hwid": p.hwid, "kind": p.kind})
}

#[test]
fn the_pure_decisions_are_the_pythons() {
    let data: Json = serde_json::from_str(include_str!("data/autoconnect_cases.json")).unwrap();
    let pure = &data["cases"][0]["pure"];
    let sets = &pure["sets"];
    let mut failures: Vec<String> = Vec::new();
    // The row sets are the scenario fixture's own; rebuild them from the classify cases.
    let mut by_name: std::collections::HashMap<String, Vec<PortInfo>> = Default::default();
    for case in pure["classify"].as_array().unwrap() {
        let name = case[0].as_str().unwrap().to_string();
        let ports: Vec<PortInfo> = case[1]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| PortInfo {
                device: p["device"].as_str().unwrap().into(),
                description: p["description"].as_str().unwrap().into(),
                hwid: p["hwid"].as_str().unwrap().into(),
                kind: p["kind"].as_str().unwrap().into(),
            })
            .collect();
        by_name.insert(name, ports);
    }
    for case in pure["classify"].as_array().unwrap() {
        let got = Json::Array(
            classify_ports(&named_rows(sets, case[0].as_str().unwrap()))
                .iter()
                .map(info_json)
                .collect(),
        );
        if got != case[1] {
            failures.push(format!("classify {}: {got} != {}", case[0], case[1]));
        }
    }
    for case in pure["pick"].as_array().unwrap() {
        let ports = &by_name[case[0].as_str().unwrap()];
        let got = pick_usb_serial(ports, case[1].as_bool().unwrap(), case[2].as_bool().unwrap())
            .map_or(Json::Null, |(d, b, k)| json!([d, b, k]));
        if got != case[3] {
            failures.push(format!(
                "pick {} {} {}: {got} != {}",
                case[0], case[1], case[2], case[3]
            ));
        }
    }
    let validator = |c: &str| corvus_core::bridge::validate_connection(c);
    for case in pure["resolve"].as_array().unwrap() {
        let ports = &by_name[case[0].as_str().unwrap()];
        let flags = case[3].as_array().unwrap();
        let options = StartupOptions {
            usb_enabled: flags[0].as_bool().unwrap(),
            sik_enabled: flags[1].as_bool().unwrap(),
            udp_fallback_enabled: flags[2].as_bool().unwrap(),
            validator: if case[0] == "none" && case[1].is_null() && case[2].is_null() {
                None
            } else {
                Some(&validator)
            },
            ..StartupOptions::default()
        };
        let d = resolve_startup_connection(case[1].as_str(), case[2].as_str(), ports, &options);
        let got = json!([d.connection_string, d.reason, d.detail]);
        if got != case[4] {
            failures.push(format!(
                "resolve {} {} {} {}: {got} != {}",
                case[0], case[1], case[2], case[3], case[4]
            ));
        }
    }
    for case in pure["suggest"].as_array().unwrap() {
        let ports = &by_name[case[0].as_str().unwrap()];
        let f = case[2].as_array().unwrap();
        let session = SessionState::new();
        let block: Map<String, Json> = [("enabled", f[0].clone()), ("usb", f[2].clone()), ("sik", f[3].clone())]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        session.apply_config(Some(&block));
        if f[1].as_bool().unwrap() {
            session.note_manual_connect();
        }
        let snapshot = case[1].as_object().unwrap();
        let got = suggest_connection(snapshot, ports, &session).map_or(Json::Null, |s| {
            json!([s.connection_string, s.kind, s.device, "new-device-while-connected"])
        });
        if got != case[3] {
            failures.push(format!(
                "suggest {} {} {}: {got} != {}",
                case[0], case[1], case[2], case[3]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.iter().take(12).cloned().collect::<Vec<_>>().join("\n")
    );
}

struct State {
    rows: Vec<PortRow>,
    snapshot: Map<String, Json>,
    stale: bool,
    running: bool,
    conn: String,
    busy: Vec<String>,
    fails: bool,
    log: Vec<Json>,
}

struct Fake(Arc<Mutex<State>>);

impl AutoBridge for Fake {
    fn is_running(&self) -> bool {
        self.0.lock().unwrap().running
    }
    fn connection_string(&self) -> String {
        self.0.lock().unwrap().conn.clone()
    }
    fn validate_connection(&self, c: &str) -> Result<(), String> {
        corvus_core::bridge::validate_connection(c)
    }
    fn switch_connection(&self, c: &str) -> Result<(), String> {
        let mut s = self.0.lock().unwrap();
        s.log.push(json!(["switch", c]));
        if s.fails {
            return Err("busy".into());
        }
        s.conn = c.into();
        Ok(())
    }
}

impl AutoStore for Fake {
    fn snapshot(&self) -> Map<String, Json> {
        self.0.lock().unwrap().snapshot.clone()
    }
    fn is_stale(&self, _timeout: f64) -> bool {
        self.0.lock().unwrap().stale
    }
    fn update(&self, fields: Vec<(String, Json)>) {
        let mut s = self.0.lock().unwrap();
        let mut map = Map::new();
        for (k, v) in fields {
            s.snapshot.insert(k.clone(), v.clone());
            map.insert(k, v);
        }
        s.log.push(json!(["store", map]));
    }
}

#[test]
fn the_watcher_decides_like_python() {
    let data: Json = serde_json::from_str(include_str!("data/autoconnect_cases.json")).unwrap();
    let mut failures = Vec::new();
    for case in data["cases"].as_array().unwrap().iter().skip(1) {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let state = Arc::new(Mutex::new(State {
            rows: Vec::new(),
            snapshot: Map::new(),
            stale: true,
            running: true,
            conn: sc["conn"].as_str().unwrap().into(),
            busy: Vec::new(),
            fails: false,
            log: Vec::new(),
        }));
        let session = Arc::new(SessionState::new());
        let (rows_state, busy_state) = (Arc::clone(&state), Arc::clone(&state));
        let watcher = AutoConnectWatcher::new(
            Arc::new(Fake(Arc::clone(&state))),
            Arc::new(Fake(Arc::clone(&state))),
            Arc::clone(&session),
            Arc::new(move || rows_state.lock().unwrap().rows.clone()),
            Arc::new(move || busy_state.lock().unwrap().busy.clone()),
            WatcherConfig::default(),
        )
        .with_clock(Arc::new(|| 1234.5));
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = state.lock().unwrap().log.len();
            let mut result = Json::Null;
            match step["op"].as_str().unwrap() {
                "tick" => result = json!(watcher.tick()),
                "ports" => state.lock().unwrap().rows = rows(&step["rows"]),
                "snapshot" => {
                    for (k, v) in step["fields"].as_object().unwrap() {
                        state.lock().unwrap().snapshot.insert(k.clone(), v.clone());
                    }
                }
                "stale" => state.lock().unwrap().stale = step["value"].as_bool().unwrap(),
                "running" => state.lock().unwrap().running = step["value"].as_bool().unwrap(),
                "busy" => {
                    state.lock().unwrap().busy = step["devices"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|d| d.as_str().unwrap().into())
                        .collect()
                }
                "manual" => session.note_manual_connect(),
                "config" => session.apply_config(step["block"].as_object()),
                "switch_fails" => state.lock().unwrap().fails = step["value"].as_bool().unwrap(),
                other => panic!("{other}"),
            }
            let s = state.lock().unwrap();
            let got = json!({
                "result": result, "log": s.log[before..].to_vec(), "conn": s.conn, "session": session.as_value(),
                "last_dial": session.last_dial_key(),
                "last_suggestion": session.last_suggestion_key(),
            });
            let want = &case["expected"]["steps"][i];
            if &got != want {
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
