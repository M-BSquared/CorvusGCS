//! The camera list and what may be said about an address (the first half of
//! `video.py`): limits, credentials kept apart from the address, the words an
//! operator reads when a camera does not answer, and the config block.

use serde_json::{json, Map, Value};

use super::url::{self, Split};
use crate::mapdata::util::quote;
use crate::peripherals::util::py_strip;
use crate::request::unquote;

pub const MAX_STREAMS: usize = 8;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_URL_LEN: usize = 2048;

pub const KIND_RTSP: &str = "rtsp";
pub const KIND_WEBRTC: &str = "webrtc";
pub const KINDS: [&str; 2] = [KIND_RTSP, KIND_WEBRTC];

/// What ffmpeg is allowed to open. `file:` and `concat:` are deliberately not
/// here: this is a camera address, not a way to read local files.
pub const SCHEMES: [&str; 6] = ["rtsp", "rtsps", "udp", "srt", "http", "https"];
pub const WEBRTC_SCHEMES: [&str; 2] = ["http", "https"];

pub const TRANSPORT_TCP: &str = "tcp";
pub const TRANSPORT_UDP: &str = "udp";
pub const TRANSPORTS: [&str; 2] = [TRANSPORT_TCP, TRANSPORT_UDP];
pub const DEFAULT_TRANSPORT: &str = TRANSPORT_TCP;

pub const STATE_IDLE: &str = "idle";
pub const STATE_STARTING: &str = "starting";
pub const STATE_LIVE: &str = "live";
pub const STATE_STALLED: &str = "stalled";
pub const STATE_ERROR: &str = "error";
pub const STATE_UNAVAILABLE: &str = "unavailable";

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `(address, username, password)` with any `user:pass@` taken out.
pub fn split_credentials(url: &str) -> (String, String, String) {
    let text = py_strip(url).to_string();
    let Ok(parts) = url::urlsplit(&text) else {
        return (text, String::new(), String::new());
    };
    let Some((userinfo, hostport)) = parts.netloc.rsplit_once('@') else {
        return (text, String::new(), String::new());
    };
    let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
    let clean = url::urlunsplit(&Split {
        netloc: hostport.to_string(),
        ..parts.clone()
    });
    (clean, unquote(user), unquote(password))
}

/// The address ffmpeg opens: `url` with the credentials put back in.
pub fn with_credentials(url: &str, username: &str, password: &str) -> String {
    if username.is_empty() {
        return url.to_string();
    }
    let Ok(parts) = url::urlsplit(url) else {
        return url.to_string();
    };
    let mut userinfo = quote(username);
    if !password.is_empty() {
        userinfo.push(':');
        userinfo.push_str(&quote(password));
    }
    url::urlunsplit(&Split {
        netloc: format!("{userinfo}@{}", parts.netloc),
        ..parts
    })
}

/// Host, port and path, for a window title bar: no scheme, no credentials.
pub fn display_address(url: &str) -> String {
    let (clean, _, _) = split_credentials(url);
    let Ok(parts) = url::urlsplit(&clean) else {
        return clean;
    };
    let mut out = format!("{}{}", parts.netloc, parts.path);
    if !parts.query.is_empty() {
        out.push('?');
        out.push_str(&parts.query);
    }
    if out.is_empty() {
        clean
    } else {
        out
    }
}

/// Why `url` cannot be a camera address of this kind, or `""` when it can.
pub fn url_problem(url: &str, kind: &str) -> String {
    let text = py_strip(url);
    let webrtc = kind == KIND_WEBRTC;
    let example = if webrtc {
        "http://192.168.144.25:8889/cam/whep"
    } else {
        "rtsp://192.168.144.25:8554/main.264"
    };
    if text.is_empty() {
        return format!("Enter the camera's address, for example {example}.");
    }
    if text.chars().count() > MAX_URL_LEN {
        return "The address is too long.".into();
    }
    if text
        .chars()
        .any(|c| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c) || (c as u32) < 32 || c as u32 == 127)
    {
        return "The address cannot contain spaces.".into();
    }
    let parts = match url::urlsplit(text) {
        Ok(p) => p,
        Err(_) => return "The port in the address is not a number.".into(),
    };
    let Ok(port) = parts.port() else {
        return "The port in the address is not a number.".into();
    };
    let scheme = parts.scheme.to_lowercase();
    let host = parts.hostname();
    if webrtc {
        if !WEBRTC_SCHEMES.contains(&scheme.as_str()) {
            return format!("A WebRTC camera is its WHEP address, for example {example}.");
        }
        if host.is_none() {
            return "The address has no host.".into();
        }
        return String::new();
    }
    if !SCHEMES.contains(&scheme.as_str()) {
        return "The address has to start with rtsp://, or udp://, srt:// or http:// for other streams.".into();
    }
    if scheme != "udp" && host.is_none() {
        return "The address has no host.".into();
    }
    if scheme == "udp" && host.is_none() && port.is_none() {
        return "A udp:// address needs at least a port, for example udp://@:5600.".into();
    }
    String::new()
}

/// `text` with every appearance of `secret` replaced, for logs and messages.
pub fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    let out = text.replace(secret, "***");
    let quoted = quote(secret);
    if quoted != secret {
        out.replace(&quoted, "***")
    } else {
        out
    }
}

const ERROR_HINTS: [(&str, &str); 10] = [
    ("401", "The camera refused the user or password."),
    ("403", "The camera refused access."),
    ("404", "The camera has no stream at that path."),
    ("Connection refused", "Nothing answered at that address and port."),
    ("timed out", "The camera did not answer."),
    ("No route to host", "That address cannot be reached from this computer."),
    (
        "Network is unreachable",
        "That address cannot be reached from this computer.",
    ),
    ("Name or service not known", "That host name is unknown."),
    ("nodename nor servname", "That host name is unknown."),
    ("Invalid data found", "No readable video came from that address."),
];

/// ffmpeg's `[tcp @ 0x7f...] ` prefix names an internal object.
fn strip_ffmpeg_prefix(text: &str) -> &str {
    let mut rest = text;
    while let Some(after) = rest.strip_prefix('[') {
        let Some(close) = after.find(']') else { break };
        let inner = &after[..close];
        let Some((_, addr)) = inner.rsplit_once("@ 0x") else {
            break;
        };
        if addr.is_empty() || !addr.chars().all(|c| c.is_ascii_hexdigit()) {
            break;
        }
        rest = after[close + 1..].trim_start_matches(|c: char| c.is_whitespace());
    }
    rest
}

/// Python's `[Errno 61] ` is a number for the platform, not for the operator.
fn strip_errno(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("[Errno ") {
        let after = &rest[i + 7..];
        let digits = after.strip_prefix('-').unwrap_or(after);
        let n = digits.chars().take_while(char::is_ascii_digit).count();
        if n > 0 && digits[n..].starts_with(']') {
            out.push_str(&rest[..i]);
            let tail = &digits[n + 1..];
            rest = tail.trim_start_matches(|c: char| c.is_whitespace());
        } else {
            out.push_str(&rest[..i + 7]);
            rest = &rest[i + 7..];
        }
    }
    out.push_str(rest);
    out
}

/// ffmpeg's last word, led by a plain sentence when the cause is a known one.
pub fn explain_error(line: &str) -> String {
    let stripped = py_strip(line);
    let text = strip_errno(strip_ffmpeg_prefix(stripped));
    let lower = text.to_lowercase();
    for (needle, hint) in ERROR_HINTS {
        if lower.contains(&needle.to_lowercase()) {
            return if text.is_empty() {
                hint.to_string()
            } else {
                format!("{hint} ({text})")
            };
        }
    }
    text
}

/// A short id for a new camera. Never shown; it keys the window and the URL.
pub fn new_stream_id() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    format!("v{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

fn clean_str(raw: Option<&Value>, limit: usize) -> String {
    match raw {
        Some(Value::String(t)) => py_strip(t).chars().take(limit).collect(),
        _ => String::new(),
    }
}

fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.chars().count()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// One saved camera as a complete object, or `None` to drop it.
pub fn coerce_stream(raw: &Value) -> Option<Value> {
    let raw_obj = raw.as_object()?;
    let kind = match raw_obj.get("kind").and_then(Value::as_str) {
        Some(k) if KINDS.contains(&k) => k,
        _ => KIND_RTSP,
    };
    let (url, url_user, url_password) = split_credentials(&clean_str(raw_obj.get("url"), MAX_URL_LEN));
    if !url_problem(&url, kind).is_empty() {
        return None;
    }
    let id = match raw_obj.get("id").and_then(Value::as_str) {
        Some(id) if valid_id(id) => id.to_string(),
        _ => new_stream_id(),
    };
    let mut username = clean_str(raw_obj.get("username"), 256);
    if username.is_empty() {
        username = url_user;
    }
    let mut password = match raw_obj.get("password") {
        Some(Value::String(p)) => p.clone(),
        _ => String::new(),
    };
    if password.is_empty() {
        password = url_password;
    }
    let password: String = password.chars().take(1024).collect();
    let transport = match raw_obj.get("transport").and_then(Value::as_str) {
        Some(t) if TRANSPORTS.contains(&t) => t,
        _ => DEFAULT_TRANSPORT,
    };
    let mut name = clean_str(raw_obj.get("name"), MAX_NAME_LEN);
    if name.is_empty() {
        name = display_address(&url);
    }
    let keep_password = !username.is_empty() || kind == KIND_WEBRTC;
    Some(json!({
        "id": id,
        "name": name,
        "kind": kind,
        "url": url,
        "username": username,
        "password": if keep_password { password } else { String::new() },
        "transport": transport,
    }))
}

/// The video block of the config, every field bounded.
pub fn settings(raw: &Value) -> Value {
    let mut streams: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    if let Some(list) = raw.get("streams").and_then(Value::as_array) {
        for entry in list {
            let Some(stream) = coerce_stream(entry) else { continue };
            let id = s(&stream, "id").to_string();
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            streams.push(stream);
            if streams.len() >= MAX_STREAMS {
                break;
            }
        }
    }
    json!({"streams": streams, "ffmpeg": clean_str(raw.get("ffmpeg"), 1024)})
}

/// The `video` coercer of the config file (`_coerce_video`).
pub fn config_block(raw: &Value) -> Option<Map<String, Value>> {
    raw.as_object()?;
    settings(raw).as_object().cloned()
}

/// A camera as the browser may see it: no password, only whether there is one.
pub fn public_stream(stream: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = stream.as_object() {
        for (k, v) in obj {
            if k != "password" {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out.entry("kind").or_insert(json!(KIND_RTSP));
    let has = stream
        .get("password")
        .and_then(Value::as_str)
        .is_some_and(|p| !p.is_empty());
    out.insert("has_password".into(), json!(has));
    out.insert("address".into(), json!(display_address(s(stream, "url"))));
    Value::Object(out)
}

fn origin(url: &str) -> (String, String, Option<u32>) {
    let Ok(parts) = url::urlsplit(url) else {
        return (String::new(), String::new(), None);
    };
    let default = match parts.scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        "rtsp" => Some(554),
        "rtsps" => Some(322),
        _ => None,
    };
    let port = match parts.port() {
        Ok(p) => p.map(u32::from).filter(|p| *p != 0).or(default),
        Err(_) => None,
    };
    (
        parts.scheme.clone(),
        parts.hostname().unwrap_or_default().to_lowercase(),
        port,
    )
}

/// Whether `b` is on the scheme, host and port of `a`.
pub fn same_origin(a: &str, b: &str) -> bool {
    origin(a) == origin(b)
}
