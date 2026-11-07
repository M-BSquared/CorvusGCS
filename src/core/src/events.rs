//! The event streams: `GET /api/telemetry` (the state, 50 Hz source, latest
//! wins) and `GET /api/events?topics=...` (one connection for the console,
//! parameters, firmware, tiles, shell, fleet and mcp topics).
//!
//! A topic belongs to the area that produces its events. It registers one
//! (`Registry::topic`) with the buffer size, a binder that attaches to the
//! producer and says what to send first, and a shaper for each event. The
//! connection thread owns the buffers; only the socket is shared by the
//! topics, so each keeps its own drop policy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::buffer::{Mux, Sink};
use crate::core::Core;
use crate::response::{SseEvent, SseNext, SseStream};
use crate::store::{ListenerId, Snapshot, VehicleStateStore};

/// What a binder returns: how to detach, and the event sent first (or none).
pub struct Binding {
    pub unbind: Box<dyn FnOnce() + Send>,
    pub initial: Option<Value>,
}

impl Binding {
    /// A topic with nothing to attach to (its service is absent): it never fires.
    pub fn none() -> Binding {
        Binding {
            unbind: Box::new(|| {}),
            initial: None,
        }
    }
}

pub type Binder = Arc<dyn Fn(&Core, Sink<Value>, Option<&str>) -> Binding + Send + Sync>;
pub type Shaper = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// One event topic.
pub struct Topic {
    pub name: &'static str,
    /// Items buffered for a slow reader (`CONSOLE_SSE_CAPACITY` and its kin).
    pub capacity: usize,
    /// Attaches `sink` to the producer; the third argument is `?job=`.
    pub bind: Binder,
    /// Turns a buffered item into the JSON sent (the identity for most).
    pub shape: Shaper,
}

impl Topic {
    pub fn new<B>(name: &'static str, capacity: usize, bind: B) -> Topic
    where
        B: Fn(&Core, Sink<Value>, Option<&str>) -> Binding + Send + Sync + 'static,
    {
        Topic {
            name,
            capacity,
            bind: Arc::new(bind),
            shape: Arc::new(|v| v),
        }
    }

    pub fn shaped<S>(mut self, shape: S) -> Topic
    where
        S: Fn(Value) -> Value + Send + Sync + 'static,
    {
        self.shape = Arc::new(shape);
        self
    }
}

const TERMINAL: [&str; 3] = ["done", "cancelled", "failed"];

struct TopicStream {
    mux: Mux<Value>,
    shapers: Vec<(String, String, Shaper)>,
    initial: Vec<SseEvent>,
    unbinds: Vec<Box<dyn FnOnce() + Send>>,
    close_on: Vec<String>,
    finished: bool,
}

impl SseStream for TopicStream {
    fn open(&mut self) -> Vec<SseEvent> {
        std::mem::take(&mut self.initial)
    }

    fn next(&mut self, wait: Duration) -> SseNext {
        if self.finished {
            return SseNext::End;
        }
        let Some(items) = self.mux.drain(wait) else {
            return SseNext::Idle;
        };
        let mut events = Vec::with_capacity(items.len());
        for (topic, item) in items {
            let Some((_, event, shape)) = self.shapers.iter().find(|(t, _, _)| *t == topic) else {
                continue;
            };
            if self.close_on.contains(&topic) {
                if let Some(state) = item.get("state").and_then(Value::as_str) {
                    self.finished = TERMINAL.contains(&state);
                } else {
                    self.finished = false;
                }
            }
            events.push(SseEvent::json(event, &shape(item)));
        }
        SseNext::Events(events)
    }

    fn close(&mut self) {
        for unbind in self.unbinds.drain(..) {
            unbind();
        }
    }
}

impl Core {
    /// A stream of `wanted` (`(topic, event name)`), for `/api/events` and the
    /// single-topic forms (`/api/params/progress` names its topic's events
    /// `progress`). `close_on` names topics whose terminal state ends the
    /// whole stream: right for a connection that follows one job, wrong for a
    /// multiplexed one. A name that is not a topic is skipped.
    pub fn topic_stream(&self, wanted: &[(&str, &str)], job: Option<&str>, close_on: &[&str]) -> Box<dyn SseStream> {
        let mux = Mux::new();
        let mut shapers = Vec::new();
        let mut unbinds = Vec::new();
        let mut initial = Vec::new();
        for (name, event) in wanted {
            let Some(topic) = self.topic(name) else { continue };
            if shapers.iter().any(|(t, _, _): &(String, String, Shaper)| t == name) {
                continue;
            }
            let sink = mux.topic(name, topic.capacity);
            let binding = (topic.bind)(self, sink, job);
            unbinds.push(binding.unbind);
            if let Some(first) = binding.initial {
                initial.push(SseEvent::json(event, &first));
            }
            shapers.push(((*name).to_string(), (*event).to_string(), topic.shape.clone()));
        }
        Box::new(TopicStream {
            mux,
            shapers,
            initial,
            unbinds,
            close_on: close_on.iter().map(|s| s.to_string()).collect(),
            finished: false,
        })
    }
}

/// The telemetry stream: the state under the event name `state`, the latest
/// snapshot only, each serialised once per version for every tab.
pub(crate) struct TelemetryStream {
    store: Arc<VehicleStateStore>,
    mux: Mux<Snapshot>,
    listener: Option<ListenerId>,
}

impl TelemetryStream {
    pub(crate) fn attach(store: Arc<VehicleStateStore>) -> TelemetryStream {
        let mux = Mux::new();
        let sink = mux.topic("state", 1);
        let listener = store.add_listener(Arc::new(move |s: &Snapshot| sink.put_latest(s.clone())));
        TelemetryStream {
            store,
            mux,
            listener: Some(listener),
        }
    }
}

impl SseStream for TelemetryStream {
    fn open(&mut self) -> Vec<SseEvent> {
        let snap = self.store.snapshot();
        vec![SseEvent::new("state", self.store.serialize_snapshot(&snap).to_vec())]
    }

    fn next(&mut self, wait: Duration) -> SseNext {
        match self.mux.drain(wait) {
            None => SseNext::Idle,
            Some(items) => SseNext::Events(
                items
                    .into_iter()
                    .map(|(_, s)| SseEvent::new("state", self.store.serialize_snapshot(&s).to_vec()))
                    .collect(),
            ),
        }
    }

    fn close(&mut self) {
        if let Some(id) = self.listener.take() {
            self.store.remove_listener(id);
        }
    }
}

/// Reads an opened stream the way a connection thread does, for tests and
/// for in-process readers: events until `want` came or `until` passed.
pub fn collect(stream: &mut dyn SseStream, until: Duration, want: usize) -> Vec<SseEvent> {
    let end = Instant::now() + until;
    let mut out = Vec::new();
    while out.len() < want {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match stream.next(left) {
            SseNext::Events(e) => out.extend(e),
            SseNext::Idle => {}
            SseNext::End => break,
        }
    }
    out
}
