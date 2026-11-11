//! The routes of `routes/firmware.py` and the firmware upload of `server.py`.

use std::sync::Arc;

use serde_json::{json, Value};

use super::ardupilot::vehicle_for_mav_type;
use super::catalog::detect_boards;
use super::flash::FlashService;
use super::util::py_strip;
use super::Firmware;
use crate::buffer::Sink;
use crate::core::Core;
use crate::events::{Binding, Topic};
use crate::module::Registry;
use crate::request::Request;
use crate::response::Response;
use crate::router::RouteOptions;

/// `MAX_FIRMWARE_BODY_BYTES` of `server.py`.
const MAX_FIRMWARE_BODY_BYTES: usize = 64 * 1024 * 1024;
/// `FIRMWARE_SSE_CAPACITY`: every step of a flash is kept, none coalesced.
const FIRMWARE_SSE_CAPACITY: usize = 16;

pub(super) fn register(reg: &mut Registry) {
    reg.topic(Topic::new("firmware", FIRMWARE_SSE_CAPACITY, bind_firmware));
    reg.get("/api/firmware/status", api_status);
    reg.get("/api/firmware/catalog", api_catalog);
    reg.post("/api/firmware/flash", api_flash);
    reg.post("/api/firmware/cancel", api_cancel);
    reg.get("/api/firmware/progress", api_progress);
    reg.post_with(
        "/api/firmware/upload",
        RouteOptions::raw(MAX_FIRMWARE_BODY_BYTES),
        api_upload,
    );
}

fn flash(core: &Core) -> Option<Arc<FlashService>> {
    core.service::<Firmware>().and_then(|f| f.flash())
}

fn bind_firmware(core: &Core, sink: Sink<Value>, _job: Option<&str>) -> Binding {
    let Some(flash) = flash(core) else {
        return Binding::none();
    };
    let id = flash.add_listener(move |e| sink.put_fifo(e.clone()));
    let st = flash.status();
    let initial = json!({"state": st["state"], "percent": st["progress"], "message": st["message"]});
    let svc = Arc::clone(&flash);
    Binding {
        unbind: Box::new(move || svc.remove_listener(id)),
        initial: Some(initial),
    }
}

fn api_progress(core: &Core, _req: &Request) -> Response {
    Response::sse(core.topic_stream(&[("firmware", "progress")], None, &[]))
}

fn api_status(core: &Core, _req: &Request) -> Response {
    match flash(core) {
        Some(f) => Response::json(f.status()),
        None => Response::json(json!({
            "state": "idle", "transport": "unknown", "device": "", "can_flash": false,
            "armed": false, "progress": 0, "message": "",
        })),
    }
}

fn api_catalog(core: &Core, req: &Request) -> Response {
    let unavailable = || {
        Response::json(json!({
            "releases": [], "cached": [], "error": "firmware catalogue unavailable", "dir": "",
        }))
    };
    let Some(firmware) = core.service::<Firmware>() else {
        return unavailable();
    };
    let (Some(flash), Some(catalog)) = (firmware.flash(), firmware.catalog()) else {
        return unavailable();
    };
    let refresh = matches!(
        req.query.get("refresh").unwrap_or("0").to_lowercase().as_str(),
        "1" | "true" | "yes"
    );
    let Ok(mut data) = catalog.catalog(refresh) else {
        return Response::json(json!({
            "releases": [], "cached": [], "error": "firmware catalogue failed", "dir": "",
            "detected": {"px4": null, "ardupilot": null},
            "suggested": {"stack": "", "vehicle": ""},
        }));
    };
    let releases = data["releases"].as_array().cloned().unwrap_or_default();
    data["detected"] = detected(&flash, &releases);
    data["suggested"] = suggested(&flash);
    Response::json(data)
}

/// Which board is plugged in, per stack, matched against the catalogue: a
/// suggestion that preselects, never decides.
fn detected(flash: &FlashService, releases: &[Value]) -> Value {
    let nothing = json!({"px4": null, "ardupilot": null});
    if releases.is_empty() {
        return nothing;
    }
    let mut identity = flash.host().board_identity();
    // A board with no firmware has no link to ask: the descriptor of the
    // bootloader port it shows up as is all there is.
    let waiting = flash.bootloader_ports();
    let has_device = identity["device"].as_str().is_some_and(|d| !d.is_empty());
    if !has_device && waiting.len() == 1 {
        identity["device"] = json!(waiting[0].device);
        identity["description"] = json!(waiting[0].description);
        identity["hwid"] = json!(waiting[0].hwid);
    }
    detect_boards(&identity, releases)
}

/// The connected aircraft's stack and ArduPilot vehicle, or blanks. The
/// vehicle is offered whichever stack is flying now.
fn suggested(flash: &FlashService) -> Value {
    let host = flash.host();
    let stack = host.stack();
    let stack = if stack == "px4" || stack == "ardupilot" {
        stack
    } else {
        String::new()
    };
    json!({"stack": stack, "vehicle": vehicle_for_mav_type(&json!(host.vehicle_type_id()))})
}

fn api_flash(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let empty = json!("");
    let (Some(Value::String(release)), Some(Value::String(board))) = (
        payload.get("release").or(Some(&empty)),
        payload.get("board").or(Some(&empty)),
    ) else {
        return Response::not_ok(400, "release and board must be strings");
    };
    let (release, board) = (py_strip(release), py_strip(board));
    if release.is_empty() || board.is_empty() {
        return Response::not_ok(400, "release and board are required");
    }
    let Some(flash) = flash(core) else {
        return Response::not_ok(503, "flash service unavailable");
    };
    if flash.start_release(release, board) {
        return Response::json(json!({"ok": true, "state": "downloading"}));
    }
    let err = flash.last_error();
    Response::not_ok(409, if err.is_empty() { "flash refused" } else { &err })
}

fn api_cancel(core: &Core, _req: &Request) -> Response {
    let Some(flash) = flash(core) else {
        return Response::not_ok(503, "flash service unavailable");
    };
    if flash.cancel() {
        Response::json(json!({"ok": true}))
    } else {
        Response::json(json!({"ok": false, "error": "no flash in progress"}))
    }
}

/// Receives a raw firmware binary and starts a flash.
fn api_upload(core: &Core, req: &Request) -> Response {
    let Some(flash) = flash(core) else {
        return Response::not_ok(503, "flash service unavailable");
    };
    if req.body.is_empty() {
        return Response::not_ok(400, "empty firmware body");
    }
    if flash.start(&req.body) {
        return Response::json(json!({"ok": true, "state": "flashing"}));
    }
    let err = flash.last_error();
    Response::not_ok(409, if err.is_empty() { "flash refused" } else { &err })
}
