//! OSM building footprints with heights, for the map's 3D mode
//! (`buildings.py`).
//!
//! Requests are snapped to the slippy map tile grid at [`CELL_ZOOM`], so a pan
//! over ground already visited is a cache hit and the cached unit is the same
//! size everywhere. A cell that is not cached is fetched by a background
//! worker, and the request that found the miss answers "nothing yet" at once:
//! a handler waiting on Overpass would hold one of the few connections the map
//! needs for imagery. A miss with no network is an empty answer, never an
//! error; a hit is served whatever its age.
//!
//! The workers are joined by [`BuildingService::close`] for a bounded time (one
//! of them may be inside a socket read with seconds left on its timeout).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use super::fetch::{Fetch, Req};
use super::util::{quote_plus, truthy};

pub const CELL_ZOOM: i64 = 15;
pub const BLOCK_ZOOM: i64 = CELL_ZOOM - 1;
pub const OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";
pub const OVERPASS_TIMEOUT_S: f64 = 60.0;
pub const OVERPASS_MAX_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_CONCURRENT_FETCHES: usize = 2;
pub const FETCH_QUEUE_DEPTH: usize = 64;
pub const PREFETCH_MAX_BLOCKS: usize = 256;
pub const PREFETCH_MAX_EXAMINED: usize = 16 * PREFETCH_MAX_BLOCKS;
pub const BULK_MAX_TRIES: u32 = 3;
pub const CELL_TTL_S: f64 = 30.0 * 24.0 * 3600.0;
pub const METRES_PER_LEVEL: f64 = 3.2;
pub const DEFAULT_HEIGHT_M: f64 = 8.0;
pub const MIN_HEIGHT_M: f64 = 2.0;
pub const MAX_HEIGHT_M: f64 = 900.0;
pub const MAX_FEATURES_PER_CELL: usize = 8000;
const COORD_PRECISION: usize = 6;
const SHUTDOWN_JOIN_S: f64 = 2.0;

pub type Cell = (i64, i64, i64);

fn round_n(x: f64, digits: usize) -> f64 {
    format!("{x:.digits$}").parse().unwrap_or(x)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `(w, s, e, n)` degrees for slippy map cell (`z`, `x`, `y`).
pub fn cell_bounds(z: i64, x: i64, y: i64) -> (f64, f64, f64, f64) {
    let n_tiles = (1i64 << z) as f64;
    let (x, y) = (x as f64, y as f64);
    let pi = std::f64::consts::PI;
    let w = x / n_tiles * 360.0 - 180.0;
    let e = (x + 1.0) / n_tiles * 360.0 - 180.0;
    let north = (pi * (1.0 - 2.0 * y / n_tiles)).sinh().atan().to_degrees();
    let south = (pi * (1.0 - 2.0 * (y + 1.0) / n_tiles)).sinh().atan().to_degrees();
    (w, south, e, north)
}

/// Metres from an OSM length tag, or None. Only the leading numeric part is
/// read; anything that is not a finite number is no height at all.
pub fn parse_length(raw: &Value) -> Option<f64> {
    match raw {
        Value::Number(n) => n.as_f64().filter(|v| v.is_finite()),
        Value::String(s) => {
            let text = s.trim().replace(',', ".");
            let mut number = String::new();
            for ch in text.chars() {
                if ch.is_ascii_digit() || (ch == '.' && !number.contains('.')) || (ch == '-' && number.is_empty()) {
                    number.push(ch);
                } else {
                    break;
                }
            }
            super::util::py_float(&number).filter(|v| v.is_finite())
        }
        _ => None,
    }
}

/// `(min_height, height)` in metres for an OSM building's tags.
pub fn building_height(tags: &Value) -> (f64, f64) {
    let get = |k: &str| tags.get(k).filter(|_| tags.is_object()).cloned().unwrap_or(Value::Null);
    let mut height = parse_length(&get("height"));
    if height.is_none() {
        if let Some(levels) = parse_length(&get("building:levels")).filter(|l| *l > 0.0) {
            height = Some(levels * METRES_PER_LEVEL + 1.0);
        }
    }
    let height = height.unwrap_or(DEFAULT_HEIGHT_M).clamp(MIN_HEIGHT_M, MAX_HEIGHT_M);
    let mut base = parse_length(&get("min_height"));
    if base.is_none() {
        base = Some(match parse_length(&get("building:min_level")) {
            Some(level) => level * METRES_PER_LEVEL,
            None => 0.0,
        });
    }
    let mut base = base.unwrap_or(0.0);
    if !base.is_finite() || base < 0.0 {
        base = 0.0;
    }
    base = base.min(height - MIN_HEIGHT_M / 2.0);
    (base.max(0.0), height)
}

fn coord(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

/// A closed `[[lon, lat], ...]` ring from an Overpass `geometry` list.
fn ring(geometry: &Value) -> Option<Vec<[f64; 2]>> {
    let list = geometry.as_array().filter(|a| a.len() >= 4)?;
    let mut ring: Vec<[f64; 2]> = Vec::with_capacity(list.len() + 1);
    for node in list {
        let node = node.as_object()?;
        let (lon, lat) = (coord(node.get("lon")?)?, coord(node.get("lat")?)?);
        ring.push([round_n(lon, COORD_PRECISION), round_n(lat, COORD_PRECISION)]);
    }
    if ring.first() != ring.last() {
        ring.push(ring[0]);
    }
    (ring.len() >= 4).then_some(ring)
}

/// Convert an Overpass `out geom` response into a FeatureCollection. A
/// multipolygon relation contributes one polygon per `outer` member; courtyards
/// are dropped rather than cut as holes.
pub fn to_geojson(overpass: &Value) -> Value {
    let mut features: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let elements = overpass.get("elements").and_then(Value::as_array);
    for element in elements.map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(element) = element.as_object() else { continue };
        let ty = element.get("type").cloned().unwrap_or(Value::Null);
        let id = element.get("id").cloned().unwrap_or(Value::Null);
        if !id.is_null() && !seen.insert(json!([ty, id]).to_string()) {
            continue;
        }
        let Some(tags) = element.get("tags").filter(|t| t.is_object()) else {
            continue;
        };
        if !truthy(tags.get("building").unwrap_or(&Value::Null)) {
            continue;
        }
        let (base, height) = building_height(tags);
        let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
        if ty == "way" {
            if let Some(r) = element.get("geometry").and_then(ring) {
                rings.push(r);
            }
        } else if ty == "relation" {
            if let Some(members) = element.get("members").and_then(Value::as_array) {
                for member in members {
                    let Some(member) = member.as_object() else { continue };
                    if member.get("role").and_then(Value::as_str) != Some("outer") {
                        continue;
                    }
                    if let Some(r) = member.get("geometry").and_then(ring) {
                        rings.push(r);
                    }
                }
            }
        }
        for r in rings {
            features.push(json!({
                "type": "Feature",
                "geometry": {"type": "Polygon", "coordinates": [r]},
                "properties": {"height": round_n(height, 1), "min_height": round_n(base, 1)},
            }));
        }
        if features.len() >= MAX_FEATURES_PER_CELL {
            break;
        }
    }
    json!({"type": "FeatureCollection", "features": features})
}

/// Did Overpass give up on the query while still answering 200?
pub fn overpass_failed(overpass: &Value) -> bool {
    let Some(obj) = overpass.as_object() else { return true };
    if let Some(remark) = obj.get("remark").and_then(Value::as_str) {
        if !remark.trim().is_empty() {
            let text = remark.to_lowercase();
            return text.contains("error") || text.contains("timed out") || text.contains("out of memory");
        }
    }
    false
}

fn clamp_lat(lat: f64) -> f64 {
    lat.clamp(-85.05112878, 85.05112878)
}

fn frac_y(lat: f64) -> f64 {
    let rad = lat.to_radians();
    (1.0 - ((rad.tan() + 1.0 / rad.cos()).ln() / std::f64::consts::PI)) / 2.0
}

/// The `(z, x, y)` grid cell containing a point.
pub fn cell_of(lon: f64, lat: f64, zoom: i64) -> Cell {
    let count = 1i64 << zoom;
    let lat = clamp_lat(lat);
    let x = ((lon + 180.0) / 360.0 * count as f64) as i64;
    let y = (frac_y(lat) * count as f64) as i64;
    (zoom, x.clamp(0, count - 1), y.clamp(0, count - 1))
}

/// Share a block's features out to its cells, each to exactly one (the cell its
/// vertex average falls in). Every cell in `cells` is answered, empty ones
/// included, so an empty cell is recorded as looked at.
pub fn split_by_cell(collection: &Value, cells: &[Cell]) -> Vec<(Cell, Value)> {
    let mut out: Vec<(Cell, Vec<Value>)> = Vec::new();
    for c in cells {
        if !out.iter().any(|(k, _)| k == c) {
            out.push((*c, Vec::new()));
        }
    }
    let zoom = cells.first().map_or(CELL_ZOOM, |c| c.0);
    for feature in collection
        .get("features")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or(&[])
    {
        let Some(ring) = feature.pointer("/geometry/coordinates/0").and_then(Value::as_array) else {
            continue;
        };
        let points: &[Value] = if ring.len() > 1 && ring.first() == ring.last() {
            &ring[..ring.len() - 1]
        } else {
            ring
        };
        if points.is_empty() {
            continue;
        }
        let (mut sx, mut sy) = (0.0, 0.0);
        let mut ok = true;
        for p in points {
            match (p.get(0).and_then(coord), p.get(1).and_then(coord)) {
                (Some(a), Some(b)) => {
                    sx += a;
                    sy += b;
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let n = points.len() as f64;
        let cell = cell_of(sx / n, sy / n, zoom);
        if let Some((_, bucket)) = out.iter_mut().find(|(k, _)| *k == cell) {
            if bucket.len() < MAX_FEATURES_PER_CELL {
                bucket.push(feature.clone());
            }
        }
    }
    out.into_iter()
        .map(|(c, f)| (c, json!({"type": "FeatureCollection", "features": f})))
        .collect()
}

/// The block a cell is fetched with. Off-grid zooms are their own block.
pub fn block_of(z: i64, x: i64, y: i64) -> Cell {
    if z != CELL_ZOOM {
        return (z, x, y);
    }
    (BLOCK_ZOOM, x >> 1, y >> 1)
}

/// The cells a block holds.
pub fn cells_of_block(z: i64, x: i64, y: i64) -> Vec<Cell> {
    if z != BLOCK_ZOOM {
        return vec![(z, x, y)];
    }
    let mut v = Vec::with_capacity(4);
    for dy in 0..2 {
        for dx in 0..2 {
            v.push((CELL_ZOOM, 2 * x + dx, 2 * y + dy));
        }
    }
    v
}

/// The Overpass QL asking for every building in the `(w, s, e, n)` box.
pub fn overpass_query(w: f64, s: f64, e: f64, n: f64) -> String {
    let bbox = format!("{s:.6},{w:.6},{n:.6},{e:.6}");
    format!(
        "[out:json][timeout:{}];(way[\"building\"]({bbox});relation[\"building\"][\"type\"=\"multipolygon\"]({bbox}););out geom;",
        OVERPASS_TIMEOUT_S as i64
    )
}

/// SQLite store of gzipped GeoJSON, one row per grid cell, in a file of its own.
/// Every method tolerates failure: a broken cache is a miss.
pub struct BuildingCache {
    path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

impl BuildingCache {
    pub fn new(path: impl Into<PathBuf>) -> Result<BuildingCache, String> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(&path).map_err(|e| e.to_string())?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))
            .map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS building_cells (
                 z INTEGER NOT NULL,
                 x INTEGER NOT NULL,
                 y INTEGER NOT NULL,
                 fetched_at REAL NOT NULL,
                 payload BLOB NOT NULL,
                 PRIMARY KEY (z, x, y)
             )",
        )
        .map_err(|e| e.to_string())?;
        Ok(BuildingCache {
            path,
            conn: Mutex::new(Some(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `(gzipped GeoJSON, fetched_at)` for a cell, or None.
    pub fn get(&self, z: i64, x: i64, y: i64) -> Option<(Vec<u8>, f64)> {
        let guard = lock(&self.conn);
        let c = guard.as_ref()?;
        c.query_row(
            "SELECT payload, fetched_at FROM building_cells WHERE z=?1 AND x=?2 AND y=?3",
            params![z, x, y],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, f64>(1)?)),
        )
        .ok()
    }

    /// Store `payload` (already gzipped) for a cell. Best effort.
    pub fn put(&self, z: i64, x: i64, y: i64, payload: &[u8]) {
        self.put_at(z, x, y, payload, now());
    }

    /// `put` with the fetch time given (a test ages a cell with it).
    pub fn put_at(&self, z: i64, x: i64, y: i64, payload: &[u8], fetched_at: f64) {
        let guard = lock(&self.conn);
        if let Some(c) = guard.as_ref() {
            let _ = c.execute(
                "INSERT OR REPLACE INTO building_cells (z, x, y, fetched_at, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![z, x, y, fetched_at, payload],
            );
        }
    }

    /// `{"cells": n, "bytes": n}`: what the operator has offline.
    pub fn stats(&self) -> Value {
        let guard = lock(&self.conn);
        let row = guard.as_ref().and_then(|c| {
            c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(payload)), 0) FROM building_cells",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok()
        });
        let (cells, bytes) = row.unwrap_or((0, 0));
        json!({"cells": cells, "bytes": bytes})
    }

    /// Close the connection. Idempotent, never fails.
    pub fn close(&self) {
        if let Some(c) = lock(&self.conn).take() {
            let _ = c.close();
        }
    }
}

#[derive(Default)]
struct Queues {
    view: VecDeque<Cell>,
    bulk: VecDeque<Cell>,
    queued: HashSet<Cell>,
    bulk_tries: HashMap<Cell, u32>,
}

struct Shared {
    cache: BuildingCache,
    net: Arc<dyn Fetch>,
    url: String,
    timeout: f64,
    user_agent: String,
    queues: Mutex<Queues>,
    wake: Condvar,
    stop: AtomicBool,
    offline_until: Mutex<f64>,
}

/// Settings of a [`BuildingService`].
#[derive(Clone)]
pub struct Options {
    pub url: String,
    pub timeout: f64,
    pub user_agent: String,
    pub workers: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            url: OVERPASS_URL.into(),
            timeout: OVERPASS_TIMEOUT_S,
            user_agent: "CorvusGCS".into(),
            workers: MAX_CONCURRENT_FETCHES,
        }
    }
}

/// Answers "what buildings are in cell (z, x, y)" from the cache, and fills the
/// cache in the background. The request thread never waits on Overpass; a block
/// is never queued twice; fetches are capped; "not coming" is said (offline, or
/// while Overpass asked us to back off). Blocks an offline download asked for
/// wait in a second list that the workers take from only when no view waits.
pub struct BuildingService {
    shared: Arc<Shared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl BuildingService {
    pub fn new(
        cache_path: impl Into<PathBuf>,
        net: Arc<dyn Fetch>,
        options: Options,
    ) -> Result<BuildingService, String> {
        let shared = Arc::new(Shared {
            cache: BuildingCache::new(cache_path)?,
            net,
            url: options.url,
            timeout: options.timeout,
            user_agent: options.user_agent,
            queues: Mutex::new(Queues::default()),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            offline_until: Mutex::new(0.0),
        });
        let mut handles = Vec::new();
        for i in 0..options.workers.max(1) {
            let s = Arc::clone(&shared);
            let h = std::thread::Builder::new()
                .name(format!("corvus-buildings-{i}"))
                .spawn(move || worker(&s))
                .map_err(|e| e.to_string())?;
            handles.push(h);
        }
        Ok(BuildingService {
            shared,
            workers: Mutex::new(handles),
        })
    }

    pub fn cache(&self) -> &BuildingCache {
        &self.shared.cache
    }

    /// `(payload, coming)` for a cell: the gzipped GeoJSON when cached, and
    /// whether a fetch that will fill the cell is queued or running. Never
    /// blocks.
    pub fn lookup(&self, z: i64, x: i64, y: i64) -> (Option<Vec<u8>>, bool) {
        if let Some((payload, fetched_at)) = self.shared.cache.get(z, x, y) {
            if now() - fetched_at >= CELL_TTL_S {
                let b = block_of(z, x, y);
                self.shared.enqueue(b);
            }
            return (Some(payload), false);
        }
        let b = block_of(z, x, y);
        (None, self.shared.enqueue(b))
    }

    /// Gzipped GeoJSON for a cell, or None when it is not cached yet (which also
    /// queues the fetch).
    pub fn cell(&self, z: i64, x: i64, y: i64) -> Option<Vec<u8>> {
        self.lookup(z, x, y).0
    }

    /// Queue the buildings of an area for offline use; returns the blocks queued.
    /// Nearest the centre first, capped at `limit`, skipping blocks whose cells
    /// are all cached and fresh.
    pub fn prefetch(&self, bounds: [f64; 4], limit: i64) -> usize {
        let mut queued = 0usize;
        let t = now();
        let cap = limit.max(0) as usize;
        for (examined, block) in blocks_outward(bounds).enumerate() {
            if queued >= cap || examined >= PREFETCH_MAX_EXAMINED {
                break;
            }
            let fresh = cells_of_block(block.0, block.1, block.2).iter().all(|c| {
                self.shared
                    .cache
                    .get(c.0, c.1, c.2)
                    .is_some_and(|hit| t - hit.1 < CELL_TTL_S)
            });
            if fresh {
                continue;
            }
            let mut q = lock(&self.shared.queues);
            if q.queued.contains(&block) {
                continue;
            }
            q.queued.insert(block);
            q.bulk.push_back(block);
            q.bulk_tries.entry(block).or_insert(0);
            drop(q);
            self.shared.wake.notify_one();
            queued += 1;
        }
        queued
    }

    /// What is on disk, and how many blocks are still waiting to be fetched.
    pub fn stats(&self) -> Value {
        let mut stats = self.shared.cache.stats();
        stats["queued"] = json!(lock(&self.shared.queues).queued.len());
        stats
    }

    /// Stop the workers, wait for them for a bounded time and close the cache.
    /// Idempotent.
    pub fn close(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        {
            let mut q = lock(&self.shared.queues);
            q.bulk.clear();
            q.bulk_tries.clear();
        }
        self.shared.wake.notify_all();
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.workers));
        let deadline = Instant::now() + Duration::from_secs_f64(SHUTDOWN_JOIN_S);
        for h in handles {
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
        self.shared.cache.close();
    }
}

impl Drop for BuildingService {
    fn drop(&mut self) {
        self.close();
    }
}

impl Shared {
    fn offline(&self) -> bool {
        now() < *lock(&self.offline_until)
    }

    /// Queue a block for fetching. True when a fetch for it is on its way.
    fn enqueue(&self, key: Cell) -> bool {
        if self.stop.load(Ordering::SeqCst) || self.offline() {
            return false;
        }
        let mut q = lock(&self.queues);
        if q.queued.contains(&key) {
            return true;
        }
        if q.view.len() >= FETCH_QUEUE_DEPTH {
            return false;
        }
        q.queued.insert(key);
        q.view.push_back(key);
        drop(q);
        self.wake.notify_one();
        true
    }

    /// The next block to fetch: a view's first, then a download's. None when the
    /// service is stopping.
    fn next(&self) -> Option<Cell> {
        let mut q = lock(&self.queues);
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(k) = q.view.pop_front() {
                return Some(k);
            }
            if !self.offline() {
                if let Some(k) = q.bulk.pop_front() {
                    return Some(k);
                }
            }
            let (guard, _) = self
                .wake
                .wait_timeout(q, Duration::from_millis(500))
                .unwrap_or_else(|e| e.into_inner());
            q = guard;
        }
    }

    /// Release a block's claim; put a download's failed block back in line.
    fn settle(&self, key: Cell, fetched: bool) {
        let mut q = lock(&self.queues);
        q.queued.remove(&key);
        let Some(tries) = q.bulk_tries.get(&key).copied() else {
            return;
        };
        if fetched || self.stop.load(Ordering::SeqCst) || tries + 1 >= BULK_MAX_TRIES {
            q.bulk_tries.remove(&key);
            return;
        }
        q.bulk_tries.insert(key, tries + 1);
        q.queued.insert(key);
        q.bulk.push_back(key);
    }

    fn back_off(&self, seconds: f64) {
        if seconds > 0.0 {
            *lock(&self.offline_until) = now() + seconds;
        }
    }

    /// One Overpass round trip for a block: `{cell: gzipped GeoJSON}`. None on
    /// any failure and on an answer Overpass itself marked as cut short.
    fn request(&self, z: i64, x: i64, y: i64) -> Option<Vec<(Cell, Vec<u8>)>> {
        let (w, s, e, n) = cell_bounds(z, x, y);
        let body = format!("data={}", quote_plus(&overpass_query(w, s, e, n)));
        let headers = [
            ("User-Agent", self.user_agent.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ];
        let req = Req {
            url: &self.url,
            headers: &headers,
            body: Some(body.as_bytes()),
            timeout: Duration::from_secs_f64(self.timeout + 5.0),
            max_body: OVERPASS_MAX_BYTES,
        };
        let answer = match self.net.send(&req) {
            Ok(a) => a,
            Err(e) if e.is_too_large() => return None,
            Err(_) => {
                self.back_off(30.0);
                return None;
            }
        };
        if answer.status >= 400 {
            self.back_off(if answer.status == 429 || answer.status == 504 {
                60.0
            } else {
                30.0
            });
            return None;
        }
        if answer.body.is_empty() {
            return None;
        }
        let parsed: Value = serde_json::from_str(&String::from_utf8_lossy(&answer.body)).ok()?;
        if overpass_failed(&parsed) {
            self.back_off(15.0);
            return None;
        }
        *lock(&self.offline_until) = 0.0;
        let split = split_by_cell(&to_geojson(&parsed), &cells_of_block(z, x, y));
        Some(split.into_iter().map(|(cell, c)| (cell, gzip_json(&c))).collect())
    }
}

fn worker(shared: &Shared) {
    while let Some(key) = shared.next() {
        let mut fetched = false;
        if let Some(cells) = shared.request(key.0, key.1, key.2) {
            fetched = true;
            for (cell, payload) in cells {
                shared.cache.put(cell.0, cell.1, cell.2, &payload);
            }
        }
        shared.settle(key, fetched);
    }
}

/// Serialize and gzip (level 6): in the cache and on the wire.
pub fn gzip_json(obj: &Value) -> Vec<u8> {
    let data = serde_json::to_vec(obj).unwrap_or_default();
    let mut z = GzEncoder::new(Vec::new(), Compression::new(6));
    let _ = z.write_all(&data);
    z.finish().unwrap_or_default()
}

/// Where the building cache lives inside the tile cache directory.
pub fn default_cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("buildings.sqlite")
}

fn cell_span(bounds: [f64; 4], zoom: i64) -> Option<(i64, i64, i64, i64)> {
    let [w, s, e, n] = bounds;
    if !bounds.iter().all(|v| v.is_finite()) {
        return None;
    }
    let s = s.max(-85.05112878);
    let n = n.min(85.05112878);
    if n < s || e < w {
        return None;
    }
    let count = 1i64 << zoom;
    let fx = |lon: f64| (((lon + 180.0) / 360.0 * count as f64) as i64).clamp(0, count - 1);
    let fy = |lat: f64| ((frac_y(lat) * count as f64) as i64).clamp(0, count - 1);
    Some((fx(w), fx(e), fy(n), fy(s)))
}

/// Every `(z, x, y)` cell covering the `(w, s, e, n)` bounds.
pub fn cells_for_bounds(bounds: [f64; 4], zoom: i64) -> Vec<Cell> {
    let Some((x0, x1, y0, y1)) = cell_span(bounds, zoom) else {
        return Vec::new();
    };
    let mut v = Vec::new();
    for x in x0..=x1 {
        for y in y0..=y1 {
            v.push((zoom, x, y));
        }
    }
    v
}

/// The blocks covering `bounds`, nearest its centre first, one at a time: square
/// rings grow out from the centre's block and nothing past the ring being
/// walked is ever computed.
pub fn blocks_outward(bounds: [f64; 4]) -> BlocksOutward {
    let [w, s, e, n] = bounds;
    match cell_span(bounds, BLOCK_ZOOM) {
        None => BlocksOutward::empty(),
        Some((x0, x1, y0, y1)) => {
            let (_, cx, cy) = cell_of((w + e) / 2.0, (s + n) / 2.0, BLOCK_ZOOM);
            let cx = cx.clamp(x0, x1);
            let cy = cy.clamp(y0, y1);
            let reach = (cx - x0).max(x1 - cx).max(cy - y0).max(y1 - cy);
            BlocksOutward {
                span: (x0, x1, y0, y1),
                centre: (cx, cy),
                reach,
                ring: 0,
                buf: VecDeque::new(),
                done: false,
            }
        }
    }
}

pub struct BlocksOutward {
    span: (i64, i64, i64, i64),
    centre: (i64, i64),
    reach: i64,
    ring: i64,
    buf: VecDeque<Cell>,
    done: bool,
}

impl BlocksOutward {
    fn empty() -> BlocksOutward {
        BlocksOutward {
            span: (0, 0, 0, 0),
            centre: (0, 0),
            reach: 0,
            ring: 0,
            buf: VecDeque::new(),
            done: true,
        }
    }
}

impl Iterator for BlocksOutward {
    type Item = Cell;

    fn next(&mut self) -> Option<Cell> {
        loop {
            if let Some(b) = self.buf.pop_front() {
                return Some(b);
            }
            if self.done || self.ring > self.reach {
                return None;
            }
            let (x0, x1, y0, y1) = self.span;
            let (cx, cy) = self.centre;
            let ring = self.ring;
            self.ring += 1;
            let mut edge: BTreeSet<(i64, i64)> = BTreeSet::new();
            let (xa, xb) = ((cx - ring).max(x0), (cx + ring).min(x1));
            let (ya, yb) = ((cy - ring).max(y0), (cy + ring).min(y1));
            for y in [cy - ring, cy + ring] {
                if y0 <= y && y <= y1 {
                    edge.extend((xa..=xb).map(|x| (x, y)));
                }
            }
            for x in [cx - ring, cx + ring] {
                if x0 <= x && x <= x1 {
                    edge.extend((ya..=yb).map(|y| (x, y)));
                }
            }
            let mut v: Vec<(i64, i64)> = edge.into_iter().collect();
            v.sort_by_key(|p| ((p.0 - cx).pow(2) + (p.1 - cy).pow(2), *p));
            self.buf.extend(v.into_iter().map(|(x, y)| (BLOCK_ZOOM, x, y)));
        }
    }
}

/// The empty FeatureCollection a cell that is not there answers with.
pub fn empty_collection() -> Value {
    Value::Object(Map::from_iter([
        ("type".to_string(), json!("FeatureCollection")),
        ("features".to_string(), json!([])),
    ]))
}
