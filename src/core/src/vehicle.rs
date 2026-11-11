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

use crate::autoconnect::{AutoConnectWatcher, SessionState, StartupOptions, WatcherConfig};
use crate::bridge::{Bridge, BridgeConfig};
use crate::core::Core;
use crate::fleet::{coerce_config, parse_vehicle_key, Fleet, FleetBridge, FleetStore, MAIN_LINK};
use crate::geofence;
use crate::hub::StoreHub;
use crate::mission;
use crate::module::{Module, Registry};
use crate::protocol::common::{TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M, VELOCITY_MAX_MS, YAW_RATE_MAX_DEG_S};
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
    /// What auto connect decided and what the operator chose by hand.
    pub session: Arc<SessionState>,
    links: Arc<Mutex<HashMap<String, Handles>>>,
    watcher: Mutex<Option<Arc<AutoConnectWatcher>>>,
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

/// How the vehicle area starts.
#[derive(Debug, Clone, Default)]
pub struct VehicleOptions {
    /// Open the main link at start (the way `serve.py` does) and run the auto
    /// connect watcher. Off, the link waits for `POST /api/mavlink/connect`.
    pub autostart: bool,
    /// A connection string from the command line, which outranks everything.
    pub connection: Option<String>,
}

pub struct VehicleModule {
    options: VehicleOptions,
}

impl VehicleModule {
    pub fn new() -> VehicleModule {
        VehicleModule {
            options: VehicleOptions::default(),
        }
    }

    pub fn with_options(options: VehicleOptions) -> VehicleModule {
        VehicleModule { options }
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
        reg.on_config(|core, cfg| {
            if let Some(svc) = core.service::<VehicleService>() {
                svc.session.apply_config(cfg.block("autoconnect"));
                svc.active().store.update([("link_auto", svc.session.as_value())]);
            }
        });
        reg.get("/api/mavlink/modes", api_mavlink_modes);
        reg.get("/api/mavlink/capabilities", api_mavlink_capabilities);
        reg.get("/api/mavlink/serial-ports", api_mavlink_serial_ports);
        reg.get("/api/mavlink/auto", api_mavlink_auto);
        reg.post("/api/mavlink/connect", api_mavlink_connect);
        reg.post("/api/mavlink/disconnect", api_mavlink_disconnect);
        reg.post("/api/mavlink/arm", api_mavlink_arm);
        reg.post("/api/mavlink/mode", api_mavlink_mode);
        reg.post("/api/mavlink/takeoff", api_mavlink_takeoff);
        reg.post("/api/mavlink/land", api_mavlink_land);
        reg.post("/api/mavlink/rtl", api_mavlink_rtl);
        reg.post("/api/mavlink/reboot", api_mavlink_reboot);
        reg.post("/api/mavlink/shell/send", api_mavlink_shell_send);
        reg.post("/api/mavlink/shell/close", api_mavlink_shell_close);
        reg.post("/api/mavlink/manual", api_mavlink_manual);
        reg.post("/api/mavlink/sethome", api_mavlink_sethome);
        reg.post("/api/mavlink/gotopoints", api_mavlink_gotopoints);
        reg.post("/api/mavlink/hold", api_mavlink_hold);
        reg.post("/api/mavlink/command", api_mavlink_command);
        reg.post("/api/mavlink/velocity", api_mavlink_velocity);
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
        let session = Arc::new(SessionState::new());
        session.apply_config(config.block("autoconnect"));
        let service = Arc::new(VehicleService {
            fleet: Arc::clone(&fleet),
            session: Arc::clone(&session),
            links,
            watcher: Mutex::new(None),
        });
        if let Some(hub) = core.service::<StoreHub>() {
            let svc = Arc::clone(&service);
            hub.set_resolver(Arc::new(move |_core, key| vehicle_target(&svc, key).map(|h| h.store)));
        }
        core.insert_service(Arc::clone(&service));
        fleet.start();
        if self.options.autostart {
            let main = service.handles(MAIN_LINK).expect("the main link has handles");
            let ports = crate::autoconnect::classify_ports(&crate::serial_ports::list_serial_ports());
            let validator = |c: &str| crate::bridge::validate_connection(c);
            let udp_fallback = config
                .block("autoconnect")
                .and_then(|b| b.get("udp_fallback"))
                .is_none_or(|v| v.as_bool().unwrap_or(true));
            let decision = crate::autoconnect::resolve_startup_connection(
                self.options.connection.as_deref(),
                Some(config.mavlink_connection()),
                &ports,
                &StartupOptions {
                    usb_enabled: session.usb(),
                    sik_enabled: session.sik(),
                    udp_fallback_enabled: udp_fallback,
                    validator: Some(&validator),
                    ..StartupOptions::default()
                },
            );
            let _ = main.bridge.set_connection(&decision.connection_string);
            session.note_decision(decision.reason, &decision.connection_string);
            main.store.update([("link_auto", session.as_value())]);
            main.bridge.start();
            let busy_fleet = Arc::clone(&fleet);
            let watcher = Arc::new(AutoConnectWatcher::new(
                Arc::new(main.bridge.clone()),
                Arc::clone(&main.store) as Arc<dyn crate::autoconnect::AutoStore>,
                Arc::clone(&session),
                Arc::new(crate::serial_ports::list_serial_ports),
                Arc::new(move || busy_fleet.extra_serial_devices()),
                WatcherConfig::default(),
            ));
            watcher.start();
            *lock(&service.watcher) = Some(watcher);
        }
        Ok(())
    }

    fn stop(&self, core: &Core) {
        if let Some(svc) = core.service::<VehicleService>() {
            if let Some(watcher) = lock(&svc.watcher).take() {
                watcher.stop();
            }
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
    if key.starts_with("main:") {
        note_manual_link(&svc, "");
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
    let (_svc, handles) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
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
    let (_svc, handles) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let count = match req.payload().get("items") {
        Some(Value::Number(n)) if n.as_i64().is_some_and(|c| c > 0) => n.as_i64().unwrap_or(0),
        _ => return Response::not_ok(400, "items must be a positive integer"),
    };
    if count > FLY_TO_MAX_POINTS as i64 {
        return Response::not_ok(400, &format!("items must be at most {FLY_TO_MAX_POINTS}"));
    }
    if !handles.bridge.start_mission(count) {
        return mission_failure(&handles.bridge, "mission start failed");
    }
    Response::json(json!({"ok": true}))
}

fn api_mission_clear(core: &Core, req: &Request) -> Response {
    let (_svc, handles) = match target_service(core, req) {
        Ok(t) => t,
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

// ------------------------------------------------- the link and its commands

/// Python's `float(value)` for the JSON values a body can hold.
fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim().replace('_', "");
            match t.to_lowercase().trim_start_matches(['+', '-']) {
                "nan" | "nan " => Some(f64::NAN),
                "inf" | "infinity" => Some(if t.starts_with('-') {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }),
                _ => t.parse::<f64>().ok(),
            }
        }
        _ => None,
    }
}

/// A JSON number that is not a bool and is finite.
fn finite_number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

fn target_service(core: &Core, req: &Request) -> Result<(Arc<VehicleService>, Handles), Response> {
    let Some(svc) = service(core) else {
        return Err(Response::not_ok(400, "not connected"));
    };
    let handles = target_of(&svc, req)?;
    Ok((svc, handles))
}

/// ok, or the bridge's reason: 503 when the link is down, 409 when the vehicle
/// or the station refused.
fn command_reply(bridge: &Bridge, ok: bool, fallback: &str, extra: Vec<(&str, Value)>) -> Response {
    if ok {
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        for (k, v) in extra {
            out.insert(k.into(), v);
        }
        return Response::json(Value::Object(out));
    }
    mission_failure(bridge, fallback)
}

fn api_mavlink_modes(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    Response::json(json!({"modes": h.bridge.available_modes(), "labels": h.bridge.mode_labels()}))
}

fn api_mavlink_capabilities(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    Response::json(h.bridge.capabilities())
}

fn api_mavlink_serial_ports(_core: &Core, _req: &Request) -> Response {
    let ports: Vec<Value> = crate::serial_ports::list_serial_ports()
        .into_iter()
        .map(|p| json!({"device": p.device, "description": p.description, "hwid": p.hwid}))
        .collect();
    Response::json(json!({"ports": ports}))
}

fn api_mavlink_auto(core: &Core, _req: &Request) -> Response {
    let Some(svc) = service(core) else {
        return Response::json(json!({"auto": {}, "suggestion": null}));
    };
    let snapshot = svc.active().store.snapshot();
    Response::json(json!({
        "auto": svc.session.as_value(),
        "suggestion": snapshot.get("link_suggestion").cloned().unwrap_or(Value::Null),
    }))
}

/// Records that the operator chose the link by hand, and clears the offer.
fn note_manual_link(svc: &VehicleService, connection: &str) {
    svc.session.note_manual_connect();
    if !connection.is_empty() {
        svc.session.note_decision("manual", connection);
    }
    if let Some(main) = svc.handles(MAIN_LINK) {
        main.store
            .update([("link_suggestion", Value::Null), ("link_auto", svc.session.as_value())]);
    }
}

fn api_mavlink_connect(core: &Core, req: &Request) -> Response {
    let conn = match req.payload().get("connection") {
        None => "udp:127.0.0.1:14550".to_string(),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Response::not_ok(400, "connection must be a non-empty string"),
    };
    let Some(svc) = service(core) else {
        return Response::not_ok(503, "mavlink not ready");
    };
    if let Err(why) = crate::bridge::validate_connection(&conn) {
        return Response::not_ok(400, &why);
    }
    let extra_link = !svc.fleet.active_is_main();
    let active_id = svc.fleet.active_link().id.clone();
    let in_use = svc.fleet.connection_in_use(&conn, &active_id);
    if !in_use.is_empty() {
        return Response::not_ok(409, &format!("{conn} is already open as {in_use}"));
    }
    if let Err(why) = svc.active().bridge.switch_connection(&conn) {
        return Response::not_ok(400, &why);
    }
    if extra_link {
        svc.fleet.link_changed();
    } else {
        note_manual_link(&svc, &conn);
    }
    Response::json(json!({"ok": true, "connection": conn}))
}

fn api_mavlink_disconnect(core: &Core, _req: &Request) -> Response {
    let Some(svc) = service(core) else {
        return Response::not_ok(503, "mavlink not ready");
    };
    svc.active().bridge.stop();
    if svc.fleet.active_is_main() {
        note_manual_link(&svc, "");
    }
    Response::json(json!({"ok": true}))
}

fn api_mavlink_arm(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let arm = match req.payload().get("arm") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Response::not_ok(400, "arm must be boolean"),
    };
    command_reply(&h.bridge, h.bridge.arm(arm), "arm command failed", vec![])
}

fn api_mavlink_mode(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    match req.payload().get("mode") {
        Some(Value::String(mode)) if !mode.is_empty() => command_reply(
            &h.bridge,
            h.bridge.set_mode(&mode.to_uppercase()),
            "mode change failed",
            vec![],
        ),
        _ => Response::json_status(json!({"error": "not connected or no mode"}), 400),
    }
}

fn parse_takeoff_altitude(value: Option<&Value>) -> Result<f64, String> {
    let value = value.unwrap_or(&Value::Null);
    if value.is_boolean() {
        return Err("takeoff altitude must be a number".into());
    }
    let Some(altitude) = py_float(value).filter(|_| !value.is_null()) else {
        return Err("takeoff altitude must be a number".into());
    };
    if !altitude.is_finite() {
        return Err("takeoff altitude must be finite".into());
    }
    if !(TAKEOFF_ALTITUDE_MIN_M..=TAKEOFF_ALTITUDE_MAX_M).contains(&altitude) {
        return Err(format!(
            "takeoff altitude must be between {} and {} m AGL",
            crate::protocol::common::fixed(TAKEOFF_ALTITUDE_MIN_M, 0),
            crate::protocol::common::fixed(TAKEOFF_ALTITUDE_MAX_M, 0)
        ));
    }
    Ok(altitude)
}

fn api_mavlink_takeoff(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let altitude = match parse_takeoff_altitude(Some(req.payload().get("altitude").unwrap_or(&json!(10.0)))) {
        Ok(a) => a,
        Err(why) => return Response::not_ok(400, &why),
    };
    command_reply(
        &h.bridge,
        h.bridge.takeoff(altitude),
        "takeoff command failed",
        vec![("altitude_agl", json!(altitude))],
    )
}

fn api_mavlink_land(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    command_reply(&h.bridge, h.bridge.land(), "land command failed", vec![])
}

fn api_mavlink_rtl(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    command_reply(&h.bridge, h.bridge.rtl(), "RTL command failed", vec![])
}

fn api_mavlink_reboot(core: &Core, _req: &Request) -> Response {
    let Some(svc) = service(core) else {
        return unavailable("not connected");
    };
    let bridge = svc.active().bridge;
    if bridge.reboot_autopilot() {
        return Response::json(json!({"ok": true}));
    }
    let error = bridge.last_error();
    let error = if error.is_empty() {
        "reboot failed".to_string()
    } else {
        error
    };
    let status = if error.contains("not connected") || error.contains("DISCONNECTED") {
        503
    } else {
        409
    };
    Response::not_ok(status, &error)
}

fn api_mavlink_shell_send(core: &Core, req: &Request) -> Response {
    let data = match req.payload().get("data") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Response::not_ok(400, "data must be non-empty text"),
    };
    let Some(svc) = service(core) else {
        return unavailable("not connected");
    };
    let bridge = svc.active().bridge;
    if bridge.shell_write(&data) {
        return Response::json(json!({"ok": true}));
    }
    let error = bridge.last_error();
    let error = if error.is_empty() {
        "shell write failed".to_string()
    } else {
        error
    };
    Response::not_ok(if error.contains("not connected") { 503 } else { 409 }, &error)
}

fn api_mavlink_shell_close(core: &Core, _req: &Request) -> Response {
    if let Some(svc) = service(core) {
        svc.active().bridge.shell_stop();
    }
    Response::json(json!({"ok": true}))
}

fn api_mavlink_manual(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let payload = req.payload();
    let mut axes = [0.0f64; 4];
    for (i, (name, low, high)) in [("x", -1.0, 1.0), ("y", -1.0, 1.0), ("z", 0.0, 1.0), ("r", -1.0, 1.0)]
        .into_iter()
        .enumerate()
    {
        let default = if name == "z" { 0.5 } else { 0.0 };
        let raw = payload.get(name).cloned().unwrap_or(json!(default));
        let Some(value) = finite_number(Some(&raw)) else {
            return Response::not_ok(400, &format!("{name} must be a finite number"));
        };
        if !(low..=high).contains(&value) {
            return Response::not_ok(
                400,
                &format!(
                    "{name} must be between {} and {}",
                    crate::protocol::common::py_g(low),
                    crate::protocol::common::py_g(high)
                ),
            );
        }
        axes[i] = value;
    }
    let buttons = match payload.get("buttons") {
        None => 0,
        Some(Value::Number(n)) if n.as_i64().is_some_and(|b| (0..=0xFFFF).contains(&b)) => n.as_i64().unwrap_or(0),
        Some(_) => return Response::not_ok(400, "buttons must be an integer between 0 and 65535"),
    };
    command_reply(
        &h.bridge,
        h.bridge.manual_control(axes[0], axes[1], axes[2], axes[3], buttons),
        "manual control send failed",
        vec![],
    )
}

fn api_mavlink_sethome(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let payload = req.payload();
    let mut got = [0.0f64; 2];
    for (i, (name, limit)) in [("lat", 90.0), ("lon", 180.0)].into_iter().enumerate() {
        let Some(value) = finite_number(payload.get(name)) else {
            return Response::not_ok(400, &format!("{name} must be a finite number"));
        };
        if !(-limit..=limit).contains(&value) {
            return Response::not_ok(
                400,
                &format!(
                    "{name} must be between {} and {}",
                    crate::protocol::common::py_g(-limit),
                    crate::protocol::common::py_g(limit)
                ),
            );
        }
        got[i] = value;
    }
    command_reply(&h.bridge, h.bridge.set_home(got[0], got[1]), "set home failed", vec![])
}

fn api_mavlink_gotopoints(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let Some(points) = req
        .payload()
        .get("points")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
    else {
        return Response::not_ok(400, "points must be a non-empty list");
    };
    if points.len() > FLY_TO_MAX_POINTS {
        return Response::not_ok(
            400,
            &format!("too many points ({}); the maximum is {FLY_TO_MAX_POINTS}", points.len()),
        );
    }
    let ground = crate::dialect::is_ground_vehicle(h.bridge.vehicle_type_id());
    let mut cleaned = Vec::new();
    for (index, item) in points.iter().enumerate() {
        let Some(item) = item.as_object() else {
            return Response::not_ok(400, &format!("point {index} must be an object"));
        };
        let alt_raw = match item.get("alt_agl") {
            None | Some(Value::Null) if ground => Some(json!(0.0)),
            other => other.cloned(),
        };
        let (lat, lon, alt) = (
            finite_number(item.get("lat")),
            finite_number(item.get("lon")),
            finite_number(alt_raw.as_ref()),
        );
        let Some(lat) = lat else {
            return Response::not_ok(400, &format!("point {index} lat must be a finite number"));
        };
        let Some(lon) = lon else {
            return Response::not_ok(400, &format!("point {index} lon must be a finite number"));
        };
        let Some(alt) = alt else {
            return Response::not_ok(400, &format!("point {index} alt_agl must be a finite number"));
        };
        if !(-90.0..=90.0).contains(&lat) {
            return Response::not_ok(400, &format!("point {index} lat must be between -90 and 90"));
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Response::not_ok(400, &format!("point {index} lon must be between -180 and 180"));
        }
        if !ground && !(TAKEOFF_ALTITUDE_MIN_M..=TAKEOFF_ALTITUDE_MAX_M).contains(&alt) {
            return Response::not_ok(
                400,
                &format!(
                    "point {index} alt_agl must be between {} and {} m AGL",
                    crate::protocol::common::fixed(TAKEOFF_ALTITUDE_MIN_M, 0),
                    crate::protocol::common::fixed(TAKEOFF_ALTITUDE_MAX_M, 0)
                ),
            );
        }
        cleaned.push(json!({"lat": lat, "lon": lon, "alt_agl": alt}));
    }
    command_reply(
        &h.bridge,
        h.bridge.fly_to_points(&Value::Array(cleaned)),
        "fly to points failed",
        vec![],
    )
}

fn api_mavlink_hold(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    command_reply(&h.bridge, h.bridge.hold(), "hold failed", vec![])
}

fn api_mavlink_command(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let payload = req.payload();
    let command: i64 = match payload.get("command") {
        Some(Value::String(raw)) if !raw.trim().is_empty() => {
            let name = raw.trim().to_uppercase();
            let name = name.strip_prefix("MAV_CMD_").unwrap_or(&name).to_string();
            match corvus_link::plain::defs::constant(&format!("MAV_CMD_{name}")) {
                Some(c) => c,
                None => return Response::not_ok(400, &format!("unknown command {raw}")),
            }
        }
        Some(Value::Number(n)) if n.as_i64().is_some_and(|c| (0..=0xFFFF).contains(&c)) => n.as_i64().unwrap_or(0),
        _ => return Response::not_ok(400, "command must be a MAV_CMD name or number"),
    };
    let positional = crate::dialect::plugin_position_commands().contains(&command);
    let limit = if positional { 4 } else { 7 };
    let raw_params = match payload.get("params") {
        None => Vec::new(),
        Some(Value::Array(a)) if a.len() <= limit => a.clone(),
        Some(_) => return Response::not_ok(400, &format!("params must be a list of at most {limit} numbers")),
    };
    let mut params = Vec::new();
    for (index, value) in raw_params.iter().enumerate() {
        if value.is_null() {
            params.push(f64::NAN);
            continue;
        }
        match finite_number(Some(value)) {
            Some(v) => params.push(v),
            None => return Response::not_ok(400, &format!("param {} must be a finite number or null", index + 1)),
        }
    }
    let mut position = None;
    let has_place = payload.contains_key("lat") || payload.contains_key("lon");
    if positional || has_place {
        if !positional {
            return Response::not_ok(400, "this command takes no position");
        }
        let mut values = [0.0f64; 3];
        for (i, (name, limit_deg)) in [("lat", Some(90.0)), ("lon", Some(180.0)), ("alt", None)]
            .into_iter()
            .enumerate()
        {
            let raw = payload
                .get(name)
                .cloned()
                .or_else(|| (name == "alt").then(|| json!(0.0)));
            let Some(value) = finite_number(raw.as_ref()) else {
                return Response::not_ok(400, &format!("{name} must be a finite number"));
            };
            if let Some(l) = limit_deg {
                if !(-l..=l).contains(&value) {
                    return Response::not_ok(
                        400,
                        &format!(
                            "{name} must be between {} and {}",
                            crate::protocol::common::py_g(-l),
                            crate::protocol::common::py_g(l)
                        ),
                    );
                }
            }
            values[i] = value;
        }
        let frame = match payload.get("frame") {
            None => "relative",
            Some(Value::String(f)) if f == "relative" || f == "amsl" => f.as_str(),
            Some(_) => return Response::not_ok(400, "frame must be \"relative\" or \"amsl\""),
        };
        position = Some((values[0], values[1], values[2], frame));
    }
    let ok = h.bridge.plugin_command(command, &params, position);
    command_reply(&h.bridge, ok, "command failed", vec![("command", json!(command))])
}

fn api_mavlink_velocity(core: &Core, req: &Request) -> Response {
    let (_, h) = match target_service(core, req) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let payload = req.payload();
    let mut v = [0.0f64; 3];
    for (i, name) in ["vx", "vy", "vz"].into_iter().enumerate() {
        let raw = payload.get(name).cloned().unwrap_or(json!(0.0));
        let Some(value) = finite_number(Some(&raw)) else {
            return Response::not_ok(400, &format!("{name} must be a finite number"));
        };
        if value.abs() > VELOCITY_MAX_MS {
            return Response::not_ok(
                400,
                &format!(
                    "{name} must be between {} and {} m/s",
                    crate::protocol::common::py_g(-VELOCITY_MAX_MS),
                    crate::protocol::common::py_g(VELOCITY_MAX_MS)
                ),
            );
        }
        v[i] = value;
    }
    let yaw_rate = match payload.get("yaw_rate") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            let Some(value) = finite_number(Some(raw)) else {
                return Response::not_ok(400, "yaw_rate must be a finite number");
            };
            if value.abs() > YAW_RATE_MAX_DEG_S {
                return Response::not_ok(
                    400,
                    &format!(
                        "yaw_rate must be between {} and {} degrees per second",
                        crate::protocol::common::py_g(-YAW_RATE_MAX_DEG_S),
                        crate::protocol::common::py_g(YAW_RATE_MAX_DEG_S)
                    ),
                );
            }
            Some(value)
        }
    };
    let body = match payload.get("frame") {
        None => false,
        Some(Value::String(f)) if f == "local" => false,
        Some(Value::String(f)) if f == "body" => true,
        Some(_) => return Response::not_ok(400, "frame must be \"local\" or \"body\""),
    };
    command_reply(
        &h.bridge,
        h.bridge.velocity_setpoint(v[0], v[1], v[2], yaw_rate, body),
        "velocity setpoint failed",
        vec![],
    )
}
