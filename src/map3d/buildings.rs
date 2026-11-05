//! OSM buildings for the 3D map (`python/src/js/map.js` `addBuildingLayer`,
//! `refreshBuildings`, corvus/buildings.py): the footprints and heights of
//! the cells the backend serves at `/api/buildings/15/x/y.json`, and the
//! extrusion of them into walls and roofs.
//!
//! The backend never waits on Overpass. A cell it has not got comes back as
//! `pending` (asked for again a few seconds later) or `unavailable`
//! (offline); here both are a miss that the cache asks for again after a
//! pause, and what is cached is only a real answer.

use std::io::Read;

use eframe::egui::Pos2;
use serde_json::Value;

use crate::map3d::camera::mercator;
use crate::map3d::gpu::{MeshData, FLOATS};

/// `BUILDING_CELL_Z`: the grid the backend addresses cells on.
pub const CELL_Z: u8 = 15;
/// `BUILDING_MIN_ZOOM`: under it the extrusions are slivers.
pub const MIN_ZOOM: f64 = 14.0;
/// `BUILDING_MAX_CELLS`.
pub const MAX_CELLS: usize = 36;
/// What one cell extrudes at most.
pub const MAX_DRAWN: usize = 4000;
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

/// `BUILDING_COLOR` as the shader's base colour.
pub fn color() -> [f32; 3] {
    [
        COLOR[0] as f32 / 255.0,
        COLOR[1] as f32 / 255.0,
        COLOR[2] as f32 / 255.0,
    ]
}

/// `fill-extrusion-opacity`.
pub const fn opacity() -> f32 {
    OPACITY
}

fn centroid(ring: &[(f64, f64)]) -> (f64, f64) {
    let n = ring.len() as f64;
    (
        ring.iter().map(|p| p.0).sum::<f64>() / n,
        ring.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

/// Walls and roofs of one cell as a mesh around `origin` (web mercator
/// metres). `ground` gives the terrain height at a position; the mesh is
/// only worth building once it knows it for the first building, which
/// `None` here reports. Each wall darkens toward its foot, which is the only
/// cue that these are volumes (`fill-extrusion-vertical-gradient`), and
/// walls facing north-west are lit a little more.
pub fn cell_mesh(list: &[Building], origin: (f64, f64), ground: &dyn Fn(f64, f64) -> Option<f64>) -> Option<MeshData> {
    let mut verts: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let push = |verts: &mut Vec<f32>, x: f64, y: f64, z: f64, shade: f32| {
        verts.extend_from_slice(&[x as f32, y as f32, z as f32, 0.0, 0.0, shade]);
        (verts.len() / FLOATS - 1) as u32
    };
    let mut resolved = false;
    for b in list.iter().take(MAX_DRAWN) {
        let (lng, lat) = centroid(&b.ring);
        let g = match ground(lng, lat) {
            Some(g) => {
                resolved = true;
                g
            }
            None if resolved => continue,
            None => return None,
        };
        let (base, top) = (g + b.min_height, g + b.height);
        let ring: Vec<(f64, f64)> = b
            .ring
            .iter()
            .map(|(lng, lat)| {
                let (x, y) = mercator(*lng, *lat);
                (x - origin.0, y - origin.1)
            })
            .collect();
        let area: f64 = (0..ring.len())
            .map(|i| {
                let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
                p.0 * q.1 - q.0 * p.1
            })
            .sum();
        let sign = if area >= 0.0 { 1.0 } else { -1.0 };
        for i in 0..ring.len() {
            let (a, c) = (ring[i], ring[(i + 1) % ring.len()]);
            let (dx, dy) = (c.0 - a.0, c.1 - a.1);
            let (nx, ny) = (dy * sign, -dx * sign);
            let len = (nx * nx + ny * ny).sqrt();
            if len < 1e-9 {
                continue;
            }
            let lit = 0.78 + 0.22 * ((nx * -0.45 + ny * 0.89) / len).max(0.0) as f32;
            let v = [
                push(&mut verts, a.0, a.1, base, lit * 0.62),
                push(&mut verts, c.0, c.1, base, lit * 0.62),
                push(&mut verts, c.0, c.1, top, lit),
                push(&mut verts, a.0, a.1, top, lit),
            ];
            indices.extend_from_slice(&[v[0], v[1], v[2], v[0], v[2], v[3]]);
        }
        let pts: Vec<Pos2> = ring.iter().map(|(x, y)| Pos2::new(*x as f32, *y as f32)).collect();
        let first = verts.len() / FLOATS;
        for p in &ring {
            push(&mut verts, p.0, p.1, top, 1.06);
        }
        for t in crate::setup::geofence_card::triangulate(&pts) {
            indices.extend_from_slice(&[(first + t[0]) as u32, (first + t[1]) as u32, (first + t[2]) as u32]);
        }
    }
    (!indices.is_empty()).then(|| MeshData::new(verts, indices))
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
    fn a_house_gets_four_walls_and_a_roof() {
        let b = Building {
            ring: vec![(11.0, 48.0), (11.001, 48.0), (11.001, 48.001), (11.0, 48.001)],
            height: 10.0,
            min_height: 0.0,
        };
        let origin = mercator(11.0, 48.0);
        let m = cell_mesh(&[b], origin, &|_, _| Some(500.0)).unwrap();
        // 4 walls of 4 vertices and a roof of 4 vertices; 8 wall and 2 roof triangles.
        assert_eq!(m.verts.len() / FLOATS, 20);
        assert_eq!(m.indices.len(), 30);
        let tops = m.verts.chunks(FLOATS).map(|v| v[2]).fold(0.0f32, f32::max);
        assert!((tops - 510.0).abs() < 1e-3, "the roof is 10 m over the ground: {tops}");
    }

    /// What the backend serves for a cell whose Overpass answer was stubbed
    /// (`buildings_cell.json.gz`, made by running the real `BuildingService`
    /// against a stand-in Overpass server: two ways and a multipolygon
    /// relation, one of the ways with a height tag, one with building:levels,
    /// and a road that is not a building). The sandbox blocks the real one.
    const SERVED: &[u8] = include_bytes!("buildings_cell.json.gz");

    #[test]
    fn a_cell_the_backend_made_from_an_overpass_answer_is_read_and_extruded() {
        assert!(
            SERVED.starts_with(&[0x1f, 0x8b]),
            "served gzipped, as the backend stores it"
        );
        let b = decode(SERVED).expect("a cell");
        let heights: Vec<f64> = b.iter().map(|b| b.height).collect();
        assert_eq!(heights, vec![12.0, 13.8, 8.0], "the road is not a building");
        assert!(b.iter().all(|b| b.min_height == 0.0 && b.ring.len() == 4));
        let origin = mercator(11.64, 48.08);
        let m = cell_mesh(&b, origin, &|_, _| Some(520.0)).expect("a mesh");
        // Three houses of four walls and a roof: 20 vertices each.
        assert_eq!(m.verts.len() / FLOATS, 60);
        let top = m.verts.chunks(FLOATS).map(|v| v[2]).fold(0.0f32, f32::max);
        assert!(
            (top - 533.8).abs() < 1e-3,
            "the tallest roof is 13.8 m over the ground: {top}"
        );
        // Nothing is under the ground, and nothing floats.
        let low = m.verts.chunks(FLOATS).map(|v| v[2]).fold(f32::MAX, f32::min);
        assert!((low - 520.0).abs() < 1e-3, "{low}");
    }

    #[test]
    fn a_cell_waits_for_the_terrain() {
        let b = Building {
            ring: vec![(11.0, 48.0), (11.001, 48.0), (11.0, 48.001)],
            height: 5.0,
            min_height: 0.0,
        };
        assert!(cell_mesh(&[b], (0.0, 0.0), &|_, _| None).is_none());
    }
}
