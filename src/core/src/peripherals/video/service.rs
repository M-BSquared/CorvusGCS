//! The configured cameras: RTSP decoders and WebRTC signalling sessions
//! (`VideoService` in `video.py`). Thread-safe: the route threads call every
//! method at once.
//!
//! Lifecycle: `shutdown` terminates every ffmpeg (SIGTERM, then SIGKILL after
//! the term timeout), joins every thread, closes every WHEP session and
//! removes the private temp folder. It is called from the module's `stop`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use super::ffmpeg;
use super::model::*;
use super::stream::{Decoder, Task, Timing, VideoStream};
use super::url::{self, Split};

pub const MAX_SDP_BYTES: usize = 256 * 1024;
pub const MAX_WHEP_SESSIONS: usize = 16;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub type Resolver = Arc<dyn Fn(&str) -> (String, String) + Send + Sync>;
pub type VersionOf = Arc<dyn Fn(&str) -> String + Send + Sync>;

#[derive(Default)]
struct EnvState {
    configured: String,
    ffmpeg: String,
    source: String,
    version: String,
    resolved_at: Option<Instant>,
    tmpdir: Option<PathBuf>,
}

/// What the service and its streams share: where ffmpeg is and the private
/// folder.
struct Env {
    timing: Timing,
    resolver: Resolver,
    version_of: VersionOf,
    st: Mutex<EnvState>,
}

impl Env {
    fn resolve(&self, force: bool) -> String {
        let configured = {
            let mut st = lock(&self.st);
            let stale = st
                .resolved_at
                .is_none_or(|t| st.ffmpeg.is_empty() && t.elapsed() > self.timing.resolve_retry);
            if !(force || stale) {
                return st.ffmpeg.clone();
            }
            st.resolved_at = Some(Instant::now());
            st.configured.clone()
        };
        let (path, source) = (self.resolver)(&configured);
        let version = if path.is_empty() {
            String::new()
        } else {
            (self.version_of)(&path)
        };
        let mut st = lock(&self.st);
        st.ffmpeg = path.clone();
        st.source = source;
        st.version = version;
        path
    }
}

impl Decoder for Env {
    fn decoder(&self) -> (String, String) {
        let path = self.resolve(false);
        (path, lock(&self.st).version.clone())
    }

    fn workdir(&self) -> std::io::Result<PathBuf> {
        let mut st = lock(&self.st);
        if let Some(d) = &st.tmpdir {
            if d.is_dir() {
                return Ok(d.clone());
            }
        }
        let mut rnd = [0u8; 6];
        let _ = getrandom::fill(&mut rnd);
        let name = format!(
            "corvus-video-{}-{}",
            std::process::id(),
            rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        st.tmpdir = Some(dir.clone());
        Ok(dir)
    }

    fn timing(&self) -> &Timing {
        &self.timing
    }
}

struct Session {
    stream: String,
    config: Value,
    location: String,
    created: Instant,
}

struct Svc {
    settings: Value,
    configs: IndexMap<String, Value>,
    streams: HashMap<String, Arc<VideoStream>>,
    retired: Vec<Arc<VideoStream>>,
    sessions: HashMap<String, Session>,
    closed: bool,
}

pub enum FrameError {
    NoSuchCamera,
    WrongKind(String),
}

pub enum Frame {
    Jpeg { seq: u64, data: Arc<Vec<u8>> },
    State { state: String, message: String, seq: u64 },
}

pub enum WhepResult {
    Ok { sdp: String, session: String },
    Failed { status: u16, error: String },
    NoSuchCamera,
    WrongKind(String),
}

pub struct VideoService {
    env: Arc<Env>,
    svc: Mutex<Svc>,
}

fn same_source(a: &Value, b: &Value) -> bool {
    ["kind", "url", "username", "password", "transport"]
        .iter()
        .all(|k| a.get(*k) == b.get(*k))
}

fn kind_of(cfg: &Value) -> &str {
    cfg.get("kind").and_then(Value::as_str).unwrap_or(KIND_RTSP)
}

/// `Authorization` for a WHEP server: Basic with a user, Bearer with a token alone.
pub fn auth_header(stream: &Value) -> String {
    let user = stream.get("username").and_then(Value::as_str).unwrap_or("");
    let password = stream.get("password").and_then(Value::as_str).unwrap_or("");
    if !user.is_empty() {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        return format!("Basic {token}");
    }
    if password.is_empty() {
        String::new()
    } else {
        format!("Bearer {password}")
    }
}

/// `{"error": "..."}` as MediaMTX and go2rtc answer errors, else the text.
pub fn whep_error(code: u16, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = crate::peripherals::util::py_strip(&text);
    let detail = match json_error_text(text) {
        Some(d) => d,
        None => text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(200)
            .collect(),
    };
    let lead = match code {
        401 | 403 => "The camera server refused the user or password.".to_string(),
        404 => "The camera server has no stream at that address.".into(),
        301 | 302 | 303 | 307 | 308 => "The camera server redirected the request, which Corvus does not follow.".into(),
        _ => format!("The camera server refused the connection ({code})."),
    };
    if detail.is_empty() {
        lead
    } else {
        format!("{lead} ({detail})")
    }
}

/// `"error"\s*:\s*"([^"]{1,300})"`, first match.
fn json_error_text(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = text[from..].find("\"error\"") {
        let after = &text[from + i + 7..];
        let after_ws = after.trim_start();
        if let Some(rest) = after_ws.strip_prefix(':') {
            if let Some(rest) = rest.trim_start().strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    let n = rest[..end].chars().count();
                    if (1..=300).contains(&n) {
                        return Some(rest[..end].to_string());
                    }
                }
            }
        }
        from += i + 7;
    }
    None
}

/// `urljoin(base, reference)` for the http addresses a WHEP server answers with.
pub fn urljoin(base: &str, reference: &str) -> String {
    if reference.is_empty() {
        return base.to_string();
    }
    let (Ok(b), Ok(r)) = (url::urlsplit(base), url::urlsplit(reference)) else {
        return reference.to_string();
    };
    if !r.scheme.is_empty() && r.scheme != b.scheme {
        return reference.to_string();
    }
    if !r.netloc.is_empty() {
        return url::urlunsplit(&Split { scheme: b.scheme, ..r });
    }
    let (path, query) = if r.path.is_empty() {
        (
            b.path.clone(),
            if r.query.is_empty() {
                b.query.clone()
            } else {
                r.query.clone()
            },
        )
    } else if r.path.starts_with('/') {
        (remove_dots(&r.path), r.query.clone())
    } else {
        let dir = b.path.rsplit_once('/').map_or("", |(d, _)| d);
        (remove_dots(&format!("{dir}/{}", r.path)), r.query.clone())
    };
    url::urlunsplit(&Split {
        scheme: b.scheme,
        netloc: b.netloc,
        path,
        query,
        fragment: r.fragment,
    })
}

fn remove_dots(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    for (i, seg) in parts.iter().enumerate() {
        match *seg {
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if i == last {
                    out.push("");
                }
            }
            "." => {
                if i == last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

enum Wire {
    Status(u16, Vec<(String, String)>, Vec<u8>),
    Down(String),
}

fn wire_error_text(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "timed out".into(),
        ureq::Error::Io(io) => crate::peripherals::util::py_oserror_text(io),
        ureq::Error::HostNotFound => "[Errno 8] nodename nor servname provided, or not known".into(),
        other => other.to_string(),
    }
}

/// One request to a WHEP server. No redirect is followed; credentials go only
/// where the operator sent them.
fn whep_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout: Duration,
    limit: usize,
) -> Wire {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut headers_all: Vec<(&str, &str)> = Vec::new();
    for (k, v) in headers {
        headers_all.push((k.as_str(), v.as_str()));
    }
    let result = match (method, body) {
        ("POST", Some(b)) => {
            let mut r = agent.post(url);
            for (k, v) in &headers_all {
                r = r.header(*k, *v);
            }
            r.send(b)
        }
        _ => {
            let mut r = agent.delete(url);
            for (k, v) in &headers_all {
                r = r.header(*k, *v);
            }
            r.call()
        }
    };
    let mut resp = match result {
        Ok(r) => r,
        Err(e) => return Wire::Down(wire_error_text(&e)),
    };
    let status = resp.status().as_u16();
    let heads = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp
        .body_mut()
        .with_config()
        .limit(limit as u64)
        .read_to_vec()
        .unwrap_or_default();
    Wire::Status(status, heads, body)
}

impl VideoService {
    pub fn new(config: &Value) -> Arc<VideoService> {
        VideoService::with(
            config,
            Timing::default(),
            Arc::new(ffmpeg::resolve_ffmpeg),
            Arc::new(ffmpeg::ffmpeg_version),
        )
    }

    pub fn with(config: &Value, timing: Timing, resolver: Resolver, version_of: VersionOf) -> Arc<VideoService> {
        let settings = settings(config);
        let env = Arc::new(Env {
            timing,
            resolver,
            version_of,
            st: Mutex::new(EnvState {
                configured: settings["ffmpeg"].as_str().unwrap_or("").to_string(),
                ..EnvState::default()
            }),
        });
        let svc = Svc {
            settings: Value::Null,
            configs: IndexMap::new(),
            streams: HashMap::new(),
            retired: Vec::new(),
            sessions: HashMap::new(),
            closed: false,
        };
        let service = Arc::new(VideoService {
            env,
            svc: Mutex::new(svc),
        });
        {
            let mut g = lock(&service.svc);
            service.rebuild(&mut g, settings);
        }
        service
    }

    pub fn timing(&self) -> &Timing {
        &self.env.timing
    }

    /// The ffmpeg in use, or `""`.
    pub fn ffmpeg_path(&self) -> String {
        self.env.resolve(false)
    }

    fn new_stream(&self, cfg: &Value) -> Arc<VideoStream> {
        VideoStream::new(cfg.clone(), Arc::clone(&self.env) as Arc<dyn Decoder>)
    }

    /// Swaps in `new`; returns the decoders to stop and the WHEP sessions to close.
    fn rebuild(&self, g: &mut Svc, new: Value) -> (Vec<Arc<VideoStream>>, Vec<String>) {
        let mut retired = Vec::new();
        let mut keep: HashMap<String, Arc<VideoStream>> = HashMap::new();
        let mut configs: IndexMap<String, Value> = IndexMap::new();
        let list = new["streams"].as_array().cloned().unwrap_or_default();
        for cfg in &list {
            configs.insert(cfg["id"].as_str().unwrap_or("").to_string(), cfg.clone());
        }
        for cfg in &list {
            if kind_of(cfg) != KIND_RTSP {
                continue;
            }
            let id = cfg["id"].as_str().unwrap_or("").to_string();
            if let Some(old) = g.streams.get(&id) {
                if same_source(&old.config(), cfg) {
                    old.set_config(cfg.clone());
                    keep.insert(id, Arc::clone(old));
                    continue;
                }
                retired.push(Arc::clone(old));
            }
            keep.insert(id, self.new_stream(cfg));
        }
        for (id, s) in &g.streams {
            if !keep.contains_key(id) {
                retired.push(Arc::clone(s));
            }
        }
        let stale = g
            .sessions
            .iter()
            .filter(|(_, s)| configs.get(&s.stream).is_none_or(|c| !same_source(&s.config, c)))
            .map(|(t, _)| t.clone())
            .collect();
        g.streams = keep;
        g.configs = configs;
        g.settings = new;
        (retired, stale)
    }

    /// Replaces the camera list and the ffmpeg path. A camera whose address,
    /// credentials or transport did not change keeps its running decoder.
    pub fn apply_settings(&self, raw: &Value) {
        let new = settings(raw);
        let (mut retired, closing, changed) = {
            let mut g = lock(&self.svc);
            if g.closed {
                return;
            }
            let changed = new["ffmpeg"] != g.settings["ffmpeg"];
            let (mut retired, stale) = self.rebuild(&mut g, new.clone());
            if changed {
                retired.extend(g.streams.values().cloned());
                let fresh: HashMap<String, Arc<VideoStream>> = g
                    .streams
                    .iter()
                    .map(|(id, s)| (id.clone(), self.new_stream(&s.config())))
                    .collect();
                g.streams = fresh;
            }
            let closing: Vec<Session> = stale.iter().filter_map(|t| g.sessions.remove(t)).collect();
            (retired, closing, changed)
        };
        if changed {
            lock(&self.env.st).configured = new["ffmpeg"].as_str().unwrap_or("").to_string();
        }
        for s in &retired {
            s.stop(false);
        }
        {
            let mut g = lock(&self.svc);
            g.retired.retain(|s| s.alive());
            g.retired.append(&mut retired);
        }
        self.close_sessions(closing, self.env.timing.whep_close_timeout);
        if changed {
            self.env.resolve(true);
        }
    }

    /// The stored settings, passwords included. Never send this to a browser.
    pub fn settings(&self) -> Value {
        lock(&self.svc).settings.clone()
    }

    pub fn frame(
        &self,
        id: &str,
        after: u64,
        timeout: Duration,
        stopping: &dyn Fn() -> bool,
    ) -> Result<Frame, FrameError> {
        let (stream, closed) = {
            let g = lock(&self.svc);
            if !g.configs.contains_key(id) {
                return Err(FrameError::NoSuchCamera);
            }
            (g.streams.get(id).cloned(), g.closed)
        };
        let Some(stream) = stream else {
            return Err(FrameError::WrongKind("This camera plays over WebRTC.".into()));
        };
        if closed {
            return Ok(Frame::State {
                state: STATE_IDLE.into(),
                message: "Corvus is shutting down.".into(),
                seq: 0,
            });
        }
        if self.env.resolve(false).is_empty() {
            return Ok(Frame::State {
                state: STATE_UNAVAILABLE.into(),
                message: "ffmpeg was not found. Video needs it to decode the camera.".into(),
                seq: 0,
            });
        }
        stream.demand();
        if let Some((seq, data)) = stream.wait_frame(after, timeout, stopping) {
            return Ok(Frame::Jpeg { seq, data });
        }
        let st = stream.frame_status();
        Ok(Frame::State {
            state: st.state.into(),
            message: st.message,
            seq: st.seq,
        })
    }

    /// Hands the window's SDP offer to the camera's WHEP server and returns its
    /// answer. The password stays here; the window gets an opaque token.
    pub fn whep_offer(&self, id: &str, sdp: &str) -> WhepResult {
        let (cfg, closed) = {
            let g = lock(&self.svc);
            (g.configs.get(id).cloned(), g.closed)
        };
        let Some(cfg) = cfg else {
            return WhepResult::NoSuchCamera;
        };
        if kind_of(&cfg) != KIND_WEBRTC {
            return WhepResult::WrongKind("This camera is decoded by ffmpeg, not played over WebRTC.".into());
        }
        let fail = |status: u16, error: String| WhepResult::Failed { status, error };
        if closed {
            return fail(503, "Corvus is shutting down.".into());
        }
        if !sdp.starts_with("v=0") || sdp.len() > MAX_SDP_BYTES {
            return fail(400, "That is not an SDP offer.".into());
        }
        let mut headers = vec![
            ("Content-Type".to_string(), "application/sdp".to_string()),
            ("Accept".to_string(), "application/sdp".to_string()),
        ];
        let auth = auth_header(&cfg);
        if !auth.is_empty() {
            headers.push(("Authorization".into(), auth));
        }
        let url = cfg["url"].as_str().unwrap_or("").to_string();
        let password = cfg["password"].as_str().unwrap_or("").to_string();
        let wire = whep_request(
            "POST",
            &url,
            &headers,
            Some(sdp.as_bytes()),
            self.env.timing.whep_timeout,
            MAX_SDP_BYTES + 1,
        );
        let (code, heads, body) = match wire {
            Wire::Down(reason) => {
                return fail(
                    502,
                    explain_error(&format!("The camera server did not answer: {reason}")),
                );
            }
            Wire::Status(code, heads, body) if code >= 300 => {
                let _ = heads;
                let cut: Vec<u8> = body.into_iter().take(4096).collect();
                return fail(502, redact(&whep_error(code, &cut), &password));
            }
            Wire::Status(code, heads, body) => (code, heads, body),
        };
        if !(code == 200 || code == 201) || body.len() > MAX_SDP_BYTES {
            return fail(502, whep_error(code, b""));
        }
        let answer = String::from_utf8_lossy(&body).into_owned();
        if !answer.starts_with("v=0") {
            return fail(
                502,
                "The camera server answered with something that is not an SDP answer.".into(),
            );
        }
        let location = heads
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let mut resource = if location.is_empty() {
            String::new()
        } else {
            urljoin(&url, &location)
        };
        if !resource.is_empty() && !same_origin(&url, &resource) {
            resource.clear();
        }
        let mut raw = [0u8; 16];
        let _ = getrandom::fill(&mut raw);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let evicted = {
            let mut g = lock(&self.svc);
            g.sessions.insert(
                token.clone(),
                Session {
                    stream: id.to_string(),
                    config: cfg,
                    location: resource,
                    created: Instant::now(),
                },
            );
            let mut evicted = Vec::new();
            while g.sessions.len() > MAX_WHEP_SESSIONS {
                let oldest = g
                    .sessions
                    .iter()
                    .min_by_key(|(_, s)| s.created)
                    .map(|(t, _)| t.clone())
                    .unwrap_or_default();
                match g.sessions.remove(&oldest) {
                    Some(s) => evicted.push(s),
                    None => break,
                }
            }
            evicted
        };
        self.close_sessions(evicted, self.env.timing.whep_close_timeout);
        WhepResult::Ok {
            sdp: answer,
            session: token,
        }
    }

    /// Ends one WebRTC session on the camera's server. False when it was not open.
    pub fn whep_close(&self, token: &str) -> bool {
        let session = lock(&self.svc).sessions.remove(token);
        match session {
            Some(s) => {
                self.close_sessions(vec![s], self.env.timing.whep_close_timeout);
                true
            }
            None => false,
        }
    }

    fn close_sessions(&self, sessions: Vec<Session>, budget: Duration) {
        let timeout = self.env.timing.whep_close_timeout;
        let tasks: Vec<Task> = sessions
            .into_iter()
            .filter(|s| !s.location.is_empty())
            .map(|s| {
                Task::spawn("corvus-video-whep-close".into(), move || {
                    let mut headers = Vec::new();
                    let auth = auth_header(&s.config);
                    if !auth.is_empty() {
                        headers.push(("Authorization".to_string(), auth));
                    }
                    let _ = whep_request("DELETE", &s.location, &headers, None, timeout, 4096);
                })
            })
            .collect();
        let end = Instant::now() + budget;
        for t in &tasks {
            t.join(end.saturating_duration_since(Instant::now()));
        }
    }

    /// Everything the Video page shows, without a single password.
    pub fn status(&self) -> Value {
        let path = self.env.resolve(false);
        let (configs, decoders, sessions, configured) = {
            let g = lock(&self.svc);
            let mut counts: HashMap<String, u64> = HashMap::new();
            for s in g.sessions.values() {
                *counts.entry(s.stream.clone()).or_insert(0) += 1;
            }
            (
                g.settings["streams"].as_array().cloned().unwrap_or_default(),
                g.streams.clone(),
                counts,
                g.settings["ffmpeg"].as_str().unwrap_or("").to_string(),
            )
        };
        let (source, version) = {
            let st = lock(&self.env.st);
            (st.source.clone(), st.version.clone())
        };
        let mut out = Vec::new();
        for cfg in &configs {
            let mut entry = public_stream(cfg).as_object().cloned().unwrap_or_default();
            let id = cfg["id"].as_str().unwrap_or("");
            let extra = match decoders.get(id) {
                Some(d) => d.status(),
                None => {
                    json!({"state": STATE_IDLE, "message": "", "seq": 0, "width": 0, "height": 0, "fps": 0.0, "age": null, "running": false})
                }
            };
            if let Some(extra) = extra.as_object() {
                for (k, v) in extra {
                    entry.insert(k.clone(), v.clone());
                }
            }
            entry.insert("sessions".into(), json!(sessions.get(id).copied().unwrap_or(0)));
            out.push(Value::Object(entry));
        }
        let reason = if !path.is_empty() {
            String::new()
        } else if !configured.is_empty() {
            format!("No ffmpeg at {configured}.")
        } else {
            "ffmpeg was not found. RTSP cameras need it to decode the video. Install ffmpeg, or give its path below."
                .into()
        };
        let mut ff = Map::new();
        ff.insert("path".into(), json!(path));
        ff.insert("source".into(), json!(source));
        ff.insert("version".into(), json!(version));
        ff.insert("configured".into(), json!(configured));
        ff.insert(
            "hides_password".into(),
            json!(!path.is_empty() && ffmpeg::hides_password(&version)),
        );
        json!({
            "available": !path.is_empty(),
            "reason": reason,
            "ffmpeg": Value::Object(ff),
            "streams": out,
            "max_streams": MAX_STREAMS,
            "schemes": SCHEMES,
            "transports": TRANSPORTS,
            "kinds": KINDS,
        })
    }

    /// Stops every decoder, closes every WHEP session, joins every thread.
    /// Never panics.
    pub fn shutdown(&self) {
        let (streams, sessions) = {
            let mut g = lock(&self.svc);
            g.closed = true;
            let mut streams: Vec<Arc<VideoStream>> = g.streams.values().cloned().collect();
            streams.append(&mut g.retired);
            (streams, g.sessions.drain().map(|(_, s)| s).collect::<Vec<_>>())
        };
        let tmpdir = lock(&self.env.st).tmpdir.take();
        for s in &streams {
            s.stop(false);
        }
        self.close_sessions(sessions, Duration::from_secs(2));
        let limit = self.env.timing.term_timeout * 2 + Duration::from_secs(2);
        for s in &streams {
            s.join(limit);
        }
        if let Some(d) = tmpdir {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// Whether any decoder thread is still running (for the lifecycle test).
    pub fn any_alive(&self) -> bool {
        let g = lock(&self.svc);
        g.streams.values().chain(g.retired.iter()).any(|s| s.alive())
    }
}
