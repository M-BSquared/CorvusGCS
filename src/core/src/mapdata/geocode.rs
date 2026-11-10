//! Place search for the map over OSM Nominatim (`corvus/geocode.py`).
//!
//! Nominatim's usage policy is enforced here, not trusted to the caller: at
//! most one request a second (with a margin) and a real User-Agent. A failure
//! is a sentence, never a 500, and puts the service in a short back off so an
//! offline operator does not spend the timeout on every keystroke. Answers are
//! cached in memory. Only `lat`, `lon` and `boundingbox` are trusted out of the
//! payload, each parsed and range checked.
//!
//! Holds no thread, socket or file handle between calls.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::fetch::{Fetch, Req};
use super::util::{finite, quote_plus, round_half_even};

pub const SEARCH_URL: &str = "https://nominatim.openstreetmap.org/search";
pub const MIN_INTERVAL_S: f64 = 1.1;
pub const MAX_WAIT_S: f64 = 3.0;
pub const FETCH_TIMEOUT_S: f64 = 6.0;
pub const MAX_RESULTS: i64 = 8;
pub const DEFAULT_RESULTS: i64 = 6;
pub const MAX_PAYLOAD_BYTES: usize = 512 * 1024;
pub const OFFLINE_BACKOFF_S: f64 = 20.0;
pub const CACHE_MAX: usize = 64;
pub const VIEWBOX_DEG: f64 = 2.0;
pub const CACHE_CELL_DEG: f64 = 1.0;

/// The search could not be run. The message is shown to the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeocodeError(pub String);

impl std::fmt::Display for GeocodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A map centre used to bias a search. Longitude first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Near {
    pub lon: f64,
    pub lat: f64,
}

fn coordinate(lat: Option<&Value>, lon: Option<&Value>) -> Option<(f64, f64)> {
    let (latitude, longitude) = (finite(lat?)?, finite(lon?)?);
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return None;
    }
    Some((latitude, longitude))
}

fn bounds(raw: Option<&Value>) -> Value {
    let Some(Value::Array(a)) = raw else {
        return Value::Null;
    };
    if a.len() != 4 {
        return Value::Null;
    }
    let v: Vec<Option<f64>> = a.iter().map(finite).collect();
    let (Some(south), Some(north), Some(west), Some(east)) = (v[0], v[1], v[2], v[3]) else {
        return Value::Null;
    };
    if !(-90.0 <= south && south <= north && north <= 90.0) {
        return Value::Null;
    }
    if !(-180.0..=180.0).contains(&west) || !(-180.0..=180.0).contains(&east) {
        return Value::Null;
    }
    if east < west {
        return Value::Null;
    }
    json!({"w": west, "s": south, "e": east, "n": north})
}

fn label(entry: &Map<String, Value>) -> String {
    let text = ["display_name", "name"]
        .iter()
        .filter_map(|k| entry.get(*k))
        .find(|v| truthy(v));
    let Some(Value::String(text)) = text else {
        return String::new();
    };
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(200).collect()
}

/// Python truthiness of a JSON value (`a or b`).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn kind(entry: &Map<String, Value>) -> String {
    for key in ["type", "category", "class"] {
        if let Some(Value::String(s)) = entry.get(key) {
            let t = s.trim();
            if !t.is_empty() {
                return t.replace('_', " ").chars().take(40).collect();
            }
        }
    }
    String::new()
}

/// Upstream JSON to the rows the page draws. Never fails.
pub fn parse_results(payload: &Value, limit: usize) -> Vec<Value> {
    let Value::Array(entries) = payload else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        let Value::Object(entry) = entry else { continue };
        let point = coordinate(entry.get("lat"), entry.get("lon"));
        let label = label(entry);
        let Some((lat, lon)) = point else { continue };
        if label.is_empty() {
            continue;
        }
        out.push(json!({
            "label": label,
            "lat": lat,
            "lon": lon,
            "bounds": bounds(entry.get("boundingbox")),
            "kind": kind(entry),
        }));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// The bias bucket a `near` falls in, for the cache key.
pub fn cache_cell(near: Option<Near>) -> Option<(i64, i64)> {
    let c = near?;
    Some((
        round_half_even(c.lon / CACHE_CELL_DEG),
        round_half_even(c.lat / CACHE_CELL_DEG),
    ))
}

/// The upstream query string.
pub fn build_query(query: &str, limit: i64, near: Option<Near>) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("q", query.to_string()),
        ("format", "jsonv2".into()),
        ("limit", limit.to_string()),
        ("addressdetails", "0".into()),
    ];
    if let Some(c) = near {
        let west = (c.lon - VIEWBOX_DEG).max(-180.0);
        let east = (c.lon + VIEWBOX_DEG).min(180.0);
        let north = (c.lat + VIEWBOX_DEG).min(90.0);
        let south = (c.lat - VIEWBOX_DEG).max(-90.0);
        params.push(("viewbox", format!("{west:.5},{north:.5},{east:.5},{south:.5}")));
        params.push(("bounded", "0".into()));
    }
    params
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The rate and cache numbers, so a test can shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub min_interval: f64,
    pub max_wait: f64,
    pub offline_backoff: f64,
    pub timeout: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            min_interval: MIN_INTERVAL_S,
            max_wait: MAX_WAIT_S,
            offline_backoff: OFFLINE_BACKOFF_S,
            timeout: FETCH_TIMEOUT_S,
        }
    }
}

type Key = (String, i64, Option<(i64, i64)>);

#[derive(Default)]
struct State {
    next_at: f64,
    offline_until: f64,
    cache: Vec<(Key, Vec<Value>)>,
}

/// One place search service for the process.
pub struct Geocoder {
    url: String,
    user_agent: String,
    limits: Limits,
    epoch: Instant,
    state: Mutex<State>,
}

impl Geocoder {
    pub fn new(user_agent: &str) -> Geocoder {
        Geocoder::with(user_agent, SEARCH_URL, Limits::default())
    }

    pub fn with(user_agent: &str, url: &str, limits: Limits) -> Geocoder {
        Geocoder {
            url: url.to_string(),
            user_agent: user_agent.to_string(),
            limits,
            epoch: Instant::now(),
            state: Mutex::new(State::default()),
        }
    }

    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claims the next slot; a wait past the ceiling claims nothing.
    fn reserve(&self) -> f64 {
        let mut st = self.state();
        let now = self.now();
        let wait = (st.next_at - now).max(0.0);
        if wait > self.limits.max_wait {
            return wait;
        }
        st.next_at = now.max(st.next_at) + self.limits.min_interval;
        wait
    }

    fn offline(&self) -> bool {
        self.now() < self.state().offline_until
    }

    fn back_off(&self, seconds: f64) {
        let until = self.now() + seconds;
        self.state().offline_until = until;
    }

    fn clear_back_off(&self) {
        self.state().offline_until = 0.0;
    }

    /// Places matching `query`, nearest `near` first where it is given. `wait`
    /// sleeps for the rate limit and returns true when the program is
    /// stopping, which ends the search. An empty list means the search ran and
    /// found nothing; an error means it could not run.
    pub fn search(
        &self,
        net: &dyn Fetch,
        query: &str,
        limit: Option<i64>,
        near: Option<Near>,
        wait: &dyn Fn(Duration) -> bool,
    ) -> Result<Vec<Value>, GeocodeError> {
        let text = query.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let limit = match limit {
            Some(n) if n != 0 => n,
            _ => DEFAULT_RESULTS,
        }
        .clamp(1, MAX_RESULTS);
        let key: Key = (text.to_lowercase(), limit, cache_cell(near));
        if let Some((_, rows)) = self.state().cache.iter().find(|(k, _)| *k == key) {
            return Ok(rows.clone());
        }
        if self.offline() {
            return Err(GeocodeError("No connection to the place-search service.".into()));
        }
        let w = self.reserve();
        if w > self.limits.max_wait {
            return Err(GeocodeError("Place search is busy. Try that again.".into()));
        }
        if w > 0.0 && wait(Duration::from_secs_f64(w)) {
            return Err(GeocodeError("No connection to the place-search service.".into()));
        }
        let payload = self.fetch(net, &build_query(&text, limit, near))?;
        let rows = parse_results(&payload, limit as usize);
        let mut st = self.state();
        if st.cache.len() >= CACHE_MAX {
            st.cache.remove(0);
        }
        st.cache.retain(|(k, _)| *k != key);
        st.cache.push((key, rows.clone()));
        Ok(rows)
    }

    fn fetch(&self, net: &dyn Fetch, query_string: &str) -> Result<Value, GeocodeError> {
        let url = format!("{}?{query_string}", self.url);
        let headers = [("User-Agent", self.user_agent.as_str()), ("Accept", "application/json")];
        let req = Req {
            url: &url,
            headers: &headers,
            body: None,
            timeout: Duration::from_secs_f64(self.limits.timeout),
            max_body: MAX_PAYLOAD_BYTES,
        };
        let unusable = || GeocodeError("The place-search service sent an unusable answer.".into());
        let resp = match net.send(&req) {
            Ok(r) => r,
            Err(e) => {
                let too_big = e.0.contains("larger than the cap");
                self.back_off(self.limits.offline_backoff);
                return Err(if too_big {
                    unusable()
                } else {
                    GeocodeError("No connection to the place-search service.".into())
                });
            }
        };
        if resp.status >= 400 {
            let seconds = if matches!(resp.status, 429 | 503) {
                60.0
            } else {
                self.limits.offline_backoff
            };
            self.back_off(seconds);
            return Err(GeocodeError("The place-search service refused the request.".into()));
        }
        if resp.body.is_empty() {
            self.back_off(self.limits.offline_backoff);
            return Err(unusable());
        }
        let text = String::from_utf8_lossy(&resp.body);
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => {
                self.clear_back_off();
                Ok(v)
            }
            Err(_) => {
                self.back_off(self.limits.offline_backoff);
                Err(unusable())
            }
        }
    }
}
