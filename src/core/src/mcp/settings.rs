//! The operator's MCP switches (`coerce_settings`, `effective_settings`,
//! `compact_settings`, `level` and `PERMISSIONS` of `mcp.py`).
//!
//! The `mcp` block of the config file keeps only what differs from the
//! defaults; booleans only, so a hand edited `"true"` reads as not set and never
//! as an accidental yes to flying.

use serde_json::{json, Value};

use crate::json::Map;
use crate::plugins::registry::is_valid_id;

/// One switch on the MCP card. `writes` is whether it changes anything: only
/// those have an "ask the operator first" switch and only those are taken
/// away by Stop.
#[derive(Debug, Clone, Copy)]
pub struct Permission {
    pub id: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub writes: bool,
    pub default_on: bool,
    pub default_confirm: bool,
}

/// The longest one direct control call may last, in seconds.
pub const CONTROL_MAX_S: f64 = 5.0;

/// The permissions added after the Python program's catalog was frozen. The
/// fixtures hold the first ten and nothing else, so tests compare those.
pub const EXTENSIONS: [&str; 6] = ["tak_read", "tak_send", "tak_setup", "map", "geofence", "camera"];

/// The switches that are not a permission, with what they are when the file
/// does not say. `assistant` lets the AI Chat plugin of this Corvus sign in
/// while the server for other programs is off, `assistant_ask` has Corvus ask
/// the operator, every time, for what is switched off below.
const SWITCHES: [(&str, bool); 4] = [
    ("enabled", false),
    ("fleet", false),
    ("assistant", true),
    ("assistant_ask", true),
];

pub const PERMISSIONS: [Permission; 16] = [
    Permission {
        id: "telemetry",
        label: "Read telemetry and status",
        hint: "Position, attitude, battery, flight mode, warnings, the vehicle list and the mission on board.",
        writes: false,
        default_on: true,
        default_confirm: false,
    },
    Permission {
        id: "params_read",
        label: "Read parameters",
        hint: "The parameter list the vehicle reported, and a fresh download of it.",
        writes: false,
        default_on: true,
        default_confirm: false,
    },
    Permission {
        id: "params_write",
        label: "Change parameters",
        hint: "Write one parameter at a time. Only names the connected firmware reported are accepted.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "mission",
        label: "Missions",
        hint: "Upload, start and clear a mission. Starting a mission arms the vehicle.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "arm",
        label: "Arm and disarm",
        hint: "Disarming in the air stops the motors.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "flight",
        label: "Takeoff, land, return, hold and flight mode",
        hint: "Also moves the home position.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "goto",
        label: "Fly to a point",
        hint: "Fly to one point or through several, at a height above home. From the ground or with several points this replaces the mission on board, arms the vehicle and takes off.",
        writes: true,
        default_on: false,
        default_confirm: false,
    },
    Permission {
        id: "control",
        label: "Direct control",
        hint: "Stick inputs and velocity setpoints. Each lasts at most 5 s; without a new one the vehicle holds.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "command",
        label: "Other MAVLink commands",
        hint: "One command from the list the capabilities name. Arming, takeoff, mode changes, calibration and flight termination are never on it.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "plugins",
        label: "Plugin tools",
        hint: "Tools the installed plugins add, for the plugins switched on below. Each plugin has its own switches too.",
        writes: true,
        default_on: false,
        default_confirm: false,
    },
    Permission {
        id: "tak_read",
        label: "Read TAK contacts and chat",
        hint: "The people, vehicles and markers heard on the TAK network (ATAK, CivTAK, WinTAK), their chat, and the state of the connections. Other people's positions: off until you allow it.",
        writes: false,
        default_on: false,
        default_confirm: false,
    },
    Permission {
        id: "tak_send",
        label: "Send to TAK",
        hint: "Put markers and chat lines on the TAK network and delete a contact for everybody on it. Whoever is on a switched on connection sees it.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "tak_setup",
        label: "Set up TAK",
        hint: "Add, switch on and remove connections to TAK networks and servers, and decide whether the aircraft's position is shared on them.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "map",
        label: "Draw on the map",
        hint: "Lines, arrows, areas, circles, markers and text on the Home map, moving the map there, a message to you, and the place search, which sends the searched name to OpenStreetMap. Nothing of it reaches the vehicle.",
        writes: false,
        default_on: true,
        default_confirm: false,
    },
    Permission {
        id: "geofence",
        label: "Change the geofence",
        hint: "Replace the area of the geofence here and on the vehicle, or take it off the vehicle.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
    Permission {
        id: "camera",
        label: "Camera and gimbal",
        hint: "Look at a point, aim the gimbal, take photos. A multicopter without a gimbal may turn to look at a point.",
        writes: true,
        default_on: false,
        default_confirm: true,
    },
];

pub fn permission(id: &str) -> Option<&'static Permission> {
    PERMISSIONS.iter().find(|p| p.id == id)
}

/// The `mcp` block, keeping only what is well formed; `None` when nothing is.
pub fn coerce_settings(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for (key, _) in SWITCHES {
        if let Some(Value::Bool(b)) = raw.get(key) {
            out.insert(key.into(), (*b).into());
        }
    }
    for key in ["permissions", "confirm"] {
        if let Some(Value::Object(block)) = raw.get(key) {
            let kept: Map = block
                .iter()
                .filter(|(k, v)| permission(k).is_some() && v.is_boolean())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !kept.is_empty() {
                out.insert(key.into(), Value::Object(kept));
            }
        }
    }
    if let Some(Value::Object(plugins)) = raw.get("plugins") {
        let kept: Map = plugins
            .iter()
            .filter(|(k, v)| is_valid_id(k) && v.is_boolean())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !kept.is_empty() {
            out.insert("plugins".into(), Value::Object(kept));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The settings in force: what the file says, the defaults for the rest.
pub fn effective_settings(raw: Option<&Value>) -> Map {
    let saved = raw.and_then(coerce_settings).unwrap_or_default();
    let block = |key: &str| saved.get(key).and_then(Value::as_object).cloned().unwrap_or_default();
    let (perms, confirm) = (block("permissions"), block("confirm"));
    let mut permissions = Map::new();
    let mut confirms = Map::new();
    for p in &PERMISSIONS {
        permissions.insert(
            p.id.into(),
            perms.get(p.id).and_then(Value::as_bool).unwrap_or(p.default_on).into(),
        );
        let c = if p.writes {
            confirm.get(p.id).and_then(Value::as_bool).unwrap_or(p.default_confirm)
        } else {
            false
        };
        confirms.insert(p.id.into(), c.into());
    }
    let mut out = Map::new();
    for (key, default) in SWITCHES {
        out.insert(key.into(), saved.get(key).cloned().unwrap_or(json!(default)));
    }
    out.insert("permissions".into(), Value::Object(permissions));
    out.insert("confirm".into(), Value::Object(confirms));
    out.insert("plugins".into(), Value::Object(block("plugins")));
    out
}

/// `settings` without what equals the defaults, for a lean config file.
pub fn compact_settings(settings: &Map) -> Option<Map> {
    let defaults = effective_settings(None);
    let mut out = Map::new();
    for (key, _) in SWITCHES {
        if settings.get(key) != defaults.get(key) {
            out.insert(key.into(), settings.get(key).cloned().unwrap_or(Value::Null));
        }
    }
    for key in ["permissions", "confirm"] {
        let default = defaults[key].as_object().cloned().unwrap_or_default();
        let changed: Map = settings
            .get(key)
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter(|(k, v)| default.get(*k) != Some(*v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        if !changed.is_empty() {
            out.insert(key.into(), Value::Object(changed));
        }
    }
    if let Some(Value::Object(p)) = settings.get("plugins") {
        if !p.is_empty() {
            out.insert("plugins".into(), Value::Object(p.clone()));
        }
    }
    coerce_settings(&Value::Object(out))
}

/// `"off"`, `"confirm"` or `"allow"` for one permission.
pub fn level(settings: &Map, permission: &str) -> &'static str {
    let on = |key: &str| {
        settings
            .get(key)
            .and_then(|b| b.get(permission))
            .and_then(Value::as_bool)
            == Some(true)
    };
    if !on("permissions") {
        "off"
    } else if on("confirm") {
        "confirm"
    } else {
        "allow"
    }
}

/// Whether a switch that is not a permission is on.
pub fn switched(settings: &Map, key: &str) -> bool {
    settings.get(key) == Some(&Value::Bool(true))
}

/// The names of those switches, for a patch of the settings.
pub fn switches() -> impl Iterator<Item = &'static str> {
    SWITCHES.iter().map(|(k, _)| *k)
}

/// [`level`] for the AI Chat plugin: the operator is at the window it runs in,
/// so what is off is asked for instead of refused, as long as they allow that.
pub fn assistant_level(settings: &Map, permission: &str) -> &'static str {
    match level(settings, permission) {
        "off" if switched(settings, "assistant_ask") => "confirm",
        other => other,
    }
}

/// Whether a plugin's tools are offered: the plugin switch, default on.
pub fn plugin_allowed(settings: &Map, plugin_id: &str) -> bool {
    settings.get("plugins").and_then(|p| p.get(plugin_id)) != Some(&Value::Bool(false))
}
