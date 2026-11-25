//! The accept loop and the connection threads.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::mcp::{self, McpHandle};
use super::security::{self, ApiKey, SECURITY_HEADERS};
use super::static_files::{http_date, serve};
use crate::core::Core;
use crate::module::{Module, Registry};
use crate::request::{Method, Origin, Request};
use crate::response::{Body, Response, SseNext};
use crate::router;
use crate::ws::{self, WebSocket};

/// How long a socket may sit silent mid request or mid body (`timeout = 120`).
const IDLE_LIMIT: Duration = Duration::from_secs(120);
/// How often a blocked wait looks at whether the core is stopping.
const SLICE: Duration = Duration::from_millis(200);
/// How long a refusal waits for the client to stop sending the body it refused.
const LINGER: Duration = Duration::from_secs(2);
const MAX_HEAD: usize = 64 * 1024;
/// How many consecutive ports a launch tries (`PORT_SEARCH_SPAN`).
pub const PORT_SEARCH_SPAN: u16 = 20;

#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// The port to try first; `0` takes any free one.
    pub port: u16,
    /// The interface; `CORVUS_BIND` or loopback when `None`.
    pub host: Option<String>,
    pub span: u16,
    /// Try the config's `http_port` instead of `port`, then the next ones,
    /// then any free port: an address an AI client keeps across restarts.
    pub from_config: bool,
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions {
            port: 8000,
            host: None,
            span: PORT_SEARCH_SPAN,
            from_config: false,
        }
    }
}

/// Where the server listens, once it does: `core.service::<HttpInfo>()`.
#[derive(Default)]
pub struct HttpInfo {
    addr: Mutex<Option<SocketAddr>>,
}

impl HttpInfo {
    pub fn addr(&self) -> Option<SocketAddr> {
        *self.addr.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `http://127.0.0.1:PORT`.
    pub fn url(&self) -> Option<String> {
        self.addr().map(|a| {
            format!(
                "http://{}:{}",
                if a.ip().is_unspecified() {
                    Ipv4Addr::LOCALHOST.into()
                } else {
                    a.ip()
                },
                a.port()
            )
        })
    }
}

struct Running {
    addr: SocketAddr,
    accept: JoinHandle<()>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

pub struct Http {
    opts: HttpOptions,
    info: Arc<HttpInfo>,
    running: Mutex<Option<Running>>,
}

impl Http {
    pub fn new(opts: HttpOptions) -> Http {
        Http {
            opts,
            info: Arc::new(HttpInfo::default()),
            running: Mutex::new(None),
        }
    }
}

/// Whether another program holds `port` on `[::1]`. `localhost` resolves to
/// `::1` first on macOS and Windows, so a window would otherwise show that
/// program's page.
fn ipv6_loopback_taken(port: u16) -> bool {
    matches!(TcpListener::bind(("::1", port)), Err(e) if e.kind() == std::io::ErrorKind::AddrInUse)
}

/// The config's `http_port` as a port to bind; out of range is the default.
fn configured_port(port: i64) -> u16 {
    u16::try_from(port).unwrap_or(crate::config::DEFAULT_HTTP_PORT as u16)
}

/// A listener actually bound to `port` or the next free one: the bind is the
/// test, so two Corvus windows launched together cannot both be told it is free.
pub fn bind(host: &str, port: u16, span: u16) -> std::io::Result<TcpListener> {
    let span = span.max(1);
    for candidate in (port as u32)..(port as u32 + span as u32) {
        let candidate = candidate as u16;
        if host == security::DEFAULT_BIND_HOST && candidate != 0 && ipv6_loopback_taken(candidate) {
            eprintln!("corvus: port {candidate} is taken on [::1]; looking for a free one");
            continue;
        }
        match TcpListener::bind((host, candidate)) {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if candidate == port {
                    eprintln!("corvus: port {port} is taken; looking for a free one");
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        format!("no free port in {}-{}", port, port as u32 + span as u32 - 1),
    ))
}

impl Module for Http {
    fn name(&self) -> &'static str {
        "http"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(self.info.clone());
        match ApiKey::from_env_or_random() {
            Ok(key) => reg.service(Arc::new(key)),
            Err(e) => reg.errors.push(e),
        }
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        let host = self.opts.host.clone().unwrap_or_else(security::bind_host);
        let listener = if self.opts.from_config {
            let port = configured_port(core.settings().get().http_port());
            match bind(&host, port, self.opts.span) {
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    eprintln!("corvus: {e}; taking any free port");
                    bind(&host, 0, 1)?
                }
                other => other?,
            }
        } else {
            bind(&host, self.opts.port, self.opts.span)?
        };
        let addr = listener.local_addr()?;
        *self.info.addr.lock().unwrap_or_else(|e| e.into_inner()) = Some(addr);
        core.advertise_port(addr.port());
        let conns: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::default();
        let accept = {
            let (core, conns) = (core.clone(), conns.clone());
            std::thread::Builder::new()
                .name("corvus-http".into())
                .spawn(move || accept_loop(listener, core, conns))?
        };
        *self.running.lock().unwrap_or_else(|e| e.into_inner()) = Some(Running { addr, accept, conns });
        Ok(())
    }

    fn stop(&self, _core: &Core) {
        let Some(r) = self.running.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        let wake = SocketAddr::new(
            if r.addr.ip().is_unspecified() {
                Ipv4Addr::LOCALHOST.into()
            } else {
                r.addr.ip()
            },
            r.addr.port(),
        );
        let _ = TcpStream::connect_timeout(&wake, Duration::from_secs(1));
        let _ = r.accept.join();
        let handles: Vec<_> = std::mem::take(&mut *r.conns.lock().unwrap_or_else(|e| e.into_inner()));
        for h in handles {
            let _ = h.join();
        }
        *self.info.addr.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn accept_loop(listener: TcpListener, core: Core, conns: Arc<Mutex<Vec<JoinHandle<()>>>>) {
    for stream in listener.incoming() {
        if core.is_stopping() {
            break;
        }
        let Ok(stream) = stream else { continue };
        let _ = stream.set_nodelay(true);
        let c = core.clone();
        let spawned = std::thread::Builder::new()
            .name("corvus-http-conn".into())
            .spawn(move || serve_connection(&c, stream));
        let mut list = conns.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|h| !h.is_finished());
        match spawned {
            Ok(h) => list.push(h),
            Err(e) => eprintln!("corvus: could not start a connection thread: {e}"),
        }
    }
}

/// Reads until `n` more bytes are there, the peer is gone, the core stops or
/// the socket has been silent too long. Returns what it got.
fn read_more(stream: &mut TcpStream, core: &Core, buf: &mut Vec<u8>, until: impl Fn(&[u8]) -> bool) -> bool {
    let _ = stream.set_read_timeout(Some(SLICE));
    let mut last = Instant::now();
    let mut chunk = [0u8; 16384];
    while !until(buf) {
        if core.is_stopping() || last.elapsed() > IDLE_LIMIT {
            return false;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return false,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                last = Instant::now();
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return false,
        }
    }
    true
}

struct Head {
    method: String,
    target: String,
    headers: crate::request::Headers,
    rest: Vec<u8>,
}

enum HeadError {
    Gone,
    Bad(u16, &'static str),
}

fn read_head(stream: &mut TcpStream, core: &Core) -> Result<Head, HeadError> {
    let mut buf = Vec::new();
    let found = |b: &[u8]| b.windows(4).any(|w| w == b"\r\n\r\n") || b.len() > MAX_HEAD;
    if !read_more(stream, core, &mut buf, found) {
        return Err(HeadError::Gone);
    }
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err(HeadError::Bad(431, "Request header fields too large"));
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let rest = buf[end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() != 3 || !parts[2].starts_with("HTTP/") {
        return Err(HeadError::Bad(400, "Bad request syntax"));
    }
    let mut headers = crate::request::Headers::default();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push(k.trim(), v);
        }
    }
    Ok(Head {
        method: parts[0].to_string(),
        target: parts[1].to_string(),
        headers,
        rest,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        202 => "Accepted",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Request Entity Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Status line and headers as the Python handler sends them: `Server` and
/// `Date`, the route's headers, the CORS pair for JSON and event streams, and
/// the hardening headers unless the route set them. `allowed` is the request's
/// origin when it is trusted ([`security::cors_allowed_origin`]).
fn head_bytes(resp: &Response, allowed: Option<&str>, length: Option<usize>) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.0 {} {}\r\nServer: CorvusGCS\r\nDate: {}\r\n",
        resp.status,
        reason(resp.status),
        http_date(now_secs())
    );
    let mut names: Vec<String> = Vec::new();
    for (k, v) in &resp.headers {
        names.push(k.to_lowercase());
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(n) = length {
        out.push_str(&format!("Content-Length: {n}\r\n"));
    }
    let ctype = resp.header("content-type").unwrap_or("");
    if ctype == "application/json" || ctype == "text/event-stream" {
        if let Some(allowed) = allowed.filter(|a| !a.is_empty()) {
            out.push_str(&format!("Access-Control-Allow-Origin: {allowed}\r\n"));
        }
        out.push_str("Vary: Origin\r\n");
    }
    for (k, v) in SECURITY_HEADERS {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(k)) {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    out.push_str("\r\n");
    out.into_bytes()
}

fn write_response(stream: &mut TcpStream, resp: Response, origin: Option<&str>) {
    match resp.body {
        Body::Empty => {
            let len = (resp.status != 304 && resp.status != 101).then_some(0);
            let head = head_bytes(
                &Response {
                    body: Body::Empty,
                    ..resp
                },
                origin,
                len,
            );
            let _ = stream.write_all(&head);
        }
        Body::Bytes(ref body) => {
            let mut bytes = head_bytes(
                &Response {
                    status: resp.status,
                    headers: resp.headers.clone(),
                    body: Body::Empty,
                },
                origin,
                Some(body.len()),
            );
            bytes.extend_from_slice(body);
            let _ = stream.write_all(&bytes);
        }
        Body::Sse(_) | Body::Upgrade(_) => unreachable!("streams are written by their own functions"),
    }
    let _ = stream.flush();
}

fn write_sse(stream: &mut TcpStream, core: &Core, resp: Response, origin: Option<&str>) {
    let Body::Sse(mut s) = resp.body else { return };
    let head = head_bytes(
        &Response {
            status: resp.status,
            headers: resp.headers,
            body: Body::Empty,
        },
        origin,
        None,
    );
    let mut ok = stream.write_all(&head).is_ok();
    if ok {
        for e in s.open() {
            if stream.write_all(&e.frame()).is_err() {
                ok = false;
                break;
            }
        }
        let _ = stream.flush();
    }
    let mut quiet_since = Instant::now();
    while ok && !core.is_stopping() {
        match s.next(SLICE) {
            SseNext::End => break,
            SseNext::Idle => {
                if quiet_since.elapsed() >= s.ping_every() {
                    ok = stream.write_all(s.ping()).and_then(|_| stream.flush()).is_ok();
                    quiet_since = Instant::now();
                }
            }
            SseNext::Events(events) => {
                for e in events {
                    if stream.write_all(&e.frame()).is_err() {
                        ok = false;
                        break;
                    }
                }
                ok = ok && stream.flush().is_ok();
                quiet_since = Instant::now();
            }
        }
    }
    s.close();
}

fn run_upgrade(stream: TcpStream, up: crate::response::Upgrade) {
    let mut s = stream;
    if s.write_all(&ws::handshake_response(&up.key))
        .and_then(|_| s.flush())
        .is_err()
    {
        return;
    }
    let _ = s.set_read_timeout(None);
    if let Ok(sock) = WebSocket::new(s, ws::MAX_MESSAGE) {
        (up.run)(sock);
    }
}

/// Takes in the body a refusal left unread. Closing on unread bytes resets the
/// connection, and a client that is still sending loses the reason with it.
fn linger(stream: &mut TcpStream, core: &Core, mut unread: usize) {
    if unread == 0 || stream.shutdown(std::net::Shutdown::Write).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(SLICE));
    let until = Instant::now() + LINGER;
    let mut chunk = [0u8; 16384];
    while unread > 0 && !core.is_stopping() && Instant::now() < until {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => unread = unread.saturating_sub(n),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return,
        }
    }
}

fn plain(status: u16, text: &str) -> Response {
    Response::bytes(status, "text/html;charset=utf-8", format!("<!DOCTYPE HTML><html><head><title>Error response</title></head><body><h1>Error response</h1><p>Error code: {status}</p><p>Message: {text}.</p></body></html>").into_bytes())
}

fn serve_connection(core: &Core, mut stream: TcpStream) {
    let peer = stream.peer_addr().ok().map(|a| a.ip());
    let head = match read_head(&mut stream, core) {
        Ok(h) => h,
        Err(HeadError::Gone) => return,
        Err(HeadError::Bad(status, text)) => {
            write_response(&mut stream, plain(status, text), None);
            if status == 431 {
                linger(&mut stream, core, usize::MAX);
            }
            return;
        }
    };
    let origin = head.headers.get("Origin").map(str::to_string);
    let cors = security::cors_allowed_origin(origin.as_deref().unwrap_or(""), head.headers.get("Host"));
    let cors = (!cors.is_empty()).then_some(cors);
    let mut unread = head
        .headers
        .get("Content-Length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .saturating_sub(head.rest.len());
    let Some(method) = Method::parse(&head.method) else {
        write_response(
            &mut stream,
            plain(501, &format!("Unsupported method ('{}')", head.method)),
            cors.as_deref(),
        );
        linger(&mut stream, core, unread);
        return;
    };
    let mut req = Request::new(method, &head.target);
    req.headers = head.headers;
    req.peer = peer;
    req.origin = Origin::Http;

    if !security::host_header_allowed(req.headers.get("Host")) {
        eprintln!(
            "corvus: rejected request for Host {:?} on {}",
            req.headers.get("Host"),
            req.path
        );
        write_response(&mut stream, Response::not_ok(403, "host not allowed"), cors.as_deref());
        linger(&mut stream, core, unread);
        return;
    }
    if method != Method::Get && !cross_site_allowed(&req) {
        write_response(
            &mut stream,
            Response::not_ok(403, "cross-site request forbidden"),
            cors.as_deref(),
        );
        linger(&mut stream, core, unread);
        return;
    }
    if let Some(refusal) = refused(core, &req) {
        write_response(&mut stream, refusal, cors.as_deref());
        linger(&mut stream, core, unread);
        return;
    }

    let upgrade_allowed = cross_site_allowed(&req);
    let response = route(core, &mut stream, req, head.rest, &mut unread);
    match response.body {
        Body::Sse(_) => write_sse(&mut stream, core, response, cors.as_deref()),
        Body::Upgrade(up) => {
            if !upgrade_allowed {
                write_response(
                    &mut stream,
                    Response::not_ok(403, "cross-site request forbidden"),
                    cors.as_deref(),
                );
            } else {
                run_upgrade(stream, up);
            }
        }
        _ => {
            write_response(&mut stream, response, cors.as_deref());
            linger(&mut stream, core, unread);
        }
    }
}

fn cross_site_allowed(req: &Request) -> bool {
    security::mutating_allowed(
        req.headers.get("Origin"),
        req.headers.get("Sec-Fetch-Site"),
        req.headers.get("Host"),
    )
}

/// Why this request may not go on to its route: asked from elsewhere for what
/// stays on this computer, or without the key for what needs it.
fn refused(core: &Core, req: &Request) -> Option<Response> {
    if security::this_computer_only(&req.path) && !req.is_loopback() {
        return Some(Response::not_ok(403, security::THIS_COMPUTER_ONLY));
    }
    if !security::needs_key(req.method, &req.path) {
        return None;
    }
    let keyed = core
        .service::<ApiKey>()
        .is_some_and(|k| k.matches(req.headers.get(security::API_KEY_HEADER)));
    (!keyed).then(|| Response::not_ok(401, security::NEEDS_KEY))
}

fn content_length(req: &Request) -> Result<usize, Response> {
    match req.headers.get("Content-Length") {
        None => Ok(0),
        Some(v) => match v.trim().parse::<i64>() {
            Ok(n) if n >= 0 => Ok(n as usize),
            _ => Err(Response::error(400, "invalid content-length")),
        },
    }
}

fn too_large(options: Option<router::RouteOptions>) -> Response {
    match options.and_then(|o| o.too_large) {
        Some(text) => Response::not_ok(413, text),
        None => Response::error(413, "request too large"),
    }
}

/// Reads the body the head promised, the way `_handle_api_post` does.
fn read_body(
    stream: &mut TcpStream,
    core: &Core,
    req: &Request,
    mut rest: Vec<u8>,
    limit: usize,
    unread: &mut usize,
) -> Result<Vec<u8>, Response> {
    let length = content_length(req)?;
    if length > limit {
        return Err(Response::error(413, "request too large"));
    }
    if length == 0 {
        return Ok(Vec::new());
    }
    *unread = 0;
    read_more(stream, core, &mut rest, |b| b.len() >= length);
    if rest.len() < length {
        return Err(Response::error(400, "request body was cut short"));
    }
    rest.truncate(length);
    Ok(rest)
}

/// Streams the body to a file under the state folder's `spool`, so a file of
/// a gigabyte never sits in memory. `None` is an empty body.
fn spool_body(
    stream: &mut TcpStream,
    core: &Core,
    req: &Request,
    rest: Vec<u8>,
    options: router::RouteOptions,
    unread: &mut usize,
) -> Result<Option<std::path::PathBuf>, Response> {
    let length = content_length(req)?;
    if length > options.max_body {
        return Err(too_large(Some(options)));
    }
    if length == 0 {
        return Ok(None);
    }
    let dir = core.paths().join("spool");
    let cut = || Response::not_ok(400, "the upload was cut short");
    std::fs::create_dir_all(&dir).map_err(|_| cut())?;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = dir.join(format!(
        "upload-{}-{}.part",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut file = std::fs::File::create(&path).map_err(|_| cut())?;
    *unread = 0;
    let mut written = 0usize;
    let mut failed = false;
    let mut put = |chunk: &[u8], written: &mut usize| -> bool {
        let take = chunk.len().min(length - *written);
        if file.write_all(&chunk[..take]).is_err() {
            return false;
        }
        *written += take;
        true
    };
    failed |= !put(&rest, &mut written);
    let _ = stream.set_read_timeout(Some(SLICE));
    let mut last = Instant::now();
    let mut chunk = vec![0u8; 64 * 1024];
    while !failed && written < length {
        if core.is_stopping() || last.elapsed() > IDLE_LIMIT {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                failed |= !put(&chunk[..n], &mut written);
                last = Instant::now();
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    drop(file);
    if failed || written < length {
        *unread = length.saturating_sub(written);
        let _ = std::fs::remove_file(&path);
        return Err(cut());
    }
    Ok(Some(path))
}

fn route(core: &Core, stream: &mut TcpStream, mut req: Request, rest: Vec<u8>, unread: &mut usize) -> Response {
    let path = req.path.clone();
    if path == "/mcp" {
        if req.method == Method::Post {
            match read_body(stream, core, &req, rest, mcp::MAX_MCP_BODY_BYTES + 1, unread) {
                Ok(b) => req.body = b,
                Err(_) => {
                    let r = Response::json_status(
                        mcp::jsonrpc_error(
                            serde_json::Value::Null,
                            mcp::JSONRPC_INVALID_REQUEST,
                            "bad or oversized body",
                        ),
                        413,
                    );
                    return r;
                }
            }
        }
        let handle = core.service::<McpHandle>();
        return mcp::handle(&req, handle.as_deref());
    }
    if !path.starts_with("/api/") {
        return match req.method {
            Method::Get => serve(core.options().web_root.as_deref(), &req),
            Method::Post => Response::error(404, "not found"),
            Method::Delete => Response::error(405, "method not allowed"),
        };
    }
    if req.method == Method::Post {
        let options = core.router().options_for(Method::Post, &path);
        if let Some(o) = options.filter(|o| o.spool) {
            match spool_body(stream, core, &req, rest, o, unread) {
                Ok(file) => req.body_file = file,
                Err(r) => return r,
            }
            let response = core.dispatch(req.clone());
            if let Some(file) = &req.body_file {
                let _ = std::fs::remove_file(file);
            }
            return response;
        }
        let limit = options.map_or(router::MAX_JSON_BODY, |o| o.max_body);
        match read_body(stream, core, &req, rest, limit, unread) {
            Ok(b) => req.body = b,
            Err(r) if r.status == 413 => return too_large(options),
            Err(r) => return r,
        }
    }
    core.dispatch(req)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};

    use super::*;
    use crate::core::{CoreBuilder, Options};
    use crate::http::mcp::McpEndpoint;
    use crate::response::{SseNext, SseStream};
    use crate::ws::{mask_frame, OP_TEXT};

    struct Probe;

    impl Module for Probe {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn register(&self, reg: &mut Registry) {
            reg.get("/api/probe", |_, req| {
                Response::json(json!({"q": req.query.get("q"), "peer": req.peer.map(|p| p.to_string())}))
            });
            reg.post("/api/probe", |_, req| {
                Response::json(Value::Object(req.payload().clone()))
            });
            reg.post_with("/api/upload", crate::router::RouteOptions::raw(16), |_, req| {
                Response::json(json!({"bytes": req.body.len()}))
            });
            reg.post_with(
                "/api/spool",
                crate::router::RouteOptions::raw(16)
                    .too_large("too big for the spool")
                    .spooled(),
                |_, req| {
                    let size = req
                        .body_file
                        .as_ref()
                        .and_then(|f| std::fs::read(f).ok())
                        .map(|b| b.len());
                    Response::json(json!({"size": size, "in_memory": req.body.len()}))
                },
            );
            reg.get("/api/mcp", |_, _| Response::json(json!({"token": "secret"})));
            reg.post("/api/ssh/send", |_, _| Response::json(json!({"ok": true})));
            reg.get("/api/ws", |core, req| {
                let core = core.clone();
                Response::upgrade(req, move |ws| {
                    while !core.is_stopping() {
                        match ws.receive(Duration::from_millis(200)) {
                            Ok(Some(text)) => {
                                if ws.send_text(&format!("echo {text}")).is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                    ws.close(crate::ws::CLOSE_GOING_AWAY, "");
                })
            });
            reg.service(Arc::new(McpHandle(Arc::new(Gate))));
        }
    }

    struct Gate;

    impl McpEndpoint for Gate {
        fn enabled(&self) -> bool {
            true
        }
        fn authorized(&self, h: Option<&str>) -> bool {
            h == Some("Bearer t0ken")
        }
        fn handle_post(
            &self,
            m: Value,
            _: Option<&str>,
            _: crate::http::mcp::Caller,
        ) -> (u16, Option<Value>, Vec<(String, String)>) {
            (
                200,
                Some(json!({"jsonrpc": "2.0", "id": m["id"], "result": {}})),
                vec![],
            )
        }
        fn open_stream(&self, _: Option<&str>, _: crate::http::mcp::Caller) -> Option<Box<dyn SseStream>> {
            None
        }
        fn end_session(&self, _: Option<&str>, _: crate::http::mcp::Caller) -> bool {
            false
        }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-core-http-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    thread_local! {
        /// The key of the core this test started, as its window has it.
        static KEY: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    }

    fn keyed() -> String {
        KEY.with(|k| format!("X-Corvus-Key: {}\r\n", k.borrow()))
    }

    fn running(name: &str) -> (Core, SocketAddr, std::path::PathBuf) {
        let d = dir(name);
        std::fs::write(d.join("index.html"), "<html>hi</html>").unwrap();
        let mut options = Options::in_dir(d.join("state"));
        options.web_root = Some(d.clone());
        let core = CoreBuilder::new(options)
            .module(Probe)
            .module(Http::new(HttpOptions {
                port: 0,
                ..HttpOptions::default()
            }))
            .build()
            .unwrap();
        core.start().unwrap();
        let addr = core.service::<HttpInfo>().unwrap().addr().unwrap();
        let key = core.service::<ApiKey>().unwrap().secret().to_string();
        KEY.with(|k| *k.borrow_mut() = key);
        (core, addr, d)
    }

    /// One request, the whole answer: `(status, headers lowercased, body)`.
    fn ask(addr: SocketAddr, raw: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let _ = s.shutdown(std::net::Shutdown::Write);
        let mut all = Vec::new();
        let _ = s.read_to_end(&mut all);
        let split = all.windows(4).position(|w| w == b"\r\n\r\n").expect("a head");
        let head = String::from_utf8_lossy(&all[..split]).into_owned();
        let mut lines = head.split("\r\n");
        let status: u16 = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|l| l.split_once(": "))
            .map(|(k, v)| (k.to_lowercase(), v.to_string()))
            .collect();
        (status, headers, all[split + 4..].to_vec())
    }

    fn get(addr: SocketAddr, path: &str, extra: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
        ask(
            addr,
            &format!("GET {path} HTTP/1.1\r\nHost: localhost:{}\r\n{extra}\r\n", addr.port()),
        )
    }

    /// A POST of this Corvus' own window.
    fn post(addr: SocketAddr, path: &str, body: &str, extra: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
        post_unkeyed(addr, path, body, &format!("{}{extra}", keyed()))
    }

    fn post_unkeyed(addr: SocketAddr, path: &str, body: &str, extra: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
        ask(
            addr,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n{extra}\r\n{body}",
                body.len()
            ),
        )
    }

    fn header<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
        h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn json_of(b: &[u8]) -> Value {
        serde_json::from_slice(b).unwrap()
    }

    #[test]
    fn a_get_is_answered_with_the_pythons_headers() {
        let (core, addr, d) = running("get");
        let (status, h, body) = get(addr, "/api/probe?q=a%20b", "");
        assert_eq!(status, 200);
        assert_eq!(json_of(&body), json!({"q": "a b", "peer": "127.0.0.1"}));
        assert_eq!(header(&h, "content-type"), Some("application/json"));
        assert_eq!(header(&h, "content-length"), Some(&*body.len().to_string()));
        assert_eq!(header(&h, "x-content-type-options"), Some("nosniff"));
        assert_eq!(header(&h, "x-frame-options"), Some("DENY"));
        assert_eq!(header(&h, "referrer-policy"), Some("no-referrer"));
        assert!(header(&h, "content-security-policy")
            .unwrap()
            .starts_with("default-src 'self'"));
        assert_eq!(header(&h, "vary"), Some("Origin"));
        assert!(header(&h, "access-control-allow-origin").is_none());
        assert!(header(&h, "date").unwrap().ends_with(" GMT"));
        let own = format!("http://localhost:{}", addr.port());
        let (_, h, _) = get(addr, "/api/probe", &format!("Origin: {own}\r\n"));
        assert_eq!(header(&h, "access-control-allow-origin"), Some(own.as_str()));
        let (_, h, _) = get(addr, "/api/probe", "Origin: http://localhost:3\r\n");
        assert!(header(&h, "access-control-allow-origin").is_none(), "another port");
        assert_eq!(get(addr, "/api/version", "").0, 200);
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn what_is_not_found_unported_or_not_allowed_says_so() {
        let (core, addr, d) = running("refusals");
        let (status, _, body) = get(addr, "/api/nowhere", "");
        assert_eq!((status, json_of(&body)), (404, json!({"error": "not found"})));
        let (status, _, body) = post(addr, "/api/mavlink/arm", "{}", "");
        assert_eq!(
            (status, json_of(&body)),
            (501, json!({"error": "Not available in this build"}))
        );
        let (status, _, body) = post(addr, "/nowhere", "{}", "");
        assert_eq!((status, json_of(&body)), (404, json!({"error": "not found"})));
        assert_eq!(
            ask(
                addr,
                &format!("DELETE /api/probe HTTP/1.1\r\nHost: localhost\r\n{}\r\n", keyed())
            )
            .0,
            405
        );
        assert_eq!(ask(addr, "PUT /api/probe HTTP/1.1\r\nHost: localhost\r\n\r\n").0, 501);
        assert_eq!(ask(addr, "garbage\r\n\r\n").0, 400);
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_page_on_another_site_cannot_write_or_read() {
        let (core, addr, d) = running("cross");
        let (status, _, body) = ask(addr, "GET /api/config HTTP/1.1\r\nHost: evil.example\r\n\r\n");
        assert_eq!(
            (status, json_of(&body)),
            (403, json!({"ok": false, "error": "host not allowed"}))
        );
        let (status, _, body) = post(addr, "/api/probe", "{}", "Origin: http://evil.example\r\n");
        assert_eq!(
            (status, json_of(&body)),
            (403, json!({"ok": false, "error": "cross-site request forbidden"}))
        );
        assert_eq!(post(addr, "/api/probe", "{}", "Sec-Fetch-Site: cross-site\r\n").0, 403);
        assert_eq!(
            post(addr, "/api/probe", "{}", "Origin: http://localhost:80\r\n").0,
            200,
            "its own origin"
        );
        assert_eq!(
            post(addr, "/api/probe", "{}", "Origin: http://localhost:9999\r\n").0,
            403,
            "a page of another program on this computer, even with the key"
        );
        assert_eq!(post(addr, "/api/probe", "{}", "Sec-Fetch-Site: same-origin\r\n").0, 200);
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_program_without_the_key_can_read_and_nothing_more() {
        let (core, addr, d) = running("no-key");
        let wrong = "X-Corvus-Key: wrong\r\n";
        let own = "Origin: http://localhost:80\r\n";
        for extra in ["", wrong, own, "Content-Type: text/plain\r\n"] {
            for path in ["/api/probe", "/api/upload", "/api/spool", "/api/ssh/send"] {
                let (status, _, body) = post_unkeyed(addr, path, "{}", extra);
                assert_eq!(
                    (status, json_of(&body)),
                    (401, json!({"ok": false, "error": security::NEEDS_KEY})),
                    "{path} {extra:?}"
                );
            }
        }
        let (status, _, body) = get(addr, "/api/mcp", "");
        assert_eq!(status, 401);
        assert!(!String::from_utf8_lossy(&body).contains("secret"), "no token");
        assert_eq!(get(addr, "/api/mcp", &keyed()).0, 200);
        assert_eq!(get(addr, "/api/probe", "").0, 200, "reading stays");
        assert_eq!(post(addr, "/api/ssh/send", "{}", "").0, 200);
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A socket from another computer cannot be made here, so the guard in
    /// front of the routes is asked directly.
    #[test]
    fn from_the_network_the_key_still_does_not_reach_this_computers_own_routes() {
        let (core, _, d) = running("network");
        let key = core.service::<ApiKey>().unwrap().secret().to_string();
        let ask_from = |peer: &str, method: Method, path: &str, with_key: bool| {
            let mut r = Request::new(method, path);
            r.peer = Some(peer.parse().unwrap());
            if with_key {
                r.headers.push(security::API_KEY_HEADER, &key);
            }
            refused(&core, &r).map(|a| a.status)
        };
        for (method, path) in [
            (Method::Post, "/api/ssh/send"),
            (Method::Get, "/api/ssh/ws"),
            (Method::Get, "/api/ssh/stream"),
            (Method::Get, "/api/ssh/sessions"),
            (Method::Post, "/api/ssh/run"),
            (Method::Post, "/api/local/run"),
            (Method::Post, "/api/mcp/confirm"),
            (Method::Get, "/api/mcp"),
            (Method::Post, "/api/settings/import"),
            (Method::Post, "/api/video/settings"),
        ] {
            assert_eq!(ask_from("192.168.1.20", method, path, true), Some(403), "{path}");
            assert_eq!(ask_from("127.0.0.1", method, path, true), None, "{path}");
        }
        assert_eq!(
            ask_from("192.168.1.20", Method::Post, "/api/mavlink/arm", true),
            None,
            "a second station with the key"
        );
        assert_eq!(
            ask_from("192.168.1.20", Method::Post, "/api/mavlink/arm", false),
            Some(401)
        );
        assert_eq!(ask_from("192.168.1.20", Method::Get, "/api/state", false), None);
        for block in ["ssh_connections", "video"] {
            let mut r = Request::new(Method::Post, "/api/config");
            r.peer = Some("192.168.1.20".parse().unwrap());
            r.body = format!("{{\"{block}\": []}}").into_bytes();
            assert_eq!(core.dispatch(r).status, 403, "{block}");
        }
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn post_bodies_follow_handle_api_post() {
        let (core, addr, d) = running("bodies");
        let (status, _, body) = post(addr, "/api/probe", r#"{"a": [1, 2]}"#, "");
        assert_eq!((status, json_of(&body)), (200, json!({"a": [1, 2]})));
        assert_eq!(json_of(&post(addr, "/api/probe", "", "").2), json!({}));
        let (status, _, body) = post(addr, "/api/probe", "[1]", "");
        assert_eq!(
            (status, json_of(&body)),
            (400, json!({"error": "json payload must be an object"}))
        );
        assert_eq!(
            json_of(&post(addr, "/api/probe", "{", "").2),
            json!({"error": "invalid json"})
        );
        let (status, _, body) = ask(
            addr,
            &format!(
                "POST /api/probe HTTP/1.1\r\nHost: localhost\r\nContent-Length: abc\r\n{}\r\n",
                keyed()
            ),
        );
        assert_eq!(
            (status, json_of(&body)),
            (400, json!({"error": "invalid content-length"}))
        );
        let (status, _, _) = ask(
            addr,
            &format!(
                "POST /api/probe HTTP/1.1\r\nHost: localhost\r\nContent-Length: -1\r\n{}\r\n",
                keyed()
            ),
        );
        assert_eq!(status, 400);
        let (status, _, body) = ask(
            addr,
            &format!(
                "POST /api/probe HTTP/1.1\r\nHost: localhost\r\nContent-Length: 9999999999\r\n{}\r\n",
                keyed()
            ),
        );
        assert_eq!((status, json_of(&body)), (413, json!({"error": "request too large"})));
        let (status, _, body) = ask(
            addr,
            &format!(
                "POST /api/probe HTTP/1.1\r\nHost: localhost\r\nContent-Length: 50\r\n{}\r\n{{\"a\"",
                keyed()
            ),
        );
        assert_eq!(
            (status, json_of(&body)),
            (400, json!({"error": "request body was cut short"}))
        );
        let (status, _, body) = post(addr, "/api/upload", "0123456789", "");
        assert_eq!((status, json_of(&body)), (200, json!({"bytes": 10})));
        assert_eq!(post(addr, "/api/upload", &"x".repeat(17), "").0, 413);
        let (status, _, body) = post(addr, "/api/spool", "0123456789", "");
        assert_eq!((status, json_of(&body)), (200, json!({"size": 10, "in_memory": 0})));
        let (status, _, body) = post(addr, "/api/spool", "", "");
        assert_eq!((status, json_of(&body)), (200, json!({"size": null, "in_memory": 0})));
        let (status, _, body) = post(addr, "/api/spool", &"x".repeat(17), "");
        assert_eq!(
            (status, json_of(&body)),
            (413, json!({"ok": false, "error": "too big for the spool"}))
        );
        let spooled = std::fs::read_dir(core.paths().join("spool")).map_or(0, Iterator::count);
        assert_eq!(spooled, 0, "the spool file is removed after the handler");
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_refused_upload_still_gets_its_reason() {
        let (core, addr, d) = running("linger");
        let size = 8 * 1024 * 1024;
        for (path, extra, status, text) in [
            ("/api/upload", "", 413, "request too large"),
            ("/api/spool", "", 413, "too big for the spool"),
            ("/nowhere", "", 404, "not found"),
            (
                "/api/probe",
                "Origin: http://evil.example\r\n",
                403,
                "cross-site request forbidden",
            ),
        ] {
            let mut s = TcpStream::connect(addr).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let key = keyed();
            let head = format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {size}\r\n{key}{extra}\r\n");
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&vec![0u8; size])
                .unwrap_or_else(|e| panic!("{path}: the body could not be sent: {e}"));
            let mut all = Vec::new();
            s.read_to_end(&mut all)
                .unwrap_or_else(|e| panic!("{path}: the answer was lost: {e}"));
            let answer = String::from_utf8_lossy(&all);
            assert!(answer.starts_with(&format!("HTTP/1.0 {status} ")), "{path}: {answer}");
            assert!(answer.contains(text), "{path}: {answer}");
        }
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_client_that_never_sends_its_refused_body_does_not_hold_the_shutdown() {
        let (core, addr, d) = running("linger-stop");
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let head = format!(
            "POST /api/upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1000\r\n{}\r\n",
            keyed()
        );
        s.write_all(head.as_bytes()).unwrap();
        let mut all = Vec::new();
        let began = Instant::now();
        s.read_to_end(&mut all).unwrap();
        assert!(all.starts_with(b"HTTP/1.0 413 "));
        assert!(began.elapsed() < LINGER, "the answer does not wait for the body");
        let began = Instant::now();
        core.stop();
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "stop took {:?}",
            began.elapsed()
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn static_files_are_served_with_validators() {
        let (core, addr, d) = running("static");
        let (status, h, body) = get(addr, "/", "");
        assert_eq!((status, body.as_slice()), (200, b"<html>hi</html>".as_slice()));
        assert_eq!(header(&h, "content-type"), Some("text/html"));
        assert_eq!(header(&h, "cache-control"), Some("no-cache"));
        assert!(header(&h, "access-control-allow-origin").is_none() && header(&h, "vary").is_none());
        let etag = header(&h, "etag").unwrap().to_string();
        let (status, h, body) = get(addr, "/index.html", &format!("If-None-Match: {etag}\r\n"));
        assert_eq!((status, body.len()), (304, 0));
        assert!(header(&h, "content-length").is_none());
        assert_eq!(get(addr, "/missing.js", "").0, 404);
        assert_eq!(get(addr, "/%2e%2e/etc/passwd", "").0, 403);
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn mcp_needs_this_computer_the_switch_and_the_token() {
        let (core, addr, d) = running("mcp");
        let rpc = r#"{"jsonrpc": "2.0", "id": 7, "method": "ping"}"#;
        let (status, h, body) = post(addr, "/mcp", rpc, "");
        assert_eq!(status, 401);
        assert_eq!(header(&h, "www-authenticate"), Some("Bearer realm=\"corvus-gcs\""));
        assert_eq!(json_of(&body)["error"]["code"], json!(-32001));
        let (status, _, body) = post(addr, "/mcp", rpc, "Authorization: Bearer t0ken\r\n");
        assert_eq!((status, json_of(&body)["id"].clone()), (200, json!(7)));
        assert_eq!(get(addr, "/mcp", "Authorization: Bearer t0ken\r\n").0, 405);
        core.stop();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_websocket_upgrades_echoes_and_ends_with_the_core() {
        let (core, addr, d) = running("ws");
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        write!(s, "GET /api/ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n").unwrap();
        let mut buf = vec![0u8; 512];
        let mut got = 0;
        while !buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
            got += s.read(&mut buf[got..]).unwrap();
        }
        let head = String::from_utf8_lossy(&buf[..got]).into_owned();
        assert!(head.starts_with("HTTP/1.1 101 Switching Protocols"), "{head}");
        assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
        s.write_all(&mask_frame(OP_TEXT, b"ping", [1, 2, 3, 4], true)).unwrap();
        let mut frame = [0u8; 2 + "echo ping".len()];
        s.read_exact(&mut frame).unwrap();
        assert_eq!(&frame[2..], b"echo ping");
        let (status, _, body) = get(addr, "/api/ws", "");
        assert_eq!(
            (status, json_of(&body)),
            (400, json!({"ok": false, "error": "not a WebSocket upgrade"}))
        );
        let began = Instant::now();
        core.stop();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "stop joined the socket's thread"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn telemetry_streams_and_stop_ends_it_and_the_listener() {
        let (core, addr, d) = running("sse");
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(s, "GET /api/telemetry HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut seen = Vec::new();
        let mut chunk = [0u8; 8192];
        while !String::from_utf8_lossy(&seen).contains("event: state\ndata: {") {
            let n = s.read(&mut chunk).unwrap();
            assert!(n > 0);
            seen.extend_from_slice(&chunk[..n]);
        }
        let text = String::from_utf8_lossy(&seen).into_owned();
        assert!(text.starts_with("HTTP/1.0 200 OK"));
        assert!(text.contains("Content-Type: text/event-stream") && text.contains("Cache-Control: no-cache"));
        core.store().set("armed", json!(true));
        while !String::from_utf8_lossy(&seen).contains("\"armed\":true") {
            let n = s.read(&mut chunk).unwrap();
            assert!(n > 0);
            seen.extend_from_slice(&chunk[..n]);
        }
        let began = Instant::now();
        core.stop();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "stop took {:?}",
            began.elapsed()
        );
        let mut rest = Vec::new();
        let _ = s.read_to_end(&mut rest);
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err(),
            "the port is released"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_next_port_is_taken_when_one_is_busy() {
        let busy = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        let l = bind("127.0.0.1", port, 20).unwrap();
        assert_ne!(l.local_addr().unwrap().port(), port);
        let err = bind("127.0.0.1", port, 1).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        assert!(err.to_string().starts_with("no free port in"));
    }

    #[test]
    fn idle_streams_pass_through_the_trait_without_a_socket() {
        struct Quiet;
        impl SseStream for Quiet {
            fn next(&mut self, _: Duration) -> SseNext {
                SseNext::Idle
            }
        }
        assert_eq!(Quiet.ping(), b"event: ping\ndata: {}\n\n");
        assert_eq!(Quiet.ping_every(), Duration::from_secs(15));
    }
}
