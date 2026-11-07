//! The MAVLink parameter protocol: the cache, the download state machine,
//! PARAM_VALUE validation and decoding, the lossy link watchdog, named reads,
//! confirmed writes, the verify pass and the batch upload.
//!
//! The twin of `ParamEngine` in `corvus_link` (`protocol/params.rs`). The
//! caller of `request_param_list`, `request_param`, `set_param`,
//! `begin_upload` and `run_upload` holds the bridge's operation lock, as the
//! Python bridge does.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use corvus_link::plain::param_value::MAV_PARAM_TYPE_REAL32;
use corvus_link::plain::param_value::{decode_value, echo_confirms, encode_value, values_match};
pub use corvus_link::plain::param_value::{
    is_integer_type, PARAM_ABSENT_AFTER_MISSES, PARAM_CACHE_MAX_ENTRIES, PARAM_DOWNLOAD_INACTIVITY_S,
    PARAM_DOWNLOAD_MAX_ROUNDS, PARAM_DOWNLOAD_TIMEOUT_S, PARAM_DOWNLOAD_TIMEOUT_S_SERIAL, PARAM_ENCODING_BYTEWISE,
    PARAM_ENCODING_C_CAST, PARAM_HASH_CHECK, PARAM_ID_MAX_BYTES, PARAM_INDEX_NONE, PARAM_RETRANSMIT_GAP_S_SERIAL,
    PARAM_RETRANSMIT_GAP_S_UDP, PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL, PARAM_RETRANSMIT_MAX_PER_ROUND_UDP,
    PARAM_VERIFY_MAX_NAMES, PARAM_VERIFY_REWRITES, PARAM_WATCHDOG_TICK_S,
};
use corvus_link::plain::{Message, Value};
use indexmap::IndexMap;

use super::{targeted, RequestHost};

/// What the engine asks of the bridge it runs on.
pub trait ParamHost: RequestHost {
    /// Seconds on a clock that only moves forwards.
    fn now(&self) -> f64;
    fn sleep(&self, seconds: f64);
    /// A sleep that ends early when the bridge stops.
    fn interruptible_sleep(&self, seconds: f64);
    /// Whether the bridge is running, and whether it is stopping.
    fn running(&self) -> bool;
    fn stopping(&self) -> bool;
    /// `bytewise` (PX4) or `c_cast` (ArduPilot).
    fn encoding(&self) -> String;
    fn slow_link(&self) -> bool;
    /// Tells the parameter listeners.
    fn notify(&self, status: &Status);
    /// Starts the download watchdog thread (only while the bridge runs).
    fn start_watchdog(&self);

    /// How long a full download may take before it is called incomplete.
    fn download_budget_s(&self) -> f64 {
        if self.slow_link() {
            PARAM_DOWNLOAD_TIMEOUT_S_SERIAL
        } else {
            PARAM_DOWNLOAD_TIMEOUT_S
        }
    }
}

/// What the listeners are told: the state and its progress, and for one
/// parameter its name, value and type.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: String,
    pub count: i64,
    pub received: i64,
    pub name: Option<String>,
    pub value: Option<f64>,
    pub ptype: Option<i64>,
}

impl Status {
    fn progress(state: &str, count: i64, received: i64) -> Status {
        Status {
            state: state.to_string(),
            count,
            received,
            name: None,
            value: None,
            ptype: None,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("state".into(), self.state.clone().into());
        m.insert("count".into(), self.count.into());
        m.insert("received".into(), self.received.into());
        if let Some(n) = &self.name {
            m.insert("name".into(), n.clone().into());
        }
        if let Some(v) = self.value {
            m.insert("value".into(), corvus_json_num(v));
        }
        if let Some(t) = self.ptype {
            m.insert("type".into(), t.into());
        }
        serde_json::Value::Object(m)
    }
}

fn corvus_json_num(v: f64) -> serde_json::Value {
    crate::json::num(v)
}

/// One cached parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub value: f64,
    pub ptype: i64,
    pub index: i64,
    pub count: i64,
    /// Arrival order, for "newer than" reads.
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub name: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadReport {
    pub state: String,
    pub written: i64,
    pub failed: Vec<Failure>,
}

/// One target of `verify_params`, as it ended.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifyRow {
    pub name: String,
    pub wanted: f64,
    pub before: Option<f64>,
    pub after: Option<f64>,
    pub rewritten: bool,
    pub ok: bool,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifyReport {
    pub results: Vec<VerifyRow>,
    pub values: IndexMap<String, f64>,
    pub missing: Vec<String>,
    pub confirmed: usize,
    pub failed: usize,
    pub all_confirmed: bool,
}

/// A `set_param` waiting for its echo.
struct Pending {
    state: Mutex<(bool, Option<i64>)>,
    cv: Condvar,
}

impl Pending {
    fn new() -> Pending {
        Pending {
            state: Mutex::new((false, None)),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, (bool, Option<i64>)> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set(&self, result: i64) {
        *self.lock() = (true, Some(result));
        self.cv.notify_all();
    }

    fn wait(&self, timeout: f64) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        let mut s = self.lock();
        while !s.0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            s = self
                .cv
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    fn result(&self) -> Option<i64> {
        self.lock().1
    }

    fn clear(&self) {
        *self.lock() = (false, None);
    }
}

struct State {
    params: IndexMap<String, Entry>,
    count: i64,
    received: i64,
    state: String,
    seen: HashSet<i64>,
    rx_seq: u64,
    misses: HashMap<String, i64>,
    started_at: f64,
    retransmit_round: i64,
    last_value_at: f64,
    pending: HashMap<String, Arc<Pending>>,
    upload_total: i64,
    upload_done: i64,
    upload_result: Option<UploadReport>,
    state_before_upload: String,
}

/// The parameter protocol of one bridge.
pub struct ParamEngine {
    st: Mutex<State>,
    upload_cancel: AtomicBool,
}

impl Default for ParamEngine {
    fn default() -> Self {
        ParamEngine::new()
    }
}

/// Python's own text for a float, so messages read as they always have
/// (`3.0`, `9.25`, `1e-05`, `1e+16`, `nan`).
pub fn float_text(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = format!("{v:?}");
    let Some((mantissa, exp)) = s.split_once('e') else {
        return s;
    };
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp),
    };
    format!("{mantissa}e{sign}{digits:0>2}")
}

/// A parameter name that fits the wire: ASCII, 1 to 16 bytes. Rejected, not
/// truncated, because a shortened name addresses a different parameter.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.is_ascii() && name.len() <= PARAM_ID_MAX_BYTES
}

fn request_list_message(target: (u8, u8)) -> Message {
    targeted("PARAM_REQUEST_LIST", target)
}

fn request_read_message(target: (u8, u8), name: &str, index: i64) -> Message {
    let mut m = targeted("PARAM_REQUEST_READ", target);
    let _ = m.set("param_id", Value::Bytes(name.as_bytes().to_vec()));
    let _ = m.set("param_index", Value::Int(index as i128));
    m
}

fn param_set_message(target: (u8, u8), name: &str, value: f64, ptype: i64) -> Message {
    let mut m = targeted("PARAM_SET", target);
    let _ = m.set("param_id", Value::Bytes(name.as_bytes().to_vec()));
    let _ = m.set("param_value", Value::Float(value));
    let _ = m.set("param_type", Value::Int(ptype as i128));
    m
}

impl ParamEngine {
    pub fn new() -> ParamEngine {
        ParamEngine {
            st: Mutex::new(State {
                params: IndexMap::new(),
                count: -1,
                received: 0,
                state: "idle".to_string(),
                seen: HashSet::new(),
                rx_seq: 0,
                misses: HashMap::new(),
                started_at: 0.0,
                retransmit_round: 0,
                last_value_at: 0.0,
                pending: HashMap::new(),
                upload_total: 0,
                upload_done: 0,
                upload_result: None,
                state_before_upload: "idle".to_string(),
            }),
            upload_cancel: AtomicBool::new(false),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn progress_of(st: &State) -> (i64, i64) {
        if st.state == "uploading" {
            (st.upload_total, st.upload_done)
        } else {
            (st.count, st.received)
        }
    }

    // ---- the state ----

    pub fn get_state(&self) -> String {
        self.state().state.clone()
    }

    /// For tests that stage a state.
    pub fn set_state(&self, value: &str) {
        self.state().state = value.to_string();
    }

    pub fn count(&self) -> i64 {
        self.state().count
    }

    pub fn received(&self) -> i64 {
        self.state().received
    }

    pub fn status(&self) -> Status {
        let st = self.state();
        let (c, r) = Self::progress_of(&st);
        Status::progress(&st.state, c, r)
    }

    pub fn progress(&self) -> (i64, i64) {
        Self::progress_of(&self.state())
    }

    /// The cache sorted by name.
    pub fn params(&self) -> Vec<Entry> {
        let mut v: Vec<Entry> = self.state().params.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// The cache in arrival order.
    pub fn cache(&self) -> Vec<Entry> {
        self.state().params.values().cloned().collect()
    }

    pub fn entry(&self, name: &str) -> Option<Entry> {
        self.state().params.get(name).cloned()
    }

    pub fn upload_result(&self) -> UploadReport {
        self.state().upload_result.clone().unwrap_or(UploadReport {
            state: "idle".into(),
            written: 0,
            failed: Vec::new(),
        })
    }

    /// Wakes every writer waiting for an echo, stops an upload and drops the
    /// claim that this vehicle's parameters are complete.
    pub fn abort(&self) {
        self.upload_cancel.store(true, Ordering::SeqCst);
        let mut st = self.state();
        st.state = "idle".to_string();
        for pending in st.pending.values() {
            pending.set(-2);
        }
        st.pending.clear();
    }

    /// Forgets the vehicle's parameters for a new link cycle.
    pub fn reset(&self) {
        self.abort();
        let mut st = self.state();
        st.params.clear();
        st.count = -1;
        st.received = 0;
        st.state = "idle".to_string();
        st.seen.clear();
        st.started_at = 0.0;
        st.retransmit_round = 0;
        st.last_value_at = 0.0;
        st.misses.clear();
    }

    /// The read back value against the wanted one, at the parameter's own precision.
    pub fn matches(&self, name: &str, got: f64, wanted: f64) -> bool {
        let ptype = self.state().params.get(name).map_or(MAV_PARAM_TYPE_REAL32, |e| e.ptype);
        values_match(ptype, got, wanted)
    }

    // ---- receive ----

    /// Caches a received PARAM_VALUE, wakes a writer waiting for it and tells
    /// the listeners. `raw` is the first four bytes of the frame's payload,
    /// the float32 as it arrived (PX4 carries integers as their own bytes).
    pub fn handle_param_value(&self, host: &dyn ParamHost, msg: &Message, raw: Option<&[u8]>) {
        let Some(Value::Bytes(id)) = msg.get("param_id") else {
            host.log("debug", "discarding malformed PARAM_VALUE");
            return;
        };
        let end = id.iter().position(|&b| b == 0).unwrap_or(id.len());
        let Some(pname) = std::str::from_utf8(&id[..end]).ok().filter(|s| s.is_ascii()) else {
            host.log("debug", "discarding malformed PARAM_VALUE");
            return;
        };
        let pname = pname.to_string();
        let int = |f: &str| msg.get(f).and_then(Value::as_i64);
        let (Some(ptype), Some(value), Some(mut pindex), Some(pcount)) = (
            int("param_type"),
            msg.get("param_value").and_then(Value::as_f64),
            int("param_index"),
            int("param_count"),
        ) else {
            host.log("debug", "discarding malformed PARAM_VALUE");
            return;
        };
        let pval = decode_value(value, ptype, &host.encoding(), raw);
        if pindex == PARAM_INDEX_NONE {
            pindex = -1;
        }
        if pname == PARAM_HASH_CHECK {
            return;
        }
        if !valid_name(&pname)
            || !pval.is_finite()
            || !(1..=10).contains(&ptype)
            || !(-1..PARAM_CACHE_MAX_ENTRIES).contains(&pindex)
            || !(0..=PARAM_CACHE_MAX_ENTRIES).contains(&pcount)
            || (pcount > 0 && pindex >= pcount)
        {
            host.log(
                "warning",
                &format!("discarding invalid PARAM_VALUE id={pname:?} index={pindex} count={pcount} type={ptype}"),
            );
            return;
        }
        let now = host.now();
        let status = {
            let mut st = self.state();
            let is_new = !st.params.contains_key(&pname);
            st.misses.remove(&pname);
            if is_new && st.params.len() as i64 >= PARAM_CACHE_MAX_ENTRIES {
                drop(st);
                host.log("warning", &format!("parameter cache limit reached; dropping {pname}"));
                return;
            }
            if is_new {
                st.received += 1;
            }
            if pindex >= 0 {
                st.seen.insert(pindex);
            }
            st.last_value_at = now;
            st.rx_seq += 1;
            let seq = st.rx_seq;
            st.params.insert(
                pname.clone(),
                Entry {
                    name: pname.clone(),
                    value: pval,
                    ptype,
                    index: pindex,
                    count: pcount,
                    seq,
                },
            );
            if pcount > 0 {
                st.count = st.count.max(pcount);
            }
            if st.state != "uploading" && st.count > 0 && st.received >= st.count {
                st.state = "complete".to_string();
            }
            let (count, received) = Self::progress_of(&st);
            if let Some(pending) = st.pending.get(&pname) {
                pending.set(0);
            }
            let mut s = Status::progress(&st.state, count, received);
            s.name = Some(pname.clone());
            s.value = Some(pval);
            s.ptype = Some(ptype);
            s
        };
        host.notify(&status);
    }

    // ---- download ----

    fn finish_incomplete(&self, host: &dyn ParamHost) {
        let (count, received) = {
            let mut st = self.state();
            if st.state != "downloading" {
                return;
            }
            st.state = "incomplete".to_string();
            (st.count, st.received)
        };
        host.notify(&Status::progress("incomplete", count, received));
    }

    /// Flips a stuck download to a terminal `incomplete` (what arrived is kept).
    pub fn finish_download_incomplete(&self, host: &dyn ParamHost) {
        self.finish_incomplete(host);
    }

    /// PARAM_REQUEST_LIST, with the state machine around it.
    pub fn request_param_list(&self, host: &dyn ParamHost) -> bool {
        if !host.connection_ready() {
            host.set_error("not connected");
            return false;
        }
        let now = host.now();
        {
            let mut st = self.state();
            if st.state == "downloading" || st.state == "uploading" {
                drop(st);
                host.set_error("another parameter operation in progress");
                return false;
            }
            st.state = "downloading".to_string();
            st.params.clear();
            st.received = 0;
            st.count = -1;
            st.seen = HashSet::new();
            st.started_at = now;
            st.last_value_at = now;
            st.retransmit_round = 0;
        }
        let sent = match host.send(request_list_message(host.target())) {
            Ok(true) => Ok(()),
            Ok(false) => Err("link closed".to_string()),
            Err(e) => Err(e),
        };
        if let Err(text) = sent {
            {
                let mut st = self.state();
                if st.state == "downloading" {
                    st.state = "idle".to_string();
                }
            }
            host.set_error(&format!("parameter list request failed: {text}"));
            return false;
        }
        host.start_watchdog();
        true
    }

    /// PARAM_REQUEST_READ by name.
    pub fn request_param(&self, host: &dyn ParamHost, name: &str) -> bool {
        if !host.connection_ready() {
            return false;
        }
        if !valid_name(name) {
            host.set_error("invalid parameter name");
            return false;
        }
        match host.send(request_read_message(host.target(), name, -1)) {
            Ok(sent) => sent,
            Err(e) => {
                host.log("debug", &format!("param read request failed ({name}): {e}"));
                false
            }
        }
    }

    /// Re-requests what a lossy link lost, then gives up to `incomplete`. The
    /// watchdog thread runs this.
    pub fn run_watchdog(&self, host: &dyn ParamHost) {
        while host.running() && !host.stopping() {
            host.interruptible_sleep(PARAM_WATCHDOG_TICK_S);
            let (state, count, received, seen, started_at, last_value, round) = {
                let st = self.state();
                (
                    st.state.clone(),
                    st.count,
                    st.received,
                    st.seen.clone(),
                    st.started_at,
                    st.last_value_at,
                    st.retransmit_round,
                )
            };
            if state != "downloading" {
                return;
            }
            if count <= 0 {
                continue;
            }
            if received >= count {
                return;
            }
            let now = host.now();
            if now - started_at > host.download_budget_s() {
                return self.finish_incomplete(host);
            }
            if now - last_value < PARAM_DOWNLOAD_INACTIVITY_S {
                continue;
            }
            if round >= PARAM_DOWNLOAD_MAX_ROUNDS {
                return self.finish_incomplete(host);
            }
            let mut missing: Vec<i64> = (0..count).filter(|i| !seen.contains(i)).collect();
            if missing.is_empty() {
                return;
            }
            self.state().retransmit_round = round + 1;
            let (per_round, gap) = if host.slow_link() {
                (PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL, PARAM_RETRANSMIT_GAP_S_SERIAL)
            } else {
                (PARAM_RETRANSMIT_MAX_PER_ROUND_UDP, PARAM_RETRANSMIT_GAP_S_UDP)
            };
            missing.truncate(per_round);
            for idx in missing {
                if host.stopping() {
                    return;
                }
                match host.send(request_read_message(host.target(), "", idx)) {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(e) => host.log("debug", &format!("param retransmit idx={idx} failed: {e}")),
                }
                host.interruptible_sleep(gap);
            }
        }
    }

    // ---- named reads ----

    /// Polls the cache until `name` appears or `timeout` passes.
    pub fn wait_for_param(&self, host: &dyn ParamHost, name: &str, timeout: f64) -> Option<Entry> {
        let deadline = host.now() + timeout;
        while host.now() < deadline {
            if let Some(e) = self.entry(name) {
                return Some(e);
            }
            host.sleep(0.02);
        }
        None
    }

    /// A named set without a full download, ended by a barrier read once the
    /// vehicle has visibly worked through the burst. With `fresh` only what
    /// arrived after the call counts.
    pub fn fetch_params(
        &self,
        host: &dyn ParamHost,
        names: &[String],
        timeout: f64,
        fresh: bool,
    ) -> IndexMap<String, f64> {
        let wanted: Vec<String> = names.iter().filter(|n| !n.is_empty()).cloned().collect();
        if wanted.is_empty() {
            return IndexMap::new();
        }
        if !host.connection_ready() {
            host.set_error("not connected");
            return IndexMap::new();
        }
        let (since, known_absent) = {
            let st = self.state();
            let since = fresh.then_some(st.rx_seq);
            let complete = st.state == "complete";
            let absent: HashSet<String> = wanted
                .iter()
                .filter(|n| {
                    !st.params.contains_key(*n)
                        && (complete || st.misses.get(*n).copied().unwrap_or(0) >= PARAM_ABSENT_AFTER_MISSES)
                })
                .cloned()
                .collect();
            (since, absent)
        };
        let snapshot = || -> IndexMap<String, f64> {
            let st = self.state();
            let mut out = IndexMap::new();
            for n in &wanted {
                if let Some(e) = st.params.get(n) {
                    if since.is_none_or(|s| e.seq > s) && !out.contains_key(n) {
                        out.insert(n.clone(), e.value);
                    }
                }
            }
            out
        };
        let still_missing = |have: &IndexMap<String, f64>| -> Vec<String> {
            wanted
                .iter()
                .filter(|n| !have.contains_key(*n) && !known_absent.contains(*n))
                .cloned()
                .collect()
        };
        let barrier_name =
            |batch: &HashSet<String>, batch_sent_after: u64, have: &IndexMap<String, f64>| -> Option<String> {
                let st = self.state();
                if st.state == "downloading" || st.state == "uploading" {
                    return None;
                }
                let usable = |name: &str| match st.params.get(name) {
                    Some(e) => !batch.contains(name) || e.seq > batch_sent_after,
                    None => false,
                };
                have.keys()
                    .find(|n| usable(n))
                    .cloned()
                    .or_else(|| st.params.keys().find(|n| usable(n)).cloned())
            };
        let barrier_answered =
            |name: &str, sent_after: u64| -> bool { self.state().params.get(name).is_some_and(|e| e.seq > sent_after) };

        let deadline = host.now() + f64::max(0.5, timeout);
        let mut have = snapshot();
        let mut missing = still_missing(&have);
        for round_index in 0..2 {
            if missing.is_empty() {
                break;
            }
            let batch: HashSet<String> = missing.iter().cloned().collect();
            let batch_sent_after = self.state().rx_seq;
            for name in missing.clone() {
                if host.stopping() || !host.connection_ready() {
                    return snapshot();
                }
                self.request_param(host, &name);
            }
            let round_deadline = if round_index > 0 {
                deadline
            } else {
                let now = host.now();
                now + f64::max(0.25, (deadline - host.now()) / 2.0)
            };
            let mut barrier: Option<(String, u64)> = None;
            let mut barrier_sent = false;
            while host.now() < f64::min(round_deadline, deadline) {
                have = snapshot();
                missing = still_missing(&have);
                if missing.is_empty() {
                    break;
                }
                if host.stopping() {
                    return have;
                }
                if let Some((name, after)) = &barrier {
                    if barrier_answered(name, *after) {
                        break;
                    }
                }
                if !barrier_sent {
                    if let Some(name) = barrier_name(&batch, batch_sent_after, &have) {
                        barrier_sent = true;
                        let sent_after = self.state().rx_seq;
                        if self.request_param(host, &name) {
                            barrier = Some((name, sent_after));
                        }
                        continue;
                    }
                }
                host.sleep(0.02);
            }
            have = snapshot();
            missing = still_missing(&have);
        }
        if !missing.is_empty() && wanted.iter().any(|n| have.contains_key(n)) {
            let mut st = self.state();
            for name in &missing {
                *st.misses.entry(name.clone()).or_insert(0) += 1;
            }
        }
        have
    }

    // ---- writes ----

    /// Writes one parameter and confirms it from the echoed PARAM_VALUE.
    pub fn set_param(&self, host: &dyn ParamHost, name: &str, value: f64) -> bool {
        host.set_error("");
        if !valid_name(name) {
            host.set_error("invalid parameter name");
            return false;
        }
        if !value.is_finite() {
            host.set_error("parameter value must be finite");
            return false;
        }
        if !host.connection_ready() {
            host.set_error("not connected");
            return false;
        }
        if host.armed() {
            host.set_error("cannot set parameter while armed");
            return false;
        }
        let mut ptype = self.state().params.get(name).map(|e| e.ptype);
        if ptype.is_none() {
            self.request_param(host, name);
            match self.wait_for_param(host, name, 2.0) {
                Some(found) => ptype = Some(found.ptype),
                None => {
                    host.set_error("parameter not found on vehicle");
                    return false;
                }
            }
        }
        let ptype = ptype.unwrap_or(MAV_PARAM_TYPE_REAL32);
        let wire_value = match encode_value(value, ptype, &host.encoding()) {
            Ok(v) => v,
            Err(text) => {
                host.set_error(&format!("{name}: {text}"));
                return false;
            }
        };
        let pending = Arc::new(Pending::new());
        self.state().pending.insert(name.to_string(), Arc::clone(&pending));
        let outcome = self.write_and_wait(host, name, value, wire_value, ptype, &pending);
        {
            let mut st = self.state();
            if st.pending.get(name).is_some_and(|p| Arc::ptr_eq(p, &pending)) {
                st.pending.remove(name);
            }
        }
        let Ok((echoed, confirmed)) = outcome else { return false };
        let Some(echoed) = echoed else {
            host.set_error("parameter write not confirmed (timeout)");
            return false;
        };
        if confirmed {
            host.console("PARAM", &format!("{name} set to {}", float_text(value)), "success");
            return true;
        }
        host.set_error(&format!(
            "parameter write not confirmed (got {} expected {})",
            float_text(echoed.value),
            float_text(value)
        ));
        false
    }

    /// Up to three PARAM_SETs, each given a second for its echo.
    fn write_and_wait(
        &self,
        host: &dyn ParamHost,
        name: &str,
        value: f64,
        wire_value: f64,
        ptype: i64,
        pending: &Arc<Pending>,
    ) -> Result<(Option<Entry>, bool), ()> {
        let mut echoed: Option<Entry> = None;
        let mut confirmed = false;
        for _attempt in 0..3 {
            if !host.connection_ready() {
                host.set_error("not connected");
                return Err(());
            }
            match host.send(param_set_message(host.target(), name, wire_value, ptype)) {
                Ok(true) => {}
                Ok(false) => {
                    host.set_error("not connected");
                    return Err(());
                }
                Err(text) => {
                    host.set_error(&format!("parameter write failed: {text}"));
                    return Err(());
                }
            }
            let mut answered = false;
            let deadline = host.now() + 1.0;
            loop {
                let remaining = deadline - host.now();
                if remaining <= 0.0 {
                    break;
                }
                if !pending.wait(remaining) {
                    break;
                }
                if pending.result() == Some(-2) {
                    host.set_error("not connected");
                    return Err(());
                }
                answered = true;
                echoed = self.state().params.get(name).cloned();
                if let Some(e) = &echoed {
                    if echo_confirms(e.ptype, e.value, value) {
                        confirmed = true;
                        break;
                    }
                }
                pending.clear();
            }
            if confirmed || answered {
                break;
            }
        }
        Ok((echoed, confirmed))
    }

    /// Reads every name fresh, writes a differing target again (never while
    /// armed) and reports what the vehicle holds now. `None` carries the
    /// reason in the host's last error.
    pub fn verify_params(
        &self,
        host: &dyn ParamHost,
        targets: &[(String, f64)],
        names: Option<&[String]>,
        rewrite: bool,
        timeout: f64,
    ) -> Option<VerifyReport> {
        host.set_error("");
        let mut wanted: IndexMap<String, f64> = IndexMap::new();
        for (name, value) in targets {
            if !valid_name(name) {
                host.set_error("invalid parameter name");
                return None;
            }
            if !value.is_finite() {
                host.set_error(&format!("{name}: value must be a finite number"));
                return None;
            }
            wanted.insert(name.clone(), *value);
        }
        let mut read_names: Vec<String> = wanted.keys().cloned().collect();
        for name in names.unwrap_or(&[]) {
            if !valid_name(name) {
                host.set_error("invalid parameter name");
                return None;
            }
            if !read_names.contains(name) {
                read_names.push(name.clone());
            }
        }
        if read_names.is_empty() {
            host.set_error("nothing to check");
            return None;
        }
        if read_names.len() > PARAM_VERIFY_MAX_NAMES {
            host.set_error(&format!(
                "too many parameters ({}); the maximum is {PARAM_VERIFY_MAX_NAMES}",
                read_names.len()
            ));
            return None;
        }
        if !host.connection_ready() {
            host.set_error("not connected");
            return None;
        }
        let mut values = self.fetch_params(host, &read_names, timeout, true);
        let mut results: Vec<VerifyRow> = Vec::new();
        let mut confirmed_count = 0usize;
        for (name, value) in &wanted {
            let before = values.get(name).copied();
            let mut row = VerifyRow {
                name: name.clone(),
                wanted: *value,
                before,
                after: before,
                rewritten: false,
                ok: false,
                error: String::new(),
            };
            let Some(held) = before else {
                row.error = "the vehicle did not answer for this parameter".into();
                results.push(row);
                continue;
            };
            if self.matches(name, held, *value) {
                row.ok = true;
                confirmed_count += 1;
                results.push(row);
                continue;
            }
            if !rewrite {
                row.error = "the vehicle holds a different value".into();
                results.push(row);
                continue;
            }
            if host.armed() {
                row.error = "not written again: the vehicle is armed".into();
                results.push(row);
                continue;
            }
            for _attempt in 0..PARAM_VERIFY_REWRITES {
                row.rewritten = true;
                row.error.clear();
                if !self.set_param(host, name, *value) {
                    let last = host.last_error();
                    row.error = if last.is_empty() {
                        "write failed".to_string()
                    } else {
                        last
                    };
                    if row.error.contains("not connected") {
                        break;
                    }
                }
                let after = self
                    .fetch_params(host, std::slice::from_ref(name), 2.0, true)
                    .get(name)
                    .copied();
                if let Some(a) = after {
                    row.after = Some(a);
                    values.insert(name.clone(), a);
                }
                if after.is_some_and(|a| self.matches(name, a, *value)) {
                    row.ok = true;
                    row.error.clear();
                    confirmed_count += 1;
                    break;
                }
                if row.error.is_empty() {
                    row.error = if after.is_none() {
                        "no answer when read back"
                    } else {
                        "the vehicle kept a different value"
                    }
                    .into();
                }
            }
            results.push(row);
        }
        host.set_error("");
        let missing: Vec<String> = read_names
            .iter()
            .filter(|n| !values.contains_key(*n))
            .cloned()
            .collect();
        Some(VerifyReport {
            results,
            values,
            missing,
            confirmed: confirmed_count,
            failed: wanted.len() - confirmed_count,
            all_confirmed: confirmed_count == wanted.len(),
        })
    }

    // ---- upload ----

    /// Starts a batch upload: checks the list and the state, claims the
    /// engine. `run_upload` then writes it, on the caller's thread.
    pub fn begin_upload(&self, host: &dyn ParamHost, params: &[(String, f64)]) -> bool {
        if params.is_empty() || params.iter().any(|(n, _)| n.is_empty()) {
            host.set_error("invalid parameter list");
            return false;
        }
        if !host.connection_ready() {
            host.set_error("not connected");
            return false;
        }
        if host.armed() {
            host.set_error("cannot upload parameters while armed");
            return false;
        }
        let mut st = self.state();
        if st.state == "downloading" || st.state == "uploading" {
            drop(st);
            host.set_error("another parameter operation in progress");
            return false;
        }
        st.state_before_upload = st.state.clone();
        st.state = "uploading".to_string();
        st.upload_total = params.len() as i64;
        st.upload_done = 0;
        st.upload_result = None;
        self.upload_cancel.store(false, Ordering::SeqCst);
        true
    }

    fn finish_upload(&self, host: &dyn ParamHost, failed: Vec<Failure>, interrupted: bool) {
        let (outcome, total, done) = {
            let mut st = self.state();
            let outcome = if interrupted { "interrupted" } else { "upload_complete" };
            let before = st.state_before_upload.clone();
            st.state = if interrupted || (before != "complete" && before != "incomplete") {
                "idle".to_string()
            } else {
                before
            };
            let written = st.upload_done;
            st.upload_result = Some(UploadReport {
                state: outcome.to_string(),
                written,
                failed,
            });
            (outcome, st.upload_total, st.upload_done)
        };
        host.notify(&Status::progress(outcome, total, done));
    }

    /// Writes the batch begun by `begin_upload`, one confirmed write at a time.
    pub fn run_upload(&self, host: &dyn ParamHost, params: &[(String, f64)]) {
        let mut failed: Vec<Failure> = Vec::new();
        for (name, value) in params {
            if !host.running() || self.upload_cancel.load(Ordering::SeqCst) {
                break;
            }
            let ok = self.set_param(host, name, *value);
            if ok {
                self.state().upload_done += 1;
            } else {
                let last = host.last_error();
                failed.push(Failure {
                    name: name.clone(),
                    error: if last.is_empty() {
                        "write failed".to_string()
                    } else {
                        last
                    },
                });
            }
            let status = {
                let st = self.state();
                let mut s = Status::progress(&st.state, st.upload_total, st.upload_done);
                s.name = Some(name.clone());
                s.value = Some(*value);
                s
            };
            host.notify(&status);
        }
        let interrupted = self.upload_cancel.load(Ordering::SeqCst) || !host.running();
        self.finish_upload(host, failed, interrupted);
    }
}
