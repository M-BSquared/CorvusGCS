//! The setup page routes of the Vehicle area (`GET /api/mounting`, `/api/safety`,
//! `/api/tuning`, `/api/battery` of `server.py`): one batched read of the
//! parameters the schema of the connected stack knows about, handed to that
//! schema to turn into a page. Only what the connected firmware answered for comes
//! back, so a parameter absent on an older firmware is one field fewer, and the
//! answer is always 200 with `connected` saying whether the vehicle answered.
//!
//! PX4's schemas are the default, and a stack Corvus does not recognise gets them
//! too: they are the primary target.

use serde_json::{json, Value};

use super::fields::Values;
use super::{
    ardupilot_battery, ardupilot_mounting, ardupilot_safety, ardupilot_tuning, battery, mounting, safety, tuning,
};
use crate::core::Core;
use crate::dialect::vehicle_class;
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::Response;
use crate::vehicle::{Handles, VehicleService};

/// How long a page's batched read waits (`fetch_params` of the Python bridge).
const FETCH_TIMEOUT_S: f64 = 4.0;

pub struct SetupSchemasModule;

impl SetupSchemasModule {
    pub fn new() -> SetupSchemasModule {
        SetupSchemasModule
    }
}

impl Default for SetupSchemasModule {
    fn default() -> Self {
        SetupSchemasModule::new()
    }
}

impl Module for SetupSchemasModule {
    fn name(&self) -> &'static str {
        "setup_schemas"
    }

    fn register(&self, reg: &mut Registry) {
        reg.get("/api/mounting", api_mounting);
        reg.get("/api/safety", api_safety);
        reg.get("/api/tuning", api_tuning);
        reg.get("/api/battery", api_battery);
    }
}

fn handles(core: &Core) -> Option<Handles> {
    core.service::<VehicleService>().map(|svc| svc.active())
}

fn is_ardupilot(h: &Handles) -> bool {
    h.store.snapshot().get("autopilot_stack").and_then(Value::as_str) == Some("ardupilot")
}

/// The firmware family the schemas key their per vehicle tables off.
fn vehicle(h: &Handles) -> &'static str {
    vehicle_class(h.bridge.vehicle_type_id()).as_str()
}

/// One page's batched read: `{name: value}` for whatever answered.
fn fetch(h: &Handles, names: &[String]) -> Values {
    h.bridge.fetch_params(names, FETCH_TIMEOUT_S).into_iter().collect()
}

/// Adds `connected` and, when nothing answered, the reason.
fn finish(mut payload: Value, h: &Handles, values: &Values) -> Response {
    payload["connected"] = json!(!values.is_empty());
    if values.is_empty() {
        let error = h.bridge.last_error();
        payload["error"] = json!(if error.is_empty() {
            "no parameters received".to_string()
        } else {
            error
        });
    }
    Response::json(payload)
}

fn api_mounting(core: &Core, _req: &Request) -> Response {
    let Some(h) = handles(core) else {
        let mut p = mounting::build(&Values::new());
        p["connected"] = json!(false);
        return Response::json(p);
    };
    if is_ardupilot(&h) {
        let values = fetch(&h, &ardupilot_mounting::param_names());
        return finish(ardupilot_mounting::build(&values), &h, &values);
    }
    let values = fetch(&h, &mounting::param_names());
    finish(mounting::build(&values), &h, &values)
}

fn api_safety(core: &Core, req: &Request) -> Response {
    let asked: Vec<String> = req
        .query
        .all("extra")
        .iter()
        .flat_map(|v| v.split(',').map(str::to_string))
        .collect();
    let Some(h) = handles(core) else {
        return Response::json(json!({"connected": false, "sections": [], "extra": [], "received": 0}));
    };
    if is_ardupilot(&h) {
        let extra = ardupilot_safety::normalise_extra(&asked);
        let mut names = ardupilot_safety::param_names();
        names.extend(extra.iter().cloned());
        let values = fetch(&h, &names);
        return finish(ardupilot_safety::build(&values, &extra, vehicle(&h)), &h, &values);
    }
    let extra = safety::normalise_extra(&asked);
    let mut names = safety::param_names();
    names.extend(extra.iter().cloned());
    let values = fetch(&h, &names);
    finish(safety::build(&values, &extra), &h, &values)
}

fn api_tuning(core: &Core, _req: &Request) -> Response {
    let Some(h) = handles(core) else {
        return Response::json(json!({"connected": false, "groups": [], "received": 0}));
    };
    if is_ardupilot(&h) {
        let values = fetch(&h, &ardupilot_tuning::param_names());
        return finish(ardupilot_tuning::build(&values, vehicle(&h)), &h, &values);
    }
    let values = fetch(&h, &tuning::param_names());
    finish(tuning::build(&values), &h, &values)
}

fn api_battery(core: &Core, _req: &Request) -> Response {
    let stored: Option<Value> = core.settings().get().block("battery").map(|m| Value::Object(m.clone()));
    let settings = crate::battery::settings(stored.as_ref());
    // Both, because they are different questions: `settings` is what the estimator
    // actually runs on, `configured` is what the operator typed, where a 0 means
    // "work it out" and has to keep meaning that when the form is drawn again.
    let configured = stored.clone().unwrap_or_else(|| json!({}));
    let wrap = |mut payload: Value| {
        payload["settings"] = settings.to_json();
        payload["configured"] = configured.clone();
        payload["chemistries"] = Value::Array(crate::battery::chemistry_options());
        payload
    };
    let Some(h) = handles(core) else {
        let mut p = wrap(
            json!({"connected": false, "sections": [], "received": 0, "pack": battery::pack_summary(&Values::new())}),
        );
        p["connected"] = json!(false);
        return Response::json(p);
    };
    if is_ardupilot(&h) {
        let values = fetch(&h, &ardupilot_battery::param_names());
        return finish(wrap(ardupilot_battery::build(&values, vehicle(&h))), &h, &values);
    }
    let values = fetch(&h, &battery::param_names());
    finish(wrap(battery::build(&values)), &h, &values)
}
