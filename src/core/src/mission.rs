//! The mission plan: what it is, what makes one valid, and where it is kept
//! (`mission.py`).
//!
//! * [`validate_plan`] is the only gate. It treats every value as hostile and
//!   answers a message instead of failing.
//! * [`plan_to_items`] lowers a validated plan to flat wire shaped items;
//!   [`lower_circles`] flies a Circle as waypoints for a stack that refuses
//!   `NAV_LOITER_TURNS`; [`items_to_plan`] reads a downloaded mission back.
//! * The file store keeps plans as plain JSON under `~/.corvus/missions`.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::paths::Paths;
use crate::protocol::common::{fixed, py_g, py_round};
use crate::protocol::missions::{DownloadedItem, PlanItem};

pub const MISSION_MAX_ITEMS: usize = 255;
pub const MISSION_ALT_MIN_M: f64 = -100.0;
pub const MISSION_ALT_MAX_M: f64 = 1000.0;
pub const MISSION_APPROACH_MIN_M: f64 = 3.0;
pub const MISSION_RADIUS_MIN_M: f64 = 1.0;
pub const MISSION_RADIUS_MAX_M: f64 = 10000.0;
pub const MISSION_SPEED_MIN_MS: f64 = 0.5;
pub const MISSION_SPEED_MAX_MS: f64 = 100.0;
pub const MISSION_HOLD_MAX_S: f64 = 3600.0;
pub const MISSION_TURNS_MAX: f64 = 100.0;
pub const MISSION_NAME_MAX: usize = 64;
pub const MISSION_POINT_NAME_MAX: usize = 40;

pub const MAV_CMD_NAV_WAYPOINT: i64 = 16;
pub const MAV_CMD_NAV_LOITER_TURNS: i64 = 18;
pub const MAV_CMD_NAV_LOITER_TIME: i64 = 19;
pub const MAV_CMD_NAV_RETURN_TO_LAUNCH: i64 = 20;
pub const MAV_CMD_NAV_LAND: i64 = 21;
pub const MAV_CMD_NAV_TAKEOFF: i64 = 22;
pub const MAV_CMD_DO_CHANGE_SPEED: i64 = 178;

pub const PLAN_AIRCRAFT: [&str; 4] = ["multirotor", "fixed_wing", "vtol", "rover"];
pub const GROUND_AIRCRAFT: &str = "rover";
pub const APPROACH_AIRCRAFT: &str = "multirotor";
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

pub const CIRCLE_RING_SPACING_M: f64 = 10.0;
pub const CIRCLE_RING_POINTS_MIN: f64 = 12.0;
pub const CIRCLE_RING_POINTS_MAX: f64 = 36.0;

/// One command parameter a plan field lands in.
pub struct ParamRule {
    pub field: &'static str,
    /// 1 to 4, or None for a field with no parameter of its own (a direction).
    pub slot: Option<usize>,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

/// One item type the planner can draw.
pub struct ItemSpec {
    pub name: &'static str,
    pub command: i64,
    pub position: bool,
    pub yaw_slot: Option<usize>,
    pub label: &'static str,
    pub params: &'static [ParamRule],
}

const fn rule(field: &'static str, slot: Option<usize>, min: f64, max: f64, default: f64) -> ParamRule {
    ParamRule {
        field,
        slot,
        min,
        max,
        default,
    }
}

pub const ITEM_SPECS: [ItemSpec; 6] = [
    ItemSpec {
        name: "takeoff",
        command: MAV_CMD_NAV_TAKEOFF,
        position: true,
        yaw_slot: Some(4),
        label: "Takeoff",
        params: &[rule("pitch", Some(1), 0.0, 45.0, 0.0)],
    },
    ItemSpec {
        name: "waypoint",
        command: MAV_CMD_NAV_WAYPOINT,
        position: true,
        yaw_slot: Some(4),
        label: "Waypoint",
        params: &[
            rule("hold", Some(1), 0.0, MISSION_HOLD_MAX_S, 0.0),
            rule("accept_radius", Some(2), 0.0, 1000.0, 0.0),
        ],
    },
    ItemSpec {
        name: "loiter_turns",
        command: MAV_CMD_NAV_LOITER_TURNS,
        position: true,
        yaw_slot: None,
        label: "Circle",
        params: &[
            rule("turns", Some(1), 0.0, MISSION_TURNS_MAX, 1.0),
            rule("radius", Some(3), MISSION_RADIUS_MIN_M, MISSION_RADIUS_MAX_M, 50.0),
            rule("direction", None, -1.0, 1.0, 1.0),
        ],
    },
    ItemSpec {
        name: "loiter_time",
        command: MAV_CMD_NAV_LOITER_TIME,
        position: true,
        yaw_slot: None,
        label: "Hold",
        params: &[
            rule("seconds", Some(1), 0.0, MISSION_HOLD_MAX_S, 30.0),
            rule("radius", Some(3), MISSION_RADIUS_MIN_M, MISSION_RADIUS_MAX_M, 50.0),
            rule("direction", None, -1.0, 1.0, 1.0),
        ],
    },
    ItemSpec {
        name: "land",
        command: MAV_CMD_NAV_LAND,
        position: true,
        yaw_slot: Some(4),
        label: "Land",
        params: &[],
    },
    ItemSpec {
        name: "rtl",
        command: MAV_CMD_NAV_RETURN_TO_LAUNCH,
        position: false,
        yaw_slot: None,
        label: "Return",
        params: &[],
    },
];

pub const ORBIT_TYPES: [&str; 2] = ["loiter_turns", "loiter_time"];

fn spec_of(name: &str) -> Option<&'static ItemSpec> {
    ITEM_SPECS.iter().find(|s| s.name == name)
}

fn type_of_command(command: i64) -> Option<&'static ItemSpec> {
    ITEM_SPECS.iter().find(|s| s.command == command)
}

/// The commands that name no place: a Return and a speed change.
pub fn positionless_commands() -> Vec<i64> {
    let mut out = vec![MAV_CMD_DO_CHANGE_SPEED];
    out.extend(ITEM_SPECS.iter().filter(|s| !s.position).map(|s| s.command));
    out
}

// ------------------------------------------------------------ python text

fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let split = exp as usize + 1;
            let (int, frac) = if digits.len() > split {
                (digits[..split].to_string(), digits[split..].to_string())
            } else {
                (format!("{digits}{}", "0".repeat(split - digits.len())), "0".to_string())
            };
            format!("{sign}{int}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let rest = &digits[1..];
        let dot = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        format!(
            "{sign}{}{dot}e{}{:02}",
            &digits[..1],
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (0x80..0xa0).contains(&(c as u32)) || c as u32 == 0xa0 || c as u32 == 0xad => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a JSON value.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => py_float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// ----------------------------------------------------------------- helpers

/// `value` as a finite float, None for a bool, a non number or a NaN.
fn finite(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

fn bounded(value: Option<&Value>, low: f64, high: f64) -> Option<f64> {
    finite(value).filter(|x| (low..=high).contains(x))
}

fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn collapse(text: &str) -> String {
    text.split(py_space)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A filename safe mission name, or "" when `raw` cannot yield one.
pub fn safe_plan_name(raw: &Value) -> String {
    let Some(text) = raw.as_str() else { return String::new() };
    let mut replaced = String::new();
    let mut in_run = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || " ._-".contains(c) {
            replaced.push(c);
            in_run = false;
        } else if !in_run {
            replaced.push(' ');
            in_run = true;
        }
    }
    let trimmed = replaced.trim_matches(|c| c == ' ' || c == '.');
    let collapsed = collapse(trimmed);
    let cut: String = collapsed.chars().take(MISSION_NAME_MAX).collect();
    py_strip(&cut).to_string()
}

fn py_strip(text: &str) -> &str {
    text.trim_matches(py_space)
}

/// A point's display name, or "" when `raw` carries none.
pub fn clean_point_name(raw: &Value) -> String {
    let Some(text) = raw.as_str() else { return String::new() };
    let junk = |c: char| {
        let n = c as u32;
        n <= 0x1f || (0x7f..=0x9f).contains(&n) || n == 0x2028 || n == 0x2029
    };
    let mut spaced = String::new();
    let mut in_run = false;
    for c in text.chars() {
        if junk(c) {
            if !in_run {
                spaced.push(' ');
            }
            in_run = true;
        } else {
            spaced.push(c);
            in_run = false;
        }
    }
    let collapsed = collapse(&spaced);
    let cut: String = collapsed.chars().take(MISSION_POINT_NAME_MAX).collect();
    py_strip(&cut).to_string()
}

/// The plan item types a flight stack will not fly, with the reason.
pub fn refused_item_types(refusals: &[(i64, String)]) -> Map<String, Value> {
    let mut out = Map::new();
    for spec in &ITEM_SPECS {
        if let Some((_, why)) = refusals.iter().find(|(c, _)| *c == spec.command) {
            out.insert(spec.name.into(), json!(why));
        }
    }
    out
}

/// The planner's name for a MAV_CMD, `command N` for one it has none for.
pub fn label_of_command(command: i64) -> String {
    type_of_command(command).map_or_else(|| format!("command {command}"), |s| s.label.to_string())
}

/// Where saved plans live: `configured` when set, else `~/.corvus/missions`.
pub fn missions_dir(configured: &str, paths: &Paths) -> PathBuf {
    let trimmed = py_strip(configured);
    if trimmed.is_empty() {
        paths.join("missions")
    } else {
        PathBuf::from(trimmed)
    }
}

// -------------------------------------------------------------- validation

fn validate_item(index: usize, raw: &Value) -> Result<Map<String, Value>, String> {
    let Some(raw) = raw.as_object() else {
        return Err(format!("item {index} must be an object"));
    };
    let kind_value = raw.get("type");
    let Some(spec) = kind_value.and_then(Value::as_str).and_then(spec_of) else {
        let shown = kind_value.map_or_else(|| "None".to_string(), py_repr);
        return Err(format!("item {index} has an unknown type {shown}"));
    };
    let kind = spec.name;
    let mut item = Map::new();
    item.insert("type".into(), json!(kind));

    if spec.position {
        let Some(lat) = bounded(raw.get("lat"), -90.0, 90.0) else {
            return Err(format!("item {index} lat must be a number between -90 and 90"));
        };
        let Some(lon) = bounded(raw.get("lon"), -180.0, 180.0) else {
            return Err(format!("item {index} lon must be a number between -180 and 180"));
        };
        let alt = if kind == "land" {
            0.0
        } else {
            match bounded(raw.get("alt"), MISSION_ALT_MIN_M, MISSION_ALT_MAX_M) {
                Some(alt) => alt,
                None => {
                    return Err(format!(
                        "item {index} alt must be between {} and {} m above home",
                        fixed(MISSION_ALT_MIN_M, 0),
                        fixed(MISSION_ALT_MAX_M, 0)
                    ))
                }
            }
        };
        item.insert("lat".into(), json!(lat));
        item.insert("lon".into(), json!(lon));
        item.insert("alt".into(), json!(alt));
    }

    for rule in spec.params {
        let given = raw.get(rule.field).filter(|v| !v.is_null());
        let Some(given) = given else {
            item.insert(rule.field.into(), json!(rule.default));
            continue;
        };
        let Some(value) = bounded(Some(given), rule.min, rule.max) else {
            return Err(format!(
                "item {index} {} must be between {} and {}",
                rule.field,
                py_g(rule.min),
                py_g(rule.max)
            ));
        };
        item.insert(rule.field.into(), json!(value));
    }

    if ORBIT_TYPES.contains(&kind) {
        let direction = item["direction"].as_f64().unwrap_or(1.0);
        item.insert("direction".into(), json!(if direction >= 0.0 { 1.0 } else { -1.0 }));
    }

    if let Some(speed) = raw.get("speed").filter(|v| !v.is_null()) {
        let Some(value) = bounded(Some(speed), MISSION_SPEED_MIN_MS, MISSION_SPEED_MAX_MS) else {
            return Err(format!(
                "item {index} speed must be between {} and {} m/s",
                py_g(MISSION_SPEED_MIN_MS),
                py_g(MISSION_SPEED_MAX_MS)
            ));
        };
        item.insert("speed".into(), json!(value));
    }

    if kind == "land" {
        if let Some(approach) = raw.get("approach_alt").filter(|v| !v.is_null()) {
            let Some(value) = bounded(Some(approach), MISSION_APPROACH_MIN_M, MISSION_ALT_MAX_M) else {
                return Err(format!(
                    "item {index} approach_alt must be between {} and {} m above home",
                    fixed(MISSION_APPROACH_MIN_M, 0),
                    fixed(MISSION_ALT_MAX_M, 0)
                ));
            };
            item.insert("approach_alt".into(), json!(value));
        }
    }

    let name = clean_point_name(raw.get("name").unwrap_or(&Value::Null));
    if !name.is_empty() {
        item.insert("name".into(), json!(name));
    }
    Ok(item)
}

/// A cleaned plan, or the reason there is none.
pub fn validate_plan(raw: &Value) -> Result<Value, String> {
    let Some(raw) = raw.as_object() else {
        return Err("plan must be an object".into());
    };
    let Some(items_raw) = raw.get("items").and_then(Value::as_array) else {
        return Err("plan items must be a list".into());
    };
    if items_raw.len() > MISSION_MAX_ITEMS {
        return Err(format!(
            "too many items ({}); the maximum is {MISSION_MAX_ITEMS}",
            items_raw.len()
        ));
    }
    let mut items = Vec::new();
    for (index, entry) in items_raw.iter().enumerate() {
        items.push(Value::Object(validate_item(index, entry)?));
    }
    let mut plan = Map::new();
    plan.insert("version".into(), json!(1));
    plan.insert("items".into(), Value::Array(items));

    let name = safe_plan_name(raw.get("name").unwrap_or(&Value::Null));
    plan.insert(
        "name".into(),
        json!(if name.is_empty() { "Mission".to_string() } else { name }),
    );

    if let Some(aircraft) = raw.get("aircraft").and_then(Value::as_str) {
        if PLAN_AIRCRAFT.contains(&aircraft) {
            plan.insert("aircraft".into(), json!(aircraft));
        }
    }

    match raw.get("home") {
        Some(Value::Object(home_raw)) => {
            let lat = bounded(home_raw.get("lat"), -90.0, 90.0);
            let lon = bounded(home_raw.get("lon"), -180.0, 180.0);
            let (Some(lat), Some(lon)) = (lat, lon) else {
                return Err("plan home must carry a finite lat and lon".into());
            };
            let mut home = Map::new();
            home.insert("lat".into(), json!(lat));
            home.insert("lon".into(), json!(lon));
            if let Some(elevation) = finite(home_raw.get("elevation")) {
                home.insert("elevation".into(), json!(elevation));
            }
            plan.insert("home".into(), Value::Object(home));
        }
        Some(Value::Null) | None => {}
        Some(_) => return Err("plan home must be an object".into()),
    }

    if raw.get("start_at_vehicle") == Some(&Value::Bool(true)) {
        plan.insert("start_at_vehicle".into(), json!(true));
    }

    if let Some(speed) = raw.get("speed").filter(|v| !v.is_null()) {
        let Some(value) = bounded(Some(speed), MISSION_SPEED_MIN_MS, MISSION_SPEED_MAX_MS) else {
            return Err(format!(
                "plan speed must be between {} and {} m/s",
                py_g(MISSION_SPEED_MIN_MS),
                py_g(MISSION_SPEED_MAX_MS)
            ));
        };
        plan.insert("speed".into(), json!(value));
    }

    let plan = Value::Object(plan);
    let wire = plan_to_items(&plan).len();
    if wire > MISSION_MAX_ITEMS {
        return Err(format!(
            "too many items ({wire} once the speed changes are counted); the maximum is {MISSION_MAX_ITEMS}"
        ));
    }
    Ok(plan)
}

// ------------------------------------------------------------- lowering

fn speed_item(speed: f64, item: i64) -> PlanItem {
    PlanItem {
        command: MAV_CMD_DO_CHANGE_SPEED,
        lat: 0.0,
        lon: 0.0,
        alt: 0.0,
        params: vec![1.0, speed, -1.0, 0.0],
        item: Some(item),
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// A validated plan as flat, wire shaped mission items. A speed is a command
/// of its own: the plan's cruise speed first, a pinned speed right before the
/// item it is flown into, none when it is already in force. A landing's
/// `approach_alt` is a waypoint above the point on a multicopter's plan.
pub fn plan_to_items(plan: &Value) -> Vec<PlanItem> {
    let mut items: Vec<PlanItem> = Vec::new();
    let mut current: Option<f64> = None;

    if let Some(speed) = number(plan.get("speed")) {
        current = Some(speed);
        items.push(speed_item(speed, 0));
    }

    let entries: &[Value] = plan.get("items").and_then(Value::as_array).map_or(&[], Vec::as_slice);
    for (index, entry) in entries.iter().enumerate() {
        let index = index as i64;
        if let Some(pinned) = number(entry.get("speed")) {
            if Some(pinned) != current {
                current = Some(pinned);
                items.push(speed_item(pinned, index));
            }
        }
        let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "land" && plan.get("aircraft").and_then(Value::as_str) == Some(APPROACH_AIRCRAFT) {
            if let Some(approach) = number(entry.get("approach_alt")) {
                items.push(PlanItem {
                    command: MAV_CMD_NAV_WAYPOINT,
                    lat: number(entry.get("lat")).unwrap_or(0.0),
                    lon: number(entry.get("lon")).unwrap_or(0.0),
                    alt: approach,
                    params: vec![0.0, 0.0, 0.0, f64::NAN],
                    item: Some(index),
                });
            }
        }
        let Some(spec) = spec_of(kind) else { continue };
        let mut params = vec![0.0, 0.0, 0.0, 0.0];
        if let Some(slot) = spec.yaw_slot {
            params[slot - 1] = f64::NAN;
        }
        for rule in spec.params {
            let Some(slot) = rule.slot else { continue };
            params[slot - 1] = number(entry.get(rule.field)).unwrap_or(rule.default);
        }
        if ORBIT_TYPES.contains(&kind) {
            params[2] = params[2].copysign(number(entry.get("direction")).unwrap_or(1.0));
        }
        items.push(PlanItem {
            command: spec.command,
            lat: number(entry.get("lat")).unwrap_or(0.0),
            lon: number(entry.get("lon")).unwrap_or(0.0),
            alt: number(entry.get("alt")).unwrap_or(0.0),
            params,
            item: Some(index),
        });
    }
    items
}

fn ring_waypoints(entry: &PlanItem, before: Option<&PlanItem>) -> Vec<PlanItem> {
    let param = |i: usize| {
        entry
            .params
            .get(i)
            .copied()
            .filter(|x| *x != 0.0 && !x.is_nan())
            .unwrap_or(0.0)
    };
    let turns = param(0).max(0.0);
    let radius = MISSION_RADIUS_MIN_M.max(param(2).abs());
    let raw_radius = entry.params.get(2).copied().unwrap_or(0.0);
    let clockwise = (if raw_radius == 0.0 { 0.0 } else { raw_radius }) >= 0.0;
    let (lat0, lon0) = (entry.lat, entry.lon);
    let k = EARTH_RADIUS_M * std::f64::consts::PI / 180.0;
    let c = 0.01f64.max(lat0.to_radians().cos());
    let mut start = std::f64::consts::FRAC_PI_2;
    if let Some(before) = before {
        let dx = (before.lon - lon0) * k * c;
        let dy = (before.lat - lat0) * k;
        if dx.hypot(dy) > 0.5 {
            start = dy.atan2(dx);
        }
    }
    let rounded = py_round_half_even(2.0 * std::f64::consts::PI * radius / CIRCLE_RING_SPACING_M);
    let per_lap = CIRCLE_RING_POINTS_MAX.min(CIRCLE_RING_POINTS_MIN.max(rounded)) as i64;
    let steps = py_round_half_even(per_lap as f64 * turns) as i64;
    let sign = if clockwise { -1.0 } else { 1.0 };
    (0..=steps)
        .map(|i| {
            let angle = start + sign * 2.0 * std::f64::consts::PI * i as f64 / per_lap as f64;
            PlanItem {
                command: MAV_CMD_NAV_WAYPOINT,
                lat: lat0 + radius * angle.sin() / k,
                lon: lon0 + radius * angle.cos() / (k * c),
                alt: entry.alt,
                params: vec![0.0, 0.0, 0.0, f64::NAN],
                item: entry.item,
            }
        })
        .collect()
}

/// Python's `round(x)`: ties go to the even integer.
fn py_round_half_even(x: f64) -> f64 {
    let floor = x.floor();
    let diff = x - floor;
    if diff < 0.5 {
        floor
    } else if diff > 0.5 {
        floor + 1.0
    } else if floor % 2.0 == 0.0 {
        floor
    } else {
        floor + 1.0
    }
}

/// `plan_to_items` output with every Circle flown as the waypoints around its
/// rim, for an autopilot that refuses `NAV_LOITER_TURNS` as a mission item.
pub fn lower_circles(items: &[PlanItem]) -> Vec<PlanItem> {
    let positionless = positionless_commands();
    let mut out: Vec<PlanItem> = Vec::new();
    let mut before: Option<PlanItem> = None;
    for entry in items {
        if entry.command == MAV_CMD_NAV_LOITER_TURNS {
            let ring = ring_waypoints(entry, before.as_ref());
            before = ring.last().cloned();
            out.extend(ring);
            continue;
        }
        out.push(entry.clone());
        if !positionless.contains(&entry.command) {
            before = Some(entry.clone());
        }
    }
    out
}

// ---------------------------------------------------------- reading back

fn is_relative(frame: i64) -> bool {
    matches!(frame, 3 | 6)
}
fn is_amsl(frame: i64) -> bool {
    matches!(frame, 0 | 5)
}
fn is_terrain(frame: i64) -> bool {
    matches!(frame, 10 | 11)
}

fn param_slot(item: &DownloadedItem, slot: usize) -> Option<f64> {
    item.params.get(slot - 1).copied().flatten().filter(|x| x.is_finite())
}

/// A mission downloaded from the vehicle as a plan: `{"plan", "plan_index",
/// "skipped", "adjusted", "error"}`. `home` stands in for a takeoff or
/// landing that names no place; `home_alt_amsl` converts an item written above
/// sea level into a height above home.
pub fn items_to_plan(
    items: &[DownloadedItem],
    home: Option<(f64, f64)>,
    home_alt_amsl: Option<f64>,
    name: &str,
) -> Value {
    let mut plan_items: Vec<Value> = Vec::new();
    let mut plan_index: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut adjusted: Vec<Value> = Vec::new();
    let mut plan_speed: Option<f64> = None;
    let mut pending_speed: Option<f64> = None;
    let mut pending_slots: Vec<usize> = Vec::new();
    let mut last_position: Option<(f64, f64)> = None;

    for (seq, raw) in items.iter().enumerate() {
        plan_index.push(Value::Null);
        let command = raw.command;

        if command == MAV_CMD_DO_CHANGE_SPEED {
            match param_slot(raw, 2) {
                Some(speed) if speed > 0.0 => {
                    let speed = speed.clamp(MISSION_SPEED_MIN_MS, MISSION_SPEED_MAX_MS);
                    if plan_items.is_empty() && plan_speed.is_none() && pending_speed.is_none() {
                        plan_speed = Some(speed);
                    } else {
                        pending_speed = Some(speed);
                    }
                }
                _ => {}
            }
            pending_slots.push(seq);
            continue;
        }

        let Some(spec) = type_of_command(command) else {
            skipped.push(json!({"seq": seq, "command": command,
                "reason": format!("command {command} is not one the planner draws")}));
            continue;
        };
        let kind = spec.name;
        let mut entry = Map::new();
        entry.insert("type".into(), json!(kind));

        if spec.position {
            let frame = raw.frame;
            let mut lat = Some(raw.lat).filter(|x| x.is_finite());
            let mut lon = Some(raw.lon).filter(|x| x.is_finite());
            if lat.is_none() || lon.is_none() || (lat == Some(0.0) && lon == Some(0.0)) {
                let fallback = last_position.or(home);
                let Some((flat, flon)) = fallback else {
                    skipped.push(json!({"seq": seq, "command": command,
                        "reason": "no position, and no home to put it at"}));
                    continue;
                };
                lat = Some(flat);
                lon = Some(flon);
                adjusted.push(json!({"seq": seq, "field": "position",
                    "reason": format!("the vehicle's item names no place; drawn at {}",
                        if last_position.is_some() { "the previous point" } else { "home" })}));
            }
            let mut alt = Some(raw.alt).filter(|x| x.is_finite()).unwrap_or(0.0);
            if is_terrain(frame) {
                skipped.push(json!({"seq": seq, "command": command,
                    "reason": "its altitude is above terrain, not above home"}));
                continue;
            }
            if is_amsl(frame) {
                let Some(home_alt) = home_alt_amsl else {
                    skipped.push(json!({"seq": seq, "command": command,
                        "reason": "its altitude is above sea level and home's is not known yet"}));
                    continue;
                };
                alt -= home_alt;
            } else if !is_relative(frame) {
                skipped.push(json!({"seq": seq, "command": command,
                    "reason": format!("frame {frame} is not a map position")}));
                continue;
            }
            if kind != "land" {
                let clamped = alt.clamp(MISSION_ALT_MIN_M, MISSION_ALT_MAX_M);
                if clamped != alt {
                    adjusted.push(json!({"seq": seq, "field": "alt",
                        "reason": format!("{} m is outside the planner's range", fixed(alt, 0))}));
                }
                entry.insert("alt".into(), json!(clamped));
            } else {
                entry.insert("alt".into(), json!(0.0));
            }
            let (lat, lon) = (lat.unwrap_or(0.0), lon.unwrap_or(0.0));
            entry.insert("lat".into(), json!(lat));
            entry.insert("lon".into(), json!(lon));
            last_position = Some((lat, lon));
        }

        for rule in spec.params {
            let Some(slot) = rule.slot else { continue };
            let Some(mut value) = param_slot(raw, slot) else {
                continue;
            };
            if ORBIT_TYPES.contains(&kind) && rule.field == "radius" {
                entry.insert("direction".into(), json!(if value < 0.0 { -1.0 } else { 1.0 }));
                value = value.abs();
                if value == 0.0 {
                    continue;
                }
            }
            let clamped = value.clamp(rule.min, rule.max);
            if clamped != value {
                adjusted.push(json!({"seq": seq, "field": rule.field,
                    "reason": format!("{} is outside the planner's range", py_g(value))}));
            }
            entry.insert(rule.field.into(), json!(clamped));
        }

        if let Some(speed) = pending_speed.take() {
            entry.insert("speed".into(), json!(speed));
        }
        let index = plan_items.len();
        plan_items.push(Value::Object(entry));
        plan_index[seq] = json!(index);
        for slot in pending_slots.drain(..) {
            plan_index[slot] = json!(index);
        }
    }

    let mut raw_plan = Map::new();
    raw_plan.insert("name".into(), json!(name));
    raw_plan.insert("items".into(), Value::Array(plan_items));
    if let Some((lat, lon)) = home {
        raw_plan.insert("home".into(), json!({"lat": lat, "lon": lon}));
    }
    if let Some(speed) = plan_speed {
        raw_plan.insert("speed".into(), json!(speed));
    }
    let (plan, error) = match validate_plan(&Value::Object(raw_plan)) {
        Ok(plan) => (plan, String::new()),
        Err(why) => (Value::Null, why),
    };
    json!({"plan": plan, "plan_index": plan_index, "skipped": skipped, "adjusted": adjusted, "error": error})
}

// -------------------------------------------------------------- distances

/// Great circle distance in metres between two `{"lat","lon"}` points.
pub fn leg_distance_m(a: &Value, b: &Value) -> f64 {
    let get = |v: &Value, k: &str| number(v.get(k)).unwrap_or(0.0);
    let lat1 = get(a, "lat").to_radians();
    let lat2 = get(b, "lat").to_radians();
    let dlat = lat2 - lat1;
    let dlon = (get(b, "lon") - get(a, "lon")).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

/// Ground distance flown along a plan, home included when it names one.
/// Orbits add their own circumference.
pub fn plan_distance_m(plan: &Value) -> f64 {
    let mut points: Vec<&Value> = Vec::new();
    if let Some(home) = plan.get("home").filter(|h| h.is_object()) {
        points.push(home);
    }
    let mut orbit = 0.0;
    for entry in plan
        .get("items")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
    {
        let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
        if !spec_of(kind).is_some_and(|s| s.position) {
            continue;
        }
        points.push(entry);
        if kind == "loiter_turns" {
            orbit += 2.0
                * std::f64::consts::PI
                * number(entry.get("radius")).unwrap_or(0.0)
                * number(entry.get("turns")).unwrap_or(0.0);
        }
    }
    let legs: Vec<f64> = points.windows(2).map(|w| leg_distance_m(w[0], w[1])).collect();
    crate::protocol::common::py_sum(&legs) + orbit
}

// ------------------------------------------------------------- file store

fn plan_path(directory: &Path, name: &str) -> PathBuf {
    directory.join(format!("{name}.json"))
}

/// Saved plans in `directory`, newest first. A missing directory is an empty
/// list; a file that will not parse or validate is skipped.
pub fn list_plans(directory: &Path) -> Vec<Value> {
    let Ok(read) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json") && p.file_stem().is_some())
        .collect();
    paths.sort();
    let mut plans: Vec<(f64, Value)> = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Ok(plan) = validate_plan(&data) else { continue };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64());
        plans.push((
            modified,
            json!({
                "name": path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                "title": plan["name"],
                "items": plan["items"].as_array().map_or(0, Vec::len),
                "distance_m": py_round(plan_distance_m(&plan), 1),
                "modified": modified,
            }),
        ));
    }
    plans.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    plans.into_iter().map(|(_, v)| v).collect()
}

/// Loads and validates one saved plan, None when it cannot be read.
pub fn read_plan(directory: &Path, name: &str) -> Option<Value> {
    let safe = safe_plan_name(&json!(name));
    if safe.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(plan_path(directory, &safe)).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    validate_plan(&data).ok()
}

/// Python's `json.dump(indent=2)`: two spaces, and everything outside ASCII as
/// `\uXXXX`.
pub fn dump_json_ascii(value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// Writes `text` to `target` through a temporary file in the same folder, so a
/// power cut leaves the old file or the new one.
pub fn write_atomic(target: &Path, prefix: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let directory = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)?;
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let temp = directory.join(format!("{prefix}{}-{n}.json", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Saves `plan` as `name`, atomically. Returns the stored name.
pub fn write_plan(directory: &Path, name: &str, plan: &Value) -> std::io::Result<String> {
    let mut safe = safe_plan_name(&json!(name));
    if safe.is_empty() {
        safe = safe_plan_name(plan.get("name").unwrap_or(&Value::Null));
    }
    if safe.is_empty() {
        safe = "Mission".into();
    }
    let text = format!("{}\n", dump_json_ascii(plan));
    write_atomic(&plan_path(directory, &safe), ".mission-", &text)?;
    Ok(safe)
}

/// Deletes one saved plan. Removing what is not there is a success.
pub fn remove_plan(directory: &Path, name: &str) -> bool {
    let safe = safe_plan_name(&json!(name));
    if safe.is_empty() {
        return false;
    }
    match std::fs::remove_file(plan_path(directory, &safe)) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}
