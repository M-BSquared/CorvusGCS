//! What plugins draw on the ground (lines, outlines and areas of `api.map`)
//! as geometry on the terrain, so the depth test hides what a hill is in
//! front of, as it hides the flown track. Text, markers and buttons stay
//! screen elements; `Map3d::hidden` tests them against the ground.

use std::sync::Arc;

use eframe::egui::{Color32, Pos2};

use crate::map3d::camera::{Camera, V3};
use crate::map3d::gpu::{self, FLOATS};
use crate::map3d::ribbon;
use crate::map3d::Map3d;
use crate::plugins::shapes::{Kind, Shape};

/// A side of a shape is cut into pieces about this long (metres), so the line
/// follows the hills between its corners.
const PIECE_M: f64 = 30.0;
/// The most points one line is made of.
const MAX_POINTS: usize = 800;
/// An area is cut into triangles with sides no longer than this (metres)...
const FILL_EDGE_M: f64 = 80.0;
/// ...and into at most this many.
const MAX_TRIANGLES: usize = 6000;
/// How far above the ground the overlay lies, in metres.
const LIFT_M: f64 = 1.0;

/// `pts` with points between them, one every `piece` metres at most, in
/// longitude and latitude. `metres` says how far apart two places are.
pub fn densify(
    pts: &[[f64; 2]],
    closed: bool,
    piece: f64,
    metres: &dyn Fn([f64; 2], [f64; 2]) -> f64,
) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = Vec::new();
    let n = pts.len();
    let sides = if closed { n } else { n.saturating_sub(1) };
    for i in 0..sides {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let cuts = ((metres(a, b) / piece).ceil() as usize).clamp(1, MAX_POINTS);
        for k in 0..cuts {
            let t = k as f64 / cuts as f64;
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    if let Some(last) = if closed { pts.first() } else { pts.last() } {
        out.push(*last);
    }
    if out.len() > MAX_POINTS {
        let step = out.len().div_ceil(MAX_POINTS);
        let last = *out.last().expect("not empty");
        out = out.into_iter().step_by(step).collect();
        out.push(last);
    }
    out
}

/// The runs a dash pattern of `on` then `off` screen pixels leaves of the
/// line through `points`. `px_per_m` says how many pixels a metre is at a
/// point, which shrinks with distance.
pub fn dash_runs(points: &[V3], on: f64, off: f64, px_per_m: &dyn Fn(V3) -> f64) -> Vec<Vec<V3>> {
    let mut runs: Vec<Vec<V3>> = Vec::new();
    let Some(first) = points.first() else { return runs };
    let (mut drawing, mut left) = (true, on);
    let mut current = vec![*first];
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0];
        let px = len * px_per_m(mid);
        if px <= 0.0 {
            continue;
        }
        let mut done = 0.0;
        while px - done > left {
            done += left;
            let t = done / px;
            let at = [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ];
            if drawing {
                current.push(at);
                runs.push(std::mem::take(&mut current));
                left = off;
            } else {
                current = vec![at];
                left = on;
            }
            drawing = !drawing;
        }
        left -= px - done;
        if drawing {
            current.push(b);
        }
    }
    if drawing && current.len() >= 2 {
        runs.push(current);
    }
    runs
}

/// Triangles with sides no longer than `edge` that fill the polygon `ring`
/// (local metres east and north): the ear clipped triangles, each cut in four
/// until short, as far as `limit` triangles allow.
pub fn fill_triangles(ring: &[[f64; 2]], edge: f64, limit: usize) -> Vec<[[f64; 2]; 3]> {
    let pts: Vec<Pos2> = ring.iter().map(|p| Pos2::new(p[0] as f32, p[1] as f32)).collect();
    let mut work: Vec<[[f64; 2]; 3]> = crate::ui::polygon::triangulate(&pts)
        .into_iter()
        .map(|t| [ring[t[0]], ring[t[1]], ring[t[2]]])
        .collect();
    let side = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
    let mut out = Vec::new();
    while let Some(t) = work.pop() {
        let longest = side(t[0], t[1]).max(side(t[1], t[2])).max(side(t[2], t[0]));
        if longest <= edge || out.len() + work.len() + 4 > limit {
            out.push(t);
            continue;
        }
        let mid = |a: [f64; 2], b: [f64; 2]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        let (ab, bc, ca) = (mid(t[0], t[1]), mid(t[1], t[2]), mid(t[2], t[0]));
        work.extend([[t[0], ab, ca], [ab, t[1], bc], [ca, bc, t[2]], [ab, bc, ca]]);
    }
    out
}

fn rgb(c: Color32) -> [f32; 3] {
    [
        f32::from(c.r()) / 255.0,
        f32::from(c.g()) / 255.0,
        f32::from(c.b()) / 255.0,
    ]
}

impl Map3d {
    /// The lines and areas plugins draw, as items of the GPU scene, lowest first.
    pub(super) fn shape_items(&self, shapes: &[Shape]) -> Vec<gpu::Item> {
        let centre = crate::map3d::camera::mercator(self.cam.lng, self.cam.lat);
        let mut items = Vec::new();
        for s in shapes.iter().filter(|s| s.visible) {
            match &s.kind {
                Kind::Line { pts, dashed } => {
                    let line = self.drape(pts, false);
                    self.stroke(&mut items, centre, &line, s, *dashed, s.width);
                }
                Kind::Polygon {
                    pts,
                    fill_opacity,
                    outline,
                    dashed,
                } if pts.len() >= 3 => {
                    if *fill_opacity > 0.0 {
                        if let Some(item) = self.fill(centre, pts, s.color, *fill_opacity) {
                            items.push(item);
                        }
                    }
                    if *outline > 0.0 {
                        let ring = self.drape(pts, true);
                        self.stroke(&mut items, centre, &ring, s, *dashed, *outline);
                    }
                }
                _ => {}
            }
        }
        items
    }

    /// The line through `pts` lying on the ground, in local metres.
    fn drape(&self, pts: &[[f64; 2]], closed: bool) -> Vec<V3> {
        let cam = &self.cam;
        let metres = |a: [f64; 2], b: [f64; 2]| {
            let (pa, pb) = (cam.local_of(a[0], a[1], 0.0), cam.local_of(b[0], b[1], 0.0));
            (pb[0] - pa[0]).hypot(pb[1] - pa[1])
        };
        densify(pts, closed, PIECE_M, &metres)
            .into_iter()
            .map(|p| cam.local_of(p[0], p[1], self.ground_at(p[0], p[1]) + LIFT_M))
            .collect()
    }

    /// A line as the casing the track has and the colour on top of it.
    fn stroke(&self, items: &mut Vec<gpu::Item>, centre: (f64, f64), line: &[V3], s: &Shape, dashed: bool, width: f32) {
        let width = f64::from(width);
        let view = self.cam.view();
        let focal = self.cam.focal();
        let px_per_m = |p: V3| {
            let depth = (p[0] - view.pos[0]) * view.forward[0]
                + (p[1] - view.pos[1]) * view.forward[1]
                + (p[2] - view.pos[2]) * view.forward[2];
            focal / depth.max(1.0)
        };
        let runs = if dashed {
            dash_runs(line, 2.0 * width, 1.5 * width, &px_per_m)
        } else {
            vec![line.to_vec()]
        };
        let bg = rgb(crate::ui::style::bg());
        for (w, color, alpha) in [(width + 2.4, bg, 0.45 * s.opacity), (width, rgb(s.color), s.opacity)] {
            for run in &runs {
                if let Some((verts, indices)) = ribbon::build(run, w, &self.cam, self.cam.ground) {
                    items.push(item(centre, verts, indices, color, alpha));
                }
            }
        }
    }

    /// An area lying on the ground: cut into small triangles so it follows
    /// the hills under it.
    fn fill(&self, centre: (f64, f64), ring: &[[f64; 2]], color: Color32, alpha: f32) -> Option<gpu::Item> {
        let cam: &Camera = &self.cam;
        let local: Vec<[f64; 2]> = ring
            .iter()
            .map(|p| {
                let l = cam.local_of(p[0], p[1], 0.0);
                [l[0], l[1]]
            })
            .collect();
        let k = cam.lat.to_radians().cos();
        let mut verts: Vec<f32> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for tri in fill_triangles(&local, FILL_EDGE_M, MAX_TRIANGLES) {
            for p in tri {
                let (lng, lat) = cam.geo_of(p[0], p[1]);
                let z = self.ground_at(lng, lat) + LIFT_M - cam.ground;
                indices.push((verts.len() / FLOATS) as u32);
                verts.extend_from_slice(&[
                    (p[0] / k) as f32,
                    (p[1] / k) as f32,
                    (z + cam.ground) as f32,
                    0.0,
                    0.0,
                    1.0,
                ]);
            }
        }
        (!indices.is_empty()).then(|| item(centre, verts, indices, rgb(color), alpha))
    }
}

fn item(origin: (f64, f64), verts: Vec<f32>, indices: Vec<u32>, color: [f32; 3], alpha: f32) -> gpu::Item {
    gpu::Item {
        mesh: Arc::new(gpu::MeshData::new(verts, indices)),
        origin,
        texture: None,
        uv: [0.0, 0.0, 1.0, 1.0],
        fallback: None,
        flat: false,
        color,
        alpha,
        transient: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metres(a: [f64; 2], b: [f64; 2]) -> f64 {
        (a[0] - b[0]).hypot(a[1] - b[1])
    }

    #[test]
    fn a_long_side_is_cut_into_pieces_and_the_ends_stay() {
        let out = densify(&[[0.0, 0.0], [100.0, 0.0]], false, 30.0, &metres);
        assert_eq!(out.len(), 5, "four pieces and the last point");
        assert_eq!(out.first(), Some(&[0.0, 0.0]));
        assert_eq!(out.last(), Some(&[100.0, 0.0]));
        assert!(out.windows(2).all(|w| metres(w[0], w[1]) <= 30.0));
    }

    #[test]
    fn a_closed_ring_comes_back_to_where_it_started() {
        let out = densify(&[[0.0, 0.0], [60.0, 0.0], [60.0, 60.0]], true, 30.0, &metres);
        assert_eq!(out.first(), out.last());
        assert!(out.len() > 6);
    }

    #[test]
    fn a_huge_shape_is_held_to_the_point_limit() {
        let ring: Vec<[f64; 2]> = (0..200).map(|i| [i as f64 * 5000.0, (i % 2) as f64 * 5000.0]).collect();
        assert!(densify(&ring, false, 30.0, &metres).len() <= MAX_POINTS + 1);
    }

    #[test]
    fn dashes_alternate_on_and_off_in_pixels() {
        let line = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0]];
        let runs = dash_runs(&line, 10.0, 5.0, &|_| 1.0);
        assert_eq!(runs.len(), 7, "100 px of 10 on and 5 off: 6 dashes and a short one");
        assert!((runs[0][0][0] - 0.0).abs() < 1e-9 && (runs[0][1][0] - 10.0).abs() < 1e-9);
        assert!((runs[1][0][0] - 15.0).abs() < 1e-9 && (runs[1][1][0] - 25.0).abs() < 1e-9);
    }

    #[test]
    fn dashes_are_shorter_in_metres_where_the_line_is_far() {
        let line = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0]];
        let near = dash_runs(&line, 10.0, 10.0, &|_| 1.0).len();
        let far = dash_runs(&line, 10.0, 10.0, &|_| 2.0).len();
        assert!(far > near);
    }

    #[test]
    fn an_area_is_cut_into_triangles_that_cover_it_and_stay_short() {
        let square = [[0.0, 0.0], [400.0, 0.0], [400.0, 400.0], [0.0, 400.0]];
        let tris = fill_triangles(&square, 80.0, 6000);
        let area: f64 = tris
            .iter()
            .map(|t| {
                ((t[1][0] - t[0][0]) * (t[2][1] - t[0][1]) - (t[2][0] - t[0][0]) * (t[1][1] - t[0][1])).abs() / 2.0
            })
            .sum();
        assert!((area - 160_000.0).abs() < 1.0, "{area}");
        assert!(tris.iter().all(|t| metres(t[0], t[1]) <= 80.0 * 1.5));
    }

    #[test]
    fn the_triangle_count_is_held_to_its_limit() {
        let square = [[0.0, 0.0], [50_000.0, 0.0], [50_000.0, 50_000.0], [0.0, 50_000.0]];
        assert!(fill_triangles(&square, 80.0, 500).len() <= 500);
    }
}
