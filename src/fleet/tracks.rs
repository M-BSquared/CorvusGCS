//! The vehicles on the Home map that are not being flown (`map-fleet.js`):
//! a track of their own in their colour, and a marker eased toward each new
//! position.
//!
//! The tracks change hands when the flown vehicle does: the track of the
//! vehicle the map stops drawing as its own goes to `stash`, and `take`
//! gives back the one of the vehicle it starts with, so a track follows the
//! vehicle and not the role it plays.

use std::collections::{HashMap, HashSet};

use crate::data::track::{metres_between, Point};

const MIN_MOVE_M: f64 = 2.0;
/// Per vehicle, all flights.
const MAX_POINTS: usize = 5000;
/// Tracks remembered.
const MAX_VEHICLES: usize = 16;
const POS_TAU: f32 = 0.35;
const HDG_TAU: f32 = 0.25;

/// One vehicle's flights.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Flights {
    pub current: Vec<Point>,
    pub past: Vec<Vec<Point>>,
    armed: Option<bool>,
}

impl Flights {
    fn points(&self) -> usize {
        self.past.iter().map(Vec::len).sum::<usize>() + self.current.len()
    }
}

/// The tracks of every vehicle, by id.
#[derive(Debug, Default)]
pub struct Tracks {
    flights: HashMap<String, Flights>,
    order: Vec<String>,
    removed: HashSet<String>,
    /// Changed since it was last written.
    pub dirty: bool,
}

impl Tracks {
    fn entry(&mut self, id: &str) -> &mut Flights {
        if !self.flights.contains_key(id) {
            if self.flights.len() >= MAX_VEHICLES {
                let oldest = self.order.remove(0);
                self.flights.remove(&oldest);
            }
            self.order.push(id.to_string());
            self.flights.insert(id.to_string(), Flights::default());
        }
        self.flights.get_mut(id).expect("just inserted")
    }

    /// Extends `id`'s track with one sample; true when it changed. A new
    /// arm closes the flight in progress into the past.
    pub fn record(&mut self, id: &str, position: [f64; 2], armed: Option<bool>) -> bool {
        let changed = self.record_inner(id, position, armed);
        self.dirty |= changed;
        changed
    }

    fn record_inner(&mut self, id: &str, position: [f64; 2], armed: Option<bool>) -> bool {
        let t = self.entry(id);
        let mut changed = false;
        if let Some(a) = armed {
            if t.armed == Some(false) && a && t.current.len() >= 2 {
                let done = std::mem::take(&mut t.current);
                t.past.push(done);
                changed = true;
            }
            t.armed = Some(a);
        }
        let p = Point {
            lng: position[0],
            lat: position[1],
            alt: None,
        };
        if !p.lng.is_finite() || !p.lat.is_finite() || (p.lng == 0.0 && p.lat == 0.0) {
            return changed;
        }
        if t.current.last().is_some_and(|l| metres_between(l, &p) < MIN_MOVE_M) {
            return changed;
        }
        t.current.push(p);
        if t.points() > MAX_POINTS {
            if t.past.is_empty() {
                t.current.remove(0);
            } else {
                t.past[0].remove(0);
                if t.past[0].len() < 2 {
                    t.past.remove(0);
                }
            }
        }
        true
    }

    /// The map hands over the track of the vehicle it stops drawing.
    pub fn stash(&mut self, id: &str, current: Vec<Point>, past: Vec<Vec<Point>>) {
        if id.is_empty() {
            return;
        }
        self.dirty = true;
        self.flights.remove(id);
        self.order.retain(|o| o != id);
        if self.removed.contains(id) {
            return;
        }
        let past: Vec<Vec<Point>> = past.into_iter().filter(|f| f.len() >= 2).collect();
        if current.len() >= 2 || !past.is_empty() {
            let t = self.entry(id);
            t.current = if current.len() >= 2 { current } else { Vec::new() };
            t.past = past;
        }
    }

    /// The map takes the track of the vehicle it starts drawing.
    pub fn take(&mut self, id: &str) -> Option<(Vec<Point>, Vec<Vec<Point>>)> {
        self.dirty = true;
        self.order.retain(|o| o != id);
        self.flights.remove(id).map(|t| (t.current, t.past))
    }

    /// The operator removed `id`: its track goes, and one handed over for
    /// it afterwards is dropped, until the vehicle is heard again.
    pub fn forget(&mut self, id: &str) {
        self.dirty = true;
        self.removed.insert(id.to_string());
        self.flights.remove(id);
        self.order.retain(|o| o != id);
    }

    /// A vehicle in the list is alive again.
    pub fn heard(&mut self, id: &str) {
        self.removed.remove(id);
    }

    pub fn clear(&mut self) {
        self.dirty = true;
        self.flights.clear();
        self.order.clear();
    }

    /// Every vehicle's flights, for the file.
    pub fn snapshot(&self) -> std::collections::BTreeMap<String, (Vec<Point>, Vec<Vec<Point>>)> {
        self.flights
            .iter()
            .map(|(id, t)| (id.clone(), (t.current.clone(), t.past.clone())))
            .collect()
    }

    /// Whether any vehicle but `owner` has a line.
    pub fn has_lines(&self, owner: &str) -> bool {
        self.flights
            .iter()
            .any(|(id, t)| id != owner && (t.current.len() >= 2 || !t.past.is_empty()))
    }

    /// Each track except `owner`'s, in the order they began.
    pub fn each<'a>(&'a self, owner: &'a str) -> impl Iterator<Item = (&'a str, &'a Flights)> + 'a {
        self.order
            .iter()
            .filter(move |id| id.as_str() != owner)
            .filter_map(|id| self.flights.get(id).map(|t| (id.as_str(), t)))
    }
}

/// A marker eased toward the position it was last told.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub lng: f64,
    pub lat: f64,
    pub heading: f32,
    target: (f64, f64, f32),
}

fn approach(cur: f64, target: f64, dt: f32, tau: f32) -> f64 {
    cur + (target - cur) * f64::from(1.0 - (-dt / tau.max(1e-4)).exp())
}

fn shortest(cur: f32, target: f32) -> f32 {
    ((target - cur) % 360.0 + 540.0) % 360.0 - 180.0
}

impl Mark {
    fn new(lng: f64, lat: f64, heading: f32) -> Mark {
        Mark {
            lng,
            lat,
            heading,
            target: (lng, lat, heading),
        }
    }

    /// One step of `dt` seconds; true while it is still moving.
    fn step(&mut self, dt: f32) -> bool {
        let (tx, ty, th) = self.target;
        let nx = approach(self.lng, tx, dt, POS_TAU);
        let ny = approach(self.lat, ty, dt, POS_TAU);
        let nh = self.heading + shortest(self.heading, th) * (1.0 - (-dt / HDG_TAU).exp());
        let done = (nx - tx).abs() < 1e-7 && (ny - ty).abs() < 1e-7 && shortest(nh, th).abs() < 0.05;
        if done {
            (self.lng, self.lat, self.heading) = (tx, ty, th);
        } else {
            (self.lng, self.lat, self.heading) = (nx, ny, nh);
        }
        !done
    }
}

/// The markers of the vehicles not flown.
#[derive(Debug, Default)]
pub struct Marks {
    marks: HashMap<String, Mark>,
}

impl Marks {
    /// Tells the marker of `id` where the vehicle is now; the first report
    /// puts it there.
    pub fn set(&mut self, id: &str, position: [f64; 2], heading: Option<f64>) {
        let m = self
            .marks
            .entry(id.to_string())
            .or_insert_with(|| Mark::new(position[0], position[1], heading.unwrap_or(0.0) as f32));
        m.target.0 = position[0];
        m.target.1 = position[1];
        if let Some(h) = heading {
            m.target.2 = h as f32;
        }
    }

    /// Drops every marker whose id is not in `keep`.
    pub fn retain(&mut self, keep: &[&str]) {
        self.marks.retain(|id, _| keep.contains(&id.as_str()));
    }

    pub fn forget(&mut self, id: &str) {
        self.marks.remove(id);
    }

    /// Eases every marker; true while any is moving.
    pub fn step(&mut self, dt: f32) -> bool {
        let mut moving = false;
        for m in self.marks.values_mut() {
            moving |= m.step(dt);
        }
        moving
    }

    pub fn get(&self, id: &str) -> Option<&Mark> {
        self.marks.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [f64; 2] = [11.64, 48.07];

    fn east(m: f64) -> [f64; 2] {
        [A[0] + m / (111_320.0 * A[1].to_radians().cos()), A[1]]
    }

    #[test]
    fn a_sample_closer_than_two_metres_is_noise() {
        let mut t = Tracks::default();
        assert!(t.record("a", A, Some(false)));
        assert!(!t.record("a", east(1.0), None));
        assert!(t.record("a", east(5.0), None));
        assert_eq!(t.each("").next().expect("a track").1.current.len(), 2);
    }

    #[test]
    fn no_fix_is_not_a_point() {
        let mut t = Tracks::default();
        assert!(!t.record("a", [0.0, 0.0], None));
        assert!(!t.record("a", [f64::NAN, 1.0], None));
    }

    #[test]
    fn a_new_arm_closes_the_flight_in_progress() {
        let mut t = Tracks::default();
        t.record("a", A, Some(true));
        t.record("a", east(10.0), Some(true));
        t.record("a", east(20.0), Some(false));
        assert!(t.record("a", east(30.0), Some(true)));
        let f = t.each("").next().expect("a track").1;
        assert_eq!(f.past.len(), 1);
        assert_eq!(f.past[0].len(), 3);
        assert_eq!(f.current.len(), 1);
    }

    #[test]
    fn the_track_follows_the_vehicle_when_the_flown_one_changes() {
        let mut t = Tracks::default();
        let line = vec![
            Point {
                lng: 1.0,
                lat: 1.0,
                alt: None,
            },
            Point {
                lng: 1.1,
                lat: 1.1,
                alt: None,
            },
        ];
        t.stash("a", line.clone(), vec![]);
        assert!(t.has_lines("b"));
        assert!(!t.has_lines("a"));
        assert_eq!(t.take("a"), Some((line.clone(), vec![])));
        assert_eq!(t.take("a"), None);
        // One point is not a line.
        t.stash("a", line[..1].to_vec(), vec![]);
        assert!(!t.has_lines("b"));
    }

    #[test]
    fn a_removed_vehicle_leaves_no_track_until_it_is_heard_again() {
        let mut t = Tracks::default();
        let line = vec![
            Point {
                lng: 1.0,
                lat: 1.0,
                alt: None,
            },
            Point {
                lng: 1.1,
                lat: 1.1,
                alt: None,
            },
        ];
        t.stash("a", line.clone(), vec![]);
        t.forget("a");
        assert!(!t.has_lines("b"));
        t.stash("a", line.clone(), vec![]);
        assert!(!t.has_lines("b"));
        t.heard("a");
        t.stash("a", line, vec![]);
        assert!(t.has_lines("b"));
    }

    #[test]
    fn the_oldest_track_goes_after_sixteen_vehicles() {
        let mut t = Tracks::default();
        for i in 0..17 {
            t.record(&format!("v{i}"), A, None);
        }
        assert_eq!(t.each("").count(), 16);
        assert!(t.each("").all(|(id, _)| id != "v0"));
    }

    #[test]
    fn a_marker_eases_to_its_target_and_stops() {
        let mut m = Marks::default();
        m.set("a", A, Some(350.0));
        m.set("a", [A[0] + 0.001, A[1]], Some(10.0));
        assert!(m.step(0.1));
        let p = m.get("a").expect("marker");
        assert!(p.lng > A[0] && p.lng < A[0] + 0.001);
        // The heading turns the short way, through north.
        assert!(p.heading > 350.0 || p.heading < 10.0);
        for _ in 0..200 {
            m.step(0.1);
        }
        let p = m.get("a").expect("marker");
        assert_eq!(p.lng, A[0] + 0.001);
        assert!(!m.step(0.1));
    }

    #[test]
    fn markers_of_vehicles_that_left_are_dropped() {
        let mut m = Marks::default();
        m.set("a", A, None);
        m.set("b", A, None);
        m.retain(&["a"]);
        assert!(m.get("a").is_some() && m.get("b").is_none());
    }
}
