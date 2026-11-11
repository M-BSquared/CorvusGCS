//! The setup pages that drive the actuators and the sensors (`routes/control.py`
//! and the calibration, autotune and stream routes of `server.py`): Motors and
//! Radio Control, their writes, the calibrations, the autotune and the high rate
//! streams of the tuning and vibration pages.
//!
//! Every write is named by a schema of the connected stack, never by the
//! request, and every one goes through `set_param`, which the vehicle itself has
//! to answer for: a parameter the firmware does not have is refused, not sent.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use super::fields::Values;
use super::routes::{fetch, fresh, handles, is_ardupilot};
use super::{ardupilot_motors, ardupilot_mounting, ardupilot_rc, motors, mounting, rc};
use crate::bridge::Bridge;
use crate::core::Core;
use crate::request::Request;
use crate::response::Response;
use crate::vehicle::{Handles, VehicleService};

const CALIB_SENSORS: [&str; 8] = [
    "gyro",
    "compass",
    "baro",
    "accel",
    "level",
    "accel_quick",
    "airspeed",
    "motor",
];
const ACCELCAL_POSITIONS: [&str; 6] = ["level", "left", "right", "nosedown", "noseup", "back"];

fn status_of(error: &str) -> u16 {
    if error.contains("not connected") {
        503
    } else {
        409
    }
}

fn status_of_lowercase(error: &str) -> u16 {
    let lower = error.to_lowercase();
    if lower.contains("disconnected") || lower.contains("not connected") {
        503
    } else {
        409
    }
}

fn not_connected() -> Response {
    Response::not_ok(503, "not connected")
}

fn active(core: &Core) -> Option<Handles> {
    core.service::<VehicleService>().map(|s| s.active())
}

/// The bridge's last error, or `fallback`.
fn reason(bridge: &Bridge, fallback: &str) -> String {
    let error = bridge.last_error();
    if error.is_empty() {
        fallback.to_string()
    } else {
        error
    }
}

fn armed(h: &Handles) -> bool {
    h.store.snapshot().get("armed").and_then(Value::as_bool) == Some(true)
}

fn with_error(mut payload: Value, h: &Handles, values: &Values) -> Value {
    payload["connected"] = json!(!values.is_empty());
    if values.is_empty() {
        payload["error"] = json!(reason(&h.bridge, "no parameters received"));
    }
    payload
}

pub(super) fn api_motors(core: &Core, req: &Request) -> Response {
    let Some(h) = handles(core) else {
        let mut p = motors::build(&Values::new());
        p["sensors"] = json!(mounting::positions(&Values::new()));
        p["connected"] = json!(false);
        return Response::json(p);
    };
    let fresh = fresh(req);
    let ardupilot = is_ardupilot(&h);
    let mut names = if ardupilot {
        ardupilot_motors::param_names()
    } else {
        motors::param_names()
    };
    names.extend(if ardupilot {
        ardupilot_mounting::position_param_names()
    } else {
        mounting::position_param_names()
    });
    let mut values = fetch(&h, &names, fresh);
    if !values.is_empty() && !ardupilot {
        let more = motors::output_param_names(&values);
        if !more.is_empty() {
            values.extend(fetch(&h, &more, fresh));
        }
    }
    let (mut payload, sensors) = if ardupilot {
        (ardupilot_motors::build(&values), ardupilot_mounting::positions(&values))
    } else {
        (motors::build(&values), mounting::positions(&values))
    };
    payload["sensors"] = json!(sensors);
    payload["position_hint"] = json!(mounting::position_hint());
    Response::json(with_error(payload, &h, &values))
}

pub(super) fn api_rc(core: &Core, req: &Request) -> Response {
    let empty = json!({"connected": false, "sections": [], "assignments": {},
                       "channel_limit": rc::MAX_CHANNELS, "received": 0});
    let Some(h) = handles(core) else {
        return Response::json(empty);
    };
    let mut channels = rc::MAX_CHANNELS;
    if let Some(n) = h.store.snapshot().get("rc_channel_count").and_then(Value::as_i64) {
        if 0 < n && n <= rc::MAX_CHANNELS {
            channels = n;
        }
    }
    let ardupilot = is_ardupilot(&h);
    let names = if ardupilot {
        ardupilot_rc::param_names(rc::MAX_CHANNELS)
    } else {
        rc::param_names(rc::MAX_CHANNELS)
    };
    let values = fetch(&h, &names, fresh(req));
    let payload = if ardupilot {
        ardupilot_rc::build(&values, channels, h.bridge.vehicle_type_id())
    } else {
        rc::build(&values, channels)
    };
    Response::json(with_error(payload, &h, &values))
}

fn write_failure(applied: &[Value], bridge: &Bridge, failed: &str) -> Response {
    let error = reason(bridge, "parameter write failed");
    let status = status_of(&error);
    Response::json_status(
        json!({"ok": false, "error": error, "applied": applied, "failed": failed}),
        status,
    )
}

pub(super) fn api_motors_assign(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let Some(h) = handles(core) else {
        return not_connected();
    };
    let ardupilot = is_ardupilot(&h);
    let max = if ardupilot {
        ardupilot_motors::MAX_MOTOR_FUNCTIONS
    } else {
        motors::MAX_MOTOR_FUNCTIONS
    };
    let motor = match payload.get("motor") {
        Some(Value::Number(n)) if n.is_i64() && (1..=max).contains(&n.as_i64().unwrap_or(0)) => n.as_i64().unwrap_or(0),
        _ => {
            return Response::json_status(
                json!({"ok": false, "error": format!("motor must be between 1 and {max}")}),
                400,
            )
        }
    };
    let unassign = matches!(payload.get("output"), Some(Value::Null));
    let mut target: Option<(String, String)> = None;
    if !unassign {
        let (bank, pin) = (payload.get("bank"), payload.get("pin"));
        let (Some(Value::String(bank)), Some(Value::Number(pin))) = (bank, pin) else {
            return Response::json_status(json!({"ok": false, "error": "bank and pin are required"}), 400);
        };
        let Some(pin) = pin.as_i64() else {
            return Response::json_status(json!({"ok": false, "error": "bank and pin are required"}), 400);
        };
        let param = if ardupilot {
            ardupilot_motors::function_param(bank, pin)
        } else {
            motors::function_param(bank, pin)
        };
        let Some(param) = param else {
            return Response::json_status(
                json!({"ok": false, "error": format!("unknown output {bank} {pin}")}),
                400,
            );
        };
        target = Some((param, format!("{bank} {pin}")));
    }
    let names: Vec<String> = if ardupilot {
        ardupilot_motors::param_names()
    } else {
        motors::param_names()
    }
    .into_iter()
    .filter(|n| n.contains("_FUNC"))
    .collect();
    let values = fetch(&h, &names, false);
    if values.is_empty() {
        let error = reason(&h.bridge, "no output parameters received");
        return Response::json_status(json!({"ok": false, "error": error}), 503);
    }
    if let Some((param, label)) = &target {
        if !values.contains_key(param) {
            return Response::json_status(
                json!({"ok": false, "error": format!("this board has no output {label}")}),
                400,
            );
        }
    }
    let entries = if ardupilot {
        ardupilot_motors::outputs(&values)
    } else {
        motors::outputs(&values)
    };
    let source = entries.iter().find(|e| e.motor == Some(motor));
    if let (Some((param, _)), Some(src)) = (&target, source) {
        if &src.param == param {
            return Response::json(json!({"ok": true, "writes": [], "note": "already assigned"}));
        }
    }
    let function_value = |m: i64| {
        if ardupilot {
            ardupilot_motors::motor_function_value(m)
        } else {
            motors::motor_function_value(m)
        }
    };
    let mut writes: Vec<(String, f64)> = Vec::new();
    match &target {
        None => {
            let Some(src) = source else {
                return Response::json(json!({"ok": true, "writes": [], "note": "already unassigned"}));
            };
            writes.push((src.param.clone(), 0.0));
        }
        Some((param, label)) => {
            let Some(occupant) = entries.iter().find(|e| &e.param == param) else {
                return Response::json_status(
                    json!({"ok": false, "error": format!("this board has no output {label}")}),
                    400,
                );
            };
            let displaced = occupant.motor;
            if displaced.is_none() && occupant.value.round() as i64 != 0 {
                return Response::json_status(
                    json!({"ok": false, "error": format!(
                        "{label} drives {}. Free it in Parameters before assigning a motor to it",
                        occupant.function)}),
                    409,
                );
            }
            if let (Some(d), None) = (displaced, source) {
                return Response::json_status(
                    json!({"ok": false, "error": format!(
                        "{label} already drives Motor {d}, and Motor {motor} has no output to swap it onto")}),
                    409,
                );
            }
            if let Some(src) = source {
                let vacated = match displaced {
                    None => 0.0,
                    Some(d) => function_value(d).unwrap_or(0.0),
                };
                writes.push((src.param.clone(), vacated));
            }
            let Some(claimed) = function_value(motor) else {
                return Response::json_status(
                    json!({"ok": false, "error": format!("this autopilot has no output function for Motor {motor}")}),
                    400,
                );
            };
            writes.push((param.clone(), claimed));
        }
    }
    let mut applied: Vec<Value> = Vec::new();
    for (name, value) in writes {
        if !h.bridge.set_param(&name, value) {
            return write_failure(&applied, &h.bridge, &name);
        }
        applied.push(json!({"param": name, "value": value}));
    }
    Response::json(json!({"ok": true, "writes": applied}))
}

pub(super) fn api_rc_calibrate(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let Some(h) = handles(core) else {
        return not_connected();
    };
    if armed(&h) {
        return Response::json_status(
            json!({"ok": false, "error": "cannot calibrate the radio while armed"}),
            409,
        );
    }
    let count = payload.get("count");
    if let Some(c) = count {
        if !c.is_null() && !(c.is_i64() || c.is_u64()) {
            return Response::json_status(json!({"ok": false, "error": "count must be an integer"}), 400);
        }
    }
    let count = count.filter(|c| !c.is_null());
    let ardupilot = is_ardupilot(&h);
    let names = if ardupilot {
        ardupilot_rc::param_names(rc::MAX_CHANNELS)
    } else {
        rc::param_names(rc::MAX_CHANNELS)
    };
    let known: HashSet<String> = fetch(&h, &names, false).into_keys().collect();
    if known.is_empty() {
        let error = reason(&h.bridge, "no parameters received");
        return Response::json_status(json!({"ok": false, "error": error}), 503);
    }
    let null = Value::Null;
    let channels = payload.get("channels").unwrap_or(&null);
    let mapping = payload.get("mapping").filter(|m| !m.is_null());
    let writes = if ardupilot {
        ardupilot_rc::calibration_writes(channels, count, Some(&known), mapping)
    } else {
        rc::calibration_writes(channels, count, Some(&known), mapping)
    };
    let writes = match writes {
        Ok(w) => w,
        Err(e) => return Response::json_status(json!({"ok": false, "error": e.0}), 400),
    };
    let mut applied: Vec<Value> = Vec::new();
    for write in writes {
        let name = write["name"].as_str().unwrap_or("").to_string();
        let value = write["value"].as_f64().unwrap_or(0.0);
        if !h.bridge.set_param(&name, value) {
            return write_failure(&applied, &h.bridge, &name);
        }
        applied.push(write);
    }
    Response::json(json!({"ok": true, "writes": applied}))
}

pub(super) fn api_calibrate(core: &Core, req: &Request) -> Response {
    let sensor = match req.payload().get("type") {
        Some(Value::String(s)) if CALIB_SENSORS.contains(&s.to_lowercase().as_str()) => s.to_lowercase(),
        _ => return Response::not_ok(400, "unknown calibration type"),
    };
    let Some(h) = active(core) else {
        return not_connected();
    };
    if h.bridge.calibrate(&sensor) {
        return Response::json(json!({"ok": true}));
    }
    let error = reason(&h.bridge, "calibration failed");
    Response::not_ok(status_of(&error), &error)
}

pub(super) fn api_calibrate_position(core: &Core, req: &Request) -> Response {
    let position = match req.payload().get("position") {
        Some(Value::String(s)) => s.trim().to_lowercase(),
        _ => String::new(),
    };
    if !ACCELCAL_POSITIONS.contains(&position.as_str()) {
        return Response::not_ok(400, "unknown position");
    }
    let Some(h) = active(core) else {
        return not_connected();
    };
    if h.bridge.accel_calibration_position(&position) {
        return Response::json(json!({"ok": true}));
    }
    let error = reason(&h.bridge, "position failed");
    Response::not_ok(status_of(&error), &error)
}

pub(super) fn api_calibrate_cancel(core: &Core, _req: &Request) -> Response {
    let Some(h) = active(core) else {
        return not_connected();
    };
    if h.bridge.cancel_calibration() {
        return Response::json(json!({"ok": true}));
    }
    let error = reason(&h.bridge, "cancel failed");
    Response::not_ok(status_of(&error), &error)
}

pub(super) fn api_autotune(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let axis = match payload.get("axis") {
        Some(Value::String(s)) => s.trim().to_lowercase(),
        _ => String::new(),
    };
    if axis != "all" {
        return Response::not_ok(400, "full autotune only; use axis 'all'");
    }
    let enabled = match payload.get("enabled") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Response::not_ok(400, "enabled must be boolean"),
    };
    let Some(h) = active(core) else {
        return not_connected();
    };
    if h.bridge.autotune(&axis, enabled) {
        return Response::json(json!({"ok": true}));
    }
    let error = reason(&h.bridge, "autotune failed");
    Response::not_ok(status_of_lowercase(&error), &error)
}

/// The shared body of the two read only streams: `enabled`, a rate from 1 to 50.
fn rate_stream(
    core: &Core,
    req: &Request,
    default_rate: f64,
    failure: &str,
    call: fn(&Bridge, bool, i64) -> bool,
) -> Response {
    let payload: &Map<String, Value> = req.payload();
    let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) else {
        return Response::not_ok(400, "enabled must be boolean");
    };
    let raw = match payload.get("rate_hz") {
        None => default_rate,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(_) => return Response::not_ok(400, "rate_hz must be a number"),
    };
    let rate = raw.trunc();
    if raw != rate || !(1.0..=50.0).contains(&rate) {
        return Response::not_ok(400, "rate_hz must be between 1 and 50 Hz");
    }
    let rate_hz = rate as i64;
    let Some(h) = active(core) else {
        return not_connected();
    };
    if call(&h.bridge, enabled, rate_hz) {
        return Response::json(json!({"ok": true, "enabled": enabled, "rate_hz": rate_hz}));
    }
    let error = reason(&h.bridge, failure);
    Response::not_ok(status_of(&error), &error)
}

pub(super) fn api_vibration_stream(core: &Core, req: &Request) -> Response {
    rate_stream(
        core,
        req,
        10.0,
        "vibration stream request failed",
        Bridge::set_vibration_stream,
    )
}

pub(super) fn api_tuning_stream(core: &Core, req: &Request) -> Response {
    rate_stream(
        core,
        req,
        20.0,
        "tuning stream request failed",
        Bridge::set_tuning_stream,
    )
}

pub(super) fn api_warnings_clear(core: &Core, _req: &Request) -> Response {
    match active(core) {
        Some(h) => h.store.clear_warnings(),
        None => core.store().clear_warnings(),
    }
    Response::json(json!({"ok": true}))
}
