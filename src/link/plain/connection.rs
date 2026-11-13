//! A link to a vehicle as plain Rust: open it from a connection string, read
//! the frames and messages that arrive, send messages, and keep the state of
//! every system heard on it.
//!
//! The connection strings are `mavlink_connection`'s: `udp:host:port` (binds
//! when `Options::input`), `udpin:`, `udpout:`, `udpbcast:`, `tcp:`, `tcpin:`,
//! `mcast:`, `uds:`/`unix:` (Unix only), a serial device (`/dev/ttyACM0`,
//! `COM3`, `device,baud`) and a bare `host:port`. A WebSocket, a log file and
//! a program to run belong to other layers and are refused here by name.
//!
//! No thread is started: a caller reads in its own loop (`recv` waits at most
//! the time it is given), so the thread that does it is the caller's to join.
//! A lost TCP or Unix socket is reported as an error and reopened by the
//! caller, whose reconnect rule (backoff, the connection cycle) is its own.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use super::codec::CodecError;
use super::frame::{encode, Event, Frame, Parser, Proto, Signing};
use super::message::{Message, Value};
use super::time::now;
use crate::link::transport::{Mcast, Serial, Tcp, TcpEvent, TcpIn, Udp};

/// What a connection is opened with.
#[derive(Debug, Clone)]
pub struct Options {
    pub source_system: u8,
    pub source_component: u8,
    /// For a serial device given without `,baud`.
    pub baud: u32,
    /// Whether `udp:host:port` binds (a ground station listening) or sends to.
    pub input: bool,
    /// How long a UDP client may be silent before it is dropped, 0 for never.
    pub udp_timeout: f64,
    /// Attempts in all for a TCP or Unix socket.
    pub retries: i64,
    pub reconnect_delay: f64,
    /// The wire format sent. MAVLink 2 unless the peer is known to speak 1.
    pub proto: Proto,
    /// DTR and RTS of a serial port: Some holds the line at that state from
    /// the moment the port opens, None leaves it asserted as usual.
    pub dtr: Option<bool>,
    pub rts: Option<bool>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            source_system: 255,
            source_component: 0,
            baud: 115_200,
            input: true,
            udp_timeout: 0.0,
            retries: 3,
            reconnect_delay: 1.0,
            proto: Proto::V2,
            dtr: None,
            rts: None,
        }
    }
}

/// Where a connection string points.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Udp {
        host: String,
        port: u16,
        input: bool,
        broadcast: bool,
    },
    Tcp {
        host: String,
        port: u16,
    },
    TcpIn {
        host: String,
        port: u16,
    },
    Uds(String),
    Mcast {
        group: String,
        port: u16,
    },
    Serial {
        device: String,
        baud: u32,
    },
}

#[derive(Debug)]
pub enum ConnectError {
    /// The string is not a connection (pymavlink's `ValueError`).
    Invalid(String),
    /// Something this layer does not open.
    Unsupported(String),
    /// The socket or port would not open.
    Io(io::Error),
    /// A serial port would not open, in pyserial's words.
    Serial { errno: Option<i32>, message: String },
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Invalid(m) | ConnectError::Unsupported(m) => f.write_str(m),
            ConnectError::Io(e) => e.fmt(f),
            ConnectError::Serial { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<io::Error> for ConnectError {
    fn from(e: io::Error) -> Self {
        ConnectError::Io(e)
    }
}

fn host_port(rest: &str, what: &str) -> Result<(String, u16), ConnectError> {
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() != 2 {
        return Err(ConnectError::Invalid(format!(
            "{what} ports must be specified as host:port"
        )));
    }
    let port: i64 = parts[1]
        .trim()
        .parse()
        .map_err(|_| ConnectError::Invalid(format!("invalid literal for int() with base 10: '{}'", parts[1])))?;
    let port = u16::try_from(port).map_err(|_| ConnectError::Invalid("bind(): port must be 0-65535.".into()))?;
    Ok((parts[0].to_string(), port))
}

/// Reads a connection string the way `mavlink_connection` does.
pub fn parse_target(device: &str, opts: &Options) -> Result<Target, ConnectError> {
    let udp = |rest: &str, input: bool, broadcast: bool| {
        host_port(rest, "UDP").map(|(host, port)| Target::Udp {
            host,
            port,
            input,
            broadcast,
        })
    };
    if let Some(rest) = device.strip_prefix("tcp:") {
        return host_port(rest, "TCP").map(|(host, port)| Target::Tcp { host, port });
    }
    if let Some(rest) = device.strip_prefix("tcpin:") {
        return host_port(rest, "TCP").map(|(host, port)| Target::TcpIn { host, port });
    }
    if let Some(rest) = device.strip_prefix("udpin:") {
        return udp(rest, true, false);
    }
    if let Some(rest) = device.strip_prefix("udpout:") {
        return udp(rest, false, false);
    }
    if let Some(rest) = device.strip_prefix("udpbcast:") {
        return udp(rest, false, true);
    }
    if let Some(rest) = device.strip_prefix("udp:") {
        return udp(rest, opts.input, false);
    }
    for prefix in ["uds:", "unix:"] {
        if let Some(path) = device.strip_prefix(prefix) {
            if path.is_empty() {
                return Err(ConnectError::Invalid(
                    "Unix domain socket path must be specified".into(),
                ));
            }
            return Ok(Target::Uds(path.to_string()));
        }
    }
    if let Some(rest) = device.strip_prefix("mcast:") {
        let parts: Vec<&str> = rest.split(':').collect();
        let (mut group, mut port) = ("239.255.145.50".to_string(), 14550u16);
        let parse = |p: &str| {
            p.trim()
                .parse::<u16>()
                .map_err(|_| ConnectError::Invalid(format!("invalid literal for int() with base 10: '{p}'")))
        };
        if parts.len() == 1 && !parts[0].is_empty() {
            port = parse(parts[0])?;
        } else if parts.len() > 1 {
            group = parts[0].to_string();
            port = parse(parts[1])?;
        }
        return Ok(Target::Mcast { group, port });
    }
    if device.starts_with("wsserver:") || device.starts_with("ws:") || device.starts_with("wss:") {
        return Err(ConnectError::Unsupported(format!(
            "{device}: a WebSocket link is not part of the plain connection"
        )));
    }
    let lower = device.to_lowercase();
    let suffix = lower.rsplit('.').next().unwrap_or("");
    if device.contains(':') && !["mavlink", "log", "raw", "tlog"].contains(&suffix) {
        return udp(device, opts.input, false);
    }
    if std::path::Path::new(device).is_file() {
        return Err(ConnectError::Unsupported(format!(
            "{device}: a log file or a program is not a link"
        )));
    }
    let (mut dev, mut baud) = (device.to_string(), opts.baud);
    if device.contains(',') && !std::path::Path::new(device).exists() {
        let (d, b) = device.split_once(',').unwrap_or((device, ""));
        baud = b
            .trim()
            .parse()
            .map_err(|_| ConnectError::Invalid(format!("invalid baud rate '{b}'")))?;
        dev = d.to_string();
    }
    Ok(Target::Serial { device: dev, baud })
}

enum Link {
    Udp(Udp),
    Tcp(Tcp),
    TcpIn(TcpIn),
    #[cfg(unix)]
    Uds(crate::link::transport::Uds),
    Mcast(Mcast),
    Serial(Serial),
}

/// What is known of one system heard on the link, from its HEARTBEATs.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemInfo {
    pub sysid: u8,
    pub compid: u8,
    pub mav_type: i64,
    pub autopilot: i64,
    pub base_mode: i64,
    pub custom_mode: i64,
    pub system_status: i64,
    /// Wall clock seconds of the last HEARTBEAT.
    pub last_heartbeat: f64,
    pub heartbeats: u64,
    /// Every message from this system.
    pub messages: u64,
}

/// One frame that arrived, with its message when it decodes.
#[derive(Debug)]
pub struct Received {
    pub frame: Frame,
    pub message: Result<Message, CodecError>,
    /// Wall clock seconds when it was read.
    pub time: f64,
}

/// What the receive path counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub bytes_received: u64,
    pub frames_received: u64,
    pub frames_sent: u64,
    pub bytes_sent: u64,
    pub crc_errors: u64,
    pub unknown_messages: u64,
    pub garbage_bytes: u64,
    pub unsigned_dropped: u64,
    pub bad_signatures: u64,
}

pub struct Connection {
    link: Link,
    description: String,
    parser: Mutex<Parser>,
    seq: AtomicU8,
    v2: AtomicBool,
    source_system: u8,
    source_component: u8,
    signing: Mutex<Option<Signing>>,
    /// The last signature timestamp accepted from each signed stream
    /// (system, component, link id), against a recorded frame sent again.
    signed_streams: Mutex<HashMap<(u8, u8, u8), u64>>,
    systems: Mutex<BTreeMap<u8, SystemInfo>>,
    counters: [AtomicU64; 9],
    closed: AtomicBool,
}

const BYTES_RX: usize = 0;
const FRAMES_RX: usize = 1;
const FRAMES_TX: usize = 2;
const BYTES_TX: usize = 3;
const CRC: usize = 4;
const UNKNOWN: usize = 5;
const GARBAGE: usize = 6;
/// Signed streams remembered at most.
const SIGNED_STREAMS: usize = 1024;
const UNSIGNED: usize = 7;
const BADSIG: usize = 8;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Opens the link `device` names.
pub fn open(device: &str, opts: &Options) -> Result<Connection, ConnectError> {
    let target = parse_target(device, opts)?;
    let link = match &target {
        Target::Udp {
            host,
            port,
            input,
            broadcast,
        } => Link::Udp(Udp::open(host, *port, *input, *broadcast, opts.udp_timeout)?),
        Target::Tcp { host, port } => {
            let stream = Tcp::connect(host, *port, opts.retries, opts.reconnect_delay, &mut |_| {})?;
            Link::Tcp(Tcp::new(stream, host, *port, false, opts.retries, opts.reconnect_delay))
        }
        Target::TcpIn { host, port } => Link::TcpIn(TcpIn::open(host, *port)?),
        #[cfg(unix)]
        Target::Uds(path) => {
            let stream = crate::link::transport::Uds::connect(path, opts.retries, &mut |_| {})?;
            Link::Uds(crate::link::transport::Uds::new(stream, path, false, opts.retries))
        }
        #[cfg(not(unix))]
        Target::Uds(_) => {
            return Err(ConnectError::Unsupported(
                "Unix domain sockets need a Unix system".into(),
            ))
        }
        Target::Mcast { group, port } => Link::Mcast(Mcast::open(group, *port)?),
        Target::Serial { device, baud } => Link::Serial(Serial::open_held(device, *baud, opts.dtr, opts.rts).map_err(
            |e| ConnectError::Serial {
                errno: e.errno,
                message: e.message,
            },
        )?),
    };
    Ok(Connection {
        link,
        description: device.to_string(),
        parser: Mutex::new(Parser::new()),
        seq: AtomicU8::new(0),
        v2: AtomicBool::new(opts.proto == Proto::V2),
        source_system: opts.source_system,
        source_component: opts.source_component,
        signing: Mutex::new(None),
        signed_streams: Mutex::new(HashMap::new()),
        systems: Mutex::new(BTreeMap::new()),
        counters: Default::default(),
        closed: AtomicBool::new(false),
    })
}

fn gone(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, what.to_string())
}

impl Connection {
    /// The string it was opened with.
    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn proto(&self) -> Proto {
        if self.v2.load(Ordering::SeqCst) {
            Proto::V2
        } else {
            Proto::V1
        }
    }

    pub fn set_proto(&self, proto: Proto) {
        self.v2.store(proto == Proto::V2, Ordering::SeqCst);
    }

    /// Signs what is sent (MAVLink 2) and, with `require`, drops what arrives
    /// unsigned or with a wrong signature.
    pub fn set_signing(&self, signing: Option<Signing>) {
        lock(&self.signed_streams).clear();
        *lock(&self.signing) = signing;
    }

    /// The systems heard so far, by system id.
    pub fn systems(&self) -> Vec<SystemInfo> {
        lock(&self.systems).values().cloned().collect()
    }

    pub fn counters(&self) -> Counters {
        let c = |i: usize| self.counters[i].load(Ordering::SeqCst);
        Counters {
            bytes_received: c(BYTES_RX),
            frames_received: c(FRAMES_RX),
            frames_sent: c(FRAMES_TX),
            bytes_sent: c(BYTES_TX),
            crc_errors: c(CRC),
            unknown_messages: c(UNKNOWN),
            garbage_bytes: c(GARBAGE),
            unsigned_dropped: c(UNSIGNED),
            bad_signatures: c(BADSIG),
        }
    }

    fn bump(&self, i: usize, by: u64) {
        self.counters[i].fetch_add(by, Ordering::SeqCst);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Closes the link; reads and writes fail from then on. Idempotent.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        match &self.link {
            Link::Udp(l) => l.close(),
            Link::Tcp(l) => l.close(),
            Link::TcpIn(l) => l.close(),
            #[cfg(unix)]
            Link::Uds(l) => l.close(),
            Link::Mcast(l) => l.close(),
            Link::Serial(l) => l.close(),
        }
    }

    fn wait_readable(&self, timeout: f64) -> bool {
        match &self.link {
            Link::Udp(l) => l.wait_readable(timeout),
            Link::Tcp(l) => l.wait_readable(timeout),
            Link::TcpIn(l) => l.wait_readable(timeout),
            #[cfg(unix)]
            Link::Uds(l) => l.wait_readable(timeout),
            Link::Mcast(l) => l.wait_readable(timeout),
            Link::Serial(l) => l.wait_readable(timeout),
        }
    }

    fn read_bytes(&self) -> io::Result<Vec<u8>> {
        const N: usize = 4096;
        let stream = |(r, event): (io::Result<Vec<u8>>, Option<TcpEvent>)| match (r, event) {
            (Ok(b), None) => Ok(b),
            (Ok(_), Some(TcpEvent::Eof)) => Err(gone("the peer closed the connection")),
            (_, Some(_)) => Err(gone("the connection was reset")),
            (Err(e), None) => Err(e),
        };
        match &self.link {
            Link::Udp(l) => l.recv(),
            Link::Tcp(l) => stream(l.recv(N)),
            Link::TcpIn(l) => Ok(l.recv(N)),
            #[cfg(unix)]
            Link::Uds(l) => stream(l.recv(N)),
            Link::Mcast(l) => l.recv(),
            Link::Serial(l) => l.recv(N),
        }
    }

    fn write_bytes(&self, buf: &[u8]) -> io::Result<()> {
        let lost = |e: Option<TcpEvent>| e.map_or(Ok(()), |_| Err(gone("the connection was reset")));
        match &self.link {
            Link::Udp(l) => l.write(buf),
            Link::Tcp(l) => return lost(l.write(buf)),
            Link::TcpIn(l) => l.write(buf),
            #[cfg(unix)]
            Link::Uds(l) => return lost(l.write(buf)),
            Link::Mcast(l) => l.write(buf),
            Link::Serial(l) => {
                l.write(buf)?;
            }
        }
        Ok(())
    }

    /// Waits up to `timeout` for bytes, then returns every frame they
    /// completed (an empty list when nothing came). A bad checksum, an
    /// unknown message and noise are counted, not returned. `Err` means the
    /// link is gone: closed here, closed by the peer or a read that failed.
    pub fn recv(&self, timeout: Duration) -> io::Result<Vec<Received>> {
        if self.is_closed() {
            return Err(gone("the connection is closed"));
        }
        if !self.wait_readable(timeout.as_secs_f64()) {
            return Ok(Vec::new());
        }
        let bytes = self.read_bytes()?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        self.bump(BYTES_RX, bytes.len() as u64);
        let events = lock(&self.parser).feed(&bytes);
        let at = now();
        let mut out = Vec::new();
        for event in events {
            match event {
                Event::Garbage(n) => self.bump(GARBAGE, n as u64),
                Event::BadCrc { .. } => self.bump(CRC, 1),
                Event::Unknown { .. } => self.bump(UNKNOWN, 1),
                Event::Frame(frame) => {
                    if !self.admit(&frame) {
                        continue;
                    }
                    self.bump(FRAMES_RX, 1);
                    let message = frame.message();
                    if let Ok(m) = &message {
                        self.track(&frame, m, at);
                    }
                    out.push(Received {
                        frame,
                        message,
                        time: at,
                    });
                }
            }
        }
        Ok(out)
    }

    /// With a key set, only a frame it signed is let through, and only once:
    /// the timestamp of a stream (system, component, link id) has to grow, so
    /// a frame recorded off the air and sent again is dropped, as the MAVLink
    /// signing rules ask. A vehicle that restarts with an older clock is
    /// heard again when the link is opened anew, which its silence leads to.
    fn admit(&self, frame: &Frame) -> bool {
        let mut signing = lock(&self.signing);
        let Some(key) = signing.as_mut() else { return true };
        if frame.signature.is_none() {
            self.bump(UNSIGNED, 1);
            return false;
        }
        if !key.verify(frame) {
            self.bump(BADSIG, 1);
            return false;
        }
        let (Some(sig), Some(stamp)) = (frame.signature, frame.timestamp()) else {
            return false;
        };
        let mut streams = lock(&self.signed_streams);
        let stream = (frame.sysid, frame.compid, sig[0]);
        if streams.get(&stream).is_some_and(|last| stamp <= *last) {
            self.bump(BADSIG, 1);
            return false;
        }
        // 256 systems of 256 components cannot all be real: start over
        // rather than grow on what a flood of made up ids leaves behind.
        if streams.len() >= SIGNED_STREAMS && !streams.contains_key(&stream) {
            streams.clear();
        }
        streams.insert(stream, stamp);
        // Never sign with a time older than one already seen on the link.
        key.timestamp = key.timestamp.max(stamp.saturating_add(1));
        true
    }

    fn track(&self, frame: &Frame, msg: &Message, at: f64) {
        let mut systems = lock(&self.systems);
        let info = systems.entry(frame.sysid).or_insert_with(|| SystemInfo {
            sysid: frame.sysid,
            compid: frame.compid,
            mav_type: 0,
            autopilot: 0,
            base_mode: 0,
            custom_mode: 0,
            system_status: 0,
            last_heartbeat: 0.0,
            heartbeats: 0,
            messages: 0,
        });
        info.messages += 1;
        if msg.id() == 0 {
            let int = |name: &str| msg.get(name).and_then(Value::as_i64).unwrap_or(0);
            info.compid = frame.compid;
            info.mav_type = int("type");
            info.autopilot = int("autopilot");
            info.base_mode = int("base_mode");
            info.custom_mode = int("custom_mode");
            info.system_status = int("system_status");
            info.last_heartbeat = at;
            info.heartbeats += 1;
        }
    }

    /// Sends `msg` from this connection's system and component, with the next
    /// sequence number. MAVLink 1 cannot carry a message id above 255.
    pub fn send(&self, msg: &Message) -> io::Result<()> {
        if self.is_closed() {
            return Err(gone("the connection is closed"));
        }
        let proto = self.proto();
        if proto == Proto::V1 && msg.id() > 255 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} needs MAVLink 2", msg.name()),
            ));
        }
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut signing = lock(&self.signing);
        let bytes = encode(
            proto,
            seq,
            self.source_system,
            self.source_component,
            msg,
            signing.as_mut(),
        )
        .map_err(|e: CodecError| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        drop(signing);
        self.write_bytes(&bytes)?;
        self.bump(FRAMES_TX, 1);
        self.bump(BYTES_TX, bytes.len() as u64);
        Ok(())
    }

    /// Writes bytes exactly as given (a second station's own frames, with its
    /// sequence numbers and signature), counted like a sent frame.
    pub fn write_raw(&self, buf: &[u8]) -> io::Result<()> {
        if self.is_closed() {
            return Err(gone("the connection is closed"));
        }
        self.write_bytes(buf)?;
        self.bump(FRAMES_TX, 1);
        self.bump(BYTES_TX, buf.len() as u64);
        Ok(())
    }

    /// Where a UDP link points or listens: its bound address.
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        match &self.link {
            Link::Udp(l) => l.local_addr().ok(),
            Link::Tcp(l) => l.local_addr().ok(),
            Link::TcpIn(l) => l.local_addr().ok(),
            Link::Mcast(l) => l.local_addr().ok(),
            _ => None,
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::defs::Wire;

    fn free_udp_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn free_tcp_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn heartbeat(autopilot: i128, mode: i128) -> Message {
        Message::named("HEARTBEAT", Wire::V2)
            .unwrap()
            .with("type", Value::Int(2))
            .with("autopilot", Value::Int(autopilot))
            .with("base_mode", Value::Int(129))
            .with("custom_mode", Value::Int(mode))
            .with("system_status", Value::Int(4))
    }

    fn first(c: &Connection) -> Received {
        for _ in 0..40 {
            let mut got = c.recv(Duration::from_millis(100)).unwrap();
            if !got.is_empty() {
                return got.remove(0);
            }
        }
        panic!("nothing arrived on {}", c.description());
    }

    #[test]
    fn connection_strings_are_read_as_mavlink_connection_reads_them() {
        let o = Options::default();
        let udp = |h: &str, p: u16, input, broadcast| Target::Udp {
            host: h.into(),
            port: p,
            input,
            broadcast,
        };
        let cases: Vec<(&str, Target)> = vec![
            ("udp:0.0.0.0:14550", udp("0.0.0.0", 14550, true, false)),
            ("udpin:127.0.0.1:14551", udp("127.0.0.1", 14551, true, false)),
            ("udpout:10.0.0.2:14550", udp("10.0.0.2", 14550, false, false)),
            ("udpbcast:192.168.1.255:14550", udp("192.168.1.255", 14550, false, true)),
            ("192.168.1.2:14550", udp("192.168.1.2", 14550, true, false)),
            (
                "tcp:10.0.0.2:5760",
                Target::Tcp {
                    host: "10.0.0.2".into(),
                    port: 5760,
                },
            ),
            (
                "tcpin:0.0.0.0:5760",
                Target::TcpIn {
                    host: "0.0.0.0".into(),
                    port: 5760,
                },
            ),
            ("uds:/tmp/mav.sock", Target::Uds("/tmp/mav.sock".into())),
            ("unix:/tmp/mav.sock", Target::Uds("/tmp/mav.sock".into())),
            (
                "mcast:",
                Target::Mcast {
                    group: "239.255.145.50".into(),
                    port: 14550,
                },
            ),
            (
                "mcast:14560",
                Target::Mcast {
                    group: "239.255.145.50".into(),
                    port: 14560,
                },
            ),
            (
                "mcast:239.1.2.3:14570",
                Target::Mcast {
                    group: "239.1.2.3".into(),
                    port: 14570,
                },
            ),
            (
                "/dev/ttyACM0",
                Target::Serial {
                    device: "/dev/ttyACM0".into(),
                    baud: 115_200,
                },
            ),
            (
                "/dev/ttyUSB0,57600",
                Target::Serial {
                    device: "/dev/ttyUSB0".into(),
                    baud: 57_600,
                },
            ),
            (
                "COM3",
                Target::Serial {
                    device: "COM3".into(),
                    baud: 115_200,
                },
            ),
        ];
        for (s, want) in cases {
            assert_eq!(parse_target(s, &o).unwrap(), want, "{s}");
        }
        let out = Options {
            input: false,
            ..Options::default()
        };
        assert_eq!(
            parse_target("udp:1.2.3.4:5", &out).unwrap(),
            udp("1.2.3.4", 5, false, false)
        );
    }

    #[test]
    fn bad_connection_strings_say_what_python_says() {
        let o = Options::default();
        let msg = |s: &str| parse_target(s, &o).unwrap_err().to_string();
        assert_eq!(msg("udpin:1.2.3.4"), "UDP ports must be specified as host:port");
        assert_eq!(msg("tcp:host:x"), "invalid literal for int() with base 10: 'x'");
        assert_eq!(msg("udpout:1.2.3.4:70000"), "bind(): port must be 0-65535.");
        assert_eq!(msg("tcp:1:2:3"), "TCP ports must be specified as host:port");
        assert_eq!(msg("uds:"), "Unix domain socket path must be specified");
        assert_eq!(msg("/dev/ttyUSB0,fast"), "invalid baud rate 'fast'");
        assert!(matches!(
            parse_target("ws://x:1", &o),
            Err(ConnectError::Unsupported(_))
        ));
        assert!(matches!(
            parse_target("wsserver:0.0.0.0:1", &o),
            Err(ConnectError::Unsupported(_))
        ));
    }

    #[test]
    fn a_ground_station_and_a_vehicle_talk_over_udp() {
        let port = free_udp_port();
        let gcs = open(&format!("udpin:127.0.0.1:{port}"), &Options::default()).unwrap();
        let vehicle = open(
            &format!("udpout:127.0.0.1:{port}"),
            &Options {
                source_system: 1,
                source_component: 1,
                ..Options::default()
            },
        )
        .unwrap();
        vehicle.send(&heartbeat(12, 0x0004_0000)).unwrap();
        let got = first(&gcs);
        let msg = got.message.unwrap();
        assert_eq!((got.frame.sysid, got.frame.compid, got.frame.seq), (1, 1, 0));
        assert_eq!(msg.get("autopilot"), Some(&Value::Int(12)));
        let systems = gcs.systems();
        assert_eq!(systems.len(), 1);
        assert_eq!(
            (
                systems[0].sysid,
                systems[0].autopilot,
                systems[0].custom_mode,
                systems[0].heartbeats
            ),
            (1, 12, 0x0004_0000, 1)
        );
        assert!(systems[0].last_heartbeat > 0.0);
        // The server answers the client it has heard.
        gcs.send(&heartbeat(8, 0)).unwrap();
        let back = first(&vehicle);
        assert_eq!((back.frame.sysid, back.frame.compid), (255, 0));
        vehicle.send(&heartbeat(12, 1)).unwrap();
        assert_eq!(first(&gcs).frame.seq, 1, "the sequence number counts up");
        let c = gcs.counters();
        assert_eq!((c.frames_received, c.frames_sent), (2, 1));
        assert!(c.bytes_received > 20);
    }

    #[test]
    fn a_stream_link_carries_frames_both_ways() {
        let port = free_tcp_port();
        let server = open(&format!("tcpin:127.0.0.1:{port}"), &Options::default()).unwrap();
        let client = open(
            &format!("tcp:127.0.0.1:{port}"),
            &Options {
                source_system: 7,
                ..Options::default()
            },
        )
        .unwrap();
        client.send(&heartbeat(3, 0)).unwrap();
        assert_eq!(first(&server).frame.sysid, 7);
        server.send(&heartbeat(4, 0)).unwrap();
        assert_eq!(first(&client).frame.sysid, 255);
        drop(server);
        let mut result = Ok(Vec::new());
        for _ in 0..50 {
            result = client.recv(Duration::from_millis(100));
            if result.is_err() {
                break;
            }
        }
        assert!(result.is_err(), "a closed peer is a lost link, not silence");
    }

    #[test]
    fn signed_links_drop_what_is_not_signed_by_their_key() {
        let port = free_udp_port();
        let key = [9u8; 32];
        let gcs = open(&format!("udpin:127.0.0.1:{port}"), &Options::default()).unwrap();
        gcs.set_signing(Some(Signing::new(key, 0, 1000)));
        let plain = open(&format!("udpout:127.0.0.1:{port}"), &Options::default()).unwrap();
        let wrong = open(&format!("udpout:127.0.0.1:{port}"), &Options::default()).unwrap();
        wrong.set_signing(Some(Signing::new([1; 32], 2, 1000)));
        let right = open(
            &format!("udpout:127.0.0.1:{port}"),
            &Options {
                source_system: 5,
                ..Options::default()
            },
        )
        .unwrap();
        right.set_signing(Some(Signing::new(key, 1, 2000)));
        plain.send(&heartbeat(1, 0)).unwrap();
        wrong.send(&heartbeat(1, 0)).unwrap();
        right.send(&heartbeat(1, 0)).unwrap();
        let got = first(&gcs);
        assert_eq!(got.frame.sysid, 5);
        assert!(got.frame.signature.is_some());
        std::thread::sleep(Duration::from_millis(50));
        let _ = gcs.recv(Duration::from_millis(50));
        let c = gcs.counters();
        assert_eq!((c.unsigned_dropped, c.bad_signatures, c.frames_received), (1, 1, 1));
    }

    #[test]
    fn a_signed_frame_sent_again_is_dropped() {
        let port = free_udp_port();
        let key = [9u8; 32];
        let gcs = open(&format!("udpin:127.0.0.1:{port}"), &Options::default()).unwrap();
        gcs.set_signing(Some(Signing::new(key, 0, 1000)));
        let raw = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut vehicle = Signing::new(key, 3, 5000);
        let one = encode(Proto::V2, 0, 1, 1, &heartbeat(1, 0), Some(&mut vehicle)).unwrap();
        let two = encode(Proto::V2, 1, 1, 1, &heartbeat(1, 0), Some(&mut vehicle)).unwrap();
        for frame in [&one, &one, &two, &one, &two] {
            raw.send_to(frame, ("127.0.0.1", port)).unwrap();
        }
        let mut seqs = Vec::new();
        for _ in 0..20 {
            for r in gcs.recv(Duration::from_millis(50)).unwrap() {
                seqs.push(r.frame.seq);
            }
        }
        assert_eq!(seqs, [0, 1], "each frame once, the copies dropped");
        assert_eq!(gcs.counters().bad_signatures, 3);
        // What this station signs next is newer than anything it heard.
        gcs.send(&heartbeat(6, 8)).unwrap();
        assert!(lock(&gcs.signing).as_ref().is_some_and(|s| s.timestamp > 5001));
    }

    #[test]
    fn noise_and_bad_frames_are_counted_not_returned() {
        let port = free_udp_port();
        let gcs = open(&format!("udpin:127.0.0.1:{port}"), &Options::default()).unwrap();
        let raw = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut frame = encode(Proto::V2, 0, 1, 1, &heartbeat(1, 0), None).unwrap();
        let last = frame.len() - 1;
        frame[last] ^= 0x55;
        let mut datagram = vec![0x11, 0x22];
        datagram.extend_from_slice(&frame);
        raw.send_to(&datagram, ("127.0.0.1", port)).unwrap();
        assert!(gcs.recv(Duration::from_secs(2)).unwrap().is_empty());
        let c = gcs.counters();
        assert_eq!(c.crc_errors, 1);
        assert!(c.garbage_bytes >= 2);
        assert!(gcs.systems().is_empty());
    }

    #[test]
    fn mavlink_1_cannot_carry_a_large_message_id() {
        let port = free_udp_port();
        let c = open(
            &format!("udpout:127.0.0.1:{port}"),
            &Options {
                proto: Proto::V1,
                ..Options::default()
            },
        )
        .unwrap();
        let big = Message::named("OPEN_DRONE_ID_BASIC_ID", Wire::V2).unwrap();
        assert_eq!(c.send(&big).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        c.set_proto(Proto::V2);
        c.send(&big).unwrap();
    }

    #[test]
    fn a_closed_connection_refuses_and_a_missing_serial_port_says_why() {
        let port = free_udp_port();
        let c = open(&format!("udpout:127.0.0.1:{port}"), &Options::default()).unwrap();
        c.close();
        c.close();
        assert!(c.is_closed());
        assert!(c.send(&heartbeat(1, 0)).is_err());
        assert!(c.recv(Duration::from_millis(1)).is_err());
        match open("/dev/definitely-not-a-port", &Options::default()) {
            Err(ConnectError::Serial { message, .. }) => assert!(!message.is_empty()),
            other => panic!("{:?}", other.err()),
        }
    }
}
