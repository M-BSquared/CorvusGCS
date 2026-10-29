//! The plan drawn on the map and the pointer on it (`drawMarkers`,
//! `drawRoute`, `drawOrbits`, `drawPattern`, the handles and `onMapClick` of
//! `mission.js`, the `.mission-point`, `.mission-home` and handle rules of
//! `mission.css`). It is an `Overlay` of the shared map, so tiles, layers,
//! downloaded areas, search and the offline dialog are the Home map's own.

use eframe::egui::{self, Color32, Id, Pos2, Rect, Shape as Paint, Stroke, Vec2};
use walkers::Projector;

use crate::map::Overlay;
use crate::mission::geometry::{self, circle_ring, distance_ll, ring_radius, stations};
use crate::mission::plan::{Item, Kind, Plan, Shape, Tool, RADIUS_MAX_M, RADIUS_MIN_M};
use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// `--plan`, `--plan-ink` and `--plan-select`: one amber for every theme.
pub const PLAN: Color32 = Color32::from_rgb(0xFF, 0xC2, 0x1A);
pub const PLAN_INK: Color32 = Color32::from_black_alpha(178);
pub const PLAN_SELECT: Color32 = Color32::WHITE;

const PLAN_LINE_W: f32 = 3.2;
const PLAN_CASING_W: f32 = PLAN_LINE_W + 3.2;
const ORBIT_LINE_W: f32 = 2.6;
const ORBIT_CASING_W: f32 = ORBIT_LINE_W + 2.8;
const WIDTH_ZOOM_NEAR: f64 = 17.0;
const WIDTH_ZOOM_FAR: f64 = 9.0;
const WIDTH_FAR_FACTOR: f32 = 1.75;
const POINT_SCALE_FAR: f64 = 1.45;

/// How much wider a line is drawn at `zoom` than its quoted width.
pub fn width_factor(zoom: f64) -> f32 {
    let held = zoom.clamp(WIDTH_ZOOM_FAR, WIDTH_ZOOM_NEAR);
    let t = ((held - WIDTH_ZOOM_FAR) / (WIDTH_ZOOM_NEAR - WIDTH_ZOOM_FAR)) as f32;
    WIDTH_FAR_FACTOR + (1.0 - WIDTH_FAR_FACTOR) * t
}

/// `pointScaleFor`: how much bigger than its quoted size a marker is at `zoom`.
pub fn point_scale(zoom: f64) -> f32 {
    if !zoom.is_finite() {
        return 1.0;
    }
    let held = zoom.clamp(WIDTH_ZOOM_FAR, WIDTH_ZOOM_NEAR);
    let out = (WIDTH_ZOOM_NEAR - held) / (WIDTH_ZOOM_NEAR - WIDTH_ZOOM_FAR);
    ((1.0 + out * (POINT_SCALE_FAR - 1.0)) * 1000.0).round() as f32 / 1000.0
}

/// The shape being drawn for an armed pattern.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// `[lon, lat]` clicked so far.
    pub pts: Vec<[f64; 2]>,
    pub cursor: Option<[f64; 2]>,
    pub spacing: f64,
    pub width: f64,
}

/// What the pointer did on the map this frame, for the page to apply.
#[derive(Default)]
pub struct Out {
    pub clicked: Option<[f64; 2]>,
    pub double_clicked: bool,
    pub secondary: Option<[f64; 2]>,
    pub hover: Option<[f64; 2]>,
    pub select: Option<u32>,
    pub moved: Option<(u32, [f64; 2])>,
    pub dropped: bool,
    pub remove: Option<u32>,
    pub start_moved: Option<[f64; 2]>,
    pub start_dropped: bool,
    pub start_remove: bool,
    pub radius: Option<(u32, f64)>,
    pub radius_dropped: bool,
    pub pattern_grab: Option<[f64; 2]>,
    pub pattern_to: Option<[f64; 2]>,
    pub pattern_dropped: bool,
}

pub struct PlanLayer<'a> {
    pub plan: &'a Plan,
    pub tool: Tool,
    pub draft: &'a Draft,
    pub leg: Option<([f64; 2], [f64; 2])>,
    pub current: Option<usize>,
    pub done_before: usize,
    pub out: &'a mut Out,
}

fn screen(p: &Projector, c: [f64; 2]) -> Pos2 {
    p.project(walkers::lon_lat(c[0], c[1]))
        .to_pos2()
        .clamp(Pos2::new(-1e6, -1e6), Pos2::new(1e6, 1e6))
}

fn lonlat(p: &Projector, at: Pos2) -> [f64; 2] {
    let c = p.unproject(at.to_vec2());
    [c.x(), c.y()]
}

fn with(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a)
}

fn dashed(painter: &egui::Painter, pts: &[Pos2], w: f32, color: Color32) {
    if pts.len() >= 2 {
        // line-dasharray [3, 2] is measured in line widths
        painter.extend(Paint::dashed_line(pts, Stroke::new(w, color), 3.0 * w, 2.0 * w));
    }
}

/// A marker with a hit box: what happened to it this frame.
#[derive(Default)]
struct Grab {
    clicked: bool,
    started: bool,
    stopped: bool,
    secondary: bool,
    moved: Option<[f64; 2]>,
}

fn grab(ui: &mut egui::Ui, id: Id, centre: Pos2, size: f32, projector: &Projector) -> Grab {
    let r = Rect::from_center_size(centre, Vec2::splat(size));
    let h = ui.interact(r, id, egui::Sense::click_and_drag());
    let mut g = Grab::default();
    let key = id.with("grab");
    if h.hovered() && !h.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if h.drag_started() {
        if let Some(q) = h.interact_pointer_pos() {
            ui.ctx().data_mut(|d| d.insert_temp(key, centre - q));
        }
        g.started = true;
    }
    if h.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(q) = h.interact_pointer_pos() {
            let off: Vec2 = ui.ctx().data(|d| d.get_temp(key)).unwrap_or(Vec2::ZERO);
            g.moved = Some(lonlat(projector, q + off));
        }
    }
    g.stopped = h.drag_stopped();
    g.clicked = h.clicked();
    g.secondary = h.secondary_clicked();
    g
}

impl PlanLayer<'_> {
    fn route(&self, painter: &egui::Painter, p: &Projector, f: f32) {
        let pts: Vec<Pos2> = stations(&self.plan.items, self.plan.home.as_ref())
            .iter()
            .map(|s| screen(p, [s.lon, s.lat]))
            .collect();
        if pts.len() < 2 {
            return;
        }
        painter.add(Paint::line(pts.clone(), Stroke::new(PLAN_CASING_W * f, PLAN_INK)));
        dashed(painter, &pts, PLAN_LINE_W * f, PLAN);
    }

    fn orbits(&self, painter: &egui::Painter, p: &Projector, f: f32) {
        for item in self.plan.items.iter().filter(|i| self.plan.ring_flown(i)) {
            let ring = circle_ring(item.lon, item.lat, ring_radius(item), 64);
            let pts: Vec<Pos2> = ring.iter().map(|c| screen(p, *c)).collect();
            painter.add(Paint::convex_polygon(pts.clone(), with(PLAN, 0.24), Stroke::NONE));
            painter.add(Paint::line(pts.clone(), Stroke::new(ORBIT_CASING_W * f, PLAN_INK)));
            dashed(painter, &pts, ORBIT_LINE_W * f, PLAN);
        }
    }

    fn leg(&self, painter: &egui::Painter, p: &Projector, f: f32) {
        if let Some((a, b)) = self.leg {
            painter.line_segment(
                [screen(p, a), screen(p, b)],
                Stroke::new((PLAN_LINE_W + 1.0) * f, style::healthy()),
            );
        }
    }

    fn draft(&self, painter: &egui::Painter, p: &Projector) {
        let Tool::Shape(shape) = self.tool else { return };
        let d = self.draft;
        if d.pts.is_empty() {
            return;
        }
        let mut pts = d.pts.clone();
        pts.extend(d.cursor);
        let px: Vec<Pos2> = pts.iter().map(|c| screen(p, *c)).collect();
        let outline = Stroke::new(2.0, PLAN);
        if shape == Shape::Circle && pts.len() >= 2 {
            let r = geometry::distance_ll(pts[0], pts[1]).max(geometry::PATTERN_CIRCLE_MIN_M);
            let ring: Vec<Pos2> = circle_ring(pts[0][0], pts[0][1], r, geometry::PATTERN_CIRCLE_SEGMENTS)
                .iter()
                .map(|c| screen(p, *c))
                .collect();
            painter.extend(Paint::dashed_line(&ring, outline, 4.0, 4.0));
        } else if shape == Shape::Area && px.len() >= 3 {
            let mut closed = px.clone();
            closed.push(px[0]);
            painter.extend(Paint::dashed_line(&closed, outline, 4.0, 4.0));
        } else if px.len() >= 2 {
            painter.extend(Paint::dashed_line(&px, outline, 4.0, 4.0));
        }
        if shape != Shape::Circle {
            let distinct = distinct_points(&pts);
            if distinct.len() >= 2 {
                let sweep = geometry::pattern_waypoints(shape, &distinct, d.spacing, d.width);
                if sweep.len() > 1 {
                    let line: Vec<Pos2> = sweep.iter().map(|c| screen(p, *c)).collect();
                    painter.add(Paint::line(line, Stroke::new(1.6, with(PLAN, 0.75))));
                }
            }
        }
        for c in &d.pts {
            let at = screen(p, *c);
            painter.circle_filled(at, 4.5 + 0.75, PLAN_INK);
            painter.circle_filled(at, 4.5 - 0.75, PLAN);
        }
    }
}

/// The clicked points with those closer than half a metre to the one before
/// left out (`distinctPoints`).
pub fn distinct_points(pts: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = Vec::new();
    for p in pts {
        if out.last().is_none_or(|l| distance_ll(*l, *p) > 0.5) {
            out.push(*p);
        }
    }
    out
}

/// The middle of the box around a group of points, `[lon, lat]`.
pub fn pattern_centre(group: &[&Item]) -> [f64; 2] {
    let (mut s, mut n, mut w, mut e) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
    for i in group {
        s = s.min(i.lat);
        n = n.max(i.lat);
        w = w.min(i.lon);
        e = e.max(i.lon);
    }
    [(w + e) / 2.0, (s + n) / 2.0]
}

struct Look {
    scale: f32,
    selected: bool,
    current: bool,
    done: bool,
}

/// One planned point: the numbered amber ring, with the kind's badge.
fn paint_point(ui: &egui::Ui, c: Pos2, number: usize, item: &Item, look: &Look) {
    let painter = ui.painter();
    let s = look.scale
        * if look.selected {
            1.18
        } else if look.current {
            1.12
        } else {
            1.0
        };
    let a = if look.done { 0.55 } else { 1.0 };
    let r = 14.0 * s;
    let border = 1.5 * s;
    // 0 0 12px plan 70%
    painter.circle_filled(c, r + 6.0 * s, with(PLAN, 0.10 * a));
    painter.circle_filled(c, r + 3.0 * s, with(PLAN, 0.14 * a));
    if look.selected {
        painter.circle_filled(c, r + 4.5 * s, with(PLAN_INK, a));
        painter.circle_filled(c, r + 3.5 * s, with(PLAN_SELECT, a));
        painter.circle_filled(c, r + 1.5 * s, with(PLAN_INK, a));
    } else if look.current {
        painter.circle_filled(c, r + 4.0 * s, with(style::healthy(), 0.8 * a));
        painter.circle_filled(c, r + 1.5 * s, with(PLAN_INK, a));
    } else {
        painter.circle_filled(c, r + border, with(PLAN_INK, a));
    }
    painter.circle_filled(c, r, with(if look.selected { PLAN } else { with(PLAN, 0.4) }, a));
    painter.circle_stroke(c, r - border / 2.0, Stroke::new(border, with(PLAN, a)));
    let ink = look.selected;
    let g = style::galley(
        ui,
        &number.to_string(),
        12.0 * s,
        Weight::W700,
        if ink {
            with(PLAN_INK, a)
        } else {
            with(Color32::WHITE, a)
        },
        0.0,
    );
    painter.galley(c - g.size() / 2.0, g, Color32::WHITE);
    if item.kind != Kind::Waypoint {
        let bc = c + Vec2::new(10.0, -10.0) * s;
        painter.circle_filled(bc, 7.0 * s, with(PLAN_INK, a));
        painter.circle_stroke(bc, 6.5 * s, Stroke::new(1.0, with(PLAN, a)));
        icons::paint(
            ui,
            item.kind.icon(),
            Rect::from_center_size(bc, Vec2::splat(9.0 * s)),
            with(PLAN, a),
        );
    }
    if !item.name.is_empty() {
        let g = style::galley(ui, &item.name, 11.0 * look.scale, Weight::W600, Color32::WHITE, 0.0);
        let pad = Vec2::new(6.0, 1.0);
        let size = g.size() + pad * 2.0 + Vec2::splat(2.0);
        let left = c.x + 14.0 * look.scale + 6.0 * look.scale;
        let rect = Rect::from_min_size(Pos2::new(left, c.y - size.y / 2.0), size);
        painter.rect_filled(rect, 4, with(PLAN_INK, 0.8));
        painter.rect_stroke(rect, 4, Stroke::new(1.0, with(PLAN, 0.55)), egui::StrokeKind::Inside);
        painter.galley(rect.min + pad + Vec2::splat(1.0), g, Color32::WHITE);
    }
}

/// The start: the landing pad mark, or with the takeoff on it that point's
/// amber ring with the H inside and the number pinned to its corner.
fn paint_start(ui: &egui::Ui, c: Pos2, takeoff: Option<(usize, bool, bool)>, following: bool, scale: f32) {
    let painter = ui.painter();
    let Some((number, selected, done)) = takeoff else {
        crate::map::paint_home(painter, c);
        if following {
            painter.circle_stroke(c, 18.0, Stroke::new(1.5, style::accent()));
        }
        return;
    };
    let s = scale * if selected { 1.18 } else { 1.0 };
    let a = if done { 0.55 } else { 1.0 };
    let r = 14.0 * s;
    painter.circle_filled(c, r + 6.0 * s, with(PLAN, 0.10 * a));
    if selected {
        painter.circle_filled(c, r + 4.5 * s, with(PLAN_INK, a));
        painter.circle_filled(c, r + 3.5 * s, with(PLAN_SELECT, a));
    }
    painter.circle_filled(c, r + 1.5 * s, with(PLAN_INK, a));
    painter.circle_filled(c, r, with(if selected { PLAN } else { with(PLAN, 0.4) }, a));
    painter.circle_stroke(c, r - 0.75 * s, Stroke::new(1.5 * s, with(PLAN, a)));
    let at = |x: f32, y: f32| c + Vec2::new(x - 16.0, y - 16.0) * (26.0 / 32.0) * s;
    let glyph = Stroke::new(2.0 * s, with(Color32::WHITE, a));
    for (p, q) in [
        ((12.4, 11.6), (12.4, 20.4)),
        ((19.6, 11.6), (19.6, 20.4)),
        ((12.4, 16.0), (19.6, 16.0)),
    ] {
        painter.line_segment([at(p.0, p.1), at(q.0, q.1)], glyph);
    }
    let nc = c + Vec2::new(r + 1.0 * s - 9.0 * s + 8.0 * s, -(r + 1.0 * s - 9.0 * s + 8.0 * s));
    let nr = 9.0 * s;
    painter.circle_filled(nc, nr + 1.5, with(PLAN_INK, a));
    // color-mix(in srgb, var(--plan) 85%, #000)
    painter.circle_filled(nc, nr, Color32::from_rgb(217, 165, 22));
    painter.circle_stroke(nc, nr - 0.75, Stroke::new(1.5, with(PLAN, a)));
    let g = style::galley(ui, &number.to_string(), 10.0 * s, Weight::W700, Color32::WHITE, 0.0);
    painter.galley(nc - g.size() / 2.0, g, Color32::WHITE);
}

impl Overlay for PlanLayer<'_> {
    fn takes_double_click(&self) -> bool {
        matches!(self.tool, Tool::Shape(_))
    }

    fn run(&mut self, ui: &mut egui::Ui, response: &egui::Response, projector: &Projector, zoom: f64) {
        let painter = ui.painter_at(response.rect);
        let f = width_factor(zoom);
        let scale = point_scale(zoom);
        self.leg(&painter, projector, f);
        self.route(&painter, projector, f);
        self.orbits(&painter, projector, f);
        self.draft(&painter, projector);

        let plan = self.plan;
        let tied = plan.start_takeoff().map(|t| t.id);
        let base = response.id.with("plan-layer");
        let mut selected_draw: Option<(usize, Pos2, usize)> = None;
        let mut number = 0;
        let mut order: Vec<(usize, Pos2, usize)> = Vec::new();
        for (index, item) in plan.items.iter().enumerate() {
            if !item.kind.position() {
                continue;
            }
            number += 1;
            if Some(item.id) == tied {
                continue;
            }
            let c = screen(projector, [item.lon, item.lat]);
            if Some(item.id) == plan.selected {
                selected_draw = Some((index, c, number));
            } else {
                order.push((index, c, number));
            }
        }
        order.extend(selected_draw);

        // The start, under the points.
        if let Some(h) = plan.home {
            let c = screen(projector, [h.lon, h.lat]);
            let takeoff = tied.map(|id| {
                let i = plan.index_of(id).unwrap_or(0);
                (plan.position_number(i), plan.selected == Some(id), i < self.done_before)
            });
            paint_start(ui, c, takeoff, plan.start_at_vehicle, scale);
            let g = grab(ui, base.with("start"), c, 28.0 * scale, projector);
            if g.clicked || g.started {
                self.out.select = tied;
            }
            if let Some(at) = g.moved {
                self.out.start_moved = Some(at);
            }
            if g.stopped {
                self.out.start_dropped = true;
            }
            if g.secondary {
                self.out.start_remove = true;
            }
        }

        for (index, c, number) in order {
            let item = &plan.items[index];
            let look = Look {
                scale,
                selected: plan.selected == Some(item.id),
                current: self.current == Some(index),
                done: index < self.done_before,
            };
            paint_point(ui, c, number, item, &look);
            let g = grab(ui, base.with(("item", item.id)), c, 28.0 * scale, projector);
            if g.clicked || g.started {
                self.out.select = Some(item.id);
            }
            if let Some(at) = g.moved {
                self.out.moved = Some((item.id, at));
            }
            if g.stopped {
                self.out.dropped = true;
            }
            if g.secondary {
                self.out.remove = Some(item.id);
            }
        }

        // The selected orbit's radius grip, due east on its ring.
        if let Some(item) = plan.selected_item().filter(|i| plan.ring_flown(i)) {
            let ring = circle_ring(item.lon, item.lat, ring_radius(item), 4);
            let c = screen(projector, ring[0]);
            let s = 13.0 * scale;
            let r = Rect::from_center_size(c, Vec2::splat(s));
            painter.rect_filled(r.expand(4.0 * scale), 6, with(PLAN, 0.18));
            painter.rect_filled(r, 3, PLAN);
            painter.rect_stroke(r, 3, Stroke::new(1.5 * scale, PLAN_INK), egui::StrokeKind::Inside);
            let g = grab(ui, base.with("radius"), c, s + 6.0, projector);
            if g.moved.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if let Some(at) = g.moved {
                let r = distance_ll([item.lon, item.lat], at).clamp(RADIUS_MIN_M, RADIUS_MAX_M);
                self.out.radius = Some((item.id, crate::data::units::js_round(r)));
            }
            if g.stopped {
                self.out.radius_dropped = true;
            }
        }

        // The move grip of the selected point's pattern.
        if let Some(group_id) = plan.selected_item().and_then(|i| i.pattern) {
            let group: Vec<&Item> = plan.items.iter().filter(|i| i.pattern == Some(group_id)).collect();
            if group.len() >= 2 {
                let centre = pattern_centre(&group);
                let c = screen(projector, centre);
                let r = 13.0 * scale;
                painter.circle_filled(c, r + 5.0 * scale, with(PLAN, 0.18));
                painter.circle_filled(c, r + 1.5 * scale, PLAN_INK);
                painter.circle_filled(c, r, PLAN);
                icons::paint(
                    ui,
                    "move",
                    Rect::from_center_size(c, Vec2::splat(15.0 * scale)),
                    PLAN_INK,
                );
                let g = grab(ui, base.with("pattern"), c, r * 2.0, projector);
                if g.started {
                    self.out.pattern_grab = Some(centre);
                }
                if let Some(at) = g.moved {
                    self.out.pattern_to = Some(at);
                }
                if g.stopped {
                    self.out.pattern_dropped = true;
                }
            }
        }

        // The map itself: placing, finishing and taking back.
        if response.hovered() && self.tool != Tool::Select {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if let Some(p) = response.hover_pos().filter(|p| response.rect.contains(*p)) {
            self.out.hover = Some(lonlat(projector, p));
        }
        let pointer = response.interact_pointer_pos().or_else(|| response.hover_pos());
        if response.clicked() {
            self.out.clicked = pointer.map(|p| lonlat(projector, p));
        }
        if response.double_clicked() {
            self.out.double_clicked = true;
        }
        if response.secondary_clicked() {
            self.out.secondary = pointer.map(|p| lonlat(projector, p));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_and_lines_grow_as_the_camera_pulls_back() {
        assert_eq!(point_scale(17.0), 1.0);
        assert_eq!(point_scale(18.5), 1.0, "close in they keep their size");
        assert_eq!(point_scale(9.0), 1.45);
        assert_eq!(point_scale(3.0), 1.45);
        assert_eq!(point_scale(13.0), 1.225);
        assert_eq!(point_scale(f64::NAN), 1.0);
        assert_eq!(width_factor(17.0), 1.0);
        assert_eq!(width_factor(9.0), 1.75);
        assert!((width_factor(13.0) - 1.375).abs() < 1e-6);
    }

    #[test]
    fn clicks_on_one_spot_are_one_point() {
        let pts = [[11.0, 48.0], [11.0, 48.0], [11.0000001, 48.0], [11.001, 48.0]];
        assert_eq!(distinct_points(&pts), vec![[11.0, 48.0], [11.001, 48.0]]);
    }

    #[test]
    fn a_pattern_is_moved_by_the_middle_of_its_box() {
        let mut a = Item::new(1, Kind::Waypoint);
        a.lat = 48.0;
        a.lon = 11.0;
        let mut b = Item::new(2, Kind::Waypoint);
        b.lat = 48.002;
        b.lon = 11.004;
        let mut c = Item::new(3, Kind::Waypoint);
        c.lat = 48.001;
        c.lon = 11.001;
        let centre = pattern_centre(&[&a, &b, &c]);
        assert!((centre[0] - 11.002).abs() < 1e-12 && (centre[1] - 48.001).abs() < 1e-12);
    }
}
