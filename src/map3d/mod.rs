//! The 3D map: the ground as a quadtree of web mercator tiles seen through a
//! tilted camera, the base layer draped on it, the aircraft, home and the
//! flown track in the air above it (`python/src/js/map.js` 3D mode). See "The 3D
//! map" in the README for how it is built and why.

pub mod buildings;
pub mod camera;
pub mod gpu;
pub mod panel;
pub mod ribbon;
pub mod terrain;
pub mod tile_cache;

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Stroke, Vec2};
use eframe::egui_glow;

use crate::data::track::{self, Point, Track};
use crate::map3d::buildings::Building;
use crate::map3d::camera::{Camera, V3};
use crate::map3d::gpu::{MeshData, Scene, FLOATS};
use crate::map3d::terrain::{DemTile, Terrain};
use crate::map3d::tile_cache::{Key, TileCache};
use crate::net::bg::Pending;
use crate::net::http;
use crate::net::http::Server;

/// A leaf is split while it would cover more than this many pixels.
const SPLIT_PX: f64 = 230.0;
/// Quads per side of a leaf.
const GRID: usize = 16;
const MAX_LEAVES: usize = 900;
const TEXTURES: usize = 400;
const IMAGE_CACHE: usize = 600;
/// `HILLSHADE_EXAGGERATION` of the web map, a little stronger as the shade is
/// painted into vertex colours here and not drawn as a layer.
const SHADE: f32 = 0.5;
/// `GROUND_COLOR`: what the ground is where its imagery has not come.
const GROUND_COLOR: [u8; 3] = [0x8B, 0x90, 0x81];
/// The light of MapLibre's hillshade: from 335 degrees, 45 above the ground.
const LIGHT: [f64; 3] = [-0.2989, 0.6409, std::f64::consts::FRAC_1_SQRT_2];

/// What the 3D view draws above the ground.
pub struct Layers3d<'a> {
    pub track: &'a Track,
    pub home: Option<(f64, f64)>,
    pub vehicle: Option<(f64, f64)>,
    pub heading: f32,
    /// The aircraft's height above the ground, in metres.
    pub height: f64,
    /// The flown vehicle's own colour, and its track's: the theme's unless
    /// the operator chose one.
    pub vehicle_color: Color32,
    pub track_color: Color32,
    pub past_color: Color32,
}

/// What the pointer did to the camera this frame.
pub struct Input {
    pub dragged: bool,
    /// Where a press-and-release landed on the ground (longitude, latitude)
    /// and whether it was the right button.
    pub click: Option<([f64; 2], bool)>,
    /// Where a left press-and-release landed on the screen.
    pub press: Option<Pos2>,
}

/// The base layer's tiles as a decoded image.
fn decode_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// One tile of the ground to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Leaf {
    z: u8,
    x: u32,
    y: u32,
}

/// The 3D view's state.
pub struct Map3d {
    pub cam: Camera,
    source: String,
    max_zoom: u8,
    imagery: TileCache<egui::ColorImage>,
    server: Server,
    ctx: egui::Context,
    /// The elevation in use and the base layer it was chosen for.
    terrain: Option<Terrain>,
    /// The default model, drawn where the chosen one has no tile (an own
    /// model covers only its survey).
    fallback: Option<Terrain>,
    terrain_for: String,
    /// The operator's pick, `auto` or an elevation model id.
    pub choice: String,
    saved: Option<Pending<Result<serde_json::Value, String>>>,
    /// The full 3D: buildings as well as relief (`three_d_detail`).
    pub full: bool,
    buildings: Option<TileCache<Vec<Building>>>,
    /// The id the GPU knows each tile's image by; the image is held so the
    /// id means that one image for as long as it is listed.
    tex_ids: HashMap<Key, (Arc<egui::ColorImage>, u64)>,
    used: Vec<Key>,
    leaf_meshes: HashMap<(u8, u32, u32), LeafMesh>,
    cell_meshes: HashMap<(u32, u32), CellMesh>,
    frame: u64,
}

/// An elevation tile and the part `(u0, v0, size)` of it a leaf covers.
type Dem = (Arc<DemTile>, (f64, f64, f64));

/// The mesh of one leaf, and the elevation it was made from.
struct LeafMesh {
    dem: Option<Dem>,
    mesh: Arc<MeshData>,
    used: u64,
}

/// A cell of buildings by its grid position.
type Cell = ((u32, u32), Arc<Vec<Building>>);

/// The mesh of one cell of buildings, and the answer it was made from.
struct CellMesh {
    cell: Arc<Vec<Building>>,
    mesh: Option<Arc<MeshData>>,
    used: u64,
}

/// Web mercator metres of tile corner `(x + u, y + v)` at zoom `z`.
fn tile_mercator(z: u8, x: f64, y: f64) -> (f64, f64) {
    let world = 2.0 * std::f64::consts::PI * crate::map3d::camera::R;
    let n = 2f64.powi(z as i32);
    (-world / 2.0 + x / n * world, world / 2.0 - y / n * world)
}

impl Map3d {
    pub fn new(ctx: &egui::Context, server: &Server, source: &str, max_zoom: u8) -> Map3d {
        Map3d {
            cam: Camera {
                lng: 0.0,
                lat: 0.0,
                zoom: 14.0,
                pitch: crate::map3d::camera::THREE_D_PITCH,
                bearing: 0.0,
                size: (800.0, 600.0),
                ground: 0.0,
            },
            source: source.to_string(),
            max_zoom,
            imagery: TileCache::new(ctx, server, IMAGE_CACHE, decode_image),
            server: server.clone(),
            ctx: ctx.clone(),
            terrain: None,
            fallback: None,
            terrain_for: String::new(),
            choice: "auto".to_string(),
            full: false,
            buildings: None,
            saved: Some({
                let s = server.clone();
                Pending::spawn(ctx, move || http::get_json(&s, "/api/config"))
            }),
            tex_ids: HashMap::new(),
            used: Vec::new(),
            leaf_meshes: HashMap::new(),
            cell_meshes: HashMap::new(),
            frame: 0,
        }
    }

    /// Takes the view of the flat map.
    pub fn start_at(&mut self, lng: f64, lat: f64, zoom: f64, bearing: f64) {
        self.cam.lng = lng;
        self.cam.lat = lat;
        self.cam.zoom = zoom;
        self.cam.bearing = bearing;
        self.cam.pitch = crate::map3d::camera::THREE_D_PITCH;
    }

    /// The base layer to drape.
    pub fn set_source(&mut self, id: &str, max_zoom: u8) {
        if self.source != id {
            self.source = id.to_string();
            self.imagery.clear();
            self.tex_ids.clear();
        }
        self.max_zoom = max_zoom;
    }

    /// The map service of the base layer, which pairs it with a model.
    fn provider(&self, sources: &serde_json::Value) -> String {
        sources
            .get("sources")
            .and_then(serde_json::Value::as_array)
            .and_then(|l| {
                l.iter()
                    .find(|s| s.get("id").and_then(serde_json::Value::as_str) == Some(&self.source))
            })
            .and_then(|s| s.get("provider").and_then(serde_json::Value::as_str))
            .unwrap_or("")
            .to_string()
    }

    /// Picks the elevation model the choice and the base layer call for, when
    /// that changed, and reads the saved choice once it has come.
    pub fn follow_sources(&mut self, sources: &serde_json::Value) {
        let mut changed = false;
        if let Some(r) = self.saved.as_ref().and_then(Pending::take) {
            self.saved = None;
            if let Some(d) = r.as_ref().ok().and_then(|c| {
                c.pointer("/config/map/three_d_detail")
                    .and_then(serde_json::Value::as_str)
            }) {
                self.full = d == "full";
            }
            if let Some(c) = r.ok().and_then(|c| {
                c.pointer("/config/map/terrain_source")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }) {
                self.choice = c;
                changed = true;
            }
        }
        if !changed && self.terrain_for == format!("{}|{}", self.source, self.choice) && self.terrain.is_some() {
            return;
        }
        self.terrain_for = format!("{}|{}", self.source, self.choice);
        let provider = self.provider(sources);
        let pick = (self.choice != "auto").then_some(self.choice.as_str());
        if let Some(spec) = terrain::choose(sources, &provider, pick) {
            if self.terrain.as_ref().map(|t| &t.spec.id) != Some(&spec.id) {
                let default = sources
                    .get("default_terrain")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                self.fallback = (spec.id != default)
                    .then(|| terrain::catalogue(sources).into_iter().find(|d| d.id == default))
                    .flatten()
                    .map(|d| Terrain::new(&self.ctx, &self.server, d));
                self.terrain = Some(Terrain::new(&self.ctx, &self.server, spec));
            }
        }
    }

    /// The rows of the elevation list and the one in use.
    pub fn elevation_rows(&self, sources: &serde_json::Value) -> Vec<(String, String)> {
        terrain::options(sources, &self.provider(sources))
    }

    /// Chooses the elevation model and keeps the choice in the config.
    pub fn set_choice(&mut self, id: &str) {
        if self.choice == id {
            return;
        }
        self.choice = id.to_string();
        let s = self.server.clone();
        let body = serde_json::json!({"map": {"terrain_source": id}});
        std::thread::spawn(move || {
            let _ = http::post_json(&s, "/api/config", &body);
        });
    }

    /// Chooses relief only or relief with buildings, and keeps it.
    pub fn set_full(&mut self, on: bool) {
        if self.full == on {
            return;
        }
        self.full = on;
        let s = self.server.clone();
        let body = serde_json::json!({"map": {"three_d_detail": if on { "full" } else { "simple" }}});
        std::thread::spawn(move || {
            let _ = http::post_json(&s, "/api/config", &body);
        });
    }

    /// The model in use, for the credit.
    pub fn terrain_credit(&self) -> Option<&str> {
        self.terrain
            .as_ref()
            .map(|t| t.spec.attribution.as_str())
            .filter(|a| !a.is_empty())
    }

    /// The ground height at a position, the centre's when none is loaded.
    fn ground_at(&self, lng: f64, lat: f64) -> f64 {
        self.terrain
            .as_ref()
            .and_then(|t| t.height_at(lng, lat, 13.0))
            .or_else(|| self.fallback.as_ref().and_then(|t| t.height_at(lng, lat, 13.0)))
            .unwrap_or(self.cam.ground)
    }

    /// The ground the view sees, as east and north metres around the centre:
    /// the screen corners met with the ground, and past the horizon the
    /// farthest that is worth drawing.
    fn ground_box(&self) -> ((f64, f64), (f64, f64)) {
        let c = &self.cam;
        let reach = c.distance() * 30.0;
        let mut min = (f64::MAX, f64::MAX);
        let mut max = (f64::MIN, f64::MIN);
        let (w, h) = c.size;
        for (sx, sy) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h), (w / 2.0, 0.0), (w / 2.0, h)] {
            let (e, n) = c.ground_at(sx, sy, 0.0).unwrap_or_else(|| {
                let b = c.bearing.to_radians();
                (b.sin() * reach, b.cos() * reach)
            });
            let (e, n) = (e.clamp(-reach, reach), n.clamp(-reach, reach));
            min = (min.0.min(e), min.1.min(n));
            max = (max.0.max(e), max.1.max(n));
        }
        (min, max)
    }

    /// The tiles that make the ground: split by how big they look.
    fn leaves(&self) -> Vec<Leaf> {
        let c = &self.cam;
        let k = c.lat.to_radians().cos();
        let (cx, cy) = crate::map3d::camera::mercator(c.lng, c.lat);
        let (min, max) = self.ground_box();
        let z0 = (c.zoom.floor() as i32 - 4).clamp(0, self.max_zoom as i32) as u8;
        let n_tiles = 2f64.powi(z0 as i32);
        let world = 2.0 * std::f64::consts::PI * crate::map3d::camera::R;
        let to_tile = |mx: f64, my: f64| {
            (
                (mx + world / 2.0) / world * n_tiles,
                (world / 2.0 - my) / world * n_tiles,
            )
        };
        let (x0, y1) = to_tile(cx + min.0 / k, cy + min.1 / k);
        let (x1, y0) = to_tile(cx + max.0 / k, cy + max.1 / k);
        let clampi = |v: f64| (v.floor().max(0.0) as u32).min(n_tiles as u32 - 1);
        let mut out = Vec::new();
        for ty in clampi(y0)..=clampi(y1) {
            for tx in clampi(x0)..=clampi(x1) {
                self.split(Leaf { z: z0, x: tx, y: ty }, &mut out);
            }
        }
        out
    }

    fn corners(&self, l: Leaf) -> [V3; 4] {
        let c = &self.cam;
        let k = c.lat.to_radians().cos();
        let (cx, cy) = crate::map3d::camera::mercator(c.lng, c.lat);
        let at = |dx: f64, dy: f64| {
            let (mx, my) = tile_mercator(l.z, l.x as f64 + dx, l.y as f64 + dy);
            [(mx - cx) * k, (my - cy) * k, -c.ground]
        };
        [at(0.0, 0.0), at(1.0, 0.0), at(1.0, 1.0), at(0.0, 1.0)]
    }

    fn visible(&self, l: Leaf) -> bool {
        let pts = self.corners(l);
        let proj: Vec<_> = pts.iter().map(|p| self.cam.project(*p)).collect();
        if proj.iter().any(Option::is_none) {
            return true;
        }
        let (w, h) = self.cam.size;
        let s: Vec<_> = proj.into_iter().flatten().collect();
        !(s.iter().all(|p| p.x < 0.0)
            || s.iter().all(|p| p.x > w)
            || s.iter().all(|p| p.y < 0.0)
            || s.iter().all(|p| p.y > h))
    }

    fn split(&self, l: Leaf, out: &mut Vec<Leaf>) {
        if !self.visible(l) {
            return;
        }
        let c = &self.cam;
        let k = c.lat.to_radians().cos();
        let metres = 2.0 * std::f64::consts::PI * crate::map3d::camera::R * k / 2f64.powi(l.z as i32);
        let pts = self.corners(l);
        let mid = [(pts[0][0] + pts[2][0]) / 2.0, (pts[0][1] + pts[2][1]) / 2.0, pts[0][2]];
        let px = c.project(mid).map_or(f64::MAX, |s| metres / s.depth * c.focal());
        if px > SPLIT_PX && l.z < self.max_zoom && out.len() < MAX_LEAVES {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                self.split(
                    Leaf {
                        z: l.z + 1,
                        x: l.x * 2 + dx,
                        y: l.y * 2 + dy,
                    },
                    out,
                );
            }
        } else {
            out.push(l);
        }
    }

    /// The imagery of a leaf or its nearest ancestor that is here, and the
    /// part of it the leaf covers.
    fn imagery_for(&mut self, l: Leaf) -> Option<(gpu::Texture, Rect)> {
        let top = l.z.min(self.max_zoom);
        let mut want = true;
        // A coarse ancestor is asked for too, so there is always something to
        // draw while the fine tile is on its way.
        let coarse = top.min(3);
        for d in 0..=top.min(12) {
            if d == coarse && d > 0 {
                want = true;
            }
            let (z, x, y) = (top - d, l.x >> d, l.y >> d);
            let key: Key = (self.source.clone(), z, x, y);
            let found = if want {
                self.imagery.get(&key)
            } else {
                self.imagery.peek(&key)
            };
            want = false;
            if let Some(image) = found {
                let id = match self.tex_ids.get(&key) {
                    Some((held, id)) if Arc::ptr_eq(held, &image) => *id,
                    _ => {
                        let id = gpu::next_id();
                        self.tex_ids.insert(key.clone(), (image.clone(), id));
                        id
                    }
                };
                self.used.push(key);
                return Some((gpu::Texture { id, image }, uv_of(l, z)));
            }
        }
        None
    }

    /// The elevation for a leaf from the model in use, else the default.
    fn dem_for(&self, l: Leaf) -> Option<Dem> {
        self.terrain
            .as_ref()
            .and_then(|t| t.tile_for(l.z, l.x, l.y))
            .or_else(|| self.fallback.as_ref().and_then(|t| t.tile_for(l.z, l.x, l.y)))
    }

    /// The mesh of a leaf: made once, again only when its elevation changes.
    fn leaf_mesh(&mut self, l: Leaf) -> (Arc<MeshData>, bool) {
        let dem = self.dem_for(l);
        let key = (l.z, l.x, l.y);
        if let Some(m) = self.leaf_meshes.get_mut(&key) {
            let same = match (&m.dem, &dem) {
                (None, None) => true,
                (Some((a, ra)), Some((b, rb))) => Arc::ptr_eq(a, b) && ra == rb,
                _ => false,
            };
            if same {
                m.used = self.frame;
                return (m.mesh.clone(), dem.is_none());
            }
        }
        let k = self.cam.lat.to_radians().cos();
        let mesh = Arc::new(build_leaf(l, dem.as_ref(), k));
        self.leaf_meshes.insert(
            key,
            LeafMesh {
                dem: dem.clone(),
                mesh: mesh.clone(),
                used: self.frame,
            },
        );
        (mesh, dem.is_none())
    }

    /// The items of the ground in view.
    fn ground_items(&mut self, leaves: &[Leaf]) -> Vec<gpu::Item> {
        self.used.clear();
        let mut items = Vec::with_capacity(leaves.len());
        for l in leaves {
            let imagery = self.imagery_for(*l);
            let (mesh, flat) = self.leaf_mesh(*l);
            let (mx, my) = tile_mercator(l.z, f64::from(l.x), f64::from(l.y));
            let (texture, uv) = match imagery {
                Some((t, uv)) => (Some(t), [uv.min.x, uv.min.y, uv.width(), uv.height()]),
                None => (None, [0.0, 0.0, 1.0, 1.0]),
            };
            items.push(gpu::Item {
                mesh,
                origin: (mx, my),
                texture,
                uv,
                flat,
                color: [
                    f32::from(GROUND_COLOR[0]) / 255.0,
                    f32::from(GROUND_COLOR[1]) / 255.0,
                    f32::from(GROUND_COLOR[2]) / 255.0,
                ],
                alpha: 1.0,
            });
        }
        if self.tex_ids.len() > TEXTURES {
            let keep: std::collections::HashSet<&Key> = self.used.iter().collect();
            self.tex_ids.retain(|k, _| keep.contains(k));
        }
        if self.leaf_meshes.len() > MAX_LEAVES * 3 {
            let frame = self.frame;
            self.leaf_meshes.retain(|_, m| m.used + 30 >= frame);
        }
        items
    }

    /// Draws the view into `rect`.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, layers: &Layers3d<'_>) -> Input {
        self.cam.size = (rect.width() as f64, rect.height() as f64);
        let resp = ui.interact(rect, ui.scope_id().with("map3d"), egui::Sense::click_and_drag());
        let origin = rect.min;
        let local = |p: Pos2| ((p.x - origin.x) as f64, (p.y - origin.y) as f64);
        let mut dragged = false;
        if resp.dragged_by(egui::PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let d = resp.drag_delta();
                let now = local(p);
                let before = (now.0 - d.x as f64, now.1 - d.y as f64);
                if ui.input(|i| i.modifiers.ctrl || i.modifiers.alt) {
                    self.cam.turn(d.x as f64, d.y as f64);
                } else {
                    self.cam.pan(before, now);
                }
                dragged = true;
            }
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            self.cam.turn(d.x as f64, d.y as f64);
            dragged = true;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.cam.zoom = (self.cam.zoom + scroll as f64 * 0.004).clamp(3.0, self.max_zoom as f64 + 2.0);
            }
        }

        if let Some(h) = self
            .terrain
            .as_ref()
            .and_then(|t| t.height_at(self.cam.lng, self.cam.lat, self.cam.zoom))
            .or_else(|| {
                self.fallback
                    .as_ref()
                    .and_then(|t| t.height_at(self.cam.lng, self.cam.lat, self.cam.zoom))
            })
        {
            self.cam.ground = h;
        }
        self.frame += 1;
        let painter = ui.painter_at(rect);
        painter.rect_filled(
            rect,
            0,
            Color32::from_rgb(GROUND_COLOR[0], GROUND_COLOR[1], GROUND_COLOR[2]),
        );
        let horizon = self.paint_sky(&painter, rect);
        let leaves = self.leaves();
        let ground_items = self.ground_items(&leaves);
        let building_items = self.building_items();
        self.imagery.trim();
        for t in self.terrain.iter().chain(self.fallback.iter()) {
            t.trim();
        }
        let scene = Scene {
            mvp: self.cam.view_projection(),
            centre: crate::map3d::camera::mercator(self.cam.lng, self.cam.lat),
            k: self.cam.lat.to_radians().cos(),
            ground: self.cam.ground,
            ground_items,
            building_items,
            line_items: self.line_items(layers),
        };
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_, p| {
                // SAFETY: egui_glow calls this on its own context, inside its paint.
                unsafe { gpu::paint(p.gl(), &scene) }
            })),
        });
        if let Some(y) = horizon {
            haze(&painter, rect, y);
        }
        if let Some(why) = gpu::failure() {
            let text = format!("The 3D map cannot draw on this OpenGL: {why}");
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(13.0),
                crate::ui::style::text_2(),
            );
        }
        self.paint_layers(&painter, rect.min, layers);
        self.paint_credit(ui, rect);
        let secondary = resp.secondary_clicked();
        let click = (resp.clicked() || secondary)
            .then(|| resp.interact_pointer_pos())
            .flatten()
            .and_then(|p| self.pick(rect, p))
            .map(|g| (g, secondary));
        let press = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();
        Input { dragged, click, press }
    }

    /// `setSky`: the sky over the horizon in the theme's own greys. At 60
    /// degrees the horizon is off the screen and this draws nothing. Returns
    /// the screen height of the horizon when it is in view.
    fn paint_sky(&self, painter: &egui::Painter, rect: Rect) -> Option<f32> {
        let c = &self.cam;
        let theta = (90.0 - c.pitch).to_radians();
        let y = (c.size.1 / 2.0 - c.focal() * theta.tan()) as f32;
        if y <= 0.0 {
            return None;
        }
        let y = y.min(rect.height());
        let (top, mid, low) = (
            crate::ui::style::surface_3(),
            crate::ui::style::surface_3(),
            crate::ui::style::surface_1(),
        );
        let mut m = Mesh::default();
        let x = [rect.left(), rect.right()];
        for (t, col) in [(0.0, top), (0.4, mid), (1.0, low)] {
            let py = rect.top() + y * t;
            m.colored_vertex(Pos2::new(x[0], py), col);
            m.colored_vertex(Pos2::new(x[1], py), col);
        }
        for i in 0..2u32 {
            let b = i * 2;
            m.add_triangle(b, b + 1, b + 2);
            m.add_triangle(b + 1, b + 3, b + 2);
        }
        painter.add(egui::Shape::mesh(m));
        Some(rect.top() + y)
    }

    /// The OSM buildings of the cells in view, when the full 3D is on and
    /// the zoom is close enough for them to be more than slivers. A cell is
    /// built into a mesh once, when the terrain under it is here.
    fn building_items(&mut self) -> Vec<gpu::Item> {
        if !self.full || self.cam.zoom < buildings::MIN_ZOOM {
            return Vec::new();
        }
        let (min, max) = self.ground_box();
        let (w, s) = self.cam.geo_of(min.0, min.1);
        let (e, n) = self.cam.geo_of(max.0, max.1);
        let cache = self.buildings.get_or_insert_with(|| {
            TileCache::with_path(
                &self.ctx,
                &self.server,
                240,
                |k| format!("/api/buildings/{}/{}/{}.json", k.1, k.2, k.3),
                std::time::Duration::from_secs(buildings::RETRY_SECS),
                buildings::decode,
            )
        });
        let mut cells: Vec<Cell> = Vec::new();
        for (x, y) in buildings::cells_in((w, s, e, n), (self.cam.lng, self.cam.lat)) {
            if let Some(cell) = cache.get(&("b".to_string(), buildings::CELL_Z, x, y)) {
                cells.push(((x, y), cell));
            }
        }
        cache.trim();
        let mut items = Vec::new();
        for ((x, y), cell) in cells {
            let fresh = self
                .cell_meshes
                .get(&(x, y))
                .is_some_and(|m| Arc::ptr_eq(&m.cell, &cell) && (m.mesh.is_some() || cell.is_empty()));
            if !fresh {
                let origin = tile_mercator(buildings::CELL_Z, f64::from(x), f64::from(y));
                let this = &*self;
                let mesh = buildings::cell_mesh(&cell, origin, &|lng, lat| this.ground_height(lng, lat)).map(Arc::new);
                self.cell_meshes.insert(
                    (x, y),
                    CellMesh {
                        cell: cell.clone(),
                        mesh,
                        used: self.frame,
                    },
                );
            }
            let Some(entry) = self.cell_meshes.get_mut(&(x, y)) else {
                continue;
            };
            entry.used = self.frame;
            if let Some(mesh) = &entry.mesh {
                items.push(gpu::Item {
                    mesh: mesh.clone(),
                    origin: tile_mercator(buildings::CELL_Z, f64::from(x), f64::from(y)),
                    texture: None,
                    uv: [0.0, 0.0, 1.0, 1.0],
                    flat: false,
                    color: buildings::color(),
                    alpha: buildings::opacity(),
                });
            }
        }
        if self.cell_meshes.len() > buildings::MAX_CELLS * 4 {
            let frame = self.frame;
            self.cell_meshes.retain(|_, m| m.used + 30 >= frame);
        }
        items
    }

    /// The loaded terrain's height at a position, `None` while none is here.
    fn ground_height(&self, lng: f64, lat: f64) -> Option<f64> {
        self.terrain
            .as_ref()
            .and_then(|t| t.height_at(lng, lat, 13.0))
            .or_else(|| self.fallback.as_ref().and_then(|t| t.height_at(lng, lat, 13.0)))
    }

    /// The elevation credit and the ground height at the centre, a pill above
    /// the map credit at the bottom left.
    fn paint_credit(&self, ui: &egui::Ui, rect: Rect) {
        let Some(credit) = self.terrain_credit() else {
            return;
        };
        let text = format!("{:.0} m  \u{b7}  {credit}", self.cam.ground);
        let g = crate::ui::style::galley(
            ui,
            &text,
            10.0,
            crate::ui::style::Weight::W400,
            crate::ui::style::text_2(),
            0.0,
        );
        let w = g.size().x.min(rect.width() - 28.0 - 13.0);
        let pill = Rect::from_min_size(
            Pos2::new(rect.left() + 14.0, rect.bottom() - 14.0 - 25.0 - 6.0 - 25.0),
            Vec2::new(w + 13.0, 25.0),
        );
        let painter = ui.painter_at(rect);
        crate::ui::style::glass_panel(&painter, "elevation-credit", pill, egui::CornerRadius::same(255), 0.38);
        ui.painter_at(pill.shrink2(Vec2::new(5.0, 0.0))).galley(
            Pos2::new(pill.left() + 9.0, pill.center().y - g.size().y / 2.0),
            g,
            crate::ui::style::text_2(),
        );
    }

    fn screen_of(&self, origin: Pos2, p: V3) -> Option<Pos2> {
        self.cam
            .project(p)
            .map(|s| Pos2::new(origin.x + s.x as f32, origin.y + s.y as f32))
    }

    /// The longitude and latitude of the ground under screen point `p`, on
    /// the plane of the centre's ground.
    pub fn pick(&self, rect: Rect, p: Pos2) -> Option<[f64; 2]> {
        let (e, n) = self
            .cam
            .ground_at(f64::from(p.x - rect.min.x), f64::from(p.y - rect.min.y), 0.0)?;
        let (lng, lat) = self.cam.geo_of(e, n);
        Some([lng, lat])
    }

    /// Where a place on the ground is on screen, when the camera sees it.
    pub fn place_on_screen(&self, rect: Rect, p: [f64; 2]) -> Option<Pos2> {
        let h = self.ground_at(p[0], p[1]);
        self.screen_of(rect.min, self.cam.local_of(p[0], p[1], h))
    }

    /// Whether the ground between the camera and `target` (local metres) is
    /// higher than the line of sight: a hill in front of it.
    fn hidden(&self, target: V3) -> bool {
        let view = self.cam.view();
        const STEPS: usize = 48;
        for i in 1..STEPS {
            let t = i as f64 / STEPS as f64;
            if t > 0.97 {
                break;
            }
            let p = [
                view.pos[0] + (target[0] - view.pos[0]) * t,
                view.pos[1] + (target[1] - view.pos[1]) * t,
                view.pos[2] + (target[2] - view.pos[2]) * t,
            ];
            let (lng, lat) = self.cam.geo_of(p[0], p[1]);
            let terrain = self.ground_at(lng, lat) - self.cam.ground;
            if terrain > p[2] + 1.0 {
                return true;
            }
        }
        false
    }

    /// The flown track as ribbons the depth test can hide behind a hill: a
    /// dark casing and the colour on top, earlier flights quieter.
    fn line_items(&self, layers: &Layers3d<'_>) -> Vec<gpu::Item> {
        let rgb = |c: Color32| {
            [
                f32::from(c.r()) / 255.0,
                f32::from(c.g()) / 255.0,
                f32::from(c.b()) / 255.0,
            ]
        };
        let centre = crate::map3d::camera::mercator(self.cam.lng, self.cam.lat);
        let mut items = Vec::new();
        let mut add = |pts: &[Point], color: Color32, past: bool| {
            let local: Vec<V3> = track::smooth(pts)
                .iter()
                .map(|p| {
                    self.cam
                        .local_of(p.lng, p.lat, self.ground_at(p.lng, p.lat) + p.alt.unwrap_or(0.0))
                })
                .collect();
            let (casing, casing_a, core, core_a) = if past {
                (4.6, 0.45, 2.2, 0.85)
            } else {
                (5.5, 0.55, 2.6, 0.95)
            };
            for (width, color, alpha) in [(casing, [0.0, 0.0, 0.0], casing_a), (core, rgb(color), core_a)] {
                if let Some((verts, indices)) = ribbon::build(&local, width, &self.cam, self.cam.ground) {
                    items.push(gpu::Item {
                        mesh: Arc::new(gpu::MeshData::new(verts, indices)),
                        origin: centre,
                        texture: None,
                        uv: [0.0, 0.0, 1.0, 1.0],
                        flat: false,
                        color,
                        alpha,
                    });
                }
            }
        };
        for flight in &layers.track.past {
            add(flight, layers.past_color, true);
        }
        add(&layers.track.current, layers.track_color, false);
        items
    }

    fn paint_layers(&self, painter: &egui::Painter, origin: Pos2, layers: &Layers3d<'_>) {
        let cam = &self.cam;
        if let Some((lng, lat)) = layers.home {
            let at = cam.local_of(lng, lat, self.ground_at(lng, lat));
            if let (false, Some(p)) = (self.hidden(at), self.screen_of(origin, at)) {
                crate::map::paint_home(painter, p);
            }
        }
        if let Some((lng, lat)) = layers.vehicle {
            let base = self.ground_at(lng, lat);
            let ground = cam.local_of(lng, lat, base);
            let air = cam.local_of(lng, lat, base + layers.height);
            if let (false, Some(g), Some(a)) = (
                self.hidden(air),
                self.screen_of(origin, ground),
                self.screen_of(origin, air),
            ) {
                painter.line_segment([g, a], Stroke::new(1.0, Color32::from_black_alpha(110)));
                painter.circle_filled(g, 3.0, Color32::from_black_alpha(90));
                crate::map::paint_vehicle_in(
                    painter,
                    a,
                    layers.heading - cam.bearing as f32,
                    layers.vehicle_color,
                    1.0,
                    true,
                    1.0,
                );
            }
        }
    }
}

/// The fog over the far ground at the horizon (`fog-ground-blend`): a band
/// that fades from the sky's colour into the ground below it.
fn haze(painter: &egui::Painter, rect: Rect, horizon: f32) {
    let band = (rect.height() * 0.12).min(rect.bottom() - horizon);
    if band <= 1.0 {
        return;
    }
    let fog = crate::ui::style::bg();
    let mut m = Mesh::default();
    let top = Color32::from_rgba_unmultiplied(fog.r(), fog.g(), fog.b(), 150);
    let none = Color32::from_rgba_unmultiplied(fog.r(), fog.g(), fog.b(), 0);
    for (y, c) in [(horizon, top), (horizon + band, none)] {
        m.colored_vertex(Pos2::new(rect.left(), y), c);
        m.colored_vertex(Pos2::new(rect.right(), y), c);
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(m));
}

/// The mesh of a leaf: a `GRID` by `GRID` height field with a skirt hanging
/// from each edge, around the leaf's north-west corner. Texture coordinates
/// run 0 to 1 over the leaf; the shade bakes the hillshade in.
fn build_leaf(l: Leaf, dem: Option<&Dem>, k: f64) -> MeshData {
    let n = GRID + 1;
    let mut heights = vec![0f32; n * n];
    if let Some((tile, (u0, v0, size))) = dem {
        for j in 0..n {
            for i in 0..n {
                let (u, v) = (i as f64 / GRID as f64, j as f64 / GRID as f64);
                heights[j * n + i] = tile.sample(u0 + u * size, v0 + v * size);
            }
        }
    }
    let side = 2.0 * std::f64::consts::PI * crate::map3d::camera::R / 2f64.powi(i32::from(l.z));
    let step = side * k / GRID as f64;
    let shade_at = |i: usize, j: usize| -> f32 {
        if dem.is_none() {
            return 1.0;
        }
        let h = |i: usize, j: usize| f64::from(heights[j * n + i]);
        let (i0, i1) = (i.saturating_sub(1), (i + 1).min(GRID));
        let (j0, j1) = (j.saturating_sub(1), (j + 1).min(GRID));
        let dzde = (h(i1, j) - h(i0, j)) / ((i1 - i0) as f64 * step);
        let dzdn = -(h(i, j1) - h(i, j0)) / ((j1 - j0) as f64 * step);
        let len = (dzde * dzde + dzdn * dzdn + 1.0).sqrt();
        let lit = (-dzde * LIGHT[0] - dzdn * LIGHT[1] + LIGHT[2]) / len;
        1.0 - SHADE * ((LIGHT[2] - lit) / LIGHT[2]).clamp(0.0, 1.0) as f32
    };
    let vertex = |i: usize, j: usize, drop: f32, shade: f32| -> [f32; FLOATS] {
        let (u, v) = (i as f32 / GRID as f32, j as f32 / GRID as f32);
        [
            (f64::from(u) * side) as f32,
            (-f64::from(v) * side) as f32,
            heights[j * n + i] - drop,
            u,
            v,
            shade,
        ]
    };
    let mut verts: Vec<f32> = Vec::with_capacity(n * n * FLOATS * 2);
    let mut indices: Vec<u32> = Vec::with_capacity(GRID * GRID * 6 + GRID * 24);
    for j in 0..n {
        for i in 0..n {
            verts.extend_from_slice(&vertex(i, j, 0.0, shade_at(i, j)));
        }
    }
    for j in 0..GRID {
        for i in 0..GRID {
            let a = (j * n + i) as u32;
            let (b, c, d) = (a + 1, a + n as u32 + 1, a + n as u32);
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    // Skirts: the edges of a leaf hang down, so a neighbour from another
    // elevation model or another level of detail leaves no crack between.
    let drop = (step * GRID as f64 * 0.05).max(25.0) as f32;
    let mut edges: Vec<((usize, usize), (usize, usize))> = Vec::new();
    for t in 0..GRID {
        edges.push(((t, 0), (t + 1, 0)));
        edges.push(((t, GRID), (t + 1, GRID)));
        edges.push(((0, t), (0, t + 1)));
        edges.push(((GRID, t), (GRID, t + 1)));
    }
    for ((ai, aj), (bi, bj)) in edges {
        let base = (verts.len() / FLOATS) as u32;
        for (i, j, dr) in [(ai, aj, 0.0), (bi, bj, 0.0), (bi, bj, drop), (ai, aj, drop)] {
            verts.extend_from_slice(&vertex(i, j, dr, shade_at(i, j) * 0.75));
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    MeshData::new(verts, indices)
}

/// The part of ancestor tile at zoom `z` that leaf `l` covers, in texture
/// coordinates.
fn uv_of(l: Leaf, z: u8) -> Rect {
    let d = l.z - z.min(l.z);
    let n = (1u32 << d) as f32;
    let (fx, fy) = ((l.x & ((1 << d) - 1)) as f32, (l.y & ((1 << d) - 1)) as f32);
    Rect::from_min_size(Pos2::new(fx / n, fy / n), Vec2::splat(1.0 / n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leaf_mesh_is_a_height_field_with_skirts_and_sound_indices() {
        let size = 8;
        // A ramp rising to the east: 0 m at the west edge, 700 m at the east.
        let heights: Vec<f32> = (0..size * size).map(|i| (i % size) as f32 * 100.0).collect();
        let dem: Dem = (Arc::new(DemTile::from_heights(size, heights)), (0.0, 0.0, 1.0));
        let leaf = Leaf { z: 10, x: 540, y: 355 };
        let m = build_leaf(leaf, Some(&dem), 0.67);
        let n = GRID + 1;
        let verts = m.verts.len() / FLOATS;
        assert_eq!(verts, n * n + GRID * 4 * 4, "the grid and four vertices a skirt quad");
        assert_eq!(m.indices.len(), GRID * GRID * 6 + GRID * 4 * 6);
        assert!(m.indices.iter().all(|i| (*i as usize) < verts));
        let z = |i: usize| m.verts[i * FLOATS + 2];
        assert_eq!(z(0), 0.0, "the west edge is at 0 m");
        assert_eq!(z(GRID), 700.0, "the east edge is at 700 m");
        let first_skirt = n * n;
        assert!(z(first_skirt + 2) < z(first_skirt), "a skirt hangs below its edge");
        let shade = |i: usize| m.verts[i * FLOATS + 5];
        assert!(shade(n + 3) <= 1.0 && shade(n + 3) > 0.0);
        let flat = build_leaf(leaf, None, 0.67);
        assert!(
            (0..n * n).all(|i| flat.verts[i * FLOATS + 5] == 1.0),
            "no elevation, no shading"
        );
    }

    #[test]
    fn a_leaf_covers_its_part_of_an_ancestor() {
        let whole = uv_of(Leaf { z: 5, x: 3, y: 4 }, 5);
        assert_eq!((whole.min, whole.size()), (Pos2::ZERO, Vec2::splat(1.0)));
        let quarter = uv_of(Leaf { z: 6, x: 7, y: 8 }, 5);
        assert_eq!((quarter.min, quarter.size()), (Pos2::new(0.5, 0.0), Vec2::splat(0.5)));
        let sixteenth = uv_of(Leaf { z: 7, x: 13, y: 14 }, 5);
        assert_eq!(
            (sixteenth.min, sixteenth.size()),
            (Pos2::new(0.25, 0.5), Vec2::splat(0.25))
        );
    }

    #[test]
    fn tile_corners_are_on_the_mercator_grid() {
        let (x, y) = tile_mercator(0, 0.0, 0.0);
        let half = std::f64::consts::PI * crate::map3d::camera::R;
        assert!((x + half).abs() < 1e-6 && (y - half).abs() < 1e-6);
        let (x, y) = tile_mercator(1, 1.0, 1.0);
        assert!(x.abs() < 1e-6 && y.abs() < 1e-6);
    }
}
