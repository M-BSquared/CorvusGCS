//! `GET /api/remoteid` and `GET /api/remoteid/status`: the Remote ID page.
//!
//! The page carries three separate things: the identity the station broadcasts
//! (configured on the ground, rendered whether or not a vehicle answers), the
//! filing findings and option lists, and the vehicle's own parameters. The status
//! poll touches no MAVLink at all, so a poll on a reconnecting link cannot pile
//! up behind a parameter read.

use serde_json::{json, Value};

use super::fields::Values;
use super::remote_id_page as page;
use super::routes::{fetch, finish, fresh, handles, is_ardupilot};
use crate::core::Core;
use crate::protocol::remote_id::settings;
use crate::request::Request;
use crate::response::Response;

fn stored(core: &Core) -> Option<Value> {
    core.settings()
        .get()
        .block("remote_id")
        .map(|m| Value::Object(m.clone()))
}

fn idle_status(enabled: bool) -> Value {
    json!({"enabled": enabled, "supported": false, "broadcasting": false,
           "last_sent_age": null, "error": "", "arm_status": null})
}

pub(super) fn api_remote_id(core: &Core, req: &Request) -> Response {
    let raw = stored(core);
    let identity = settings(raw.as_ref());
    let mut base = json!({
        "connected": false, "sections": [], "received": 0,
        "identity": identity.to_json(),
        "configured": raw.clone().unwrap_or_else(|| json!({})),
        "schema": page::schema(),
        "findings": page::findings(&identity),
        "status": idle_status(identity.enabled),
        "suggested_ua_type": 0,
    });
    let Some(h) = handles(core) else {
        return Response::json(base);
    };
    base["status"] = h.bridge.remote_id_status();
    base["suggested_ua_type"] = json!(page::ua_type_for_vehicle(h.bridge.vehicle_type_id()));
    let names = if is_ardupilot(&h) {
        page::ardupilot_param_names()
    } else {
        page::px4_param_names()
    };
    let values: Values = fetch(&h, &names, fresh(req));
    let built = if is_ardupilot(&h) {
        page::ardupilot_build(&values)
    } else {
        page::px4_build(&values)
    };
    if let (Some(out), Some(extra)) = (base.as_object_mut(), built.as_object()) {
        out.extend(extra.clone());
    }
    finish(base, &h, &values)
}

pub(super) fn api_remote_id_status(core: &Core, _req: &Request) -> Response {
    let raw = stored(core);
    let identity = settings(raw.as_ref());
    let status = match handles(core) {
        Some(h) => h.bridge.remote_id_status(),
        None => idle_status(identity.enabled),
    };
    Response::json(json!({
        "identity": identity.to_json(), "findings": page::findings(&identity), "status": status,
    }))
}
