//! The sensor pages of Safety & Sensors (python/src/js/setup-safety.js: the sensor
//! view, `sensorHeader`, the hardware presets, the parameters the operator
//! adds by name).
//!
//! This file is the model and the rules: what the backend describes for a
//! sensor, which writes a switch or a preset makes and in what order, and the
//! list of extra parameters. `sensor_view.rs` draws it; `safety.rs` owns the
//! requests.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde_json::Value;

/// `CUSTOM_PRESET`: the entry that is not a product.
pub const CUSTOM: &str = "custom";
/// `EXTRA_LIMIT`: at most this many operator-added names are read.
pub const EXTRA_LIMIT: usize = 24;

/// One entry of a clearing list: a plain name is written to 0, an object
/// names its own value and, for a port being freed, the port it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub enum Clear {
    Name(String),
    Param {
        param: String,
        value: f64,
        port: Option<f64>,
    },
}

/// One model the picker offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Driver {
    pub id: String,
    pub label: String,
    pub blocked: String,
    pub serial: bool,
    pub wiring: String,
    pub param: Option<String>,
    pub value: Option<f64>,
    pub port_clear: Vec<Clear>,
    pub port_writes: Vec<(String, f64)>,
    pub extra: Vec<(String, f64)>,
}

/// One serial port the picker offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub value: String,
    pub label: String,
    pub blocked: String,
}

/// The switch of a sensor section.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Toggle {
    pub label: String,
    pub detail: String,
    pub enabled: bool,
    pub problems: Vec<String>,
    pub note: String,
    pub drivers: Vec<Driver>,
    pub selected: String,
    pub ports: Vec<Port>,
    pub port: Option<String>,
    pub clear: Vec<Clear>,
    pub enable: Vec<(String, f64)>,
    pub disable: Vec<(String, f64)>,
    pub reboot: bool,
}

/// One write a preset makes.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetWrite {
    pub param: String,
    pub value: Option<f64>,
    pub label: String,
    pub port: bool,
    pub port_param: bool,
}

/// A hardware preset: a product, with the whole setup it needs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Preset {
    pub id: String,
    pub label: String,
    pub bus: String,
    pub model: String,
    pub summary: String,
    pub wiring: String,
    pub note: String,
    pub supported: bool,
    pub unsupported: String,
    pub serial: bool,
    pub active: bool,
    pub reboot: bool,
    pub missing: Vec<String>,
    pub writes: Vec<PresetWrite>,
    pub kept: Vec<(String, f64, String)>,
    pub clear: Option<Vec<Clear>>,
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn pairs(v: Option<&Value>) -> Vec<(String, f64)> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|w| Some((w.get("param")?.as_str()?.to_string(), w.get("value")?.as_f64()?)))
                .collect()
        })
        .unwrap_or_default()
}

fn clears(v: Option<&Value>) -> Vec<Clear> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| match c {
                    Value::String(s) => Some(Clear::Name(s.clone())),
                    Value::Object(_) => Some(Clear::Param {
                        param: c.get("param")?.as_str().filter(|p| !p.is_empty())?.to_string(),
                        value: c.get("value").and_then(Value::as_f64).unwrap_or(0.0),
                        port: c.get("port").and_then(Value::as_f64),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A port's value as the picker keys it: `3`, not `3.0`.
fn key(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(match n.as_i64() {
            Some(i) => i.to_string(),
            None => n.to_string(),
        }),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Reads the `toggle` of a sensor section.
pub fn parse_toggle(t: &Value) -> Toggle {
    Toggle {
        label: text(t, "label"),
        detail: text(t, "detail"),
        enabled: t.get("enabled") == Some(&Value::Bool(true)),
        problems: t
            .get("problems")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        note: text(t, "note"),
        drivers: t
            .get("drivers")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|d| Driver {
                        id: d.get("id").and_then(key).unwrap_or_default(),
                        label: text(d, "label"),
                        blocked: text(d, "blocked"),
                        serial: d.get("serial") == Some(&Value::Bool(true)),
                        wiring: text(d, "wiring"),
                        param: d.get("param").and_then(Value::as_str).map(String::from),
                        value: d.get("value").and_then(Value::as_f64),
                        port_clear: clears(d.get("port_clear")),
                        port_writes: pairs(d.get("port_writes")),
                        extra: pairs(d.get("extra")),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        selected: t.get("selected").and_then(key).unwrap_or_default(),
        ports: t
            .get("ports")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|p| Port {
                        value: p.get("value").and_then(key).unwrap_or_default(),
                        label: text(p, "label"),
                        blocked: text(p, "blocked"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        port: t.get("port").and_then(key),
        clear: clears(t.get("clear")),
        enable: pairs(t.get("enable")),
        disable: pairs(t.get("disable")),
        reboot: t.get("reboot") == Some(&Value::Bool(true)),
    }
}

/// Reads the `presets` of a sensor section.
pub fn parse_presets(v: &Value) -> Vec<Preset> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| Preset {
                    id: p.get("id").and_then(key).unwrap_or_default(),
                    label: text(p, "label"),
                    bus: text(p, "bus"),
                    model: text(p, "model"),
                    summary: text(p, "summary"),
                    wiring: text(p, "wiring"),
                    note: text(p, "note"),
                    supported: p.get("supported") != Some(&Value::Bool(false)),
                    unsupported: text(p, "unsupported"),
                    serial: p.get("serial") == Some(&Value::Bool(true)),
                    active: p.get("active") == Some(&Value::Bool(true)),
                    reboot: p.get("reboot") == Some(&Value::Bool(true)),
                    missing: p
                        .get("missing")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                    writes: p
                        .get("writes")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .map(|w| PresetWrite {
                                    param: text(w, "param"),
                                    value: w.get("value").and_then(Value::as_f64),
                                    label: text(w, "label"),
                                    port: w.get("port") == Some(&Value::Bool(true)),
                                    port_param: w.get("port_param") == Some(&Value::Bool(true)),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    kept: p
                        .get("kept")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .map(|w| {
                                    (
                                        text(w, "param"),
                                        w.get("value").and_then(Value::as_f64).unwrap_or(0.0),
                                        text(w, "label"),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    clear: p.get("clear").filter(|c| c.is_array()).map(|c| clears(Some(c))),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `presetOptionLabel`: the bus belongs in the label, and a module this
/// firmware cannot run or one already running says so there.
pub fn preset_option_label(p: &Preset) -> String {
    let parts = [p.bus.as_str(), p.model.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    let mut label = p.label.clone();
    if !parts.is_empty() {
        label += &format!(" \u{b7} {parts}");
    }
    if !p.supported {
        label += "  (cannot be set up here)";
    } else if p.active {
        label += "  (in use)";
    }
    label
}

/// `portParam`: a parameter name with the SERIAL port number filled in.
pub fn port_param(template: &str, port: &str) -> String {
    template.replace("{port}", port)
}

/// `portProblem`: why the serial port value cannot take this sensor, or "".
pub fn port_problem(t: &Toggle, value: &str) -> String {
    t.ports
        .iter()
        .find(|p| p.value == value)
        .map(|p| p.blocked.clone())
        .unwrap_or_default()
}

/// `clearWrites`: the zeroing writes that come first in a chain. The port the
/// operator just picked is never freed again.
pub fn clear_writes(list: &[Clear], port: Option<f64>) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for c in list {
        match c {
            Clear::Name(n) => out.push((n.clone(), 0.0)),
            Clear::Param { param, value, port: p } => {
                if p.is_some() && *p == port {
                    continue;
                }
                out.push((param.clone(), *value));
            }
        }
    }
    out
}

/// `driverWrites`: everything one picker entry writes to start its driver,
/// in order: free the other ports on its protocol, set its own port, the
/// prerequisites it carries, and the driver last.
pub fn driver_writes(d: &Driver, port: Option<f64>, port_text: &str) -> Vec<(String, f64)> {
    let mut out = clear_writes(&d.port_clear, port);
    for (p, v) in &d.port_writes {
        out.push((port_param(p, port_text), *v));
    }
    out.extend(d.extra.iter().cloned());
    if let Some(p) = &d.param {
        out.push((p.clone(), d.value.or(port).unwrap_or(f64::NAN)));
    }
    out
}

/// `applySensor`: the whole chain behind the switch, or why it cannot run.
/// `port_text` is the port picker's value, `""` when nothing is picked.
pub fn sensor_writes(
    t: &Toggle,
    driver: Option<&Driver>,
    port_text: &str,
    on: bool,
) -> Result<Vec<(String, f64)>, String> {
    let driver = if on { driver } else { None };
    if let Some(d) = driver {
        if !d.blocked.is_empty() {
            return Err(d.blocked.clone());
        }
    }
    let serial = driver.is_some_and(|d| d.serial);
    let port = if serial { port_text.parse::<f64>().ok() } else { None };
    if serial && port.is_none() {
        return Err("pick the serial port first".into());
    }
    if serial {
        let taken = port_problem(t, port_text);
        if !taken.is_empty() {
            return Err(taken);
        }
    }
    let mut writes: Vec<(String, f64)> = clear_writes(&t.clear, port)
        .into_iter()
        .filter(|(n, _)| !driver.is_some_and(|d| d.param.as_deref() == Some(n.as_str())))
        .collect();
    if let Some(d) = driver {
        writes.extend(driver_writes(d, port, port_text));
    }
    writes.extend(if on { t.enable.clone() } else { t.disable.clone() });
    Ok(writes)
}

/// `applyPreset`: everything the preset writes, or why it cannot be applied.
pub fn preset_writes(t: &Toggle, p: &Preset, port_text: &str) -> Result<Vec<(String, f64)>, String> {
    if !p.supported {
        return Err(if p.unsupported.is_empty() {
            "not supported".into()
        } else {
            p.unsupported.clone()
        });
    }
    let port = if p.serial { port_text.parse::<f64>().ok() } else { None };
    if p.serial && port.is_none() {
        return Err("pick the serial port first".into());
    }
    if p.serial {
        let taken = port_problem(t, port_text);
        if !taken.is_empty() {
            return Err(taken);
        }
    }
    let own: Vec<(String, f64)> = p
        .writes
        .iter()
        .map(|w| {
            (
                if w.port_param {
                    port_param(&w.param, port_text)
                } else {
                    w.param.clone()
                },
                if w.port {
                    port.unwrap_or(f64::NAN)
                } else {
                    w.value.unwrap_or(0.0)
                },
            )
        })
        .collect();
    let clear = p.clear.as_deref().unwrap_or(&t.clear);
    let mut writes: Vec<(String, f64)> = clear_writes(clear, port)
        .into_iter()
        .filter(|(n, _)| !own.iter().any(|(o, _)| o == n))
        .collect();
    writes.extend(own);
    Ok(writes)
}

/// `EXTRA_NAME_RE`: `^[A-Z][A-Z0-9_]{0,15}$`.
pub fn valid_extra_name(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=16).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// The parameters the operator added by name, per sensor section. They are
/// kept in `~/.corvus/safety-extra.json`: they describe how this operator
/// works on their airframe, not something the aircraft can be asked about.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extras {
    map: BTreeMap<String, Vec<String>>,
    path: Option<PathBuf>,
}

impl Extras {
    /// The list from the default file; an unreadable one is an empty list.
    pub fn load_default() -> Extras {
        let path = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus").join("safety-extra.json"));
        match path {
            Some(p) => Extras::load(p),
            None => Extras::default(),
        }
    }

    /// `readExtra`: names that are not parameter names are dropped.
    pub fn load(path: PathBuf) -> Extras {
        let mut map = BTreeMap::new();
        let raw: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        if let Some(o) = raw.as_object() {
            for (k, v) in o {
                if let Some(a) = v.as_array() {
                    let names: Vec<String> = a
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|n| valid_extra_name(n))
                        .map(String::from)
                        .collect();
                    map.insert(k.clone(), names);
                }
            }
        }
        Extras { map, path: Some(path) }
    }

    fn save(&self) {
        let Some(p) = &self.path else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let v: serde_json::Map<String, Value> = self
            .map
            .iter()
            .filter(|(_, n)| !n.is_empty())
            .map(|(k, n)| (k.clone(), Value::from(n.clone())))
            .collect();
        let _ = std::fs::write(p, Value::Object(v).to_string());
    }

    /// `extraNames`.
    pub fn names(&self, section: &str) -> &[String] {
        self.map.get(section).map_or(&[], Vec::as_slice)
    }

    /// `allExtraNames`: every name on every sensor, once, capped.
    pub fn all(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for names in self.map.values() {
            for n in names {
                if !seen.contains(n) {
                    seen.push(n.clone());
                }
            }
        }
        seen.truncate(EXTRA_LIMIT);
        seen
    }

    /// `extraProblem`: why this name cannot be added to the section, or "".
    /// `rendered` maps a parameter to the card it is already on.
    pub fn problem(&self, section: &str, name: &str, rendered: &HashMap<String, String>) -> String {
        if name.is_empty() {
            return "type a parameter name".into();
        }
        if !valid_extra_name(name) {
            return "not a parameter name: letters, digits and _, up to 16 characters".into();
        }
        if self.names(section).iter().any(|n| n == name) {
            return "already added".into();
        }
        if let Some(card) = rendered.get(name) {
            return format!("already on this page, under {card}");
        }
        if self.all().len() >= EXTRA_LIMIT {
            return format!("at most {EXTRA_LIMIT} extra parameters");
        }
        String::new()
    }

    pub fn add(&mut self, section: &str, name: &str) {
        self.map.entry(section.to_string()).or_default().push(name.to_string());
        self.save();
    }

    pub fn remove(&mut self, section: &str, name: &str) {
        if let Some(v) = self.map.get_mut(section) {
            v.retain(|n| n != name);
            if v.is_empty() {
                self.map.remove(section);
            }
        }
        self.save();
    }

    /// `pruneExtra`: a name the page shows on a card of its own is not kept
    /// as an extra as well.
    pub fn prune(&mut self, rendered: &HashMap<String, String>) {
        let mut changed = false;
        for names in self.map.values_mut() {
            let before = names.len();
            names.retain(|n| !rendered.contains_key(n));
            changed |= names.len() != before;
        }
        self.map.retain(|_, v| !v.is_empty());
        if changed {
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn range() -> Toggle {
        parse_toggle(&json!({
            "label": "Distance sensor", "enabled": false, "state": "off",
            "drivers": [
                {"id": "lw", "label": "Lightware", "serial": true, "param": "SENS_EN_SF1XX", "value": 1,
                 "port_clear": [{"param": "SER_GPS1_BAUD", "value": 0, "port": 1}, {"param": "SER_TEL2_BAUD", "value": 0, "port": 2}],
                 "port_writes": [{"param": "SERIAL{port}_PROTOCOL", "value": 9}],
                 "extra": [{"param": "CAN_EN", "value": 1}]},
                {"id": "i2c", "label": "I2C lidar", "param": "SENS_EN_LL40LS", "value": 2},
                {"id": "ext", "label": "External / MAVLink", "param": null},
                {"id": "bad", "label": "Odd", "blocked": "needs a reboot first"}
            ],
            "selected": "lw",
            "ports": [{"value": 1, "label": "GPS 1"}, {"value": 2, "label": "TELEM 2", "blocked": "TELEM 2 carries MAVLink"}],
            "port": 1,
            "clear": ["SENS_EN_TFMINI", {"param": "SERIAL1_PROTOCOL", "value": -1, "port": 1}],
            "enable": [{"param": "EKF2_RNG_CTRL", "value": 1}],
            "disable": [{"param": "EKF2_RNG_CTRL", "value": 0}],
            "reboot": true
        }))
    }

    #[test]
    fn a_toggle_is_read_with_its_drivers_and_ports() {
        let t = range();
        assert_eq!((t.drivers.len(), t.ports.len()), (4, 2));
        assert_eq!(t.port.as_deref(), Some("1"));
        assert_eq!(t.drivers[0].value, Some(1.0));
        assert_eq!(t.drivers[2].param, None);
        assert_eq!(
            t.clear[1],
            Clear::Param {
                param: "SERIAL1_PROTOCOL".into(),
                value: -1.0,
                port: Some(1.0)
            }
        );
    }

    #[test]
    fn switching_a_serial_sensor_on_clears_then_sets_the_port_and_the_driver_last() {
        let t = range();
        let w = sensor_writes(&t, Some(&t.drivers[0]), "1", true).unwrap();
        let names: Vec<&str> = w.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "SENS_EN_TFMINI",
                "SER_TEL2_BAUD",
                "SERIAL1_PROTOCOL",
                "CAN_EN",
                "SENS_EN_SF1XX",
                "EKF2_RNG_CTRL"
            ]
        );
        assert_eq!(w.last(), Some(&("EKF2_RNG_CTRL".to_string(), 1.0)));
        assert_eq!(w[w.len() - 2], ("SENS_EN_SF1XX".to_string(), 1.0));
    }

    #[test]
    fn the_port_just_picked_is_not_freed_again_and_a_driver_without_a_value_takes_the_port() {
        let t = range();
        let w = sensor_writes(&t, Some(&t.drivers[0]), "2", true);
        assert_eq!(w, Err("TELEM 2 carries MAVLink".to_string()));
        let mut d = t.drivers[0].clone();
        d.value = None;
        let w = driver_writes(&d, Some(1.0), "1");
        assert!(!w.iter().any(|(n, _)| n == "SER_GPS1_BAUD"));
        assert_eq!(w.last(), Some(&("SENS_EN_SF1XX".to_string(), 1.0)));
        assert!(w.contains(&("SERIAL1_PROTOCOL".to_string(), 9.0)));
    }

    #[test]
    fn switching_off_clears_what_runs_and_writes_the_estimator_off() {
        let t = range();
        let w = sensor_writes(&t, Some(&t.drivers[0]), "1", false).unwrap();
        assert_eq!(
            w,
            vec![
                ("SENS_EN_TFMINI".to_string(), 0.0),
                ("SERIAL1_PROTOCOL".to_string(), -1.0),
                ("EKF2_RNG_CTRL".to_string(), 0.0)
            ]
        );
    }

    #[test]
    fn a_blocked_driver_and_a_missing_port_refuse_the_switch() {
        let t = range();
        assert_eq!(
            sensor_writes(&t, Some(&t.drivers[3]), "", true),
            Err("needs a reboot first".to_string())
        );
        assert_eq!(
            sensor_writes(&t, Some(&t.drivers[0]), "", true),
            Err("pick the serial port first".to_string())
        );
        assert!(sensor_writes(&t, Some(&t.drivers[1]), "", true).is_ok());
    }

    #[test]
    fn a_driver_that_is_the_clear_list_entry_is_not_written_twice() {
        let mut t = range();
        t.clear = vec![Clear::Name("SENS_EN_LL40LS".into())];
        let w = sensor_writes(&t, Some(&t.drivers[1]), "", true).unwrap();
        assert_eq!(w.iter().filter(|(n, _)| n == "SENS_EN_LL40LS").count(), 1);
        assert_eq!(w[0], ("SENS_EN_LL40LS".to_string(), 2.0));
    }

    fn preset() -> Preset {
        parse_presets(&json!([{
            "id": "tf", "label": "Benewake TFmini", "bus": "UART", "model": "TFmini", "serial": true,
            "supported": true, "active": true,
            "writes": [
                {"param": "SENS_EN_TFMINI", "value": null, "port": true, "label": "port"},
                {"param": "SERIAL{port}_BAUD", "value": 115200, "port_param": true, "label": "baud"},
                {"param": "EKF2_RNG_A_HMAX", "value": 8, "label": "max"}
            ],
            "kept": [{"param": "EKF2_RNG_QLTY_T", "value": 3, "label": "already enough"}],
            "clear": ["SENS_EN_SF1XX", "SENS_EN_TFMINI"],
            "missing": ["EKF2_X"]
        }]))
        .remove(0)
    }

    #[test]
    fn a_preset_names_the_port_in_its_writes_and_does_not_clear_what_it_sets() {
        let t = range();
        let w = preset_writes(&t, &preset(), "1").unwrap();
        assert_eq!(
            w,
            vec![
                ("SENS_EN_SF1XX".to_string(), 0.0),
                ("SENS_EN_TFMINI".to_string(), 1.0),
                ("SERIAL1_BAUD".to_string(), 115200.0),
                ("EKF2_RNG_A_HMAX".to_string(), 8.0),
            ]
        );
    }

    #[test]
    fn a_preset_refuses_a_missing_or_taken_port_and_an_unsupported_module() {
        let t = range();
        assert_eq!(
            preset_writes(&t, &preset(), ""),
            Err("pick the serial port first".to_string())
        );
        assert_eq!(
            preset_writes(&t, &preset(), "2"),
            Err("TELEM 2 carries MAVLink".to_string())
        );
        let mut p = preset();
        p.supported = false;
        p.unsupported = "no driver".into();
        assert_eq!(preset_writes(&t, &p, "1"), Err("no driver".to_string()));
    }

    #[test]
    fn a_preset_without_a_clear_list_uses_the_sensors() {
        let t = range();
        let mut p = preset();
        p.clear = None;
        p.writes.clear();
        let w = preset_writes(&t, &p, "1").unwrap();
        assert_eq!(w, vec![("SENS_EN_TFMINI".to_string(), 0.0)]);
    }

    #[test]
    fn the_dropdown_label_carries_the_bus_the_model_and_the_state() {
        let mut p = preset();
        assert_eq!(
            preset_option_label(&p),
            "Benewake TFmini \u{b7} UART \u{b7} TFmini  (in use)"
        );
        p.supported = false;
        assert!(preset_option_label(&p).ends_with("(cannot be set up here)"));
    }

    #[test]
    fn extra_names_are_checked_where_they_are_typed() {
        assert!(valid_extra_name("EKF2_RNG_QLTY_T"));
        assert!(!valid_extra_name("ekf2"));
        assert!(!valid_extra_name("1ABC"));
        assert!(!valid_extra_name("ABCDEFGHIJKLMNOPQ"));
        let mut e = Extras::default();
        let rendered: HashMap<String, String> = [("EKF2_RNG_DELAY".to_string(), "Parameters".to_string())].into();
        assert_eq!(e.problem("rangefinder", "", &rendered), "type a parameter name");
        assert!(e
            .problem("rangefinder", "bad name", &rendered)
            .starts_with("not a parameter name"));
        assert_eq!(
            e.problem("rangefinder", "EKF2_RNG_DELAY", &rendered),
            "already on this page, under Parameters"
        );
        e.map.insert("rangefinder".into(), vec!["EKF2_X".into()]);
        assert_eq!(e.problem("rangefinder", "EKF2_X", &rendered), "already added");
        assert_eq!(e.problem("rangefinder", "EKF2_Y", &rendered), "");
    }

    #[test]
    fn the_extra_list_is_capped_pruned_and_kept_in_a_file() {
        let dir = std::env::temp_dir().join(format!("corvus-extra-{}", std::process::id()));
        let path = dir.join("safety-extra.json");
        let _ = std::fs::remove_dir_all(&dir);
        let mut e = Extras::load(path.clone());
        for i in 0..EXTRA_LIMIT {
            e.add("rangefinder", &format!("P{i}"));
        }
        assert_eq!(
            e.problem("flow", "ONE_MORE", &HashMap::new()),
            format!("at most {EXTRA_LIMIT} extra parameters")
        );
        let again = Extras::load(path.clone());
        assert_eq!(again.names("rangefinder").len(), EXTRA_LIMIT);
        let mut e = again;
        e.remove("rangefinder", "P0");
        assert_eq!(e.names("rangefinder").len(), EXTRA_LIMIT - 1);
        let rendered: HashMap<String, String> = [("P1".to_string(), "X".to_string())].into();
        e.prune(&rendered);
        assert!(!e.names("rangefinder").contains(&"P1".to_string()));
        assert_eq!(Extras::load(path).names("rangefinder").len(), EXTRA_LIMIT - 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_with_a_bad_name_loses_only_that_name() {
        let dir = std::env::temp_dir().join(format!("corvus-extra-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("x.json");
        std::fs::write(&path, r#"{"flow": ["GOOD_ONE", "bad", 3], "junk": 5}"#).unwrap();
        let e = Extras::load(path);
        assert_eq!(e.names("flow"), ["GOOD_ONE"]);
        assert!(e.names("junk").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
