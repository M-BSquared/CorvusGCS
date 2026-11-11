//! Licensed tile APIs: turning an operator's key into a tile URL template
//! (`corvus/tile_sessions.py`).
//!
//! Google's Map Tiles API wants a session created first, Bing's REST API hands
//! out the tile URL in an imagery metadata answer. One call is made, its answer
//! turned into an ordinary template for `build_tile_url` and kept until it runs
//! out, so a pan costs one call per layer rather than one per tile.
//!
//! No threads and no open sockets: every call is a request with a timeout made
//! on the thread that asked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::fetch::{Fetch, Req};
use super::tile_http::mono;
use super::tile_sources::{self as ts, Licensed};
use super::util::{json_float, now_secs, quote, truthy};

const TIMEOUT_S: f64 = 10.0;
const MAX_BODY: usize = 256 * 1024;
const RETRY_AFTER_REFUSED_S: f64 = 60.0;
const RETRY_AFTER_OFFLINE_S: f64 = 15.0;
const EXPIRY_MARGIN_S: f64 = 600.0;
const BING_TTL_S: f64 = 12.0 * 3600.0;
const GOOGLE_DEFAULT_TTL_S: f64 = 12.0 * 3600.0;
const BING_HOST_SUFFIXES: [&str; 2] = [".virtualearth.net", ".bing.com"];

/// A licensed API would not hand out a tile template. `offline` separates
/// "could not reach it" (counts against the shared upstream breaker) from "it
/// answered no" (a wrong key or an API not enabled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub message: String,
    pub offline: bool,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn refused(message: &str) -> SessionError {
    SessionError {
        message: message.to_string(),
        offline: false,
    }
}

/// Where the two APIs live; a test points them at a stub.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub google_session: String,
    pub google_tiles: String,
    pub bing_metadata: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            google_session: ts::GOOGLE_TILES_SESSION_URL.into(),
            google_tiles: ts::GOOGLE_TILES_URL.into(),
            bing_metadata: ts::BING_METADATA_URL.into(),
        }
    }
}

type Entry = (Option<String>, f64, Option<SessionError>);
type Key = (String, String);

/// The templates handed out so far, by source and key.
pub struct Sessions {
    endpoints: Endpoints,
    user_agent: String,
    cache: Mutex<HashMap<Key, Entry>>,
    key_locks: Mutex<HashMap<Key, Arc<Mutex<()>>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sessions {
    pub fn new(user_agent: &str) -> Sessions {
        Sessions::with(user_agent, Endpoints::default())
    }

    pub fn with(user_agent: &str, endpoints: Endpoints) -> Sessions {
        Sessions {
            endpoints,
            user_agent: user_agent.to_string(),
            cache: Mutex::new(HashMap::new()),
            key_locks: Mutex::new(HashMap::new()),
        }
    }

    /// A tile template for `source_id` on the licensed API, using `token`.
    /// Cached per source and key until the session runs out; one caller per
    /// source and key makes the call, the rest wait for its answer.
    pub fn resolve(&self, net: &dyn Fetch, source_id: &str, token: &str) -> Result<String, SessionError> {
        self.resolve_at(net, source_id, token, mono())
    }

    pub fn resolve_at(&self, net: &dyn Fetch, source_id: &str, token: &str, now: f64) -> Result<String, SessionError> {
        let Some(block) = ts::licensed(source_id) else {
            return Err(refused(&format!("'{source_id}' has no licensed API")));
        };
        let key = (source_id.to_string(), token.to_string());
        let key_lock = Arc::clone(lock(&self.key_locks).entry(key.clone()).or_default());
        let _held = lock(&key_lock);
        if let Some((template, until, error)) = lock(&self.cache).get(&key).cloned() {
            if now < until {
                return match template {
                    Some(t) => Ok(t),
                    None => Err(error.unwrap_or_else(|| refused("the map service refused the key"))),
                };
            }
        }
        match self.fetch(net, &block, token) {
            Ok((template, ttl)) => {
                lock(&self.cache).insert(key, (Some(template.clone()), now + ttl, None));
                Ok(template)
            }
            Err(e) => {
                let retry = if e.offline {
                    RETRY_AFTER_OFFLINE_S
                } else {
                    RETRY_AFTER_REFUSED_S
                };
                lock(&self.cache).insert(key, (None, now + retry, Some(e.clone())));
                Err(e)
            }
        }
    }

    /// Forgets the template, so the next tile opens a fresh session.
    pub fn invalidate(&self, source_id: &str, token: &str) {
        lock(&self.cache).remove(&(source_id.to_string(), token.to_string()));
    }

    pub fn clear(&self) {
        lock(&self.cache).clear();
    }

    fn fetch(&self, net: &dyn Fetch, block: &Licensed, token: &str) -> Result<(String, f64), SessionError> {
        match block.api {
            "google_tiles" => self.google_session(net, block, token),
            "bing_rest" => self.bing_metadata(net, block, token),
            other => Err(refused(&format!("unknown licensed API '{other}'"))),
        }
    }

    fn request_json(
        &self,
        net: &dyn Fetch,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Map<String, Value>, SessionError> {
        let data = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
        let mut headers = vec![("User-Agent", self.user_agent.as_str())];
        if data.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        let req = Req {
            url,
            headers: &headers,
            body: data.as_deref(),
            timeout: Duration::from_secs_f64(TIMEOUT_S),
            max_body: MAX_BODY,
        };
        let resp = match net.send(&req) {
            Ok(r) => r,
            Err(e) if e.is_too_large() => return Err(refused("the map service answer was too large")),
            Err(_) => {
                return Err(SessionError {
                    message: "the map service could not be reached".into(),
                    offline: true,
                })
            }
        };
        if resp.status >= 400 {
            return Err(refused(&format!(
                "the map service refused the key (HTTP {})",
                resp.status
            )));
        }
        let text = std::str::from_utf8(&resp.body).map_err(|_| refused("the map service answer was not JSON"))?;
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(o)) => Ok(o),
            Ok(_) => Err(refused("the map service answer was not an object")),
            Err(_) => Err(refused("the map service answer was not JSON")),
        }
    }

    fn google_session(&self, net: &dyn Fetch, block: &Licensed, token: &str) -> Result<(String, f64), SessionError> {
        let mut body = json!({"mapType": block.map_type, "language": "en-US", "region": "US"});
        if !block.layer_types.is_empty() {
            body["layerTypes"] = json!(block.layer_types);
        }
        let url = ts::build_tile_url(&self.endpoints.google_session, 0, 0, 0, token);
        let answer = self.request_json(net, &url, Some(&body))?;
        let session = match answer.get("session") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => return Err(refused("the map service opened no session")),
        };
        let mut ttl = GOOGLE_DEFAULT_TTL_S;
        let expiry = answer.get("expiry").and_then(json_float).unwrap_or(0.0);
        if expiry > 0.0 {
            ttl = f64::max(0.0, f64::min(ttl, expiry - now_secs() as f64 - EXPIRY_MARGIN_S));
            if ttl <= 0.0 {
                return Err(refused("the map service opened an expired session"));
            }
        }
        let template = self.endpoints.google_tiles.replace("{session}", &quote(&session));
        Ok((template, ttl))
    }

    fn bing_metadata(&self, net: &dyn Fetch, block: &Licensed, token: &str) -> Result<(String, f64), SessionError> {
        let meta = self
            .endpoints
            .bing_metadata
            .replace("{imagery_set}", &quote(block.imagery_set));
        let url = ts::build_tile_url(&meta, 0, 0, 0, token);
        let answer = self.request_json(net, &url, None)?;
        let no_address = || refused("the map service sent no tile address");
        let resource = answer
            .get("resourceSets")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("resources"))
            .and_then(|v| v.get(0))
            .ok_or_else(no_address)?;
        let image_url = resource.get("imageUrl").ok_or_else(no_address)?;
        let subdomains = match resource.get("imageUrlSubdomains") {
            Some(v) if truthy(v) => v.clone(),
            _ => Value::Array(Vec::new()),
        };
        let (Value::String(image_url), Value::Array(subdomains)) = (image_url, &subdomains) else {
            return Err(no_address());
        };
        let standard: Vec<Value> = (0..4).map(|i| json!(format!("t{i}"))).collect();
        let subdomain = if *subdomains == standard {
            "t{s}".to_string()
        } else if let Some(Value::String(first)) = subdomains.first().filter(|v| v.is_string()) {
            if !first.is_empty() && first.chars().all(char::is_alphanumeric) {
                first.clone()
            } else if image_url.contains("{subdomain}") {
                return Err(refused("the map service sent no tile servers"));
            } else {
                String::new()
            }
        } else if image_url.contains("{subdomain}") {
            return Err(refused("the map service sent no tile servers"));
        } else {
            String::new()
        };
        let template = image_url
            .replace("{subdomain}", &subdomain)
            .replace("{quadkey}", "{q}")
            .replace("{culture}", "en-US");
        let probe = template.replace("{s}", "0").replace("{q}", "0");
        let (scheme, host) = split_scheme_host(&probe);
        if scheme != "https"
            || !BING_HOST_SUFFIXES.iter().any(|s| host.ends_with(s))
            || probe.contains('{')
            || probe.contains('}')
            || !template.contains("{q}")
        {
            return Err(refused("the map service sent a tile address Corvus will not use"));
        }
        Ok((template, BING_TTL_S))
    }
}

/// `urlsplit(url)`'s scheme and `.hostname`, lowercased.
fn split_scheme_host(url: &str) -> (String, String) {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let Some((scheme, rest)) = url.split_once(':') else {
        return (String::new(), String::new());
    };
    let valid = !scheme.is_empty()
        && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid {
        return (String::new(), String::new());
    }
    let scheme = scheme.to_ascii_lowercase();
    let Some(after) = rest.strip_prefix("//") else {
        return (scheme, String::new());
    };
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(stripped) = hostport.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    (scheme, host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_split_like_urlsplit() {
        assert_eq!(
            split_scheme_host("https://ecn.t0.tiles.virtualearth.net/tiles/a0.jpeg?g=1"),
            ("https".into(), "ecn.t0.tiles.virtualearth.net".into())
        );
        assert_eq!(
            split_scheme_host("HTTPS://User:pw@Host.Bing.com:8080/x"),
            ("https".into(), "host.bing.com".into())
        );
        assert_eq!(split_scheme_host("//host/x"), (String::new(), String::new()));
        assert_eq!(split_scheme_host("http:/x"), ("http".into(), String::new()));
        assert_eq!(
            split_scheme_host("https://evil.com#.bing.com"),
            ("https".into(), "evil.com".into())
        );
    }
}
