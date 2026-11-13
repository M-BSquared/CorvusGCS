//! The links themselves: UDP, TCP (client and listener) and serial, with
//! the non-blocking read and blocking wait pymavlink's `mavfile` subclasses
//! use, and the same error handling.
//!
//! Nothing here touches Python. The connection releases the GIL around the
//! calls that can wait (`wait_readable`, connecting, sleeping between
//! retries).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

use crate::lock;

pub const UDP_MAX_PACKET_LEN: usize = 65535;

fn ebadf() -> io::Error {
    #[cfg(unix)]
    {
        io::Error::from_raw_os_error(libc::EBADF)
    }
    #[cfg(not(unix))]
    {
        io::Error::from_raw_os_error(10038)
    }
}

/// Resolve `host:port` the way an AF_INET socket's `bind`/`connect` does.
pub fn resolve_v4(host: &str, port: u16) -> io::Result<SocketAddr> {
    let host = if host.is_empty() { "0.0.0.0" } else { host };
    let mut last = None;
    for addr in (host, port).to_socket_addrs()? {
        if addr.is_ipv4() {
            return Ok(addr);
        }
        last = Some(addr);
    }
    last.ok_or_else(|| io::Error::other("[Errno 8] nodename nor servname provided, or not known"))
}

#[cfg(unix)]
pub type RawHandle = std::os::unix::io::RawFd;
#[cfg(windows)]
pub type RawHandle = std::os::windows::io::RawSocket;

/// Wait up to `timeout` seconds for the handle to become readable, as
/// `select.select([fd], [], [], timeout)` does.
#[cfg(unix)]
pub fn wait_readable(fd: RawHandle, timeout: f64) -> bool {
    let ms = if timeout <= 0.0 {
        0
    } else {
        (timeout * 1000.0).ceil().min(i32::MAX as f64) as i32
    };
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return rc > 0;
    }
}

/// Wait until the descriptor can take more data; a negative timeout waits
/// as long as it takes.
#[cfg(unix)]
pub fn wait_writable(fd: RawHandle, timeout: f64) -> bool {
    let ms = if timeout < 0.0 {
        -1
    } else {
        (timeout * 1000.0).ceil().min(i32::MAX as f64) as i32
    };
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return rc > 0;
    }
}

#[cfg(windows)]
pub fn wait_readable(sock: RawHandle, timeout: f64) -> bool {
    use windows_sys::Win32::Networking::WinSock::{WSAPoll, POLLRDNORM, WSAPOLLFD};
    let ms = if timeout <= 0.0 {
        0
    } else {
        (timeout * 1000.0).ceil().min(i32::MAX as f64) as i32
    };
    let mut pfd = WSAPOLLFD {
        fd: sock as usize,
        events: POLLRDNORM as i16,
        revents: 0,
    };
    let rc = unsafe { WSAPoll(&mut pfd, 1, ms) };
    rc > 0
}

// ---------------------------------------------------------------- UDP ----

pub struct Udp {
    sock: RwLock<Option<Arc<UdpSocket>>>,
    pub server: bool,
    broadcast: AtomicBool,
    timeout: f64,
    clients: Mutex<(Vec<SocketAddr>, Vec<f64>)>,
    destination: Mutex<Option<(String, u16)>>,
    resolved_host: Mutex<Option<String>>,
    last_address: Mutex<Option<SocketAddr>>,
}

impl Udp {
    pub fn open(host: &str, port: u16, input: bool, broadcast: bool, timeout: f64) -> io::Result<Udp> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        let mut destination = None;
        if input {
            socket.set_reuse_address(true)?;
            socket.bind(&resolve_v4(host, port)?.into())?;
        } else {
            destination = Some((host.to_string(), port));
            if broadcast {
                socket.set_broadcast(true)?;
            }
            // Windows fails a read on a socket nothing has sent from yet, so it
            // is bound first, to any free port: the destination's own number
            // would make a peer on this computer answer to itself.
            if cfg!(windows) {
                socket.bind(&SocketAddr::from(([0, 0, 0, 0], 0)).into())?;
            }
        }
        socket.set_nonblocking(true)?;
        let sock: UdpSocket = socket.into();
        Ok(Udp {
            sock: RwLock::new(Some(Arc::new(sock))),
            server: input,
            broadcast: AtomicBool::new(broadcast),
            timeout,
            clients: Mutex::new((Vec::new(), Vec::new())),
            destination: Mutex::new(destination),
            resolved_host: Mutex::new(None),
            last_address: Mutex::new(None),
        })
    }

    fn socket(&self) -> io::Result<Arc<UdpSocket>> {
        self.sock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(ebadf)
    }

    pub fn recv(&self) -> io::Result<Vec<u8>> {
        let sock = self.socket()?;
        let mut buf = vec![0u8; UDP_MAX_PACKET_LEN];
        let (n, addr) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(Vec::new())
            }
            Err(e) => return Err(e),
        };
        buf.truncate(n);
        if self.server {
            let mut clients = lock(&self.clients);
            let now = crate::plain::time::now();
            match clients.0.iter().position(|a| *a == addr) {
                Some(i) => clients.1[i] = now,
                None => {
                    clients.0.push(addr);
                    clients.1.push(now);
                }
            }
        } else if self.broadcast.load(Ordering::SeqCst) {
            *lock(&self.last_address) = Some(addr);
        }
        Ok(buf)
    }

    /// `mavudp.write`: every socket error is swallowed.
    pub fn write(&self, buf: &[u8]) {
        let _ = self.try_write(buf);
    }

    fn try_write(&self, buf: &[u8]) -> io::Result<()> {
        let sock = self.socket()?;
        if self.server {
            let now = crate::plain::time::now();
            let mut clients = lock(&self.clients);
            let count = clients.0.len();
            let mut remove = Vec::new();
            for i in 0..count {
                let address = clients.0[i];
                if count == 1 || self.timeout <= 0.0 || clients.1[i] + self.timeout > now {
                    sock.send_to(buf, address)?;
                } else if count > 1 && remove.len() < count - 1 {
                    remove.push(i);
                }
            }
            for i in remove.into_iter().rev() {
                clients.0.remove(i);
                clients.1.remove(i);
            }
            return Ok(());
        }
        let last = *lock(&self.last_address);
        if let Some(last) = last {
            if self.broadcast.load(Ordering::SeqCst) {
                *lock(&self.destination) = Some((last.ip().to_string(), last.port()));
                self.broadcast.store(false, Ordering::SeqCst);
                sock.connect(last)?;
            }
        }
        let Some((host, port)) = lock(&self.destination).clone() else {
            return Ok(());
        };
        let mut resolved = lock(&self.resolved_host);
        let target = if resolved.as_deref() != Some(host.as_str()) {
            *resolved = Some(host.clone());
            let addr = resolve_v4(&host, port)?;
            *lock(&self.destination) = Some((addr.ip().to_string(), port));
            *resolved = Some(addr.ip().to_string());
            addr
        } else {
            resolve_v4(&host, port)?
        };
        drop(resolved);
        sock.send_to(buf, target)?;
        Ok(())
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        match self.raw() {
            Some(h) => wait_readable(h, timeout),
            None => false,
        }
    }

    pub fn raw(&self) -> Option<RawHandle> {
        let sock = self.socket().ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            Some(sock.as_raw_fd())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            Some(sock.as_raw_socket())
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket()?.local_addr()
    }

    pub fn close(&self) {
        *self.sock.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn destination(&self) -> Option<(String, u16)> {
        lock(&self.destination).clone()
    }

    pub fn clients(&self) -> Vec<SocketAddr> {
        lock(&self.clients).0.clone()
    }
}

// ---------------------------------------------------------------- TCP ----

pub enum TcpEvent {
    Disconnect,
    Eof,
}

pub struct Tcp {
    stream: RwLock<Option<Arc<TcpStream>>>,
    pub destination: (String, u16),
    pub autoreconnect: bool,
    pub retries: i64,
    pub reconnect_delay: f64,
}

impl Tcp {
    /// `mavtcp.do_connect`: `retries` attempts in all, `reconnect_delay`
    /// apart, reporting each failure through `report`.
    pub fn connect(
        host: &str,
        port: u16,
        retries: i64,
        reconnect_delay: f64,
        report: &mut dyn FnMut(&io::Error),
    ) -> io::Result<TcpStream> {
        let mut retries = if retries <= 0 { 1 } else { retries };
        loop {
            retries -= 1;
            let attempt = resolve_v4(host, port).and_then(TcpStream::connect);
            match attempt {
                Ok(stream) => {
                    stream.set_nonblocking(true)?;
                    stream.set_nodelay(true)?;
                    return Ok(stream);
                }
                Err(e) => {
                    if retries <= 0 {
                        return Err(e);
                    }
                    report(&e);
                    std::thread::sleep(Duration::from_secs_f64(reconnect_delay.max(0.0)));
                }
            }
        }
    }

    pub fn new(
        stream: TcpStream,
        host: &str,
        port: u16,
        autoreconnect: bool,
        retries: i64,
        reconnect_delay: f64,
    ) -> Tcp {
        Tcp {
            stream: RwLock::new(Some(Arc::new(stream))),
            destination: (host.to_string(), port),
            autoreconnect,
            retries,
            reconnect_delay,
        }
    }

    fn stream(&self) -> Option<Arc<TcpStream>> {
        self.stream.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_open(&self) -> bool {
        self.stream().is_some()
    }

    pub fn replace(&self, stream: Option<TcpStream>) {
        *self.stream.write().unwrap_or_else(|e| e.into_inner()) = stream.map(Arc::new);
    }

    /// Non-blocking read of up to `n` bytes. The event says what pymavlink
    /// would have reported (and possibly reconnected on) before returning.
    pub fn recv(&self, n: usize) -> (io::Result<Vec<u8>>, Option<TcpEvent>) {
        let Some(stream) = self.stream() else {
            return (Err(ebadf()), None);
        };
        let mut buf = vec![0u8; n.max(1)];
        match (&*stream).read(&mut buf) {
            Ok(0) => (Ok(Vec::new()), Some(TcpEvent::Eof)),
            Ok(k) => {
                buf.truncate(k);
                (Ok(buf), None)
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => (Ok(Vec::new()), None),
            Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => {
                (Err(e), Some(TcpEvent::Disconnect))
            }
            Err(e) => (Err(e), None),
        }
    }

    /// One `send` call; errors are swallowed, a reset is reported.
    pub fn write(&self, buf: &[u8]) -> Option<TcpEvent> {
        let stream = self.stream()?;
        match (&*stream).write(buf) {
            Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => {
                Some(TcpEvent::Disconnect)
            }
            _ => None,
        }
    }

    pub fn raw(&self) -> Option<RawHandle> {
        let stream = self.stream()?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            Some(stream.as_raw_fd())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            Some(stream.as_raw_socket())
        }
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        match self.raw() {
            Some(h) => wait_readable(h, timeout),
            None => {
                std::thread::sleep(Duration::from_secs_f64(timeout.clamp(0.0, 0.5)));
                true
            }
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream().ok_or_else(ebadf)?.local_addr()
    }

    pub fn close(&self) {
        self.replace(None);
    }
}

pub struct TcpIn {
    listener: RwLock<Option<Arc<TcpListener>>>,
    stream: RwLock<Option<Arc<TcpStream>>>,
}

impl TcpIn {
    pub fn open(host: &str, port: u16) -> io::Result<TcpIn> {
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_reuse_address(true)?;
        socket.bind(&resolve_v4(host, port)?.into())?;
        socket.listen(1)?;
        socket.set_nonblocking(true)?;
        let _ = socket.set_nodelay(true);
        Ok(TcpIn {
            listener: RwLock::new(Some(Arc::new(socket.into()))),
            stream: RwLock::new(None),
        })
    }

    fn stream(&self) -> Option<Arc<TcpStream>> {
        self.stream.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn has_client(&self) -> bool {
        self.stream().is_some()
    }

    fn drop_stream(&self) {
        *self.stream.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn recv(&self, n: usize) -> Vec<u8> {
        if self.stream().is_none() {
            let listener = self.listener.read().unwrap_or_else(|e| e.into_inner()).clone();
            let Some(listener) = listener else {
                return Vec::new();
            };
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_nonblocking(true);
                    *self.stream.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(stream));
                }
                Err(_) => return Vec::new(),
            }
        }
        let Some(stream) = self.stream() else {
            return Vec::new();
        };
        let mut buf = vec![0u8; n.max(1)];
        match (&*stream).read(&mut buf) {
            Ok(k) => {
                buf.truncate(k);
                buf
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Vec::new(),
            Err(_) => {
                self.drop_stream();
                Vec::new()
            }
        }
    }

    pub fn write(&self, buf: &[u8]) {
        let Some(stream) = self.stream() else {
            return;
        };
        if let Err(e) = (&*stream).write(buf) {
            if e.kind() == io::ErrorKind::BrokenPipe {
                self.drop_stream();
            }
        }
    }

    pub fn raw(&self) -> Option<RawHandle> {
        #[cfg(unix)]
        use std::os::unix::io::AsRawFd;
        #[cfg(windows)]
        use std::os::windows::io::AsRawSocket;
        if let Some(s) = self.stream() {
            #[cfg(unix)]
            return Some(s.as_raw_fd());
            #[cfg(windows)]
            return Some(s.as_raw_socket());
        }
        let listener = self.listener.read().unwrap_or_else(|e| e.into_inner()).clone()?;
        #[cfg(unix)]
        return Some(listener.as_raw_fd());
        #[cfg(windows)]
        return Some(listener.as_raw_socket());
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        match self.raw() {
            Some(h) => wait_readable(h, timeout),
            None => false,
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        let listener = self
            .listener
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(ebadf)?;
        listener.local_addr()
    }

    pub fn close(&self) {
        self.drop_stream();
        *self.listener.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

// ------------------------------------------------------------- serial ----

/// Why a serial port would not open, in pyserial's words.
pub struct SerialOpenError {
    pub errno: Option<i32>,
    pub message: String,
}

#[cfg(windows)]
type NativePort = serialport::COMPort;

pub struct Serial {
    #[cfg(windows)]
    port: Mutex<Option<NativePort>>,
    #[cfg(unix)]
    fd: Mutex<Option<std::os::unix::io::RawFd>>,
    #[cfg(unix)]
    write_lock: Mutex<()>,
    pub device: String,
    pub baud: u32,
    closed: AtomicBool,
}

#[cfg(unix)]
fn strerror(code: i32) -> String {
    let text = io::Error::from_raw_os_error(code).to_string();
    match text.rfind(" (os error") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

impl Serial {
    /// pyserial's open at 1200 baud (mavserial's workaround for kernels that
    /// otherwise keep the old rate), then the real rate.
    #[cfg(unix)]
    pub fn open(device: &str, baud: u32) -> Result<Serial, SerialOpenError> {
        Self::open_with(device, baud, true, (true, true))
    }

    /// `open` with DTR and RTS set to the given states (None leaves the usual
    /// asserted state). A transmitter module or ESP32 board wires them to its
    /// reset and boot pins, so they are set at once and never pulsed.
    pub fn open_held(device: &str, baud: u32, dtr: Option<bool>, rts: Option<bool>) -> Result<Serial, SerialOpenError> {
        Self::open_with(device, baud, true, (dtr.unwrap_or(true), rts.unwrap_or(true)))
    }

    /// pyserial's own open, straight at *baud*: for a device that is not a
    /// flight controller, where the 1200 baud touch means nothing or worse.
    pub fn open_direct(device: &str, baud: u32) -> Result<Serial, SerialOpenError> {
        Self::open_with(device, baud, false, (true, true))
    }

    #[cfg(unix)]
    fn open_with(device: &str, baud: u32, touch: bool, lines: (bool, bool)) -> Result<Serial, SerialOpenError> {
        use crate::link::serial_posix as ps;
        let fd = ps::open(device).map_err(|e| {
            let code = e.raw_os_error().unwrap_or(libc::EIO);
            SerialOpenError {
                errno: Some(code),
                message: format!(
                    "could not open port {device}: [Errno {code}] {}: '{device}'",
                    strerror(code)
                ),
            }
        })?;
        let setup = (|| -> io::Result<()> {
            ps::configure(fd, if touch { 1200 } else { baud }, false)?;
            if lines == (true, true) {
                ps::raise_modem_lines(fd, true)?;
            } else {
                ps::set_modem_lines(fd, lines.0, lines.1)?;
            }
            ps::flush_input(fd);
            ps::configure(fd, baud, false)
        })();
        if let Err(e) = setup {
            unsafe { libc::close(fd) };
            return Err(SerialOpenError {
                errno: e.raw_os_error(),
                message: e.to_string(),
            });
        }
        Ok(Serial {
            fd: Mutex::new(Some(fd)),
            write_lock: Mutex::new(()),
            device: device.to_string(),
            baud,
            closed: AtomicBool::new(false),
        })
    }

    #[cfg(windows)]
    pub fn open(device: &str, baud: u32) -> Result<Serial, SerialOpenError> {
        Self::open_with(device, baud, true, (true, true))
    }

    #[cfg(windows)]
    fn open_with(device: &str, baud: u32, touch: bool, lines: (bool, bool)) -> Result<Serial, SerialOpenError> {
        use serialport::SerialPort;
        let mut port = serialport::new(device, if touch { 1200 } else { baud })
            .timeout(Duration::from_secs(3600))
            .flow_control(serialport::FlowControl::None)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .open_native()
            .map_err(|e| Self::open_error(device, &e))?;
        let _ = port.write_data_terminal_ready(lines.0);
        let _ = port.write_request_to_send(lines.1);
        let _ = port.clear(serialport::ClearBuffer::Input);
        port.set_baud_rate(baud).map_err(|e| Self::open_error(device, &e))?;
        Ok(Serial {
            port: Mutex::new(Some(port)),
            device: device.to_string(),
            baud,
            closed: AtomicBool::new(false),
        })
    }

    #[cfg(windows)]
    fn open_error(device: &str, e: &serialport::Error) -> SerialOpenError {
        let errno = match e.kind() {
            serialport::ErrorKind::Io(kind) => match kind {
                io::ErrorKind::PermissionDenied => Some(13),
                io::ErrorKind::NotFound => Some(2),
                _ => None,
            },
            serialport::ErrorKind::NoDevice => Some(2),
            _ => None,
        };
        let description = e.description.clone();
        if cfg!(windows) {
            let detail = match errno {
                Some(13) => format!("PermissionError(13, '{description}', None, 5)"),
                Some(2) => format!("FileNotFoundError(2, '{description}', None, 2)"),
                _ => format!("OSError(22, '{description}', None, 87)"),
            };
            return SerialOpenError {
                errno: None,
                message: format!("could not open port '{device}': {detail}"),
            };
        }
        let code = errno.unwrap_or(5);
        SerialOpenError {
            errno: Some(code),
            message: format!("could not open port {device}: [Errno {code}] {description}: '{device}'"),
        }
    }

    /// Non-blocking read of up to `n` bytes, with pyserial's verdict on a
    /// port that says it is readable and then returns nothing.
    pub fn recv(&self, n: usize) -> io::Result<Vec<u8>> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::other("Attempting to use a port that is not open"));
        }
        #[cfg(unix)]
        {
            let Some(fd) = *lock(&self.fd) else {
                return Err(ebadf());
            };
            if !wait_readable(fd, 0.0) {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u8; n.max(1)];
            let rc = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) {
                    return Ok(Vec::new());
                }
                return Err(io::Error::new(err.kind(), format!("read failed: {err}")));
            }
            if rc == 0 {
                return Err(io::Error::other(
                    "device reports readiness to read but returned no data (device disconnected or multiple access on port?)",
                ));
            }
            buf.truncate(rc as usize);
            Ok(buf)
        }
        #[cfg(windows)]
        {
            use serialport::SerialPort;
            let mut guard = lock(&self.port);
            let Some(port) = guard.as_mut() else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "Attempting to use a port that is not open",
                ));
            };
            let waiting = port.bytes_to_read().map_err(io::Error::from)? as usize;
            let take = waiting.min(n);
            if take == 0 {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u8; take];
            let k = port.read(&mut buf)?;
            buf.truncate(k);
            Ok(buf)
        }
    }

    /// Drop what the driver has received and not been read, pyserial's
    /// `reset_input_buffer`.
    pub fn reset_input(&self) -> io::Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::other("Attempting to use a port that is not open"));
        }
        #[cfg(unix)]
        if let Some(fd) = *lock(&self.fd) {
            crate::link::serial_posix::flush_input(fd);
        }
        #[cfg(windows)]
        {
            use serialport::SerialPort;
            if let Some(port) = lock(&self.port).as_ref() {
                port.clear(serialport::ClearBuffer::Input).map_err(io::Error::from)?;
            }
        }
        Ok(())
    }

    /// Bytes the driver holds that have not been read, pyserial's
    /// `in_waiting`.
    pub fn waiting(&self) -> io::Result<usize> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::other("Attempting to use a port that is not open"));
        }
        #[cfg(unix)]
        {
            let Some(fd) = *lock(&self.fd) else {
                return Err(ebadf());
            };
            let mut n: libc::c_int = 0;
            if unsafe { libc::ioctl(fd, libc::FIONREAD as _, &mut n) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n.max(0) as usize)
        }
        #[cfg(windows)]
        {
            use serialport::SerialPort;
            let guard = lock(&self.port);
            let Some(port) = guard.as_ref() else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "Attempting to use a port that is not open",
                ));
            };
            Ok(port.bytes_to_read().map_err(io::Error::from)? as usize)
        }
    }

    /// pyserial's `read(n)` with a timeout: up to *n* bytes, returning early
    /// only once *n* have come or *timeout* seconds have passed.
    pub fn read_timeout(&self, n: usize, timeout: f64) -> io::Result<Vec<u8>> {
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        let mut out = Vec::new();
        while out.len() < n {
            let left = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_secs_f64();
            if !self.wait_readable(left) {
                if self.closed.load(Ordering::SeqCst) {
                    return Err(io::Error::other("Attempting to use a port that is not open"));
                }
                break;
            }
            out.extend(self.recv(n - out.len())?);
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
        Ok(out)
    }

    /// All of *buf*, or a timeout error once *timeout* seconds have passed
    /// with the driver's buffer still full.
    pub fn write_timeout(&self, buf: &[u8], timeout: f64) -> io::Result<usize> {
        #[cfg(unix)]
        {
            if self.closed.load(Ordering::SeqCst) {
                return Err(io::Error::other("Attempting to use a port that is not open"));
            }
            let _guard = lock(&self.write_lock);
            let Some(fd) = *lock(&self.fd) else {
                return Err(ebadf());
            };
            let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
            let mut rest = buf;
            while !rest.is_empty() {
                let rc = unsafe { libc::write(fd, rest.as_ptr() as *const libc::c_void, rest.len()) };
                if rc < 0 {
                    let err = io::Error::last_os_error();
                    match err.kind() {
                        io::ErrorKind::Interrupted => continue,
                        io::ErrorKind::WouldBlock => {
                            let left = deadline
                                .saturating_duration_since(std::time::Instant::now())
                                .as_secs_f64();
                            if left <= 0.0 || !wait_writable(fd, left) {
                                return Err(io::Error::new(io::ErrorKind::TimedOut, "Write timeout"));
                            }
                            continue;
                        }
                        _ => return Err(err),
                    }
                }
                rest = &rest[rc as usize..];
            }
            Ok(buf.len())
        }
        #[cfg(windows)]
        {
            let _ = timeout;
            self.write(buf)
        }
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::other("Attempting to use a port that is not open"));
        }
        #[cfg(unix)]
        {
            let _guard = lock(&self.write_lock);
            let Some(fd) = *lock(&self.fd) else {
                return Err(ebadf());
            };
            crate::link::serial_posix::write_all(fd, buf)
        }
        #[cfg(windows)]
        {
            let mut guard = lock(&self.port);
            let Some(port) = guard.as_mut() else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "Attempting to use a port that is not open",
                ));
            };
            port.write_all(buf)?;
            Ok(buf.len())
        }
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        #[cfg(unix)]
        {
            match *lock(&self.fd) {
                Some(fd) if !self.closed.load(Ordering::SeqCst) => wait_readable(fd, timeout),
                _ => false,
            }
        }
        #[cfg(not(unix))]
        {
            std::thread::sleep(Duration::from_secs_f64(timeout.clamp(0.0, 0.5)));
            true
        }
    }

    pub fn set_rtscts(&self, enable: bool) {
        #[cfg(unix)]
        if let Some(fd) = *lock(&self.fd) {
            let _ = crate::link::serial_posix::set_rtscts(fd, enable);
        }
        #[cfg(windows)]
        {
            use serialport::SerialPort;
            if let Some(port) = lock(&self.port).as_mut() {
                let flow = if enable {
                    serialport::FlowControl::Hardware
                } else {
                    serialport::FlowControl::None
                };
                let _ = port.set_flow_control(flow);
            }
        }
    }

    pub fn set_baudrate(&self, baud: u32) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(fd) = *lock(&self.fd) {
            crate::link::serial_posix::configure(fd, baud, false)?;
        }
        #[cfg(windows)]
        {
            use serialport::SerialPort;
            if let Some(port) = lock(&self.port).as_mut() {
                port.set_baud_rate(baud).map_err(io::Error::from)?;
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    pub fn fileno(&self) -> Option<i64> {
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        lock(&self.fd).map(|fd| fd as i64)
    }

    #[cfg(not(unix))]
    pub fn fileno(&self) -> Option<i64> {
        None
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        {
            let _guard = lock(&self.write_lock);
            if let Some(fd) = lock(&self.fd).take() {
                unsafe { libc::close(fd) };
            }
        }
        #[cfg(windows)]
        {
            *lock(&self.port) = None;
        }
    }
}

// ------------------------------------------------- Unix domain socket ----

/// `mavuds`: a stream socket to a filesystem path, with mavtcp's handling.
#[cfg(unix)]
pub struct Uds {
    stream: RwLock<Option<Arc<std::os::unix::net::UnixStream>>>,
    pub path: String,
    pub autoreconnect: bool,
    pub retries: i64,
}

#[cfg(unix)]
impl Uds {
    /// `mavuds.do_connect`: one second between attempts, as pymavlink has it.
    pub fn connect(
        path: &str,
        retries: i64,
        report: &mut dyn FnMut(&io::Error),
    ) -> io::Result<std::os::unix::net::UnixStream> {
        let mut retries = if retries <= 0 { 1 } else { retries };
        loop {
            retries -= 1;
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(stream) => {
                    stream.set_nonblocking(true)?;
                    return Ok(stream);
                }
                Err(e) => {
                    if retries <= 0 {
                        return Err(e);
                    }
                    report(&e);
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    pub fn new(stream: std::os::unix::net::UnixStream, path: &str, autoreconnect: bool, retries: i64) -> Uds {
        Uds {
            stream: RwLock::new(Some(Arc::new(stream))),
            path: path.to_string(),
            autoreconnect,
            retries,
        }
    }

    fn stream(&self) -> Option<Arc<std::os::unix::net::UnixStream>> {
        self.stream.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_open(&self) -> bool {
        self.stream().is_some()
    }

    pub fn replace(&self, stream: Option<std::os::unix::net::UnixStream>) {
        *self.stream.write().unwrap_or_else(|e| e.into_inner()) = stream.map(Arc::new);
    }

    pub fn recv(&self, n: usize) -> (io::Result<Vec<u8>>, Option<TcpEvent>) {
        let Some(stream) = self.stream() else {
            return (Err(ebadf()), None);
        };
        let mut buf = vec![0u8; n.max(1)];
        match (&*stream).read(&mut buf) {
            Ok(0) => (Ok(Vec::new()), Some(TcpEvent::Eof)),
            Ok(k) => {
                buf.truncate(k);
                (Ok(buf), None)
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => (Ok(Vec::new()), None),
            Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => {
                (Err(e), Some(TcpEvent::Disconnect))
            }
            Err(e) => (Err(e), None),
        }
    }

    pub fn write(&self, buf: &[u8]) -> Option<TcpEvent> {
        let stream = self.stream()?;
        match (&*stream).write(buf) {
            Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => {
                Some(TcpEvent::Disconnect)
            }
            _ => None,
        }
    }

    pub fn raw(&self) -> Option<RawHandle> {
        use std::os::unix::io::AsRawFd;
        self.stream().map(|s| s.as_raw_fd())
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        match self.raw() {
            Some(h) => wait_readable(h, timeout),
            None => {
                std::thread::sleep(Duration::from_secs_f64(timeout.clamp(0.0, 0.5)));
                true
            }
        }
    }

    pub fn close(&self) {
        self.replace(None);
    }
}

// ---------------------------------------------------------- multicast ----

/// `mavmcast`: a receive socket joined to the group and a separate send
/// socket, whose own port identifies (and drops) what this side sent.
pub struct Mcast {
    rx: RwLock<Option<Arc<UdpSocket>>>,
    tx: RwLock<Option<Arc<UdpSocket>>>,
    myport: Mutex<Option<u16>>,
}

impl Mcast {
    pub fn open(group: &str, port: u16) -> io::Result<Mcast> {
        let group_ip: std::net::Ipv4Addr = group.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("illegal IP address string passed to inet_aton: {group}"),
            )
        })?;
        let rx = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        rx.set_reuse_address(true)?;
        let bind_ip = if cfg!(windows) {
            std::net::Ipv4Addr::UNSPECIFIED
        } else {
            group_ip
        };
        rx.bind(&SocketAddr::from((bind_ip, port)).into())?;
        rx.join_multicast_v4(&group_ip, &std::net::Ipv4Addr::UNSPECIFIED)?;
        rx.set_nonblocking(true)?;
        let tx = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
        tx.set_reuse_address(true)?;
        tx.set_nonblocking(true)?;
        tx.connect(&SocketAddr::from((group_ip, port)).into())?;
        Ok(Mcast {
            rx: RwLock::new(Some(Arc::new(rx.into()))),
            tx: RwLock::new(Some(Arc::new(tx.into()))),
            myport: Mutex::new(None),
        })
    }

    pub fn recv(&self) -> io::Result<Vec<u8>> {
        let rx = self
            .rx
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(ebadf)?;
        let mut buf = vec![0u8; UDP_MAX_PACKET_LEN];
        let (n, addr) = match rx.recv_from(&mut buf) {
            Ok(v) => v,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(Vec::new())
            }
            Err(e) => return Err(e),
        };
        let mut myport = lock(&self.myport);
        if myport.is_none() {
            if let Some(tx) = self.tx.read().unwrap_or_else(|e| e.into_inner()).clone() {
                *myport = tx.local_addr().ok().map(|a| a.port());
            }
        }
        if *myport == Some(addr.port()) {
            return Ok(Vec::new());
        }
        buf.truncate(n);
        Ok(buf)
    }

    pub fn write(&self, buf: &[u8]) {
        if let Some(tx) = self.tx.read().unwrap_or_else(|e| e.into_inner()).clone() {
            let _ = tx.send(buf);
        }
    }

    pub fn raw(&self) -> Option<RawHandle> {
        let rx = self.rx.read().unwrap_or_else(|e| e.into_inner()).clone()?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            Some(rx.as_raw_fd())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            Some(rx.as_raw_socket())
        }
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        self.raw().map(|h| wait_readable(h, timeout)).unwrap_or(false)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.rx
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(ebadf)?
            .local_addr()
    }

    pub fn close(&self) {
        *self.rx.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self.tx.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

// ------------------------------------------------------ child process ----

/// `mavchildexec`: MAVLink over a child's stdin and stdout. Closing ends
/// the child the way pymavlink does (terminate, five seconds, kill), so it
/// never outlives the link.
#[cfg(unix)]
pub struct ChildExec {
    child: Mutex<Option<std::process::Child>>,
    stdin: Mutex<Option<std::process::ChildStdin>>,
    stdout_fd: Mutex<Option<std::os::unix::io::RawFd>>,
    stdout: Mutex<Option<std::process::ChildStdout>>,
}

#[cfg(unix)]
impl ChildExec {
    pub fn spawn(path: &str) -> io::Result<ChildExec> {
        use std::os::unix::io::AsRawFd;
        let mut child = std::process::Command::new(path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let fd = stdout.as_ref().map(|s| s.as_raw_fd());
        if let Some(fd) = fd {
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Ok(ChildExec {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(stdin),
            stdout_fd: Mutex::new(fd),
            stdout: Mutex::new(stdout),
        })
    }

    /// One byte at a time, as pymavlink reads a child.
    pub fn recv(&self) -> Vec<u8> {
        let mut guard = lock(&self.stdout);
        let Some(out) = guard.as_mut() else {
            return Vec::new();
        };
        let mut b = [0u8; 1];
        match out.read(&mut b) {
            Ok(1) => b.to_vec(),
            _ => Vec::new(),
        }
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<()> {
        match lock(&self.stdin).as_mut() {
            Some(stdin) => stdin.write_all(buf),
            None => Err(ebadf()),
        }
    }

    pub fn raw(&self) -> Option<RawHandle> {
        *lock(&self.stdout_fd)
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        self.raw().map(|h| wait_readable(h, timeout)).unwrap_or(false)
    }

    pub fn close(&self) {
        *lock(&self.stdin) = None;
        *lock(&self.stdout) = None;
        *lock(&self.stdout_fd) = None;
        if let Some(mut child) = lock(&self.child).take() {
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                    _ => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(unix)]
impl Drop for ChildExec {
    fn drop(&mut self) {
        self.close();
    }
}

// ---------------------------------------------------------- log files ----

/// The file under `mavlogfile`: tlogs (an 8-byte big-endian microsecond
/// stamp before every frame), Mission Planner's text-stamped logs, and raw
/// MAVLink with no stamps at all.
pub struct LogFile {
    file: Mutex<Option<std::fs::File>>,
    pub filename: String,
    pub filesize: u64,
}

impl LogFile {
    pub fn open(filename: &str, write: bool, append: bool) -> io::Result<LogFile> {
        let file = if write {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .append(append)
                .truncate(!append)
                .open(filename)?
        } else {
            std::fs::File::open(filename)?
        };
        let filesize = std::fs::metadata(filename)?.len();
        Ok(LogFile {
            file: Mutex::new(Some(file)),
            filename: filename.to_string(),
            filesize,
        })
    }

    pub fn read(&self, n: usize) -> io::Result<Vec<u8>> {
        let mut guard = lock(&self.file);
        let Some(f) = guard.as_mut() else {
            return Err(io::Error::other("read of closed file"));
        };
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match f.read(&mut buf[got..])? {
                0 => break,
                k => got += k,
            }
        }
        buf.truncate(got);
        Ok(buf)
    }

    pub fn tell(&self) -> u64 {
        use std::io::Seek;
        lock(&self.file)
            .as_mut()
            .and_then(|f| f.stream_position().ok())
            .unwrap_or(0)
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<()> {
        match lock(&self.file).as_mut() {
            Some(f) => f.write_all(buf),
            None => Err(io::Error::other("write to closed file")),
        }
    }

    pub fn close(&self) {
        *lock(&self.file) = None;
    }
}
