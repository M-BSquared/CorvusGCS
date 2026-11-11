//! The route table: exact paths and patterns, per method.

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::Core;
use crate::request::{Method, Request};
use crate::response::Response;

/// A route handler. It runs on the thread of the connection (or of the
/// in-process caller), so it may block, and it must not panic on bad input:
/// a panic is answered with 500.
pub type Handler = Arc<dyn Fn(&Core, &Request) -> Response + Send + Sync>;

/// How the body of a POST route is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// A JSON object, parsed before the handler runs and read with
    /// `Request::payload`. An empty body is `{}`.
    Json,
    /// The bytes as they arrived, in `Request::body` (an upload).
    Raw,
    /// No body is read (every GET).
    None,
}

/// Per route settings.
#[derive(Debug, Clone, Copy)]
pub struct RouteOptions {
    pub body: Body,
    /// The largest body the route accepts, answered with 413 beyond it.
    pub max_body: usize,
    /// The text of the 413 beyond the cap, answered as `{"ok": false, "error":
    /// text}`; `None` answers the router's `{"error": "request too large"}`.
    pub too_large: Option<&'static str>,
    /// The body is written to a file as it arrives and `Request::body_file`
    /// names it, so an upload of any size is never held in memory.
    pub spool: bool,
}

/// The cap of a JSON route (`MAX_JSON_BODY_BYTES` in `server.py`).
pub const MAX_JSON_BODY: usize = 8 * 1024 * 1024;
/// The cap of the settings import (`MAX_SETTINGS_BODY_BYTES`).
pub const MAX_SETTINGS_BODY: usize = 32 * 1024 * 1024;

impl RouteOptions {
    pub const GET: RouteOptions = RouteOptions {
        body: Body::None,
        max_body: 0,
        too_large: None,
        spool: false,
    };
    pub const JSON: RouteOptions = RouteOptions {
        body: Body::Json,
        max_body: MAX_JSON_BODY,
        too_large: None,
        spool: false,
    };

    pub fn raw(max_body: usize) -> RouteOptions {
        RouteOptions {
            body: Body::Raw,
            max_body,
            too_large: None,
            spool: false,
        }
    }

    /// Answers the 413 beyond the cap with `text` (the Python route's own words).
    pub fn too_large(mut self, text: &'static str) -> RouteOptions {
        self.too_large = Some(text);
        self
    }

    /// Streams the body to a file (`Request::body_file`) instead of memory.
    pub fn spooled(mut self) -> RouteOptions {
        self.spool = true;
        self
    }

    pub fn json(max_body: usize) -> RouteOptions {
        RouteOptions {
            body: Body::Json,
            max_body,
            too_large: None,
            spool: false,
        }
    }
}

#[derive(Clone)]
pub struct Route {
    pub handler: Handler,
    pub options: RouteOptions,
}

/// A path with captures: `/api/tiles/{source}/{z:d}/{x:d}/{y:d}.png`.
///
/// `{name}` matches one non-empty segment, `{name:d}` one made of digits,
/// and `{name*}` as the last segment the rest of the path. Text after the `}`
/// in a segment (`{y:d}.png`) must follow the capture. A pattern is tried
/// before the exact table, as the Python dispatch tries its regular
/// expressions, and a path that does not fit falls through to it.
#[derive(Debug, Clone)]
pub struct Pattern {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone)]
enum Segment {
    Literal(String),
    Capture { name: String, digits: bool, suffix: String },
    Rest(String),
}

impl Pattern {
    pub fn parse(pattern: &str) -> Result<Pattern, String> {
        let body = pattern
            .strip_prefix('/')
            .ok_or_else(|| format!("pattern {pattern:?} must start with /"))?;
        let parts: Vec<&str> = body.split('/').collect();
        let mut segments = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let Some(open) = part.find('{') else {
                segments.push(Segment::Literal((*part).to_string()));
                continue;
            };
            if open != 0 {
                return Err(format!("pattern {pattern:?}: a capture starts its segment"));
            }
            let close = part
                .find('}')
                .ok_or_else(|| format!("pattern {pattern:?}: unclosed {{"))?;
            let inner = &part[1..close];
            let suffix = part[close + 1..].to_string();
            if let Some(name) = inner.strip_suffix('*') {
                if i != parts.len() - 1 || !suffix.is_empty() {
                    return Err(format!("pattern {pattern:?}: {{name*}} ends the pattern"));
                }
                segments.push(Segment::Rest(name.to_string()));
            } else {
                let (name, digits) = match inner.split_once(':') {
                    Some((n, "d")) => (n, true),
                    Some((_, t)) => return Err(format!("pattern {pattern:?}: unknown type {t:?}")),
                    None => (inner, false),
                };
                segments.push(Segment::Capture {
                    name: name.to_string(),
                    digits,
                    suffix,
                });
            }
        }
        Ok(Pattern { segments })
    }

    pub fn matches(&self, path: &str) -> Option<Vec<(String, String)>> {
        let rest = path.strip_prefix('/')?;
        let mut parts = rest.split('/');
        let mut out = Vec::new();
        for seg in &self.segments {
            match seg {
                Segment::Literal(l) => {
                    if parts.next()? != l {
                        return None;
                    }
                }
                Segment::Capture { name, digits, suffix } => {
                    let part = parts.next()?;
                    let value = part.strip_suffix(suffix.as_str())?;
                    if value.is_empty() || (*digits && !value.bytes().all(|b| b.is_ascii_digit())) {
                        return None;
                    }
                    out.push((name.clone(), value.to_string()));
                }
                Segment::Rest(name) => {
                    let first = parts.next()?;
                    let tail: Vec<&str> = std::iter::once(first).chain(parts.by_ref()).collect();
                    let value = tail.join("/");
                    if value.is_empty() {
                        return None;
                    }
                    out.push((name.clone(), value));
                    return Some(out);
                }
            }
        }
        if parts.next().is_some() {
            return None;
        }
        Some(out)
    }
}

#[derive(Default)]
pub struct Router {
    exact: HashMap<(Method, String), Route>,
    patterns: Vec<(Method, Pattern, Route)>,
    unavailable: HashMap<(Method, String), ()>,
    unavailable_patterns: Vec<(Method, Pattern)>,
}

pub struct Matched<'a> {
    pub route: &'a Route,
    pub params: Vec<(String, String)>,
}

impl Router {
    pub fn add(&mut self, method: Method, path: &str, route: Route) -> Result<(), String> {
        if self.exact.insert((method, path.to_string()), route).is_some() {
            return Err(format!("{} {path} is registered twice", method.as_str()));
        }
        Ok(())
    }

    pub fn add_pattern(&mut self, method: Method, pattern: &str, route: Route) -> Result<(), String> {
        let parsed = Pattern::parse(pattern)?;
        self.patterns.push((method, parsed, route));
        Ok(())
    }

    /// Names a route of an area that is not ported: it answers 501.
    pub fn add_unavailable(&mut self, method: Method, path: &str) {
        self.unavailable.insert((method, path.to_string()), ());
    }

    pub fn add_unavailable_pattern(&mut self, method: Method, pattern: &str) -> Result<(), String> {
        self.unavailable_patterns.push((method, Pattern::parse(pattern)?));
        Ok(())
    }

    pub fn find(&self, method: Method, path: &str) -> Option<Matched<'_>> {
        for (m, pattern, route) in &self.patterns {
            if *m == method {
                if let Some(params) = pattern.matches(path) {
                    return Some(Matched { route, params });
                }
            }
        }
        self.exact.get(&(method, path.to_string())).map(|route| Matched {
            route,
            params: Vec::new(),
        })
    }

    /// Whether `method path` is a route of an area that is not ported yet.
    pub fn is_unavailable(&self, method: Method, path: &str) -> bool {
        self.unavailable.contains_key(&(method, path.to_string()))
            || self
                .unavailable_patterns
                .iter()
                .any(|(m, p)| *m == method && p.matches(path).is_some())
    }

    /// The exact routes registered, as `(method, path)`, sorted.
    pub fn exact_routes(&self) -> Vec<(Method, String)> {
        let mut v: Vec<_> = self.exact.keys().cloned().collect();
        v.sort_by(|a, b| (a.1.as_str(), a.0.as_str()).cmp(&(b.1.as_str(), b.0.as_str())));
        v
    }

    pub fn unavailable_routes(&self) -> Vec<(Method, String)> {
        let mut v: Vec<_> = self.unavailable.keys().cloned().collect();
        v.sort_by(|a, b| (a.1.as_str(), a.0.as_str()).cmp(&(b.1.as_str(), b.0.as_str())));
        v
    }

    /// The largest body any route registered for `method path` takes.
    pub fn options_for(&self, method: Method, path: &str) -> Option<RouteOptions> {
        self.find(method, path).map(|m| m.route.options)
    }
}

pub(crate) fn apply_params(req: &mut Request, params: Vec<(String, String)>) {
    req.set_params(params);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Pattern {
        Pattern::parse(s).unwrap()
    }

    #[test]
    fn tile_pattern_matches_the_python_regex() {
        let pat = p("/api/tiles/{source}/{z:d}/{x:d}/{y:d}.png");
        let m = pat.matches("/api/tiles/esri/12/2200/1400.png").unwrap();
        assert_eq!(
            m,
            vec![
                ("source".to_string(), "esri".to_string()),
                ("z".to_string(), "12".to_string()),
                ("x".to_string(), "2200".to_string()),
                ("y".to_string(), "1400".to_string()),
            ]
        );
        assert!(pat.matches("/api/tiles/esri/12/2200/14x0.png").is_none());
        assert!(pat.matches("/api/tiles/esri/12/2200/1400.jpg").is_none());
        assert!(pat.matches("/api/tiles/esri/12/2200/1400.png/more").is_none());
        assert!(pat.matches("/api/tiles/jobs").is_none());
        assert!(pat.matches("/api/tiles//12/2200/1400.png").is_none());
    }

    #[test]
    fn rest_capture_takes_the_tail() {
        let pat = p("/api/plugins/asset/{id}/{rest*}");
        let m = pat.matches("/api/plugins/asset/vibration/js/main.js").unwrap();
        assert_eq!(m[0], ("id".to_string(), "vibration".to_string()));
        assert_eq!(m[1], ("rest".to_string(), "js/main.js".to_string()));
        assert!(pat.matches("/api/plugins/asset/vibration").is_none());
        assert!(pat.matches("/api/plugins/asset/vibration/").is_none());
    }

    #[test]
    fn bad_patterns_are_refused() {
        assert!(Pattern::parse("api/x").is_err());
        assert!(Pattern::parse("/api/{x").is_err());
        assert!(Pattern::parse("/api/{x:q}").is_err());
        assert!(Pattern::parse("/api/{r*}/x").is_err());
        assert!(Pattern::parse("/api/a{x}").is_err());
    }
}
