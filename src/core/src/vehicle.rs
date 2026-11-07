//! The vehicle area of the core: the main MAVLink link, the vehicle list around
//! it, and the routes that talk to them (`routes/fleet.py`, `routes/geofence.py`
//! and `routes/mission.py`).
//!
//! The module makes the main [`Bridge`] on the core's own state store and the
//! [`Fleet`] around it, and publishes both as [`VehicleService`]. It does not
//! open the link: that is the LINK tab's connect and auto connect, which come
//! with the rest of `routes/vehicle.py`. A request that names `vehicle`
//! (`<link>:<system id>`) acts on that vehicle's bridge and store, as
//! `targets_vehicle` does in Python.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Map, Value};

use crate::bridge::{Bridge, BridgeConfig};
use crate::core::Core;
use crate::fleet::{coerce_config, parse_vehicle_key, Fleet, FleetBridge, FleetStore, MAIN_LINK};
use crate::geofence;
use crate::hub::StoreHub;
use crate::mission;
use crate::module::{Module, Registry};
use crate::protocol::missions::FLY_TO_MAX_POINTS;
use crate::request::Request;
use crate::response::Response;
use crate::store::VehicleStateStore;
use crate::Topic;

/// One link's bridge and the store it fills.
#[derive(Clone)]
pub struct Handles {
    pub bridge: Bridge,
    pub store: Arc<VehicleStateStore>,
}

/// The vehicle list and the concrete bridge and store of every link.
pub struct VehicleService {
    pub fleet: Arc<Fleet>,
    links: Arc<Mutex<HashMap<String, Handles>>>,
}

impl VehicleService {
    /// The handles of link `link_id` while the fleet still has it.
    pub fn handles(&self, link_id: &str) -> Option<Handles> {
        self.fleet.link(link_id)?;
        lock(&self.links).get(link_id).cloned()
    }

    /// The handles of the link being flown.
    pub fn active(&self) -> Handles {
        let id = self.fleet.active_link().id.clone();
        self.handles(&id)
            .or_else(|| lock(&self.links).get(MAIN_LINK).cloned())
            .expect("the main link has handles")
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct VehicleModule;

impl VehicleModule {
    pub fn new() -> VehicleModule {
        VehicleModule
    }
}

impl Default for VehicleModule {
    fn default() -> Self {
        VehicleModule::new()
    }
}

impl Module for VehicleModule {
    fn name(&self) -> &'static str {
        "vehicle"
    }

    fn register(&self, reg: &mut Registry) {
        reg.config_block("fleet", |v| coerce_config(v).and_then(|c| c.as_object().cloned()));
        reg.topic(Topic::new("fleet", 1, |core, sink, _job| {
            let Some(svc) = core.service::<VehicleService>() else {
                return crate::Binding::none();
            };
            let id = svc
                .fleet
                .add_listener(Arc::new(move |snapshot: &Value| sink.put_latest(snapshot.clone())));
            let fleet = Arc::clone(&svc.fleet);
            crate::Binding {
                unbind: Box::new(move || fleet.remove_listener(id)),
                initial: Some(svc.fleet.snapshot()),
            }
        }));
        reg.get("/api/fleet", api_fleet);
        reg.post("/api/fleet/select", api_fleet_select);
        reg.post("/api/fleet/vehicle", api_fleet_vehicle);
        reg.post("/api/fleet/links/add", api_fleet_link_add);
        reg.post("/api/fleet/links/remove", api_fleet_link_remove);
        reg.post("/api/fleet/vehicle/remove", api_fleet_vehicle_remove);
        reg.get("/api/geofence", api_geofence);
        reg.post("/api/geofence/save", api_geofence_save);
        reg.post("/api/geofence/upload", api_geofence_upload);
        reg.post("/api/geofence/clear", api_geofence_clear);
        reg.post("/api/mission/upload", api_mission_upload);
        reg.post("/api/mission/start", api_mission_start);
        reg.post("/api/mission/clear", api_mission_clear);
        reg.post("/api/mission/download", api_mission_download);
        reg.get("/api/mission/plans", api_mission_plans);
        reg.post("/api/mission/plans/save", api_mission_plans_save);
        reg.post("/api/mission/plans/load", api_mission_plans_load);
        reg.post("/api/mission/plans/remove", api_mission_plans_remove);
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        let settings = core.settings();
        let config = settings.get();
        let store = core.store();
        let tlog_dir = config.str_field("tlog_dir").trim();
        let log_dir = if tlog_dir.is_empty() {
            core.paths().join("logs")
        } else {
            tlog_dir.into()
        };
        let main = Bridge::new(
            Arc::clone(&store),
            BridgeConfig::new(config.mavlink_connection(), log_dir.clone()),
        );
        let links: Arc<Mutex<HashMap<String, Handles>>> = Arc::default();
        lock(&links).insert(
            MAIN_LINK.into(),
            Handles {
                bridge: main.clone(),
                store: Arc::clone(&store),
            },
        );

        let pending: Arc<Mutex<Option<Arc<VehicleStateStore>>>> = Arc::default();
        let (factory_links, factory_pending) = (Arc::clone(&links), Arc::clone(&pending));
        let bridge_factory: crate::fleet::BridgeFactory = Box::new(move |link_id, _store, connection| {
            let extra = lock(&factory_pending).take().ok_or("no store for the link")?;
            let mut cfg = BridgeConfig::new(connection, log_dir.clone());
            cfg.link_id = link_id.to_string();
            let bridge = Bridge::new(Arc::clone(&extra), cfg);
            lock(&factory_links).insert(
                link_id.to_string(),
                Handles {
                    bridge: bridge.clone(),
                    store: extra,
                },
            );
            Ok(Arc::new(bridge) as Arc<dyn FleetBridge>)
        });
        let store_factory: crate::fleet::StoreFactory = Box::new(move || {
            let extra = Arc::new(VehicleStateStore::new());
            *lock(&pending) = Some(Arc::clone(&extra));
            extra as Arc<dyn FleetStore>
        });
        let persist_settings = Arc::clone(&settings);
        let persist: crate::fleet::Persist = Box::new(move |block| {
            let _ = persist_settings.update_quiet(|cfg| cfg.set("fleet", block));
        });
        let fleet = Fleet::new(Arc::new(main) as Arc<dyn FleetBridge>, store as Arc<dyn FleetStore>)
            .with_factories(bridge_factory, store_factory)
            .with_persist(persist);

        if let Some(hub) = core.service::<StoreHub>() {
            let hook_links = Arc::clone(&links);
            let hook_hub = Arc::clone(&hub);
            fleet.set_switch_hook(Box::new(move |link| {
                if let Some(handles) = lock(&hook_links).get(&link.id) {
                    hook_hub.set_current(Arc::clone(&handles.store));
                }
            }));
        }
        fleet.load(&config.block("fleet").map_or(Value::Null, |b| Value::Object(b.clone())));
        let fleet = Arc::new(fleet);
        let service = Arc::new(VehicleService {
            fleet: Arc::clone(&fleet),
            links,
        });
        if let Some(hub) = core.service::<StoreHub>() {
            let svc = Arc::clone(&service);
            hub.set_resolver(Arc::new(move |_core, key| vehicle_target(&svc, key).map(|h| h.store)));
        }
        core.insert_service(service);
        fleet.start();
        Ok(())
    }

    fn stop(&self, core: &Core) {
        if let Some(svc) = core.service::<VehicleService>() {
            svc.fleet.stop();
            if let Some(main) = svc.handles(MAIN_LINK) {
                main.bridge.stop();
            }
        }
    }
}

// ---------------------------------------------------------------- helpers

fn service(core: &Core) -> Option<Arc<VehicleService>> {
    core.service::<VehicleService>()
}

fn unavailable(what: &str) -> Response {
    Response::not_ok(503, what)
}

/// The bridge and store that fly vehicle `key`, or the refusal: 400 for an id
/// that is not one, 409 for a vehicle whose link is closed or flies another
/// system, 503 while its link has lost it.
fn vehicle_target(svc: &VehicleService, key: &str) -> Result<Handles, Response> {
    let Some((link_id, system_id)) = parse_vehicle_key(key) else {
        return Err(Response::not_ok(400, "unknown vehicle"));
    };
    let Some(handles) = svc.handles(&link_id) else {
        return Err(Response::not_ok(409, "that vehicle's link is not open"));
    };
    match FleetBridge::controlled_system(&handles.bridge) {
        Some(c) if c != system_id => Err(Response::not_ok(409, &format!("this link flies system {c}"))),
        Some(_) => Ok(handles),
        None => Err(Response::not_ok(503, "that vehicle is not connected")),
    }
}

/// The vehicle a request acts on: the one it names (`vehicle` in the body or
/// the query), else the one being flown.
fn target_of(svc: &VehicleService, req: &Request) -> Result<Handles, Response> {
    let named = req
        .payload()
        .get("vehicle")
        .cloned()
        .or_else(|| req.query.get("vehicle").map(|s| json!(s)));
    match named {
        None | Some(Value::Null) => Ok(svc.active()),
        Some(Value::String(s)) if s.is_empty() => Ok(svc.active()),
        Some(Value::String(s)) => vehicle_target(svc, &s),
        Some(_) => Err(Response::not_ok(400, "unknown vehicle")),
    }
}

fn str_of<'a>(payload: &'a Map<String, Value>, key: &str) -> &'a str {
    payload.get(key).and_then(Value::as_str).unwrap_or("\u{0}not a string")
}

fn is_text(payload: &Map<String, Value>, key: &str) -> bool {
    payload.get(key).is_some_and(Value::is_string)
}

// ------------------------------------------------------------- the fleet

fn api_fleet(core: &Core, _req: &Request) -> Response {
    match service(core) {
        Some(svc) => Response::json(svc.fleet.snapshot()),
        None => {
            Response::json(json!({"active": "", "active_link": "main", "links": [], "vehicles": [], "palette": []}))
        }
    }
}

fn fleet_of(core: &Core, what: &str) -> Result<Arc<VehicleService>, Response> {
    service(core).ok_or_else(|| unavailable(what))
}

fn api_fleet_select(core: &Core, req: &Request) -> Response {
    let svc = match fleet_of(core, "only one vehicle is supported here") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = if is_text(req.payload(), "id") {
        str_of(req.payload(), "id")
    } else {
        "\u{0}"
    };
    let error = svc.fleet.select(id);
    if !error.is_empty() {
        return Response::not_ok(400, &error);
    }
    Response::json(json!({"ok": true, "fleet": svc.fleet.snapshot()}))
}

fn api_fleet_vehicle(core: &Core, req: &Request) -> Response {
    let svc = match fleet_of(core, "only one vehicle is supported here") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let payload = req.payload();
    let (name, color) = (payload.get("name"), payload.get("color"));
    if name.is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Response::not_ok(400, "name must be text");
    }
    if color.is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Response::not_ok(400, "colour must be #RRGGBB");
    }
    let id = if is_text(payload, "id") {
        str_of(payload, "id")
    } else {
        "\u{0}"
    };
    let error = svc.fleet.set_meta(id, name, color);
    if !error.is_empty() {
        return Response::not_ok(400, &error);
    }
    Response::json(json!({"ok": true, "fleet": svc.fleet.snapshot()}))
}

fn api_fleet_link_add(core: &Core, req: &Request) -> Response {
    let svc = match fleet_of(core, "only one link is supported here") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let connection = req.payload().get("connection").and_then(Value::as_str);
    let (id, error) = match connection {
        Some(c) => svc.fleet.add_link(c),
        None => (None, "connection must be a non-empty string".to_string()),
    };
    match id {
        Some(id) => Response::json(json!({"ok": true, "id": id, "fleet": svc.fleet.snapshot()})),
        None => Response::not_ok(400, &error),
    }
}

fn api_fleet_link_remove(core: &Core, req: &Request) -> Response {
    let svc = match fleet_of(core, "only one link is supported here") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = if is_text(req.payload(), "id") {
        str_of(req.payload(), "id")
    } else {
        "\u{0}"
    };
    let error = svc.fleet.remove_link(id);
    if !error.is_empty() {
        return Response::not_ok(400, &error);
    }
    Response::json(json!({"ok": true, "fleet": svc.fleet.snapshot()}))
}

fn api_fleet_vehicle_remove(core: &Core, req: &Request) -> Response {
    let svc = match fleet_of(core, "only one vehicle is supported here") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let payload = req.payload();
    let confirm = match payload.get("confirm_armed") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Response::not_ok(400, "confirm_armed must be true or false"),
    };
    let key = if is_text(payload, "id") {
        str_of(payload, "id")
    } else {
        "\u{0}"
    };
    let (error, status) = svc.fleet.remove_vehicle(key, confirm);
    if !error.is_empty() {
        return Response::not_ok(status, &error);
    }
    Response::json(json!({"ok": true, "fleet": svc.fleet.snapshot()}))
}

// ------------------------------------------------------------- geofence

fn geofence_file(core: &Core) -> std::path::PathBuf {
    let config = core.settings().path().to_path_buf();
    let directory = std::fs::canonicalize(config.parent().unwrap_or(std::path::Path::new(".")))
        .unwrap_or_else(|_| config.parent().map(std::path::Path::to_path_buf).unwrap_or_default());
    directory.join(geofence::FILE_NAME)
}

fn geofence_payload(svc: Option<&Arc<VehicleService>>, stored: &Value) -> Value {
    let polygon = stored["polygon"].clone();
    let on_vehicle = svc.and_then(|s| s.active().bridge.vehicle_fence());
    let list = |p: &[(f64, f64)]| Value::Array(p.iter().map(|(lon, lat)| json!([lon, lat])).collect());
    json!({
        "ok": true,
        "polygon": polygon,
        "show_on_map": stored["show_on_map"],
        "on_vehicle": on_vehicle.as_ref().map_or(Value::Null, |p| json!(list(p) == polygon)),
        "vehicle_has_fence": on_vehicle.as_ref().map_or(Value::Null, |p| json!(!p.is_empty())),
        "max_vertices": geofence::MAX_VERTICES,
    })
}

fn api_geofence(core: &Core, _req: &Request) -> Response {
    let stored = geofence::load(&geofence_file(core));
    Response::json(geofence_payload(service(core).as_ref(), &stored))
}

fn api_geofence_save(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let path = geofence_file(core);
    let mut stored = geofence::load(&path);
    if let Some(raw) = payload.get("polygon") {
        match geofence::validate_polygon(raw) {
            Ok(polygon) => {
                stored["polygon"] = Value::Array(polygon.iter().map(|(lon, lat)| json!([lon, lat])).collect());
            }
            Err(why) => return Response::not_ok(400, &why),
        }
    }
    if let Some(show) = payload.get("show_on_map") {
        let Some(show) = show.as_bool() else {
            return Response::not_ok(400, "show_on_map must be true or false");
        };
        stored["show_on_map"] = json!(show);
    }
    if let Err(e) = geofence::save(&stored, &path) {
        return Response::not_ok(500, &format!("could not save: {e}"));
    }
    Response::json(geofence_payload(service(core).as_ref(), &stored))
}

/// Why the bridge refused, with the status that says whose fault: 503 for a
/// link that is not there, 409 for a vehicle that answered and said no.
fn mission_failure(bridge: &Bridge, fallback: &str) -> Response {
    let error = bridge.last_error();
    let error = if error.is_empty() { fallback.to_string() } else { error };
    let status = if error.contains("DISCONNECTED") { 503 } else { 409 };
    Response::not_ok(status, &error)
}

fn api_geofence_upload(core: &Core, _req: &Request) -> Response {
    let stored = geofence::load(&geofence_file(core));
    let polygon: Vec<(f64, f64)> = stored["polygon"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| (p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(0.0)))
                .collect()
        })
        .unwrap_or_default();
    if polygon.is_empty() {
        return Response::not_ok(400, "no area has been drawn");
    }
    let Some(svc) = service(core) else {
        return unavailable("not connected");
    };
    let bridge = svc.active().bridge;
    if !bridge.upload_fence(&polygon) {
        return mission_failure(&bridge, "geofence upload failed");
    }
    Response::json(geofence_payload(Some(&svc), &stored))
}

fn api_geofence_clear(core: &Core, _req: &Request) -> Response {
    let Some(svc) = service(core) else {
        return unavailable("not connected");
    };
    let bridge = svc.active().bridge;
    if !bridge.clear_fence() {
        return mission_failure(&bridge, "geofence clear failed");
    }
    Response::json(geofence_payload(Some(&svc), &geofence::load(&geofence_file(core))))
}

// -------------------------------------------------------------- missions

fn missions_dir(core: &Core) -> std::path::PathBuf {
    let config = core.settings().get();
    let configured = config.str_field("missions_dir").trim().to_string();
    let expanded = match configured.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => match std::env::var("HOME") {
            Ok(home) => format!("{home}{rest}"),
            Err(_) => configured.clone(),
        },
        _ => configured.clone(),
    };
    mission::missions_dir(&expanded, core.paths())
}

fn api_mission_upload(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let plan = match mission::validate_plan(payload.get("plan").unwrap_or(&Value::Null)) {
        Ok(plan) => plan,
        Err(why) => return Response::not_ok(400, &why),
    };
    if plan["items"].as_array().is_none_or(Vec::is_empty) {
        return Response::not_ok(400, "the mission is empty");
    }
    let start = match payload.get("start") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Response::not_ok(400, "start must be true or false"),
    };
    let Some(svc) = service(core) else {
        return Response::not_ok(400, "not connected");
    };
    let handles = match target_of(&svc, req) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let bridge = &handles.bridge;
    if plan.get("aircraft").and_then(Value::as_str) == Some(mission::GROUND_AIRCRAFT)
        && !crate::dialect::is_ground_vehicle(bridge.vehicle_type_id())
    {
        return Response::not_ok(
            409,
            "This plan is drawn for a rover or a boat. Its points are at ground level, so an aircraft would fly into the ground. Pick the aircraft in the planner and set the heights first.",
        );
    }
    let items = bridge.lower_mission_plan(&mission::plan_to_items(&plan));
    if !bridge.upload_mission_plan(&items) {
        return mission_failure(bridge, "mission upload failed");
    }
    let revision = handles.store.get("mission_revision").unwrap_or(Value::Null);
    if !start {
        return Response::json(json!({"ok": true, "items": items.len(), "started": false, "revision": revision}));
    }
    if !bridge.start_mission(items.len() as i64) {
        return mission_failure(bridge, "mission start failed");
    }
    Response::json(json!({"ok": true, "items": items.len(), "started": true, "revision": revision}))
}

fn api_mission_start(core: &Core, req: &Request) -> Response {
    let count = match req.payload().get("items") {
        Some(Value::Number(n)) if n.as_i64().is_some_and(|c| c > 0) => n.as_i64().unwrap_or(0),
        _ => return Response::not_ok(400, "items must be a positive integer"),
    };
    if count > FLY_TO_MAX_POINTS as i64 {
        return Response::not_ok(400, &format!("items must be at most {FLY_TO_MAX_POINTS}"));
    }
    let Some(svc) = service(core) else {
        return Response::not_ok(400, "not connected");
    };
    let handles = match target_of(&svc, req) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !handles.bridge.start_mission(count) {
        return mission_failure(&handles.bridge, "mission start failed");
    }
    Response::json(json!({"ok": true}))
}

fn api_mission_clear(core: &Core, req: &Request) -> Response {
    let Some(svc) = service(core) else {
        return Response::not_ok(400, "not connected");
    };
    let handles = match target_of(&svc, req) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !handles.bridge.clear_mission() {
        return mission_failure(&handles.bridge, "mission clear failed");
    }
    Response::json(json!({"ok": true}))
}

fn api_mission_download(core: &Core, _req: &Request) -> Response {
    let Some(svc) = service(core) else {
        return unavailable("not connected");
    };
    let bridge = svc.active().bridge;
    let Some(result) = bridge.download_mission() else {
        return mission_failure(&bridge, "mission download failed");
    };
    let mut out = result.as_object().cloned().unwrap_or_default();
    out.insert("ok".into(), json!(true));
    Response::json(Value::Object(out))
}

fn api_mission_plans(core: &Core, _req: &Request) -> Response {
    let directory = missions_dir(core);
    Response::json(json!({"dir": directory.to_string_lossy(), "plans": mission::list_plans(&directory)}))
}

fn api_mission_plans_save(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let plan = match mission::validate_plan(payload.get("plan").unwrap_or(&Value::Null)) {
        Ok(plan) => plan,
        Err(why) => return Response::not_ok(400, &why),
    };
    let mut name = mission::safe_plan_name(payload.get("name").unwrap_or(&Value::Null));
    if name.is_empty() {
        name = plan["name"].as_str().unwrap_or("").to_string();
    }
    if name.is_empty() {
        return Response::not_ok(400, "a mission needs a name");
    }
    let directory = missions_dir(core);
    match mission::write_plan(&directory, &name, &plan) {
        Ok(stored) => Response::json(json!({"ok": true, "name": stored, "dir": directory.to_string_lossy()})),
        Err(e) => Response::not_ok(500, &format!("could not save: {e}")),
    }
}

fn api_mission_plans_load(core: &Core, req: &Request) -> Response {
    let name = mission::safe_plan_name(req.payload().get("name").unwrap_or(&Value::Null));
    if name.is_empty() {
        return Response::not_ok(400, "name must be a mission name");
    }
    match mission::read_plan(&missions_dir(core), &name) {
        Some(plan) => Response::json(json!({"ok": true, "name": name, "plan": plan})),
        None => Response::not_ok(
            404,
            &format!("no saved mission named {}", mission::py_repr(&json!(name))),
        ),
    }
}

fn api_mission_plans_remove(core: &Core, req: &Request) -> Response {
    let name = mission::safe_plan_name(req.payload().get("name").unwrap_or(&Value::Null));
    if name.is_empty() {
        return Response::not_ok(400, "name must be a mission name");
    }
    if !mission::remove_plan(&missions_dir(core), &name) {
        return Response::not_ok(500, "could not delete the mission");
    }
    Response::json(json!({"ok": true}))
}
