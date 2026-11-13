//! A line as geometry the depth test can hide: a flat ribbon, as wide on the
//! screen as asked wherever it is, built from the points of the track in the
//! camera's local metres. The GPU draws it with the ground, so a hill in
//! front of the track hides it, as it hides the real aircraft's path.

use crate::map3d::camera::{Camera, V3};
use crate::map3d::gpu::FLOATS;

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: V3) -> Option<V3> {
    let n = dot(a, a).sqrt();
    (n > 1e-9).then(|| [a[0] / n, a[1] / n, a[2] / n])
}

/// The vertices and triangles of a ribbon along `points` (metres east, north
/// and up of the view centre's ground), `width_px` wide on the screen.
/// Positions are written as the GPU reads them: mercator metres from the
/// view centre, and the height above sea level (`ground` added back).
pub fn build(points: &[V3], width_px: f64, cam: &Camera, ground: f64) -> Option<(Vec<f32>, Vec<u32>)> {
    if points.len() < 2 {
        return None;
    }
    let view = cam.view();
    let k = cam.lat.to_radians().cos();
    let mut verts: Vec<f32> = Vec::with_capacity(points.len() * 2 * FLOATS);
    let mut indices: Vec<u32> = Vec::new();
    let mut last_dir: Option<V3> = None;
    for (i, p) in points.iter().enumerate() {
        let before = if i > 0 { unit(sub(*p, points[i - 1])) } else { None };
        let after = points.get(i + 1).and_then(|n| unit(sub(*n, *p)));
        let dir = match (before, after) {
            (Some(a), Some(b)) => unit([a[0] + b[0], a[1] + b[1], a[2] + b[2]]).unwrap_or(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => last_dir.unwrap_or([1.0, 0.0, 0.0]),
        };
        last_dir = Some(dir);
        // Sideways on the screen: across the line and the view direction.
        let depth = dot(sub(*p, view.pos), view.forward).max(1.0);
        let half = width_px / 2.0 * depth / cam.focal();
        let side = unit(cross(dir, view.forward)).unwrap_or(view.right);
        for sign in [-1.0, 1.0] {
            let q = [
                p[0] + side[0] * half * sign,
                p[1] + side[1] * half * sign,
                p[2] + side[2] * half * sign,
            ];
            verts.extend_from_slice(&[
                (q[0] / k) as f32,
                (q[1] / k) as f32,
                (q[2] + ground) as f32,
                0.0,
                0.0,
                1.0,
            ]);
        }
        if i > 0 {
            let b = (i as u32 - 1) * 2;
            indices.extend_from_slice(&[b, b + 1, b + 2, b + 1, b + 3, b + 2]);
        }
    }
    Some((verts, indices))
}

/// A line laid on the ground as a volume the renderer colours the ground
/// inside of (`gpu::Item::volume`): a band along `points` (metres east, north
/// and up of the view centre's ground), `width_px` wide on the screen, walled
/// from `bottom` to `top` (heights above sea level). Only where the points
/// are on the map counts, not their height, so a line over trees or roofs
/// neither twists nor sinks into them. A line that ends where it began is
/// joined there. Appends to `verts` and `indices`.
pub fn band(
    points: &[V3],
    width_px: f64,
    cam: &Camera,
    (bottom, top): (f64, f64),
    verts: &mut Vec<f32>,
    indices: &mut Vec<u32>,
) {
    let mut pts: Vec<V3> = Vec::with_capacity(points.len());
    for p in points {
        if pts.last().is_none_or(|q| (p[0] - q[0]).hypot(p[1] - q[1]) > 1e-3) {
            pts.push(*p);
        }
    }
    let n = pts.len();
    if n < 2 {
        return;
    }
    let (first, last) = (points[0], points[points.len() - 1]);
    let closed = n > 3 && (first[0] - last[0]).hypot(first[1] - last[1]) <= 1e-3;
    if closed {
        // The last point was kept apart from the one before it, not from the first.
        pts[n - 1] = pts[0];
    }
    let view = cam.view();
    let (focal, near, k) = (cam.focal(), cam.near(), cam.lat.to_radians().cos());
    let along = |a: V3, b: V3| {
        let (x, y) = (b[0] - a[0], b[1] - a[1]);
        let l = x.hypot(y);
        (l > 1e-9).then(|| (x / l, y / l))
    };
    let base = (verts.len() / FLOATS) as u32;
    // A ring's last point is its first: one cross section serves both.
    let sections = if closed { n - 1 } else { n };
    for i in 0..sections {
        let before = if i > 0 {
            along(pts[i - 1], pts[i])
        } else if closed {
            along(pts[n - 2], pts[0])
        } else {
            None
        };
        let after = if i + 1 < n {
            along(pts[i], pts[i + 1])
        } else if closed {
            along(pts[0], pts[1])
        } else {
            None
        };
        let (t, mitre) = match (before, after) {
            (Some(a), Some(b)) => {
                let (x, y) = (a.0 + b.0, a.1 + b.1);
                let l = x.hypot(y);
                if l > 1e-6 {
                    let t = (x / l, y / l);
                    // A corner is as wide across as the sides that meet in
                    // it, up to twice: past that the corner is cut short.
                    (t, 1.0 / (t.0 * b.0 + t.1 * b.1).max(0.5))
                } else {
                    (b, 1.0)
                }
            }
            (Some(a), None) => (a, 1.0),
            (None, Some(b)) => (b, 1.0),
            (None, None) => ((1.0, 0.0), 1.0),
        };
        let left = (-t.1, t.0);
        let p = pts[i];
        let ray = sub(p, view.pos);
        let depth = dot(ray, view.forward).max(near);
        // The band lies on the ground, so it is seen at a slant: wider on
        // the ground where the view runs across it, as wide on the screen.
        let slant = unit(ray).map_or(1.0, |r| {
            let c = left.0 * r[0] + left.1 * r[1];
            (1.0 - c * c).max(0.0).sqrt().max(0.35)
        });
        let half = width_px / 2.0 * depth / focal / slant * mitre;
        for side in [-1.0, 1.0] {
            let (x, y) = (p[0] + left.0 * half * side, p[1] + left.1 * half * side);
            for h in [bottom, top] {
                verts.extend_from_slice(&[(x / k) as f32, (y / k) as f32, h as f32, 0.0, 0.0, 1.0]);
            }
        }
    }
    // Four vertices a point: right bottom, right top, left bottom, left top.
    // Every face is counter clockwise seen from outside.
    let quad = |indices: &mut Vec<u32>, q: [u32; 4]| {
        indices.extend_from_slice(&[
            base + q[0],
            base + q[1],
            base + q[2],
            base + q[0],
            base + q[2],
            base + q[3],
        ]);
    };
    for i in 0..n as u32 - 1 {
        let (a, b) = (i * 4, (i + 1) % sections as u32 * 4);
        quad(indices, [a + 1, b + 1, b + 3, a + 3]);
        quad(indices, [a, a + 2, b + 2, b]);
        quad(indices, [a, b, b + 1, a + 1]);
        quad(indices, [b + 2, a + 2, a + 3, b + 3]);
    }
    if !closed {
        let e = (n as u32 - 1) * 4;
        quad(indices, [2, 0, 1, 3]);
        quad(indices, [e, e + 2, e + 3, e + 1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        Camera {
            lng: 11.6,
            lat: 48.0,
            zoom: 16.0,
            pitch: 60.0,
            bearing: 0.0,
            size: (1000.0, 700.0),
            ground: 500.0,
        }
    }

    #[test]
    fn a_line_needs_two_points() {
        assert!(build(&[], 3.0, &cam(), 500.0).is_none());
        assert!(build(&[[0.0, 0.0, 0.0]], 3.0, &cam(), 500.0).is_none());
    }

    #[test]
    fn a_ribbon_is_two_vertices_a_point_and_two_triangles_a_segment() {
        let pts = [[0.0, 0.0, 10.0], [20.0, 0.0, 10.0], [40.0, 10.0, 12.0]];
        let (v, i) = build(&pts, 3.0, &cam(), 500.0).expect("a ribbon");
        assert_eq!(v.len(), 3 * 2 * FLOATS);
        assert_eq!(i.len(), 2 * 6);
        assert!(i.iter().all(|&n| (n as usize) < v.len() / FLOATS));
    }

    #[test]
    fn the_ribbon_is_as_wide_on_the_screen_as_asked() {
        let c = cam();
        let pts = [[0.0, 0.0, 0.0], [30.0, 0.0, 0.0]];
        let (v, _) = build(&pts, 4.0, &c, 500.0).expect("a ribbon");
        let k = c.lat.to_radians().cos();
        let local = |n: usize| -> V3 {
            [
                f64::from(v[n * FLOATS]) * k,
                f64::from(v[n * FLOATS + 1]) * k,
                f64::from(v[n * FLOATS + 2]) - 500.0,
            ]
        };
        let (a, b) = (c.project(local(0)).expect("seen"), c.project(local(1)).expect("seen"));
        let px = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
        assert!((px - 4.0).abs() < 0.3, "{px}");
    }

    #[test]
    fn the_height_is_above_sea_level() {
        let (v, _) = build(&[[0.0, 0.0, 25.0], [5.0, 0.0, 25.0]], 2.0, &cam(), 500.0).expect("a ribbon");
        let mid = (f64::from(v[2]) + f64::from(v[FLOATS + 2])) / 2.0;
        assert!((mid - 525.0).abs() < 0.01, "{mid}");
    }

    /// Six times the volume the triangles of a mesh enclose, by the
    /// divergence theorem: positive when every face looks outwards.
    fn volume6(v: &[f32], i: &[u32]) -> f64 {
        let at = |n: u32| -> V3 {
            let n = n as usize * FLOATS;
            [f64::from(v[n]), f64::from(v[n + 1]), f64::from(v[n + 2])]
        };
        i.chunks(3).map(|t| dot(at(t[0]), cross(at(t[1]), at(t[2])))).sum()
    }

    /// Every edge of a closed surface is walked once each way.
    fn is_closed(i: &[u32]) -> bool {
        let mut edges = std::collections::HashMap::new();
        for t in i.chunks(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a.min(b), a.max(b))).or_insert(0i32) += if a < b { 1 } else { -1 };
            }
        }
        edges.values().all(|n| *n == 0)
    }

    #[test]
    fn a_band_is_a_closed_volume_that_looks_outwards() {
        let c = cam();
        let open = [
            [0.0, 0.0, 3.0],
            [40.0, 0.0, 30.0],
            [80.0, 30.0, -4.0],
            [80.0, 90.0, 12.0],
        ];
        let ring = [
            [0.0, 0.0, 0.0],
            [60.0, 0.0, 9.0],
            [60.0, 60.0, 0.0],
            [0.0, 60.0, 5.0],
            [0.0, 0.0, 0.0],
        ];
        for pts in [&open[..], &ring[..]] {
            let (mut v, mut i) = (Vec::new(), Vec::new());
            band(pts, 4.0, &c, (400.0, 600.0), &mut v, &mut i);
            let ring = pts.first() == pts.last();
            assert_eq!(v.len(), (pts.len() - usize::from(ring)) * 4 * FLOATS);
            assert!(i.iter().all(|&n| (n as usize) < v.len() / FLOATS));
            assert!(is_closed(&i), "no open edge");
            assert!(volume6(&v, &i) > 0.0, "faces look outwards");
        }
    }

    #[test]
    fn a_band_does_not_follow_the_height_of_its_points() {
        // Over a surface model the ground under a line jumps by a tree's
        // height from one point to the next; the ribbon twisted with it.
        let c = cam();
        let flat: Vec<V3> = (0..20).map(|i| [f64::from(i) * 30.0, 0.0, 0.0]).collect();
        let rough: Vec<V3> = (0..20)
            .map(|i| [f64::from(i) * 30.0, 0.0, f64::from(i % 2) * 0.5])
            .collect();
        let build = |pts: &[V3]| {
            let (mut v, mut i) = (Vec::new(), Vec::new());
            band(pts, 3.0, &c, (480.0, 540.0), &mut v, &mut i);
            v
        };
        let (a, b) = (build(&flat), build(&rough));
        let k = c.lat.to_radians().cos();
        for n in 0..a.len() / FLOATS {
            let d = (f64::from(a[n * FLOATS] - b[n * FLOATS]) * k)
                .hypot(f64::from(a[n * FLOATS + 1] - b[n * FLOATS + 1]) * k);
            assert!(d < 0.01, "vertex {n} moved {d} m sideways");
            assert_eq!(a[n * FLOATS + 2], b[n * FLOATS + 2]);
        }
    }

    #[test]
    fn a_band_is_as_wide_on_the_screen_as_asked() {
        let c = cam();
        for pts in [
            [[0.0, 0.0, 0.0], [30.0, 0.0, 0.0]],
            [[0.0, 0.0, 0.0], [0.0, 30.0, 0.0]],
            [[0.0, 0.0, 0.0], [20.0, 20.0, 0.0]],
        ] {
            let (mut v, mut i) = (Vec::new(), Vec::new());
            band(&pts, 4.0, &c, (500.0, 500.0), &mut v, &mut i);
            let k = c.lat.to_radians().cos();
            let local = |n: usize| -> V3 { [f64::from(v[n * FLOATS]) * k, f64::from(v[n * FLOATS + 1]) * k, 0.0] };
            let (a, b) = (c.project(local(0)).expect("seen"), c.project(local(2)).expect("seen"));
            let across = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
            assert!((3.0..=5.5).contains(&across), "{pts:?}: {across} px");
        }
    }
}
