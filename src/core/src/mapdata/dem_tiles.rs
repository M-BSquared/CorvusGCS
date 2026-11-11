//! Elevation tiles the cache does not hold, derived from ones it does
//! (`dem_tiles.py`).
//!
//! 3D mode drapes the map over a raster-dem source. Offline a tile that was
//! never downloaded is a 404, and MapLibre then fills that square from a coarser
//! tile, so two neighbours come from grids of different resolution and their
//! edges do not meet. The nearest cached ancestor is decoded, the requested
//! square is cut out of it and resampled bilinearly to full size, and the edges
//! are bent to meet the neighbours. Derived tiles are never written to the
//! cache. Both height packings (Terrarium, Mapbox Terrain-RGB) are linear in
//! the packed 24 bit integer, so the resampling needs no knowledge of which
//! one a source uses.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::{Compression, Crc};

const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Decoded tiles kept by the one shared cache and by a [`DemFallback`].
pub const DECODED_CACHE_SIZE: usize = 32;
/// Derived tiles a [`DemFallback`] keeps.
pub const DERIVED_CACHE_SIZE: usize = 48;

/// Packed 24 bit value to metres, per RGB packing: `height = packed * scale + offset`.
pub fn encoding(name: &str) -> Option<(f64, f64)> {
    match name {
        "terrarium" => Some((1.0 / 256.0, -32768.0)),
        "mapbox" => Some((0.1, -10000.0)),
        _ => None,
    }
}

/// A decoded elevation tile: packed 24 bit heights, row-major.
#[derive(Debug)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub values: Vec<u32>,
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

fn unfilter(raw: &[u8], width: usize, height: usize, bpp: usize) -> Option<Vec<u8>> {
    let stride = width.checked_mul(bpp)?;
    let need = height.checked_mul(stride.checked_add(1)?)?;
    if raw.len() < need {
        return None;
    }
    let mut out = vec![0u8; height * stride];
    let mut prev = vec![0u8; stride];
    let mut pos = 0;
    for row in 0..height {
        let kind = raw[pos];
        let mut line = raw[pos + 1..pos + 1 + stride].to_vec();
        pos += stride + 1;
        match kind {
            0 => {}
            1 => {
                for i in bpp..stride {
                    line[i] = line[i].wrapping_add(line[i - bpp]);
                }
            }
            2 => {
                for i in 0..stride {
                    line[i] = line[i].wrapping_add(prev[i]);
                }
            }
            3 => {
                for i in 0..stride {
                    let left = if i >= bpp { line[i - bpp] as u32 } else { 0 };
                    line[i] = line[i].wrapping_add(((left + prev[i] as u32) >> 1) as u8);
                }
            }
            4 => {
                for i in 0..stride {
                    if i >= bpp {
                        line[i] = line[i].wrapping_add(paeth(line[i - bpp], prev[i], prev[i - bpp]));
                    } else {
                        line[i] = line[i].wrapping_add(prev[i]);
                    }
                }
            }
            _ => return None,
        }
        out[row * stride..(row + 1) * stride].copy_from_slice(&line);
        prev = line;
    }
    Some(out)
}

/// Decode an RGB(A) elevation PNG into packed heights, or None. Only 8 bit,
/// non-interlaced truecolour images are accepted; alpha is ignored.
pub fn decode(data: &[u8]) -> Option<Grid> {
    if data.len() < 8 || &data[..8] != PNG_MAGIC {
        return None;
    }
    let (mut width, mut height, mut channels) = (0usize, 0usize, 0usize);
    let mut idat: Vec<u8> = Vec::new();
    let mut have_idat = false;
    let mut pos = 8usize;
    while pos + 8 <= data.len() {
        let length = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let kind = &data[pos + 4..pos + 8];
        let end = (pos + 8).saturating_add(length).min(data.len());
        let body = &data[pos + 8..end];
        pos = pos.saturating_add(12).saturating_add(length);
        match kind {
            b"IHDR" => {
                if body.len() != 13 {
                    return None;
                }
                width = u32::from_be_bytes(body[0..4].try_into().ok()?) as usize;
                height = u32::from_be_bytes(body[4..8].try_into().ok()?) as usize;
                let (depth, colour, interlace) = (body[8], body[9], body[12]);
                if depth != 8 || interlace != 0 {
                    return None;
                }
                channels = match colour {
                    2 => 3,
                    6 => 4,
                    _ => return None,
                };
            }
            b"IDAT" => {
                idat.extend_from_slice(body);
                have_idat = true;
            }
            b"IEND" => break,
            _ => {}
        }
    }
    if width == 0 || height == 0 || channels == 0 || !have_idat {
        return None;
    }
    let mut raw = Vec::new();
    ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).ok()?;
    let pixels = unfilter(&raw, width, height, channels)?;
    let values = (0..width * height)
        .map(|i| {
            let p = &pixels[i * channels..];
            (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
        })
        .collect();
    Some(Grid { width, height, values })
}

/// A small least recently used map.
struct Lru<K, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> Lru<K, V> {
    fn new(cap: usize) -> Lru<K, V> {
        Lru {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    fn touch(&mut self, key: &K) {
        if let Some(i) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(i).unwrap();
            self.order.push_back(k);
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        let v = self.map.get(key).cloned();
        if v.is_some() {
            self.touch(key);
        }
        v
    }

    fn put(&mut self, key: K, value: V) {
        if self.map.insert(key.clone(), value).is_some() {
            self.touch(&key);
        } else {
            self.order.push_back(key);
        }
        while self.map.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.map.retain(|k, _| keep(k));
        self.order.retain(|k| keep(k));
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

type SharedDecoded = Mutex<Lru<Vec<u8>, Arc<Grid>>>;

fn shared() -> &'static SharedDecoded {
    static SHARED: std::sync::OnceLock<SharedDecoded> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| Mutex::new(Lru::new(DECODED_CACHE_SIZE)))
}

/// `decode`, remembered by the PNG's bytes (one coarse tile is the ancestor of
/// every finer one cut from it).
fn decode_shared(data: &[u8]) -> Option<Arc<Grid>> {
    if let Some(g) = shared().lock().unwrap().get(&data.to_vec()) {
        return Some(g);
    }
    let grid = Arc::new(decode(data)?);
    shared().lock().unwrap().put(data.to_vec(), grid.clone());
    Some(grid)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut crc = Crc::new();
    crc.update(kind);
    crc.update(body);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// Encode packed 24 bit heights as an 8 bit RGB PNG.
pub fn encode(width: usize, height: usize, values: &[u32]) -> Vec<u8> {
    let stride = 3 * width;
    let mut raw = Vec::with_capacity(height * (stride + 1));
    for r in 0..height {
        raw.push(0);
        for v in &values[r * width..(r + 1) * width] {
            raw.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, *v as u8]);
        }
    }
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut z = ZlibEncoder::new(Vec::new(), Compression::new(6));
    z.write_all(&raw).expect("write to a Vec");
    let packed = z.finish().expect("finish into a Vec");
    let mut out = PNG_MAGIC.to_vec();
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &packed);
    chunk(&mut out, b"IEND", &[]);
    out
}

type Axis = Vec<(usize, usize, f64)>;

/// Source index pair and weight for every output pixel along one axis.
fn axis(size: usize, scale: i64, offset: i64, out_size: Option<usize>) -> Axis {
    let count = out_size.unwrap_or(size);
    let last = size - 1;
    (0..count)
        .map(|i| {
            let s = (offset * count as i64 + i as i64) as f64 + 0.5;
            let s = s / scale as f64 * (size as f64 / count as f64) - 0.5;
            let s = s.max(0.0).min(last as f64);
            let lo = s.floor() as usize;
            let hi = if lo < last { lo + 1 } else { lo };
            (lo, hi, s - lo as f64)
        })
        .collect()
}

fn resample_float(
    grid: &Grid,
    dz: u32,
    sub_x: i64,
    sub_y: i64,
    out_w: Option<usize>,
    out_h: Option<usize>,
) -> Vec<f64> {
    let scale = 1i64 << dz;
    let (width, values) = (grid.width, &grid.values);
    let cols = axis(width, scale, sub_x, out_w);
    let rows = axis(grid.height, scale, sub_y, out_h);
    let mut horizontal: HashMap<usize, Vec<f64>> = HashMap::new();
    let mut along = |row: usize| -> Vec<f64> {
        horizontal
            .entry(row)
            .or_insert_with(|| {
                let src = &values[row * width..row * width + width];
                cols.iter()
                    .map(|&(lo, hi, f)| src[lo] as f64 + (src[hi] as i64 - src[lo] as i64) as f64 * f)
                    .collect()
            })
            .clone()
    };
    let mut out = Vec::with_capacity(cols.len() * rows.len());
    for &(lo, hi, f) in &rows {
        let a = along(lo);
        if hi == lo || f == 0.0 {
            out.extend(a);
            continue;
        }
        let b = along(hi);
        out.extend(a.iter().zip(&b).map(|(p, q)| p + (q - p) * f));
    }
    out
}

fn pack(values: &[f64]) -> Vec<u32> {
    let top = (1i64 << 24) - 1;
    values.iter().map(|v| ((v + 0.5) as i64).clamp(0, top) as u32).collect()
}

/// The square `dz` zooms below `grid` at (`sub_x`, `sub_y`), full size.
pub fn resample(grid: &Grid, dz: u32, sub_x: i64, sub_y: i64) -> Vec<u32> {
    pack(&resample_float(grid, dz, sub_x, sub_y, None, None))
}

fn cut_edge(grid: &Grid, dz: u32, sub_x: i64, sub_y: i64, side: char) -> Vec<f64> {
    let scale = 1i64 << dz;
    let (width, values) = (grid.width, &grid.values);
    let cols = axis(width, scale, sub_x, None);
    let rows = axis(grid.height, scale, sub_y, None);
    let points: Vec<((usize, usize, f64), (usize, usize, f64))> = if side == 'w' || side == 'e' {
        let col = if side == 'w' { cols[0] } else { cols[cols.len() - 1] };
        rows.iter().map(|&r| (r, col)).collect()
    } else {
        let row = if side == 'n' { rows[0] } else { rows[rows.len() - 1] };
        cols.iter().map(|&c| (row, c)).collect()
    };
    points
        .into_iter()
        .map(|((rlo, rhi, fy), (clo, chi, fx))| {
            let at = |r: usize, c: usize| values[r * width + c] as f64;
            let (a, b) = (at(rlo, clo), at(rlo, chi));
            let (c, d) = (at(rhi, clo), at(rhi, chi));
            let upper = a + (b - a) * fx;
            let lower = c + (d - c) * fx;
            upper + (lower - upper) * fy
        })
        .collect()
}

fn edge(values: &[f64], width: usize, height: usize, side: char) -> Vec<f64> {
    match side {
        'w' => (0..height).map(|r| values[r * width]).collect(),
        'e' => (0..height).map(|r| values[r * width + width - 1]).collect(),
        'n' => values[..width].to_vec(),
        _ => values[(height - 1) * width..height * width].to_vec(),
    }
}

fn edge_u32(values: &[u32], width: usize, height: usize, side: char) -> Vec<f64> {
    match side {
        'w' => (0..height).map(|r| values[r * width] as f64).collect(),
        'e' => (0..height).map(|r| values[r * width + width - 1] as f64).collect(),
        'n' => values[..width].iter().map(|&v| v as f64).collect(),
        _ => values[(height - 1) * width..height * width]
            .iter()
            .map(|&v| v as f64)
            .collect(),
    }
}

fn blend_edges(values: &[f64], width: usize, height: usize, deltas: &HashMap<char, Vec<f64>>) -> Vec<f64> {
    let zero_v = vec![0.0; height];
    let zero_u = vec![0.0; width];
    let left = deltas.get(&'w').unwrap_or(&zero_v);
    let right = deltas.get(&'e').unwrap_or(&zero_v);
    let top = deltas.get(&'n').unwrap_or(&zero_u);
    let bottom = deltas.get(&'s').unwrap_or(&zero_u);
    let us: Vec<f64> = (0..width).map(|i| i as f64 / (width - 1) as f64).collect();
    let mut out = Vec::with_capacity(values.len());
    for j in 0..height {
        let v = j as f64 / (height - 1) as f64;
        let a = left[j];
        let span = right[j] - a;
        let iv = 1.0 - v;
        for (i, h) in values[j * width..(j + 1) * width].iter().enumerate() {
            out.push(h + a + span * us[i] + top[i] * iv + bottom[i] * v);
        }
    }
    out
}

/// The same ground in another height packing and at another pixel size (how a
/// keyed service's tile is stood in for by the free one). None when a packing
/// is unknown or the PNG unreadable.
pub fn transcode(png: &[u8], from: &str, to: &str, size: usize) -> Option<Vec<u8>> {
    let (scale_in, offset_in) = encoding(from)?;
    let (scale_out, offset_out) = encoding(to)?;
    if size < 2 {
        return None;
    }
    let grid = decode_shared(png)?;
    let mut values = resample_float(&grid, 0, 0, 0, Some(size), Some(size));
    if (scale_in, offset_in) != (scale_out, offset_out) {
        let factor = scale_in / scale_out;
        let shift = (offset_in - offset_out) / scale_out;
        for v in values.iter_mut() {
            *v = *v * factor + shift;
        }
    }
    Some(encode(size, size, &pack(&values)))
}

/// Metres for every pixel of `png`, or of the square `dz` zooms below it,
/// re-cut to `size` pixels a side when given.
pub fn heights(png: &[u8], enc: &str, dz: u32, sub_x: i64, sub_y: i64, size: Option<usize>) -> Option<Vec<f64>> {
    let (scale, offset) = encoding(enc)?;
    let grid = decode_shared(png)?;
    let values = resample_float(&grid, dz, sub_x, sub_y, size, size);
    Some(values.into_iter().map(|v| v * scale + offset).collect())
}

/// A `size` square PNG of metres `values`, row-major, in `enc`.
pub fn encode_heights(size: usize, values: &[f64], enc: &str) -> Option<Vec<u8>> {
    let (scale, offset) = encoding(enc)?;
    let packed: Vec<f64> = values.iter().map(|v| (v - offset) / scale).collect();
    Some(encode(size, size, &pack(&packed)))
}

/// One shot: derive a child tile from an ancestor PNG.
pub fn overzoom(ancestor_png: &[u8], dz: u32, sub_x: i64, sub_y: i64) -> Option<Vec<u8>> {
    if dz < 1 || sub_x < 0 || sub_y < 0 || sub_x >= (1i64 << dz) || sub_y >= (1i64 << dz) {
        return None;
    }
    let grid = decode_shared(ancestor_png)?;
    Some(encode(grid.width, grid.height, &resample(&grid, dz, sub_x, sub_y)))
}

const SIDES: [(char, i64, i64, char); 4] = [('w', -1, 0, 'e'), ('e', 1, 0, 'w'), ('n', 0, -1, 's'), ('s', 0, 1, 'n')];

/// Reads one tile of a source from the cache; None is a miss.
pub type GetTile<'a> = &'a dyn Fn(i64, i64, i64) -> Option<Vec<u8>>;

type Key = (String, i64, i64, i64);

/// Answers a missing elevation tile from the nearest cached ancestor, with the
/// edges bent to meet the neighbours. Holds two bounded in-memory caches and no
/// thread, so there is nothing to close on shutdown.
pub struct DemFallback {
    decoded: Mutex<Lru<Key, Arc<Grid>>>,
    derived: Mutex<Lru<Key, Arc<Vec<u8>>>>,
}

impl Default for DemFallback {
    fn default() -> Self {
        DemFallback::new(DECODED_CACHE_SIZE, DERIVED_CACHE_SIZE)
    }
}

impl DemFallback {
    pub fn new(decoded_size: usize, derived_size: usize) -> DemFallback {
        DemFallback {
            decoded: Mutex::new(Lru::new(decoded_size)),
            derived: Mutex::new(Lru::new(derived_size)),
        }
    }

    /// A PNG for (`z`, `x`, `y`) derived from a cached ancestor, or None when no
    /// ancestor is cached at all.
    pub fn tile(&self, source: &str, z: i64, x: i64, y: i64, get_tile: GetTile<'_>) -> Option<Vec<u8>> {
        let key = (source.to_string(), z, x, y);
        if let Some(hit) = self.derived.lock().unwrap().get(&key) {
            return Some((*hit).clone());
        }
        let (grid, dz, sub_x, sub_y) = self.finest(source, z, x, y, get_tile)?;
        let (width, height) = (grid.width, grid.height);
        let mut values = resample_float(&grid, dz, sub_x, sub_y, None, None);
        let count = 1i64 << z;
        let mut deltas: HashMap<char, Vec<f64>> = HashMap::new();
        for (side, dx, dy, theirs) in SIDES {
            let ny = y + dy;
            if !(0..count).contains(&ny) {
                continue;
            }
            let nx = (x + dx).rem_euclid(count);
            let Some((line, real)) = self.neighbour_edge(source, z, nx, ny, theirs, get_tile) else {
                continue;
            };
            let mine = edge(&values, width, height, side);
            if line.len() != mine.len() {
                continue;
            }
            let share = if real { 1.0 } else { 0.5 };
            deltas.insert(
                side,
                mine.iter()
                    .zip(&line)
                    .map(|(own, other)| (other - own) * share)
                    .collect(),
            );
        }
        if !deltas.is_empty() {
            values = blend_edges(&values, width, height, &deltas);
        }
        let blob = encode(width, height, &pack(&values));
        self.derived.lock().unwrap().put(key, Arc::new(blob.clone()));
        Some(blob)
    }

    /// Drop what is remembered, for one source or all of them.
    pub fn forget(&self, source: Option<&str>) {
        let mut decoded = self.decoded.lock().unwrap();
        let mut derived = self.derived.lock().unwrap();
        match source {
            None => {
                decoded.clear();
                derived.clear();
            }
            Some(s) => {
                decoded.retain(|k| k.0 != s);
                derived.retain(|k| k.0 != s);
            }
        }
    }

    fn finest(
        &self,
        source: &str,
        z: i64,
        x: i64,
        y: i64,
        get_tile: GetTile<'_>,
    ) -> Option<(Arc<Grid>, u32, i64, i64)> {
        for dz in 1..=z {
            if let Some(grid) = self.grid(source, z - dz, x >> dz, y >> dz, get_tile) {
                let mask = (1i64 << dz) - 1;
                return Some((grid, dz as u32, x & mask, y & mask));
            }
        }
        None
    }

    fn neighbour_edge(
        &self,
        source: &str,
        z: i64,
        x: i64,
        y: i64,
        side: char,
        get_tile: GetTile<'_>,
    ) -> Option<(Vec<f64>, bool)> {
        if let Some(grid) = self.grid(source, z, x, y, get_tile) {
            return Some((edge_u32(&grid.values, grid.width, grid.height, side), true));
        }
        let (ancestor, dz, sx, sy) = self.finest(source, z, x, y, get_tile)?;
        Some((cut_edge(&ancestor, dz, sx, sy, side), false))
    }

    fn grid(&self, source: &str, z: i64, x: i64, y: i64, get_tile: GetTile<'_>) -> Option<Arc<Grid>> {
        let key = (source.to_string(), z, x, y);
        if let Some(g) = self.decoded.lock().unwrap().get(&key) {
            return Some(g);
        }
        let blob = get_tile(z, x, y)?;
        let grid = Arc::new(decode(&blob)?);
        self.decoded.lock().unwrap().put(key, grid.clone());
        Some(grid)
    }
}
