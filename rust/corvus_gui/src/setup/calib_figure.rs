//! The aircraft figure of the calibration wizard (`src/js/calib-figures.js`):
//! one raven model rotated by the pose matrix, projected orthographically from
//! a fixed three quarter view, painter sorted and Lambert shaded. The six
//! accelerometer sides and the compass rotation are the same bird seen from
//! the same camera, which is what makes them comparable.
//!
//! The model is the web figure's own mesh (`calib_raven.json`, the output of
//! its `buildModel`), so a face is where the web page draws it. Body frame:
//! +X nose, +Y right wing, +Z up.

use std::sync::OnceLock;

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke};
use serde_json::Value;

use crate::setup::calib_protocol::Pose;
use crate::ui::style;

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];
type Q = [f64; 4];

const AZ: f64 = 35.0 * std::f64::consts::PI / 180.0;
const EL: f64 = 24.0 * std::f64::consts::PI / 180.0;
const HALF: f64 = std::f64::consts::FRAC_PI_2;

const DEMO_LEAD_MS: f64 = 500.0;
const DEMO_MOVE_MS: f64 = 1300.0;
const DEMO_HOLD_MS: f64 = 1800.0;
const DEMO_FAINT: f64 = 0.45;
const TWEEN_MS: f64 = 450.0;
const SPIN_RATE: f64 = 0.9;

/// The fixed camera: forward, right and up in the world frame.
struct Cam {
    f: V3,
    r: V3,
    u: V3,
}

fn cam() -> Cam {
    let f = [EL.cos() * AZ.cos(), EL.cos() * AZ.sin(), EL.sin()];
    let (rx, ry) = (-f[1], f[0]);
    let rl = rx.hypot(ry);
    let r = [rx / rl, ry / rl, 0.0];
    let u = [
        f[1] * r[2] - f[2] * r[1],
        f[2] * r[0] - f[0] * r[2],
        f[0] * r[1] - f[1] * r[0],
    ];
    Cam { f, r, u }
}

fn rot_x(a: f64) -> M3 {
    let (c, s) = (a.cos(), a.sin());
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn rot_y(a: f64) -> M3 {
    let (c, s) = (a.cos(), a.sin());
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

fn rot_z(a: f64) -> M3 {
    let (c, s) = (a.cos(), a.sin());
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    m
}

fn apply(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The attitude of each pose as a matrix (`POSE_M`).
pub fn pose_matrix(p: Pose) -> M3 {
    match p {
        Pose::Level => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        Pose::NoseDown => rot_y(HALF),
        Pose::TailDown => rot_y(-HALF),
        Pose::Left => rot_x(HALF),
        Pose::Right => rot_x(-HALF),
        Pose::UpsideDown => rot_x(std::f64::consts::PI),
    }
}

fn quat_from_matrix(m: &M3) -> Q {
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            s / 4.0,
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            s / 4.0,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][2] - m[2][0]) / s,
            (m[0][1] + m[1][0]) / s,
            s / 4.0,
            (m[1][2] + m[2][1]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[1][0] - m[0][1]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            s / 4.0,
        ]
    }
}

fn matrix_from_quat(q: &Q) -> M3 {
    let [w, x, y, z] = *q;
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

/// A turn that keeps the bird rigid: element wise matrix blending would shear it.
fn slerp(a: &Q, b: &Q, t: f64) -> Q {
    let mut dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let mut end = *b;
    if dot < 0.0 {
        dot = -dot;
        end = [-b[0], -b[1], -b[2], -b[3]];
    }
    if dot > 0.9995 {
        let mut q = [0.0; 4];
        for i in 0..4 {
            q[i] = a[i] + t * (end[i] - a[i]);
        }
        let n = q.iter().map(|v| v * v).sum::<f64>().sqrt().max(f64::MIN_POSITIVE);
        return q.map(|v| v / n);
    }
    let theta = dot.min(1.0).acos();
    let sin = theta.sin();
    let wa = ((1.0 - t) * theta).sin() / sin;
    let wb = (t * theta).sin() / sin;
    let mut q = [0.0; 4];
    for i in 0..4 {
        q[i] = wa * a[i] + wb * end[i];
    }
    q
}

fn ease(t: f64) -> f64 {
    let c = t.clamp(0.0, 1.0);
    c * c * (3.0 - 2.0 * c)
}

fn pose_quat(p: Pose) -> Q {
    quat_from_matrix(&pose_matrix(p))
}

struct Face {
    v: Vec<usize>,
    base: [u8; 3],
}

struct Model {
    verts: Vec<V3>,
    faces: Vec<Face>,
    radius: f64,
    light: V3,
}

fn role_color(role: &str) -> [u8; 3] {
    let hex = match role {
        "body" => 0x252C37,
        "head" => 0x303844,
        "wing" => 0x1F252E,
        "wingLayer" => 0x343E4B,
        "tail" => 0x1F252E,
        "tailLayer" => 0x303946,
        "crest" => 0x303946,
        "beak" => 0xE8842E,
        "beakTip" => 0xC96620,
        "beakLine" => 0x7F3A16,
        "eye" => 0xFFD58A,
        "pupil" => 0x10141C,
        "eyeHighlight" => 0xFFF9E8,
        "brow" => 0x28313D,
        "leg" => 0x464A4F,
        "talon" => 0x2C3035,
        _ => 0x252C37,
    };
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

fn model() -> &'static Model {
    static M: OnceLock<Model> = OnceLock::new();
    M.get_or_init(|| {
        let v: Value = serde_json::from_str(include_str!("calib_raven.json")).expect("the raven model");
        let verts: Vec<V3> = v["verts"]
            .as_array()
            .expect("verts")
            .iter()
            .map(|p| {
                let a = p.as_array().expect("a vertex");
                [
                    a[0].as_f64().unwrap_or(0.0),
                    a[1].as_f64().unwrap_or(0.0),
                    a[2].as_f64().unwrap_or(0.0),
                ]
            })
            .collect();
        let faces = v["faces"]
            .as_array()
            .expect("faces")
            .iter()
            .map(|f| Face {
                v: f["v"]
                    .as_array()
                    .expect("indices")
                    .iter()
                    .filter_map(|i| i.as_u64().map(|i| i as usize))
                    .collect(),
                base: role_color(f["role"].as_str().unwrap_or("body")),
            })
            .collect();
        let radius = verts
            .iter()
            .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
            .fold(0.0, f64::max);
        let l: V3 = [-0.32, 0.52, 0.79];
        let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        Model {
            verts,
            faces,
            radius,
            light: [l[0] / n, l[1] / n, l[2] / n],
        }
    })
}

fn face_normal(a: V3, b: V3, c: V3) -> V3 {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    let len = if len == 0.0 { 1.0 } else { len };
    [n[0] / len, n[1] / len, n[2] / len]
}

/// `shade`: each channel times `amount`, rounded and clamped.
fn shade(base: [u8; 3], amount: f64) -> [u8; 3] {
    base.map(|c| (f64::from(c) * amount).round().clamp(0.0, 255.0) as u8)
}

/// How a figure is built: the small strip chip, or the stage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub compact: bool,
    pub spin: bool,
    pub marker: Option<&'static str>,
}

impl View {
    /// The drawing's own size in its units.
    pub fn size(&self) -> (f64, f64) {
        if self.compact {
            (128.0, 100.0)
        } else {
            (240.0, 178.0)
        }
    }

    fn scale(&self) -> f64 {
        (if self.compact { 46.0 } else { 78.0 }) / model().radius
    }

    fn center(&self) -> (f64, f64) {
        let (w, h) = self.size();
        (w / 2.0, if self.compact { h / 2.0 - 4.0 } else { 74.0 })
    }
}

/// What a frame draws, in the drawing's units, back to front.
#[derive(Default)]
pub struct Scene {
    pub faces: Vec<(Vec<[f64; 2]>, [u8; 3])>,
    pub ground: Vec<[[f64; 2]; 2]>,
    pub shadow: Vec<[f64; 2]>,
    /// The ring's runs behind the bird and in front of it.
    pub back_ring: Vec<Vec<[f64; 2]>>,
    pub front_ring: Vec<Vec<[f64; 2]>>,
    /// The arrow head and whether it is behind the bird.
    pub head: Vec<[f64; 2]>,
    pub head_behind: bool,
    pub marker: Option<[f64; 2]>,
}

fn project(view: &View, c: &Cam, p: V3) -> [f64; 2] {
    let (cx, cy) = view.center();
    let s = view.scale();
    [
        cx + s * (p[0] * c.r[0] + p[1] * c.r[1] + p[2] * c.r[2]),
        cy - s * (p[0] * c.u[0] + p[1] * c.u[1] + p[2] * c.u[2]),
    ]
}

fn depth(c: &Cam, p: V3) -> f64 {
    p[0] * c.f[0] + p[1] * c.f[1] + p[2] * c.f[2]
}

/// The scene for an attitude: `base` is the pose matrix, `angle` the spin
/// about the world vertical when the view spins.
pub fn scene(view: &View, base: &M3, angle: f64) -> Scene {
    let m = model();
    let c = cam();
    let matrix = if view.spin { mul(&rot_z(angle), base) } else { *base };
    let world: Vec<V3> = m.verts.iter().map(|v| apply(&matrix, *v)).collect();
    let mut order: Vec<(usize, f64)> = m
        .faces
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let total: f64 = f.v.iter().map(|&k| depth(&c, world[k])).sum();
            (i, total / f.v.len() as f64)
        })
        .collect();
    order.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut sc = Scene::default();
    for (i, _) in order {
        let f = &m.faces[i];
        let pts: Vec<V3> = f.v.iter().map(|&k| world[k]).collect();
        let n = face_normal(pts[0], pts[1], pts[2]);
        let lambert = (n[0] * m.light[0] + n[1] * m.light[1] + n[2] * m.light[2]).max(0.0);
        sc.faces.push((
            pts.iter().map(|p| project(view, &c, *p)).collect(),
            shade(f.base, 0.86 + 0.18 * lambert),
        ));
    }
    let r = m.radius;
    if !view.compact {
        let ground_z = -r * 1.06;
        let radius = r * 0.95;
        for i in 0..=4 {
            let v = -radius + 2.0 * radius * f64::from(i) / 4.0;
            sc.ground.push([
                project(view, &c, [v, -radius, ground_z]),
                project(view, &c, [v, radius, ground_z]),
            ]);
            sc.ground.push([
                project(view, &c, [-radius, v, ground_z]),
                project(view, &c, [radius, v, ground_z]),
            ]);
        }
        sc.shadow = (0..28)
            .map(|i| {
                let a = f64::from(i) / 28.0 * std::f64::consts::TAU;
                project(view, &c, [a.cos() * r * 0.6, a.sin() * r * 0.6, ground_z])
            })
            .collect();
    }
    if view.spin {
        // the rotation asked for is about the world vertical, so the guide
        // ring is a world frame circle, split at the horizon
        let radius = r * 0.98;
        let ring: Vec<V3> = (0..=48)
            .map(|i| {
                let a = f64::from(i) / 48.0 * std::f64::consts::TAU;
                [a.cos() * radius, a.sin() * radius, -0.15]
            })
            .collect();
        let mut run: Vec<V3> = Vec::new();
        let mut run_behind = depth(&c, ring[0]) < 0.0;
        let flush = |run: &mut Vec<V3>, behind: bool, sc: &mut Scene| {
            if run.len() > 1 {
                let line: Vec<[f64; 2]> = run.iter().map(|p| project(view, &c, *p)).collect();
                if behind {
                    sc.back_ring.push(line);
                } else {
                    sc.front_ring.push(line);
                }
            }
            run.clear();
        };
        for p in &ring {
            let behind = depth(&c, *p) < 0.0;
            if behind != run_behind && !run.is_empty() {
                run.push(*p);
                flush(&mut run, run_behind, &mut sc);
                run_behind = behind;
            }
            run.push(*p);
        }
        flush(&mut run, run_behind, &mut sc);
        let a = std::f64::consts::PI * 0.5;
        let tip = [a.cos() * radius, a.sin() * radius, -0.15];
        let tangent = [-a.sin(), a.cos(), 0.0];
        let radial = [a.cos(), a.sin(), 0.0];
        sc.head = vec![
            project(view, &c, [tip[0] + tangent[0] * 0.3, tip[1] + tangent[1] * 0.3, tip[2]]),
            project(
                view,
                &c,
                [
                    tip[0] - tangent[0] * 0.12 + radial[0] * 0.16,
                    tip[1] - tangent[1] * 0.12 + radial[1] * 0.16,
                    tip[2],
                ],
            ),
            project(
                view,
                &c,
                [
                    tip[0] - tangent[0] * 0.12 - radial[0] * 0.16,
                    tip[1] - tangent[1] * 0.12 - radial[1] * 0.16,
                    tip[2],
                ],
            ),
        ];
        sc.head_behind = depth(&c, tip) < 0.0;
    }
    if view.marker == Some("nose") {
        sc.marker = Some(project(view, &c, apply(&matrix, [1.42, 0.0, -0.04])));
    }
    sc
}

/// The turn the figure is showing, kept between frames.
pub struct Figure {
    pub view: View,
    pose: Pose,
    from: Option<Pose>,
    motion: bool,
    angle: f64,
    previous: f64,
    shown: Q,
    shown_opacity: f64,
    tween: Option<(Q, f64, f64)>,
    demo_start: f64,
}

/// What `frame` answers: the attitude, the opacity and whether it moves.
struct Frame {
    base: M3,
    opacity: f64,
    animating: bool,
}

impl Figure {
    pub fn new(view: View, pose: Pose) -> Figure {
        Figure {
            view,
            pose,
            from: None,
            motion: false,
            angle: 0.0,
            previous: 0.0,
            shown: pose_quat(pose),
            shown_opacity: 1.0,
            tween: None,
            demo_start: 0.0,
        }
    }

    fn demo_active(&self) -> bool {
        self.motion && self.from.is_some_and(|f| f != self.pose)
    }

    /// Takes a new target; a turn to it starts from where the figure is, and
    /// a loop from its start.
    pub fn set(&mut self, pose: Pose, spin: bool, marker: Option<&'static str>, from: Option<Pose>, motion: bool) {
        let mut changed = false;
        if pose != self.pose {
            self.pose = pose;
            changed = true;
        }
        if from != self.from {
            self.from = from;
            changed = true;
        }
        if motion != self.motion {
            self.motion = motion;
            changed = true;
        }
        if spin != self.view.spin {
            self.view.spin = spin;
            changed = true;
        }
        if marker != self.view.marker {
            self.view.marker = marker;
            changed = true;
        }
        if !changed {
            return;
        }
        self.demo_start = 0.0;
        if self.demo_active() {
            self.tween = None;
        } else {
            let t = pose_quat(self.pose);
            let dot = (self.shown[0] * t[0] + self.shown[1] * t[1] + self.shown[2] * t[2] + self.shown[3] * t[3]).abs();
            self.tween = (dot < 0.99999 || self.shown_opacity < 1.0).then_some((self.shown, self.shown_opacity, 0.0));
        }
    }

    /// The attitude and opacity at `time` (milliseconds).
    fn frame(&mut self, time: f64) -> Frame {
        if self.demo_active() {
            if self.demo_start == 0.0 {
                self.demo_start = if time == 0.0 { 1.0 } else { time };
            }
            let cycle = DEMO_LEAD_MS + DEMO_MOVE_MS + DEMO_HOLD_MS;
            let t = (time - self.demo_start).rem_euclid(cycle);
            let p = ease((t - DEMO_LEAD_MS) / DEMO_MOVE_MS);
            let from = self.from.unwrap_or(self.pose);
            self.shown = slerp(&pose_quat(from), &pose_quat(self.pose), p);
            self.shown_opacity = DEMO_FAINT + (1.0 - DEMO_FAINT) * p;
            return Frame {
                base: matrix_from_quat(&self.shown),
                opacity: self.shown_opacity,
                animating: true,
            };
        }
        if let Some((from, opacity, start)) = self.tween {
            let start = if start == 0.0 {
                let s = if time == 0.0 { 1.0 } else { time };
                self.tween = Some((from, opacity, s));
                s
            } else {
                start
            };
            let p = ease((time - start) / TWEEN_MS);
            self.shown = slerp(&from, &pose_quat(self.pose), p);
            self.shown_opacity = opacity + (1.0 - opacity) * p;
            if p >= 1.0 {
                self.tween = None;
            }
            return Frame {
                base: if self.tween.is_some() {
                    matrix_from_quat(&self.shown)
                } else {
                    pose_matrix(self.pose)
                },
                opacity: self.shown_opacity,
                animating: self.tween.is_some(),
            };
        }
        self.shown = pose_quat(self.pose);
        self.shown_opacity = 1.0;
        Frame {
            base: pose_matrix(self.pose),
            opacity: 1.0,
            animating: false,
        }
    }

    /// Draws into `rect` (the drawing's aspect, scaled to fit and centred) at
    /// `time` in seconds; true while it moves, so the page keeps repainting.
    pub fn paint(&mut self, ui: &egui::Ui, rect: Rect, time: f64) -> bool {
        let ms = time * 1000.0;
        if self.previous != 0.0 && self.view.spin {
            self.angle += (ms - self.previous) / 1000.0 * SPIN_RATE;
        }
        self.previous = ms;
        let frame = self.frame(ms);
        let sc = scene(&self.view, &frame.base, self.angle);
        let (vw, vh) = self.view.size();
        let k = (f64::from(rect.width()) / vw).min(f64::from(rect.height()) / vh) as f32;
        let origin = Pos2::new(
            rect.center().x - vw as f32 * k / 2.0,
            rect.center().y - vh as f32 * k / 2.0,
        );
        draw(ui.painter(), &sc, origin, k, frame.opacity as f32, time, self.view);
        let moving = self.view.spin || frame.animating;
        if !moving {
            self.previous = 0.0;
        }
        moving
    }
}

/// Paints a scene with `origin` and `k` px per unit.
pub fn draw(painter: &egui::Painter, sc: &Scene, origin: Pos2, k: f32, opacity: f32, time: f64, view: View) {
    let at = |p: &[f64; 2]| Pos2::new(origin.x + p[0] as f32 * k, origin.y + p[1] as f32 * k);
    let nav = style::nav();
    let ring = Stroke::new(1.8 * k, nav.gamma_multiply(0.92));
    for l in &sc.ground {
        painter.line_segment(
            [at(&l[0]), at(&l[1])],
            Stroke::new(0.6 * k, style::border().gamma_multiply(0.7)),
        );
    }
    if sc.shadow.len() > 2 {
        painter.add(Shape::convex_polygon(
            sc.shadow.iter().map(at).collect(),
            Color32::from_black_alpha(41),
            Stroke::NONE,
        ));
    }
    for run in &sc.back_ring {
        painter.add(Shape::line(run.iter().map(at).collect(), ring));
    }
    if sc.head.len() == 3 && sc.head_behind {
        painter.add(Shape::convex_polygon(
            sc.head.iter().map(at).collect(),
            nav,
            Stroke::NONE,
        ));
    }
    for (pts, rgb) in &sc.faces {
        let fill = Color32::from_rgb(rgb[0], rgb[1], rgb[2]).gamma_multiply(opacity);
        fill_polygon(painter, pts.iter().map(at).collect(), fill);
    }
    for run in &sc.front_ring {
        painter.add(Shape::line(run.iter().map(at).collect(), ring));
    }
    if sc.head.len() == 3 && !sc.head_behind {
        painter.add(Shape::convex_polygon(
            sc.head.iter().map(at).collect(),
            nav,
            Stroke::NONE,
        ));
    }
    if let Some(m) = &sc.marker {
        // `calibMarkerPulse`: 1.8 s, 1 to 0.35 and back
        let phase = (time / 1.8).fract() as f32 * std::f32::consts::TAU;
        let alpha = 0.675 + 0.325 * phase.cos();
        painter.circle_stroke(
            at(m),
            (if view.compact { 3.0 } else { 5.0 }) * k,
            Stroke::new(1.6 * k, nav.gamma_multiply(alpha)),
        );
    }
}

fn cross(o: Pos2, a: Pos2, b: Pos2) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

/// A polygon: convex ones go through egui's anti aliased path, concave ones
/// are cut into triangles first.
fn fill_polygon(painter: &egui::Painter, pts: Vec<Pos2>, fill: Color32) {
    let pts = clean(pts);
    if pts.len() < 3 {
        return;
    }
    let n = pts.len();
    let signs: Vec<f32> = (0..n)
        .map(|i| cross(pts[i], pts[(i + 1) % n], pts[(i + 2) % n]))
        .collect();
    let convex = signs.iter().all(|s| *s >= -1e-3) || signs.iter().all(|s| *s <= 1e-3);
    // egui feathers an edge outwards along the bisector of its corner, which
    // turns a sharp corner into a long spike, so thin faces are filled plain
    let sharp = (0..n).any(|i| {
        let (a, b, c) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
        let (u, v) = (a - b, c - b);
        let denom = u.length() * v.length();
        denom > 0.0 && (u.x * v.x + u.y * v.y) / denom > 0.94
    });
    if convex && !sharp {
        painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
        return;
    }
    let mut mesh = egui::Mesh::default();
    for p in &pts {
        mesh.colored_vertex(*p, fill);
    }
    for t in triangulate(&pts) {
        mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
    }
    painter.add(Shape::mesh(mesh));
}

/// The outline without repeated points, which egui's edge feathering turns
/// into long spikes, and `None` worth drawing when what is left has no area.
fn clean(pts: Vec<Pos2>) -> Vec<Pos2> {
    let mut out: Vec<Pos2> = Vec::with_capacity(pts.len());
    for p in pts {
        if out.last().is_none_or(|q| (p - *q).length() > 0.05) {
            out.push(p);
        }
    }
    while out.len() > 1 && (out[0] - out[out.len() - 1]).length() <= 0.05 {
        out.pop();
    }
    let n = out.len();
    if n < 3 {
        return Vec::new();
    }
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (out[i], out[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum::<f32>()
        / 2.0;
    if area.abs() < 0.05 {
        return Vec::new();
    }
    out
}

/// Ear clipping of a simple polygon: index triples.
pub fn triangulate(pts: &[Pos2]) -> Vec<[usize; 3]> {
    let n = pts.len();
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let mut idx: Vec<usize> = (0..n).collect();
    if area < 0.0 {
        idx.reverse();
    }
    let mut out = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (a, b, c) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            if cross(pts[a], pts[b], pts[c]) <= 0.0 {
                continue;
            }
            let inside = idx.iter().any(|&p| {
                p != a && p != b && p != c && {
                    let (d1, d2, d3) = (
                        cross(pts[a], pts[b], pts[p]),
                        cross(pts[b], pts[c], pts[p]),
                        cross(pts[c], pts[a], pts[p]),
                    );
                    d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0
                }
            });
            if !inside {
                out.push([a, b, c]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        out.push([idx[0], idx[1], idx[2]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the web figure drew (`calib_figure_cases.json`, made by running
    /// calib-figures.js against a recording document).
    const CASES: &str = include_str!("calib_figure_cases.json");

    fn points(s: &str) -> Vec<[f64; 2]> {
        s.split(' ')
            .map(|p| {
                let (x, y) = p.split_once(',').expect("x,y");
                [x.parse().unwrap(), y.parse().unwrap()]
            })
            .collect()
    }

    fn fill(s: &str) -> [u8; 3] {
        let v: Vec<u8> = s
            .trim_start_matches("rgb(")
            .trim_end_matches(')')
            .split(',')
            .map(|c| c.parse().unwrap())
            .collect();
        [v[0], v[1], v[2]]
    }

    fn near(a: &[f64; 2], b: &[f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 0.11 && (a[1] - b[1]).abs() < 0.11
    }

    fn same_face(pts: &[[f64; 2]], rgb: &[u8; 3], web: &[[f64; 2]], w: &[u8; 3]) -> bool {
        pts.len() == web.len()
            && pts.iter().zip(web).all(|(a, b)| near(a, b))
            && rgb
                .iter()
                .zip(w)
                .all(|(a, b)| (i16::from(*a) - i16::from(*b)).abs() <= 1)
    }

    /// `exact`: the order is the web's too. Faces at the very same depth may
    /// swap where the two languages round differently, which draws the same
    /// picture, so only the figure that has no such tie is held to the order.
    fn check(case: &Value, ours: &Scene, exact: bool) {
        let polys = case["polys"].as_array().unwrap();
        let mut taken = vec![false; ours.faces.len()];
        for (i, p) in polys.iter().enumerate() {
            let web = points(p["p"].as_str().unwrap());
            let w = fill(p["f"].as_str().unwrap());
            let found = if exact {
                let (pts, rgb) = &ours.faces[i];
                same_face(pts, rgb, &web, &w).then_some(i)
            } else {
                ours.faces
                    .iter()
                    .enumerate()
                    .find(|(k, (pts, rgb))| !taken[*k] && same_face(pts, rgb, &web, &w))
                    .map(|(k, _)| k)
            };
            let k = found.unwrap_or_else(|| panic!("{:?} face {i} is not where the web puts it", case["opts"]));
            taken[k] = true;
        }
    }

    fn figure_of(case: &Value) -> (View, M3) {
        let o = &case["opts"];
        let pose = Pose::ALL
            .into_iter()
            .find(|p| p.key() == o["pose"].as_str().unwrap_or("level"))
            .unwrap();
        let view = View {
            compact: o["compact"].as_bool().unwrap_or(false),
            spin: o["spin"].as_bool().unwrap_or(false),
            marker: (o["marker"] == "nose").then_some("nose"),
        };
        (view, pose_matrix(pose))
    }

    #[test]
    fn the_model_is_the_web_figures_and_as_big() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        assert!((model().radius - cases["radius"].as_f64().unwrap()).abs() < 1e-4);
        assert_eq!(model().faces.len() as u64, cases["faces"].as_u64().unwrap());
    }

    #[test]
    fn every_face_lands_where_the_web_figure_puts_it_in_the_same_order() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        for (i, case) in cases["figs"].as_array().unwrap().iter().enumerate() {
            let (view, base) = figure_of(case);
            check(case, &scene(&view, &base, 0.0), i == 0);
        }
    }

    #[test]
    fn the_ground_the_ring_and_the_marker_are_the_web_figures() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        let figs = cases["figs"].as_array().unwrap();
        // the stage figure: a 5 by 5 grid and a shadow
        let (view, base) = figure_of(&figs[3]);
        let sc = scene(&view, &base, 0.0);
        assert_eq!(sc.ground.len(), 10);
        assert_eq!(sc.shadow.len(), 28);
        let lines: Vec<&Value> = figs[3]["ground"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|g| g["n"] == "line")
            .collect();
        for (g, web) in sc.ground.iter().zip(lines) {
            let a = &web["a"];
            let want = |k: &str| a[k].as_str().unwrap().parse::<f64>().unwrap();
            assert!(near(&g[0], &[want("x1"), want("y1")]) && near(&g[1], &[want("x2"), want("y2")]));
        }
        // the guide ring of a compass figure
        let (view, base) = figure_of(&figs[5]);
        let sc = scene(&view, &base, 0.0);
        let web: Vec<&Value> = figs[5]["arrows"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g.as_array().unwrap())
            .filter(|n| n["n"] == "polyline")
            .collect();
        let ours: Vec<&Vec<[f64; 2]>> = sc.back_ring.iter().chain(sc.front_ring.iter()).collect();
        assert_eq!(web.len(), ours.len());
        let total: usize = web.iter().map(|w| points(w["p"].as_str().unwrap()).len()).sum();
        assert_eq!(total, ours.iter().map(|o| o.len()).sum::<usize>());
        let first = points(web[0]["p"].as_str().unwrap());
        assert!(near(&first[0], &ours[0][0]), "the ring starts where the web ring does");
        // the nose marker of the airspeed figure
        let (view, base) = figure_of(&figs[4]);
        let m = scene(&view, &base, 0.0).marker.expect("a marker");
        let web = &figs[4]["marker"][0];
        let want = |k: &str| web[k].as_str().unwrap().parse::<f64>().unwrap();
        assert!(near(&m, &[want("cx"), want("cy")]));
    }

    #[test]
    fn a_turn_between_poses_stays_rigid_and_ends_on_the_target() {
        let a = pose_quat(Pose::Level);
        let b = pose_quat(Pose::UpsideDown);
        let mid = slerp(&a, &b, 0.5);
        let norm: f64 = mid.iter().map(|v| v * v).sum();
        assert!((norm - 1.0).abs() < 1e-9);
        let end = matrix_from_quat(&slerp(&a, &b, 1.0));
        let want = pose_matrix(Pose::UpsideDown);
        for i in 0..3 {
            for j in 0..3 {
                assert!((end[i][j] - want[i][j]).abs() < 1e-9);
            }
        }
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert_eq!(ease(0.5), 0.5);
    }

    #[test]
    fn a_new_pose_is_turned_to_and_a_demo_loops_between_two() {
        let view = View {
            compact: false,
            spin: false,
            marker: None,
        };
        let mut f = Figure::new(view, Pose::Level);
        f.set(Pose::Left, false, None, None, false);
        assert!(f.frame(1000.0).animating, "turning to the new pose");
        assert!(!f.frame(1000.0 + TWEEN_MS + 1.0).animating, "arrived");
        let mut g = Figure::new(view, Pose::Left);
        g.set(Pose::Left, false, None, Some(Pose::Level), false);
        assert!(!g.frame(0.0).animating, "no motion without the cue");
        g.set(Pose::Left, false, None, Some(Pose::Level), true);
        let first = g.frame(1000.0);
        assert!(
            first.animating && (first.opacity - DEMO_FAINT).abs() < 1e-9,
            "starts faint at the side it is on"
        );
        let hold = g.frame(1000.0 + DEMO_LEAD_MS + DEMO_MOVE_MS + 10.0);
        assert!((hold.opacity - 1.0).abs() < 1e-9, "solid at the target");
    }

    #[test]
    fn a_concave_face_is_cut_into_triangles_that_cover_it() {
        // an L shape
        let l = [
            Pos2::new(0.0, 0.0),
            Pos2::new(2.0, 0.0),
            Pos2::new(2.0, 1.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(1.0, 2.0),
            Pos2::new(0.0, 2.0),
        ];
        let tris = triangulate(&l);
        assert_eq!(tris.len(), 4);
        let area: f32 = tris.iter().map(|t| cross(l[t[0]], l[t[1]], l[t[2]]).abs() / 2.0).sum();
        assert!((area - 3.0).abs() < 1e-5);
    }
}
