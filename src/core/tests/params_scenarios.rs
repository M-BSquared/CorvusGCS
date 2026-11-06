//! The Rust parameter engine plays every scenario the Python engine played
//! (`python/tools/dump_protocol_cases.py`) and must do the same: the messages
//! it sends, what it tells the listeners, the errors it sets, what it returns
//! and what its cache holds.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use corvus_core::link::plain::{defs::Wire, Message, Value};
use corvus_core::protocol::params::{Entry, ParamEngine, ParamHost, Status};
use corvus_core::protocol::RequestHost;
use serde_json::{json, Value as Json};

#[derive(Clone)]
struct VParam {
    name: String,
    value: f64,
    ptype: i64,
    bits: Option<u32>,
}

fn nan_text(v: f64) -> Json {
    if v.is_nan() {
        json!("nan")
    } else {
        json!(v)
    }
}

struct Fake {
    engine: Arc<ParamEngine>,
    sc: Json,
    clock: Mutex<f64>,
    sent: Mutex<Vec<Json>>,
    statuses: Mutex<Vec<Json>>,
    errors: Mutex<Vec<String>>,
    console: Mutex<Vec<Json>>,
    last_error: Mutex<String>,
    watchdog: AtomicUsize,
    running: AtomicBool,
    ticks: AtomicUsize,
    veh: Mutex<Vec<VParam>>,
}

impl Fake {
    fn new(sc: &Json) -> Arc<Fake> {
        let veh = sc["vehicle"]["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| VParam {
                name: p["name"].as_str().unwrap().into(),
                value: p["value"].as_f64().unwrap_or(0.0),
                ptype: p["type"].as_i64().unwrap(),
                bits: p["bits"].as_u64().map(|b| b as u32),
            })
            .collect();
        Arc::new(Fake {
            engine: Arc::new(ParamEngine::new()),
            sc: sc.clone(),
            clock: Mutex::new(1000.0),
            sent: Mutex::default(),
            statuses: Mutex::default(),
            errors: Mutex::default(),
            console: Mutex::default(),
            last_error: Mutex::default(),
            watchdog: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            ticks: AtomicUsize::new(0),
            veh: Mutex::new(veh),
        })
    }

    fn flag(&self, key: &str, default: bool) -> bool {
        self.sc[key].as_bool().unwrap_or(default)
    }

    fn encoding_name(&self) -> String {
        self.sc["encoding"].as_str().unwrap_or("bytewise").to_string()
    }

    fn deliver(&self, p: &VParam, index: i64, count: i64) {
        let (value, raw) = match p.bits {
            Some(b) => (f32::from_bits(b) as f64, b),
            None => ((p.value as f32) as f64, (p.value as f32).to_bits()),
        };
        self.deliver_raw(&p.name, value, p.ptype, index, count, raw);
    }

    fn deliver_raw(&self, name: &str, value: f64, ptype: i64, index: i64, count: i64, raw: u32) {
        let m = Message::named("PARAM_VALUE", Wire::V2)
            .unwrap()
            .with("param_id", Value::Bytes(name.as_bytes().to_vec()))
            .with("param_value", Value::Float(value))
            .with("param_type", Value::Int(ptype as i128))
            .with("param_index", Value::Int(index as i128))
            .with("param_count", Value::Int(count as i128));
        self.engine.handle_param_value(self, &m, Some(&raw.to_le_bytes()));
    }

    fn react(&self, method: &str, name: &str, index: i64, wire_value: f64) {
        let veh = &self.sc["vehicle"];
        let params = self.veh.lock().unwrap().clone();
        let count = veh["count"].as_i64().unwrap_or(params.len() as i64);
        let listed = |key: &str, i: usize| {
            veh[key]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_u64() == Some(i as u64)))
        };
        match method {
            "param_request_list_send" => {
                for (i, p) in params.iter().enumerate() {
                    if !listed("drop", i) {
                        self.deliver(p, i as i64, count);
                    }
                }
            }
            "param_request_read_send" if veh["answer_reads"].as_bool().unwrap_or(true) => {
                for (i, p) in params.iter().enumerate() {
                    if ((!name.is_empty() && p.name == name) || (name.is_empty() && i as i64 == index))
                        && !listed("never", i)
                    {
                        self.deliver(p, i as i64, count);
                    }
                }
            }
            "param_set_send" => {
                let mode = veh["echo"].as_str().unwrap_or("ok");
                for (i, p) in params.iter().enumerate() {
                    if p.name == name && mode != "none" {
                        let mut now = p.clone();
                        if mode == "ok" {
                            if self.encoding_name() == "bytewise" && p.ptype <= 6 {
                                now.bits = Some((wire_value as f32).to_bits());
                            } else {
                                now.bits = None;
                                now.value = wire_value;
                            }
                            self.veh.lock().unwrap()[i] = now.clone();
                        }
                        self.deliver(&now, i as i64, count);
                    }
                }
            }
            _ => {}
        }
    }
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::Bytes(b)) => {
            String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]).into_owned()
        }
        _ => String::new(),
    }
}

impl RequestHost for Fake {
    fn connection_ready(&self) -> bool {
        self.flag("ready", true)
    }
    fn armed(&self) -> bool {
        self.flag("armed", false)
    }
    fn send(&self, msg: Message) -> Result<bool, String> {
        let (method, args, name, index, wire_value): (&str, Vec<Json>, String, i64, f64) = match msg.name() {
            "PARAM_REQUEST_LIST" => ("param_request_list_send", vec![], String::new(), 0, 0.0),
            "PARAM_REQUEST_READ" => {
                let name = text_of(msg.get("param_id"));
                let index = msg.get("param_index").and_then(Value::as_i64).unwrap();
                (
                    "param_request_read_send",
                    vec![json!(name), json!(index)],
                    name,
                    index,
                    0.0,
                )
            }
            "PARAM_SET" => {
                let name = text_of(msg.get("param_id"));
                let value = msg.get("param_value").and_then(Value::as_f64).unwrap();
                let ptype = msg.get("param_type").and_then(Value::as_i64).unwrap();
                (
                    "param_set_send",
                    vec![json!(name), nan_text(value), json!(ptype)],
                    name,
                    0,
                    value,
                )
            }
            other => panic!("{other}"),
        };
        if self.sc["send_fails"].as_str() == Some(method) {
            return Err("write failed".into());
        }
        self.sent.lock().unwrap().push(json!([method, args]));
        self.react(method, &name, index, wire_value);
        Ok(true)
    }
    fn target(&self) -> (u8, u8) {
        (1, 1)
    }
    fn set_error(&self, text: &str) {
        *self.last_error.lock().unwrap() = text.to_string();
        self.errors.lock().unwrap().push(text.to_string());
    }
    fn last_error(&self) -> String {
        self.last_error.lock().unwrap().clone()
    }
    fn console(&self, source: &str, text: &str, level: &str) {
        self.console.lock().unwrap().push(json!([source, text, level]));
    }
}

impl ParamHost for Fake {
    fn now(&self) -> f64 {
        *self.clock.lock().unwrap()
    }
    fn sleep(&self, seconds: f64) {
        *self.clock.lock().unwrap() += seconds;
    }
    fn interruptible_sleep(&self, seconds: f64) {
        let ticks = self.ticks.fetch_add(1, Ordering::SeqCst) + 1;
        if ticks >= self.sc["max_ticks"].as_u64().unwrap_or(400) as usize {
            self.running.store(false, Ordering::SeqCst);
        }
        self.sleep(seconds);
    }
    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    fn stopping(&self) -> bool {
        false
    }
    fn encoding(&self) -> String {
        self.encoding_name()
    }
    fn slow_link(&self) -> bool {
        self.flag("slow", false)
    }
    fn notify(&self, status: &Status) {
        self.statuses.lock().unwrap().push(status.to_json());
    }
    fn start_watchdog(&self) {
        self.watchdog.fetch_add(1, Ordering::SeqCst);
    }
}

fn entry_json(e: &Entry) -> Json {
    json!({"name": e.name, "value": nan_text(e.value), "type": e.ptype, "index": e.index, "count": e.count})
}

fn pairs(list: &Json) -> Vec<(String, f64)> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|p| (p["name"].as_str().unwrap().to_string(), p["value"].as_f64().unwrap()))
        .collect()
}

fn text_list(list: &Json) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect()
}

fn dict(map: &indexmap::IndexMap<String, f64>) -> Json {
    Json::Object(map.iter().map(|(k, v)| (k.clone(), nan_text(*v))).collect())
}

fn opt(v: Option<f64>) -> Json {
    v.map_or(Json::Null, nan_text)
}

fn play(sc: &Json) -> Json {
    let host = Fake::new(sc);
    let eng = host.engine.clone();
    let mut steps = Vec::new();
    for step in sc["steps"].as_array().unwrap() {
        let before = (
            host.sent.lock().unwrap().len(),
            host.statuses.lock().unwrap().len(),
            host.errors.lock().unwrap().len(),
            host.console.lock().unwrap().len(),
            host.now(),
        );
        let result = match step["op"].as_str().unwrap() {
            "request_list" => json!(eng.request_param_list(&*host)),
            "request" => json!(eng.request_param(&*host, step["name"].as_str().unwrap())),
            "deliver" => {
                let p = &step["param"];
                let value = if p["value"] == "nan" {
                    f64::NAN
                } else {
                    p["value"].as_f64().unwrap_or(0.0)
                };
                let raw = p["bits"]
                    .as_u64()
                    .map_or_else(|| (value as f32).to_bits(), |b| b as u32);
                host.deliver_raw(
                    p["name"].as_str().unwrap(),
                    value,
                    p["type"].as_i64().unwrap(),
                    p["index"].as_i64().unwrap(),
                    p["count"].as_i64().unwrap(),
                    raw,
                );
                Json::Null
            }
            "watchdog" => {
                eng.run_watchdog(&*host);
                Json::Null
            }
            "wait_for" => {
                let e = eng.wait_for_param(
                    &*host,
                    step["name"].as_str().unwrap(),
                    step["timeout"].as_f64().unwrap_or(2.0),
                );
                e.map_or(Json::Null, |e| entry_json(&e))
            }
            "set" => json!(eng.set_param(
                &*host,
                step["name"].as_str().unwrap(),
                step["value"].as_f64().unwrap_or(f64::NAN)
            )),
            "fetch" => dict(&eng.fetch_params(
                &*host,
                &text_list(&step["names"]),
                step["timeout"].as_f64().unwrap_or(4.0),
                step["fresh"].as_bool().unwrap_or(false),
            )),
            "verify" => {
                let names = step.get("names").filter(|n| !n.is_null()).map(text_list);
                let r = eng.verify_params(
                    &*host,
                    &pairs(&step["targets"]),
                    names.as_deref(),
                    step["rewrite"].as_bool().unwrap_or(true),
                    step["timeout"].as_f64().unwrap_or(4.0),
                );
                r.map_or(Json::Null, |r| {
                    json!({
                        "results": r.results.iter().map(|x| json!({
                            "name": x.name, "wanted": x.wanted, "before": opt(x.before), "after": opt(x.after),
                            "rewritten": x.rewritten, "ok": x.ok, "error": x.error,
                        })).collect::<Vec<_>>(),
                        "values": dict(&r.values), "missing": r.missing, "confirmed": r.confirmed,
                        "failed": r.failed, "all_confirmed": r.all_confirmed,
                    })
                })
            }
            "upload" => {
                let params = pairs(&step["params"]);
                let begun = eng.begin_upload(&*host, &params);
                let mut out = json!({"begun": begun});
                if begun {
                    eng.run_upload(&*host, &params);
                    let r = eng.upload_result();
                    out["result"] = json!({
                        "state": r.state, "written": r.written, "failed": r.failed.len(),
                        "errors": r.failed.iter().map(|f| json!({"name": f.name, "error": f.error})).collect::<Vec<_>>(),
                    });
                }
                out
            }
            "abort" => {
                eng.abort();
                Json::Null
            }
            "reset" => {
                eng.reset();
                Json::Null
            }
            "set_state" => {
                eng.set_state(step["state"].as_str().unwrap());
                Json::Null
            }
            other => panic!("{other}"),
        };
        let tail = |v: &Mutex<Vec<Json>>, from: usize| Json::Array(v.lock().unwrap()[from..].to_vec());
        steps.push(json!({
            "result": result,
            "sent": tail(&host.sent, before.0),
            "statuses": tail(&host.statuses, before.1),
            "errors": host.errors.lock().unwrap()[before.2..].to_vec(),
            "console": tail(&host.console, before.3),
            "elapsed": host.now() - before.4,
        }));
    }
    let cache: serde_json::Map<String, Json> = eng.cache().iter().map(|e| (e.name.clone(), entry_json(e))).collect();
    let st = eng.status();
    json!({
        "steps": steps,
        "final": {
            "state": eng.get_state(), "count": eng.count(), "received": eng.received(), "status": st.to_json(),
            "params": eng.params().iter().map(|e| json!({"name": e.name, "value": nan_text(e.value), "type": e.ptype})).collect::<Vec<_>>(),
            "watchdog_started": host.watchdog.load(Ordering::SeqCst),
            "last_error": host.last_error(), "cache": cache,
        },
    })
}

#[test]
fn every_scenario_ends_where_the_python_engine_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/params_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 25);
    for case in cases {
        let name = case["scenario"]["name"].as_str().unwrap();
        let got = play(&case["scenario"]);
        let want = &case["expected"];
        for (i, (g, w)) in got["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(want["steps"].as_array().unwrap())
            .enumerate()
        {
            let op = &case["scenario"]["steps"][i]["op"];
            for key in ["result", "sent", "statuses", "errors", "console"] {
                assert_eq!(
                    g[key], w[key],
                    "{name} step {i} ({op}): {key}\n got {}\nwant {}",
                    g[key], w[key]
                );
            }
            let (ge, we) = (g["elapsed"].as_f64().unwrap(), w["elapsed"].as_f64().unwrap());
            assert!((ge - we).abs() < 1e-3, "{name} step {i} ({op}): elapsed {ge} != {we}");
        }
        for key in [
            "state",
            "count",
            "received",
            "status",
            "params",
            "watchdog_started",
            "last_error",
            "cache",
        ] {
            assert_eq!(got["final"][key], want["final"][key], "{name}: final {key}");
        }
    }
}

#[test]
fn floats_read_as_python_prints_them() {
    use corvus_core::protocol::params::float_text;
    for (v, s) in [
        (3.0, "3.0"),
        (9.25, "9.25"),
        (-0.5, "-0.5"),
        (1e-5, "1e-05"),
        (1e16, "1e+16"),
        (1.5e300, "1.5e+300"),
        (123456.0, "123456.0"),
    ] {
        assert_eq!(float_text(v), s);
    }
    assert_eq!(float_text(f64::NAN), "nan");
    assert_eq!(float_text(f64::NEG_INFINITY), "-inf");
}
