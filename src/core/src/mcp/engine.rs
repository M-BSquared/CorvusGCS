//! The MCP server's state and protocol (`McpService` of `mcp.py`): settings,
//! sessions, confirmations, plugin calls, direct control and the JSON-RPC.
//!
//! Three rules decide what an agent may do, and all three are the operator's:
//! the server is off until it is switched on (and then answers this computer
//! with the token only); every kind of act has its own switch, and a tool whose
//! permission is off is not even listed; and a tool never talks to the link, it
//! calls the same endpoint a button calls ([`Dispatch`], which the module wires
//! to `Core::internal_call`), so an agent gets the same validation, the same PX4
//! and ArduPilot sequences and the same refusals.
//!
//! Direct control is the one place that is not a single call: a vehicle flown
//! that way is fed setpoints at [`CONTROL_RATE_HZ`] by a thread of this service,
//! and when no new call has come by the time the last one ran out, the vehicle is
//! put in hold. An agent that goes quiet stops the aircraft. The thread is
//! joined in [`McpService::shutdown`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::{json, Value};

use super::settings::{self, level, plugin_allowed, Permission, CONTROL_MAX_S, PERMISSIONS};
use super::tools::{self, fmt, Run, Tool, CONTROL_DEFAULT_S, MAX_PARAMS_LISTED};
use crate::config::truthy;
use crate::http::mcp::TokenStore;
use crate::json::Map;
use crate::plugins::registry::is_valid_id;

pub const SERVER_NAME: &str = "corvus-gcs";
pub const SERVER_TITLE: &str = "Corvus GCS";
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
pub const DEFAULT_PROTOCOL: &str = "2025-06-18";
/// How long a tool waits for the operator, under the 60 s most clients give a call.
pub const CONFIRM_TIMEOUT_S: f64 = 45.0;
pub const PLUGIN_CALL_TIMEOUT_S: f64 = 30.0;
/// A window that has not announced itself for this long is gone, and its plugin
/// tools with it. It announces every 10 s.
pub const WINDOW_TTL_S: f64 = 25.0;
/// A client seen within this window counts as connected for the indicator.
pub const CLIENT_ACTIVE_S: f64 = 120.0;
pub const CONTROL_RATE_HZ: f64 = 5.0;
pub const MAX_SESSIONS: usize = 16;
pub const MAX_PLUGIN_TOOLS: usize = 64;
pub const MAX_SCHEMA_BYTES: usize = 8192;
pub const MAX_DESCRIPTION: usize = 1000;
pub const STREAM_QUEUE: usize = 32;
pub const WHERE: &str = "Settings, Connections, MCP";

pub const JSONRPC_PARSE_ERROR: i64 = -32700;
pub const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub const JSONRPC_INTERNAL_ERROR: i64 = -32603;

pub const INSTRUCTIONS: &str = "Corvus GCS is a ground control station connected to a real vehicle (PX4 or ArduPilot). Every tool acts on that vehicle through the station, with the checks the operator's own buttons have. The operator decides which tools you have and may have to approve a call in Corvus first; a refused call says why. Read get_vehicle_state before acting, and check the result of every command. Heights are metres above home unless a tool says otherwise. Direct control lasts only for the duration you give; the vehicle holds when you stop sending.";

/// Runs one Corvus endpoint: `(method, path, body)` to `(status, body)`.
pub type Dispatch = Arc<dyn Fn(&str, &str, Option<Map>) -> (u16, Value) + Send + Sync>;
/// The raw `mcp` block of the config.
pub type LoadSettings = Arc<dyn Fn() -> Option<Value> + Send + Sync>;
/// Persists the `mcp` block (`None` removes it).
pub type SaveSettings = Arc<dyn Fn(Option<Map>) -> std::io::Result<()> + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Listener = Arc<dyn Fn(&Value) + Send + Sync>;

/// How long the two waits last in real time. The sentences an agent reads
/// always name the constants above.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub confirm: Duration,
    pub plugin_call: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            confirm: Duration::from_secs_f64(CONFIRM_TIMEOUT_S),
            plugin_call: Duration::from_secs_f64(PLUGIN_CALL_TIMEOUT_S),
        }
    }
}

/// A tool that ran and failed: told to the agent, not raised as a protocol error.
#[derive(Debug)]
pub struct ToolError(pub String);

type ToolResult = Result<Value, ToolError>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn obj(v: Value) -> Map {
    v.as_object().cloned().unwrap_or_default()
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round_ties_even() / 10.0
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `^[A-Za-z0-9_-]{1,max}$`, where `$` lets one trailing newline pass.
fn short_id(s: &str, max: usize) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    (1..=max).contains(&s.chars().count()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `urllib.parse.quote(text, safe="")`.
fn quote(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn jsonrpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// `{"content": [...], "isError": ...}`, with `structuredContent` for an object.
pub fn tool_result(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => crate::plugins::update_check::dump_py(other),
    };
    let mut out = obj(json!({"content": [{"type": "text", "text": text}], "isError": is_error}));
    if value.is_object() && !is_error {
        out.insert("structuredContent".into(), value.clone());
    }
    Value::Object(out)
}

/// The name a plugin tool is listed under: the plugin id, then its own.
pub fn plugin_tool_name(plugin_id: &str, name: &str) -> String {
    let safe: String = plugin_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    first_chars(&format!("{safe}_{name}"), 64)
}

/// The tools a window announced, keeping only well formed ones.
pub fn clean_plugin_tools(raw: &Value) -> Vec<Map> {
    let mut out: Vec<Map> = Vec::new();
    let Some(list) = raw.as_array() else {
        return out;
    };
    for entry in list {
        if out.len() >= MAX_PLUGIN_TOOLS {
            break;
        }
        let Some(entry) = entry.as_object() else { continue };
        let Some(plugin) = entry.get("plugin").and_then(Value::as_str).filter(|p| is_valid_id(p)) else {
            continue;
        };
        let Some(name) = entry.get("name").and_then(Value::as_str).filter(|n| short_id(n, 48)) else {
            continue;
        };
        let full = plugin_tool_name(plugin, name);
        if out.iter().any(|t| t["full"] == json!(full)) || tools::find(&full).is_some() {
            continue;
        }
        let schema = match entry.get("input_schema") {
            Some(s) if s.get("type") == Some(&json!("object")) => s.clone(),
            _ => json!({"type": "object", "properties": {}}),
        };
        if crate::plugins::update_check::dump_py(&schema).len() > MAX_SCHEMA_BYTES {
            continue;
        }
        let text = |k: &str, max: usize, default: &str| match entry.get(k) {
            Some(Value::String(s)) => first_chars(s, max),
            _ => default.to_string(),
        };
        let mut t = Map::new();
        t.insert("plugin".into(), plugin.into());
        t.insert("name".into(), name.into());
        t.insert("full".into(), full.into());
        t.insert("title".into(), text("title", 120, name).into());
        t.insert("description".into(), text("description", MAX_DESCRIPTION, "").into());
        t.insert("input_schema".into(), schema);
        t.insert("confirm".into(), (entry.get("confirm") == Some(&json!(true))).into());
        t.insert(
            "read_only".into(),
            (entry.get("read_only") == Some(&json!(true))).into(),
        );
        t.insert("plugin_name".into(), text("plugin_name", 80, plugin).into());
        out.push(t);
    }
    out
}

fn schema(tool: &Tool, with_vehicle: bool) -> Value {
    let mut props = tool.properties.clone();
    if with_vehicle {
        props.insert(
            "vehicle".into(),
            json!({"type": "string", "description": "Vehicle id from list_vehicles. Leave out for the vehicle the operator flies."}),
        );
    }
    let mut s = Map::new();
    s.insert("type".into(), "object".into());
    s.insert("properties".into(), Value::Object(props));
    if !tool.required.is_empty() {
        s.insert("required".into(), json!(tool.required));
    }
    Value::Object(s)
}

/// The permissions with the tools each one opens, for the settings card.
pub fn tool_catalog() -> Value {
    Value::Array(
        PERMISSIONS
            .iter()
            .map(|p: &Permission| {
                json!({
                    "id": p.id, "label": p.label, "hint": p.hint, "writes": p.writes,
                    "tools": tools::builtin().iter().filter(|t| t.permission == p.id).map(|t| t.name).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

// ------------------------------------------------------------------ state

pub struct StreamQueue {
    items: Mutex<VecDeque<Option<Value>>>,
    wake: Condvar,
}

impl StreamQueue {
    fn put(&self, item: Option<Value>) {
        let mut q = lock(&self.items);
        if q.len() < STREAM_QUEUE {
            q.push_back(item);
            self.wake.notify_all();
        }
    }

    /// Waits up to `wait` for the next item: `Ok(None)` is the end of the stream.
    pub fn next(&self, wait: Duration) -> Option<Option<Value>> {
        let mut q = lock(&self.items);
        if q.is_empty() {
            q = self.wake.wait_timeout(q, wait).unwrap_or_else(|e| e.into_inner()).0;
        }
        q.pop_front()
    }
}

struct Session {
    client: String,
    client_version: String,
    seen: f64,
    streams: Vec<Arc<StreamQueue>>,
}

struct Window {
    seen: f64,
    tools: Vec<Map>,
}

enum Answer {
    Allow,
    Refuse(String),
}

struct Pending {
    id: String,
    client: String,
    tool: String,
    summary: String,
    permission: &'static str,
    deadline: f64,
    answer: Mutex<Option<Answer>>,
    wake: Condvar,
}

struct Call {
    outcome: Mutex<Option<Result<Value, String>>>,
    wake: Condvar,
}

struct Control {
    kind: &'static str,
    vehicle: Option<String>,
    path: &'static str,
    body: Map,
    until: f64,
}

#[derive(Default)]
struct State {
    sessions: IndexMap<String, Session>,
    windows: IndexMap<String, Window>,
    pending: IndexMap<String, Arc<Pending>>,
    calls: HashMap<String, Arc<Call>>,
    last: Option<Value>,
    tool_sig: Vec<String>,
    control: Option<Control>,
    stopping: bool,
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> (i % 16 * 8)) as u8 ^ (i as u8);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The MCP server.
pub struct McpService {
    dispatch: Dispatch,
    load: LoadSettings,
    save: SaveSettings,
    tokens: TokenStore,
    clock: Clock,
    timing: Timing,
    me: Weak<McpService>,
    state: Mutex<State>,
    control_wake: Condvar,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next_listener: Mutex<u64>,
    control_thread: Mutex<Option<JoinHandle<()>>>,
}

impl McpService {
    pub fn new(
        dispatch: Dispatch,
        load: LoadSettings,
        save: SaveSettings,
        token_path: impl Into<std::path::PathBuf>,
        clock: Clock,
        timing: Timing,
    ) -> Arc<McpService> {
        Arc::new_cyclic(|me| McpService {
            dispatch,
            load,
            save,
            tokens: TokenStore::new(token_path),
            clock,
            timing,
            me: me.clone(),
            state: Mutex::new(State::default()),
            control_wake: Condvar::new(),
            listeners: Mutex::new(Vec::new()),
            next_listener: Mutex::new(0),
            control_thread: Mutex::new(None),
        })
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    // ---- settings ----------------------------------------------------

    /// The settings in force.
    pub fn settings(&self) -> Map {
        settings::effective_settings((self.load)().as_ref())
    }

    pub fn enabled(&self) -> bool {
        self.settings().get("enabled") == Some(&json!(true))
    }

    /// Merges `patch` into the settings, persists, and tells everyone. The error
    /// is the sentence the route answers 400 with, or `Err(Err(io))` for a save
    /// that failed.
    pub fn update_settings(&self, patch: &Map) -> Result<Map, UpdateError> {
        let mut current = self.settings();
        for key in ["enabled", "fleet"] {
            if let Some(v) = patch.get(key) {
                if !v.is_boolean() {
                    return Err(UpdateError::Invalid(format!("{key} must be true or false")));
                }
                current.insert(key.into(), v.clone());
            }
        }
        for key in ["permissions", "confirm", "plugins"] {
            let Some(block) = patch.get(key) else { continue };
            let Some(block) = block.as_object() else {
                return Err(UpdateError::Invalid(format!("{key} must be an object")));
            };
            for (name, value) in block {
                if !value.is_boolean() {
                    return Err(UpdateError::Invalid(format!("{key}.{name} must be true or false")));
                }
                if key != "plugins" && settings::permission(name).is_none() {
                    return Err(UpdateError::Invalid(format!("unknown permission {name}")));
                }
                if key == "plugins" && !is_valid_id(name) {
                    return Err(UpdateError::Invalid(format!(
                        "invalid plugin id {}",
                        crate::plugins::config::py_repr(name)
                    )));
                }
                if let Some(Value::Object(m)) = current.get_mut(key) {
                    m.insert(name.clone(), value.clone());
                }
            }
        }
        (self.save)(settings::compact_settings(&current)).map_err(UpdateError::Save)?;
        if current.get("enabled") != Some(&json!(true)) {
            self.deny_all("The operator switched the MCP server off.");
            self.end_control("MCP switched off");
        }
        if current["permissions"].get("control") != Some(&json!(true)) {
            self.end_control("direct control switched off");
        }
        if patch.contains_key("enabled") {
            let on = if current.get("enabled") == Some(&json!(true)) {
                "on"
            } else {
                "off"
            };
            self.log(&format!("MCP server {on}"), "info", false);
        }
        self.changed();
        Ok(self.settings())
    }

    /// The config was replaced (an import, a reset): re-read and tell everyone.
    pub fn settings_reloaded(&self) {
        if !self.enabled() {
            self.deny_all("The operator switched the MCP server off.");
            self.end_control("MCP switched off");
        }
        self.changed();
    }

    /// The operator's Stop: every permission that changes anything goes off.
    /// Pending confirmations are refused and direct control ends in hold.
    /// Reading stays, so the agent can still see what happened.
    pub fn stop(&self) -> Result<Map, std::io::Error> {
        let mut current = self.settings();
        if let Some(Value::Object(perms)) = current.get_mut("permissions") {
            for p in PERMISSIONS.iter().filter(|p| p.writes) {
                perms.insert(p.id.into(), false.into());
            }
        }
        (self.save)(settings::compact_settings(&current))?;
        self.deny_all("The operator stopped the AI client.");
        self.end_control("stopped by the operator");
        self.log(
            "MCP: the operator took every permission that changes the vehicle away",
            "warning",
            true,
        );
        self.changed();
        Ok(self.settings())
    }

    // ---- token -------------------------------------------------------

    pub fn token(&self) -> String {
        self.tokens.token()
    }

    /// A new token. Clients holding the old one are refused from now on.
    pub fn regenerate_token(&self) -> String {
        let token = self.tokens.regenerate();
        lock(&self.state).sessions.clear();
        self.log(
            "MCP: new access token, clients have to be given it again",
            "info",
            false,
        );
        self.emit_status();
        token
    }

    pub fn authorized(&self, header: Option<&str>) -> bool {
        self.tokens.authorized(header)
    }

    // ---- listeners (the UI's "mcp" event topic) -----------------------

    pub fn add_listener(&self, f: Listener) -> u64 {
        let mut next = lock(&self.next_listener);
        *next += 1;
        lock(&self.listeners).push((*next, f));
        *next
    }

    pub fn remove_listener(&self, id: u64) {
        lock(&self.listeners).retain(|(i, _)| *i != id);
    }

    fn emit(&self, event: Value) {
        let listeners: Vec<Listener> = lock(&self.listeners).iter().map(|(_, f)| f.clone()).collect();
        for f in listeners {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&event))).is_err() {
                eprintln!("corvus: mcp: a listener failed");
            }
        }
    }

    fn emit_status(&self) {
        let mut s = self.status();
        s.insert("type".into(), "status".into());
        self.emit(Value::Object(s));
    }

    fn log(&self, text: &str, level: &str, notify: bool) {
        eprintln!("corvus: {text}");
        self.emit(json!({"type": "log", "text": text, "level": level, "notify": notify}));
    }

    // ---- status ------------------------------------------------------

    fn pending_event(&self, p: &Pending, now: f64) -> Value {
        json!({
            "type": "confirm", "id": p.id, "client": p.client, "tool": p.tool, "summary": p.summary,
            "permission": p.permission,
            "permission_label": settings::permission(p.permission).map_or("", |x| x.label),
            "expires_in_s": round1((p.deadline - now).max(0.0)),
        })
    }

    /// What the settings card and the flight indicator show.
    pub fn status(&self) -> Map {
        let now = self.now();
        let settings = self.settings();
        let mut st = lock(&self.state);
        // One line per client: a client that reconnects opens a new session and
        // leaves the old one behind until it is evicted.
        let mut order: Vec<&Session> = st.sessions.values().collect();
        order.sort_by(|a, b| b.seen.total_cmp(&a.seen));
        let mut clients: Vec<Value> = Vec::new();
        let mut seen_names: Vec<(&str, &str)> = Vec::new();
        for s in order {
            let key = (s.client.as_str(), s.client_version.as_str());
            if seen_names.contains(&key) {
                continue;
            }
            seen_names.push(key);
            clients.push(json!({"name": key.0, "version": key.1, "seen_s": round1(now - s.seen)}));
        }
        let control = st.control.as_ref().map(|c| {
            json!({"kind": c.kind, "vehicle": c.vehicle.clone().unwrap_or_default(),
                   "ends_in_s": round1((c.until - now).max(0.0))})
        });
        let pending: Vec<Value> = st.pending.values().map(|p| self.pending_event(p, now)).collect();
        let windows = live_windows(&mut st, now).len();
        let last = st.last.clone();
        let active = clients
            .iter()
            .any(|c| c["seen_s"].as_f64().unwrap_or(f64::MAX) <= CLIENT_ACTIVE_S);
        let writes = PERMISSIONS
            .iter()
            .filter(|p| p.writes)
            .any(|p| settings["permissions"].get(p.id) == Some(&json!(true)));
        clients.truncate(8);
        let mut out = Map::new();
        out.insert("enabled".into(), settings["enabled"].clone());
        out.insert("active".into(), active.into());
        out.insert("clients".into(), Value::Array(clients));
        out.insert("control".into(), control.unwrap_or(Value::Null));
        out.insert("writes".into(), writes.into());
        out.insert("pending".into(), Value::Array(pending));
        out.insert("windows".into(), windows.into());
        out.insert("last".into(), last.unwrap_or(Value::Null));
        out
    }

    // ---- windows and plugin tools ------------------------------------

    /// A Corvus window says it is open, with the plugin tools it can run.
    pub fn window_heartbeat(&self, window_id: &Value, tools: &Value) -> Result<Value, String> {
        let Some(id) = window_id.as_str().filter(|i| short_id(i, 64)) else {
            return Err("id must be a short identifier".into());
        };
        let cleaned = clean_plugin_tools(tools);
        let n = cleaned.len();
        let now = self.now();
        lock(&self.state).windows.insert(
            id.to_string(),
            Window {
                seen: now,
                tools: cleaned,
            },
        );
        self.check_tools_changed();
        Ok(json!({"tools": n}))
    }

    /// Every plugin tool an open window offers, the first window first.
    pub fn plugin_tools(&self) -> Vec<Map> {
        let now = self.now();
        let mut st = lock(&self.state);
        live_windows(&mut st, now);
        let mut windows: Vec<(&String, &Window)> = st.windows.iter().collect();
        windows.sort_by(|a, b| a.1.seen.total_cmp(&b.1.seen));
        let mut out: IndexMap<String, Map> = IndexMap::new();
        for (id, window) in windows {
            for t in &window.tools {
                let full = t["full"].as_str().unwrap_or("").to_string();
                out.entry(full).or_insert_with(|| {
                    let mut t = t.clone();
                    t.insert("window".into(), id.clone().into());
                    t
                });
            }
        }
        out.into_values().collect()
    }

    /// The plugins that offer tools, with their tool names, for the card.
    pub fn plugin_summary(&self) -> Value {
        let mut grouped: IndexMap<String, Map> = IndexMap::new();
        for t in self.plugin_tools() {
            let plugin = t["plugin"].as_str().unwrap_or("").to_string();
            let entry = grouped
                .entry(plugin.clone())
                .or_insert_with(|| obj(json!({"id": plugin, "name": t["plugin_name"], "tools": []})));
            if let Some(Value::Array(list)) = entry.get_mut("tools") {
                list.push(t["full"].clone());
            }
        }
        let mut list: Vec<Map> = grouped.into_values().collect();
        list.sort_by_key(|e| e["name"].as_str().unwrap_or("").to_lowercase());
        Value::Array(list.into_iter().map(Value::Object).collect())
    }

    // ---- the tool list -----------------------------------------------

    /// The tools this agent may call right now, as `tools/list` gives them.
    pub fn tools(&self) -> Vec<Value> {
        let settings = self.settings();
        let fleet = settings.get("fleet") == Some(&json!(true));
        let mut out = Vec::new();
        for t in tools::builtin() {
            if level(&settings, t.permission) == "off" {
                continue;
            }
            out.push(json!({
                "name": t.name, "title": t.title, "description": t.description,
                "inputSchema": schema(t, t.vehicle && fleet),
                "annotations": {"title": t.title, "readOnlyHint": t.read_only,
                                "destructiveHint": !t.read_only, "openWorldHint": true},
            }));
        }
        if level(&settings, "plugins") != "off" {
            for t in self.plugin_tools() {
                if !plugin_allowed(&settings, t["plugin"].as_str().unwrap_or("")) {
                    continue;
                }
                let description = format!(
                    "{} (plugin {})",
                    t["description"].as_str().unwrap_or(""),
                    t["plugin_name"].as_str().unwrap_or("")
                );
                out.push(json!({
                    "name": t["full"], "title": t["title"], "description": description.trim(),
                    "inputSchema": t["input_schema"],
                    "annotations": {"title": t["title"], "readOnlyHint": t["read_only"],
                                    "destructiveHint": t["read_only"] != json!(true)},
                }));
            }
        }
        out
    }

    fn tool_sig(&self) -> Vec<String> {
        let mut sig: Vec<String> = self
            .tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        sig.sort();
        sig
    }

    fn check_tools_changed(&self) {
        let sig = if self.enabled() { self.tool_sig() } else { Vec::new() };
        let streams: Vec<Arc<StreamQueue>> = {
            let mut st = lock(&self.state);
            if sig == st.tool_sig {
                return;
            }
            st.tool_sig = sig;
            st.sessions.values().flat_map(|s| s.streams.iter().cloned()).collect()
        };
        let note = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        for q in streams {
            q.put(Some(note.clone()));
        }
    }

    fn changed(&self) {
        self.check_tools_changed();
        self.emit_status();
    }

    // ---- JSON-RPC ----------------------------------------------------

    /// One POSTed JSON-RPC message (or batch): `(status, reply, headers)`. A
    /// reply of `None` is a 202 with no body: only notifications or responses.
    pub fn handle_post(
        &self,
        message: &Value,
        session_id: Option<&str>,
    ) -> (u16, Option<Value>, Vec<(String, String)>) {
        let mut headers = Vec::new();
        let batch = message.is_array();
        let messages: Vec<&Value> = match message {
            Value::Array(a) => a.iter().collect(),
            other => vec![other],
        };
        if batch && messages.is_empty() {
            return (
                400,
                Some(jsonrpc_error(Value::Null, JSONRPC_INVALID_REQUEST, "empty batch")),
                headers,
            );
        }
        let is_init = messages.iter().any(|m| m.get("method") == Some(&json!("initialize")));
        let mut session: Option<String> = None;
        if let Some(sid) = session_id.filter(|s| !s.is_empty() && !is_init) {
            let mut st = lock(&self.state);
            match st.sessions.get_mut(sid) {
                Some(s) => {
                    s.seen = self.now();
                    session = Some(sid.to_string());
                }
                None => {
                    return (
                        404,
                        Some(jsonrpc_error(
                            Value::Null,
                            JSONRPC_INVALID_REQUEST,
                            "unknown session, initialize again",
                        )),
                        headers,
                    )
                }
            }
        }
        let mut replies = Vec::new();
        for msg in messages {
            let (reply, new_session) = self.rpc(msg, session.as_deref());
            if let Some(sid) = new_session {
                headers.retain(|(k, _): &(String, String)| k != "Mcp-Session-Id");
                headers.push(("Mcp-Session-Id".into(), sid));
            }
            if let Some(r) = reply {
                replies.push(r);
            }
        }
        if replies.is_empty() {
            return (202, None, headers);
        }
        let body = if batch {
            Value::Array(replies)
        } else {
            replies.remove(0)
        };
        (200, Some(body), headers)
    }

    fn rpc(&self, msg: &Value, session: Option<&str>) -> (Option<Value>, Option<String>) {
        let Some(m) = msg.as_object().filter(|m| m.get("jsonrpc") == Some(&json!("2.0"))) else {
            return (
                Some(jsonrpc_error(
                    Value::Null,
                    JSONRPC_INVALID_REQUEST,
                    "not a JSON-RPC 2.0 message",
                )),
                None,
            );
        };
        if !m.contains_key("id") {
            return (None, None);
        }
        let id = m["id"].clone();
        let Some(method) = m.get("method").and_then(Value::as_str) else {
            // A response to something we never ask: nothing to answer.
            return (None, None);
        };
        let params = match m.get("params") {
            None => Map::new(),
            Some(Value::Object(p)) => p.clone(),
            Some(v) if !truthy(v) => Map::new(),
            Some(_) => {
                return (
                    Some(jsonrpc_error(id, JSONRPC_INVALID_PARAMS, "params must be an object")),
                    None,
                )
            }
        };
        match method {
            "initialize" => {
                let (result, sid) = self.initialize(&params);
                (Some(jsonrpc_result(&id, result)), Some(sid))
            }
            "ping" => (Some(jsonrpc_result(&id, json!({}))), None),
            "tools/list" => (Some(jsonrpc_result(&id, json!({"tools": self.tools()}))), None),
            "tools/call" => {
                let name = params.get("name");
                let args = match params.get("arguments") {
                    None => Some(Map::new()),
                    Some(Value::Object(a)) => Some(a.clone()),
                    Some(v) if !truthy(v) => Some(Map::new()),
                    Some(_) => None,
                };
                let (Some(Value::String(name)), Some(args)) = (name, args) else {
                    return (
                        Some(jsonrpc_error(
                            id,
                            JSONRPC_INVALID_PARAMS,
                            "tools/call needs a name and an arguments object",
                        )),
                        None,
                    );
                };
                match self.call_tool(name, &args, session) {
                    None => (
                        Some(jsonrpc_error(
                            id,
                            JSONRPC_INVALID_PARAMS,
                            &format!("unknown tool {name}"),
                        )),
                        None,
                    ),
                    Some(r) => (Some(jsonrpc_result(&id, r)), None),
                }
            }
            "resources/list" => (Some(jsonrpc_result(&id, json!({"resources": []}))), None),
            "prompts/list" => (Some(jsonrpc_result(&id, json!({"prompts": []}))), None),
            "resources/templates/list" => (Some(jsonrpc_result(&id, json!({"resourceTemplates": []}))), None),
            other => (
                Some(jsonrpc_error(
                    id,
                    JSONRPC_METHOD_NOT_FOUND,
                    &format!("unknown method {other}"),
                )),
                None,
            ),
        }
    }

    fn initialize(&self, params: &Map) -> (Value, String) {
        let version = match params.get("protocolVersion").and_then(Value::as_str) {
            Some(v) if PROTOCOL_VERSIONS.contains(&v) => v,
            _ => DEFAULT_PROTOCOL,
        };
        let info = params
            .get("clientInfo")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let text = |k: &str, default: &str, max: usize| {
            let v = info.get(k).filter(|v| truthy(v));
            first_chars(&v.map_or_else(|| default.to_string(), |v| fmt(Some(v))), max)
        };
        let (client, client_version) = (text("name", "MCP client", 80), text("version", "", 40));
        let sid = random_hex(16);
        let now = self.now();
        let sig = self.tool_sig();
        {
            let mut st = lock(&self.state);
            if st.sessions.len() >= MAX_SESSIONS {
                let oldest = st
                    .sessions
                    .iter()
                    .min_by(|a, b| a.1.seen.total_cmp(&b.1.seen))
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    drop_session(&mut st, &k);
                }
            }
            st.sessions.insert(
                sid.clone(),
                Session {
                    client: client.clone(),
                    client_version,
                    seen: now,
                    streams: Vec::new(),
                },
            );
            st.tool_sig = sig;
        }
        self.log(&format!("MCP: {client} connected"), "info", false);
        self.emit_status();
        (
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": SERVER_NAME, "title": SERVER_TITLE, "version": crate::version::version()},
                "instructions": INSTRUCTIONS,
            }),
            sid,
        )
    }

    /// A client said goodbye (`DELETE /mcp`).
    pub fn end_session(&self, session_id: Option<&str>) -> bool {
        {
            let mut st = lock(&self.state);
            let Some(sid) = session_id.filter(|s| st.sessions.contains_key(*s)) else {
                return false;
            };
            drop_session(&mut st, sid);
        }
        self.emit_status();
        true
    }

    /// The queue a `GET /mcp` stream drains, or `None` without a session.
    pub(super) fn open_stream(&self, session_id: Option<&str>) -> Option<Arc<StreamQueue>> {
        let mut st = lock(&self.state);
        let session = st.sessions.get_mut(session_id.unwrap_or(""))?;
        let q = Arc::new(StreamQueue {
            items: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
        });
        session.streams.push(q.clone());
        Some(q)
    }

    pub(super) fn close_stream(&self, session_id: Option<&str>, q: &Arc<StreamQueue>) {
        let mut st = lock(&self.state);
        if let Some(s) = st.sessions.get_mut(session_id.unwrap_or("")) {
            s.streams.retain(|x| !Arc::ptr_eq(x, q));
        }
    }

    // ---- calling a tool ----------------------------------------------

    /// Runs one tool for an agent. `None` for a name that is no tool at all.
    pub fn call_tool(&self, name: &str, args: &Map, session: Option<&str>) -> Option<Value> {
        let settings = self.settings();
        let client = session
            .and_then(|s| lock(&self.state).sessions.get(s).map(|s| s.client.clone()))
            .unwrap_or_else(|| "AI client".to_string());
        let tool = tools::find(name);
        let plugin_tool = if tool.is_none() {
            Some(self.plugin_tools().into_iter().find(|t| t["full"] == json!(name))?)
        } else {
            None
        };
        let permission: &'static str = tool.map_or("plugins", |t| t.permission);
        let perm_level = level(&settings, permission);
        let plugin_off = plugin_tool
            .as_ref()
            .is_some_and(|t| !plugin_allowed(&settings, t["plugin"].as_str().unwrap_or("")));
        if perm_level == "off" || plugin_off {
            let label = settings::permission(permission).map_or("", |p| p.label);
            return Some(self.refused(
                name,
                &format!("The operator has not allowed this in Corvus ({label}, under {WHERE})."),
            ));
        }
        let vehicle = args.get("vehicle").filter(|v| !v.is_null());
        if let Some(v) = vehicle {
            if !tool.is_some_and(|t| t.vehicle) {
                return Some(self.refused(name, "This tool does not take a vehicle."));
            }
            if settings.get("fleet") != Some(&json!(true)) {
                return Some(self.refused(
                    name,
                    "The operator allows only the vehicle they fly. Leave vehicle out.",
                ));
            }
            if !v.as_str().is_some_and(|s| !s.is_empty()) {
                return Some(self.refused(name, "vehicle must be an id from list_vehicles"));
            }
        }
        let summary = match (&tool, &plugin_tool) {
            (Some(t), _) => (t.summary)(args).unwrap_or_else(|| name.to_string()),
            (None, Some(p)) => p["title"].as_str().unwrap_or(name).to_string(),
            _ => name.to_string(),
        };
        let where_ = vehicle.map(|v| format!(" ({})", fmt(Some(v)))).unwrap_or_default();
        let mut ask = perm_level == "confirm" || plugin_tool.as_ref().is_some_and(|p| p["confirm"] == json!(true));
        if ask && tool.is_some_and(|t| t.permission == "control") && self.control_live(vehicle.and_then(Value::as_str))
        {
            ask = false;
        }
        if ask && tool.is_some_and(|t| t.name == "stop_control") {
            ask = false;
        }
        if ask {
            if let Err(reason) = self.confirm(&client, name, &format!("{summary}{where_}"), permission) {
                return Some(self.refused(name, &reason));
            }
        }
        let read_only = tool.map_or_else(
            || plugin_tool.as_ref().is_some_and(|p| p["read_only"] == json!(true)),
            |t| t.read_only,
        );
        if !read_only {
            self.log(&format!("MCP {client}: {summary}{where_}"), "cmd", false);
        }
        let outcome = match (&tool, &plugin_tool) {
            (Some(t), _) => self.run(t, args),
            (None, Some(p)) => self.call_plugin(p, args),
            _ => unreachable!("a tool was found above"),
        };
        match outcome {
            Err(ToolError(why)) => {
                self.remember(name, false);
                if !read_only {
                    self.log(&format!("MCP {client}: {summary} failed: {why}"), "error", true);
                }
                Some(tool_result(&Value::String(why), true))
            }
            Ok(value) => {
                self.remember(name, true);
                let value = if value.is_null() { json!({"ok": true}) } else { value };
                Some(tool_result(&value, false))
            }
        }
    }

    fn refused(&self, name: &str, reason: &str) -> Value {
        self.remember(name, false);
        self.log(&format!("MCP: {name} refused. {reason}"), "warning", true);
        tool_result(&Value::String(reason.to_string()), true)
    }

    fn remember(&self, name: &str, ok: bool) {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        lock(&self.state).last = Some(json!({"tool": name, "ok": ok, "at": at}));
        self.emit_status();
    }

    /// Runs one Corvus endpoint for a tool; a refusal is a [`ToolError`].
    pub fn api(&self, method: &str, path: &str, body: Option<Map>, vehicle: Option<&str>) -> ToolResult {
        let (mut path, mut body) = (path.to_string(), body);
        if let Some(v) = vehicle.filter(|v| !v.is_empty()) {
            if method == "GET" {
                path = format!("{path}?vehicle={}", quote(v));
            } else {
                let mut b = body.take().unwrap_or_default();
                b.insert("vehicle".into(), v.into());
                body = Some(b);
            }
        }
        let (status, reply) = (self.dispatch)(method, &path, if method == "GET" { None } else { body });
        let reply = if reply.is_object() {
            reply
        } else {
            json!({"value": reply})
        };
        let error = reply.get("error").filter(|e| truthy(e));
        if status >= 400 || reply.get("ok") == Some(&json!(false)) || (error.is_some() && status >= 400) {
            return Err(ToolError(match error {
                Some(e) => fmt(Some(e)),
                None => format!("refused ({status})"),
            }));
        }
        Ok(reply)
    }

    fn run(&self, tool: &Tool, args: &Map) -> ToolResult {
        let vehicle = args.get("vehicle").and_then(Value::as_str);
        match tool.run {
            Run::Get(path, key) => {
                let out = self.api("GET", path, None, vehicle)?;
                Ok(match key {
                    Some(k) => out.get(k).cloned().unwrap_or(Value::Null),
                    None => out,
                })
            }
            Run::Post(path, body) => {
                let b = body.map_or_else(Map::new, |f| f(args));
                self.api("POST", path, Some(b), vehicle)
            }
            Run::ListParameters => {
                let mut params = self.params_or_hint()?;
                if let Some(prefix) = args.get("prefix").filter(|p| !p.is_null()) {
                    let Some(prefix) = prefix.as_str() else {
                        return Err(ToolError("prefix must be a string".into()));
                    };
                    let up = prefix.to_uppercase();
                    params.retain(|p| fmt(p.get("name")).to_uppercase().starts_with(&up));
                }
                let total = params.len();
                params.truncate(MAX_PARAMS_LISTED);
                Ok(json!({"count": total, "truncated": total > MAX_PARAMS_LISTED, "params": params}))
            }
            Run::GetParameters => {
                let names: Vec<&str> = match args.get("names") {
                    Some(Value::Array(a)) if !a.is_empty() && a.iter().all(Value::is_string) => {
                        a.iter().filter_map(Value::as_str).collect()
                    }
                    _ => return Err(ToolError("names must be a non-empty list of parameter names".into())),
                };
                let all = self.params_or_hint()?;
                let by_name: HashMap<String, &Value> = all.iter().map(|p| (fmt(p.get("name")), p)).collect();
                let found: Vec<&Value> = names.iter().filter_map(|n| by_name.get(*n).copied()).collect();
                let missing: Vec<&&str> = names.iter().filter(|n| !by_name.contains_key(**n)).collect();
                Ok(json!({"params": found, "missing": missing}))
            }
            Run::SetParameter => {
                let name = req_str(args, "name")?;
                let value = num(args, "value", None)?;
                let known: Vec<String> = self.params_or_hint()?.iter().map(|p| fmt(p.get("name"))).collect();
                if !known.contains(&name) {
                    return Err(ToolError(format!(
                        "{name} is not a parameter the connected firmware reported."
                    )));
                }
                self.api(
                    "POST",
                    "/api/params/set",
                    Some(obj(json!({"name": name, "value": value}))),
                    None,
                )?;
                Ok(json!({"ok": true, "name": name, "value": value}))
            }
            Run::FlyTo => {
                let points = match args.get("points") {
                    Some(Value::Array(p)) if !p.is_empty() => p,
                    _ => return Err(ToolError("points must be a non-empty list".into())),
                };
                let mut wire = Vec::new();
                for (i, p) in points.iter().enumerate() {
                    let Some(p) = p.as_object() else {
                        return Err(ToolError(format!("point {i} must be an object")));
                    };
                    let mut item = obj(json!({"lat": p.get("lat"), "lon": p.get("lon")}));
                    if let Some(alt) = p.get("alt").filter(|a| !a.is_null()) {
                        item.insert("alt_agl".into(), alt.clone());
                    }
                    wire.push(Value::Object(item));
                }
                self.api(
                    "POST",
                    "/api/mavlink/gotopoints",
                    Some(obj(json!({"points": wire}))),
                    vehicle,
                )
            }
            Run::Command => {
                let mut body = obj(json!({"command": req_str(args, "command")?}));
                let params = match args.get("params") {
                    None => json!([]),
                    Some(p @ Value::Array(_)) => p.clone(),
                    Some(_) => return Err(ToolError("params must be a list of numbers".into())),
                };
                body.insert("params".into(), params);
                for key in ["lat", "lon", "alt", "frame"] {
                    if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
                        body.insert(key.into(), v.clone());
                    }
                }
                self.api("POST", "/api/mavlink/command", Some(body), vehicle)
            }
            Run::Manual => self.control("manual", args),
            Run::Velocity => self.control("velocity", args),
            Run::StopControl => self.stop_control(vehicle),
        }
    }

    fn params_or_hint(&self) -> Result<Vec<Value>, ToolError> {
        let body = self.api("GET", "/api/params", None, None)?;
        if !body.get("complete").is_some_and(truthy) {
            let get = |k: &str, d: &str| body.get(k).map_or_else(|| d.to_string(), |v| fmt(Some(v)));
            return Err(ToolError(format!(
                "The parameter list is not loaded (state {}, {} of {}). Call download_parameters and try again in a few seconds.",
                get("state", "idle"),
                get("received", "0"),
                get("count", "0")
            )));
        }
        Ok(body
            .get("params")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    // ---- confirmation -------------------------------------------------

    /// Asks the operator. `Ok`, or the reason it was not allowed.
    fn confirm(&self, client: &str, tool: &str, summary: &str, permission: &'static str) -> Result<(), String> {
        let now = self.now();
        let pending = {
            let mut st = lock(&self.state);
            if live_windows(&mut st, now).is_empty() {
                return Err("This needs the operator's approval, and no Corvus window is open to ask in.".into());
            }
            let p = Arc::new(Pending {
                id: random_hex(8),
                client: client.into(),
                tool: tool.into(),
                summary: summary.into(),
                permission,
                deadline: now + CONFIRM_TIMEOUT_S,
                answer: Mutex::new(None),
                wake: Condvar::new(),
            });
            st.pending.insert(p.id.clone(), p.clone());
            p
        };
        self.emit(self.pending_event(&pending, now));
        self.emit_status();
        {
            let guard = lock(&pending.answer);
            let _ = pending
                .wake
                .wait_timeout_while(guard, self.timing.confirm, |a| a.is_none())
                .unwrap_or_else(|e| e.into_inner());
        }
        lock(&self.state).pending.shift_remove(&pending.id);
        let answer = lock(&pending.answer).take();
        self.emit(json!({"type": "confirm_done", "id": pending.id, "allowed": matches!(answer, Some(Answer::Allow))}));
        self.emit_status();
        match answer {
            Some(Answer::Allow) => Ok(()),
            None => Err(format!("The operator did not answer within {} s.", CONFIRM_TIMEOUT_S)),
            Some(Answer::Refuse(why)) => Err(why),
        }
    }

    /// The operator's answer from the confirmation dialog.
    pub fn answer(&self, pending_id: &Value, allow: &Value) -> bool {
        let Some(id) = pending_id.as_str() else { return false };
        let Some(p) = lock(&self.state).pending.get(id).cloned() else {
            return false;
        };
        *lock(&p.answer) = Some(if allow == &json!(true) {
            Answer::Allow
        } else {
            Answer::Refuse("The operator refused.".into())
        });
        p.wake.notify_all();
        true
    }

    fn deny_all(&self, reason: &str) {
        let st = lock(&self.state);
        for p in st.pending.values() {
            *lock(&p.answer) = Some(Answer::Refuse(reason.to_string()));
            p.wake.notify_all();
        }
        for c in st.calls.values() {
            *lock(&c.outcome) = Some(Err(reason.to_string()));
            c.wake.notify_all();
        }
    }

    // ---- plugin calls --------------------------------------------------

    fn call_plugin(&self, tool: &Map, args: &Map) -> ToolResult {
        let id = random_hex(8);
        let call = Arc::new(Call {
            outcome: Mutex::new(None),
            wake: Condvar::new(),
        });
        lock(&self.state).calls.insert(id.clone(), call.clone());
        self.emit(
            json!({"type": "call", "id": id, "window": tool["window"], "plugin": tool["plugin"],
                         "tool": tool["name"], "args": args}),
        );
        let outcome = {
            let guard = lock(&call.outcome);
            let (mut guard, _) = call
                .wake
                .wait_timeout_while(guard, self.timing.plugin_call, |o| o.is_none())
                .unwrap_or_else(|e| e.into_inner());
            guard.take()
        };
        lock(&self.state).calls.remove(&id);
        match outcome {
            None => Err(ToolError(format!(
                "The plugin did not answer within {PLUGIN_CALL_TIMEOUT_S} s."
            ))),
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(ToolError(if e.is_empty() { "The plugin refused.".into() } else { e })),
        }
    }

    /// A window's answer to a plugin call.
    pub fn call_result(&self, call_id: &Value, ok: &Value, result: &Value, error: &Value) -> bool {
        let Some(id) = call_id.as_str() else { return false };
        let Some(call) = lock(&self.state).calls.get(id).cloned() else {
            return false;
        };
        *lock(&call.outcome) = Some(if ok == &json!(true) {
            Ok(result.clone())
        } else {
            Err(error.as_str().unwrap_or("").to_string())
        });
        call.wake.notify_all();
        true
    }

    // ---- direct control -------------------------------------------------

    fn control_live(&self, vehicle: Option<&str>) -> bool {
        let now = self.now();
        lock(&self.state)
            .control
            .as_ref()
            .is_some_and(|c| c.vehicle.as_deref() == vehicle.filter(|v| !v.is_empty()) && c.until > now)
    }

    /// Starts or extends direct control: repeated until it runs out, then hold.
    fn control(&self, kind: &'static str, args: &Map) -> ToolResult {
        let seconds = num(args, "duration_s", Some(CONTROL_DEFAULT_S))?;
        if !(0.2..=CONTROL_MAX_S).contains(&seconds) {
            return Err(ToolError(format!("duration_s must be between 0.2 and {CONTROL_MAX_S}")));
        }
        let vehicle = args
            .get("vehicle")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        let (body, path) = if kind == "manual" {
            (
                obj(json!({"x": num(args, "x", Some(0.0))?, "y": num(args, "y", Some(0.0))?,
                           "z": num(args, "z", Some(0.5))?, "r": num(args, "r", Some(0.0))?})),
                "/api/mavlink/manual",
            )
        } else {
            let mut body = obj(
                json!({"vx": num(args, "vx", Some(0.0))?, "vy": num(args, "vy", Some(0.0))?,
                                      "vz": num(args, "vz", Some(0.0))?}),
            );
            if args.get("yaw_rate").is_some_and(|v| !v.is_null()) {
                body.insert("yaw_rate".into(), json!(num(args, "yaw_rate", None)?));
            }
            let frame = args.get("frame").cloned().unwrap_or(json!("local"));
            if frame != json!("local") && frame != json!("body") {
                return Err(ToolError("frame must be \"local\" or \"body\"".into()));
            }
            body.insert("frame".into(), frame);
            (body, "/api/mavlink/velocity")
        };
        // The first frame goes now, through the endpoint, so a refusal (wrong
        // mode, a plane, no link) is the agent's answer rather than a silent
        // stream of failures.
        self.api("POST", path, Some(body.clone()), vehicle.as_deref())?;
        let mut frame_body = body;
        if let Some(v) = &vehicle {
            frame_body.insert("vehicle".into(), v.clone().into());
        }
        {
            let mut st = lock(&self.state);
            st.control = Some(Control {
                kind,
                vehicle,
                path,
                body: frame_body,
                until: self.now() + seconds,
            });
        }
        {
            let mut thread = lock(&self.control_thread);
            if thread.as_ref().is_none_or(JoinHandle::is_finished) {
                if let Some(me) = self.me.upgrade() {
                    *thread = std::thread::Builder::new()
                        .name("corvus-mcp-control".into())
                        .spawn(move || me.control_loop())
                        .ok();
                }
            }
        }
        self.control_wake.notify_all();
        self.emit_status();
        Ok(json!({"ok": true, "holds_after_s": seconds}))
    }

    /// Ends direct control and holds.
    fn stop_control(&self, vehicle: Option<&str>) -> ToolResult {
        let (active, target) = {
            let mut st = lock(&self.state);
            let active = st.control.is_some();
            let target = match &st.control {
                Some(c) => c.vehicle.clone(),
                None => vehicle.filter(|v| !v.is_empty()).map(str::to_string),
            };
            st.control = None;
            (active, target)
        };
        self.control_wake.notify_all();
        self.api("POST", "/api/mavlink/hold", Some(Map::new()), target.as_deref())?;
        self.emit_status();
        Ok(json!({"ok": true, "was_active": active}))
    }

    fn end_control(&self, why: &str) {
        let control = lock(&self.state).control.take();
        self.control_wake.notify_all();
        if let Some(c) = control {
            self.hold(c.vehicle.as_deref(), why);
        }
    }

    fn hold(&self, vehicle: Option<&str>, why: &str) {
        match self.api("POST", "/api/mavlink/hold", Some(Map::new()), vehicle) {
            Ok(_) => self.log(&format!("MCP: direct control ended ({why}), holding"), "warning", true),
            Err(ToolError(e)) => self.log(
                &format!("MCP: direct control ended ({why}) but hold failed: {e}"),
                "error",
                true,
            ),
        }
        self.emit_status();
    }

    fn control_loop(&self) {
        let period = Duration::from_secs_f64(1.0 / CONTROL_RATE_HZ);
        loop {
            let (path, body, vehicle, expired) = {
                let mut st = lock(&self.state);
                while st.control.is_none() && !st.stopping {
                    st = self
                        .control_wake
                        .wait_timeout(st, Duration::from_secs(5))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                if st.stopping {
                    return;
                }
                let Some(c) = st.control.as_ref() else { continue };
                let (path, body, vehicle) = (c.path, c.body.clone(), c.vehicle.clone());
                let expired = self.now() >= c.until;
                if expired {
                    st.control = None;
                }
                (path, body, vehicle, expired)
            };
            if expired {
                self.hold(vehicle.as_deref(), "no new command");
                continue;
            }
            let (status, reply) = (self.dispatch)("POST", path, Some(body));
            if status >= 400 {
                eprintln!("corvus: mcp: control frame refused: {}", fmt(reply.get("error")));
            }
            let st = lock(&self.state);
            if !st.stopping {
                let _ = self.control_wake.wait_timeout(st, period);
            }
        }
    }

    // ---- teardown --------------------------------------------------------

    /// Refuses what waits, holds a vehicle under direct control, joins the thread.
    pub fn shutdown(&self) {
        self.deny_all("Corvus GCS is closing.");
        let control = {
            let mut st = lock(&self.state);
            let control = st.control.take();
            st.stopping = true;
            let ids: Vec<String> = st.sessions.keys().cloned().collect();
            for id in ids {
                drop_session(&mut st, &id);
            }
            control
        };
        self.control_wake.notify_all();
        if let Some(c) = control {
            let body = c.vehicle.map(|v| obj(json!({"vehicle": v})));
            let _ = (self.dispatch)("POST", "/api/mavlink/hold", Some(body.unwrap_or_default()));
        }
        let thread = lock(&self.control_thread).take();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
}

/// Why `update_settings` did not apply.
#[derive(Debug)]
pub enum UpdateError {
    /// A value that is not right: the route answers 400 with the sentence.
    Invalid(String),
    /// The file could not be written.
    Save(std::io::Error),
}

fn live_windows(st: &mut State, now: f64) -> &IndexMap<String, Window> {
    st.windows.retain(|_, w| now - w.seen <= WINDOW_TTL_S);
    &st.windows
}

fn drop_session(st: &mut State, sid: &str) {
    if let Some(s) = st.sessions.shift_remove(sid) {
        for q in s.streams {
            q.put(None);
        }
    }
}

fn num(args: &Map, key: &str, default: Option<f64>) -> Result<f64, ToolError> {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| ToolError(format!("{key} must be a number"))),
        None => default.ok_or_else(|| ToolError(format!("{key} must be a number"))),
        Some(_) => Err(ToolError(format!("{key} must be a number"))),
    }
}

fn req_str(args: &Map, key: &str) -> Result<String, ToolError> {
    match args.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(ToolError(format!("{key} must be a non-empty string"))),
    }
}

impl StreamQueue {
    #[allow(dead_code)]
    fn len(&self) -> usize {
        lock(&self.items).len()
    }
}
