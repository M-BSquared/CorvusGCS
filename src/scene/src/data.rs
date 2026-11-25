//! What the simulated aircraft is made of (`data/data.json`): the parameters
//! of a set up PX4 1.18, what each airframe changes, PX4's defaults and
//! descriptions for some, and what its shell prints. A whole number is an
//! INT32 on the wire, one with a point a REAL32.

use std::sync::OnceLock;

use serde_json::{json, Map, Value};

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| serde_json::from_str(include_str!("../data/data.json")).expect("data.json"))
}

/// The parameters of `airframe`, with both extra sensors and padded to the size of a real
/// firmware's table when asked for.
pub fn parameters(
    airframe: &str,
    sensors: bool,
    full: bool,
    set: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let d = data();
    let mut t = d["table"].as_object().cloned().unwrap_or_default();
    let frame = d["airframes"]
        .get(airframe)
        .ok_or(format!("no airframe called {airframe}"))?;
    for name in frame["drop"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        t.remove(name);
    }
    for (block, wanted) in [(&frame["set"], true), (&d["sensors"], sensors), (&d["padding"], full)] {
        if wanted {
            t.extend(block.as_object().cloned().unwrap_or_default());
        }
    }
    t.extend(set.clone());
    Ok(t)
}

/// `parameters.json` as PX4 serves it over MAVLink FTP, compressed.
pub fn metadata(table: &Map<String, Value>) -> Vec<u8> {
    let d = data();
    let list: Vec<Value> = table
        .iter()
        .map(|(name, value)| {
            let int = value.is_i64();
            let default = d["defaults"].get(name).unwrap_or(value);
            let default = if int {
                json!(default.as_f64().unwrap_or(0.0) as i64)
            } else {
                json!(default.as_f64().unwrap_or(0.0))
            };
            let mut e = json!({"name": name, "type": if int { "Int32" } else { "Float" }, "default": default});
            if let Some(doc) = d["docs"].get(name) {
                for (key, i) in [("shortDesc", 0), ("units", 1), ("min", 2), ("max", 3)] {
                    if !doc[i].is_null() && doc[i] != "" {
                        e[key] = doc[i].clone();
                    }
                }
                let values: Vec<Value> = doc[4]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| json!({"value": v[0], "description": v[1]}))
                    .collect();
                if !values.is_empty() {
                    e["values"] = json!(values);
                }
            }
            if ["CAL_", "SYS_AUTOSTART", "SENS_BOARD_ROT"]
                .iter()
                .any(|p| name.starts_with(p))
            {
                e["rebootRequired"] = json!(true);
            }
            e
        })
        .collect();
    let text = json!({"version": 1, "parameters": list}).to_string();
    let mut out = Vec::new();
    lzma_rs::xz_compress(&mut text.as_bytes(), &mut out).expect("in memory");
    out
}

/// The ULog of the hop the pictures fly.
pub fn flight_log() -> Vec<u8> {
    let mut out = Vec::new();
    let packed: &[u8] = include_bytes!("../data/sortie.ulg.xz");
    lzma_rs::xz_decompress(&mut &packed[..], &mut out).expect("sortie.ulg.xz");
    out
}

/// What PX4's shell prints for one line, with its line endings.
pub fn shell(line: &str) -> String {
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return String::new();
    }
    let text = match data()["nsh"][&line].as_str() {
        Some(t) => t.to_string(),
        None => format!("nsh: {}: command not found", line.split(' ').next().unwrap_or("")),
    };
    text.replace('\n', "\r\n") + "\r\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_are_a_set_up_aircraft() {
        let none = Map::new();
        let t = parameters("quad_x", false, false, &none).unwrap();
        assert!(t.len() > 200 && t["SYS_AUTOSTART"].is_i64());
        assert!(parameters("quad_x", false, true, &none).unwrap().len() > 1000);
        assert_ne!(parameters("hexa_x", false, false, &none).unwrap(), t);
        assert_ne!(parameters("quad_x", true, false, &none).unwrap(), t);
        assert!(parameters("blimp", false, false, &none).is_err());
        assert!(!metadata(&t).is_empty());
    }

    #[test]
    fn the_log_and_the_shell_are_there() {
        assert_eq!(&flight_log()[..4], b"ULog");
        assert!(shell("ver  all").contains("PX4_FMU_V6X"));
        assert!(shell("nope").contains("command not found"));
    }
}
