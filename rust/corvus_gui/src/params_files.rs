//! The parameter files of the Parameters page (src/js/setup-parameters.js):
//! the three formats Export writes and Import reads, and what an import
//! would change, worked out before anything is sent.

use std::collections::HashMap;

use serde_json::Value;

use crate::params::{values_equal, Param};

/// A file format the page exchanges parameters in.
pub struct Format {
    pub id: &'static str,
    pub label: &'static str,
    pub suffix: &'static str,
    pub kind: &'static str,
}

pub const FORMATS: [Format; 3] = [
    Format {
        id: "qgc",
        label: "QGroundControl (.params)",
        suffix: ".params",
        kind: "QGroundControl",
    },
    Format {
        id: "mission-planner",
        label: "Mission Planner (.param)",
        suffix: ".param",
        kind: "Mission Planner",
    },
    Format {
        id: "json",
        label: "Corvus (.json)",
        suffix: ".json",
        kind: "Corvus",
    },
];

/// The format with this id, the first one when it is unknown.
pub fn format(id: &str) -> &'static Format {
    FORMATS.iter().find(|f| f.id == id).unwrap_or(&FORMATS[0])
}

/// The format a file name's suffix says, if it says one.
pub fn format_from_name(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    FORMATS.iter().find(|f| lower.ends_with(f.suffix)).map(|f| f.id)
}

/// `name` with the suffix of format `id`, whichever suffix it had.
pub fn with_suffix(name: &str, id: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    let lower = name.to_lowercase();
    let stem = FORMATS
        .iter()
        .find(|f| lower.ends_with(f.suffix))
        .map_or(name, |f| &name[..name.len() - f.suffix.len()]);
    format!("{stem}{}", format(id).suffix)
}

/// A parameter file, read.
#[derive(Debug, PartialEq)]
pub struct Parsed {
    pub params: Vec<(String, f64)>,
    pub skipped: usize,
}

fn is_name(s: &str) -> bool {
    (1..=16).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `parseParamFile`: Corvus JSON, QGroundControl's tab separated rows, or
/// Mission Planner's `NAME,VALUE` lines.
pub fn parse(text: &str, file_name: &str) -> Result<Parsed, String> {
    let body = text.trim_start_matches('\u{feff}').trim();
    if file_name.to_lowercase().ends_with(".json") || body.starts_with('{') {
        let doc: Value = serde_json::from_str(body).map_err(|_| "File is not valid JSON".to_string())?;
        let list = doc
            .get("params")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .ok_or_else(|| "Parameter file has no parameters".to_string())?;
        let mut params = Vec::new();
        for p in list {
            let name = p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty());
            let value = p.get("value").and_then(Value::as_f64).filter(|v| v.is_finite());
            match (name, value) {
                (Some(n), Some(v)) => params.push((n.to_string(), v)),
                _ => return Err("Parameter file has an invalid entry".into()),
            }
        }
        return Ok(Parsed { params, skipped: 0 });
    }
    if body.is_empty() {
        return Err("Parameter file is empty".into());
    }
    let mut rows: Vec<(Option<u32>, String, f64)> = Vec::new();
    let mut qgc = false;
    for (index, raw) in body.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let (component, name, text) = if cols.len() >= 5 && digits(cols[0]) && digits(cols[1]) {
            qgc = true;
            (cols[1].parse::<u32>().ok(), cols[2], cols[3])
        } else {
            let parts: Vec<&str> = line
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|p| !p.is_empty())
                .collect();
            if parts.len() < 2 {
                return Err(format!("Line {} is not a parameter", index + 1));
            }
            (None, parts[0], parts[1])
        };
        if !is_name(name) {
            return Err(format!(
                "Line {}: \"{}\" is not a parameter name",
                index + 1,
                clip(name, 20)
            ));
        }
        let value =
            number(text).ok_or_else(|| format!("Line {}: \"{}\" is not a number", index + 1, clip(text, 20)))?;
        rows.push((component, name.to_string(), value));
    }
    if rows.is_empty() {
        return Err("Parameter file has no parameters".into());
    }
    let keep: Vec<&(Option<u32>, String, f64)> = if qgc {
        let comps: Vec<u32> = rows.iter().filter_map(|r| r.0).collect();
        let wanted = if comps.contains(&1) {
            Some(1)
        } else {
            comps.first().copied()
        };
        rows.iter().filter(|r| r.0.is_none() || r.0 == wanted).collect()
    } else {
        rows.iter().collect()
    };
    let skipped = rows.len() - keep.len();
    let mut order: Vec<String> = Vec::new();
    let mut merged: HashMap<&str, f64> = HashMap::new();
    for r in keep {
        if merged.insert(&r.1, r.2).is_none() {
            order.push(r.1.clone());
        }
    }
    let params = order
        .into_iter()
        .map(|n| {
            let v = merged[n.as_str()];
            (n, v)
        })
        .collect();
    Ok(Parsed { params, skipped })
}

/// One value an import would change.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub name: String,
    pub from: f64,
    pub to: f64,
    pub kind: i64,
}

/// What a file would do to the vehicle.
#[derive(Debug, PartialEq)]
pub struct Preview {
    pub loaded: bool,
    pub total: usize,
    pub same: usize,
    pub changes: Vec<Change>,
    pub unknown: Vec<String>,
    pub skipped: usize,
    pub to_write: Vec<(String, f64)>,
    pub reboot_note: bool,
}

/// `openImportPreview`: against a loaded set only the values that differ are
/// written and a name the firmware does not have is never sent; without one
/// everything is written.
pub fn preview(parsed: &Parsed, loaded: Option<&[Param]>, reboot: &dyn Fn(&str) -> bool) -> Preview {
    let total = parsed.params.len();
    let mut p = Preview {
        loaded: loaded.is_some(),
        total,
        same: 0,
        changes: Vec::new(),
        unknown: Vec::new(),
        skipped: parsed.skipped,
        to_write: Vec::new(),
        reboot_note: false,
    };
    match loaded {
        Some(set) => {
            let by_name: HashMap<&str, &Param> = set.iter().map(|q| (q.name.as_str(), q)).collect();
            for (name, value) in &parsed.params {
                match by_name.get(name.as_str()) {
                    None => p.unknown.push(name.clone()),
                    Some(cur) if values_equal(cur.value, *value, cur.kind) => p.same += 1,
                    Some(cur) => p.changes.push(Change {
                        name: name.clone(),
                        from: cur.value,
                        to: *value,
                        kind: cur.kind,
                    }),
                }
            }
            p.to_write = p.changes.iter().map(|c| (c.name.clone(), c.to)).collect();
            p.reboot_note = p.changes.iter().any(|c| reboot(&c.name));
        }
        None => p.to_write = parsed.params.clone(),
    }
    p
}

/// `n parameter(s)`.
pub fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The sentence over the import list.
pub fn preview_text(p: &Preview, file_name: &str) -> String {
    let mut text = if p.loaded {
        let mut t = format!(
            "{} of {} in {file_name} {} from the vehicle.",
            p.changes.len(),
            plural(p.total, "parameter"),
            if p.changes.len() == 1 { "differs" } else { "differ" }
        );
        if p.same > 0 {
            t.push_str(&format!(" {} already match.", p.same));
        }
        if !p.unknown.is_empty() {
            t.push_str(&format!(
                " {} {} not on this vehicle and skipped.",
                plural(p.unknown.len(), "parameter"),
                if p.unknown.len() == 1 { "is" } else { "are" }
            ));
        }
        t
    } else {
        format!(
            "{} in {file_name}. Download the vehicle's parameters first to see which of them change anything; without them every value is written.",
            plural(p.total, "parameter")
        )
    };
    if p.skipped > 0 {
        text.push_str(&format!(
            " {} for other components {} skipped.",
            plural(p.skipped, "row"),
            if p.skipped == 1 { "is" } else { "are" }
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(list: &[(&str, f64, i64)]) -> Vec<Param> {
        list.iter()
            .map(|(n, v, k)| Param {
                name: n.to_string(),
                value: *v,
                kind: *k,
            })
            .collect()
    }

    #[test]
    fn suffixes_follow_the_format() {
        assert_eq!(with_suffix("a.params", "json"), "a.json");
        assert_eq!(with_suffix("a", "mission-planner"), "a.param");
        assert_eq!(with_suffix("  ", "json"), "");
        assert_eq!(format_from_name("X.PARAM"), Some("mission-planner"));
        assert_eq!(format_from_name("x.txt"), None);
    }

    #[test]
    fn qgc_keeps_the_autopilot_component() {
        let t = "# header\n1\t1\tMC_ROLL_P\t6.5\t9\n1\t2\tCAM_X\t1\t6\n1\t1\tSYS_ID\t1\t6\n";
        let p = parse(t, "a.params").unwrap();
        assert_eq!(p.params, vec![("MC_ROLL_P".into(), 6.5), ("SYS_ID".into(), 1.0)]);
        assert_eq!(p.skipped, 1);
    }

    #[test]
    fn mission_planner_lines_and_the_last_value_wins() {
        let p = parse("ACRO_P,1.5\nACRO_P 2\nFOO\t3", "a.param").unwrap();
        assert_eq!(p.params, vec![("ACRO_P".into(), 2.0), ("FOO".into(), 3.0)]);
    }

    #[test]
    fn json_files() {
        let p = parse(
            r#"{"params":[{"name":"A","value":1},{"name":"B","value":2.5}]}"#,
            "x.json",
        )
        .unwrap();
        assert_eq!(p.params.len(), 2);
        assert_eq!(parse("{", "x.json").unwrap_err(), "File is not valid JSON");
        assert_eq!(
            parse(r#"{"params":[]}"#, "x.json").unwrap_err(),
            "Parameter file has no parameters"
        );
        assert_eq!(
            parse(r#"{"params":[{"name":"A"}]}"#, "x.json").unwrap_err(),
            "Parameter file has an invalid entry"
        );
    }

    #[test]
    fn bad_lines_say_which() {
        assert_eq!(parse("", "a.param").unwrap_err(), "Parameter file is empty");
        assert_eq!(parse("ONLY", "a.param").unwrap_err(), "Line 1 is not a parameter");
        assert_eq!(
            parse("OK 1\nBAD-NAME 2", "a.param").unwrap_err(),
            "Line 2: \"BAD-NAME\" is not a parameter name"
        );
        assert_eq!(
            parse("OK abc", "a.param").unwrap_err(),
            "Line 1: \"abc\" is not a number"
        );
    }

    #[test]
    fn a_loaded_set_writes_only_what_differs() {
        let parsed = Parsed {
            params: vec![("A".into(), 1.0), ("B".into(), 3.0), ("GONE".into(), 1.0)],
            skipped: 2,
        };
        let p = preview(&parsed, Some(&set(&[("A", 1.0, 9), ("B", 2.0, 9)])), &|n| n == "B");
        assert_eq!((p.same, p.changes.len(), p.unknown.len()), (1, 1, 1));
        assert_eq!(p.to_write, vec![("B".to_string(), 3.0)]);
        assert!(p.reboot_note);
        assert_eq!(
            preview_text(&p, "f.param"),
            "1 of 3 parameters in f.param differs from the vehicle. 1 already match. 1 parameter is not on this vehicle and skipped. 2 rows for other components are skipped."
        );
    }

    #[test]
    fn without_a_set_everything_is_written() {
        let parsed = Parsed {
            params: vec![("A".into(), 1.0)],
            skipped: 0,
        };
        let p = preview(&parsed, None, &|_| false);
        assert_eq!(p.to_write.len(), 1);
        assert!(preview_text(&p, "f").starts_with("1 parameter in f. Download the vehicle's"));
    }
}
