//! The 3D map: the ground as a quadtree of web mercator tiles seen through a
//! tilted camera, the base layer draped on it, the aircraft, home and the
//! flown track in the air above it (`src/js/map.js` 3D mode). See "The 3D
//! map" in the README for how it is built and why.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, TextureHandle, TextureOptions, Vec2};

use crate::camera3d::{self, Camera, V3};
use crate::http::Server;
use crate::tile_cache::{Key, TileCache};
use crate::track::{self, Point, Track};

/// A leaf is split while it would cover more than this many pixels.
const SPLIT_PX: f64 = 360.0;
/// Quads per side of a leaf.
const GRID: usize = 16;
const MAX_LEAVES: usize = 420;
const TEXTURES: usize = 400;
const IMAGE_CACHE: usize = 600;

/// What the 3D view draws above the ground.
pub struct Layers3d<'a> {
    pub track: &'a Track,
    pub home: Option<(f64, f64)>,
    pub vehicle: Option<(f64, f64)>,
    pub heading: f32,
    /// The aircraft's height above the ground, in metres.
    pub height: f64,
}

/// What the pointer did to the camera this frame.
pub struct Input {
    pub dragged: bool,
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
    textures: HashMap<Key, TextureHandle>,
    used: Vec<Key>,
}

/// Web mercator metres of tile corner `(x + u, y + v)` at zoom `z`.
fn tile_mercator(z: u8, x: f64, y: f64) -> (f64, f64) {
    let world = 2.0 * std::f64::consts::PI * camera3d::R;
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
                pitch: camera3d::THREE_D_PITCH,
                bearing: 0.0,
                size: (800.0, 600.0),
                ground: 0.0,
            },
            source: source.to_string(),
            max_zoom,
            imagery: TileCache::new(ctx, server, IMAGE_CACHE, decode_image),
            textures: HashMap::new(),
            used: Vec::new(),
        }
    }

    /// Takes the view of the flat map.
    pub fn start_at(&mut self, lng: f64, lat: f64, zoom: f64, bearing: f64) {
        self.cam.lng = lng;
        self.cam.lat = lat;
        self.cam.zoom = zoom;
        self.cam.bearing = bearing;
        self.cam.pitch = camera3d::THREE_D_PITCH;
    }

    /// The base layer to drape.
    pub fn set_source(&mut self, id: &str, max_zoom: u8) {
        if self.source != id {
            self.source = id.to_string();
            self.imagery.clear();
            self.textures.clear();
        }
        self.max_zoom = max_zoom;
    }

    /// The tiles that make the ground: split by how big they look.
    fn leaves(&self) -> Vec<Leaf> {
        let c = &self.cam;
        let k = c.lat.to_radians().cos();
        let (cx, cy) = camera3d::mercator(c.lng, c.lat);
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
        let z0 = (c.zoom.floor() as i32 - 4).clamp(0, self.max_zoom as i32) as u8;
        let n_tiles = 2f64.powi(z0 as i32);
        let world = 2.0 * std::f64::consts::PI * camera3d::R;
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
        let (cx, cy) = camera3d::mercator(c.lng, c.lat);
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
        let metres = 2.0 * std::f64::consts::PI * camera3d::R * k / 2f64.powi(l.z as i32);
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
    fn imagery_for(&mut self, ctx: &egui::Context, l: Leaf) -> Option<(egui::TextureId, Rect)> {
        let top = l.z.min(self.max_zoom);
        let mut want = true;
        for d in 0..=8u8 {
            if d > top {
                break;
            }
            let (z, x, y) = (top - d, l.x >> d, l.y >> d);
            let key: Key = (self.source.clone(), z, x, y);
            if let Some(tex) = self.textures.get(&key) {
                self.used.push(key);
                return Some((tex.id(), uv_of(l, z)));
            }
            let found = if want {
                self.imagery.get(&key)
            } else {
                self.imagery.peek(&key)
            };
            want = false;
            if let Some(img) = found {
                let tex = ctx.load_texture(
                    format!("img{}/{}/{}", z, x, y),
                    Arc::unwrap_or_clone(img),
                    TextureOptions::LINEAR,
                );
                let id = tex.id();
                self.textures.insert(key.clone(), tex);
                self.used.push(key);
                return Some((id, uv_of(l, z)));
            }
        }
        None
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

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0, crate::style::adi_sky().gamma_multiply(0.55));
        let mut leaves = self.leaves();
        let cam = self.cam;
        let depth = |l: &Leaf| {
            let c = self.corners(*l);
            let mid = [(c[0][0] + c[2][0]) / 2.0, (c[0][1] + c[2][1]) / 2.0, c[0][2]];
            cam.project(mid).map_or(0.0, |s| s.depth)
        };
        leaves.sort_by(|a, b| depth(b).total_cmp(&depth(a)));
        self.used.clear();
        for l in &leaves {
            self.draw_leaf(ui.ctx(), &painter, origin, *l);
        }
        if self.textures.len() > TEXTURES {
            let keep: std::collections::HashSet<&Key> = self.used.iter().collect();
            self.textures.retain(|k, _| keep.contains(k));
        }
        self.imagery.trim();
        self.paint_layers(&painter, origin, layers);
        Input { dragged }
    }

    fn draw_leaf(&mut self, ctx: &egui::Context, painter: &egui::Painter, origin: Pos2, l: Leaf) {
        let Some((tex, uv)) = self.imagery_for(ctx, l) else {
            return;
        };
        let c = self.cam;
        let k = c.lat.to_radians().cos();
        let (cx, cy) = camera3d::mercator(c.lng, c.lat);
        let mut pts: Vec<Option<(Pos2, f64)>> = Vec::with_capacity((GRID + 1) * (GRID + 1));
        for j in 0..=GRID {
            for i in 0..=GRID {
                let (u, v) = (i as f64 / GRID as f64, j as f64 / GRID as f64);
                let (mx, my) = tile_mercator(l.z, l.x as f64 + u, l.y as f64 + v);
                let p = [(mx - cx) * k, (my - cy) * k, -c.ground];
                pts.push(
                    c.project(p)
                        .map(|s| (Pos2::new(origin.x + s.x as f32, origin.y + s.y as f32), s.depth)),
                );
            }
        }
        let mut quads: Vec<(f64, usize, usize)> = Vec::new();
        for j in 0..GRID {
            for i in 0..GRID {
                let ids = [
                    j * (GRID + 1) + i,
                    j * (GRID + 1) + i + 1,
                    (j + 1) * (GRID + 1) + i + 1,
                    (j + 1) * (GRID + 1) + i,
                ];
                if ids.iter().all(|k| pts[*k].is_some()) {
                    let d: f64 = ids.iter().map(|k| pts[*k].map_or(0.0, |p| p.1)).sum::<f64>() / 4.0;
                    quads.push((d, i, j));
                }
            }
        }
        quads.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut mesh = egui::Mesh::with_texture(tex);
        for (_, i, j) in quads {
            let base = mesh.vertices.len() as u32;
            for (di, dj) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
                let (ii, jj) = (i + di, j + dj);
                let (pos, _) = pts[jj * (GRID + 1) + ii].expect("a drawn quad has its corners");
                let (u, v) = (ii as f32 / GRID as f32, jj as f32 / GRID as f32);
                mesh.vertices.push(egui::epaint::Vertex {
                    pos,
                    uv: Pos2::new(uv.min.x + u * uv.width(), uv.min.y + v * uv.height()),
                    color: Color32::WHITE,
                });
            }
            mesh.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    fn screen_of(&self, origin: Pos2, p: V3) -> Option<Pos2> {
        self.cam
            .project(p)
            .map(|s| Pos2::new(origin.x + s.x as f32, origin.y + s.y as f32))
    }

    fn paint_layers(&self, painter: &egui::Painter, origin: Pos2, layers: &Layers3d<'_>) {
        let cam = &self.cam;
        let line = |pts: &[Point]| -> Vec<Pos2> {
            pts.iter()
                .filter_map(|p| self.screen_of(origin, cam.to_local(p.lng, p.lat, cam.ground + p.alt.unwrap_or(0.0))))
                .collect()
        };
        for flight in &layers.track.past {
            crate::map::paint_flight(painter, &line(&track::smooth(flight)), crate::style::track_past(), true);
        }
        crate::map::paint_flight(
            painter,
            &line(&track::smooth(&layers.track.current)),
            crate::style::track(),
            false,
        );
        if let Some((lng, lat)) = layers.home {
            if let Some(p) = self.screen_of(origin, cam.to_local(lng, lat, cam.ground)) {
                crate::map::paint_home(painter, p);
            }
        }
        if let Some((lng, lat)) = layers.vehicle {
            let ground = cam.to_local(lng, lat, cam.ground);
            let air = cam.to_local(lng, lat, cam.ground + layers.height);
            if let (Some(g), Some(a)) = (self.screen_of(origin, ground), self.screen_of(origin, air)) {
                painter.line_segment([g, a], Stroke::new(1.0, Color32::from_black_alpha(110)));
                painter.circle_filled(g, 3.0, Color32::from_black_alpha(90));
                crate::map::paint_vehicle(painter, a, layers.heading - cam.bearing as f32);
            }
        }
    }
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
        let half = std::f64::consts::PI * camera3d::R;
        assert!((x + half).abs() < 1e-6 && (y - half).abs() < 1e-6);
        let (x, y) = tile_mercator(1, 1.0, 1.0);
        assert!(x.abs() < 1e-6 && y.abs() < 1e-6);
    }
}
