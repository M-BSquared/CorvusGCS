//! What plugins draw on the ground (lines, outlines and areas of `api.map`)
//! as geometry on the terrain, so the depth test hides what a hill is in
//! front of, as it hides the flown track. Text, markers and buttons stay
//! screen elements; `Map3d::hidden` tests them against the ground.
//!
//! A shape is laid on the ground once ([`Draped`]) and again only when it
//! moves or more elevation arrives: reading the ground is the costly part, and
//! a frame only turns the draped points into ribbons for the camera. A dashed
//! line is cut into at most `MAX_DASHES` dashes, only where it is in front of
//! the camera, and all of one stroke goes to the GPU as one mesh.

use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{Color32, Pos2};

use crate::map3d::camera::{mercator, unmercator, V3};
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
const FILL_EDGE_M: f64 = 30.0;
/// ...and into at most this many.
const MAX_TRIANGLES: usize = 8000;
/// How far above the ground the overlay lies, in metres.
const LIFT_M: f64 = 2.0;
/// The most dashes one stroke is cut into; past it the pattern grows longer.
/// Each dash is a quad or two of one mesh, so this bounds the vertices too.
pub const MAX_DASHES: usize = 600;
/// Milliseconds a frame may spend laying shapes on the ground. One shape is
/// always laid; the rest keep the form they had and wait for the next frame.
const DRAPE_MS: u128 = 6;
/// Frames a shape that is no longer drawn keeps its draped form.
const DRAPE_KEEP_FRAMES: u64 = 120;
/// How far the view centre's height may move before a shape that took it
/// for ground not yet known is laid again, in metres.
const GUESS_SLACK_M: f64 = 1.0;

/// A shape laid on the ground: the points of its line and the mesh of its
/// area, with what they were made from.
pub(super) struct Draped {
    sig: u64,
    generation: u64,
    /// The view centre's height taken for ground that was not known yet.
    guessed: Option<f64>,
    /// Longitude, latitude and height above sea level of the line.
    line: Vec<[f64; 3]>,
    /// The area's mesh, in mercator metres around its origin.
    fill: Option<(Arc<gpu::MeshData>, (f64, f64))>,
    used: u64,
}

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
/// point, which shrinks with distance, and `None` where the point is not in
/// front of the camera: no dash is cut there, and the line breaks. However
/// close the line comes to the camera, it gives at most `MAX_DASHES` dashes;
/// the pattern is stretched to fit.
pub fn dash_runs(points: &[V3], on: f64, off: f64, px_per_m: &dyn Fn(V3) -> Option<f64>) -> Vec<Vec<V3>> {
    let mut runs: Vec<Vec<V3>> = Vec::new();
    let Some(first) = points.first() else { return runs };
    if !(on > 0.0 && off >= 0.0) {
        return runs;
    }
    let length = |a: V3, b: V3| ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
    let mid = |a: V3, b: V3| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0];
    let seen = |a: V3, b: V3| px_per_m(mid(a, b)).filter(|p| p.is_finite() && *p > 0.0);
    let total: f64 = points
        .windows(2)
        .filter_map(|w| seen(w[0], w[1]).map(|p| length(w[0], w[1]) * p))
        .sum();
    let stretch = (total / ((on + off) * MAX_DASHES as f64)).max(1.0);
    let (on, off) = (on * stretch, off * stretch);
    let (mut drawing, mut left) = (true, on);
    let mut current = vec![*first];
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let Some(scale) = seen(a, b) else {
            if drawing {
                if current.len() >= 2 {
                    runs.push(std::mem::take(&mut current));
                }
                current = vec![b];
            }
            continue;
        };
        let px = length(a, b) * scale;
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

/// What the geometry of a shape is made from, so a draped form is made again
/// only when that changes and not when a plugin redraws the same shape.
fn signature(pts: &[[f64; 2]], closed: bool, fill: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (closed, fill, pts.len()).hash(&mut h);
    for p in pts {
        (p[0].to_bits(), p[1].to_bits()).hash(&mut h);
    }
    h.finish()
}

/// Mercator `x` moved to the copy of the world nearest `centre_x`.
fn nearest_copy(x: f64, centre_x: f64) -> f64 {
    let world = 2.0 * std::f64::consts::PI * crate::map3d::camera::R;
    x - ((x - centre_x) / world).round() * world
}

/// One mesh of all the ribbons of `runs`.
pub fn ribbons(
    runs: &[Vec<V3>],
    width_px: f64,
    cam: &crate::map3d::camera::Camera,
    ground: f64,
) -> Option<(Vec<f32>, Vec<u32>)> {
    let (mut verts, mut indices): (Vec<f32>, Vec<u32>) = (Vec::new(), Vec::new());
    for run in runs {
        if let Some((v, i)) = ribbon::build(run, width_px, cam, ground) {
            let base = (verts.len() / FLOATS) as u32;
            indices.extend(i.into_iter().map(|k| k + base));
            verts.extend(v);
        }
    }
    (!indices.is_empty()).then_some((verts, indices))
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
    pub(super) fn shape_items(&mut self, shapes: &[Shape]) -> Vec<gpu::Item> {
        let centre = mercator(self.cam.lng, self.cam.lat);
        let generation = self.terrain_generation();
        let mut spent = std::time::Duration::ZERO;
        let mut laid = 0;
        let mut items = Vec::new();
        for s in shapes.iter().filter(|s| s.visible) {
            let (pts, closed, fill, outline, dashed, width) = match &s.kind {
                Kind::Line { pts, dashed } => (pts, false, 0.0, true, *dashed, s.width),
                Kind::Polygon {
                    pts,
                    fill_opacity,
                    outline,
                    dashed,
                } if pts.len() >= 3 => (pts, true, *fill_opacity, *outline > 0.0, *dashed, *outline),
                _ => continue,
            };
            let key = format!("{}\u{1f}{}", s.owner, s.key);
            let sig = signature(pts, closed, fill > 0.0);
            let stale = self.draped.get(&key).is_none_or(|d| {
                d.sig != sig
                    || d.generation != generation
                    || d.guessed.is_some_and(|g| (g - self.cam.ground).abs() > GUESS_SLACK_M)
            });
            if stale {
                if laid == 0 || spent.as_millis() < DRAPE_MS {
                    let started = std::time::Instant::now();
                    let d = self.drape_shape(pts, closed, fill > 0.0, sig, generation);
                    self.draped.insert(key.clone(), d);
                    spent += started.elapsed();
                    laid += 1;
                } else {
                    self.ctx.request_repaint();
                }
            }
            let Some(d) = self.draped.get_mut(&key) else { continue };
            d.used = self.frame;
            let area = d.fill.clone();
            let line: Vec<V3> = if outline {
                d.line.iter().map(|p| self.cam.local_of(p[0], p[1], p[2])).collect()
            } else {
                Vec::new()
            };
            if let Some((mesh, origin)) = area {
                items.push(gpu::Item {
                    mesh,
                    origin: (nearest_copy(origin.0, centre.0), origin.1),
                    texture: None,
                    uv: [0.0, 0.0, 1.0, 1.0],
                    fallback: None,
                    flat: false,
                    color: rgb(s.color),
                    alpha: fill,
                    transient: false,
                });
            }
            if outline {
                self.stroke(&mut items, centre, &line, s, dashed, width);
            }
        }
        let frame = self.frame;
        self.draped.retain(|_, d| d.used + DRAPE_KEEP_FRAMES >= frame);
        items
    }

    /// A shape laid on the ground as it is known now. Ground not known yet
    /// is taken at the view centre's height, and the shape is laid again
    /// once more is known.
    fn drape_shape(&self, pts: &[[f64; 2]], closed: bool, fill: bool, sig: u64, generation: u64) -> Draped {
        let cam = &self.cam;
        let guessed = Cell::new(false);
        let height = |lng: f64, lat: f64| {
            self.known_ground(lng, lat).unwrap_or_else(|| {
                guessed.set(true);
                cam.ground
            })
        };
        let metres = |a: [f64; 2], b: [f64; 2]| {
            let (pa, pb) = (cam.local_of(a[0], a[1], 0.0), cam.local_of(b[0], b[1], 0.0));
            (pb[0] - pa[0]).hypot(pb[1] - pa[1])
        };
        let line = densify(pts, closed, PIECE_M, &metres)
            .into_iter()
            .map(|p| [p[0], p[1], height(p[0], p[1]) + LIFT_M])
            .collect();
        let fill = if fill { fill_mesh(pts, &height) } else { None };
        Draped {
            sig,
            generation,
            guessed: guessed.get().then_some(cam.ground),
            line,
            fill,
            used: self.frame,
        }
    }

    /// A line as the casing the track has and the colour on top of it, each
    /// one mesh however many dashes it has.
    fn stroke(&self, items: &mut Vec<gpu::Item>, centre: (f64, f64), line: &[V3], s: &Shape, dashed: bool, width: f32) {
        let width = f64::from(width);
        let view = self.cam.view();
        let focal = self.cam.focal();
        let near = self.cam.near();
        let px_per_m = |p: V3| {
            let depth = (p[0] - view.pos[0]) * view.forward[0]
                + (p[1] - view.pos[1]) * view.forward[1]
                + (p[2] - view.pos[2]) * view.forward[2];
            (depth >= near).then(|| focal / depth)
        };
        let runs = if dashed {
            dash_runs(line, 2.0 * width, 1.5 * width, &px_per_m)
        } else {
            vec![line.to_vec()]
        };
        let bg = rgb(crate::ui::style::bg());
        let solid = [line.to_vec()];
        for (w, color, alpha, parts) in [
            (width + 2.4, bg, 0.45 * s.opacity, &solid[..]),
            (width, rgb(s.color), s.opacity, &runs[..]),
        ] {
            if let Some((verts, indices)) = ribbons(parts, w, &self.cam, self.cam.ground) {
                items.push(item(centre, verts, indices, color, alpha));
            }
        }
    }
}

/// An area lying on the ground, cut into small triangles so it follows the
/// hills under it, in mercator metres around its first corner (the origin
/// returned). `height` gives the ground's height above sea level.
fn fill_mesh(ring: &[[f64; 2]], height: &dyn Fn(f64, f64) -> f64) -> Option<(Arc<gpu::MeshData>, (f64, f64))> {
    let first = ring.first()?;
    let origin = mercator(first[0], first[1]);
    let k = first[1].to_radians().cos();
    let local: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| {
            let (x, y) = mercator(p[0], p[1]);
            [(x - origin.0) * k, (y - origin.1) * k]
        })
        .collect();
    let mut verts: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for tri in fill_triangles(&local, FILL_EDGE_M, MAX_TRIANGLES) {
        for p in tri {
            let (mx, my) = (p[0] / k, p[1] / k);
            let (lng, lat) = unmercator(origin.0 + mx, origin.1 + my);
            indices.push((verts.len() / FLOATS) as u32);
            verts.extend_from_slice(&[mx as f32, my as f32, (height(lng, lat) + LIFT_M) as f32, 0.0, 0.0, 1.0]);
        }
    }
    (!indices.is_empty()).then(|| (Arc::new(gpu::MeshData::new(verts, indices)), origin))
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
        let runs = dash_runs(&line, 10.0, 5.0, &|_| Some(1.0));
        assert_eq!(runs.len(), 7, "100 px of 10 on and 5 off: 6 dashes and a short one");
        assert!((runs[0][0][0] - 0.0).abs() < 1e-9 && (runs[0][1][0] - 10.0).abs() < 1e-9);
        assert!((runs[1][0][0] - 15.0).abs() < 1e-9 && (runs[1][1][0] - 25.0).abs() < 1e-9);
    }

    #[test]
    fn dashes_are_shorter_in_metres_where_the_line_is_far() {
        let line = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0]];
        let near = dash_runs(&line, 10.0, 10.0, &|_| Some(1.0)).len();
        let far = dash_runs(&line, 10.0, 10.0, &|_| Some(2.0)).len();
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

    #[test]
    fn a_line_that_passes_behind_the_camera_is_not_cut_there() {
        let line: Vec<V3> = (0..=100).map(|i| [f64::from(i), 0.0, 0.0]).collect();
        let seen = |p: V3| (p[0] > 20.0 && p[0] < 80.0).then_some(1.0);
        let runs = dash_runs(&line, 1.0, 1.0, &seen);
        assert!(!runs.is_empty());
        assert!(
            runs.iter().flatten().all(|p| (20.0..=80.0).contains(&p[0])),
            "dashes only where the line is seen"
        );
        assert!(
            dash_runs(&line, 1.0, 1.0, &|_| None).is_empty(),
            "nothing seen, nothing cut"
        );
    }

    #[test]
    fn a_line_right_at_the_camera_gives_a_bounded_number_of_dashes() {
        let line = [[0.0, 0.0, 0.0], [3000.0, 0.0, 0.0]];
        let runs = dash_runs(&line, 5.0, 3.75, &|_| Some(1000.0));
        assert!(runs.len() <= MAX_DASHES + 1, "{} dashes", runs.len());
        assert!(runs.len() >= MAX_DASHES / 2, "still a dashed line: {}", runs.len());
        assert!(
            dash_runs(&line, 0.0, 3.0, &|_| Some(1.0)).is_empty(),
            "no dash length, no dashes"
        );
    }

    fn radar(owner: &'static str, at: [f64; 2], radius: f64) -> Shape {
        Shape {
            owner,
            key: format!("{owner}-c"),
            kind: Kind::Polygon {
                pts: crate::plugins::shapes::circle_ring(at, radius, None),
                fill_opacity: 0.14,
                outline: 2.5,
                dashed: true,
            },
            color: Color32::from_rgb(91, 141, 239),
            width: 2.5,
            opacity: 1.0,
            visible: true,
        }
    }

    fn close_view(zoom: f64, pitch: f64) -> Map3d {
        let ctx = eframe::egui::Context::default();
        let mut m = Map3d::new(&ctx, &crate::net::http::Server::local(9), "satellite", 19);
        m.cam.set_centre((11.6385, 48.0795));
        m.cam.zoom = zoom;
        m.cam.pitch = pitch;
        m.cam.size = (1400.0, 900.0);
        m
    }

    #[test]
    fn a_dashed_area_around_a_close_camera_is_three_meshes_of_bounded_size() {
        // A 500 m radar zone with the camera inside it once cut its dashed rim
        // into some 200 000 ribbons a frame at zoom 21, each a mesh of its own:
        // the GPU driver, and with it the whole machine, stalled.
        for zoom in [12.0, 15.0, 17.0, 19.0, 21.0] {
            for pitch in [0.0, 45.0, 60.0] {
                let mut m = close_view(zoom, pitch);
                let items = m.shape_items(&[radar("radar", [11.6385, 48.0795], 500.0)]);
                assert!(items.len() <= 3, "zoom {zoom} pitch {pitch}: {} meshes", items.len());
                let verts: usize = items.iter().map(|i| i.mesh.verts.len() / FLOATS).sum();
                assert!(
                    verts <= MAX_TRIANGLES * 3 + MAX_POINTS * 2 + MAX_DASHES * 8 + 16,
                    "zoom {zoom} pitch {pitch}: {verts} vertices"
                );
            }
        }
    }

    #[test]
    fn a_shape_is_laid_on_the_ground_once_and_again_when_it_moves() {
        let mut m = close_view(18.0, 60.0);
        let first = m.shape_items(&[radar("radar", [11.6385, 48.0795], 300.0)]);
        let area = |items: &[gpu::Item]| items.iter().find(|i| !i.transient).map(|i| i.mesh.id);
        let id = area(&first).expect("the area is a kept mesh");
        m.frame += 1;
        m.cam.bearing = 90.0;
        let again = m.shape_items(&[radar("radar", [11.6385, 48.0795], 300.0)]);
        assert_eq!(area(&again), Some(id), "a turned camera keeps the area as laid");
        m.frame += 1;
        let moved = m.shape_items(&[radar("radar", [11.64, 48.0795], 300.0)]);
        assert_ne!(area(&moved), Some(id), "a moved shape is laid again");
        m.frame += DRAPE_KEEP_FRAMES + 2;
        m.shape_items(&[]);
        assert!(m.draped.is_empty(), "a shape that went is forgotten");
    }

    #[test]
    fn many_new_shapes_are_laid_a_few_a_frame() {
        let mut m = close_view(16.0, 60.0);
        let zones: Vec<Shape> = (0..200)
            .map(|i| {
                let mut z = radar("radar", [11.6 + f64::from(i) * 0.001, 48.08], 400.0);
                z.key = format!("zone-{i}");
                z
            })
            .collect();
        let started = std::time::Instant::now();
        m.shape_items(&zones);
        assert!(m.draped.len() < zones.len(), "{} laid in one frame", m.draped.len());
        assert!(started.elapsed().as_millis() < 2000, "{:?}", started.elapsed());
        for _ in 0..400 {
            m.frame += 1;
            m.shape_items(&zones);
            if m.draped.len() == zones.len() {
                break;
            }
        }
        assert_eq!(m.draped.len(), zones.len(), "and every one in the end");
    }
}
