//! Second station forwarding (`mavlink_forwarder.py`): every frame the
//! vehicle link receives is mirrored to UDP endpoints (QGroundControl on
//! 127.0.0.1:14550 by default), and, when the operator allows it, whole frames
//! from the configured endpoints are put on the vehicle's uplink.
//!
//! The receive loop only appends to a bounded drop-oldest queue that a sender
//! thread drains, so a wedged endpoint never stalls the link to the aircraft.
//! Inbound datagrams are split into whole MAVLink frames before anything is
//! injected. An endpoint is learned only on a whole frame, may command only
//! when the operator named it, and the vehicle link's own socket is never
//! mirrored to. Two threads, one socket, all released by [`Forwarder::stop`].

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use socket2::{Domain, Protocol, Socket, Type};

use crate::clock::{Clock, SystemClock};

pub const DEFAULT_PORT: u16 = 14550;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_LISTEN_HOST: &str = "127.0.0.1";
pub const DEFAULT_LISTEN_PORT: u16 = 14551;

const STX_V1: u8 = 0xFE;
const STX_V2: u8 = 0xFD;
const V1_OVERHEAD: usize = 8;
const V2_OVERHEAD: usize = 12;
const V2_SIGNATURE_LEN: usize = 13;
const IFLAG_SIGNED: u8 = 0x01;
const MAX_FRAME: usize = 280;
const MAX_QUEUE: usize = 2000;
const MAX_RX_BUFFERS: usize = 64;
/// The system id Corvus itself transmits under (`mavlink_bridge.GCS_SYSTEM_ID`).
const CORVUS_SYSTEM_ID: u8 = 254;

type Addr = (String, u16);

fn text(addr: &Addr) -> String {
    format!("{}:{}", addr.0, addr.1)
}

/// Python's `int(text)` for the digits an operator could type: a sign, spaces
/// around, single underscores between digits.
fn py_int(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if !clean.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let value: i64 = clean.parse().ok()?;
    Some(if negative { -value } else { value })
}

/// `"host:port"` or `"host"` as `(host, port)`; None when unusable. One bad
/// line costs that line, not the whole feature.
pub fn parse_endpoint(raw: &str, default_port: u16) -> Option<(String, u16)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (host, port_text) = match raw.rfind(':') {
        Some(i) if i > 0 => (&raw[..i], &raw[i + 1..]),
        _ => (raw, ""),
    };
    let port = if port_text.is_empty() {
        i64::from(default_port)
    } else {
        py_int(port_text)?
    };
    if !(0 < port && port < 65536) || host.is_empty() {
        return None;
    }
    Some((host.to_string(), port as u16))
}

/// Whole MAVLink frames in `buf` by length and framing, and what is left. The
/// CRC belongs to the link crate; this only keeps a truncated or garbage
/// datagram from putting a partial frame on the uplink.
pub fn split_frames(buf: &[u8]) -> (Vec<&[u8]>, &[u8]) {
    let mut frames = Vec::new();
    let mut i = 0;
    let n = buf.len();
    while i < n {
        let total = match buf[i] {
            STX_V1 => {
                if n - i < 2 {
                    break;
                }
                buf[i + 1] as usize + V1_OVERHEAD
            }
            STX_V2 => {
                if n - i < 3 {
                    break;
                }
                let mut total = buf[i + 1] as usize + V2_OVERHEAD;
                if buf[i + 2] & IFLAG_SIGNED != 0 {
                    total += V2_SIGNATURE_LEN;
                }
                total
            }
            _ => {
                i += 1;
                continue;
            }
        };
        if total > MAX_FRAME {
            i += 1;
            continue;
        }
        if n - i < total {
            break;
        }
        frames.push(&buf[i..i + total]);
        i += total;
    }
    (frames, &buf[i..])
}

/// The system id a whole frame was sent under, 0 when unreadable.
pub fn frame_source_system(frame: &[u8]) -> u8 {
    if frame.len() >= 6 && frame[0] == STX_V1 {
        return frame[3];
    }
    if frame.len() >= 8 && frame[0] == STX_V2 {
        return frame[5];
    }
    0
}

/// What the OS says for an I/O error, without the "(os error N)" Rust adds.
fn strerror(err: &std::io::Error) -> String {
    let full = err.to_string();
    match full.rfind(" (os error") {
        Some(i) => full[..i].to_string(),
        None => full,
    }
}

#[cfg(unix)]
fn machine_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is as long as the length passed.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

#[cfg(not(unix))]
fn machine_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

/// This machine's own IPv4 addresses, as far as the system can tell.
fn local_host_addresses() -> Vec<String> {
    let Some(name) = machine_name() else { return Vec::new() };
    (name.as_str(), 0)
        .to_socket_addrs()
        .map(|it| {
            it.filter_map(|a| {
                if let IpAddr::V4(v4) = a.ip() {
                    Some(v4.to_string())
                } else {
                    None
                }
            })
            .collect()
        })
        .unwrap_or_default()
}

/// Where the log lines go (`warning`, `info`).
pub type Logger = Arc<dyn Fn(&str, &str) + Send + Sync>;
pub type Inject = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;
/// Where the vehicle link itself is bound, asked afresh each time.
pub type LinkAddress = Arc<dyn Fn() -> Option<(String, u16)> + Send + Sync>;

#[derive(Clone)]
pub struct ForwarderConfig {
    pub host: String,
    pub port: u16,
    pub listen_host: String,
    /// 0 means any free port.
    pub listen_port: u16,
    pub endpoints: Vec<String>,
    pub allow_commands: bool,
    /// An endpoint that said nothing for this long is forgotten.
    pub peer_ttl_s: f64,
    /// How long a name that would not resolve is left alone.
    pub resolve_retry_s: f64,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        ForwarderConfig {
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
            listen_host: DEFAULT_LISTEN_HOST.into(),
            listen_port: DEFAULT_LISTEN_PORT,
            endpoints: Vec::new(),
            allow_commands: false,
            peer_ttl_s: 30.0,
            resolve_retry_s: 30.0,
        }
    }
}

struct State {
    sock: Option<Arc<UdpSocket>>,
    bound: Option<Addr>,
    resolved: Vec<Addr>,
    unresolved: Vec<Addr>,
    resolved_at: f64,
    peers: Vec<(Addr, f64)>,
    local_hosts: Vec<String>,
    error: String,
    notice: String,
    sent: u64,
    received: u64,
    injected: u64,
    dropped: u64,
    sysid_conflict: bool,
    refused: u64,
    refused_peers: BTreeSet<Addr>,
    loop_blocked: Option<Addr>,
}

struct Inner {
    cfg: ForwarderConfig,
    static_eps: Vec<Addr>,
    inject: Option<Inject>,
    link_address: Option<LinkAddress>,
    logger: Logger,
    clock: Arc<dyn Clock>,
    st: Mutex<State>,
    out: Mutex<VecDeque<Vec<u8>>>,
    wake: Condvar,
    stopping: AtomicBool,
    rx_buf: Mutex<HashMap<Addr, Vec<u8>>>,
}

const DRAIN: Duration = Duration::from_millis(200);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Mirrors the vehicle link onto UDP endpoints; optionally accepts theirs.
pub struct Forwarder {
    inner: Arc<Inner>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Forwarder {
    pub fn new(
        cfg: ForwarderConfig,
        inject: Option<Inject>,
        link_address: Option<LinkAddress>,
        logger: Logger,
    ) -> Forwarder {
        Forwarder::with_clock(cfg, inject, link_address, logger, Arc::new(SystemClock))
    }

    pub fn with_clock(
        mut cfg: ForwarderConfig,
        inject: Option<Inject>,
        link_address: Option<LinkAddress>,
        logger: Logger,
        clock: Arc<dyn Clock>,
    ) -> Forwarder {
        if cfg.host.is_empty() {
            cfg.host = DEFAULT_HOST.into();
        }
        if cfg.port == 0 {
            cfg.port = DEFAULT_PORT;
        }
        if cfg.listen_host.is_empty() {
            cfg.listen_host = DEFAULT_LISTEN_HOST.into();
        }
        let own = (cfg.listen_host.clone(), cfg.listen_port);
        let mut static_eps: Vec<Addr> = Vec::new();
        let items = std::iter::once(format!("{}:{}", cfg.host, cfg.port)).chain(cfg.endpoints.iter().cloned());
        for item in items {
            let Some(parsed) = parse_endpoint(&item, DEFAULT_PORT) else {
                logger("warning", &format!("ignoring unusable forward endpoint '{item}'"));
                continue;
            };
            if static_eps.contains(&parsed) {
                continue;
            }
            if parsed == own {
                logger(
                    "warning",
                    &format!("ignoring forward endpoint '{item}': it is this forwarder's own address"),
                );
                continue;
            }
            static_eps.push(parsed);
        }
        let st = State {
            sock: None,
            bound: None,
            resolved: Vec::new(),
            unresolved: Vec::new(),
            resolved_at: 0.0,
            peers: Vec::new(),
            local_hosts: Vec::new(),
            error: String::new(),
            notice: String::new(),
            sent: 0,
            received: 0,
            injected: 0,
            dropped: 0,
            sysid_conflict: false,
            refused: 0,
            refused_peers: BTreeSet::new(),
            loop_blocked: None,
        };
        Forwarder {
            inner: Arc::new(Inner {
                cfg,
                static_eps,
                inject,
                link_address,
                logger,
                clock,
                st: Mutex::new(st),
                out: Mutex::new(VecDeque::new()),
                wake: Condvar::new(),
                stopping: AtomicBool::new(false),
                rx_buf: Mutex::new(HashMap::new()),
            }),
            threads: Mutex::new(Vec::new()),
        }
    }

    /// Binds and runs. False (with the error set) only if nothing binds.
    pub fn start(&self) -> bool {
        let inner = &self.inner;
        if lock(&inner.st).sock.is_some() {
            return true;
        }
        let (sock, bound, note) = inner.bind();
        let Some(sock) = sock else {
            let error = lock(&inner.st).error.clone();
            (inner.logger)("warning", &format!("MAVLink forwarding disabled: {error}"));
            return false;
        };
        {
            let mut st = lock(&inner.st);
            st.sock = Some(Arc::new(sock));
            st.bound = Some(bound.clone());
            st.error.clear();
            st.notice = note;
            st.sysid_conflict = false;
            st.refused = 0;
            st.refused_peers.clear();
            st.loop_blocked = None;
            st.local_hosts = local_host_addresses();
        }
        inner.resolve_static(true);
        inner.stopping.store(false, Ordering::SeqCst);
        let mut threads = lock(&self.threads);
        for (name, rx) in [("mav-forward-rx", true), ("mav-forward-tx", false)] {
            let inner = Arc::clone(&self.inner);
            if let Ok(handle) = std::thread::Builder::new().name(name.into()).spawn(move || {
                if rx {
                    inner.rx_loop();
                } else {
                    inner.tx_loop();
                }
            }) {
                threads.push(handle);
            }
        }
        let targets = if inner.static_eps.is_empty() {
            "nothing".to_string()
        } else {
            inner.static_eps.iter().map(text).collect::<Vec<_>>().join(", ")
        };
        (inner.logger)(
            "info",
            &format!(
                "MAVLink forwarding: mirroring to {targets}, listening on {}:{} (commands from other stations: {})",
                bound.0,
                bound.1,
                if inner.cfg.allow_commands { "allowed" } else { "blocked" }
            ),
        );
        true
    }

    /// A forwarder on `bridge`: its frames mirrored out, a configured
    /// endpoint's commands injected, and the bridge's own listening socket
    /// never mirrored into. Call [`Forwarder::attach`] to start the mirror.
    pub fn for_bridge(cfg: ForwarderConfig, bridge: &crate::bridge::Bridge, logger: Logger) -> Arc<Forwarder> {
        let inject: Inject = {
            let bridge = bridge.clone();
            Arc::new(move |frame: &[u8]| bridge.inject_raw(frame))
        };
        let link_address: LinkAddress = {
            let bridge = bridge.clone();
            Arc::new(move || bridge.local_udp_address())
        };
        Arc::new(Forwarder::new(cfg, Some(inject), Some(link_address), logger))
    }

    /// Sends every raw frame the bridge receives to this forwarder.
    pub fn attach(self: &Arc<Self>, bridge: &crate::bridge::Bridge) {
        let fwd = Arc::clone(self);
        bridge.set_frame_sink(Some(Arc::new(move |raw: &[u8]| fwd.feed(raw))));
    }

    /// Stops sending frames here. Detach before the forwarder is dropped: the
    /// bridge's sink holds it.
    pub fn detach(bridge: &crate::bridge::Bridge) {
        bridge.set_frame_sink(None);
    }

    /// Idempotent teardown: threads joined, socket closed, queue dropped.
    pub fn stop(&self) {
        let inner = &self.inner;
        inner.stopping.store(true, Ordering::SeqCst);
        lock(&inner.out).clear();
        inner.wake.notify_all();
        lock(&inner.st).sock = None;
        for handle in lock(&self.threads).drain(..) {
            let _ = handle.join();
        }
        let mut st = lock(&inner.st);
        st.peers.clear();
        st.bound = None;
        drop(st);
        lock(&inner.rx_buf).clear();
    }

    /// Queues one raw frame from the aircraft. Never blocks.
    pub fn feed(&self, raw: &[u8]) {
        let inner = &self.inner;
        if inner.stopping.load(Ordering::SeqCst) || raw.is_empty() || lock(&inner.st).sock.is_none() {
            return;
        }
        let mut out = lock(&inner.out);
        let full = out.len() >= MAX_QUEUE;
        if full {
            out.pop_front();
        }
        out.push_back(raw.to_vec());
        drop(out);
        if full {
            lock(&inner.st).dropped += 1;
        }
        inner.wake.notify_one();
    }

    pub fn error(&self) -> String {
        lock(&self.inner.st).error.clone()
    }

    pub fn notice(&self) -> String {
        lock(&self.inner.st).notice.clone()
    }

    /// The address actually bound, which is not always the one asked for.
    pub fn listen_address(&self) -> (String, u16) {
        self.inner.listen_address()
    }

    /// What the LINK tab shows.
    pub fn status(&self) -> Value {
        let inner = &self.inner;
        let (listen_host, listen_port) = inner.listen_address();
        let peers = inner.live_peers();
        let targets = inner.targets();
        let st = lock(&inner.st);
        let running = st.sock.is_some() && !inner.stopping.load(Ordering::SeqCst);
        json!({
            "running": running,
            "host": inner.cfg.host,
            "port": inner.cfg.port,
            "listen_host": listen_host,
            "listen_port": listen_port,
            "listen_port_requested": inner.cfg.listen_port,
            "allow_commands": inner.cfg.allow_commands,
            "endpoints": inner.static_eps.iter().map(text).collect::<Vec<_>>(),
            "peers": peers.iter().map(text).collect::<Vec<_>>(),
            "targets": targets.iter().map(text).collect::<Vec<_>>(),
            "frames_sent": st.sent,
            "datagrams_received": st.received,
            "frames_injected": st.injected,
            "dropped": st.dropped,
            "sysid_conflict": st.sysid_conflict,
            "commands_refused": st.refused,
            "commands_refused_from": st.refused_peers.iter().map(text).collect::<Vec<_>>(),
            "loop_blocked": st.loop_blocked.as_ref().map(text).unwrap_or_default(),
            "notice": if st.notice.is_empty() { Self::loop_text(&st) } else { st.notice.clone() },
            "error": st.error,
        })
    }

    fn loop_text(st: &State) -> String {
        match &st.loop_blocked {
            None => String::new(),
            Some((host, port)) => format!(
                "Not mirroring to {host}:{port}: Corvus receives the vehicle on that port, so every frame would come straight back. Choose another forwarding port, for example 14560, and set the other station's UDP link to it."
            ),
        }
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Inner {
    fn listen_address(&self) -> Addr {
        lock(&self.st)
            .bound
            .clone()
            .unwrap_or((self.cfg.listen_host.clone(), self.cfg.listen_port))
    }

    fn bind(&self) -> (Option<UdpSocket>, Addr, String) {
        let requested = self.cfg.listen_port;
        for (port, fallback) in [(requested, false), (0, true)] {
            if fallback && requested == 0 {
                break;
            }
            match self.open(port) {
                Ok(sock) => {
                    let bound = sock
                        .local_addr()
                        .map(|a| (a.ip().to_string(), a.port()))
                        .unwrap_or((self.cfg.listen_host.clone(), port));
                    let mut note = String::new();
                    if fallback {
                        note = format!(
                            "port {requested} was busy, listening on {}:{} instead",
                            bound.0, bound.1
                        );
                        (self.logger)("warning", &format!("MAVLink forwarding: {note}"));
                    }
                    return (Some(sock), bound, note);
                }
                Err(err) => {
                    lock(&self.st).error = format!(
                        "cannot listen on {}:{} ({})",
                        self.cfg.listen_host,
                        port,
                        strerror(&err)
                    );
                }
            }
        }
        let error = lock(&self.st).error.clone();
        (None, (self.cfg.listen_host.clone(), requested), error)
    }

    fn open(&self, port: u16) -> std::io::Result<UdpSocket> {
        let ip: Ipv4Addr = self.cfg.listen_host.parse().or_else(|_| {
            (self.cfg.listen_host.as_str(), port)
                .to_socket_addrs()?
                .find_map(|a| if let IpAddr::V4(v4) = a.ip() { Some(v4) } else { None })
                .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "no IPv4 address"))
        })?;
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::new(IpAddr::V4(ip), port).into())?;
        let sock: UdpSocket = socket.into();
        sock.set_read_timeout(Some(DRAIN))?;
        Ok(sock)
    }

    /// Turns the static endpoints' host names into addresses, once; afterwards
    /// only the names that failed, at most every `resolve_retry_s`.
    fn resolve_static(&self, force: bool) {
        let now = self.clock.monotonic();
        let (pending, mut keep) = {
            let st = lock(&self.st);
            if !force && (st.unresolved.is_empty() || now - st.resolved_at < self.cfg.resolve_retry_s) {
                return;
            }
            if force {
                (self.static_eps.clone(), Vec::new())
            } else {
                (st.unresolved.clone(), st.resolved.clone())
            }
        };
        let mut unresolved: Vec<Addr> = Vec::new();
        for (host, port) in pending {
            let found = (host.as_str(), port)
                .to_socket_addrs()
                .ok()
                .and_then(|mut it| it.find(|a| a.is_ipv4()));
            match found {
                None => {
                    (self.logger)("warning", &format!("forward endpoint {host}:{port} does not resolve"));
                    unresolved.push((host, port));
                }
                Some(addr) => {
                    let entry = (addr.ip().to_string(), addr.port());
                    if !keep.contains(&entry) {
                        keep.push(entry);
                    }
                }
            }
        }
        let mut st = lock(&self.st);
        st.resolved = keep;
        st.unresolved = unresolved;
        st.resolved_at = now;
    }

    fn live_peers(&self) -> Vec<Addr> {
        let now = self.clock.monotonic();
        let mut st = lock(&self.st);
        let live: Vec<(Addr, f64)> = st
            .peers
            .iter()
            .filter(|(_, seen)| now - seen <= self.cfg.peer_ttl_s)
            .cloned()
            .collect();
        let addrs = live.iter().map(|(a, _)| a.clone()).collect();
        if live.len() != st.peers.len() {
            st.peers = live;
        }
        addrs
    }

    fn targets(&self) -> Vec<Addr> {
        let (bound, resolved) = {
            let st = lock(&self.st);
            (st.bound.clone(), st.resolved.clone())
        };
        let mut out: Vec<Addr> = Vec::new();
        for addr in resolved.into_iter().chain(self.live_peers()) {
            if out.contains(&addr) || Some(&addr) == bound.as_ref() {
                continue;
            }
            if self.is_vehicle_link(&addr) {
                self.note_loop(&addr);
                continue;
            }
            out.push(addr);
        }
        out
    }

    /// Is `addr` the socket Corvus receives the aircraft on? SITL and
    /// QGroundControl both default to UDP 14550, so accepting both defaults
    /// would mirror every frame straight back onto the vehicle link.
    fn is_vehicle_link(&self, addr: &Addr) -> bool {
        let Some(getter) = &self.link_address else { return false };
        let Some((host, port)) = getter() else { return false };
        if addr.1 != port {
            return false;
        }
        let peer = addr.0.as_str();
        if host.is_empty() || host == "0.0.0.0" {
            return peer.starts_with("127.") || lock(&self.st).local_hosts.iter().any(|h| h == peer);
        }
        if peer == host {
            return true;
        }
        peer.starts_with("127.") && host.starts_with("127.")
    }

    fn note_loop(&self, addr: &Addr) {
        {
            let mut st = lock(&self.st);
            if st.loop_blocked.as_ref() == Some(addr) {
                return;
            }
            st.loop_blocked = Some(addr.clone());
        }
        (self.logger)(
            "warning",
            &format!(
                "not forwarding to {}:{}: that is the port Corvus receives the vehicle on, so every frame would come straight back. Set forwarding to another port (for example 14560) and point the other station's UDP link at it.",
                addr.0, addr.1
            ),
        );
    }

    fn tx_loop(&self) {
        while !self.stopping.load(Ordering::SeqCst) {
            let batch: Vec<Vec<u8>> = {
                let mut out = lock(&self.out);
                if out.is_empty() {
                    let (guard, _) = self.wake.wait_timeout(out, DRAIN).unwrap_or_else(|e| e.into_inner());
                    drop(guard);
                    continue;
                }
                out.drain(..).collect()
            };
            let Some(sock) = lock(&self.st).sock.clone() else {
                continue;
            };
            let retry = !lock(&self.st).unresolved.is_empty();
            if retry {
                self.resolve_static(false);
            }
            let targets = self.targets();
            if targets.is_empty() {
                continue;
            }
            for frame in &batch {
                for (host, port) in &targets {
                    let sent = host.parse::<Ipv4Addr>().map_err(|_| ()).and_then(|ip| {
                        sock.send_to(frame, SocketAddr::new(IpAddr::V4(ip), *port))
                            .map_err(|_| ())
                    });
                    let mut st = lock(&self.st);
                    if sent.is_ok() {
                        st.sent += 1;
                    } else {
                        st.dropped += 1;
                    }
                }
            }
        }
    }

    fn rx_loop(&self) {
        let mut buf = vec![0u8; 65535];
        while !self.stopping.load(Ordering::SeqCst) {
            let Some(sock) = lock(&self.st).sock.clone() else { break };
            let (len, from) = match sock.recv_from(&mut buf) {
                Ok(got) => got,
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
                Err(_) => {
                    if self.stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if len == 0 {
                continue;
            }
            let addr: Addr = (from.ip().to_string(), from.port());
            self.take_datagram(&addr, &buf[..len]);
        }
    }

    fn take_datagram(&self, addr: &Addr, data: &[u8]) {
        if lock(&self.st).bound.as_ref() == Some(addr) {
            return;
        }
        if self.is_vehicle_link(addr) {
            self.note_loop(addr);
            return;
        }
        lock(&self.st).received += 1;
        let frames: Vec<Vec<u8>> = {
            let mut rx = lock(&self.rx_buf);
            let mut buffered = rx.get(addr).cloned().unwrap_or_default();
            buffered.extend_from_slice(data);
            let (frames, remainder) = split_frames(&buffered);
            let frames: Vec<Vec<u8>> = frames.into_iter().map(<[u8]>::to_vec).collect();
            if !remainder.is_empty() && remainder.len() < MAX_FRAME {
                rx.insert(addr.clone(), remainder.to_vec());
            } else {
                rx.remove(addr);
            }
            frames
        };
        if lock(&self.rx_buf).len() > MAX_RX_BUFFERS {
            self.drop_stale_rx_buffers();
        }
        if frames.is_empty() {
            return;
        }
        let now = self.clock.monotonic();
        let may_command = {
            let mut st = lock(&self.st);
            match st.peers.iter_mut().find(|(a, _)| a == addr) {
                Some(entry) => entry.1 = now,
                None => st.peers.push((addr.clone(), now)),
            }
            self.cfg.allow_commands && st.resolved.contains(addr)
        };
        if self.cfg.allow_commands && !may_command {
            self.note_command_refused(addr);
        }
        for frame in &frames {
            if frame_source_system(frame) == CORVUS_SYSTEM_ID {
                self.note_sysid_conflict();
                continue;
            }
            let Some(inject) = (if may_command { self.inject.as_ref() } else { None }) else {
                continue;
            };
            if inject(frame) {
                lock(&self.st).injected += 1;
            }
        }
    }

    fn drop_stale_rx_buffers(&self) {
        let now = self.clock.monotonic();
        let live: Vec<Addr> = lock(&self.st)
            .peers
            .iter()
            .filter(|(_, seen)| now - seen <= self.cfg.peer_ttl_s)
            .map(|(a, _)| a.clone())
            .collect();
        lock(&self.rx_buf).retain(|a, _| live.contains(a));
    }

    fn note_command_refused(&self, addr: &Addr) {
        let first = {
            let mut st = lock(&self.st);
            st.refused += 1;
            st.refused_peers.insert(addr.clone())
        };
        if !first {
            return;
        }
        let configured = if self.static_eps.is_empty() {
            "none".to_string()
        } else {
            self.static_eps.iter().map(text).collect::<Vec<_>>().join(", ")
        };
        (self.logger)(
            "warning",
            &format!(
                "refusing to forward commands from {}:{}. Commanding is allowed only from the configured endpoints ({configured}). Add it to the forwarding endpoint list if this station should be able to command the aircraft.",
                addr.0, addr.1
            ),
        );
    }

    fn note_sysid_conflict(&self) {
        {
            let mut st = lock(&self.st);
            if st.sysid_conflict {
                return;
            }
            st.sysid_conflict = true;
        }
        (self.logger)(
            "warning",
            &format!(
                "the other station is transmitting as MAVLink system {CORVUS_SYSTEM_ID}, the same id Corvus uses. Give it its own (QGroundControl: Application Settings \u{2192} MAVLink \u{2192} Ground Station system ID)"
            ),
        );
    }
}
