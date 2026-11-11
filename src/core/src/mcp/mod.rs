//! The AI client area of the core: the MCP server behind `POST /mcp` and the
//! routes its settings card and the Corvus window use (`mcp.py`,
//! `routes/mcp.py`).
//!
//! The protocol and every decision about what an agent may do are in
//! [`engine`]; [`http::mcp`](crate::http::mcp) only checks that a request may
//! reach it at all (this computer, switched on, the right token). A tool runs an
//! existing endpoint through `Core::internal_call`, never the link, so a tool
//! cannot do what a button could not; an endpoint that changes the vehicle is
//! not reachable over MCP until a tool and its permission are added in
//! [`tools`] on purpose.
//!
//! [`McpModule`] is the one line an area adds to the core.

pub mod engine;
pub mod settings;
pub mod tools;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::core::Core;
use crate::http::mcp::{McpEndpoint, McpHandle};
use crate::http::HttpInfo;
use crate::json::Map;
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::{Response, SseEvent, SseNext, SseStream};
use crate::Binding;
use crate::Topic;

pub use engine::{McpService, Timing, ToolError, UpdateError};

/// The `mcp` event topic holds few items and none may be dropped: status,
/// confirmations, plugin calls and log lines (`MCP_SSE_CAPACITY`).
const TOPIC_CAPACITY: usize = 64;

/// The core the engine calls its endpoints through, set while the core runs.
/// A cell and not the core itself, so the engine does not keep the core alive.
#[derive(Default, Clone)]
struct CoreCell(Arc<Mutex<Option<Core>>>);

impl CoreCell {
    fn get(&self) -> Option<Core> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, core: Option<Core>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = core;
    }
}

/// The AI client module.
#[derive(Default)]
pub struct McpModule {
    cell: CoreCell,
    timing: Option<Timing>,
}

impl McpModule {
    pub fn new() -> McpModule {
        McpModule::default()
    }

    /// With the two waits shortened, for a test that must not sit for 45 s.
    pub fn with_timing(timing: Timing) -> McpModule {
        McpModule {
            cell: CoreCell::default(),
            timing: Some(timing),
        }
    }
}

struct Endpoint(Arc<McpService>);

struct McpStream {
    svc: Arc<McpService>,
    session: String,
    queue: Arc<engine::StreamQueue>,
}

impl SseStream for McpStream {
    fn next(&mut self, wait: Duration) -> SseNext {
        match self.queue.next(wait) {
            Some(Some(note)) => SseNext::Events(vec![SseEvent::new(
                "message",
                crate::plugins::update_check::dump_py(&note).into_bytes(),
            )]),
            Some(None) => SseNext::End,
            None => SseNext::Idle,
        }
    }

    fn close(&mut self) {
        self.svc.close_stream(Some(&self.session), &self.queue);
    }

    fn ping(&self) -> &'static [u8] {
        b": ping\n\n"
    }
}

impl McpEndpoint for Endpoint {
    fn enabled(&self) -> bool {
        self.0.enabled()
    }

    fn authorized(&self, authorization: Option<&str>) -> bool {
        self.0.authorized(authorization)
    }

    fn handle_post(&self, message: Value, session: Option<&str>) -> (u16, Option<Value>, Vec<(String, String)>) {
        self.0.handle_post(&message, session)
    }

    fn open_stream(&self, session: Option<&str>) -> Option<Box<dyn SseStream>> {
        let queue = self.0.open_stream(session)?;
        Some(Box::new(McpStream {
            svc: self.0.clone(),
            session: session.unwrap_or("").to_string(),
            queue,
        }))
    }

    fn end_session(&self, session: Option<&str>) -> bool {
        self.0.end_session(session)
    }
}

impl Module for McpModule {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn register(&self, reg: &mut Registry) {
        reg.config_block("mcp", settings::coerce_settings);
        reg.topic(Topic::new("mcp", TOPIC_CAPACITY, |core, sink, _job| {
            let Some(svc) = core.service::<McpService>() else {
                return Binding::none();
            };
            let id = svc.add_listener(Arc::new(move |event: &Value| sink.put_fifo(event.clone())));
            let mut initial = svc.status();
            initial.insert("type".into(), "status".into());
            let svc2 = svc.clone();
            Binding {
                unbind: Box::new(move || svc2.remove_listener(id)),
                initial: Some(Value::Object(initial)),
            }
        }));
        reg.get("/api/mcp", api_mcp);
        reg.post("/api/mcp/settings", api_settings);
        reg.post("/api/mcp/token", api_token);
        reg.get("/api/mcp/status", api_status);
        reg.post("/api/mcp/stop", api_stop);
        reg.post("/api/mcp/confirm", api_confirm);
        reg.post("/api/mcp/window", api_window);
        reg.post("/api/mcp/result", api_result);
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        self.cell.set(Some(core.clone()));
        let (dispatch_cell, load_core, save_core) = (self.cell.clone(), core.settings(), core.settings());
        let dispatch: engine::Dispatch = Arc::new(move |method, path, body| match dispatch_cell.get() {
            Some(core) => core.internal_call(method, path, body),
            None => (503, crate::json::not_ok("Corvus GCS is closing.")),
        });
        let load: engine::LoadSettings = Arc::new(move || load_core.get().get("mcp").cloned());
        // The service saves with `update_quiet`: its own switches are not news
        // to the config listeners, and the status event says what changed.
        let save: engine::SaveSettings =
            Arc::new(move |block: Option<Map>| save_core.update_quiet(|cfg| cfg.set("mcp", block.map(Value::Object))));
        let clock: engine::Clock = Arc::new(crate::clock::monotonic);
        let svc = McpService::new(
            dispatch,
            load,
            save,
            core.paths().mcp_token(),
            clock,
            self.timing.unwrap_or_default(),
        );
        core.insert_service(svc.clone());
        core.insert_service(Arc::new(McpHandle(Arc::new(Endpoint(svc)))));
        Ok(())
    }

    fn stop(&self, core: &Core) {
        if let Some(svc) = core.service::<McpService>() {
            svc.shutdown();
        }
        self.cell.set(None);
    }
}

// ---------------------------------------------------------------- routes

fn service(core: &Core) -> Result<Arc<McpService>, Response> {
    core.service::<McpService>()
        .ok_or_else(|| Response::not_ok(503, "MCP is not available"))
}

macro_rules! svc {
    ($core:expr) => {
        match service($core) {
            Ok(s) => s,
            Err(r) => return r,
        }
    };
}

fn local_only(req: &Request) -> Option<Response> {
    (!req.is_loopback())
        .then(|| Response::not_ok(403, "The MCP server can only be set up on the computer Corvus runs on."))
}

fn url(core: &Core) -> String {
    let port = core
        .service::<HttpInfo>()
        .and_then(|i| i.addr())
        .map_or(8000, |a| a.port());
    format!("http://127.0.0.1:{port}/mcp")
}

/// Everything the MCP card shows: settings, catalog, plugins, status, how to
/// connect. The token goes to this computer only: the URL it unlocks is
/// loopback, so nobody elsewhere has a use for it.
fn api_mcp(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let local = req.is_loopback();
    Response::json(json!({
        "settings": svc.settings(),
        "catalog": engine::tool_catalog(),
        "plugins": svc.plugin_summary(),
        "status": svc.status(),
        "url": url(core),
        "token": if local { svc.token() } else { String::new() },
        "local": local,
        "control_max_s": settings::CONTROL_MAX_S,
        "confirm_timeout_s": engine::CONFIRM_TIMEOUT_S,
    }))
}

fn api_settings(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    if let Some(refusal) = local_only(req) {
        return refusal;
    }
    match svc.update_settings(req.payload()) {
        Ok(settings) => Response::json(json!({"ok": true, "settings": settings, "status": svc.status()})),
        Err(UpdateError::Invalid(why)) => Response::not_ok(400, &why),
        Err(UpdateError::Save(e)) => {
            Response::not_ok(500, &format!("not saved: {}", crate::plugins::config::io_text(&e)))
        }
    }
}

fn api_token(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    if let Some(refusal) = local_only(req) {
        return refusal;
    }
    Response::json(json!({"ok": true, "token": svc.regenerate_token()}))
}

fn api_status(core: &Core, _req: &Request) -> Response {
    Response::json(Value::Object(svc!(core).status()))
}

/// The flight indicator's Stop: take every permission that changes anything away.
fn api_stop(core: &Core, _req: &Request) -> Response {
    let svc = svc!(core);
    match svc.stop() {
        Ok(settings) => Response::json(json!({"ok": true, "settings": settings, "status": svc.status()})),
        Err(e) => Response::not_ok(500, &format!("not saved: {}", crate::plugins::config::io_text(&e))),
    }
}

fn api_confirm(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let payload = req.payload();
    let Some(allow) = payload.get("allow").filter(|a| a.is_boolean()) else {
        return Response::not_ok(400, "allow must be true or false");
    };
    if !svc.answer(payload.get("id").unwrap_or(&Value::Null), allow) {
        return Response::not_ok(404, "nothing is waiting under that id");
    }
    Response::json(json!({"ok": true}))
}

/// A Corvus window is open, and these are the plugin tools it can run.
fn api_window(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let payload = req.payload();
    match svc.window_heartbeat(
        payload.get("id").unwrap_or(&Value::Null),
        payload.get("tools").unwrap_or(&Value::Null),
    ) {
        Ok(Value::Object(mut result)) => {
            let mut out = Map::new();
            out.insert("ok".into(), true.into());
            out.append(&mut result);
            Response::json(Value::Object(out))
        }
        Ok(_) => Response::json(json!({"ok": true})),
        Err(why) => Response::not_ok(400, &why),
    }
}

/// A window's answer to a plugin tool call.
fn api_result(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    let null = Value::Null;
    if !svc.call_result(
        p.get("id").unwrap_or(&null),
        p.get("ok").unwrap_or(&null),
        p.get("result").unwrap_or(&null),
        p.get("error").unwrap_or(&null),
    ) {
        return Response::not_ok(404, "no call is waiting under that id");
    }
    Response::json(json!({"ok": true}))
}
