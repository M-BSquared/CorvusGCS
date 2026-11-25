//! One HTTP request to any server, plain or over TLS: to a language model, or
//! to the station's own `/mcp`. In the program a thread does it ([`blocking`]);
//! a plugin asks the program, which needs the plugin's permission for it.
//!
//! A server that thinks for a minute sends nothing for a minute, so a read
//! cannot simply block: the socket reads in short slices and asks after each
//! one whether the request was given up, or the time allowed is up.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::net::bg::Pending;
#[cfg(target_family = "wasm")]
use crate::net::http::Server;

/// The most of an answer that is read into memory.
pub const BODY_MAX: u64 = 8 * 1024 * 1024;

/// What to ask.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    /// `http://…`, `https://…`, or a path of this station that begins with `/`.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// How long the whole answer may take.
    pub wait: Duration,
}

/// What came back.
#[derive(Debug, Clone, Default)]
pub struct Answer {
    pub status: u16,
    /// Names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Answer {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// An address taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    /// Without the brackets of an IPv6 address.
    pub host: String,
    pub port: u16,
    /// From its `/`, with the query.
    pub path: String,
}

impl Url {
    pub fn parse(text: &str) -> Result<Url, String> {
        let bad = || format!("{text}: not an address");
        let (tls, rest) = match (text.strip_prefix("https://"), text.strip_prefix("http://")) {
            (Some(r), _) => (true, r),
            (_, Some(r)) => (false, r),
            _ => return Err(format!("{text}: only http:// and https:// addresses work")),
        };
        if text.bytes().any(|b| b.is_ascii_control() || b == b' ') {
            return Err(bad());
        }
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
            Some(_) => return Err(bad()),
            None => (rest, "/"),
        };
        // A name and password in the address are not for this client to send.
        if authority.contains('@') {
            return Err(bad());
        }
        let standard = if tls { 443 } else { 80 };
        let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
            let (h, after) = inner.split_once(']').ok_or_else(bad)?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| bad())?,
                None if after.is_empty() => standard,
                None => return Err(bad()),
            };
            (h, port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, p.parse().map_err(|_| bad())?),
                None => (authority, standard),
            }
        };
        if host.is_empty() {
            return Err(bad());
        }
        Ok(Url {
            tls,
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }

    /// `host:port`, for a message.
    pub fn label(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == if self.tls { 443 } else { 80 } {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

/// A request under way, as something to wait for. Dropping it gives it up.
pub struct Fetch {
    answer: Pending<Result<Answer, String>>,
    #[cfg(not(target_family = "wasm"))]
    _running: Option<native::Running>,
    #[cfg(target_family = "wasm")]
    id: Option<u64>,
}

impl Fetch {
    /// The answer, once, when it is there.
    pub fn take(&mut self) -> Option<Result<Answer, String>> {
        let answer = self.answer.take();
        #[cfg(target_family = "wasm")]
        if answer.is_some() {
            self.id = None;
        }
        answer
    }
}

/// Polled again by its owner after every delivery, so it wakes nobody.
impl Future for Fetch {
    type Output = Result<Answer, String>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.take() {
            Some(a) => Poll::Ready(a),
            None => Poll::Pending,
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub use native::{blocking, start, start_where, Cancel};

#[cfg(target_family = "wasm")]
pub fn start(_ctx: &egui::Context, _server: &Server, request: Request) -> Fetch {
    let (answer, filler) = Pending::slot();
    let ask = crate::wire::Fetch {
        id: 0,
        url: request.url,
        method: request.method,
        headers: request.headers,
        body: request.body,
        timeout_ms: request.wait.as_millis() as u64,
    };
    let id = crate::guest::mail::fetch(ask, move |f| {
        filler.fill(match f.error {
            Some(e) => Err(e),
            None => Ok(Answer {
                status: f.status,
                headers: f.headers,
                body: f.body,
            }),
        });
    });
    Fetch { answer, id: Some(id) }
}

#[cfg(target_family = "wasm")]
impl Drop for Fetch {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            crate::guest::mail::cancel_fetch(id);
        }
    }
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::io::{self, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::{Answer, Fetch, Request, Url, BODY_MAX};
    use crate::net::bg::Pending;
    use crate::net::http::{read_response, Server};

    const SLICE: Duration = Duration::from_millis(250);
    const CONNECT: Duration = Duration::from_secs(3);

    /// Whether a request should end.
    #[derive(Clone, Default)]
    pub struct Cancel(Arc<AtomicBool>);

    impl Cancel {
        pub fn cancel(&self) {
            self.0.store(true, Ordering::SeqCst);
        }

        pub fn is_set(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    pub(super) struct Running {
        cancel: Cancel,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.cancel.cancel();
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// Asks on a thread of its own. A path that begins with `/` is this station's.
    pub fn start(ctx: &egui::Context, server: &Server, mut request: Request) -> Fetch {
        if request.url.starts_with('/') {
            request.url = format!("{}{}", server.url(), request.url);
        }
        start_where(ctx, request, |_| true)
    }

    /// [`start`] for a whole address, connecting only where `may` allows.
    pub fn start_where(
        ctx: &egui::Context,
        request: Request,
        may: impl Fn(SocketAddr) -> bool + Send + 'static,
    ) -> Fetch {
        let (answer, filler) = Pending::slot();
        let cancel = Cancel::default();
        let (stop, ctx) = (cancel.clone(), ctx.clone());
        let thread = std::thread::Builder::new()
            .name("fetch".into())
            .spawn(move || {
                filler.fill(blocking(&request, &stop, &may));
                ctx.request_repaint();
            })
            .ok();
        Fetch {
            answer,
            _running: Some(Running { cancel, thread }),
        }
    }

    trait Io: Read + Write + Send {}
    impl<T: Read + Write + Send> Io for T {}

    /// A stream whose reads wait in slices and give up on `cancel` or the deadline.
    struct Patient {
        inner: Box<dyn Io>,
        cancel: Cancel,
        deadline: Instant,
    }

    impl Read for Patient {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                if self.cancel.is_set() {
                    // Not `Interrupted`: the readers of `std` try that one again, for ever.
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "stopped"));
                }
                if Instant::now() >= self.deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no answer in time"));
                }
                match self.inner.read(buf) {
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
                    other => return other,
                }
            }
        }
    }

    impl Write for Patient {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
        static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
        CONFIG
            .get_or_init(|| {
                let mut roots = rustls::RootCertStore::empty();
                roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
                if roots.is_empty() {
                    return Err("This computer has no trusted certificates, so https cannot be used.".into());
                }
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                let config = rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .map_err(|e| e.to_string())?
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                Ok(Arc::new(config))
            })
            .clone()
    }

    fn connect(url: &Url, cancel: &Cancel, may: &dyn Fn(SocketAddr) -> bool) -> Result<TcpStream, String> {
        let mut last = format!("{} did not resolve.", url.host);
        let addrs = (url.host.as_str(), url.port)
            .to_socket_addrs()
            .map_err(|e| format!("{}: {e}", url.label()))?;
        for addr in addrs {
            if cancel.is_set() {
                return Err("Stopped.".into());
            }
            // Asked of the address itself: a name can stand for any.
            if !may(addr) {
                return Err(format!("{}: this plugin may not connect there.", url.label()));
            }
            match TcpStream::connect_timeout(&addr, CONNECT) {
                Ok(s) => return Ok(s),
                Err(e) => last = format!("{}: {e}", url.label()),
            }
        }
        Err(last)
    }

    fn read_body(mut body: impl Read) -> io::Result<String> {
        let mut bytes = Vec::new();
        match (&mut body).take(BODY_MAX).read_to_end(&mut bytes) {
            Ok(_) => {}
            // A TLS server that closes without saying so after its last byte.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && !bytes.is_empty() => {}
            Err(e) => return Err(e),
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn is_token(text: &str) -> bool {
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    }

    /// Sends one request and reads the whole answer. `may` says whether an address may be
    /// connected to. Redirects are not followed.
    pub fn blocking(request: &Request, cancel: &Cancel, may: &dyn Fn(SocketAddr) -> bool) -> Result<Answer, String> {
        let url = Url::parse(&request.url)?;
        let fail = |e: io::Error| match e.kind() {
            _ if cancel.is_set() => "Stopped.".to_string(),
            io::ErrorKind::TimedOut => format!("{} did not answer in time.", url.label()),
            _ => format!("{}: {e}", url.label()),
        };
        if !is_token(&request.method) {
            return Err("That is no method.".into());
        }
        if request
            .headers
            .iter()
            .any(|(k, v)| !is_token(k) || v.contains(['\r', '\n', '\0']))
        {
            return Err("A header holds a line break.".into());
        }
        let tcp = connect(&url, cancel, may)?;
        tcp.set_read_timeout(Some(SLICE)).map_err(fail)?;
        tcp.set_write_timeout(Some(Duration::from_secs(30))).map_err(fail)?;
        let _ = tcp.set_nodelay(true);
        let inner: Box<dyn Io> = if url.tls {
            let name = rustls::pki_types::ServerName::try_from(url.host.clone())
                .map_err(|_| format!("{}: not a valid host name.", url.host))?;
            let conn = rustls::ClientConnection::new(tls_config()?, name).map_err(|e| e.to_string())?;
            Box::new(rustls::StreamOwned::new(conn, tcp))
        } else {
            Box::new(tcp)
        };
        let mut stream = Patient {
            inner,
            cancel: cancel.clone(),
            deadline: Instant::now() + request.wait,
        };
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: corvus-gcs-plugin\r\n",
            request.method,
            url.path,
            url.host_header()
        );
        for (k, v) in &request.headers {
            // These are this client's to say.
            if ["host", "connection", "content-length", "transfer-encoding"]
                .iter()
                .any(|own| k.eq_ignore_ascii_case(own))
            {
                continue;
            }
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(b) = &request.body {
            head.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        head.push_str("\r\n");
        head.push_str(request.body.as_deref().unwrap_or(""));
        stream.write_all(head.as_bytes()).map_err(fail)?;
        stream.flush().map_err(fail)?;
        let response = read_response(BufReader::new(stream)).map_err(fail)?;
        let (status, headers) = (response.status, response.headers.clone());
        let body = read_body(response.body).map_err(fail)?;
        Ok(Answer { status, headers, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::Server;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Instant;

    fn whole(request: &[u8]) -> bool {
        let text = String::from_utf8_lossy(request);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let want = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: ")?.parse().ok())
            .unwrap_or(0);
        body.len() >= want
    }

    fn serve(reply: &'static [u8], hold: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                // The whole request: closing on unread bytes resets the connection, and
                // on Windows the client then loses the answer.
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                while !whole(&got) {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got.extend_from_slice(&buf[..n]),
                    }
                }
                std::thread::sleep(hold);
                let _ = s.write_all(reply);
            }
        });
        format!("http://127.0.0.1:{port}/x")
    }

    fn get(url: &str, wait: u64) -> Result<Answer, String> {
        let request = Request {
            method: "GET".into(),
            url: url.into(),
            wait: Duration::from_millis(wait),
            ..Request::default()
        };
        blocking(&request, &Cancel::default(), &|_| true)
    }

    #[test]
    fn an_address_is_taken_apart() {
        let u = Url::parse("https://api.example.com/v1/chat?x=1").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.path.as_str()),
            (true, "api.example.com", 443, "/v1/chat?x=1")
        );
        let u = Url::parse("http://[::1]:9").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("::1", 9, "/"));
        assert_eq!(u.label(), "::1:9");
        for bad in [
            "ftp://x",
            "x",
            "http://",
            "http://a@b/",
            "http://x:99999",
            "http://x/a b",
            "http://x/\r\nHost: y",
            "http://x?q",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_answer_is_read_with_its_headers_whole_or_in_chunks() {
        let url = serve(
            b"HTTP/1.1 200 OK\r\nMcp-Session-Id: abc\r\nContent-Length: 8\r\n\r\n{\"a\": 1}",
            Duration::ZERO,
        );
        let a = get(&url, 5000).unwrap();
        assert_eq!((a.status, a.body.as_str()), (200, "{\"a\": 1}"));
        assert_eq!(a.header("mcp-session-id"), Some("abc"));
        let url = serve(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
            Duration::ZERO,
        );
        assert_eq!(get(&url, 5000).unwrap().body, "abcde");
    }

    #[test]
    fn giving_up_ends_a_request_the_server_never_answers() {
        let url = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", Duration::from_secs(30));
        let ctx = egui::Context::default();
        let fetch = start(
            &ctx,
            &Server::local(1),
            Request {
                method: "GET".into(),
                url,
                wait: Duration::from_secs(60),
                ..Request::default()
            },
        );
        std::thread::sleep(Duration::from_millis(400));
        let t = Instant::now();
        drop(fetch);
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_slow_server_ends_at_the_time_allowed() {
        let url = serve(b"", Duration::from_secs(30));
        let e = get(&url, 600).unwrap_err();
        assert!(e.contains("did not answer in time"), "{e}");
    }

    #[test]
    fn nothing_listening_is_an_error_naming_the_address() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let e = get(&format!("http://127.0.0.1:{port}"), 2000).unwrap_err();
        assert!(e.contains(&port.to_string()), "{e}");
    }

    #[test]
    fn a_header_or_method_that_could_say_more_is_refused_and_so_is_a_forbidden_address() {
        let ask = |method: &str, header: (&str, &str)| {
            let request = Request {
                method: method.into(),
                url: "http://127.0.0.1:9/".into(),
                headers: vec![(header.0.into(), header.1.into())],
                wait: Duration::from_secs(2),
                ..Request::default()
            };
            blocking(&request, &Cancel::default(), &|_| false).unwrap_err()
        };
        assert!(ask("GET", ("X", "a\r\nb")).contains("line break"));
        assert!(ask("GET", ("X: y\r\nZ", "a")).contains("line break"));
        assert!(ask("GET / HTTP/1.1\r\n", ("X", "a")).contains("no method"));
        assert!(ask("GET", ("X", "a")).contains("may not connect"));
    }
}
