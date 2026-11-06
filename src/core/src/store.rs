//! The thread-safe Vehicle State Store (`state_store.py`).
//!
//! The MAVLink side writes telemetry here and the interface and the HTTP
//! layer read it. Fields, defaults, the order of the keys and every rule
//! below are the Python store's:
//!
//! * a listener is called with the latest snapshot at most every
//!   `NOTIFY_MIN_INTERVAL` (about 30 Hz), except when the update touches a key
//!   of `IMMEDIATE_KEYS` (arming, mode, link transitions), which fires on the
//!   call; listeners are never called with the store's lock held;
//! * `snapshot()` is the live state whatever the coalescing, cached per
//!   `version()`: reads with no mutation between return the same `Arc`;
//! * a listener that panics costs only that update for itself, and the
//!   failure is reported at most every 5 s with the count of those hidden.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use crate::json::Map;

pub const NOTIFY_MIN_INTERVAL: f64 = 1.0 / 30.0;

/// State changes that reach listeners at once, skipping the coalescing.
pub const IMMEDIATE_KEYS: &[&str] = &[
    "armed",
    "mode",
    "link_status",
    "link_quality",
    "connected",
    "landed_state",
    "autotune_state",
    "link_auto",
    "link_suggestion",
    "link_bootloader",
    "flight_termination",
    "kill_switch",
];

/// What `forget_vehicle` keeps: the link and the RTK base belong to this
/// station, not to the aircraft on the other end.
pub const STATION_KEYS: &[&str] = &[
    "link_status",
    "link_connection",
    "link_error",
    "link_quality",
    "link_auto",
    "link_suggestion",
    "link_bootloader",
    "rtk_state",
    "rtk_source",
    "rtk_accuracy",
    "rtk_progress",
];

const FAILURE_LOG_INTERVAL_S: f64 = 5.0;
const DEFAULTS: &str = include_str!("../tests/data/state_defaults.json");

/// A snapshot: the whole state, read-only, shared.
pub type Snapshot = Arc<Map>;

/// Called with each pushed snapshot.
pub type Listener = Arc<dyn Fn(&Snapshot) + Send + Sync>;

/// Identifies a listener for `remove_listener`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerId(u64);

/// The history series the store keeps, longest `history_len` values each.
const SERIES: [(&str, &str); 3] = [
    ("altitude", "altitude_amsl"),
    ("groundspeed", "groundspeed"),
    ("battery", "battery_percent"),
];

struct Inner {
    data: Map,
    history: [VecDeque<Value>; 3],
    listeners: Vec<(u64, Listener)>,
    next_listener: u64,
    last_heartbeat: Option<f64>,
    last_notify: Option<f64>,
    data_version: u64,
    snapshot: Option<(u64, Snapshot)>,
}

struct Serialized {
    version: u64,
    bytes: Arc<Vec<u8>>,
}

struct Failures {
    last_logged: Option<f64>,
    suppressed: u64,
}

pub struct VehicleStateStore {
    inner: Mutex<Inner>,
    defaults: Map,
    history_len: usize,
    clock: Arc<dyn Clock>,
    failures: Mutex<Failures>,
    serialized: Mutex<Option<Serialized>>,
}

type Pending = (Option<Snapshot>, Vec<Listener>);

impl VehicleStateStore {
    pub fn new() -> VehicleStateStore {
        Self::with_clock(600, Arc::new(SystemClock)).expect("600 is a valid history length")
    }

    /// A store keeping `history_len` values per series; zero is refused, as
    /// the Python store refuses it.
    pub fn with_history(history_len: usize) -> Result<VehicleStateStore, String> {
        Self::with_clock(history_len, Arc::new(SystemClock))
    }

    pub fn with_clock(history_len: usize, clock: Arc<dyn Clock>) -> Result<VehicleStateStore, String> {
        if history_len == 0 {
            return Err("history_len must be a positive integer".into());
        }
        let defaults: Map = serde_json::from_str(DEFAULTS).expect("state_defaults.json is a JSON object");
        Ok(VehicleStateStore {
            inner: Mutex::new(Inner {
                data: defaults.clone(),
                history: Default::default(),
                listeners: Vec::new(),
                next_listener: 0,
                last_heartbeat: None,
                last_notify: None,
                data_version: 0,
                snapshot: None,
            }),
            defaults,
            history_len,
            clock,
            failures: Mutex::new(Failures {
                last_logged: None,
                suppressed: 0,
            }),
            serialized: Mutex::new(None),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn snapshot_locked(inner: &mut Inner) -> Snapshot {
        match &inner.snapshot {
            Some((v, s)) if *v == inner.data_version => s.clone(),
            _ => {
                let s = Arc::new(inner.data.clone());
                inner.snapshot = Some((inner.data_version, s.clone()));
                s
            }
        }
    }

    /// Under the lock: decides whether listeners are called now and stages
    /// the snapshot and the list to call after the lock is released. With no
    /// listener the clock is not even read.
    fn dispatch_locked(&self, inner: &mut Inner, immediate: bool) -> Pending {
        if inner.listeners.is_empty() {
            return (None, Vec::new());
        }
        let now = self.clock.monotonic();
        let due = inner.last_notify.is_none_or(|last| now - last >= NOTIFY_MIN_INTERVAL);
        if immediate || due {
            inner.last_notify = Some(now);
            let snapshot = Self::snapshot_locked(inner);
            return (Some(snapshot), inner.listeners.iter().map(|(_, f)| f.clone()).collect());
        }
        (None, Vec::new())
    }

    fn notify(&self, (snapshot, listeners): Pending) {
        let Some(snapshot) = snapshot else { return };
        for f in listeners {
            if catch_unwind(AssertUnwindSafe(|| f(&snapshot))).is_err() {
                if let Some(message) = self.listener_failure() {
                    eprintln!("corvus: {message}");
                }
            }
        }
    }

    /// The line to report for a listener failure, or `None` while the last
    /// report is under 5 s old (the failure is then only counted).
    pub fn listener_failure(&self) -> Option<String> {
        let now = self.clock.monotonic();
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = f.last_logged {
            if now - last < FAILURE_LOG_INTERVAL_S {
                f.suppressed += 1;
                return None;
            }
        }
        let suppressed = std::mem::take(&mut f.suppressed);
        let elapsed = f.last_logged.map_or(0.0, |last| now - last);
        f.last_logged = Some(now);
        Some(if suppressed > 0 {
            format!(
                "state listener failed; dropping this update for it ({suppressed} further failures suppressed in the last {elapsed:.0}s)"
            )
        } else {
            "state listener failed; dropping this update for it".to_string()
        })
    }

    /// Sets fields and notifies. A key the store does not hold is ignored.
    /// History series follow `altitude_amsl`, `groundspeed` and
    /// `battery_percent` whenever they are in the call.
    pub fn update<I, K>(&self, fields: I)
    where
        I: IntoIterator<Item = (K, Value)>,
        K: AsRef<str>,
    {
        let fields: Vec<(K, Value)> = fields.into_iter().collect();
        let immediate = fields.iter().any(|(k, _)| IMMEDIATE_KEYS.contains(&k.as_ref()));
        let pending = {
            let mut inner = self.lock();
            let mut mutated = false;
            for (k, v) in &fields {
                if let Some(slot) = inner.data.get_mut(k.as_ref()) {
                    *slot = v.clone();
                    mutated = true;
                }
            }
            for (i, (_, key)) in SERIES.iter().enumerate() {
                if fields.iter().any(|(k, _)| k.as_ref() == *key) {
                    let value = inner.data[*key].clone();
                    let h = &mut inner.history[i];
                    h.push_back(value);
                    while h.len() > self.history_len {
                        h.pop_front();
                    }
                }
            }
            if mutated {
                inner.data_version += 1;
            }
            self.dispatch_locked(&mut inner, immediate)
        };
        self.notify(pending);
    }

    /// `update` with one field.
    pub fn set(&self, key: &str, value: Value) {
        self.update([(key, value)]);
    }

    /// Inserts or refreshes a warning. `event` tags what it reports (`kill`,
    /// `unkill`, `disarm`, `prearm`); a line that comes again is live again.
    /// `meta` is the time shown with it, the local clock when `None`.
    pub fn merge_warning(&self, text: &str, level: &str, meta: Option<&str>, max_warnings: usize, event: Option<&str>) {
        let severity = |l: &str| match l {
            "warning" => 1,
            "critical" => 2,
            _ => 0,
        };
        let timestamp = match meta {
            Some(m) if !m.is_empty() => m.to_string(),
            _ => self.clock.local_hms(),
        };
        let pending = {
            let mut inner = self.lock();
            let mut warnings = warnings_of(&inner.data);
            let existing = warnings
                .iter()
                .position(|w| w.get("msg").and_then(Value::as_str) == Some(text));
            match existing {
                None => {
                    let mut entry = Map::new();
                    entry.insert("level".into(), level.into());
                    entry.insert("msg".into(), text.into());
                    entry.insert("meta".into(), timestamp.into());
                    if let Some(e) = event.filter(|e| !e.is_empty()) {
                        entry.insert("event".into(), e.into());
                    }
                    warnings.push(entry);
                }
                Some(i) => {
                    let w = &mut warnings[i];
                    w.insert("meta".into(), timestamp.into());
                    w.shift_remove("resolved");
                    if let Some(e) = event.filter(|e| !e.is_empty()) {
                        w.insert("event".into(), e.into());
                    }
                    let old = w.get("level").and_then(Value::as_str).unwrap_or("info").to_string();
                    if severity(level) > severity(&old) {
                        w.insert("level".into(), level.into());
                    }
                }
            }
            if max_warnings > 0 && warnings.len() > max_warnings {
                warnings.drain(..warnings.len() - max_warnings);
            }
            inner.data.insert(
                "warnings".into(),
                Value::Array(warnings.into_iter().map(Value::Object).collect()),
            );
            inner.data_version += 1;
            self.dispatch_locked(&mut inner, false)
        };
        self.notify(pending);
    }

    /// Marks every warning tagged with one of `events` as resolved. Resolved
    /// is not removed: the line stays on the board for the interface to let go
    /// of. Returns whether anything changed; a call that changes nothing
    /// notifies nobody.
    pub fn resolve_warnings(&self, events: &[&str]) -> bool {
        let pending = {
            let mut inner = self.lock();
            let mut warnings = warnings_of(&inner.data);
            let tagged = |w: &Map| {
                w.get("event")
                    .and_then(Value::as_str)
                    .is_some_and(|e| events.contains(&e))
            };
            let live = |w: &Map| !w.get("resolved").is_some_and(truthy);
            if !warnings.iter().any(|w| tagged(w) && live(w)) {
                return false;
            }
            for w in &mut warnings {
                if tagged(w) {
                    w.insert("resolved".into(), true.into());
                }
            }
            inner.data.insert(
                "warnings".into(),
                Value::Array(warnings.into_iter().map(Value::Object).collect()),
            );
            inner.data_version += 1;
            self.dispatch_locked(&mut inner, false)
        };
        self.notify(pending);
        true
    }

    pub fn clear_warnings(&self) {
        let pending = {
            let mut inner = self.lock();
            inner.data.insert("warnings".into(), Value::Array(Vec::new()));
            inner.data_version += 1;
            self.dispatch_locked(&mut inner, false)
        };
        self.notify(pending);
    }

    /// Drops everything the previous aircraft said, before another is
    /// latched. Notifies at once, like a disconnect.
    pub fn forget_vehicle(&self) {
        let pending = {
            let mut inner = self.lock();
            for (key, value) in &self.defaults {
                if !STATION_KEYS.contains(&key.as_str()) {
                    inner.data.insert(key.clone(), value.clone());
                }
            }
            for h in &mut inner.history {
                h.clear();
            }
            inner.last_heartbeat = None;
            inner.data_version += 1;
            self.dispatch_locked(&mut inner, true)
        };
        self.notify(pending);
    }

    /// Records a heartbeat and returns the instant recorded, so the caller
    /// does not read the clock twice for one arrival. The first one after a
    /// disconnect notifies at once; the rest only refresh the staleness clock.
    pub fn heartbeat(&self) -> f64 {
        let (stamp, pending) = {
            let mut inner = self.lock();
            let was_connected = inner.data.get("connected").is_some_and(truthy);
            let stamp = self.clock.monotonic();
            inner.last_heartbeat = Some(stamp);
            inner.data.insert("connected".into(), true.into());
            let pending = if was_connected {
                (None, Vec::new())
            } else {
                inner.data_version += 1;
                self.dispatch_locked(&mut inner, true)
            };
            (stamp, pending)
        };
        self.notify(pending);
        stamp
    }

    /// Marks the vehicle disconnected and notifies at once. What belonged to
    /// the dead link goes with it; what the mission IS stays known.
    pub fn set_disconnected(&self) {
        let pending = {
            let mut inner = self.lock();
            let d = &mut inner.data;
            d.insert("connected".into(), false.into());
            d.insert("armed".into(), false.into());
            d.insert("mode".into(), "DISCONNECTED".into());
            d.insert("mode_label".into(), "DISCONNECTED".into());
            d.insert("prearm_ok".into(), Value::Null);
            d.insert("prearm_reasons".into(), Value::Array(Vec::new()));
            d.insert("landed_state".into(), 0.into());
            d.insert("flight_termination".into(), false.into());
            d.insert("kill_switch".into(), false.into());
            if d.get("autotune_state").and_then(Value::as_str) == Some("running") {
                d.insert("autotune_state".into(), "failed".into());
            }
            d.insert("setpoints_live".into(), false.into());
            d.insert("mission_state".into(), "".into());
            d.insert("rc_live".into(), false.into());
            d.insert("rangefinder_distance".into(), Value::Null);
            d.insert("link_status".into(), "disconnected".into());
            inner.data_version += 1;
            self.dispatch_locked(&mut inner, true)
        };
        self.notify(pending);
    }

    /// True when no heartbeat came within `timeout` seconds. Measured on the
    /// monotonic clock: a wall clock corrected by NTP or the GPS must not make
    /// a healthy link look stale, or a dead one look alive.
    pub fn is_stale(&self, timeout: f64) -> bool {
        let inner = self.lock();
        match inner.last_heartbeat {
            None => true,
            Some(last) => (self.clock.monotonic() - last) > timeout,
        }
    }

    /// The live state, read-only. Rebuilt only when `version()` moved.
    pub fn snapshot(&self) -> Snapshot {
        Self::snapshot_locked(&mut self.lock())
    }

    /// `snap` as JSON, serialised once per version however many tabs read it.
    /// A snapshot older than the store's current one (it waited in a buffer
    /// while the store moved on) is sent as it is but never cached for the
    /// newer version.
    pub fn serialize_snapshot(&self, snap: &Snapshot) -> Arc<Vec<u8>> {
        let version = self.version();
        let mut cache = self.serialized.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.as_ref() {
            if c.version == version {
                return c.bytes.clone();
            }
        }
        let bytes = Arc::new(serde_json::to_vec(&**snap).unwrap_or_else(|_| b"{}".to_vec()));
        if Arc::ptr_eq(snap, &self.snapshot()) {
            *cache = Some(Serialized {
                version,
                bytes: bytes.clone(),
            });
        }
        bytes
    }

    /// One field of the live state.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.lock().data.get(key).cloned()
    }

    /// `altitude`, `groundspeed` or `battery`, oldest first; empty for any
    /// other name.
    pub fn history(&self, key: &str) -> Vec<Value> {
        let inner = self.lock();
        SERIES
            .iter()
            .position(|(name, _)| *name == key)
            .map(|i| inner.history[i].iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The mutation counter: the same version is a byte-identical snapshot.
    pub fn version(&self) -> u64 {
        self.lock().data_version
    }

    pub fn add_listener(&self, f: Listener) -> ListenerId {
        let mut inner = self.lock();
        inner.next_listener += 1;
        let id = inner.next_listener;
        inner.listeners.push((id, f));
        ListenerId(id)
    }

    pub fn remove_listener(&self, id: ListenerId) {
        self.lock().listeners.retain(|(i, _)| *i != id.0);
    }

    /// Nothing to stop: the store owns no thread. Kept so the shutdown path
    /// calls every service the same way.
    pub fn shutdown(&self) {}

    /// The current state as JSON text.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&*self.snapshot()).unwrap_or_else(|_| "{}".into())
    }
}

impl Default for VehicleStateStore {
    fn default() -> Self {
        Self::new()
    }
}

fn warnings_of(data: &Map) -> Vec<Map> {
    data.get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|w| w.as_object().cloned()).collect())
        .unwrap_or_default()
}

/// Python truthiness for the flags the store reads back.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake(Mutex<f64>);

    impl Clock for Fake {
        fn monotonic(&self) -> f64 {
            *self.0.lock().unwrap()
        }
        fn local_hms(&self) -> String {
            "12:00:00".into()
        }
    }

    fn cases() -> Value {
        serde_json::from_str(include_str!("../tests/data/state_store_cases.json")).unwrap()
    }

    fn args(step: &Value) -> Vec<(String, Value)> {
        step["args"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[test]
    fn the_constants_are_the_pythons() {
        let c = cases();
        assert_eq!(c["notify_min_interval"].as_f64().unwrap(), NOTIFY_MIN_INTERVAL);
        let mut immediate: Vec<&str> = IMMEDIATE_KEYS.to_vec();
        immediate.sort();
        assert_eq!(json!(immediate), c["immediate_keys"]);
        let mut station: Vec<&str> = STATION_KEYS.to_vec();
        station.sort();
        assert_eq!(json!(station), c["station_keys"]);
    }

    #[test]
    fn scripted_runs_end_where_the_python_store_ends() {
        for case in cases()["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let clock = Arc::new(Fake(Mutex::new(0.0)));
            let store =
                VehicleStateStore::with_clock(case["history_len"].as_u64().unwrap() as usize, clock.clone()).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let last: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
            for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let before = calls.load(Ordering::SeqCst);
                let mut result = Value::Null;
                match step["op"].as_str().unwrap() {
                    "listen" => {
                        let (calls, last) = (calls.clone(), last.clone());
                        store.add_listener(Arc::new(move |s: &Snapshot| {
                            calls.fetch_add(1, Ordering::SeqCst);
                            *last.lock().unwrap() = Some(s.clone());
                        }));
                    }
                    "tick" => *clock.0.lock().unwrap() = step["to"].as_f64().unwrap(),
                    "update" => store.update(args(step)),
                    "merge_warning" => store.merge_warning(
                        step["text"].as_str().unwrap(),
                        step["level"].as_str().unwrap(),
                        step["meta"].as_str(),
                        step.get("max_warnings").and_then(Value::as_u64).unwrap_or(20) as usize,
                        step["event"].as_str(),
                    ),
                    "resolve_warnings" => {
                        let events: Vec<&str> = step["events"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(Value::as_str)
                            .collect();
                        result = json!(store.resolve_warnings(&events));
                    }
                    "clear_warnings" => store.clear_warnings(),
                    "forget_vehicle" => store.forget_vehicle(),
                    "heartbeat" => result = json!(store.heartbeat()),
                    "set_disconnected" => store.set_disconnected(),
                    "is_stale" => result = json!(store.is_stale(step["timeout"].as_f64().unwrap())),
                    op => panic!("{op}"),
                }
                let expect = &case["results"][i];
                let what = format!("{name} step {i} {}", step["op"]);
                assert_eq!(
                    calls.load(Ordering::SeqCst) - before,
                    expect["calls"].as_u64().unwrap() as usize,
                    "{what}: listener calls"
                );
                assert_eq!(store.version(), expect["version"].as_u64().unwrap(), "{what}: version");
                assert_eq!(result, expect["result"], "{what}: result");
            }
            assert_eq!(
                Value::Object((*store.snapshot()).clone()),
                case["snapshot"],
                "{name}: snapshot"
            );
            let last = last.lock().unwrap().clone();
            match last {
                Some(s) => assert_eq!(
                    Value::Object((*s).clone()),
                    case["last_notified"],
                    "{name}: last notified"
                ),
                None => assert!(case["last_notified"].is_null(), "{name}"),
            }
            for key in ["altitude", "groundspeed", "battery", "other"] {
                assert_eq!(
                    Value::Array(store.history(key)),
                    case["history"][key],
                    "{name}: history {key}"
                );
            }
            let parsed: Value = serde_json::from_str(&store.to_json()).unwrap();
            let expected: Value = serde_json::from_str(case["json"].as_str().unwrap()).unwrap();
            assert_eq!(parsed, expected, "{name}: json");
        }
    }

    #[test]
    fn a_fresh_store_holds_the_python_defaults_in_order() {
        let store = VehicleStateStore::new();
        let defaults: Map = serde_json::from_str(include_str!("../tests/data/state_defaults.json")).unwrap();
        assert_eq!(*store.snapshot(), defaults);
        let snap = store.snapshot();
        let keys: Vec<&String> = snap.keys().collect();
        let want: Vec<&String> = defaults.keys().collect();
        assert_eq!(keys, want);
        assert!(defaults.len() > 100);
    }

    #[test]
    fn reads_without_a_mutation_share_one_snapshot() {
        let store = VehicleStateStore::new();
        let a = store.snapshot();
        assert!(Arc::ptr_eq(&a, &store.snapshot()));
        store.set("heading", json!(1.0));
        let b = store.snapshot();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(a["heading"], json!(0.0));
        assert_eq!(b["heading"], json!(1.0));
        store.set("not_a_field", json!(1));
        assert!(Arc::ptr_eq(&b, &store.snapshot()));
    }

    #[test]
    fn a_zero_history_is_refused() {
        assert!(VehicleStateStore::with_history(0).is_err());
        assert!(VehicleStateStore::with_history(1).is_ok());
    }

    #[test]
    fn a_failing_listener_does_not_stop_the_others_and_is_reported_sparingly() {
        let clock = Arc::new(Fake(Mutex::new(100.0)));
        let store = VehicleStateStore::with_clock(10, clock.clone()).unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        store.add_listener(Arc::new(|_: &Snapshot| panic!("a wedged tab")));
        let s = seen.clone();
        store.add_listener(Arc::new(move |_: &Snapshot| {
            s.fetch_add(1, Ordering::SeqCst);
        }));
        store.set("armed", json!(true));
        store.set("armed", json!(false));
        assert_eq!(seen.load(Ordering::SeqCst), 2);

        *clock.0.lock().unwrap() = 200.0;
        assert_eq!(
            store.listener_failure().unwrap(),
            "state listener failed; dropping this update for it (1 further failures suppressed in the last 100s)"
        );
        *clock.0.lock().unwrap() = 201.0;
        assert_eq!(store.listener_failure(), None);
        assert_eq!(store.listener_failure(), None);
        *clock.0.lock().unwrap() = 210.0;
        assert_eq!(
            store.listener_failure().unwrap(),
            "state listener failed; dropping this update for it (2 further failures suppressed in the last 10s)"
        );
    }

    #[test]
    fn a_removed_listener_is_not_called() {
        let store = VehicleStateStore::new();
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        let id = store.add_listener(Arc::new(move |_: &Snapshot| {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        store.set("armed", json!(true));
        store.remove_listener(id);
        store.set("armed", json!(false));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn many_threads_can_write_and_read() {
        let store = Arc::new(VehicleStateStore::new());
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let s = store.clone();
                std::thread::spawn(move || {
                    for i in 0..500 {
                        s.update([("heading", json!(i as f64)), ("altitude_amsl", json!(t as f64))]);
                        let _ = s.snapshot();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(store.history("altitude").len(), 600);
        assert_eq!(store.version(), 2000);
    }
}
