//! Tools a small model calls well: flat arguments, one job each. Telemetry by group, one shape
//! per drawing tool, and the plan of the operator's mission planner.
//!
//! They add nothing a vehicle can be told: a drawing is a drawing of `super::more`, a plan goes
//! through the planner's own checks, and uploading it is the route the planner's button uses.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::engine::{obj, McpService, ToolError};
use super::geo::{self, Place};
use super::more::{
    degrees, err, facts, flying, here, opt, pattern, pattern_props, place, place_props, place_schema, places,
    places_schema, round, say, where_text, wire, within, Craft, ToolResult,
};
use super::tools::{get, pick, tool, Run, Tool};
use crate::json::Map;
use crate::mission::{MISSION_MAX_ITEMS, PLAN_AIRCRAFT};

/// The names of the tools here, in the order a client lists them.
pub const NAMES: [&str; 13] = [
    "get_telemetry",
    "draw_line",
    "draw_area",
    "draw_circle",
    "draw_marker",
    "get_mission_plan",
    "open_mission_plan",
    "plan_mission",
    "plan_survey",
    "edit_mission_item",
    "remove_mission_item",
    "set_mission_plan",
    "upload_mission_plan",
];

// ------------------------------------------------------------- arguments

/// Arguments as the schema means them, for what models often write instead: a number, a truth
/// value, a list or an object as text, one thing where a list of them is meant, null for a
/// value left out.
pub fn tidy(args: &Map, properties: &Map) -> Map {
    args.iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let v = properties.get(k).map_or_else(|| v.clone(), |schema| fit(v, schema));
            (k.clone(), v)
        })
        .collect()
}

fn fit(v: &Value, schema: &Value) -> Value {
    let wanted = schema.get("type").and_then(Value::as_str).unwrap_or("");
    let read = match (v, wanted) {
        (Value::String(s), "number") => s.trim().parse::<f64>().ok().filter(|n| n.is_finite()).map(|n| json!(n)),
        (Value::String(s), "integer") => s.trim().parse::<i64>().ok().map(|n| json!(n)),
        (Value::Number(n), "integer") => n
            .as_f64()
            .filter(|f| n.as_i64().is_none() && f.fract() == 0.0 && f.abs() < 1e9)
            .map(|f| json!(f as i64)),
        (Value::String(s), "boolean") => match s.trim().to_lowercase().as_str() {
            "true" | "yes" => Some(json!(true)),
            "false" | "no" => Some(json!(false)),
            _ => None,
        },
        (Value::String(s), "array" | "object") => serde_json::from_str::<Value>(s)
            .ok()
            .filter(|p| p.is_array() || p.is_object()),
        _ => None,
    };
    let v = read.unwrap_or_else(|| v.clone());
    match (v, wanted) {
        (Value::Array(list), "array") => {
            let each = schema.get("items").unwrap_or(&Value::Null);
            Value::Array(list.iter().map(|x| fit(x, each)).collect())
        }
        (one @ (Value::Object(_) | Value::String(_)), "array") => {
            Value::Array(vec![fit(&one, schema.get("items").unwrap_or(&Value::Null))])
        }
        (Value::Object(o), "object") => match schema.get("properties").and_then(Value::as_object) {
            Some(p) => Value::Object(tidy(&o, p)),
            None => Value::Object(o),
        },
        (v, _) => v,
    }
}

// ------------------------------------------------------------- telemetry

/// The groups of `get_telemetry`: what a value is called here, and the field of the state.
/// The unit is in the name, since a model reads nothing else.
const GROUPS: [(&str, &[(&str, &str)]); 10] = [
    (
        "overview",
        &[
            ("armed", "armed"),
            ("mode", "mode"),
            ("height_above_home_m", "altitude_agl"),
            ("groundspeed_m_s", "groundspeed"),
            ("heading_deg", "heading"),
            ("battery_percent", "battery_percent"),
            ("gps_fix", "gps_fix"),
            ("link_quality", "link_quality"),
        ],
    ),
    (
        "position",
        &[
            ("height_above_home_m", "altitude_agl"),
            ("altitude_amsl_m", "altitude_amsl"),
            ("heading_deg", "heading"),
            ("rangefinder_m", "rangefinder_distance"),
        ],
    ),
    (
        "attitude",
        &[
            ("roll_deg", "roll"),
            ("pitch_deg", "pitch"),
            ("yaw_deg", "yaw"),
            ("roll_rate_deg_s", "rollspeed"),
            ("pitch_rate_deg_s", "pitchspeed"),
            ("yaw_rate_deg_s", "yawspeed"),
        ],
    ),
    (
        "speed",
        &[
            ("groundspeed_m_s", "groundspeed"),
            ("airspeed_m_s", "airspeed"),
            ("climb_m_s", "vspeed"),
            ("north_m_s", "vx"),
            ("east_m_s", "vy"),
            ("down_m_s", "vz"),
        ],
    ),
    (
        "battery",
        &[
            ("battery_percent", "battery_percent"),
            ("voltage_v", "battery_voltage"),
            ("current_a", "battery_current"),
            ("cells", "battery_cells"),
            ("cell_voltage_v", "battery_cell_voltage"),
            ("consumed_mah", "battery_consumed_mah"),
            ("temperature_c", "battery_temperature"),
            ("remaining_s", "battery_endurance_est"),
        ],
    ),
    (
        "gps",
        &[
            ("gps_fix", "gps_fix"),
            ("satellites", "gps_satellites"),
            ("hdop", "gps_hdop"),
            ("horizontal_accuracy_m", "gps_h_acc"),
            ("vertical_accuracy_m", "gps_v_acc"),
            ("jamming", "gps_jamming"),
            ("spoofing", "gps_spoofing"),
            ("rtk", "rtk_state"),
        ],
    ),
    (
        "link",
        &[
            ("link_quality", "link_quality"),
            ("connection", "link_connection"),
            ("rc_live", "rc_live"),
            ("rc_rssi", "rc_rssi"),
            ("heartbeat_jitter_ms", "heartbeat_jitter_ms"),
        ],
    ),
    (
        "mission",
        &[
            ("mission_state", "mission_state"),
            ("current_item", "mission_seq"),
            ("reached_item", "mission_reached"),
            ("items", "mission_total"),
        ],
    ),
    (
        "health",
        &[
            ("ready_to_arm", "prearm_ok"),
            ("not_ready_because", "prearm_reasons"),
            ("warnings", "warnings"),
            ("vibration_x", "vibration_x"),
            ("vibration_y", "vibration_y"),
            ("vibration_z", "vibration_z"),
            ("kill_switch", "kill_switch"),
        ],
    ),
    (
        "vehicle",
        &[
            ("id", "vehicle_id"),
            ("type", "vehicle_type"),
            ("autopilot", "autopilot"),
            ("firmware", "px4_version"),
        ],
    ),
];
const WATCH_MAX_S: f64 = 20.0;
const WATCH_STEP: Duration = Duration::from_millis(200);

/// Where a state says "not known" with a number no measurement has.
const UNKNOWN: [(&str, f64); 9] = [
    ("battery_endurance_est", -1.0),
    ("gps_hdop", 99.0),
    ("gps_h_acc", -1.0),
    ("gps_v_acc", -1.0),
    ("rc_rssi", -1.0),
    ("mission_seq", -1.0),
    ("mission_reached", -1.0),
    ("mission_total", -1.0),
    ("battery_cells", 0.0),
];

fn reading(s: &Value, key: &str) -> Value {
    let v = s.get(key).cloned().unwrap_or(Value::Null);
    match v.as_f64() {
        Some(n) if UNKNOWN.iter().any(|(k, x)| *k == key && *x == n) => Value::Null,
        Some(n) if v.is_f64() => json!(round(n, 3)),
        _ if v == json!("") => Value::Null,
        _ => v,
    }
}

/// The values of these groups, and where the vehicle is when position is one of them.
fn readings(s: &Value, groups: &[&str], home: Option<Place>) -> Map {
    let mut out = Map::new();
    for (name, fields) in GROUPS.iter().filter(|(n, _)| groups.contains(n)) {
        for (called, key) in fields.iter() {
            out.insert((*called).into(), reading(s, key));
        }
        if *name == "overview" {
            out.insert("flying".into(), flying(s).into());
        }
        if matches!(*name, "overview" | "position") {
            if let Some(at) = here(s).filter(|p| *p != [0.0, 0.0]) {
                out.insert("lat".into(), json!(round(at[0], 7)));
                out.insert("lon".into(), json!(round(at[1], 7)));
                if let Some(home) = home {
                    out.insert("distance_from_home_m".into(), json!(round(geo::distance(home, at), 1)));
                    out.insert("bearing_from_home_deg".into(), json!(degrees(geo::bearing(home, at))));
                }
            }
        }
    }
    out
}

fn get_telemetry(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let names: Vec<&str> = GROUPS.iter().map(|(n, _)| *n).collect();
    let mut groups: Vec<&str> = Vec::new();
    for g in args.get("groups").and_then(Value::as_array).into_iter().flatten() {
        match g.as_str().map(str::trim) {
            Some("all") => groups = names.clone(),
            Some(g) => match names.iter().find(|n| **n == g) {
                Some(n) => groups.push(n),
                None => return Err(err(format!("groups may hold {} or all", names.join(", ")))),
            },
            None => return Err(err("groups must be a list of names")),
        }
    }
    if groups.is_empty() {
        groups.push("overview");
    }
    let seconds = within(args, "seconds", Some(0.0), 0.0, WATCH_MAX_S)?;
    let home = craft.spot("home").ok();
    let mut out = readings(craft.fresh()?, &groups, home);
    if seconds <= 0.0 {
        return Ok(Value::Object(out));
    }
    let mut seen: std::collections::BTreeMap<String, (f64, f64, f64, u32)> = Default::default();
    let started = Instant::now();
    loop {
        for (k, v) in &out {
            if let Some(n) = v.as_f64() {
                let e = seen.entry(k.clone()).or_insert((n, n, 0.0, 0));
                *e = (e.0.min(n), e.1.max(n), e.2 + n, e.3 + 1);
            }
        }
        if started.elapsed().as_secs_f64() >= seconds {
            break;
        }
        if svc.pause(WATCH_STEP) {
            return Err(err("Corvus GCS is closing."));
        }
        out = readings(craft.fresh()?, &groups, home);
    }
    let watched: Map = seen
        .into_iter()
        .map(|(k, (low, high, sum, n))| {
            (
                k,
                json!({"min": low, "max": high, "mean": round(sum / f64::from(n.max(1)), 3)}),
            )
        })
        .collect();
    out.insert("over_s".into(), json!(seconds));
    out.insert("watched".into(), Value::Object(watched));
    Ok(Value::Object(out))
}

// --------------------------------------------------------------- drawing

fn draw(svc: &McpService, args: &Map, kind: &str, round_one: bool) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let mut shape = pick(
        args,
        &[
            "id",
            "text",
            "color",
            "icon",
            "width",
            "dashed",
            "fill_opacity",
            "radius_m",
        ],
    );
    shape.insert("kind".into(), kind.into());
    let at: Vec<Place> = if round_one {
        let p = place(&mut craft, &Value::Object(args.clone()), "the place")?;
        shape.insert("lat".into(), json!(p[0]));
        shape.insert("lon".into(), json!(p[1]));
        vec![p]
    } else {
        let p = places(&mut craft, args.get("points"), "points")?;
        shape.insert("points".into(), p.iter().map(|x| wire(*x)).collect());
        p
    };
    let body = obj(json!({"shapes": [shape], "clear": false}));
    let reply = svc.api("POST", "/api/map/drawings", Some(body), None)?;
    let mut out = obj(json!({"id": reply.get("ids").and_then(|l| l.get(0))}));
    match kind {
        "line" | "arrow" => {
            out.insert("length_m".into(), json!(round(geo::length(&at), 1)));
        }
        "polygon" => {
            out.insert("area_m2".into(), json!(round(geo::area(&at), 0)));
        }
        _ => out.extend(obj(wire([round(at[0][0], 7), round(at[0][1], 7)]))),
    }
    Ok(Value::Object(out))
}

fn draw_line(svc: &McpService, args: &Map) -> ToolResult {
    let arrow = args.get("arrow") == Some(&json!(true));
    draw(svc, args, if arrow { "arrow" } else { "line" }, false)
}

fn draw_area(svc: &McpService, args: &Map) -> ToolResult {
    draw(svc, args, "polygon", false)
}

fn draw_circle(svc: &McpService, args: &Map) -> ToolResult {
    draw(svc, args, "circle", true)
}

fn draw_marker(svc: &McpService, args: &Map) -> ToolResult {
    draw(svc, args, "marker", true)
}

// --------------------------------------------------------------- planner

const EMPTY: &str = "The mission planner is empty. plan_mission makes a plan.";
/// How long the answer waits for the window to have judged a plan.
const CHECK_WAIT: Duration = Duration::from_millis(1500);
const PAGE_OFF: &str =
    "The mission planner page is switched off under Settings, Layout. The plan is shown there once it is on.";

/// The planner's plan and its number.
fn planner(svc: &McpService) -> Result<(Map, u64), ToolError> {
    let reply = svc.api("GET", "/api/mission/planner", None, None)?;
    let plan = reply.get("plan").and_then(Value::as_object).cloned();
    let plan = plan.unwrap_or_else(|| obj(json!({"name": "Mission", "items": []})));
    Ok((plan, reply.get("seq").and_then(Value::as_u64).unwrap_or(0)))
}

fn items_of(plan: &Map) -> Vec<Value> {
    plan.get("items").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn spot(item: &Value) -> Option<Place> {
    Some([item.get("lat")?.as_f64()?, item.get("lon")?.as_f64()?])
}

/// What a type is called here, and in a plan.
const TYPES: [(&str, &str); 6] = [
    ("takeoff", "takeoff"),
    ("waypoint", "waypoint"),
    ("circle", "loiter_turns"),
    ("wait", "loiter_time"),
    ("land", "land"),
    ("return", "rtl"),
];
/// What a value of a point is called here, and in a plan; a wait's `hold_s` is its `seconds`.
const FIELDS: [(&str, &str); 5] = [
    ("altitude", "alt"),
    ("radius_m", "radius"),
    ("turns", "turns"),
    ("speed", "speed"),
    ("approach_altitude", "approach_alt"),
];

fn hold_key(kind: &str) -> &'static str {
    if kind == "loiter_time" {
        "seconds"
    } else {
        "hold"
    }
}

/// A plan as these tools speak of it: points counted from 1, with how far each is from the last.
fn told(plan: &Map) -> Value {
    let mut last = plan.get("home").and_then(spot);
    let mut total = 0.0;
    let mut items = Vec::new();
    for (i, raw) in items_of(plan).iter().enumerate() {
        let kind = raw.get("type").and_then(Value::as_str).unwrap_or("");
        let called = TYPES.iter().find(|(_, t)| *t == kind).map_or(kind, |(c, _)| *c);
        let mut out = obj(json!({"item": i + 1, "type": called}));
        for (ours, theirs) in FIELDS.iter().chain(&[("hold_s", hold_key(kind)), ("name", "name")]) {
            let Some(v) = raw.get(*theirs).filter(|v| !v.is_null()) else {
                continue;
            };
            let idle = *ours == "hold_s" && v.as_f64() == Some(0.0);
            if !idle && !(kind == "land" && *ours == "altitude") {
                out.insert((*ours).into(), v.clone());
            }
        }
        if let Some(at) = spot(raw) {
            out.extend(obj(wire([round(at[0], 7), round(at[1], 7)])));
            if let Some(before) = last {
                let d = geo::distance(before, at);
                total += d;
                out.insert("from_previous_m".into(), json!(round(d, 1)));
            }
            last = Some(at);
        }
        items.push(Value::Object(out));
    }
    json!({"name": plan.get("name"), "start": plan.get("home").and_then(spot).map(wire),
           "count": items.len(), "length_m": round(total, 0), "items": items})
}

/// The plan with what the window says about it, once it shows it.
fn judged(svc: &McpService, plan: &Map) -> Value {
    let mut out = told(plan);
    let asked = Instant::now();
    loop {
        let reply = svc
            .api("GET", "/api/mission/planner", None, None)
            .unwrap_or(Value::Null);
        if let Some(checks) = reply.get("checks").and_then(Value::as_object) {
            for (k, v) in checks {
                out[k.as_str()] = v.clone();
            }
            return out;
        }
        let watched = reply.get("page").is_some_and(|p| !p.is_null()) && svc.window_open();
        if !watched || asked.elapsed() > CHECK_WAIT || svc.pause(Duration::from_millis(100)) {
            return out;
        }
    }
}

fn get_mission_plan(svc: &McpService, _args: &Map) -> ToolResult {
    Ok(judged(svc, &planner(svc)?.0))
}

fn open_mission_plan(svc: &McpService, args: &Map) -> ToolResult {
    let from_vehicle = args.get("from_vehicle") == Some(&json!(true));
    let name = args.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("");
    let reply = match (from_vehicle, name.is_empty()) {
        (true, true) => {
            if super::settings::level(&svc.settings(), "telemetry") == "off" {
                return Err(err("The operator has not allowed reading what the vehicle holds."));
            }
            Craft::of(svc, args).post("/api/mission/download", json!({}))?
        }
        (false, false) => svc.api(
            "POST",
            "/api/mission/plans/load",
            Some(obj(json!({"name": name}))),
            None,
        )?,
        _ => return Err(err("Give the name of a saved mission, or from_vehicle true.")),
    };
    let Some(plan) = reply.get("plan").and_then(Value::as_object).cloned() else {
        let why = reply.get("error").and_then(Value::as_str);
        return Err(err(why.unwrap_or("This mission cannot be shown as a plan.")));
    };
    let mut out = put(svc, args, plan)?;
    if from_vehicle {
        out["not_drawn"] = reply.get("skipped").cloned().unwrap_or(Value::Null);
        if let (Some(seq), Some(rev), false) = (
            planner(svc).ok().map(|(_, seq)| seq),
            reply.get("revision"),
            args.contains_key("vehicle"),
        ) {
            let body = obj(json!({"seq": seq, "revision": rev}));
            let _ = svc.api("POST", "/api/mission/planner/sent", Some(body), None);
        }
    }
    Ok(out)
}

fn set_mission_plan(svc: &McpService, args: &Map) -> ToolResult {
    let (mut plan, _) = planner(svc)?;
    if let Some(name) = args.get("name").filter(|v| v.is_string()) {
        plan.insert("name".into(), name.clone());
    }
    if let Some(start) = args.get("start") {
        let at = place(&mut Craft::of(svc, args), start, "start")?;
        plan.remove("start_at_vehicle");
        plan.insert("home".into(), wire(at));
    }
    match args.get("start_at_vehicle").and_then(Value::as_bool) {
        Some(true) => {
            let at = Craft::of(svc, args).spot("position")?;
            plan.insert("home".into(), wire(at));
            plan.insert("start_at_vehicle".into(), json!(true));
        }
        Some(false) => {
            plan.remove("start_at_vehicle");
        }
        None => {}
    }
    if let Some(kind) = args.get("aircraft") {
        if !PLAN_AIRCRAFT.iter().any(|a| kind == a) {
            return Err(err(format!("aircraft must be one of {}", PLAN_AIRCRAFT.join(", "))));
        }
        plan.insert("aircraft".into(), kind.clone());
    }
    put(svc, args, plan)
}

/// The planner's plan saved under a name, or another plan that is given.
pub(super) fn save_mission(svc: &McpService, args: &Map) -> ToolResult {
    let mut body = pick(args, &["name", "plan"]);
    if !body.contains_key("plan") {
        let (plan, _) = planner(svc)?;
        if items_of(&plan).is_empty() {
            return Err(err(EMPTY));
        }
        body.insert("plan".into(), Value::Object(plan));
    }
    svc.api("POST", "/api/mission/plans/save", Some(body), None)
}

/// One point as a plan has it. `alt` is the height when it names none.
fn item(craft: &mut Craft, raw: &Map, alt: f64, what: &str) -> Result<Map, ToolError> {
    let called = raw.get("type").and_then(Value::as_str).unwrap_or("waypoint");
    let Some((_, kind)) = TYPES.iter().find(|(c, t)| *c == called || *t == called) else {
        let names: Vec<&str> = TYPES.iter().map(|(c, _)| *c).collect();
        return Err(err(format!("{what}: type must be one of {}", names.join(", "))));
    };
    let mut out = obj(json!({"type": kind}));
    if *kind == "rtl" {
        return Ok(out);
    }
    out.insert("alt".into(), json!(alt));
    out.extend(obj(wire(place(craft, &Value::Object(raw.clone()), what)?)));
    set_values(&mut out, raw)?;
    Ok(out)
}

fn set_values(out: &mut Map, raw: &Map) -> Result<(), ToolError> {
    let kind = out.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    for (ours, theirs) in FIELDS.iter().chain(&[("hold_s", hold_key(&kind))]) {
        if let Some(n) = opt(raw, ours)? {
            out.insert((*theirs).into(), json!(n));
        }
    }
    if let Some(cw) = raw.get("clockwise").and_then(Value::as_bool) {
        out.insert("direction".into(), json!(if cw { 1.0 } else { -1.0 }));
    }
    if let Some(name) = raw.get("name").and_then(Value::as_str) {
        out.insert("name".into(), name.into());
    }
    Ok(())
}

/// Puts a plan into the planner and answers it as the planner sees it.
fn put(svc: &McpService, args: &Map, plan: Map) -> ToolResult {
    let body = obj(json!({"plan": plan}));
    let reply = svc.api("POST", "/api/mission/planner", Some(body), None)?;
    let shown = show_planner(svc, args);
    let plan = reply
        .get("plan")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = judged(svc, &plan);
    if !shown && reply.get("page") == Some(&json!(false)) {
        out["note"] = PAGE_OFF.into();
    }
    Ok(out)
}

/// Brings the planner to the front, where the operator lets a client open pages.
fn show_planner(svc: &McpService, args: &Map) -> bool {
    let allowed = super::settings::level(&svc.settings(), "pages") != "off";
    if args.get("show") == Some(&json!(false)) || !allowed || !svc.window_open() {
        return false;
    }
    let body = obj(json!({"page": "mission"}));
    svc.api("POST", "/api/window/open", Some(body), None).is_ok()
}

/// Puts a new plan into the planner, or adds its points to the plan there before its ending.
fn store(svc: &McpService, args: &Map, mut items: Vec<Map>, end: &str) -> ToolResult {
    let at = args.get("at").and_then(Value::as_u64).map(|n| n as usize);
    let append = at.is_some() || args.get("append") == Some(&json!(true));
    let (old, _) = planner(svc)?;
    let mut plan = if append { old.clone() } else { Map::new() };
    let mut kept = if append { items_of(&old) } else { Vec::new() };
    let is = |v: &Value, kinds: &[&str]| {
        v.get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| kinds.contains(&t))
    };
    if append {
        let ending = kept.iter().rposition(|i| !is(i, &["land", "rtl"])).map_or(0, |i| i + 1);
        let ending = match at {
            Some(n) if (1..=kept.len() + 1).contains(&n) => (n - 1).min(ending),
            Some(_) => return Err(err(format!("at must be 1 to {}", kept.len() + 1))),
            None => ending,
        };
        let tail = kept.split_off(ending);
        // a plan that is there begins as it does, unless a takeoff is asked for
        let asked = args.get("takeoff") == Some(&json!(true));
        let begun = kept.iter().chain(&tail).any(|i| is(i, &["takeoff"]));
        let fresh = kept.is_empty() && tail.is_empty();
        let mut first = Vec::new();
        items.retain(|i| {
            let takeoff = i["type"] == json!("takeoff");
            if takeoff && !begun && (asked || fresh) {
                first.push(Value::Object(i.clone()));
            }
            !takeoff
        });
        first.append(&mut kept);
        kept = first;
        kept.extend(items.into_iter().map(Value::Object));
        kept.extend(tail);
    } else {
        for key in ["home", "aircraft", "start_at_vehicle"] {
            if let Some(v) = old.get(key) {
                plan.insert(key.into(), v.clone());
            }
        }
        let last = items.iter().rev().find(|i| i.contains_key("lat")).cloned();
        kept.extend(items.into_iter().map(Value::Object));
        match (end, last) {
            ("rtl", _) => kept.push(json!({"type": "rtl"})),
            ("land", Some(l)) => kept.push(json!({"type": "land", "lat": l["lat"], "lon": l["lon"]})),
            ("land" | "hold", _) => {}
            _ => return Err(err("end must be rtl, land or hold")),
        }
        plan.insert(
            "name".into(),
            args.get("name").cloned().unwrap_or_else(|| "AI mission".into()),
        );
    }
    if let Some(start) = kept.iter().find(|i| is(i, &["takeoff"])).and_then(spot) {
        let takes = !append || !plan.contains_key("home");
        if takes && plan.get("start_at_vehicle") != Some(&json!(true)) {
            plan.insert("home".into(), wire(start));
        }
    }
    if kept.len() > MISSION_MAX_ITEMS {
        return Err(err(format!("a mission may have at most {MISSION_MAX_ITEMS} points")));
    }
    plan.insert("items".into(), Value::Array(kept));
    put(svc, args, plan)
}

/// A takeoff where the vehicle stands, or where the plan begins when there is none.
fn takeoff(craft: &mut Craft, svc: &McpService, args: &Map, first: Place, alt: f64) -> Result<Option<Map>, ToolError> {
    if args.get("takeoff") == Some(&json!(false)) || craft.ground() {
        return Ok(None);
    }
    let at = match args.get("start") {
        Some(v) => place(craft, v, "start")?,
        None => craft
            .spot("position")
            .ok()
            .or_else(|| planner(svc).ok()?.0.get("home").and_then(spot))
            .unwrap_or(first),
    };
    Ok(Some(obj(
        json!({"type": "takeoff", "lat": at[0], "lon": at[1], "alt": alt}),
    )))
}

fn end_of(args: &Map) -> &str {
    args.get("end").and_then(Value::as_str).unwrap_or("rtl")
}

fn plan_mission(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let Some(list) = args
        .get("waypoints")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
    else {
        return Err(err(
            "waypoints must be a non-empty list, each with lat and lon or a place relative to the vehicle or home",
        ));
    };
    let alt = if craft.ground() {
        0.0
    } else {
        within(args, "altitude", Some(30.0), -500.0, 10_000.0)?
    };
    let speed = opt(args, "speed")?;
    let mut items = Vec::new();
    for (i, raw) in list.iter().enumerate() {
        let what = format!("waypoint {}", i + 1);
        let raw = match raw {
            Value::Object(o) => o.clone(),
            other => obj(wire(place(&mut craft, other, &what)?)),
        };
        let mut one = item(&mut craft, &raw, alt, &what)?;
        if let (Some(s), false) = (speed, one.contains_key("speed")) {
            if !["takeoff", "land", "rtl"].iter().any(|t| one["type"] == json!(t)) {
                one.insert("speed".into(), json!(s));
            }
        }
        items.push(one);
    }
    let kind = |m: Option<&Map>| {
        m.and_then(|m| m["type"].as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let end = match kind(items.last()).as_str() {
        "land" | "rtl" => "hold",
        _ => end_of(args),
    };
    if kind(items.first()) != "takeoff" {
        let first = items
            .iter()
            .find_map(|i| spot(&Value::Object(i.clone())))
            .ok_or_else(|| err("a mission needs at least one place"))?;
        if let Some(t) = takeoff(&mut craft, svc, args, first, alt)? {
            items.insert(0, t);
        }
    }
    store(svc, args, items, end)
}

fn plan_survey(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let mut grid = args.clone();
    grid.insert("pattern".into(), "grid".into());
    grid.insert("takeoff".into(), false.into());
    grid.insert("end".into(), "hold".into());
    let p = pattern(svc, &grid)?;
    let alt = if craft.ground() {
        0.0
    } else {
        within(args, "altitude", None, 1.0, 1000.0)?
    };
    let mut items: Vec<Map> = items_of(&p.plan).iter().filter_map(Value::as_object).cloned().collect();
    if let Some(s) = p.speed {
        for i in &mut items {
            i.insert("speed".into(), json!(s));
        }
    }
    if let Some(t) = p
        .path
        .first()
        .map(|first| takeoff(&mut craft, svc, args, *first, alt))
        .transpose()?
        .flatten()
    {
        items.insert(0, t);
    }
    store(svc, args, items, end_of(args))?;
    let mut out = facts(&p);
    out.insert("next".into(), "get_mission_plan lists the points.".into());
    Ok(Value::Object(out))
}

/// The place of `item` in a plan of `n`, counted from 1.
fn index(args: &Map, n: usize) -> Result<usize, ToolError> {
    let i = args.get("item").and_then(Value::as_u64).unwrap_or(0) as usize;
    if (1..=n).contains(&i) {
        Ok(i - 1)
    } else if n == 0 {
        Err(err(EMPTY))
    } else {
        Err(err(format!("item must be 1 to {n}, as get_mission_plan counts them")))
    }
}

fn edit_mission_item(svc: &McpService, args: &Map) -> ToolResult {
    let mut craft = Craft::of(svc, args);
    let (mut plan, _) = planner(svc)?;
    let mut items = items_of(&plan);
    let i = index(args, items.len())?;
    let mut it = items[i].as_object().cloned().unwrap_or_default();
    if it.get("type") == Some(&json!("rtl")) {
        return Err(err("A return has nothing to change. remove_mission_item takes it out."));
    }
    let is_place = [
        "lat",
        "lon",
        "from",
        "north_m",
        "east_m",
        "forward_m",
        "right_m",
        "bearing_deg",
    ]
    .iter()
    .any(|k| args.contains_key(*k));
    if is_place {
        let mut at = args.clone();
        let offset_only = !["lat", "lon", "from"].iter().any(|k| args.contains_key(*k));
        if let (true, Some(p)) = (offset_only, spot(&items[i])) {
            at.extend(obj(wire(p)));
        }
        it.extend(obj(wire(place(&mut craft, &Value::Object(at), "the place")?)));
    }
    set_values(&mut it, args)?;
    items[i] = Value::Object(it);
    if args.contains_key("move_to") {
        let to = args.get("move_to").and_then(Value::as_u64).unwrap_or(0) as usize;
        if !(1..=items.len()).contains(&to) {
            return Err(err(format!("move_to must be 1 to {}", items.len())));
        }
        let moved = items.remove(i);
        items.insert(to - 1, moved);
        let wrong = |v: &Value, kind: &str, i: usize, end: bool| {
            v.get("type") == Some(&json!(kind)) && i != if end { items.len() - 1 } else { 0 }
        };
        let broken = items
            .iter()
            .enumerate()
            .any(|(i, v)| wrong(v, "takeoff", i, false) || wrong(v, "land", i, true) || wrong(v, "rtl", i, true));
        if broken {
            return Err(err("A takeoff stays first, a landing or return last."));
        }
    }
    plan.insert("items".into(), Value::Array(items));
    put(svc, args, plan)
}

fn remove_mission_item(svc: &McpService, args: &Map) -> ToolResult {
    let (mut plan, _) = planner(svc)?;
    let mut items = items_of(&plan);
    if args.get("all") == Some(&json!(true)) {
        items.clear();
    } else {
        let i = index(args, items.len())?;
        items.remove(i);
    }
    plan.insert("items".into(), Value::Array(items));
    put(svc, args, plan)
}

fn upload_mission_plan(svc: &McpService, args: &Map) -> ToolResult {
    let (plan, seq) = planner(svc)?;
    if items_of(&plan).is_empty() {
        return Err(err(EMPTY));
    }
    let start = args.get("start") == Some(&json!(true));
    let craft = Craft::of(svc, args);
    let reply = craft.post("/api/mission/upload", json!({"plan": plan, "start": start}))?;
    if !args.contains_key("vehicle") {
        let body = obj(json!({"seq": seq, "revision": get(&obj(reply.clone()), "revision")}));
        let _ = svc.api("POST", "/api/mission/planner/sent", Some(body), None);
    }
    Ok(Value::Object(pick(&obj(reply), &["items", "started"])))
}

// ------------------------------------------------------------- the table

fn style_props(extra: Value) -> Map {
    let mut p = obj(json!({
        "text": {"type": "string", "description": "A label"},
        "color": {"type": "string", "description": "A name such as red, or #RRGGBB"},
        "id": {"type": "string", "description": "Drawing it again moves it"},
    }));
    p.extend(obj(extra));
    p
}

fn with_place(mut p: Map) -> Map {
    p.extend(place_props());
    p
}

fn count(a: &Map, key: &str) -> String {
    let n = a.get(key).and_then(Value::as_array).map_or(0, Vec::len);
    format!("{n} point{}", if n == 1 { "" } else { "s" })
}

/// Every tool here, in the order of [`NAMES`].
pub fn tools() -> Vec<Tool> {
    let mut groups: Vec<&str> = GROUPS.iter().map(|(n, _)| *n).collect();
    groups.push("all");
    let types: Vec<&str> = TYPES.iter().map(|(c, _)| *c).collect();
    let values = json!({
        "altitude": {"type": "number", "description": "Metres above home"},
        "speed": {"type": "number", "description": "m/s from here on"},
        "hold_s": {"type": "number", "description": "Seconds to stay, of a waypoint or wait"},
        "radius_m": {"type": "number", "description": "Of a circle or wait"},
        "turns": {"type": "number", "description": "Of a circle"},
        "clockwise": {"type": "boolean", "description": "Of a circle or wait"},
        "approach_altitude": {"type": "number", "description": "Of a landing: the height it begins at, above home"},
        "name": {"type": "string"},
    });
    let waypoint = {
        let mut p = with_place(obj(values.clone()));
        p.insert(
            "type".into(),
            json!({"type": "string", "enum": types, "default": "waypoint"}),
        );
        json!({"type": "object", "properties": p})
    };
    let plan_rest = json!({
        "takeoff": {"type": "boolean", "default": true, "description": "Begin with a takeoff"},
        "end": {"type": "string", "enum": ["rtl", "land", "hold"], "default": "rtl"},
        "name": {"type": "string"},
        "append": {"type": "boolean", "default": false, "description": "Add to the plan there instead of replacing it"},
        "at": {"type": "integer", "description": "Add before this point instead, counted from 1"},
        "show": {"type": "boolean", "default": true, "description": "Bring the mission planner to the front"},
    });
    vec![
        tool("get_telemetry", "telemetry", "Telemetry",
            "Live telemetry by group, short: overview (armed, mode, height, speed, heading, battery, position), position, attitude, speed, battery, gps, link, mission, health, vehicle or all. Without groups the overview. With seconds it watches that long and answers min, max and mean too. null means not reported.",
            obj(json!({
                "groups": {"type": "array", "items": {"type": "string", "enum": groups}},
                "seconds": {"type": "number", "minimum": 0, "maximum": WATCH_MAX_S, "default": 0},
            })), &[], Run::Built(get_telemetry), |_| Some("read telemetry".into()), true, true),
        tool("draw_line", "map", "Draw a line",
            "Draw a line through points on the operator's map, or an arrow to the last one.",
            style_props(json!({
                "points": places_schema("Two or more, in order"),
                "arrow": {"type": "boolean", "default": false},
                "dashed": {"type": "boolean"}, "width": {"type": "number"},
            })), &["points"], Run::Built(draw_line),
            |a| Some(format!("draw a line of {} on the map", count(a, "points"))), true, false),
        tool("draw_area", "map", "Draw an area",
            "Draw a filled area on the operator's map from its corners.",
            style_props(json!({
                "points": places_schema("Three or more corners, in order"),
                "fill_opacity": {"type": "number", "minimum": 0, "maximum": 1},
            })), &["points"], Run::Built(draw_area),
            |a| Some(format!("draw an area of {} on the map", count(a, "points"))), true, false),
        tool("draw_circle", "map", "Draw a circle",
            "Draw a circle on the operator's map around lat and lon, or around a place relative to the vehicle or home.",
            with_place(style_props(json!({
                "radius_m": {"type": "number"}, "fill_opacity": {"type": "number", "minimum": 0, "maximum": 1},
            }))), &["radius_m"], Run::Built(draw_circle),
            |a| Some(format!("draw a circle at {}", where_text(Some(&Value::Object(a.clone()))))), true, false),
        tool("draw_marker", "map", "Draw a marker",
            "Put a marker on the operator's map at lat and lon, or at a place relative to the vehicle or home.",
            with_place(style_props(json!({
                "icon": {"type": "string", "description": "pin, flag, target, hazard, fire, person, vehicle, building, tree, camera, antenna, home, drone, boat, medical, water, gate"},
            }))), &[], Run::Built(draw_marker),
            |a| Some(format!("draw a marker at {}", where_text(Some(&Value::Object(a.clone()))))), true, false),
        tool("get_mission_plan", "planner", "Plan in the planner",
            "The plan in the operator's mission planner: its points counted from 1, heights, distances, and the warnings the planner gives. It is not what the vehicle holds until it is uploaded; download_mission reads that.",
            Map::new(), &[], Run::Built(get_mission_plan), |_| Some("read the mission planner".into()), false, true),
        tool("open_mission_plan", "planner", "Open a mission",
            "Open a saved mission in the operator's mission planner, or read the one the vehicle holds into it. list_saved_missions names the saved ones.",
            obj(json!({"name": {"type": "string"}, "from_vehicle": {"type": "boolean", "default": false}})), &[],
            Run::Built(open_mission_plan), |a| Some(match a.get("name").and_then(Value::as_str) {
                Some(n) => format!("open the mission {}", n.chars().take(60).collect::<String>()),
                None => "read the vehicle's mission into the planner".into(),
            }), true, false),
        tool("plan_mission", "planner", "Plan a mission",
            "Make a mission in the operator's mission planner, where they see and change it. Nothing is sent to the vehicle; upload_mission_plan does that. A takeoff at the start and a return at the end are added unless told otherwise.",
            {
                let mut p = obj(json!({
                    "waypoints": {"type": "array", "minItems": 1, "items": waypoint, "description": "In order. Each lat and lon, or from vehicle or home with an offset"},
                    "altitude": {"type": "number", "default": 30, "description": "Metres above home, for points that name none"},
                    "speed": {"type": "number", "description": "m/s"},
                }));
                p.extend(obj(plan_rest.clone()));
                p
            }, &["waypoints"], Run::Built(plan_mission),
            |a| Some(format!("plan a mission of {}", count(a, "waypoints"))), true, false),
        tool("plan_survey", "planner", "Plan a survey",
            "Cover an area back and forth as a mission in the operator's mission planner. Nothing is sent to the vehicle; upload_mission_plan does that.",
            {
                let mut p = pattern_props();
                for key in ["pattern", "center", "radius_m"] {
                    p.remove(key);
                }
                p.insert("points".into(), places_schema("The corners of the area"));
                p.extend(obj(plan_rest));
                p
            }, &["points", "altitude", "spacing_m"], Run::Built(plan_survey),
            |a| Some(format!("plan a survey over {}", count(a, "points"))), true, false),
        tool("edit_mission_item", "planner", "Change a point of the plan",
            "Change one point of the plan in the mission planner: move it to lat and lon, or by an offset from where it is, or change its height, speed and so on. What is left out stays.",
            {
                let mut p = with_place(obj(values));
                p.insert("move_to".into(), json!({"type": "integer", "description": "Its new place in the order"}));
                p.insert("item".into(), json!({"type": "integer", "description": "Counted from 1, as get_mission_plan does"}));
                p
            }, &["item"], Run::Built(edit_mission_item),
            |a| Some(format!("change point {} of the plan", say(a, "item"))), true, false),
        tool("remove_mission_item", "planner", "Remove a point of the plan",
            "Take one point out of the plan in the mission planner, or all of them.",
            obj(json!({"item": {"type": "integer"}, "all": {"type": "boolean", "default": false}})), &[],
            Run::Built(remove_mission_item), |a| Some(if a.get("all") == Some(&json!(true)) {
                "empty the mission planner".into()
            } else {
                format!("take point {} out of the plan", say(a, "item"))
            }), false, false),
        tool("set_mission_plan", "planner", "Name and start of the plan",
            "Set the name of the plan in the mission planner, where it starts, and what kind of aircraft it is for. A connected vehicle decides its own kind.",
            obj(json!({
                "name": {"type": "string"}, "start": place_schema("The start"),
                "start_at_vehicle": {"type": "boolean", "description": "Keep the start where the vehicle stands"},
                "aircraft": {"type": "string", "enum": PLAN_AIRCRAFT},
            })), &[], Run::Built(set_mission_plan), |_| Some("set the name or start of the plan".into()), true, false),
        tool("upload_mission_plan", "mission", "Upload the plan",
            "Upload the plan in the mission planner to the vehicle. With start true it is started too, which arms the vehicle.",
            obj(json!({"start": {"type": "boolean", "default": false}})), &[], Run::Built(upload_mission_plan),
            |a| Some(if a.get("start") == Some(&json!(true)) {
                "upload the plan in the mission planner and start it, which arms the vehicle".into()
            } else {
                "upload the plan in the mission planner".into()
            }), true, false),
    ]
}

/// Whether what is called `name` takes its arguments through [`tidy`]: the tools added since the
/// first list was frozen, whose arguments nothing else is held to.
pub fn is_tidied(name: &str) -> bool {
    NAMES.contains(&name) || super::more::NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_and_the_names_agree_and_stay_flat() {
        let list = tools();
        assert_eq!(list.iter().map(|t| t.name).collect::<Vec<_>>(), NAMES);
        for t in list {
            assert!(super::super::settings::permission(t.permission).is_some(), "{}", t.name);
            assert!(t.required.iter().all(|r| t.properties.contains_key(*r)), "{}", t.name);
            assert!((t.summary)(&Map::new()).is_some(), "{}", t.name);
            assert!(
                !t.description.contains('\u{2014}') && !t.description.contains(" - "),
                "{}",
                t.name
            );
        }
        let state: Map = serde_json::from_str(include_str!("../../tests/data/state_defaults.json")).unwrap();
        for (group, fields) in GROUPS {
            for (_, key) in fields {
                assert!(state.contains_key(*key), "{group} names {key}, which no state has");
            }
        }
    }

    #[test]
    fn arguments_as_models_write_them_become_what_the_schema_means() {
        let t = tools().into_iter().find(|t| t.name == "plan_mission").unwrap();
        let args = obj(json!({
            "waypoints": "[{\"lat\": \"48.1\", \"lon\": 11.6, \"hold_s\": \"5\"}]",
            "altitude": "40", "takeoff": "false", "name": null,
        }));
        assert_eq!(
            Value::Object(tidy(&args, &t.properties)),
            json!({"waypoints": [{"lat": 48.1, "lon": 11.6, "hold_s": 5.0}], "altitude": 40.0, "takeoff": false})
        );
        let t = tools().into_iter().find(|t| t.name == "draw_line").unwrap();
        let args = obj(json!({"points": {"from": "vehicle"}, "text": "12"}));
        assert_eq!(
            Value::Object(tidy(&args, &t.properties)),
            json!({"points": [{"from": "vehicle"}], "text": "12"}),
            "one place is a list of one, and text stays text"
        );
        let t = tools().into_iter().find(|t| t.name == "edit_mission_item").unwrap();
        let args = obj(json!({"item": 2.0, "altitude": "high"}));
        assert_eq!(
            Value::Object(tidy(&args, &t.properties)),
            json!({"item": 2, "altitude": "high"}),
            "what cannot be read is left for the refusal to name"
        );
    }

    #[test]
    fn a_plan_is_told_in_the_words_it_is_made_with() {
        let plan = obj(json!({"name": "M", "home": {"lat": 48.0, "lon": 11.0}, "items": [
            {"type": "takeoff", "lat": 48.0, "lon": 11.0, "alt": 30.0, "pitch": 0.0},
            {"type": "loiter_time", "lat": 48.001, "lon": 11.0, "alt": 40.0, "seconds": 20.0, "radius": 50.0, "direction": 1.0},
            {"type": "rtl"}]}));
        let t = told(&plan);
        assert_eq!(t["count"], 3);
        assert_eq!(t["items"][1]["type"], "wait");
        assert_eq!(t["items"][1]["hold_s"], 20.0);
        assert_eq!(t["items"][2], json!({"item": 3, "type": "return"}));
        assert!((t["length_m"].as_f64().unwrap() - 111.0).abs() < 1.0);
    }

    #[test]
    fn a_number_that_means_unknown_is_null() {
        let s = json!({"gps_hdop": 99.0, "mission_seq": -1, "battery_percent": 0, "gps_fix": ""});
        assert_eq!(reading(&s, "gps_hdop"), Value::Null);
        assert_eq!(reading(&s, "mission_seq"), Value::Null);
        assert_eq!(reading(&s, "gps_fix"), Value::Null);
        assert_eq!(reading(&s, "battery_percent"), 0);
    }
}
