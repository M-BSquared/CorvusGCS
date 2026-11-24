//! TAK: Corvus on a Team Awareness Kit network (ATAK, CivTAK, WinTAK, iTAK, a
//! TAK Server or FreeTAKServer).
//!
//! The module speaks Cursor on Target ([`cot`]) over UDP multicast, UDP, TCP and
//! TLS ([`service`]): it listens for the contacts and markers of the others,
//! keeps them for the map, and, when the operator allows it, shares the
//! aircraft as a friendly unmanned aircraft and sends markers and chat.
//!
//! It lives in the core and not in the plugin so that it has the lifecycle
//! every socket here has (started in `start`, joined in `stop`), runs without a
//! window, and so that an AI client reaches it the way it reaches everything
//! else: through routes, with permissions of their own (`tak_read`, `tak_send`
//! and `tak_setup` in [`crate::mcp::settings`]). The plugin `tak` draws it.
//!
//! Off by default: no socket is opened until a connection is switched on, and
//! the aircraft's position is shared only when `share_position` is set.

pub mod config;
pub mod cot;
pub mod service;
mod tls;
pub mod xml;

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::core::Core;
use crate::events::{Binding, Topic};
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::Response;

pub use config::Settings;
pub use service::{TakService, TrackQuery};

/// The `tak` topic: updates are small and a slow reader notices a gap by `seq`.
const TOPIC_CAPACITY: usize = 128;
const MARKER_STALE_S: f64 = 3600.0;

#[derive(Default)]
pub struct TakModule {
    service: Arc<TakService>,
}

impl TakModule {
    pub fn new() -> TakModule {
        TakModule::default()
    }
}

impl Module for TakModule {
    fn name(&self) -> &'static str {
        "tak"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::clone(&self.service));
        reg.config_block("tak", config::coerce_settings);
        reg.topic(Topic::new("tak", TOPIC_CAPACITY, |core, sink, _job| {
            let Some(svc) = core.service::<TakService>() else {
                return Binding::none();
            };
            let id = svc.add_listener(Arc::new(move |event: &Value| sink.put_fifo(event.clone())));
            let svc2 = svc.clone();
            Binding {
                unbind: Box::new(move || svc2.remove_listener(id)),
                initial: Some(snapshot(&svc)),
            }
        }));
        reg.get("/api/tak", api_status);
        reg.get("/api/tak/tracks", api_tracks_get);
        reg.post("/api/tak/tracks", api_tracks_post);
        reg.get("/api/tak/chat", api_chat_get);
        reg.post("/api/tak/settings", api_settings);
        reg.post("/api/tak/connection", api_connection);
        reg.post("/api/tak/connection/remove", api_connection_remove);
        reg.post("/api/tak/marker", api_marker);
        reg.post("/api/tak/chat", api_chat_post);
        reg.post("/api/tak/delete", api_delete);
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        let mut settings = config::effective(core.settings().get().get("tak"));
        if settings.uid.is_empty() {
            settings.uid = format!("CORVUS-{}", cot::random_id());
            let value = settings.to_value();
            if let Err(e) = core.settings().update_quiet(|cfg| cfg.set("tak", Some(value))) {
                eprintln!("tak: the identity was not saved: {e}");
            }
        }
        self.service.start(settings, core.store())
    }

    fn stop(&self, _core: &Core) {
        self.service.stop();
    }
}

fn snapshot(svc: &TakService) -> Value {
    let all = svc.tracks(&TrackQuery {
        include_stale: true,
        limit: 1000,
        ..TrackQuery::default()
    });
    json!({"type": "snapshot", "seq": svc.seq(), "status": svc.status(), "contacts": all, "chat": svc.chat()})
}

// ---------------------------------------------------------------- routes

macro_rules! svc {
    ($core:expr) => {
        match $core.service::<TakService>() {
            Some(s) => s,
            None => return Response::not_ok(503, "TAK is not available"),
        }
    };
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

fn flag(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.as_str() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_i64().map(|i| i != 0),
        _ => None,
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Where the aircraft is, when it is known.
fn aircraft(core: &Core) -> Option<(f64, f64)> {
    let store = core.store();
    if store.get("connected") != Some(Value::Bool(true)) {
        return None;
    }
    let pos = store.get("position")?;
    let (lon, lat) = (pos.get(0)?.as_f64()?, pos.get(1)?.as_f64()?);
    (lat.is_finite() && lon.is_finite() && !(lat == 0.0 && lon == 0.0)).then_some((lat, lon))
}

fn api_status(core: &Core, _req: &Request) -> Response {
    let svc = svc!(core);
    let mut v = svc.status();
    if let Some(o) = v.as_object_mut() {
        o.insert("teams".into(), json!(config::TEAMS));
        o.insert("roles".into(), json!(config::ROLES));
        o.insert("kinds".into(), json!(config::KINDS));
    }
    Response::json(v)
}

fn track_query(core: &Core, p: &Map<String, Value>) -> TrackQuery {
    let near = match (number(p.get("near_lat")), number(p.get("near_lon"))) {
        (Some(lat), Some(lon)) => Some((lat, lon)),
        _ => aircraft(core),
    };
    TrackQuery {
        text: text(p.get("query")),
        class: text(p.get("class")),
        affiliation: text(p.get("affiliation")),
        include_stale: flag(p.get("stale")).unwrap_or(true),
        near,
        within_m: number(p.get("within_m")).filter(|m| *m >= 0.0),
        limit: number(p.get("limit")).map_or(0, |n| n.max(1.0) as usize),
    }
}

fn tracks_answer(core: &Core, p: &Map<String, Value>) -> Response {
    let svc = svc!(core);
    let list = svc.tracks(&track_query(core, p));
    Response::json(json!({"count": list.len(), "contacts": list}))
}

fn api_tracks_get(core: &Core, req: &Request) -> Response {
    let mut p = Map::new();
    for k in [
        "query",
        "class",
        "affiliation",
        "stale",
        "near_lat",
        "near_lon",
        "within_m",
        "limit",
    ] {
        if let Some(v) = req.query.get(k) {
            p.insert(k.into(), v.into());
        }
    }
    tracks_answer(core, &p)
}

fn api_tracks_post(core: &Core, req: &Request) -> Response {
    tracks_answer(core, req.payload())
}

fn api_chat_get(core: &Core, _req: &Request) -> Response {
    let svc = svc!(core);
    Response::json(json!({"messages": svc.chat()}))
}

/// Applies `next`, saves it, and answers with the status.
fn commit(core: &Core, svc: &TakService, next: Settings) -> Response {
    svc.apply(next.clone());
    let value = next.to_value();
    match core.settings().update(core, |cfg| cfg.set("tak", Some(value))) {
        Ok(()) => Response::json(json!({"ok": true, "status": svc.status()})),
        Err(e) => Response::not_ok(500, &format!("not saved: {}", crate::plugins::config::io_text(&e))),
    }
}

fn api_settings(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    match svc.settings().merged(&Value::Object(req.payload().clone())) {
        Ok(next) => commit(core, &svc, next),
        Err(why) => Response::not_ok(400, &why),
    }
}

fn api_connection(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    match svc.settings().with_connection(&Value::Object(req.payload().clone())) {
        Ok((next, changed)) => {
            let r = commit(core, &svc, next);
            if r.status != 200 {
                return r;
            }
            Response::json(json!({"ok": true, "connection": changed.to_value(), "status": svc.status()}))
        }
        Err(why) => Response::not_ok(400, &why),
    }
}

fn api_connection_remove(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let Some(id) = text(req.payload().get("id")) else {
        return Response::not_ok(400, "id is required");
    };
    match svc.settings().without_connection(&id) {
        Ok(next) => commit(core, &svc, next),
        Err(why) => Response::not_ok(404, &why),
    }
}

fn api_marker(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    let (Some(lat), Some(lon)) = (number(p.get("lat")), number(p.get("lon"))) else {
        return Response::not_ok(400, "lat and lon are required numbers");
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Response::not_ok(400, "lat and lon must be on the globe");
    }
    let kind = text(p.get("type")).unwrap_or_else(|| "b-m-p-s-p-i".into());
    if !cot::marker_type_ok(&kind) {
        return Response::not_ok(
            400,
            "type must be a CoT atom such as a-f-G-U-C or a spot point b-m-p-s-p-i",
        );
    }
    let settings = svc.settings();
    let uid = match text(p.get("uid")) {
        Some(u) if u.len() <= 64 && u.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) => u,
        Some(_) => return Response::not_ok(400, "uid may hold letters, digits, . - and _ (64 at most)"),
        None => format!("{}.m.{}", settings.uid, cot::random_id()),
    };
    let stale_s = number(p.get("stale_s"))
        .unwrap_or(MARKER_STALE_S)
        .clamp(10.0, 604_800.0);
    let t = service::now();
    let event = cot::marker(
        &uid,
        &kind,
        &text(p.get("callsign")).unwrap_or_else(|| "Marker".into()),
        lat,
        lon,
        number(p.get("hae")),
        &text(p.get("remarks")).unwrap_or_default(),
        stale_s,
        t,
    );
    match svc.send(&event) {
        Ok(n) => {
            svc.record_own_marker(event);
            Response::json(json!({"ok": true, "uid": uid, "sent_to": n}))
        }
        Err(why) => Response::not_ok(409, &why),
    }
}

fn api_chat_post(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    let Some(body) = text(p.get("text")) else {
        return Response::not_ok(400, "text is required");
    };
    if body.chars().count() > 500 {
        return Response::not_ok(400, "text is at most 500 characters");
    }
    let s = svc.settings();
    let (lat, lon) = aircraft(core).unwrap_or((0.0, 0.0));
    let event = cot::chat(
        &s.uid,
        &s.callsign,
        &text(p.get("room")).unwrap_or_default(),
        &body,
        lat,
        lon,
        service::now(),
    );
    match svc.send(&event) {
        Ok(n) => Response::json(json!({"ok": true, "sent_to": n})),
        Err(why) => Response::not_ok(409, &why),
    }
}

fn api_delete(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let Some(uid) = text(req.payload().get("uid")) else {
        return Response::not_ok(400, "uid is required");
    };
    if uid.len() > 128 {
        return Response::not_ok(400, "uid is too long");
    }
    let s = svc.settings();
    match svc.send(&cot::delete(&uid, &s.uid, service::now())) {
        Ok(n) => {
            let removed = svc.forget(&uid);
            Response::json(json!({"ok": true, "sent_to": n, "removed": removed}))
        }
        Err(why) => Response::not_ok(409, &why),
    }
}
