//! Elevation for the 3D map: the RGB packed height tiles of the backend's
//! `terrain` sources, read as the web map reads them (`python/src/js/map.js`
//! `terrainFor`, `decodeDemPixel`).
//!
//! The tiles come through `/api/tiles/<dem>/z/x/y.png` like every map tile,
//! so the backend's cache serves them offline. An operator's own model
//! (GeoTIFF, XYZ) is served as the same tiles, which is why nothing here
//! reads those files.

use std::sync::Arc;

use eframe::egui;
use serde_json::Value;

use crate::map3d::tile_cache::{Key, TileCache};
use crate::net::http::Server;

/// How a height is packed into a pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Terrarium,
    Mapbox,
}

/// One elevation model of `terrain` in `/api/tiles/sources`.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub encoding: Encoding,
    pub maxzoom: u8,
    pub usable: bool,
    pub attribution: String,
}

/// The models the backend lists.
pub fn catalogue(sources: &Value) -> Vec<Spec> {
    sources
        .get("terrain")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|d| {
                    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    let id = s("id");
                    if id.is_empty() {
                        return None;
                    }
                    let flag = |k: &str| d.get(k).and_then(Value::as_bool).unwrap_or(false);
                    Some(Spec {
                        label: s("label"),
                        provider: s("provider"),
                        encoding: if s("encoding") == "mapbox" {
                            Encoding::Mapbox
                        } else {
                            Encoding::Terrarium
                        },
                        maxzoom: d.get("maxzoom").and_then(Value::as_u64).unwrap_or(15) as u8,
                        usable: !flag("token_required") || flag("token_set"),
                        attribution: s("attribution"),
                        id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `terrainFor`: the operator's pick, else the model of the map service in
/// use, else the preferred one, else the default.
pub fn choose(sources: &Value, provider: &str, choice: Option<&str>) -> Option<Spec> {
    let list = catalogue(sources);
    let id_of = |k: &str| sources.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let default = id_of("default_terrain");
    let preferred = id_of("preferred_terrain");
    let fallback = list.iter().find(|d| d.id == default).or(list.first()).cloned();
    if let Some(c) = choice.filter(|c| *c != "auto") {
        if let Some(d) = list.iter().find(|d| d.id == c && d.usable) {
            return Some(d.clone());
        }
    }
    if let Some(own) = list
        .iter()
        .find(|d| Some(&d.id) != fallback.as_ref().map(|f| &f.id) && d.provider == provider && d.usable)
    {
        return Some(own.clone());
    }
    match list.iter().find(|d| d.id == preferred && d.usable) {
        Some(p) => Some(p.clone()),
        None => fallback,
    }
}

/// The rows of the 3D panel's elevation list: `auto` with what it lands on,
/// then the usable models with the preferred one first.
pub fn options(sources: &Value, provider: &str) -> Vec<(String, String)> {
    let list = catalogue(sources);
    if list.is_empty() {
        return Vec::new();
    }
    let preferred = sources.get("preferred_terrain").and_then(Value::as_str).unwrap_or("");
    let auto = choose(sources, provider, None)
        .map(|d| format!("Automatic ({})", if d.label.is_empty() { &d.id } else { &d.label }))
        .unwrap_or_else(|| "Automatic".to_string());
    let mut usable: Vec<&Spec> = list.iter().filter(|d| d.usable).collect();
    usable.sort_by_key(|d| d.id != preferred);
    std::iter::once(("auto".to_string(), auto))
        .chain(usable.into_iter().map(|d| {
            (
                d.id.clone(),
                if d.label.is_empty() {
                    d.id.clone()
                } else {
                    d.label.clone()
                },
            )
        }))
        .collect()
}

/// Metres above sea level of one pixel.
pub fn decode_pixel(enc: Encoding, r: u8, g: u8, b: u8) -> f32 {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    match enc {
        Encoding::Terrarium => r * 256.0 + g + b / 256.0 - 32768.0,
        Encoding::Mapbox => -10000.0 + (r * 65536.0 + g * 256.0 + b) * 0.1,
    }
}

/// One elevation tile.
pub struct DemTile {
    size: usize,
    heights: Vec<f32>,
}

impl DemTile {
    /// A tile of known heights, row by row, for tests.
    #[cfg(test)]
    pub fn from_heights(size: usize, heights: Vec<f32>) -> DemTile {
        assert_eq!(heights.len(), size * size);
        DemTile { size, heights }
    }

    pub fn decode(bytes: &[u8], enc: Encoding) -> Option<DemTile> {
        let img = image::load_from_memory(bytes).ok()?.to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        if w != h || w < 2 {
            return None;
        }
        let heights = img.pixels().map(|p| decode_pixel(enc, p[0], p[1], p[2])).collect();
        Some(DemTile { size: w, heights })
    }

    /// The height at `(u, v)` over the tile, each in 0 to 1, bilinear.
    pub fn sample(&self, u: f64, v: f64) -> f32 {
        let max = (self.size - 1) as f64;
        let (x, y) = (u.clamp(0.0, 1.0) * max, v.clamp(0.0, 1.0) * max);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.size - 1), (y0 + 1).min(self.size - 1));
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let at = |x: usize, y: usize| self.heights[y * self.size + x];
        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// The elevation model in use and its tiles.
pub struct Terrain {
    pub spec: Spec,
    cache: TileCache<DemTile>,
}

impl Terrain {
    pub fn new(ctx: &egui::Context, server: &Server, spec: Spec) -> Terrain {
        let enc = spec.encoding;
        Terrain {
            cache: TileCache::new(ctx, server, 300, move |b| DemTile::decode(b, enc)),
            spec,
        }
    }

    /// The tile for `(z, x, y)` or the nearest ancestor that is here, with
    /// the part of it that tile covers as `(u0, v0, size)`. The tile asked
    /// for is queued when it is not.
    pub fn tile_for(&self, z: u8, x: u32, y: u32) -> Option<(Arc<DemTile>, (f64, f64, f64))> {
        let top = z.min(self.spec.maxzoom);
        let (x, y) = (x >> (z - top), y >> (z - top));
        let coarse = top.min(3);
        for d in 0..=top.min(12) {
            let key: Key = (self.spec.id.clone(), top - d, x >> d, y >> d);
            let found = if d == 0 || d == coarse {
                self.cache.get(&key)
            } else {
                self.cache.peek(&key)
            };
            if let Some(t) = found {
                let n = (1u64 << d) as f64;
                let (fx, fy) = ((x & ((1 << d) - 1)) as f64, (y & ((1 << d) - 1)) as f64);
                return Some((t, (fx / n, fy / n, 1.0 / n)));
            }
        }
        None
    }

    /// The ground height at a position, from what is loaded at about `zoom`.
    pub fn height_at(&self, lng: f64, lat: f64, zoom: f64) -> Option<f64> {
        let z = (zoom.round() as i64).clamp(0, self.spec.maxzoom as i64) as u8;
        let (mx, my) = crate::map3d::camera::mercator(lng, lat);
        let world = 2.0 * std::f64::consts::PI * crate::map3d::camera::R;
        let n = (1u64 << z) as f64;
        let (fx, fy) = ((mx + world / 2.0) / world * n, (world / 2.0 - my) / world * n);
        let (tx, ty) = (fx.floor().max(0.0) as u32, fy.floor().max(0.0) as u32);
        let (tile, (u0, v0, size)) = self.tile_for(z, tx, ty)?;
        Some(tile.sample(u0 + (fx - tx as f64) * size, v0 + (fy - ty as f64) * size) as f64)
    }

    pub fn trim(&self) {
        self.cache.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sources() -> Value {
        json!({
            "default_terrain": "terrain",
            "preferred_terrain": "copernicus",
            "terrain": [
                {"id": "terrain", "provider": "terrain", "encoding": "terrarium", "maxzoom": 15},
                {"id": "copernicus", "provider": "copernicus", "encoding": "terrarium", "maxzoom": 17},
                {"id": "mapbox_terrain", "provider": "mapbox", "encoding": "mapbox", "maxzoom": 15,
                 "token_required": true, "token_set": false},
                {"id": "mapbox_ok", "provider": "mapbox", "encoding": "mapbox", "maxzoom": 15,
                 "token_required": true, "token_set": true}
            ]
        })
    }

    #[test]
    fn the_panel_lists_automatic_first_then_the_usable_models() {
        let o = options(&sources(), "esri");
        let ids: Vec<&str> = o.iter().map(|(i, _)| i.as_str()).collect();
        assert_eq!(ids, ["auto", "copernicus", "terrain", "mapbox_ok"]);
        assert!(o[0].1.starts_with("Automatic (") && o[0].1.ends_with(')'));
        assert!(options(&json!({}), "esri").is_empty());
    }

    #[test]
    fn heights_are_unpacked_as_the_web_unpacks_them() {
        assert_eq!(decode_pixel(Encoding::Terrarium, 128, 0, 0), 0.0);
        assert_eq!(
            decode_pixel(Encoding::Terrarium, 130, 100, 128),
            2.0 * 256.0 + 100.0 + 0.5
        );
        let m = decode_pixel(Encoding::Mapbox, 1, 134, 160);
        assert!((m - (-10000.0 + (65536.0 + 134.0 * 256.0 + 160.0) * 0.1)).abs() < 1e-3);
    }

    #[test]
    fn the_model_is_chosen_like_terrain_for() {
        let s = sources();
        assert_eq!(choose(&s, "esri", None).unwrap().id, "copernicus", "the preferred one");
        assert_eq!(
            choose(&s, "mapbox", None).unwrap().id,
            "mapbox_ok",
            "the service's own, with its key"
        );
        assert_eq!(
            choose(&s, "esri", Some("terrain")).unwrap().id,
            "terrain",
            "the operator's pick"
        );
        assert_eq!(
            choose(&s, "esri", Some("mapbox_terrain")).unwrap().id,
            "copernicus",
            "a pick without a key"
        );
        assert_eq!(choose(&s, "esri", Some("auto")).unwrap().id, "copernicus");
        let only = json!({"default_terrain": "terrain", "terrain": [{"id": "terrain", "provider": "terrain"}]});
        assert_eq!(choose(&only, "esri", None).unwrap().id, "terrain");
        assert!(choose(&json!({}), "esri", None).is_none());
    }

    #[test]
    fn a_tile_is_sampled_between_its_pixels() {
        let t = DemTile {
            size: 2,
            heights: vec![0.0, 10.0, 20.0, 30.0],
        };
        assert_eq!(t.sample(0.0, 0.0), 0.0);
        assert_eq!(t.sample(1.0, 1.0), 30.0);
        assert_eq!(t.sample(0.5, 0.5), 15.0);
        assert_eq!(t.sample(2.0, -1.0), 10.0, "outside is clamped");
    }
}
