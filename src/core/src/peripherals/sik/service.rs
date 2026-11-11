//! SiK telemetry radio AT sessions (`sik_service.py` over
//! `corvus_link.sik.SikServiceEngine`): the half of the radio page that owns a
//! port.
//!
//! A radio in AT command mode has stopped relaying, so a session on the live
//! link's device stops the bridge for its duration and restarts it afterwards.
//! A session on any other port does not touch the bridge. Both are refused
//! while armed. The whole session is synchronous, on the caller's thread,
//! under one busy flag, and ends by itself: there is no thread to join.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use corvus_link::plain::transport::Serial;
use serde_json::{json, Map, Value};

use super::config::{self, SikConfigError};
use crate::peripherals::ports::{self, classify_hwid, classify_serial_device, is_windows_com_port, PortRow};
use crate::peripherals::util::py_oserror_text;

pub const ARMED_MESSAGE: &str =
    "Radio configuration is refused while the vehicle is armed. A radio in command mode is not relaying telemetry.";
pub const BUSY_MESSAGE: &str = "A radio configuration session is already running.";

/// Why a request did not complete: a bad write is the operator's (400), a
/// condition they can act on is theirs too (409), anything else is the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SikFailure {
    Config(String),
    Sik(String),
    Other(String),
}

impl From<SikConfigError> for SikFailure {
    fn from(e: SikConfigError) -> SikFailure {
        SikFailure::Config(e.0)
    }
}

fn sik(text: impl Into<String>) -> SikFailure {
    SikFailure::Sik(text.into())
}

/// The durations of a session; a test shortens them.
#[derive(Debug, Clone, Copy)]
pub struct SikTiming {
    /// Silence the radio requires either side of `+++`.
    pub guard: f64,
    /// How long one command may stay quiet before its reply is complete.
    pub idle: f64,
    pub local_timeout: f64,
    pub remote_timeout: f64,
    /// Time the radio is given to come back up after `ATZ`.
    pub reboot: f64,
}

impl Default for SikTiming {
    fn default() -> SikTiming {
        SikTiming {
            guard: 1.25,
            idle: 0.35,
            local_timeout: 1.5,
            remote_timeout: 6.0,
            reboot: 2.0,
        }
    }
}

/// A serial port as an AT session uses it.
pub trait SikPort: Send + Sync {
    fn in_waiting(&self) -> io::Result<usize>;
    fn read(&self, n: usize) -> io::Result<Vec<u8>>;
    fn write(&self, data: &[u8]) -> io::Result<()>;
    fn reset_input(&self) -> io::Result<()>;
    fn close(&self);
}

/// The real port: opened straight at the radio's baud rate.
pub struct SikSerial(Serial);

impl SikSerial {
    pub fn open(device: &str, baud: u32) -> io::Result<SikSerial> {
        Serial::open_direct(device, baud)
            .map(SikSerial)
            .map_err(crate::peripherals::util::serial_open_error)
    }
}

impl SikPort for SikSerial {
    fn in_waiting(&self) -> io::Result<usize> {
        self.0.waiting()
    }

    fn read(&self, n: usize) -> io::Result<Vec<u8>> {
        self.0.read_timeout(n, 0.1)
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        self.0.write_timeout(data, 2.0).map(|_| ())
    }

    fn reset_input(&self) -> io::Result<()> {
        self.0.reset_input()
    }

    fn close(&self) {
        self.0.close();
    }
}

/// What a session does once the radio answers: the session and the baud it was reached at.
type SessionBody<'a> = dyn FnMut(&SikSession, i64) -> Result<Map<String, Value>, SikFailure> + 'a;

pub type OpenSikPort = Arc<dyn Fn(&str, u32) -> io::Result<Arc<dyn SikPort>> + Send + Sync>;

/// What the service needs from the vehicle side.
pub trait SikHost: Send + Sync {
    fn armed(&self) -> bool;
    /// The connection string of the link, `""` when none.
    fn connection_string(&self) -> String;
    /// Whether the link's connect and receive cycle is live.
    fn link_running(&self) -> bool;
    fn stop_link(&self);
    /// Puts the link back on `conn` and starts it; `Err` says why it could not.
    fn restart_link(&self, conn: &str) -> Result<(), String>;
    fn list_ports(&self) -> Vec<PortRow>;
}

/// One open serial port, held in AT command mode.
pub struct SikSession {
    port: Arc<dyn SikPort>,
    timing: SikTiming,
    log: Arc<Mutex<Vec<String>>>,
}

fn sleep(seconds: f64) {
    thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
}

impl SikSession {
    /// Reads until the radio goes quiet, or until `timeout`: there is no
    /// framing to key on, so the end of a reply is defined by silence.
    fn read_reply(&self, timeout: f64) -> Result<String, SikFailure> {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        let mut chunks: Vec<u8> = Vec::new();
        let mut any = false;
        let mut last = Instant::now();
        while Instant::now() < deadline {
            let waiting = self.port.in_waiting().unwrap_or(0);
            let mut data = Vec::new();
            if waiting > 0 {
                data = self
                    .port
                    .read(waiting)
                    .map_err(|e| sik(format!("serial read failed: {}", py_oserror_text(&e))))?;
            }
            if !data.is_empty() {
                chunks.extend(data);
                any = true;
                last = Instant::now();
            } else if any && last.elapsed().as_secs_f64() >= self.timing.idle {
                break;
            } else {
                sleep(0.02);
            }
        }
        Ok(chunks
            .iter()
            .map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' })
            .collect())
    }

    /// Sends one AT/RT command and returns whatever came back (the echo stays
    /// in the reply).
    pub fn command(&self, text: &str, timeout: Option<f64>) -> Result<String, SikFailure> {
        let remote = text.to_uppercase().starts_with("RT");
        let timeout = timeout.unwrap_or(if remote {
            self.timing.remote_timeout
        } else {
            self.timing.local_timeout
        });
        let sent = (|| -> io::Result<()> {
            self.port.reset_input()?;
            self.port.write(format!("{text}\r\n").as_bytes())
        })();
        if let Err(e) = sent {
            return Err(sik(format!("serial write failed: {}", py_oserror_text(&e))));
        }
        self.read_reply(timeout)
    }

    /// `+++` with its guard times. True when the radio answered.
    fn enter_command_mode(&self, attempts: u32) -> Result<bool, SikFailure> {
        for attempt in 1..=attempts.max(1) {
            let _ = self.port.reset_input();
            sleep(self.timing.guard);
            if let Err(e) = self.port.write(b"+++") {
                return Err(sik(format!("serial write failed: {}", py_oserror_text(&e))));
            }
            sleep(self.timing.guard);
            let reply = self.read_reply(self.timing.local_timeout)?;
            if reply.to_uppercase().contains("OK") {
                return Ok(true);
            }
            if !config::parse_version(&self.command("ATI", None)?).is_empty() {
                return Ok(true);
            }
            lock(&self.log).push(format!("sik: +++ attempt {attempt} got {reply:?}"));
        }
        Ok(false)
    }

    /// `ATO`: hands the port back to the telemetry stream; best effort.
    fn leave_command_mode(&self) {
        if let Err(e) = self.command("ATO", Some(0.4)) {
            lock(&self.log).push(format!("sik: ATO failed: {e:?}"));
        }
    }

    /// Reads one end of the link, or `None` when the radio did not answer.
    pub fn read_radio(&self, remote: bool) -> Result<Option<Value>, SikFailure> {
        let prefix = if remote { "RT" } else { "AT" };
        let registers = config::parse_registers(&self.command(&format!("{prefix}I5"), None)?);
        if registers.is_empty() {
            return Ok(None);
        }
        let version = config::parse_version(&self.command(&format!("{prefix}I"), None)?);
        let board = config::parse_version(&self.command(&format!("{prefix}I2"), None)?);
        Ok(Some(config::describe_radio(&registers, &version, &board)))
    }

    /// `ATI7`: signal and noise at both ends, or `None`.
    fn read_link_quality(&self) -> Result<Option<Value>, SikFailure> {
        match self.command("ATI7", None) {
            Ok(reply) => Ok(config::parse_rssi(&reply)),
            Err(SikFailure::Sik(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Runs a write plan, checking each command before sending the next.
    fn apply(&self, writes: &[Value], remote: bool) -> Result<(), SikFailure> {
        for command in config::write_plan(writes, remote) {
            let reply = self.command(&command, None)?;
            if command.to_uppercase().ends_with('Z') {
                sleep(self.timing.reboot);
                continue;
            }
            if reply.to_uppercase().contains("ERROR") {
                return Err(sik(format!("the radio rejected {command}")));
            }
        }
        Ok(())
    }

    /// `AT&F`, then write and reboot: back to the firmware's defaults.
    fn factory_reset(&self, remote: bool) -> Result<(), SikFailure> {
        let prefix = if remote { "RT" } else { "AT" };
        if self
            .command(&format!("{prefix}&F"), None)?
            .to_uppercase()
            .contains("ERROR")
        {
            return Err(sik("the radio rejected the factory reset"));
        }
        self.command(&format!("{prefix}&W"), None)?;
        self.command(&format!("{prefix}Z"), None)?;
        sleep(self.timing.reboot);
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Do two port names refer to the same device? Case insensitive for Windows
/// COM names, exact elsewhere.
pub fn same_device(left: &str, right: &str) -> bool {
    let (left, right) = (left.trim(), right.trim());
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if is_windows_com_port(left) || is_windows_com_port(right) {
        return left.to_lowercase() == right.to_lowercase();
    }
    left == right
}

/// The classification of a link: `usb`, `sik`, `udp`, `tcp` or `unknown`
/// (`MavlinkBridge.transport`).
pub fn link_transport(conn: &str, rows: &[PortRow]) -> &'static str {
    if ["udp:", "udpin:", "udpout:", "udpbcast:"]
        .iter()
        .any(|p| conn.starts_with(p))
    {
        return "udp";
    }
    if conn.starts_with("tcp:") || conn.starts_with("tcpin:") {
        return "tcp";
    }
    if conn.starts_with("serial:") {
        let (device, _) = ports::split_serial_connection(conn);
        let by_name = classify_serial_device(&device, "", "");
        if by_name != "unknown" && by_name != "rtk" {
            return by_name;
        }
        let name = device.trim().to_lowercase();
        let mut aliases = vec![name.clone()];
        for (this, other) in [("/dev/tty.", "/dev/cu."), ("/dev/cu.", "/dev/tty.")] {
            if let Some(rest) = name.strip_prefix(this) {
                aliases.push(format!("{other}{rest}"));
            }
        }
        let by_descriptor =
            is_windows_com_port(&device) || name.starts_with("/dev/cu.") || name.starts_with("/dev/tty.");
        if by_descriptor {
            for alias in &aliases {
                if let Some(p) = rows.iter().find(|p| p.device.trim().to_lowercase() == *alias) {
                    let hwid = format!("{} {}", p.hwid, p.description);
                    if !hwid.trim().is_empty() {
                        return classify_hwid(&hwid);
                    }
                }
            }
            return "unknown";
        }
    }
    "unknown"
}

/// Gate, port ownership and bridge orchestration for SiK radio sessions. One
/// session at a time.
pub struct SikService {
    host: Arc<dyn SikHost>,
    open: OpenSikPort,
    timing: SikTiming,
    busy: AtomicBool,
    shutting_down: AtomicBool,
    log: Arc<Mutex<Vec<String>>>,
}

impl SikService {
    pub fn new(host: Arc<dyn SikHost>, open: OpenSikPort, timing: SikTiming) -> SikService {
        SikService {
            host,
            open,
            timing,
            busy: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            log: Arc::default(),
        }
    }

    /// The real serial port.
    pub fn system_open() -> OpenSikPort {
        Arc::new(|device, baud| Ok(Arc::new(SikSerial::open(device, baud)?) as Arc<dyn SikPort>))
    }

    /// What the service logged (for tests and the console).
    pub fn log(&self) -> Vec<String> {
        lock(&self.log).clone()
    }

    /// What the page needs before any radio has been touched.
    pub fn status(&self) -> Value {
        let conn = self.host.connection_string();
        let rows = self.host.list_ports();
        let (mut link_device, mut link_baud) = (String::new(), config::DEFAULT_BAUD);
        if conn.starts_with("serial:") {
            let (d, b) = ports::split_serial_connection(&conn);
            link_device = d;
            link_baud = i64::from(b);
        }
        let transport = if conn.is_empty() {
            "unknown"
        } else {
            link_transport(&conn, &rows)
        };
        let port_rows: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "device": r.device,
                    "description": r.description,
                    "kind": classify_serial_device(&r.device, &r.hwid, &r.description),
                    "is_link": same_device(&r.device, &link_device),
                })
            })
            .collect();
        let armed = self.host.armed();
        let busy = self.busy.load(Ordering::SeqCst);
        let blocked = if armed {
            ARMED_MESSAGE
        } else if busy {
            BUSY_MESSAGE
        } else {
            ""
        };
        json!({
            "ports": port_rows,
            "link_device": link_device,
            "link_baud": link_baud,
            "transport": transport,
            "armed": armed,
            "busy": busy,
            "can_configure": blocked.is_empty(),
            "blocked_reason": blocked,
            "default_baud": config::DEFAULT_BAUD,
            "bauds": config::baud_candidates(),
        })
    }

    fn run(
        &self,
        device: &str,
        baud: Option<i64>,
        body: &mut SessionBody<'_>,
    ) -> Result<Map<String, Value>, SikFailure> {
        if self.host.armed() {
            return Err(sik(ARMED_MESSAGE));
        }
        let device = device.trim();
        if device.is_empty() {
            return Err(sik("no serial port was selected"));
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(sik(BUSY_MESSAGE));
        }
        let result = self.run_locked(device, baud, body);
        self.busy.store(false, Ordering::SeqCst);
        result
    }

    /// Opens `device` and escapes into command mode, finding the baud if
    /// needed: the caller's own baud gets two attempts and the guesses after it
    /// one each.
    fn open_session(&self, device: &str, baud: Option<i64>) -> Result<(Arc<dyn SikPort>, SikSession, i64), SikFailure> {
        let mut candidates: Vec<i64> = Vec::new();
        for b in baud.into_iter().chain(config::baud_candidates().iter().copied()) {
            if b != 0 && !candidates.contains(&b) {
                candidates.push(b);
            }
        }
        let mut last_error = String::new();
        for (index, &candidate) in candidates.iter().enumerate() {
            let port = match (self.open)(device, candidate as u32) {
                Ok(p) => p,
                Err(e) => {
                    last_error = e.to_string();
                    continue;
                }
            };
            let session = SikSession {
                port: Arc::clone(&port),
                timing: self.timing,
                log: Arc::clone(&self.log),
            };
            match session.enter_command_mode(if index == 0 { 2 } else { 1 }) {
                Ok(true) => return Ok((port, session, candidate)),
                Ok(false) => {}
                Err(e) => {
                    port.close();
                    return Err(e);
                }
            }
            port.close();
            last_error = format!("no answer at {candidate} baud");
        }
        Err(sik(if last_error.is_empty() {
            format!("could not reach a radio on {device}")
        } else {
            format!(
                "could not reach a radio on {device} ({last_error}). Check that this is the radio's port, that nothing else has it open, and that the radio has power."
            )
        }))
    }

    fn run_locked(
        &self,
        device: &str,
        baud: Option<i64>,
        body: &mut SessionBody<'_>,
    ) -> Result<Map<String, Value>, SikFailure> {
        let conn = self.host.connection_string();
        let link_device = if conn.starts_with("serial:") {
            ports::split_serial_connection(&conn).0
        } else {
            String::new()
        };
        let owns_link = same_device(device, &link_device) && self.host.link_running();
        let restore = if owns_link { conn } else { String::new() };
        if owns_link {
            lock(&self.log).push(format!("sik: stopping the MAVLink bridge to configure {device}"));
            self.host.stop_link();
        }
        let mut opened: Option<(Arc<dyn SikPort>, SikSession)> = None;
        let outcome = (|| -> Result<Map<String, Value>, SikFailure> {
            let (port, session, reached) = self.open_session(device, baud)?;
            opened = Some((port, session));
            let session = &opened.as_ref().expect("just set").1;
            let mut result = body(session, reached)?;
            result.entry("device").or_insert(json!(device));
            result.entry("baud").or_insert(json!(reached));
            Ok(result)
        })();
        if let Some((port, session)) = &opened {
            session.leave_command_mode();
            port.close();
        }
        self.restart_bridge(&restore);
        outcome
    }

    fn restart_bridge(&self, conn: &str) {
        if conn.is_empty() || self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        match self.host.restart_link(conn) {
            Ok(()) => lock(&self.log).push(format!("sik: MAVLink bridge restarted on {conn}")),
            Err(e) => lock(&self.log).push(format!("sik: restarting the MAVLink bridge failed: {e}")),
        }
    }

    /// Reads both ends of the link and reports where they disagree.
    pub fn load(
        &self,
        device: &str,
        baud: Option<i64>,
        include_remote: bool,
    ) -> Result<Map<String, Value>, SikFailure> {
        self.run(device, baud, &mut |session, _| {
            let Some(local) = session.read_radio(false)? else {
                return Err(sik(
                    "the radio answered the escape but reported no settings. Its firmware may not support ATI5.",
                ));
            };
            let remote = if include_remote {
                session.read_radio(true)?
            } else {
                None
            };
            let mut out = Map::new();
            out.insert("local".into(), local.clone());
            out.insert("remote".into(), remote.clone().unwrap_or(Value::Null));
            out.insert("remote_reachable".into(), json!(remote.is_some()));
            out.insert("mismatches".into(), json!(config::compare(&local, remote.as_ref())));
            out.insert("link".into(), session.read_link_quality()?.unwrap_or(Value::Null));
            Ok(out)
        })
    }

    /// Writes settings to the far radio first, then the near one: the remote
    /// is reachable only through the local one and only while the two agree on
    /// the air settings, so writing the local end first could sever the path
    /// to a radio that has not been told.
    pub fn save(
        &self,
        device: &str,
        baud: Option<i64>,
        local: Option<&Map<String, Value>>,
        remote: Option<&Map<String, Value>>,
    ) -> Result<Map<String, Value>, SikFailure> {
        let result = self.run(device, baud, &mut |session, _| {
            let mut applied_remote: Vec<Value> = Vec::new();
            let mut applied_local: Vec<Value> = Vec::new();
            let mut warnings: Vec<Value> = Vec::new();
            if let Some(remote) = remote.filter(|r| !r.is_empty()) {
                let Some(current) = session.read_radio(true)? else {
                    return Err(sik(
                        "the remote radio is not answering, so its settings cannot be written. Power the aircraft and wait for a solid green LED on both radios.",
                    ));
                };
                let writes = config::validate_writes(remote, &config::registers_of(&current))?;
                session.apply(&writes, true)?;
                applied_remote = writes;
            }
            let mut new_serial_speed = 0;
            if let Some(local) = local.filter(|l| !l.is_empty()) {
                let Some(current) = session.read_radio(false)? else {
                    return Err(sik("the local radio stopped reporting its settings"));
                };
                let writes = config::validate_writes(local, &config::registers_of(&current))?;
                session.apply(&writes, false)?;
                for w in &writes {
                    if w["name"] == "SERIAL_SPEED" {
                        new_serial_speed = config::baud_from_register(w["value"].as_i64().unwrap_or(0));
                    }
                }
                applied_local = writes;
            }
            if new_serial_speed != 0 {
                warnings.push(json!(format!(
                    "This radio now talks to its cable at {new_serial_speed} baud. Corvus has moved the link to match; the radio at the other end keeps its own baud, which must still match the autopilot's telemetry port."
                )));
            }
            let mut out = Map::new();
            out.insert("applied".into(), json!({"remote": applied_remote, "local": applied_local}));
            out.insert("warnings".into(), Value::Array(warnings));
            out.insert("new_serial_speed".into(), json!(new_serial_speed));
            Ok(out)
        })?;
        // Reconnect at the new baud when the operator just changed the speed of
        // the wire this link runs over.
        let speed = result.get("new_serial_speed").and_then(Value::as_i64).unwrap_or(0);
        if speed != 0 {
            let dev = result
                .get("device")
                .and_then(Value::as_str)
                .unwrap_or(device)
                .to_string();
            self.retune_link(&dev, speed);
        }
        Ok(result)
    }

    /// Factory defaults for one end of the link (`AT&F` / `RT&F`).
    pub fn reset(&self, device: &str, baud: Option<i64>, target: &str) -> Result<Map<String, Value>, SikFailure> {
        if target != "local" && target != "remote" {
            return Err(sik("target must be 'local' or 'remote'"));
        }
        self.run(device, baud, &mut |session, _| {
            if target == "remote" && session.read_radio(true)?.is_none() {
                return Err(sik("the remote radio is not answering"));
            }
            session.factory_reset(target == "remote")?;
            let mut out = Map::new();
            out.insert("target".into(), json!(target));
            Ok(out)
        })
    }

    /// Moves the live link to a new baud after the local radio changed speed.
    fn retune_link(&self, device: &str, baud: i64) {
        let conn = self.host.connection_string();
        let link_device = if conn.starts_with("serial:") {
            ports::split_serial_connection(&conn).0
        } else {
            String::new()
        };
        if !same_device(device, &link_device) {
            return;
        }
        self.host.stop_link();
        self.restart_bridge(&format!("serial:{device}:{baud}"));
    }

    /// Stops restarting the bridge; a session in flight finishes its close.
    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }
}
