//! The ground under the route (`sampleGround`, `pointAtDistance`,
//! `terrainZoomFor` of `mission.js`): the elevation model sampled along the
//! plan, in metres above the start, so the profile and the clearance are read
//! against the same zero as every height in the plan.
//!
//! Best effort: the tiles come through the backend's cache and arrive when
//! they arrive. A plan with no ground under it is a drawable state.

use eframe::egui;
use serde_json::Value;

use crate::map3d::terrain::{self, Terrain};
use crate::mission::geometry::{Ground, Station};
use crate::net::http::Server;

/// How many points the terrain profile is sampled at.
pub const PROFILE_SAMPLES: usize = 240;
/// Tiles one pass may touch; past this the zoom drops a level.
const TILE_BUDGET: u32 = 48;
/// A pass that has not resolved after this many tries is let go of.
const MAX_TRIES: u32 = 40;

/// The point `distance` metres along a station list, as `(lon, lat)`.
pub fn point_at_distance(points: &[Station], distance: f64) -> (f64, f64) {
    for i in 1..points.len() {
        if distance <= points[i].distance {
            let span = points[i].distance - points[i - 1].distance;
            let f = if span > 0.0 {
                (distance - points[i - 1].distance) / span
            } else {
                0.0
            };
            return (
                points[i - 1].lon + (points[i].lon - points[i - 1].lon) * f,
                points[i - 1].lat + (points[i].lat - points[i - 1].lat) * f,
            );
        }
    }
    points.last().map_or((0.0, 0.0), |l| (l.lon, l.lat))
}

fn lon_to_tile(lon: f64, zoom: u8) -> i64 {
    (((lon + 180.0) / 360.0) * f64::from(1u32 << zoom)).floor() as i64
}

fn lat_to_tile(lat: f64, zoom: u8) -> i64 {
    let r = lat.to_radians();
    ((1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * f64::from(1u32 << zoom)).floor() as i64
}

/// The highest DEM zoom whose tile count for this route stays inside the
/// budget. A long route is sampled coarsely rather than not at all.
pub fn zoom_for(points: &[Station], total: f64, max_zoom: u8) -> u8 {
    let max = max_zoom.min(14);
    let (mut w, mut e, mut s, mut n) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in points {
        w = w.min(p.lon);
        e = e.max(p.lon);
        s = s.min(p.lat);
        n = n.max(p.lat);
    }
    let mut zoom = max;
    while zoom > 6 {
        let (x0, x1) = (lon_to_tile(w, zoom), lon_to_tile(e, zoom));
        let (y0, y1) = (lat_to_tile(n, zoom), lat_to_tile(s, zoom));
        if ((x1 - x0 + 1) * (y1 - y0 + 1)) as u32 <= TILE_BUDGET {
            return zoom;
        }
        zoom -= 1;
    }
    if total > 200_000.0 {
        7
    } else {
        8
    }
}

/// What a pass found.
pub struct Found {
    pub ground: Option<Ground>,
    /// The elevation of the start, which the plan keeps with its home.
    pub base: Option<f64>,
}

#[derive(Default)]
pub struct Sampler {
    terrain: Option<Terrain>,
    sources: Value,
    server: Option<Server>,
    key: String,
    tries: u32,
    next: f64,
}

impl Sampler {
    pub fn new(server: Server, sources: Value) -> Sampler {
        Sampler {
            server: Some(server),
            sources,
            ..Sampler::default()
        }
    }

    /// The route's identity: where it goes, not how high.
    fn key_of(points: &[Station], drives: bool) -> String {
        let mut k = String::from(if drives { "d" } else { "f" });
        for p in points {
            k += &format!("{:.7},{:.7};", p.lat, p.lon);
        }
        k
    }

    /// One look at the route: starts a pass when it changed, and retries one
    /// that tiles were missing for. Returns the result when there is one.
    pub fn update(&mut self, ctx: &egui::Context, now: f64, points: &[Station], drives: bool) -> Option<Found> {
        let key = Self::key_of(points, drives);
        let changed = key != self.key;
        if changed {
            self.key = key;
            self.tries = 0;
            self.next = 0.0;
        }
        if drives || points.len() < 2 || points.last().is_none_or(|p| p.distance <= 0.0) {
            return changed.then_some(Found {
                ground: None,
                base: None,
            });
        }
        if self.tries >= MAX_TRIES || now < self.next {
            return None;
        }
        if self.terrain.is_none() {
            let spec = terrain::choose(&self.sources, "", None)?;
            self.terrain = Some(Terrain::new(ctx, self.server.as_ref()?, spec));
        }
        let t = self.terrain.as_ref()?;
        self.tries += 1;
        self.next = now + 0.5;
        let total = points.last().map_or(0.0, |p| p.distance);
        let zoom = f64::from(zoom_for(points, total, t.spec.maxzoom));
        let mut distances = Vec::with_capacity(PROFILE_SAMPLES);
        let mut heights = Vec::with_capacity(PROFILE_SAMPLES);
        let mut missing = 0;
        for i in 0..PROFILE_SAMPLES {
            let d = i as f64 / (PROFILE_SAMPLES - 1) as f64 * total;
            let (lon, lat) = point_at_distance(points, d);
            let h = t.height_at(lon, lat, zoom);
            missing += usize::from(h.is_none());
            distances.push(d);
            heights.push(h);
        }
        if missing > 0 && self.tries < MAX_TRIES {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return None;
        }
        self.tries = MAX_TRIES;
        if heights.iter().all(Option::is_none) {
            return Some(Found {
                ground: None,
                base: None,
            });
        }
        let base = heights[0].unwrap_or(0.0);
        Some(Found {
            base: heights[0],
            ground: Some(Ground {
                distances,
                elevations: heights.iter().map(|h| h.map_or(0.0, |v| v - base)).collect(),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::geometry::StationKind;
    use crate::mission::plan::Kind;

    fn st(lon: f64, lat: f64, distance: f64) -> Station {
        Station {
            id: 1,
            kind: StationKind::Item(Kind::Waypoint),
            lat,
            lon,
            alt: 0.0,
            distance,
            approach: None,
            pitch: None,
        }
    }

    #[test]
    fn a_distance_along_the_route_is_a_point_on_it() {
        let pts = [st(11.0, 48.0, 0.0), st(11.0, 48.01, 1000.0), st(11.02, 48.01, 2000.0)];
        assert_eq!(point_at_distance(&pts, 0.0), (11.0, 48.0));
        let (lon, lat) = point_at_distance(&pts, 500.0);
        assert!((lon - 11.0).abs() < 1e-12 && (lat - 48.005).abs() < 1e-12);
        let (lon, lat) = point_at_distance(&pts, 1500.0);
        assert!((lon - 11.01).abs() < 1e-12 && (lat - 48.01).abs() < 1e-12);
        assert_eq!(
            point_at_distance(&pts, 9999.0),
            (11.02, 48.01),
            "past the end is the end"
        );
        assert_eq!(
            point_at_distance(&[st(1.0, 2.0, 0.0), st(1.0, 2.0, 0.0)], 0.0),
            (1.0, 2.0)
        );
    }

    #[test]
    fn the_zoom_drops_until_the_tiles_fit_the_budget() {
        let near = [st(11.0, 48.0, 0.0), st(11.003, 48.001, 300.0)];
        assert_eq!(
            zoom_for(&near, 300.0, 14),
            14,
            "a small plan is read as finely as the model goes"
        );
        assert_eq!(zoom_for(&near, 300.0, 10), 10, "and no finer");
        let far = [st(8.0, 47.0, 0.0), st(13.0, 50.0, 400_000.0)];
        let z = zoom_for(&far, 400_000.0, 14);
        assert!((7..10).contains(&z), "{z}");
    }

    #[test]
    fn tile_numbers_are_the_slippy_ones() {
        assert_eq!(lon_to_tile(-180.0, 1), 0);
        assert_eq!(lon_to_tile(0.0, 1), 1);
        assert_eq!(lat_to_tile(0.0, 1), 1);
        assert_eq!(lat_to_tile(85.0, 1), 0);
    }
}
