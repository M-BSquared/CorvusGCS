//! A flown track laid on the ground once, for the 3D view.
//!
//! Reading the ground under a point is the costly part of drawing a track, and
//! a long flight has tens of thousands of points. A [`Trail`] keeps the
//! smoothed points with the ground height under each: a track that grew keeps
//! the heights of its unchanged start, and elevation that arrives later is
//! read again a slice per frame while the heights already known are drawn.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::data::track::{self, Point};

/// Points laid in a frame before the time limit is looked at, so a trail
/// always moves on.
const LAY_AT_LEAST: usize = 256;

/// A flight's smoothed points and the ground height under each.
#[derive(Default)]
pub struct Trail {
    /// What the raw track was when it was last smoothed.
    raw: u64,
    pts: Vec<Point>,
    /// The ground under `pts`, as far as it is laid.
    heights: Vec<f64>,
    /// `heights[..fresh]` were read from the elevation of `generation`.
    fresh: usize,
    generation: u64,
    pub used: u64,
}

fn same(a: &Point, b: &Point) -> bool {
    a.lng.to_bits() == b.lng.to_bits()
        && a.lat.to_bits() == b.lat.to_bits()
        && a.alt.map(f64::to_bits) == b.alt.map(f64::to_bits)
}

/// The length and the two ends of a track: a recorder appends, simplifies
/// the end or thins the whole, and each of those moves one of them.
fn signature(raw: &[Point]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    raw.len().hash(&mut h);
    for p in raw.first().into_iter().chain(raw.last()) {
        (p.lng.to_bits(), p.lat.to_bits(), p.alt.map(f64::to_bits)).hash(&mut h);
    }
    h.finish()
}

impl Trail {
    /// Follows the raw track and the elevation of `generation`. The smoothed
    /// points are made again when the track changed, and the heights of the
    /// start they share with the last ones are kept.
    pub fn follow(&mut self, raw: &[Point], generation: u64) {
        let sig = signature(raw);
        if sig != self.raw || self.pts.is_empty() != raw.is_empty() {
            self.raw = sig;
            let pts = track::smooth(raw);
            let common = self.pts.iter().zip(&pts).take_while(|(a, b)| same(a, b)).count();
            self.heights.truncate(common);
            self.fresh = self.fresh.min(common);
            self.pts = pts;
        }
        if generation != self.generation {
            self.generation = generation;
            self.fresh = 0;
        }
    }

    /// Reads the ground under the points that have no height yet, then again
    /// under those read from older elevation, until `until`. `ground` answers
    /// the height above sea level at a longitude and latitude.
    pub fn lay(&mut self, ground: &dyn Fn(f64, f64) -> f64, until: Instant) {
        let mut done = 0;
        let mut more = || {
            done += 1;
            done <= LAY_AT_LEAST || Instant::now() < until
        };
        while self.heights.len() < self.pts.len() && more() {
            let p = &self.pts[self.heights.len()];
            if self.fresh == self.heights.len() {
                self.fresh += 1;
            }
            self.heights.push(ground(p.lng, p.lat));
        }
        if self.fresh >= self.heights.len() {
            self.fresh = self.heights.len();
            return;
        }
        while self.fresh < self.heights.len() && more() {
            let p = &self.pts[self.fresh];
            self.heights[self.fresh] = ground(p.lng, p.lat);
            self.fresh += 1;
        }
    }

    /// Whether every point has a height read from the elevation followed.
    pub fn settled(&self) -> bool {
        self.heights.len() == self.pts.len() && self.fresh == self.heights.len()
    }

    /// The points that have a ground height, with it.
    pub fn laid(&self) -> impl Iterator<Item = (&Point, f64)> {
        self.pts.iter().zip(self.heights.iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    fn flight(n: usize) -> Vec<Point> {
        (0..n)
            .map(|i| Point {
                lng: 11.6 + i as f64 * 0.0005,
                lat: 48.08 + (i % 3) as f64 * 0.0001,
                alt: Some(50.0),
            })
            .collect()
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn the_ground_is_read_once_and_a_longer_track_reads_only_its_new_end() {
        let reads = Cell::new(0);
        let ground = |_: f64, _: f64| {
            reads.set(reads.get() + 1);
            500.0
        };
        let mut t = Trail::default();
        t.follow(&flight(40), 1);
        t.lay(&ground, later());
        let first = reads.get();
        assert_eq!(first, t.laid().count());
        assert!(t.settled());
        t.follow(&flight(40), 1);
        t.lay(&ground, later());
        assert_eq!(reads.get(), first, "an unchanged track is not read again");
        t.follow(&flight(41), 1);
        t.lay(&ground, later());
        assert!(
            reads.get() - first < 10,
            "{} read for one new point",
            reads.get() - first
        );
        assert!(t.laid().all(|(_, h)| h == 500.0));
    }

    #[test]
    fn new_elevation_is_read_again_a_slice_at_a_time_while_the_old_is_drawn() {
        let mut t = Trail::default();
        t.follow(&flight(2000), 1);
        t.lay(&|_, _| 400.0, later());
        let n = t.laid().count();
        t.follow(&flight(2000), 2);
        t.lay(&|_, _| 450.0, Instant::now());
        assert_eq!(t.laid().count(), n, "every point still has a height");
        let new = t.laid().filter(|(_, h)| *h == 450.0).count();
        assert!(
            new >= LAY_AT_LEAST && new < n,
            "{new} of {n} read again in a frame out of time"
        );
        while !t.settled() {
            t.lay(&|_, _| 450.0, Instant::now());
        }
        assert!(t.laid().all(|(_, h)| h == 450.0));
    }

    #[test]
    fn a_frame_out_of_time_still_lays_a_slice_of_a_long_track() {
        let mut t = Trail::default();
        t.follow(&flight(5000), 1);
        t.lay(&|_, _| 0.0, Instant::now());
        let laid = t.laid().count();
        assert!((LAY_AT_LEAST..5000).contains(&laid), "{laid}");
        assert!(!t.settled());
    }

    #[test]
    fn a_new_flight_in_the_same_place_starts_over() {
        let mut t = Trail::default();
        t.follow(&flight(30), 1);
        t.lay(&|_, _| 1.0, later());
        let mut other = flight(30);
        for p in &mut other {
            p.lat += 0.01;
        }
        t.follow(&other, 1);
        assert_eq!(t.laid().count(), 0, "nothing of the old flight is kept");
        t.follow(&[], 1);
        t.lay(&|_, _| 1.0, later());
        assert!(t.settled() && t.laid().count() == 0);
    }
}
