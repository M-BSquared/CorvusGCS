//! The planner's pure geometry: distances, stations, the altitude profile,
//! durations and the pattern sweeps (`src/js/mission-geometry.js`), with no
//! screen and no map.

use crate::mission::plan::{clamp_number, Home, Item, Kind, ReturnProfile};
use crate::mission::plan::{Aircraft, DEFAULT_RADIUS_M};

pub const NOMINAL_SPEED_MS: f64 = 10.0;
/// A fixed wing's climb-out when the takeoff names no pitch.
pub const TAKEOFF_PITCH_DEG: f64 = 10.0;
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;
pub const PATTERN_CIRCLE_MIN_M: f64 = 10.0;
pub const PATTERN_CIRCLE_SEGMENTS: usize = 48;
const PATTERN_MITER_LIMIT: f64 = 2.5;

/// A pattern setting: bounds and default.
#[derive(Debug, Clone, Copy)]
pub struct Range {
    pub min: f64,
    pub max: f64,
    pub def: f64,
}

pub const PATTERN_SPACING_M: Range = Range {
    min: 5.0,
    max: 500.0,
    def: 30.0,
};
pub const PATTERN_WIDTH_M: Range = Range {
    min: 10.0,
    max: 2000.0,
    def: 100.0,
};

/// Great-circle distance in metres between two points.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let d_lat = p2 - p1;
    let d_lon = (lon2 - lon1).to_radians();
    let h = (d_lat / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

/// Distance between two `[lon, lat]` points.
pub fn distance_ll(a: [f64; 2], b: [f64; 2]) -> f64 {
    distance_m(a[1], a[0], b[1], b[0])
}

/// What a station is: the start, or an item of the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StationKind {
    Home,
    Item(Kind),
}

/// A profile station: where the route is, how high, and how far along.
#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    /// The item's id; 0 for the start.
    pub id: u32,
    pub kind: StationKind,
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub distance: f64,
    pub approach: Option<f64>,
    pub pitch: Option<f64>,
}

impl Station {
    fn is(&self, kind: Kind) -> bool {
        self.kind == StationKind::Item(kind)
    }
}

/// The route as profile stations: the start when there is one, then every
/// positioned item in order, each with the cumulative ground distance. A
/// return is a station too, drawn back at the start.
pub fn stations(list: &[Item], start: Option<&Home>) -> Vec<Station> {
    let mut out = Vec::new();
    let mut cumulative = 0.0;
    let mut previous: Option<(f64, f64)> = None;
    if let Some(s) = start {
        previous = Some((s.lat, s.lon));
        out.push(Station {
            id: 0,
            kind: StationKind::Home,
            lat: s.lat,
            lon: s.lon,
            alt: 0.0,
            distance: 0.0,
            approach: None,
            pitch: None,
        });
    }
    for item in list {
        let place = if item.kind.position() {
            (item.lat, item.lon)
        } else if let Some(s) = start {
            (s.lat, s.lon)
        } else {
            continue;
        };
        if let Some(p) = previous {
            cumulative += distance_m(p.0, p.1, place.0, place.1);
        }
        previous = Some(place);
        out.push(Station {
            id: item.id,
            kind: StationKind::Item(item.kind),
            lat: place.0,
            lon: place.1,
            alt: if item.kind.position() { item.alt } else { 0.0 },
            distance: cumulative,
            approach: (item.kind == Kind::Land).then_some(item.approach_alt).flatten(),
            pitch: (item.kind == Kind::Takeoff && item.pitch > 0.0).then_some(item.pitch),
        });
    }
    out
}

/// A drawn and draggable point of the profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub id: u32,
    /// `None` for the start; `Descent` marks are separate.
    pub kind: MarkKind,
    pub distance: f64,
    pub alt: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Home,
    Item(Kind),
    /// The corner a multicopter's landing descent starts from.
    Descent,
}

/// The profile's line, in metres along and metres up, and its marks.
pub struct Shape {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub marks: Vec<Mark>,
}

/// The route as the altitude profile draws it, for the plan's aircraft.
pub fn profile_shape(points: &[Station], kind: Option<Aircraft>, ret: Option<&ReturnProfile>) -> Shape {
    let mut shape = Shape {
        x: Vec::new(),
        y: Vec::new(),
        marks: Vec::new(),
    };
    for (index, station) in points.iter().enumerate() {
        let before = index.checked_sub(1).map(|i| &points[i]);
        let after = points.get(index + 1);
        let mut mark = Mark {
            id: station.id,
            kind: match station.kind {
                StationKind::Home => MarkKind::Home,
                StationKind::Item(k) => MarkKind::Item(k),
            },
            distance: station.distance,
            alt: station.alt,
        };
        let mut at = |m: f64, h: f64| {
            shape.x.push(m);
            shape.y.push(h);
        };
        match (kind, before) {
            (Some(Aircraft::FixedWing), Some(b)) if station.is(Kind::Takeoff) => {
                mark.distance = climb_out_end(b, station, after);
                at(mark.distance, station.alt);
            }
            (Some(Aircraft::Multirotor), Some(b)) if station.is(Kind::Land) => {
                let from = station.approach.unwrap_or(b.alt);
                at(station.distance, from);
                shape.marks.push(Mark {
                    id: station.id,
                    kind: MarkKind::Descent,
                    distance: station.distance,
                    alt: from,
                });
                at(station.distance, station.alt);
            }
            (Some(k @ (Aircraft::Multirotor | Aircraft::FixedWing)), Some(b)) if station.is(Kind::Rtl) => {
                let path = return_path(b, station, k, ret);
                for (d, h) in &path {
                    at(*d, *h);
                }
                mark.alt = path.last().map_or(mark.alt, |p| p.1);
            }
            _ => at(station.distance, station.alt),
        }
        shape.marks.push(mark);
    }
    shape
}

pub fn profile_marks(points: &[Station], kind: Option<Aircraft>, ret: Option<&ReturnProfile>) -> Vec<Mark> {
    profile_shape(points, kind, ret).marks
}

/// A fixed wing's climb-out: the run it needs, the room the first leg gives
/// it, the height and the pitch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Climb {
    pub run: f64,
    pub room: f64,
    pub height: f64,
    pub pitch: f64,
}

pub fn takeoff_climb(before: &Station, station: &Station, after: Option<&Station>) -> Option<Climb> {
    let height = station.alt - before.alt;
    if height <= 0.0 || height.is_nan() {
        return None;
    }
    let pitch = station.pitch.filter(|p| *p > 0.0).unwrap_or(TAKEOFF_PITCH_DEG);
    let run = height / pitch.to_radians().tan();
    let room = after.map_or(f64::INFINITY, |a| (a.distance - before.distance).max(0.0));
    Some(Climb {
        run,
        room,
        height,
        pitch,
    })
}

/// Where a fixed wing's climb-out reaches the takeoff height, in metres along.
pub fn climb_out_end(before: &Station, station: &Station, after: Option<&Station>) -> f64 {
    match takeoff_climb(before, station, after) {
        None => station.distance,
        Some(c) => station.distance.max(before.distance + c.run.min(c.room)),
    }
}

/// A Return as (metres along, metres up) after the point before it.
pub fn return_path(
    before: &Station,
    station: &Station,
    kind: Aircraft,
    ret: Option<&ReturnProfile>,
) -> Vec<(f64, f64)> {
    let known = ret.is_some_and(|r| r.known);
    let r = ret.copied().unwrap_or_default();
    if kind == Aircraft::FixedWing {
        let circle = if known {
            r.arrive_at.or(r.climb_to).unwrap_or(before.alt)
        } else {
            before.alt
        };
        return vec![(station.distance, circle)];
    }
    let high = match (known, r.climb_to) {
        (true, Some(c)) => before.alt.max(c),
        _ => before.alt,
    };
    let mut path = Vec::new();
    if high > before.alt {
        path.push((before.distance, high));
    }
    path.push((station.distance, high));
    if let (true, Some(a)) = (known, r.arrive_at) {
        if a < high {
            path.push((station.distance, a));
        }
    }
    if !(known && r.hold) {
        path.push((station.distance, station.alt));
    }
    path
}

/// The glide into the first landing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Approach {
    pub drop: f64,
    pub run: f64,
    pub degrees: f64,
}

pub fn landing_approach(points: &[Station]) -> Option<Approach> {
    let index = points.iter().position(|s| s.is(Kind::Land))?;
    if index < 1 {
        return None;
    }
    let (from, to) = (&points[index - 1], &points[index]);
    let drop = from.alt - to.alt;
    if drop <= 0.0 || drop.is_nan() {
        return None;
    }
    let run = (to.distance - from.distance).max(0.0);
    Some(Approach {
        drop,
        run,
        degrees: drop.atan2(run).to_degrees(),
    })
}

/// Ground distance the plan covers, orbit circumferences included.
pub fn route_length(list: &[Item], start: Option<&Home>) -> f64 {
    let points = stations(list, start);
    let mut total = points.last().map_or(0.0, |p| p.distance);
    for item in list {
        if item.kind == Kind::LoiterTurns {
            total += 2.0 * std::f64::consts::PI * item.radius * item.turns;
        }
    }
    total
}

/// The speed each item is reached at, in item order: a point that pins a
/// speed changes it from there on.
pub fn speed_by_item(list: &[Item], speed: Option<f64>) -> Vec<(u32, f64)> {
    let mut current = speed.filter(|s| *s > 0.0).unwrap_or(NOMINAL_SPEED_MS);
    list.iter()
        .map(|item| {
            if let Some(s) = item.speed.filter(|s| *s > 0.0) {
                current = s;
            }
            (item.id, current)
        })
        .collect()
}

/// Seconds the plan is likely to take, with timed holds on top.
pub fn route_duration(list: &[Item], start: Option<&Home>, speed: Option<f64>) -> f64 {
    let points = stations(list, start);
    let speeds = speed_by_item(list, speed);
    let at = |id: u32| speeds.iter().find(|s| s.0 == id).map_or(NOMINAL_SPEED_MS, |s| s.1);
    let mut seconds = 0.0;
    for i in 1..points.len() {
        let mps = at(points[i].id);
        seconds += (points[i].distance - points[i - 1].distance) / mps;
    }
    for item in list {
        match item.kind {
            Kind::LoiterTurns => {
                seconds += 2.0 * std::f64::consts::PI * item.radius * item.turns / at(item.id);
            }
            Kind::LoiterTime => seconds += item.seconds,
            Kind::Waypoint => seconds += item.hold,
            _ => {}
        }
    }
    seconds
}

/// A closed ring of `[lon, lat]` approximating a circle of `radius` metres.
pub fn circle_ring(lon: f64, lat: f64, radius: f64, segments: usize) -> Vec<[f64; 2]> {
    let d_lat = (radius / EARTH_RADIUS_M).to_degrees();
    let d_lon = d_lat / lat.to_radians().cos().max(0.01);
    (0..=segments)
        .map(|i| {
            let a = i as f64 / segments as f64 * 2.0 * std::f64::consts::PI;
            [lon + d_lon * a.cos(), lat + d_lat * a.sin()]
        })
        .collect()
}

/// An orbit's radius as drawn: its own, or the default for junk.
pub fn ring_radius(item: &Item) -> f64 {
    if item.radius > 0.0 {
        item.radius
    } else {
        DEFAULT_RADIUS_M
    }
}

/// Metres east/north of an origin and back, flat earth.
pub struct Frame {
    origin: [f64; 2],
    k: f64,
    c: f64,
}

pub fn local_frame(origin: [f64; 2]) -> Frame {
    Frame {
        origin,
        k: EARTH_RADIUS_M * std::f64::consts::PI / 180.0,
        c: origin[1].to_radians().cos().max(0.01),
    }
}

impl Frame {
    pub fn to_xy(&self, p: [f64; 2]) -> [f64; 2] {
        [
            (p[0] - self.origin[0]) * self.k * self.c,
            (p[1] - self.origin[1]) * self.k,
        ]
    }

    pub fn to_ll(&self, q: [f64; 2]) -> [f64; 2] {
        [
            self.origin[0] + q[0] / (self.k * self.c),
            self.origin[1] + q[1] / self.k,
        ]
    }
}

/// The back and forth sweep over a polygon of metres: parallel passes
/// `spacing` apart along the polygon's longest edge, alternating direction.
pub fn sweep_polygon(ring: &[[f64; 2]], spacing: f64) -> Vec<[f64; 2]> {
    if ring.len() < 3 || spacing <= 0.0 || spacing.is_nan() {
        return Vec::new();
    }
    let mut angle = 0.0;
    let mut longest = -1.0;
    for (i, a) in ring.iter().enumerate() {
        let b = ring[(i + 1) % ring.len()];
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        if len > longest {
            longest = len;
            angle = (b[1] - a[1]).atan2(b[0] - a[0]);
        }
    }
    let (cos, sin) = (angle.cos(), angle.sin());
    let rot: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| [p[0] * cos + p[1] * sin, -p[0] * sin + p[1] * cos])
        .collect();
    let lo = rot.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let hi = rot.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
    if hi - lo <= 0.0 || (hi - lo).is_nan() {
        return Vec::new();
    }
    let lines = (((hi - lo) / spacing).ceil() as usize).max(1);
    let step = (hi - lo) / lines as f64;
    let mut out = Vec::new();
    for k in 0..lines {
        let y = lo + (k as f64 + 0.5) * step;
        let mut xs = Vec::new();
        for (i, a) in rot.iter().enumerate() {
            let b = rot[(i + 1) % rot.len()];
            if (a[1] <= y && b[1] > y) || (b[1] <= y && a[1] > y) {
                xs.push(a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
            }
        }
        xs.sort_by(|u, v| u.total_cmp(v));
        let mut runs: Vec<(f64, f64)> = Vec::new();
        let mut i = 0;
        while i + 1 < xs.len() {
            if xs[i + 1] - xs[i] > 0.01 {
                runs.push((xs[i], xs[i + 1]));
            }
            i += 2;
        }
        if k % 2 == 1 {
            runs = runs.into_iter().rev().map(|(u, v)| (v, u)).collect();
        }
        for (u, v) in runs {
            out.push([u, y]);
            out.push([v, y]);
        }
    }
    out.into_iter()
        .map(|p| [p[0] * cos - p[1] * sin, p[0] * sin + p[1] * cos])
        .collect()
}

/// The sweep along a corridor: passes parallel to the centre line, spread
/// across `width` metres, `spacing` apart, alternating direction.
pub fn sweep_corridor(path: &[[f64; 2]], width: f64, spacing: f64) -> Vec<[f64; 2]> {
    let mut line: Vec<[f64; 2]> = Vec::new();
    for p in path {
        if line.last().is_none_or(|l| (p[0] - l[0]).hypot(p[1] - l[1]) > 0.5) {
            line.push(*p);
        }
    }
    if line.len() < 2 || width <= 0.0 || spacing <= 0.0 || width.is_nan() || spacing.is_nan() {
        return Vec::new();
    }
    let normals: Vec<[f64; 2]> = line
        .windows(2)
        .map(|w| {
            let (dx, dy) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
            let len = dx.hypot(dy);
            [-dy / len, dx / len]
        })
        .collect();
    let vertex: Vec<([f64; 2], f64)> = (0..line.len())
        .map(|i| {
            let a = normals[i.saturating_sub(1)];
            let b = normals[i.min(normals.len() - 1)];
            let (mut nx, mut ny) = (a[0] + b[0], a[1] + b[1]);
            let len = nx.hypot(ny);
            if len < 1e-6 {
                return (a, 1.0);
            }
            nx /= len;
            ny /= len;
            let dot = nx * a[0] + ny * a[1];
            ([nx, ny], PATTERN_MITER_LIMIT.min(1.0 / dot.max(1e-6)))
        })
        .collect();
    let lanes = ((width / spacing).ceil() as usize).max(1);
    let mut out = Vec::new();
    for j in 0..lanes {
        let d = -width / 2.0 + (j as f64 + 0.5) * (width / lanes as f64);
        let mut pass: Vec<[f64; 2]> = line
            .iter()
            .enumerate()
            .map(|(i, p)| {
                [
                    p[0] + vertex[i].0[0] * d * vertex[i].1,
                    p[1] + vertex[i].0[1] * d * vertex[i].1,
                ]
            })
            .collect();
        if j % 2 == 1 {
            pass.reverse();
        }
        out.extend(pass);
    }
    out
}

/// What a pattern covers.
pub use crate::mission::plan::Shape as PatternShape;

/// The waypoints that cover a pattern, as `[lon, lat]`. A circle is no sweep:
/// it is one Circle item, so it has no waypoints here.
pub fn pattern_waypoints(shape: PatternShape, pts: &[[f64; 2]], spacing: f64, width: f64) -> Vec<[f64; 2]> {
    let spacing = clamp_number(
        spacing,
        PATTERN_SPACING_M.min,
        PATTERN_SPACING_M.max,
        PATTERN_SPACING_M.def,
    );
    let width = clamp_number(width, PATTERN_WIDTH_M.min, PATTERN_WIDTH_M.max, PATTERN_WIDTH_M.def);
    let Some(first) = pts.first() else {
        return Vec::new();
    };
    let frame = local_frame(*first);
    let xy: Vec<[f64; 2]> = pts.iter().map(|p| frame.to_xy(*p)).collect();
    let path = match shape {
        PatternShape::Corridor => sweep_corridor(&xy, width, spacing),
        PatternShape::Area => sweep_polygon(&xy, spacing),
        PatternShape::Circle => Vec::new(),
    };
    let mut out: Vec<[f64; 2]> = Vec::new();
    for p in path {
        let ll = frame.to_ll(p);
        if out.last().is_none_or(|l| distance_ll(*l, ll) > 0.5) {
            out.push(ll);
        }
    }
    out
}

/// Stations meant to be on the ground: their clearance is zero by design.
pub fn grounded(kind: StationKind) -> bool {
    matches!(
        kind,
        StationKind::Home | StationKind::Item(Kind::Land) | StationKind::Item(Kind::Rtl)
    )
}

/// The sampled ground under the route, in metres above the start.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ground {
    pub distances: Vec<f64>,
    pub elevations: Vec<f64>,
}

/// Ground clearance at every station, or None where it does not apply.
pub fn clearances(points: &[Station], ground: Option<&Ground>) -> Vec<Option<f64>> {
    let Some(g) = ground.filter(|g| !g.distances.is_empty()) else {
        return points.iter().map(|_| None).collect();
    };
    points
        .iter()
        .map(|s| {
            if grounded(s.kind) {
                None
            } else {
                interpolate_at(g, s.distance).map(|under| s.alt - under)
            }
        })
        .collect()
}

/// Linear read of a sampled profile at `distance` metres along it.
pub fn interpolate_at(p: &Ground, distance: f64) -> Option<f64> {
    let (xs, ys) = (&p.distances, &p.elevations);
    if xs.is_empty() {
        return None;
    }
    if distance <= xs[0] {
        return Some(ys[0]);
    }
    if distance >= xs[xs.len() - 1] {
        return Some(ys[ys.len() - 1]);
    }
    let (mut low, mut high) = (0, xs.len() - 1);
    while high - low > 1 {
        let mid = (low + high) >> 1;
        if xs[mid] <= distance {
            low = mid;
        } else {
            high = mid;
        }
    }
    let span = xs[high] - xs[low];
    if span <= 0.0 || span.is_nan() {
        return Some(ys[low]);
    }
    Some(ys[low] + (ys[high] - ys[low]) * ((distance - xs[low]) / span))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: Home = Home {
        lat: 48.0,
        lon: 11.0,
        elevation: None,
    };

    fn item(id: u32, kind: Kind, lat: f64, alt: f64) -> Item {
        let mut i = Item::new(id, kind);
        i.lat = lat;
        i.lon = 11.0;
        i.alt = alt;
        i
    }

    fn route() -> Vec<Item> {
        vec![
            item(1, Kind::Takeoff, 48.0, 20.0),
            item(2, Kind::Waypoint, 48.01, 50.0),
            Item::new(3, Kind::Rtl),
        ]
    }

    #[test]
    fn a_degree_of_latitude_is_about_111_km() {
        assert!((distance_m(48.0, 11.0, 49.0, 11.0) - 111_195.0).abs() < 200.0);
        assert_eq!(distance_m(48.0, 11.0, 48.0, 11.0), 0.0);
    }

    #[test]
    fn stations_start_at_home_and_a_return_comes_back_to_it() {
        let s = stations(&route(), Some(&HOME));
        let kinds: Vec<StationKind> = s.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            [
                StationKind::Home,
                StationKind::Item(Kind::Takeoff),
                StationKind::Item(Kind::Waypoint),
                StationKind::Item(Kind::Rtl)
            ]
        );
        assert_eq!((s[0].distance, s[0].alt, s[1].distance), (0.0, 0.0, 0.0));
        assert!(s[2].distance > 1000.0);
        assert_eq!((s[3].lat, s[3].lon), (HOME.lat, HOME.lon));
        assert!(s[3].distance > s[2].distance);
        assert!(
            stations(&[Item::new(1, Kind::Rtl)], None).is_empty(),
            "a return with no start"
        );
        assert_eq!(stations(&route()[..2], None).len(), 2);
    }

    #[test]
    fn orbits_add_their_circumference_to_the_length_and_holds_to_the_time() {
        let mut orbit = item(1, Kind::LoiterTurns, 48.0, 30.0);
        orbit.turns = 2.0;
        orbit.radius = 100.0;
        let len = route_length(&[orbit], Some(&HOME));
        assert!((len - 2.0 * 2.0 * std::f64::consts::PI * 100.0).abs() < 1e-6);
        let mut w = item(1, Kind::Waypoint, 48.0, 30.0);
        w.hold = 30.0;
        let mut h = item(2, Kind::LoiterTime, 48.0, 30.0);
        h.seconds = 60.0;
        assert!((route_duration(&[w, h], Some(&HOME), Some(10.0)) - 90.0).abs() < 1e-6);
    }

    #[test]
    fn a_pinned_speed_carries_to_the_points_after_it() {
        let mut a = item(1, Kind::Waypoint, 48.0, 30.0);
        let mut b = a.clone();
        let mut c = a.clone();
        let mut d = a.clone();
        a.id = 1;
        b.id = 2;
        b.speed = Some(4.0);
        c.id = 3;
        d.id = 4;
        d.speed = Some(12.0);
        let speeds: Vec<f64> = speed_by_item(&[a.clone(), b, c, d], Some(9.0))
            .iter()
            .map(|s| s.1)
            .collect();
        assert_eq!(speeds, [9.0, 4.0, 4.0, 12.0]);
        assert!(speed_by_item(&[a], None)[0].1 > 0.0);
    }

    #[test]
    fn a_slow_leg_is_timed_at_its_own_speed() {
        let a = item(1, Kind::Waypoint, 48.008993, 30.0);
        let mut b = item(2, Kind::Waypoint, 48.017986, 30.0);
        b.speed = Some(5.0);
        let secs = route_duration(&[a, b], Some(&HOME), Some(10.0));
        assert!((secs - 300.0).abs() < 5.0, "{secs}");
    }

    #[test]
    fn the_ring_closes_and_starts_due_east() {
        let ring = circle_ring(11.0, 48.0, 100.0, 32);
        assert_eq!(ring.len(), 33);
        assert_eq!(ring[0], ring[32]);
        assert!((distance_ll([11.0, 48.0], ring[0]) - 100.0).abs() < 5.0);
        assert!((ring[0][1] - 48.0).abs() < 1e-9 && ring[0][0] > 11.0);
    }

    fn station(id: u32, kind: StationKind, alt: f64, distance: f64) -> Station {
        Station {
            id,
            kind,
            lat: 0.0,
            lon: 0.0,
            alt,
            distance,
            approach: None,
            pitch: None,
        }
    }

    fn line_points() -> Vec<Station> {
        vec![
            station(0, StationKind::Home, 0.0, 0.0),
            station(1, StationKind::Item(Kind::Takeoff), 30.0, 0.0),
            station(2, StationKind::Item(Kind::Waypoint), 40.0, 300.0),
            station(3, StationKind::Item(Kind::Land), 0.0, 700.0),
        ]
    }

    #[test]
    fn a_multicopter_comes_straight_down_and_a_fixed_wing_glides() {
        let points = line_points();
        let copter = profile_shape(&points, Some(Aircraft::Multirotor), None);
        assert_eq!(copter.x, [0.0, 0.0, 300.0, 700.0, 700.0]);
        assert_eq!(copter.y, [0.0, 30.0, 40.0, 40.0, 0.0]);
        assert!(copter
            .marks
            .iter()
            .any(|m| m.kind == MarkKind::Descent && m.alt == 40.0));
        let plane = profile_shape(&points, Some(Aircraft::FixedWing), None);
        assert_eq!(plane.y, [0.0, 30.0, 40.0, 0.0]);
        assert_eq!(&plane.x[2..], &[300.0, 700.0]);
        let plain = profile_shape(&points, None, None);
        assert_eq!(
            (plain.x, plain.y),
            (vec![0.0, 0.0, 300.0, 700.0], vec![0.0, 30.0, 40.0, 0.0])
        );
        let ret = vec![
            station(0, StationKind::Home, 0.0, 0.0),
            station(1, StationKind::Item(Kind::Waypoint), 25.0, 200.0),
            station(2, StationKind::Item(Kind::Rtl), 0.0, 400.0),
        ];
        assert_eq!(
            profile_shape(&ret, Some(Aircraft::Multirotor), None).y,
            [0.0, 25.0, 25.0, 0.0]
        );
    }

    #[test]
    fn a_return_is_flown_at_the_vehicles_own_heights() {
        let ret = vec![
            station(0, StationKind::Home, 0.0, 0.0),
            station(1, StationKind::Item(Kind::Waypoint), 25.0, 200.0),
            station(2, StationKind::Item(Kind::Rtl), 0.0, 400.0),
        ];
        let profile = ReturnProfile {
            known: true,
            hold: false,
            climb_to: Some(60.0),
            arrive_at: Some(10.0),
        };
        let y = profile_shape(&ret, Some(Aircraft::Multirotor), Some(&profile)).y;
        assert_eq!(y, [0.0, 25.0, 60.0, 60.0, 10.0, 0.0]);
        let hold = ReturnProfile { hold: true, ..profile };
        let y = profile_shape(&ret, Some(Aircraft::Multirotor), Some(&hold)).y;
        assert_eq!(y.last(), Some(&10.0), "it waits above the start");
        let y = profile_shape(&ret, Some(Aircraft::FixedWing), Some(&profile)).y;
        assert_eq!(y, [0.0, 25.0, 10.0]);
    }

    #[test]
    fn the_glide_into_a_landing_is_measured_from_the_point_before() {
        let points = vec![
            station(0, StationKind::Home, 0.0, 0.0),
            station(1, StationKind::Item(Kind::Waypoint), 40.0, 100.0),
            station(2, StationKind::Item(Kind::Land), 0.0, 300.0),
        ];
        let g = landing_approach(&points).expect("a glide");
        assert_eq!((g.drop, g.run), (40.0, 200.0));
        assert!((g.degrees - 11.31).abs() < 0.01);
        let under = vec![
            station(1, StationKind::Item(Kind::Waypoint), 40.0, 0.0),
            station(2, StationKind::Item(Kind::Land), 0.0, 0.0),
        ];
        assert_eq!(landing_approach(&under).map(|g| g.degrees), Some(90.0));
        assert!(landing_approach(&points[..2]).is_none());
    }

    #[test]
    fn a_fixed_wing_climbs_out_along_the_first_leg() {
        let points = vec![
            station(0, StationKind::Home, 0.0, 0.0),
            station(1, StationKind::Item(Kind::Takeoff), 30.0, 0.0),
            station(2, StationKind::Item(Kind::Waypoint), 30.0, 100.0),
        ];
        let c = takeoff_climb(&points[0], &points[1], points.get(2)).expect("a climb");
        assert!((c.run - 30.0 / 10f64.to_radians().tan()).abs() < 1e-9);
        assert_eq!((c.room, c.pitch), (100.0, 10.0));
        let end = climb_out_end(&points[0], &points[1], points.get(2));
        assert_eq!(end, 100.0, "it stops at the next point");
        assert!(takeoff_climb(
            &points[0],
            &station(1, StationKind::Item(Kind::Takeoff), 0.0, 0.0),
            None
        )
        .is_none());
    }

    fn square() -> Vec<[f64; 2]> {
        vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]
    }

    #[test]
    fn an_area_is_covered_in_lines_that_alternate() {
        let path = sweep_polygon(&square(), 25.0);
        assert_eq!(path.len(), 8, "four passes of two ends");
        let near = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6;
        assert!(near(path[0], [0.0, 12.5]) && near(path[1], [100.0, 12.5]));
        assert!(near(path[2], [100.0, 37.5]) && near(path[3], [0.0, 37.5]));
        assert!(sweep_polygon(&square()[..2], 25.0).is_empty());
        assert!(sweep_polygon(&square(), 0.0).is_empty());
    }

    #[test]
    fn a_corridor_is_swept_in_lanes_across_its_width() {
        let path = sweep_corridor(&[[0.0, 0.0], [200.0, 0.0]], 100.0, 25.0);
        assert_eq!(path.len(), 8);
        let ys: Vec<f64> = path.iter().map(|p| p[1]).collect();
        assert_eq!(ys, [-37.5, -37.5, -12.5, -12.5, 12.5, 12.5, 37.5, 37.5]);
        assert_eq!(path[0][0], 0.0);
        assert_eq!(path[2][0], 200.0, "the second lane comes back");
        assert!(sweep_corridor(&[[0.0, 0.0]], 100.0, 25.0).is_empty());
    }

    #[test]
    fn pattern_waypoints_come_back_as_degrees_and_a_circle_has_none() {
        let corners = [[11.0, 48.0], [11.001, 48.0], [11.001, 48.001], [11.0, 48.001]];
        let w = pattern_waypoints(PatternShape::Area, &corners, 30.0, 100.0);
        assert!(w.len() >= 4);
        assert!(w
            .iter()
            .all(|p| (10.999..11.002).contains(&p[0]) && (47.999..48.002).contains(&p[1])));
        assert!(pattern_waypoints(PatternShape::Circle, &corners, 30.0, 100.0).is_empty());
        assert!(pattern_waypoints(PatternShape::Area, &[], 30.0, 100.0).is_empty());
    }

    #[test]
    fn clearance_is_the_height_over_the_ground_except_where_the_ground_is_meant() {
        let g = Ground {
            distances: vec![0.0, 1000.0, 2000.0],
            elevations: vec![0.0, 100.0, 50.0],
        };
        assert_eq!(interpolate_at(&g, 500.0), Some(50.0));
        assert_eq!(interpolate_at(&g, 1500.0), Some(75.0));
        assert_eq!(interpolate_at(&g, -100.0), Some(0.0));
        assert_eq!(interpolate_at(&g, 99999.0), Some(50.0));
        let pts = vec![
            station(1, StationKind::Item(Kind::Waypoint), 80.0, 500.0),
            station(2, StationKind::Item(Kind::Land), 0.0, 1000.0),
            station(0, StationKind::Home, 0.0, 0.0),
        ];
        assert_eq!(clearances(&pts, Some(&g)), [Some(30.0), None, None]);
        assert_eq!(clearances(&pts[..1], None), [None]);
    }
}
