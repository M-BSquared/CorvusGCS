//! Map tiles fetched and decoded on worker threads, kept in a bounded cache.
//!
//! `get` is the whole interface for a frame: it answers from the cache and
//! otherwise queues the tile, newest first, and the frame that finds it
//! there picks it up. Requests nobody repeats age out of the queue.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::net::http::{self, Server};

/// A tile: source id, zoom, column and row.
pub type Key = (String, u8, u32, u32);

const WORKERS: usize = 6;
const QUEUE_MAX: usize = 640;
const RETRY_AFTER: Duration = Duration::from_secs(20);

type Decoder<T> = Arc<dyn Fn(&[u8]) -> Option<T> + Send + Sync>;

enum Slot<T> {
    Loading,
    Ready(Arc<T>),
    Failed(Instant),
}

struct Inner<T> {
    slots: HashMap<Key, Slot<T>>,
    order: VecDeque<Key>,
    queue: VecDeque<Key>,
    /// The weight of the tiles held, by `Shared::weigh`.
    bytes: usize,
}

type PathOf = Arc<dyn Fn(&Key) -> String + Send + Sync>;

struct Shared<T> {
    weigh: fn(&T) -> usize,
    path: PathOf,
    retry: Duration,
    inner: Mutex<Inner<T>>,
    wake: Condvar,
    stop: AtomicBool,
    /// Counts every tile that came and every clear, so what was made from
    /// the tiles knows when it may be out of date.
    generation: AtomicU64,
}

/// A cache of decoded tiles of type `T`.
pub struct TileCache<T: Send + Sync + 'static> {
    shared: Arc<Shared<T>>,
    capacity: usize,
    max_bytes: usize,
    threads: Vec<JoinHandle<()>>,
}

impl<T: Send + Sync + 'static> TileCache<T> {
    /// Fetches through `server` and decodes with `decode`; repaints `ctx`
    /// whenever a tile lands. Trimmed to at most `capacity` tiles and
    /// `max_bytes` of decoded tiles as `weigh` counts them, whatever size the
    /// tiles come in.
    pub fn weighed(
        ctx: &egui::Context,
        server: &Server,
        capacity: usize,
        weigh: fn(&T) -> usize,
        max_bytes: usize,
        decode: impl Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
    ) -> TileCache<T> {
        let mut c = TileCache::build(
            ctx,
            server,
            capacity,
            weigh,
            |k| format!("/api/tiles/{}/{}/{}/{}.png", k.0, k.1, k.2, k.3),
            RETRY_AFTER,
            decode,
        );
        c.max_bytes = max_bytes;
        c
    }

    /// A cache of anything addressed by `(name, z, x, y)`: `path` says where
    /// the backend serves it and `retry` how long a miss waits before it is
    /// asked for again.
    pub fn with_path(
        ctx: &egui::Context,
        server: &Server,
        capacity: usize,
        path: impl Fn(&Key) -> String + Send + Sync + 'static,
        retry: Duration,
        decode: impl Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
    ) -> TileCache<T> {
        TileCache::build(ctx, server, capacity, |_| 0, path, retry, decode)
    }

    fn build(
        ctx: &egui::Context,
        server: &Server,
        capacity: usize,
        weigh: fn(&T) -> usize,
        path: impl Fn(&Key) -> String + Send + Sync + 'static,
        retry: Duration,
        decode: impl Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
    ) -> TileCache<T> {
        let shared = Arc::new(Shared {
            weigh,
            path: Arc::new(path),
            retry,
            inner: Mutex::new(Inner {
                slots: HashMap::new(),
                order: VecDeque::new(),
                queue: VecDeque::new(),
                bytes: 0,
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        });
        let decode = Arc::new(decode);
        let threads = (0..WORKERS)
            .filter_map(|i| {
                let (sh, srv, ctx, dec) = (shared.clone(), server.clone(), ctx.clone(), decode.clone());
                std::thread::Builder::new()
                    .name(format!("tiles-{i}"))
                    .spawn(move || worker(sh, srv, ctx, dec))
                    .ok()
            })
            .collect();
        TileCache {
            shared,
            capacity,
            max_bytes: usize::MAX,
            threads,
        }
    }

    /// The tile when it is here; queues it when it is not.
    pub fn get(&self, key: &Key) -> Option<Arc<T>> {
        let mut g = self.shared.inner.lock().ok()?;
        match g.slots.get(key) {
            Some(Slot::Ready(t)) => return Some(t.clone()),
            Some(Slot::Loading) => {
                if let Some(i) = g.queue.iter().position(|k| k == key) {
                    let k = g.queue.remove(i).expect("position is in range");
                    g.queue.push_back(k);
                }
                return None;
            }
            Some(Slot::Failed(at)) if at.elapsed() < self.shared.retry => return None,
            _ => {}
        }
        g.slots.insert(key.clone(), Slot::Loading);
        g.queue.push_back(key.clone());
        while g.queue.len() > QUEUE_MAX {
            if let Some(old) = g.queue.pop_front() {
                g.slots.remove(&old);
            }
        }
        drop(g);
        self.shared.wake.notify_one();
        None
    }

    /// Every tile asked for and not here yet.
    #[cfg(test)]
    pub fn queued(&self) -> Vec<Key> {
        self.shared.inner.lock().map_or(Vec::new(), |g| {
            g.slots
                .iter()
                .filter(|(_, s)| matches!(s, Slot::Loading))
                .map(|(k, _)| k.clone())
                .collect()
        })
    }

    /// The tile when it is here, never queueing it.
    pub fn peek(&self, key: &Key) -> Option<Arc<T>> {
        match self.shared.inner.lock().ok()?.slots.get(key) {
            Some(Slot::Ready(t)) => Some(t.clone()),
            _ => None,
        }
    }

    /// How many tiles the backend could not give, within the retry time.
    pub fn failures(&self) -> usize {
        let Ok(g) = self.shared.inner.lock() else {
            return 0;
        };
        g.slots
            .values()
            .filter(|s| matches!(s, Slot::Failed(at) if at.elapsed() < self.shared.retry))
            .count()
    }

    /// Drops every tile, for a changed source.
    pub fn clear(&self) {
        if let Ok(mut g) = self.shared.inner.lock() {
            g.slots.clear();
            g.order.clear();
            g.queue.clear();
            g.bytes = 0;
        }
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Moves on whenever a tile arrives or the cache is cleared. Tiles
    /// trimmed away do not move it: what was made from them stays right.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }

    /// What the tiles held weigh, as `weigh` counts them.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.shared.inner.lock().map_or(0, |g| g.bytes)
    }

    /// How many decoded tiles are held.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.shared.inner.lock().map_or(0, |g| g.order.len())
    }

    /// Evicts the oldest tiles past the capacity.
    pub fn trim(&self) {
        if let Ok(mut g) = self.shared.inner.lock() {
            while g.order.len() > self.capacity || (g.bytes > self.max_bytes && g.order.len() > 1) {
                let Some(old) = g.order.pop_front() else { break };
                if let Some(Slot::Ready(t)) = g.slots.remove(&old) {
                    g.bytes = g.bytes.saturating_sub((self.shared.weigh)(&t));
                }
            }
        }
    }
}

impl<T: Send + Sync + 'static> Drop for TileCache<T> {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn worker<T: Send + Sync + 'static>(shared: Arc<Shared<T>>, server: Server, ctx: egui::Context, decode: Decoder<T>) {
    loop {
        let key = {
            let Ok(mut g) = shared.inner.lock() else {
                return;
            };
            loop {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(k) = g.queue.pop_back() {
                    break k;
                }
                g = match shared.wake.wait_timeout(g, Duration::from_millis(250)) {
                    Ok((g, _)) => g,
                    Err(_) => return,
                };
            }
        };
        let path = (shared.path)(&key);
        let tile = http::get_bytes(&server, &path).and_then(|b| decode(&b));
        if let Ok(mut g) = shared.inner.lock() {
            if g.slots.contains_key(&key) {
                match tile {
                    Some(t) => {
                        g.bytes += (shared.weigh)(&t);
                        g.slots.insert(key.clone(), Slot::Ready(Arc::new(t)));
                        g.order.push_back(key);
                        shared.generation.fetch_add(1, Ordering::Relaxed);
                    }
                    None => {
                        g.slots.insert(key, Slot::Failed(Instant::now()));
                    }
                }
            }
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn a_tile_is_fetched_once_decoded_and_then_served_from_the_cache() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h2 = hits.clone();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                h2.fetch_add(1, Ordering::SeqCst);
                let body = if head.contains("/api/tiles/sat/3/1/2.png") {
                    "abc"
                } else {
                    ""
                };
                let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        let ctx = egui::Context::default();
        let cache: TileCache<usize> = TileCache::weighed(&ctx, &server, 10, |_| 0, usize::MAX, |b| Some(b.len()));
        let key: Key = ("sat".into(), 3, 1, 2);
        assert!(cache.get(&key).is_none(), "the first ask only queues it");
        let deadline = Instant::now() + Duration::from_secs(3);
        let tile = loop {
            if let Some(t) = cache.peek(&key) {
                break t;
            }
            assert!(Instant::now() < deadline, "the tile did not arrive");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(*tile, 3);
        assert_eq!(cache.generation(), 1, "one tile came");
        assert!(cache.get(&key).is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let missing: Key = ("sat".into(), 9, 9, 9);
        cache.get(&missing);
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            cache.get(&missing).is_none(),
            "a tile the backend does not have stays failed"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2, "and is not asked for again at once");
    }

    #[test]
    fn a_byte_budget_trims_the_oldest_tiles_whatever_their_count() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789");
            }
        });
        let ctx = egui::Context::default();
        let cache: TileCache<Vec<u8>> = TileCache::weighed(&ctx, &server, 100, |t| t.len(), 25, |b| Some(b.to_vec()));
        for x in 0..5 {
            let key: Key = ("sat".into(), 3, x, 0);
            cache.get(&key);
            let deadline = Instant::now() + Duration::from_secs(3);
            while cache.peek(&key).is_none() {
                assert!(Instant::now() < deadline, "tile {x} did not arrive");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(cache.bytes(), 50);
        cache.trim();
        assert_eq!((cache.held(), cache.bytes()), (2, 20));
        assert!(cache.peek(&("sat".into(), 3, 4, 0)).is_some(), "the newest stays");
        assert!(cache.peek(&("sat".into(), 3, 0, 0)).is_none(), "the oldest goes");
        let before = cache.generation();
        cache.clear();
        assert_eq!((cache.held(), cache.bytes()), (0, 0));
        assert!(cache.generation() > before, "a clear is a change too");
    }
}
