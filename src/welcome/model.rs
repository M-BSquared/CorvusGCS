//! The first start setup's rules (`choicesFrom` and `configPatch` of
//! `python/src/js/welcome.js`): what the steps start from, and the one
//! `POST /api/config` body Finish writes.

use serde_json::{json, Value};

use crate::settings::model;

/// The four steps after the start page.
pub const STEPS: [(&str, &str); 4] = [
    ("look", "Look"),
    ("units", "Units"),
    ("workspace", "Workspace"),
    ("controls", "Controls"),
];

/// What the operator chose so far.
#[derive(Debug, Clone, PartialEq)]
pub struct Choices {
    pub language: String,
    pub theme: String,
    pub units: [String; 4],
    pub sensitivity: String,
    pub mission_page: bool,
    pub betaflight_always: bool,
    pub console_tab: bool,
    pub ssh_tab: bool,
    pub virtual_joystick: bool,
    pub gamepad: bool,
    pub arrow_keys: bool,
    pub wasd_keys: bool,
    pub updates: bool,
}

fn controls(cfg: &Value, key: &str) -> bool {
    model::flag(cfg, "controls", key, false)
}

/// What the steps start from: the config where it says something, and what is
/// on screen where it does not.
pub fn choices_from(cfg: &Value) -> Choices {
    Choices {
        language: cfg
            .get("language")
            .and_then(Value::as_str)
            .filter(|l| !l.is_empty())
            .unwrap_or("en")
            .to_string(),
        theme: crate::ui::style::theme_from_config(cfg).to_string(),
        units: model::units_of(cfg),
        sensitivity: model::review_of(cfg).to_string(),
        mission_page: model::flag(cfg, "ui", "mission_page", false),
        betaflight_always: model::flag(cfg, "ui", "betaflight_always", false),
        console_tab: model::flag(cfg, "ui", "console_tab", true),
        ssh_tab: model::flag(cfg, "ui", "ssh_tab", true),
        virtual_joystick: controls(cfg, "virtual_joystick"),
        gamepad: controls(cfg, "gamepad"),
        arrow_keys: controls(cfg, "arrow_keys"),
        wasd_keys: controls(cfg, "wasd_keys"),
        updates: !(cfg.get("updates").and_then(|u| u.get("check")) == Some(&Value::Bool(false))),
    }
}

/// The `POST /api/config` body for `c`. `ui`, `controls`, `review` and
/// `updates` are merged per key by the backend, so this sets exactly these
/// and leaves the rest, the interface size and the key strength included.
pub fn config_patch(c: &Choices) -> Value {
    json!({
        "language": c.language,
        "theme": {"name": c.theme},
        "ui": {
            "units": model::units_value(&c.units),
            "mission_page": c.mission_page,
            "betaflight_always": c.betaflight_always,
            "console_tab": c.console_tab,
            "ssh_tab": c.ssh_tab,
        },
        "controls": {
            "virtual_joystick": c.virtual_joystick,
            "gamepad": c.gamepad,
            "arrow_keys": c.arrow_keys,
            "wasd_keys": c.wasd_keys,
        },
        "review": {"sensitivity": c.sensitivity},
        "updates": {"check": c.updates},
    })
}

/// The config as the screen should show it while the steps are filled in:
/// everything but the Mission and Betaflight entries, which appear on Finish.
pub fn preview(base: &Value, c: &Choices) -> Value {
    let mut cfg = base.clone();
    let mut patch = config_patch(c);
    if let Some(ui) = patch.get_mut("ui").and_then(Value::as_object_mut) {
        ui.remove("mission_page");
        ui.remove("betaflight_always");
    }
    model::merge(&mut cfg, &patch);
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_station_with_nothing_set_starts_from_the_defaults() {
        let c = choices_from(&json!({}));
        assert_eq!(c.language, "en");
        assert_eq!(c.theme, "light");
        assert_eq!(c.sensitivity, "normal");
        assert!(!c.mission_page && c.console_tab && c.ssh_tab && c.updates);
        assert!(
            !c.betaflight_always,
            "Betaflight follows the aircraft unless asked otherwise"
        );
        assert!(!c.virtual_joystick && !c.gamepad && !c.arrow_keys && !c.wasd_keys);
    }

    #[test]
    fn what_the_config_says_wins() {
        let c = choices_from(&json!({
            "language": "de",
            "theme": {"name": "blue"},
            "ui": {"mission_page": true, "betaflight_always": true, "console_tab": false, "ssh_tab": false},
            "controls": {"virtual_joystick": true, "wasd_keys": true},
            "review": {"sensitivity": "strict"},
            "updates": {"check": false},
        }));
        assert_eq!(c.language, "de");
        assert_eq!(c.theme, "blue");
        assert!(c.mission_page && c.betaflight_always && !c.console_tab && !c.ssh_tab);
        assert!(c.virtual_joystick && c.wasd_keys && !c.gamepad && !c.arrow_keys);
        assert_eq!(c.sensitivity, "strict");
        assert!(!c.updates);
    }

    #[test]
    fn an_unknown_sensitivity_is_normal() {
        assert_eq!(
            choices_from(&json!({"review": {"sensitivity": "wild"}})).sensitivity,
            "normal"
        );
    }

    #[test]
    fn finish_writes_exactly_these_keys() {
        let mut c = choices_from(&json!({}));
        c.language = "fr".into();
        c.theme = "pink".into();
        c.mission_page = true;
        c.betaflight_always = true;
        c.ssh_tab = false;
        c.arrow_keys = true;
        c.updates = false;
        c.sensitivity = "sensitive".into();
        let p = config_patch(&c);
        assert_eq!(p["language"], "fr");
        assert_eq!(p["theme"], json!({"name": "pink"}));
        assert_eq!(p["ui"]["mission_page"], true);
        assert_eq!(p["ui"]["betaflight_always"], true);
        assert_eq!(p["ui"]["console_tab"], true);
        assert_eq!(p["ui"]["ssh_tab"], false);
        assert_eq!(p["ui"]["units"], model::units_value(&c.units));
        assert_eq!(
            p["controls"],
            json!({"virtual_joystick": false, "gamepad": false, "arrow_keys": true, "wasd_keys": false})
        );
        assert_eq!(p["review"], json!({"sensitivity": "sensitive"}));
        assert_eq!(p["updates"], json!({"check": false}));
        // The interface size and the key strength are not this dialog's.
        assert!(p["ui"].get("scale").is_none());
        assert!(p["controls"].get("key_gain").is_none());
        let mut keys: Vec<&String> = p.as_object().expect("an object").keys().collect();
        keys.sort();
        assert_eq!(keys, ["controls", "language", "review", "theme", "ui", "updates"]);
    }

    #[test]
    fn the_preview_leaves_the_mission_and_betaflight_entries_for_finish() {
        let base = json!({"ui": {"scale": 1.25, "mission_page": false, "betaflight_always": false}});
        let mut c = choices_from(&base);
        c.mission_page = true;
        c.betaflight_always = true;
        c.theme = "green".into();
        let cfg = preview(&base, &c);
        assert_eq!(cfg["ui"]["mission_page"], false);
        assert_eq!(cfg["ui"]["betaflight_always"], false);
        assert_eq!(cfg["ui"]["scale"], 1.25);
        assert_eq!(cfg["theme"]["name"], "green");
    }
}
