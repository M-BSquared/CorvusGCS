//! The flown track, recorded as the web map records it (python/src/js/map.js).
//!
//! The track keeps the SHAPE of the flight, not a point every so many
//! metres: the last point follows the aircraft, and becomes a fixed vertex
//! only when the straight line from the vertex before it would miss one of
//! the samples in between by more than a tolerance (a quarter of the fix's
//! own horizontal accuracy, 0.1 to 0.5 m). A hover adds nothing, a straight
//! leg is two points, a turn as many as its curve needs. It is drawn with
//! the curved stretches filled in by a Hermite curve, the way it is drawn
//! there, and earlier flights stay as history until a new arm closes one.

/// One track point: longitude, latitude and the height above home, which
/// rides along for a future 3D view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub lng: f64,
    pub lat: f64,
    pub alt: Option<f64>,
}

const TRACK_JITTER_M: f64 = 0.3;
const TRACK_JITTER_V_M: f64 = 1.0;
const TRACK_TOLERANCE_V_M: f64 = 1.0;
const TRACK_TOLERANCE_MIN_M: f64 = 0.1;
const TRACK_TOLERANCE_MAX_M: f64 = 0.5;
const TRACK_TOLERANCE_M: f64 = 0.25;
const TRACK_WINDOW_MAX: usize = 400;
const TRACK_MAX_POINTS: usize = 20000;
const TRACK_SMOOTH_STEP_M: f64 = 1.5;
const TRACK_SMOOTH_MAX_SEG_M: f64 = 20.0;
const TRACK_SMOOTH_MAX_TURN: f64 = std::f64::consts::PI / 3.0;
const R: f64 = 6371000.0;

/// Metres between two points, equirectangular (exact enough at telemetry
/// spacing and far cheaper than haversine).
pub fn metres_between(a: &Point, b: &Point) -> f64 {
    let lat1 = a.lat.to_radians();
    let lat2 = b.lat.to_radians();
    let d_lat = lat2 - lat1;
    let d_lng = (b.lng - a.lng).to_radians();
    let x = d_lng * ((lat1 + lat2) / 2.0).cos();
    (x * x + d_lat * d_lat).sqrt() * R
}

/// Metres from `p` to the segment `a`-`b`, clamped to the segment, height
/// weighed in by `z_scale` where all three have one.
fn metres_from_segment(p: &Point, a: &Point, b: &Point, z_scale: f64) -> f64 {
    let k = a.lat.to_radians().cos();
    let zs = match (z_scale > 0.0, a.alt, b.alt, p.alt) {
        (true, Some(_), Some(_), Some(_)) => z_scale,
        _ => 0.0,
    };
    let xyz = |c: &Point| {
        (
            (c.lng - a.lng).to_radians() * k * R,
            (c.lat - a.lat).to_radians() * R,
            if zs > 0.0 {
                (c.alt.unwrap_or(0.0) - a.alt.unwrap_or(0.0)) * zs
            } else {
                0.0
            },
        )
    };
    let (bx, by, bz) = xyz(b);
    let (px, py, pz) = xyz(p);
    let len2 = bx * bx + by * by + bz * bz;
    let t = if len2 > 0.0 {
        ((px * bx + py * by + pz * bz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (dx, dy, dz) = (px - t * bx, py - t * by, pz - t * bz);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn metres_moved(a: &Point, b: &Point, z_scale: f64) -> f64 {
    let flat = metres_between(a, b);
    match (z_scale > 0.0, a.alt, b.alt) {
        (true, Some(za), Some(zb)) => flat.hypot((zb - za) * z_scale),
        _ => flat,
    }
}

/// The tolerance for a fix whose horizontal accuracy is `h_acc` metres.
pub fn tolerance(h_acc: Option<f64>) -> f64 {
    match h_acc {
        Some(acc) if acc > 0.0 && acc.is_finite() => (acc / 4.0).clamp(TRACK_TOLERANCE_MIN_M, TRACK_TOLERANCE_MAX_M),
        _ => TRACK_TOLERANCE_M,
    }
}

/// Every flight: earlier ones, then the one in progress.
#[derive(Debug, Default)]
pub struct Track {
    pub past: Vec<Vec<Point>>,
    pub current: Vec<Point>,
    window: Vec<Point>,
    last_armed: Option<bool>,
    /// Bumped on every change, so a painter can tell when to redraw.
    pub revision: u64,
}

impl Track {
    /// Extends the track with a sample; true when it changed.
    pub fn record(&mut self, lng: f64, lat: f64, h_acc: Option<f64>, altitude: Option<f64>) -> bool {
        if !lng.is_finite() || !lat.is_finite() {
            return false;
        }
        // [0, 0] is the state store's "no fix yet", not the Gulf of Guinea.
        if lng == 0.0 && lat == 0.0 {
            return false;
        }
        let alt = altitude
            .filter(|a| a.is_finite())
            .map(|a| crate::data::units::js_round(a * 10.0) / 10.0);
        let p = Point { lng, lat, alt };
        if let Some(tip) = self.current.last() {
            if metres_moved(tip, &p, TRACK_JITTER_M / TRACK_JITTER_V_M) < TRACK_JITTER_M {
                return false;
            }
        }
        if self.window.is_empty() {
            self.current.push(p);
        } else {
            let anchor = self.current[self.current.len() - 2];
            let tol = tolerance(h_acc);
            let z_scale = tol / TRACK_TOLERANCE_V_M;
            let strays = self.window.len() >= TRACK_WINDOW_MAX
                || self
                    .window
                    .iter()
                    .any(|s| metres_from_segment(s, &anchor, &p, z_scale) > tol);
            if strays {
                self.window.clear();
                self.current.push(p);
                self.trim();
            } else {
                let last = self.current.len() - 1;
                self.current[last] = p;
            }
        }
        if self.current.len() > 1 {
            self.window.push(p);
        }
        self.revision += 1;
        true
    }

    fn point_count(&self) -> usize {
        self.past.iter().map(Vec::len).sum::<usize>() + self.current.len()
    }

    /// Over the cap the OLDEST point goes, the first flight's before the
    /// current one's.
    fn trim(&mut self) {
        if self.point_count() <= TRACK_MAX_POINTS {
            return;
        }
        if let Some(first) = self.past.first_mut() {
            first.remove(0);
            if first.len() < 2 {
                self.past.remove(0);
            }
        } else {
            self.current.remove(0);
        }
    }

    /// Discards every flight, past and present.
    pub fn clear(&mut self) {
        self.past.clear();
        self.current.clear();
        self.window.clear();
        self.revision += 1;
    }

    /// Puts another vehicle's flights in place of these and returns these.
    /// Its first sample is the baseline again, so the other vehicle's armed
    /// state is not read as a takeoff.
    pub fn swap(&mut self, current: Vec<Point>, past: Vec<Vec<Point>>) -> (Vec<Point>, Vec<Vec<Point>>) {
        self.window.clear();
        self.last_armed = None;
        self.revision += 1;
        (
            std::mem::replace(&mut self.current, current),
            std::mem::replace(&mut self.past, past),
        )
    }

    /// Whether there is a track to clear: a flight under way needs two
    /// points to be a line (`updateTrackControl`).
    pub fn has_line(&self) -> bool {
        self.current.len() >= 2 || !self.past.is_empty()
    }

    /// Closes the flight in progress; a single point is not a flight.
    pub fn start_new_flight(&mut self) -> bool {
        let closed = self.current.len() >= 2;
        let flight = std::mem::take(&mut self.current);
        self.window.clear();
        if closed {
            self.past.push(flight);
            self.revision += 1;
        }
        closed
    }

    /// True on the disarmed to armed edge only: the first sample is a
    /// baseline, so an app opened mid flight does not split the track.
    pub fn check_for_takeoff(&mut self, armed: Option<bool>) -> bool {
        let Some(armed) = armed else { return false };
        let previous = self.last_armed.replace(armed);
        previous == Some(false) && armed
    }
}

/// The track as drawn: `coords` with the curved stretches filled in.
pub fn smooth(coords: &[Point]) -> Vec<Point> {
    if coords.len() < 3 {
        return coords.to_vec();
    }
    let mut out = vec![coords[0]];
    for i in 0..coords.len() - 1 {
        let prev = if i > 0 { Some(&coords[i - 1]) } else { None };
        let next = coords.get(i + 2);
        out.extend(smooth_segment(prev, &coords[i], &coords[i + 1], next));
        out.push(coords[i + 1]);
    }
    out
}

/// The points strictly between `a` and `b` on a cubic Hermite curve whose
/// tangent at each end bisects the turn there; empty for a straight stretch,
/// a long leg, or where both ends are corners.
fn smooth_segment(prev: Option<&Point>, a: &Point, b: &Point, next: Option<&Point>) -> Vec<Point> {
    let k = a.lat.to_radians().cos();
    let xy = |c: &Point| ((c.lng - a.lng) * k, c.lat - a.lat);
    let bb = xy(b);
    let len = bb.0.hypot(bb.1);
    let len_m = metres_between(a, b);
    if len.is_nan() || len <= 0.0 || len_m > TRACK_SMOOTH_MAX_SEG_M || len_m < 2.0 * TRACK_SMOOTH_STEP_M {
        return Vec::new();
    }
    let chord = (bb.0 / len, bb.1 / len);
    let tangent = |u: (f64, f64), v: (f64, f64)| {
        let lu = u.0.hypot(u.1);
        let lv = v.0.hypot(v.1);
        if lu.is_nan() || lu <= 0.0 || lv.is_nan() || lv <= 0.0 {
            return chord;
        }
        let cos = (u.0 * v.0 + u.1 * v.1) / (lu * lv);
        if cos.clamp(-1.0, 1.0).acos() > TRACK_SMOOTH_MAX_TURN {
            return chord;
        }
        let d = (u.0 / lu + v.0 / lv, u.1 / lu + v.1 / lv);
        let ld = d.0.hypot(d.1);
        if ld > 0.0 {
            (d.0 / ld, d.1 / ld)
        } else {
            chord
        }
    };
    let m0 = match prev {
        Some(p) => {
            let f = xy(p);
            tangent((-f.0, -f.1), bb)
        }
        None => chord,
    };
    let m1 = match next {
        Some(n) => {
            let t = xy(n);
            tangent(bb, (t.0 - bb.0, t.1 - bb.1))
        }
        None => chord,
    };
    let straight = |m: (f64, f64)| (m.0 * chord.1 - m.1 * chord.0).abs() < 1e-6;
    if straight(m0) && straight(m1) {
        return Vec::new();
    }
    let steps = (len_m / TRACK_SMOOTH_STEP_M).ceil() as usize;
    (1..steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let (t2, t3) = (t * t, t * t * t);
            let h10 = (t3 - 2.0 * t2 + t) * len;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = (t3 - t2) * len;
            let x = h10 * m0.0 + h01 * bb.0 + h11 * m1.0;
            let y = h10 * m0.1 + h01 * bb.1 + h11 * m1.1;
            let alt = match (a.alt, b.alt) {
                (Some(za), Some(zb)) => Some(za + (zb - za) * t),
                _ => None,
            };
            Point {
                lng: a.lng + x / k,
                lat: a.lat + y,
                alt,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `metres` north of a fixed point, as a longitude/latitude pair.
    fn north(metres: f64) -> (f64, f64) {
        (11.64, 48.08 + (metres / R).to_degrees())
    }

    fn east(metres: f64, of_north: f64) -> (f64, f64) {
        let (lng, lat) = north(of_north);
        (lng + (metres / (R * lat.to_radians().cos())).to_degrees(), lat)
    }

    #[test]
    fn a_straight_leg_is_two_points_and_a_hover_adds_nothing() {
        let mut t = Track::default();
        for i in 0..100 {
            let (lng, lat) = north(i as f64);
            t.record(lng, lat, Some(0.5), Some(10.0));
        }
        assert_eq!(t.current.len(), 2);
        let tip = t.current[1];
        assert!(
            !t.record(tip.lng, tip.lat + 1e-7, Some(0.5), Some(10.0)),
            "a hover moved the track"
        );
    }

    #[test]
    fn a_corner_becomes_a_vertex() {
        let mut t = Track::default();
        for i in 0..=50 {
            let (lng, lat) = north(i as f64);
            t.record(lng, lat, None, None);
        }
        for i in 1..=50 {
            let (lng, lat) = east(i as f64, 50.0);
            t.record(lng, lat, None, None);
        }
        assert_eq!(t.current.len(), 3, "{:?}", t.current);
        assert!(
            metres_between(
                &t.current[1],
                &Point {
                    lng: north(50.0).0,
                    lat: north(50.0).1,
                    alt: None
                }
            ) < 1.5
        );
    }

    #[test]
    fn no_fix_is_not_a_point_and_tolerance_follows_accuracy() {
        let mut t = Track::default();
        assert!(!t.record(0.0, 0.0, None, None));
        assert!(!t.record(f64::NAN, 1.0, None, None));
        assert_eq!(tolerance(None), 0.25);
        assert_eq!(tolerance(Some(0.02)), 0.1);
        assert_eq!(tolerance(Some(1.2)), 0.3);
        assert_eq!(tolerance(Some(9.0)), 0.5);
    }

    #[test]
    fn an_arm_closes_the_flight_but_not_on_the_first_sample() {
        let mut t = Track::default();
        assert!(!t.check_for_takeoff(Some(true)), "opened mid flight");
        assert!(!t.check_for_takeoff(Some(false)));
        assert!(t.check_for_takeoff(Some(true)));
        assert!(!t.check_for_takeoff(None));
        let (a, b) = (north(0.0), north(10.0));
        t.record(a.0, a.1, None, None);
        t.record(b.0, b.1, None, None);
        assert!(t.start_new_flight());
        assert_eq!((t.past.len(), t.current.len()), (1, 0));
        assert!(!t.start_new_flight(), "an empty flight is not one");
    }

    #[test]
    fn a_curve_is_drawn_round_and_a_long_leg_straight() {
        // An orbit of 30 m, a point every 30 degrees: 15.5 m legs, 30 degree
        // turns, so every stretch is a curve. A 90 degree turn would be a
        // corner and stay one.
        let pts: Vec<Point> = (0..6)
            .map(|i| {
                let a = (i as f64 * 30.0).to_radians();
                let (lng, lat) = east(30.0 * a.sin(), 30.0 * a.cos());
                Point { lng, lat, alt: None }
            })
            .collect();
        let drawn = smooth(&pts);
        assert!(drawn.len() > pts.len());
        let legs: Vec<Point> = [north(0.0), north(100.0), east(100.0, 100.0)]
            .iter()
            .map(|&(lng, lat)| Point { lng, lat, alt: None })
            .collect();
        assert_eq!(smooth(&legs), legs);
    }

    #[test]
    fn clearing_drops_every_flight() {
        let mut t = Track::default();
        t.record(11.0, 48.0, None, None);
        assert!(!t.has_line(), "one point is not a line");
        t.record(11.001, 48.0, None, None);
        t.start_new_flight();
        assert!(t.has_line());
        let before = t.revision;
        t.clear();
        assert!(!t.has_line() && t.past.is_empty() && t.current.is_empty());
        assert!(t.revision > before);
    }
}
