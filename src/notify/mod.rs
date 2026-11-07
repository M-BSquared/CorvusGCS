//! The notification board, as python/src/js/topbar.js keeps it, with
//! python/src/js/notification_dedupe.js and the toasts of python/src/js/ui.js.
//!
//! Remote notifications are the vehicle's (`state.warnings`), local ones are
//! commands this station sent that were refused. The rules are topbar.js's:
//!
//! * Nothing deletes itself: info ages out after a minute, a resolved line a
//!   minute after it resolved, a warning or critical stays until dismissed.
//! * The badge counts what is unread while anything is, and what still
//!   stands once it has all been seen; its colour is the worst unread level,
//!   grey ("read") when everything has been seen.
//! * Closing the board marks it read, not opening it.
//! * A fresh link or an autopilot reboot clears the board; arming or
//!   disarming marks it read (a disarm keeps what arrived with it new).
//! * A refused command and the vehicle's own line about it are one
//!   notification: the tracker pairs them and the local one goes.
//! * Warnings and criticals are toasted (again after ten seconds at the
//!   earliest), and so are the kill switch, a disarm and Ready to fly.

pub mod overlay;

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use crate::home::topbar;

const INFO_TTL_MS: f64 = 60000.0;
const RESOLVED_TTL_MS: f64 = 60000.0;
const MILESTONE_GRACE_MS: f64 = 3000.0;
const TOAST_REPEAT_MS: f64 = 10000.0;
const READY_HOLD_MS: f64 = 1500.0;
const DISARM_EVENT_WINDOW_MS: f64 = 3000.0;
const DEDUPE_WINDOW_MS: f64 = 3000.0;
const TOAST_STACK_MAX: usize = 3;
/// What the board keeps of its own lines and of commands in flight; the
/// oldest go first, so a long session cannot grow either without bound.
const LOCAL_MAX: usize = 200;
const ATTEMPTS_MAX: usize = 100;

/// One line on the board.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub local_id: Option<u64>,
    pub level: String,
    pub msg: String,
    pub meta: String,
    pub resolved: bool,
    pub event: String,
}

impl Notification {
    fn from_remote(w: &Value) -> Notification {
        let s = |k: &str| w.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        Notification {
            local_id: None,
            level: s("level"),
            msg: s("msg"),
            meta: s("meta"),
            resolved: w.get("resolved") == Some(&Value::Bool(true)),
            event: s("event"),
        }
    }

    /// topbar.js notificationKey.
    pub fn key(&self) -> String {
        match self.local_id {
            Some(id) => format!("local:{id}"),
            None => {
                let level = if self.level.is_empty() { "info" } else { &self.level };
                format!("remote:{level}:{}:{}", self.msg, self.meta)
            }
        }
    }

    /// PX4 says "error", the board draws "critical".
    pub fn level(&self) -> &str {
        match self.level.as_str() {
            "" => "info",
            "error" => "critical",
            l => l,
        }
    }
}

/// What the badge says.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub items: Vec<Notification>,
    pub unread: Vec<String>,
    pub count: usize,
    /// "healthy", "info", "warning", "critical" or "read".
    pub level: &'static str,
}

/// The command a message is about (notification_dedupe actionFromMessage).
pub fn action_from_message(message: &str) -> &'static str {
    let t = message.to_lowercase();
    let words: Vec<&str> = t
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &str| words.contains(&w);
    let seq = |a: &str, b: &str| words.windows(2).any(|p| p[0] == a && p[1] == b);
    if ["disarm", "disarmed", "disarming"].iter().any(|w| has(w)) {
        return "disarm";
    }
    if ["arm", "armed", "arming"].iter().any(|w| has(w)) {
        return "arm";
    }
    if has("takeoff") || seq("take", "off") || t.contains("take-off") {
        return "takeoff";
    }
    if has("rtl") || words.windows(3).any(|p| p == ["return", "to", "launch"]) {
        return "rtl";
    }
    if ["land", "landed", "landing"].iter().any(|w| has(w)) {
        return "land";
    }
    if seq("fly", "to") || seq("drive", "to") || seq("driving", "to") {
        return "gotopoints";
    }
    if has("mode") {
        return "mode";
    }
    ""
}

/// Whether a message reports a failure (isFailureMessage).
pub fn is_failure_message(message: &str) -> bool {
    let t = message.to_lowercase();
    let words: Vec<&str> = t
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    [
        "fail",
        "reject",
        "deni",
        "unsupported",
        "disconnect",
        "unknown",
        "invalid",
        "error",
        "unavailable",
    ]
    .iter()
    .any(|s| t.contains(s))
        || words
            .windows(2)
            .any(|p| (p[0] == "time" || p[0] == "timed") && p[1] == "out")
        || t.contains("timeout")
        || words.windows(2).any(|p| p[0] == "did" && p[1] == "not")
        || words
            .windows(2)
            .any(|p| p[0] == "no" && ["home", "global", "altitude", "connection"].contains(&p[1]))
}

#[derive(Debug, Clone, PartialEq)]
enum AttemptStatus {
    Pending,
    Failed,
}

#[derive(Debug, Clone)]
struct Attempt {
    actions: Vec<&'static str>,
    started_at: f64,
    completed_at: f64,
    status: AttemptStatus,
    remote_matched: bool,
    local_id: Option<u64>,
}

/// A toast: the card in the top right corner.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub level: &'static str,
    pub title: String,
    pub text: String,
    pub at: f64,
    pub duration: f64,
}

/// The board and everything it remembers.
#[derive(Debug, Default)]
pub struct Board {
    local: BTreeMap<u64, Notification>,
    next_local: u64,
    dismissed: HashSet<String>,
    read: HashSet<String>,
    first_seen: HashMap<String, f64>,
    resolved_at: HashMap<String, f64>,
    remote_keys: Option<HashSet<String>>,
    attempts: BTreeMap<u64, Attempt>,
    next_attempt: u64,
    prev_connected: Option<bool>,
    prev_armed: Option<bool>,
    prev_boot_ms: f64,
    prev_ready: Option<String>,
    ready_from: Option<String>,
    ready_since: f64,
    ready_announced: bool,
    last_disarm_reason_at: f64,
    plain_disarm_toast: Option<(u64, f64)>,
    toasted_at: HashMap<String, f64>,
    pub toasts: Vec<Toast>,
    next_toast: u64,
    /// The board popover is open.
    pub open: bool,
    /// Set when the backend's own store should be cleared too.
    pub clear_remote: bool,
    /// Draw the severity mark through the icon (Settings, Notifications).
    pub marks: bool,
    remote: Vec<Notification>,
}

fn local_time() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let t = (secs + crate::panel::console::local_offset()).rem_euclid(86400);
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

impl Board {
    /// Everything on the board now, minus the dismissed and the aged out;
    /// also where the per-key memories are pruned.
    pub fn visible(&mut self, now: f64) -> Vec<Notification> {
        let all: Vec<Notification> = self
            .remote
            .iter()
            .cloned()
            .chain(self.local.values().cloned())
            .collect();
        let active: HashSet<String> = all.iter().map(Notification::key).collect();
        self.dismissed.retain(|k| active.contains(k));
        self.read.retain(|k| active.contains(k));
        self.first_seen.retain(|k, _| active.contains(k));
        self.resolved_at.retain(|k, _| active.contains(k));
        all.into_iter()
            .filter(|n| {
                let key = n.key();
                if self.dismissed.contains(&key) {
                    return false;
                }
                let first = *self.first_seen.entry(key.clone()).or_insert(now);
                if n.resolved {
                    let at = *self.resolved_at.entry(key).or_insert(now);
                    return now - at < RESOLVED_TTL_MS;
                }
                self.resolved_at.remove(&key);
                if n.level() != "info" {
                    return true;
                }
                now - first < INFO_TTL_MS
            })
            .collect()
    }

    /// The badge.
    pub fn summary(&mut self, now: f64) -> Summary {
        let items = self.visible(now);
        let standing: Vec<&Notification> = items.iter().filter(|n| !n.resolved).collect();
        let unread: Vec<&Notification> = standing
            .iter()
            .copied()
            .filter(|n| !self.read.contains(&n.key()))
            .collect();
        let level = if !unread.is_empty() {
            let levels: Vec<&str> = unread.iter().map(|n| n.level()).collect();
            if levels.contains(&"critical") {
                "critical"
            } else if levels.contains(&"warning") {
                "warning"
            } else {
                "info"
            }
        } else if !standing.is_empty() {
            "read"
        } else {
            "healthy"
        };
        let count = if unread.is_empty() {
            standing.len()
        } else {
            unread.len()
        };
        let unread = unread.iter().map(|n| n.key()).collect();
        Summary {
            items,
            unread,
            count,
            level,
        }
    }

    /// Everything first seen before `seen_before` has been seen.
    pub fn mark_all_read(&mut self, now: f64, seen_before: f64) {
        for n in self.visible(now) {
            let key = n.key();
            if self.first_seen.get(&key).copied().unwrap_or(0.0) >= seen_before {
                continue;
            }
            self.read.insert(key);
        }
    }

    /// Opens or closes the board; closing marks it read.
    pub fn set_open(&mut self, open: bool, now: f64) {
        let was = self.open;
        self.open = open;
        if was && !open {
            self.mark_all_read(now, f64::INFINITY);
        }
    }

    pub fn dismiss(&mut self, key: &str) {
        if let Some(id) = key.strip_prefix("local:").and_then(|i| i.parse::<u64>().ok()) {
            self.local.remove(&id);
        } else {
            self.dismissed.insert(key.to_string());
        }
    }

    /// Empties the board, the backend's store with it (`clear_remote`).
    pub fn clear_all(&mut self) {
        for n in &self.remote {
            self.dismissed.insert(n.key());
        }
        self.local.clear();
        self.read.clear();
        self.first_seen.clear();
        self.resolved_at.clear();
        self.clear_remote = true;
    }

    /// A refused command or an application error; toasted as well.
    pub fn add_local(&mut self, level: &str, message: &str, now: f64) -> u64 {
        self.next_local += 1;
        let id = self.next_local;
        let msg = if message.is_empty() {
            "Command failed".to_string()
        } else {
            message.to_string()
        };
        let n = Notification {
            local_id: Some(id),
            level: level.into(),
            msg: msg.clone(),
            meta: local_time(),
            resolved: false,
            event: String::new(),
        };
        let lvl = n.level().to_string();
        self.local.insert(id, n);
        while self.local.len() > LOCAL_MAX {
            self.local.pop_first();
        }
        self.toast(&lvl, &msg, "", "", now);
        id
    }

    /// A command is on its way (beginCommand); `actions` are what a vehicle
    /// line about it would name.
    pub fn begin(&mut self, actions: &[&'static str], now: f64) -> u64 {
        self.next_attempt += 1;
        let id = self.next_attempt;
        self.attempts.insert(
            id,
            Attempt {
                actions: actions.to_vec(),
                started_at: now,
                completed_at: 0.0,
                status: AttemptStatus::Pending,
                remote_matched: false,
                local_id: None,
            },
        );
        while self.attempts.len() > ATTEMPTS_MAX {
            self.attempts.pop_first();
        }
        id
    }

    pub fn succeeded(&mut self, attempt: u64) {
        self.attempts.remove(&attempt);
    }

    /// The command was refused: one critical notification, unless the
    /// vehicle's own line about it is already on the board.
    pub fn failed(&mut self, attempt: u64, message: &str, now: f64) {
        let should_add = self.attempts.get(&attempt).is_none_or(|a| !a.remote_matched);
        if should_add {
            let id = self.add_local("critical", message, now);
            if let Some(a) = self.attempts.get_mut(&attempt) {
                a.local_id = Some(id);
            }
        }
        if let Some(a) = self.attempts.get_mut(&attempt) {
            a.status = AttemptStatus::Failed;
            a.completed_at = now;
        }
    }

    fn expire_attempts(&mut self, now: f64) {
        self.attempts
            .retain(|_, a| a.status != AttemptStatus::Failed || now - a.completed_at <= DEDUPE_WINDOW_MS + 50.0);
    }

    /// notification_dedupe matchRemote.
    fn match_remote(&mut self, n: &Notification, now: f64) -> Option<u64> {
        let action = action_from_message(&n.msg);
        if action.is_empty() || !is_failure_message(&n.msg) {
            return None;
        }
        let candidates: Vec<u64> = self
            .attempts
            .iter()
            .filter(|(_, a)| {
                !a.remote_matched
                    && a.actions.contains(&action)
                    && (a.status == AttemptStatus::Pending || now - a.completed_at <= DEDUPE_WINDOW_MS)
            })
            .map(|(id, _)| *id)
            .collect();
        let get = |id: &u64| &self.attempts[id];
        let mut local_first: Vec<u64> = candidates
            .iter()
            .copied()
            .filter(|id| get(id).status == AttemptStatus::Failed && get(id).local_id.is_some())
            .collect();
        local_first.sort_by(|a, b| get(a).started_at.total_cmp(&get(b).started_at));
        let pending = candidates
            .iter()
            .rev()
            .copied()
            .find(|id| get(id).status == AttemptStatus::Pending);
        let failed = candidates
            .iter()
            .rev()
            .copied()
            .find(|id| get(id).status == AttemptStatus::Failed);
        let id = local_first.first().copied().or(pending).or(failed)?;
        let a = self.attempts.get_mut(&id)?;
        a.remote_matched = true;
        a.local_id
    }

    /// Takes in one telemetry state: the vehicle's lines, the milestones
    /// and readiness, as handleTelemetryState does.
    pub fn update(&mut self, state: &Value, now: f64) {
        self.expire_attempts(now);
        self.sync_remote(state, now);
        self.milestones(state, now);
        self.readiness(state, now);
        self.toasts.retain(|t| t.duration <= 0.0 || now - t.at < t.duration);
        self.toasted_at.retain(|_, at| now - *at < TOAST_REPEAT_MS);
    }

    fn sync_remote(&mut self, state: &Value, now: f64) {
        let remote: Vec<Notification> = state
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(Notification::from_remote).collect())
            .unwrap_or_default();
        let next: HashSet<String> = remote.iter().map(Notification::key).collect();
        self.remote = remote.clone();
        let Some(prev) = self.remote_keys.replace(next) else {
            return;
        };
        let added: Vec<Notification> = remote
            .into_iter()
            .filter(|n| !prev.contains(&n.key()) && !n.resolved)
            .collect();
        let mut duplicates = HashSet::new();
        for n in &added {
            if let Some(local) = self.match_remote(n, now) {
                if self.local.remove(&local).is_some() {
                    duplicates.insert(n.key());
                }
            }
        }
        let fresh: Vec<&Notification> = added.iter().filter(|n| !duplicates.contains(&n.key())).collect();
        for n in &fresh {
            if let Some(title) = event_title(&n.event) {
                if n.event == "disarm" {
                    self.last_disarm_reason_at = now;
                    if let Some((id, at)) = self.plain_disarm_toast.take() {
                        if now - at < DISARM_EVENT_WINDOW_MS {
                            self.toasts.retain(|t| t.id != id);
                        }
                    }
                }
                let lvl = n.level().to_string();
                self.toast(&lvl, &n.msg, title, &n.event, now);
            }
        }
        // A line the vehicle has resolved takes its toast down with it.
        let resolved: HashSet<&str> = self
            .remote
            .iter()
            .filter(|n| n.resolved)
            .map(|n| n.msg.as_str())
            .collect();
        if !resolved.is_empty() {
            self.toasts.retain(|t| !resolved.contains(t.text.as_str()));
            for r in &resolved {
                self.toasted_at.remove(*r);
            }
        }
        for n in fresh
            .iter()
            .filter(|n| event_title(&n.event).is_none() && n.level() != "info")
        {
            let lvl = n.level().to_string();
            self.toast(&lvl, &n.msg, "", "", now);
        }
    }

    fn milestones(&mut self, state: &Value, now: f64) {
        let connected = state.get("connected").is_some_and(|v| v.as_bool() == Some(true));
        let armed = state.get("armed").is_some_and(|v| v.as_bool() == Some(true));
        let boot = state.get("boot_ms").and_then(Value::as_f64).unwrap_or(0.0);
        let rebooted = self.prev_boot_ms > 0.0 && boot > 0.0 && boot < self.prev_boot_ms;
        if (self.prev_connected == Some(false) && connected) || rebooted {
            self.clear_all();
        } else if self.prev_armed.is_some_and(|p| p != armed) && connected && self.prev_connected == Some(true) {
            let before = if armed { f64::INFINITY } else { now - MILESTONE_GRACE_MS };
            self.mark_all_read(now, before);
            if !armed {
                self.plain_disarm(now);
            }
        }
        self.prev_connected = Some(connected);
        self.prev_armed = Some(armed);
        if boot > 0.0 {
            self.prev_boot_ms = boot;
        }
    }

    fn plain_disarm(&mut self, now: f64) {
        let since = now - self.last_disarm_reason_at;
        if (0.0..DISARM_EVENT_WINDOW_MS).contains(&since) {
            return;
        }
        let id = self.toast(
            "info",
            "The vehicle is disarmed. Motors are off",
            "Disarmed",
            "disarm",
            now,
        );
        self.plain_disarm_toast = id.map(|id| (id, now));
    }

    fn readiness(&mut self, state: &Value, now: f64) {
        let tone = readiness_tone(state);
        if tone != "ready" {
            self.ready_since = 0.0;
            self.ready_announced = false;
        } else if self.prev_ready.is_none() {
            self.ready_announced = true;
        } else if self.prev_ready.as_deref() != Some("ready") {
            self.ready_since = now;
            self.ready_from = self.prev_ready.clone();
        }
        self.prev_ready = Some(tone.to_string());
        if self.ready_announced || self.ready_since == 0.0 || now - self.ready_since < READY_HOLD_MS {
            return;
        }
        self.ready_announced = true;
        if matches!(self.ready_from.as_deref(), Some("armed") | Some("flying")) {
            return;
        }
        self.toast("info", "Preflight checks pass", "Ready to fly", "ready", now);
    }

    /// toastNotification and ui.toast: only warnings, criticals and vehicle
    /// events; the same text again after ten seconds at the earliest; three
    /// at most, a non-critical one going first.
    pub fn toast(&mut self, level: &str, message: &str, title: &str, event: &str, now: f64) -> Option<u64> {
        if level != "warning" && level != "critical" && event.is_empty() {
            return None;
        }
        if let Some(last) = self.toasted_at.get(message) {
            if now - last < TOAST_REPEAT_MS {
                return None;
            }
        }
        self.toasted_at.insert(message.to_string(), now);
        let level: &'static str = match level {
            "warning" => "warning",
            "critical" => "critical",
            _ => "info",
        };
        let title = if title.is_empty() {
            match level {
                "warning" => "Warning",
                "critical" => "Error",
                _ => "Info",
            }
            .to_string()
        } else {
            title.to_string()
        };
        let duration = match level {
            "warning" => 6000.0,
            "critical" => 20000.0,
            _ => 4500.0,
        };
        while self.toasts.len() >= TOAST_STACK_MAX {
            let victim = self.toasts.iter().position(|t| t.level != "critical").unwrap_or(0);
            self.toasts.remove(victim);
        }
        self.next_toast += 1;
        let id = self.next_toast;
        self.toasts.push(Toast {
            id,
            level,
            title,
            text: message.to_string(),
            at: now,
            duration,
        });
        Some(id)
    }

    pub fn close_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }
}

fn event_title(event: &str) -> Option<&'static str> {
    match event {
        "kill" | "unkill" => Some("Kill switch"),
        "disarm" => Some("Disarmed"),
        _ => None,
    }
}

/// The readiness block's tone, as topbar.js readiness() names it.
fn readiness_tone(state: &Value) -> &'static str {
    let b = topbar::readiness(state);
    match b.value.as_str() {
        "KILL SWITCH" | "TERMINATED" => "killed",
        "FLYING" => "flying",
        "ARMED" => "armed",
        "READY" => "ready",
        "NOT READY" => "notready",
        _ => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn remote(level: &str, msg: &str, meta: &str) -> Value {
        json!({"level": level, "msg": msg, "meta": meta})
    }

    #[test]
    fn keys_and_levels() {
        let n = Notification::from_remote(&remote("error", "Compass", "12:00"));
        assert_eq!(n.key(), "remote:error:Compass:12:00");
        assert_eq!(n.level(), "critical");
    }

    #[test]
    fn unread_then_read_then_dismissed() {
        let mut b = Board::default();
        let st = json!({"connected": true, "warnings": [remote("warning", "Low battery", "1"), remote("info", "Mode accepted", "2")]});
        b.update(&st, 0.0);
        let s = b.summary(0.0);
        assert_eq!((s.count, s.level), (2, "warning"));
        b.set_open(true, 1.0);
        b.set_open(false, 2.0);
        let s = b.summary(3.0);
        assert_eq!((s.count, s.level), (2, "read"), "closing the board marks it read");
        let s = b.summary(61000.0);
        assert_eq!(s.items.len(), 1, "info ages out after a minute");
        b.dismiss("remote:warning:Low battery:1");
        assert_eq!(b.summary(61001.0).level, "healthy");
    }

    #[test]
    fn a_fresh_link_clears_and_arming_marks_read() {
        let mut b = Board::default();
        b.update(&json!({"connected": false}), 0.0);
        b.add_local("critical", "Arm failed", 1.0);
        b.update(&json!({"connected": true, "armed": false}), 2.0);
        assert_eq!(b.summary(3.0).count, 0, "a fresh link clears");
        assert!(b.clear_remote);
        b.add_local("warning", "Something", 4.0);
        b.update(&json!({"connected": true, "armed": true}), 5.0);
        assert_eq!(b.summary(6.0).level, "read");
    }

    #[test]
    fn a_refused_command_and_the_vehicles_reason_are_one_line() {
        let mut b = Board::default();
        b.update(&json!({"connected": true, "warnings": []}), 0.0);
        let a = b.begin(&["arm"], 10.0);
        b.failed(a, "Arm failed", 20.0);
        assert_eq!(b.summary(21.0).items.len(), 1);
        b.update(
            &json!({"connected": true, "warnings": [remote("critical", "Arming denied: no GPS", "x")]}),
            100.0,
        );
        let s = b.summary(101.0);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].msg, "Arming denied: no GPS");
    }

    #[test]
    fn the_board_keeps_a_bounded_number_of_its_own_lines() {
        let mut b = Board::default();
        for i in 0..(LOCAL_MAX + 50) {
            b.add_local("critical", &format!("failure {i}"), i as f64);
        }
        assert_eq!(b.local.len(), LOCAL_MAX);
        let kept = b.visible(1.0);
        assert!(kept.iter().any(|n| n.msg == format!("failure {}", LOCAL_MAX + 49)));
        assert!(!kept.iter().any(|n| n.msg == "failure 0"));
    }

    #[test]
    fn commands_in_flight_and_toast_memories_are_bounded() {
        let mut b = Board::default();
        for _ in 0..(ATTEMPTS_MAX + 30) {
            b.begin(&["arm"], 0.0);
        }
        assert_eq!(b.attempts.len(), ATTEMPTS_MAX);
        for i in 0..500 {
            b.toast("warning", &format!("line {i}"), "", "", 0.0);
        }
        assert_eq!(b.toasted_at.len(), 500);
        b.update(&json!({}), TOAST_REPEAT_MS + 1.0);
        assert!(b.toasted_at.is_empty(), "a text past its repeat window is forgotten");
    }

    #[test]
    fn toasts_follow_ui_js() {
        let mut b = Board::default();
        assert!(b.toast("info", "plain", "", "", 0.0).is_none(), "info is not toasted");
        assert!(b.toast("warning", "a", "", "", 0.0).is_some());
        assert!(
            b.toast("warning", "a", "", "", 5000.0).is_none(),
            "the same text again within ten seconds"
        );
        b.toast("critical", "b", "", "", 1.0);
        b.toast("warning", "c", "", "", 2.0);
        b.toast("warning", "d", "", "", 3.0);
        assert_eq!(b.toasts.len(), 3);
        assert!(b.toasts.iter().any(|t| t.text == "b"), "the critical stays");
        assert_eq!(b.toasts.last().unwrap().title, "Warning");
        b.update(&json!({}), 6500.0);
        assert_eq!(
            b.toasts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["b"],
            "warnings go after six seconds"
        );
    }

    #[test]
    fn ready_to_fly_once_after_the_hold() {
        let mut b = Board::default();
        let notready = json!({"connected": true, "prearm_ok": false});
        let ready = json!({"connected": true, "prearm_ok": true});
        b.update(&notready, 0.0);
        b.update(&ready, 100.0);
        assert!(b.toasts.is_empty());
        b.update(&ready, 1700.0);
        assert_eq!(b.toasts[0].title, "Ready to fly");
    }

    #[test]
    fn actions_and_failures_as_the_dedupe_reads_them() {
        assert_eq!(action_from_message("Arming denied"), "arm");
        assert_eq!(action_from_message("Disarm failed"), "disarm");
        assert_eq!(action_from_message("Take-off rejected"), "takeoff");
        assert_eq!(action_from_message("Return to launch failed"), "rtl");
        assert_eq!(action_from_message("Mode HOLD rejected"), "mode");
        assert!(is_failure_message("Command timed out"));
        assert!(is_failure_message("no home position"));
        assert!(!is_failure_message("Mode accepted"));
    }
}
