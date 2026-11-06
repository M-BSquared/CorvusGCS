//! The geofence area: what this station keeps of it and the geometry the card
//! and the Home map share (`Corvus.geofence`, python/src/js/geofence.js).
//!
//! The backend keeps the polygon (`/api/geofence`) and says whether it is the
//! one on the vehicle. Everything here is the same copy for the whole app:
//! the Setup card draws and uploads it, the Home map draws its outline.

use std::sync::{Arc, Mutex};

use eframe::egui;
use serde_json::Value;

use crate::net::http::{self, Server};

/// `[longitude, latitude]` corners.
pub type Corner = [f64; 2];

/// `MIN_CORNER_GAP_M`: corners closer than this are one corner.
const MIN_CORNER_GAP_M: f64 = 0.5;
const EARTH_R: f64 = 6_371_008.8;

/// What the backend stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Fence {
    pub loaded: bool,
    pub polygon: Vec<Corner>,
    pub show_on_map: bool,
    pub on_vehicle: Option<bool>,
    pub vehicle_has_fence: Option<bool>,
    pub max_vertices: usize,
}

impl Default for Fence {
    fn default() -> Fence {
        Fence {
            loaded: false,
            polygon: Vec::new(),
            show_on_map: true,
            on_vehicle: None,
            vehicle_has_fence: None,
            max_vertices: 64,
        }
    }
}

impl Fence {
    /// `adopt`: the answer of `GET /api/geofence` or of a save.
    pub fn from_value(v: &Value, previous_max: usize) -> Option<Fence> {
        if !v.is_object() {
            return None;
        }
        Some(Fence {
            loaded: true,
            polygon: v
                .get("polygon")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?]))
                        .collect()
                })
                .unwrap_or_default(),
            show_on_map: v.get("show_on_map") != Some(&Value::Bool(false)),
            on_vehicle: v.get("on_vehicle").and_then(Value::as_bool),
            vehicle_has_fence: v.get("vehicle_has_fence").and_then(Value::as_bool),
            max_vertices: v
                .get("max_vertices")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .map_or(previous_max, |n| n as usize),
        })
    }
}

/// The one copy of the area.
pub type Shared = Arc<Mutex<Fence>>;

/// What every page that draws or edits the area needs.
#[derive(Clone)]
pub struct Env {
    pub fence: Shared,
    /// `/api/tiles/sources`, for the maps the card draws on.
    pub sources: Value,
}

impl Env {
    pub fn new(sources: Value) -> Env {
        Env {
            fence: Arc::new(Mutex::new(Fence::default())),
            sources,
        }
    }

    /// The area as the stored copy has it now.
    pub fn get(&self) -> Fence {
        self.fence.lock().map(|f| f.clone()).unwrap_or_default()
    }

    pub fn adopt(&self, v: &Value) {
        if let Ok(mut f) = self.fence.lock() {
            if let Some(next) = Fence::from_value(v, f.max_vertices) {
                *f = next;
            }
        }
    }

    /// Reads the stored area once, on a thread, as the web does at boot.
    pub fn load(&self, ctx: &egui::Context, server: &Server) {
        let (env, server, ctx) = (self.clone(), server.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Ok(v) = http::get_json(&server, "/api/geofence") {
                env.adopt(&v);
                ctx.request_repaint();
            }
        });
    }
}

/// `distanceM`: metres between two corners on a local flat projection.
pub fn distance_m(a: Corner, b: Corner) -> f64 {
    let rad = std::f64::consts::PI / 180.0;
    let d_lat = (b[1] - a[1]) * rad;
    let d_lon = (b[0] - a[0]) * rad;
    let x = d_lon * (((a[1] + b[1]) / 2.0) * rad).cos();
    (x * x + d_lat * d_lat).sqrt() * EARTH_R
}

/// `distinctCorners`: drops corners that land on the one before (a double
/// click places two) and a last one that lands on the first.
pub fn distinct_corners(pts: &[Corner]) -> Vec<Corner> {
    let mut out: Vec<Corner> = Vec::new();
    for p in pts {
        if out.last().is_none_or(|last| distance_m(*last, *p) > MIN_CORNER_GAP_M) {
            out.push(*p);
        }
    }
    if out.len() > 1 && distance_m(out[0], out[out.len() - 1]) <= MIN_CORNER_GAP_M {
        out.pop();
    }
    out
}

/// `contains`: whether the point lies inside the ring.
pub fn contains(polygon: &[Corner], point: Corner) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let (x, y) = (point[0], point[1]);
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (xi, yi) = (polygon[i][0], polygon[i][1]);
        let (xj, yj) = (polygon[j][0], polygon[j][1]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn orient(p: Corner, q: Corner, r: Corner) -> f64 {
    (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
}

fn crosses(a: Corner, b: Corner, c: Corner, d: Corner) -> bool {
    let (d1, d2, d3, d4) = (orient(c, d, a), orient(c, d, b), orient(a, b, c), orient(a, b, d));
    d1 != 0.0 && d2 != 0.0 && d3 != 0.0 && d4 != 0.0 && (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

/// `selfIntersects`: whether two edges of the closed ring cross. The backend
/// refuses such an area, and so does the vehicle.
pub fn self_intersects(polygon: &[Corner]) -> bool {
    let n = polygon.len();
    for i in 0..n {
        for j in i + 1..n {
            if j == i + 1 || (j + 1) % n == i {
                continue;
            }
            if crosses(polygon[i], polygon[(i + 1) % n], polygon[j], polygon[(j + 1) % n]) {
                return true;
            }
        }
    }
    false
}

/// `areaM2`: the area of the ring in square metres.
pub fn area_m2(polygon: &[Corner]) -> f64 {
    if polygon.len() < 3 {
        return 0.0;
    }
    let lat0 = polygon.iter().map(|p| p[1]).sum::<f64>() / polygon.len() as f64;
    let kx = (std::f64::consts::PI / 180.0) * EARTH_R * (lat0 * std::f64::consts::PI / 180.0).cos();
    let ky = (std::f64::consts::PI / 180.0) * EARTH_R;
    let mut sum = 0.0;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
        sum += (a[0] * kx) * (b[1] * ky) - (b[0] * kx) * (a[1] * ky);
    }
    sum.abs() / 2.0
}

/// `problemOf`: why the polygon cannot be kept, or "".
pub fn problem_of(polygon: &[Corner], max_vertices: usize) -> String {
    if polygon.len() < 3 {
        return "An area needs at least 3 corners.".into();
    }
    if max_vertices > 0 && polygon.len() > max_vertices {
        return format!("An area may have at most {max_vertices} corners.");
    }
    if self_intersects(polygon) {
        return "The edges of the area cross each other.".into();
    }
    String::new()
}

/// The backend answers a failed transfer with the bare result of the
/// protocol ("Geofence upload failed: ACK_TIMEOUT"). This says what happened
/// to the operator's fence and what to do, in words a translation can carry.
/// Anything it does not know comes back as it was.
pub fn explain(message: &str, clearing: bool) -> String {
    let has = |w: &str| message.contains(w);
    if has("DISCONNECTED") {
        "There is no link to the vehicle. Nothing was changed on it.".into()
    } else if has("ACK_TIMEOUT") {
        if clearing {
            "The vehicle stopped answering, so its fence was not removed. Check the link and try again.".into()
        } else {
            "The vehicle stopped answering, so its fence was not changed. Check the link and upload again.".into()
        }
    } else if has("INTERRUPTED") || has("CANCELLED") {
        "The transfer was interrupted. The vehicle's fence was not changed. Try again.".into()
    } else if [
        "DENIED",
        "FAILED",
        "UNSUPPORTED",
        "TEMPORARILY_REJECTED",
        "NOT_IN_CONTROL",
    ]
    .iter()
    .any(|w| has(w))
    {
        "The vehicle refused the area. Its fence was not changed.".into()
    } else {
        message.to_string()
    }
}

/// `formatArea`.
pub fn format_area(m2: f64) -> String {
    if m2 >= 1e6 {
        format!("{} km\u{b2}", crate::data::units::js_to_fixed(m2 / 1e6, 2))
    } else if m2 >= 1e4 {
        format!("{} ha", crate::data::units::js_to_fixed(m2 / 1e4, 2))
    } else {
        format!("{} m\u{b2}", crate::data::units::js_round(m2))
    }
}

/// The web-mercator fraction of the world, 0 to 1, a point lies at.
fn mercator(c: Corner) -> (f64, f64) {
    let lat = c[1].clamp(-85.05, 85.05).to_radians();
    (
        (c[0] + 180.0) / 360.0,
        (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0,
    )
}

/// `fitBounds`: the zoom and centre that frame the corners in `size` points
/// with `padding` on every side, at most `max_zoom`.
pub fn fit(corners: &[Corner], size: (f32, f32), padding: f32, max_zoom: f64) -> Option<(f64, Corner)> {
    if corners.len() < 2 {
        return None;
    }
    let pts: Vec<(f64, f64)> = corners.iter().map(|c| mercator(*c)).collect();
    let (x0, x1) = pts
        .iter()
        .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (y0, y1) = pts
        .iter()
        .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let aw = f64::from((size.0 - 2.0 * padding).max(1.0));
    let ah = f64::from((size.1 - 2.0 * padding).max(1.0));
    let z = |avail: f64, frac: f64| {
        if frac <= 0.0 {
            f64::MAX
        } else {
            (avail / (256.0 * frac)).log2()
        }
    };
    let zoom = z(aw, x1 - x0).min(z(ah, y1 - y0)).clamp(0.0, max_zoom);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let lon = cx * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * cy)).sinh().atan().to_degrees();
    Some((zoom, [lon, lat]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SQUARE: [Corner; 4] = [[11.60, 48.10], [11.61, 48.10], [11.61, 48.11], [11.60, 48.11]];

    #[test]
    fn a_corner_on_the_one_before_is_one_corner() {
        let pts = [[11.6, 48.1], [11.6, 48.1], [11.61, 48.1], [11.61, 48.11], [11.6, 48.1]];
        assert_eq!(
            distinct_corners(&pts),
            vec![[11.6, 48.1], [11.61, 48.1], [11.61, 48.11]]
        );
    }

    #[test]
    fn a_point_is_inside_or_outside_the_ring() {
        assert!(contains(&SQUARE, [11.605, 48.105]));
        assert!(!contains(&SQUARE, [11.62, 48.105]));
        assert!(!contains(&SQUARE[..2], [11.605, 48.105]));
    }

    #[test]
    fn a_bow_tie_crosses_itself_and_a_square_does_not() {
        assert!(!self_intersects(&SQUARE));
        let bow = [[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]];
        assert!(self_intersects(&bow));
    }

    #[test]
    fn the_area_is_in_square_metres_and_said_in_the_unit_that_reads_well() {
        let a = area_m2(&SQUARE);
        assert!(
            (a - 0.01 * 111_195.0 * 0.01 * 111_195.0 * 48.105f64.to_radians().cos()).abs() < 3000.0,
            "{a}"
        );
        assert_eq!(format_area(250.4), "250 m\u{b2}");
        assert_eq!(format_area(25_000.0), "2.50 ha");
        assert_eq!(format_area(2_500_000.0), "2.50 km\u{b2}");
    }

    #[test]
    fn an_area_is_refused_with_the_words_the_web_uses() {
        assert_eq!(problem_of(&SQUARE[..2], 64), "An area needs at least 3 corners.");
        assert_eq!(problem_of(&SQUARE, 3), "An area may have at most 3 corners.");
        let bow = [[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]];
        assert_eq!(problem_of(&bow, 64), "The edges of the area cross each other.");
        assert_eq!(problem_of(&SQUARE, 64), "");
    }

    #[test]
    fn a_failed_transfer_is_said_in_words_and_an_unknown_answer_is_kept() {
        // what the backend answers: python/corvus/mavlink_common.py MAV_RESULT_TEXT
        assert!(explain("Geofence upload failed: ACK_TIMEOUT", false).starts_with("The vehicle stopped answering"));
        assert!(explain("Geofence clear failed: ACK_TIMEOUT", true).contains("not removed"));
        assert!(explain("Geofence upload failed: DISCONNECTED", false).starts_with("There is no link"));
        assert!(explain("Geofence upload failed: FAILED", false).starts_with("The vehicle refused"));
        assert!(explain("Geofence upload failed: INTERRUPTED", false).starts_with("The transfer was interrupted"));
        assert_eq!(explain("no area has been drawn", false), "no area has been drawn");
    }

    #[test]
    fn the_stored_area_is_read_with_its_defaults() {
        let f = Fence::from_value(
            &json!({"polygon": [[1, 2], [3, 4], "x"], "show_on_map": false, "on_vehicle": true}),
            64,
        )
        .unwrap();
        assert_eq!(f.polygon, vec![[1.0, 2.0], [3.0, 4.0]]);
        assert!(!f.show_on_map && f.loaded);
        assert_eq!(
            (f.on_vehicle, f.vehicle_has_fence, f.max_vertices),
            (Some(true), None, 64)
        );
        let g = Fence::from_value(&json!({"max_vertices": 12}), 64).unwrap();
        assert!(g.show_on_map);
        assert_eq!(g.max_vertices, 12);
        assert!(Fence::from_value(&json!(5), 64).is_none());
    }

    #[test]
    fn the_view_that_frames_an_area_centres_it_and_stays_under_the_top_zoom() {
        let (zoom, centre) = fit(&SQUARE, (400.0, 300.0), 36.0, 18.0).unwrap();
        assert!((centre[0] - 11.605).abs() < 1e-6 && (centre[1] - 48.105).abs() < 1e-3);
        // 0.01 degrees of longitude across 328 px
        let want = (328.0f64 / (256.0 * 0.01 / 360.0)).log2();
        assert!(zoom <= want + 1e-9 && zoom > want - 1.5, "{zoom} {want}");
        let (tiny, _) = fit(&[[11.6, 48.1], [11.600_001, 48.100_001]], (400.0, 300.0), 36.0, 18.0).unwrap();
        assert_eq!(tiny, 18.0);
        assert!(fit(&SQUARE[..1], (400.0, 300.0), 36.0, 18.0).is_none());
    }
}
