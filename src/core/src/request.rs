//! A request as a route handler sees it. The fields answer what a Python
//! handler reads from `self`: `self.path` and `urlparse(...).query`,
//! `self.headers`, the body, `self.client_address`.

use std::net::IpAddr;

use crate::json::Map;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "GET" => Some(Method::Get),
            "POST" => Some(Method::Post),
            "DELETE" => Some(Method::Delete),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Delete => "DELETE",
        }
    }
}

/// Header fields, looked up without regard to case.
#[derive(Debug, Clone, Default)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn push(&mut self, name: &str, value: &str) {
        self.0.push((name.to_string(), value.trim().to_string()));
    }

    /// The first value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// The query string as `urllib.parse.parse_qs` reads it: names with their
/// values, pairs without a value dropped, `+` a space.
#[derive(Debug, Clone, Default)]
pub struct Query(Vec<(String, Vec<String>)>);

impl Query {
    pub fn parse(raw: &str) -> Query {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for pair in raw.split('&') {
            if pair.is_empty() {
                continue;
            }
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            let (name, value) = (unquote(&name.replace('+', " ")), unquote(&value.replace('+', " ")));
            match out.iter_mut().find(|(k, _)| *k == name) {
                Some((_, values)) => values.push(value),
                None => out.push((name, vec![value])),
            }
        }
        Query(out)
    }

    /// `parse_qs(q).get(name, [None])[0]`: the first value.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.all(name).first().map(String::as_str)
    }

    pub fn all(&self, name: &str) -> &[String] {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }
}

/// `urllib.parse.unquote`: percent escapes decoded as UTF-8, bad bytes replaced.
pub fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Where a request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A socket of the HTTP server.
    Http,
    /// `Core::internal_call`: a tool or the interface, in this process.
    InProcess,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    /// The path alone, percent escapes as the client wrote them.
    pub path: String,
    pub query: Query,
    pub headers: Headers,
    /// The body as it arrived (for a raw route), or empty.
    pub body: Vec<u8>,
    /// The file a spooled route's body was streamed to (the server removes it
    /// after the handler returns; a handler that keeps it moves it away).
    pub body_file: Option<std::path::PathBuf>,
    /// The peer's address, loopback for an in-process call.
    pub peer: Option<IpAddr>,
    pub origin: Origin,
    params: Vec<(String, String)>,
    payload: Map,
}

impl Request {
    /// A request for `target` (`/api/x?a=1`). A fragment is dropped.
    pub fn new(method: Method, target: &str) -> Request {
        let target = target.split('#').next().unwrap_or("");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        Request {
            method,
            path: path.to_string(),
            query: Query::parse(query),
            headers: Headers::default(),
            body: Vec::new(),
            body_file: None,
            peer: None,
            origin: Origin::Http,
            params: Vec::new(),
            payload: Map::new(),
        }
    }

    /// An in-process call: loopback, the payload already an object.
    pub fn internal(method: Method, target: &str, payload: Map) -> Request {
        let mut r = Request::new(method, target);
        r.origin = Origin::InProcess;
        r.peer = Some(IpAddr::from([127, 0, 0, 1]));
        r.payload = payload;
        r
    }

    /// The JSON object of a POST route (`{}` for a GET).
    pub fn payload(&self) -> &Map {
        &self.payload
    }

    pub fn set_payload(&mut self, payload: Map) {
        self.payload = payload;
    }

    /// A capture of the matched pattern (`{source}` in `/api/tiles/{source}/...`).
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub(crate) fn set_params(&mut self, params: Vec<(String, String)>) {
        self.params = params;
    }

    pub fn is_loopback(&self) -> bool {
        self.peer.is_some_and(|ip| {
            ip.is_loopback() || matches!(ip, IpAddr::V6(v) if v.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_follows_parse_qs() {
        let q = Query::parse("topics=console,params&job=a+b&empty=&bare&topics=x&n=%C3%A4");
        assert_eq!(q.get("topics"), Some("console,params"));
        assert_eq!(q.all("topics"), ["console,params", "x"]);
        assert_eq!(q.get("job"), Some("a b"));
        assert_eq!(q.get("empty"), None);
        assert_eq!(q.get("bare"), None);
        assert_eq!(q.get("n"), Some("ä"));
        assert_eq!(q.get("missing"), None);
    }

    #[test]
    fn unquote_keeps_bad_escapes() {
        assert_eq!(unquote("a%20b"), "a b");
        assert_eq!(unquote("100%"), "100%");
        assert_eq!(unquote("%zz"), "%zz");
        assert_eq!(unquote("%4"), "%4");
        assert_eq!(unquote("%41"), "A");
    }

    #[test]
    fn target_is_split_and_headers_ignore_case() {
        let mut r = Request::new(Method::Get, "/api/languages/strings?code=de#top");
        assert_eq!(r.path, "/api/languages/strings");
        assert_eq!(r.query.get("code"), Some("de"));
        r.headers.push("Content-Length", " 12 ");
        assert_eq!(r.headers.get("content-length"), Some("12"));
        assert_eq!(r.headers.get("x-none"), None);
    }

    #[test]
    fn loopback_covers_mapped_v6() {
        let mut r = Request::new(Method::Get, "/");
        r.peer = Some("::ffff:127.0.0.1".parse().unwrap());
        assert!(r.is_loopback());
        r.peer = Some("192.168.1.2".parse().unwrap());
        assert!(!r.is_loopback());
        assert!(Request::internal(Method::Get, "/", Map::new()).is_loopback());
    }
}
