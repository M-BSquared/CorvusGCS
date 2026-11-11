//! The first setup routes (`routes/first_setup.py`): what the wizard needs to
//! know about the connected controller, and choosing its airframe.
//!
//! The values an airframe write carries come from the tables of
//! [`super::first_setup`], never from the request, and a follow up write goes
//! only to a parameter the vehicle answers for.

use serde_json::{json, Value};

use super::first_setup;
use super::routes::{fetch, handles};
use crate::core::Core;
use crate::request::Request;
use crate::response::Response;
use crate::serial_ports::usb_identity;
use crate::vehicle::Handles;

fn context(h: &Handles) -> (String, &'static str, String) {
    let transport = h.bridge.transport();
    (
        h.bridge.stack_name().to_string(),
        first_setup::vehicle_for_mav_type(h.bridge.vehicle_type_id()),
        if transport.is_empty() {
            "unknown".into()
        } else {
            transport
        },
    )
}

/// What the wizard remembers a "do not ask again" against: the board's USB
/// identity, so a second fresh controller is offered the wizard again.
fn fingerprint(h: &Handles, stack: &str) -> String {
    let identity = h.bridge.board_identity();
    let hwid = identity["hwid"].as_str().unwrap_or("");
    let usb = usb_identity(hwid).unwrap_or_default();
    let id = |k: &str| identity[k].as_i64().unwrap_or(0);
    format!("{stack}|{}|{}|{usb}", id("vendor_id"), id("product_id"))
}

pub(super) fn api_first_setup(core: &Core, _req: &Request) -> Response {
    let Some(h) = handles(core) else {
        return Response::json(json!({"connected": false, "needs_setup": false, "prompt": false}));
    };
    let (stack, vehicle, transport) = context(&h);
    let values = fetch(&h, &first_setup::param_names(&stack), false);
    if values.is_empty() {
        return Response::json(json!({
            "connected": false, "stack": stack, "vehicle": vehicle, "transport": transport,
            "needs_setup": false, "prompt": false, "facts": {}, "reasons": [],
            "presets": first_setup::presets(&stack, vehicle), "preset_current": null,
            "fingerprint": "",
        }));
    }
    let mut payload = first_setup::assess(&stack, vehicle, &transport, &values);
    payload["connected"] = json!(true);
    payload["fingerprint"] = json!(fingerprint(&h, &stack));
    Response::json(payload)
}

pub(super) fn api_first_setup_airframe(core: &Core, req: &Request) -> Response {
    let refuse = |status: u16, error: &str| Response::json_status(json!({"ok": false, "error": error}), status);
    let preset = match req.payload().get("preset") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(i64::MAX),
        _ => return refuse(400, "preset must be an integer"),
    };
    let Some(h) = handles(core) else {
        return refuse(503, "not connected");
    };
    if h.store.snapshot().get("armed").and_then(Value::as_bool) == Some(true) {
        return refuse(409, "disarm before choosing an airframe");
    }
    let (stack, vehicle, _) = context(&h);
    let Some(writes) = first_setup::writes_for(&stack, vehicle, preset) else {
        return refuse(400, "unknown airframe for this vehicle");
    };
    let mut written: Vec<String> = Vec::new();
    for (index, (name, value)) in writes.iter().enumerate() {
        if index > 0 && !fetch(&h, std::slice::from_ref(name), false).contains_key(name) {
            continue;
        }
        if h.bridge.set_param(name, *value) {
            written.push(name.clone());
            continue;
        }
        let error = h.bridge.last_error();
        let error = if error.is_empty() {
            format!("could not write {name}")
        } else {
            error
        };
        if index == 0 {
            return refuse(
                if error.to_lowercase().contains("not connected") {
                    503
                } else {
                    409
                },
                &error,
            );
        }
        return Response::json(json!({"ok": true, "written": written, "warning": error}));
    }
    Response::json(json!({"ok": true, "written": written, "reboot": true}))
}
