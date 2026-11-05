//! What the connected flight stack can actually do (`python/src/js/capabilities.js`).
//!
//! Corvus speaks PX4 and ArduPilot, and they do not offer the same feature
//! set: ArduPilot has no MAVLink shell, calibrates its ESCs through a
//! parameter, runs its autotune as a flight mode. A control that is offered
//! and then refused is worse than one that is not there, so the backend
//! publishes the answer at `GET /api/mavlink/capabilities` and this module
//! caches it per stack and vehicle: reconnecting to a different aircraft
//! changes the answer. Everything degrades to "allowed": a fetch that fails
//! returns the permissive default, because a page that hides its own controls
//! whenever the network hiccups is worse than one that tries and is refused.
//!
//! A feature a stack lacks is reported, not hidden: `reason` is the sentence
//! to put on the greyed control.

use serde_json::Value;

use crate::net::bg::Pending;
use crate::net::http::{self, Server};

/// MAV_TYPE names of the vehicles that drive or sail. Anything not listed,
/// including a vehicle that is not connected, is treated as an aircraft.
pub const GROUND_TYPES: [&str; 3] = ["GROUNDED_ROVER", "GROUND_ROVER", "SURFACE_BOAT"];

/// Does the connected vehicle drive or sail instead of fly? From the
/// telemetry snapshot, so a bar can relabel itself on the frame the
/// heartbeat arrives rather than after a fetch.
pub fn is_ground(state: &Value) -> bool {
    state.get("connected") == Some(&Value::Bool(true))
        && GROUND_TYPES.contains(&state.get("vehicle_type").and_then(Value::as_str).unwrap_or(""))
}

/// The verb a command on the map is worded with: "Fly" or "Drive".
pub fn move_verb(state: &Value) -> &'static str {
    if is_ground(state) {
        "Drive"
    } else {
        "Fly"
    }
}

/// A feature a control may need. `Shell` and `EscCalibration` are for the
/// controls the plugins and the ESC card will grey the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Feature {
    Shell,
    EscCalibration,
    Calibration(&'static str),
}

/// The capability document.
#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    pub stack: String,
    pub label: String,
    pub shell: bool,
    pub esc_calibration: bool,
    pub autotune: String,
    pub autotune_mode: String,
    pub guided_mode: String,
    pub mission_mode: String,
    pub stick_mode: String,
    pub stick_modes: Vec<String>,
    pub calibrations: Vec<String>,
    pub ground: bool,
}

impl Default for Capabilities {
    fn default() -> Capabilities {
        Capabilities {
            stack: String::new(),
            label: String::new(),
            shell: true,
            esc_calibration: true,
            autotune: "command".into(),
            autotune_mode: String::new(),
            guided_mode: String::new(),
            mission_mode: "MISSION".into(),
            stick_mode: String::new(),
            stick_modes: Vec::new(),
            calibrations: Vec::new(),
            ground: false,
        }
    }
}

impl Capabilities {
    /// The default with what the document says laid over it.
    pub fn from_json(v: &Value) -> Capabilities {
        let d = Capabilities::default();
        let text = |k: &str, d: &str| v.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let flag = |k: &str, d: bool| v.get(k).and_then(Value::as_bool).unwrap_or(d);
        let list = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default()
        };
        Capabilities {
            stack: text("stack", &d.stack),
            label: text("label", &d.label),
            shell: flag("shell", d.shell),
            esc_calibration: flag("esc_calibration", d.esc_calibration),
            autotune: text("autotune", &d.autotune),
            autotune_mode: text("autotune_mode", &d.autotune_mode),
            guided_mode: text("guided_mode", &d.guided_mode),
            mission_mode: text("mission_mode", &d.mission_mode),
            stick_mode: text("stick_mode", &d.stick_mode),
            stick_modes: list("stick_modes"),
            calibrations: list("calibrations"),
            ground: flag("ground", false),
        }
    }

    /// Does this stack offer `name` as a calibration? Unknown (a document
    /// that lists none) is yes.
    pub fn has_calibration(&self, name: &str) -> bool {
        self.calibrations.is_empty() || self.calibrations.iter().any(|c| c == name)
    }

    /// Whether the control for `f` works on this stack.
    pub fn offers(&self, f: Feature) -> bool {
        match f {
            Feature::Shell => self.shell,
            Feature::EscCalibration => self.esc_calibration,
            Feature::Calibration(name) => self.has_calibration(name),
        }
    }

    /// The sentence a greyed control carries: "Not available on this
    /// autopilot" is an answer an operator can act on; a missing button is
    /// not. None when the control works.
    pub fn reason(&self, f: Feature) -> Option<String> {
        if self.offers(f) {
            return None;
        }
        Some("Not available on the connected autopilot.".to_string())
    }

    /// Is `mode` one the sticks fly in?
    pub fn is_stick_mode(&self, mode: &str) -> bool {
        self.stick_modes.iter().any(|m| m == mode)
    }
}

static CURRENT: std::sync::RwLock<Option<Capabilities>> = std::sync::RwLock::new(None);

/// What is cached right now (`Corvus.capabilities.peek()`): the permissive
/// default until a document has come.
pub fn peek() -> Capabilities {
    CURRENT.read().ok().and_then(|c| c.clone()).unwrap_or_default()
}

/// The document for the stack and vehicle now connected, fetched once per
/// pair.
#[derive(Default)]
pub struct Cache {
    key: Option<String>,
    caps: Capabilities,
    load: Option<Pending<(String, Capabilities)>>,
}

/// What the cached document is for: the stack, and which vehicle, since a
/// switch between two vehicles on one stack (a copter and a rover) changes
/// what can be offered as much as a change of stack does.
pub fn cache_key(state: &Value) -> String {
    let s = |k: &str| state.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    format!("{}|{}", s("autopilot_stack"), s("vehicle_id"))
}

impl Cache {
    /// The document for the vehicle in `state`; asks again when the stack or
    /// the vehicle changed. Until the answer comes it is the permissive
    /// default.
    pub fn get(&mut self, ctx: &eframe::egui::Context, server: &Server, state: &Value) -> &Capabilities {
        if let Some((key, caps)) = self.load.as_ref().and_then(Pending::take) {
            self.load = None;
            self.key = Some(key);
            if let Ok(mut c) = CURRENT.write() {
                *c = Some(caps.clone());
            }
            self.caps = caps;
        }
        let want = cache_key(state);
        let connected = state.get("connected") == Some(&Value::Bool(true));
        if connected && self.load.is_none() && self.key.as_deref() != Some(want.as_str()) {
            let s = server.clone();
            let stack = state
                .get("autopilot_stack")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            self.load = Some(Pending::spawn(ctx, move || {
                // Permissive, deliberately, when the fetch fails.
                let caps = match http::get_json(&s, "/api/mavlink/capabilities") {
                    Ok(v) => Capabilities::from_json(&v),
                    Err(_) => Capabilities {
                        stack,
                        ..Capabilities::default()
                    },
                };
                (want, caps)
            }));
        }
        &self.caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REASON: &str = "Not available on the connected autopilot.";

    #[test]
    fn a_rover_and_a_boat_drive_and_everything_else_flies() {
        let st = |t: &str, c: bool| json!({"connected": c, "vehicle_type": t});
        assert!(is_ground(&st("GROUND_ROVER", true)));
        assert!(is_ground(&st("GROUNDED_ROVER", true)));
        assert!(is_ground(&st("SURFACE_BOAT", true)));
        assert!(!is_ground(&st("QUADROTOR", true)));
        assert!(!is_ground(&st("GROUND_ROVER", false)));
        assert!(!is_ground(&json!({})));
        assert_eq!(move_verb(&st("GROUND_ROVER", true)), "Drive");
        assert_eq!(move_verb(&st("FIXED_WING", true)), "Fly");
    }

    #[test]
    fn the_default_allows_everything() {
        let c = Capabilities::default();
        assert!(c.offers(Feature::Shell) && c.offers(Feature::EscCalibration));
        assert!(c.has_calibration("anything"));
        assert_eq!(c.reason(Feature::Shell), None);
        assert_eq!(c.mission_mode, "MISSION");
    }

    #[test]
    fn what_the_document_says_wins() {
        let c = Capabilities::from_json(&json!({
            "stack": "ardupilot", "label": "ArduPilot", "shell": false, "esc_calibration": false,
            "autotune": "mode", "autotune_mode": "AUTOTUNE", "guided_mode": "GUIDED",
            "mission_mode": "AUTO", "stick_mode": "POSHOLD", "stick_modes": ["POSHOLD", "LOITER"],
            "calibrations": ["accel", "compass"], "ground": true
        }));
        assert_eq!(c.stack, "ardupilot");
        assert!(!c.offers(Feature::Shell) && !c.offers(Feature::EscCalibration));
        assert_eq!(c.reason(Feature::Shell), Some(REASON.to_string()));
        assert_eq!(c.reason(Feature::EscCalibration), Some(REASON.to_string()));
        assert!(c.has_calibration("compass") && !c.has_calibration("gyro"));
        assert_eq!(c.reason(Feature::Calibration("gyro")), Some(REASON.to_string()));
        assert!(c.is_stick_mode("LOITER") && !c.is_stick_mode("AUTO"));
        assert_eq!(c.mission_mode, "AUTO");
        assert!(c.ground);
    }

    #[test]
    fn a_stack_that_says_nothing_about_a_feature_offers_it() {
        let c = Capabilities::from_json(&json!({"label": "PX4"}));
        assert_eq!(c.reason(Feature::Shell), None);
    }

    #[test]
    fn the_cache_is_for_a_stack_and_a_vehicle() {
        let a = json!({"autopilot_stack": "px4", "vehicle_id": "main:1"});
        let b = json!({"autopilot_stack": "px4", "vehicle_id": "link-2:1"});
        assert_eq!(cache_key(&a), "px4|main:1");
        assert_ne!(cache_key(&a), cache_key(&b));
        assert_eq!(cache_key(&json!({})), "|");
    }
}
