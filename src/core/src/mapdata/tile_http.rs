//! The interactive tile path's helpers (`corvus/tile_http.py`): the offline
//! circuit breaker, the memory of tiles a regional elevation source does not
//! have, and the download progress bus. (The per source downloader pool is in
//! `tile_downloader.rs`.)

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use serde_json::Value;

/// After this many consecutive failures the fill path stops trying.
pub const TILE_UPSTREAM_FAIL_THRESHOLD: u32 = 3;
/// How long to stay tripped before probing the network again.
pub const TILE_UPSTREAM_COOLDOWN_S: f64 = 30.0;

/// Seconds on a monotonic clock that started with the process.
pub fn mono() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct BreakerState {
    failures: u32,
    open_until: f64,
}

/// Circuit breaker around the interactive tile cache fill. Field use is
/// offline, so a pan onto ground that was not downloaded makes a whole
/// viewport of misses at once and each one that tries the network costs the
/// full timeout. After `threshold` consecutive failures misses fail instantly
/// for `cooldown` seconds; the first request after that goes through as a
/// probe, and any success closes the breaker.
pub struct UpstreamBreaker {
    threshold: u32,
    cooldown: f64,
    state: Mutex<BreakerState>,
}

impl Default for UpstreamBreaker {
    fn default() -> Self {
        UpstreamBreaker::new(TILE_UPSTREAM_FAIL_THRESHOLD, TILE_UPSTREAM_COOLDOWN_S)
    }
}

impl UpstreamBreaker {
    pub fn new(threshold: u32, cooldown: f64) -> UpstreamBreaker {
        UpstreamBreaker {
            threshold: threshold.max(1),
            cooldown,
            state: Mutex::new(BreakerState::default()),
        }
    }

    /// Should a network attempt be made now. After the cooldown this is true
    /// once per window (the probe).
    pub fn allow(&self) -> bool {
        self.allow_at(mono())
    }

    pub fn allow_at(&self, now: f64) -> bool {
        let mut s = lock(&self.state);
        if s.failures < self.threshold {
            return true;
        }
        if now >= s.open_until {
            s.open_until = now + self.cooldown;
            return true;
        }
        false
    }

    pub fn record_success(&self) {
        let mut s = lock(&self.state);
        s.failures = 0;
        s.open_until = 0.0;
    }

    pub fn record_failure(&self) {
        self.record_failure_at(mono());
    }

    pub fn record_failure_at(&self, now: f64) {
        let mut s = lock(&self.state);
        s.failures += 1;
        if s.failures >= self.threshold {
            s.open_until = now + self.cooldown;
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open_at(mono())
    }

    pub fn is_open_at(&self, now: f64) -> bool {
        let s = lock(&self.state);
        s.failures >= self.threshold && now < s.open_until
    }

    pub fn reset(&self) {
        self.record_success();
    }
}

/// Tiles a regional elevation source said it does not have (HTTP 404). Above
/// its `sparse_above` zoom a missing tile means its children are missing too.
/// Memory only and bounded.
pub struct AbsentTiles {
    limit: usize,
    keys: Mutex<HashSet<(String, i64, i64, i64)>>,
}

impl Default for AbsentTiles {
    fn default() -> Self {
        AbsentTiles::new(20000)
    }
}

impl AbsentTiles {
    pub fn new(limit: usize) -> AbsentTiles {
        AbsentTiles {
            limit: limit.max(1),
            keys: Mutex::new(HashSet::new()),
        }
    }

    pub fn add(&self, source: &str, z: i64, x: i64, y: i64) {
        let mut keys = lock(&self.keys);
        if keys.len() >= self.limit {
            keys.clear();
        }
        keys.insert((source.to_string(), z, x, y));
    }

    /// Is (z, x, y), or an ancestor finer than `floor`, known absent?
    pub fn covers(&self, source: &str, z: i64, x: i64, y: i64, floor: i64) -> bool {
        let keys = lock(&self.keys);
        let mut level = z;
        while level > floor {
            let shift = z - level;
            if keys.contains(&(source.to_string(), level, x >> shift, y >> shift)) {
                return true;
            }
            level -= 1;
        }
        false
    }
}

/// A subscriber of one job's progress.
pub type Listener = Arc<dyn Fn(&Value) + Send + Sync>;

/// Pub sub fan out, so many streams can watch one download job: the
/// downloader calls the one callback `make_on_progress` returns and the bus
/// routes each update to the subscribers of its `job_id`.
#[derive(Default)]
pub struct ProgressBus {
    next: Mutex<u64>,
    subs: Mutex<HashMap<String, Vec<(u64, Listener)>>>,
}

impl ProgressBus {
    pub fn new() -> ProgressBus {
        ProgressBus::default()
    }

    /// Subscribes `f` to `job_id`; the id is what `unsubscribe` takes.
    pub fn subscribe(&self, job_id: &str, f: Listener) -> u64 {
        let id = {
            let mut n = lock(&self.next);
            *n += 1;
            *n
        };
        lock(&self.subs).entry(job_id.to_string()).or_default().push((id, f));
        id
    }

    pub fn unsubscribe(&self, job_id: &str, id: u64) {
        let mut subs = lock(&self.subs);
        if let Some(list) = subs.get_mut(job_id) {
            list.retain(|(i, _)| *i != id);
            if list.is_empty() {
                subs.remove(job_id);
            }
        }
    }

    /// The single `on_progress` callback to hand to the downloader. An update
    /// without a `job_id` cannot be routed and is dropped.
    pub fn make_on_progress(self: &Arc<Self>) -> Listener {
        let bus = Arc::clone(self);
        Arc::new(move |progress: &Value| {
            let Some(job) = progress.get("job_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                return;
            };
            let subs: Vec<Listener> = lock(&bus.subs)
                .get(job)
                .map(|l| l.iter().map(|(_, f)| Arc::clone(f)).collect())
                .unwrap_or_default();
            for f in subs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(progress)));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn the_breaker_trips_probes_once_and_recovers() {
        let b = UpstreamBreaker::new(3, 30.0);
        assert!(b.allow_at(0.0));
        b.record_failure_at(1.0);
        b.record_failure_at(2.0);
        assert!(b.allow_at(2.5));
        b.record_failure_at(3.0);
        assert!(b.is_open_at(10.0));
        assert!(!b.allow_at(10.0));
        assert!(!b.allow_at(32.9));
        assert!(b.allow_at(33.0), "the probe");
        assert!(!b.allow_at(33.1), "only one probe a window");
        b.record_success();
        assert!(b.allow_at(33.2));
        assert!(!b.is_open_at(33.2));
        b.record_failure_at(40.0);
        b.reset();
        assert!(b.allow_at(40.1));
    }

    #[test]
    fn an_absent_tile_covers_its_descendants_down_to_the_floor() {
        let a = AbsentTiles::new(10);
        a.add("copernicus", 13, 4, 6);
        assert!(a.covers("copernicus", 15, 17, 25, 12));
        assert!(a.covers("copernicus", 13, 4, 6, 12));
        assert!(
            !a.covers("copernicus", 15, 17, 25, 13),
            "the floor level itself is never asked"
        );
        assert!(!a.covers("terrain", 15, 17, 25, 12));
        assert!(!a.covers("copernicus", 15, 20, 25, 12));
    }

    #[test]
    fn an_absent_memory_is_bounded() {
        let a = AbsentTiles::new(2);
        a.add("s", 13, 0, 0);
        a.add("s", 13, 1, 0);
        a.add("s", 13, 2, 0);
        assert!(!a.covers("s", 13, 0, 0, 12));
        assert!(a.covers("s", 13, 2, 0, 12));
    }

    #[test]
    fn progress_goes_to_the_subscribers_of_its_job_only() {
        let bus = Arc::new(ProgressBus::new());
        let (a, b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (a2, b2) = (a.clone(), b.clone());
        let ia = bus.subscribe(
            "j1",
            Arc::new(move |_| {
                a2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.subscribe(
            "j2",
            Arc::new(move |_| {
                b2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.subscribe("j1", Arc::new(|_| panic!("a bad subscriber")));
        let cb = bus.make_on_progress();
        cb(&json!({"job_id": "j1", "state": "running"}));
        cb(&json!({"state": "running"}));
        cb(&json!({"job_id": "", "state": "running"}));
        assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (1, 0));
        bus.unsubscribe("j1", ia);
        cb(&json!({"job_id": "j1"}));
        assert_eq!(a.load(Ordering::SeqCst), 1);
    }
}
