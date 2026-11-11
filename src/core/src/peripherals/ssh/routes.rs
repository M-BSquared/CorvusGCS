//! The SSH endpoints of `server.py` that start a connection or keep the saved
//! list: `/api/ssh/connect`, `/connections` (GET, POST, remove), `/run` and
//! `/power`. The session registry endpoints (`sessions`, `send`, `stream`...)
//! are in `shell::routes`.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Map, Value};

use super::client::Login;
use super::commands::{compose_power_command, compose_remote_command, power_flag};
use super::hostkeys::HostKeyFiles;
use super::run::{run_command, RunOptions};
use super::session::SshSession;
use crate::core::Core;
use crate::module::Registry;
use crate::peripherals::shell::session::{Terminal, Terminals};
use crate::peripherals::util::py_strip;
use crate::peripherals::Peripherals;
use crate::protocol::common::py_int_of;
use crate::request::Request;
use crate::response::Response;

pub fn register(reg: &mut Registry) {
    reg.get("/api/ssh/connections", api_connections);
    reg.post("/api/ssh/connect", api_connect);
    reg.post("/api/ssh/connections", api_connections_upsert);
    reg.post("/api/ssh/connections/remove", api_connections_remove);
    reg.post("/api/ssh/run", api_run);
    reg.post("/api/ssh/power", api_power);
}

/// `_config_write_lock`: a read-modify-write of the saved list is one step.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

fn config_guard() -> MutexGuard<'static, ()> {
    CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn terminals(core: &Core) -> Option<Arc<Terminals>> {
    core.service::<Peripherals>().map(|p| Arc::clone(&p.terminals))
}

fn saved_list(core: &Core) -> Vec<Map<String, Value>> {
    core.settings().get().ssh_connections().into_iter().cloned().collect()
}

fn saved_named(core: &Core, name: &str) -> Option<Map<String, Value>> {
    saved_list(core)
        .into_iter()
        .find(|e| e.get("name").and_then(Value::as_str) == Some(name))
}

fn store(core: &Core, list: Vec<Map<String, Value>>) {
    let value = Value::Array(list.into_iter().map(Value::Object).collect());
    let _ = core
        .settings()
        .update(core, |cfg| cfg.set("ssh_connections", Some(value)));
}

/// The saved connections with the live `connected` merged in and no password.
fn connections_public(core: &Core) -> Vec<Value> {
    let mut live: std::collections::HashMap<String, bool> = Default::default();
    if let Some(t) = terminals(core) {
        for s in t.list() {
            live.insert(
                s["name"].as_str().unwrap_or("").to_string(),
                s["connected"].as_bool().unwrap_or(false),
            );
        }
    }
    saved_list(core)
        .into_iter()
        .map(|mut entry| {
            let has = entry
                .shift_remove("password")
                .is_some_and(|p| crate::protocol::common::py_truthy(&p));
            entry.insert("has_password".into(), json!(has));
            let name = entry.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            entry.insert("connected".into(), json!(live.get(&name).copied().unwrap_or(false)));
            Value::Object(entry)
        })
        .collect()
}

fn api_connections(core: &Core, _req: &Request) -> Response {
    Response::json(json!({"connections": connections_public(core)}))
}

/// `int(raw)` with a boolean refused, the range checked.
fn port_of(raw: Option<&Value>) -> Result<u16, &'static str> {
    let raw = raw.cloned().unwrap_or(json!(22));
    if raw.is_boolean() {
        return Err("port must be an integer");
    }
    let Some(n) = py_int_of(&raw) else {
        return Err("port must be an integer");
    };
    u16::try_from(n).map_err(|_| "port must be between 0 and 65535")
}

/// `_coerce_port`: a saved entry's port, or the default.
fn coerce_port(raw: Option<&Value>, default: u16) -> u16 {
    match raw {
        Some(Value::Bool(_)) | None => default,
        Some(v) => py_int_of(v).and_then(|n| u16::try_from(n).ok()).unwrap_or(default),
    }
}

fn host_key_files(core: &Core) -> HostKeyFiles {
    HostKeyFiles::standard(core.paths().home())
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("")
}

fn api_connect(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let Some(terminals) = terminals(core) else {
        return Response::error(500, "ssh not ready");
    };
    let name = match payload.get("name") {
        None => "device".to_string(),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Response::error(400, "name must be a non-empty string"),
    };
    let mut host = match payload.get("host") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Response::error(400, "host must be a string"),
    };
    let borrow = match payload.get("from") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Response::error(400, "from must be a string"),
    };
    let mut port = match port_of(payload.get("port")) {
        Ok(p) => p,
        Err(m) => return Response::error(400, m),
    };
    let mut username = payload.get("username").cloned().unwrap_or(json!("corvus"));
    let mut password: Option<String> = payload.get("password").and_then(Value::as_str).map(str::to_string);
    let mut key_path: Option<String> = payload.get("key_path").and_then(Value::as_str).map(str::to_string);
    let mut lookup = String::new();
    if host.is_empty() {
        lookup = if borrow.is_empty() {
            name.clone()
        } else {
            borrow.clone()
        };
        let Some(saved) = saved_named(core, &lookup) else {
            let msg = if borrow.is_empty() {
                "no host".to_string()
            } else {
                format!("no saved connection named {}", py_repr(&lookup))
            };
            return Response::json_status(json!({"error": msg, "needs": "connection", "connection": lookup}), 400);
        };
        host = str_of(&saved, "host").to_string();
        port = coerce_port(saved.get("port"), 22);
        if let Some(u) = saved.get("username") {
            username = u.clone();
        }
        if payload.get("password").is_none_or(Value::is_null) {
            password = Some(str_of(&saved, "password").to_string()).filter(|p| !p.is_empty());
        }
        if key_path.as_deref().is_none_or(str::is_empty) {
            key_path = Some(str_of(&saved, "key_path").to_string()).filter(|p| !p.is_empty());
        }
        if host.is_empty() {
            return Response::json_status(
                json!({"error": "no host", "needs": "connection", "connection": lookup}),
                400,
            );
        }
    }
    let username = match username {
        Value::String(s) if !s.is_empty() => s,
        _ => {
            let mut body = json!({"error": "username is required"});
            if !lookup.is_empty() {
                body["needs"] = json!("credentials");
                body["connection"] = json!(lookup);
            }
            return Response::json_status(body, 400);
        }
    };
    let login = Login {
        username: username.clone(),
        password: password.filter(|p| !p.is_empty()),
        key_path: key_path.filter(|p| !p.is_empty()),
        ssh_dir: None,
    };
    let files = host_key_files(core);
    let (n, h) = (name.clone(), host.clone());
    let ok = terminals.open(&name, &|| {
        SshSession::new(&n, &h, port, login.clone(), files.clone()) as Arc<dyn Terminal>
    });
    let mut reply = json!({"ok": ok, "connected": ok, "name": name, "host": host, "port": port, "username": username});
    if !ok {
        let why = terminals.connect_error(&name);
        reply["error"] = json!(if why.is_empty() {
            "Connection failed.".to_string()
        } else {
            why
        });
        if !lookup.is_empty() && terminals.connect_auth_failed(&name) {
            reply["needs"] = json!("credentials");
            reply["connection"] = json!(lookup);
        }
    }
    Response::json(reply)
}

/// `repr(str)` for a name in a message.
fn py_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

fn api_connections_upsert(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let name = match payload.get("name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Response::error(400, "name must be a non-empty string"),
    };
    let host = match payload.get("host") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Response::error(400, "host must be a non-empty string"),
    };
    let port = match port_of(payload.get("port")) {
        Ok(p) => p,
        Err(m) => return Response::error(400, m),
    };
    let username = match payload.get("username") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Response::error(400, "username must be a string"),
    };
    let key_path = payload
        .get("key_path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let has_password = payload.contains_key("password");
    let mut password = payload
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let original = match payload.get("original_name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => name.clone(),
    };
    {
        let _held = config_guard();
        let mut list = saved_list(core);
        let index = list
            .iter()
            .position(|e| e.get("name").and_then(Value::as_str) == Some(original.as_str()));
        if name != original
            && list
                .iter()
                .any(|e| e.get("name").and_then(Value::as_str) == Some(name.as_str()))
        {
            return Response::error(400, &format!("\"{name}\" is already saved. Pick another name."));
        }
        if !has_password {
            password = index
                .map(|i| str_of(&list[i], "password").to_string())
                .unwrap_or_default();
        }
        let mut entry = Map::new();
        entry.insert("name".into(), json!(name));
        entry.insert("host".into(), json!(host));
        entry.insert("port".into(), json!(port));
        entry.insert("username".into(), json!(username));
        entry.insert("key_path".into(), json!(key_path));
        entry.insert("password".into(), json!(password));
        match index {
            Some(i) => list[i] = entry,
            None => list.push(entry),
        }
        store(core, list);
    }
    Response::json(json!({"ok": true, "connections": connections_public(core)}))
}

fn api_connections_remove(core: &Core, req: &Request) -> Response {
    let name = match req.payload().get("name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Response::error(400, "name must be a non-empty string"),
    };
    if let Some(t) = terminals(core) {
        t.disconnect(&name);
    }
    {
        let _held = config_guard();
        let list: Vec<_> = saved_list(core)
            .into_iter()
            .filter(|e| e.get("name").and_then(Value::as_str) != Some(name.as_str()))
            .collect();
        store(core, list);
    }
    Response::json(json!({"ok": true, "connections": connections_public(core)}))
}

fn bad(message: &str) -> Response {
    Response::not_ok(400, message)
}

fn stop_flag(core: &Core) -> Arc<std::sync::atomic::AtomicBool> {
    core.service::<Peripherals>()
        .map(|p| Arc::clone(&p.ssh_stop))
        .unwrap_or_default()
}

fn api_run(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let command = match payload.get("command") {
        Some(Value::String(s)) if !py_strip(s).is_empty() => py_strip(s).to_string(),
        _ => return bad("command must be a non-empty string"),
    };
    let directory = match payload.get("directory") {
        None => String::new(),
        Some(Value::String(s)) => py_strip(s).to_string(),
        Some(_) => return bad("directory must be a string"),
    };
    let detach = match payload.get("detach") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return bad("detach must be a boolean"),
    };
    let name = match payload.get("name") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return bad("name must be a string"),
    };
    let mut host = match payload.get("host") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return bad("host must be a string"),
    };
    let mut username = payload
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut port = match port_of(payload.get("port")) {
        Ok(p) => p,
        Err(m) => return bad(m),
    };
    let mut password: Option<String> = None;
    let mut key_path: Option<String> = None;
    let mut saved: Option<Map<String, Value>> = None;
    if !name.is_empty() {
        saved = saved_named(core, &name);
        if saved.is_none() && host.is_empty() {
            return Response::json_status(
                json!({"ok": false, "error": format!("no saved connection named {}", py_repr(&name)),
                       "needs": "connection", "connection": name}),
                400,
            );
        }
        if let Some(s) = &saved {
            if host.is_empty() {
                host = str_of(s, "host").to_string();
            }
            if !payload.contains_key("port") {
                port = coerce_port(s.get("port"), 22);
            }
            if username.is_empty() {
                username = str_of(s, "username").to_string();
            }
            password = Some(str_of(s, "password").to_string()).filter(|p| !p.is_empty());
            key_path = Some(str_of(s, "key_path").to_string()).filter(|p| !p.is_empty());
        }
    }
    let from_saved = !name.is_empty() && saved.is_some();
    if host.is_empty() {
        let mut body = json!({"ok": false, "error": "no host"});
        if from_saved {
            body["needs"] = json!("connection");
            body["connection"] = json!(name);
        }
        return Response::json_status(body, 400);
    }
    if username.is_empty() {
        let mut body = json!({"ok": false, "error": "username is required"});
        if from_saved {
            body["needs"] = json!("credentials");
            body["connection"] = json!(name);
        }
        return Response::json_status(body, 400);
    }
    let line = compose_remote_command(&directory, &command, detach);
    let opts = RunOptions {
        host: host.clone(),
        command: line.clone(),
        port,
        login: Login {
            username: username.clone(),
            password,
            key_path,
            ssh_dir: None,
        },
        timeout: 20.0,
        stdin_data: None,
    };
    let stop = stop_flag(core);
    let mut result = run_command(&opts, &host_key_files(core), &stop)
        .as_object()
        .cloned()
        .unwrap_or_default();
    result.insert("command".into(), json!(line));
    result.insert("host".into(), json!(host));
    result.insert("username".into(), json!(username));
    result.insert("port".into(), json!(port));
    let auth_failed = result.shift_remove("auth_failed").is_some_and(|v| v == json!(true));
    if from_saved && auth_failed {
        result.insert("needs".into(), json!("credentials"));
        result.insert("connection".into(), json!(name));
    }
    Response::json(Value::Object(result))
}

fn api_power(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let action = payload.get("action").and_then(Value::as_str).unwrap_or("");
    if power_flag(action).is_none() {
        return bad("action must be reboot or shutdown");
    }
    let name = match payload.get("name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return bad("name must be a non-empty string"),
    };
    let Some(saved) = saved_named(core, &name) else {
        return Response::json_status(
            json!({"ok": false, "error": format!("no saved connection named {}", py_repr(&name)),
                   "needs": "connection", "connection": name}),
            400,
        );
    };
    let host = str_of(&saved, "host").to_string();
    let username = str_of(&saved, "username").to_string();
    if host.is_empty() || username.is_empty() {
        return Response::json_status(
            json!({"ok": false, "error": "the connection has no host or user",
                   "needs": if host.is_empty() { "connection" } else { "credentials" }, "connection": name}),
            400,
        );
    }
    let password = Some(str_of(&saved, "password").to_string()).filter(|p| !p.is_empty());
    let key_path = Some(str_of(&saved, "key_path").to_string()).filter(|p| !p.is_empty());
    let sudo_password = if key_path.is_none() { password.clone() } else { None };
    let opts = RunOptions {
        host,
        command: compose_power_command(action, sudo_password.is_some()),
        port: coerce_port(saved.get("port"), 22),
        login: Login {
            username,
            password,
            key_path,
            ssh_dir: None,
        },
        timeout: 20.0,
        stdin_data: sudo_password.map(|p| format!("{p}\n")),
    };
    let stop = stop_flag(core);
    let mut result = run_command(&opts, &host_key_files(core), &stop)
        .as_object()
        .cloned()
        .unwrap_or_default();
    if result.get("ok") != Some(&json!(true)) {
        let detail = str_of(&result, "stderr").to_lowercase();
        if detail.contains("password is required") || detail.contains("no tty present") {
            result.insert(
                "error".into(),
                json!("sudo needs a password on this computer. Save the password with the connection or allow passwordless sudo."),
            );
        } else if detail.contains("try again") || detail.contains("incorrect password") {
            result.insert("error".into(), json!("sudo refused the saved password."));
        }
    }
    if result.shift_remove("auth_failed").is_some_and(|v| v == json!(true)) {
        result.insert("needs".into(), json!("credentials"));
        result.insert("connection".into(), json!(name));
    }
    result.shift_remove("stdout");
    Response::json(Value::Object(result))
}
