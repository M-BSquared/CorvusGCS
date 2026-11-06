//! What a route handler returns.

use std::time::Duration;

use serde_json::Value;

use crate::json;

/// One server-sent event: the name after `event:` and the JSON text after
/// `data:`, already encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub name: String,
    pub data: Vec<u8>,
}

impl SseEvent {
    pub fn new(name: &str, data: Vec<u8>) -> SseEvent {
        SseEvent {
            name: name.to_string(),
            data,
        }
    }

    pub fn json(name: &str, value: &Value) -> SseEvent {
        SseEvent::new(name, serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec()))
    }

    /// `event: NAME\ndata: DATA\n\n`, the bytes the Python `_send_sse` writes.
    pub fn frame(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + self.name.len() + 16);
        out.extend_from_slice(b"event: ");
        out.extend_from_slice(self.name.as_bytes());
        out.extend_from_slice(b"\ndata: ");
        out.extend_from_slice(&self.data);
        out.extend_from_slice(b"\n\n");
        out
    }
}

/// What a stream has for the connection thread right now.
pub enum SseNext {
    Events(Vec<SseEvent>),
    /// Nothing arrived within the wait.
    Idle,
    /// The stream is over; the connection closes.
    End,
}

/// A server-sent event stream. The connection thread owns it: it writes
/// `open()`, then calls `next` in short waits until the peer is gone, the
/// stream ends or the core stops, and always calls `close()` last.
pub trait SseStream: Send {
    /// The events sent before anything else (a snapshot).
    fn open(&mut self) -> Vec<SseEvent> {
        Vec::new()
    }

    /// Waits up to `wait` for events.
    fn next(&mut self, wait: Duration) -> SseNext;

    /// Detaches from whatever feeds it. Called exactly once.
    fn close(&mut self) {}

    /// The bytes written after `ping_every` of silence.
    fn ping(&self) -> &'static [u8] {
        b"event: ping\ndata: {}\n\n"
    }

    fn ping_every(&self) -> Duration {
        Duration::from_secs(15)
    }
}

pub enum Body {
    Empty,
    Bytes(Vec<u8>),
    Sse(Box<dyn SseStream>),
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    pub fn empty(status: u16) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    /// 200 with a JSON body.
    pub fn json(value: Value) -> Response {
        Response::json_status(value, 200)
    }

    pub fn json_status(value: Value, status: u16) -> Response {
        let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"null".to_vec());
        Response::bytes(status, "application/json", body)
    }

    /// `{"error": message}` with `status`.
    pub fn error(status: u16, message: &str) -> Response {
        Response::json_status(json::error(message), status)
    }

    /// `{"ok": false, "error": message}` with `status`.
    pub fn not_ok(status: u16, message: &str) -> Response {
        Response::json_status(json::not_ok(message), status)
    }

    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Response {
        Response {
            status,
            headers: vec![("Content-Type".into(), content_type.into())],
            body: Body::Bytes(body),
        }
    }

    /// An event stream (the 200 and its headers; the server adds the rest).
    pub fn sse(stream: Box<dyn SseStream>) -> Response {
        Response {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "text/event-stream".into()),
                ("Cache-Control".into(), "no-cache".into()),
                ("Connection".into(), "keep-alive".into()),
            ],
            body: Body::Sse(stream),
        }
    }

    /// 501 for a route whose area is not ported yet.
    pub fn unavailable() -> Response {
        Response::error(501, UNAVAILABLE)
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body parsed as JSON, for in-process callers and tests.
    pub fn json_body(&self) -> Option<Value> {
        match &self.body {
            Body::Bytes(b) => serde_json::from_slice(b).ok(),
            _ => None,
        }
    }
}

/// The text of every 501 of an area that is not ported yet. The interface
/// shows it as an offline notice.
pub const UNAVAILABLE: &str = "Not available in this build";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_frames_match_the_python_writer() {
        let e = SseEvent::json("state", &json!({"a": 1}));
        assert_eq!(e.frame(), b"event: state\ndata: {\"a\":1}\n\n".to_vec());
    }

    #[test]
    fn responses_carry_json() {
        let r = Response::error(404, "not found");
        assert_eq!(r.status, 404);
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.json_body(), Some(json!({"error": "not found"})));
        assert_eq!(Response::unavailable().status, 501);
        assert_eq!(
            Response::unavailable().json_body(),
            Some(json!({"error": "Not available in this build"}))
        );
    }
}
