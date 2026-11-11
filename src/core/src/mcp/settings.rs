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

pub const PERMISSIONS: [Permission; 10] = [
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
        hint: "Fly to one point or through several, at a height above home.",
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
];

pub fn permission(id: &str) -> Option<&'static Permission> {
    PERMISSIONS.iter().find(|p| p.id == id)
}

/// The `mcp` block, keeping only what is well formed; `None` when nothing is.
pub fn coerce_settings(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for key in ["enabled", "fleet"] {
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
    out.insert("enabled".into(), saved.get("enabled").cloned().unwrap_or(json!(false)));
    out.insert("fleet".into(), saved.get("fleet").cloned().unwrap_or(json!(false)));
    out.insert("permissions".into(), Value::Object(permissions));
    out.insert("confirm".into(), Value::Object(confirms));
    out.insert("plugins".into(), Value::Object(block("plugins")));
    out
}

/// `settings` without what equals the defaults, for a lean config file.
pub fn compact_settings(settings: &Map) -> Option<Map> {
    let defaults = effective_settings(None);
    let mut out = Map::new();
    for key in ["enabled", "fleet"] {
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

/// Whether a plugin's tools are offered: the plugin switch, default on.
pub fn plugin_allowed(settings: &Map, plugin_id: &str) -> bool {
    settings.get("plugins").and_then(|p| p.get(plugin_id)) != Some(&Value::Bool(false))
}
