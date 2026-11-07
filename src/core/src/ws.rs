//! A WebSocket (RFC 6455) on one of the HTTP server's own connections
//! (`websocket.py`).
//!
//! Why a terminal wants one: a browser holds at most six HTTP/1.1 connections
//! per host, shared by every window, and two are taken for good by the
//! telemetry and event streams. A WebSocket is counted apart and carries the
//! input as well. Only what a browser sends is accepted: masked frames, text,
//! and messages no larger than `MAX_MESSAGE`. No extension or subprotocol is
//! offered, so none can be in use.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sha1::{Digest, Sha1};

use crate::request::Headers;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub const OP_CONTINUATION: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

pub const CLOSE_NORMAL: u16 = 1000;
pub const CLOSE_GOING_AWAY: u16 = 1001;
pub const CLOSE_PROTOCOL_ERROR: u16 = 1002;
pub const CLOSE_UNSUPPORTED: u16 = 1003;
pub const CLOSE_INVALID_DATA: u16 = 1007;
pub const CLOSE_TOO_BIG: u16 = 1009;

/// A terminal's messages are keystrokes and a paste now and then.
pub const MAX_MESSAGE: usize = 1024 * 1024;

fn tokens(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Why `headers` are not a WebSocket opening handshake, if they are not.
pub fn handshake_problem(headers: &Headers) -> Option<&'static str> {
    let get = |n: &str| headers.get(n).unwrap_or("");
    if !tokens(get("Upgrade")).iter().any(|t| t == "websocket")
        || !tokens(get("Connection")).iter().any(|t| t == "upgrade")
    {
        return Some("not a WebSocket upgrade");
    }
    if get("Sec-WebSocket-Version").trim() != "13" {
        return Some("unsupported WebSocket version");
    }
    match STANDARD.decode(get("Sec-WebSocket-Key").trim()) {
        Ok(k) if k.len() == 16 => None,
        _ => Some("bad Sec-WebSocket-Key"),
    }
}

/// The `Sec-WebSocket-Accept` that answers `Sec-WebSocket-Key` `key`.
pub fn accept_key(key: &str) -> String {
    let mut h = Sha1::new();
    h.update(key.trim().as_bytes());
    h.update(GUID.as_bytes());
    STANDARD.encode(h.finalize())
}

/// The whole `101` answer, status line included.
pub fn handshake_response(key: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    )
    .into_bytes()
}

/// One unmasked frame, as a server sends it.
pub fn encode_frame(opcode: u8, payload: &[u8], fin: bool) -> Vec<u8> {
    let head = if fin { 0x80 } else { 0 } | (opcode & 0x0F);
    let n = payload.len();
    let mut out = vec![head];
    if n < 126 {
        out.push(n as u8);
    } else if n < 1 << 16 {
        out.push(126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

/// One masked frame, as a client sends it. For the tests.
pub fn mask_frame(opcode: u8, payload: &[u8], mask: [u8; 4], fin: bool) -> Vec<u8> {
    let head = if fin { 0x80 } else { 0 } | (opcode & 0x0F);
    let n = payload.len();
    let mut out = vec![head];
    if n < 126 {
        out.push(0x80 | n as u8);
    } else if n < 1 << 16 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// The peer broke the protocol; `code` is the close code that says how.
#[derive(Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub code: u16,
    pub reason: &'static str,
}

fn perr(code: u16, reason: &'static str) -> ProtocolError {
    ProtocolError { code, reason }
}

/// The connection is over: closed by either side, or gone.
#[derive(Debug, PartialEq, Eq)]
pub struct Closed(pub String);

/// Turns the bytes a client sends into whole messages.
pub struct FrameReader {
    buf: Vec<u8>,
    max: usize,
    parts: Vec<u8>,
    opcode: Option<u8>,
}

impl FrameReader {
    pub fn new(max_message: usize) -> FrameReader {
        FrameReader {
            buf: Vec::new(),
            max: max_message,
            parts: Vec::new(),
            opcode: None,
        }
    }

    /// Every message `data` completed: a text or binary message with its
    /// fragments joined, or a control frame.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<(u8, Vec<u8>)>, ProtocolError> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some((fin, opcode, payload)) = self.frame()? {
            if opcode >= 0x8 {
                if !fin || payload.len() > 125 {
                    return Err(perr(CLOSE_PROTOCOL_ERROR, "bad control frame"));
                }
                out.push((opcode, payload));
                continue;
            }
            match opcode {
                OP_CONTINUATION => {
                    if self.opcode.is_none() {
                        return Err(perr(CLOSE_PROTOCOL_ERROR, "continuation without a message"));
                    }
                }
                OP_TEXT | OP_BINARY => {
                    if self.opcode.is_some() {
                        return Err(perr(CLOSE_PROTOCOL_ERROR, "new message inside a fragmented one"));
                    }
                    self.opcode = Some(opcode);
                }
                _ => return Err(perr(CLOSE_PROTOCOL_ERROR, "unknown opcode")),
            }
            if self.parts.len() + payload.len() > self.max {
                return Err(perr(CLOSE_TOO_BIG, "message too big"));
            }
            self.parts.extend_from_slice(&payload);
            if fin {
                out.push((self.opcode.take().unwrap_or(OP_TEXT), std::mem::take(&mut self.parts)));
            }
        }
        Ok(out)
    }

    #[allow(clippy::type_complexity)]
    fn frame(&mut self) -> Result<Option<(bool, u8, Vec<u8>)>, ProtocolError> {
        let buf = &self.buf;
        if buf.len() < 2 {
            return Ok(None);
        }
        let (b0, b1) = (buf[0], buf[1]);
        if b0 & 0x70 != 0 {
            return Err(perr(CLOSE_PROTOCOL_ERROR, "no extension was agreed on"));
        }
        if b1 & 0x80 == 0 {
            return Err(perr(CLOSE_PROTOCOL_ERROR, "a client frame must be masked"));
        }
        let mut n = (b1 & 0x7F) as u64;
        let mut pos = 2;
        if n == 126 {
            if buf.len() < 4 {
                return Ok(None);
            }
            n = u16::from_be_bytes([buf[2], buf[3]]) as u64;
            pos = 4;
        } else if n == 127 {
            if buf.len() < 10 {
                return Ok(None);
            }
            n = u64::from_be_bytes(buf[2..10].try_into().unwrap_or([0; 8]));
            pos = 10;
        }
        if n > self.max as u64 {
            return Err(perr(CLOSE_TOO_BIG, "message too big"));
        }
        let n = n as usize;
        if buf.len() < pos + 4 + n {
            return Ok(None);
        }
        let mask = [buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]];
        let payload: Vec<u8> = buf[pos + 4..pos + 4 + n]
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect();
        self.buf.drain(..pos + 4 + n);
        Ok(Some((b0 & 0x80 != 0, b0 & 0x0F, payload)))
    }
}

/// A server side WebSocket over a connected socket, after the handshake.
/// `send_text` may be called from any thread, `receive` from one only, and
/// `close` from either, more than once.
/// A whole message: its opcode and payload.
type Message = (u8, Vec<u8>);

pub struct WebSocket {
    send: Mutex<TcpStream>,
    recv: Mutex<(TcpStream, FrameReader, Vec<Message>)>,
    closed: AtomicBool,
}

impl WebSocket {
    pub fn new(stream: TcpStream, max_message: usize) -> std::io::Result<WebSocket> {
        let recv = stream.try_clone()?;
        Ok(WebSocket {
            send: Mutex::new(stream),
            recv: Mutex::new((recv, FrameReader::new(max_message), Vec::new())),
            closed: AtomicBool::new(false),
        })
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn send_frame(&self, frame: &[u8]) -> Result<(), Closed> {
        if self.closed() {
            return Err(Closed("closed".into()));
        }
        let mut s = self.send.lock().unwrap_or_else(|e| e.into_inner());
        s.write_all(frame).and_then(|_| s.flush()).map_err(|e| {
            self.closed.store(true, Ordering::SeqCst);
            Closed(e.to_string())
        })
    }

    pub fn send_text(&self, text: &str) -> Result<(), Closed> {
        self.send_frame(&encode_frame(OP_TEXT, text.as_bytes(), true))
    }

    pub fn ping(&self, payload: &[u8]) -> Result<(), Closed> {
        self.send_frame(&encode_frame(OP_PING, &payload[..payload.len().min(125)], true))
    }

    /// Says goodbye, if the connection is still there to say it on.
    pub fn close(&self, code: u16, reason: &str) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut body = code.to_be_bytes().to_vec();
        body.extend_from_slice(&reason.as_bytes()[..reason.len().min(123)]);
        let mut s = self.send.lock().unwrap_or_else(|e| e.into_inner());
        let _ = s.write_all(&encode_frame(OP_CLOSE, &body, true));
        let _ = s.flush();
    }

    /// The next text message, or `None` when none came within `timeout`.
    /// Answers pings on its own. Once the peer closed or went away (after
    /// answering its close), and on a protocol error (answered with the close
    /// code it deserves), the result is `Closed`.
    pub fn receive(&self, timeout: Duration) -> Result<Option<String>, Closed> {
        loop {
            let pending = {
                let mut r = self.recv.lock().unwrap_or_else(|e| e.into_inner());
                (!r.2.is_empty()).then(|| r.2.remove(0))
            };
            if let Some((opcode, payload)) = pending {
                if let Some(text) = self.handle(opcode, payload)? {
                    return Ok(Some(text));
                }
                continue;
            }
            if self.closed() {
                return Err(Closed("closed".into()));
            }
            let mut r = self.recv.lock().unwrap_or_else(|e| e.into_inner());
            let _ = r.0.set_read_timeout(Some(timeout.max(Duration::from_millis(1))));
            let mut chunk = [0u8; 65536];
            match r.0.read(&mut chunk) {
                Ok(0) => {
                    self.closed.store(true, Ordering::SeqCst);
                    return Err(Closed("peer went away".into()));
                }
                Ok(n) => match r.1.feed(&chunk[..n]) {
                    Ok(messages) => r.2.extend(messages),
                    Err(e) => {
                        drop(r);
                        self.close(e.code, e.reason);
                        return Err(Closed(e.reason.into()));
                    }
                },
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Ok(None)
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.closed.store(true, Ordering::SeqCst);
                    return Err(Closed(e.to_string()));
                }
            }
        }
    }

    fn handle(&self, opcode: u8, payload: Vec<u8>) -> Result<Option<String>, Closed> {
        match opcode {
            OP_TEXT => match String::from_utf8(payload) {
                Ok(t) => Ok(Some(t)),
                Err(_) => {
                    self.close(CLOSE_INVALID_DATA, "text is not UTF-8");
                    Err(Closed("text is not UTF-8".into()))
                }
            },
            OP_PING => self.send_frame(&encode_frame(OP_PONG, &payload, true)).map(|_| None),
            OP_PONG => Ok(None),
            OP_CLOSE => {
                self.close(CLOSE_NORMAL, "");
                Err(Closed("closed by the peer".into()))
            }
            _ => {
                self.close(CLOSE_UNSUPPORTED, "text only");
                Err(Closed("binary message".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        let mut h = Headers::default();
        for (k, v) in pairs {
            h.push(k, v);
        }
        h
    }

    #[test]
    fn the_accept_key_is_the_rfc_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert!(String::from_utf8(handshake_response("dGhlIHNhbXBsZSBub25jZQ=="))
            .unwrap()
            .contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
    }

    #[test]
    fn the_handshake_is_checked_in_the_pythons_order() {
        let ok = [
            ("Upgrade", "WebSocket"),
            ("Connection", "keep-alive, Upgrade"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ];
        assert_eq!(handshake_problem(&headers(&ok)), None);
        assert_eq!(handshake_problem(&headers(&[])), Some("not a WebSocket upgrade"));
        assert_eq!(
            handshake_problem(&headers(&[ok[0], ("Connection", "close")])),
            Some("not a WebSocket upgrade")
        );
        assert_eq!(
            handshake_problem(&headers(&[ok[0], ok[1], ("Sec-WebSocket-Version", "8")])),
            Some("unsupported WebSocket version")
        );
        assert_eq!(
            handshake_problem(&headers(&[ok[0], ok[1], ok[2], ("Sec-WebSocket-Key", "abc")])),
            Some("bad Sec-WebSocket-Key")
        );
    }

    #[test]
    fn frames_round_trip_and_fragments_join() {
        let mut r = FrameReader::new(MAX_MESSAGE);
        let text = r.feed(&mask_frame(OP_TEXT, b"hello", [1, 2, 3, 4], true)).unwrap();
        assert_eq!(text, vec![(OP_TEXT, b"hello".to_vec())]);
        let mut wire = mask_frame(OP_TEXT, b"he", [9, 9, 9, 9], false);
        wire.extend(mask_frame(OP_PING, b"p", [1, 1, 1, 1], true));
        wire.extend(mask_frame(OP_CONTINUATION, b"llo", [5, 6, 7, 8], true));
        let got = r.feed(&wire).unwrap();
        assert_eq!(got, vec![(OP_PING, b"p".to_vec()), (OP_TEXT, b"hello".to_vec())]);
        let big = vec![b'x'; 70_000];
        let got = r.feed(&mask_frame(OP_TEXT, &big, [1, 2, 3, 4], true)).unwrap();
        assert_eq!(got[0].1.len(), 70_000);
        assert_eq!(encode_frame(OP_TEXT, b"hi", true), vec![0x81, 2, b'h', b'i']);
        assert_eq!(encode_frame(OP_TEXT, &[0; 300], true)[..4], [0x81, 126, 1, 44]);
    }

    #[test]
    fn a_frame_in_pieces_waits_for_the_rest() {
        let wire = mask_frame(OP_TEXT, b"split", [3, 3, 3, 3], true);
        let mut r = FrameReader::new(MAX_MESSAGE);
        for b in &wire[..wire.len() - 1] {
            assert!(r.feed(&[*b]).unwrap().is_empty());
        }
        assert_eq!(
            r.feed(&wire[wire.len() - 1..]).unwrap(),
            vec![(OP_TEXT, b"split".to_vec())]
        );
    }

    #[test]
    fn what_a_client_may_not_send_is_refused_with_its_code() {
        let err = |wire: &[u8]| FrameReader::new(10).feed(wire).unwrap_err();
        assert_eq!(err(&[0x81, 0x01, b'x']).code, CLOSE_PROTOCOL_ERROR, "unmasked");
        assert_eq!(
            err(&[0xC1, 0x80, 0, 0, 0, 0]).code,
            CLOSE_PROTOCOL_ERROR,
            "extension bit"
        );
        assert_eq!(
            err(&mask_frame(OP_CONTINUATION, b"x", [0; 4], true)).code,
            CLOSE_PROTOCOL_ERROR,
            "stray continuation"
        );
        assert_eq!(
            err(&mask_frame(OP_PING, b"x", [0; 4], false)).code,
            CLOSE_PROTOCOL_ERROR,
            "fragmented control"
        );
        assert_eq!(
            err(&mask_frame(0x3, b"x", [0; 4], true)).code,
            CLOSE_PROTOCOL_ERROR,
            "unknown opcode"
        );
        assert_eq!(err(&mask_frame(OP_TEXT, &[b'x'; 11], [0; 4], true)).code, CLOSE_TOO_BIG);
        let mut r = FrameReader::new(MAX_MESSAGE);
        r.feed(&mask_frame(OP_TEXT, b"a", [0; 4], false)).unwrap();
        assert_eq!(
            r.feed(&mask_frame(OP_TEXT, b"b", [0; 4], true)).unwrap_err().code,
            CLOSE_PROTOCOL_ERROR
        );
    }

    fn pair() -> (WebSocket, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (server, _) = l.accept().unwrap();
        (WebSocket::new(server, MAX_MESSAGE).unwrap(), client)
    }

    #[test]
    fn a_socket_sends_receives_answers_pings_and_closes() {
        let (ws, mut client) = pair();
        ws.send_text("out").unwrap();
        let mut got = [0u8; 5];
        client.read_exact(&mut got).unwrap();
        assert_eq!(got, [0x81, 3, b'o', b'u', b't']);

        assert_eq!(ws.receive(Duration::from_millis(20)).unwrap(), None);
        client
            .write_all(&mask_frame(OP_PING, b"x", [1, 2, 3, 4], true))
            .unwrap();
        client
            .write_all(&mask_frame(OP_TEXT, b"in", [1, 2, 3, 4], true))
            .unwrap();
        assert_eq!(ws.receive(Duration::from_secs(2)).unwrap().as_deref(), Some("in"));
        let mut pong = [0u8; 3];
        client.read_exact(&mut pong).unwrap();
        assert_eq!(pong, [0x8A, 1, b'x']);

        client
            .write_all(&mask_frame(OP_CLOSE, &[3, 232], [1, 2, 3, 4], true))
            .unwrap();
        assert!(ws.receive(Duration::from_secs(2)).is_err());
        assert!(ws.closed());
        assert!(ws.send_text("late").is_err());
        let mut bye = [0u8; 4];
        client.read_exact(&mut bye).unwrap();
        assert_eq!(bye, [0x88, 2, 3, 232]);
        ws.close(CLOSE_NORMAL, "again");
    }

    #[test]
    fn a_protocol_error_is_answered_with_a_close_and_ends_the_socket() {
        let (ws, mut client) = pair();
        client.write_all(&[0x81, 0x01, b'x']).unwrap();
        assert!(ws.receive(Duration::from_secs(2)).is_err());
        let mut head = [0u8; 4];
        client.read_exact(&mut head).unwrap();
        assert_eq!(&head[..2], &[0x88, 2 + "a client frame must be masked".len() as u8]);
        assert_eq!(u16::from_be_bytes([head[2], head[3]]), CLOSE_PROTOCOL_ERROR);
    }

    #[test]
    fn a_vanished_peer_ends_receive() {
        let (ws, client) = pair();
        drop(client);
        assert!(ws.receive(Duration::from_secs(2)).is_err());
    }

    #[test]
    fn frames_are_the_pythons() {
        let c: serde_json::Value = serde_json::from_str(include_str!("../tests/data/http_cases.json")).unwrap();
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            c["accept_key"].as_str().unwrap()
        );
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(&mask_frame(OP_TEXT, b"hello", [1, 2, 3, 4], true)),
            c["mask"].as_str().unwrap()
        );
        let heads: Vec<String> = [0usize, 125, 126, 70000]
            .iter()
            .map(|n| {
                hex(&encode_frame(OP_TEXT, &vec![b'x'; *n], true))
                    .chars()
                    .take(12)
                    .collect::<String>()
            })
            .collect();
        assert_eq!(serde_json::json!(heads), c["encode"]);
    }
}
