//! The update check against the Corvus GCS releases on GitHub
//! (`update_check.py` and the `/api/update` routes of `server.py`).
//!
//! Offline contract, because the same laptop runs the firmware catalogue:
//!
//! * Nothing here runs at startup. The check happens when the interface asks.
//! * Every answer is cached to `<state dir>/update.json`, so a launch with no
//!   internet reports the last known release and a repeated launch does not
//!   hammer GitHub: a fresh check is made at most every [`CHECK_INTERVAL_S`]
//!   (sooner after a failure, which knows nothing).
//! * A failed check is never an error the operator sees: it returns the cached
//!   release with a message the Settings page can show on request.
//!
//! Only the release list is trusted from the network. The page the dialog opens
//! is built from the tag, never read out of the payload or the cache file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::http_client;
use crate::config::{py_int, truthy};
use crate::core::Core;
use crate::json::Map;
use crate::module::Registry;
use crate::request::Request;
use crate::response::Response;

pub const REPO: &str = "M-BSquared/CorvusGCS";
pub const RELEASES_URL: &str = "https://api.github.com/repos/M-BSquared/CorvusGCS/releases?per_page=10";
pub const UPDATE_FILE: &str = "update.json";
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(6);
/// A release list is a few dozen KB; nobody is waiting, but a huge body must
/// not exhaust memory.
pub const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
/// How stale a successful check may be before the next request fetches again.
pub const CHECK_INTERVAL_S: f64 = 6.0 * 3600.0;
/// After a failure: often enough that plugging in a hotspot is noticed, rarely
/// enough that an offline session is not retrying on every page load.
pub const RETRY_INTERVAL_S: f64 = 900.0;
/// Release notes are shown in a dialog, not a document viewer.
pub const MAX_NOTES_CHARS: usize = 4000;

fn release_page_url(tag: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/{tag}")
}

const RELEASES_PAGE_URL: &str = "https://github.com/M-BSquared/CorvusGCS/releases";

/// `^v?(\d+(?:\.\d+)*)$` (a trailing newline passes, as with Python's `$`):
/// the digits groups of `text`, or `None`.
fn match_tag(text: &str) -> Option<Vec<u128>> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let digits = text.strip_prefix('v').unwrap_or(text);
    let mut out = Vec::new();
    for part in digits.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out.push(part.parse::<u128>().ok()?);
    }
    Some(out)
}

/// `2026.09.27` or `v2026.09.27` as a comparable list. `None` for anything that
/// is not a plain dotted number: a release named `nightly` or a `0.0.0-unknown`
/// fallback must never be compared, because a wrong comparison shows the
/// operator an update that does not exist.
pub fn parse_version(text: &str) -> Option<Vec<u128>> {
    match_tag(text.trim())
}

/// Whether `candidate` is strictly newer than `current`. Unparsable on either
/// side means no update; CalVer parts compare as integers, and a shorter list
/// sorts first (`2026.09` before `2026.09.01`).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The release page for `tag`, or the releases index for an odd tag.
pub fn release_page(tag: &str) -> String {
    if match_tag(tag).is_some() {
        release_page_url(tag)
    } else {
        RELEASES_PAGE_URL.to_string()
    }
}

/// `str(value)` for the JSON values a release payload can hold.
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

/// `str(v or "")`.
fn py_str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(x) if truthy(x) => py_str(Some(x)),
        _ => String::new(),
    }
}

/// The newest stable release of a GitHub release payload. The payload is
/// untrusted: every field is coerced, drafts and prereleases are dropped, and
/// the winner is chosen by comparing parsed versions, not by the order GitHub
/// returned.
pub fn pick_latest(payload: &Value) -> Option<Map> {
    let items = payload.as_array()?;
    let mut best: Option<Map> = None;
    let mut best_version: Option<Vec<u128>> = None;
    for item in items {
        let Some(item) = item.as_object() else { continue };
        if item.get("draft").is_some_and(truthy) || item.get("prerelease").is_some_and(truthy) {
            continue;
        }
        let tag = py_str_or_empty(item.get("tag_name"));
        let Some(version) = parse_version(&tag) else {
            continue;
        };
        if best_version.as_ref().is_some_and(|b| version <= *b) {
            continue;
        }
        let notes = py_str_or_empty(item.get("body")).trim().to_string();
        let mut assets = Vec::new();
        if let Some(Value::Array(list)) = item.get("assets") {
            for asset in list.iter().filter_map(Value::as_object) {
                let size = asset
                    .get("size")
                    .filter(|s| truthy(s))
                    .and_then(|s| match s {
                        Value::Bool(b) => Some(i64::from(*b)),
                        other => py_int(other),
                    })
                    .unwrap_or(0);
                assets.push(json!({"name": py_str_or_empty(asset.get("name")), "size": size}));
            }
        }
        let mut m = Map::new();
        m.insert("tag".into(), tag.clone().into());
        // The tag's own text minus the "v", so the zero padding CalVer writes
        // survives: the dismissed version match is a string compare.
        m.insert("version".into(), tag.trim_start_matches('v').into());
        let name = py_str_or_empty(item.get("name"));
        m.insert("name".into(), if name.is_empty() { tag.clone() } else { name }.into());
        m.insert("published".into(), py_str_or_empty(item.get("published_at")).into());
        m.insert(
            "notes".into(),
            notes.chars().take(MAX_NOTES_CHARS).collect::<String>().into(),
        );
        m.insert("assets".into(), Value::Array(assets));
        m.insert("url".into(), release_page(&tag).into());
        best_version = Some(version);
        best = Some(m);
    }
    best
}

/// Python's `json.dump(value, f)`: `, ` and `: ` between items and every
/// character outside ASCII as `\uXXXX`.
pub fn dump_py(value: &Value) -> String {
    fn ascii(s: &str) -> String {
        let quoted = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
        let mut out = String::with_capacity(quoted.len());
        for c in quoted.chars() {
            if c.is_ascii() {
                out.push(c);
            } else {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
        }
        out
    }
    match value {
        Value::Object(m) => {
            let items: Vec<String> = m.iter().map(|(k, v)| format!("{}: {}", ascii(k), dump_py(v))).collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(dump_py).collect::<Vec<_>>().join(", ")),
        Value::String(s) => ascii(s),
        other => other.to_string(),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// The cached "is there a newer release?" answer, backed by one JSON file.
/// Thread safe: request threads share one, and a check already under way is not
/// started a second time.
pub struct UpdateChecker {
    dir: PathBuf,
    url: String,
    lock: Mutex<()>,
}

impl UpdateChecker {
    pub fn new(state_dir: impl Into<PathBuf>) -> UpdateChecker {
        UpdateChecker::with_url(state_dir, RELEASES_URL)
    }

    /// Reads the release list from `url` instead of GitHub: a stub in a test.
    pub fn with_url(state_dir: impl Into<PathBuf>, url: &str) -> UpdateChecker {
        UpdateChecker {
            dir: state_dir.into(),
            url: url.to_string(),
            lock: Mutex::new(()),
        }
    }

    fn cache_path(&self) -> PathBuf {
        self.dir.join(UPDATE_FILE)
    }

    /// The last stored result; empty when there is no usable cache.
    pub fn load_cached(&self) -> Map {
        match std::fs::read_to_string(self.cache_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    fn store_cached(&self, data: &Map) {
        let path = self.cache_path();
        let result = (|| {
            std::fs::create_dir_all(&self.dir)?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, dump_py(&Value::Object(data.clone())))?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = result {
            eprintln!("corvus: could not cache the update check: {e}");
        }
    }

    /// Fetches the release list: `(latest release, error)`. Never fails: in the
    /// field this call fails on every launch, and that is a normal state.
    pub fn fetch(&self) -> (Option<Map>, String) {
        let ua = format!("CorvusGCS/{}", crate::version::version());
        let reply = match http_client::get(
            &self.url,
            &ua,
            "application/vnd.github+json",
            FETCH_TIMEOUT,
            MAX_PAYLOAD_BYTES,
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("corvus: update check failed: {e}");
                return (None, format!("could not reach the release server ({e})"));
            }
        };
        if reply.status >= 400 {
            let why = format!("HTTP Error {}: {}", reply.status, reason(reply.status));
            eprintln!("corvus: update check failed: {why}");
            return (None, format!("could not reach the release server ({why})"));
        }
        if reply.truncated || reply.body.len() > MAX_PAYLOAD_BYTES {
            return (None, "the release list was unexpectedly large".into());
        }
        match serde_json::from_slice::<Value>(&reply.body) {
            Ok(payload) => (pick_latest(&payload), String::new()),
            Err(e) => {
                eprintln!("corvus: update check failed: {e}");
                (None, format!("could not reach the release server ({e})"))
            }
        }
    }

    fn is_stale(cached: &Map, now: f64) -> bool {
        let Some(checked_at) = cached.get("checked_at").and_then(Value::as_f64) else {
            return true;
        };
        // A clock that moved backwards (or a cache copied from another machine)
        // must not pin the check as fresh forever.
        if checked_at > now {
            return true;
        }
        let interval = if cached.get("latest").is_some_and(truthy) {
            CHECK_INTERVAL_S
        } else {
            RETRY_INTERVAL_S
        };
        now - checked_at >= interval
    }

    /// The update status as the API serves it. `force` always goes to the network
    /// (the Settings page's "Check now"); otherwise the cached answer is used
    /// until it goes stale, so opening the app repeatedly costs nothing and
    /// works offline.
    pub fn check(&self, force: bool) -> Map {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.check_at(force, now)
    }

    pub fn check_at(&self, force: bool, now: f64) -> Map {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let cached = self.load_cached();
        let mut error = py_str_or_empty(cached.get("error"));
        let mut latest = cached.get("latest").filter(|v| v.is_object()).cloned();
        let mut checked_at = cached.get("checked_at").cloned();
        if force || Self::is_stale(&cached, now) {
            let (fetched, fetch_error) = self.fetch();
            error = fetch_error;
            if let Some(f) = fetched {
                latest = Some(Value::Object(f));
            }
            checked_at = Some(json!(now));
            // The failed attempt is stored too, so an offline session backs off
            // instead of trying the timeout on every page load.
            let mut stored = Map::new();
            stored.insert("checked_at".into(), json!(now));
            stored.insert("latest".into(), latest.clone().unwrap_or(Value::Null));
            stored.insert("error".into(), error.clone().into());
            self.store_cached(&stored);
        }
        status(latest.as_ref().and_then(Value::as_object), &error, checked_at.as_ref())
    }

    /// The stored answer with no network access at all.
    pub fn cached_status(&self) -> Map {
        let cached = self.load_cached();
        status(
            cached.get("latest").and_then(Value::as_object),
            &py_str_or_empty(cached.get("error")),
            cached.get("checked_at"),
        )
    }
}

fn status(latest: Option<&Map>, error: &str, checked_at: Option<&Value>) -> Map {
    let current = crate::version::version();
    let get = |k: &str| latest.and_then(|l| l.get(k)).cloned();
    let text = |k: &str| get(k).unwrap_or_else(|| "".into());
    let version = get("version").map(|v| py_str(Some(&v))).unwrap_or_default();
    let mut out = Map::new();
    out.insert("current".into(), current.into());
    out.insert("latest".into(), text("version"));
    out.insert("update_available".into(), is_newer(&version, current).into());
    out.insert("name".into(), text("name"));
    // Rebuilt from the tag on the way out, never read back from the stored
    // object: the cache is an ordinary file, and anything that can write there
    // could otherwise put its own URL in front of POST /api/update/open.
    out.insert("url".into(), release_page(&py_str_or_empty(get("tag").as_ref())).into());
    out.insert("published".into(), text("published"));
    out.insert("notes".into(), text("notes"));
    out.insert("assets".into(), get("assets").unwrap_or_else(|| json!([])));
    out.insert(
        "checked_at".into(),
        match checked_at {
            Some(v @ Value::Number(_)) => v.clone(),
            _ => json!(0),
        },
    );
    out.insert("error".into(), error.into());
    out
}

// ---------------------------------------------------------------- routes

/// What the `/api/update` routes use: the checker, and what opens a web page.
/// What opens a web page: true when a launcher was started.
pub type BrowserOpener = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct UpdateService {
    pub checker: UpdateChecker,
    pub browser: BrowserOpener,
}

fn service(core: &Core) -> Option<Arc<UpdateService>> {
    core.service::<UpdateService>()
}

pub(super) fn register(reg: &mut Registry) {
    reg.get("/api/update", api_update);
    reg.post("/api/update/skip", api_update_skip);
    reg.post("/api/update/open", api_update_open);
}

fn updates_block(core: &Core) -> Map {
    core.settings().get().block("updates").cloned().unwrap_or_default()
}

fn api_update(core: &Core, req: &Request) -> Response {
    let block = updates_block(core);
    let enabled = block.get("check") != Some(&Value::Bool(false));
    let skipped = py_str_or_empty(block.get("skipped"));
    let Some(svc) = service(core) else {
        return Response::error(503, "update check unavailable");
    };
    let refresh = {
        let v = req
            .query
            .get("refresh")
            .filter(|v| !v.is_empty())
            .unwrap_or("0")
            .to_lowercase();
        matches!(v.as_str(), "1" | "true" | "yes")
    };
    let mut data = if enabled {
        svc.checker.check(refresh)
    } else {
        // Off means off: report what is already on disk, reach for nothing.
        let mut d = svc.checker.cached_status();
        d.insert("update_available".into(), false.into());
        d
    };
    data.insert("enabled".into(), enabled.into());
    data.insert("skipped".into(), skipped.into());
    Response::json(Value::Object(data))
}

fn api_update_skip(core: &Core, req: &Request) -> Response {
    let version = match req.payload().get("version") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    };
    let partial = json!({"updates": {"skipped": version}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let public = match core.settings().apply_partial(core, &partial) {
        Ok(p) => p,
        Err(e) => return Response::not_ok(400, &e),
    };
    let stored = py_str_or_empty(updates_block(core).get("skipped"));
    Response::json(json!({"ok": true, "skipped": stored, "config": public}))
}

fn api_update_open(core: &Core, _req: &Request) -> Response {
    let Some(svc) = service(core) else {
        return Response::not_ok(503, "update check unavailable");
    };
    let url = py_str(svc.checker.cached_status().get("url"));
    if url.is_empty() {
        return Response::not_ok(404, "no release page known");
    }
    // The URL is re-derived from the cache and never read from the request, and
    // its scheme is checked again here: whatever reaches the opener is about to
    // be handed to a program launcher on every desktop.
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        eprintln!("corvus: refusing to open a non web release URL: {url}");
        return Response::not_ok(404, "no release page known");
    }
    if (svc.browser)(&url) {
        Response::json(json!({"ok": true, "url": url}))
    } else {
        Response::json(json!({"ok": false, "error": "no browser available", "url": url}))
    }
}

/// The service the routes use, for the state folder `dir`.
pub(super) fn make_service(dir: &Path, url: Option<&str>, browser: Option<BrowserOpener>) -> UpdateService {
    UpdateService {
        checker: match url {
            Some(u) => UpdateChecker::with_url(dir, u),
            None => UpdateChecker::new(dir),
        },
        browser: browser.unwrap_or_else(|| Arc::new(super::file_manager::open_url)),
    }
}
