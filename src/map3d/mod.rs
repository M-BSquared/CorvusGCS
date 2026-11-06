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

use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Shape, Stroke, Vec2};
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
/// Leaf meshes built in one frame at most; the rest wait for the next, nearest first.
const MESH_BUILDS_PER_FRAME: usize = 40;
/// The highest ground a leaf may have, in metres above sea level, for knowing
/// whether it is in view before its elevation is here.
const HIGHEST_GROUND: f64 = 5000.0;
/// Heights over the view centre's ground, in metres, the ground box also reaches to.
const RELIEF: [f64; 2] = [-600.0, 2500.0];
/// A side of the geofence is cut into pieces about this long (metres) to test them for hills.
const FENCE_PIECE_M: f64 = 40.0;
/// How far above the ground under it the camera is kept, in metres.
const KEEP_ABOVE_GROUND: f64 = 8.0;
const TEXTURES: usize = 400;
const IMAGE_CACHE: usize = 1000;
/// Decoded imagery held at most, in bytes: tiles are 256 pixels at four bytes,
/// but a source may serve larger ones, and a count alone would not hold then.
const IMAGE_BYTES: usize = 160 << 20;
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
    /// What the plugins draw.
    pub shapes: &'a [crate::plugins::shapes::Shape],
    /// The flown vehicle's own colour, and its track's: the theme's unless
    /// the operator chose one.
    pub vehicle_color: Color32,
    pub track_color: Color32,
    /// One colour per earlier flight, oldest first.
    pub past_colors: Vec<Color32>,
    /// A thin line at the height flown, above the track on the ground.
    pub in_air: bool,
    /// The geofence area when the Home map is asked to show it.
    pub fence: Option<&'a [[f64; 2]]>,
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

/// What the 3D map says when this OpenGL cannot draw it.
pub fn failure_text(why: &str) -> String {
    crate::i18n::tr_fmt(FAILURE_TEXT, &[("a", why)])
}

impl Leaf {
    /// The column, counted from the main copy of the world's west edge.
    fn x_world(&self) -> f64 {
        f64::from(self.x) + f64::from(self.wrap) * 2f64.powi(i32::from(self.z))
    }
}

/// How long the ground waits for imagery before the map says it has none.
const OFFLINE_AFTER_MS: u64 = 2500;
/// A native only text (`lang/native`).
const OFFLINE_TEXT: &str = "No map saved for this area. It is not available offline.";
/// A native only text (`lang/native`).
const FAILURE_TEXT: &str = "The 3D map cannot draw on this OpenGL: {a}";

/// The base layer's tiles as a decoded image.
fn decode_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// What a decoded tile image holds in memory.
fn image_bytes(image: &egui::ColorImage) -> usize {
    image.pixels.len() * std::mem::size_of::<egui::Color32>()
}

/// One tile of the ground to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Leaf {
    z: u8,
    x: u32,
    y: u32,
    /// Which copy of the world it is in: -1 west of the antimeridian's far
    /// side, 0 the main one, 1 east of it.
    wrap: i32,
}

/// The 3D view's state.
pub struct Map3d {
    pub cam: Camera,
    /// Height of the map credit under the elevation pill, set by the map.
    pub credit_h: f32,
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
    /// The numbers of the last frame over the map (F3, or `CORVUS_GUI_3D_DEBUG`).
    debug: bool,
    logged: std::time::Instant,
    /// Since when most of the ground has had no image and the backend has
    /// failed to give some: the map has nothing saved here.
    blank_since: Option<std::time::Instant>,
}

/// An elevation tile and the part `(u0, v0, size)` of it a leaf covers.
type Dem = (Arc<DemTile>, (f64, f64, f64));

/// A leaf's images (its own, then its fallback) and its elevation.
type Wanted = ([Option<(gpu::Texture, Rect)>; 2], Option<Dem>);

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
            credit_h: 25.0,
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
            imagery: TileCache::weighed(ctx, server, IMAGE_CACHE, image_bytes, IMAGE_BYTES, decode_image),
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
            debug: std::env::var_os("CORVUS_GUI_3D_DEBUG").is_some(),
            logged: std::time::Instant::now(),
            blank_since: None,
        }
    }

    /// Takes the view of the flat map.
    pub fn start_at(&mut self, lng: f64, lat: f64, zoom: f64, bearing: f64) {
        self.cam.set_centre((lng, lat));
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

    /// Lets go of everything the view holds for drawing, for when it is left:
    /// tiles, meshes and (through the GPU's own release) the textures. What
    /// the operator chose stays.
    pub fn release(&mut self) {
        self.imagery.clear();
        for t in self.terrain.iter().chain(self.fallback.iter()) {
            t.clear();
        }
        if let Some(b) = &self.buildings {
            b.clear();
        }
        self.tex_ids.clear();
        self.used.clear();
        self.leaf_meshes.clear();
        self.cell_meshes.clear();
        gpu::request_release();
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
            let flat = c.ground_at(sx, sy, 0.0).unwrap_or_else(|| {
                let b = c.bearing.to_radians();
                (b.sin() * reach, b.cos() * reach)
            });
            // The ground is not a plane: a ridge near the bottom of the view
            // meets the ray sooner than the plane does, a valley later.
            let mut hits = vec![flat];
            hits.extend(RELIEF.iter().filter_map(|up| c.ground_at(sx, sy, *up)));
            for (e, n) in hits {
                let (e, n) = (e.clamp(-reach, reach), n.clamp(-reach, reach));
                min = (min.0.min(e), min.1.min(n));
                max = (max.0.max(e), max.1.max(n));
            }
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
        let clampy = |v: f64| (v.floor().max(0.0) as u32).min(n_tiles as u32 - 1);
        // Columns run past the edges of the world: the map repeats there.
        let columns = |lo: f64, hi: f64| {
            (lo.floor() as i64)..=(hi.floor() as i64).min(lo.floor() as i64 + 2 * n_tiles as i64 + 2)
        };
        let mut out = Vec::new();
        for ty in clampy(y0)..=clampy(y1) {
            for tx in columns(x0, x1) {
                let n = n_tiles as i64;
                self.split(
                    Leaf {
                        z: z0,
                        x: tx.rem_euclid(n) as u32,
                        y: ty,
                        wrap: tx.div_euclid(n) as i32,
                    },
                    &mut out,
                );
            }
        }
        out
    }

    fn corners(&self, l: Leaf) -> [V3; 4] {
        let c = &self.cam;
        let k = c.lat.to_radians().cos();
        let (cx, cy) = crate::map3d::camera::mercator(c.lng, c.lat);
        let at = |dx: f64, dy: f64| {
            let (mx, my) = tile_mercator(l.z, l.x_world() + dx, l.y as f64 + dy);
            [(mx - cx) * k, (my - cy) * k, -c.ground]
        };
        [at(0.0, 0.0), at(1.0, 0.0), at(1.0, 1.0), at(0.0, 1.0)]
    }

    fn split(&self, l: Leaf, out: &mut Vec<Leaf>) {
        let pts = self.corners(l);
        // The tile is on the sea level plane here, the ground over it up to
        // the highest mountains: it is in view if any of that is.
        let mut slab = pts.to_vec();
        slab.extend(pts.iter().map(|p| [p[0], p[1], p[2] + HIGHEST_GROUND]));
        if !self.cam.sees(&slab) {
            return;
        }
        let c = &self.cam;
        let k = c.lat.to_radians().cos();
        let metres = 2.0 * std::f64::consts::PI * crate::map3d::camera::R * k / 2f64.powi(l.z as i32);
        // How far along the view the tile is, nearest corner, but never
        // nearer than a third of the camera's distance: a tile under the
        // camera is split as far as one just in front of it, not endlessly.
        let depth = pts
            .iter()
            .map(|p| c.clip(*p)[2])
            .fold(f64::MAX, f64::min)
            .max(c.distance() * 0.3);
        let px = metres / depth * c.focal();
        if px > SPLIT_PX && l.z < self.max_zoom && out.len() < MAX_LEAVES {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                self.split(
                    Leaf {
                        z: l.z + 1,
                        x: l.x * 2 + dx,
                        y: l.y * 2 + dy,
                        wrap: l.wrap,
                    },
                    out,
                );
            }
        } else {
            out.push(l);
        }
    }

    /// The id the GPU knows a tile's image by.
    fn texture_of(&mut self, key: &Key, image: Arc<egui::ColorImage>) -> gpu::Texture {
        let id = match self.tex_ids.get(key) {
            Some((held, id)) if Arc::ptr_eq(held, &image) => *id,
            _ => {
                let id = gpu::next_id();
                self.tex_ids.insert(key.clone(), (image.clone(), id));
                id
            }
        };
        self.used.push(key.clone());
        gpu::Texture { id, image }
    }

    /// The imagery of a leaf or its nearest ancestor that is here, and the
    /// part of it the leaf covers; then the next ancestor that is here, which
    /// the GPU draws until the first is uploaded.
    fn imagery_for(&mut self, l: Leaf) -> [Option<(gpu::Texture, Rect)>; 2] {
        let top = l.z.min(self.max_zoom);
        let mut want = true;
        // A coarse ancestor is asked for too, so there is always something to
        // draw while the fine tile is on its way.
        let coarse = top.min(3);
        let mut found: [Option<(gpu::Texture, Rect)>; 2] = [None, None];
        let mut n = 0;
        for d in 0..=top.min(12) {
            if d == coarse && d > 0 {
                want = true;
            }
            let (z, x, y) = (top - d, l.x >> d, l.y >> d);
            let key: Key = (self.source.clone(), z, x, y);
            let image = if want {
                self.imagery.get(&key)
            } else {
                self.imagery.peek(&key)
            };
            want = false;
            if let Some(image) = image {
                let t = self.texture_of(&key, image);
                found[n] = Some((t, uv_of(l, z)));
                n += 1;
                if n == 2 {
                    break;
                }
            }
        }
        found
    }

    /// The elevation for a leaf from the model in use, else the default.
    fn dem_for(&self, l: Leaf) -> Option<Dem> {
        self.terrain
            .as_ref()
            .and_then(|t| t.tile_for_leaf(l.z, l.x, l.y))
            .or_else(|| self.fallback.as_ref().and_then(|t| t.tile_for_leaf(l.z, l.x, l.y)))
    }

    /// The mesh of a leaf: made once, again only when its elevation changes.
    /// Past this frame's build budget a leaf keeps the mesh it had, or waits.
    fn leaf_mesh(&mut self, l: Leaf, dem: Option<Dem>, builds: &mut usize) -> Option<(Arc<MeshData>, bool)> {
        let key = (l.z, l.x, l.y);
        if let Some(m) = self.leaf_meshes.get_mut(&key) {
            let same = match (&m.dem, &dem) {
                (None, None) => true,
                (Some((a, ra)), Some((b, rb))) => Arc::ptr_eq(a, b) && ra == rb,
                _ => false,
            };
            m.used = self.frame;
            if same || *builds >= MESH_BUILDS_PER_FRAME {
                return Some((m.mesh.clone(), m.dem.is_none()));
            }
        }
        if *builds >= MESH_BUILDS_PER_FRAME {
            return None;
        }
        *builds += 1;
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
        Some((mesh, dem.is_none()))
    }

    /// The items of the ground in view, and how many meshes were built for
    /// them. Leaves nearest the view centre come first, so what is built late
    /// is what is far away.
    fn ground_items(&mut self, leaves: &[Leaf]) -> (Vec<gpu::Item>, usize, usize) {
        self.used.clear();
        let mut items = Vec::with_capacity(leaves.len());
        let mut builds = 0;
        let mut blank = 0;
        let (cx, cy) = crate::map3d::camera::mercator(self.cam.lng, self.cam.lat);
        let world = 2.0 * std::f64::consts::PI * crate::map3d::camera::R;
        let mut order: Vec<(f64, Leaf)> = leaves
            .iter()
            .map(|l| {
                let side = world / 2f64.powi(i32::from(l.z));
                let (mx, my) = tile_mercator(l.z, l.x_world() + 0.5, f64::from(l.y) + 0.5);
                ((mx - cx).hypot(my - cy) / side, *l)
            })
            .collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        // The tile cache serves the last asked first, so the tiles are asked
        // for from the far leaves to the near ones: what is under the eye
        // arrives first.
        let mut wanted: HashMap<(u8, u32, u32, i32), Wanted> = HashMap::new();
        for (_, l) in order.iter().rev() {
            let imagery = self.imagery_for(*l);
            wanted.insert((l.z, l.x, l.y, l.wrap), (imagery, self.dem_for(*l)));
        }
        for (_, l) in order {
            let Some(([first, second], dem)) = wanted.remove(&(l.z, l.x, l.y, l.wrap)) else {
                continue;
            };
            blank += usize::from(first.is_none() && second.is_none());
            let Some((mesh, flat)) = self.leaf_mesh(l, dem, &mut builds) else {
                continue;
            };
            let (mx, my) = tile_mercator(l.z, l.x_world(), f64::from(l.y));
            let rect_uv = |r: &Rect| [r.min.x, r.min.y, r.width(), r.height()];
            let (texture, uv) = match &first {
                Some((t, r)) => (Some(t.clone()), rect_uv(r)),
                None => (None, [0.0, 0.0, 1.0, 1.0]),
            };
            let fallback = second.map(|(t, r)| (t, rect_uv(&r)));
            items.push(gpu::Item {
                mesh,
                origin: (mx, my),
                texture,
                uv,
                fallback,
                flat,
                color: [
                    f32::from(GROUND_COLOR[0]) / 255.0,
                    f32::from(GROUND_COLOR[1]) / 255.0,
                    f32::from(GROUND_COLOR[2]) / 255.0,
                ],
                alpha: 1.0,
                transient: false,
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
        (items, builds, blank)
    }

    /// Draws the view into `rect`.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, layers: &Layers3d<'_>) -> Input {
        self.cam.size = (rect.width() as f64, rect.height() as f64);
        let resp = ui.interact(rect, ui.scope_id().with("map3d"), egui::Sense::click_and_drag());
        if resp.hovered() && ui.input(|i| i.key_pressed(egui::Key::F3)) {
            self.debug = !self.debug;
        }
        let origin = rect.min;
        let local = |p: Pos2| ((p.x - origin.x) as f64, (p.y - origin.y) as f64);
        let mut dragged = false;
        let touch = ui.input(|i| i.multi_touch());
        if let Some(t) = touch {
            // Two fingers (tablet mode): pinch zooms to the fingers, twisting
            // turns the bearing, sliding up and down tilts.
            let at = local(t.center_pos);
            if (t.zoom_delta - 1.0).abs() > 1e-4 {
                let zoom = (self.cam.zoom + f64::from(t.zoom_delta).log2())
                    .clamp(crate::map3d::camera::MIN_ZOOM, self.max_zoom as f64 + 2.0);
                self.cam.zoom_at(at.0, at.1, zoom);
            }
            self.cam.bearing = (self.cam.bearing - f64::from(t.rotation_delta).to_degrees()).rem_euclid(360.0);
            self.cam.turn(0.0, f64::from(t.translation_delta.y) * 0.5);
            dragged = true;
        }
        if touch.is_none() && resp.dragged_by(egui::PointerButton::Primary) {
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
            if let (true, Some(p)) = (scroll != 0.0, ui.input(|i| i.pointer.hover_pos())) {
                let at = local(p);
                let zoom = (self.cam.zoom + scroll as f64 * 0.004)
                    .clamp(crate::map3d::camera::MIN_ZOOM, self.max_zoom as f64 + 2.0);
                self.cam.zoom_at(at.0, at.1, zoom);
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
        let planning = std::time::Instant::now();
        {
            let (terrain, fallback) = (&self.terrain, &self.fallback);
            self.cam.keep_above(
                |lng, lat| {
                    terrain
                        .iter()
                        .chain(fallback.iter())
                        .find_map(|t| t.height_at(lng, lat, 13.0))
                },
                KEEP_ABOVE_GROUND,
            );
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
        let (ground_items, built, blank) = self.ground_items(&leaves);
        self.watch_blank(ui, rect, leaves.len(), blank);
        let building_items = self.building_items();
        self.imagery.trim();
        for t in self.terrain.iter().chain(self.fallback.iter()) {
            t.trim();
        }
        let line_items = self.line_items(layers);
        gpu::note_plan(leaves.len(), built, planning.elapsed().as_secs_f32() * 1000.0);
        if built >= MESH_BUILDS_PER_FRAME || gpu::deferred() {
            ui.ctx().request_repaint();
        }
        let scene = Scene {
            mvp: self.cam.view_projection(),
            centre: crate::map3d::camera::mercator(self.cam.lng, self.cam.lat),
            k: self.cam.lat.to_radians().cos(),
            ground: self.cam.ground,
            ground_items,
            building_items,
            line_items,
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
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                failure_text(&why),
                egui::FontId::proportional(13.0),
                crate::ui::style::text_2(),
            );
        }
        self.paint_layers(ui, &painter, rect.min, layers);
        self.paint_credit(ui, rect);
        if self.debug {
            self.paint_debug(ui, &painter, rect);
        }
        let secondary = resp.secondary_clicked();
        let click = (resp.clicked() || secondary)
            .then(|| resp.interact_pointer_pos())
            .flatten()
            .and_then(|p| self.pick(rect, p))
            .map(|g| (g, secondary));
        let press = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();
        Input { dragged, click, press }
    }

    /// Says so when the ground has stayed without imagery and the backend
    /// has had none to give: offline with nothing saved for this place.
    fn watch_blank(&mut self, ui: &egui::Ui, rect: Rect, leaves: usize, blank: usize) {
        let empty = blank * 2 > leaves && self.imagery.failures() > 0;
        if !empty {
            self.blank_since = None;
            return;
        }
        let since = *self.blank_since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() < std::time::Duration::from_millis(OFFLINE_AFTER_MS) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(OFFLINE_AFTER_MS));
            return;
        }
        let painter = ui.painter_at(rect);
        let g = crate::ui::style::galley(
            ui,
            OFFLINE_TEXT,
            13.0,
            crate::ui::style::Weight::W500,
            crate::ui::style::text_2(),
            0.0,
        );
        let pill = Rect::from_center_size(
            Pos2::new(rect.center().x, rect.center().y + rect.height() * 0.18),
            g.size() + Vec2::new(32.0, 18.0),
        );
        crate::ui::style::glass_panel(&painter, "offline-3d", pill, egui::CornerRadius::same(255), 0.5);
        painter.galley(
            Pos2::new(pill.center().x - g.size().x / 2.0, pill.center().y - g.size().y / 2.0),
            g,
            crate::ui::style::text_2(),
        );
    }

    /// The counters of the last frame in a corner, and on stderr every 5 s.
    fn paint_debug(&mut self, ui: &egui::Ui, painter: &egui::Painter, rect: Rect) {
        let s = gpu::stats();
        let dt = ui.input(|i| i.unstable_dt) * 1000.0;
        let text = format!(
            "frame {dt:.1} ms  plan {:.1} ms  gl {:.1} ms\nleaves {}  built {}  draws {} ({} imaged)\nmeshes +{} live {}  textures +{} live {}\nribbons {}  deferred {}\nzoom {:.2}  pitch {:.0}  bearing {:.0}",
            s.plan_ms,
            s.submit_ms,
            s.leaves,
            s.built,
            s.draws,
            s.textured,
            s.meshes_uploaded,
            s.live_meshes,
            s.textures_uploaded,
            s.live_textures,
            s.transient,
            s.deferred,
            self.cam.zoom,
            self.cam.pitch,
            self.cam.bearing,
        );
        if self.logged.elapsed().as_secs() >= 5 {
            self.logged = std::time::Instant::now();
            eprintln!("corvus-gcs 3d: {}", text.replace('\n', " | "));
        }
        let galley = painter.layout(text, egui::FontId::monospace(11.0), Color32::WHITE, rect.width() - 28.0);
        let at = Rect::from_min_size(Pos2::new(rect.left() + 14.0, rect.top() + 70.0), galley.size());
        painter.rect_filled(at.expand(6.0), 6, Color32::from_black_alpha(160));
        painter.galley(at.min, galley, Color32::WHITE);
        ui.ctx().request_repaint();
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
                    fallback: None,
                    flat: false,
                    color: buildings::color(),
                    alpha: buildings::opacity(),
                    transient: false,
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
            Pos2::new(rect.left() + 14.0, rect.bottom() - 14.0 - self.credit_h - 6.0 - 25.0),
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
        let at = self.cam.local_of(p[0], p[1], h);
        if self.hidden(at) {
            return None;
        }
        self.screen_of(rect.min, at)
    }

    /// Whether the ground between the camera and `target` (local metres) is
    /// higher than the line of sight: a hill in front of it.
    fn hidden(&self, target: V3) -> bool {
        line_blocked(&self.cam, target, &|lng, lat| self.ground_at(lng, lat))
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
        let mut add = |pts: &[Point], color: Color32, past: bool, air: bool| {
            let local: Vec<V3> = track::smooth(pts)
                .iter()
                .map(|p| {
                    let up = if air { p.alt.unwrap_or(0.0) } else { 0.0 };
                    self.cam.local_of(p.lng, p.lat, self.ground_at(p.lng, p.lat) + up)
                })
                .collect();
            let (casing, casing_a, core, core_a) = match (past, air) {
                (_, true) => (2.4, 0.4, 1.2, 0.8),
                (true, false) => (4.6, 0.45, 2.2, 0.85),
                (false, false) => (5.5, 0.55, 2.6, 0.95),
            };
            for (width, color, alpha) in [(casing, [0.0, 0.0, 0.0], casing_a), (core, rgb(color), core_a)] {
                if let Some((verts, indices)) = ribbon::build(&local, width, &self.cam, self.cam.ground) {
                    items.push(gpu::Item {
                        mesh: Arc::new(gpu::MeshData::new(verts, indices)),
                        origin: centre,
                        texture: None,
                        uv: [0.0, 0.0, 1.0, 1.0],
                        fallback: None,
                        flat: false,
                        color,
                        alpha,
                        transient: true,
                    });
                }
            }
        };
        for (flight, color) in layers.track.past.iter().zip(&layers.past_colors) {
            add(flight, *color, true, false);
            if layers.in_air {
                add(flight, *color, true, true);
            }
        }
        add(&layers.track.current, layers.track_color, false, false);
        if layers.in_air {
            add(&layers.track.current, layers.track_color, false, true);
        }
        items
    }

    fn paint_layers(&self, ui: &egui::Ui, painter: &egui::Painter, origin: Pos2, layers: &Layers3d<'_>) {
        let cam = &self.cam;
        let project = |p: [f64; 2]| self.screen_of(origin, cam.local_of(p[0], p[1], self.ground_at(p[0], p[1])));
        crate::plugins::shapes::paint_areas(painter, &project, layers.shapes);
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
        if let Some(fence) = layers.fence.filter(|f| f.len() >= 3) {
            self.paint_fence(painter, origin, fence);
        }
        let project_mark = |p: [f64; 2]| {
            let at = cam.local_of(p[0], p[1], self.ground_at(p[0], p[1]));
            if self.hidden(at) {
                None
            } else {
                self.screen_of(origin, at)
            }
        };
        crate::plugins::shapes::paint_marks(ui, painter, &project_mark, layers.shapes, None);
    }

    /// The geofence on the ground (`setPolygonOverlay`: warning colour, 3 px,
    /// dashed 2 to 1.5, a 6 percent wash): each side is cut into short pieces
    /// and the ones behind a hill are left out, as the track is.
    fn paint_fence(&self, painter: &egui::Painter, origin: Pos2, fence: &[[f64; 2]]) {
        let warning = crate::ui::style::warning();
        let mut runs: Vec<Vec<Pos2>> = vec![Vec::new()];
        let mut seen = 0usize;
        let mut total = 0usize;
        for i in 0..fence.len() {
            let (a, b) = (fence[i], fence[(i + 1) % fence.len()]);
            let local = |t: f64| {
                let (lng, lat) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
                self.cam.local_of(lng, lat, self.ground_at(lng, lat) + 1.0)
            };
            let (pa, pb) = (local(0.0), local(1.0));
            let metres = (pb[0] - pa[0]).hypot(pb[1] - pa[1]);
            let pieces = ((metres / FENCE_PIECE_M).ceil() as usize).clamp(1, 64);
            for k in 0..=pieces {
                let p = local(k as f64 / pieces as f64);
                total += 1;
                match self.screen_of(origin, p).filter(|_| !self.hidden(p)) {
                    Some(s) => {
                        seen += 1;
                        runs.last_mut().expect("a run is open").push(s);
                    }
                    None => {
                        if !runs.last().is_some_and(Vec::is_empty) {
                            runs.push(Vec::new());
                        }
                    }
                }
            }
        }
        for run in runs.iter().filter(|r| r.len() >= 2) {
            painter.extend(Shape::dashed_line(run, Stroke::new(3.0, warning), 6.0, 4.5));
        }
        if seen * 2 > total {
            let pts: Vec<Pos2> = fence
                .iter()
                .filter_map(|c| self.screen_of(origin, self.cam.local_of(c[0], c[1], self.ground_at(c[0], c[1]))))
                .collect();
            if pts.len() == fence.len() {
                let mut mesh = Mesh::default();
                for p in &pts {
                    mesh.colored_vertex(*p, warning.gamma_multiply(0.06));
                }
                for t in crate::setup::geofence_card::triangulate(&pts) {
                    mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
                }
                painter.add(mesh);
            }
        }
    }
}

/// Whether the ground, as `ground` gives its height above sea level at a
/// longitude and latitude, rises above the straight line from the camera to
/// `target` (local metres). The last 3 percent of the line is not tested, so
/// the ground a marker stands on does not hide it.
fn line_blocked(cam: &Camera, target: V3, ground: &dyn Fn(f64, f64) -> f64) -> bool {
    let view = cam.view();
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
        let (lng, lat) = cam.geo_of(p[0], p[1]);
        if ground(lng, lat) - cam.ground > p[2] + 1.0 {
            return true;
        }
    }
    false
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
    // One more row and column of heights all round, so the shade of an edge
    // vertex is read across the edge and matches the leaf next to it.
    let m = n + 2;
    let mut padded = vec![0f32; m * m];
    if let Some((tile, (u0, v0, size))) = dem {
        for j in 0..m {
            for i in 0..m {
                let (u, v) = ((i as f64 - 1.0) / GRID as f64, (j as f64 - 1.0) / GRID as f64);
                padded[j * m + i] = tile.sample(u0 + u * size, v0 + v * size);
            }
        }
    }
    let heights: Vec<f32> = (0..n * n).map(|q| padded[(q / n + 1) * m + q % n + 1]).collect();
    let side = 2.0 * std::f64::consts::PI * crate::map3d::camera::R / 2f64.powi(i32::from(l.z));
    let step = side * k / GRID as f64;
    let shade_at = |i: usize, j: usize| -> f32 {
        if dem.is_none() {
            return 1.0;
        }
        let h = |i: usize, j: usize| f64::from(padded[j * m + i]);
        let (pi, pj) = (i + 1, j + 1);
        let dzde = (h(pi + 1, pj) - h(pi - 1, pj)) / (2.0 * step);
        let dzdn = -(h(pi, pj + 1) - h(pi, pj - 1)) / (2.0 * step);
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
        let leaf = Leaf {
            z: 10,
            x: 540,
            y: 355,
            wrap: 0,
        };
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
    fn neighbouring_leaves_shade_their_shared_edge_alike() {
        let size = 64;
        let heights: Vec<f32> = (0..size * size)
            .map(|i| {
                let (x, y) = ((i % size) as f32, (i / size) as f32);
                300.0 * (x * 0.21).sin() + 200.0 * (y * 0.17).cos()
            })
            .collect();
        let tile = Arc::new(DemTile::from_heights(size, heights));
        let west: Dem = (tile.clone(), (0.0, 0.25, 0.5));
        let east: Dem = (tile, (0.5, 0.25, 0.5));
        let (a, b) = (
            build_leaf(
                Leaf {
                    z: 12,
                    x: 2184,
                    y: 1420,
                    wrap: 0,
                },
                Some(&west),
                0.68,
            ),
            build_leaf(
                Leaf {
                    z: 12,
                    x: 2185,
                    y: 1420,
                    wrap: 0,
                },
                Some(&east),
                0.68,
            ),
        );
        let n = GRID + 1;
        for j in 0..n {
            let (right_of_west, left_of_east) = (a.verts[(j * n + GRID) * FLOATS + 5], b.verts[(j * n) * FLOATS + 5]);
            assert!(
                (right_of_west - left_of_east).abs() < 1e-5,
                "row {j}: {right_of_west} against {left_of_east}"
            );
        }
    }

    fn view_at(lng: f64, lat: f64, zoom: f64, pitch: f64) -> Map3d {
        let ctx = egui::Context::default();
        let mut m = Map3d::new(&ctx, &Server::local(9), "satellite", 19);
        m.cam.set_centre((lng, lat));
        m.cam.zoom = zoom;
        m.cam.pitch = pitch;
        m
    }

    #[test]
    fn the_ground_continues_across_the_antimeridian() {
        let m = view_at(179.95, 10.0, 8.0, 0.0);
        let leaves = m.leaves();
        let n = |z: u8| 1u32 << z;
        assert!(leaves.iter().any(|l| l.wrap == 0), "the east edge of the main world");
        assert!(leaves.iter().any(|l| l.wrap == 1), "its far side, the same tiles again");
        assert!(leaves.iter().all(|l| l.x < n(l.z) && l.y < n(l.z)));
        let east = leaves.iter().find(|l| l.wrap == 1).unwrap();
        let west = leaves
            .iter()
            .find(|l| l.wrap == 0 && l.z == east.z && l.x == n(east.z) - 1);
        assert!(west.is_some(), "the copy sits next to the last column");
        assert!(east.x_world() >= f64::from(n(east.z)), "and is counted past it");
    }

    #[test]
    fn the_view_holds_near_the_poles() {
        for pitch in [0.0, 60.0] {
            let m = view_at(10.0, 89.0, 6.0, pitch);
            let leaves = m.leaves();
            assert!(!leaves.is_empty());
            assert!(leaves.iter().all(|l| l.y < (1 << l.z)), "rows stay on the map");
            assert!(m.cam.lat <= crate::map3d::camera::MAX_LAT);
        }
    }

    #[test]
    fn two_hundred_camera_jumps_leave_the_caches_bounded() {
        let mut m = view_at(0.0, 0.0, 12.0, 60.0);
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let (mut most_meshes, mut most_textures) = (0, 0);
        for _ in 0..200 {
            m.cam.set_centre((next() * 360.0 - 180.0, next() * 140.0 - 70.0));
            m.cam.zoom = 3.0 + next() * 14.0;
            m.cam.bearing = next() * 360.0;
            m.cam.pitch = next() * 60.0;
            m.frame += 40;
            let leaves = m.leaves();
            assert!(leaves.len() <= MAX_LEAVES + 8, "{} leaves", leaves.len());
            let (items, built, _) = m.ground_items(&leaves);
            assert!(built <= MESH_BUILDS_PER_FRAME);
            assert!(items.len() <= leaves.len());
            m.imagery.trim();
            most_meshes = most_meshes.max(m.leaf_meshes.len());
            most_textures = most_textures.max(m.tex_ids.len());
        }
        assert!(most_meshes <= MAX_LEAVES * 4, "{most_meshes} leaf meshes held");
        assert!(
            most_textures <= TEXTURES + MAX_LEAVES * 2,
            "{most_textures} texture ids held"
        );
    }

    #[test]
    fn a_ridge_hides_what_lies_behind_it_and_not_what_is_above_it() {
        let m = view_at(11.0, 47.0, 14.0, 60.0);
        let cam = m.cam;
        let wall_at = cam.geo_of(0.0, 150.0).1;
        let ridge = move |_: f64, lat: f64| if (lat - wall_at).abs() < 0.0003 { 400.0 } else { 0.0 };
        let behind = cam.local_of(11.0, cam.geo_of(0.0, 600.0).1, 0.0);
        assert!(
            line_blocked(&cam, behind, &ridge),
            "a marker on the ground behind the ridge"
        );
        let high = cam.local_of(11.0, cam.geo_of(0.0, 600.0).1, 900.0);
        assert!(!line_blocked(&cam, high, &ridge), "an aircraft above the ridge line");
        let before = cam.local_of(11.0, cam.geo_of(0.0, -300.0).1, 0.0);
        assert!(!line_blocked(&cam, before, &ridge), "a marker on this side of it");
        assert!(!line_blocked(&cam, behind, &|_, _| 0.0), "flat ground hides nothing");
    }

    #[test]
    fn a_sea_level_flat_view_needs_a_handful_of_leaves() {
        let m = view_at(11.62, 48.1, 14.0, 60.0);
        let n = m.leaves().len();
        assert!((4..=300).contains(&n), "{n} leaves for one screen");
    }

    #[test]
    fn a_leaf_covers_its_part_of_an_ancestor() {
        let whole = uv_of(
            Leaf {
                z: 5,
                x: 3,
                y: 4,
                wrap: 0,
            },
            5,
        );
        assert_eq!((whole.min, whole.size()), (Pos2::ZERO, Vec2::splat(1.0)));
        let quarter = uv_of(
            Leaf {
                z: 6,
                x: 7,
                y: 8,
                wrap: 0,
            },
            5,
        );
        assert_eq!((quarter.min, quarter.size()), (Pos2::new(0.5, 0.0), Vec2::splat(0.5)));
        let sixteenth = uv_of(
            Leaf {
                z: 7,
                x: 13,
                y: 14,
                wrap: 0,
            },
            5,
        );
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

    #[test]
    fn the_worst_case_the_view_can_hold_stays_under_a_gigabyte() {
        let mib = 1usize << 20;
        let texture = 256 * 256 * 4 * 4 / 3;
        let (mesh_verts, mesh_indices) = (17 * 17 + 4 * 17 * 2, GRID * GRID * 6 + GRID * 24);
        let mesh = mesh_verts * gpu::FLOATS * 4 + mesh_indices * 4;
        let cpu = IMAGE_BYTES
            + terrain::DEM_BYTES
            + TEXTURES.max(MAX_LEAVES * 2) * std::mem::size_of::<usize>()
            + MAX_LEAVES * 3 * mesh;
        let gpu = gpu::MAX_TEXTURES * texture * 2 + gpu::MAX_MESHES * mesh * 2;
        assert!(
            cpu / mib < 400,
            "{} MiB of decoded tiles and meshes on the CPU side",
            cpu / mib
        );
        assert!(gpu / mib < 600, "{} MiB of textures and meshes for the GPU", gpu / mib);
        assert!((cpu + gpu) / mib < 900, "{} MiB in all", (cpu + gpu) / mib);
    }
}
