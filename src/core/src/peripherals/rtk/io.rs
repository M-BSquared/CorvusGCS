//! Where RTCM corrections come from: a base station's serial port, or a socket
//! held open to an NTRIP caster (`corvus_link.rtk.io`).
//!
//! [`Port`] and [`Socket`] are the devices: a serial port and a TCP stream by
//! default, a fake in a test. [`Transport`] is what the session reads, writes
//! and closes.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_link::plain::transport::Serial;

use crate::peripherals::util::{py_oserror_text, py_strip};

pub const NTRIP_READ_BYTES: usize = 4096;

/// A session could not be run, with the reason an operator can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtkError(pub String);

impl std::fmt::Display for RtkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A serial port as the transport needs it (pyserial's `in_waiting`, `read`,
/// `write` and `close`).
pub trait Port: Send + Sync {
    /// Up to `max` bytes; waits for the first one for the port's read timeout.
    fn read(&self, max: usize) -> io::Result<Vec<u8>>;
    fn in_waiting(&self) -> io::Result<usize>;
    fn write(&self, data: &[u8]) -> io::Result<()>;
    fn close(&self);
}

/// A connected stream with a read timeout: a timeout reads as `TimedOut`.
pub trait Socket: Send + Sync {
    fn recv(&self, max: usize) -> io::Result<Vec<u8>>;
    fn send_all(&self, data: &[u8]) -> io::Result<()>;
    fn close(&self);
}

/// What a correction source has to be able to do.
pub trait Transport: Send + Sync {
    fn read(&self) -> Result<Vec<u8>, RtkError>;
    fn write(&self, data: &[u8]) -> Result<(), RtkError>;
    fn close(&self);
    fn label(&self) -> String;
    /// Whether the session may configure what is behind it.
    fn configurable(&self) -> bool;
    fn device(&self) -> String;
    fn baud(&self) -> u32;
    /// Bytes already read while opening it; taken once.
    fn take_pending(&self) -> Vec<u8>;
}

/// The default serial port: opened straight at its baud rate.
pub struct SerialPort {
    port: Serial,
    timeout: f64,
    write_timeout: f64,
}

impl SerialPort {
    pub fn open(device: &str, baud: u32, timeout: f64, write_timeout: f64) -> io::Result<SerialPort> {
        let port = Serial::open_direct(device, baud).map_err(crate::peripherals::util::serial_open_error)?;
        Ok(SerialPort {
            port,
            timeout,
            write_timeout,
        })
    }
}

impl Port for SerialPort {
    fn read(&self, max: usize) -> io::Result<Vec<u8>> {
        self.port.read_timeout(max, self.timeout)
    }

    fn in_waiting(&self) -> io::Result<usize> {
        self.port.waiting()
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        self.port.write_timeout(data, self.write_timeout).map(|_| ())
    }

    fn close(&self) {
        self.port.close();
    }
}

/// One TCP connection; closing it from another thread ends a read.
pub struct TcpSocket {
    stream: Mutex<Option<Arc<TcpStream>>>,
}

impl TcpSocket {
    /// `socket.create_connection((host, port), timeout)`: every address the
    /// name resolves to in turn, the last error if none answers.
    pub fn connect(host: &str, port: u16, connect_timeout: f64, read_timeout: f64) -> io::Result<TcpSocket> {
        let mut last: Option<io::Error> = None;
        let wait = Duration::from_secs_f64(connect_timeout.max(1e-6));
        for addr in (host, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, wait) {
                Ok(stream) => {
                    let t = Some(Duration::from_secs_f64(read_timeout.max(1e-6)));
                    stream.set_read_timeout(t)?;
                    stream.set_write_timeout(t)?;
                    return Ok(TcpSocket {
                        stream: Mutex::new(Some(Arc::new(stream))),
                    });
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("getaddrinfo returns an empty list")))
    }

    fn stream(&self) -> io::Result<Arc<TcpStream>> {
        lock(&self.stream)
            .clone()
            .ok_or_else(|| io::Error::from_raw_os_error(9))
    }
}

impl Socket for TcpSocket {
    fn recv(&self, max: usize) -> io::Result<Vec<u8>> {
        let stream = self.stream()?;
        let mut buf = vec![0u8; max.max(1)];
        match (&*stream).read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Ok(buf)
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
            }
            Err(e) => Err(e),
        }
    }

    fn send_all(&self, data: &[u8]) -> io::Result<()> {
        (&*self.stream()?).write_all(data)
    }

    fn close(&self) {
        if let Some(s) = lock(&self.stream).take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// A serial port, opened read/write at one baud rate.
pub struct SerialTransport {
    port: Arc<dyn Port>,
    device: String,
    baud: u32,
    pending: Mutex<Vec<u8>>,
}

impl SerialTransport {
    pub fn new(port: Arc<dyn Port>, device: &str, baud: u32) -> SerialTransport {
        SerialTransport {
            port,
            device: device.to_string(),
            baud,
            pending: Mutex::new(Vec::new()),
        }
    }
}

impl Transport for SerialTransport {
    fn read(&self) -> Result<Vec<u8>, RtkError> {
        let waiting = self.port.in_waiting().unwrap_or(0);
        self.port
            .read(waiting.clamp(1, 4096))
            .map_err(|e| RtkError(format!("read failed: {}", py_oserror_text(&e))))
    }

    fn write(&self, data: &[u8]) -> Result<(), RtkError> {
        self.port
            .write(data)
            .map_err(|e| RtkError(format!("write failed: {}", py_oserror_text(&e))))
    }

    fn close(&self) {
        self.port.close();
    }

    fn label(&self) -> String {
        format!("{} @ {}", self.device, self.baud)
    }

    fn configurable(&self) -> bool {
        true
    }

    fn device(&self) -> String {
        self.device.clone()
    }

    fn baud(&self) -> u32 {
        self.baud
    }

    fn take_pending(&self) -> Vec<u8> {
        std::mem::take(&mut lock(&self.pending))
    }
}

/// A socket holding one NTRIP caster stream open.
pub struct NtripTransport {
    sock: Arc<dyn Socket>,
    label: String,
    pending: Mutex<Vec<u8>>,
}

impl NtripTransport {
    pub fn new(sock: Arc<dyn Socket>, label: &str) -> NtripTransport {
        NtripTransport {
            sock,
            label: label.to_string(),
            pending: Mutex::new(Vec::new()),
        }
    }

    pub fn set_pending(&self, bytes: Vec<u8>) {
        *lock(&self.pending) = bytes;
    }
}

impl Transport for NtripTransport {
    fn read(&self) -> Result<Vec<u8>, RtkError> {
        match self.sock.recv(NTRIP_READ_BYTES) {
            Ok(d) => Ok(d),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(Vec::new()),
            Err(e) => Err(RtkError(format!("caster read failed: {}", py_oserror_text(&e)))),
        }
    }

    /// A caster takes nothing from a ground station relaying to a rover.
    fn write(&self, _data: &[u8]) -> Result<(), RtkError> {
        Ok(())
    }

    fn close(&self) {
        self.sock.close();
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn configurable(&self) -> bool {
        false
    }

    fn device(&self) -> String {
        self.label.clone()
    }

    fn baud(&self) -> u32 {
        0
    }

    fn take_pending(&self) -> Vec<u8> {
        std::mem::take(&mut lock(&self.pending))
    }
}

/// The GET a caster expects, as NTRIP 2.0 with the 1.0 answer accepted.
pub fn ntrip_request(host: &str, port: i64, mountpoint: &str, username: &str, password: &str) -> Vec<u8> {
    let mut text = format!(
        "GET /{mountpoint} HTTP/1.1\r\nHost: {host}:{port}\r\nNtrip-Version: Ntrip/2.0\r\nUser-Agent: NTRIP CorvusGCS/{}\r\nAccept: */*\r\nConnection: close",
        crate::version::version()
    );
    if !username.is_empty() || !password.is_empty() {
        use base64::Engine;
        let raw = format!("{username}:{password}");
        text.push_str(&format!(
            "\r\nAuthorization: Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
        ));
    }
    text.push_str("\r\n\r\n");
    text.chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .collect()
}

/// Consumes the caster's response header and returns the bytes after it: a
/// caster answers and then streams in the same socket, so the first
/// correction is routinely in the same packet as the last header line.
pub fn read_ntrip_header(sock: &dyn Socket, timeout: f64) -> Result<Vec<u8>, RtkError> {
    let mut buf: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    while Instant::now() < deadline {
        let chunk = match sock.recv(NTRIP_READ_BYTES) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => {
                return Err(RtkError(format!(
                    "the caster closed the connection: {}",
                    py_oserror_text(&e)
                )))
            }
        };
        if chunk.is_empty() {
            return Err(RtkError("the caster closed the connection without answering".into()));
        }
        buf.extend(chunk);
        let Some(split) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            if buf.len() > 8192 {
                return Err(RtkError("the caster sent no usable response".into()));
            }
            continue;
        };
        let head: String = buf[..split].iter().map(|&b| b as char).collect();
        let rest = buf[split + 4..].to_vec();
        let first = head.split("\r\n").next().unwrap_or("");
        if first.contains("200") || first.to_uppercase().starts_with("ICY 200") {
            return Ok(rest);
        }
        if first.contains("401") {
            return Err(RtkError("the caster rejected the username or password".into()));
        }
        if first.contains("404") {
            return Err(RtkError("the caster has no such mountpoint".into()));
        }
        let shown = py_strip(first);
        let shown = if shown.is_empty() { "no response" } else { shown };
        return Err(RtkError(format!("the caster refused the stream: {shown}")));
    }
    Err(RtkError("the caster did not answer in time".into()))
}
