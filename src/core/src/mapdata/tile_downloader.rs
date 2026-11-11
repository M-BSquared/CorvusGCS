//! Rate limited, resumable, cancellable XYZ tile downloader for offline
//! caching (`corvus/tile_downloader.py`).
//!
//! The operator downloads a map region at home, then flies offline. A job
//! enumerates the slippy map tiles of a WGS84 area and zoom range, skips what
//! the [`TileCache`] holds (so an interrupted job resumes where it stopped) and
//! fetches the rest with a small pool of threads. Fetched tiles are written in
//! batches of 256 in one transaction, and a tile counts as done only once it
//! is in the file.
//!
//! Every job runs on threads of its own: one orchestrator and `max_workers`
//! fetchers. `shutdown` cancels every job, drops the queued tiles and waits for
//! the orchestrators for a bounded time; a request already in flight ends by
//! its own 15 s timeout.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::fetch::{Fetch, Req};
use super::tile_cache::{Region, TileCache};
use super::tile_http::{mono, Listener};
use super::tile_sources as ts;
use super::util::{py_float, uuid4_hex, Event};

/// A single job never exceeds this many tiles; bigger regions must be split.
pub const MAX_TILES_PER_JOB: i64 = 50_000;
const FETCH_TIMEOUT_S: f64 = 15.0;
const MAX_TILE_BYTES: usize = 4 * 1024 * 1024;
const SUBMIT_PAUSE_S: f64 = 0.05;
const WRITE_BATCH: usize = 256;
const WRITE_BATCH_SECONDS: f64 = 1.0;
const RETRY_STATUSES: [u16; 7] = [408, 425, 429, 500, 502, 503, 504];
const THROTTLE_STATUSES: [u16; 2] = [429, 503];
const DEFAULT_BACKOFF_S: f64 = 2.0;
const MAX_BACKOFF_S: f64 = 30.0;
/// How long `shutdown` waits for the orchestrators.
const SHUTDOWN_JOIN_S: f64 = 2.0;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Bounds are `(west, south, east, north)` in WGS84 degrees.
pub type Bounds = (f64, f64, f64, f64);
/// `(z, x_min, x_max, y_min, y_max)`.
pub type Range = (i64, i64, i64, i64, i64);

/// Slippy map column for a longitude at zoom `z`.
pub fn lon_to_x(lon: f64, z: i64) -> i64 {
    let n = (1u64 << z) as f64;
    ((lon + 180.0) / 360.0 * n).floor() as i64
}

/// Slippy map row for a latitude at zoom `z` (north is row 0), clamped to the
/// span Web Mercator is defined on.
pub fn lat_to_y(lat: f64, z: i64) -> i64 {
    let lat = lat.clamp(-85.0511, 85.0511);
    let r = lat.to_radians();
    let n = (1u64 << z) as f64;
    ((1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor() as i64
}

/// Per zoom ranges and the tile count. x and y are clamped to `[0, 2^z - 1]`;
/// a box across the antimeridian just has its edge columns swapped.
pub fn ranges_for_bounds(bounds: Bounds, minzoom: i64, maxzoom: i64) -> (Vec<Range>, i64) {
    let (w, s, e, n) = bounds;
    let mut ranges = Vec::new();
    let mut total = 0;
    for z in minzoom..=maxzoom {
        let n_tiles = 1i64 << z;
        let (mut x_min, mut x_max) = (lon_to_x(w, z), lon_to_x(e, z));
        if x_min > x_max {
            std::mem::swap(&mut x_min, &mut x_max);
        }
        x_min = x_min.min(n_tiles - 1).max(0);
        x_max = x_max.min(n_tiles - 1).max(0);
        let (mut y_min, mut y_max) = (lat_to_y(n, z), lat_to_y(s, z));
        if y_min > y_max {
            std::mem::swap(&mut y_min, &mut y_max);
        }
        y_min = y_min.min(n_tiles - 1).max(0);
        y_max = y_max.min(n_tiles - 1).max(0);
        total += (x_max - x_min + 1) * (y_max - y_min + 1);
        ranges.push((z, x_min, x_max, y_min, y_max));
    }
    (ranges, total)
}

pub fn tile_count(bounds: Bounds, minzoom: i64, maxzoom: i64) -> i64 {
    ranges_for_bounds(bounds, minzoom, maxzoom).1
}

pub fn tile_ranges(bounds: Bounds, minzoom: i64, maxzoom: i64) -> Vec<Range> {
    ranges_for_bounds(bounds, minzoom, maxzoom).0
}

/// Every `(z, x, y)` covering `bounds` across the zoom range, zoom by zoom,
/// columns before rows.
pub fn enumerate_tiles(bounds: Bounds, minzoom: i64, maxzoom: i64) -> impl Iterator<Item = (i64, i64, i64)> {
    tile_ranges(bounds, minzoom, maxzoom)
        .into_iter()
        .flat_map(|(z, x0, x1, y0, y1)| (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (z, x, y))))
}

/// A predicate: is `(z, x, y)` one of the tiles `enumerate_tiles` yields,
/// answered from the ranges instead of the area.
pub fn tile_cover(bounds: Bounds, minzoom: i64, maxzoom: i64) -> impl Fn((i64, i64, i64)) -> bool {
    let by_zoom: HashMap<i64, (i64, i64, i64, i64)> = tile_ranges(bounds, minzoom, maxzoom)
        .into_iter()
        .map(|(z, a, b, c, d)| (z, (a, b, c, d)))
        .collect();
    move |(z, x, y)| {
        by_zoom
            .get(&z)
            .is_some_and(|s| s.0 <= x && x <= s.1 && s.2 <= y && y <= s.3)
    }
}

/// Seconds a throttled upstream asked us to wait: the delta seconds form of
/// `Retry-After` only, anything else the default, capped.
pub fn retry_after(header: Option<&str>) -> f64 {
    let delay = header.and_then(py_float).unwrap_or(DEFAULT_BACKOFF_S);
    let delay = if delay.is_finite() && delay >= 0.0 {
        delay
    } else {
        DEFAULT_BACKOFF_S
    };
    delay.min(MAX_BACKOFF_S)
}

#[derive(Default)]
struct Counters {
    state: String,
    done: i64,
    total: i64,
    failed: i64,
    error: Option<String>,
    pending: Vec<(i64, i64, i64, Vec<u8>)>,
    pending_since: f64,
    backoff_until: f64,
}

#[derive(Default)]
struct Queue {
    tiles: VecDeque<(i64, i64, i64)>,
    closed: bool,
}

struct Job {
    job_id: String,
    source: String,
    ranges: Vec<Range>,
    template: String,
    token: String,
    on_progress: Option<Listener>,
    cancel: Event,
    counters: Mutex<Counters>,
    queue: Mutex<Queue>,
    queue_cv: Condvar,
    finished: Mutex<bool>,
    finished_cv: Condvar,
    has_thread: bool,
}

impl Job {
    fn snapshot(&self) -> Value {
        let c = lock(&self.counters);
        json!({
            "job_id": self.job_id, "source": self.source, "state": c.state, "done": c.done,
            "total": c.total, "failed": c.failed, "error": c.error,
        })
    }

    fn fire(&self) {
        if let Some(f) = &self.on_progress {
            let snap = self.snapshot();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&snap)));
        }
    }
}

/// What a test can shorten.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub submit_pause: f64,
    pub fetch_timeout: f64,
    pub batch_seconds: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            submit_pause: SUBMIT_PAUSE_S,
            fetch_timeout: FETCH_TIMEOUT_S,
            batch_seconds: WRITE_BATCH_SECONDS,
        }
    }
}

/// Resumable, cancellable, rate limited tile fetcher for one cache.
pub struct TileDownloader {
    engine: Arc<Engine>,
    jobs: Mutex<Vec<(String, Arc<Job>)>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    down: AtomicBool,
}

/// What the threads of a job share: the cache, the network and the stop flag.
struct Engine {
    cache: Arc<TileCache>,
    net: Arc<dyn Fetch>,
    user_agent: String,
    max_workers: usize,
    timing: Timing,
    stop: Event,
}

impl TileDownloader {
    pub fn new(cache: Arc<TileCache>, net: Arc<dyn Fetch>, user_agent: &str, max_workers: usize) -> TileDownloader {
        TileDownloader::with_timing(cache, net, user_agent, max_workers, Timing::default())
    }

    pub fn with_timing(
        cache: Arc<TileCache>,
        net: Arc<dyn Fetch>,
        user_agent: &str,
        max_workers: usize,
        timing: Timing,
    ) -> TileDownloader {
        TileDownloader {
            engine: Arc::new(Engine {
                cache,
                net,
                user_agent: user_agent.to_string(),
                max_workers: max_workers.max(1),
                timing,
                stop: Event::new(),
            }),
            jobs: Mutex::new(Vec::new()),
            threads: Mutex::new(Vec::new()),
            down: AtomicBool::new(false),
        }
    }

    /// Schedules a download job and returns its id. A job whose tile set is
    /// empty or over the cap is recorded in state `failed` and its id returned
    /// (never an error), so the endpoint can say why.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        source: &str,
        template: &str,
        bounds: Bounds,
        minzoom: i64,
        maxzoom: i64,
        on_progress: Option<Listener>,
        token: &str,
    ) -> String {
        let job_id = uuid4_hex();
        if !(0..=60).contains(&minzoom) || !(0..=60).contains(&maxzoom) {
            self.record_failed(&job_id, source, 0, "enumeration error: zoom out of range");
            return job_id;
        }
        let (ranges, total) = ranges_for_bounds(bounds, minzoom, maxzoom);
        if total == 0 {
            self.record_failed(&job_id, source, 0, "no tiles in range");
            return job_id;
        }
        if total > MAX_TILES_PER_JOB {
            self.record_failed(
                &job_id,
                source,
                total,
                &format!("too many tiles ({total}); per-job cap is {MAX_TILES_PER_JOB}"),
            );
            return job_id;
        }
        let job = Arc::new(Job {
            job_id: job_id.clone(),
            source: source.to_string(),
            ranges,
            template: template.to_string(),
            token: token.to_string(),
            on_progress,
            cancel: Event::new(),
            counters: Mutex::new(Counters {
                state: "running".into(),
                total,
                ..Counters::default()
            }),
            queue: Mutex::new(Queue::default()),
            queue_cv: Condvar::new(),
            finished: Mutex::new(false),
            finished_cv: Condvar::new(),
            has_thread: true,
        });
        let me = Arc::clone(&self.engine);
        let j2 = Arc::clone(&job);
        let spawned = std::thread::Builder::new()
            .name(format!("tile-dl-{}", &job_id[..8]))
            .spawn(move || me.run(&j2));
        match spawned {
            Ok(handle) => {
                lock(&self.jobs).push((job_id.clone(), job));
                let mut threads = lock(&self.threads);
                threads.retain(|t| !t.is_finished());
                threads.push(handle);
            }
            Err(e) => self.record_failed(&job_id, source, total, &format!("enumeration error: {e}")),
        }
        job_id
    }

    fn find(&self, job_id: &str) -> Option<Arc<Job>> {
        lock(&self.jobs)
            .iter()
            .find(|(id, _)| id == job_id)
            .map(|(_, j)| Arc::clone(j))
    }

    /// Signals a running job to stop; true when the job is known.
    pub fn cancel(&self, job_id: &str) -> bool {
        match self.find(job_id) {
            Some(job) => {
                job.cancel.set();
                true
            }
            None => false,
        }
    }

    /// Waits up to `timeout` seconds for a job to end; true once it has. An
    /// unknown job counts as ended.
    pub fn wait(&self, job_id: &str, timeout: f64) -> bool {
        let Some(job) = self.find(job_id) else { return true };
        if !job.has_thread {
            return true;
        }
        let guard = lock(&job.finished);
        let (guard, _) = job
            .finished_cv
            .wait_timeout_while(guard, Duration::from_secs_f64(timeout.max(0.0)), |f| !*f)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// A snapshot of the job's public state.
    pub fn status(&self, job_id: &str) -> Option<Value> {
        self.find(job_id).map(|j| j.snapshot())
    }

    /// Snapshots of every job: running, done, cancelled and failed.
    pub fn list_jobs(&self) -> Vec<Value> {
        let jobs: Vec<Arc<Job>> = lock(&self.jobs).iter().map(|(_, j)| Arc::clone(j)).collect();
        jobs.iter().map(|j| j.snapshot()).collect()
    }

    /// Cancels every job, drops the queued fetches and joins the orchestrators
    /// for a bounded time. Idempotent.
    pub fn shutdown(&self) {
        if self.down.swap(true, Ordering::SeqCst) {
            return;
        }
        self.engine.stop.set();
        let jobs: Vec<Arc<Job>> = lock(&self.jobs).iter().map(|(_, j)| Arc::clone(j)).collect();
        for job in &jobs {
            job.cancel.set();
            let mut q = lock(&job.queue);
            q.tiles.clear();
            q.closed = true;
            job.queue_cv.notify_all();
        }
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.threads));
        let deadline = Instant::now() + Duration::from_secs_f64(SHUTDOWN_JOIN_S);
        for h in handles {
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }

    fn record_failed(&self, job_id: &str, source: &str, total: i64, error: &str) {
        let job = Arc::new(Job {
            job_id: job_id.to_string(),
            source: source.to_string(),
            ranges: Vec::new(),
            template: String::new(),
            token: String::new(),
            on_progress: None,
            cancel: Event::new(),
            counters: Mutex::new(Counters {
                state: "failed".into(),
                total,
                error: Some(error.to_string()),
                ..Counters::default()
            }),
            queue: Mutex::new(Queue::default()),
            queue_cv: Condvar::new(),
            finished: Mutex::new(true),
            finished_cv: Condvar::new(),
            has_thread: false,
        });
        lock(&self.jobs).push((job_id.to_string(), job));
    }
}

impl Engine {
    fn stopped(&self, job: &Job) -> bool {
        job.cancel.is_set() || self.stop.is_set()
    }

    /// Sits out a throttle the upstream asked for. False if the job was stopped.
    fn wait_backoff(&self, job: &Job) -> bool {
        loop {
            let delay = lock(&job.counters).backoff_until - mono();
            if delay <= 0.0 {
                return !self.stopped(job);
            }
            if job.cancel.wait(Duration::from_secs_f64(delay.min(MAX_BACKOFF_S))) {
                return false;
            }
        }
    }

    fn download_one(&self, job: &Job, z: i64, x: i64, y: i64) {
        if self.stopped(job) {
            return;
        }
        let url = ts::build_tile_url(&job.template, z, x, y, &job.token);
        let headers = [("User-Agent", self.user_agent.as_str())];
        let req = Req {
            url: &url,
            headers: &headers,
            body: None,
            timeout: Duration::from_secs_f64(self.timing.fetch_timeout),
            max_body: MAX_TILE_BYTES,
        };
        let mut blob: Option<Vec<u8>> = None;
        for attempt in 0..2 {
            if !self.wait_backoff(job) {
                break;
            }
            match self.net.send(&req) {
                Ok(resp) if resp.status < 400 => {
                    if !resp.body.is_empty() && ts::looks_like_image(&resp.body) {
                        blob = Some(resp.body);
                    }
                    break;
                }
                Ok(resp) => {
                    if THROTTLE_STATUSES.contains(&resp.status) {
                        let until = mono() + retry_after(resp.header("Retry-After"));
                        let mut c = lock(&job.counters);
                        c.backoff_until = c.backoff_until.max(until);
                    }
                    if !RETRY_STATUSES.contains(&resp.status) {
                        break;
                    }
                }
                Err(e) if e.is_too_large() => break,
                Err(_) => {}
            }
            if attempt == 1 || self.stopped(job) {
                break;
            }
        }
        let Some(blob) = blob else {
            lock(&job.counters).failed += 1;
            job.fire();
            return;
        };
        let now = mono();
        let due = {
            let mut c = lock(&job.counters);
            if c.pending.is_empty() {
                c.pending_since = now;
            }
            c.pending.push((z, x, y, blob));
            c.pending.len() >= WRITE_BATCH || now - c.pending_since >= self.timing.batch_seconds
        };
        if due {
            self.flush_pending(job);
        }
    }

    /// Commits whatever this job has buffered, and counts it; a whole batch
    /// succeeds or fails together. Never fails.
    fn flush_pending(&self, job: &Job) -> usize {
        let batch = {
            let mut c = lock(&job.counters);
            c.pending_since = 0.0;
            std::mem::take(&mut c.pending)
        };
        if batch.is_empty() {
            return 0;
        }
        let written = self.cache.put_tiles(&batch).unwrap_or(0);
        {
            let mut c = lock(&job.counters);
            c.done += written as i64;
            c.failed += (batch.len() - written) as i64;
        }
        job.fire();
        written
    }

    fn worker(&self, job: &Job) {
        loop {
            let tile = {
                let mut q = lock(&job.queue);
                loop {
                    if let Some(t) = q.tiles.pop_front() {
                        break Some(t);
                    }
                    if q.closed {
                        break None;
                    }
                    q = job.queue_cv.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            };
            let Some((z, x, y)) = tile else { return };
            self.download_one(job, z, x, y);
        }
    }

    fn run(self: &Arc<Self>, job: &Arc<Job>) {
        let workers: Vec<JoinHandle<()>> = (0..self.max_workers)
            .filter_map(|_| {
                let (me, job) = (Arc::clone(self), Arc::clone(job));
                std::thread::Builder::new()
                    .name("tile-dl-worker".into())
                    .spawn(move || me.worker(&job))
                    .ok()
            })
            .collect();
        'outer: for (z, x_min, x_max, y_min, y_max) in job.ranges.iter().copied() {
            for x in x_min..=x_max {
                for y in y_min..=y_max {
                    if self.stopped(job) {
                        break 'outer;
                    }
                    if self.cache.has_tile(z, x, y) {
                        lock(&job.counters).done += 1;
                        job.fire();
                        continue;
                    }
                    {
                        let mut q = lock(&job.queue);
                        if q.closed {
                            break 'outer;
                        }
                        q.tiles.push_back((z, x, y));
                        job.queue_cv.notify_one();
                    }
                    // Paced: do not hammer the upstream.
                    if self.stop.wait(Duration::from_secs_f64(self.timing.submit_pause)) {
                        break 'outer;
                    }
                }
            }
        }
        let stopped = self.stopped(job);
        {
            let mut q = lock(&job.queue);
            // Queued fetches are dropped only when the job was stopped; a
            // normal run drains them so the final counters are accurate.
            if stopped {
                q.tiles.clear();
            }
            q.closed = true;
            job.queue_cv.notify_all();
        }
        for w in workers {
            let _ = w.join();
        }
        self.flush_pending(job);
        {
            let mut c = lock(&job.counters);
            if c.state == "running" {
                if self.stopped(job) {
                    c.state = "cancelled".into();
                } else if c.done == 0 && c.failed > 0 {
                    c.state = "failed".into();
                    c.error = Some("no tile could be downloaded. Check the internet connection and try again".into());
                } else {
                    c.state = "done".into();
                }
            }
        }
        job.fire();
        *lock(&job.finished) = true;
        job.finished_cv.notify_all();
    }
}

/// One downloader per source, behind the single `start`, `cancel`, `status`,
/// `list_jobs` and `shutdown` the routes want.
pub struct DownloaderPool {
    downloaders: Vec<(String, Arc<TileDownloader>)>,
}

impl DownloaderPool {
    pub fn new(
        caches: &[(String, Arc<TileCache>)],
        net: &Arc<dyn Fetch>,
        user_agent: &str,
        max_workers: usize,
    ) -> DownloaderPool {
        DownloaderPool {
            downloaders: caches
                .iter()
                .map(|(sid, cache)| {
                    (
                        sid.clone(),
                        Arc::new(TileDownloader::new(
                            Arc::clone(cache),
                            Arc::clone(net),
                            user_agent,
                            max_workers,
                        )),
                    )
                })
                .collect(),
        }
    }

    pub fn from_downloaders(downloaders: Vec<(String, Arc<TileDownloader>)>) -> DownloaderPool {
        DownloaderPool { downloaders }
    }

    /// Starts a job for `source`; `Err` names a source without a downloader.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        source: &str,
        template: &str,
        bounds: Bounds,
        minzoom: i64,
        maxzoom: i64,
        on_progress: Option<Listener>,
        token: &str,
    ) -> Result<String, String> {
        let dl = self
            .downloaders
            .iter()
            .find(|(s, _)| s == source)
            .map(|(_, d)| d)
            .ok_or_else(|| format!("no downloader for source '{source}'"))?;
        Ok(dl.start(source, template, bounds, minzoom, maxzoom, on_progress, token))
    }

    pub fn cancel(&self, job_id: &str) -> bool {
        self.downloaders.iter().any(|(_, d)| d.cancel(job_id))
    }

    pub fn wait(&self, job_id: &str, timeout: f64) -> bool {
        for (_, d) in &self.downloaders {
            if d.status(job_id).is_some() {
                return d.wait(job_id, timeout);
            }
        }
        true
    }

    pub fn status(&self, job_id: &str) -> Option<Value> {
        self.downloaders.iter().find_map(|(_, d)| d.status(job_id))
    }

    pub fn list_jobs(&self) -> Vec<Value> {
        self.downloaders.iter().flat_map(|(_, d)| d.list_jobs()).collect()
    }

    pub fn shutdown(&self) {
        for (_, d) in &self.downloaders {
            d.shutdown();
        }
    }
}

/// Deletes the tiles of `region` that no other region still covers. Regions
/// overlap by design, so every candidate is checked against the remaining
/// regions first. Returns the number of tiles removed.
pub fn delete_region_tiles(cache: &TileCache, region: &Region) -> usize {
    let area = |r: &Region| (r.bounds(), r.minzoom, r.maxzoom);
    let (b, lo, hi) = area(region);
    let mut doomed: std::collections::HashSet<(i64, i64, i64)> = enumerate_tiles(b, lo, hi).collect();
    for other in cache.list_regions() {
        if doomed.is_empty() {
            break;
        }
        if other.id == region.id {
            continue;
        }
        let (ob, olo, ohi) = area(&other);
        let still_needed = tile_cover(ob, olo, ohi);
        doomed.retain(|t| !still_needed(*t));
    }
    if doomed.is_empty() {
        return 0;
    }
    let list: Vec<_> = doomed.into_iter().collect();
    cache.delete_tiles(&list)
}
