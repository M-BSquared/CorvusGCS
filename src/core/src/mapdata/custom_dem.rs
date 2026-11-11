//! The operator's own elevation models, served as elevation tiles
//! (`custom_dem.py`).
//!
//! Each model is converted once, on import, into plain float32 grids under
//! `<tile cache dir>/custom-dem/`, and Terrarium tiles are cut out of them on
//! demand. Each grid keeps its own coordinate system, and every tile pixel is
//! taken from Web Mercator into it. Halved overviews are built on import and a
//! tile reads the one nearest its own ground resolution. Pixels a model does
//! not cover are filled from the free default elevation the caller hands in as
//! `base`; without it the model's nearest edge is held outward and the tile is
//! marked incomplete.
//!
//! Grids are memory mapped, never read whole. [`CustomDemStore::close`] unmaps
//! every one (the module's `stop` calls it), and removing a model unmaps it
//! before its files are deleted, because Windows refuses to delete a mapped
//! file.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use memmap2::Mmap;
use serde_json::{json, Map, Value};

use super::crs::{self, Crs};
use super::dem_import::{guess_epsg, read_grid, DemImportError};
use super::dem_tiles::{encode_heights, Lru};
use super::util::{iso_utc, now_secs, uuid4_hex};

pub const TILE_SIZE: usize = 256;
pub const ENCODING: &str = "terrarium";
pub const ID_PREFIX: &str = "dem_";
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_MODELS: usize = 64;
pub const MAX_GRIDS_PER_MODEL: usize = 400;
pub const LATTICE_STEP: usize = 16;
pub const MIN_OVERVIEW_SIDE: usize = 32;
pub const RENDERED_CACHE_SIZE: usize = 96;
pub const PROVISIONAL_TTL_S: f64 = 30.0;
pub const PROVISIONAL_CACHE_SIZE: usize = 96;
pub const MIN_MAXZOOM: i64 = 10;
pub const MAX_MAXZOOM: i64 = 18;
const EARTH_PX_M: f64 = 156543.03392804097;

pub const SURFACE_STEP_M: f64 = 2.0;
pub const SURFACE_FRACTION: f64 = 0.0007;
pub const CLASSIFY_MAX_RES_M: f64 = 5.0;
pub const CLASSIFY_PAIRS: usize = 300_000;
pub const CLASSIFY_MIN_PAIRS: usize = 2_000;

/// The free default elevation around a model: heights in metres for the 256 by
/// 256 pixels of a tile, or None.
pub type BaseFn<'a> = &'a dyn Fn(i64, i64, i64) -> Option<Vec<f64>>;

/// `^dem_[0-9a-f]{10}$`.
pub fn is_model_id(name: &str) -> bool {
    name.len() == 14
        && name.starts_with(ID_PREFIX)
        && name[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An operator name goes into the map's attribution line, which MapLibre
/// renders as HTML: markup characters are dropped rather than escaped.
pub fn clean_name(raw: &str, fallback: &str) -> String {
    fn clean(s: &str) -> String {
        s.chars()
            .filter(|c| !matches!(c, '<' | '>' | '&' | '"' | '\'' | '`') && (*c as u32) >= 0x20)
            .collect()
    }
    let text = clean(raw);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text.chars().take(MAX_NAME_CHARS).collect();
    let text = text.trim().to_string();
    if !text.is_empty() {
        return text;
    }
    let fb: String = clean(fallback).trim().chars().take(MAX_NAME_CHARS).collect();
    if fb.is_empty() {
        "Elevation model".to_string()
    } else {
        fb
    }
}

/// Longitude and latitude of global pixel (`px`, `py`) at zoom `z`.
pub fn tile_lonlat(z: i64, px: f64, py: f64) -> (f64, f64) {
    let world = (TILE_SIZE as i64 * (1i64 << z)) as f64;
    let lon = px / world * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * py / world))
        .sinh()
        .atan()
        .to_degrees();
    (lon, lat)
}

/// Global pixel of (`lon`, `lat`) at zoom `z`.
pub fn lonlat_pixel(z: i64, lon: f64, lat: f64) -> (f64, f64) {
    let world = (TILE_SIZE as i64 * (1i64 << z)) as f64;
    let lat = lat.clamp(-85.05112878, 85.05112878);
    let s = lat.to_radians().sin();
    (
        (lon + 180.0) / 360.0 * world,
        (0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * std::f64::consts::PI)) * world,
    )
}

pub fn maxzoom_for(resolution_m: f64, lat: f64) -> i64 {
    let ground = EARTH_PX_M * lat.to_radians().cos().max(0.05);
    if resolution_m <= 0.0 {
        return MAX_MAXZOOM;
    }
    let z = (ground / resolution_m).log2().ceil() as i64;
    z.clamp(MIN_MAXZOOM, MAX_MAXZOOM)
}

/// The grid at half resolution: the mean of each 2 by 2 block's valid cells.
pub fn overview(values: &[f32], width: usize, height: usize) -> (Vec<f32>, usize, usize) {
    let w2 = width.div_ceil(2);
    let h2 = height.div_ceil(2);
    let mut out = vec![f32::NAN; w2 * h2];
    for r2 in 0..h2 {
        let r = 2 * r2;
        let top = r * width;
        let bottom = if r + 1 < height { Some(top + width) } else { None };
        for c2 in 0..w2 {
            let c = 2 * c2;
            let (mut total, mut n) = (0.0f64, 0u32);
            let mut take = |v: f32| {
                if !v.is_nan() {
                    total += f64::from(v);
                    n += 1;
                }
            };
            take(values[top + c]);
            if c + 1 < width {
                take(values[top + c + 1]);
            }
            if let Some(b) = bottom {
                take(values[b + c]);
                if c + 1 < width {
                    take(values[b + c + 1]);
                }
            }
            out[r2 * w2 + c2] = if n > 0 { (total / f64::from(n)) as f32 } else { f32::NAN };
        }
    }
    (out, w2, h2)
}

/// `"terrain"`, `"surface"` or `"unknown"` for one grid.
pub fn classify(values: &dyn Fn(usize) -> f32, width: usize, height: usize, resolution_m: f64) -> &'static str {
    if !(0.0 < resolution_m && resolution_m <= CLASSIFY_MAX_RES_M) || width < 2 || height < 2 {
        return "unknown";
    }
    let limit = SURFACE_STEP_M.max(1.5 * resolution_m);
    let rows = 1usize.max((height - 1).min(CLASSIFY_PAIRS / (2 * width)));
    let stride = 1usize.max((height - 1) / rows);
    let (mut pairs, mut steps) = (0usize, 0usize);
    let mut r = 0;
    while r < height - 1 {
        let top = r * width;
        let below = top + width;
        for c in 0..width - 1 {
            let a = values(top + c);
            if a.is_nan() {
                continue;
            }
            let b = values(top + c + 1);
            if !b.is_nan() {
                pairs += 1;
                if (f64::from(b) - f64::from(a)).abs() > limit {
                    steps += 1;
                }
            }
            let d = values(below + c);
            if !d.is_nan() {
                pairs += 1;
                if (f64::from(d) - f64::from(a)).abs() > limit {
                    steps += 1;
                }
            }
        }
        r += stride;
    }
    if pairs < CLASSIFY_MIN_PAIRS {
        return "unknown";
    }
    if steps as f64 / pairs as f64 >= SURFACE_FRACTION {
        "surface"
    } else {
        "terrain"
    }
}

fn model_kind(grids: &[Value]) -> String {
    let mut known: Vec<&str> = Vec::new();
    for g in grids {
        if let Some(k) = g.get("kind").and_then(Value::as_str) {
            if k != "unknown" && !known.contains(&k) {
                known.push(k);
            }
        }
    }
    match known.len() {
        0 => "unknown".into(),
        1 => known[0].into(),
        _ => "mixed".into(),
    }
}

fn round_n(x: f64, digits: usize) -> f64 {
    format!("{x:.digits$}").parse().unwrap_or(x)
}

fn write_f32(path: &Path, values: &[f32]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    let mut data = Vec::with_capacity(values.len() * 4);
    for v in values {
        data.extend_from_slice(&v.to_le_bytes());
    }
    File::create(&tmp)?.write_all(&data)?;
    std::fs::rename(&tmp, path)
}

/// One mapped level of a grid: its size, pixel size and the open map.
struct Level {
    width: usize,
    height: usize,
    dx: f64,
    dy: f64,
    map: Mmap,
}

impl Level {
    fn open(path: &Path, width: usize, height: usize, dx: f64, dy: f64) -> Result<Level, DemImportError> {
        let file = File::open(path).map_err(|e| DemImportError(e.to_string()))?;
        // SAFETY: the file is only ever replaced, never edited in place (imports
        // write a new file under a fresh name), and the map is dropped before a
        // model's files are deleted.
        let map = unsafe { Mmap::map(&file) }.map_err(|e| DemImportError(e.to_string()))?;
        if map.len() < 4 * width * height {
            return Err(DemImportError(
                "a stored elevation grid is shorter than its size".into(),
            ));
        }
        Ok(Level {
            width,
            height,
            dx,
            dy,
            map,
        })
    }

    fn get(&self, i: usize) -> f32 {
        f32::from_le_bytes(self.map[4 * i..4 * i + 4].try_into().unwrap())
    }
}

/// A grid's metadata, its coordinate system and its mapped levels.
pub struct LoadedGrid {
    crs: Crs,
    x0: f64,
    y0: f64,
    bounds: [f64; 4],
    resolution_m: f64,
    levels: Vec<Level>,
}

fn num(v: &Value, key: &str) -> Result<f64, DemImportError> {
    v.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| DemImportError(format!("model.json lacks {key}")))
}

impl LoadedGrid {
    fn new(folder: &Path, meta: &Value) -> Result<LoadedGrid, DemImportError> {
        let epsg = meta.get("epsg").and_then(Value::as_i64).unwrap_or(0);
        let crs = crs::get(epsg).ok_or_else(|| DemImportError(format!("EPSG {epsg} is no longer supported")))?;
        let b = meta
            .get("bounds")
            .and_then(Value::as_array)
            .filter(|b| b.len() == 4)
            .ok_or_else(|| DemImportError("model.json lacks bounds".into()))?;
        let bounds = [0, 1, 2, 3].map(|i| b[i].as_f64().unwrap_or(0.0));
        let mut levels = Vec::new();
        for level in meta
            .get("levels")
            .and_then(Value::as_array)
            .ok_or_else(|| DemImportError("model.json lacks levels".into()))?
        {
            let file = level.get("file").and_then(Value::as_str).unwrap_or("");
            levels.push(Level::open(
                &folder.join(file),
                num(level, "width")? as usize,
                num(level, "height")? as usize,
                num(level, "dx")?,
                num(level, "dy")?,
            )?);
        }
        Ok(LoadedGrid {
            crs,
            x0: num(meta, "x0")?,
            y0: num(meta, "y0")?,
            bounds,
            resolution_m: num(meta, "resolution_m")?,
            levels,
        })
    }

    /// The finest level not finer than half of a tile pixel's ground size.
    fn pick_level(&self, ground_m: f64) -> &Level {
        let mut index = 0;
        let mut ratio = if self.resolution_m > 0.0 {
            ground_m / self.resolution_m
        } else {
            1.0
        };
        while index + 1 < self.levels.len() && ratio >= 2.0 {
            ratio /= 2.0;
            index += 1;
        }
        &self.levels[index]
    }
}

struct Model {
    meta: Value,
    grids: Arc<Mutex<Vec<LoadedGrid>>>,
}

type Key = (String, i64, i64, i64);

struct Inner {
    models: IndexMap<String, Model>,
    rendered: Lru<Key, Arc<Vec<u8>>>,
    provisional: Lru<Key, (Arc<Vec<u8>>, Instant)>,
}

impl Inner {
    fn forget(&mut self, model_id: &str) {
        self.rendered.retain(|k| k.0 != model_id);
        self.provisional.retain(|k| k.0 != model_id);
    }
}

/// Every custom model on disk, and the tiles cut from them. Thread safe:
/// imports, removals and tile renders arrive on different handler threads. The
/// store lock guards the model table and the rendered caches; each model's own
/// lock (the mutex around its grids) is held while its maps are read, so
/// removing a model waits for the tile being cut from it.
pub struct CustomDemStore {
    root: PathBuf,
    inner: Mutex<Inner>,
    closed: AtomicBool,
}

fn summarise(meta: &mut Value) {
    let grids = meta["grids"].as_array().cloned().unwrap_or_default();
    let col = |g: &Value, i: usize| g["bounds"][i].as_f64().unwrap_or(0.0);
    let fold =
        |f: &dyn Fn(f64, f64) -> f64, init: f64, get: &dyn Fn(&Value) -> f64| grids.iter().map(get).fold(init, f);
    let bounds = [
        fold(&f64::min, f64::INFINITY, &|g| col(g, 0)),
        fold(&f64::min, f64::INFINITY, &|g| col(g, 1)),
        fold(&f64::max, f64::NEG_INFINITY, &|g| col(g, 2)),
        fold(&f64::max, f64::NEG_INFINITY, &|g| col(g, 3)),
    ];
    let resolution = fold(&f64::min, f64::INFINITY, &|g| g["resolution_m"].as_f64().unwrap_or(0.0));
    let min = fold(&f64::min, f64::INFINITY, &|g| g["min"].as_f64().unwrap_or(0.0));
    let max = fold(&f64::max, f64::NEG_INFINITY, &|g| g["max"].as_f64().unwrap_or(0.0));
    let bytes: i64 = grids.iter().map(|g| g["bytes"].as_i64().unwrap_or(0)).sum();
    let mid_lat = (bounds[1] + bounds[3]) / 2.0;
    meta["bounds"] = json!(bounds);
    meta["resolution_m"] = json!(resolution);
    meta["min"] = json!(min);
    meta["max"] = json!(max);
    meta["bytes"] = json!(bytes);
    meta["kind"] = json!(model_kind(&grids));
    meta["maxzoom"] = json!(maxzoom_for(resolution, mid_lat));
}

fn save(root: &Path, meta: &Value) -> std::io::Result<()> {
    let folder = root.join(meta["id"].as_str().unwrap_or(""));
    let path = folder.join("model.json");
    let tmp = folder.join("model.json.part");
    let buf = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, path)
}

fn lonlat_bounds(grid: &super::dem_import::Grid, crs: &Crs) -> Result<[f64; 4], DemImportError> {
    let x1 = grid.x0 + grid.width as f64 * grid.dx;
    let y1 = grid.y0 - grid.height as f64 * grid.dy;
    let (mut lons, mut lats) = (Vec::new(), Vec::new());
    let steps = 16;
    for k in 0..=steps {
        let fx = grid.x0 + (x1 - grid.x0) * k as f64 / steps as f64;
        let fy = y1 + (grid.y0 - y1) * k as f64 / steps as f64;
        for (px, py) in [(fx, grid.y0), (fx, y1), (grid.x0, fy), (x1, fy)] {
            let (lon, lat) = crs.to_lonlat(px, py);
            lons.push(lon);
            lats.push(lat);
        }
    }
    let mn = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
    let mx = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !lons.iter().chain(&lats).all(|v| v.is_finite())
        || mn(&lons) < -180.0
        || mx(&lons) > 180.0
        || mn(&lats) < -85.0
        || mx(&lats) > 85.0
        || mx(&lons) - mn(&lons) > 20.0
        || mx(&lats) - mn(&lats) > 20.0
    {
        return Err(DemImportError(format!(
            "in {} (EPSG {}) these coordinates are not a place on Earth. The file is probably in another coordinate system",
            crs.name, crs.epsg
        )));
    }
    Ok([
        round_n(mn(&lons), 7),
        round_n(mn(&lats), 7),
        round_n(mx(&lons), 7),
        round_n(mx(&lats), 7),
    ])
}

fn stats(values: &[f32]) -> (f64, f64, usize) {
    let (mut lo, mut hi, mut n) = (f64::INFINITY, f64::NEG_INFINITY, 0usize);
    for &v in values {
        if !v.is_nan() {
            n += 1;
            lo = lo.min(f64::from(v));
            hi = hi.max(f64::from(v));
        }
    }
    if n > 0 {
        (round_n(lo, 2), round_n(hi, 2), n)
    } else {
        (0.0, 0.0, 0)
    }
}

fn splitext_stem(base: &str) -> &str {
    let lead = base.len() - base.trim_start_matches('.').len();
    match base[lead..].rfind('.') {
        Some(i) => &base[..lead + i],
        None => base,
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl CustomDemStore {
    /// The store over `root`, with every model found there loaded. A model
    /// that cannot be loaded is skipped, never hiding the rest.
    pub fn new(root: impl Into<PathBuf>) -> CustomDemStore {
        let store = CustomDemStore {
            root: root.into(),
            inner: Mutex::new(Inner {
                models: IndexMap::new(),
                rendered: Lru::new(RENDERED_CACHE_SIZE),
                provisional: Lru::new(PROVISIONAL_CACHE_SIZE),
            }),
            closed: AtomicBool::new(false),
        };
        store.load_all();
        store
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn load_all(&self) {
        let Ok(rd) = std::fs::read_dir(&self.root) else { return };
        let mut names: Vec<String> = rd.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
        names.sort();
        for name in names {
            if !is_model_id(&name) {
                continue;
            }
            let folder = self.root.join(&name);
            let Some((mut meta, grids)) = Self::read_model(&folder, &name) else {
                continue;
            };
            let missing: Vec<usize> = meta["grids"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .enumerate()
                        .filter(|(_, g)| g.get("kind").is_none())
                        .map(|(i, _)| i)
                        .collect()
                })
                .unwrap_or_default();
            if !missing.is_empty() {
                for i in missing {
                    let level = &grids[i].levels[0];
                    let res = meta["grids"][i]["resolution_m"].as_f64().unwrap_or(0.0);
                    let kind = classify(&|k| level.get(k), level.width, level.height, res);
                    meta["grids"][i]["kind"] = json!(kind);
                }
                summarise(&mut meta);
                let _ = save(&self.root, &meta);
            }
            self.inner.lock().unwrap().models.insert(
                name,
                Model {
                    meta,
                    grids: Arc::new(Mutex::new(grids)),
                },
            );
        }
    }

    fn read_model(folder: &Path, name: &str) -> Option<(Value, Vec<LoadedGrid>)> {
        let text = std::fs::read_to_string(folder.join("model.json")).ok()?;
        let meta: Value = serde_json::from_str(&text).ok()?;
        if meta.get("id").and_then(Value::as_str) != Some(name) {
            return None;
        }
        let list = meta.get("grids").and_then(Value::as_array).filter(|a| !a.is_empty())?;
        let mut grids = Vec::new();
        for g in list {
            grids.push(LoadedGrid::new(folder, g).ok()?);
        }
        Some((meta, grids))
    }

    pub fn has(&self, model_id: &str) -> bool {
        self.inner.lock().unwrap().models.contains_key(model_id)
    }

    /// Every model, newest first, as the settings page shows it.
    pub fn list_models(&self) -> Vec<Value> {
        let mut metas: Vec<Value> = self
            .inner
            .lock()
            .unwrap()
            .models
            .values()
            .map(|m| m.meta.clone())
            .collect();
        metas.sort_by(|a, b| {
            let (x, y) = (
                a["created_at"].as_str().unwrap_or(""),
                b["created_at"].as_str().unwrap_or(""),
            );
            y.cmp(x)
        });
        metas
            .iter()
            .map(|meta| {
                let files: Vec<Value> = meta["grids"]
                    .as_array()
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
                    .iter()
                    .map(|g| {
                        json!({
                            "file_name": g.get("file_name").cloned().unwrap_or(json!("")),
                            "epsg": g["epsg"],
                            "crs_name": g.get("crs_name").cloned().unwrap_or(json!("")),
                            "width": g["levels"][0]["width"],
                            "height": g["levels"][0]["height"],
                            "resolution_m": g["resolution_m"],
                            "kind": g.get("kind").cloned().unwrap_or(json!("unknown")),
                        })
                    })
                    .collect();
                json!({
                    "id": meta["id"],
                    "name": meta["name"],
                    "created_at": meta.get("created_at").cloned().unwrap_or(json!("")),
                    "bounds": meta["bounds"],
                    "resolution_m": meta["resolution_m"],
                    "maxzoom": meta["maxzoom"],
                    "min": meta["min"],
                    "max": meta["max"],
                    "bytes": meta.get("bytes").cloned().unwrap_or(json!(0)),
                    "kind": meta.get("kind").cloned().unwrap_or(json!("unknown")),
                    "files": files,
                })
            })
            .collect()
    }

    /// The models as elevation sources, shaped like `tile_sources::list_terrain`.
    pub fn terrain_entries(&self) -> Vec<Value> {
        self.list_models()
            .into_iter()
            .map(|m| {
                let name = m["name"].as_str().unwrap_or("");
                json!({
                    "id": m["id"],
                    "label": m["name"],
                    "provider": "custom",
                    "encoding": ENCODING,
                    "tile_size": TILE_SIZE,
                    "maxzoom": m["maxzoom"],
                    "sparse_above": null,
                    "attribution": format!("Elevation: {name} (own data)"),
                    "custom": true,
                    "kind": m["kind"],
                    "bounds": m["bounds"],
                })
            })
            .collect()
    }

    /// Add the grid in `path` to a model, a new one unless `model_id` is
    /// given. `epsg` overrides the coordinate system the file states.
    pub fn import_file(
        &self,
        path: &Path,
        filename: &str,
        name: &str,
        epsg: Option<i64>,
        model_id: Option<&str>,
    ) -> Result<Value, DemImportError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(DemImportError("the elevation store is shut down".into()));
        }
        {
            let inner = self.inner.lock().unwrap();
            if let Some(id) = model_id {
                match inner.models.get(id) {
                    None => return Err(DemImportError("that elevation model no longer exists".into())),
                    Some(m) if m.meta["grids"].as_array().map_or(0, Vec::len) >= MAX_GRIDS_PER_MODEL => {
                        return Err(DemImportError(format!(
                            "one model holds at most {MAX_GRIDS_PER_MODEL} files"
                        )));
                    }
                    _ => {}
                }
            } else if inner.models.len() >= MAX_MODELS {
                return Err(DemImportError(format!(
                    "there are already {MAX_MODELS} elevation models. Remove one first"
                )));
            }
        }
        let grid = read_grid(path, filename)?;
        let code = match epsg.filter(|e| *e != 0).or(grid.epsg).or_else(|| guess_epsg(&grid)) {
            Some(c) => c,
            None => {
                return Err(DemImportError(
                    "the file does not say which coordinate system it uses. Choose it under Coordinate system and load it again (German state data is usually ETRS89 / UTM zone 32N, EPSG 25832)"
                        .into(),
                ))
            }
        };
        let crs = crs::get(code).ok_or_else(|| {
            DemImportError(format!(
                "EPSG {code} is not a coordinate system Corvus knows. Supported: {}",
                crs::SUPPORTED_HINT
            ))
        })?;
        let bounds = lonlat_bounds(&grid, &crs)?;
        let (lo, hi, valid) = stats(&grid.values);
        if valid == 0 {
            return Err(DemImportError("the file holds no heights, only no-data cells".into()));
        }
        let mid_lat = (bounds[1] + bounds[3]) / 2.0;
        let resolution = if crs.geographic {
            grid.dx * 111320.0 * mid_lat.to_radians().cos()
        } else if matches!(code, 3857 | 900913 | 102100 | 102113) {
            grid.dx * mid_lat.to_radians().cos()
        } else {
            grid.dx
        }
        .max(0.01);

        let creating = model_id.is_none();
        let model_id = match model_id {
            Some(id) => id.to_string(),
            None => format!("{ID_PREFIX}{}", &uuid4_hex()[..10]),
        };
        let folder = self.root.join(&model_id);
        std::fs::create_dir_all(&folder).map_err(|e| DemImportError(e.to_string()))?;
        let stem = format!("g{:x}{}", now_millis(), &uuid4_hex()[..4]);
        let mut written: Vec<String> = Vec::new();
        let built = (|| -> Result<(Value, LoadedGrid), DemImportError> {
            let io = |e: std::io::Error| DemImportError(e.to_string());
            let mut levels = Vec::new();
            let mut values = grid.values.clone();
            let (mut width, mut height) = (grid.width, grid.height);
            let (mut dx, mut dy) = (grid.dx, grid.dy);
            let mut level = 0;
            loop {
                let fname = format!("{stem}.{level}.f32");
                write_f32(&folder.join(&fname), &values).map_err(io)?;
                written.push(fname.clone());
                levels.push(json!({"file": fname, "width": width, "height": height, "dx": dx, "dy": dy}));
                if width.min(height) < 2 * MIN_OVERVIEW_SIDE {
                    break;
                }
                let (v, w, h) = overview(&values, width, height);
                values = v;
                width = w;
                height = h;
                dx *= 2.0;
                dy *= 2.0;
                level += 1;
            }
            let mut bytes = 0u64;
            for f in &written {
                bytes += std::fs::metadata(folder.join(f)).map_err(io)?.len();
            }
            let kind = classify(&|i| grid.values[i], grid.width, grid.height, resolution);
            let mut m = Map::new();
            m.insert(
                "file_name".into(),
                json!(basename(filename).chars().take(120).collect::<String>()),
            );
            m.insert("epsg".into(), json!(code));
            m.insert("crs_name".into(), json!(crs.name));
            m.insert("x0".into(), json!(grid.x0));
            m.insert("y0".into(), json!(grid.y0));
            m.insert("bounds".into(), json!(bounds));
            m.insert("resolution_m".into(), json!(round_n(resolution, 3)));
            m.insert("min".into(), json!(lo));
            m.insert("max".into(), json!(hi));
            m.insert("kind".into(), json!(kind));
            m.insert("levels".into(), Value::Array(levels));
            m.insert("bytes".into(), json!(bytes));
            let grid_meta = Value::Object(m);
            let loaded = LoadedGrid::new(&folder, &grid_meta)?;
            Ok((grid_meta, loaded))
        })();
        let (grid_meta, loaded) = match built {
            Ok(v) => v,
            Err(e) => {
                for f in &written {
                    let _ = std::fs::remove_file(folder.join(f));
                }
                if creating {
                    let _ = std::fs::remove_dir_all(&folder);
                }
                return Err(e);
            }
        };

        let mut inner = self.inner.lock().unwrap();
        if creating {
            let stem_name = splitext_stem(basename(filename));
            let meta = json!({
                "id": model_id,
                "name": clean_name(name, stem_name),
                "created_at": iso_utc(now_secs()),
                "grids": [],
            });
            inner.models.insert(
                model_id.clone(),
                Model {
                    meta,
                    grids: Arc::new(Mutex::new(Vec::new())),
                },
            );
        }
        let Some(model) = inner.models.get_mut(&model_id) else {
            return Err(DemImportError("that elevation model no longer exists".into()));
        };
        model.meta["grids"].as_array_mut().unwrap().push(grid_meta);
        model.grids.lock().unwrap().push(loaded);
        summarise(&mut model.meta);
        let meta = model.meta.clone();
        inner.forget(&model_id);
        save(&self.root, &meta).map_err(|e| DemImportError(e.to_string()))?;
        drop(inner);
        Ok(self
            .list_models()
            .into_iter()
            .find(|m| m["id"] == model_id.as_str())
            .unwrap_or(Value::Null))
    }

    /// Give `model_id` a new name. The name that stands, or None if unknown.
    pub fn rename(&self, model_id: &str, name: &str) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        let model = inner.models.get_mut(model_id)?;
        let current = model.meta["name"].as_str().unwrap_or("").to_string();
        let cleaned = clean_name(name, &current);
        model.meta["name"] = json!(cleaned);
        let meta = model.meta.clone();
        let _ = save(&self.root, &meta);
        Some(cleaned)
    }

    /// Delete `model_id` and its files. False when there is no such model.
    pub fn remove(&self, model_id: &str) -> bool {
        let model = {
            let mut inner = self.inner.lock().unwrap();
            let Some(model) = inner.models.shift_remove(model_id) else {
                return false;
            };
            inner.forget(model_id);
            model
        };
        model.grids.lock().unwrap().clear();
        let _ = std::fs::remove_dir_all(self.root.join(model_id));
        true
    }

    /// Unmap every grid. The store answers nothing afterwards.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let models: Vec<Model> = {
            let mut inner = self.inner.lock().unwrap();
            inner.rendered.clear();
            inner.provisional.clear();
            let drained: Vec<Model> = inner.models.drain(..).map(|(_, m)| m).collect();
            drained
        };
        for m in models {
            m.grids.lock().unwrap().clear();
        }
    }

    /// Does tile (`z`, `x`, `y`) overlap the model at all?
    pub fn covers(&self, model_id: &str, z: i64, x: i64, y: i64) -> bool {
        let b = {
            let inner = self.inner.lock().unwrap();
            let Some(m) = inner.models.get(model_id) else {
                return false;
            };
            [0, 1, 2, 3].map(|i| m.meta["bounds"][i].as_f64().unwrap_or(0.0))
        };
        let size = TILE_SIZE as f64;
        let (west, north) = tile_lonlat(z, x as f64 * size, y as f64 * size);
        let (east, south) = tile_lonlat(z, (x + 1) as f64 * size, (y + 1) as f64 * size);
        !(east < b[0] || west > b[2] || north < b[1] || south > b[3])
    }

    /// A Terrarium PNG for the tile, and whether it is complete. Incomplete
    /// means the default elevation around the model was not available, so the
    /// model's edge was held outward instead; such a tile is remembered only
    /// briefly. None for an unknown model.
    pub fn tile(&self, model_id: &str, z: i64, x: i64, y: i64, base: Option<BaseFn<'_>>) -> Option<(Vec<u8>, bool)> {
        let key: Key = (model_id.to_string(), z, x, y);
        let grids = {
            let mut inner = self.inner.lock().unwrap();
            if !inner.models.contains_key(model_id) {
                return None;
            }
            if let Some(hit) = inner.rendered.get(&key) {
                return Some(((*hit).clone(), true));
            }
            inner.models[model_id].grids.clone()
        };
        let base_values = base
            .and_then(|f| f(z, x, y))
            .filter(|v| v.len() == TILE_SIZE * TILE_SIZE);
        if base_values.is_none() {
            if let Some(held) = self.provisional_hit(&key) {
                return Some((held, false));
            }
        }
        let (values, filled) = {
            let guard = grids.lock().unwrap();
            if guard.is_empty() || guard[0].levels.is_empty() {
                return None;
            }
            render_tile(&guard, z, x, y, base_values.as_deref())
        };
        let blob = encode_heights(TILE_SIZE, &values, ENCODING)?;
        let complete = base_values.is_some() || filled;
        let mut inner = self.inner.lock().unwrap();
        if inner.models.contains_key(model_id) {
            if complete {
                inner.rendered.put(key, Arc::new(blob.clone()));
            } else {
                inner.provisional.put(key, (Arc::new(blob.clone()), Instant::now()));
            }
        }
        Some((blob, complete))
    }

    fn provisional_hit(&self, key: &Key) -> Option<Vec<u8>> {
        let mut inner = self.inner.lock().unwrap();
        let (blob, made) = inner.provisional.get(key)?;
        if made.elapsed() > Duration::from_secs_f64(PROVISIONAL_TTL_S) {
            inner.provisional.retain(|k| k != key);
            return None;
        }
        Some((*blob).clone())
    }
}

impl Drop for CustomDemStore {
    fn drop(&mut self) {
        self.close();
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Heights for every pixel of a tile, and whether the grids filled all of it.
/// A later grid wins where two overlap. Pixels no grid covers take `base`;
/// without it they hold the nearest grid edge.
pub fn render_tile(grids: &[LoadedGrid], z: i64, x: i64, y: i64, base: Option<&[f64]>) -> (Vec<f64>, bool) {
    let size = TILE_SIZE;
    let mut out: Vec<f64> = match base {
        Some(b) => b.to_vec(),
        None => vec![f64::NAN; size * size],
    };
    let mut covered = vec![false; size * size];
    let (ox, oy) = (x * size as i64, y * size as i64);
    let centre_lat = tile_lonlat(z, ox as f64 + size as f64 / 2.0, oy as f64 + size as f64 / 2.0).1;
    let ground = EARTH_PX_M * centre_lat.to_radians().cos().max(0.05) / (1i64 << z) as f64;
    let steps = size / LATTICE_STEP;
    for grid in grids {
        let [w, s, e, n] = grid.bounds;
        let (px0, py0) = lonlat_pixel(z, w, n);
        let (px1, py1) = lonlat_pixel(z, e, s);
        let c_lo = 0.max((px0 - ox as f64).floor() as i64 - 1);
        let c_hi = (size as i64 - 1).min((px1 - ox as f64).ceil() as i64 + 1);
        let r_lo = 0.max((py0 - oy as f64).floor() as i64 - 1);
        let r_hi = (size as i64 - 1).min((py1 - oy as f64).ceil() as i64 + 1);
        if c_lo > c_hi || r_lo > r_hi {
            continue;
        }
        let level = grid.pick_level(ground);
        sample(
            grid,
            level,
            z,
            ox,
            oy,
            steps,
            (c_lo, c_hi, r_lo, r_hi),
            &mut out,
            &mut covered,
            false,
        );
    }
    let filled = covered.iter().all(|c| *c);
    if base.is_none() && !filled {
        if let Some(nearest) = nearest_grid(grids, z, ox, oy) {
            let level = nearest.pick_level(ground);
            sample(
                nearest,
                level,
                z,
                ox,
                oy,
                steps,
                (0, size as i64 - 1, 0, size as i64 - 1),
                &mut out,
                &mut covered,
                true,
            );
        }
        for v in out.iter_mut() {
            if v.is_nan() {
                *v = 0.0;
            }
        }
    }
    (out, filled)
}

fn nearest_grid(grids: &[LoadedGrid], z: i64, ox: i64, oy: i64) -> Option<&LoadedGrid> {
    let half = TILE_SIZE as f64 / 2.0;
    let (cx, cy) = tile_lonlat(z, ox as f64 + half, oy as f64 + half);
    let mut best = None;
    let mut best_d = f64::INFINITY;
    for grid in grids {
        let [w, s, e, n] = grid.bounds;
        let dx = (w - cx).max(0.0).max(cx - e);
        let dy = (s - cy).max(0.0).max(cy - n);
        let d = dx * dx + dy * dy;
        if d < best_d {
            best = Some(grid);
            best_d = d;
        }
    }
    best
}

/// Bilinear heights from `level` into `out` for the pixel window given
/// (`c_lo, c_hi, r_lo, r_hi`). With `clamp`, only pixels not yet covered are
/// written, and the grid's edge is held outward for those outside it.
#[allow(clippy::too_many_arguments)]
fn sample(
    grid: &LoadedGrid,
    level: &Level,
    z: i64,
    ox: i64,
    oy: i64,
    steps: usize,
    window: (i64, i64, i64, i64),
    out: &mut [f64],
    covered: &mut [bool],
    clamp: bool,
) {
    let (c_lo, c_hi, r_lo, r_hi) = window;
    let (x0, y0) = (grid.x0, grid.y0);
    let (dx, dy) = (level.dx, level.dy);
    let (width, height) = (level.width, level.height);
    let step = LATTICE_STEP;
    let mut cols: Vec<Vec<f64>> = Vec::with_capacity(steps + 1);
    let mut rows: Vec<Vec<f64>> = Vec::with_capacity(steps + 1);
    for k in 0..=steps {
        let mut row_c = Vec::with_capacity(steps + 1);
        let mut row_r = Vec::with_capacity(steps + 1);
        for m in 0..=steps {
            let (lon, lat) = tile_lonlat(z, (ox + (m * step) as i64) as f64, (oy + (k * step) as i64) as f64);
            let (gx, gy) = grid.crs.from_lonlat(lon, lat);
            row_c.push((gx - x0) / dx - 0.5);
            row_r.push((y0 - gy) / dy - 0.5);
        }
        cols.push(row_c);
        rows.push(row_r);
    }
    let (last_c, last_r) = (width - 1, height - 1);
    let (lo_c, hi_c) = (-0.5, width as f64 - 0.5);
    let (lo_r, hi_r) = (-0.5, height as f64 - 0.5);
    let size = TILE_SIZE;
    let inv = 1.0 / step as f64;
    for j in r_lo..=r_hi {
        let v = (j as f64 + 0.5) * inv;
        let k = (v as usize).min(steps - 1);
        let fv = v - k as f64;
        let line_c: Vec<f64> = cols[k]
            .iter()
            .zip(&cols[k + 1])
            .map(|(a, b)| a + (b - a) * fv)
            .collect();
        let line_r: Vec<f64> = rows[k]
            .iter()
            .zip(&rows[k + 1])
            .map(|(a, b)| a + (b - a) * fv)
            .collect();
        let base_index = j as usize * size;
        for i in c_lo..=c_hi {
            let index = base_index + i as usize;
            if clamp && covered[index] {
                continue;
            }
            let u = (i as f64 + 0.5) * inv;
            let m = (u as usize).min(steps - 1);
            let fu = u - m as f64;
            let mut c = line_c[m] + (line_c[m + 1] - line_c[m]) * fu;
            let mut r = line_r[m] + (line_r[m + 1] - line_r[m]) * fu;
            if c.is_nan() || r.is_nan() {
                continue;
            }
            if !clamp && (c < lo_c || c > hi_c || r < lo_r || r > hi_r) {
                continue;
            }
            if c < 0.0 {
                c = 0.0;
            } else if c > last_c as f64 {
                c = last_c as f64;
            }
            if r < 0.0 {
                r = 0.0;
            } else if r > last_r as f64 {
                r = last_r as f64;
            }
            let (c0, r0) = (c as usize, r as usize);
            let (fc, fr) = (c - c0 as f64, r - r0 as f64);
            let c1 = if c0 < last_c { c0 + 1 } else { c0 };
            let r1 = if r0 < last_r { r0 + 1 } else { r0 };
            let (top, bot) = (r0 * width, r1 * width);
            let v00 = f64::from(level.get(top + c0));
            let v01 = f64::from(level.get(top + c1));
            let v10 = f64::from(level.get(bot + c0));
            let v11 = f64::from(level.get(bot + c1));
            if !(v00.is_nan() || v01.is_nan() || v10.is_nan() || v11.is_nan()) {
                let a = v00 + (v01 - v00) * fc;
                let b = v10 + (v11 - v10) * fc;
                out[index] = a + (b - a) * fr;
                covered[index] = true;
                continue;
            }
            let (mut total, mut weight) = (0.0, 0.0);
            for (val, wgt) in [
                (v00, (1.0 - fc) * (1.0 - fr)),
                (v01, fc * (1.0 - fr)),
                (v10, (1.0 - fc) * fr),
                (v11, fc * fr),
            ] {
                if !val.is_nan() && wgt > 0.0 {
                    total += val * wgt;
                    weight += wgt;
                }
            }
            if weight > 0.0 {
                out[index] = total / weight;
                covered[index] = true;
            }
        }
    }
}
