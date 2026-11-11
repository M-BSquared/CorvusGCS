//! The terminal registry (`SshBridge` and the output side of `SshSession` in
//! `ssh_bridge.py`): named sessions with a replay buffer and subscribers, of
//! which the shell on this computer is one and an SSH connection will be
//! another.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How much recent output a late subscriber is given first (characters).
pub const REPLAY_LIMIT: usize = 256 * 1024;
/// How many chunks a slow stream may hold (`SSH_SSE_CAPACITY`).
pub const STREAM_CAPACITY: usize = 256;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub type Subscriber = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct Fan {
    subs: Vec<(u64, Subscriber)>,
    replay: VecDeque<String>,
    replay_len: usize,
}

/// The replay buffer and the subscribers, under one lock held across a whole
/// publish: that is what lets a late subscriber see every chunk exactly once.
#[derive(Default)]
pub struct Output {
    fan: Mutex<Fan>,
    next: AtomicU64,
}

impl Output {
    pub fn publish(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut fan = lock(&self.fan);
        fan.replay.push_back(text.to_string());
        fan.replay_len += text.chars().count();
        while fan.replay_len > REPLAY_LIMIT && fan.replay.len() > 1 {
            if let Some(old) = fan.replay.pop_front() {
                fan.replay_len -= old.chars().count();
            }
        }
        for (_, sub) in &fan.subs {
            let sub = Arc::clone(sub);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sub(text)));
        }
    }

    /// Registers a subscriber, replaying recent output to it first.
    pub fn add_sub(&self, f: Subscriber, replay: bool) -> u64 {
        let mut fan = lock(&self.fan);
        if replay && !fan.replay.is_empty() {
            let buffered: String = fan.replay.iter().map(String::as_str).collect();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&buffered)));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        fan.subs.push((id, f));
        id
    }

    pub fn remove_sub(&self, id: u64) {
        lock(&self.fan).subs.retain(|(i, _)| *i != id);
    }
}

/// A terminal session: the shell on this computer, or an SSH connection.
pub trait Terminal: Send + Sync {
    fn name(&self) -> String;
    fn host(&self) -> String;
    fn port(&self) -> i64;
    fn username(&self) -> String;
    fn connected(&self) -> bool;
    /// Starts it; false says why in `error`.
    fn connect(&self) -> bool;
    fn error(&self) -> String;
    fn auth_failed(&self) -> bool {
        false
    }
    fn send(&self, data: &str);
    fn resize(&self, cols: i64, rows: i64) -> bool;
    fn disconnect(&self);
    fn output(&self) -> &Output;
}

/// A bounded queue fed by a session's output, read by one stream.
pub struct Feed {
    items: Mutex<VecDeque<String>>,
    wake: Condvar,
}

impl Feed {
    pub fn new() -> Arc<Feed> {
        Arc::new(Feed {
            items: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
        })
    }

    /// Appends, dropping the oldest when full.
    pub fn put(&self, text: &str) {
        let mut items = lock(&self.items);
        if items.len() >= STREAM_CAPACITY {
            items.pop_front();
        }
        items.push_back(text.to_string());
        self.wake.notify_all();
    }

    /// The next chunk, waiting up to `timeout`.
    pub fn get(&self, timeout: Duration) -> Option<String> {
        let end = Instant::now() + timeout;
        let mut items = lock(&self.items);
        loop {
            if let Some(t) = items.pop_front() {
                return Some(t);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            items = self.wake.wait_timeout(items, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    pub fn drain(&self) -> Vec<String> {
        lock(&self.items).drain(..).collect()
    }
}

/// Subscribes a [`Feed`] to a terminal's output, with the replay first.
pub fn subscribe(terminal: &Arc<dyn Terminal>) -> (Arc<Feed>, u64) {
    let feed = Feed::new();
    let f = Arc::clone(&feed);
    let id = terminal.output().add_sub(Arc::new(move |t: &str| f.put(t)), true);
    (feed, id)
}

#[derive(Default)]
struct Registry {
    sessions: indexmap::IndexMap<String, Arc<dyn Terminal>>,
    attempts: HashMap<String, u64>,
    errors: HashMap<String, String>,
    auth_failed: HashSet<String>,
    shutting_down: bool,
    /// Sessions still connecting: not in `sessions` yet, but `shutdown` ends them.
    pending: Vec<Arc<dyn Terminal>>,
}

/// Manages terminal sessions keyed by name.
#[derive(Default)]
pub struct Terminals {
    inner: Mutex<Registry>,
    closed: AtomicBool,
}

impl Terminals {
    pub fn new() -> Terminals {
        Terminals::default()
    }

    /// Replaces whatever runs under `name` with the session `make` builds. The
    /// blocking connect runs outside the lock so the API stays responsive.
    pub fn open(&self, name: &str, make: &dyn Fn() -> Arc<dyn Terminal>) -> bool {
        let (attempt, old) = {
            let mut r = lock(&self.inner);
            if r.shutting_down {
                return false;
            }
            let attempt = r.attempts.get(name).copied().unwrap_or(0) + 1;
            r.attempts.insert(name.to_string(), attempt);
            r.errors.remove(name);
            r.auth_failed.remove(name);
            (attempt, r.sessions.get(name).cloned())
        };
        if let Some(old) = old {
            old.disconnect();
        }
        let session = make();
        {
            let mut r = lock(&self.inner);
            if r.shutting_down {
                return false;
            }
            r.pending.push(Arc::clone(&session));
        }
        let ok = session.connect();
        lock(&self.inner).pending.retain(|p| !Arc::ptr_eq(p, &session));
        if !ok {
            let mut r = lock(&self.inner);
            if r.attempts.get(name) == Some(&attempt) {
                r.errors.insert(name.to_string(), session.error());
                if session.auth_failed() {
                    r.auth_failed.insert(name.to_string());
                }
            }
            return false;
        }
        let (stale, sneaked) = {
            let mut r = lock(&self.inner);
            if r.shutting_down || r.attempts.get(name) != Some(&attempt) {
                (true, None)
            } else {
                let sneaked = r.sessions.get(name).cloned();
                r.sessions.insert(name.to_string(), Arc::clone(&session));
                (false, sneaked)
            }
        };
        if stale {
            session.disconnect();
            return false;
        }
        if let Some(s) = sneaked {
            if !Arc::ptr_eq(&s, &session) {
                s.disconnect();
            }
        }
        true
    }

    /// Why the latest connect under `name` failed, or "".
    pub fn connect_error(&self, name: &str) -> String {
        lock(&self.inner).errors.get(name).cloned().unwrap_or_default()
    }

    pub fn connect_auth_failed(&self, name: &str) -> bool {
        lock(&self.inner).auth_failed.contains(name)
    }

    pub fn disconnect(&self, name: &str) -> bool {
        let session = {
            let mut r = lock(&self.inner);
            *r.attempts.entry(name.to_string()).or_insert(0) += 1;
            r.errors.remove(name);
            r.auth_failed.remove(name);
            r.sessions.shift_remove(name)
        };
        match session {
            Some(s) => {
                s.disconnect();
                true
            }
            None => false,
        }
    }

    pub fn send(&self, name: &str, data: &str) -> bool {
        let session = lock(&self.inner).sessions.get(name).cloned();
        match session {
            Some(s) if s.connected() => {
                s.send(data);
                true
            }
            _ => false,
        }
    }

    pub fn resize(&self, name: &str, cols: i64, rows: i64) -> bool {
        let session = lock(&self.inner).sessions.get(name).cloned();
        session.is_some_and(|s| s.resize(cols, rows))
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Terminal>> {
        lock(&self.inner).sessions.get(name).cloned()
    }

    /// Connection info for all sessions.
    pub fn list(&self) -> Vec<Value> {
        lock(&self.inner)
            .sessions
            .values()
            .map(|s| {
                json!({
                    "name": s.name(),
                    "host": s.host(),
                    "port": s.port(),
                    "username": s.username(),
                    "connected": s.connected(),
                })
            })
            .collect()
    }

    /// The names of the sessions, in the order the list has them.
    pub fn connected_names(&self) -> Vec<String> {
        lock(&self.inner)
            .sessions
            .values()
            .filter(|s| s.connected())
            .map(|s| s.name())
            .collect()
    }

    /// Disconnects every session and refuses new ones.
    pub fn shutdown(&self) {
        let sessions: Vec<Arc<dyn Terminal>> = {
            let mut r = lock(&self.inner);
            r.shutting_down = true;
            let names: Vec<String> = r.attempts.keys().cloned().collect();
            for n in names {
                *r.attempts.entry(n).or_insert(0) += 1;
            }
            r.errors.clear();
            r.auth_failed.clear();
            let mut all: Vec<Arc<dyn Terminal>> = r.sessions.drain(..).map(|(_, s)| s).collect();
            all.extend(r.pending.iter().cloned());
            all
        };
        self.closed.store(true, Ordering::SeqCst);
        for s in sessions {
            s.disconnect();
        }
    }
}
