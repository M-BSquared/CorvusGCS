//! MAVLink over WebSocket: the client (`ws:` and `wss:`) and the single
//! client server (`wsserver:`) pymavlink's `mavwebsocket_client` and
//! `mavwebsocket` are, on tungstenite for the protocol and rustls for TLS.
//!
//! Binary messages carry the MAVLink bytes in both directions. A text frame
//! is ignored. A close frame ends the exchange. The client reconnects when
//! the socket is reset and on every call while it has no connection; the
//! server takes one client at a time and goes back to accepting when that
//! client leaves. Reads never block: a message that is not there yet is an
//! empty answer.
//!
//! `wss:` trusts the system's roots (and the file `SSL_CERT_FILE` names, as
//! Python's `ssl.create_default_context()` does) and checks the host name.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tungstenite::handshake::server::NoCallback;
use tungstenite::handshake::{HandshakeError, MidHandshake};
use tungstenite::{Error as WsFrameError, Message, ServerHandshake, WebSocket};

use crate::link::transport::{resolve_v4, wait_readable, RawHandle};
use crate::lock;

/// What opening or using a WebSocket link can fail with, by the Python
/// exception it becomes.
pub enum WsError {
    Value(String),
    Os(io::Error),
    Runtime(String),
    #[cfg_attr(feature = "tls", allow(dead_code))]
    Unsupported(String),
}

impl From<io::Error> for WsError {
    fn from(e: io::Error) -> Self {
        WsError::Os(e)
    }
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn raw_of(stream: &TcpStream) -> RawHandle {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        stream.as_raw_fd()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        stream.as_raw_socket()
    }
}

fn is_gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionAborted
    )
}

fn is_unreachable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::HostUnreachable
    )
}

/// Python's `int(text)` for a port, with its message.
fn parse_port(text: &str) -> Result<u16, WsError> {
    let trimmed = text.trim();
    let digits = trimmed.strip_prefix('+').unwrap_or(trimmed);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_') {
        return Err(WsError::Value(format!(
            "invalid literal for int() with base 10: '{text}'"
        )));
    }
    digits.replace('_', "").parse::<u16>().map_err(|_| {
        WsError::Os(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bind(): port must be 0-65535.",
        ))
    })
}

// -------------------------------------------------------------- stream ----

enum Stream {
    Plain(TcpStream),
    #[cfg(feature = "tls")]
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            #[cfg(feature = "tls")]
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => s.flush(),
        }
    }
}

#[cfg(feature = "tls")]
fn tls_stream(host: &str, tcp: TcpStream) -> Result<Stream, WsError> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, ServerName};

    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if let Some(file) = std::env::var_os("SSL_CERT_FILE") {
        if let Ok(iter) = CertificateDer::pem_file_iter(&file) {
            for cert in iter.flatten() {
                let _ = roots.add(cert);
            }
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| WsError::Runtime(format!("TLS setup failed: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).map_err(|e| {
        WsError::Os(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid server name {host}: {e}"),
        ))
    })?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name)
        .map_err(|e| WsError::Os(io::Error::new(io::ErrorKind::InvalidData, e.to_string())))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

#[cfg(not(feature = "tls"))]
fn tls_stream(_host: &str, _tcp: TcpStream) -> Result<Stream, WsError> {
    Err(WsError::Unsupported(
        "corvus_link was built without TLS, so wss: links are unavailable".into(),
    ))
}

// -------------------------------------------------------------- client ----

struct ClientState {
    ws: Option<WebSocket<Stream>>,
    buffer: Vec<u8>,
}

/// `ws:host:port[:resource]` and `wss:host:port[:resource]`.
pub struct WsClient {
    host: String,
    port: u16,
    resource: String,
    tls: bool,
    state: Mutex<ClientState>,
    raw: Mutex<Option<RawHandle>>,
}

impl WsClient {
    pub fn open(device: &str) -> Result<WsClient, WsError> {
        let a: Vec<&str> = device.split(':').collect();
        if a.len() < 3 {
            return Err(WsError::Value(
                "WebSocket ports must be specified as protocol:host:port".into(),
            ));
        }
        let client = WsClient {
            host: a[1].to_string(),
            port: parse_port(a[2])?,
            resource: if a.len() > 3 { a[3].to_string() } else { "/".to_string() },
            tls: a[0].eq_ignore_ascii_case("wss"),
            state: Mutex::new(ClientState {
                ws: None,
                buffer: Vec::new(),
            }),
            raw: Mutex::new(None),
        };
        client.connect()?;
        Ok(client)
    }

    fn drop_socket(&self) {
        let mut st = lock(&self.state);
        if let Some(ws) = st.ws.take() {
            let _ = ws.get_ref().tcp().shutdown(std::net::Shutdown::Both);
        }
        *lock(&self.raw) = None;
    }

    /// Open the TCP connection (and TLS), then the WebSocket handshake. A
    /// refused or unreachable host leaves the link without a socket, to be
    /// tried again by the next call.
    fn connect(&self) -> Result<(), WsError> {
        self.drop_socket();
        let tcp = match TcpStream::connect((self.host.as_str(), self.port)) {
            Ok(s) => s,
            Err(e) if is_unreachable(&e) => return Ok(()),
            Err(e) => return Err(WsError::Os(e)),
        };
        tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let stream = if self.tls {
            tls_stream(&self.host, tcp)?
        } else {
            Stream::Plain(tcp)
        };
        let resource = if self.resource.starts_with('/') {
            self.resource.clone()
        } else {
            format!("/{}", self.resource)
        };
        let scheme = if self.tls { "wss" } else { "ws" };
        let uri = format!("{scheme}://{}:{}{resource}", self.host, self.port);
        let (ws, _) = tungstenite::client(uri.as_str(), stream).map_err(|e| match e {
            HandshakeError::Failure(WsFrameError::Io(io)) if self.tls && io.kind() == io::ErrorKind::InvalidData => {
                WsError::Os(io)
            }
            HandshakeError::Failure(WsFrameError::Io(io)) if io.kind() == io::ErrorKind::UnexpectedEof => {
                WsError::Runtime("WebSocket handshake failed".into())
            }
            HandshakeError::Failure(e) => WsError::Runtime(format!("WebSocket handshake failed: {e}")),
            HandshakeError::Interrupted(_) => WsError::Runtime("WebSocket handshake failed".into()),
        })?;
        ws.get_ref().tcp().set_read_timeout(None)?;
        ws.get_ref().tcp().set_write_timeout(None)?;
        ws.get_ref().tcp().set_nonblocking(true)?;
        *lock(&self.raw) = Some(raw_of(ws.get_ref().tcp()));
        lock(&self.state).ws = Some(ws);
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        lock(&self.state).ws.is_some()
    }

    pub fn recv(&self, n: usize) -> Result<Vec<u8>, WsError> {
        if !self.is_open() {
            self.connect()?;
            return Ok(Vec::new());
        }
        let mut reconnect = false;
        let out = {
            let mut st = lock(&self.state);
            if !st.buffer.is_empty() {
                return Ok(std::mem::take(&mut st.buffer));
            }
            let mut out = Vec::new();
            loop {
                let Some(ws) = st.ws.as_mut() else { break };
                match ws.read() {
                    Ok(Message::Binary(b)) => {
                        out.extend_from_slice(&b);
                        if out.len() >= n {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => {
                        out.clear();
                        break;
                    }
                    Ok(_) => continue,
                    Err(WsFrameError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(WsFrameError::Io(e)) if is_gone(&e) => {
                        reconnect = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            if let Some(ws) = st.ws.as_mut() {
                let _ = ws.flush();
            }
            out
        };
        if reconnect {
            self.connect()?;
            return Ok(Vec::new());
        }
        Ok(out)
    }

    pub fn write(&self, buf: &[u8]) -> Result<(), WsError> {
        if !self.is_open() {
            self.connect()?;
            return Ok(());
        }
        let failed = {
            let mut st = lock(&self.state);
            match st.ws.as_mut() {
                Some(ws) => {
                    matches!(ws.send(Message::Binary(buf.to_vec().into())), Err(WsFrameError::Io(e)) if is_gone(&e))
                }
                None => false,
            }
        };
        if failed {
            self.connect()?;
        }
        Ok(())
    }

    pub fn raw(&self) -> Option<RawHandle> {
        *lock(&self.raw)
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
        self.drop_socket();
    }
}

// -------------------------------------------------------------- server ----

enum Peer {
    None,
    Handshaking(Box<MidHandshake<ServerHandshake<TcpStream, NoCallback>>>),
    Open(Box<WebSocket<TcpStream>>),
}

/// `wsserver:host:port`: one client at a time.
pub struct WsServer {
    listener: RwLock<Option<Arc<TcpListener>>>,
    peer: Mutex<Peer>,
    peer_raw: Mutex<Option<RawHandle>>,
}

impl WsServer {
    pub fn open(device: &str) -> Result<WsServer, WsError> {
        let a: Vec<&str> = device.split(':').collect();
        if a.len() != 2 {
            return Err(WsError::Value("WebSocket ports must be specified as host:port".into()));
        }
        let port = parse_port(a[1])?;
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_reuse_address(true)?;
        socket.bind(&resolve_v4(a[0], port)?.into())?;
        socket.listen(1)?;
        socket.set_nonblocking(true)?;
        let _ = socket.set_nodelay(true);
        Ok(WsServer {
            listener: RwLock::new(Some(Arc::new(socket.into()))),
            peer: Mutex::new(Peer::None),
            peer_raw: Mutex::new(None),
        })
    }

    fn listener(&self) -> Option<Arc<TcpListener>> {
        self.listener.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn drop_peer(&self, peer: &mut Peer) {
        match std::mem::replace(peer, Peer::None) {
            Peer::Open(ws) => {
                let _ = ws.get_ref().shutdown(std::net::Shutdown::Both);
            }
            Peer::Handshaking(mid) => {
                let _ = mid.get_ref().get_ref().shutdown(std::net::Shutdown::Both);
            }
            Peer::None => {}
        }
        *lock(&self.peer_raw) = None;
    }

    pub fn has_client(&self) -> bool {
        !matches!(*lock(&self.peer), Peer::None)
    }

    pub fn recv(&self, n: usize) -> Vec<u8> {
        let mut peer = lock(&self.peer);
        if matches!(*peer, Peer::None) {
            let Some(listener) = self.listener() else {
                return Vec::new();
            };
            let Ok((stream, _)) = listener.accept() else {
                return Vec::new();
            };
            let _ = stream.set_nodelay(true);
            let _ = stream.set_nonblocking(true);
            *lock(&self.peer_raw) = Some(raw_of(&stream));
            *peer = match tungstenite::accept(stream) {
                Ok(ws) => Peer::Open(Box::new(ws)),
                Err(HandshakeError::Interrupted(mid)) => Peer::Handshaking(Box::new(mid)),
                Err(HandshakeError::Failure(_)) => {
                    *lock(&self.peer_raw) = None;
                    Peer::None
                }
            };
        } else if matches!(*peer, Peer::Handshaking(_)) {
            let Peer::Handshaking(mid) = std::mem::replace(&mut *peer, Peer::None) else {
                return Vec::new();
            };
            *peer = match mid.handshake() {
                Ok(ws) => Peer::Open(Box::new(ws)),
                Err(HandshakeError::Interrupted(mid)) => Peer::Handshaking(Box::new(mid)),
                Err(HandshakeError::Failure(_)) => {
                    *lock(&self.peer_raw) = None;
                    Peer::None
                }
            };
        }
        let mut out = Vec::new();
        let mut leave = false;
        if let Peer::Open(ws) = &mut *peer {
            loop {
                match ws.read() {
                    Ok(Message::Binary(b)) => {
                        out.extend_from_slice(&b);
                        if out.len() >= n {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => {
                        leave = true;
                        break;
                    }
                    Ok(_) => continue,
                    Err(WsFrameError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        out.clear();
                        leave = true;
                        break;
                    }
                }
            }
            if !leave {
                let _ = ws.flush();
            }
        }
        if leave {
            if let Peer::Open(ws) = &mut *peer {
                let _ = ws.flush();
            }
            self.drop_peer(&mut peer);
        }
        out
    }

    pub fn write(&self, buf: &[u8]) {
        let mut peer = lock(&self.peer);
        let gone = match &mut *peer {
            Peer::Open(ws) => {
                matches!(ws.send(Message::Binary(buf.to_vec().into())), Err(WsFrameError::Io(e)) if is_gone(&e))
            }
            _ => false,
        };
        if gone {
            self.drop_peer(&mut peer);
        }
    }

    pub fn raw(&self) -> Option<RawHandle> {
        if let Some(h) = *lock(&self.peer_raw) {
            return Some(h);
        }
        let listener = self.listener()?;
        Some(raw_of_listener(&listener))
    }

    pub fn wait_readable(&self, timeout: f64) -> bool {
        match self.raw() {
            Some(h) => wait_readable(h, timeout),
            None => false,
        }
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener()
            .ok_or_else(|| io::Error::from_raw_os_error(if cfg!(unix) { 9 } else { 10038 }))?
            .local_addr()
    }

    pub fn close(&self) {
        {
            let mut peer = lock(&self.peer);
            self.drop_peer(&mut peer);
        }
        *self.listener.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn raw_of_listener(listener: &TcpListener) -> RawHandle {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        listener.as_raw_fd()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        listener.as_raw_socket()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(text: &str) -> Result<u16, String> {
        match parse_port(text) {
            Ok(p) => Ok(p),
            Err(WsError::Value(m)) => Err(format!("value: {m}")),
            Err(WsError::Os(e)) => Err(format!("os: {e}")),
            Err(_) => Err("other".into()),
        }
    }

    #[test]
    fn a_port_reads_like_pythons_int() {
        assert_eq!(port("14550"), Ok(14550));
        assert_eq!(port(" 80 "), Ok(80));
        assert_eq!(port("+81"), Ok(81));
        assert_eq!(port("1_0"), Ok(10));
        assert_eq!(port("0"), Ok(0));
    }

    #[test]
    fn a_port_that_is_not_a_number_names_what_was_given() {
        assert_eq!(
            port("x"),
            Err("value: invalid literal for int() with base 10: 'x'".to_string())
        );
        assert!(port("").unwrap_err().starts_with("value:"));
        assert!(port("-1").unwrap_err().starts_with("value:"));
    }

    #[test]
    fn a_port_past_the_range_fails_as_the_bind_would() {
        assert!(port("70000").unwrap_err().starts_with("os:"));
    }
}
