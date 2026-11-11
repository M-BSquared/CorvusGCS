//! Live per message statistics for Live Messages on the Analysis page
//! (`mavlink_inspector.py`).
//!
//! The bridge hands every decoded frame to [`MessageScope::record`]. A counter,
//! two timestamps and the newest message are kept per (system, component,
//! message) key, and nothing is counted at all unless the page is polling: every
//! snapshot keeps the counter awake for a few seconds, and it goes back to sleep
//! (and forgets its numbers) when the page closes or pauses.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Value};

use crate::clock;

const MAX_KEYS: usize = 512;
const RATE_WINDOW_S: f64 = 1.0;
const SILENT_AFTER_S: f64 = 2.5;
const ACTIVE_FOR_S: f64 = 3.0;
const MAX_STRING: usize = 256;
const MAX_LIST: usize = 64;

struct Entry {
    msg_id: u32,
    count: u64,
    last: f64,
    rate: f64,
    win_start: f64,
    win_count: u64,
    size: usize,
    msg: Message,
}

struct Inner {
    entries: HashMap<(i64, i64, String), Entry>,
    since: f64,
    dropped: u64,
    active_until: f64,
}

/// A thread safe table of every message type seen on the link.
pub struct MessageScope {
    inner: Mutex<Inner>,
}

impl Default for MessageScope {
    fn default() -> Self {
        MessageScope::new()
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn json_safe(value: &Wv, depth: u32) -> Value {
    match value {
        Wv::Int(i) => i64::try_from(*i).map_or_else(|_| json!(i.to_string()), |v| json!(v)),
        Wv::Float(f) => {
            if f.is_finite() {
                json!(f)
            } else {
                Value::Null
            }
        }
        Wv::Bytes(_) => {
            let text = value.as_text().unwrap_or_default();
            json!(text.chars().take(MAX_STRING).collect::<String>())
        }
        Wv::Array(items) if depth < 2 => {
            Value::Array(items.iter().take(MAX_LIST).map(|v| json_safe(v, depth + 1)).collect())
        }
        Wv::Array(items) => json!(format!("{items:?}").chars().take(MAX_STRING).collect::<String>()),
    }
}

impl MessageScope {
    pub fn new() -> MessageScope {
        MessageScope {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                since: unix_now(),
                dropped: 0,
                active_until: 0.0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Counts one decoded message; returns at once while nobody is watching.
    pub fn record(&self, msg: &Message, system: i64, component: i64, size: usize) {
        let t = clock::monotonic();
        {
            if t > self.lock().active_until {
                return;
            }
        }
        let name = msg.name();
        if name == "BAD_DATA" {
            return;
        }
        let mut inner = self.lock();
        let key = (system, component, name.to_string());
        if !inner.entries.contains_key(&key) {
            if inner.entries.len() >= MAX_KEYS {
                inner.dropped += 1;
                return;
            }
            inner.entries.insert(
                key.clone(),
                Entry {
                    msg_id: msg.id(),
                    count: 0,
                    last: t,
                    rate: 0.0,
                    win_start: t,
                    win_count: 0,
                    size: 0,
                    msg: msg.clone(),
                },
            );
        }
        let Some(entry) = inner.entries.get_mut(&key) else {
            return;
        };
        entry.count += 1;
        entry.win_count += 1;
        entry.last = t;
        entry.msg = msg.clone();
        let elapsed = t - entry.win_start;
        if elapsed >= RATE_WINDOW_S {
            entry.rate = entry.win_count as f64 / elapsed;
            entry.win_start = t;
            entry.win_count = 0;
        }
        if size > 0 {
            entry.size = size;
        }
    }

    /// Forgets everything, e.g. when the operator presses Clear.
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.dropped = 0;
        inner.since = unix_now();
    }

    /// Rows for every message type, plus the fields of the `select` row. Asking
    /// also wakes the counter; the first ask after a quiet spell counts afresh.
    pub fn snapshot(&self, select: &str) -> Value {
        let t = clock::monotonic();
        let mut inner = self.lock();
        if t > inner.active_until {
            inner.entries.clear();
            inner.dropped = 0;
            inner.since = unix_now();
        }
        inner.active_until = t + ACTIVE_FOR_S;
        let mut rows: Vec<(i64, i64, String, Value)> = Vec::new();
        let mut chosen: Option<(&Message, String)> = None;
        for ((system, component, name), e) in &inner.entries {
            let age = (t - e.last).max(0.0);
            let silent = age > SILENT_AFTER_S;
            let key = format!("{system}:{component}:{name}");
            rows.push((
                *system,
                *component,
                name.clone(),
                json!({
                    "key": key, "system": system, "component": component, "name": name,
                    "id": e.msg_id, "count": e.count,
                    "rate": if silent { 0.0 } else { round2(e.rate) },
                    "age": round2(age), "size": e.size,
                }),
            ));
            if !select.is_empty() && key == select {
                chosen = Some((&e.msg, key));
            }
        }
        rows.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
        let total: u64 = inner.entries.values().map(|e| e.count).sum();
        let (fields, selected) = match chosen {
            Some((msg, key)) => (Some(fields_of(msg)), key),
            None => (None, String::new()),
        };
        json!({
            "rows": rows.into_iter().map(|r| r.3).collect::<Vec<_>>(),
            "total": total, "dropped": inner.dropped, "since": inner.since,
            "selected": selected, "fields": fields,
        })
    }
}

/// Name, value and unit of every field of one message, in declared order.
fn fields_of(msg: &Message) -> Vec<Value> {
    msg.def()
        .fields(msg.wire())
        .iter()
        .zip(msg.values())
        .map(|(def, value)| json!({"name": def.name, "value": json_safe(value, 0), "unit": def.units}))
        .collect()
}
