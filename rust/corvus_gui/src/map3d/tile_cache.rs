//! Map tiles fetched and decoded on worker threads, kept in a bounded cache.
//!
//! `get` is the whole interface for a frame: it answers from the cache and
//! otherwise queues the tile, newest first, and the frame that finds it
//! there picks it up. Requests nobody repeats age out of the queue.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::net::http::{self, Server};

/// A tile: source id, zoom, column and row.
pub type Key = (String, u8, u32, u32);

const WORKERS: usize = 6;
const QUEUE_MAX: usize = 200;
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
}

type PathOf = Arc<dyn Fn(&Key) -> String + Send + Sync>;

struct Shared<T> {
    path: PathOf,
    retry: Duration,
    inner: Mutex<Inner<T>>,
    wake: Condvar,
    stop: AtomicBool,
}

/// A cache of decoded tiles of type `T`.
pub struct TileCache<T: Send + Sync + 'static> {
    shared: Arc<Shared<T>>,
    capacity: usize,
    threads: Vec<JoinHandle<()>>,
}

impl<T: Send + Sync + 'static> TileCache<T> {
    /// Fetches through `server` and decodes with `decode`; repaints `ctx`
    /// whenever a tile lands.
    pub fn new(
        ctx: &egui::Context,
        server: &Server,
        capacity: usize,
        decode: impl Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
    ) -> TileCache<T> {
        TileCache::with_path(
            ctx,
            server,
            capacity,
            |k| format!("/api/tiles/{}/{}/{}/{}.png", k.0, k.1, k.2, k.3),
            RETRY_AFTER,
            decode,
        )
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
        let shared = Arc::new(Shared {
            path: Arc::new(path),
            retry,
            inner: Mutex::new(Inner {
                slots: HashMap::new(),
                order: VecDeque::new(),
                queue: VecDeque::new(),
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let decode = Arc::new(decode);
        let threads = (0..WORKERS)
            .map(|i| {
                let (sh, srv, ctx, dec) = (shared.clone(), server.clone(), ctx.clone(), decode.clone());
                std::thread::Builder::new()
                    .name(format!("tiles-{i}"))
                    .spawn(move || worker(sh, srv, ctx, dec))
                    .expect("a tile worker starts")
            })
            .collect();
        TileCache {
            shared,
            capacity,
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

    /// The tile when it is here, never queueing it.
    pub fn peek(&self, key: &Key) -> Option<Arc<T>> {
        match self.shared.inner.lock().ok()?.slots.get(key) {
            Some(Slot::Ready(t)) => Some(t.clone()),
            _ => None,
        }
    }

    /// Drops every tile, for a changed source.
    pub fn clear(&self) {
        if let Ok(mut g) = self.shared.inner.lock() {
            g.slots.clear();
            g.order.clear();
            g.queue.clear();
        }
    }

    /// Evicts the oldest tiles past the capacity.
    pub fn trim(&self) {
        if let Ok(mut g) = self.shared.inner.lock() {
            while g.order.len() > self.capacity {
                if let Some(old) = g.order.pop_front() {
                    g.slots.remove(&old);
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
                        g.slots.insert(key.clone(), Slot::Ready(Arc::new(t)));
                        g.order.push_back(key);
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
        let cache: TileCache<usize> = TileCache::new(&ctx, &server, 10, |b| Some(b.len()));
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
}
