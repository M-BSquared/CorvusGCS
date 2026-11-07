//! The fence area (`geofence.py`): one inclusion polygon the operator draws,
//! kept in `~/.corvus/geofence.json` with the map toggle.

use std::path::Path;

use serde_json::{json, Value};

use crate::mission::{dump_json_ascii, write_atomic};
use crate::paths::Paths;

pub const MAX_VERTICES: usize = 64;
pub const MIN_VERTICES: usize = 3;
pub const FILE_NAME: &str = "geofence.json";

/// Where the area is kept: `~/.corvus/geofence.json`.
pub fn default_path(paths: &Paths) -> std::path::PathBuf {
    paths.join(FILE_NAME)
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

type Point = (f64, f64);

fn segments_cross(a: Point, b: Point, c: Point, d: Point) -> bool {
    let orient = |p: Point, q: Point, r: Point| (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
    let (d1, d2) = (orient(c, d, a), orient(c, d, b));
    let (d3, d4) = (orient(a, b, c), orient(a, b, d));
    ((d1 > 0.0) != (d2 > 0.0)) && d1 != 0.0 && d2 != 0.0 && ((d3 > 0.0) != (d4 > 0.0)) && d3 != 0.0 && d4 != 0.0
}

/// Whether any two non adjacent edges of the closed ring cross.
pub fn self_intersects(polygon: &[Point]) -> bool {
    let n = polygon.len();
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[(i + 1) % n]);
        for j in i + 1..n {
            if (j + 1) % n == i || j == (i + 1) % n {
                continue;
            }
            let (c, d) = (polygon[j], polygon[(j + 1) % n]);
            if segments_cross(a, b, c, d) {
                return true;
            }
        }
    }
    false
}

/// An area from the browser, `[[lon, lat], ...]`, cleaned. An empty list is
/// valid and means "no area". A closing corner that repeats the first is
/// dropped, since the ring closes itself.
pub fn validate_polygon(raw: &Value) -> Result<Vec<Point>, String> {
    let list = match raw {
        Value::Null => return Ok(Vec::new()),
        Value::Array(list) => list,
        _ => return Err("the area must be a list of [lon, lat] corners".into()),
    };
    let mut polygon: Vec<Point> = Vec::new();
    for corner in list {
        let Some(pair) = corner.as_array().filter(|p| p.len() >= 2) else {
            return Err("every corner must be [lon, lat]".into());
        };
        let (Some(lon), Some(lat)) = (number(&pair[0]), number(&pair[1])) else {
            return Err("a corner is not a position on the globe".into());
        };
        if lon.abs() > 180.0 || lat.abs() > 90.0 {
            return Err("a corner is not a position on the globe".into());
        }
        polygon.push((lon, lat));
    }
    if polygon.len() > 1 && polygon.first() == polygon.last() {
        polygon.pop();
    }
    if polygon.is_empty() {
        return Ok(polygon);
    }
    if polygon.len() < MIN_VERTICES {
        return Err(format!("an area needs at least {MIN_VERTICES} corners"));
    }
    if polygon.len() > MAX_VERTICES {
        return Err(format!("an area may have at most {MAX_VERTICES} corners"));
    }
    if self_intersects(&polygon) {
        return Err("the edges of the area cross each other".into());
    }
    Ok(polygon)
}

fn empty() -> Value {
    json!({"polygon": [], "show_on_map": true})
}

fn polygon_json(polygon: &[Point]) -> Value {
    Value::Array(polygon.iter().map(|(lon, lat)| json!([lon, lat])).collect())
}

/// The stored area and its map toggle. A missing or broken file is "none".
pub fn load(path: &Path) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return empty();
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return empty();
    };
    let Some(data) = data.as_object() else { return empty() };
    let polygon = validate_polygon(data.get("polygon").unwrap_or(&Value::Null)).unwrap_or_default();
    let show = data.get("show_on_map").and_then(Value::as_bool).unwrap_or(true);
    json!({"polygon": polygon_json(&polygon), "show_on_map": show})
}

/// Writes the area atomically: the field laptop loses power mid write.
pub fn save(state: &Value, path: &Path) -> std::io::Result<()> {
    let polygon = state
        .get("polygon")
        .filter(|p| p.as_array().is_some_and(|a| !a.is_empty()))
        .cloned()
        .unwrap_or(json!([]));
    let show = state.get("show_on_map").is_none_or(truthy);
    let data = json!({"polygon": polygon, "show_on_map": show});
    write_atomic(path, ".geofence-", &format!("{}\n", dump_json_ascii(&data)))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
