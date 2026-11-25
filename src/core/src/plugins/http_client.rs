//! A small blocking HTTP client for the release list of the update check and
//! the plugins the operator installs.
//!
//! `http` for a stub on this computer and `https` for GitHub (rustls, the
//! system's certificate store, the `ring` provider chosen explicitly so a
//! second provider elsewhere in the program cannot make the choice ambiguous).
//! A GET with a timeout on every step, a cap on the body, `Connection: close`,
//! chunked bodies, up to three redirects and word of how much has arrived. Nothing else in the core needs
//! more, so nothing more is here.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What a GET gave back.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// The body was longer than the cap and was cut there.
    pub truncated: bool,
}

struct Url {
    https: bool,
    host: String,
    port: u16,
    target: String,
}

fn parse_url(url: &str) -> Result<Url, String> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("unsupported URL {url}"));
    };
    let (authority, target) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (
            h.trim_matches(['[', ']']).to_string(),
            p.parse().map_err(|_| format!("bad port in {url}"))?,
        ),
        _ => (
            authority.trim_matches(['[', ']']).to_string(),
            if https { 443 } else { 80 },
        ),
    };
    if host.is_empty() {
        return Err(format!("no host in {url}"));
    }
    Ok(Url {
        https,
        host,
        port,
        target,
    })
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(loaded.certs);
    if roots.is_empty() {
        return Err("no trusted certificates on this system".into());
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The longest silence of a server, and the longest a single address may take to answer.
const STALL: Duration = Duration::from_secs(30);
const CONNECT: Duration = Duration::from_secs(10);
const MAX_HEAD: usize = 64 * 1024;

/// Told how much of the body has arrived, and how long it is when the server said so.
pub type Seen<'a> = &'a mut dyn FnMut(usize, Option<usize>);

struct Head {
    status: u16,
    location: Option<String>,
    chunked: bool,
    length: Option<usize>,
}

fn parse_head(raw: &[u8]) -> Result<Head, String> {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or("the server sent no status")?;
    let header = |name: &str| {
        lines
            .clone()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    let chunked = header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    let length = header("content-length")
        .and_then(|v| v.parse().ok())
        .filter(|_| !chunked);
    Ok(Head {
        status,
        location: header("location"),
        chunked,
        length,
    })
}

/// Reads one answer: until its length is there or the peer closes, the cap is passed or the
/// deadline is. The body is cut where the cap is passed, and the last value says so.
fn read_answer(
    stream: &mut dyn Read,
    cap: usize,
    deadline: Instant,
    seen: Seen,
) -> Result<(Head, Vec<u8>, bool), String> {
    let mut raw = Vec::new();
    let mut head: Option<Head> = None;
    let mut buf = [0u8; 16 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err("timed out".into());
        }
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err("timed out".into())
            }
            // A TLS peer that closes without close_notify after its last byte.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && head.is_some() => 0,
            Err(e) => return Err(e.to_string()),
        };
        if n == 0 {
            let head = head.ok_or("the server sent no complete answer")?;
            return match head.length {
                Some(_) => Err("the connection was lost".into()),
                None => Ok((head, raw, false)),
            };
        }
        let from = raw.len().saturating_sub(3);
        raw.extend_from_slice(&buf[..n]);
        if head.is_none() {
            match raw[from..].windows(4).position(|w| w == b"\r\n\r\n") {
                Some(i) => {
                    head = Some(parse_head(&raw[..from + i])?);
                    raw.drain(..from + i + 4);
                }
                None if raw.len() > MAX_HEAD => return Err("the server sent no complete answer".into()),
                None => {}
            }
        }
        let Some(h) = &head else { continue };
        if h.status == 200 {
            seen(raw.len().min(h.length.unwrap_or(usize::MAX)), h.length);
        }
        let over = match h.length {
            Some(length) if length > cap => raw.len() > cap,
            Some(length) => {
                if raw.len() >= length {
                    raw.truncate(length);
                    return Ok((head.take().ok_or("no answer")?, raw, false));
                }
                false
            }
            // Room for the marks of the chunks.
            None => raw.len() > cap + cap / 8 + 64 * 1024,
        };
        if over {
            return Ok((head.take().ok_or("no answer")?, raw, true));
        }
    }
}

fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&data[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if data.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// Connects to the first address of the host that answers.
fn connect(url: &Url, deadline: Instant) -> Result<TcpStream, String> {
    let mut last = format!("{} did not resolve", url.host);
    let found = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?;
    for addr in found {
        let left = deadline.saturating_duration_since(Instant::now()).min(CONNECT);
        if left.is_zero() {
            return Err("timed out".into());
        }
        match TcpStream::connect_timeout(&addr, left) {
            Ok(s) => return Ok(s),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn once(
    url: &Url,
    user_agent: &str,
    accept: &str,
    timeout: Duration,
    cap: usize,
    seen: Seen,
) -> Result<(Reply, Option<String>), String> {
    let deadline = Instant::now() + timeout;
    let mut tcp = connect(url, deadline)?;
    let quiet = timeout.min(STALL);
    tcp.set_read_timeout(Some(quiet)).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(quiet)).map_err(|e| e.to_string())?;
    let host_header = if (url.https && url.port == 443) || (!url.https && url.port == 80) {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: {user_agent}\r\nAccept: {accept}\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        url.target
    );
    let (head, mut body, truncated) = if url.https {
        let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|e| e.to_string())?;
        let conn = rustls::ClientConnection::new(tls_config()?, name).map_err(|e| e.to_string())?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        tls.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
        read_answer(&mut tls, cap, deadline, seen)?
    } else {
        tcp.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
        read_answer(&mut tcp, cap, deadline, seen)?
    };
    if head.chunked {
        body = match dechunk(&body) {
            Some(b) => b,
            None if truncated => body,
            None => return Err("the connection was lost".into()),
        };
    }
    let over = truncated || body.len() > cap;
    body.truncate(cap.saturating_add(1));
    Ok((
        Reply {
            status: head.status,
            body,
            truncated: over,
        },
        head.location,
    ))
}

/// `GET url`: the answer whatever its status, or the reason there was none.
/// The body is cut one byte past `cap`, so a caller can tell "too large".
pub fn get(url: &str, user_agent: &str, accept: &str, timeout: Duration, cap: usize) -> Result<Reply, String> {
    get_seen(url, user_agent, accept, timeout, cap, &mut |_, _| ())
}

/// [`get`], told of every piece of the body as it arrives.
pub fn get_seen(
    url: &str,
    user_agent: &str,
    accept: &str,
    timeout: Duration,
    cap: usize,
    seen: Seen,
) -> Result<Reply, String> {
    let mut current = url.to_string();
    for _ in 0..4 {
        let parsed = parse_url(&current)?;
        let (reply, location) = once(&parsed, user_agent, accept, timeout, cap, seen)?;
        match (reply.status, location) {
            (301 | 302 | 303 | 307 | 308, Some(next)) if parsed.https && next.starts_with("http:") => {
                return Err("the server redirected from https to plain http".into())
            }
            (301 | 302 | 303 | 307 | 308, Some(next)) if next.starts_with("http") => current = next,
            _ => return Ok(reply),
        }
    }
    Err("too many redirects".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_split_into_host_port_and_target() {
        let u = parse_url("https://api.github.com/repos/x?per_page=10").unwrap();
        assert_eq!(
            (u.https, u.host.as_str(), u.port, u.target.as_str()),
            (true, "api.github.com", 443, "/repos/x?per_page=10")
        );
        let u = parse_url("http://127.0.0.1:8123").unwrap();
        assert_eq!(
            (u.https, u.host.as_str(), u.port, u.target.as_str()),
            (false, "127.0.0.1", 8123, "/")
        );
        let u = parse_url("http://[::1]:9/a").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 9));
        assert!(parse_url("ftp://x").is_err());
    }

    #[test]
    fn a_chunked_body_is_put_together() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
        assert!(dechunk(b"5\r\nab").is_none());
    }

    /// Answers one connection with `reply`, and closes.
    fn serve(reply: Vec<u8>) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 1024];
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(n) if n > 0 => got.extend_from_slice(&buf[..n]),
                    _ => return,
                }
            }
            let _ = s.write_all(&reply);
        });
        format!("http://127.0.0.1:{port}/x")
    }

    type Told = Vec<(usize, Option<usize>)>;

    fn fetch(reply: Vec<u8>, cap: usize) -> (Result<Reply, String>, Told) {
        let mut all = Vec::new();
        let r = get_seen(&serve(reply), "t", "*/*", Duration::from_secs(10), cap, &mut |n, of| {
            all.push((n, of))
        });
        (r, all)
    }

    #[test]
    fn what_has_arrived_is_told_up_to_the_whole() {
        let body = vec![7u8; 300_000];
        let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        reply.extend_from_slice(&body);
        let (r, seen) = fetch(reply, 1 << 20);
        let r = r.unwrap();
        assert_eq!((r.body, r.truncated), (body, false));
        assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0));
        assert_eq!(seen.last(), Some(&(300_000, Some(300_000))));
    }

    #[test]
    fn an_answer_that_ends_early_is_not_taken_for_a_whole_one() {
        let (r, _) = fetch(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".to_vec(), 100);
        assert!(r.unwrap_err().contains("lost"));
        let (r, _) = fetch(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab".to_vec(),
            100,
        );
        assert!(r.unwrap_err().contains("lost"));
        let (r, _) = fetch(b"HTTP/1.1 200 OK\r\nContent-Le".to_vec(), 100);
        assert!(r.is_err());
    }

    #[test]
    fn a_body_over_the_cap_is_cut_and_one_without_a_length_is_read_to_its_end() {
        let (r, _) = fetch(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789".to_vec(), 4);
        let r = r.unwrap();
        assert_eq!((r.body.len(), r.truncated), (5, true));
        let (r, seen) = fetch(b"HTTP/1.1 200 OK\r\n\r\nabc".to_vec(), 100);
        assert_eq!(r.unwrap().body, b"abc");
        assert_eq!(seen.last(), Some(&(3, None)));
        let (r, seen) = fetch(b"HTTP/1.1 404 No\r\nContent-Length: 2\r\n\r\nno".to_vec(), 100);
        assert_eq!(r.unwrap().status, 404);
        assert!(seen.is_empty(), "only the body that was asked for counts");
    }
}
