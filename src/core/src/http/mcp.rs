//! `/mcp` at the HTTP layer (`routes/mcp.py`): who may talk to it.
//!
//! The protocol and every decision about what an agent may do belong to the AI
//! area (`McpEndpoint`). This layer only checks that a request may reach the
//! server at all (this computer, switched on, the right bearer token), reads
//! the body, and writes the reply. The token lives in its own file,
//! `~/.corvus/mcp-token`, created on first use.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

use crate::request::{Method, Request};
use crate::response::{Response, SseStream};

pub const MAX_MCP_BODY_BYTES: usize = 1024 * 1024;
/// Where an operator finds the switch and the token.
pub const WHERE: &str = "Settings, Connections, MCP";
pub const JSONRPC_PARSE_ERROR: i64 = -32700;
pub const JSONRPC_INVALID_REQUEST: i64 = -32600;

pub fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// What the AI area implements: the protocol behind `/mcp`.
pub trait McpEndpoint: Send + Sync {
    /// Is the server switched on in the settings?
    fn enabled(&self) -> bool;

    /// Does the `Authorization` header carry the token?
    fn authorized(&self, authorization: Option<&str>) -> bool;

    /// One POSTed JSON-RPC message or batch: `(status, reply, headers)`; a
    /// reply of `None` is a 202 with no body.
    fn handle_post(&self, message: Value, session: Option<&str>) -> (u16, Option<Value>, Vec<(String, String)>);

    /// The event stream of a session (`GET /mcp`), or `None` without one.
    fn open_stream(&self, session: Option<&str>) -> Option<Box<dyn SseStream>>;

    /// A client said goodbye (`DELETE /mcp`).
    fn end_session(&self, session: Option<&str>) -> bool;
}

/// The service the AI area registers to turn `/mcp` on.
pub struct McpHandle(pub Arc<dyn McpEndpoint>);

fn reply(status: u16, payload: Option<Value>, headers: &[(String, String)]) -> Response {
    let mut r = match payload {
        Some(p) => Response::json_status(p, status),
        None => Response::empty(status),
    };
    for (k, v) in headers {
        r = r.with_header(k, v);
    }
    r
}

/// This computer, switched on, and the token: or the reply saying which is missing.
fn admit(req: &Request, mcp: Option<&McpHandle>) -> Result<Arc<dyn McpEndpoint>, Response> {
    let refuse = |status: u16, code: i64, msg: &str| reply(status, Some(jsonrpc_error(Value::Null, code, msg)), &[]);
    let Some(mcp) = mcp else {
        return Err(refuse(503, -32000, "MCP is not available"));
    };
    let svc = mcp.0.clone();
    if !req.is_loopback() {
        return Err(refuse(403, -32000, "The Corvus MCP server only answers this computer."));
    }
    if !svc.enabled() {
        return Err(refuse(
            403,
            -32000,
            &format!("The MCP server is switched off in Corvus GCS ({WHERE})."),
        ));
    }
    if !svc.authorized(req.headers.get("Authorization")) {
        let msg = format!("Missing or wrong token. Copy it from {WHERE}.");
        return Err(reply(
            401,
            Some(jsonrpc_error(Value::Null, -32001, &msg)),
            &[("WWW-Authenticate".into(), "Bearer realm=\"corvus-gcs\"".into())],
        ));
    }
    Ok(svc)
}

pub fn handle(req: &Request, mcp: Option<&McpHandle>) -> Response {
    let svc = match admit(req, mcp) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let session = req.headers.get("Mcp-Session-Id");
    match req.method {
        Method::Post => {
            let length = req
                .headers
                .get("Content-Length")
                .map_or(Some(0), |v| v.trim().parse::<i64>().ok());
            match length {
                Some(n) if n >= 0 && n as usize <= MAX_MCP_BODY_BYTES && req.body.len() <= MAX_MCP_BODY_BYTES => {}
                Some(n) if n > 0 => {
                    return reply(
                        413,
                        Some(jsonrpc_error(
                            Value::Null,
                            JSONRPC_INVALID_REQUEST,
                            "bad or oversized body",
                        )),
                        &[],
                    );
                }
                _ => {
                    return reply(
                        400,
                        Some(jsonrpc_error(
                            Value::Null,
                            JSONRPC_INVALID_REQUEST,
                            "bad or oversized body",
                        )),
                        &[],
                    )
                }
            }
            let Ok(message) = serde_json::from_slice::<Value>(&req.body) else {
                return reply(
                    400,
                    Some(jsonrpc_error(Value::Null, JSONRPC_PARSE_ERROR, "invalid JSON")),
                    &[],
                );
            };
            let (status, payload, headers) = svc.handle_post(message, session);
            reply(status, payload, &headers)
        }
        Method::Get => {
            let wants_stream = req
                .headers
                .get("Accept")
                .is_some_and(|a| a.contains("text/event-stream"));
            match wants_stream.then(|| svc.open_stream(session)).flatten() {
                Some(stream) => Response::sse(stream),
                None => reply(405, None, &[("Allow".into(), "POST, DELETE".into())]),
            }
        }
        Method::Delete => reply(if svc.end_session(session) { 200 } else { 404 }, None, &[]),
    }
}

/// The access token, created on first use and kept in its own file.
pub struct TokenStore {
    path: PathBuf,
    token: Mutex<Option<String>>,
}

impl TokenStore {
    pub fn new(path: impl Into<PathBuf>) -> TokenStore {
        TokenStore {
            path: path.into(),
            token: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A token of at least 32 characters read from the file, or a new one.
    pub fn token(&self) -> String {
        let mut cached = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = cached.as_ref() {
            return t.clone();
        }
        let text = std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .trim()
            .to_string();
        let token = if text.chars().count() >= 32 {
            text
        } else {
            self.write_new()
        };
        *cached = Some(token.clone());
        token
    }

    /// A new token. Clients holding the old one are refused from now on.
    pub fn regenerate(&self) -> String {
        let mut cached = self.token.lock().unwrap_or_else(|e| e.into_inner());
        let token = self.write_new();
        *cached = Some(token.clone());
        token
    }

    fn write_new(&self) -> String {
        let mut bytes = [0u8; 32];
        if getrandom::fill(&mut bytes).is_err() {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            bytes[..16].copy_from_slice(&t.to_le_bytes());
            bytes[16..20].copy_from_slice(&std::process::id().to_le_bytes());
        }
        let token = URL_SAFE_NO_PAD.encode(bytes);
        if let Err(e) = self.save(&token) {
            eprintln!("corvus: mcp: token not saved to {}: {e}", self.path.display());
        }
        token
    }

    fn save(&self, token: &str) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_file_name(format!(
            "{}.tmp",
            self.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
        ));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(format!("{token}\n").as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &self.path)
    }

    /// Whether an `Authorization` header carries the token: scheme `Bearer`
    /// in any case, the value compared in constant time.
    pub fn authorized(&self, header: Option<&str>) -> bool {
        let Some(h) = header else { return false };
        let (scheme, value) = h.trim().split_once(' ').unwrap_or((h.trim(), ""));
        let value = value.trim();
        if !scheme.eq_ignore_ascii_case("bearer") || value.is_empty() {
            return false;
        }
        constant_time_eq(value.as_bytes(), self.token().as_bytes())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-core-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_token_is_made_once_and_kept() {
        let d = dir("token");
        let store = TokenStore::new(d.join("mcp-token"));
        let t = store.token();
        assert_eq!(t.len(), 43);
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_eq!(std::fs::read_to_string(d.join("mcp-token")).unwrap(), format!("{t}\n"));
        assert_eq!(
            TokenStore::new(d.join("mcp-token")).token(),
            t,
            "read back from the file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(d.join("mcp-token")).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let new = store.regenerate();
        assert_ne!(new, t);
        assert!(!store.authorized(Some(&format!("Bearer {t}"))));
        assert!(store.authorized(Some(&format!("Bearer {new}"))));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_short_token_in_the_file_is_replaced() {
        let d = dir("short");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mcp-token"), "short\n").unwrap();
        assert_eq!(TokenStore::new(d.join("mcp-token")).token().len(), 43);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn bearer_is_checked_the_way_python_checks_it() {
        let d = dir("bearer");
        let store = TokenStore::new(d.join("mcp-token"));
        let t = store.token();
        assert!(store.authorized(Some(&format!("bearer   {t} "))));
        assert!(store.authorized(Some(&format!("BEARER {t}"))));
        assert!(!store.authorized(Some(&format!("Basic {t}"))));
        assert!(!store.authorized(Some("Bearer")));
        assert!(!store.authorized(Some("Bearer nope")));
        assert!(!store.authorized(Some(&t)));
        assert!(!store.authorized(None));
        std::fs::remove_dir_all(&d).unwrap();
    }

    struct Fake {
        on: bool,
    }

    impl McpEndpoint for Fake {
        fn enabled(&self) -> bool {
            self.on
        }
        fn authorized(&self, h: Option<&str>) -> bool {
            h == Some("Bearer good")
        }
        fn handle_post(&self, m: Value, session: Option<&str>) -> (u16, Option<Value>, Vec<(String, String)>) {
            if m.get("method") == Some(&json!("notify")) {
                return (202, None, vec![]);
            }
            (
                200,
                Some(json!({"echo": m, "session": session})),
                vec![("Mcp-Session-Id".into(), "s1".into())],
            )
        }
        fn open_stream(&self, session: Option<&str>) -> Option<Box<dyn SseStream>> {
            session.map(|_| -> Box<dyn SseStream> { Box::new(Closed) })
        }
        fn end_session(&self, session: Option<&str>) -> bool {
            session == Some("s1")
        }
    }

    struct Closed;
    impl SseStream for Closed {
        fn next(&mut self, _: std::time::Duration) -> crate::response::SseNext {
            crate::response::SseNext::End
        }
    }

    fn req(method: Method, token: Option<&str>) -> Request {
        let mut r = Request::new(method, "/mcp");
        r.peer = Some("127.0.0.1".parse().unwrap());
        if let Some(t) = token {
            r.headers.push("Authorization", t);
        }
        r
    }

    fn body(r: &Response) -> Value {
        r.json_body().unwrap()
    }

    #[test]
    fn admission_names_what_is_missing() {
        let on = McpHandle(Arc::new(Fake { on: true }));
        let off = McpHandle(Arc::new(Fake { on: false }));
        let r = handle(&req(Method::Post, Some("Bearer good")), None);
        assert_eq!(
            (r.status, body(&r)["error"]["message"].clone()),
            (503, json!("MCP is not available"))
        );
        let mut far = req(Method::Post, Some("Bearer good"));
        far.peer = Some("10.0.0.5".parse().unwrap());
        let r = handle(&far, Some(&on));
        assert_eq!(
            (r.status, body(&r)["error"]["message"].clone()),
            (403, json!("The Corvus MCP server only answers this computer."))
        );
        let r = handle(&req(Method::Post, Some("Bearer good")), Some(&off));
        assert_eq!(
            (r.status, body(&r)["error"]["message"].clone()),
            (
                403,
                json!("The MCP server is switched off in Corvus GCS (Settings, Connections, MCP).")
            )
        );
        let r = handle(&req(Method::Post, Some("Bearer bad")), Some(&on));
        assert_eq!(r.status, 401);
        assert_eq!(r.header("www-authenticate"), Some("Bearer realm=\"corvus-gcs\""));
        assert_eq!(
            body(&r)["error"],
            json!({"code": -32001, "message": "Missing or wrong token. Copy it from Settings, Connections, MCP."})
        );
        assert_eq!(body(&r)["jsonrpc"], json!("2.0"));
    }

    #[test]
    fn post_get_and_delete_follow_routes_mcp_py() {
        let on = McpHandle(Arc::new(Fake { on: true }));
        let mut post = req(Method::Post, Some("Bearer good"));
        post.body = br#"{"method": "ping"}"#.to_vec();
        post.headers.push("Mcp-Session-Id", "s0");
        let r = handle(&post, Some(&on));
        assert_eq!(r.status, 200);
        assert_eq!(r.header("mcp-session-id"), Some("s1"));
        assert_eq!(body(&r)["session"], json!("s0"));
        let mut note = req(Method::Post, Some("Bearer good"));
        note.body = br#"{"method": "notify"}"#.to_vec();
        let r = handle(&note, Some(&on));
        assert_eq!((r.status, r.json_body()), (202, None));
        let mut junk = req(Method::Post, Some("Bearer good"));
        junk.body = b"{".to_vec();
        let r = handle(&junk, Some(&on));
        assert_eq!((r.status, body(&r)["error"]["code"].clone()), (400, json!(-32700)));
        let mut big = req(Method::Post, Some("Bearer good"));
        big.headers
            .push("Content-Length", &(MAX_MCP_BODY_BYTES + 1).to_string());
        assert_eq!(handle(&big, Some(&on)).status, 413);
        let mut weird = req(Method::Post, Some("Bearer good"));
        weird.headers.push("Content-Length", "abc");
        assert_eq!(handle(&weird, Some(&on)).status, 400);

        let mut get = req(Method::Get, Some("Bearer good"));
        assert_eq!(handle(&get, Some(&on)).status, 405);
        assert_eq!(handle(&get, Some(&on)).header("allow"), Some("POST, DELETE"));
        get.headers.push("Accept", "text/event-stream");
        assert_eq!(handle(&get, Some(&on)).status, 405, "no session");
        get.headers.push("Mcp-Session-Id", "s1");
        let r = handle(&get, Some(&on));
        assert_eq!((r.status, r.header("content-type")), (200, Some("text/event-stream")));

        let mut del = req(Method::Delete, Some("Bearer good"));
        assert_eq!(handle(&del, Some(&on)).status, 404);
        del.headers.push("Mcp-Session-Id", "s1");
        assert_eq!(handle(&del, Some(&on)).status, 200);
    }
}
