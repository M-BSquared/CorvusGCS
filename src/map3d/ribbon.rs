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
}
