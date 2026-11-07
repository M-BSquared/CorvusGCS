//! Bounded buffers between the threads that produce events and the thread
//! that writes a stream (`sse_buffers.py`).
//!
//! Several named topics share one lock and one wake-up, so one reader thread
//! serves them all, and each keeps its own drop policy: the latest wins
//! (`put_latest`), the oldest goes first (`put_fifo`), or the console's rule
//! (`put_console`: repeated lines collapse, a full buffer gives up quiet lines
//! before urgent ones, and order is never changed).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Topic<T> {
    name: String,
    capacity: usize,
    items: VecDeque<T>,
}

struct Shared<T> {
    topics: Mutex<Vec<Topic<T>>>,
    wake: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The reader side: several topic buffers behind one lock.
pub struct Mux<T> {
    shared: Arc<Shared<T>>,
}

/// The writer side of one topic. Cheap to clone.
pub struct Sink<T> {
    shared: Arc<Shared<T>>,
    index: usize,
}

impl<T> Clone for Sink<T> {
    fn clone(&self) -> Sink<T> {
        Sink {
            shared: self.shared.clone(),
            index: self.index,
        }
    }
}

impl<T> Default for Mux<T> {
    fn default() -> Self {
        Mux::new()
    }
}

impl<T> Mux<T> {
    pub fn new() -> Mux<T> {
        Mux {
            shared: Arc::new(Shared {
                topics: Mutex::new(Vec::new()),
                wake: Condvar::new(),
            }),
        }
    }

    /// Adds a topic of at least one item and returns what writes into it.
    pub fn topic(&self, name: &str, capacity: usize) -> Sink<T> {
        let mut topics = lock(&self.shared.topics);
        topics.push(Topic {
            name: name.to_string(),
            capacity: capacity.max(1),
            items: VecDeque::new(),
        });
        Sink {
            shared: self.shared.clone(),
            index: topics.len() - 1,
        }
    }

    /// Everything buffered, `(topic, item)` in topic order, without waiting.
    pub fn drain_now(&self) -> Vec<(String, T)> {
        let mut topics = lock(&self.shared.topics);
        Self::take_all(&mut topics)
    }

    fn take_all(topics: &mut [Topic<T>]) -> Vec<(String, T)> {
        let mut out = Vec::new();
        for t in topics.iter_mut() {
            let name = &t.name;
            out.extend(t.items.drain(..).map(|item| (name.clone(), item)));
        }
        out
    }

    /// Waits up to `timeout` for something, then returns all of it. `None`
    /// when nothing arrived: the cue for a keep-alive and a look at whether
    /// the core is stopping.
    pub fn drain(&self, timeout: Duration) -> Option<Vec<(String, T)>> {
        let deadline = Instant::now() + timeout;
        let mut topics = lock(&self.shared.topics);
        loop {
            let out = Self::take_all(&mut topics);
            if !out.is_empty() {
                return Some(out);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            topics = self
                .shared
                .wake
                .wait_timeout(topics, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    pub fn len(&self) -> usize {
        lock(&self.shared.topics).iter().map(|t| t.items.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Sink<T> {
    /// Replaces whatever is buffered: only the newest matters.
    pub fn put_latest(&self, item: T) {
        let mut topics = lock(&self.shared.topics);
        let t = &mut topics[self.index];
        t.items.clear();
        t.items.push_back(item);
        self.shared.wake.notify_all();
    }

    /// Appends, dropping the oldest when full.
    pub fn put_fifo(&self, item: T) {
        let mut topics = lock(&self.shared.topics);
        let t = &mut topics[self.index];
        if t.items.len() >= t.capacity {
            t.items.pop_front();
        }
        t.items.push_back(item);
        self.shared.wake.notify_all();
    }
}

impl Sink<Value> {
    /// A console line: the same line again moves to the end instead of
    /// repeating (except shell output), and a full buffer drops its newest
    /// quiet line, or the new line when only urgent ones are held, or its
    /// newest line for an urgent one.
    pub fn put_console(&self, entry: Value) {
        let urgent = is_urgent(&entry);
        let key = console_key(&entry);
        let shell = entry.get("name").and_then(Value::as_str) == Some("SHELL");
        let mut topics = lock(&self.shared.topics);
        let t = &mut topics[self.index];
        if !shell {
            t.items.retain(|item| console_key(item) != key);
        }
        if t.items.len() >= t.capacity {
            match t.items.iter().rposition(|item| !is_urgent(item)) {
                None => {
                    if !urgent {
                        return;
                    }
                    t.items.pop_back();
                }
                Some(i) => {
                    t.items.remove(i);
                }
            }
        }
        t.items.push_back(entry);
        self.shared.wake.notify_all();
    }
}

fn is_urgent(entry: &Value) -> bool {
    matches!(
        entry.get("level").and_then(Value::as_str),
        Some("error" | "critical" | "warning")
    )
}

fn console_key(entry: &Value) -> (Option<&Value>, Option<&Value>, Option<&Value>) {
    (entry.get("name"), entry.get("text"), entry.get("level"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cases() -> Value {
        serde_json::from_str(include_str!("../tests/data/sse_buffer_cases.json")).unwrap()
    }

    #[test]
    fn single_topic_buffers_match_the_python_ones() {
        for case in cases()["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mux = Mux::new();
            let sink = mux.topic("t", case["capacity"].as_u64().unwrap() as usize);
            for item in case["items"].as_array().unwrap() {
                match case["op"].as_str().unwrap() {
                    "put_latest" => sink.put_latest(item.clone()),
                    "put_fifo" => sink.put_fifo(item.clone()),
                    "put_console" => sink.put_console(item.clone()),
                    op => panic!("{op}"),
                }
            }
            assert_eq!(mux.len() as u64, case["qsize"].as_u64().unwrap(), "{name}");
            let mut all: Vec<Value> = mux.drain_now().into_iter().map(|(_, v)| v).collect();
            let first = all.remove(0);
            assert_eq!(first, case["first"], "{name}");
            assert_eq!(Value::Array(all), case["rest"], "{name}");
            assert!(case["then_empty"].as_bool().unwrap());
            assert!(mux.is_empty());
        }
    }

    #[test]
    fn topics_keep_their_own_policy_behind_one_lock() {
        let mux = Mux::new();
        let spec = cases()["mux"].clone();
        let sinks: Vec<Sink<Value>> = spec["topics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| mux.topic(t[0].as_str().unwrap(), t[1].as_u64().unwrap() as usize))
            .collect();
        sinks[0].put_latest(json!({"n": 1}));
        sinks[0].put_latest(json!({"n": 2}));
        sinks[1].put_console(json!({"name": "A", "text": "x", "level": "info"}));
        sinks[1].put_console(json!({"name": "A", "text": "y", "level": "error"}));
        sinks[2].put_latest(json!({"f": 1}));
        sinks[2].put_latest(json!({"f": 2}));
        for i in 0..4 {
            sinks[3].put_fifo(json!({"p": i}));
        }
        let drained: Vec<Value> = mux
            .drain(Duration::ZERO)
            .unwrap()
            .into_iter()
            .map(|(t, v)| json!([t, v]))
            .collect();
        assert_eq!(Value::Array(drained), spec["drained"]);
        assert_eq!(mux.drain(Duration::ZERO), None);
        assert!(spec["then_empty"].as_bool().unwrap());
    }

    #[test]
    fn drain_wakes_when_a_writer_puts() {
        let mux: Mux<i32> = Mux::new();
        let sink = mux.topic("t", 4);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            sink.put_fifo(7);
        });
        let got = mux.drain(Duration::from_secs(5)).unwrap();
        assert_eq!(got, vec![("t".to_string(), 7)]);
        t.join().unwrap();
        let began = Instant::now();
        assert_eq!(mux.drain(Duration::from_millis(40)), None);
        assert!(began.elapsed() >= Duration::from_millis(35));
    }
}
