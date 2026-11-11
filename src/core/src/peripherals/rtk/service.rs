//! The RTK base station: one thread, one source of corrections, one link
//! (`rtk_service.py` over `corvus_link.rtk.service`).
//!
//! The session runs only once the operator has switched RTK on. A base plugged
//! in is found, surveyed and streaming without another setting being touched,
//! and the search is narrow in three ways: only a port whose USB descriptor
//! says GNSS receiver is a candidate; the port the MAVLink link is on is never
//! one; and nothing is written to a device that has not answered `MON-VER` as
//! a u-blox receiver, except the RTCM it already emits, which is read and
//! forwarded without configuring anything.
//!
//! One thread, one condition variable for its sleeps, one transport open at a
//! time. `stop` sets the flag and joins; a session that configured a receiver
//! puts it back out of base mode on the way out. The transport is closed under
//! a thread that did not come back, as the last resort.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::config::{self, fixed_position_problem, ntrip_problem, public_settings, survey_progress};
use super::io::{
    ntrip_request, read_ntrip_header, NtripTransport, Port, RtkError, SerialPort, SerialTransport, Socket, TcpSocket,
    Transport,
};
use super::wire::{self, RtcmFramer, UbxParser, BAUD_CANDIDATES};
use crate::peripherals::ports::{is_phantom_device, is_rtk_device, PortRow};

pub const STATE_OFF: &str = "off";
pub const STATE_SEARCHING: &str = "searching";
pub const STATE_CONNECTING: &str = "connecting";
pub const STATE_SURVEYING: &str = "surveying";
pub const STATE_ACTIVE: &str = "active";
pub const STATE_ERROR: &str = "error";

/// The durations the session works to; a test shortens them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How often the search looks for a newly plugged in base.
    pub search_interval: f64,
    /// Backoff after a session ends badly.
    pub retry_interval: f64,
    /// How long one blocking read may wait.
    pub read_timeout: f64,
    /// How long a candidate baud rate is given to produce a MON-VER reply.
    pub identify_timeout: f64,
    /// How long a device that never identified itself is listened to.
    pub listen_timeout: f64,
    /// How long the old survey is given to stop before the new one is configured.
    pub survey_stop_timeout: f64,
    /// Corrections older than this mean the source has gone quiet.
    pub source_silence: f64,
    pub ntrip_connect_timeout: f64,
    /// The pause between two configuration frames.
    pub frame_gap: f64,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            search_interval: 2.5,
            retry_interval: 5.0,
            read_timeout: 0.5,
            identify_timeout: 1.2,
            listen_timeout: 5.0,
            survey_stop_timeout: 3.0,
            source_silence: 20.0,
            ntrip_connect_timeout: 10.0,
            frame_gap: 0.05,
        }
    }
}

/// What the session needs from the vehicle side.
pub trait RtkHost: Send + Sync {
    /// Puts one RTCM frame on every link; whether the main one took it.
    fn inject_rtcm(&self, frame: &[u8]) -> bool;
    /// `{bytes, messages, dropped, age}` of what was injected.
    fn rtcm_stats(&self) -> Value;
    fn forget_rtcm_session(&self);
    /// The connection string of the main link.
    fn connection_string(&self) -> String;
    fn is_connected(&self) -> bool;
    /// The four scalar fields the rest of the app reads.
    fn store_update(&self, state: &str, source: &str, accuracy: f64, progress: i64);
    /// `(gps_fix, gps_satellites, gps_hdop)` of the vehicle.
    fn vehicle(&self) -> (Value, Value, Value);
}

pub type OpenPort = Arc<dyn Fn(&str, u32) -> io::Result<Arc<dyn Port>> + Send + Sync>;
pub type ListPorts = Arc<dyn Fn() -> Vec<PortRow> + Send + Sync>;
pub type OpenSocket = Arc<dyn Fn(&str, u16, f64) -> io::Result<Arc<dyn Socket>> + Send + Sync>;

/// The injectable devices.
#[derive(Clone)]
pub struct RtkIo {
    pub open_port: OpenPort,
    pub list_ports: ListPorts,
    pub open_socket: OpenSocket,
}

impl RtkIo {
    /// A real serial port, the system's port list and a TCP connection.
    pub fn system(timing: Timing) -> RtkIo {
        let read_timeout = timing.read_timeout;
        RtkIo {
            open_port: Arc::new(move |device, baud| {
                Ok(Arc::new(SerialPort::open(device, baud, read_timeout, 2.0)?) as Arc<dyn Port>)
            }),
            list_ports: Arc::new(crate::peripherals::ports::list_serial_ports),
            open_socket: Arc::new(move |host, port, timeout| {
                Ok(Arc::new(TcpSocket::connect(host, port, timeout, read_timeout)?) as Arc<dyn Socket>)
            }),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct Ctl {
    stop: bool,
    restart: bool,
}

struct State {
    settings: Map<String, Value>,
    state: String,
    message: String,
    error: String,
    device: String,
    baud: i64,
    receiver: Option<Value>,
    survey: Option<Value>,
    warning: String,
    frames: i64,
    frame_bytes: i64,
    crc_errors: i64,
    last_frame_at: Option<Instant>,
    started_at: Option<Instant>,
    messages_seen: Map<String, Value>,
}

struct Inner {
    host: Arc<dyn RtkHost>,
    io: RtkIo,
    timing: Timing,
    state: Mutex<State>,
    ctl: Mutex<Ctl>,
    ctl_changed: Condvar,
    thread: Mutex<Option<JoinHandle<()>>>,
    transport: Mutex<Option<Arc<dyn Transport>>>,
    alive: AtomicBool,
    log: Mutex<VecDeque<String>>,
}

/// Finds a source of RTCM corrections and keeps it on the link.
pub struct RtkService {
    inner: Arc<Inner>,
}

impl RtkService {
    pub fn new(host: Arc<dyn RtkHost>, settings: &Value, io: RtkIo, timing: Timing) -> RtkService {
        RtkService {
            inner: Arc::new(Inner {
                host,
                io,
                timing,
                state: Mutex::new(State {
                    settings: config::settings(settings),
                    state: STATE_OFF.into(),
                    message: String::new(),
                    error: String::new(),
                    device: String::new(),
                    baud: 0,
                    receiver: None,
                    survey: None,
                    warning: String::new(),
                    frames: 0,
                    frame_bytes: 0,
                    crc_errors: 0,
                    last_frame_at: None,
                    started_at: None,
                    messages_seen: Map::new(),
                }),
                ctl: Mutex::new(Ctl::default()),
                ctl_changed: Condvar::new(),
                thread: Mutex::new(None),
                transport: Mutex::new(None),
                alive: AtomicBool::new(false),
                log: Mutex::new(VecDeque::new()),
            }),
        }
    }

    /// Starts the session thread. Idempotent.
    pub fn start(&self) {
        let inner = &self.inner;
        let mut slot = lock(&inner.thread);
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        {
            let mut c = lock(&inner.ctl);
            c.stop = false;
            c.restart = false;
        }
        inner.alive.store(true, Ordering::SeqCst);
        let me = Arc::clone(inner);
        let handle = thread::Builder::new().name("rtk-base".into()).spawn(move || {
            me.run();
            me.alive.store(false, Ordering::SeqCst);
        });
        match handle {
            Ok(h) => *slot = Some(h),
            Err(_) => inner.alive.store(false, Ordering::SeqCst),
        }
        drop(slot);
        let intent = inner.describe_intent();
        inner.note(format!("RTK service started ({intent})"));
    }

    /// Stops the session thread and releases the port or socket; the session
    /// takes a configured receiver out of base mode on its own way out. The
    /// transport is not closed from here first: the session's own exit path
    /// has to write to it. Closing under a thread that did not come back is
    /// the last resort.
    pub fn stop(&self, timeout: f64) {
        let inner = &self.inner;
        {
            let mut c = lock(&inner.ctl);
            c.stop = true;
            c.restart = true;
        }
        inner.ctl_changed.notify_all();
        let handle = lock(&inner.thread).take();
        if let Some(h) = handle {
            if !join_within(&h, timeout) {
                if let Some(t) = lock(&inner.transport).clone() {
                    t.close();
                }
                if !join_within(&h, 1.0) {
                    inner.note(format!("RTK session thread did not stop within {timeout:.1}s"));
                    return;
                }
            }
            let _ = h.join();
        }
        inner.publish(STATE_OFF, "RTK corrections are not running");
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// The settings in force, resolved against the defaults.
    pub fn settings(&self) -> Map<String, Value> {
        config::settings(&Value::Object(lock(&self.inner.state).settings.clone()))
    }

    /// Replaces the settings and restarts the session against them: what the
    /// running session learned belongs to the source it learned it from.
    pub fn apply_settings(&self, raw: &Value) -> Map<String, Value> {
        let inner = &self.inner;
        let resolved = config::settings(raw);
        {
            let mut st = lock(&inner.state);
            st.settings = resolved.clone();
            st.receiver = None;
            st.survey = None;
            st.warning.clear();
            st.error.clear();
        }
        inner.reset_counters();
        inner.set_restart();
        let enabled = resolved.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        if enabled && !self.is_alive() && !lock(&inner.ctl).stop {
            self.start();
        }
        resolved
    }

    /// Starts the survey again from zero: the session is torn down and rebuilt.
    pub fn restart_survey(&self) {
        {
            let mut st = lock(&self.inner.state);
            st.survey = None;
            st.warning.clear();
        }
        self.inner.set_restart();
    }

    /// Everything the RTK page shows, in one read.
    pub fn status(&self) -> Value {
        let inner = &self.inner;
        let settings = self.settings();
        let st = {
            let st = lock(&inner.state);
            (
                st.state.clone(),
                st.message.clone(),
                st.error.clone(),
                st.device.clone(),
                st.baud,
                st.receiver.clone(),
                st.survey.clone(),
                st.warning.clone(),
                st.frames,
                st.frame_bytes,
                st.crc_errors,
                st.last_frame_at,
                st.started_at,
                st.messages_seen.clone(),
            )
        };
        let (
            state,
            message,
            error,
            device,
            baud,
            receiver,
            survey,
            warning,
            frames,
            frame_bytes,
            crc_errors,
            last,
            started,
            seen,
        ) = st;
        let target_duration = settings.get("survey_duration").and_then(Value::as_i64).unwrap_or(180);
        let target_accuracy = settings.get("survey_accuracy").and_then(Value::as_f64).unwrap_or(2.0);
        let age = |since: Option<Instant>| {
            since
                .map(|t| Value::from(t.elapsed().as_secs_f64()))
                .unwrap_or(Value::Null)
        };
        let (fix, sats, hdop) = inner.host.vehicle();
        let receiver_v = receiver.clone().unwrap_or(Value::Null);
        json!({
            "enabled": settings.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            "state": state,
            "message": message,
            "error": error,
            "warning": warning,
            "source": settings.get("source").cloned().unwrap_or(json!("usb")),
            "mode": settings.get("mode").cloned().unwrap_or(json!("survey")),
            "device": device,
            "baud": baud,
            "receiver": receiver_v,
            "receiver_label": wire::receiver_label(&receiver_v),
            "survey": survey.clone().unwrap_or(Value::Null),
            "survey_progress": survey_progress(survey.as_ref(), target_duration, target_accuracy),
            "survey_target": {
                "accuracy": settings.get("survey_accuracy").cloned().unwrap_or(Value::Null),
                "duration": settings.get("survey_duration").cloned().unwrap_or(Value::Null),
            },
            "frames": frames,
            "frame_bytes": frame_bytes,
            "crc_errors": crc_errors,
            "source_age": age(last),
            "uptime": age(started),
            "messages": seen,
            "injected": inner.host.rtcm_stats(),
            "link_ready": inner.host.is_connected(),
            "vehicle": {"fix": fix, "satellites": sats, "hdop": hdop},
            "ports": inner.candidate_ports().into_iter()
                .map(|p| json!({"device": p.device, "description": p.description, "hwid": p.hwid}))
                .collect::<Vec<_>>(),
            "settings": public_settings(&settings),
            "defaults": public_settings(&config::defaults()),
        })
    }

    /// What the service logged, newest last (kept short, for tests and the console).
    pub fn log(&self) -> Vec<String> {
        lock(&self.inner.log).iter().cloned().collect()
    }
}

fn join_within(h: &JoinHandle<()>, seconds: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while !h.is_finished() {
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

impl Inner {
    fn note(&self, text: String) {
        let mut log = lock(&self.log);
        if log.len() >= 200 {
            log.pop_front();
        }
        log.push_back(text);
    }

    fn stopped(&self) -> bool {
        lock(&self.ctl).stop
    }

    fn restarting(&self) -> bool {
        lock(&self.ctl).restart
    }

    fn set_restart(&self) {
        lock(&self.ctl).restart = true;
        self.ctl_changed.notify_all();
    }

    /// `_stop.wait(seconds)`: returns early once stopping.
    fn wait(&self, seconds: f64) {
        let end = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
        let mut c = lock(&self.ctl);
        while !c.stop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            c = self
                .ctl_changed
                .wait_timeout(c, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn settings(&self) -> Map<String, Value> {
        config::settings(&Value::Object(lock(&self.state).settings.clone()))
    }

    fn describe_intent(&self) -> String {
        let s = self.settings();
        if !s.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
            return "disabled".into();
        }
        if s.get("source").and_then(Value::as_str) == Some("ntrip") {
            return "NTRIP".into();
        }
        format!(
            "USB base station, {}",
            s.get("mode").and_then(Value::as_str).unwrap_or("survey")
        )
    }

    // -- the session thread ------------------------------------------------

    fn run(self: &Arc<Self>) {
        let t = self.timing;
        while !self.stopped() {
            let settings = self.settings();
            if !settings.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
                self.publish(STATE_OFF, "RTK corrections are switched off");
                self.wait(t.search_interval);
                continue;
            }
            lock(&self.ctl).restart = false;
            let transport = match self.acquire(&settings) {
                Ok(t) => t,
                Err(e) => {
                    self.fail(&e.0);
                    None
                }
            };
            let Some(transport) = transport else {
                let pause = if lock(&self.state).state == STATE_ERROR {
                    t.retry_interval
                } else {
                    t.search_interval
                };
                self.wait(pause);
                continue;
            };
            *lock(&self.transport) = Some(Arc::clone(&transport));
            let receiver = lock(&self.state).receiver.clone();
            let outcome = self.run_session(&transport, &settings, receiver.as_ref());
            if let Err(e) = &outcome {
                self.fail(&e.0);
            }
            self.release(&transport, &settings, receiver.as_ref());
            *lock(&self.transport) = None;
            if !self.stopped() {
                let pause = if lock(&self.state).state == STATE_ERROR {
                    t.retry_interval
                } else {
                    0.5
                };
                self.wait(pause);
            }
        }
    }

    fn acquire(&self, settings: &Map<String, Value>) -> Result<Option<Arc<dyn Transport>>, RtkError> {
        if settings.get("source").and_then(Value::as_str) == Some("ntrip") {
            return self.acquire_ntrip(settings).map(Some);
        }
        self.acquire_serial(settings)
    }

    fn acquire_serial(&self, settings: &Map<String, Value>) -> Result<Option<Arc<dyn Transport>>, RtkError> {
        let pinned = settings
            .get("device")
            .and_then(Value::as_str)
            .map(|s| crate::peripherals::util::py_strip(s).to_string())
            .unwrap_or_default();
        let candidates = self.candidate_ports();
        let device = if !pinned.is_empty() {
            if self.link_device() == pinned {
                return Err(RtkError(
                    "that port is the vehicle's telemetry link. Pick another, or let Corvus find the base station itself"
                        .into(),
                ));
            }
            pinned
        } else {
            let Some(first) = candidates.first() else {
                self.publish(
                    STATE_SEARCHING,
                    "Looking for a base station. Plug an RTK GNSS receiver into this computer",
                );
                return Ok(None);
            };
            first.device.clone()
        };
        self.publish(STATE_CONNECTING, &format!("Opening {device}"));
        let bauds: Vec<u32> = match settings.get("baud").and_then(Value::as_i64).filter(|b| *b != 0) {
            Some(b) => vec![b as u32],
            None => BAUD_CANDIDATES.to_vec(),
        };
        let silent = "the receiver did not answer";
        let mut last_error = String::new();
        for &baud in &bauds {
            let port = match (self.io.open_port)(&device, baud) {
                Ok(p) => p,
                Err(e) => {
                    let text = e.to_string();
                    last_error = if text.is_empty() {
                        format!("{:?}", e.kind())
                    } else {
                        text
                    };
                    continue;
                }
            };
            let transport: Arc<dyn Transport> = Arc::new(SerialTransport::new(port, &device, baud));
            let (kind, version) = self.probe(&transport);
            if kind.is_some() {
                let mut st = lock(&self.state);
                st.device = device.clone();
                st.baud = i64::from(baud);
                st.receiver = version;
                return Ok(Some(transport));
            }
            transport.close();
            last_error = silent.into();
        }
        if !last_error.is_empty() && last_error != silent {
            return Err(RtkError(format!("could not open {device}: {last_error}")));
        }
        let first = *bauds.first().ok_or_else(|| RtkError("no baud rate to try".into()))?;
        let port = (self.io.open_port)(&device, first)
            .map_err(|e| RtkError(format!("could not open the correction source: {e}")))?;
        {
            let mut st = lock(&self.state);
            st.device = device.clone();
            st.baud = i64::from(first);
            st.receiver = None;
        }
        Ok(Some(Arc::new(SerialTransport::new(port, &device, first))))
    }

    /// What is on this port at this baud rate: `("ubx", version)` for a u-blox
    /// receiver, `("rtcm", None)` for a base already streaming.
    fn probe(&self, transport: &Arc<dyn Transport>) -> (Option<&'static str>, Option<Value>) {
        if transport
            .write(&wire::poll_frame(wire::UBX_CLASS_MON, wire::UBX_ID_MON_VER))
            .is_err()
        {
            return (None, None);
        }
        let mut ubx = UbxParser::new();
        let mut framer = RtcmFramer::new();
        let deadline = Instant::now() + Duration::from_secs_f64(self.timing.identify_timeout);
        while Instant::now() < deadline && !self.stopped() {
            let chunk = match transport.read() {
                Ok(c) => c,
                Err(_) => return (None, None),
            };
            if chunk.is_empty() {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            for (class, id, payload) in ubx.feed(&chunk) {
                if class == wire::UBX_CLASS_MON && id == wire::UBX_ID_MON_VER {
                    let version = wire::parse_mon_ver(&payload);
                    let label = wire::receiver_label(&version);
                    self.note(format!(
                        "RTK receiver on {} at {}: {}",
                        transport.device(),
                        transport.baud(),
                        if label.is_empty() { "unknown" } else { &label }
                    ));
                    return (Some("ubx"), Some(version));
                }
            }
            if !framer.feed(&chunk).is_empty() {
                self.note(format!(
                    "RTK corrections already streaming from {} at {}",
                    transport.device(),
                    transport.baud()
                ));
                return (Some("rtcm"), None);
            }
        }
        (None, None)
    }

    fn acquire_ntrip(&self, settings: &Map<String, Value>) -> Result<Arc<dyn Transport>, RtkError> {
        let ntrip = settings
            .get("ntrip")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let problem = ntrip_problem(&ntrip);
        if !problem.is_empty() {
            return Err(RtkError(problem.into()));
        }
        let text = |k: &str| -> String {
            ntrip
                .get(k)
                .and_then(Value::as_str)
                .map(|s| crate::peripherals::util::py_strip(s).to_string())
                .unwrap_or_default()
        };
        let host = text("host");
        let port = ntrip.get("port").and_then(Value::as_i64).unwrap_or(2101);
        let mountpoint = text("mountpoint").trim_start_matches('/').to_string();
        let label = format!("{host}:{port}/{mountpoint}");
        self.publish(STATE_CONNECTING, &format!("Connecting to {label}"));
        let sock = (self.io.open_socket)(&host, port as u16, self.timing.ntrip_connect_timeout).map_err(|e| {
            RtkError(format!(
                "could not open the correction source: {}",
                crate::peripherals::util::py_oserror_text(&e)
            ))
        })?;
        let transport = Arc::new(NtripTransport::new(Arc::clone(&sock), &label));
        let exchange = (|| -> Result<Vec<u8>, ExchangeError> {
            let request = ntrip_request(
                &host,
                port,
                &mountpoint,
                ntrip.get("username").and_then(Value::as_str).unwrap_or(""),
                ntrip.get("password").and_then(Value::as_str).unwrap_or(""),
            );
            sock.send_all(&request).map_err(ExchangeError::Os)?;
            read_ntrip_header(sock.as_ref(), self.timing.ntrip_connect_timeout).map_err(ExchangeError::Rtk)
        })();
        match exchange {
            Ok(header) => {
                {
                    let mut st = lock(&self.state);
                    st.device = label;
                    st.baud = 0;
                }
                transport.set_pending(header);
                Ok(transport)
            }
            Err(ExchangeError::Rtk(e)) => {
                transport.close();
                Err(e)
            }
            Err(ExchangeError::Os(e)) => {
                transport.close();
                Err(RtkError(format!(
                    "could not reach {label}: {}",
                    crate::peripherals::util::py_oserror_text(&e)
                )))
            }
        }
    }

    // -- one session -------------------------------------------------------

    fn run_session(
        &self,
        transport: &Arc<dyn Transport>,
        settings: &Map<String, Value>,
        version: Option<&Value>,
    ) -> Result<(), RtkError> {
        let mut framer = RtcmFramer::new();
        let mut ubx = UbxParser::new();
        self.reset_counters();
        lock(&self.state).started_at = Some(Instant::now());
        let pending = transport.take_pending();
        if !pending.is_empty() {
            self.consume(&pending, &mut framer, &mut ubx);
        }
        let configurable = transport.configurable()
            && version.is_some()
            && settings.get("source").and_then(Value::as_str) != Some("ntrip");
        if configurable {
            self.configure(transport, settings, &mut framer, &mut ubx, version.expect("a version"))?;
        } else {
            self.publish(
                STATE_ACTIVE,
                &format!("Forwarding corrections from {}", transport.label()),
            );
        }
        let deadline_quiet = Instant::now() + Duration::from_secs_f64(self.timing.listen_timeout);
        while !self.stopped() && !self.restarting() {
            let chunk = transport.read()?;
            if !chunk.is_empty() {
                self.consume(&chunk, &mut framer, &mut ubx);
            } else {
                thread::sleep(Duration::from_millis(20));
            }
            let (last, frames) = {
                let st = lock(&self.state);
                (st.last_frame_at, st.frames)
            };
            let now = Instant::now();
            if frames == 0 && now > deadline_quiet {
                return Err(RtkError(format!(
                    "{} produced no corrections. Check that the base station has power and a clear view of the sky",
                    transport.label()
                )));
            }
            if frames != 0 {
                if let Some(last) = last {
                    if now.duration_since(last).as_secs_f64() > self.timing.source_silence {
                        return Err(RtkError(format!("{} stopped sending corrections", transport.label())));
                    }
                }
            }
        }
        Ok(())
    }

    fn configure(
        &self,
        transport: &Arc<dyn Transport>,
        settings: &Map<String, Value>,
        framer: &mut RtcmFramer,
        ubx: &mut UbxParser,
        version: &Value,
    ) -> Result<(), RtkError> {
        let modern = wire::supports_valset(version);
        let msm7 = settings.get("msm7").and_then(Value::as_bool).unwrap_or(false);
        if settings.get("mode").and_then(Value::as_str) == Some("fixed") {
            let fixed = settings
                .get("fixed")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let problem = fixed_position_problem(&fixed);
            if !problem.is_empty() {
                return Err(RtkError(problem.into()));
            }
            let f = |k: &str| fixed.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let frames = if modern {
                wire::fixed_position_frames(f("latitude"), f("longitude"), f("altitude"), f("accuracy"), msm7)
            } else {
                wire::legacy_fixed_position_frames(f("latitude"), f("longitude"), f("altitude"), f("accuracy"), msm7)
            };
            self.publish(STATE_CONNECTING, "Setting the fixed base position");
            self.write_all(transport, &frames, false)?;
            self.activate(transport, modern, msm7)?;
            self.publish(STATE_ACTIVE, "Correcting from the fixed base position");
            return Ok(());
        }
        self.publish(STATE_CONNECTING, "Starting the survey");
        let stop = if modern {
            wire::stop_output_frames(msm7)
        } else {
            wire::legacy_stop_output_frames(msm7)
        };
        self.write_all(transport, &stop, false)?;
        self.await_survey_stopped(transport, ubx, framer)?;
        let duration = settings.get("survey_duration").and_then(Value::as_i64).unwrap_or(180);
        let accuracy = settings.get("survey_accuracy").and_then(Value::as_f64).unwrap_or(2.0);
        let frames = if modern {
            wire::survey_in_frames(duration, accuracy, msm7)
        } else {
            wire::legacy_survey_in_frames(duration, accuracy, msm7)
        };
        self.write_all(transport, &frames, false)?;
        self.publish(STATE_SURVEYING, "Surveying: the base is learning where it is");
        self.await_survey_complete(transport, ubx, framer)?;
        self.activate(transport, modern, msm7)?;
        self.publish(STATE_ACTIVE, "The survey is complete and corrections are flowing");
        Ok(())
    }

    fn activate(&self, transport: &Arc<dyn Transport>, modern: bool, msm7: bool) -> Result<(), RtkError> {
        let frames = if modern {
            wire::activate_output_frames(msm7, true)
        } else {
            wire::legacy_activate_output_frames(msm7)
        };
        self.write_all(transport, &frames, false)
    }

    /// Writes `frames` in order with a pause between them; `force` writes even
    /// once stopping, for the teardown that has to happen because of it.
    fn write_all(&self, transport: &Arc<dyn Transport>, frames: &[Vec<u8>], force: bool) -> Result<(), RtkError> {
        for frame in frames {
            if self.stopped() && !force {
                return Ok(());
            }
            transport.write(frame)?;
            thread::sleep(Duration::from_secs_f64(self.timing.frame_gap));
        }
        Ok(())
    }

    /// Waits until NAV-SVIN reports no survey running. A receiver that never
    /// says is a warning, not an error: asking for survey in while one runs is
    /// a no-op on a u-blox receiver.
    fn await_survey_stopped(
        &self,
        transport: &Arc<dyn Transport>,
        ubx: &mut UbxParser,
        framer: &mut RtcmFramer,
    ) -> Result<(), RtkError> {
        let deadline = Instant::now() + Duration::from_secs_f64(self.timing.survey_stop_timeout);
        let poll = wire::poll_frame(wire::UBX_CLASS_NAV, wire::UBX_ID_NAV_SVIN);
        let mut seen = false;
        while Instant::now() < deadline && !self.stopped() {
            transport.write(&poll)?;
            let end = Instant::now() + Duration::from_millis(300);
            while Instant::now() < end {
                let chunk = transport.read()?;
                if chunk.is_empty() {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                let Some(survey) = self.consume(&chunk, framer, ubx) else {
                    continue;
                };
                seen = true;
                let flag = |k: &str| survey.get(k).and_then(Value::as_bool).unwrap_or(false);
                if !flag("active") && !flag("valid") {
                    return Ok(());
                }
            }
        }
        self.warn(if seen {
            "the previous survey did not stop; its settings may still apply"
        } else {
            "the receiver did not report its survey state"
        });
        Ok(())
    }

    /// Reads NAV-SVIN until the survey is valid and no longer running. No
    /// timeout, on purpose: a progress bar that is not moving is the honest
    /// report of a survey the sky will not allow.
    fn await_survey_complete(
        &self,
        transport: &Arc<dyn Transport>,
        ubx: &mut UbxParser,
        framer: &mut RtcmFramer,
    ) -> Result<(), RtkError> {
        let poll = wire::poll_frame(wire::UBX_CLASS_NAV, wire::UBX_ID_NAV_SVIN);
        let mut next_poll: Option<Instant> = None;
        while !self.stopped() && !self.restarting() {
            let now = Instant::now();
            if next_poll.is_none_or(|n| now >= n) {
                transport.write(&poll)?;
                next_poll = Some(now + Duration::from_secs(1));
            }
            let chunk = transport.read()?;
            if chunk.is_empty() {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            if let Some(survey) = self.consume(&chunk, framer, ubx) {
                let flag = |k: &str| survey.get(k).and_then(Value::as_bool).unwrap_or(false);
                if flag("valid") && !flag("active") {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    // -- bytes in ----------------------------------------------------------

    /// Feeds one read to both parsers, injects the corrections it finished and
    /// returns the survey report it carried.
    fn consume(&self, chunk: &[u8], framer: &mut RtcmFramer, ubx: &mut UbxParser) -> Option<Value> {
        let mut survey = None;
        for frame in framer.feed(chunk) {
            self.note_frame(&frame);
            self.host.inject_rtcm(&frame);
        }
        for (class, id, payload) in ubx.feed(chunk) {
            if class == wire::UBX_CLASS_NAV && id == wire::UBX_ID_NAV_SVIN {
                if let Some(parsed) = wire::parse_nav_svin(&payload) {
                    survey = Some(parsed.clone());
                    lock(&self.state).survey = Some(parsed.clone());
                    self.publish_progress(&parsed);
                }
            }
        }
        lock(&self.state).crc_errors = framer.crc_errors;
        survey
    }

    fn note_frame(&self, frame: &[u8]) {
        let number = wire::message_number(frame);
        let mut st = lock(&self.state);
        st.frames += 1;
        st.frame_bytes += frame.len() as i64;
        st.last_frame_at = Some(Instant::now());
        if number != 0 {
            let key = number.to_string();
            let n = st.messages_seen.get(&key).and_then(Value::as_i64).unwrap_or(0) + 1;
            st.messages_seen.insert(key, Value::from(n));
        }
    }

    // -- teardown ----------------------------------------------------------

    /// Closes the source, taking a configured receiver out of base mode. Runs
    /// on the way out of every session, including the one that ends because
    /// the application is shutting down.
    fn release(&self, transport: &Arc<dyn Transport>, settings: &Map<String, Value>, version: Option<&Value>) {
        if transport.configurable() {
            if let Some(version) = version {
                let msm7 = settings.get("msm7").and_then(Value::as_bool).unwrap_or(false);
                let frames = if wire::supports_valset(version) {
                    wire::disable_base_frames(msm7)
                } else {
                    wire::legacy_disable_base_frames(msm7)
                };
                if self.write_all(transport, &frames, true).is_err() {
                    self.note("could not take the receiver out of base mode".into());
                }
            }
        }
        transport.close();
    }

    // -- state -------------------------------------------------------------

    /// The serial device the MAVLink link is using, or "". Read as Python
    /// reads it: the second `:` separated field of a `serial:` string.
    fn link_device(&self) -> String {
        let conn = self.host.connection_string();
        if !conn.starts_with("serial:") {
            return String::new();
        }
        conn.split(':').nth(1).unwrap_or("").to_string()
    }

    /// Serial ports that look like a base station, the link's port excluded.
    fn candidate_ports(&self) -> Vec<PortRow> {
        let link = self.link_device();
        (self.io.list_ports)()
            .into_iter()
            .filter(|p| {
                !p.device.is_empty()
                    && p.device != link
                    && !is_phantom_device(&p.device)
                    && is_rtk_device(&p.device, &p.hwid, &p.description)
            })
            .collect()
    }

    fn publish(&self, state: &str, message: &str) {
        let (survey, settings) = {
            let mut st = lock(&self.state);
            st.state = state.into();
            st.message = message.into();
            if state != STATE_ERROR {
                st.error.clear();
            }
            (st.survey.clone(), st.settings.clone())
        };
        self.store_update(state, survey.as_ref(), &settings);
    }

    fn publish_progress(&self, survey: &Value) {
        let (state, settings) = {
            let st = lock(&self.state);
            (st.state.clone(), st.settings.clone())
        };
        self.store_update(&state, Some(survey), &settings);
    }

    /// The four scalar fields the rest of the app reads, into the store.
    fn store_update(&self, state: &str, survey: Option<&Value>, settings: &Map<String, Value>) {
        let source = settings.get("source").and_then(Value::as_str).unwrap_or("");
        let accuracy = survey
            .and_then(|s| s.get("accuracy"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let progress = survey_progress(
            survey,
            settings.get("survey_duration").and_then(Value::as_i64).unwrap_or(180),
            settings.get("survey_accuracy").and_then(Value::as_f64).unwrap_or(2.0),
        );
        self.host.store_update(state, source, accuracy, progress);
    }

    fn fail(&self, reason: &str) {
        let (survey, settings) = {
            let mut st = lock(&self.state);
            st.state = STATE_ERROR.into();
            st.error = reason.into();
            st.message = reason.into();
            (st.survey.clone(), st.settings.clone())
        };
        self.store_update(STATE_ERROR, survey.as_ref(), &settings);
        self.note(format!("RTK: {reason}"));
    }

    fn warn(&self, text: &str) {
        lock(&self.state).warning = text.into();
        self.note(format!("RTK: {text}"));
    }

    fn reset_counters(&self) {
        {
            let mut st = lock(&self.state);
            st.frames = 0;
            st.frame_bytes = 0;
            st.crc_errors = 0;
            st.last_frame_at = None;
            st.messages_seen = Map::new();
        }
        self.host.forget_rtcm_session();
    }
}

enum ExchangeError {
    Rtk(RtkError),
    Os(io::Error),
}
