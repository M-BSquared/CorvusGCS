//! Readers for an operator's own elevation grids: GeoTIFF, XYZ and ESRI ASCII
//! (`dem_import.py`).
//!
//! Every reader returns the same [`Grid`]: north-up rows of float32 heights with
//! NaN where the file has no data, the outer corner of its top-left pixel, the
//! pixel size and the EPSG code when the file states one. A file that cannot be
//! read correctly is refused with a sentence an operator can act on, never read
//! approximately. LZW, Deflate, PackBits and uncompressed TIFFs are read, in
//! strips or tiles, with or without a predictor.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use flate2::read::ZlibDecoder;

use super::util::{py_float, py_repr, round_half_even};

/// A grid is held in memory once while it is converted: 120 million cells.
pub const MAX_CELLS: usize = 120_000_000;

/// A file that is not a usable elevation grid, with the reason.
#[derive(Debug, Clone, PartialEq)]
pub struct DemImportError(pub String);

impl std::fmt::Display for DemImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DemImportError {}

fn err<T>(text: impl Into<String>) -> Result<T, DemImportError> {
    Err(DemImportError(text.into()))
}

fn io(e: std::io::Error) -> DemImportError {
    DemImportError(e.to_string())
}

/// North-up float32 heights and where they lie. `x0`/`y0` is the outer
/// (top-left) corner of the top-left pixel in the grid's own coordinates.
#[derive(Debug, Clone)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub x0: f64,
    pub y0: f64,
    pub dx: f64,
    pub dy: f64,
    pub values: Vec<f32>,
    pub epsg: Option<i64>,
}

/// `f"{n:,}"`.
pub fn commas(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn check_size(width: usize, height: usize) -> Result<(), DemImportError> {
    if width < 2 || height < 2 {
        return err("the grid is smaller than 2 by 2 points");
    }
    if width.saturating_mul(height) > MAX_CELLS {
        return err(format!(
            "the grid has {} points, more than the {} one file may have. Split it into smaller tiles",
            commas(width * height),
            commas(MAX_CELLS)
        ));
    }
    Ok(())
}

/// `"tiff"`, `"asc"` or `"xyz"` from the file's first bytes and name.
pub fn detect_format(path: &Path, filename: &str) -> Result<&'static str, DemImportError> {
    let mut head = vec![0u8; 512];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).map_err(io)?;
    head.truncate(n);
    if matches!(head.get(..4), Some(b"II*\x00" | b"MM\x00*" | b"II+\x00" | b"MM\x00+")) {
        return Ok("tiff");
    }
    let text: String = head.iter().map(|&b| b as char).collect();
    let text = text.trim_start().to_lowercase();
    if text.starts_with("ncols") || text.starts_with("nrows") {
        return Ok("asc");
    }
    let lower = filename.to_lowercase();
    if lower.ends_with(".tif") || lower.ends_with(".tiff") {
        return err("this .tif file is not a TIFF image");
    }
    Ok("xyz")
}

/// Read `path` as whichever of the three formats it is.
pub fn read_grid(path: &Path, filename: &str) -> Result<Grid, DemImportError> {
    match detect_format(path, filename)? {
        "tiff" => read_geotiff(path),
        "asc" => read_esri_ascii(path),
        _ => read_xyz(path),
    }
}

fn compression_name(code: u64) -> Option<&'static str> {
    Some(match code {
        6 => "old JPEG",
        7 => "JPEG",
        34887 => "LERC",
        34925 => "LZMA",
        50000 => "Zstandard",
        50001 => "WebP",
        34712 => "JPEG 2000",
        _ => return None,
    })
}

/// TIFF LZW (MSB first, early change), as libtiff writes it.
fn lzw_decode(data: &[u8]) -> Result<Vec<u8>, DemImportError> {
    let fresh = || -> Vec<Vec<u8>> {
        let mut t: Vec<Vec<u8>> = (0..=255u8).map(|i| vec![i]).collect();
        t.push(Vec::new());
        t.push(Vec::new());
        t
    };
    let mut out = Vec::new();
    let mut table = fresh();
    let mut bits = 9usize;
    let mut prev: Option<Vec<u8>> = None;
    let mut pos = 0usize;
    let total = data.len() * 8;
    let damaged = || DemImportError("the TIFF's LZW data is damaged".into());
    while pos + bits <= total {
        let byte = pos >> 3;
        let mut chunk = 0u32;
        for k in 0..3 {
            chunk = chunk << 8 | *data.get(byte + k).unwrap_or(&0) as u32;
        }
        let code = ((chunk >> (24 - (pos & 7) - bits)) & ((1 << bits) - 1)) as usize;
        pos += bits;
        if code == 256 {
            table.truncate(258);
            bits = 9;
            prev = None;
            continue;
        }
        if code == 257 {
            break;
        }
        let Some(p) = prev.take() else {
            let entry = table.get(code).ok_or_else(damaged)?.clone();
            out.extend_from_slice(&entry);
            prev = Some(entry);
            continue;
        };
        let entry = if code < table.len() {
            let entry = table[code].clone();
            let mut grown = p;
            grown.push(*entry.first().ok_or_else(damaged)?);
            table.push(grown);
            entry
        } else if code == table.len() {
            let mut entry = p.clone();
            entry.push(p[0]);
            table.push(entry.clone());
            entry
        } else {
            return Err(damaged());
        };
        out.extend_from_slice(&entry);
        prev = Some(entry);
        let size = table.len();
        if size >= 511 && bits == 9 {
            bits = 10;
        } else if size >= 1023 && bits == 10 {
            bits = 11;
        } else if size >= 2047 && bits == 11 {
            bits = 12;
        }
    }
    Ok(out)
}

fn packbits_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let c = data[i] as usize;
        i += 1;
        if c < 128 {
            let end = (i + c + 1).min(data.len());
            out.extend_from_slice(&data[i..end]);
            i += c + 1;
        } else if c > 128 {
            if let Some(&b) = data.get(i) {
                out.extend(std::iter::repeat_n(b, 257 - c));
            }
            i += 1;
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Order {
    Little,
    Big,
}

struct Tiff {
    file: File,
    order: Order,
    big: bool,
    tags: HashMap<u16, Vec<f64>>,
    text: HashMap<u16, String>,
}

fn type_size(t: u16) -> Option<usize> {
    Some(match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => return None,
    })
}

impl Tiff {
    fn u16(&self, b: &[u8]) -> u64 {
        let a: [u8; 2] = b[..2].try_into().unwrap();
        u64::from(if self.order == Order::Little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    }

    fn u32(&self, b: &[u8]) -> u64 {
        let a: [u8; 4] = b[..4].try_into().unwrap();
        u64::from(if self.order == Order::Little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }

    fn u64(&self, b: &[u8]) -> u64 {
        let a: [u8; 8] = b[..8].try_into().unwrap();
        if self.order == Order::Little {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        }
    }

    fn open(path: &Path) -> Result<Tiff, DemImportError> {
        let mut file = File::open(path).map_err(io)?;
        let mut head = [0u8; 16];
        let mut got = 0;
        while got < 16 {
            let n = file.read(&mut head[got..]).map_err(io)?;
            if n == 0 {
                break;
            }
            got += n;
        }
        let order = match &head[..2] {
            b"II" => Order::Little,
            b"MM" => Order::Big,
            _ => return err("not a TIFF file"),
        };
        let mut t = Tiff {
            file,
            order,
            big: false,
            tags: HashMap::new(),
            text: HashMap::new(),
        };
        let magic = t.u16(&head[2..4]);
        if magic != 42 && magic != 43 {
            return err("not a TIFF file");
        }
        t.big = magic == 43;
        let first = if t.big { t.u64(&head[8..16]) } else { t.u32(&head[4..8]) };
        t.read_ifd(first)?;
        Ok(t)
    }

    fn read_exact_at(&mut self, offset: u64, size: usize) -> Result<Vec<u8>, DemImportError> {
        self.file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut buf = Vec::new();
        (&mut self.file).take(size as u64).read_to_end(&mut buf).map_err(io)?;
        Ok(buf)
    }

    fn read_ifd(&mut self, offset: u64) -> Result<(), DemImportError> {
        let damaged = || DemImportError("the TIFF header is damaged".into());
        let (count, entry_size, inline) = if self.big {
            let c = self.read_exact_at(offset, 8)?;
            if c.len() < 8 {
                return Err(damaged());
            }
            (self.u64(&c), 20usize, 8usize)
        } else {
            let c = self.read_exact_at(offset, 2)?;
            if c.len() < 2 {
                return Err(damaged());
            }
            (self.u16(&c), 12, 4)
        };
        if count > 4096 {
            return err("the TIFF header is damaged");
        }
        let skip = if self.big { 8 } else { 2 };
        let raw = self.read_exact_at(offset + skip, count as usize * entry_size)?;
        if raw.len() < count as usize * entry_size {
            return Err(damaged());
        }
        for i in 0..count as usize {
            let e = &raw[i * entry_size..(i + 1) * entry_size];
            let (tag, typ, n, reference) = if self.big {
                (
                    self.u16(&e[0..2]) as u16,
                    self.u16(&e[2..4]) as u16,
                    self.u64(&e[4..12]),
                    &e[12..20],
                )
            } else {
                (
                    self.u16(&e[0..2]) as u16,
                    self.u16(&e[2..4]) as u16,
                    self.u32(&e[4..8]),
                    &e[8..12],
                )
            };
            let Some(size) = type_size(typ) else { continue };
            let total = (size as u64).checked_mul(n).ok_or_else(damaged)?;
            let blob = if total <= inline as u64 {
                reference[..total as usize].to_vec()
            } else {
                let at = if self.big {
                    self.u64(reference)
                } else {
                    self.u32(reference)
                };
                if total > 1 << 30 {
                    return Err(damaged());
                }
                let b = self.read_exact_at(at, total as usize)?;
                if (b.len() as u64) < total {
                    return Err(damaged());
                }
                b
            };
            match typ {
                2 => {
                    let end = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
                    self.text.insert(tag, blob[..end].iter().map(|&b| b as char).collect());
                }
                5 | 10 => {
                    let mut vals = Vec::new();
                    for k in 0..n as usize {
                        let a = &blob[k * 8..k * 8 + 8];
                        let (num, den) = if typ == 5 {
                            (self.u32(&a[..4]) as f64, self.u32(&a[4..]) as f64)
                        } else {
                            (
                                self.u32(&a[..4]) as u32 as i32 as f64,
                                self.u32(&a[4..]) as u32 as i32 as f64,
                            )
                        };
                        vals.push(if den != 0.0 { num / den } else { 0.0 });
                    }
                    self.tags.insert(tag, vals);
                }
                _ => {
                    let vals = blob.chunks(size).map(|c| self.number(typ, c)).collect();
                    self.tags.insert(tag, vals);
                }
            }
        }
        Ok(())
    }

    fn number(&self, typ: u16, c: &[u8]) -> f64 {
        match typ {
            1 | 7 => f64::from(c[0]),
            6 => f64::from(c[0] as i8),
            3 => self.u16(c) as f64,
            8 => f64::from(self.u16(c) as u16 as i16),
            4 => self.u32(c) as f64,
            9 => f64::from(self.u32(c) as u32 as i32),
            11 => f64::from(f32::from_bits(self.u32(c) as u32)),
            12 => f64::from_bits(self.u64(c)),
            16 | 18 => self.u64(c) as f64,
            17 => self.u64(c) as i64 as f64,
            _ => 0.0,
        }
    }

    fn get(&self, tag: u16) -> Option<&Vec<f64>> {
        self.tags.get(&tag)
    }

    fn one(&self, tag: u16) -> Option<f64> {
        self.tags.get(&tag).and_then(|v| v.first().copied())
    }
}

fn geokeys(t: &Tiff) -> HashMap<u64, u64> {
    let mut keys = HashMap::new();
    let Some(dir) = t.get(34735) else { return keys };
    if dir.len() < 4 {
        return keys;
    }
    let count = dir[3] as usize;
    for i in 0..count {
        let base = 4 + 4 * i;
        if base + 4 > dir.len() {
            break;
        }
        if dir[base + 1] == 0.0 {
            keys.insert(dir[base] as u64, dir[base + 3] as u64);
        }
    }
    keys
}

fn read_geotiff(path: &Path) -> Result<Grid, DemImportError> {
    let mut tiff = Tiff::open(path)?;
    read_geotiff_from(&mut tiff)
}

fn read_geotiff_from(tiff: &mut Tiff) -> Result<Grid, DemImportError> {
    let (Some(w), Some(h)) = (tiff.one(256), tiff.one(257)) else {
        return err("the TIFF has no image size");
    };
    if w == 0.0 || h == 0.0 {
        return err("the TIFF has no image size");
    }
    let (width, height) = (w as usize, h as usize);
    check_size(width, height)?;
    let bits = tiff.get(258).and_then(|v| v.first()).copied().unwrap_or(1.0) as u64;
    let samples = tiff.one(277).unwrap_or(1.0) as usize;
    let fmt = tiff.get(339).and_then(|v| v.first()).copied().unwrap_or(1.0) as u64;
    let compression = tiff.one(259).unwrap_or(1.0) as u64;
    let predictor = tiff.one(317).unwrap_or(1.0) as u64;
    let planar = tiff.one(284).unwrap_or(1.0) as u64;
    if ![1, 5, 8, 32946, 32773].contains(&compression) {
        let name = compression_name(compression)
            .map(String::from)
            .unwrap_or_else(|| format!("method {compression}"));
        return err(format!(
            "the TIFF is compressed with {name}, which Corvus cannot read. Save it again with LZW or Deflate compression (QGIS: Export, Save As)"
        ));
    }
    let code = match (fmt, bits) {
        (3, 32) => 'f',
        (3, 64) => 'd',
        (1, 8) => 'B',
        (1, 16) => 'H',
        (1, 32) => 'I',
        (2, 8) => 'b',
        (2, 16) => 'h',
        (2, 32) => 'i',
        _ => {
            return err(format!(
                "the TIFF stores {bits} bit samples of format {fmt}, which is not an elevation grid Corvus can read"
            ))
        }
    };
    let size = (bits / 8) as usize;

    let keys = geokeys(tiff);
    let scale = tiff.get(33550).filter(|v| !v.is_empty()).cloned();
    let tie = tiff.get(33922).filter(|v| !v.is_empty()).cloned();
    let matrix = tiff.get(34264).filter(|v| !v.is_empty()).cloned();
    let mut flip = false;
    let (dx, mut dy, mut x0, mut y0);
    let tiepoint = match (&scale, &tie) {
        (Some(s), Some(t)) if t.len() >= 6 => Some((s.clone(), t.clone())),
        _ => None,
    };
    if let Some((scale, tie)) = tiepoint {
        dx = scale[0];
        dy = *scale.get(1).unwrap_or(&0.0);
        let (i, j, x, y) = (tie[0], tie[1], tie[3], tie[4]);
        if dy < 0.0 {
            dy = -dy;
            flip = true;
        }
        x0 = x - i * dx;
        y0 = if !flip { y + j * dy } else { y - j * dy };
    } else if let Some(m) = matrix.as_ref().filter(|m| m.len() >= 16) {
        let (a, b, d) = (m[0], m[1], m[3]);
        let (e, f, hh) = (m[4], m[5], m[7]);
        if b.abs() > 1e-9 || e.abs() > 1e-9 {
            return err("the TIFF is rotated, which Corvus cannot place on the map");
        }
        dx = a;
        x0 = d;
        dy = -f;
        y0 = hh;
        if dy < 0.0 {
            dy = -dy;
            flip = true;
        }
    } else {
        return err("the TIFF has no georeferencing. Use a GeoTIFF, or the XYZ file it came with");
    }
    if !(dx > 0.0 && dy > 0.0) {
        return err("the TIFF's pixel size is not positive");
    }
    if keys.get(&1025) == Some(&2) {
        x0 -= dx / 2.0;
        y0 += if !flip { dy / 2.0 } else { -dy / 2.0 };
    }

    let mut epsg = None;
    let model = keys.get(&1024).copied();
    let projected = keys.get(&3072).copied();
    let geographic = keys.get(&2048).copied();
    if let Some(p) = projected.filter(|p| *p > 0 && *p < 32767) {
        epsg = Some(p as i64);
    } else if model == Some(2) {
        if let Some(g) = geographic.filter(|g| *g > 0 && *g < 32767) {
            epsg = Some(g as i64);
        }
    }

    let nodata = tiff.text.get(&42113).and_then(|s| py_float(s));

    let tiled = tiff.get(322).is_some();
    let (tw, th, offsets, counts);
    if tiled {
        tw = tiff.one(322).unwrap_or(0.0) as usize;
        th = tiff.one(323).unwrap_or(0.0) as usize;
        offsets = tiff.get(324).cloned();
        counts = tiff.get(325).cloned();
    } else {
        tw = width;
        let rows = tiff.one(278).unwrap_or(height as f64) as usize;
        th = (if rows == 0 { height } else { rows }).min(height);
        offsets = tiff.get(273).cloned();
        counts = tiff.get(279).cloned();
    }
    let (Some(offsets), Some(counts)) = (offsets, counts) else {
        return err("the TIFF's image data is missing");
    };
    if offsets.is_empty() || counts.is_empty() || tw == 0 || th == 0 {
        return err("the TIFF's image data is missing");
    }
    let across = width.div_ceil(tw);
    let down = height.div_ceil(th);
    let chunk_samples = if planar == 1 { samples.max(1) } else { 1 };
    let chunk_count = across * down;
    if offsets.len() < chunk_count || counts.len() < chunk_count {
        return err("the TIFF's image data is incomplete");
    }

    let mut out = vec![f32::NAN; width * height];
    let stride = tw * chunk_samples;
    for index in 0..chunk_count {
        let mut blob = tiff.read_exact_at(offsets[index] as u64, counts[index] as usize)?;
        match compression {
            5 => blob = lzw_decode(&blob)?,
            8 | 32946 => {
                let mut raw = Vec::new();
                ZlibDecoder::new(&blob[..])
                    .read_to_end(&mut raw)
                    .map_err(|_| DemImportError("the TIFF's Deflate data is damaged".into()))?;
                blob = raw;
            }
            32773 => blob = packbits_decode(&blob),
            _ => {}
        }
        let col0 = (index % across) * tw;
        let row0 = (index / across) * th;
        let rows = if tiled { th } else { th.min(height - row0) };
        let need = rows * stride * size;
        if blob.len() < need {
            blob.resize(need, 0);
        }
        let values: Vec<f64> = if predictor == 3 {
            let restored = undo_float_predictor(&blob[..need], rows, tw, chunk_samples, size, tiff.order);
            samples_of(&restored, code, tiff.order)
        } else {
            let mut v = samples_of(&blob[..need], code, tiff.order);
            if predictor == 2 {
                undo_horizontal(&mut v, rows, tw, chunk_samples, code)?;
            }
            v
        };
        let span = tw.min(width.saturating_sub(col0));
        for r in 0..rows {
            let dest_row = row0 + r;
            if dest_row >= height {
                break;
            }
            let src = r * stride;
            let dest = dest_row * width + col0;
            for c in 0..span {
                out[dest + c] = values[src + c * chunk_samples] as f32;
            }
        }
    }
    if nodata.is_some() || code == 'f' || code == 'd' {
        mask_nodata(&mut out, nodata);
    }
    if flip {
        let mut flipped = Vec::with_capacity(out.len());
        for r in (0..height).rev() {
            flipped.extend_from_slice(&out[r * width..(r + 1) * width]);
        }
        out = flipped;
    }
    Ok(Grid {
        width,
        height,
        x0,
        y0,
        dx,
        dy,
        values: out,
        epsg,
    })
}

fn samples_of(blob: &[u8], code: char, order: Order) -> Vec<f64> {
    let le = order == Order::Little;
    macro_rules! each {
        ($t:ty, $n:expr, $conv:expr) => {
            blob.chunks_exact($n)
                .map(|c| {
                    let a: [u8; $n] = c.try_into().unwrap();
                    let v = if le {
                        <$t>::from_le_bytes(a)
                    } else {
                        <$t>::from_be_bytes(a)
                    };
                    $conv(v)
                })
                .collect()
        };
    }
    match code {
        'f' => each!(f32, 4, |v: f32| f64::from(v)),
        'd' => each!(f64, 8, |v: f64| v),
        'B' => blob.iter().map(|&b| f64::from(b)).collect(),
        'b' => blob.iter().map(|&b| f64::from(b as i8)).collect(),
        'H' => each!(u16, 2, |v: u16| f64::from(v)),
        'h' => each!(i16, 2, |v: i16| f64::from(v)),
        'I' => each!(u32, 4, |v: u32| f64::from(v)),
        _ => each!(i32, 4, |v: i32| f64::from(v)),
    }
}

/// Predictor 2, in place: each sample is stored as the difference to its left.
fn undo_horizontal(
    values: &mut [f64],
    rows: usize,
    width: usize,
    samples: usize,
    code: char,
) -> Result<(), DemImportError> {
    if code == 'f' || code == 'd' {
        return err("the TIFF uses predictor 2 on floating point data");
    }
    let stride = width * samples;
    let signed = matches!(code, 'b' | 'h' | 'i');
    let bits: u32 = match code {
        'B' | 'b' => 8,
        'H' | 'h' => 16,
        _ => 32,
    };
    let top = 1i64 << bits;
    let half = top >> 1;
    for r in 0..rows {
        let base = r * stride;
        for i in base + samples..base + stride {
            let mut v = (values[i] as i64 + values[i - samples] as i64) & (top - 1);
            if signed && v >= half {
                v -= top;
            }
            values[i] = v as f64;
        }
    }
    Ok(())
}

/// Predictor 3: byte-wise differencing over byte planes, most significant first.
fn undo_float_predictor(blob: &[u8], rows: usize, width: usize, samples: usize, size: usize, order: Order) -> Vec<u8> {
    let stride = width * samples;
    let row_bytes = stride * size;
    let mut out = vec![0u8; blob.len()];
    for r in 0..rows {
        let mut row = blob[r * row_bytes..(r + 1) * row_bytes].to_vec();
        for i in samples..row_bytes {
            row[i] = row[i].wrapping_add(row[i - samples]);
        }
        let mut restored = vec![0u8; row_bytes];
        for k in 0..size {
            let pos = if order == Order::Little { size - 1 - k } else { k };
            for n in 0..stride {
                restored[pos + n * size] = row[k * stride + n];
            }
        }
        out[r * row_bytes..(r + 1) * row_bytes].copy_from_slice(&restored);
    }
    out
}

/// NaN for the file's no-data value and for anything that cannot be ground.
fn mask_nodata(values: &mut [f32], nodata: Option<f64>) {
    for v in values.iter_mut() {
        if v.is_nan() {
            continue;
        }
        let f = f64::from(*v);
        if nodata.is_some_and(|nd| f == nd || (f - nd).abs() < 1e-3) || f < -12000.0 || f > 10000.0 {
            *v = f32::NAN;
        }
    }
}

/// Lines as Python's text mode reads them (`\n`, `\r\n` and `\r` all end a
/// line), decoded as latin-1.
struct Lines<R: BufRead> {
    inner: R,
    queue: std::collections::VecDeque<String>,
}

impl<R: BufRead> Lines<R> {
    fn new(inner: R) -> Lines<R> {
        Lines {
            inner,
            queue: Default::default(),
        }
    }

    fn next_line(&mut self) -> Option<String> {
        loop {
            if let Some(l) = self.queue.pop_front() {
                return Some(l);
            }
            let mut raw = Vec::new();
            if self.inner.read_until(b'\n', &mut raw).ok()? == 0 {
                return None;
            }
            let text: String = raw.iter().map(|&b| b as char).collect();
            let ends = text.ends_with('\n');
            let body = text.strip_suffix('\n').unwrap_or(&text);
            let body = if ends {
                body.strip_suffix('\r').unwrap_or(body)
            } else {
                body
            };
            let parts: Vec<&str> = body.split('\r').collect();
            let last = parts.len() - 1;
            for (i, p) in parts.iter().enumerate() {
                let end = if i < last || ends { "\n" } else { "" };
                self.queue.push_back(format!("{p}{end}"));
            }
        }
    }
}

/// An ESRI ASCII grid (`.asc`): a short header, then rows north to south.
pub fn read_esri_ascii(path: &Path) -> Result<Grid, DemImportError> {
    let file = File::open(path).map_err(io)?;
    let mut lines = Lines::new(BufReader::new(file));
    let mut header: HashMap<String, f64> = HashMap::new();
    let mut line = String::new();
    while let Some(l) = lines.next_line() {
        line = l;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 && !parts[0].is_empty() && parts[0].chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
            match py_float(parts[1]) {
                Some(v) => {
                    header.insert(parts[0].to_lowercase(), v);
                    continue;
                }
                None => return err(format!("the header line {} is unreadable", py_repr(line.trim()))),
            }
        }
        break;
    }
    let (Some(w), Some(h)) = (header.get("ncols"), header.get("nrows")) else {
        return err("the ASCII grid has no ncols or nrows");
    };
    let (width, height) = (w.trunc() as usize, h.trunc() as usize);
    if *w < 0.0 || *h < 0.0 {
        return err("the grid is smaller than 2 by 2 points");
    }
    check_size(width, height)?;
    let cell = |k: &str| header.get("cellsize").or_else(|| header.get(k)).copied();
    let (dx, dy) = (cell("dx"), cell("dy"));
    let (Some(dx), Some(dy)) = (dx.filter(|v| *v != 0.0), dy.filter(|v| *v != 0.0)) else {
        return err("the ASCII grid has no cell size");
    };
    if dx <= 0.0 || dy <= 0.0 {
        return err("the ASCII grid has no cell size");
    }
    let x0 = if let Some(v) = header.get("xllcorner") {
        *v
    } else if let Some(v) = header.get("xllcenter") {
        v - dx / 2.0
    } else {
        return err("the ASCII grid has no xllcorner");
    };
    let ybottom = if let Some(v) = header.get("yllcorner") {
        *v
    } else if let Some(v) = header.get("yllcenter") {
        v - dy / 2.0
    } else {
        return err("the ASCII grid has no yllcorner");
    };
    let nodata = header.get("nodata_value").copied();
    let count = width * height;
    let mut values: Vec<f32> = Vec::with_capacity(count);
    let mut pending: Vec<String> = line.split_whitespace().map(String::from).collect();
    loop {
        for token in &pending {
            match py_float(token) {
                Some(v) => values.push(v as f32),
                None => return err(format!("{} in the ASCII grid is not a number", py_repr(token))),
            }
        }
        if values.len() >= count {
            break;
        }
        let Some(next) = lines.next_line() else { break };
        pending = next.split_whitespace().map(String::from).collect();
    }
    if values.len() < count {
        return err(format!(
            "the ASCII grid ends after {} of its {} values",
            commas(values.len()),
            commas(count)
        ));
    }
    values.truncate(count);
    mask_nodata(&mut values, nodata);
    Ok(Grid {
        width,
        height,
        x0,
        y0: ybottom + height as f64 * dy,
        dx,
        dy,
        values,
        epsg: None,
    })
}

fn xyz_rows(path: &Path, mut f: impl FnMut(f64, f64, f64)) -> Result<(), DemImportError> {
    let file = File::open(path).map_err(io)?;
    let mut lines = Lines::new(BufReader::new(file));
    while let Some(line) = lines.next_line() {
        let t = line.trim();
        let parts: Vec<&str> = split_runs(t);
        if parts.len() < 3 {
            continue;
        }
        let (Some(x), Some(y), Some(z)) = (py_float(parts[0]), py_float(parts[1]), py_float(parts[2])) else {
            continue;
        };
        f(x, y, z);
    }
    Ok(())
}

/// `re.split(r"[\s,;]+", text)`.
fn split_runs(text: &str) -> Vec<&str> {
    let is_sep = |c: char| c.is_whitespace() || c == ',' || c == ';';
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_run = false;
    let mut run_start = 0;
    for (i, c) in text.char_indices() {
        if is_sep(c) {
            if !in_run {
                in_run = true;
                run_start = i;
            }
        } else if in_run {
            parts.push(&text[start..run_start]);
            start = i;
            in_run = false;
        }
    }
    if in_run {
        parts.push(&text[start..run_start]);
        parts.push("");
    } else {
        parts.push(&text[start..]);
    }
    parts
}

fn round6(x: f64) -> f64 {
    format!("{x:.6}").parse().unwrap_or(x)
}

fn spacing(values: &HashSet<u64>) -> f64 {
    let mut ordered: Vec<f64> = values.iter().map(|b| f64::from_bits(*b)).collect();
    ordered.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut best = f64::INFINITY;
    for w in ordered.windows(2) {
        let step = w[1] - w[0];
        if 1e-9 < step && step < best {
            best = step;
        }
    }
    best
}

fn key(x: f64) -> u64 {
    let r = round6(x);
    (if r == 0.0 { 0.0 } else { r }).to_bits()
}

/// An XYZ point list on a regular grid, one `x y z` per line, any order. Two
/// passes over the file so the memory needed is the grid's, not the point
/// list's. Points that are not on the grid the spacing implies are a scattered
/// point cloud and are refused.
pub fn read_xyz(path: &Path) -> Result<Grid, DemImportError> {
    let (mut xmin, mut ymin) = (f64::INFINITY, f64::INFINITY);
    let (mut xmax, mut ymax) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut xs: HashSet<u64> = HashSet::new();
    let mut ys: HashSet<u64> = HashSet::new();
    let mut points = 0usize;
    xyz_rows(path, |x, y, _| {
        points += 1;
        xmin = xmin.min(x);
        xmax = xmax.max(x);
        ymin = ymin.min(y);
        ymax = ymax.max(y);
        if xs.len() < 200_000 {
            xs.insert(key(x));
        }
        if ys.len() < 200_000 {
            ys.insert(key(y));
        }
    })?;
    let _ = ymin;
    if points < 4 {
        return err("the file holds no x y z lines");
    }
    let dx = spacing(&xs);
    let dy = spacing(&ys);
    if !(dx.is_finite() && dy.is_finite()) {
        return err("the points do not form a grid (all on one line)");
    }
    let width = (round_half_even((xmax - xmin) / dx) + 1).max(0) as usize;
    let height = (round_half_even((ymax - ymin) / dy) + 1).max(0) as usize;
    check_size(width, height)?;
    if (points as f64) < 0.25 * (width * height) as f64 {
        return err("the points are not on a regular grid. Corvus reads gridded XYZ files, not scattered point clouds");
    }
    let mut values = vec![f32::NAN; width * height];
    let mut off_grid = 0usize;
    xyz_rows(path, |x, y, z| {
        let fc = (x - xmin) / dx;
        let fr = (ymax - y) / dy;
        let c = round_half_even(fc);
        let r = round_half_even(fr);
        if (fc - c as f64).abs() > 0.1
            || (fr - r as f64).abs() > 0.1
            || c < 0
            || r < 0
            || c as usize >= width
            || r as usize >= height
        {
            off_grid += 1;
            return;
        }
        if -12000.0 < z && z < 10000.0 {
            values[r as usize * width + c as usize] = z as f32;
        }
    })?;
    if off_grid as f64 > points as f64 * 0.01 {
        return err("the points are not on a regular grid. Corvus reads gridded XYZ files, not scattered point clouds");
    }
    Ok(Grid {
        width,
        height,
        x0: xmin - dx / 2.0,
        y0: ymax + dy / 2.0,
        dx,
        dy,
        values,
        epsg: None,
    })
}

/// EPSG 4326 for a grid whose coordinates can only be degrees, else None.
pub fn guess_epsg(grid: &Grid) -> Option<i64> {
    let x1 = grid.x0 + grid.width as f64 * grid.dx;
    let y1 = grid.y0 - grid.height as f64 * grid.dy;
    if (-180.5..=180.5).contains(&grid.x0)
        && (-180.5..=180.5).contains(&x1)
        && (-90.5..=90.5).contains(&y1)
        && (-90.5..=90.5).contains(&grid.y0)
        && grid.dx < 1.0
    {
        return Some(4326);
    }
    None
}
