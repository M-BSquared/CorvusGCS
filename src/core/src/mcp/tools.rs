//! The tools Corvus itself offers an AI client, in the order a client lists
//! them (`builtin_tools` of `mcp.py`): the name, the permission that opens it,
//! its schema and what it calls. A tool never talks to the link: `Run` names an
//! endpoint, and the engine calls it in process.

use std::sync::OnceLock;

use serde_json::{json, Value};

use super::engine::{McpService, ToolError};
use super::settings::CONTROL_MAX_S;
use crate::json::Map;

pub const MAX_PARAMS_LISTED: usize = 300;
pub const CONTROL_DEFAULT_S: f64 = 1.0;

/// What a tool does when it runs.
#[derive(Clone, Copy)]
pub enum Run {
    /// `GET path`, answering `key` of the reply when there is one.
    Get(&'static str, Option<&'static str>),
    /// `POST path` with the body this makes of the arguments.
    Post(&'static str, Option<fn(&Map) -> Map>),
    ListParameters,
    GetParameters,
    SetParameter,
    FlyTo,
    Command,
    Manual,
    Velocity,
    StopControl,
    /// A tool of [`super::more`], which runs its endpoints itself.
    Built(fn(&McpService, &Map) -> Result<Value, ToolError>),
}

pub struct Tool {
    pub name: &'static str,
    pub permission: &'static str,
    pub title: &'static str,
    pub description: String,
    pub properties: Map,
    pub required: &'static [&'static str],
    pub run: Run,
    /// The line the console and the confirmation show; `None` where Python's
    /// summary would raise, which falls back to the tool's name.
    pub summary: fn(&Map) -> Option<String>,
    /// Whether it may be pointed at another vehicle of the fleet.
    pub vehicle: bool,
    pub read_only: bool,
}

pub(super) fn obj(v: Value) -> Map {
    v.as_object().cloned().unwrap_or_default()
}

/// `str(value)` of a JSON value as an f-string shows it.
pub fn fmt(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

fn said(text: &'static str) -> fn(&Map) -> Option<String> {
    // A closure that captures nothing coerces to a fn pointer only when it is
    // written inline, so each text gets its own small function below.
    match text {
        "read the vehicle state" => |_| Some("read the vehicle state".into()),
        "list the vehicles" => |_| Some("list the vehicles".into()),
        "read the capabilities" => |_| Some("read the capabilities".into()),
        "list the flight modes" => |_| Some("list the flight modes".into()),
        "read the mission on board" => |_| Some("read the mission on board".into()),
        "list parameters" => |_| Some("list parameters".into()),
        "download the parameters" => |_| Some("download the parameters".into()),
        "start the mission" => |_| Some("start the mission".into()),
        "clear the mission" => |_| Some("clear the mission".into()),
        "arm" => |_| Some("arm".into()),
        "disarm" => |_| Some("disarm".into()),
        "land" => |_| Some("land".into()),
        "return to launch" => |_| Some("return to launch".into()),
        "hold" => |_| Some("hold".into()),
        "stop and hold" => |_| Some("stop and hold".into()),
        _ => |_| None,
    }
}

fn latlon() -> Map {
    obj(json!({
        "lat": {"type": "number", "minimum": -90, "maximum": 90, "description": "Latitude, degrees"},
        "lon": {"type": "number", "minimum": -180, "maximum": 180, "description": "Longitude, degrees"},
    }))
}

fn plan_schema() -> Value {
    json!({
        "type": "object",
        "description": "A Corvus mission plan. items is the route in order. Each item has a type: takeoff, waypoint, loiter_turns, loiter_time, land or rtl. Every type but rtl has lat, lon and alt (metres above home; land ignores alt). Optional fields: waypoint hold (s) and accept_radius (m); loiter_turns turns, radius (m) and direction (1 clockwise, -1 counterclockwise); loiter_time seconds, radius and direction; takeoff pitch (degrees). Plan level: name, speed (m/s).",
        "properties": {
            "name": {"type": "string"},
            "speed": {"type": "number"},
            "items": {"type": "array", "items": {"type": "object"}},
        },
        "required": ["items"],
    })
}

pub(super) fn get(a: &Map, key: &str) -> Value {
    a.get(key).cloned().unwrap_or(Value::Null)
}

/// The listed arguments, as they are, and nothing else: a tool passes on only
/// what its schema names.
pub(super) fn pick(a: &Map, keys: &[&str]) -> Map {
    keys.iter()
        .filter_map(|k| a.get(*k).map(|v| ((*k).to_string(), v.clone())))
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn tool(
    name: &'static str,
    permission: &'static str,
    title: &'static str,
    description: &str,
    properties: Map,
    required: &'static [&'static str],
    run: Run,
    summary: fn(&Map) -> Option<String>,
    vehicle: bool,
    read_only: bool,
) -> Tool {
    Tool {
        name,
        permission,
        title,
        description: description.to_string(),
        properties,
        required,
        run,
        summary,
        vehicle,
        read_only,
    }
}

fn build() -> Vec<Tool> {
    let none = Map::new;
    let duration = json!({"type": "number", "minimum": 0.2, "maximum": CONTROL_MAX_S, "default": CONTROL_DEFAULT_S});
    vec![
        tool("get_vehicle_state", "telemetry", "Vehicle state",
            "The live state of the vehicle: position, altitudes (amsl, agl), attitude, speeds, battery, GPS, flight mode, armed, landed state, home, mission progress, warnings and pre-arm reasons.",
            none(), &[], Run::Get("/api/state", None), said("read the vehicle state"), true, true),
        tool("list_vehicles", "telemetry", "Vehicles",
            "Every vehicle Corvus hears, its links, and which one the operator flies. The ids are what the vehicle argument of other tools takes.",
            none(), &[], Run::Get("/api/fleet", None), said("list the vehicles"), false, true),
        tool("get_capabilities", "telemetry", "Capabilities",
            "What the connected autopilot can do through Corvus: the stack (PX4 or ArduPilot), hold and velocity modes, and the commands send_command accepts.",
            none(), &[], Run::Get("/api/mavlink/capabilities", None), said("read the capabilities"), true, true),
        tool("list_flight_modes", "telemetry", "Flight modes",
            "The flight mode names the connected firmware accepts, for set_flight_mode.",
            none(), &[], Run::Get("/api/mavlink/modes", Some("modes")), said("list the flight modes"), true, true),
        tool("download_mission", "telemetry", "Mission on board",
            "Read the mission the vehicle holds, as a Corvus plan.",
            none(), &[], Run::Post("/api/mission/download", None), said("read the mission on board"), false, true),
        tool("list_parameters", "params_read", "List parameters",
            &format!("The parameters the vehicle reported, name, value and type, at most {MAX_PARAMS_LISTED} per call. Narrow it with a name prefix such as MPC_."),
            obj(json!({"prefix": {"type": "string", "description": "Only names starting with this"}})), &[],
            Run::ListParameters, said("list parameters"), false, true),
        tool("get_parameters", "params_read", "Read parameters",
            "The current values of the named parameters.",
            obj(json!({"names": {"type": "array", "items": {"type": "string"}}})), &["names"],
            Run::GetParameters,
            |a| {
                let joined = match a.get("names") {
                    Some(Value::Array(list)) => list.iter().map(|v| fmt(Some(v))).collect::<Vec<_>>().join(", "),
                    _ => String::new(),
                };
                Some(format!("read {}", joined.chars().take(120).collect::<String>()))
            },
            false, true),
        tool("download_parameters", "params_read", "Reload parameters",
            "Ask the vehicle for its full parameter list again. Takes a few seconds; list_parameters answers once it is complete.",
            none(), &[], Run::Post("/api/params/download", None), said("download the parameters"), false, true),
        tool("set_parameter", "params_write", "Change a parameter",
            "Write one parameter. The name must be one the vehicle reported.",
            obj(json!({"name": {"type": "string"}, "value": {"type": "number"}})), &["name", "value"],
            Run::SetParameter, |a| Some(format!("set {} to {}", fmt(a.get("name")), fmt(a.get("value")))), false, false),
        tool("upload_mission", "mission", "Upload a mission",
            "Upload a mission plan to the vehicle. With start true it is started too, which arms the vehicle.",
            obj(json!({"plan": plan_schema(), "start": {"type": "boolean", "default": false}})), &["plan"],
            Run::Post("/api/mission/upload", Some(|a| obj(json!({"plan": get(a, "plan"), "start": a.get("start") == Some(&Value::Bool(true))}))) ),
            |a| {
                let plan = match a.get("plan") {
                    Some(Value::Object(p)) => Some(p.clone()),
                    None | Some(Value::Null) => Some(Map::new()),
                    Some(v) if !crate::config::truthy(v) => Some(Map::new()),
                    _ => None,
                }?;
                let n = match plan.get("items") {
                    Some(Value::Array(i)) => i.len(),
                    _ => 0,
                };
                let lead = if a.get("start") == Some(&Value::Bool(true)) {
                    "upload and start a mission of "
                } else {
                    "upload a mission of "
                };
                Some(format!("{lead}{n} items"))
            },
            true, false),
        tool("start_mission", "mission", "Start the mission",
            "Run the mission already on the vehicle. items is how many items it has. This arms the vehicle.",
            obj(json!({"items": {"type": "integer", "minimum": 1}})), &["items"],
            Run::Post("/api/mission/start", Some(|a| obj(json!({"items": get(a, "items")})))), said("start the mission"), true, false),
        tool("clear_mission", "mission", "Clear the mission",
            "Delete the mission stored on the vehicle.",
            none(), &[], Run::Post("/api/mission/clear", None), said("clear the mission"), true, false),
        tool("arm", "arm", "Arm", "Arm the motors.",
            none(), &[], Run::Post("/api/mavlink/arm", Some(|_| obj(json!({"arm": true})))), said("arm"), true, false),
        tool("disarm", "arm", "Disarm", "Disarm the motors. In the air this stops them.",
            none(), &[], Run::Post("/api/mavlink/arm", Some(|_| obj(json!({"arm": false})))), said("disarm"), true, false),
        tool("takeoff", "flight", "Take off",
            "Take off to an altitude above home, in metres. Corvus arms the vehicle in the order the autopilot needs.",
            obj(json!({"altitude": {"type": "number", "description": "Metres above home"}})), &["altitude"],
            Run::Post("/api/mavlink/takeoff", Some(|a| obj(json!({"altitude": get(a, "altitude")})))),
            |a| Some(format!("take off to {} m", fmt(a.get("altitude")))), true, false),
        tool("land", "flight", "Land", "Land where the vehicle is.",
            none(), &[], Run::Post("/api/mavlink/land", None), said("land"), true, false),
        tool("return_to_launch", "flight", "Return", "Fly back to home and land there.",
            none(), &[], Run::Post("/api/mavlink/rtl", None), said("return to launch"), true, false),
        tool("hold", "flight", "Hold", "Stop where the vehicle is and keep it there.",
            none(), &[], Run::Post("/api/mavlink/hold", None), said("hold"), true, false),
        tool("set_flight_mode", "flight", "Flight mode",
            "Switch to a flight mode named by list_flight_modes.",
            obj(json!({"mode": {"type": "string"}})), &["mode"],
            Run::Post("/api/mavlink/mode", Some(|a| obj(json!({"mode": get(a, "mode")})))),
            |a| Some(format!("mode {}", fmt(a.get("mode")))), true, false),
        tool("set_home", "flight", "Set home",
            "Move the home position, where return flies to.",
            latlon(), &["lat", "lon"],
            Run::Post("/api/mavlink/sethome", Some(|a| obj(json!({"lat": get(a, "lat"), "lon": get(a, "lon")})))),
            |a| Some(format!("set home to {}, {}", fmt(a.get("lat")), fmt(a.get("lon")))), true, false),
        tool("fly_to", "goto", "Fly to",
            "Fly to one point, or through several in order. alt is metres above home; a rover or boat ignores it. One point in the air leaves the mission on board alone. From the ground, or with several points, the points replace the mission on board and it is started, which arms the vehicle; an aircraft on the ground takes off.",
            {
                let mut props = latlon();
                props.insert("alt".into(), json!({"type": "number", "description": "Metres above home"}));
                obj(json!({"points": {"type": "array", "minItems": 1, "items": {
                    "type": "object", "properties": props, "required": ["lat", "lon"]}}}))
            },
            &["points"], Run::FlyTo,
            |a| match a.get("points") {
                Some(Value::Array(p)) if p.len() == 1 && p[0].is_object() => {
                    Some(format!("fly to {}, {}", fmt(p[0].get("lat")), fmt(p[0].get("lon"))))
                }
                Some(Value::Array(p)) => Some(format!("fly through {} points", p.len())),
                _ => Some("fly through 0 points".into()),
            },
            true, false),
        tool("manual_control", "control", "Stick input",
            "Hold stick positions for duration_s seconds (MANUAL_CONTROL). x pitch forward, y roll right, r yaw right, each -1 to 1; z throttle 0 to 1, 0.5 holds height in position modes. Without a new call the vehicle holds.",
            obj(json!({
                "x": {"type": "number", "minimum": -1, "maximum": 1},
                "y": {"type": "number", "minimum": -1, "maximum": 1},
                "z": {"type": "number", "minimum": 0, "maximum": 1},
                "r": {"type": "number", "minimum": -1, "maximum": 1},
                "duration_s": duration.clone(),
            })), &[], Run::Manual, |_| Some("stick input".into()), true, false),
        tool("set_velocity", "control", "Velocity",
            "Fly at a velocity for duration_s seconds. vx, vy, vz in m/s: north, east, down, or with frame body forward, right, down. yaw_rate in degrees per second. Without a new call the vehicle holds. It sends setpoints only and never changes the flight mode: start it, then call set_flight_mode with the capabilities' velocity_mode while it runs (PX4 enters OFFBOARD only once setpoints arrive). Never on a plane.",
            obj(json!({
                "vx": {"type": "number"}, "vy": {"type": "number"}, "vz": {"type": "number"},
                "yaw_rate": {"type": "number"},
                "frame": {"type": "string", "enum": ["local", "body"], "default": "local"},
                "duration_s": duration,
            })), &[], Run::Velocity,
            |a| {
                let n = |k: &str| a.get(k).map_or_else(|| "0".to_string(), |v| fmt(Some(v)));
                Some(format!("velocity {}, {}, {} m/s", n("vx"), n("vy"), n("vz")))
            },
            true, false),
        tool("stop_control", "control", "Stop control",
            "End direct control now and put the vehicle in hold.",
            none(), &[], Run::StopControl, said("stop and hold"), true, false),
        tool("send_command", "command", "MAVLink command",
            "Send one MAV_CMD from the capabilities' command list, with up to seven params. A command that names a place takes lat, lon and alt.",
            obj(json!({
                "command": {"type": "string", "description": "e.g. DO_CHANGE_SPEED"},
                "params": {"type": "array", "items": {"type": "number"}, "maxItems": 7},
                "lat": {"type": "number"}, "lon": {"type": "number"},
                "alt": {"type": "number"}, "frame": {"type": "string"},
            })), &["command"], Run::Command, |a| Some(format!("command {}", fmt(a.get("command")))), true, false),
    ]
    .into_iter()
    .chain(tak_tools())
    .chain(super::more::tools())
    .collect()
}

/// The TAK tools: ATAK, CivTAK, WinTAK and a TAK server on one side, the map
/// and the AI client on the other. They call the routes of `crate::tak`.
fn tak_tools() -> Vec<Tool> {
    let none = Map::new;
    let kinds = json!(["multicast", "udp", "tcp", "tls"]);
    vec![
        tool("tak_status", "tak_read", "TAK status",
            "How Corvus stands on the TAK network: its callsign and team, whether it shares the aircraft's position, each connection (kind, host, port, on or off, connected or not, counts) and how many contacts it knows.",
            none(), &[], Run::Get("/api/tak", None), |_| Some("read the TAK status".into()), false, true),
        tool("tak_list_contacts", "tak_read", "TAK contacts",
            "The contacts and markers heard on the TAK network: people, vehicles, aircraft, spot markers and drawings, with callsign, type, affiliation (friend, hostile, neutral, unknown), position, course, speed, battery, remarks and the distance from the aircraft, nearest first. Narrow it with query (callsign, uid or remarks), class, affiliation or within_m.",
            obj(json!({
                "query": {"type": "string", "description": "Part of a callsign, uid or remark"},
                "class": {"type": "string", "enum": ["unit", "point", "route", "drawing", "alert"]},
                "affiliation": {"type": "string", "enum": ["friend", "hostile", "neutral", "unknown"]},
                "within_m": {"type": "number", "minimum": 0, "description": "Only contacts this close to the aircraft (or to near_lat, near_lon), metres"},
                "near_lat": {"type": "number", "minimum": -90, "maximum": 90},
                "near_lon": {"type": "number", "minimum": -180, "maximum": 180},
                "stale": {"type": "boolean", "default": true, "description": "false leaves out contacts that stopped reporting"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200},
            })), &[],
            Run::Post("/api/tak/tracks", Some(|a| pick(a, &["query", "class", "affiliation", "within_m", "near_lat", "near_lon", "stale", "limit"]))),
            |a| Some(match a.get("query") {
                Some(Value::String(q)) if !q.is_empty() => format!("list TAK contacts matching {}", q.chars().take(60).collect::<String>()),
                _ => "list TAK contacts".into(),
            }), false, true),
        tool("tak_get_chat", "tak_read", "TAK chat",
            "The latest chat lines heard on the TAK network (GeoChat): who wrote, to which room, and when.",
            none(), &[], Run::Get("/api/tak/chat", Some("messages")), |_| Some("read the TAK chat".into()), false, true),
        tool("tak_send_marker", "tak_send", "Send a TAK marker",
            "Put a marker on every TAK client that hears a switched on connection that may send: a spot point (type b-m-p-s-p-i) by default, or a CoT atom such as a-h-G (hostile ground) or a-f-G-U-C (friendly ground unit). It stays for stale_s seconds (an hour by default). Returns its uid; send again with the same uid to move it.",
            {
                let mut p = latlon();
                p.insert("callsign".into(), json!({"type": "string", "description": "The label ATAK shows"}));
                p.insert("type".into(), json!({"type": "string", "default": "b-m-p-s-p-i"}));
                p.insert("remarks".into(), json!({"type": "string"}));
                p.insert("hae".into(), json!({"type": "number", "description": "Height above the ellipsoid, metres"}));
                p.insert("stale_s".into(), json!({"type": "number", "minimum": 10, "maximum": 604800, "default": 3600}));
                p.insert("uid".into(), json!({"type": "string", "description": "An uid you sent before, to move that marker"}));
                p
            },
            &["lat", "lon"],
            Run::Post("/api/tak/marker", Some(|a| pick(a, &["lat", "lon", "callsign", "type", "remarks", "hae", "stale_s", "uid"]))),
            |a| Some(format!("send a TAK marker at {}, {}", fmt(a.get("lat")), fmt(a.get("lon")))), false, false),
        tool("tak_send_chat", "tak_send", "Send TAK chat",
            "Write one chat line to everybody on the TAK network (room All Chat Rooms) or to a named room. At most 500 characters.",
            obj(json!({"text": {"type": "string", "maxLength": 500}, "room": {"type": "string"}})), &["text"],
            Run::Post("/api/tak/chat", Some(|a| pick(a, &["text", "room"]))),
            |a| Some(format!("send TAK chat: {}", fmt(a.get("text")).chars().take(80).collect::<String>())), false, false),
        tool("tak_delete_contact", "tak_send", "Delete a TAK contact",
            "Order every TAK client to delete the contact or marker with this uid, and forget it here. Use it for markers Corvus sent; deleting somebody else's contact removes it from all their maps.",
            obj(json!({"uid": {"type": "string"}})), &["uid"],
            Run::Post("/api/tak/delete", Some(|a| pick(a, &["uid"]))),
            |a| Some(format!("delete the TAK contact {}", fmt(a.get("uid")))), false, false),
        tool("tak_set_connection", "tak_setup", "Switch a TAK connection",
            "Switch a connection from tak_status on or off, or allow or forbid it to send (a connection that does not send only listens).",
            obj(json!({"id": {"type": "string"}, "enabled": {"type": "boolean"}, "send": {"type": "boolean"}})), &["id"],
            Run::Post("/api/tak/connection", Some(|a| pick(a, &["id", "enabled", "send"]))),
            |a| Some(match a.get("enabled") {
                Some(Value::Bool(true)) => format!("switch on the TAK connection {}", fmt(a.get("id"))),
                Some(Value::Bool(false)) => format!("switch off the TAK connection {}", fmt(a.get("id"))),
                _ => format!("change the TAK connection {}", fmt(a.get("id"))),
            }), false, false),
        tool("tak_add_connection", "tak_setup", "Add a TAK connection",
            "Add a connection, switched off unless enabled is true. multicast joins a group on the local network (239.2.3.1 port 6969 is what ATAK and WinTAK use to see each other), udp talks to one address, tcp to a TAK server or FreeTAKServer (usually port 8087). tls to a server that needs encryption (usually 8089) works with the system's certificates only; a server with its own CA or a client certificate is set up in the TAK plugin.",
            obj(json!({
                "name": {"type": "string"},
                "kind": {"type": "string", "enum": kinds},
                "host": {"type": "string", "description": "A group address for multicast, otherwise a host name or address"},
                "port": {"type": "integer", "minimum": 1, "maximum": 65535},
                "enabled": {"type": "boolean", "default": false},
                "send": {"type": "boolean", "default": true},
            })), &["kind", "host", "port"],
            Run::Post("/api/tak/connection", Some(|a| pick(a, &["name", "kind", "host", "port", "enabled", "send"]))),
            |a| Some(format!("add a TAK connection to {}:{}", fmt(a.get("host")), fmt(a.get("port")))), false, false),
        tool("tak_remove_connection", "tak_setup", "Remove a TAK connection",
            "Remove a connection from tak_status.",
            obj(json!({"id": {"type": "string"}})), &["id"],
            Run::Post("/api/tak/connection/remove", Some(|a| pick(a, &["id"]))),
            |a| Some(format!("remove the TAK connection {}", fmt(a.get("id")))), false, false),
        tool("tak_configure", "tak_setup", "Configure TAK",
            "Set who Corvus is on the TAK network: callsign, team colour and role, and whether the aircraft's position is shared (share_position) and how often (share_interval_s, 1 to 60). Sharing sends the aircraft's live position to everybody on the switched on connections that may send.",
            obj(json!({
                "callsign": {"type": "string", "maxLength": 32},
                "team": {"type": "string", "enum": ["White", "Yellow", "Orange", "Magenta", "Red", "Maroon", "Purple", "Dark Blue", "Blue", "Cyan", "Teal", "Green", "Dark Green", "Brown"]},
                "role": {"type": "string", "enum": ["Team Member", "Team Lead", "HQ", "Sniper", "Medic", "Forward Observer", "RTO", "K9"]},
                "share_position": {"type": "boolean"},
                "share_interval_s": {"type": "number", "minimum": 1, "maximum": 60},
            })), &[],
            Run::Post("/api/tak/settings", Some(|a| pick(a, &["callsign", "team", "role", "share_position", "share_interval_s"]))),
            |a| Some(match a.get("share_position") {
                Some(Value::Bool(true)) => "share the aircraft's position on TAK".into(),
                Some(Value::Bool(false)) => "stop sharing the aircraft's position on TAK".into(),
                _ => "change the TAK identity".into(),
            }), false, false),
    ]
}

/// The tools added since the Python program's list was frozen, for the tests
/// that hold the rest to it.
pub fn is_extension(name: &str) -> bool {
    super::more::NAMES.contains(&name) || name.starts_with("tak_")
}

static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();

/// Every tool Corvus itself offers, in the order a client lists them.
pub fn builtin() -> &'static [Tool] {
    TOOLS.get_or_init(build)
}

pub fn find(name: &str) -> Option<&'static Tool> {
    builtin().iter().find(|t| t.name == name)
}
