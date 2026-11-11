//! The five SiK radio endpoints of `server.py`. They are the only ones that
//! configure something other than the autopilot. All of the acting ones are
//! POST, the read included: loading takes the serial port away from the
//! MAVLink bridge and puts a radio into command mode, a side effect on the
//! aircraft's telemetry link that has no business behind a method a browser
//! may retry or prefetch.

use serde_json::{json, Map, Value};

use super::config;
use super::service::SikFailure;
use crate::core::Core;
use crate::module::Registry;
use crate::peripherals::Peripherals;
use crate::request::Request;
use crate::response::Response;

pub fn register(reg: &mut Registry) {
    reg.get("/api/sik/status", api_status);
    reg.post("/api/sik/load", api_load);
    reg.post("/api/sik/save", api_save);
    reg.post("/api/sik/reset", api_reset);
}

fn api_status(core: &Core, _req: &Request) -> Response {
    let Some(sik) = core.service::<Peripherals>().and_then(|p| p.sik()) else {
        return Response::json(json!({
            "ports": [], "link_device": "", "link_baud": config::DEFAULT_BAUD,
            "transport": "unknown", "armed": false, "busy": false,
            "can_configure": false,
            "blocked_reason": "radio configuration is unavailable",
            "default_baud": config::DEFAULT_BAUD,
            "bauds": config::baud_candidates(),
            "schema": config::schema(),
        }));
    };
    let mut status = sik.status();
    status["schema"] = config::schema();
    Response::json(status)
}

/// `device` and optional `baud` out of a request body, or the refusal.
fn target(payload: &Map<String, Value>) -> Result<(String, Option<i64>), Response> {
    let device = match payload.get("device") {
        Some(Value::String(s)) if !crate::peripherals::util::py_strip(s).is_empty() => {
            crate::peripherals::util::py_strip(s).to_string()
        }
        _ => return Err(Response::not_ok(400, "device is required")),
    };
    match payload.get("baud") {
        None | Some(Value::Null) => Ok((device, None)),
        Some(Value::Number(n)) if n.as_i64().is_some_and(|b| b > 0) => Ok((device, n.as_i64())),
        Some(_) => Err(Response::not_ok(400, "baud must be a positive integer")),
    }
}

fn answer(result: Option<Result<Map<String, Value>, SikFailure>>) -> Response {
    match result {
        None => Response::not_ok(503, "radio configuration unavailable"),
        Some(Err(SikFailure::Config(e))) => Response::not_ok(400, &e),
        Some(Err(SikFailure::Sik(e))) => Response::not_ok(409, &e),
        Some(Err(SikFailure::Other(e))) => Response::not_ok(500, &format!("radio session failed: {e}")),
        Some(Ok(mut r)) => {
            r.insert("ok".into(), json!(true));
            Response::json(Value::Object(r))
        }
    }
}

fn api_load(core: &Core, req: &Request) -> Response {
    let (device, baud) = match target(req.payload()) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let include_remote = match req.payload().get("remote") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Response::not_ok(400, "remote must be boolean"),
    };
    let sik = core.service::<Peripherals>().and_then(|p| p.sik());
    answer(sik.map(|s| s.load(&device, baud, include_remote)))
}

fn api_save(core: &Core, req: &Request) -> Response {
    let (device, baud) = match target(req.payload()) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let mut parts: Vec<Option<Map<String, Value>>> = Vec::new();
    for name in ["local", "remote"] {
        match req.payload().get(name) {
            None | Some(Value::Null) => parts.push(None),
            Some(Value::Object(o)) => parts.push(Some(o.clone())),
            Some(_) => return Response::not_ok(400, &format!("{name} must be an object")),
        }
    }
    let (local, remote) = (parts[0].take(), parts[1].take());
    if local.as_ref().is_none_or(Map::is_empty) && remote.as_ref().is_none_or(Map::is_empty) {
        return Response::not_ok(400, "nothing to write");
    }
    let sik = core.service::<Peripherals>().and_then(|p| p.sik());
    answer(sik.map(|s| s.save(&device, baud, local.as_ref(), remote.as_ref())))
}

fn api_reset(core: &Core, req: &Request) -> Response {
    let (device, baud) = match target(req.payload()) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let which = match req.payload().get("target") {
        None => "local",
        Some(Value::String(s)) if s == "local" || s == "remote" => s.as_str(),
        Some(_) => return Response::not_ok(400, "target must be 'local' or 'remote'"),
    };
    let sik = core.service::<Peripherals>().and_then(|p| p.sik());
    answer(sik.map(|s| s.reset(&device, baud, which)))
}
