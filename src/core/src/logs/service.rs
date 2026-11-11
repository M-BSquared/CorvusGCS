//! Flight log download: on-board logs over MAVLink, plus the local tlogs
//! (`log_service.py`).
//!
//! Downloads are sequential by design: the MAVLink log protocol has one session
//! per vehicle, so two downloads would interleave their LOG_DATA and corrupt
//! both files. A multi select becomes a queue on one worker thread.
//!
//! The worker is joined by [`LogService::shutdown`]. The sink that receives
//! LOG_ENTRY and LOG_DATA is attached only while a job runs and is always
//! detached when it ends.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::localtime;
use crate::clock::monotonic;
use crate::protocol::common::Event;

/// One LOG_ENTRY or LOG_DATA, as the service reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum LogMsg {
    Entry {
        id: i64,
        size: i64,
        utc: i64,
        num_logs: i64,
    },
    Data {
        id: i64,
        ofs: i64,
        count: i64,
        data: Vec<u8>,
    },
}

pub type LogSink = Arc<dyn Fn(&LogMsg) + Send + Sync>;

/// What the service needs of the vehicle link (the bridge's log methods).
pub trait LogLink: Send + Sync {
    fn is_connected(&self) -> bool;
    /// Routes LOG_ENTRY and LOG_DATA to `sink`; `None` detaches.
    fn set_log_sink(&self, sink: Option<LogSink>);
    fn request_log_list(&self, start: i64, end: i64) -> bool;
    fn request_log_data(&self, id: i64, offset: i64, count: i64) -> bool;
    fn log_request_end(&self) -> bool;
    fn erase_logs(&self) -> bool;
    fn last_command_error(&self) -> String;
    /// `.ulg` or `.bin`, by the connected stack.
    fn log_suffix(&self) -> &'static str;
}

/// One LOG_DATA carries 90 bytes.
pub const LOG_CHUNK_BYTES: usize = 90;
pub const REQUEST_SPAN_BYTES: usize = 90 * 80;
pub const LIST_ATTEMPTS: usize = 4;
pub const STALL_REPRIME_AFTER: u32 = 2;
pub const MAX_LOG_BYTES: i64 = 2 * 1024 * 1024 * 1024;
/// States in which the service owns the vehicle's single log session.
pub const BUSY_STATES: [&str; 3] = ["listing", "downloading", "erasing"];
pub const LOG_SUFFIXES: [&str; 2] = [".ulg", ".bin"];

/// The waits of the protocol, in seconds. A test shortens them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// No LOG_ENTRY for this long: the listing is done.
    pub list_timeout: f64,
    /// Still nothing: ask again.
    pub list_retry: f64,
    /// No LOG_DATA at all: re-request.
    pub data_stall_timeout: f64,
    /// The stream went quiet mid span: re-request.
    pub data_quiet: f64,
    /// No progress at all for this long: fail.
    pub download_give_up: f64,
    /// Wait for the session opening LOG_ENTRY.
    pub prime_timeout: f64,
    /// The vehicle needs a moment to clear the card.
    pub erase_settle: f64,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            list_timeout: 6.0,
            list_retry: 1.5,
            data_stall_timeout: 4.0,
            data_quiet: 0.7,
            download_give_up: 60.0,
            prime_timeout: 3.0,
            erase_settle: 2.0,
        }
    }
}

type Dir = Arc<dyn Fn() -> String + Send + Sync>;
type Listener = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Debug, Clone)]
struct Entry {
    id: i64,
    size: i64,
    utc: i64,
    num_logs: i64,
}

#[derive(Debug, Clone)]
struct Completed {
    id: i64,
    ok: bool,
    detail: String,
}

struct Inner {
    state: String,
    message: String,
    percent: i64,
    entries: Vec<Entry>,
    queue: Vec<i64>,
    current: Option<i64>,
    completed: Vec<Completed>,
    listeners: Vec<(u64, Listener)>,
    last_error: String,
}

/// Lists and downloads flight logs. One job at a time, always cancellable.
pub struct LogService {
    link: Arc<dyn LogLink>,
    log_dir: Dir,
    tlog_dir: Option<Dir>,
    timing: Timing,
    me: std::sync::Weak<LogService>,
    inner: Mutex<Inner>,
    cancel: Event,
    got_data: Arc<Event>,
    worker: Mutex<Option<JoinHandle<()>>>,
    next_listener: AtomicU64,
    shutting_down: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the OS says for an I/O error, without the "(os error N)" Rust adds
/// (`OSError.strerror`).
pub fn strerror(err: &std::io::Error) -> String {
    let full = err.to_string();
    match full.rfind(" (os error") {
        Some(i) => full[..i].to_string(),
        None => full,
    }
}

/// A filename fragment that cannot escape the target directory.
pub fn safe_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "log".to_string()
    } else {
        trimmed.to_string()
    }
}

/// When a tlog's recording opened, as an epoch timestamp: the stamp in the
/// recorder's name (this machine's local time), else `fallback`.
pub fn tlog_started(name: &str, fallback: f64) -> f64 {
    let b = name.as_bytes();
    let digits = |from: usize, n: usize| -> Option<i32> {
        let part = b.get(from..from + n)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (Some(y), Some(mo), Some(d)) = (digits(0, 4), digits(4, 2), digits(6, 2)) else {
        return fallback;
    };
    if b.get(8) != Some(&b'-') {
        return fallback;
    }
    let (Some(h), Some(mi), Some(s)) = (digits(9, 2), digits(11, 2), digits(13, 2)) else {
        return fallback;
    };
    let Some(mut stamp) = localtime::mktime(y, mo, d, h, mi, s) else {
        return fallback;
    };
    if b.get(15) == Some(&b'-') {
        let micro: String = name[16..].chars().take_while(char::is_ascii_digit).take(6).collect();
        if !micro.is_empty() {
            let padded = format!("{micro:0<6}");
            stamp += padded.parse::<f64>().unwrap_or(0.0) / 1_000_000.0;
        }
    }
    stamp
}

fn mtime_of(meta: &std::fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .map(|t| match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        })
        .unwrap_or(0.0)
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl LogService {
    pub fn new(link: Arc<dyn LogLink>, log_dir: Dir, tlog_dir: Option<Dir>) -> Arc<LogService> {
        LogService::with_timing(link, log_dir, tlog_dir, Timing::default())
    }

    pub fn with_timing(link: Arc<dyn LogLink>, log_dir: Dir, tlog_dir: Option<Dir>, timing: Timing) -> Arc<LogService> {
        Arc::new_cyclic(|me| LogService {
            link,
            log_dir,
            tlog_dir,
            timing,
            me: me.clone(),
            inner: Mutex::new(Inner {
                state: "idle".into(),
                message: String::new(),
                percent: 0,
                entries: Vec::new(),
                queue: Vec::new(),
                current: None,
                completed: Vec::new(),
                listeners: Vec::new(),
                last_error: String::new(),
            }),
            cancel: Event::new(),
            got_data: Arc::new(Event::new()),
            worker: Mutex::new(None),
            next_listener: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        })
    }

    /// Why the last refused call was refused (`last_error`).
    pub fn last_error(&self) -> String {
        lock(&self.inner).last_error.clone()
    }

    fn refuse(&self, text: &str) -> bool {
        lock(&self.inner).last_error = text.to_string();
        false
    }

    // ------------------------------------------------------------- status

    /// Everything the Analysis page renders, in one snapshot.
    pub fn status(&self) -> Value {
        let (mut entries, state, message, percent, current, queued, completed) = {
            let g = lock(&self.inner);
            (
                g.entries.clone(),
                g.state.clone(),
                g.message.clone(),
                g.percent,
                g.current,
                g.queue.clone(),
                g.completed.clone(),
            )
        };
        entries.sort_by_key(|e| std::cmp::Reverse(e.id));
        let saved = self.saved_logs();
        let rows: Vec<Value> = entries
            .iter()
            .map(|e| {
                // Same id AND same size: the card recycles ids, so an id alone
                // would claim last month's log as this flight's.
                let found = saved
                    .iter()
                    .find(|f| f["id"].as_i64() == Some(e.id))
                    .filter(|f| f["size"].as_i64() == Some(e.size));
                json!({
                    "id": e.id,
                    "size": e.size,
                    "utc": e.utc,
                    "num_logs": e.num_logs,
                    "downloaded": found.is_some(),
                    "file": found.map_or(String::new(), |f| f["path"].as_str().unwrap_or("").to_string()),
                    "file_name": found.map_or(String::new(), |f| f["name"].as_str().unwrap_or("").to_string()),
                })
            })
            .collect();
        json!({
            "state": state,
            "message": message,
            "percent": percent,
            "current": current,
            "queued": queued,
            "completed": completed
                .iter()
                .map(|c| json!({"id": c.id, "ok": c.ok, "detail": c.detail}))
                .collect::<Vec<_>>(),
            "logs": rows,
            "saved": saved,
            "tlogs": self.local_tlogs(),
            "dir": self.resolve_dir(),
            "connected": self.link.is_connected(),
        })
    }

    fn log_suffix(&self) -> &'static str {
        let suffix = self.link.log_suffix();
        if LOG_SUFFIXES.contains(&suffix) {
            suffix
        } else {
            ".ulg"
        }
    }

    /// On-board logs already in the download folder, newest first. The id is
    /// recovered from the `log_<id>_<date>.<ext>` name; a file that does not
    /// follow it is listed with no id.
    pub fn saved_logs(&self) -> Vec<Value> {
        let directory = self.resolve_dir();
        if directory.is_empty() {
            return Vec::new();
        }
        let Ok(names) = std::fs::read_dir(&directory) else {
            return Vec::new();
        };
        let mut out: Vec<(f64, Value)> = Vec::new();
        for item in names.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if !LOG_SUFFIXES.iter().any(|s| name.ends_with(s)) {
                continue;
            }
            let path = Path::new(&directory).join(&name);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let id = name.strip_prefix("log_").and_then(|rest| {
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                digits.parse::<i64>().ok().filter(|_| !digits.is_empty())
            });
            let mtime = mtime_of(&meta);
            out.push((
                mtime,
                json!({
                    "name": name,
                    "id": id,
                    "size": meta.len(),
                    "mtime": mtime,
                    "path": path_text(&path),
                }),
            ));
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out.into_iter().map(|(_, v)| v).collect()
    }

    fn resolve_dir(&self) -> String {
        (self.log_dir)()
    }

    /// Where the recordings live, or an empty string when nothing is configured.
    pub fn resolve_tlog_dir(&self) -> String {
        self.tlog_dir.as_ref().map(|f| f()).unwrap_or_default()
    }

    /// tlogs recorded on this laptop, newest session first. `started` is when
    /// the recording opened (from the recorder's name), `mtime` its last frame.
    pub fn local_tlogs(&self) -> Vec<Value> {
        let directory = self.resolve_tlog_dir();
        if directory.is_empty() {
            return Vec::new();
        }
        let Ok(names) = std::fs::read_dir(&directory) else {
            return Vec::new();
        };
        let mut out: Vec<(f64, Value)> = Vec::new();
        for item in names.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".tlog") {
                continue;
            }
            let path = Path::new(&directory).join(&name);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let mtime = mtime_of(&meta);
            let started = tlog_started(&name, mtime);
            out.push((
                started,
                json!({
                    "name": name,
                    "size": meta.len(),
                    "started": started,
                    "mtime": mtime,
                    "path": path_text(&path),
                }),
            ));
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out.into_iter().map(|(_, v)| v).collect()
    }

    // ------------------------------------------------------------ listing

    /// Asks the vehicle what logs it has. False on refusal.
    pub fn refresh(&self) -> bool {
        if !self.link.is_connected() {
            return self.refuse("not connected");
        }
        {
            let mut g = lock(&self.inner);
            if BUSY_STATES.contains(&g.state.as_str()) {
                g.last_error = "a log operation is already running".into();
                return false;
            }
        }
        self.cancel.clear();
        self.set("listing", 0, "Asking the vehicle for its logs…");
        self.spawn("log-list", |me| me.run_list());
        true
    }

    fn spawn(&self, name: &str, job: impl FnOnce(Arc<LogService>) + Send + 'static) {
        let Some(me) = self.me.upgrade() else {
            return;
        };
        let handle = std::thread::Builder::new().name(name.into()).spawn(move || job(me));
        if let Ok(handle) = handle {
            *lock(&self.worker) = Some(handle);
        }
    }

    fn run_list(&self) {
        let seen: Arc<Mutex<Vec<Entry>>> = Arc::default();
        // Every id the vehicle named, empty slots included: PX4's num_logs
        // covers the whole directory, so the completion test counts those too.
        let heard: Arc<Mutex<std::collections::BTreeSet<i64>>> = Arc::default();
        let expected = Arc::new(Mutex::new(0i64));
        let last_seen = Arc::new(Mutex::new(0.0f64));
        let answered = Arc::new(Event::new());

        let sink: LogSink = {
            let (seen, heard, expected, last_seen, answered) = (
                Arc::clone(&seen),
                Arc::clone(&heard),
                Arc::clone(&expected),
                Arc::clone(&last_seen),
                Arc::clone(&answered),
            );
            Arc::new(move |msg: &LogMsg| {
                let LogMsg::Entry {
                    id,
                    size,
                    utc,
                    num_logs,
                } = msg
                else {
                    return;
                };
                *lock(&last_seen) = monotonic();
                answered.set();
                lock(&heard).insert(*id);
                if *num_logs != 0 {
                    *lock(&expected) = *num_logs;
                }
                // An empty slot is not something the operator can act on.
                if *size <= 0 {
                    return;
                }
                let expected_now = *lock(&expected);
                let mut seen = lock(&seen);
                let entry = Entry {
                    id: *id,
                    size: *size,
                    utc: *utc,
                    num_logs: expected_now,
                };
                match seen.iter_mut().find(|e| e.id == *id) {
                    Some(slot) => *slot = entry,
                    None => seen.push(entry),
                }
            })
        };

        let mut attempts = 0usize;
        self.link.set_log_sink(Some(sink));
        let outcome = (|| {
            while !self.cancel.is_set() {
                if *lock(&last_seen) == 0.0 {
                    // LOG_REQUEST_LIST is unacknowledged, so a dropped request
                    // looks like an empty card. Ask again.
                    if attempts >= LIST_ATTEMPTS {
                        break;
                    }
                    if !self.link.request_log_list(0, 0xFFFF) {
                        return Err(());
                    }
                    attempts += 1;
                    let deadline = monotonic() + self.timing.list_retry;
                    while monotonic() < deadline && !self.cancel.is_set() {
                        if answered.wait(0.05) {
                            break;
                        }
                    }
                    continue;
                }
                let exp = *lock(&expected);
                if exp != 0 && lock(&heard).len() as i64 >= exp {
                    break;
                }
                if monotonic() - *lock(&last_seen) > self.timing.list_timeout {
                    break;
                }
                self.cancel.wait(0.1);
            }
            Ok(())
        })();
        // No log_request_end here: it would close the session a download needs.
        self.link.set_log_sink(None);
        if outcome.is_err() {
            self.set("failed", 0, "could not ask the vehicle for its logs");
            return;
        }

        lock(&self.inner).entries = lock(&seen).clone();
        if self.cancel.is_set() {
            self.set("cancelled", 0, "Listing cancelled");
            return;
        }
        let count = lock(&self.inner).entries.len();
        if count == 0 {
            self.set("idle", 0, "No logs on the vehicle");
        } else {
            self.set("idle", 0, &format!("{count} log(s) on the vehicle"));
        }
    }

    /// Re-opens the vehicle's log session and confirms it with a LOG_ENTRY.
    fn prime_session(&self) -> bool {
        let heard = Arc::new(Event::new());
        let sink: LogSink = {
            let heard = Arc::clone(&heard);
            Arc::new(move |msg: &LogMsg| {
                if matches!(msg, LogMsg::Entry { .. }) {
                    heard.set();
                }
            })
        };
        self.link.set_log_sink(Some(sink));
        let result = (|| {
            for _ in 0..LIST_ATTEMPTS {
                if self.cancel.is_set() {
                    return false;
                }
                if !self.link.request_log_list(0, 0xFFFF) {
                    return false;
                }
                let deadline = monotonic() + self.timing.prime_timeout;
                while monotonic() < deadline {
                    if heard.is_set() {
                        return true;
                    }
                    if self.cancel.is_set() {
                        return false;
                    }
                    heard.wait(0.1);
                }
            }
            heard.is_set()
        })();
        self.link.set_log_sink(None);
        result
    }

    // ----------------------------------------------------------- download

    /// Queues one or more logs and downloads them one after another.
    pub fn start_download(&self, ids: &[i64]) -> bool {
        if !self.link.is_connected() {
            return self.refuse("not connected");
        }
        let wanted: Vec<i64> = {
            let mut g = lock(&self.inner);
            if BUSY_STATES.contains(&g.state.as_str()) {
                g.last_error = "a log operation is already running".into();
                return false;
            }
            let wanted: Vec<i64> = ids
                .iter()
                .copied()
                .filter(|i| g.entries.iter().any(|e| e.id == *i))
                .collect();
            if wanted.is_empty() {
                g.last_error = "no known log selected".into();
                return false;
            }
            g.queue = wanted.clone();
            g.completed.clear();
            wanted
        };
        let directory = self.resolve_dir();
        if directory.is_empty() {
            return self.refuse("no download folder configured");
        }
        if let Err(e) = std::fs::create_dir_all(&directory) {
            return self.refuse(&format!("cannot use the download folder ({})", strerror(&e)));
        }
        self.cancel.clear();
        self.set("downloading", 0, &format!("Downloading {} log(s)…", wanted.len()));
        self.spawn("log-download", |me| me.run_download());
        true
    }

    fn run_download(&self) {
        let directory = self.resolve_dir();
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.download_queue(&directory)));
        self.link.set_log_sink(None);
        self.link.log_request_end();
        lock(&self.inner).current = None;
        match crashed {
            Ok(true) => {}
            Ok(false) => return,
            Err(_) => {
                let percent = lock(&self.inner).percent;
                self.set("failed", percent, "log download failed unexpectedly");
                return;
            }
        }
        let (done, failed) = {
            let g = lock(&self.inner);
            (
                g.completed.iter().filter(|c| c.ok).count(),
                g.completed.iter().filter(|c| !c.ok).count(),
            )
        };
        if self.cancel.is_set() {
            let percent = lock(&self.inner).percent;
            self.set("cancelled", percent, &format!("Cancelled, {done} log(s) saved"));
        } else if failed > 0 {
            self.set("failed", 100, &format!("{done} saved, {failed} failed"));
        } else {
            self.set("done", 100, &format!("{done} log(s) saved to {directory}"));
        }
    }

    /// False when the session could not be opened (the state is already set).
    fn download_queue(&self, directory: &str) -> bool {
        // Open the session before asking for a byte: the listing that filled
        // the picker may be minutes old.
        if !self.prime_session() {
            if self.cancel.is_set() {
                // The Python left the state at "downloading" here for good.
                return true;
            }
            self.set("failed", 0, "the vehicle would not open a log session");
            return false;
        }
        while !self.cancel.is_set() {
            let (log_id, entry, remaining) = {
                let mut g = lock(&self.inner);
                if g.queue.is_empty() {
                    break;
                }
                let log_id = g.queue.remove(0);
                g.current = Some(log_id);
                let entry = g.entries.iter().find(|e| e.id == log_id).cloned();
                (log_id, entry, g.queue.len())
            };
            let Some(entry) = entry else {
                continue;
            };
            let (ok, detail) = self.download_one(log_id, &entry, directory, remaining);
            lock(&self.inner).completed.push(Completed { id: log_id, ok, detail });
        }
        true
    }

    fn download_one(&self, log_id: i64, entry: &Entry, directory: &str, remaining: usize) -> (bool, String) {
        let size = entry.size;
        if size <= 0 || size > MAX_LOG_BYTES {
            return (false, "implausible log size".into());
        }
        let size = size as usize;
        let name = self.filename(log_id, entry.utc);
        let path = Path::new(directory).join(&name);

        struct Assembly {
            buffer: Vec<u8>,
            // 0 = still missing, 1 = held
            received: Vec<u8>,
            flow_at: f64,
        }
        let asm = Arc::new(Mutex::new(Assembly {
            buffer: vec![0u8; size],
            received: vec![0u8; size],
            flow_at: 0.0,
        }));
        let sink: LogSink = {
            let asm = Arc::clone(&asm);
            let got = Arc::clone(&self.got_data);
            Arc::new(move |msg: &LogMsg| {
                let LogMsg::Data { id, ofs, count, data } = msg else {
                    return;
                };
                if *id != log_id || *ofs < 0 {
                    return;
                }
                let offset = *ofs as usize;
                let take = (*count).clamp(0, data.len() as i64) as usize;
                let end = (offset + take).min(size);
                if offset >= size || end <= offset {
                    return;
                }
                {
                    let mut a = lock(&asm);
                    a.buffer[offset..end].copy_from_slice(&data[..end - offset]);
                    a.received[offset..end].fill(1);
                    a.flow_at = monotonic();
                }
                got.set();
            })
        };
        let missing = |from: usize, to: usize| -> bool { lock(&asm).received[from..to].contains(&0) };
        let flow = || lock(&asm).flow_at;

        self.link.set_log_sink(Some(sink.clone()));
        let t = self.timing;
        let outcome: Result<(), String> = (|| {
            let mut last_progress = monotonic();
            let mut stalls = 0u32;
            let mut reprimes_left = 1;
            let mut offset = 0usize;
            while offset < size {
                if self.cancel.is_set() {
                    return Err("cancelled".into());
                }
                self.got_data.clear();
                let span = REQUEST_SPAN_BYTES.min(size - offset);
                let before = flow();
                if !self.link.request_log_data(log_id, offset as i64, span as i64) {
                    return Err("link lost".into());
                }
                // Wait for the span, then advance past every byte now held.
                let deadline = monotonic() + t.data_stall_timeout;
                while monotonic() < deadline {
                    if self.cancel.is_set() {
                        return Err("cancelled".into());
                    }
                    if !missing(offset, offset + span) {
                        break;
                    }
                    self.got_data.wait(0.1);
                    self.got_data.clear();
                    // A span arrives in one burst: once packets stop, the
                    // missing ones are lost, not late.
                    let quiet = flow();
                    if quiet > before && monotonic() - quiet > t.data_quiet {
                        break;
                    }
                }
                let next = lock(&asm).received[offset..]
                    .iter()
                    .position(|b| *b == 0)
                    .map_or(size, |p| offset + p);
                if next > offset {
                    offset = next;
                    stalls = 0;
                    last_progress = monotonic();
                    let total = size.max(1);
                    let queued = if remaining > 0 {
                        format!(" (+{remaining} queued)")
                    } else {
                        String::new()
                    };
                    self.set(
                        "downloading",
                        (offset * 100 / total) as i64,
                        &format!("{name}: {}/{} KB{queued}", offset / 1024, size / 1024),
                    );
                    continue;
                }
                stalls += 1;
                // Not one byte came back: the request was lost or the vehicle
                // dropped the session. Re-open it once before waiting a minute.
                if stalls >= STALL_REPRIME_AFTER && reprimes_left > 0 {
                    reprimes_left -= 1;
                    self.link.set_log_sink(None);
                    let reopened = self.prime_session();
                    self.link.set_log_sink(Some(sink.clone()));
                    if !reopened {
                        return Err("the vehicle closed its log session".into());
                    }
                    stalls = 0;
                    last_progress = monotonic();
                    continue;
                }
                if monotonic() - last_progress > t.download_give_up {
                    return Err("the vehicle stopped sending log data".into());
                }
            }
            Ok(())
        })();
        self.link.set_log_sink(None);
        if let Err(detail) = outcome {
            return (false, detail);
        }
        let payload = std::mem::take(&mut lock(&asm).buffer);

        let tmp = {
            let mut s = path.clone().into_os_string();
            s.push(".part");
            std::path::PathBuf::from(s)
        };
        let written = std::fs::write(&tmp, &payload).and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            let full = e.raw_os_error() == Some(if cfg!(windows) { 112 } else { 28 });
            if full {
                return (false, "the download folder is full".into());
            }
            return (false, format!("could not write {name} ({})", strerror(&e)));
        }
        (true, path_text(&path))
    }

    /// `log_<id>_<local date>.<ext>`: sortable, readable a month later. The
    /// stamp is this machine's local time, matching the tlog names.
    pub fn filename(&self, log_id: i64, utc: i64) -> String {
        let mut stamp = String::new();
        if utc > 0 {
            if let Some(local) = localtime::localtime(utc) {
                stamp = format!("_{}", local.stamp());
            }
        }
        format!(
            "{}{}",
            safe_component(&format!("log_{log_id:03}{stamp}")),
            self.log_suffix()
        )
    }

    // -------------------------------------------------------------- erase

    /// Erases every on-board log. False on refusal. MAVLink has no per log
    /// delete, so this is all or nothing by protocol.
    pub fn erase(&self) -> bool {
        if !self.link.is_connected() {
            return self.refuse("not connected");
        }
        {
            let mut g = lock(&self.inner);
            if BUSY_STATES.contains(&g.state.as_str()) {
                g.last_error = "a log operation is already running".into();
                return false;
            }
        }
        self.cancel.clear();
        self.set("erasing", 0, "Erasing all logs on the vehicle…");
        self.spawn("log-erase", |me| me.run_erase());
        true
    }

    fn run_erase(&self) {
        if !self.link.erase_logs() {
            let error = self.link.last_command_error();
            let text = if error.is_empty() {
                "the vehicle refused to erase its logs".to_string()
            } else {
                error
            };
            self.set("failed", 0, &text);
            return;
        }
        {
            let mut g = lock(&self.inner);
            g.entries.clear();
            g.completed.clear();
        }
        // Give the card time to clear, then ask again: the answer is what the
        // operator sees, not our optimism.
        self.cancel.wait(self.timing.erase_settle);
        lock(&self.inner).state = "listing".into();
        self.run_list();
    }

    // ----------------------------------------------- cancel and shutdown

    pub fn cancel(&self) -> bool {
        {
            let mut g = lock(&self.inner);
            if !BUSY_STATES.contains(&g.state.as_str()) {
                return false;
            }
            g.queue.clear();
        }
        self.cancel.set();
        self.got_data.set();
        true
    }

    /// Idempotent teardown. Never leaves the sink attached.
    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.cancel.set();
        self.got_data.set();
        let worker = lock(&self.worker).take();
        if let Some(handle) = worker {
            let limit = Instant::now() + Duration::from_secs(3);
            while !handle.is_finished() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        self.link.set_log_sink(None);
        lock(&self.inner).state = "idle".into();
    }

    pub fn add_listener(&self, f: impl Fn(&Value) + Send + Sync + 'static) -> u64 {
        let id = self.next_listener.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner).listeners.push((id, Arc::new(f)));
        id
    }

    pub fn remove_listener(&self, id: u64) {
        lock(&self.inner).listeners.retain(|(i, _)| *i != id);
    }

    fn set(&self, state: &str, percent: i64, message: &str) {
        let (listeners, current, queued) = {
            let mut g = lock(&self.inner);
            g.state = state.into();
            g.percent = percent;
            g.message = message.into();
            (
                g.listeners.iter().map(|(_, f)| Arc::clone(f)).collect::<Vec<_>>(),
                g.current,
                g.queue.len(),
            )
        };
        let mut entry = Map::new();
        entry.insert("state".into(), json!(state));
        entry.insert("percent".into(), json!(percent));
        entry.insert("message".into(), json!(message));
        entry.insert("current".into(), json!(current));
        entry.insert("queued".into(), json!(queued));
        let entry = Value::Object(entry);
        for f in listeners {
            f(&entry);
        }
    }
}
