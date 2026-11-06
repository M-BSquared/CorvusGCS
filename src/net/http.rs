//! A small HTTP/1.1 client for the Corvus server.
//!
//! The server is `http.server` on the same machine or on the local network,
//! plain HTTP with one request per connection. That is a few dozen lines
//! over a `TcpStream`, and owning them gives the one thing an off-the-shelf
//! client makes awkward: a read timeout per read on a stream that never ends
//! (SSE), so a reader thread notices a stop request within a fraction of a
//! second instead of blocking until the server says something.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// The longest line of a response head or a chunk header, the most header
/// lines, and the most a body read into memory may hold: an answer past these
/// is an error, never an allocation without end.
const LINE_MAX: u64 = 16 * 1024;
const HEADERS_MAX: usize = 200;
pub const BODY_MAX: u64 = 256 * 1024 * 1024;

/// `read_line` that gives up on a line longer than `LINE_MAX`.
pub fn read_line_capped<R: BufRead>(reader: &mut R, line: &mut String) -> io::Result<usize> {
    let n = reader.by_ref().take(LINE_MAX).read_line(line)?;
    if n as u64 >= LINE_MAX && !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a line of the response is too long",
        ));
    }
    Ok(n)
}

/// Where the server is: `http://host:port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
}

impl Server {
    /// Reads `http://host[:port][/]`. Anything else is an error naming it.
    pub fn parse(url: &str) -> Result<Server, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("{url}: only http:// servers are supported"))?;
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() || rest.contains('/') {
            return Err(format!("{url}: expected http://host:port"));
        }
        let bad_port = |p: &str| format!("{url}: bad port {p:?}");
        let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
            let (h, after) = inner.split_once(']').ok_or_else(|| format!("{url}: unclosed ["))?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse::<u16>().map_err(|_| bad_port(p))?,
                None if after.is_empty() => 80,
                None => return Err(format!("{url}: expected http://host:port")),
            };
            (format!("[{h}]"), port)
        } else {
            match rest.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| bad_port(p))?),
                None => (rest.to_string(), 80),
            }
        };
        Ok(Server { host, port })
    }

    /// The server on the loopback interface at `port`.
    pub fn local(port: u16) -> Server {
        Server {
            host: "127.0.0.1".into(),
            port,
        }
    }

    /// `http://host:port`, for showing.
    pub fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    fn connect(&self, timeout: Duration) -> io::Result<TcpStream> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{} did not resolve", self.host));
        let host = self.host.trim_start_matches('[').trim_end_matches(']');
        for addr in (host, self.port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

/// A response: status, headers (names lower case) and the body reader.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn BufRead + Send>,
}

impl Response {
    /// A header by lower case name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// The whole body as text.
    pub fn text(mut self) -> io::Result<String> {
        let mut out = String::new();
        (&mut self.body).take(BODY_MAX).read_to_string(&mut out)?;
        Ok(out)
    }
}

/// Sends one request and reads the response head. `read_timeout` applies to
/// every later read of the body too.
pub fn request(
    server: &Server,
    method: &str,
    path: &str,
    body: Option<&str>,
    accept: &str,
    read_timeout: Duration,
) -> io::Result<Response> {
    let mut stream = server.connect(Duration::from_secs(3))?;
    stream.set_read_timeout(Some(read_timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    stream.set_nodelay(true)?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: {accept}\r\nConnection: close\r\n",
        server.host, server.port
    );
    if let Some(b) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(b) = body {
        stream.write_all(b.as_bytes())?;
    }
    read_response(BufReader::new(stream))
}

/// Parses a response head from `reader` and wraps the body after it.
pub fn read_response<R: BufRead + Send + 'static>(mut reader: R) -> io::Result<Response> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let mut line = String::new();
    read_line_capped(&mut reader, &mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| bad("no HTTP status line"))?;
    let mut headers = Vec::new();
    loop {
        line.clear();
        if read_line_capped(&mut reader, &mut line)? == 0 {
            return Err(bad("connection closed in the response head"));
        }
        if headers.len() >= HEADERS_MAX {
            return Err(bad("too many header lines"));
        }
        let l = line.trim_end_matches(['\r', '\n']);
        if l.is_empty() {
            break;
        }
        if let Some((n, v)) = l.split_once(':') {
            headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let find = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
    let body: Box<dyn BufRead + Send> = if find("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    {
        Box::new(BufReader::new(Chunked {
            inner: reader,
            left: 0,
            done: false,
        }))
    } else if let Some(len) = find("content-length").and_then(|v| v.parse::<u64>().ok()) {
        Box::new(reader.take(len))
    } else {
        Box::new(reader)
    };
    Ok(Response { status, headers, body })
}

/// `Transfer-Encoding: chunked`, undone.
struct Chunked<R> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut size = String::new();
            read_line_capped(&mut self.inner, &mut size)?;
            let size = size.trim().split(';').next().unwrap_or("");
            self.left = usize::from_str_radix(size, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let want = buf.len().min(self.left);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= n;
        if self.left == 0 {
            let mut crlf = String::new();
            read_line_capped(&mut self.inner, &mut crlf)?;
        }
        Ok(n)
    }
}

/// GET a JSON document.
pub fn get_json(server: &Server, path: &str) -> Result<serde_json::Value, String> {
    let r = request(server, "GET", path, None, "application/json", Duration::from_secs(10))
        .map_err(|e| format!("{}{path}: {e}", server.url()))?;
    json_body(server, path, r)
}

/// GET a binary body (a map tile), `None` unless the answer is a 200.
pub fn get_bytes(server: &Server, path: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let r = request(server, "GET", path, None, "*/*", Duration::from_secs(15)).ok()?;
    if r.status != 200 {
        return None;
    }
    let mut out = Vec::new();
    r.body.take(BODY_MAX).read_to_end(&mut out).ok()?;
    Some(out)
}

/// POST a JSON document and read the JSON answer.
pub fn post_json(server: &Server, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let text = body.to_string();
    let r = request(
        server,
        "POST",
        path,
        Some(&text),
        "application/json",
        Duration::from_secs(10),
    )
    .map_err(|e| format!("{}{path}: {e}", server.url()))?;
    json_body(server, path, r)
}

/// POST a JSON document and read the JSON answer whatever the status: the
/// body is the thing worth showing when a remote command fails, so an HTTP
/// error does not throw it away. A network error or a body that is not JSON
/// is still an error.
pub fn post_json_any(
    server: &Server,
    path: &str,
    body: &serde_json::Value,
) -> Result<(u16, serde_json::Value), String> {
    let text = body.to_string();
    let r = request(
        server,
        "POST",
        path,
        Some(&text),
        "application/json",
        Duration::from_secs(60),
    )
    .map_err(|e| format!("{}{path}: {e}", server.url()))?;
    let status = r.status;
    let text = r.text().map_err(|e| format!("{}{path}: {e}", server.url()))?;
    serde_json::from_str(&text)
        .map(|v| (status, v))
        .map_err(|_| format!("Invalid response from {path}"))
}

/// GET a JSON document that may take a while to be made (a log read into
/// plots); `wait` bounds each read.
pub fn get_json_wait(server: &Server, path: &str, wait: Duration) -> Result<serde_json::Value, String> {
    let r = request(server, "GET", path, None, "application/json", wait)
        .map_err(|e| format!("{}{path}: {e}", server.url()))?;
    json_body(server, path, r)
}

/// `encodeURIComponent`: everything but letters, digits and `-_.!~*'()` as
/// percent escapes of its UTF-8 bytes.
pub fn encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// POST raw bytes (a file) and read the JSON answer; `wait` bounds each read,
/// for a conversion the backend does before it answers.
pub fn post_bytes(server: &Server, path: &str, body: &[u8], wait: Duration) -> Result<serde_json::Value, String> {
    let fail = |e: io::Error| format!("{}{path}: {e}", server.url());
    let mut stream = server.connect(Duration::from_secs(3)).map_err(fail)?;
    stream.set_read_timeout(Some(wait)).map_err(fail)?;
    stream.set_write_timeout(Some(Duration::from_secs(60))).map_err(fail)?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        server.host,
        server.port,
        body.len()
    );
    stream.write_all(head.as_bytes()).map_err(fail)?;
    stream.write_all(body).map_err(fail)?;
    let r = read_response(BufReader::new(stream)).map_err(fail)?;
    json_body(server, path, r)
}

fn json_body(server: &Server, path: &str, r: Response) -> Result<serde_json::Value, String> {
    let status = r.status;
    let text = r.text().map_err(|e| format!("{}{path}: {e}", server.url()))?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| format!("Invalid response from {path}"))?;
    if !(200..300).contains(&status) {
        let said = value.get("error").and_then(|e| e.as_str()).map(str::to_string);
        return Err(said.unwrap_or_else(|| format!("Request failed ({status})")));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &'static str) -> Response {
        read_response(BufReader::new(raw.as_bytes())).unwrap()
    }

    #[test]
    fn a_head_line_without_an_end_is_refused() {
        let long = format!("HTTP/1.1 200 OK\r\nX-Long: {}\r\n\r\n", "a".repeat(40_000));
        let e = read_response(BufReader::new(std::io::Cursor::new(long.into_bytes())))
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn too_many_header_lines_are_refused() {
        let many = format!("HTTP/1.1 200 OK\r\n{}\r\n", "X-A: b\r\n".repeat(HEADERS_MAX + 5));
        let e = read_response(BufReader::new(std::io::Cursor::new(many.into_bytes())))
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_line_at_the_limit_with_its_end_is_read() {
        let ok = format!("HTTP/1.1 200 OK\r\nX-Long: {}\r\n\r\nbody", "a".repeat(1000));
        let r = read_response(BufReader::new(std::io::Cursor::new(ok.into_bytes()))).unwrap();
        assert_eq!(r.header("x-long").map(str::len), Some(1000));
    }

    #[test]
    fn server_urls() {
        assert_eq!(Server::parse("http://127.0.0.1:8080").unwrap(), Server::local(8080));
        assert_eq!(
            Server::parse("http://gcs.local/").unwrap(),
            Server {
                host: "gcs.local".into(),
                port: 80
            }
        );
        assert_eq!(
            Server::parse("http://[::1]:9").unwrap(),
            Server {
                host: "[::1]".into(),
                port: 9
            }
        );
        assert_eq!(
            Server::parse("http://[::1]").unwrap(),
            Server {
                host: "[::1]".into(),
                port: 80
            }
        );
        assert!(Server::parse("https://x:1").is_err());
        assert!(Server::parse("http://x:1/api").is_err());
        assert!(Server::parse("http://x:port").is_err());
    }

    #[test]
    fn a_body_with_a_length_stops_there() {
        let r = parse("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}tail");
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.text().unwrap(), "{}");
    }

    #[test]
    fn a_chunked_body_is_put_back_together() {
        let r =
            parse("HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n");
        assert_eq!(r.status, 404);
        assert_eq!(r.text().unwrap(), "abcde");
    }

    #[test]
    fn without_a_length_the_body_runs_to_the_end() {
        let r = parse("HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: ping\ndata: {}\n\n");
        assert_eq!(r.text().unwrap(), "event: ping\ndata: {}\n\n");
    }

    #[test]
    fn a_post_carries_its_json_and_a_refusal_carries_its_reason() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        let t = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for answer in [
                "HTTP/1.0 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                "HTTP/1.0 400 Bad Request\r\nContent-Length: 18\r\n\r\n{\"error\":\"no fix\"}",
            ] {
                let (mut s, _) = listener.accept().unwrap();
                // Head and body may come in two reads.
                let mut got = Vec::new();
                let mut buf = [0u8; 2048];
                while !got.ends_with(b"}") {
                    let n = s.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                }
                seen.push(String::from_utf8_lossy(&got).into_owned());
                s.write_all(answer.as_bytes()).unwrap();
            }
            seen
        });
        let ok = post_json(&server, "/api/mavlink/arm", &serde_json::json!({"arm": true})).unwrap();
        assert_eq!(ok["ok"], true);
        let refused = post_json(&server, "/api/mavlink/takeoff", &serde_json::json!({"altitude": 10.0}));
        assert_eq!(refused.unwrap_err(), "no fix");
        let seen = t.join().unwrap();
        assert!(seen[0].starts_with("POST /api/mavlink/arm HTTP/1.1\r\n"));
        assert!(seen[0].contains("Content-Type: application/json\r\n"));
        assert!(seen[0].ends_with("{\"arm\":true}"));
        assert!(seen[1].ends_with("{\"altitude\":10.0}"));
    }

    #[test]
    fn a_truncated_head_is_an_error() {
        assert!(read_response(BufReader::new("HTTP/1.0 200 OK\r\nX: y".as_bytes())).is_err());
        assert!(read_response(BufReader::new("garbage\r\n\r\n".as_bytes())).is_err());
    }
}
