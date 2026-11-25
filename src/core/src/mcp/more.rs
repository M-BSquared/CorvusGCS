//! The tools added to the first set: drawing on the map, places worked out for
//! the client, flying relative to the vehicle, patterns, the geofence, the
//! camera, and waiting for what was asked to happen.
//!
//! They follow the rule of every tool: they run endpoints (`McpService::api`),
//! never the link, so the checks, the PX4 and ArduPilot sequences and the
//! refusals are the buttons' own. What a vehicle can do is read from its
//! capabilities, never from which autopilot it is.

use std::time::Duration;

use serde_json::{json, Value};

use super::engine::{num, obj, quote, req_str, McpService, ToolError};
use super::geo::{self, Place};
use super::tools::{fmt, get, pick, tool, Run, Tool};
use crate::json::Map;
use crate::mission::MISSION_MAX_ITEMS;

pub(super) type ToolResult = Result<Value, ToolError>;

/// The names of the tools here, in the order a client lists them.
pub const NAMES: [&str; 32] = [
    "resolve_place",
    "measure",
    "wait_for",
    "plan_pattern",
    "get_geofence",
    "list_saved_missions",
    "get_saved_mission",
    "draw_on_map",
    "list_drawings",
    "remove_drawings",
    "show_on_map",
    "open_page",
    "search_place",
    "notify_operator",
    "fly_pattern",
    "save_mission",
    "resume_mission",
    "set_mission_item",
    "set_speed",
    "set_heading",
    "move_by",
    "change_altitude",
    "orbit",
    "fly_drawing",
    "set_geofence",
    "clear_geofence",
    "look_at",
    "stop_looking",
    "point_gimbal",
    "take_photo",
    "set_photo_distance",
    "face_place",
];

const WAIT_MAX_S: f64 = 30.0;
const WAIT_POLL: Duration = Duration::from_millis(400);
/// The farthest one relative move goes.
const MOVE_MAX_M: f64 = 5_000.0;
/// How far from its reference a place may be.
const REACH_MAX_M: f64 = 100_000.0;
/// Height above home that counts as flying when the firmware does not say.
const AIRBORNE_M: f64 = 1.5;
const ORBIT_POINTS: usize = 24;
const ORBIT_TURNS_MAX: f64 = 10.0;
/// The points a pattern may have: a mission with the takeoff, the speed and the end.
const PATTERN_POINTS: usize = MISSION_MAX_ITEMS - 5;
const IN_AIR: [i64; 3] = [2, 3, 4];

pub(super) fn err(text: impl Into<String>) -> ToolError {
    ToolError(text.into())
}

// ------------------------------------------------------------- the vehicle

/// The vehicle a call is about: its state and abilities, read when needed.
pub(super) struct Craft<'a> {
    svc: &'a McpService,
    id: Option<&'a str>,
    state: Option<Value>,
    caps: Option<Value>,
}

impl<'a> Craft<'a> {
    pub(super) fn of(svc: &'a McpService, args: &'a Map) -> Craft<'a> {
        Craft {
            svc,
            id: args.get("vehicle").and_then(Value::as_str),
            state: None,
            caps: None,
        }
    }

    pub(super) fn get(&self, path: &str) -> ToolResult {
        self.svc.api("GET", path, None, self.id)
    }

    pub(super) fn post(&self, path: &str, body: Value) -> ToolResult {
        self.svc.api("POST", path, Some(obj(body)), self.id)
    }

    pub(super) fn fresh(&mut self) -> Result<&Value, ToolError> {
        self.state = None;
        self.state()
    }

    pub(super) fn state(&mut self) -> Result<&Value, ToolError> {
        if self.state.is_none() {
            self.state = Some(self.get("/api/state")?);
        }
        let s = self.state.as_ref().unwrap_or(&Value::Null);
        if s.get("connected") == Some(&json!(true)) {
            Ok(s)
        } else {
            Err(err("No vehicle is connected."))
        }
    }

    /// A position of the state, which is kept longitude first.
    pub(super) fn spot(&mut self, key: &str) -> Result<Place, ToolError> {
        let what = if key == "home" { "home" } else { "the vehicle" };
        let v = self.state()?.get(key).cloned().unwrap_or(Value::Null);
        match (v.get(0).and_then(Value::as_f64), v.get(1).and_then(Value::as_f64)) {
            (Some(lon), Some(lat)) if lat.is_finite() && lon.is_finite() && (lat, lon) != (0.0, 0.0) => Ok([lat, lon]),
            _ => Err(err(format!("The position of {what} is not known."))),
        }
    }

    pub(super) fn number(&mut self, key: &str) -> Result<f64, ToolError> {
        let v = self.state()?.get(key).and_then(Value::as_f64);
        v.filter(|n| n.is_finite())
            .ok_or_else(|| err(format!("The vehicle has not reported its {key}.")))
    }

    pub(super) fn caps(&mut self) -> Result<&Value, ToolError> {
        if self.caps.is_none() {
            self.caps = Some(self.get("/api/mavlink/capabilities")?);
        }
        Ok(self.caps.as_ref().unwrap_or(&Value::Null))
    }

    pub(super) fn ground(&mut self) -> bool {
        self.caps().is_ok_and(|c| c.get("ground") == Some(&json!(true)))
    }

    pub(super) fn class_is(&mut self, class: &str) -> bool {
        self.caps().is_ok_and(|c| c.get("vehicle") == Some(&json!(class)))
    }

    pub(super) fn takes(&mut self, command: &str) -> bool {
        self.caps().is_ok_and(|c| {
            c.get("commands")
                .and_then(Value::as_array)
                .is_some_and(|l| l.contains(&json!(command)))
        })
    }

    pub(super) fn airborne(&mut self) -> Result<bool, ToolError> {
        let s = self.state()?;
        Ok(flying(s))
    }

    /// Nothing here starts a flight: a vehicle on the ground is told so.
    pub(super) fn must_fly(&mut self) -> Result<(), ToolError> {
        if self.ground() || self.airborne()? {
            Ok(())
        } else {
            Err(err("The vehicle is not flying. Call takeoff first."))
        }
    }

    pub(super) fn command(&self, command: &str, params: Value) -> ToolResult {
        self.post("/api/mavlink/command", json!({"command": command, "params": params}))
    }

    /// One point or several in order; a height is left out for a rover.
    pub(super) fn go(&mut self, points: &[Place], alt: f64) -> ToolResult {
        let ground = self.ground();
        let wire: Vec<Value> = points
            .iter()
            .map(|p| {
                if ground {
                    json!({"lat": p[0], "lon": p[1]})
                } else {
                    json!({"lat": p[0], "lon": p[1], "alt_agl": alt})
                }
            })
            .collect();
        self.post("/api/mavlink/gotopoints", json!({"points": wire}))
    }
}

pub(super) fn flying(s: &Value) -> bool {
    let landed = s.get("landed_state").and_then(Value::as_i64).unwrap_or(0);
    let agl = s.get("altitude_agl").and_then(Value::as_f64).unwrap_or(0.0);
    let armed = s.get("armed") == Some(&json!(true));
    IN_AIR.contains(&landed) || (landed == 0 && armed && agl > AIRBORNE_M)
}

fn brief(s: &Value) -> Map {
    let mut out = Map::new();
    for key in [
        "armed",
        "mode",
        "altitude_agl",
        "groundspeed",
        "heading",
        "battery_percent",
        "mission_seq",
    ] {
        out.insert(key.into(), s.get(key).cloned().unwrap_or(Value::Null));
    }
    out.insert("flying".into(), flying(s).into());
    out
}

// ----------------------------------------------------------------- places

pub(super) fn opt(v: &Map, key: &str) -> Result<Option<f64>, ToolError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x
            .as_f64()
            .filter(|n| n.is_finite())
            .map(Some)
            .ok_or_else(|| err(format!("{key} must be a number"))),
    }
}

/// A place written another way: `[lat, lon]`, "48.1, 11.6", "vehicle" or "home".
fn spelled_place(v: &Value) -> Option<Map> {
    let pair = |lat: f64, lon: f64| Some(obj(json!({"lat": lat, "lon": lon})));
    match v {
        Value::Array(l) if l.len() == 2 => pair(l[0].as_f64()?, l[1].as_f64()?),
        Value::String(s) => match s.trim() {
            from @ ("vehicle" | "home") => Some(obj(json!({"from": from}))),
            text => {
                let (lat, lon) = text.split_once(',')?;
                pair(lat.trim().parse().ok()?, lon.trim().parse().ok()?)
            }
        },
        _ => None,
    }
}

/// A place: `lat` and `lon`, or the vehicle or home, and from there any of
/// north and east, forward and right, or a bearing and a distance.
pub(super) fn place(craft: &mut Craft, v: &Value, what: &str) -> Result<Place, ToolError> {
    let spelled = spelled_place(v);
    let Some(v) = spelled.as_ref().or(v.as_object()) else {
        return Err(err(format!("{what} must be an object with lat and lon, or from")));
    };
    let from = v.get("from").and_then(Value::as_str);
    let mut at = match (opt(v, "lat")?, opt(v, "lon")?, from) {
        (Some(lat), Some(lon), _) => {
            if lat.abs() > 90.0 || lon.abs() > 180.0 {
                return Err(err(format!("{what} is not on the globe")));
            }
            [lat, lon]
        }
        (None, None, Some("home")) => craft.spot("home")?,
        (None, None, Some("vehicle")) => craft.spot("position")?,
        (None, None, Some(_)) => return Err(err("from must be vehicle or home")),
        (None, None, None) if moves(v) => craft.spot("position")?,
        _ => {
            return Err(err(format!(
                "{what} needs lat and lon, or from (vehicle or home) with an offset"
            )))
        }
    };
    let (mut north, mut east) = (opt(v, "north_m")?.unwrap_or(0.0), opt(v, "east_m")?.unwrap_or(0.0));
    let (forward, right) = (opt(v, "forward_m")?.unwrap_or(0.0), opt(v, "right_m")?.unwrap_or(0.0));
    if forward != 0.0 || right != 0.0 {
        let (sin, cos) = craft.number("heading")?.to_radians().sin_cos();
        north += forward * cos - right * sin;
        east += forward * sin + right * cos;
    }
    if north.hypot(east) > REACH_MAX_M {
        return Err(err(format!(
            "{what} is more than {REACH_MAX_M} m away from its reference"
        )));
    }
    at = geo::offset(at, north, east);
    match (opt(v, "bearing_deg")?, opt(v, "distance_m")?) {
        (Some(b), Some(d)) if (0.0..=REACH_MAX_M).contains(&d) => at = geo::project(at, b, d),
        (None, None) => {}
        (Some(_), Some(_)) => return Err(err(format!("distance_m must be between 0 and {REACH_MAX_M}"))),
        _ => return Err(err("bearing_deg and distance_m go together")),
    }
    Ok(at)
}

fn moves(v: &Map) -> bool {
    ["north_m", "east_m", "forward_m", "right_m", "bearing_deg", "distance_m"]
        .iter()
        .any(|k| v.get(*k).is_some_and(|x| !x.is_null()))
}

pub(super) fn places(craft: &mut Craft, v: Option<&Value>, what: &str) -> Result<Vec<Place>, ToolError> {
    let Some(list) = v.and_then(Value::as_array).filter(|l| !l.is_empty()) else {
        return Err(err(format!("{what} must be a non-empty list of places")));
    };
    if list.len() > crate::drawings::MAX_POINTS {
        return Err(err(format!("{what} may hold at most {}", crate::drawings::MAX_POINTS)));
    }
    list.iter()
        .enumerate()
        .map(|(i, p)| place(craft, p, &format!("{what} {i}")))
        .collect()
}

pub(super) fn wire(p: Place) -> Value {
    json!({"lat": p[0], "lon": p[1]})
}

pub(super) fn round(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

/// A direction to a tenth of a degree, 0 and never 360.
pub(super) fn degrees(x: f64) -> f64 {
    round(x, 1).rem_euclid(360.0)
}

pub(super) fn within(args: &Map, key: &str, default: Option<f64>, low: f64, high: f64) -> Result<f64, ToolError> {
    let v = num(args, key, default)?;
    if (low..=high).contains(&v) {
        Ok(v)
    } else {
        Err(err(format!("{key} must be between {low} and {high}")))
    }
}

// --------------------------------------------------------------- reading

fn resolve_place(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let at = place(&mut craft, &get(args, "place"), "place")?;
    let mut out = obj(wire(at));
    for (key, name) in [("position", "vehicle"), ("home", "home")] {
        if let Ok(from) = craft.spot(key) {
            out.insert(
                format!("from_{name}"),
                json!({"distance_m": round(geo::distance(from, at), 1), "bearing_deg": degrees(geo::bearing(from, at))}),
            );
        }
    }
    Ok(Value::Object(out))
}

fn measure(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let mut path = places(&mut craft, args.get("points"), "points")?;
    if path.len() < 2 {
        return Err(err("points needs at least two places"));
    }
    let closed = args.get("closed") == Some(&json!(true));
    let area = geo::area(&path);
    if closed {
        path.push(path[0]);
    }
    let legs: Vec<Value> = path
        .windows(2)
        .map(|w| {
            json!({"distance_m": round(geo::distance(w[0], w[1]), 1),
                   "bearing_deg": degrees(geo::bearing(w[0], w[1]))})
        })
        .collect();
    let mut out = obj(json!({"total_m": round(geo::length(&path), 1), "legs": legs}));
    if closed {
        out.insert("area_m2".into(), json!(round(area, 0)));
    }
    Ok(Value::Object(out))
}

const CONDITIONS: [&str; 10] = [
    "airborne",
    "landed",
    "armed",
    "disarmed",
    "altitude",
    "arrived",
    "stopped",
    "mode",
    "mission_item",
    "mission_done",
];

/// What `wait_for` waits for, checked once before it starts.
enum Until {
    Airborne,
    Landed,
    Armed(bool),
    Altitude(f64),
    Arrived(Place, f64),
    Stopped,
    Mode(String),
    MissionItem(i64),
    MissionDone,
}

impl Until {
    fn of(craft: &mut Craft, args: &Map) -> Result<Until, ToolError> {
        Ok(match req_str(args, "until")?.as_str() {
            "airborne" => Until::Airborne,
            "landed" => Until::Landed,
            "armed" => Until::Armed(true),
            "disarmed" => Until::Armed(false),
            "altitude" => Until::Altitude(num(args, "altitude", None)?),
            "arrived" => {
                let at = place(craft, &get(args, "place"), "place")?;
                Until::Arrived(at, within(args, "radius_m", Some(5.0), 0.5, 1000.0)?)
            }
            "stopped" => Until::Stopped,
            "mode" => Until::Mode(req_str(args, "mode")?.to_uppercase()),
            "mission_item" => Until::MissionItem(within(args, "item", None, 0.0, 10_000.0)? as i64),
            "mission_done" => Until::MissionDone,
            _ => return Err(err(format!("until must be one of {}", CONDITIONS.join(", ")))),
        })
    }

    fn met(&self, s: &Value) -> bool {
        let n = |k: &str| s.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
        let i = |k: &str| s.get(k).and_then(Value::as_i64).unwrap_or(-1);
        let armed = s.get("armed") == Some(&json!(true));
        match self {
            Until::Airborne => flying(s),
            Until::Landed => !flying(s) && (i("landed_state") == 1 || !armed),
            Until::Armed(want) => armed == *want,
            Until::Altitude(m) => (n("altitude_agl") - m).abs() <= 1.0,
            Until::Arrived(at, r) => here(s).is_some_and(|p| geo::distance(p, *at) <= *r),
            Until::Stopped => n("groundspeed").abs() < 0.5 && n("vspeed").abs() < 0.3,
            Until::Mode(m) => ["mode", "mode_label"].iter().any(|k| {
                s.get(*k)
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.to_uppercase() == *m)
            }),
            Until::MissionItem(item) => i("mission_seq") >= *item,
            Until::MissionDone => {
                s.get("mission_state") == Some(&json!("complete"))
                    || (i("mission_total") > 0 && i("mission_reached") >= i("mission_total") - 1)
            }
        }
    }
}

pub(super) fn here(s: &Value) -> Option<Place> {
    let p = s.get("position")?;
    Some([p.get(1)?.as_f64()?, p.get(0)?.as_f64()?])
}

fn wait_for(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let until = Until::of(&mut craft, args)?;
    let limit = within(args, "timeout_s", Some(20.0), 1.0, WAIT_MAX_S)?;
    let started = std::time::Instant::now();
    loop {
        let s = craft.fresh()?.clone();
        let reached = until.met(&s);
        let waited = started.elapsed().as_secs_f64();
        if reached || waited >= limit {
            let mut out = brief(&s);
            if let (Until::Arrived(at, _), Some(p)) = (&until, here(&s)) {
                out.insert("distance_m".into(), json!(round(geo::distance(p, *at), 1)));
            }
            out.insert("reached".into(), reached.into());
            out.insert("waited_s".into(), json!(round(waited, 1)));
            return Ok(Value::Object(out));
        }
        if svc.pause(WAIT_POLL) {
            return Err(err("Corvus GCS is closing."));
        }
    }
}

// -------------------------------------------------------------- patterns

const PATTERNS: [&str; 4] = ["grid", "spiral", "circle", "line"];

/// A pattern as a plan, with what is worth knowing about it.
pub(super) struct Pattern {
    pub(super) plan: Map,
    pub(super) path: Vec<Place>,
    pub(super) area: Vec<Place>,
    pub(super) length: f64,
    pub(super) speed: Option<f64>,
}

pub(super) fn pattern(svc: &McpService, args: &Map) -> Result<Pattern, ToolError> {
    let mut craft = Craft::of(svc, args);
    let kind = req_str(args, "pattern")?;
    let ground = craft.ground();
    let alt = if ground {
        0.0
    } else {
        within(args, "altitude", None, 1.0, 1000.0)?
    };
    let spacing = |d: Option<f64>| within(args, "spacing_m", d, 1.0, 10_000.0);
    let radius = || within(args, "radius_m", None, 1.0, 50_000.0);
    let angle = within(args, "angle_deg", Some(0.0), -360.0, 360.0)?;
    let too_many = || {
        err(format!(
            "This takes more than {PATTERN_POINTS} points. Make spacing_m larger or the area smaller."
        ))
    };
    let mut area = Vec::new();
    let path = match kind.as_str() {
        "grid" => {
            area = places(&mut craft, args.get("points"), "points")?;
            if area.len() < 3 {
                return Err(err("a grid needs the corners of its area as points, at least three"));
            }
            geo::grid(&area, spacing(None)?, angle, PATTERN_POINTS).ok_or_else(too_many)?
        }
        "spiral" => {
            let c = place(&mut craft, &get(args, "center"), "center")?;
            geo::spiral(c, radius()?, spacing(None)?, angle, PATTERN_POINTS).ok_or_else(too_many)?
        }
        "circle" => {
            let c = place(&mut craft, &get(args, "center"), "center")?;
            let mut ring = geo::ring(c, radius()?, ORBIT_POINTS, angle, true);
            ring.push(ring[0]);
            ring
        }
        "line" => places(&mut craft, args.get("points"), "points")?,
        _ => return Err(err(format!("pattern must be one of {}", PATTERNS.join(", ")))),
    };
    if path.len() > PATTERN_POINTS {
        return Err(too_many());
    }
    let last = match path.last() {
        Some(p) => *p,
        None => return Err(err("the pattern has no points")),
    };
    let speed = match opt(args, "speed")? {
        Some(_) => Some(within(args, "speed", None, 0.5, 100.0)?),
        None => None,
    };
    let mut items = Vec::new();
    if !ground && args.get("takeoff") != Some(&json!(false)) && !craft.airborne().unwrap_or(false) {
        let at = craft
            .spot("position")
            .or_else(|_| craft.spot("home"))
            .unwrap_or(path[0]);
        items.push(json!({"type": "takeoff", "lat": at[0], "lon": at[1], "alt": alt}));
    }
    items.extend(
        path.iter()
            .map(|p| json!({"type": "waypoint", "lat": p[0], "lon": p[1], "alt": alt})),
    );
    match args.get("end").and_then(Value::as_str).unwrap_or("rtl") {
        "rtl" => items.push(json!({"type": "rtl"})),
        "land" if !ground => items.push(json!({"type": "land", "lat": last[0], "lon": last[1]})),
        "land" | "hold" => {}
        _ => return Err(err("end must be rtl, land or hold")),
    }
    let name = match args.get("name").and_then(Value::as_str).map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => format!("AI {kind}"),
    };
    let mut plan = obj(json!({"name": name, "items": items}));
    if let Some(s) = speed {
        plan.insert("speed".into(), json!(s));
    }
    let length = geo::length(&path);
    Ok(Pattern {
        plan,
        path,
        area,
        length,
        speed,
    })
}

/// Shows a pattern on the map: its area, its path with the direction, its start.
/// False when the operator does not want anything drawn.
fn preview(svc: &McpService, p: &Pattern) -> bool {
    if super::settings::level(&svc.settings(), "map") == "off" {
        return false;
    }
    let mut shapes = Vec::new();
    if p.area.len() >= 3 {
        let corners: Vec<Value> = p.area.iter().map(|q| wire(*q)).collect();
        shapes.push(
            json!({"kind": "polygon", "id": "pattern-area", "points": corners, "color": "white",
                           "fill_opacity": 0.08, "dashed": true}),
        );
    }
    if p.path.len() >= 2 {
        let path: Vec<Value> = p.path.iter().map(|q| wire(*q)).collect();
        shapes.push(json!({"kind": "arrow", "id": "pattern-path", "points": path, "color": "cyan"}));
    }
    shapes.push(
        json!({"kind": "marker", "id": "pattern-start", "lat": p.path[0][0], "lon": p.path[0][1],
                       "icon": "flag", "text": "Start", "color": "lime"}),
    );
    svc.api("POST", "/api/map/drawings", Some(obj(json!({"shapes": shapes}))), None)
        .is_ok()
}

pub(super) fn facts(p: &Pattern) -> Map {
    let mut out = obj(json!({"points": p.path.len(), "length_m": round(p.length, 0)}));
    if let Some(s) = p.speed {
        out.insert("flight_time_s".into(), json!(round(p.length / s, 0)));
    }
    out
}

fn plan_pattern(svc: &McpService, args: &Map) -> ToolResult {
    let p = pattern(svc, args)?;
    let mut out = facts(&p);
    out.insert("drawn".into(), preview(svc, &p).into());
    out.insert("next".into(), "fly_pattern with the same arguments uploads it.".into());
    Ok(Value::Object(out))
}

fn fly_pattern(svc: &McpService, args: &Map) -> ToolResult {
    let p = pattern(svc, args)?;
    let start = args.get("start") == Some(&json!(true));
    let reply = Craft::of(svc, args).post("/api/mission/upload", json!({"plan": p.plan, "start": start}))?;
    preview(svc, &p);
    let mut out = facts(&p);
    for key in ["items", "started"] {
        out.insert(key.into(), reply.get(key).cloned().unwrap_or(Value::Null));
    }
    Ok(Value::Object(out))
}

// ------------------------------------------------------------------- map

/// What a shape that does not name its kind is.
fn likely_kind(raw: &Map) -> Option<&'static str> {
    let has = |k: &str| raw.get(k).is_some_and(|v| !v.is_null());
    if has("points") {
        return Some("line");
    }
    Some(match (has("radius_m"), has("from_deg"), has("icon"), has("text")) {
        (true, true, ..) => "sector",
        (true, false, ..) => "circle",
        (false, _, false, true) => "text",
        _ => "marker",
    })
}

fn draw_on_map(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let Some(list) = args.get("shapes").and_then(Value::as_array).filter(|l| !l.is_empty()) else {
        return Err(err("shapes must be a non-empty list"));
    };
    if list.len() > crate::drawings::MAX_SHAPES {
        return Err(err(format!("at most {} shapes", crate::drawings::MAX_SHAPES)));
    }
    let mut shapes = Vec::new();
    for (i, raw) in list.iter().enumerate() {
        let Some(raw) = raw.as_object() else {
            return Err(err(format!("shape {i} must be an object")));
        };
        let mut shape = raw.clone();
        if !raw.contains_key("kind") {
            if let Some(kind) = likely_kind(raw) {
                shape.insert("kind".into(), kind.into());
            }
        }
        if raw.contains_key("points") {
            let points = places(&mut craft, raw.get("points"), &format!("shape {i} points"))?;
            shape.insert("points".into(), points.into_iter().map(wire).collect());
        } else {
            let at = place(&mut craft, &Value::Object(raw.clone()), &format!("shape {i}"))?;
            shape.insert("lat".into(), json!(at[0]));
            shape.insert("lon".into(), json!(at[1]));
        }
        shapes.push(Value::Object(shape));
    }
    let clear = args.get("clear") == Some(&json!(true));
    svc.api(
        "POST",
        "/api/map/drawings",
        Some(obj(json!({"shapes": shapes, "clear": clear}))),
        None,
    )
}

fn remove_drawings(svc: &McpService, args: &Map) -> ToolResult {
    svc.api(
        "POST",
        "/api/map/drawings/remove",
        Some(pick(args, &["ids", "all"])),
        None,
    )
}

fn show_on_map(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let mut body = pick(args, &["ids", "zoom", "open"]);
    let query = match args.get("query") {
        None | Some(Value::Null) => None,
        Some(_) => Some(search_place(svc, args)?["results"][0].clone()),
    };
    let points = match (args.get("points"), &query) {
        (_, Some(found)) => {
            let at = |k: &str| found.get(k).and_then(Value::as_f64);
            match (at("lat"), at("lon").or_else(|| at("lng"))) {
                (Some(lat), Some(lon)) => vec![[lat, lon]],
                _ => return Err(err("No place of that name was found.")),
            }
        }
        (Some(p), _) => places(&mut craft, Some(p), "points")?,
        (None, _) if !body.contains_key("ids") => vec![craft.spot("position")?],
        (None, _) => Vec::new(),
    };
    body.insert("points".into(), points.into_iter().map(wire).collect());
    svc.api("POST", "/api/map/show", Some(body), None)?;
    Ok(match query {
        Some(found) => json!({"ok": true, "shown": found}),
        None => json!({"ok": true}),
    })
}

fn open_page(svc: &McpService, args: &Map) -> ToolResult {
    let page = req_str(args, "page")?;
    if !svc.window_open() {
        return Err(err("No Corvus window is open."));
    }
    svc.api("POST", "/api/window/open", Some(obj(json!({ "page": page }))), None)
}

fn search_place(svc: &McpService, args: &Map) -> ToolResult {
    let query = req_str(args, "query")?;
    if query.chars().count() > 120 {
        return Err(err("query must be at most 120 characters"));
    }
    let reply = svc.api("GET", &format!("/api/geocode?q={}&limit=5", quote(&query)), None, None)?;
    Ok(json!({"results": reply.get("results").cloned().unwrap_or(json!([]))}))
}

fn notify_operator(svc: &McpService, args: &Map) -> ToolResult {
    let text: String = req_str(args, "text")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect();
    let level = match args.get("level").and_then(Value::as_str).unwrap_or("info") {
        l @ ("info" | "warning" | "critical") => l,
        _ => return Err(err("level must be info, warning or critical")),
    };
    svc.say(&text, level);
    Ok(json!({"ok": true}))
}

// ---------------------------------------------------------------- flying

fn move_by(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let up = opt(args, "up_m")?.unwrap_or(0.0);
    let offsets = pick(args, &["north_m", "east_m", "forward_m", "right_m"]);
    if up == 0.0 && !moves(&offsets) {
        return Err(err("Say how far: north_m, east_m, forward_m, right_m or up_m."));
    }
    craft.must_fly()?;
    let from = craft.spot("position")?;
    let mut offsets = offsets;
    offsets.insert("from".into(), "vehicle".into());
    let to = place(&mut craft, &Value::Object(offsets), "the target")?;
    let distance = geo::distance(from, to);
    if distance > MOVE_MAX_M {
        return Err(err(format!(
            "One move goes at most {MOVE_MAX_M} m. Use fly_to for a farther point."
        )));
    }
    let alt = craft.number("altitude_agl")? + up;
    craft.go(&[to], alt)?;
    Ok(json!({"ok": true, "lat": to[0], "lon": to[1], "altitude": round(alt, 1), "distance_m": round(distance, 1)}))
}

fn change_altitude(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let alt = num(args, "altitude", None)?;
    if craft.ground() {
        return Err(err("This vehicle drives or sails, it has no height to change."));
    }
    craft.must_fly()?;
    let at = craft.spot("position")?;
    craft.go(&[at], alt)?;
    Ok(json!({"ok": true, "altitude": alt}))
}

fn orbit(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let center = place(&mut craft, &get(args, "center"), "center")?;
    let radius = within(args, "radius_m", None, 2.0, 2_000.0)?;
    let turns = within(args, "turns", Some(1.0), 0.0, ORBIT_TURNS_MAX)?;
    let clockwise = args.get("clockwise") != Some(&json!(false));
    craft.must_fly()?;
    let alt = match opt(args, "altitude")? {
        Some(a) => a,
        None => craft.number("altitude_agl")?,
    };
    if !craft.ground() && craft.takes("DO_ORBIT") {
        let speed = match opt(args, "speed")? {
            Some(_) => json!(within(args, "speed", None, 0.5, 30.0)?),
            None => Value::Null,
        };
        let signed = if clockwise { radius } else { -radius };
        craft.post(
            "/api/mavlink/command",
            json!({"command": "DO_ORBIT", "params": [signed, speed, 0, turns * std::f64::consts::TAU],
                   "lat": center[0], "lon": center[1], "alt": alt, "frame": "relative"}),
        )?;
        return Ok(json!({"ok": true, "flown_as": "orbit", "note": "It circles until hold or another command."}));
    }
    if turns < 1.0 {
        return Err(err(
            "This vehicle circles as a route and cannot do it without end: give turns, 1 or more.",
        ));
    }
    let at = craft.spot("position")?;
    let start = geo::bearing(center, at);
    let one = geo::ring(center, radius, ORBIT_POINTS, start, clockwise);
    let count = (turns * ORBIT_POINTS as f64).round() as usize;
    let mut path: Vec<Place> = (0..=count).map(|i| one[i % ORBIT_POINTS]).collect();
    path.truncate(250);
    craft.go(&path, alt)?;
    Ok(json!({"ok": true, "flown_as": "route", "points": path.len(),
              "note": "The circle replaced the mission on board."}))
}

fn fly_drawing(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let id = req_str(args, "id")?;
    let list = svc.api("GET", "/api/map/drawings", None, None)?;
    let shape = list
        .get("shapes")
        .and_then(Value::as_array)
        .and_then(|l| l.iter().find(|s| s.get("id") == Some(&json!(id))))
        .and_then(Value::as_object)
        .ok_or_else(|| err(format!("There is no drawing called {id}.")))?;
    let mut path = crate::drawings::outline(shape);
    match shape.get("kind").and_then(Value::as_str) {
        Some("sector") => {
            return Err(err(
                "A sector is not a route. Use a line, arrow, polygon, circle, marker or text.",
            ))
        }
        Some("polygon" | "circle") => path.extend(path.first().copied()),
        _ => {}
    }
    if path.len() > 250 {
        return Err(err(format!(
            "This drawing has {} points, at most 250 can be flown.",
            path.len()
        )));
    }
    if args.get("reverse") == Some(&json!(true)) {
        path.reverse();
    }
    let alt = if craft.ground() {
        0.0
    } else {
        num(args, "altitude", None)?
    };
    craft.go(&path, alt)?;
    Ok(json!({"ok": true, "points": path.len(), "length_m": round(geo::length(&path), 0)}))
}

fn set_speed(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let speed = within(args, "speed", None, 0.5, 60.0)?;
    // A plane is flown by its airspeed, everything else over ground.
    let kind = if craft.class_is("plane") { 0 } else { 1 };
    craft.command("DO_CHANGE_SPEED", json!([kind, speed, -1, 0, 0, 0, 0]))
}

fn turn(craft: &mut Craft, heading: f64) -> ToolResult {
    let heading = heading.rem_euclid(360.0);
    if craft.class_is("plane") {
        return Err(err("A plane cannot turn on the spot. Fly it to a point instead."));
    }
    if craft.ground() {
        return Err(err("This vehicle turns by driving. Send it to a point instead."));
    }
    craft.must_fly()?;
    if craft.takes("CONDITION_YAW") {
        craft.command("CONDITION_YAW", json!([heading, 0, 0, 0, 0, 0, 0]))?;
    } else {
        let at = craft.spot("position")?;
        let alt = craft.number("altitude_agl")?;
        let wrapped = if heading > 180.0 { heading - 360.0 } else { heading };
        craft.post(
            "/api/mavlink/command",
            json!({"command": "DO_REPOSITION", "params": [-1, 1, 0, wrapped.to_radians()],
                   "lat": at[0], "lon": at[1], "alt": alt, "frame": "relative"}),
        )?;
    }
    Ok(json!({"ok": true, "heading": round(heading, 1)}))
}

fn set_heading(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let mut heading = num(args, "heading", None)?;
    if args.get("relative") == Some(&json!(true)) {
        heading += craft.number("heading")?;
    }
    turn(&mut craft, heading)
}

fn face_place(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let to = place(&mut craft, &get(args, "place"), "place")?;
    let from = craft.spot("position")?;
    if geo::distance(from, to) < 1.0 {
        return Err(err("The vehicle is at that place already."));
    }
    turn(&mut craft, geo::bearing(from, to))
}

fn resume_mission(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    craft.must_fly()?;
    let mode = craft
        .caps()?
        .get("mission_mode")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if mode.is_empty() {
        return Err(err("This vehicle has no mission mode."));
    }
    craft.post("/api/mavlink/mode", json!({"mode": mode}))
}

fn set_mission_item(svc: &McpService, args: &Map) -> ToolResult {
    let item = within(args, "item", None, 0.0, 10_000.0)?.trunc();
    Craft::of(svc, args).command("DO_SET_MISSION_CURRENT", json!([item, 0, 0, 0, 0, 0, 0]))
}

// -------------------------------------------------------------- geofence

fn set_geofence(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let corners = places(&mut craft, args.get("points"), "points")?;
    let polygon: Vec<Value> = corners.iter().map(|p| json!([p[1], p[0]])).collect();
    craft.post("/api/geofence/save", json!({"polygon": polygon}))?;
    match craft.post("/api/geofence/upload", json!({})) {
        Ok(_) => Ok(json!({"ok": true, "corners": corners.len(), "area_m2": round(geo::area(&corners), 0)})),
        Err(ToolError(why)) => Err(err(format!("Saved in Corvus, but not on the vehicle: {why}"))),
    }
}

// ---------------------------------------------------------------- camera

fn look_at(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let at = place(&mut craft, &get(args, "place"), "place")?;
    let alt = within(args, "altitude", Some(0.0), -1000.0, 5000.0)?;
    craft.post(
        "/api/mavlink/command",
        json!({"command": "DO_SET_ROI_LOCATION", "params": [0], "lat": at[0], "lon": at[1], "alt": alt,
               "frame": "relative"}),
    )
}

fn stop_looking(svc: &McpService, args: &Map) -> ToolResult {
    Craft::of(svc, args).command("DO_SET_ROI_NONE", json!([0, 0, 0, 0, 0, 0, 0]))
}

fn point_gimbal(svc: &McpService, args: &Map) -> ToolResult {
    let pitch = within(args, "pitch_deg", Some(0.0), -90.0, 45.0)?;
    let yaw = within(args, "yaw_deg", Some(0.0), -180.0, 180.0)?;
    // Level with the horizon (roll and pitch lock); yaw lock counts from north.
    let flags = match args.get("yaw_frame").and_then(Value::as_str).unwrap_or("vehicle") {
        "vehicle" => 4 + 8,
        "north" => 4 + 8 + 16,
        _ => return Err(err("yaw_frame must be vehicle or north")),
    };
    Craft::of(svc, args).command(
        "DO_GIMBAL_MANAGER_PITCHYAW",
        json!([pitch, yaw, null, null, flags, 0, 0]),
    )
}

fn take_photo(svc: &McpService, args: &Map) -> ToolResult {
    Craft::of(svc, args).command("DO_DIGICAM_CONTROL", json!([0, 0, 0, 0, 1, 0, 0]))
}

fn set_photo_distance(svc: &McpService, args: &Map) -> ToolResult {
    let d = within(args, "distance_m", None, 0.0, 10_000.0)?;
    Craft::of(svc, args).command("DO_SET_CAM_TRIGG_DIST", json!([d, 0, 0, 0, 0, 0, 0]))
}

// ------------------------------------------------------------- the list

/// The properties of a place, for a schema.
pub(super) fn place_props() -> Map {
    obj(json!({
        "lat": {"type": "number"}, "lon": {"type": "number"},
        "from": {"type": "string", "enum": ["vehicle", "home"]},
        "north_m": {"type": "number"}, "east_m": {"type": "number"},
        "forward_m": {"type": "number"}, "right_m": {"type": "number"},
        "bearing_deg": {"type": "number"}, "distance_m": {"type": "number"},
    }))
}

pub(super) fn place_schema(what: &str) -> Value {
    json!({"type": "object", "properties": place_props(),
           "description": format!("{what}: lat and lon, or from vehicle or home with an offset")})
}

pub(super) fn places_schema(what: &str) -> Value {
    json!({"type": "array", "items": place_schema("A place"), "description": what})
}

pub(super) fn pattern_props() -> Map {
    obj(json!({
        "pattern": {"type": "string", "enum": PATTERNS, "description": "grid: back and forth over an area. spiral: an expanding square search. circle: once around. line: along the points"},
        "points": places_schema("The corners of a grid, or the points of a line"),
        "center": place_schema("The middle of a spiral or circle"),
        "radius_m": {"type": "number", "description": "Of a spiral or circle"},
        "spacing_m": {"type": "number", "description": "Between the lines of a grid or spiral"},
        "angle_deg": {"type": "number", "description": "Direction of the lines of a grid, 0 is north"},
        "altitude": {"type": "number", "description": "Metres above home"},
        "speed": {"type": "number", "description": "m/s"},
        "takeoff": {"type": "boolean", "default": true, "description": "Begin with a takeoff when it is on the ground"},
        "end": {"type": "string", "enum": ["rtl", "land", "hold"], "default": "rtl"},
        "name": {"type": "string"},
    }))
}

pub(super) fn say(a: &Map, key: &str) -> String {
    fmt(a.get(key)).chars().take(80).collect()
}

/// "48.1, 11.6" or "40 m north of the vehicle", for the question.
pub(super) fn where_text(v: Option<&Value>) -> String {
    let Some(v) = v.and_then(Value::as_object) else {
        return "a place".into();
    };
    let base = match (v.get("lat"), v.get("lon"), v.get("from").and_then(Value::as_str)) {
        (Some(lat), Some(lon), _) => format!("{}, {}", fmt(Some(lat)), fmt(Some(lon))),
        (_, _, Some("home")) => "home".into(),
        _ => "the vehicle".to_string(),
    };
    let parts = offsets_text(v);
    if parts.is_empty() {
        base
    } else {
        format!("{parts} from {base}")
    }
}

fn offsets_text(v: &Map) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (key, plus, minus) in [
        ("north_m", "north", "south"),
        ("east_m", "east", "west"),
        ("forward_m", "forward", "back"),
        ("right_m", "right", "left"),
        ("up_m", "up", "down"),
    ] {
        if let Some(n) = v.get(key).and_then(Value::as_f64).filter(|n| *n != 0.0) {
            parts.push(format!(
                "{} m {}",
                round(n.abs(), 1),
                if n > 0.0 { plus } else { minus }
            ));
        }
    }
    if let (Some(b), Some(d)) = (v.get("bearing_deg"), v.get("distance_m")) {
        parts.push(format!("{} m towards {}\u{b0}", fmt(Some(d)), fmt(Some(b))));
    }
    parts.join(", ")
}

/// Every tool here, in the order of [`NAMES`].
pub fn tools() -> Vec<Tool> {
    let none = Map::new;
    let shape = {
        let mut p = place_props();
        for (k, v) in obj(json!({
            "kind": {"type": "string", "enum": ["line", "arrow", "polygon", "circle", "sector", "marker", "text"]},
            "id": {"type": "string", "description": "Your name for it; drawing it again moves it"},
            "points": places_schema("Of a line, arrow (it points at the last one) or polygon"),
            "radius_m": {"type": "number", "description": "Of a circle or sector"},
            "from_deg": {"type": "number", "description": "A sector begins at this bearing"},
            "to_deg": {"type": "number", "description": "and goes clockwise to this one"},
            "text": {"type": "string", "description": "A label, or the text itself"},
            "icon": {"type": "string", "description": "Of a marker: pin, flag, target, hazard, fire, person, vehicle, building, tree, camera, antenna, radar, pad, home, drone, plane, boat, medical, power, water, gate"},
            "color": {"type": "string", "description": "cyan, lime, green, magenta, blue, white, red, orange, yellow, black, gray or #RRGGBB"},
            "width": {"type": "number"}, "dashed": {"type": "boolean"},
            "fill_opacity": {"type": "number", "minimum": 0, "maximum": 1}, "size": {"type": "number"},
        })) {
            p.insert(k, v);
        }
        json!({"type": "object", "properties": p, "required": ["kind"]})
    };
    let alt = json!({"type": "number", "description": "Metres above home"});
    vec![
        tool("resolve_place", "telemetry", "Work out a place",
            "Coordinates of a place given relative to the vehicle, home or another point, with its distance and bearing from both.",
            obj(json!({"place": place_schema("The place")})), &["place"],
            Run::Built(resolve_place), |_| Some("work out a place".into()), true, true),
        tool("measure", "telemetry", "Measure",
            "Distances and bearings along points, the total, and with closed the area inside them.",
            obj(json!({"points": places_schema("In order"), "closed": {"type": "boolean", "default": false}})), &["points"],
            Run::Built(measure), |_| Some("measure".into()), true, true),
        tool("wait_for", "telemetry", "Wait",
            "Wait until something has happened, at most timeout_s seconds (30 is the most), and answer whether it did with the state then. Use it after takeoff, fly_to, land or a mission, and call it again to wait longer.",
            obj(json!({
                "until": {"type": "string", "enum": CONDITIONS},
                "altitude": alt.clone(), "place": place_schema("For arrived"),
                "radius_m": {"type": "number", "default": 5}, "mode": {"type": "string"},
                "item": {"type": "integer"}, "timeout_s": {"type": "number", "minimum": 1, "maximum": WAIT_MAX_S, "default": 20},
            })), &["until"], Run::Built(wait_for), |a| Some(format!("wait for {}", say(a, "until"))), true, true),
        tool("plan_pattern", "telemetry", "Preview a pattern",
            "Work out a search or survey pattern and draw it on the operator's map, without sending anything to the vehicle.",
            pattern_props(), &["pattern", "altitude"], Run::Built(plan_pattern),
            |a| Some(format!("preview a {} pattern", say(a, "pattern"))), true, true),
        tool("get_geofence", "telemetry", "Geofence",
            "The area of the geofence as [lon, lat] corners, and whether the vehicle holds it.",
            none(), &[], Run::Get("/api/geofence", None), |_| Some("read the geofence".into()), true, true),
        tool("list_saved_missions", "telemetry", "Saved missions",
            "The missions saved in Corvus.",
            none(), &[], Run::Get("/api/mission/plans", Some("plans")), |_| Some("list the saved missions".into()), false, true),
        tool("get_saved_mission", "telemetry", "Read a saved mission",
            "One saved mission as a plan, for upload_mission.",
            obj(json!({"name": {"type": "string"}})), &["name"],
            Run::Post("/api/mission/plans/load", Some(|a| pick(a, &["name"]))),
            |a| Some(format!("read the mission {}", say(a, "name"))), false, true),
        tool("draw_on_map", "map", "Draw on the map",
            "Draw on the operator's Home map, flat and 3D: lines, arrows, polygons, circles, sectors (a field of view), markers and text. Nothing of it reaches the vehicle. A line, arrow or polygon has points; the others stand at lat and lon, or at a place relative to the vehicle or home. Answers the ids.",
            obj(json!({
                "shapes": {"type": "array", "minItems": 1, "items": shape},
                "clear": {"type": "boolean", "default": false, "description": "Take everything drawn before away"},
            })), &["shapes"], Run::Built(draw_on_map),
            |a| Some(format!("draw on the map: {}", match a.get("shapes") {
                Some(Value::Array(l)) if l.len() == 1 => fmt(l[0].get("kind")).chars().take(20).collect::<String>(),
                Some(Value::Array(l)) => format!("{} shapes", l.len()),
                _ => "nothing".into(),
            })), true, false),
        tool("list_drawings", "map", "Drawings",
            "What is drawn on the map now, with the ids.",
            none(), &[], Run::Get("/api/map/drawings", None), |_| Some("list the drawings".into()), false, true),
        tool("remove_drawings", "map", "Remove drawings",
            "Take drawings off the map: the named ids, or all of them.",
            obj(json!({"ids": {"type": "array", "items": {"type": "string"}}, "all": {"type": "boolean"}})), &[],
            Run::Built(remove_drawings), |_| Some("remove drawings".into()), false, false),
        tool("show_on_map", "map", "Show on the map",
            "Move the operator's map to other places so that these, or drawings, are in view: anywhere on earth, by coordinates, relative to the vehicle or home, or by name with query. Without any, the vehicle.",
            obj(json!({
                "points": places_schema("Places to frame"), "ids": {"type": "array", "items": {"type": "string"}},
                "query": {"type": "string", "description": "A place by its name or address, from OpenStreetMap. Needs internet"},
                "zoom": {"type": "number", "minimum": 1, "maximum": 20, "description": "For one place: 4 a country, 10 a city, 16 a field"},
                "open": {"type": "boolean", "default": false, "description": "Bring the map to the front when another page is open"},
            })), &[],
            Run::Built(show_on_map), |a| Some(match a.get("query").and_then(Value::as_str) {
                Some(q) => format!("move the map to {}", q.chars().take(60).collect::<String>()),
                None => "move the map".into(),
            }), true, true),
        tool("open_page", "pages", "Open a page",
            "Bring a page of the Corvus window to the front: home, the mission planner, setup and its pages, analysis, logs, the settings and their tabs, the tabs of the side panel, the notifications, the flat or the 3D map, following the vehicle on it, or plugin:<id> for a plugin's view. It only opens it. Corvus stays where it is while a page with unfinished work is open and tells the operator.",
            obj(json!({"page": {"type": "string", "description": format!("plugin:<id> or one of {}", crate::drawings::PAGES.join(", "))}})), &["page"],
            Run::Built(open_page), |a| Some(format!("open the page {}", say(a, "page"))), false, false),
        tool("search_place", "map", "Search a place",
            "Coordinates of a place by its name or address, from OpenStreetMap. Needs internet.",
            obj(json!({"query": {"type": "string"}})), &["query"],
            Run::Built(search_place), |a| Some(format!("search for {}", say(a, "query"))), false, true),
        tool("notify_operator", "map", "Message",
            "Put a line on the operator's notification board. For something they must not miss, not for conversation.",
            obj(json!({"text": {"type": "string", "maxLength": 300}, "level": {"type": "string", "enum": ["info", "warning", "critical"], "default": "info"}})), &["text"],
            Run::Built(notify_operator), |a| Some(format!("say: {}", say(a, "text"))), false, false),
        tool("fly_pattern", "mission", "Fly a pattern",
            "Work out a pattern as plan_pattern does and upload it as the mission. With start true it is started too, which arms the vehicle.",
            { let mut p = pattern_props(); p.insert("start".into(), json!({"type": "boolean", "default": false})); p },
            &["pattern", "altitude"], Run::Built(fly_pattern),
            |a| Some(format!("{} a {} pattern at {} m", if a.get("start") == Some(&json!(true)) { "upload and start" } else { "upload" }, say(a, "pattern"), say(a, "altitude"))), true, false),
        tool("save_mission", "mission", "Save a mission",
            "Save the plan in the mission planner under a name, or another plan, replacing one of that name.",
            obj(json!({"name": {"type": "string"}, "plan": {"type": "object", "description": "As upload_mission takes it; without it the plan in the mission planner"}})), &["name"],
            Run::Built(super::simple::save_mission),
            |a| Some(format!("save the mission {}", say(a, "name"))), false, false),
        tool("resume_mission", "mission", "Resume the mission",
            "Go on with the mission in the air, after a hold. hold is what pauses it.",
            none(), &[], Run::Built(resume_mission), |_| Some("resume the mission".into()), true, false),
        tool("set_mission_item", "mission", "Jump in the mission",
            "Make this item the current one, counted as mission_seq in get_vehicle_state and current_item in get_telemetry.",
            obj(json!({"item": {"type": "integer", "minimum": 0}})), &["item"],
            Run::Built(set_mission_item), |a| Some(format!("jump to mission item {}", say(a, "item"))), true, false),
        tool("set_speed", "flight", "Speed",
            "How fast it flies from now on in guided flight and missions, m/s (airspeed on a plane).",
            obj(json!({"speed": {"type": "number", "minimum": 0.5, "maximum": 60}})), &["speed"],
            Run::Built(set_speed), |a| Some(format!("fly at {} m/s", say(a, "speed"))), true, false),
        tool("set_heading", "flight", "Turn",
            "Turn a multicopter on the spot to a heading in degrees, 0 is north; relative counts from where it points now. ArduPilot turns only in GUIDED.",
            obj(json!({"heading": {"type": "number"}, "relative": {"type": "boolean", "default": false}})), &["heading"],
            Run::Built(set_heading),
            |a| Some(format!("turn {}{}\u{b0}", if a.get("relative") == Some(&json!(true)) { "by " } else { "to " }, say(a, "heading"))), true, false),
        tool("move_by", "goto", "Move",
            "Fly a distance from where it is: north and east, or forward and right along its heading, and up (negative is down). Only in the air.",
            obj(json!({"north_m": {"type": "number"}, "east_m": {"type": "number"}, "forward_m": {"type": "number"},
                       "right_m": {"type": "number"}, "up_m": {"type": "number"}})), &[],
            Run::Built(move_by), |a| Some(format!("move {}", offsets_text(a))), true, false),
        tool("change_altitude", "goto", "Change height",
            "Climb or descend to a height above home where it is. Only in the air.",
            obj(json!({"altitude": alt.clone()})), &["altitude"],
            Run::Built(change_altitude), |a| Some(format!("go to {} m height", say(a, "altitude"))), true, false),
        tool("orbit", "goto", "Orbit",
            "Circle a place in the air. Where the autopilot has no orbit of its own, the circle is flown as a route that replaces the mission on board. turns 0 circles until hold, where the autopilot can.",
            obj(json!({"center": place_schema("The middle"), "radius_m": {"type": "number", "minimum": 2, "maximum": 2000},
                       "altitude": alt.clone(), "speed": {"type": "number"}, "turns": {"type": "number", "minimum": 0, "maximum": ORBIT_TURNS_MAX, "default": 1},
                       "clockwise": {"type": "boolean", "default": true}})), &["center", "radius_m"],
            Run::Built(orbit), |a| Some(format!("orbit {} at {} m radius", where_text(a.get("center")), say(a, "radius_m"))), true, false),
        tool("fly_drawing", "goto", "Fly along a drawing",
            "Fly along a drawing on the map: a line or arrow from end to end, a polygon or circle around, a marker as a point. Like fly_to with several points this replaces the mission on board, arms the vehicle and takes off.",
            obj(json!({"id": {"type": "string"}, "altitude": alt.clone(), "reverse": {"type": "boolean", "default": false}})), &["id", "altitude"],
            Run::Built(fly_drawing), |a| Some(format!("fly along the drawing {} at {} m", say(a, "id"), say(a, "altitude"))), true, false),
        tool("set_geofence", "geofence", "Set the geofence",
            "Replace the geofence by the area inside these corners, here and on the vehicle.",
            obj(json!({"points": places_schema("The corners, 3 to 64")})), &["points"],
            Run::Built(set_geofence),
            |a| Some(format!("set a geofence of {} corners", a.get("points").and_then(Value::as_array).map_or(0, Vec::len))), true, false),
        tool("clear_geofence", "geofence", "Clear the geofence",
            "Take the geofence off the vehicle.",
            none(), &[], Run::Post("/api/geofence/clear", None), |_| Some("take the geofence off the vehicle".into()), true, false),
        tool("look_at", "camera", "Look at a place",
            "Point the camera (or the nose, without a gimbal) at a place and keep it there. altitude is the height of the place above home.",
            obj(json!({"place": place_schema("Where to look"), "altitude": {"type": "number", "default": 0}})), &["place"],
            Run::Built(look_at), |a| Some(format!("look at {}", where_text(a.get("place")))), true, false),
        tool("stop_looking", "camera", "Stop looking",
            "Stop looking at the place.", none(), &[], Run::Built(stop_looking), |_| Some("stop looking at a place".into()), true, false),
        tool("point_gimbal", "camera", "Aim the gimbal",
            "Aim the gimbal: pitch_deg -90 is straight down, yaw_deg counts from the nose or from north. Some autopilots take it only from the station that controls the gimbal.",
            obj(json!({"pitch_deg": {"type": "number", "minimum": -90, "maximum": 45}, "yaw_deg": {"type": "number", "minimum": -180, "maximum": 180},
                       "yaw_frame": {"type": "string", "enum": ["vehicle", "north"], "default": "vehicle"}})), &[],
            Run::Built(point_gimbal), |_| Some("aim the gimbal".into()), true, false),
        tool("take_photo", "camera", "Photo", "Take one photo with the camera the autopilot triggers.",
            none(), &[], Run::Built(take_photo), |_| Some("take a photo".into()), true, false),
        tool("set_photo_distance", "camera", "Photos by distance",
            "Take a photo every so many metres flown. 0 stops it.",
            obj(json!({"distance_m": {"type": "number", "minimum": 0}})), &["distance_m"],
            Run::Built(set_photo_distance), |a| Some(format!("take a photo every {} m", say(a, "distance_m"))), true, false),
        tool("face_place", "flight", "Turn to a place",
            "Turn a multicopter on the spot so that it points at a place.",
            obj(json!({"place": place_schema("The place")})), &["place"],
            Run::Built(face_place), |a| Some(format!("turn to {}", where_text(a.get("place")))), true, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_and_the_names_agree_and_every_permission_exists() {
        let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        assert_eq!(names, NAMES);
        for t in tools() {
            assert!(super::super::settings::permission(t.permission).is_some(), "{}", t.name);
            for r in t.required {
                assert!(t.properties.contains_key(*r), "{} requires {r}", t.name);
            }
            assert!((t.summary)(&Map::new()).is_some(), "{}", t.name);
            assert!(
                !t.description.contains('\u{2014}') && !t.description.contains(" - "),
                "{}",
                t.name
            );
        }
    }

    #[test]
    fn the_question_says_where() {
        assert_eq!(where_text(Some(&json!({"lat": 48.1, "lon": 11.6}))), "48.1, 11.6");
        assert_eq!(
            where_text(Some(&json!({"from": "home", "north_m": -20}))),
            "20 m south from home"
        );
        assert_eq!(
            where_text(Some(&json!({"from": "vehicle", "bearing_deg": 90, "distance_m": 5}))),
            "5 m towards 90\u{b0} from the vehicle"
        );
        assert_eq!(
            offsets_text(&obj(json!({"forward_m": 3, "up_m": -2}))),
            "3 m forward, 2 m down"
        );
    }
}
