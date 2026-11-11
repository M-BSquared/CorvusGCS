//! One RTSP camera: an ffmpeg process while somebody is watching, none
//! otherwise (`VideoStream` in `video.py`).
//!
//! A supervisor thread owns the process. It starts it on the first
//! [`VideoStream::demand`], restarts it with a backoff when it exits or goes
//! silent, and stops it once nobody has asked for a frame in the idle time.
//! Each supervisor carries a generation number, so a process being torn down
//! can never publish a frame or a state into the one that replaced it.

use std::collections::VecDeque;
use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::ffmpeg::{self, JpegSplitter, MAX_FRAME_BYTES};
use super::model::*;

/// The waits of the video code. The defaults are the Python's; a test shortens
/// them.
#[derive(Debug, Clone)]
pub struct Timing {
    pub idle_stop: Duration,
    pub stall: Duration,
    pub watchdog: Duration,
    pub retry_backoff: Vec<Duration>,
    pub frame_wait: Duration,
    pub term_timeout: Duration,
    pub list_keep: Duration,
    pub resolve_retry: Duration,
    pub whep_timeout: Duration,
    pub whep_close_timeout: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            idle_stop: Duration::from_secs(5),
            stall: Duration::from_secs(3),
            watchdog: Duration::from_secs(10),
            retry_backoff: [1.0, 2.0, 4.0, 8.0]
                .iter()
                .map(|s| Duration::from_secs_f64(*s))
                .collect(),
            frame_wait: Duration::from_secs(2),
            term_timeout: Duration::from_secs(2),
            list_keep: Duration::from_secs(5),
            resolve_retry: Duration::from_secs(5),
            whep_timeout: Duration::from_secs(10),
            whep_close_timeout: Duration::from_secs(3),
            poll: Duration::from_millis(250),
        }
    }
}

const MAX_ERROR_LINE: usize = 4096;

/// Frame numbers are drawn from one counter for the whole process, so a camera
/// rebuilt under the same id still numbers its first frame above anything a
/// window open on it has already seen.
static FRAME_SEQ: AtomicU64 = AtomicU64::new(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the streams need from the service: the ffmpeg to run and a private
/// folder for the address lists.
pub trait Decoder: Send + Sync {
    fn decoder(&self) -> (String, String);
    fn workdir(&self) -> std::io::Result<PathBuf>;
    fn timing(&self) -> &Timing;
}

/// A thread that can be waited for with a limit; one that does not end in time
/// is left to end by itself.
pub struct Task {
    handle: Mutex<Option<JoinHandle<()>>>,
    done: Arc<(Mutex<bool>, Condvar)>,
}

impl Task {
    pub fn spawn<F: FnOnce() + Send + 'static>(name: String, f: F) -> Task {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let d = Arc::clone(&done);
        let handle = std::thread::Builder::new().name(name).spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            *lock(&d.0) = true;
            d.1.notify_all();
        });
        Task {
            handle: Mutex::new(handle.ok()),
            done,
        }
    }

    pub fn finished(&self) -> bool {
        *lock(&self.done.0)
    }

    pub fn join(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        let mut done = lock(&self.done.0);
        while !*done {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            done = self
                .done
                .1
                .wait_timeout(done, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(done);
        if let Some(h) = lock(&self.handle).take() {
            let _ = h.join();
        }
        true
    }
}

struct Inner {
    config: Value,
    thread: Option<Arc<Task>>,
    gen: u64,
    runs: u64,
    last_demand: Option<Instant>,
    state: &'static str,
    message: String,
    frame: Option<Arc<Vec<u8>>>,
    seq: u64,
    frame_at: Option<Instant>,
    size: Option<(u32, u32)>,
    fps: f64,
    proc: Option<Arc<Mutex<Child>>>,
}

pub struct VideoStream {
    inner: Mutex<Inner>,
    cond: Condvar,
    stop: AtomicBool,
    env: Arc<dyn Decoder>,
}

pub struct FrameStatus {
    pub state: &'static str,
    pub message: String,
    pub seq: u64,
}

fn signal_term(child: &Arc<Mutex<Child>>) {
    let mut c = lock(child);
    if matches!(c.try_wait(), Ok(None)) {
        #[cfg(unix)]
        // SAFETY: a signal to a child this process started; failure is ignored.
        unsafe {
            libc::kill(c.id() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = c.kill();
    }
}

/// SIGTERM, then SIGKILL after the term timeout. Never panics.
fn terminate(child: &Arc<Mutex<Child>>, term: Duration) {
    signal_term(child);
    let wait = |limit: Duration| -> bool {
        let end = Instant::now() + limit;
        loop {
            match lock(child).try_wait() {
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) => {}
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    if wait(term) {
        return;
    }
    let _ = lock(child).kill();
    wait(term);
}

impl VideoStream {
    pub fn new(config: Value, env: Arc<dyn Decoder>) -> Arc<VideoStream> {
        Arc::new(VideoStream {
            inner: Mutex::new(Inner {
                config,
                thread: None,
                gen: 0,
                runs: 0,
                last_demand: None,
                state: STATE_IDLE,
                message: String::new(),
                frame: None,
                seq: 0,
                frame_at: None,
                size: None,
                fps: 0.0,
                proc: None,
            }),
            cond: Condvar::new(),
            stop: AtomicBool::new(false),
            env,
        })
    }

    pub fn config(&self) -> Value {
        lock(&self.inner).config.clone()
    }

    pub fn set_config(&self, config: Value) {
        lock(&self.inner).config = config;
    }

    fn cfg_str(&self, key: &str) -> String {
        lock(&self.inner)
            .config
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    fn open_url(&self) -> String {
        with_credentials(
            &self.cfg_str("url"),
            &self.cfg_str("username"),
            &self.cfg_str("password"),
        )
    }

    fn scrub(&self, text: &str, list_path: &str) -> String {
        let mut out = text.replace(&self.open_url(), &self.cfg_str("url"));
        if !list_path.is_empty() {
            out = out.replace(list_path, &display_address(&self.cfg_str("url")));
        }
        redact(&out, &self.cfg_str("password"))
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Waits up to `d`; true when the stream was stopped meanwhile.
    fn wait_stop(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        let mut g = lock(&self.inner);
        loop {
            if self.stopped() {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            g = self.cond.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Somebody wants frames: keep the decoder running, start it if needed.
    pub fn demand(self: &Arc<Self>) {
        let mut g = lock(&self.inner);
        g.last_demand = Some(Instant::now());
        if self.stopped() {
            return;
        }
        if g.thread.as_ref().is_some_and(|t| !t.finished()) {
            return;
        }
        g.gen += 1;
        let gen = g.gen;
        if g.state == STATE_IDLE || g.state == STATE_UNAVAILABLE {
            g.state = STATE_STARTING;
            g.message = "Connecting to the camera.".into();
        }
        let me = Arc::clone(self);
        let id = g.config.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        g.thread = Some(Arc::new(Task::spawn(format!("corvus-video-{id}"), move || {
            me.supervise(gen)
        })));
    }

    /// The newest frame once its number is above `after`, or `None` on timeout.
    pub fn wait_frame(
        &self,
        after: u64,
        timeout: Duration,
        stopping: &dyn Fn() -> bool,
    ) -> Option<(u64, Arc<Vec<u8>>)> {
        let end = Instant::now() + timeout;
        let mut g = lock(&self.inner);
        loop {
            if g.seq > after {
                if let Some(f) = &g.frame {
                    return Some((g.seq, Arc::clone(f)));
                }
            }
            if stopping() {
                return None;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() || self.stopped() {
                return None;
            }
            g = self
                .cond
                .wait_timeout(g, left.min(Duration::from_millis(250)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// State, the last message, and what the picture looks like.
    pub fn status(&self) -> Value {
        let g = lock(&self.inner);
        let now = Instant::now();
        let stall = self.env.timing().stall;
        let mut state = g.state;
        if state == STATE_LIVE && g.frame_at.is_some_and(|t| now.duration_since(t) > stall) {
            state = STATE_STALLED;
        }
        let age = g.frame_at.map(|t| now.duration_since(t).as_secs_f64());
        let message = if state != STATE_STALLED {
            g.message.clone()
        } else {
            format!("No picture for {} s.", age.unwrap_or(0.0) as i64)
        };
        json!({
            "state": state,
            "message": message,
            "seq": g.seq,
            "width": g.size.map(|s| s.0).unwrap_or(0),
            "height": g.size.map(|s| s.1).unwrap_or(0),
            "fps": (g.fps * 10.0).round() / 10.0,
            "age": age.map(|a| (a * 10.0).round() / 10.0),
            "running": g.thread.as_ref().is_some_and(|t| !t.finished()),
        })
    }

    pub fn frame_status(&self) -> FrameStatus {
        let st = self.status();
        FrameStatus {
            state: match st["state"].as_str().unwrap_or(STATE_IDLE) {
                STATE_STARTING => STATE_STARTING,
                STATE_LIVE => STATE_LIVE,
                STATE_STALLED => STATE_STALLED,
                STATE_ERROR => STATE_ERROR,
                STATE_UNAVAILABLE => STATE_UNAVAILABLE,
                _ => STATE_IDLE,
            },
            message: st["message"].as_str().unwrap_or("").to_string(),
            seq: st["seq"].as_u64().unwrap_or(0),
        }
    }

    /// Stops the decoder for good and, with `join`, waits for its thread.
    pub fn stop(&self, join: bool) {
        self.stop.store(true, Ordering::SeqCst);
        let (proc, thread) = {
            let g = lock(&self.inner);
            self.cond.notify_all();
            (g.proc.clone(), g.thread.clone())
        };
        if let Some(p) = proc {
            signal_term(&p);
        }
        if join {
            if let Some(t) = thread {
                t.join(self.env.timing().term_timeout * 2 + Duration::from_secs(2));
            }
        }
    }

    /// Waits for a stream `stop` was called on without `join`.
    pub fn join(&self, timeout: Duration) {
        let t = lock(&self.inner).thread.clone();
        if let Some(t) = t {
            t.join(timeout);
        }
    }

    pub fn alive(&self) -> bool {
        lock(&self.inner).thread.as_ref().is_some_and(|t| !t.finished())
    }

    fn set(&self, gen: u64, state: &'static str, message: &str) {
        let mut g = lock(&self.inner);
        if gen == g.gen {
            g.state = state;
            g.message = message.to_string();
            self.cond.notify_all();
        }
    }

    fn publish(&self, gen: u64, frame: Vec<u8>) {
        let now = Instant::now();
        let mut g = lock(&self.inner);
        if gen != g.gen {
            return;
        }
        if let Some(prev) = g.frame_at {
            let dt = now.duration_since(prev).as_secs_f64();
            if dt > 0.0 {
                let rate = 1.0 / dt;
                g.fps = if g.fps == 0.0 { rate } else { g.fps * 0.9 + rate * 0.1 };
            }
        }
        if g.size.is_none() || g.seq.is_multiple_of(50) {
            g.size = ffmpeg::jpeg_size(&frame).or(g.size);
        }
        g.frame = Some(Arc::new(frame));
        g.seq = FRAME_SEQ.fetch_add(1, Ordering::SeqCst);
        g.frame_at = Some(now);
        g.state = STATE_LIVE;
        g.message.clear();
        self.cond.notify_all();
    }

    fn idle(&self) -> bool {
        let g = lock(&self.inner);
        g.last_demand.is_none_or(|t| t.elapsed() > self.env.timing().idle_stop)
    }

    fn supervise(self: &Arc<Self>, gen: u64) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut failures = 0usize;
            while !self.stopped() {
                let (ffmpeg, version) = self.env.decoder();
                if ffmpeg.is_empty() {
                    self.set(
                        gen,
                        STATE_UNAVAILABLE,
                        "ffmpeg was not found. Video needs it to decode the camera.",
                    );
                    return;
                }
                let ran_live = self.run_once(gen, &ffmpeg, &version);
                if self.stopped() || self.idle() {
                    return;
                }
                failures = if ran_live { 0 } else { failures + 1 };
                let backoff = &self.env.timing().retry_backoff;
                let pause = backoff[failures.min(backoff.len() - 1)];
                if self.wait_stop(pause) || self.idle() {
                    return;
                }
            }
        }));
        if result.is_err() {
            self.set(gen, STATE_ERROR, "The video decoder failed. See the log.");
        }
        let mut g = lock(&self.inner);
        if gen == g.gen {
            if g.state != STATE_ERROR && g.state != STATE_UNAVAILABLE {
                g.state = STATE_IDLE;
                g.message.clear();
            }
            g.frame = None;
            g.frame_at = None;
            g.fps = 0.0;
            self.cond.notify_all();
        }
    }

    fn command(&self, ffmpeg: &str, version: &str) -> std::io::Result<(Vec<String>, String)> {
        let url = self.open_url();
        let transport = {
            let t = self.cfg_str("transport");
            if t.is_empty() {
                DEFAULT_TRANSPORT.to_string()
            } else {
                t
            }
        };
        if !ffmpeg::hides_password(version) {
            return Ok((
                ffmpeg::ffmpeg_command(ffmpeg, &url, &transport, "", version),
                String::new(),
            ));
        }
        let runs = {
            let mut g = lock(&self.inner);
            g.runs += 1;
            g.runs
        };
        let path = self
            .env
            .workdir()?
            .join(format!("{}-{runs}.ffconcat", self.cfg_str("id")));
        ffmpeg::unlink(&path);
        ffmpeg::write_private(&path, &ffmpeg::concat_list(&url, &transport, version))?;
        let p = path.to_string_lossy().into_owned();
        Ok((ffmpeg::ffmpeg_command(ffmpeg, &url, &transport, &p, version), p))
    }

    /// One ffmpeg run, start to exit. True when it produced a picture.
    fn run_once(self: &Arc<Self>, gen: u64, ffmpeg: &str, version: &str) -> bool {
        self.set(gen, STATE_STARTING, "Connecting to the camera.");
        let (cmd, mut list_path) = match self.command(ffmpeg, version) {
            Ok(c) => c,
            Err(e) => {
                self.set(
                    gen,
                    STATE_ERROR,
                    &format!(
                        "The camera address could not be handed to ffmpeg: {}",
                        crate::peripherals::util::py_oserror_text(&e)
                    ),
                );
                return false;
            }
        };
        let mut command = ffmpeg::command(&cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                if !list_path.is_empty() {
                    ffmpeg::unlink(std::path::Path::new(&list_path));
                }
                self.set(
                    gen,
                    STATE_ERROR,
                    &format!(
                        "ffmpeg could not be started: {}",
                        crate::peripherals::util::os_error_text(&e)
                    ),
                );
                return false;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let proc = Arc::new(Mutex::new(child));
        lock(&self.inner).proc = Some(Arc::clone(&proc));
        // A stop that came in while the process was starting.
        if self.stopped() {
            signal_term(&proc);
        }
        let scrub_path = list_path.clone();
        let errors: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let got_frame = Arc::new(AtomicBool::new(false));
        let started = Instant::now();

        let me = Arc::clone(self);
        let got = Arc::clone(&got_frame);
        let out_task = Task::spawn(format!("corvus-video-out-{gen}"), move || {
            let Some(mut stream) = stdout else { return };
            let mut splitter = JpegSplitter::new(MAX_FRAME_BYTES);
            let mut buf = vec![0u8; 65536];
            loop {
                let n = match stream.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let mut frames = splitter.feed(&buf[..n]);
                if let Some(last) = frames.pop() {
                    me.publish(gen, last);
                    got.store(true, Ordering::SeqCst);
                }
            }
        });
        let me = Arc::clone(self);
        let errs = Arc::clone(&errors);
        let sp = scrub_path.clone();
        let err_task = Task::spawn(format!("corvus-video-err-{gen}"), move || {
            let Some(stream) = stderr else { return };
            let mut reader = std::io::BufReader::new(stream);
            loop {
                let mut raw = Vec::new();
                match (&mut reader).take(MAX_ERROR_LINE as u64).read_until(b'\n', &mut raw) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&raw);
                let line = crate::peripherals::util::py_strip(&line);
                if !line.is_empty() {
                    let mut e = lock(&errs);
                    if e.len() >= 20 {
                        e.pop_front();
                    }
                    e.push_back(me.scrub(line, &sp));
                }
            }
        });

        let timing = self.env.timing().clone();
        let mut reason = String::new();
        loop {
            if !matches!(lock(&proc).try_wait(), Ok(None)) {
                break;
            }
            if self.wait_stop(timing.poll.min(Duration::from_millis(250))) || self.idle() {
                break;
            }
            if !list_path.is_empty()
                && (got_frame.load(Ordering::SeqCst) || started.elapsed() > timing.list_keep)
                && ffmpeg::unlink(std::path::Path::new(&list_path))
            {
                list_path.clear();
            }
            let last = {
                let g = lock(&self.inner);
                if got_frame.load(Ordering::SeqCst) {
                    g.frame_at.unwrap_or(started)
                } else {
                    started
                }
            };
            if last.elapsed() > timing.watchdog {
                let s = timing.watchdog.as_secs();
                reason = if got_frame.load(Ordering::SeqCst) {
                    format!("No picture for {s} s. Reconnecting.")
                } else {
                    format!("No picture after {s} s. Check the address and the network.")
                };
                break;
            }
        }
        terminate(&proc, timing.term_timeout);
        out_task.join(timing.term_timeout);
        err_task.join(timing.term_timeout);
        if !list_path.is_empty() {
            ffmpeg::unlink(std::path::Path::new(&list_path));
        }
        {
            let mut g = lock(&self.inner);
            if g.proc.as_ref().is_some_and(|p| Arc::ptr_eq(p, &proc)) {
                g.proc = None;
            }
        }
        if !(self.stopped() || self.idle()) {
            let mut detail = if reason.is_empty() {
                last_error(&lock(&errors))
            } else {
                reason
            };
            if detail.is_empty() {
                detail = if got_frame.load(Ordering::SeqCst) {
                    "The camera stopped sending.".into()
                } else {
                    "ffmpeg ended without a picture.".into()
                };
            }
            self.set(gen, STATE_ERROR, &detail);
        }
        got_frame.load(Ordering::SeqCst)
    }
}

/// The most telling of ffmpeg's last lines: it ends with a generic "Error
/// opening input files" after the line that says why.
fn last_error(errors: &VecDeque<String>) -> String {
    for line in errors.iter().rev() {
        if !line.starts_with("Error opening input file") {
            return explain_error(line);
        }
    }
    errors.back().map(|l| explain_error(l)).unwrap_or_default()
}
