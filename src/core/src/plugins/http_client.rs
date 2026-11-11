//! A small blocking HTTP client for the one outgoing request the core makes
//! on its own: the release list of the update check.
//!
//! `http` for a stub on this computer and `https` for GitHub (rustls, the
//! system's certificate store, the `ring` provider chosen explicitly so a
//! second provider elsewhere in the program cannot make the choice ambiguous).
//! A GET with a timeout on every step, a cap on the body, `Connection: close`,
//! chunked bodies and up to three redirects. Nothing else in the core needs
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

/// Reads until the peer closes, the cap is passed or the deadline is.
fn read_all(stream: &mut dyn Read, cap: usize, deadline: Instant) -> Result<(Vec<u8>, bool), String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err("timed out".into());
        }
        match stream.read(&mut buf) {
            Ok(0) => return Ok((out, false)),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > cap + 64 * 1024 {
                    return Ok((out, true));
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err("timed out".into())
            }
            // A TLS peer that closes without close_notify after its last byte.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => return Ok((out, false)),
            Err(e) => return Err(e.to_string()),
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

fn once(
    url: &Url,
    user_agent: &str,
    accept: &str,
    timeout: Duration,
    cap: usize,
) -> Result<(Reply, Option<String>), String> {
    let deadline = Instant::now() + timeout;
    let addr = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| format!("{} did not resolve", url.host))?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    let host_header = if (url.https && url.port == 443) || (!url.https && url.port == 80) {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: {user_agent}\r\nAccept: {accept}\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        url.target
    );
    let (raw, truncated) = if url.https {
        let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|e| e.to_string())?;
        let conn = rustls::ClientConnection::new(tls_config()?, name).map_err(|e| e.to_string())?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        tls.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
        read_all(&mut tls, cap, deadline)?
    } else {
        let mut tcp = tcp;
        tcp.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
        read_all(&mut tcp, cap, deadline)?
    };
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "the server sent no complete answer".to_string())?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "the server sent no status".to_string())?;
    let header = |name: &str| {
        lines
            .clone()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    let location = header("location");
    let chunked = header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    let mut body = raw[split + 4..].to_vec();
    if chunked {
        body = dechunk(&body).unwrap_or(body);
    } else if let Some(n) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.truncate(n);
    }
    let over = truncated || body.len() > cap;
    body.truncate(cap + 1);
    Ok((
        Reply {
            status,
            body,
            truncated: over,
        },
        location,
    ))
}

/// `GET url`: the answer whatever its status, or the reason there was none.
/// The body is cut one byte past `cap`, so a caller can tell "too large".
pub fn get(url: &str, user_agent: &str, accept: &str, timeout: Duration, cap: usize) -> Result<Reply, String> {
    let mut current = url.to_string();
    for _ in 0..4 {
        let parsed = parse_url(&current)?;
        let (reply, location) = once(&parsed, user_agent, accept, timeout, cap)?;
        match (reply.status, location) {
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
}
