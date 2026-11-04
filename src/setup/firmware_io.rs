//! The two requests of the Firmware page that `net::http` has no call for: a
//! catalogue read that may take longer than a status read (the backend asks
//! two release servers and waits for each), and the raw upload of an image
//! file (`Content-Type: application/octet-stream`, the name in the query).
//!
//! The upload is sent in chunks and checks a flag between them, so closing the
//! page ends it the way the web page's `AbortController` does instead of
//! leaving a thread writing into a socket nobody reads.

use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::net::http::{self, Server};
use crate::setup::firmware_model as model;

/// The catalogue is fetched from the network when the cache is empty, once per
/// release server, so it gets a minute rather than ten seconds.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(90);
const CHUNK: usize = 64 * 1024;

/// What `/api/firmware/upload` accepts at most (`MAX_FIRMWARE_BODY_BYTES`).
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// A JSON answer, or the message the backend gave, or a plain one.
fn answer(server: &Server, path: &str, r: http::Response) -> Result<Value, String> {
    let status = r.status;
    let text = r.text().map_err(|e| format!("{}{path}: {e}", server.url()))?;
    let value: Value = serde_json::from_str(&text).map_err(|_| format!("Invalid response from {path}"))?;
    if !(200..300).contains(&status) || value.get("ok") == Some(&Value::Bool(false)) {
        let said = value.get("error").and_then(Value::as_str).filter(|e| !e.is_empty());
        return Err(said.map_or_else(|| format!("Request failed ({status})"), str::to_string));
    }
    Ok(value)
}

/// `GET /api/firmware/catalog[?refresh=1]`.
pub fn get_catalog(server: &Server, refresh: bool) -> Result<Value, String> {
    let path = if refresh {
        "/api/firmware/catalog?refresh=1"
    } else {
        "/api/firmware/catalog"
    };
    let r = http::request(server, "GET", path, None, "application/json", CATALOG_TIMEOUT)
        .map_err(|e| format!("{}{path}: {e}", server.url()))?;
    answer(server, path, r)
}

/// `~` and `~/` at the start of a typed path.
pub fn expand_home(path: &str) -> String {
    let p = path.trim().trim_matches(|c| c == '"' || c == '\'');
    if p == "~" || p.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return format!("{}{}", home.to_string_lossy(), &p[1..]);
        }
    }
    p.to_string()
}

/// Sends `body` to `path` as an octet stream and reads the JSON answer.
/// Returns an error when `cancel` is raised before the body is out.
pub fn post_octets(server: &Server, path: &str, body: &[u8], cancel: &AtomicBool) -> Result<Value, String> {
    let fail = |e: io::Error| format!("{}{path}: {e}", server.url());
    let host = server.host.trim_start_matches('[').trim_end_matches(']');
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{} did not resolve", server.host));
    let mut stream = None;
    for addr in (host, server.port).to_socket_addrs().map_err(fail)? {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = e,
        }
    }
    let mut stream = stream.ok_or_else(|| fail(last))?;
    stream.set_nodelay(true).map_err(fail)?;
    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(fail)?;
    stream.set_read_timeout(Some(Duration::from_secs(60))).map_err(fail)?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        server.host,
        server.port,
        body.len()
    );
    stream.write_all(head.as_bytes()).map_err(fail)?;
    for chunk in body.chunks(CHUNK) {
        if cancel.load(Ordering::SeqCst) {
            return Err("Upload cancelled".into());
        }
        stream.write_all(chunk).map_err(fail)?;
    }
    let response = http::read_response(io::BufReader::new(stream)).map_err(fail)?;
    answer(server, path, response)
}

/// Reads the image at the path the operator typed and uploads it. The answer
/// is `{ok, state}`; a refusal carries the backend's own reason.
pub fn upload_file(server: &Server, typed: &str, cancel: &Arc<AtomicBool>) -> Result<Value, String> {
    let path = expand_home(typed);
    let meta = std::fs::metadata(&path).map_err(|e| format!("Could not read {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a file"));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!("{path} is larger than {} MB", MAX_IMAGE_BYTES / (1024 * 1024)));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("Could not read {path}: {e}"))?;
    post_octets(server, &model::upload_path(&model::file_name_of(&path)), &bytes, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    fn serve_once(status: &'static str, body: &'static str) -> (Server, std::thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&got[..at]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if got.len() >= at + 4 + len {
                        break;
                    }
                }
            }
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            s.write_all(reply.as_bytes()).unwrap();
            got
        });
        (server, t)
    }

    #[test]
    fn an_image_goes_out_raw_with_its_name_in_the_query() {
        let (server, t) = serve_once("200 OK", r#"{"ok": true, "state": "idle"}"#);
        let cancel = AtomicBool::new(false);
        let r = post_octets(&server, "/api/firmware/upload?name=fw.px4", b"\x00\x01binary", &cancel).unwrap();
        assert_eq!(r["state"], "idle");
        let got = t.join().unwrap();
        let text = String::from_utf8_lossy(&got);
        assert!(text.starts_with("POST /api/firmware/upload?name=fw.px4 HTTP/1.1\r\n"));
        assert!(text.contains("Content-Type: application/octet-stream\r\n"));
        assert!(text.contains("Content-Length: 8\r\n"));
        assert!(got.ends_with(b"\x00\x01binary"));
    }

    #[test]
    fn a_refusal_carries_the_backends_reason() {
        let (server, t) = serve_once("409 Conflict", r#"{"ok": false, "error": "flash needs a USB link"}"#);
        let r = post_octets(&server, "/api/firmware/upload", b"x", &AtomicBool::new(false));
        t.join().unwrap();
        assert_eq!(r.unwrap_err(), "flash needs a USB link");
    }

    #[test]
    fn a_closed_page_ends_the_upload_before_the_body_goes() {
        let (server, t) = serve_once("200 OK", "{}");
        let cancel = AtomicBool::new(true);
        let r = post_octets(&server, "/api/firmware/upload", &vec![0u8; 3 * CHUNK], &cancel);
        assert_eq!(r.unwrap_err(), "Upload cancelled");
        let _ = t.join();
    }

    #[test]
    fn a_missing_file_is_named() {
        let server = Server::local(9);
        let e = upload_file(&server, "/no/such/firmware.px4", &Arc::new(AtomicBool::new(false))).unwrap_err();
        assert!(e.starts_with("Could not read /no/such/firmware.px4"), "{e}");
    }

    #[test]
    fn a_home_shortcut_is_expanded() {
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~/fw.px4"), format!("{}/fw.px4", home.to_string_lossy()));
        }
        assert_eq!(expand_home(" \"/x/fw.px4\" "), "/x/fw.px4");
        assert_eq!(expand_home("~other"), "~other");
    }
}
