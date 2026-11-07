//! What the vehicle says in words: STATUSTEXT reassembly and publishing, the
//! vehicle events a line can report (the kill switch), and the preflight
//! report requests.
//!
//! The twin of `corvus_link`'s status engine (`bridge/status.rs`). MAVLink 2
//! STATUSTEXT arrives in chunks of fifty bytes that are put back together
//! before they are shown. The kill switch is learned from its own line, from
//! a failing preflight check, or from a heartbeat's termination flag, and
//! released by a pass or by a whole report that no longer names it. The
//! engine owns the preflight state; the kill evidence time is shared with the
//! receive path and lives with the bridge (`StatusHost`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use corvus_link::plain::defs::constant_or;
use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Map, Value};

use super::targeted;
use crate::dialect::{Dialect, VehicleEvent};

pub const KILL_RECHECK_S: f64 = 5.0;
pub const KILL_SETTLE_S: f64 = 3.0;
pub const PREARM_CHANGE_REQUEST_MIN_S: f64 = 2.0;
pub const PREARM_REASONS_MAX: usize = 20;
pub const PREARM_REPORT_GAP_S: f64 = 2.0;
pub const PREARM_REPORT_REQUEST_EVERY_S: f64 = 30.0;
pub const STATUSTEXT_CHUNK_BYTES: usize = 50;
pub const STATUSTEXT_CHUNK_TIMEOUT_S: f64 = 10.0;
pub const STATUSTEXT_MAX_PARTIALS: usize = 64;

fn level_rank(level: &str) -> i64 {
    match level {
        "info" => 0,
        "warning" => 1,
        "critical" => 2,
        _ => -1,
    }
}

/// What the status engine asks of the bridge it runs on.
pub trait StatusHost: Send + Sync {
    fn now(&self) -> f64;
    fn console(&self, source: &str, text: &str, level: &str);
    fn log(&self, level: &str, text: &str);
    fn dialect(&self) -> Dialect;
    fn store_update(&self, fields: Map<String, Value>);
    /// The `armed` and `kill_switch` flags the store holds.
    fn armed(&self) -> bool;
    fn kill_switch(&self) -> bool;
    fn resolve_warnings(&self, kinds: &[&str]);
    /// Merges a warning into the vehicle state, tagged with the event it belongs to.
    fn merge_warning(&self, text: &str, level: &str, event: Option<&str>);
    /// When the kill switch was last evidenced (a line, a failing check, a
    /// terminated heartbeat); shared with the receive path.
    fn kill_evidence_at(&self) -> f64;
    fn set_kill_evidence_at(&self, at: f64);
    fn target(&self) -> (u8, u8);
    fn has_connection(&self) -> bool;
    /// Sends one message under the link's send lock. `Ok(false)` is "the link
    /// is no longer this bridge's".
    fn send(&self, msg: Message) -> Result<bool, String>;
}

struct Partial {
    inserted: u64,
    parts: BTreeMap<i64, String>,
    final_seq: Option<i64>,
    severity: i64,
    updated: f64,
}

struct State {
    chunks: HashMap<(i64, i64, i64), Partial>,
    inserted: u64,
    prearm_reasons: Vec<String>,
    prearm_reason_at: f64,
    prearm_requested_at: f64,
    prearm_last: Option<bool>,
    prearm_report_started_at: f64,
    prearm_report_kill: bool,
}

/// The status state of one bridge.
pub struct StatusEngine {
    state: Mutex<State>,
}

impl Default for StatusEngine {
    fn default() -> Self {
        StatusEngine::new()
    }
}

/// A STATUSTEXT as it was received, with who sent it.
pub struct StatusText<'a> {
    pub msg: &'a Message,
    pub system: i64,
    pub component: i64,
}

fn int_field(msg: &Message, name: &str) -> Option<i64> {
    match msg.get(name)? {
        Wv::Int(i) => Some(*i as i64),
        _ => None,
    }
}

impl StatusEngine {
    pub fn new() -> StatusEngine {
        StatusEngine {
            state: Mutex::new(State {
                chunks: HashMap::new(),
                inserted: 0,
                prearm_reasons: Vec::new(),
                prearm_reason_at: 0.0,
                prearm_requested_at: 0.0,
                prearm_last: None,
                prearm_report_started_at: 0.0,
                prearm_report_kill: false,
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn prearm_reasons(&self) -> Vec<String> {
        self.st().prearm_reasons.clone()
    }

    pub fn has_prearm_reasons(&self) -> bool {
        !self.st().prearm_reasons.is_empty()
    }

    pub fn partial_count(&self) -> usize {
        self.st().chunks.len()
    }

    pub fn prearm_requested_at(&self) -> f64 {
        self.st().prearm_requested_at
    }

    pub fn prearm_reason_at(&self) -> f64 {
        self.st().prearm_reason_at
    }

    pub fn prearm_report_started_at(&self) -> f64 {
        self.st().prearm_report_started_at
    }

    pub fn prearm_report_kill(&self) -> bool {
        self.st().prearm_report_kill
    }

    pub fn prearm_last(&self) -> Option<bool> {
        self.st().prearm_last
    }

    /// Forgets the preflight state of the link that just ended.
    pub fn forget_link(&self) {
        let mut st = self.st();
        st.chunks.clear();
        st.prearm_reasons.clear();
        st.prearm_reason_at = 0.0;
        st.prearm_requested_at = 0.0;
        st.prearm_last = None;
        st.prearm_report_started_at = 0.0;
        st.prearm_report_kill = false;
    }

    // ------------------------------------------------------- STATUSTEXT

    /// Reassembles MAVLink 2 STATUSTEXT chunks and publishes complete messages.
    pub fn handle_statustext(&self, host: &dyn StatusHost, frame: &StatusText) {
        let now = host.now();
        self.cleanup_statustext_chunks(now);
        let msg = frame.msg;
        let raw_bytes: Vec<u8> = match msg.get("text") {
            Some(Wv::Bytes(b)) => b.clone(),
            _ => Vec::new(),
        };
        let end = raw_bytes.iter().position(|b| *b == 0).unwrap_or(raw_bytes.len());
        let text = String::from_utf8_lossy(&raw_bytes[..end]).into_owned();
        let severity = int_field(msg, "severity").unwrap_or(0);
        let chunk_id = int_field(msg, "id").unwrap_or(0);
        let chunk_seq = int_field(msg, "chunk_seq").unwrap_or(0);
        let has_terminator = raw_bytes.contains(&0);
        let is_final = has_terminator || raw_bytes.len() < STATUSTEXT_CHUNK_BYTES;

        if chunk_id == 0 {
            self.publish_statustext(host, &text, severity);
            return;
        }
        let key = (frame.system, frame.component, chunk_id);
        let complete = {
            let mut st = self.st();
            if !st.chunks.contains_key(&key) && st.chunks.len() >= STATUSTEXT_MAX_PARTIALS {
                // Evict the least recently updated rather than refusing the
                // new one: the oldest partial is the one least likely to still
                // be completed, and dropping the newest would stall the live message.
                let oldest = st
                    .chunks
                    .iter()
                    .min_by(|a, b| {
                        // Ties go to the one that has been there longest, as a dict iterates.
                        (a.1.updated, a.1.inserted)
                            .partial_cmp(&(b.1.updated, b.1.inserted))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(k, _)| *k);
                if let Some(k) = oldest {
                    st.chunks.remove(&k);
                }
            }
            st.inserted += 1;
            let inserted = st.inserted;
            let partial = st.chunks.entry(key).or_insert_with(|| Partial {
                inserted,
                parts: BTreeMap::new(),
                final_seq: None,
                severity,
                updated: now,
            });
            partial.parts.insert(chunk_seq, text);
            partial.severity = partial.severity.min(severity);
            partial.updated = now;
            if is_final {
                partial.final_seq = Some(chunk_seq);
            }
            let Some(final_seq) = partial.final_seq else { return };
            if !(0..=final_seq).all(|seq| partial.parts.contains_key(&seq)) {
                return;
            }
            let joined: String = (0..=final_seq).map(|seq| partial.parts[&seq].as_str()).collect();
            let severity = partial.severity;
            st.chunks.remove(&key);
            (joined, severity)
        };
        self.publish_statustext(host, &complete.0, complete.1);
    }

    pub fn cleanup_statustext_chunks(&self, now: f64) {
        self.st()
            .chunks
            .retain(|_, p| now - p.updated <= STATUSTEXT_CHUNK_TIMEOUT_S);
    }

    fn publish_statustext(&self, host: &dyn StatusHost, text: &str, severity: i64) {
        // PX4 ends a STATUSTEXT that doubles one of its events with a tab (a
        // marker for stations that show the event instead). Corvus shows the
        // text, and without the strip the same message is two warnings.
        let text = text.trim_end_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
        let level = if severity <= 3 {
            "critical"
        } else if severity <= 5 {
            "warning"
        } else {
            "info"
        };
        host.console("STATUSTEXT", text, level);
        let dialect = host.dialect();
        if let Some(event) = dialect.vehicle_event(text) {
            self.note_vehicle_event(host, &event, level);
            return;
        }
        let reason = dialect.prearm_failure(text);
        if let Some(reason) = &reason {
            self.note_prearm_failure(host, reason);
            if let Some(kill) = dialect.prearm_event(reason) {
                self.st().prearm_report_kill = true;
                host.set_kill_evidence_at(host.now());
                // The switch, named by the check it fails because its own line
                // was never seen. Said once, in that line's words, so the board
                // and the bar call it the same thing however it was learned.
                if !host.kill_switch() {
                    self.note_vehicle_event(host, &kill, level);
                }
                return;
            }
        }
        if level != "info" || !dialect.is_routine_statustext(text) {
            host.merge_warning(text, level, reason.as_ref().map(|_| "prearm"));
        }
    }

    /// Latches the kill switch a line reports and puts the line on the board.
    fn note_vehicle_event(&self, host: &dyn StatusHost, event: &VehicleEvent, level: &str) {
        if event.kind == "kill" {
            host.set_kill_evidence_at(host.now());
            host.store_update(fields(&[("kill_switch", json!(true))]));
        }
        let worst = if level_rank(&event.level) > level_rank(level) {
            event.level.as_str()
        } else {
            level
        };
        host.merge_warning(&event.message, worst, Some(&event.kind));
        if event.kind == "unkill" {
            self.release_kill_switch(host);
        }
    }

    /// The switch is off: clear it, and the lockdown blamed on it. PX4 reports
    /// the kill switch as flight termination in a heartbeat that comes once a
    /// second, so waiting for the next one had the bar read TERMINATED for up
    /// to a second after every release.
    pub fn release_kill_switch(&self, host: &dyn StatusHost) {
        let mut changes = fields(&[("kill_switch", json!(false))]);
        if host.kill_switch() {
            changes.insert("flight_termination".into(), json!(false));
        }
        host.store_update(changes);
        host.resolve_warnings(&["kill"]);
    }

    // -------------------------------------------------- preflight report

    fn note_prearm_failure(&self, host: &dyn StatusHost, reason: &str) {
        let now = host.now();
        let update = {
            let mut st = self.st();
            if now - st.prearm_reason_at > PREARM_REPORT_GAP_S {
                st.prearm_reasons.clear();
                st.prearm_report_started_at = now;
                st.prearm_report_kill = false;
            }
            st.prearm_reason_at = now;
            if st.prearm_reasons.iter().any(|r| r == reason) || st.prearm_reasons.len() >= PREARM_REASONS_MAX {
                return;
            }
            st.prearm_reasons.push(reason.to_string());
            st.prearm_reasons.clone()
        };
        host.store_update(fields(&[("prearm_reasons", json!(update))]));
    }

    pub fn clear_prearm_reasons(&self, host: &dyn StatusHost) {
        {
            let mut st = self.st();
            st.prearm_reasons.clear();
            st.prearm_reason_at = 0.0;
        }
        host.store_update(fields(&[("prearm_reasons", json!([]))]));
    }

    /// Reacts to SYS_STATUS's verdict: clears the reasons, or goes and gets them.
    pub fn note_prearm_state(&self, host: &dyn StatusHost, prearm: Option<bool>) {
        let now = host.now();
        let previous = {
            let mut st = self.st();
            std::mem::replace(&mut st.prearm_last, prearm)
        };
        let (armed, kill_switch) = (host.armed(), host.kill_switch());
        if prearm == Some(true) {
            if self.has_prearm_reasons() {
                self.clear_prearm_reasons(host);
            }
            host.resolve_warnings(&["prearm"]);
            // Disarmed and ready means the switch is off: an engaged one fails
            // a check of its own on both stacks. Armed, PX4 skips that check,
            // so a pass says nothing about the switch. Nor does a pass that
            // may predate the switch's own line (KILL_SETTLE_S).
            if !armed {
                if kill_switch {
                    if now - host.kill_evidence_at() >= KILL_SETTLE_S {
                        self.release_kill_switch(host);
                    }
                } else {
                    host.resolve_warnings(&["kill"]);
                }
            }
            return;
        }
        if prearm != Some(false) {
            return;
        }
        self.check_kill_released_by_report(host, now, armed, kill_switch);
        let requested_at = self.st().prearm_requested_at;
        if previous == Some(true) && !armed && now - requested_at >= PREARM_CHANGE_REQUEST_MIN_S {
            // Ready a moment ago, not now. If that was a kill switch whose line
            // was lost, the report names it within a second.
            self.st().prearm_requested_at = now;
            self.request_prearm_report(host);
            return;
        }
        let dialect = host.dialect();
        if kill_switch
            && !armed
            && dialect.prearm_report_complete()
            && now - requested_at.max(host.kill_evidence_at()) >= KILL_RECHECK_S
        {
            self.st().prearm_requested_at = now;
            self.request_prearm_report(host);
            return;
        }
        if self.has_prearm_reasons() {
            return;
        }
        if now - requested_at < PREARM_REPORT_REQUEST_EVERY_S {
            return;
        }
        self.st().prearm_requested_at = now;
        self.request_prearm_report(host);
    }

    /// Releases the switch when a whole report asked for since says nothing of
    /// it. Only on a stack whose report names every failing check, only while
    /// disarmed (the checks do not run armed), and only for a report that began
    /// after the last request, which was itself made after the switch was last
    /// reported engaged. A report still arriving is not judged.
    fn check_kill_released_by_report(&self, host: &dyn StatusHost, now: f64, armed: bool, kill_switch: bool) {
        if !kill_switch || armed {
            return;
        }
        if !host.dialect().prearm_report_complete() {
            return;
        }
        let (report_kill, started_at, requested_at, reason_at) = {
            let st = self.st();
            (
                st.prearm_report_kill,
                st.prearm_report_started_at,
                st.prearm_requested_at,
                st.prearm_reason_at,
            )
        };
        if report_kill || started_at == 0.0 {
            return;
        }
        if requested_at <= host.kill_evidence_at() {
            return;
        }
        if started_at < requested_at {
            return;
        }
        if now - reason_at <= PREARM_REPORT_GAP_S {
            return;
        }
        self.release_kill_switch(host);
    }

    /// Asks for the preflight report now. Fire and forget: it runs on the
    /// receive thread, so it cannot wait for its own ACK, and whether it worked
    /// is judged by the reasons arriving.
    pub fn request_prearm_report(&self, host: &dyn StatusHost) {
        if !host.has_connection() {
            return;
        }
        let nan = f64::NAN;
        let mut msg = targeted("COMMAND_LONG", host.target())
            .with(
                "command",
                Wv::Int(i128::from(constant_or("MAV_CMD_RUN_PREARM_CHECKS", 401))),
            )
            .with("confirmation", Wv::Int(0));
        for n in 1..=7 {
            msg = msg.with(&format!("param{n}"), Wv::Float(nan));
        }
        // The next SYS_STATUS asks again.
        if let Err(e) = host.send(msg) {
            host.log("debug", &format!("prearm report request failed: {e}"));
        }
    }
}

fn fields(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}
