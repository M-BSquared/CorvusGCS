//! The terminal endpoints of `server.py` that belong to the shell on this
//! computer and to the session registry: `/api/local/*`, `/api/ssh/sessions`,
//! `/send`, `/resize`, `/disconnect`, the output stream and the WebSocket.
//! Starting a program is nothing the operator cannot already do on their own
//! laptop, but it is not something a machine on the flight line network may
//! do, so the ones that start one answer only to this computer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::local::{self, LocalSession};
use super::session::{subscribe, Feed, Terminal, Terminals};
use crate::core::Core;
use crate::module::Registry;
use crate::peripherals::{local_only, Peripherals};
use crate::request::Request;
use crate::response::{Response, SseEvent, SseNext, SseStream};
use crate::ws::{WebSocket, CLOSE_GOING_AWAY, CLOSE_NORMAL};

pub fn register(reg: &mut Registry) {
    reg.get("/api/local/status", api_status);
    reg.post("/api/local/connect", api_connect);
    reg.post("/api/local/run", api_run);
    reg.get("/api/ssh/sessions", api_sessions);
    reg.post("/api/ssh/send", api_send);
    reg.post("/api/ssh/resize", api_resize);
    reg.post("/api/ssh/disconnect", api_disconnect);
    reg.get("/api/ssh/stream", api_stream);
    reg.get("/api/ssh/ws", api_ws);
}

fn terminals(core: &Core) -> Option<Arc<Terminals>> {
    core.service::<Peripherals>().map(|p| Arc::clone(&p.terminals))
}

fn api_status(core: &Core, _req: &Request) -> Response {
    let mut caps = local::capabilities();
    let running = core.service::<Peripherals>().map(|p| p.runner.count()).unwrap_or(0);
    caps["background"] = json!(true);
    caps["running"] = json!(running);
    Response::json(caps)
}

fn api_connect(core: &Core, req: &Request) -> Response {
    if let Some(refused) = local_only(req) {
        return refused;
    }
    let payload = req.payload();
    let name = match payload.get("name") {
        Some(Value::String(s)) if !crate::peripherals::util::py_strip(s).is_empty() && s.chars().count() <= 128 => {
            s.clone()
        }
        _ => return Response::not_ok(400, "name must be a non-empty string"),
    };
    let directory = match payload.get("directory") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let Some(terminals) = terminals(core) else {
        return Response::json_status(
            json!({"ok": false, "connected": false, "error": "terminals are unavailable"}),
            503,
        );
    };
    let n = name.clone();
    let connected = terminals.open(&name, &|| LocalSession::new(&n, &directory, "") as Arc<dyn Terminal>);
    let mut answer = json!({"ok": connected, "connected": connected, "name": name});
    if !connected {
        let why = terminals.connect_error(&name);
        answer["error"] = json!(if why.is_empty() {
            "The shell could not be started.".to_string()
        } else {
            why
        });
    }
    Response::json(answer)
}

fn api_run(core: &Core, req: &Request) -> Response {
    if let Some(refused) = local_only(req) {
        return refused;
    }
    let payload = req.payload();
    let command = payload.get("command");
    let directory = payload.get("directory").cloned().unwrap_or(json!(""));
    let (Some(Value::String(command)), Value::String(directory)) = (command, &directory) else {
        return Response::not_ok(400, "command and directory must be strings");
    };
    let Some(p) = core.service::<Peripherals>() else {
        return Response::not_ok(503, "Programs on this computer are unavailable.");
    };
    let mut result = p.runner.run(command, directory);
    result["command"] = json!(command);
    Response::json(result)
}

fn api_sessions(core: &Core, _req: &Request) -> Response {
    let sessions = terminals(core).map(|t| t.list()).unwrap_or_default();
    Response::json(json!({"sessions": sessions}))
}

fn api_send(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let name = payload.get("name").cloned().unwrap_or(json!(""));
    let data = payload.get("data").cloned().unwrap_or(json!(""));
    let Value::String(data) = data else {
        return Response::not_ok(400, "data must be a string");
    };
    let named = crate::protocol::common::py_truthy(&name);
    match (terminals(core), named) {
        (Some(t), true) => {
            let ok = name.as_str().is_some_and(|n| t.send(n, &data));
            Response::json(json!({"ok": ok}))
        }
        _ => Response::error(400, "no session"),
    }
}

fn api_resize(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let name = match payload.get("name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Response::not_ok(400, "name must be a non-empty string"),
    };
    let mut dims = [0i64; 2];
    for (i, key) in ["cols", "rows"].iter().enumerate() {
        let raw = payload.get(*key);
        let value = match raw {
            Some(Value::Bool(_)) | None => None,
            Some(v) => crate::protocol::common::py_int_of(v),
        };
        let Some(v) = value else {
            return Response::not_ok(400, &format!("{key} must be an integer"));
        };
        if !(1..=1000).contains(&v) {
            return Response::not_ok(400, &format!("{key} out of range"));
        }
        dims[i] = v;
    }
    let Some(t) = terminals(core) else {
        return Response::not_ok(400, "no session");
    };
    Response::json(json!({"ok": t.resize(&name, dims[0], dims[1])}))
}

fn api_disconnect(core: &Core, req: &Request) -> Response {
    let name = req.payload().get("name").cloned().unwrap_or(json!(""));
    match (terminals(core), crate::protocol::common::py_truthy(&name)) {
        (Some(t), true) => {
            let ok = name.as_str().is_some_and(|n| t.disconnect(n));
            Response::json(json!({"ok": ok}))
        }
        _ => Response::error(400, "no session"),
    }
}

/// One session's output as a server sent event stream: `output`, `closed`
/// and the server's own `ping`.
struct OutputStream {
    terminals: Arc<Terminals>,
    name: String,
    feed: Option<Arc<Feed>>,
    sub: u64,
    wanted: String,
    ended: bool,
}

impl SseStream for OutputStream {
    fn open(&mut self) -> Vec<SseEvent> {
        if self.feed.is_none() {
            self.ended = true;
            let name = if self.wanted.is_empty() {
                Value::Null
            } else {
                json!(self.wanted)
            };
            return vec![SseEvent::json("closed", &json!({"name": name}))];
        }
        Vec::new()
    }

    fn next(&mut self, wait: Duration) -> SseNext {
        let Some(feed) = &self.feed else {
            return SseNext::End;
        };
        if self.ended {
            return SseNext::End;
        }
        if let Some(text) = feed.get(wait.min(Duration::from_secs(1))) {
            return SseNext::Events(vec![SseEvent::json(
                "output",
                &json!({"text": text, "name": self.name}),
            )]);
        }
        let live = self.terminals.get(&self.name);
        if live.is_none_or(|l| !l.connected()) {
            self.ended = true;
            return SseNext::Events(vec![SseEvent::json("closed", &json!({"name": self.name}))]);
        }
        SseNext::Idle
    }

    fn close(&mut self) {
        if let Some(t) = self.terminals.get(&self.name) {
            t.output().remove_sub(self.sub);
        }
    }
}

fn api_stream(core: &Core, req: &Request) -> Response {
    let wanted = req.query.get("name").unwrap_or("").to_string();
    let Some(terminals) = terminals(core) else {
        return Response::sse(Box::new(OutputStream {
            terminals: Arc::new(Terminals::new()),
            name: String::new(),
            feed: None,
            sub: 0,
            wanted,
            ended: false,
        }));
    };
    let mut attached: Option<(String, Arc<Feed>, u64)> = None;
    for name in terminals.connected_names() {
        if !wanted.is_empty() && name != wanted {
            continue;
        }
        if let Some(t) = terminals.get(&name) {
            let (feed, id) = subscribe(&t);
            attached = Some((name, feed, id));
        }
        break;
    }
    let (name, feed, sub) = match attached {
        Some((n, f, i)) => (n, Some(f), i),
        None => (String::new(), None, 0),
    };
    Response::sse(Box::new(OutputStream {
        terminals,
        name,
        feed,
        sub,
        wanted,
        ended: false,
    }))
}

/// One message from a terminal; anything malformed is ignored.
fn ws_input(terminals: &Terminals, name: &str, text: &str) {
    let Ok(Value::Object(msg)) = serde_json::from_str::<Value>(text) else {
        return;
    };
    match msg.get("type").and_then(Value::as_str) {
        Some("input") => {
            if let Some(Value::String(d)) = msg.get("data") {
                if !d.is_empty() {
                    terminals.send(name, d);
                }
            }
        }
        Some("resize") => {
            let dim = |k: &str| msg.get(k).and_then(Value::as_i64).filter(|v| (1..=1000).contains(v));
            if let (Some(c), Some(r)) = (dim("cols"), dim("rows")) {
                terminals.resize(name, c, r);
            }
        }
        _ => {}
    }
}

/// One terminal over a WebSocket: output out, keystrokes and size in. Held to
/// what the POSTs are held to (the server refuses a page on another site
/// before the upgrade). A session that is not there, or not connected, is
/// answered with `closed` right after the upgrade.
fn api_ws(core: &Core, req: &Request) -> Response {
    let name = req.query.get("name").unwrap_or("").to_string();
    let terminals = terminals(core);
    let core = core.clone();
    Response::upgrade(req, move |ws: WebSocket| {
        let session = terminals
            .as_ref()
            .and_then(|t| if name.is_empty() { None } else { t.get(&name) });
        let (Some(terminals), Some(session)) = (terminals, session.filter(|s| s.connected())) else {
            let n = if name.is_empty() { Value::Null } else { json!(name) };
            let _ = ws.send_text(&json!({"type": "closed", "name": n}).to_string());
            ws.close(CLOSE_NORMAL, "");
            return;
        };
        let ws = Arc::new(ws);
        let (feed, sub) = subscribe(&session);
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (ws, feed, done, terminals, name, core) = (
                Arc::clone(&ws),
                Arc::clone(&feed),
                Arc::clone(&done),
                Arc::clone(&terminals),
                name.clone(),
                core.clone(),
            );
            std::thread::Builder::new()
                .name(format!("ssh-ws-{name}"))
                .spawn(move || {
                    let mut idle = 0.0;
                    while !done.load(Ordering::SeqCst) {
                        if let Some(text) = feed.get(Duration::from_secs(1)) {
                            idle = 0.0;
                            if ws
                                .send_text(&json!({"type": "output", "text": text}).to_string())
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                        if core.is_stopping() {
                            break;
                        }
                        if terminals.get(&name).is_none_or(|l| !l.connected()) {
                            for text in feed.drain() {
                                let _ = ws.send_text(&json!({"type": "output", "text": text}).to_string());
                            }
                            let _ = ws.send_text(&json!({"type": "closed", "name": name}).to_string());
                            break;
                        }
                        idle += 1.0;
                        if idle >= 15.0 {
                            idle = 0.0;
                            if ws.ping(b"").is_err() {
                                break;
                            }
                        }
                    }
                    done.store(true, Ordering::SeqCst);
                })
        };
        while !done.load(Ordering::SeqCst) && !core.is_stopping() {
            match ws.receive(Duration::from_secs(1)) {
                Ok(Some(text)) => ws_input(&terminals, &name, &text),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        done.store(true, Ordering::SeqCst);
        session.output().remove_sub(sub);
        if let Ok(h) = writer {
            let _ = h.join();
        }
        ws.close(
            if core.is_stopping() {
                CLOSE_GOING_AWAY
            } else {
                CLOSE_NORMAL
            },
            "",
        );
    })
}
