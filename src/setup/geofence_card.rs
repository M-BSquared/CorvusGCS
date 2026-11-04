//! The Geofence card of Safety & Sensors and its large editor
//! (python/src/js/setup-geofence.js, the `.geofence-*` rules of setup-forms.css).
//!
//! The area is drawn the way the planner's Area pattern is: click a corner,
//! click the next, Enter or a double click closes it, a right click takes the
//! last corner back, Escape gives up. Once closed every corner can be dragged,
//! a right click on a corner removes it and the small handle in the middle of
//! an edge adds one there. Every finished edit is saved on this station at
//! once; putting it on the vehicle is its own press.

use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::{json, Value};
use walkers::{HttpOptions, HttpTiles, MapMemory, Plugin, Projector};

use crate::map::{describe, CorvusTiles};
use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::geofence::{self, Corner, Env, Fence};
use crate::setup::schema_form::{Field, FormState};
use crate::ui::icons;
use crate::ui::kit::{self, Button, Variant};
use crate::ui::style::{self, Weight};

const DEFAULT_CENTER: Corner = [11.6, 48.1];
const DEFAULT_ZOOM: f64 = 15.0;
const MAP_H: f32 = 280.0;
const SOURCE: &str = "satellite";

/// What the section of `GET /api/safety` says, or the words used without one.
pub struct Props<'a> {
    pub hint: &'a str,
    pub inactive: &'a str,
    pub enable_writes: &'a [(String, f64)],
    pub fields: &'a [Field],
    pub connected: bool,
    pub armed: bool,
}

enum Kind {
    /// Keeping an area; the message is said when it has been kept.
    Save(Option<String>),
    Show,
    Upload(Vec<(String, f64)>),
    Clear,
    Enable,
}

struct Op {
    kind: Kind,
    done: Pending<Result<Value, String>>,
}

struct GeoMap {
    tiles: HttpTiles,
    memory: MapMemory,
    attribution: &'static str,
}

/// What the pointer did on the map this frame.
#[derive(Default)]
struct Out {
    clicks: Vec<Corner>,
    finish: bool,
    undo: bool,
    cursor: Option<Corner>,
    drag: Option<(usize, Corner)>,
    drag_started: bool,
    drop: bool,
    remove: Option<usize>,
    mid: Option<usize>,
}

struct Layers<'a> {
    poly: &'a [Corner],
    draft: &'a [Corner],
    cursor: Option<Corner>,
    drawing: bool,
    vehicle: Option<Corner>,
    can_add: bool,
    out: &'a mut Out,
}

fn screen(p: &Projector, c: Corner) -> Pos2 {
    p.project(walkers::lon_lat(c[0], c[1]))
        .to_pos2()
        .clamp(Pos2::new(-1e6, -1e6), Pos2::new(1e6, 1e6))
}

/// Ear clipping: triangles that fill a simple polygon, convex or not.
pub fn triangulate(pts: &[Pos2]) -> Vec<[usize; 3]> {
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let mut idx: Vec<usize> = if area >= 0.0 {
        (0..n).collect()
    } else {
        (0..n).rev().collect()
    };
    let cross = |a: Pos2, b: Pos2, c: Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let inside =
        |p: Pos2, a: Pos2, b: Pos2, c: Pos2| cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0;
    let mut out = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < 10_000 {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for k in 0..m {
            let (i0, i1, i2) = (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
            let (a, b, c) = (pts[i0], pts[i1], pts[i2]);
            if cross(a, b, c) <= 0.0 {
                continue;
            }
            if idx
                .iter()
                .any(|&j| j != i0 && j != i1 && j != i2 && inside(pts[j], a, b, c))
            {
                continue;
            }
            out.push([i0, i1, i2]);
            idx.remove(k);
            clipped = true;
            break;
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

impl Plugin for Layers<'_> {
    fn run(self: Box<Self>, ui: &mut egui::Ui, response: &egui::Response, projector: &Projector, _m: &MapMemory) {
        let this = *self;
        let painter = ui.painter_at(response.rect);
        let warning = style::warning();
        let ring: Vec<Corner> = if this.drawing {
            this.draft.iter().copied().chain(this.cursor).collect()
        } else {
            this.poly.to_vec()
        };
        let pts: Vec<Pos2> = ring.iter().map(|c| screen(projector, *c)).collect();
        if pts.len() >= 3 {
            let fill = warning.gamma_multiply(0.12);
            let mut mesh = Mesh::default();
            for p in &pts {
                mesh.colored_vertex(*p, fill);
            }
            for t in triangulate(&pts) {
                mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
            }
            painter.add(mesh);
            let mut closed = pts.clone();
            closed.push(pts[0]);
            painter.extend(Shape::dashed_line(&closed, Stroke::new(2.5, warning), 5.0, 3.75));
        } else if pts.len() == 2 {
            painter.extend(Shape::dashed_line(&pts, Stroke::new(2.5, warning), 5.0, 3.75));
        }
        if this.drawing {
            for c in this.draft {
                let p = screen(projector, *c);
                painter.circle_filled(p, 6.0, style::bg());
                painter.circle_filled(p, 4.25, warning);
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if let Some(p) = response.hover_pos().filter(|p| response.rect.contains(*p)) {
                let at = projector.unproject(p.to_vec2());
                this.out.cursor = Some([at.x(), at.y()]);
            }
            let pointer = response.interact_pointer_pos().or_else(|| response.hover_pos());
            if response.clicked() || response.double_clicked() {
                if let Some(p) = pointer {
                    let at = projector.unproject(p.to_vec2());
                    if response.clicked() {
                        this.out.clicks.push([at.x(), at.y()]);
                    }
                }
            }
            if response.double_clicked() {
                this.out.finish = true;
            }
            if response.secondary_clicked() {
                this.out.undo = true;
            }
        } else if this.poly.len() >= 3 {
            let n = this.poly.len();
            for i in 0..n {
                let p = screen(projector, this.poly[i]);
                let r = Rect::from_center_size(p, Vec2::splat(18.0));
                let h = ui.interact(r, response.id.with(("gfc", i)), egui::Sense::click_and_drag());
                if h.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
                if h.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    if let Some(q) = h.interact_pointer_pos() {
                        let at = projector.unproject(q.to_vec2());
                        this.out.drag = Some((i, [at.x(), at.y()]));
                    }
                }
                if h.drag_started() {
                    this.out.drag_started = true;
                }
                if h.drag_stopped() {
                    this.out.drop = true;
                }
                if h.secondary_clicked() {
                    this.out.remove = Some(i);
                }
                painter.circle_filled(p, 7.0, style::bg());
                painter.circle_filled(p, 5.0, warning);
                h.on_hover_text("Drag to move. Right click removes the corner.");
            }
            if this.can_add {
                for i in 0..n {
                    let (a, b) = (this.poly[i], this.poly[(i + 1) % n]);
                    let p = screen(projector, [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]);
                    let h = ui.interact(
                        Rect::from_center_size(p, Vec2::splat(14.0)),
                        response.id.with(("gfm", i)),
                        egui::Sense::click(),
                    );
                    let grow = if h.hovered() { 1.2 } else { 1.0 };
                    let alpha = if h.hovered() { 1.0 } else { 0.85 };
                    painter.circle_filled(p, 5.0 * grow, style::bg().gamma_multiply(alpha));
                    painter.circle_stroke(p, 4.0 * grow, Stroke::new(2.0, warning.gamma_multiply(alpha)));
                    if h.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
                    }
                    if h.clicked() {
                        this.out.mid = Some(i);
                    }
                    h.on_hover_text("Click to add a corner here");
                }
            }
        }
        if let Some(c) = this.vehicle {
            let p = screen(projector, c);
            painter.circle_filled(p, 8.0, Color32::WHITE);
            painter.circle_filled(p, 6.0, style::vehicle());
        }
    }
}

/// The corner masks of a rounded, clipped box: the map is a rectangle, the
/// box it sits in has round corners.
fn mask_corners(painter: &egui::Painter, rect: Rect, radius: f32, colour: Color32) {
    for (corner, sx, sy) in [
        (rect.left_top(), 1.0, 1.0),
        (rect.right_top(), -1.0, 1.0),
        (rect.left_bottom(), 1.0, -1.0),
        (rect.right_bottom(), -1.0, -1.0),
    ] {
        let centre = corner + Vec2::new(sx * radius, sy * radius);
        let mut mesh = Mesh::default();
        mesh.colored_vertex(corner, colour);
        for k in 0..=8 {
            let a = std::f32::consts::FRAC_PI_2 * k as f32 / 8.0;
            let p = centre + Vec2::new(-sx * a.cos() * radius, -sy * a.sin() * radius);
            mesh.colored_vertex(p, colour);
        }
        for k in 0..8u32 {
            mesh.add_triangle(0, 1 + k, 2 + k);
        }
        painter.add(mesh);
    }
}

pub struct GeofenceCard {
    server: Server,
    env: Env,
    map: Option<GeoMap>,
    poly: Vec<Corner>,
    seen: Fence,
    drawing: bool,
    draft: Vec<Corner>,
    cursor: Option<Corner>,
    dragging: bool,
    busy: bool,
    expanded: bool,
    hint: String,
    status: (&'static str, String),
    ops: Vec<Op>,
    errors: Vec<String>,
    reload: bool,
    want_fit: bool,
}

impl GeofenceCard {
    pub fn new(server: Server, env: Env, ctx: &egui::Context) -> GeofenceCard {
        env.load(ctx, &server);
        let fence = env.get();
        GeofenceCard {
            server,
            env,
            map: None,
            poly: fence.polygon.clone(),
            seen: fence,
            drawing: false,
            draft: Vec::new(),
            cursor: None,
            dragging: false,
            busy: false,
            expanded: false,
            hint: String::new(),
            status: ("", String::new()),
            ops: Vec::new(),
            errors: Vec::new(),
            reload: false,
            want_fit: true,
        }
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    /// Whether the page should read itself again (the fence was switched on).
    pub fn take_reload(&mut self) -> bool {
        std::mem::take(&mut self.reload)
    }

    fn say(&mut self, cls: &'static str, text: impl Into<String>) {
        self.status = (cls, text.into());
    }

    fn ensure_map(&mut self, ctx: &egui::Context) {
        if self.map.is_some() {
            return;
        }
        let (attribution, max_zoom) = describe(&self.env.sources, SOURCE);
        let attribution: &'static str = Box::leak(attribution.into_boxed_str());
        let source = CorvusTiles {
            base: self.server.url(),
            id: SOURCE.into(),
            attribution,
            max_zoom,
        };
        let options = HttpOptions {
            cache: None,
            ..Default::default()
        };
        let mut memory = MapMemory::default();
        let _ = memory.set_zoom(DEFAULT_ZOOM);
        memory.center_at(walkers::lon_lat(DEFAULT_CENTER[0], DEFAULT_CENTER[1]));
        self.map = Some(GeoMap {
            tiles: HttpTiles::with_options(source, options, ctx.clone()),
            memory,
            attribution,
        });
    }

    fn start(&mut self, kind: Kind, ctx: &egui::Context, path: &'static str, body: Value) {
        let s = self.server.clone();
        self.ops.push(Op {
            kind,
            done: Pending::spawn(ctx, move || http::post_json(&s, path, &body)),
        });
    }

    /// `keep`: save `next` on this station; on refusal the last saved area
    /// comes back.
    fn keep(&mut self, ctx: &egui::Context, next: Vec<Corner>, message: Option<String>) {
        self.busy = true;
        self.start(
            Kind::Save(message),
            ctx,
            "/api/geofence/save",
            json!({ "polygon": next }),
        );
    }

    /// Takes in the answers of the requests on their way.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.ops.len() {
            let Some(r) = self.ops[i].done.take() else {
                i += 1;
                continue;
            };
            let op = self.ops.remove(i);
            match (op.kind, r) {
                (Kind::Save(message), Ok(v)) => {
                    self.env.adopt(&v);
                    self.busy = false;
                    if let Some(m) = message {
                        self.say("ok", m);
                    }
                }
                (Kind::Save(_), Err(e)) => {
                    self.busy = false;
                    self.say(
                        "err",
                        if e.is_empty() {
                            "Could not save the area".to_string()
                        } else {
                            e
                        },
                    );
                    self.poly = self.env.get().polygon;
                }
                (Kind::Show, Ok(v)) => self.env.adopt(&v),
                (Kind::Show, Err(e)) => {
                    self.say(
                        "err",
                        if e.is_empty() {
                            "Could not save the setting".to_string()
                        } else {
                            e
                        },
                    );
                }
                (Kind::Upload(writes), Ok(v)) => {
                    self.env.adopt(&v);
                    if writes.is_empty() {
                        self.finish_upload(false);
                    } else {
                        self.say("pending", "Switching the fence on\u{2026}");
                        let s = self.server.clone();
                        self.ops.push(Op {
                            kind: Kind::Enable,
                            done: Pending::spawn(ctx, move || {
                                crate::setup::safety::write_all(&s, writes).map(|()| Value::Null)
                            }),
                        });
                    }
                }
                (Kind::Upload(_), Err(e)) => {
                    self.busy = false;
                    let msg = if e.is_empty() { "Upload failed".to_string() } else { e };
                    self.say("err", msg.clone());
                    self.errors.push(format!("Geofence upload failed: {msg}"));
                }
                (Kind::Enable, Ok(_)) => self.finish_upload(true),
                (Kind::Enable, Err(e)) => {
                    self.busy = false;
                    let msg = if e.is_empty() { "Upload failed".to_string() } else { e };
                    self.say("err", msg.clone());
                    self.errors.push(format!("Geofence upload failed: {msg}"));
                }
                (Kind::Clear, Ok(v)) => {
                    self.env.adopt(&v);
                    self.busy = false;
                    self.say("ok", "The vehicle holds no fence now. The area is still kept here.");
                }
                (Kind::Clear, Err(e)) => {
                    self.busy = false;
                    self.say(
                        "err",
                        if e.is_empty() {
                            "Could not remove the fence".to_string()
                        } else {
                            e
                        },
                    );
                }
            }
        }
        let fence = self.env.get();
        if fence != self.seen {
            if !self.drawing && !self.dragging {
                self.poly = fence.polygon.clone();
            }
            self.seen = fence;
        }
    }

    fn finish_upload(&mut self, wrote: bool) {
        self.busy = false;
        self.say("ok", "The vehicle holds the area.");
        if wrote {
            self.reload = true;
        }
    }

    fn start_drawing(&mut self) {
        self.drawing = true;
        self.draft.clear();
        self.cursor = None;
        self.say("", "");
        self.hint = "Click to place corners. Enter or a double click closes the area, \
                     right click undoes a corner, Escape cancels."
            .into();
    }

    fn stop_drawing(&mut self) {
        self.drawing = false;
        self.draft.clear();
        self.cursor = None;
        self.hint.clear();
    }

    fn finish_drawing(&mut self, ctx: &egui::Context, connected: bool) {
        let pts = geofence::distinct_corners(&self.draft);
        let problem = geofence::problem_of(&pts, self.env.get().max_vertices);
        if !problem.is_empty() {
            self.hint = format!("{problem} Keep clicking, or press Escape to cancel.");
            return;
        }
        self.stop_drawing();
        self.poly = pts.clone();
        self.keep(
            ctx,
            pts,
            Some(if connected {
                "Area saved. Upload it to put it on the vehicle.".into()
            } else {
                "Area saved.".into()
            }),
        );
    }

    fn delete_area(&mut self, ctx: &egui::Context) {
        if self.poly.is_empty() {
            return;
        }
        let message = if self.env.get().vehicle_has_fence == Some(true) {
            "Area deleted here. The vehicle still holds its fence until you remove it."
        } else {
            "Area deleted."
        };
        self.poly.clear();
        self.keep(ctx, Vec::new(), Some(message.into()));
    }

    fn upload(&mut self, ctx: &egui::Context, writes: Vec<(String, f64)>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.say("pending", "Uploading the area\u{2026}");
        self.start(Kind::Upload(writes), ctx, "/api/geofence/upload", json!({}));
    }

    fn remove_from_vehicle(&mut self, ctx: &egui::Context) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.say("pending", "Removing the fence from the vehicle\u{2026}");
        self.start(Kind::Clear, ctx, "/api/geofence/clear", json!({}));
    }

    fn vehicle(state: &Value) -> Option<Corner> {
        let a = state.get("position")?.as_array()?;
        let (lng, lat) = (a.first()?.as_f64()?, a.get(1)?.as_f64()?);
        (lng.is_finite() && lat.is_finite() && (lng != 0.0 || lat != 0.0)).then_some([lng, lat])
    }

    /// `summary`: the line under the map.
    pub fn summary(&self, connected: bool, vehicle: Option<Corner>) -> String {
        if self.drawing {
            let n = geofence::distinct_corners(&self.draft).len();
            return format!("Drawing: {n} {}.", if n == 1 { "corner" } else { "corners" });
        }
        if self.poly.len() < 3 {
            return "No area drawn.".into();
        }
        let fence = self.env.get();
        let where_ = if !connected {
            ""
        } else {
            match fence.on_vehicle {
                Some(true) => " On the vehicle.",
                Some(false) => " Not on the vehicle in this form, upload it.",
                None => " Not uploaded over this link yet.",
            }
        };
        let outside = if vehicle.is_some_and(|v| !geofence::contains(&self.poly, v)) {
            " The vehicle is outside this area."
        } else {
            ""
        };
        format!(
            "{} corners, {}.{where_}{outside}",
            self.poly.len(),
            geofence::format_area(geofence::area_m2(&self.poly))
        )
    }

    fn draw_label(&self) -> &'static str {
        if self.drawing {
            "Finish"
        } else if self.poly.len() >= 3 {
            "Redraw"
        } else {
            "Draw area"
        }
    }

    /// The opening view: the area if there is one, else the aircraft, else
    /// the default.
    fn frame(&mut self, rect: Rect, vehicle: Option<Corner>) {
        let Some(map) = self.map.as_mut() else { return };
        if self.poly.len() >= 3 {
            let pad = if self.expanded { 90.0 } else { 36.0 };
            if let Some((zoom, c)) = geofence::fit(&self.poly, (rect.width(), rect.height()), pad, 18.0) {
                let _ = map.memory.set_zoom(zoom);
                map.memory.center_at(walkers::lon_lat(c[0], c[1]));
            }
        } else if let Some(v) = vehicle {
            let _ = map.memory.set_zoom(16.0);
            map.memory.center_at(walkers::lon_lat(v[0], v[1]));
        }
    }

    /// Draws the map into `rect`, with its controls on top.
    fn map_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        rect: Rect,
        state: &Value,
        armed: bool,
        connected: bool,
    ) {
        self.ensure_map(ctx);
        let vehicle = Self::vehicle(state);
        if std::mem::take(&mut self.want_fit) {
            self.frame(rect, vehicle);
        }
        let fence = self.env.get();
        let can_add = self.poly.len() < fence.max_vertices.max(1);
        let mut out = Out::default();
        let drawing = self.drawing;
        let expanded = self.expanded;
        let response = {
            let Some(map) = self.map.as_mut() else { return };
            let layers = Layers {
                poly: &self.poly,
                draft: &self.draft,
                cursor: self.cursor,
                drawing,
                vehicle,
                can_add,
                out: &mut out,
            };
            let centre = vehicle.unwrap_or(DEFAULT_CENTER);
            let widget = walkers::Map::new(
                Some(&mut map.tiles as &mut dyn walkers::Tiles),
                &mut map.memory,
                walkers::lon_lat(centre[0], centre[1]),
            )
            .with_plugin(layers)
            .zoom_with_ctrl(!expanded)
            .double_click_to_zoom(!drawing)
            .zoom_speed(2.0);
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| ui.add(widget))
                .inner
        };
        let _ = response;
        self.apply(ctx, out, connected, armed);

        let painter = ui.painter().clone();
        let cardfill = style::bg();
        mask_corners(&painter, rect, f32::from(style::RADIUS), cardfill);
        painter.rect_stroke(
            rect,
            style::RADIUS,
            Stroke::new(1.0, style::border()),
            egui::StrokeKind::Inside,
        );

        // the hint over the map
        if !self.hint.is_empty() {
            let g = kit::wrapped_weight(
                ui,
                &self.hint,
                11.0,
                Weight::W400,
                style::text_1(),
                rect.width() - 60.0 - 20.0,
                1.21,
            );
            let r = Rect::from_min_size(
                Pos2::new(rect.left() + 8.0, rect.top() + 8.0),
                Vec2::new(rect.width() - 60.0, g.size().y + 12.0),
            );
            painter.rect_filled(r, style::RADIUS_BTN, style::bg().gamma_multiply(0.82));
            painter.galley(r.min + Vec2::new(10.0, 6.0), g, style::text_1());
        }
        // the zoom control: two 29 px buttons, white, as MapLibre draws it
        let group = Rect::from_min_size(
            Pos2::new(rect.right() - 11.0 - 29.0, rect.top() + 11.0),
            Vec2::new(29.0, 58.0),
        );
        painter.rect_filled(group.expand(2.0), 6, Color32::from_black_alpha(26));
        painter.rect_filled(group, 4, Color32::WHITE);
        painter.hline(
            group.x_range(),
            group.center().y,
            Stroke::new(1.0, Color32::from_gray(221)),
        );
        for (k, (icon, by)) in [("plus", 1), ("minus", -1)].into_iter().enumerate() {
            let r = Rect::from_min_size(group.min + Vec2::new(0.0, 29.0 * k as f32), Vec2::splat(29.0));
            let resp = ui.interact(r, ui.scope_id().with(("geozoom", expanded, k)), egui::Sense::click());
            if resp.hovered() {
                painter.rect_filled(r.shrink(0.5), 3, Color32::from_black_alpha(13));
            }
            icons::paint(
                ui,
                icon,
                Rect::from_center_size(r.center(), Vec2::splat(17.0)),
                Color32::from_gray(51),
            );
            if resp.clicked() {
                if let Some(m) = self.map.as_mut() {
                    let _ = if by > 0 {
                        m.memory.zoom_in()
                    } else {
                        m.memory.zoom_out()
                    };
                }
            }
        }
        // the compact credit: a round "i", the text on hover
        if let Some(m) = &self.map {
            if !m.attribution.is_empty() {
                let r = Rect::from_min_size(
                    Pos2::new(rect.left() + 10.0, rect.bottom() - 10.0 - 24.0),
                    Vec2::splat(24.0),
                );
                let resp = ui
                    .interact(r, ui.scope_id().with(("geoattr", expanded)), egui::Sense::hover())
                    .on_hover_text(m.attribution);
                painter.circle_filled(
                    r.center(),
                    12.0,
                    Color32::from_white_alpha(if resp.hovered() { 200 } else { 128 }),
                );
                icons::paint(
                    ui,
                    "info",
                    Rect::from_center_size(r.center(), Vec2::splat(15.0)),
                    Color32::from_gray(51),
                );
            }
        }
        if !expanded {
            let b = Button::new("Large map", Variant::Secondary)
                .icon("maximize-2")
                .small()
                .title("Edit the area on a large map");
            let (bw, bh) = (b.width(ui), b.height());
            if b.show(
                ui,
                Rect::from_min_size(
                    Pos2::new(rect.right() - 9.0 - bw, rect.bottom() - 9.0 - bh),
                    Vec2::new(bw, bh),
                ),
                "geoexpand",
            ) {
                self.open_editor();
            }
        }
        if self.drawing && ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p)) {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }

    /// What the pointer did on the map.
    fn apply(&mut self, ctx: &egui::Context, out: Out, connected: bool, armed: bool) {
        if self.drawing {
            self.cursor = out.cursor.or(self.cursor);
            for c in out.clicks {
                self.draft.push(c);
                if geofence::distinct_corners(&self.draft).len() >= 3 {
                    self.hint = "Enter or a double click closes the area. Right click undoes a corner.".into();
                }
            }
            if out.undo {
                self.draft.pop();
            }
            if out.finish {
                self.finish_drawing(ctx, connected);
            }
            return;
        }
        let max = self.env.get().max_vertices;
        if out.drag_started {
            self.dragging = true;
        }
        if let Some((i, c)) = out.drag {
            if i < self.poly.len() {
                self.poly[i] = c;
            }
        }
        if out.drop {
            self.dragging = false;
            let problem = geofence::problem_of(&self.poly, max);
            if !problem.is_empty() {
                self.say("err", format!("{problem} The corner went back."));
                self.poly = self.env.get().polygon;
            } else {
                let next = self.poly.clone();
                self.keep(ctx, next, Some("Area saved.".into()));
            }
        }
        if let Some(i) = out.remove {
            if armed || self.busy {
                return;
            }
            if self.poly.len() <= 3 {
                self.say("err", "An area needs at least 3 corners.");
                return;
            }
            let next: Vec<Corner> = self
                .poly
                .iter()
                .enumerate()
                .filter(|(k, _)| *k != i)
                .map(|(_, c)| *c)
                .collect();
            let problem = geofence::problem_of(&next, max);
            if !problem.is_empty() {
                self.say("err", problem);
                return;
            }
            self.poly = next.clone();
            self.keep(ctx, next, None);
        }
        if let Some(i) = out.mid {
            if armed || self.busy {
                return;
            }
            let n = self.poly.len();
            let (a, b) = (self.poly[i], self.poly[(i + 1) % n]);
            let mut next = self.poly.clone();
            next.insert(i + 1, [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]);
            self.poly = next.clone();
            self.keep(ctx, next, None);
        }
    }

    fn open_editor(&mut self) {
        if self.expanded {
            return;
        }
        self.expanded = true;
        self.want_fit = true;
    }

    fn close_editor(&mut self) {
        if !self.expanded {
            return;
        }
        if self.drawing {
            self.stop_drawing();
        }
        self.expanded = false;
        self.want_fit = true;
    }

    /// `onDraw`: drawing happens on the large map.
    fn on_draw(&mut self, ctx: &egui::Context, connected: bool) {
        if self.drawing {
            self.finish_drawing(ctx, connected);
            return;
        }
        self.open_editor();
        self.start_drawing();
    }

    fn show_switch_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let on = self.env.get().show_on_map;
        let lg = style::galley(ui, "Show on the Home map", 12.0, Weight::W600, style::text_1(), 0.0);
        let row_h = lg.size().y.max(22.0);
        ui.painter()
            .galley(Pos2::new(x, y + (row_h - lg.size().y) / 2.0), lg, style::text_1());
        if kit::switch(ui, Pos2::new(x + w - 38.0, y), on, true, "geoshow") {
            if let Ok(mut f) = self.env.fence.lock() {
                f.show_on_map = !on;
            }
            self.start(Kind::Show, ctx, "/api/geofence/save", json!({ "show_on_map": !on }));
        }
        let hg = kit::wrapped_weight(
            ui,
            "Draws the area as a dashed outline on the Home map.",
            10.5,
            Weight::W400,
            style::text_3(),
            w - 50.0,
            1.4,
        );
        ui.painter()
            .galley(Pos2::new(x, y + row_h + 8.0), hg.clone(), style::text_3());
        row_h + 8.0 + hg.size().y
    }

    /// The card under its title: hint, map, summary, buttons, status, the
    /// action field and the switch. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    pub fn show_body(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        server: &Server,
        form: &mut FormState,
        p: &Props,
        state: &Value,
        x: f32,
        y: f32,
        w: f32,
    ) -> f32 {
        self.poll(ctx);
        let painter = ui.painter().clone();
        let mut cy = y;
        let hint = if p.hint.is_empty() {
            "Draw the area the vehicle may fly in. Leaving it triggers the action below."
        } else {
            p.hint
        };
        let g = kit::wrapped_weight(ui, hint, 10.5, Weight::W400, style::text_3(), w, 1.4);
        painter.galley(Pos2::new(x, cy), g.clone(), style::text_3());
        cy += g.size().y + 10.0;

        // the map slot, or its note while the large editor holds the map
        let slot = Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, MAP_H));
        if self.expanded {
            painter.add(Shape::dashed_line(
                &[
                    slot.left_top(),
                    slot.right_top(),
                    slot.right_bottom(),
                    slot.left_bottom(),
                    slot.left_top(),
                ],
                Stroke::new(1.0, style::border_strong()),
                4.0,
                3.0,
            ));
            let g = style::galley(
                ui,
                "Editing on the large map.",
                12.0,
                Weight::W400,
                style::text_2(),
                0.0,
            );
            painter.galley(slot.center() - g.size() / 2.0, g, style::text_2());
        } else {
            self.map_view(ui, ctx, slot, state, p.armed, p.connected);
        }
        cy += MAP_H + 10.0;

        let summary = self.summary(p.connected, Self::vehicle(state));
        let stale = !self.drawing && self.poly.len() >= 3 && p.connected && self.env.get().on_vehicle != Some(true);
        let colour = if stale { style::warning() } else { style::text_2() };
        let g = kit::wrapped_weight(ui, &summary, 12.0, Weight::W400, colour, w, 1.21);
        painter.galley(Pos2::new(x, cy), g.clone(), colour);
        cy += g.size().y.max(15.0) + 10.0;

        // the buttons, wrapping
        let fence = self.env.get();
        let has = self.poly.len() >= 3;
        let draw_disabled = self.busy || (self.drawing && geofence::distinct_corners(&self.draft).len() < 3);
        let reason = if !p.connected {
            "Connect to a vehicle first"
        } else if p.armed {
            "Not while the vehicle is armed"
        } else {
            ""
        };
        let specs: [(&str, &str, Variant, bool, &str); 4] = [
            (self.draw_label(), "pen-line", Variant::Secondary, !draw_disabled, ""),
            (
                "Delete area",
                "trash-2",
                Variant::Secondary,
                !(self.busy || self.drawing || !has),
                "",
            ),
            (
                "Upload to vehicle",
                "upload",
                Variant::Primary,
                !(self.busy || self.drawing || !has || !p.connected || p.armed),
                reason,
            ),
            (
                "Remove from vehicle",
                "circle-x",
                Variant::Secondary,
                !(self.busy || self.drawing || !p.connected || p.armed || fence.vehicle_has_fence == Some(false)),
                reason,
            ),
        ];
        let (mut bx, mut by, mut row_h) = (x, cy, 0.0f32);
        let mut pressed = None;
        for (i, (label, icon, variant, enabled, title)) in specs.into_iter().enumerate() {
            let b = Button::new(label, variant)
                .icon(icon)
                .small()
                .enabled(enabled)
                .title(title);
            let (bw, bh) = (b.width(ui), b.height());
            if bx > x && bx + bw > x + w {
                bx = x;
                by += row_h + 8.0;
                row_h = 0.0;
            }
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(bx, by), Vec2::new(bw, bh)),
                ("geobtn", i),
            ) {
                pressed = Some(i);
            }
            row_h = row_h.max(bh);
            bx += bw + 8.0;
        }
        cy = by + row_h + 10.0;
        match pressed {
            Some(0) => self.on_draw(ctx, p.connected),
            Some(1) => self.delete_area(ctx),
            Some(2) => self.upload(ctx, p.enable_writes.to_vec()),
            Some(3) => self.remove_from_vehicle(ctx),
            _ => {}
        }

        // the status line holds its place even when empty
        let (cls, text) = self.status.clone();
        let sh = if text.is_empty() {
            0.0
        } else {
            let c = match cls {
                "ok" => style::healthy(),
                "err" => style::critical(),
                _ => style::nav(),
            };
            let g = kit::wrapped_weight(ui, &text, 10.5, Weight::W400, c, w, 1.4);
            painter.galley(Pos2::new(x, cy), g.clone(), c);
            g.size().y
        };
        cy += sh + 10.0;

        if !p.inactive.is_empty() {
            let g = kit::wrapped_weight(ui, p.inactive, 10.5, Weight::W400, style::warning(), w, 1.4);
            painter.galley(Pos2::new(x, cy), g.clone(), style::warning());
            cy += g.size().y + 10.0;
        }
        if !p.fields.is_empty() {
            cy = form.show(ui, ctx, server, p.fields, x, cy, w, p.armed) + 10.0;
        } else if !p.connected {
            let g = kit::wrapped_weight(
                ui,
                "Connect to a vehicle to choose what it does when it leaves the area.",
                10.5,
                Weight::W400,
                style::text_3(),
                w,
                1.4,
            );
            painter.galley(Pos2::new(x, cy), g.clone(), style::text_3());
            cy += g.size().y + 10.0;
        }
        self.show_switch_row(ui, ctx, x, cy, w).mul_add(1.0, cy)
    }

    /// The large editor over the whole window, while it is open.
    pub fn overlay(&mut self, ctx: &egui::Context, state: &Value, connected: bool, armed: bool) {
        if !self.expanded {
            return;
        }
        // Escape first gives up the corners being drawn, then closes
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.drawing {
                self.stop_drawing();
            } else {
                self.close_editor();
                return;
            }
        }
        if self.drawing {
            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.finish_drawing(ctx, connected);
            } else if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                self.draft.pop();
            }
        }
        let screen = ctx.content_rect();
        let panel = screen.shrink(24.0);
        let mut close = false;
        let mut draw = false;
        let mut delete = false;
        egui::Area::new(egui::Id::unique("geofence-editor"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(screen);
                let scrim = ui.allocate_rect(screen, egui::Sense::click());
                let scrim_colour = if style::is_dark() {
                    Color32::from_black_alpha(77)
                } else {
                    Color32::from_rgba_unmultiplied(22, 28, 38, 89)
                };
                ui.painter().rect_filled(screen, 0, scrim_colour);
                let painter = ui.painter().clone();
                painter.add(
                    egui::epaint::Shadow {
                        offset: [0, 16],
                        blur: 48,
                        spread: 0,
                        color: Color32::from_black_alpha(if style::is_dark() { 128 } else { 46 }),
                    }
                    .as_shape(panel, 14),
                );
                painter.rect_filled(panel, 14, style::surface_1());
                painter.rect_stroke(panel, 14, Stroke::new(1.0, style::border()), egui::StrokeKind::Inside);
                let inner = panel.shrink(14.0);
                let title = style::galley(ui, "Geofence area", 13.0, Weight::W600, style::text_1(), 0.0);
                let summary_text = self.summary(connected, Self::vehicle(state));
                let sg = style::galley(ui, &summary_text, 12.0, Weight::W400, style::text_2(), 0.0);
                painter.galley(inner.min, title.clone(), style::text_1());
                painter.galley(
                    inner.min + Vec2::new(0.0, title.size().y + 2.0),
                    sg.clone(),
                    style::text_2(),
                );
                let has = self.poly.len() >= 3;
                let draw_disabled = self.busy || (self.drawing && geofence::distinct_corners(&self.draft).len() < 3);
                let btns: [(&str, &str, Variant, bool); 3] = [
                    (self.draw_label(), "pen-line", Variant::Secondary, !draw_disabled),
                    (
                        "Delete area",
                        "trash-2",
                        Variant::Secondary,
                        !(self.busy || self.drawing || !has),
                    ),
                    ("Done", "check", Variant::Primary, !self.busy),
                ];
                let widths: Vec<f32> = btns
                    .iter()
                    .map(|(l, i, v, e)| Button::new(l, *v).icon(i).small().enabled(*e).width(ui))
                    .collect();
                let bh = Button::new("x", Variant::Primary).small().height();
                let mut bx = inner.right();
                let head_h = (title.size().y + 2.0 + sg.size().y).max(bh);
                for (k, (l, i, v, e)) in btns.iter().enumerate().rev() {
                    bx -= widths[k];
                    let b = Button::new(l, *v).icon(i).small().enabled(*e);
                    if b.show(
                        ui,
                        Rect::from_min_size(
                            Pos2::new(bx, inner.top() + (head_h - bh) / 2.0),
                            Vec2::new(widths[k], bh),
                        ),
                        ("geoeditbtn", k),
                    ) {
                        match k {
                            0 => draw = true,
                            1 => delete = true,
                            _ => close = true,
                        }
                    }
                    bx -= 8.0;
                }
                let body = Rect::from_min_max(Pos2::new(inner.left(), inner.top() + head_h + 12.0), inner.max);
                self.map_view(ui, ctx, body, state, armed, connected);
                if scrim.clicked() && !self.drawing && !scrim.interact_pointer_pos().is_some_and(|p| panel.contains(p))
                {
                    close = true;
                }
            });
        if draw {
            self.on_draw(ctx, connected);
        }
        if delete {
            self.delete_area(ctx);
        }
        if close {
            self.close_editor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Pos2 {
        Pos2::new(x, y)
    }

    fn area(pts: &[Pos2], tris: &[[usize; 3]]) -> f32 {
        tris.iter()
            .map(|t| {
                let (a, b, c) = (pts[t[0]], pts[t[1]], pts[t[2]]);
                ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() / 2.0
            })
            .sum()
    }

    #[test]
    fn a_square_is_two_triangles() {
        let sq = [p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)];
        let t = triangulate(&sq);
        assert_eq!(t.len(), 2);
        assert!((area(&sq, &t) - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_concave_polygon_is_filled_without_its_notch() {
        // an L: 3 squares of 10 x 10
        let l = [
            p(0.0, 0.0),
            p(20.0, 0.0),
            p(20.0, 10.0),
            p(10.0, 10.0),
            p(10.0, 20.0),
            p(0.0, 20.0),
        ];
        let t = triangulate(&l);
        assert_eq!(t.len(), 4);
        assert!((area(&l, &t) - 300.0).abs() < 1e-3);
        // the same ring the other way round
        let r: Vec<Pos2> = l.iter().rev().copied().collect();
        assert!((area(&r, &triangulate(&r)) - 300.0).abs() < 1e-3);
    }

    #[test]
    fn fewer_than_three_points_fill_nothing() {
        assert!(triangulate(&[p(0.0, 0.0), p(1.0, 1.0)]).is_empty());
    }
}
