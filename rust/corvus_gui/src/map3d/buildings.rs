//! OSM buildings for the 3D map (`src/js/map.js` `addBuildingLayer`,
//! `refreshBuildings`, corvus/buildings.py): the footprints and heights of
//! the cells the backend serves at `/api/buildings/15/x/y.json`, and the
//! extrusion of them into walls and roofs.
//!
//! The backend never waits on Overpass. A cell it has not got comes back as
//! `pending` (asked for again a few seconds later) or `unavailable`
//! (offline); here both are a miss that the cache asks for again after a
//! pause, and what is cached is only a real answer.

use std::io::Read;

use eframe::egui::{self, Color32, Mesh, Pos2};
use serde_json::Value;

use crate::map3d::camera::{Camera, V3};

/// `BUILDING_CELL_Z`: the grid the backend addresses cells on.
pub const CELL_Z: u8 = 15;
/// `BUILDING_MIN_ZOOM`: under it the extrusions are slivers.
pub const MIN_ZOOM: f64 = 14.0;
/// `BUILDING_MAX_CELLS`.
pub const MAX_CELLS: usize = 36;
/// What one frame extrudes at most, the nearest first. The web map holds
/// 20000 in a GPU source; here every one is projected each frame.
pub const MAX_DRAWN: usize = 6000;
/// `BUILDING_RETRY_MS`.
pub const RETRY_SECS: u64 = 3;
/// `BUILDING_COLOR`.
const COLOR: [u8; 3] = [0xA7, 0xB0, 0xBD];
/// `fill-extrusion-opacity`.
const OPACITY: f32 = 0.85;

/// One building: its outline in longitude and latitude and its height above
/// the ground, from `min_height` to `height`.
#[derive(Debug, Clone, PartialEq)]
pub struct Building {
    pub ring: Vec<(f64, f64)>,
    pub height: f64,
    pub min_height: f64,
}

/// The cells of one answer; `None` for a miss: `pending`, `unavailable` or
/// something that is not a cell.
pub fn decode(bytes: &[u8]) -> Option<Vec<Building>> {
    let mut plain = Vec::new();
    let body: &[u8] = if bytes.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut plain).ok()?;
        &plain
    } else {
        bytes
    };
    let doc: Value = serde_json::from_slice(body).ok()?;
    if doc.get("pending") == Some(&Value::Bool(true))
        || doc.get("unavailable").is_some_and(|u| u != &Value::Bool(false))
    {
        return None;
    }
    let list = doc.get("features")?.as_array()?;
    Some(list.iter().filter_map(feature).collect())
}

fn feature(f: &Value) -> Option<Building> {
    let ring = f.pointer("/geometry/coordinates/0")?.as_array()?;
    let mut pts: Vec<(f64, f64)> = ring
        .iter()
        .filter_map(|c| Some((c.get(0)?.as_f64()?, c.get(1)?.as_f64()?)))
        .collect();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    if pts.len() < 3 {
        return None;
    }
    let num = |k: &str| f.pointer(&format!("/properties/{k}")).and_then(Value::as_f64);
    Some(Building {
        ring: pts,
        height: num("height").unwrap_or(6.0),
        min_height: num("min_height").unwrap_or(0.0),
    })
}

/// The cells of a view box `(west, south, east, north)`, nearest to `centre`
/// first, at most `MAX_CELLS` (`buildingCellsIn`): square rings grown out of
/// the middle, so a long view is not walked cell by cell.
pub fn cells_in(bbox: (f64, f64, f64, f64), centre: (f64, f64)) -> Vec<(u32, u32)> {
    let n = f64::from(1u32 << CELL_Z);
    let cx = |lon: f64| (((lon + 180.0) / 360.0 * n).floor().max(0.0) as u32).min((1 << CELL_Z) - 1);
    let cy = |lat: f64| {
        let r = lat.clamp(-85.05112878, 85.05112878).to_radians();
        let f = (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0;
        ((f * n).floor().max(0.0) as u32).min((1 << CELL_Z) - 1)
    };
    let (x0, x1, y0, y1) = (cx(bbox.0), cx(bbox.2), cy(bbox.3), cy(bbox.1));
    if x1 < x0 || y1 < y0 {
        return Vec::new();
    }
    let (mx, my) = (i64::from(cx(centre.0)), i64::from(cy(centre.1)));
    let inside = |x: i64, y: i64| x >= i64::from(x0) && x <= i64::from(x1) && y >= i64::from(y0) && y <= i64::from(y1);
    let reach = i64::from((x1 - x0).max(y1 - y0));
    let mut out = Vec::new();
    for ring in 0..=reach {
        for dx in -ring..=ring {
            for dy in -ring..=ring {
                if out.len() >= MAX_CELLS {
                    return out;
                }
                if ring > 0 && dx.abs() != ring && dy.abs() != ring {
                    continue;
                }
                if inside(mx + dx, my + dy) {
                    out.push(((mx + dx) as u32, (my + dy) as u32));
                }
            }
        }
    }
    out
}

fn scaled(c: [u8; 3], k: f32, alpha: f32) -> Color32 {
    let f = |v: u8| (f32::from(v) * k).clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(f(c[0]), f(c[1]), f(c[2]), (alpha * 255.0) as u8)
}

/// Walls and roofs of `list` in one mesh, drawn far to near. `ground` gives
/// the terrain height at a position. Walls facing away are left out; each
/// wall darkens toward its foot, which is the only cue that these are
/// volumes (`fill-extrusion-vertical-gradient`).
pub fn extrude(cam: &Camera, origin: Pos2, list: &[&Building], ground: &dyn Fn(f64, f64) -> f64) -> Mesh {
    let eye = camera_position(cam);
    let mut items: Vec<(f64, &Building, f64)> = list
        .iter()
        .map(|b| {
            let (lng, lat) = centroid(&b.ring);
            let g = ground(lng, lat);
            let c = cam.local_of(lng, lat, g);
            let d = (c[0] - eye[0]).powi(2) + (c[1] - eye[1]).powi(2) + (c[2] - eye[2]).powi(2);
            (d, *b, g)
        })
        .collect();
    items.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut mesh = Mesh::default();
    let at = |s: crate::map3d::camera::Screen| Pos2::new(origin.x + s.x as f32, origin.y + s.y as f32);
    for (_, b, g) in items {
        let base = g + b.min_height;
        let top = g + b.height;
        let local: Vec<(V3, V3)> = b
            .ring
            .iter()
            .map(|(lng, lat)| (cam.local_of(*lng, *lat, base), cam.local_of(*lng, *lat, top)))
            .collect();
        let area: f64 = (0..local.len())
            .map(|i| {
                let (p, q) = (local[i].0, local[(i + 1) % local.len()].0);
                p[0] * q[1] - q[0] * p[1]
            })
            .sum();
        let sign = if area >= 0.0 { 1.0 } else { -1.0 };
        for i in 0..local.len() {
            let (a, b2) = (local[i], local[(i + 1) % local.len()]);
            let (dx, dy) = (b2.0[0] - a.0[0], b2.0[1] - a.0[1]);
            let normal = (dy * sign, -dx * sign);
            let len = (normal.0 * normal.0 + normal.1 * normal.1).sqrt();
            if len < 1e-6 {
                continue;
            }
            let (mx, my) = ((a.0[0] + b2.0[0]) / 2.0, (a.0[1] + b2.0[1]) / 2.0);
            if normal.0 * (eye[0] - mx) + normal.1 * (eye[1] - my) <= 0.0 {
                continue;
            }
            let lit = 0.78 + 0.22 * ((normal.0 * -0.45 + normal.1 * 0.89) / len).max(0.0) as f32;
            let (Some(p0), Some(p1), Some(p2), Some(p3)) =
                (cam.project(a.0), cam.project(b2.0), cam.project(b2.1), cam.project(a.1))
            else {
                continue;
            };
            let v = mesh.vertices.len() as u32;
            for (s, k) in [(p0, 0.62), (p1, 0.62), (p2, 1.0), (p3, 1.0)] {
                mesh.colored_vertex(at(s), scaled(COLOR, lit * k, OPACITY));
            }
            mesh.add_triangle(v, v + 1, v + 2);
            mesh.add_triangle(v, v + 2, v + 3);
        }
        let roof: Vec<Option<Pos2>> = local.iter().map(|p| cam.project(p.1).map(at)).collect();
        if roof.iter().all(Option::is_some) {
            let pts: Vec<Pos2> = roof.into_iter().flatten().collect();
            let v = mesh.vertices.len() as u32;
            for p in &pts {
                mesh.colored_vertex(*p, scaled(COLOR, 1.06, OPACITY));
            }
            for t in crate::setup::geofence_card::triangulate(&pts) {
                mesh.add_triangle(v + t[0] as u32, v + t[1] as u32, v + t[2] as u32);
            }
        }
    }
    mesh
}

fn camera_position(cam: &Camera) -> V3 {
    cam.eye()
}

fn centroid(ring: &[(f64, f64)]) -> (f64, f64) {
    let n = ring.len() as f64;
    (
        ring.iter().map(|p| p.0).sum::<f64>() / n,
        ring.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

/// What a frame of buildings is painted into.
pub fn paint(painter: &egui::Painter, mesh: Mesh) {
    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gz(s: &str) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(s.as_bytes()).unwrap();
        e.finish().unwrap()
    }

    const CELL: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[11.0,48.0],[11.001,48.0],[11.001,48.001],[11.0,48.0]]]},"properties":{"height":12.5,"min_height":2}},
        {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[11.0,48.0],[11.001,48.0]]]},"properties":{}}]}"#;

    #[test]
    fn a_gzipped_cell_is_read_and_a_degenerate_outline_is_dropped() {
        let b = decode(&gz(CELL)).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].height, b[0].min_height, b[0].ring.len()), (12.5, 2.0, 3));
        assert_eq!(decode(CELL.as_bytes()).unwrap().len(), 1, "plain JSON too");
    }

    #[test]
    fn a_cell_without_an_answer_is_a_miss() {
        assert!(decode(br#"{"type":"FeatureCollection","features":[],"pending":true}"#).is_none());
        assert!(decode(br#"{"type":"FeatureCollection","features":[],"unavailable":true}"#).is_none());
        assert!(decode(b"not json").is_none());
        assert_eq!(
            decode(br#"{"type":"FeatureCollection","features":[]}"#).unwrap().len(),
            0
        );
    }

    #[test]
    fn cells_come_nearest_first_and_are_capped() {
        let bbox = (11.60, 48.04, 11.70, 48.11);
        let cells = cells_in(bbox, (11.65, 48.075));
        assert!(cells.len() <= MAX_CELLS && cells.len() > 4);
        let n = f64::from(1u32 << CELL_Z);
        let centre = (((11.65f64 + 180.0) / 360.0 * n).floor() as u32, 0);
        assert_eq!(cells[0].0, centre.0, "the cell under the centre first");
        assert!(
            cells_in((11.7, 48.1, 11.6, 48.0), (11.65, 48.05)).is_empty(),
            "an inverted box has none"
        );
    }

    #[test]
    fn a_house_gets_front_walls_and_a_roof() {
        let cam = Camera {
            lng: 11.0005,
            lat: 48.0005,
            zoom: 18.0,
            pitch: 60.0,
            bearing: 0.0,
            size: (800.0, 600.0),
            ground: 0.0,
        };
        let b = Building {
            ring: vec![(11.0, 48.0), (11.001, 48.0), (11.001, 48.001), (11.0, 48.001)],
            height: 10.0,
            min_height: 0.0,
        };
        let mesh = extrude(&cam, Pos2::ZERO, &[&b], &|_, _| 0.0);
        let walls = (mesh.vertices.len() - 4) / 4;
        assert!((1..=2).contains(&walls), "only the walls that face the camera: {walls}");
        assert_eq!(mesh.indices.len() % 3, 0);
        assert!(!mesh.is_empty());
    }
}
