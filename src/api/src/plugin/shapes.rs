//! What a plugin draws on the Home map: lines, areas, text, buttons and icon
//! markers (`api.map` of `python/src/js/plugins.js`, drawn by
//! `python/src/js/map-overlays.js` and `.map-overlay-*` of `map.css`).
//!
//! The shapes are kept as data in longitude and latitude and painted through a
//! projection, so the flat map and the tilted 3D map draw the same set and
//! both answer the pointer for markers and buttons.

use eframe::egui::{self, Color32, Pos2, Rect, Shape as Paint, Stroke, StrokeKind, Vec2};

use crate::ui::style::{self, Weight};

const EARTH_RADIUS_M: f64 = 6_371_008.8;
pub const TEXT_MAX: usize = 200;
/// `OVERLAY_DEFAULT_COLOR`.
pub const DEFAULT_COLOR: Color32 = Color32::from_rgb(0x2B, 0xC4, 0xE4);

/// `LINE_COLORS`: colours that read against imagery and cannot be mistaken
/// for the track (red) or the plan route (amber). `(id, label, colour)`.
#[allow(dead_code)]
pub const COLORS: [(&str, &str, Color32); 5] = [
    ("cyan", "Cyan", Color32::from_rgb(0x2B, 0xC4, 0xE4)),
    ("lime", "Lime", Color32::from_rgb(0x7B, 0xD3, 0x89)),
    ("magenta", "Magenta", Color32::from_rgb(0xC7, 0x7D, 0xFF)),
    ("blue", "Blue", Color32::from_rgb(0x5B, 0x8D, 0xEF)),
    ("white", "White", Color32::WHITE),
];

/// `#RRGGBB` as a colour.
pub fn hex(text: &str) -> Option<Color32> {
    let t = text.strip_prefix('#')?;
    if t.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(t, 16).ok()?;
    Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// The corners of a circle of `radius` metres around `[lng, lat]`, as a ring
/// (not closed): spherical, 8 to 256 corners. Empty for an unusable centre or
/// radius (`circleRing`).
pub fn circle_ring(center: [f64; 2], radius: f64, segments: Option<usize>) -> Vec<[f64; 2]> {
    let [lng, lat] = center;
    if !lng.is_finite() || !lat.is_finite() || lat.abs() > 90.0 || lng.abs() > 180.0 {
        return Vec::new();
    }
    if !radius.is_finite() || radius <= 0.0 || radius > 1e6 {
        return Vec::new();
    }
    let n = segments.unwrap_or(72).clamp(8, 256);
    let rad = std::f64::consts::PI / 180.0;
    let (phi1, lambda1) = (lat * rad, lng * rad);
    let delta = radius / EARTH_RADIUS_M;
    (0..n)
        .map(|i| {
            let theta = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            let phi2 = (phi1.sin() * delta.cos() + phi1.cos() * delta.sin() * theta.cos()).asin();
            let lambda2 =
                lambda1 + (theta.sin() * delta.sin() * phi1.cos()).atan2(delta.cos() - phi1.sin() * phi2.sin());
            let mut out_lng = lambda2 / rad;
            if out_lng > 180.0 {
                out_lng -= 360.0;
            } else if out_lng < -180.0 {
                out_lng += 360.0;
            }
            [out_lng, (phi2 / rad).clamp(-90.0, 90.0)]
        })
        .collect()
}

/// `cleanOverlayCoords`: the points that are real positions.
pub fn clean_coords(coords: &[[f64; 2]]) -> Vec<[f64; 2]> {
    coords
        .iter()
        .copied()
        .filter(|c| c[0].is_finite() && c[1].is_finite() && c[1].abs() <= 90.0 && c[0].abs() <= 180.0)
        .collect()
}

fn plain(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(TEXT_MAX)
        .collect()
}

/// What a shape is.
#[allow(dead_code)] // every kind of `api.map`, whether or not a bundled plugin draws it
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Line {
        pts: Vec<[f64; 2]>,
        dashed: bool,
    },
    Polygon {
        pts: Vec<[f64; 2]>,
        fill_opacity: f32,
        outline: f32,
        dashed: bool,
    },
    Text {
        at: [f64; 2],
        text: String,
        size: f32,
        dot: bool,
    },
    Button {
        at: [f64; 2],
        text: String,
        title: String,
        size: f32,
        disabled: bool,
    },
    Marker {
        at: [f64; 2],
        icon: Vec<String>,
        text: String,
        title: String,
        size: f32,
        selected: bool,
        draggable: bool,
        clickable: bool,
    },
}

/// One shape and the style every kind shares.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub owner: &'static str,
    pub key: String,
    pub kind: Kind,
    pub color: Color32,
    pub width: f32,
    pub opacity: f32,
    pub visible: bool,
}

/// The style options of a draw call (`overlayStyle`).
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: Color32,
    pub opacity: Option<f32>,
    pub visible: bool,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            color: DEFAULT_COLOR,
            opacity: None,
            visible: true,
        }
    }
}

impl Style {
    pub fn color(color: Color32) -> Style {
        Style {
            color,
            ..Style::default()
        }
    }

    pub fn opacity(mut self, opacity: f32) -> Style {
        self.opacity = Some(opacity.clamp(0.0, 1.0));
        self
    }
}

fn width_of(raw: Option<f32>, fallback: f32) -> f32 {
    match raw {
        Some(w) if w.is_finite() && w > 0.0 => w.min(12.0),
        _ => fallback,
    }
}

/// A plugin's shapes, by key. Drawing a key again replaces whatever it was.
#[derive(Debug, Default)]
pub struct ShapeSet {
    owner: &'static str,
    shapes: Vec<Shape>,
    changed: bool,
}

#[allow(dead_code)] // the whole of `api.map`, whether or not a bundled plugin uses it
impl ShapeSet {
    pub fn new(owner: &'static str) -> ShapeSet {
        ShapeSet {
            owner,
            shapes: Vec::new(),
            changed: true,
        }
    }

    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// Whether anything moved since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    fn put(&mut self, key: &str, kind: Kind, style: Style, width: f32, text_like: bool) -> bool {
        if key.is_empty() || key.len() > 64 {
            return false;
        }
        let shape = Shape {
            owner: self.owner,
            key: key.to_string(),
            kind,
            color: style.color,
            width,
            opacity: style.opacity.unwrap_or(if text_like { 1.0 } else { 0.95 }),
            visible: style.visible,
        };
        self.changed = true;
        match self.shapes.iter_mut().find(|s| s.key == key) {
            Some(slot) => *slot = shape,
            None => self.shapes.push(shape),
        }
        true
    }

    /// `drawLine`: false for fewer than two usable points.
    pub fn line(&mut self, key: &str, coords: &[[f64; 2]], style: Style, width: Option<f32>, dashed: bool) -> bool {
        let pts = clean_coords(coords);
        if pts.len() < 2 {
            return false;
        }
        self.put(key, Kind::Line { pts, dashed }, style, width_of(width, 3.0), false)
    }

    /// `drawPolygon`: the ring closes itself; false for fewer than three corners.
    pub fn polygon(
        &mut self,
        key: &str,
        coords: &[[f64; 2]],
        style: Style,
        fill_opacity: Option<f32>,
        outline: Option<f32>,
        dashed: bool,
    ) -> bool {
        let mut pts = clean_coords(coords);
        if pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        if pts.len() < 3 {
            return false;
        }
        let outline = match outline {
            Some(0.0) => 0.0,
            other => width_of(other, 2.0),
        };
        let fill = fill_opacity
            .filter(|f| f.is_finite())
            .map_or(0.25, |f| f.clamp(0.0, 1.0));
        self.put(
            key,
            Kind::Polygon {
                pts,
                fill_opacity: fill,
                outline,
                dashed,
            },
            style,
            outline,
            false,
        )
    }

    /// `drawCircle`.
    pub fn circle(
        &mut self,
        key: &str,
        center: [f64; 2],
        radius: f64,
        style: Style,
        fill_opacity: Option<f32>,
        outline: Option<f32>,
    ) -> bool {
        let ring = circle_ring(center, radius, None);
        if ring.is_empty() {
            return false;
        }
        self.polygon(key, &ring, style, fill_opacity, outline, false)
    }

    /// `drawText`: plain text, 9 to 32 px (12 by default).
    pub fn text(&mut self, key: &str, at: [f64; 2], text: &str, style: Style, size: Option<f32>, dot: bool) -> bool {
        let text = plain(text);
        if clean_coords(&[at]).is_empty() || text.is_empty() {
            return false;
        }
        let size = size
            .filter(|s| s.is_finite())
            .map_or(12.0, |s| s.round().clamp(9.0, 32.0));
        self.put(key, Kind::Text { at, text, size, dot }, style, 0.0, true)
    }

    /// `drawButton`: a label that takes the pointer; 9 to 32 px (13 by default).
    #[allow(clippy::too_many_arguments)]
    pub fn button(
        &mut self,
        key: &str,
        at: [f64; 2],
        text: &str,
        title: &str,
        style: Style,
        size: Option<f32>,
        disabled: bool,
    ) -> bool {
        let text = plain(text);
        if clean_coords(&[at]).is_empty() || text.is_empty() {
            return false;
        }
        let size = size
            .filter(|s| s.is_finite())
            .map_or(13.0, |s| s.round().clamp(9.0, 32.0));
        let title = title.chars().take(TEXT_MAX).collect();
        self.put(
            key,
            Kind::Button {
                at,
                text,
                title,
                size,
                disabled,
            },
            style,
            0.0,
            true,
        )
    }

    /// `drawMarker`: a round badge with a glyph and a label below; 10 to 48 px
    /// (28 by default). `icon` is a symbol's name, or empty for a plain dot.
    #[allow(clippy::too_many_arguments)]
    pub fn marker(
        &mut self,
        key: &str,
        at: [f64; 2],
        icon: &str,
        label: &str,
        title: &str,
        style: Style,
        size: Option<f32>,
        selected: bool,
        draggable: bool,
        clickable: bool,
    ) -> bool {
        if clean_coords(&[at]).is_empty() {
            return false;
        }
        let size = size
            .filter(|s| s.is_finite())
            .map_or(28.0, |s| s.round().clamp(10.0, 48.0));
        self.put(
            key,
            Kind::Marker {
                at,
                icon: icon_paths(icon)
                    .unwrap_or_default()
                    .iter()
                    .map(|p| p.to_string())
                    .collect(),
                text: plain(label),
                title: title.chars().take(TEXT_MAX).collect(),
                size,
                selected,
                draggable,
                clickable,
            },
            style,
            0.0,
            true,
        )
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.shapes.len();
        self.shapes.retain(|s| s.key != key);
        let gone = self.shapes.len() != before;
        self.changed |= gone;
        gone
    }

    pub fn clear(&mut self) {
        self.changed |= !self.shapes.is_empty();
        self.shapes.clear();
    }

    pub fn has(&self, key: &str) -> bool {
        self.shapes.iter().any(|s| s.key == key)
    }

    pub fn set_visible(&mut self, key: &str, on: bool) -> bool {
        match self.shapes.iter_mut().find(|s| s.key == key) {
            Some(s) => {
                self.changed |= s.visible != on;
                s.visible = on;
                true
            }
            None => false,
        }
    }
}

/// `Corvus.mapIcons`: `(name, label, paths)` of the symbols a marker knows,
/// on a 24 by 24 grid drawn as strokes.
pub const ICONS: &[(&str, &str, &[&str])] = &[
    ("gate", "Gate", &["M5 21V4", "M19 21V4", "M5 4h14", "M9 15h6"]),
    ("tree", "Tree", &["M12 22v-7", "M12 3c-3 0-5.5 2.2-5.5 5 0 .6.1 1.1.3 1.6A4 4 0 0 0 7 17h10a4 4 0 0 0 .2-7.4c.2-.5.3-1 .3-1.6 0-2.8-2.5-5-5.5-5z"]),
    ("radar", "Radar", &["M12 12l6-6", "M12 11a1 1 0 1 0 0 2 1 1 0 0 0 0-2z", "M7.8 16.2a6 6 0 1 1 8.4 0", "M4.9 19.1a10 10 0 1 1 14.2 0"]),
    ("camera", "Camera", &["M4 8h3l2-3h6l2 3h3v11H4z", "M12 11a3 3 0 1 0 0 6 3 3 0 0 0 0-6z"]),
    ("antenna", "Antenna", &["M12 12v9", "M8 21h8", "M12 8a1 1 0 1 0 0 2 1 1 0 0 0 0-2z", "M7.8 7.8a6 6 0 0 0 0 8.4", "M16.2 7.8a6 6 0 0 1 0 8.4"]),
    ("person", "Person", &["M12 4a3 3 0 1 0 0 6 3 3 0 0 0 0-6z", "M5 21v-1a5 5 0 0 1 5-5h4a5 5 0 0 1 5 5v1"]),
    ("vehicle", "Vehicle", &["M3 15v-4l2-5h14l2 5v4z", "M6 15v3", "M18 15v3", "M3 11h18"]),
    ("building", "Building", &["M5 21V4h9v17", "M14 9h5v12", "M3 21h18", "M8 8h3", "M8 12h3", "M8 16h3"]),
    ("explosion", "Explosion", &["M12 2l2 6 6-2-3 5.5 5 3.5-6 .5 1 6-5-4-5 4 1-6-6-.5 5-3.5L4 6l6 2z"]),
    ("fire", "Fire", &["M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.07-2.14-.22-4.05 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.15.43-2.29 1-3a2.5 2.5 0 0 0 2.5 2.5z"]),
    ("hazard", "Hazard", &["M12 3L2 20h20z", "M12 10v4", "M12 17v.5"]),
    ("target", "Target", &["M12 4a8 8 0 1 0 0 16 8 8 0 0 0 0-16z", "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z", "M12 1v4", "M12 19v4", "M1 12h4", "M19 12h4"]),
    ("pad", "Landing pad", &["M4 4h16v16H4z", "M9 8v8", "M15 8v8", "M9 12h6"]),
    ("flag", "Flag", &["M5 21V4", "M5 4h12l-2 4 2 4H5"]),
    ("pin", "Pin", &["M12 21s-7-6.2-7-11a7 7 0 0 1 14 0c0 4.8-7 11-7 11z", "M12 7.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z"]),
    ("home", "Home", &["M3 11l9-8 9 8", "M5 10v11h14V10", "M10 21v-6h4v6"]),
    ("drone", "Drone", &["M12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4z", "M10.5 10.5L6 6", "M13.5 10.5L18 6", "M10.5 13.5L6 18", "M13.5 13.5L18 18", "M4 4.5h4", "M16 4.5h4", "M4 19.5h4", "M16 19.5h4"]),
    ("boat", "Boat", &["M2 17h20l-3 4H5z", "M12 3v12", "M12 4l6 9h-6"]),
    ("medical", "Medical", &["M10 3h4v7h7v4h-7v7h-4v-7H3v-4h7z"]),
    ("power", "Power", &["M13 2L4 14h7l-1 8 9-12h-7z"]),
    ("water", "Water", &["M12 3s6 6.5 6 11a6 6 0 0 1-12 0c0-4.5 6-11 6-11z"]),
];

/// The path strings of a named symbol.
pub fn icon_paths(name: &str) -> Option<&'static [&'static str]> {
    ICONS.iter().find(|i| i.0 == name).map(|i| i.2)
}

fn icon_svg(paths: &[String], stroke: f32) -> Vec<u8> {
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"white\" stroke-width=\"{stroke}\" stroke-linecap=\"round\" stroke-linejoin=\"round\">",
    );
    for p in paths {
        s.push_str(&format!("<path d=\"{p}\"/>"));
    }
    s.push_str("</svg>");
    s.into_bytes()
}

/// Where a shape is on the screen: its centre, or `None` when it is behind
/// the camera.
pub type Project<'a> = &'a dyn Fn([f64; 2]) -> Option<Pos2>;

fn faded(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a.clamp(0.0, 1.0))
}

fn bg() -> Color32 {
    style::bg()
}

/// Lines and areas, which lie under the flown track.
pub fn paint_areas(painter: &egui::Painter, project: Project<'_>, shapes: &[Shape]) {
    for s in shapes.iter().filter(|s| s.visible) {
        match &s.kind {
            Kind::Line { pts, dashed } => {
                let screen: Vec<Pos2> = pts.iter().filter_map(|p| project(*p)).collect();
                if screen.len() == pts.len() {
                    stroke_line(painter, &screen, s, *dashed, s.width);
                }
            }
            Kind::Polygon {
                pts,
                fill_opacity,
                outline,
                dashed,
            } => {
                let screen: Vec<Pos2> = pts.iter().filter_map(|p| project(*p)).collect();
                if screen.len() != pts.len() {
                    continue;
                }
                if *fill_opacity > 0.0 {
                    let mut mesh = egui::Mesh::default();
                    let c = faded(s.color, *fill_opacity);
                    for p in &screen {
                        mesh.colored_vertex(*p, c);
                    }
                    for t in crate::ui::polygon::triangulate(&screen) {
                        mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
                    }
                    painter.add(mesh);
                }
                if *outline > 0.0 {
                    let mut closed = screen.clone();
                    closed.push(screen[0]);
                    stroke_line(painter, &closed, s, *dashed, *outline);
                }
            }
            _ => {}
        }
    }
}

fn stroke_line(painter: &egui::Painter, pts: &[Pos2], s: &Shape, dashed: bool, width: f32) {
    let casing = Stroke::new(width + 2.4, faded(bg(), 0.45 * s.opacity));
    let line = Stroke::new(width, faded(s.color, s.opacity));
    painter.add(Paint::line(pts.to_vec(), casing));
    if dashed {
        painter.extend(Paint::dashed_line(pts, line, 2.0 * width, 1.5 * width));
    } else {
        painter.add(Paint::line(pts.to_vec(), line));
    }
}

/// The size a shape takes, for the pointer.
pub fn button_rect(ui: &egui::Ui, at: Pos2, text: &str, size: f32) -> Rect {
    let g = style::galley(ui, text, size, Weight::W600, Color32::WHITE, 0.0);
    Rect::from_center_size(
        at,
        Vec2::new(g.size().x + 24.0 + 3.0, g.size().y.max(size * 1.2) + 12.0 + 3.0),
    )
}

pub fn marker_rect(at: Pos2, size: f32) -> Rect {
    Rect::from_center_size(at, Vec2::splat(size))
}

/// Text, markers and buttons, over everything. `hot` is the key under the
/// pointer, `disabled_clickable` is not needed: a disabled button just dims.
pub fn paint_marks(ui: &egui::Ui, painter: &egui::Painter, project: Project<'_>, shapes: &[Shape], hot: Option<&str>) {
    for s in shapes.iter().filter(|s| s.visible) {
        match &s.kind {
            Kind::Text { at, text, size, dot } => {
                let Some(c) = project(*at) else { continue };
                paint_label(ui, painter, c, text, *size, *dot, s);
            }
            Kind::Button {
                at,
                text,
                size,
                disabled,
                ..
            } => {
                let Some(c) = project(*at) else { continue };
                let r = button_rect(ui, c, text, *size);
                let on = hot == Some(s.key.as_str()) && !disabled;
                let op = if *disabled { 0.45 } else { s.opacity };
                painter.rect_filled(
                    r.translate(Vec2::new(0.0, 2.0)).expand(1.0),
                    8,
                    Color32::from_black_alpha(70),
                );
                let fill = if on {
                    lerp(bg(), s.color, 0.22)
                } else {
                    faded(bg(), 0.88)
                };
                painter.rect_filled(r, 8, faded(fill, op));
                painter.rect_stroke(r, 8, Stroke::new(1.5, faded(s.color, op)), StrokeKind::Inside);
                let g = style::galley(ui, text, *size, Weight::W600, faded(s.color, op), 0.0);
                painter.galley(r.center() - g.size() / 2.0, g, s.color);
            }
            Kind::Marker {
                at,
                icon,
                text,
                size,
                selected,
                ..
            } => {
                let Some(c) = project(*at) else { continue };
                paint_marker(
                    ui,
                    painter,
                    c,
                    icon,
                    text,
                    *size,
                    *selected,
                    hot == Some(s.key.as_str()),
                    s,
                );
            }
            _ => {}
        }
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

#[allow(clippy::too_many_arguments)]
fn halo(
    ui: &egui::Ui,
    painter: &egui::Painter,
    at: Pos2,
    text: &str,
    size: f32,
    weight: Weight,
    color: Color32,
    halo: Color32,
) {
    let g = style::galley(ui, text, size, weight, color, 0.0);
    let origin = at - Vec2::new(0.0, 0.0);
    for (dx, dy) in [
        (-1.0, 0.0),
        (1.0, 0.0),
        (0.0, -1.0),
        (0.0, 1.0),
        (-1.0, 1.0),
        (1.0, 1.0),
    ] {
        painter.galley_with_override_text_color(origin + Vec2::new(dx, dy), g.clone(), halo);
    }
    painter.galley_with_override_text_color(origin, g, color);
}

fn paint_label(ui: &egui::Ui, painter: &egui::Painter, c: Pos2, text: &str, size: f32, dot: bool, s: &Shape) {
    let g = style::galley(ui, text, size, Weight::W600, s.color, 0.0);
    let dot_w = if dot { 10.0 + 6.0 } else { 0.0 };
    let total = g.size().x + dot_w;
    let x0 = c.x - total / 2.0;
    let col = faded(s.color, s.opacity);
    if dot {
        let dc = Pos2::new(x0 + 5.0, c.y);
        painter.circle_filled(dc, 7.0, bg());
        painter.circle_filled(dc, 5.0, col);
    }
    halo(
        ui,
        painter,
        Pos2::new(x0 + dot_w, c.y - g.size().y / 2.0),
        text,
        size,
        Weight::W600,
        col,
        bg(),
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_marker(
    ui: &egui::Ui,
    painter: &egui::Painter,
    c: Pos2,
    icon: &[String],
    label: &str,
    size: f32,
    selected: bool,
    hot: bool,
    s: &Shape,
) {
    let ink = Color32::from_rgb(11, 14, 18);
    let r = size / 2.0 * if hot { 1.1 } else { 1.0 };
    let col = faded(s.color, s.opacity);
    let white = Color32::from_white_alpha((0.85 * 255.0 * s.opacity) as u8);
    painter.circle_filled(c + Vec2::new(0.0, 2.0), r + 1.5, Color32::from_black_alpha(80));
    if icon.is_empty() {
        if selected {
            painter.circle_filled(c, r + 6.5, faded(ink, 0.95 * s.opacity));
            painter.circle_filled(c, r + 4.5, white);
            painter.circle_filled(c, r + 2.0, col);
        }
        painter.circle_filled(c, r + 2.0, Color32::from_white_alpha((0.95 * 255.0 * s.opacity) as u8));
        painter.circle_filled(c, r, faded(ink, 0.95 * s.opacity));
        painter.circle_filled(c, r - 3.0, col);
    } else {
        if selected {
            painter.circle_filled(c, r + 6.5, white);
            painter.circle_filled(c, r + 4.5, col);
            painter.circle_filled(c, r + 2.0, faded(ink, 0.95 * s.opacity));
        }
        painter.circle_filled(c, r + 1.5, white);
        painter.circle_filled(c, r, faded(ink, 0.78 * s.opacity));
        painter.circle_stroke(c, r - 1.0, Stroke::new(2.0, col));
        let side = (r * 2.0) * 0.62;
        paint_paths(ui, icon, Rect::from_center_size(c, Vec2::splat(side)), col, 2.2);
    }
    if !label.is_empty() {
        let g = style::galley(ui, label, 12.0, Weight::W700, Color32::WHITE, 0.0);
        let at = Pos2::new(c.x - g.size().x / 2.0, c.y + size / 2.0 + 5.0);
        halo(
            ui,
            painter,
            at,
            label,
            12.0,
            Weight::W700,
            Color32::WHITE,
            Color32::BLACK,
        );
    }
}

/// A named symbol drawn as strokes on a 24 grid into `rect` (`glyph` of
/// `map-entities.js`, the marker's own glyph).
pub fn paint_glyph(ui: &egui::Ui, name: &str, rect: Rect, color: Color32, stroke: f32) {
    if let Some(p) = icon_paths(name) {
        let paths: Vec<String> = p.iter().map(|s| s.to_string()).collect();
        paint_paths(ui, &paths, rect, color, stroke);
    }
}

fn paint_paths(ui: &egui::Ui, paths: &[String], rect: Rect, color: Color32, stroke: f32) {
    let key = paths.join("|").replace(['/', ' '], "_");
    let uri = format!("bytes://plugin-icon/{stroke}/{key}.svg");
    egui::Image::from_bytes(uri, icon_svg(paths, stroke))
        .tint(color)
        .paint_at(ui, rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_circle_has_the_requested_corners_at_the_requested_distance() {
        let ring = circle_ring([11.5, 48.1], 100.0, None);
        assert_eq!(ring.len(), 72);
        let d = |p: [f64; 2]| {
            let k = 111_194.926_6;
            let dx = (p[0] - 11.5) * k * 48.1f64.to_radians().cos();
            let dy = (p[1] - 48.1) * k;
            (dx * dx + dy * dy).sqrt()
        };
        for p in &ring {
            assert!((d(*p) - 100.0).abs() < 0.2, "{}", d(*p));
        }
        assert_eq!(circle_ring([11.5, 48.1], 100.0, Some(3)).len(), 8);
        assert_eq!(circle_ring([11.5, 48.1], 100.0, Some(999)).len(), 256);
    }

    #[test]
    fn a_circle_needs_a_real_centre_and_a_radius_up_to_a_thousand_kilometres() {
        assert!(circle_ring([200.0, 0.0], 10.0, None).is_empty());
        assert!(circle_ring([0.0, 91.0], 10.0, None).is_empty());
        assert!(circle_ring([0.0, 0.0], 0.0, None).is_empty());
        assert!(circle_ring([0.0, 0.0], 1_000_001.0, None).is_empty());
        assert!(circle_ring([f64::NAN, 0.0], 10.0, None).is_empty());
        assert_eq!(circle_ring([0.0, 0.0], 1_000_000.0, None).len(), 72);
    }

    #[test]
    fn a_circle_matches_what_the_web_helper_answered() {
        let want = [
            [11.6, 48.10224830090932],
            [11.60238059371483, 48.10158976423638],
            [11.603366563803245, 48.09999995083644],
            [11.602380446481606, 48.09841018660007],
            [11.6, 48.0977516990907],
            [11.597619553518394, 48.09841018660007],
            [11.596633436196754, 48.09999995083644],
            [11.59761940628517, 48.10158976423638],
        ];
        let got = circle_ring([11.6, 48.1], 250.0, Some(8));
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            assert!((g[0] - w[0]).abs() < 1e-9 && (g[1] - w[1]).abs() < 1e-9, "{g:?} {w:?}");
        }
    }

    #[test]
    fn the_first_corner_of_a_circle_lies_due_north() {
        let ring = circle_ring([10.0, 50.0], 1000.0, None);
        assert!((ring[0][0] - 10.0).abs() < 1e-9);
        assert!(ring[0][1] > 50.0);
    }

    #[test]
    fn unusable_points_are_dropped() {
        let c = clean_coords(&[[1.0, 2.0], [181.0, 0.0], [0.0, 91.0], [f64::NAN, 0.0], [-180.0, -90.0]]);
        assert_eq!(c, vec![[1.0, 2.0], [-180.0, -90.0]]);
    }

    #[test]
    fn a_line_needs_two_points_and_an_area_three() {
        let mut s = ShapeSet::new("t");
        assert!(!s.line("l", &[[1.0, 1.0]], Style::default(), None, false));
        assert!(s.line("l", &[[1.0, 1.0], [2.0, 2.0]], Style::default(), None, false));
        assert!(!s.polygon("p", &[[1.0, 1.0], [2.0, 2.0]], Style::default(), None, None, false));
        assert!(s.polygon(
            "p",
            &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]],
            Style::default(),
            None,
            None,
            false
        ));
        match &s.shapes()[1].kind {
            Kind::Polygon {
                pts,
                fill_opacity,
                outline,
                ..
            } => {
                assert_eq!(pts.len(), 3, "the closing point is dropped");
                assert_eq!(*fill_opacity, 0.25);
                assert_eq!(*outline, 2.0);
            }
            _ => panic!("a polygon"),
        }
    }

    #[test]
    fn a_key_drawn_again_replaces_what_it_was() {
        let mut s = ShapeSet::new("t");
        s.line("k", &[[0.0, 0.0], [1.0, 1.0]], Style::default(), None, false);
        s.text("k", [0.0, 0.0], "hi", Style::default(), None, false);
        assert_eq!(s.shapes().len(), 1);
        assert!(matches!(s.shapes()[0].kind, Kind::Text { .. }));
        assert!(s.remove("k"));
        assert!(!s.remove("k"));
    }

    #[test]
    fn widths_sizes_and_opacities_follow_the_web_defaults_and_bounds() {
        let mut s = ShapeSet::new("t");
        s.line("a", &[[0.0, 0.0], [1.0, 1.0]], Style::default(), Some(40.0), false);
        s.text("b", [0.0, 0.0], "x", Style::default(), Some(99.0), false);
        s.text("c", [0.0, 0.0], "x", Style::default(), Some(2.0), false);
        s.button("d", [0.0, 0.0], "x", "", Style::default(), None, false);
        s.marker(
            "e",
            [0.0, 0.0],
            "tree",
            "",
            "",
            Style::default(),
            Some(80.0),
            false,
            false,
            false,
        );
        let get = |k: &str| s.shapes().iter().find(|x| x.key == k).unwrap().clone();
        assert_eq!(get("a").width, 12.0);
        assert_eq!(get("a").opacity, 0.95);
        assert!(matches!(get("b").kind, Kind::Text { size, .. } if size == 32.0));
        assert!(matches!(get("c").kind, Kind::Text { size, .. } if size == 9.0));
        assert!(matches!(get("d").kind, Kind::Button { size, .. } if size == 13.0));
        assert!(matches!(get("e").kind, Kind::Marker { size, .. } if size == 48.0));
        assert_eq!(get("b").opacity, 1.0, "text is read, not seen through");
    }

    #[test]
    fn text_is_plain_one_line_and_cut_at_two_hundred() {
        let mut s = ShapeSet::new("t");
        s.text(
            "t",
            [0.0, 0.0],
            &format!("a\n  b {}", "x".repeat(300)),
            Style::default(),
            None,
            false,
        );
        match &s.shapes()[0].kind {
            Kind::Text { text, .. } => {
                assert!(text.starts_with("a b x"));
                assert_eq!(text.chars().count(), 200);
            }
            _ => panic!(),
        }
        assert!(!s.text("e", [0.0, 0.0], "  \n ", Style::default(), None, false));
    }

    #[test]
    fn every_symbol_has_paths_and_a_name_the_web_uses() {
        assert_eq!(ICONS.len(), 21);
        assert!(icon_paths("tree").is_some());
        assert!(icon_paths("nope").is_none());
        assert_eq!(hex("#7BD389"), Some(Color32::from_rgb(0x7B, 0xD3, 0x89)));
        assert_eq!(hex("7BD389"), None);
    }
}
