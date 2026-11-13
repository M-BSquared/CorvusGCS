//! The MAVLink telemetry log (`tlog`): the writer and the bridge's session
//! handling around it.
//!
//! Every raw frame the bridge receives goes into a `.tlog` file so a flight
//! is attributable to the build that produced it. The file starts with one
//! metadata line (a magic prefix and JSON, ended by a newline) that a reader
//! skips in one read; raw frames follow verbatim. The receive loop never
//! blocks on disk: `write_frame` only appends to a bounded, drop oldest
//! queue (recent data survives a slow disk), and a writer thread drains it and
//! flushes. `stop` drains the rest, closes the file and joins the thread with a
//! bound, so a stuck disk cannot hang shutdown.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::clock::{local_datetime, utc_datetime};

/// A reader identifies the metadata line by this prefix.
pub const MAGIC: &[u8] = b"#CORVUS-TLOG ";
/// Frames kept in flight before the oldest are dropped.
pub const DEFAULT_MAX_QUEUE: usize = 10_000;
/// How long the writer waits for new frames before looking at the stop flag.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Python's `json.dumps` of a string: ASCII only, `\uXXXX` for the rest.
fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `datetime.now(UTC).isoformat()`.
fn iso_utc_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let t = utc_datetime(now);
    let base = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    if t.micro == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{:06}+00:00", t.micro)
    }
}

struct Shared {
    path: PathBuf,
    file: Mutex<Option<File>>,
    buf: Mutex<VecDeque<Vec<u8>>>,
    cond: Condvar,
    stop: AtomicBool,
    conn_str: Mutex<String>,
    header_written: AtomicBool,
    failed: AtomicBool,
    max_queue: usize,
    on_failure: Box<dyn Fn(&str) + Send + Sync>,
}

impl Shared {
    fn note_failure(&self, message: &str) {
        let already = self.failed.swap(true, Ordering::SeqCst);
        self.buf.lock().unwrap_or_else(|e| e.into_inner()).clear();
        if !already {
            (self.on_failure)(&format!("{message}: {}", self.path.display()));
        }
    }

    fn header(&self) -> Vec<u8> {
        let conn = self.conn_str.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let meta = format!(
            "{{\"product\": \"Corvus GCS\", \"version\": {}, \"conn\": {}, \"started_at\": {}, \"format\": \"mavlink-raw\"}}",
            json_string(crate::version::version()),
            json_string(&conn),
            json_string(&iso_utc_now())
        );
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(meta.as_bytes());
        out.push(b'\n');
        out
    }

    /// The metadata line, once.
    fn ensure_header(&self) -> std::io::Result<()> {
        if self.header_written.load(Ordering::SeqCst) {
            return Ok(());
        }
        let header = self.header();
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if self.header_written.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(file) = guard.as_mut() {
            file.write_all(&header)?;
            file.flush()?;
            self.header_written.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn write_batch(&self, batch: &[Vec<u8>]) -> std::io::Result<()> {
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let Some(file) = guard.as_mut() else { return Ok(()) };
        for raw in batch {
            file.write_all(raw)?;
        }
        file.flush()
    }

    fn drain_until_stopped(&self) -> std::io::Result<()> {
        while !self.stop.load(Ordering::SeqCst) {
            let batch: Vec<Vec<u8>> = {
                let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
                while buf.is_empty() && !self.stop.load(Ordering::SeqCst) {
                    let (guard, _) = self
                        .cond
                        .wait_timeout(buf, DRAIN_TIMEOUT)
                        .unwrap_or_else(|e| e.into_inner());
                    buf = guard;
                }
                if buf.is_empty() && self.stop.load(Ordering::SeqCst) {
                    break;
                }
                buf.drain(..).collect()
            };
            if !batch.is_empty() {
                self.ensure_header()?;
                self.write_batch(&batch)?;
            }
        }
        // Final drain on stop so no enqueued frame is lost.
        let batch: Vec<Vec<u8>> = self.buf.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        if !batch.is_empty() {
            self.ensure_header()?;
            self.write_batch(&batch)?;
        }
        // An empty tlog still carries its metadata line for attributability.
        self.ensure_header()
    }
}

/// A thread safe raw frame writer with a background drain thread.
pub struct TlogWriter {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl TlogWriter {
    /// Opens `path` for appending (creating its folder). `on_failure` is told
    /// once when a write fails, as a field laptop fills its disk mid flight.
    pub fn new(path: &Path, on_failure: impl Fn(&str) + Send + Sync + 'static) -> std::io::Result<TlogWriter> {
        TlogWriter::with_queue(path, DEFAULT_MAX_QUEUE, on_failure)
    }

    pub fn with_queue(
        path: &Path,
        max_queue: usize,
        on_failure: impl Fn(&str) + Send + Sync + 'static,
    ) -> std::io::Result<TlogWriter> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().append(true).create(true).open(path)?;
        let shared = Arc::new(Shared {
            path: path.to_path_buf(),
            file: Mutex::new(Some(file)),
            buf: Mutex::new(VecDeque::new()),
            cond: Condvar::new(),
            stop: AtomicBool::new(false),
            conn_str: Mutex::new(String::new()),
            header_written: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            max_queue: max_queue.max(1),
            on_failure: Box::new(on_failure),
        });
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new().name("tlog-writer".into()).spawn(move || {
            if worker.drain_until_stopped().is_err() {
                worker.note_failure("tlog writer stopped after a write failure");
            }
            if let Some(file) = worker.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                let _ = file.flush();
            }
        })?;
        Ok(TlogWriter {
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// Amends the `conn` field before the header is written; no-op after.
    pub fn set_conn(&self, conn: &str) {
        if self.shared.header_written.load(Ordering::SeqCst) {
            return;
        }
        *self.shared.conn_str.lock().unwrap_or_else(|e| e.into_inner()) = conn.to_string();
    }

    /// Enqueues one raw frame, never blocking on disk; the oldest buffered
    /// frame goes when the queue is full. A stopped writer discards frames.
    pub fn write_frame(&self, raw: &[u8]) {
        if self.shared.stop.load(Ordering::SeqCst) || self.shared.failed.load(Ordering::SeqCst) {
            return;
        }
        let mut buf = self.shared.buf.lock().unwrap_or_else(|e| e.into_inner());
        if buf.len() >= self.shared.max_queue {
            buf.pop_front();
        }
        buf.push_back(raw.to_vec());
        drop(buf);
        self.shared.cond.notify_one();
    }

    /// Drains the rest, flushes, closes and joins the writer, with a bound.
    /// Idempotent.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.cond.notify_all();
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(thread) = thread {
            let deadline = Instant::now() + JOIN_TIMEOUT;
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
        // Best effort final header, flush and close, so a failing disk cannot
        // skip the close and leak the descriptor.
        if self.shared.ensure_header().is_err() {
            self.shared.note_failure("tlog header write failed");
        }
        let mut guard = self.shared.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut file) = guard.take() {
            let _ = file.flush();
        }
    }
}

impl Drop for TlogWriter {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the tlog session handling asks of the bridge.
pub trait TlogHost: Send + Sync {
    /// Is the bridge running? A direct connect in a unit test is not.
    fn running(&self) -> bool;
    fn conn_string(&self) -> String;
    fn log_dir(&self) -> PathBuf;
    /// `%Y%m%d-%H%M%S-%f` of the local time now.
    fn stamp(&self) -> String {
        let t = local_datetime();
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}-{:06}",
            t.year, t.month, t.day, t.hour, t.minute, t.second, t.micro
        )
    }
    fn log(&self, level: &str, text: &str);
    /// Asks for AUTOPILOT_VERSION again, so a new file carries its own.
    fn request_version(&self);
}

/// The frame consumer next to the tlog (the forwarder); it gets the bytes
/// exactly as they came off the wire.
pub type FrameSink = Arc<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Default)]
struct Session {
    tlog: Option<Arc<TlogWriter>>,
    armed: bool,
    sink: Option<FrameSink>,
}

/// The tlog session of one bridge.
#[derive(Default)]
pub struct TlogSession {
    state: Mutex<Session>,
}

/// A tlog path in `log_dir` that is not already taken. The name is a
/// timestamp because that sorts. The clock is not trusted to be unique on its
/// own (Windows' ticks at about 1 ms, so two reconnects can land on one name
/// and the second would overwrite a flight's recorded telemetry): a taken name
/// gets a counter. The separator is "_" and not "-", so the names still sort
/// into session order: "-" (0x2D) sorts before "." (0x2E).
pub fn next_tlog_path(log_dir: &Path, stamp: &str) -> PathBuf {
    let mut path = log_dir.join(format!("{stamp}.tlog"));
    let mut n = 1;
    while path.exists() {
        path = log_dir.join(format!("{stamp}_{n:02}.tlog"));
        n += 1;
    }
    path
}

impl TlogSession {
    pub fn new() -> TlogSession {
        TlogSession::default()
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Session> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_active(&self) -> bool {
        self.st().tlog.is_some()
    }

    pub fn current_path(&self) -> Option<PathBuf> {
        self.st().tlog.as_ref().map(|t| t.path().to_path_buf())
    }

    pub fn set_frame_sink(&self, sink: Option<FrameSink>) {
        self.st().sink = sink;
    }

    /// Opens a fresh tlog for this flight session. A log folder error must
    /// never break the link: on failure the tlog is disabled for the session.
    pub fn start_tlog(&self, host: &Arc<dyn TlogHost>) {
        if !host.running() {
            return;
        }
        let dir = host.log_dir();
        let attempt = (|| -> std::io::Result<TlogWriter> {
            std::fs::create_dir_all(&dir)?;
            let path = next_tlog_path(&dir, &host.stamp());
            let reporter = Arc::clone(host);
            let writer = TlogWriter::new(&path, move |text| reporter.log("warning", text))?;
            writer.set_conn(&host.conn_string());
            Ok(writer)
        })();
        // The new file is opened before the old one is closed and swapped in
        // at once, so a frame or a path lookup during a roll never sees a gap.
        let previous = match attempt {
            Ok(writer) => self.st().tlog.replace(Arc::new(writer)),
            Err(e) => {
                host.log("warning", &format!("tlog disabled: {e}"));
                self.st().tlog.take()
            }
        };
        if let Some(previous) = previous {
            previous.stop();
        }
    }

    /// Starts the next tlog when the vehicle disarms after a flight. Two
    /// sorties on one battery swap used to share a file. The disarm heartbeat
    /// is already in the closing file (frames are logged before they are
    /// dispatched), so that file holds the whole flight.
    pub fn roll_tlog_on_disarm(&self, host: &Arc<dyn TlogHost>, armed: bool) {
        let (was_armed, has_tlog) = {
            let mut st = self.st();
            let was = st.armed;
            st.armed = armed;
            (was, st.tlog.is_some())
        };
        if !was_armed || armed || !has_tlog {
            return;
        }
        self.start_tlog(host);
        if self.is_active() {
            host.request_version();
        }
    }

    /// Stops and closes the current tlog; idempotent.
    pub fn stop_tlog(&self, _host: &dyn TlogHost) {
        let tlog = self.st().tlog.take();
        if let Some(tlog) = tlog {
            tlog.stop();
        }
    }

    /// Logs a raw frame if a tlog is active, and hands the same bytes to the
    /// forwarder when one is attached. Both are non blocking appends.
    pub fn write_tlog(&self, raw: Option<&[u8]>) {
        let (tlog, sink) = {
            let st = self.st();
            (st.tlog.clone(), st.sink.clone())
        };
        let Some(raw) = raw else { return };
        if let Some(tlog) = tlog {
            tlog.write_frame(raw);
        }
        if let Some(sink) = sink {
            sink(raw);
        }
    }
}
