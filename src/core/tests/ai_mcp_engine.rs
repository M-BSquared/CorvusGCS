//! The MCP engine answers what `McpService` of `mcp.py` answers: the same
//! settings, tool list, schemas, permission checks, error texts, JSON-RPC
//! envelopes, events and calls to the endpoints, for the same requests
//! (`python/tools/dump_ai_cases.py mcp_engine`).

mod common_ai;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common_ai::Tmp;
use corvus_core::mcp::engine::*;
use corvus_core::mcp::settings::*;
use corvus_core::mcp::tools;
use serde_json::{json, Map, Value};

fn fixture() -> Value {
    serde_json::from_str(include_str!("data/ai_mcp_engine.json")).unwrap()
}

#[test]
fn the_settings_are_coerced_made_effective_and_compacted_as_python_does() {
    let f = fixture();
    for case in f["settings"].as_array().unwrap() {
        let input = &case["in"];
        assert_eq!(json!(coerce_settings(input)), case["coerce"], "coerce {input}");
        let eff = effective_settings(Some(input));
        assert_eq!(Value::Object(eff.clone()), case["effective"], "effective {input}");
        assert_eq!(json!(compact_settings(&eff)), case["compact"], "compact {input}");
    }
    for pair in f["levels"].as_array().unwrap() {
        let id = pair[0].as_str().unwrap();
        let s = effective_settings(Some(&json!({"permissions": {id: true}, "confirm": {id: true}})));
        assert_eq!(json!(level(&s, id)), pair[1], "level {id}");
    }
    for t in f["plugin_names"].as_array().unwrap() {
        assert_eq!(
            json!(plugin_tool_name(t[0].as_str().unwrap(), t[1].as_str().unwrap())),
            t[2]
        );
    }
    assert_eq!(tool_catalog(), f["catalog"]);
    let built: Vec<Value> = tools::builtin()
        .iter()
        .map(|t| json!([t.name, t.permission, t.vehicle, t.read_only]))
        .collect();
    assert_eq!(Value::Array(built), f["builtin"]);
    assert_eq!(f["constants"], json!([CONFIRM_TIMEOUT_S, CONTROL_MAX_S, WHERE]));
}

struct Rig {
    svc: Arc<McpService>,
    raw: Arc<Mutex<Option<Value>>>,
    calls: Arc<Mutex<Vec<Value>>>,
    now: Arc<Mutex<f64>>,
    _tmp: Tmp,
}

fn rig(replies: &Value) -> Rig {
    let tmp = Tmp::new("mcp");
    let table: HashMap<String, (u16, Value)> = replies
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), (v[0].as_u64().unwrap() as u16, v[1].clone())))
        .collect();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let dispatch: Dispatch = Arc::new(move |method, path, body| {
        seen.lock().unwrap().push(json!([method, path, body]));
        let key = format!("{method} {}", path.split('?').next().unwrap());
        table.get(&key).cloned().unwrap_or((200, json!({"ok": true})))
    });
    let raw: Arc<Mutex<Option<Value>>> = Arc::default();
    let (r1, r2) = (Arc::clone(&raw), Arc::clone(&raw));
    let load: LoadSettings = Arc::new(move || r1.lock().unwrap().clone());
    let save: SaveSettings = Arc::new(move |block: Option<Map<String, Value>>| {
        *r2.lock().unwrap() = block.map(Value::Object);
        Ok(())
    });
    let now = Arc::new(Mutex::new(100.0));
    let n = Arc::clone(&now);
    let clock: Clock = Arc::new(move || *n.lock().unwrap());
    let timing = Timing {
        confirm: Duration::from_secs(10),
        plugin_call: Duration::from_millis(30),
    };
    let svc = McpService::new(dispatch, load, save, tmp.0.join("mcp-token"), clock, timing);
    Rig {
        svc,
        raw,
        calls,
        now,
        _tmp: tmp,
    }
}

/// The values the run decides by itself, taken out the way the dump takes them out.
fn normalize(v: &Value, sids: &[(String, String)]) -> Value {
    match v {
        Value::String(s) => {
            if let Some((_, name)) = sids.iter().find(|(real, _)| real == s) {
                return json!(name);
            }
            json!(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| normalize(x, sids)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let n = match (k.as_str(), x) {
                        ("at", Value::Number(_)) => json!(0),
                        ("id", Value::String(s)) if s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
                            json!("<id>")
                        }
                        ("version", Value::String(s)) if s == corvus_core::version::version() => json!("<version>"),
                        _ => normalize(x, sids),
                    };
                    (k.clone(), n)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn args_of(step: &Value) -> Map<String, Value> {
    step["args"].as_object().cloned().unwrap_or_default()
}

#[test]
fn every_step_ends_where_the_python_service_ends() {
    let f = fixture();
    let r = rig(&f["replies"]);
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let ev = Arc::clone(&events);
    r.svc
        .add_listener(Arc::new(move |e: &Value| ev.lock().unwrap().push(e.clone())));
    let mut sids: Vec<(String, String)> = Vec::new();
    let mut last: Option<String> = None;

    for (i, entry) in f["steps"].as_array().unwrap().iter().enumerate() {
        let step = &entry["step"];
        let session = |sel: &Value| -> Option<String> {
            match sel.as_str() {
                Some("last") => last.clone(),
                Some(other) => Some(other.to_string()),
                None => None,
            }
        };
        let result: Value = match step["op"].as_str().unwrap() {
            "settings" => {
                *r.raw.lock().unwrap() = Some(step["raw"].clone());
                r.svc.settings_reloaded();
                Value::Object(r.svc.settings())
            }
            "post" => {
                let (status, reply, headers) = r
                    .svc
                    .handle_post(&step["message"], session(&step["session"]).as_deref());
                let mut h = Map::new();
                for (k, v) in headers {
                    if k == "Mcp-Session-Id" {
                        sids.push((v.clone(), format!("<sid{}>", sids.len() + 1)));
                        last = Some(v.clone());
                    }
                    h.insert(k, v.into());
                }
                json!({"status": status, "reply": reply, "headers": h})
            }
            "tools" => Value::Array(r.svc.tools()),
            "status" => Value::Object(r.svc.status()),
            "call" => r
                .svc
                .call_tool(step["name"].as_str().unwrap(), &args_of(step), None)
                .unwrap_or(Value::Null),
            "call_confirmed" => {
                let svc = Arc::clone(&r.svc);
                let (name, args) = (step["name"].as_str().unwrap().to_string(), args_of(step));
                let t = std::thread::spawn(move || svc.call_tool(&name, &args, None));
                let started = std::time::Instant::now();
                while r.svc.status()["pending"].as_array().unwrap().is_empty() && !t.is_finished() {
                    assert!(started.elapsed() < Duration::from_secs(5), "step {i}");
                    std::thread::sleep(Duration::from_millis(5));
                }
                let pending = r.svc.status()["pending"].as_array().unwrap().clone();
                if let Some(p) = pending.first() {
                    r.svc.answer(&p["id"], &step["allow"]);
                }
                let out = t.join().unwrap();
                let pending: Vec<Value> = pending
                    .into_iter()
                    .map(|mut p| {
                        p["id"] = json!("<id>");
                        p
                    })
                    .collect();
                json!({"pending": pending, "result": out})
            }
            "window" => match r.svc.window_heartbeat(&step["id"], &step["tools"]) {
                Ok(v) => v,
                Err(e) => json!({"error": e}),
            },
            "advance" => {
                let mut n = r.now.lock().unwrap();
                *n += step["seconds"].as_f64().unwrap();
                json!(*n)
            }
            "update" => match r.svc.update_settings(step["patch"].as_object().unwrap()) {
                Ok(s) => Value::Object(s),
                Err(UpdateError::Invalid(e)) => json!({"error": e}),
                Err(e) => panic!("{e:?}"),
            },
            "stop" => Value::Object(r.svc.stop().unwrap()),
            "end_session" => json!(r.svc.end_session(session(&step["session"]).as_deref())),
            "answer" => json!(r.svc.answer(&step["id"], &step["allow"])),
            "result" => json!(r.svc.call_result(&step["id"], &step["ok"], &Value::Null, &Value::Null)),
            other => panic!("{other}"),
        };
        if step["op"] == "call_confirmed" {
            for e in events.lock().unwrap().iter_mut() {
                if e.get("id").is_some() {
                    e["id"] = json!("<id>");
                }
            }
        }
        let calls: Vec<Value> = std::mem::take(&mut *r.calls.lock().unwrap());
        let mut dedup: Vec<Value> = Vec::new();
        for c in calls {
            if dedup.last() != Some(&c) {
                dedup.push(c);
            }
        }
        let evs: Vec<Value> = std::mem::take(&mut *events.lock().unwrap());
        let saved = r.raw.lock().unwrap().clone().unwrap_or(Value::Null);
        let got = json!({
            "result": normalize(&result, &sids),
            "calls": normalize(&Value::Array(dedup), &sids),
            "events": normalize(&Value::Array(evs), &sids),
            "saved": normalize(&saved, &sids),
        });
        for key in ["result", "calls", "events", "saved"] {
            assert_eq!(got[key], entry[key], "step {i} {key}: {step}");
        }
    }
    r.svc.shutdown();
}

#[test]
fn direct_control_repeats_until_it_runs_out_and_then_holds() {
    let f = fixture();
    let r = rig(&f["replies"]);
    *r.raw.lock().unwrap() =
        Some(json!({"enabled": true, "permissions": {"control": true}, "confirm": {"control": false}}));
    let out = r
        .svc
        .call_tool(
            "manual_control",
            json!({"x": 0.5, "duration_s": 0.5}).as_object().unwrap(),
            None,
        )
        .unwrap();
    assert_eq!(out["isError"], false);
    assert_eq!(out["structuredContent"], json!({"ok": true, "holds_after_s": 0.5}));
    assert!(r.svc.status()["control"]["kind"] == "manual");
    // the clock runs past the end of the call: the loop holds
    *r.now.lock().unwrap() += 1.0;
    let started = std::time::Instant::now();
    while r.svc.status()["control"] != Value::Null {
        assert!(started.elapsed() < Duration::from_secs(5), "never held");
        std::thread::sleep(Duration::from_millis(20));
    }
    let calls = r.calls.lock().unwrap().clone();
    assert_eq!(
        calls[0],
        json!(["POST", "/api/mavlink/manual", {"x": 0.5, "y": 0.0, "z": 0.5, "r": 0.0}])
    );
    assert_eq!(calls.last().unwrap(), &json!(["POST", "/api/mavlink/hold", {}]));
    // stop_control ends it at once, with no question asked
    r.svc
        .call_tool("manual_control", json!({}).as_object().unwrap(), None)
        .unwrap();
    let stop = r.svc.call_tool("stop_control", &Map::new(), None).unwrap();
    assert_eq!(stop["structuredContent"], json!({"ok": true, "was_active": true}));
    r.svc.shutdown();
}

#[test]
fn shutdown_refuses_what_waits_and_joins_the_control_thread() {
    let f = fixture();
    let r = rig(&f["replies"]);
    *r.raw.lock().unwrap() =
        Some(json!({"enabled": true, "permissions": {"control": true}, "confirm": {"control": false}}));
    r.svc.call_tool("manual_control", &Map::new(), None).unwrap();
    r.svc.shutdown();
    let calls = r.calls.lock().unwrap().clone();
    assert!(calls.contains(&json!(["POST", "/api/mavlink/hold", {}])), "{calls:?}");
    let after = r.calls.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(r.calls.lock().unwrap().len(), after, "the control thread is gone");
    assert!(r
        .svc
        .handle_post(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}), None)
        .1
        .is_some());
}
