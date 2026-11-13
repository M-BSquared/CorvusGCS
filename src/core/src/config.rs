//! The operator configuration, `~/.corvus/config.json` (`config.py`).
//!
//! Same file, same coercion, same bytes on save: a missing file is the
//! defaults, a malformed one is the defaults with a warning, unknown keys are
//! ignored and every known key is coerced to the type the Python program
//! accepts, so the Rust and the Python program can share a folder.
//!
//! The foundation coerces the blocks it owns (theme, map, map_tokens,
//! branding, controls, forwarding, autoconnect, ui, updates, plugins,
//! parameters, review). A block that belongs to an area (battery, remote_id,
//! rtk, video, checklists, fleet, mcp) passes through as an object until that
//! area registers its coercer (`Registry::config_block`), so nothing the area
//! has not ported yet is lost on a save.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::json::{self, Map};

pub const MAX_CONFIG_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The key order of the saved file.
pub const FIELD_ORDER: &[&str] = &[
    "mavlink_connection",
    "http_port",
    "tile_cache_dir",
    "tlog_dir",
    "params_dir",
    "firmware_dir",
    "log_download_dir",
    "missions_dir",
    "tile_sources",
    "stream_rates",
    "ssh_connections",
    "theme",
    "language",
    "map",
    "map_tokens",
    "branding",
    "controls",
    "ui",
    "forwarding",
    "autoconnect",
    "updates",
    "battery",
    "remote_id",
    "rtk",
    "video",
    "plugins",
    "parameters",
    "review",
    "checklists",
    "fleet",
    "mcp",
];

const STRING_FIELDS: &[&str] = &[
    "tile_cache_dir",
    "tlog_dir",
    "params_dir",
    "firmware_dir",
    "log_download_dir",
    "missions_dir",
];

pub const DEFAULT_MAVLINK_CONNECTION: &str = "udp:0.0.0.0:14550";
pub const DEFAULT_HTTP_PORT: i64 = 8000;

/// Turns the raw value of an area's block into what is kept, or `None` for
/// "use the built-in defaults".
pub type Coercer = Arc<dyn Fn(&Value) -> Option<Map> + Send + Sync>;

/// The coercers areas registered, by config key.
#[derive(Clone, Default)]
pub struct Coercers(HashMap<String, Coercer>);

impl Coercers {
    pub fn insert(&mut self, key: &str, c: Coercer) -> bool {
        self.0.insert(key.to_string(), c).is_none()
    }

    fn get(&self, key: &str) -> Option<&Coercer> {
        self.0.get(key)
    }
}

/// A loaded configuration: every field that has a value, under its config
/// key. The scalar fields and `ssh_connections` are always there; a block is
/// there when it is set.
#[derive(Debug, Clone, PartialEq)]
pub struct CorvusConfig {
    fields: Map,
}

impl Default for CorvusConfig {
    fn default() -> Self {
        let mut f = Map::new();
        f.insert("mavlink_connection".into(), DEFAULT_MAVLINK_CONNECTION.into());
        f.insert("http_port".into(), DEFAULT_HTTP_PORT.into());
        for k in STRING_FIELDS {
            f.insert((*k).into(), "".into());
        }
        f.insert("ssh_connections".into(), Value::Array(Vec::new()));
        CorvusConfig { fields: f }
    }
}

impl CorvusConfig {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    /// A block as an object, when it is set.
    pub fn block(&self, key: &str) -> Option<&Map> {
        self.fields.get(key).and_then(Value::as_object)
    }

    pub fn str_field(&self, key: &str) -> &str {
        self.fields.get(key).and_then(Value::as_str).unwrap_or("")
    }

    pub fn mavlink_connection(&self) -> &str {
        self.str_field("mavlink_connection")
    }

    pub fn http_port(&self) -> i64 {
        self.fields
            .get("http_port")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_HTTP_PORT)
    }

    pub fn language(&self) -> Option<&str> {
        self.fields.get("language").and_then(Value::as_str)
    }

    pub fn ssh_connections(&self) -> Vec<&Map> {
        self.fields
            .get("ssh_connections")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_object).collect())
            .unwrap_or_default()
    }

    /// Sets or clears (`None`) a field as the update path does.
    pub fn set(&mut self, key: &str, value: Option<Value>) {
        match value {
            Some(v) => {
                self.fields.insert(key.to_string(), v);
            }
            None => {
                self.fields.shift_remove(key);
            }
        }
    }

    /// The config as the file holds it: stable key order, unset blocks left out.
    pub fn to_dict(&self) -> Map {
        let mut out = Map::new();
        for key in FIELD_ORDER {
            let Some(v) = self.fields.get(*key) else { continue };
            if *key == "language" && v.as_str().is_none_or(str::is_empty) {
                continue;
            }
            if *key == "video" {
                let streams: Vec<Value> = v
                    .get("streams")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(Value::is_object)
                    .collect();
                let mut video = Map::new();
                video.insert("streams".into(), Value::Array(streams));
                video.insert("ffmpeg".into(), v.get("ffmpeg").cloned().unwrap_or_else(|| "".into()));
                out.insert((*key).into(), Value::Object(video));
                continue;
            }
            out.insert((*key).into(), v.clone());
        }
        out
    }

    /// `to_dict` with every secret taken out: SSH and camera passwords, the
    /// NTRIP password (blanked, `has_password` says whether there was one)
    /// and the map keys. The one door a response may carry the config through.
    pub fn to_public(&self) -> Map {
        let mut public = self.to_dict();
        public.shift_remove("map_tokens");
        if let Some(Value::Object(rtk)) = public.get_mut("rtk") {
            if let Some(Value::Object(ntrip)) = rtk.get_mut("ntrip") {
                let has = ntrip.get("password").is_some_and(truthy);
                ntrip.insert("has_password".into(), has.into());
                ntrip.insert("password".into(), "".into());
            }
        }
        if let Some(Value::Object(video)) = public.get_mut("video") {
            if let Some(Value::Array(streams)) = video.get_mut("streams") {
                for s in streams.iter_mut() {
                    if let Value::Object(s) = s {
                        let has = s.shift_remove("password").is_some_and(|p| truthy(&p));
                        s.insert("has_password".into(), has.into());
                    }
                }
            }
        }
        let redacted: Vec<Value> = public
            .get("ssh_connections")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_object)
                    .map(|e| {
                        let mut e = e.clone();
                        e.shift_remove("password");
                        Value::Object(e)
                    })
                    .collect()
            })
            .unwrap_or_default();
        public.insert("ssh_connections".into(), Value::Array(redacted));
        public
    }
}

pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `int(value)` as Python does it for the values a config can hold; `None`
/// where Python raises. Booleans are the caller's to refuse.
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            (f.is_finite() && f.abs() < 9.2e18).then(|| f.trunc() as i64)
        }),
        Value::String(s) => parse_py_int(s),
        _ => None,
    }
}

fn parse_py_int(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, digits) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let n: i64 = digits.replace('_', "").parse().ok()?;
    Some(if neg { -n } else { n })
}

fn coerce_int(v: Option<&Value>, default: i64) -> i64 {
    match v {
        Some(Value::Bool(_)) | None => default,
        Some(v) => py_int(v).unwrap_or(default),
    }
}

fn nonempty(m: Map) -> Option<Map> {
    (!m.is_empty()).then_some(m)
}

fn strings_of(raw: &Value, keys: &[&str]) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for k in keys {
        if let Some(v) = raw.get(*k).filter(|v| v.is_string()) {
            out.insert((*k).into(), v.clone());
        }
    }
    nonempty(out)
}

fn coerce_ssh_entry(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let name = raw.get("name")?.as_str().filter(|n| !n.is_empty())?;
    let text = |k: &str| raw.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut port = coerce_int(Some(raw.get("port").unwrap_or(&Value::from(22))), 22);
    if !(1..=65535).contains(&port) {
        port = 22;
    }
    let mut e = Map::new();
    e.insert("name".into(), name.into());
    e.insert("host".into(), text("host").into());
    e.insert("port".into(), port.into());
    e.insert("username".into(), text("username").into());
    e.insert("key_path".into(), text("key_path").into());
    e.insert("password".into(), text("password").into());
    Some(e)
}

fn coerce_ssh_connections(raw: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(list)) = raw else {
        return Vec::new();
    };
    let mut out: Vec<Map> = Vec::new();
    for entry in list {
        let Some(c) = coerce_ssh_entry(entry) else { continue };
        let name = c["name"].as_str().unwrap_or("").to_string();
        match out.iter_mut().find(|e| e["name"].as_str() == Some(name.as_str())) {
            Some(existing) => *existing = c,
            None => out.push(c),
        }
    }
    out.into_iter().map(Value::Object).collect()
}

/// Whether `code` can name a language file and sit in a URL
/// (`^[a-z]{2,3}(-[A-Za-z0-9]{2,8}){0,2}$`).
pub fn is_valid_language_code(code: &str) -> bool {
    let mut parts = code.split('-');
    let Some(first) = parts.next() else { return false };
    if !(2..=3).contains(&first.len()) || !first.bytes().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    rest.len() <= 2
        && rest
            .iter()
            .all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

const MAP_TOKEN_MAX: usize = 512;

fn coerce_map_tokens(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for (provider, value) in raw {
        let Some(value) = value.as_str() else { continue };
        let token = value.trim();
        if provider.is_empty() || token.is_empty() {
            continue;
        }
        if token.chars().count() > MAP_TOKEN_MAX {
            eprintln!("corvus: config: map_tokens.{provider} is implausibly long; dropped");
            continue;
        }
        if !token.chars().all(|c| c.is_ascii_alphanumeric() || "._~-".contains(c)) {
            eprintln!("corvus: config: map_tokens.{provider} has characters an API key cannot contain; dropped");
            continue;
        }
        out.insert(provider.clone(), token.into());
    }
    nonempty(out)
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn coerce_controls(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for k in ["virtual_joystick", "gamepad", "arrow_keys", "wasd_keys"] {
        if let Some(b) = json::bool_of(raw, k) {
            out.insert(k.into(), b.into());
        }
    }
    if let Some(g) = number(raw.get("key_gain")).filter(|g| g.is_finite()) {
        out.insert("key_gain".into(), Value::from(g.clamp(0.05, 1.0)));
    }
    nonempty(out)
}

fn coerce_forwarding(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for k in ["enabled", "allow_commands"] {
        if let Some(b) = json::bool_of(raw, k) {
            out.insert(k.into(), b.into());
        }
    }
    for k in ["host", "listen_host"] {
        if let Some(s) = raw
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.insert(k.into(), s.into());
        }
    }
    if let Some(p) = raw
        .get("port")
        .filter(|v| json::is_int(v))
        .and_then(Value::as_i64)
        .filter(|p| (1..=65535).contains(p))
    {
        out.insert("port".into(), p.into());
    }
    if let Some(p) = raw
        .get("listen_port")
        .filter(|v| json::is_int(v))
        .and_then(Value::as_i64)
        .filter(|p| (0..=65535).contains(p))
    {
        out.insert("listen_port".into(), p.into());
    }
    if let Some(list) = raw.get("endpoints").and_then(Value::as_array) {
        let kept: Vec<Value> = list
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Value::from)
            .collect();
        if !kept.is_empty() {
            out.insert("endpoints".into(), Value::Array(kept));
        }
    }
    nonempty(out)
}

/// The auto-connect toggles and their defaults (all on).
pub const AUTOCONNECT_KEYS: [&str; 4] = ["enabled", "usb", "sik", "udp_fallback"];

fn coerce_autoconnect(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for k in AUTOCONNECT_KEYS {
        match raw.get(k) {
            Some(Value::Bool(b)) => {
                out.insert(k.into(), (*b).into());
            }
            Some(Value::Null) | None => {}
            Some(other) => eprintln!("corvus: config: autoconnect.{k} must be true or false, not {other}, using true"),
        }
    }
    nonempty(out)
}

const UI_UNITS: [(&str, &[&str]); 4] = [
    ("length", &["m", "ft"]),
    ("distance", &["km", "mi", "nmi"]),
    ("speed", &["ms", "kmh", "mph", "kn"]),
    ("temperature", &["c", "f"]),
];

const UI_BOOLS: &[&str] = &[
    "inverted_app_icon",
    "app_icon_backplate",
    "topbar_status_dots",
    "mission_page",
    "betaflight_always",
    "notification_marks",
    "flight_bar_shrink",
    "takeoff_land_split",
    "compass_nose_up",
    "windows_in_app",
    "solid_terminals",
    "corvus_title_bar",
    "track_earlier_flights",
    "track_rainbow",
    "track_clear_on_restart",
    "track_in_air",
    "flight_hud",
    "hud_square",
    "topbar_show_altitude",
    "topbar_show_groundspeed",
    "topbar_show_vspeed",
    "topbar_show_rangefinder",
    "measure_tool",
    "console_tab",
    "ssh_tab",
    "plugin_tabs",
];

fn coerce_ui(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    if let Some(units) = raw.get("units").and_then(Value::as_object) {
        let mut kept = Map::new();
        for (q, allowed) in UI_UNITS {
            if let Some(v) = units
                .get(q)
                .filter(|v| v.as_str().is_some_and(|s| allowed.contains(&s)))
            {
                kept.insert(q.into(), v.clone());
            }
        }
        if !kept.is_empty() {
            out.insert("units".into(), Value::Object(kept));
        }
    }
    if let Some(v) = raw
        .get("topbar_altitude")
        .filter(|v| matches!(v.as_str(), Some("amsl" | "relative")))
    {
        out.insert("topbar_altitude".into(), v.clone());
    }
    if let Some(r) = number(raw.get("map_animation_rate")).filter(|r| *r == 30.0 || *r == 60.0) {
        out.insert("map_animation_rate".into(), (r as i64).into());
    }
    if let Some(s) = number(raw.get("scale")).filter(|s| s.is_finite()) {
        out.insert("scale".into(), Value::from(s.clamp(0.5, 3.0)));
    }
    for k in UI_BOOLS {
        if let Some(b) = json::bool_of(raw, k) {
            out.insert((*k).into(), b.into());
        }
    }
    nonempty(out)
}

/// `^v?(\d+(\.\d+)*)$`, as the tuple of numbers, or `None`.
pub fn parse_version(text: &str) -> Option<Vec<u64>> {
    let t = text.trim();
    let t = t.strip_prefix('v').unwrap_or(t);
    let parts: Vec<&str> = t.split('.').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    parts.iter().map(|p| p.parse().ok()).collect()
}

fn coerce_updates(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    if let Some(b) = json::bool_of(raw, "check") {
        out.insert("check".into(), b.into());
    }
    if let Some(s) = raw
        .get("skipped")
        .and_then(Value::as_str)
        .filter(|s| parse_version(s).is_some())
    {
        out.insert(
            "skipped".into(),
            s.trim()
                .trim_start_matches('v')
                .trim_start_matches("")
                .to_string()
                .into(),
        );
    }
    nonempty(out)
}

fn coerce_plugins(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    nonempty(
        raw.iter()
            .filter(|(k, v)| !k.is_empty() && v.is_object())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

fn coerce_parameters(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    if let Some(b) = json::bool_of(raw, "cache_defaults") {
        out.insert("cache_defaults".into(), b.into());
    }
    nonempty(out)
}

const SENSITIVITIES: [&str; 3] = ["relaxed", "normal", "strict"];

fn coerce_review(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let v = raw.get("sensitivity")?.as_str()?.trim().to_lowercase();
    SENSITIVITIES.contains(&v.as_str()).then(|| {
        let mut m = Map::new();
        m.insert("sensitivity".into(), v.into());
        m
    })
}

fn coerce_block(key: &str, raw: &Value, coercers: &Coercers) -> Option<Map> {
    match key {
        "tile_sources" | "stream_rates" => raw.as_object().cloned(),
        "theme" => strings_of(raw, &["name", "accent"]),
        "map" => strings_of(
            raw,
            &["base_layer", "provider", "three_d", "three_d_detail", "terrain_source"],
        ),
        "map_tokens" => coerce_map_tokens(raw),
        "branding" => strings_of(raw, &["logo"]),
        "controls" => coerce_controls(raw),
        "forwarding" => coerce_forwarding(raw),
        "autoconnect" => coerce_autoconnect(raw),
        "ui" => coerce_ui(raw),
        "updates" => coerce_updates(raw),
        "plugins" => coerce_plugins(raw),
        "parameters" => coerce_parameters(raw),
        "review" => coerce_review(raw),
        _ => match coercers.get(key) {
            Some(c) => c(raw),
            None => raw.as_object().cloned().and_then(nonempty),
        },
    }
}

/// Builds a config from a parsed file, ignoring unknown keys. Never fails.
pub fn build(data: &Map, coercers: &Coercers) -> CorvusConfig {
    let mut cfg = CorvusConfig::default();
    if let Some(c) = data
        .get("mavlink_connection")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        cfg.set("mavlink_connection", Some(c.into()));
    }
    if let Some(v) = data.get("http_port") {
        let port = coerce_int(Some(v), DEFAULT_HTTP_PORT);
        cfg.set(
            "http_port",
            Some(
                (if (0..=65535).contains(&port) {
                    port
                } else {
                    DEFAULT_HTTP_PORT
                })
                .into(),
            ),
        );
    }
    for k in STRING_FIELDS {
        if let Some(s) = data.get(*k).filter(|v| v.is_string()) {
            cfg.set(k, Some(s.clone()));
        }
    }
    cfg.set(
        "ssh_connections",
        Some(Value::Array(coerce_ssh_connections(data.get("ssh_connections")))),
    );
    if let Some(l) = data
        .get("language")
        .and_then(Value::as_str)
        .filter(|l| is_valid_language_code(l))
    {
        cfg.set("language", Some(l.into()));
    }
    for key in FIELD_ORDER {
        let owned_block = !matches!(
            *key,
            "mavlink_connection" | "http_port" | "ssh_connections" | "language"
        ) && !STRING_FIELDS.contains(key);
        if !owned_block {
            continue;
        }
        if let Some(raw) = data.get(*key) {
            if let Some(block) = coerce_block(key, raw, coercers) {
                cfg.set(key, Some(Value::Object(block)));
            }
        }
    }
    cfg
}

/// Reads the file at `path`. Missing is the defaults, silently; unreadable or
/// malformed is the defaults with a warning. Never fails.
pub fn load(path: &Path, coercers: &Coercers) -> CorvusConfig {
    if !path.is_file() {
        return CorvusConfig::default();
    }
    let text = match read_limited(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "corvus: config file {} unreadable/malformed; using defaults: {e}",
                path.display()
            );
            return CorvusConfig::default();
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => build(&m, coercers),
        Ok(_) => {
            eprintln!(
                "corvus: config file {} top-level is not a JSON object; using defaults",
                path.display()
            );
            CorvusConfig::default()
        }
        Err(e) => {
            eprintln!(
                "corvus: config file {} unreadable/malformed; using defaults: {e}",
                path.display()
            );
            CorvusConfig::default()
        }
    }
}

fn read_limited(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(format!("config exceeds {MAX_CONFIG_FILE_BYTES} bytes"));
    }
    String::from_utf8(raw).map_err(|e| e.to_string())
}

/// The text `save` writes: two-space indent, non-ASCII as it is, a final newline.
pub fn to_file_text(cfg: &CorvusConfig) -> String {
    let mut text = serde_json::to_string_pretty(&Value::Object(cfg.to_dict())).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    text
}

/// Writes the config atomically with mode 0o600 (it carries SSH, NTRIP and
/// camera passwords and the map keys): a temp file beside it, synced, then
/// renamed over the real one, so a reader never sees half a file.
pub fn save(cfg: &CorvusConfig, path: &Path) -> std::io::Result<()> {
    let text = to_file_text(cfg);
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.json".into());
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = dir.join(format!(
        "{name}.{}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn as_object<'a>(m: &'a Map, key: &str) -> Option<&'a Map> {
    m.get(key).and_then(Value::as_object)
}

/// The keys `POST /api/config` takes; the others belong to their own endpoint.
const KNOWN: &[&str] = &[
    "mavlink_connection",
    "http_port",
    "tile_cache_dir",
    "tlog_dir",
    "params_dir",
    "firmware_dir",
    "log_download_dir",
    "missions_dir",
    "tile_sources",
    "stream_rates",
    "ssh_connections",
    "theme",
    "language",
    "map",
    "branding",
    "controls",
    "ui",
    "updates",
    "autoconnect",
    "battery",
    "remote_id",
    "parameters",
    "review",
    "checklists",
];
const OWNED_ELSEWHERE: &[&str] = &["forwarding", "map_tokens", "plugins", "fleet", "mcp"];
/// Blocks a partial update merges key by key into the stored one.
const MERGED_BLOCKS: &[&str] = &[
    "map",
    "controls",
    "ui",
    "updates",
    "autoconnect",
    "battery",
    "parameters",
    "review",
    "checklists",
];
/// Blocks a partial update replaces whole.
const REPLACED_BLOCKS: &[&str] = &["theme", "branding", "tile_sources", "stream_rates"];

fn merged_with_stored(config: &CorvusConfig) -> Map {
    let mut m = Map::new();
    for k in [
        "mavlink_connection",
        "http_port",
        "tile_cache_dir",
        "tlog_dir",
        "params_dir",
        "firmware_dir",
        "log_download_dir",
        "missions_dir",
        "ssh_connections",
        "tile_sources",
        "stream_rates",
        "theme",
        "language",
        "map",
        "map_tokens",
        "branding",
        "controls",
        "ui",
        "autoconnect",
        "updates",
        "battery",
        "remote_id",
        "plugins",
        "parameters",
        "review",
        "checklists",
    ] {
        if let Some(v) = config.get(k) {
            m.insert(k.into(), v.clone());
        }
    }
    m
}

/// A partial update (`POST /api/config`) applied the way
/// `_apply_config_partial` does: validated key by key (the first refusal
/// wins), merged into what is stored, coerced like a loaded file. Returns the
/// new config, or the refusal text. Only the fields that went through the
/// merge are replaced.
pub fn apply_partial(config: &CorvusConfig, partial: &Map, coercers: &Coercers) -> Result<CorvusConfig, String> {
    let mut merged = merged_with_stored(config);
    for (key, value) in partial {
        let key = key.as_str();
        if !KNOWN.contains(&key) {
            if !OWNED_ELSEWHERE.contains(&key) {
                eprintln!("corvus: dropping unknown config key {key:?} in POST /api/config");
            }
            continue;
        }
        match key {
            "ssh_connections" => {
                if !value.is_array() {
                    return Err("ssh_connections must be a list".into());
                }
                merged.insert(key.into(), value.clone());
            }
            "language" => match value.as_str() {
                Some(s) if s.is_empty() || is_valid_language_code(s) => {
                    merged.insert(key.into(), value.clone());
                }
                _ => return Err("language must be a language code such as de or pt-BR".into()),
            },
            "remote_id" => {
                let Some(v) = value.as_object() else {
                    return Err("remote_id must be an object".into());
                };
                let nested = match as_object(&merged, key) {
                    Some(base) => {
                        let mut nested = base.clone();
                        for (k, sub) in v {
                            match (sub.as_object(), nested.get(k).and_then(Value::as_object)) {
                                (Some(new), Some(old)) => {
                                    let mut joined = old.clone();
                                    joined.extend(new.clone());
                                    nested.insert(k.clone(), Value::Object(joined));
                                }
                                _ => {
                                    nested.insert(k.clone(), sub.clone());
                                }
                            }
                        }
                        nested
                    }
                    None => v.clone(),
                };
                merged.insert(key.into(), Value::Object(nested));
            }
            k if MERGED_BLOCKS.contains(&k) => {
                let Some(v) = value.as_object() else {
                    return Err(format!("{k} must be an object"));
                };
                let mut base = as_object(&merged, k).cloned().unwrap_or_default();
                base.extend(v.clone());
                merged.insert(k.into(), Value::Object(base));
            }
            k if REPLACED_BLOCKS.contains(&k) => {
                if !value.is_object() {
                    return Err(format!("{k} must be an object"));
                }
                merged.insert(k.into(), value.clone());
            }
            "mavlink_connection" => match value.as_str() {
                Some(s) if !s.is_empty() => {
                    merged.insert(key.into(), value.clone());
                }
                _ => return Err("mavlink_connection must be a non-empty string".into()),
            },
            "http_port" => {
                let port = match value {
                    Value::Bool(_) => None,
                    v => py_int(v),
                };
                let Some(port) = port else {
                    return Err("http_port must be an integer".into());
                };
                if !(0..=65535).contains(&port) {
                    return Err("http_port must be between 0 and 65535".into());
                }
                merged.insert(key.into(), port.into());
            }
            k => {
                if !value.is_string() {
                    return Err(format!("{k} must be a string"));
                }
                merged.insert(k.into(), value.clone());
            }
        }
    }
    let new = build(&merged, coercers);
    let mut out = config.clone();
    for key in merged.keys() {
        out.set(key, new.get(key).cloned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cases() -> Value {
        serde_json::from_str(include_str!("../tests/data/config_cases.json")).unwrap()
    }

    fn partials() -> Value {
        serde_json::from_str(include_str!("../tests/data/config_partial_cases.json")).unwrap()
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn every_python_case_loads_and_saves_the_same() {
        let dir = tmp("config-cases");
        let c = cases();
        for case in c["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let path = dir.join(format!("{name}.json"));
            std::fs::write(&path, case["text"].as_str().unwrap()).unwrap();
            let cfg = load(&path, &Coercers::default());
            assert_eq!(Value::Object(cfg.to_dict()), case["loaded"], "{name}: loaded");
            assert_eq!(Value::Object(cfg.to_public()), case["public"], "{name}: public");
            assert_eq!(to_file_text(&cfg), case["saved"].as_str().unwrap(), "{name}: file text");
        }
        let defaults = load(&dir.join("absent.json"), &Coercers::default());
        assert_eq!(Value::Object(defaults.to_dict()), c["defaults"]);
        let order: Vec<&str> = c["field_order"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(order, FIELD_ORDER);
        assert_eq!(c["max_bytes"].as_u64().unwrap(), MAX_CONFIG_FILE_BYTES);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_over_the_cap_is_the_defaults() {
        let dir = tmp("config-big");
        let path = dir.join("big.json");
        let mut text = String::from("{\"http_port\": 1}");
        text.push_str(&" ".repeat(MAX_CONFIG_FILE_BYTES as usize + 1));
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            Value::Object(load(&path, &Coercers::default()).to_dict()),
            cases()["too_big"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn partial_updates_end_as_the_python_handler_leaves_them() {
        let spec = partials();
        let base = build(spec["base"].as_object().unwrap(), &Coercers::default());
        for case in spec["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let result = apply_partial(&base, case["partial"].as_object().unwrap(), &Coercers::default());
            match result {
                Ok(cfg) => {
                    assert!(case["error"].is_null(), "{name}: expected an error");
                    assert_eq!(Value::Object(cfg.to_public()), case["public"], "{name}: public");
                    assert_eq!(to_file_text(&cfg), case["saved"].as_str().unwrap(), "{name}: saved");
                }
                Err(e) => {
                    assert_eq!(Some(e.as_str()), case["error"].as_str(), "{name}");
                }
            }
        }
    }

    #[test]
    fn an_area_block_passes_through_until_its_coercer_is_registered() {
        let raw = json!({"battery": {"estimate": true, "cells": 4}, "mcp": {}});
        let cfg = build(raw.as_object().unwrap(), &Coercers::default());
        assert_eq!(cfg.block("battery").unwrap()["cells"], json!(4));
        assert!(cfg.block("mcp").is_none());
        let mut c = Coercers::default();
        c.insert("battery", Arc::new(|_| None));
        assert!(build(raw.as_object().unwrap(), &c).block("battery").is_none());
    }

    #[test]
    fn saving_is_atomic_and_private() {
        let dir = tmp("config-save");
        let path = dir.join("nested").join("config.json");
        let mut cfg = CorvusConfig::default();
        cfg.set("language", Some("de".into()));
        save(&cfg, &path).unwrap();
        save(&cfg, &path).unwrap();
        assert_eq!(load(&path, &Coercers::default()), cfg);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(leftovers.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn python_int_rules() {
        assert_eq!(py_int(&json!("1_0")), Some(10));
        assert_eq!(py_int(&json!(" 7 ")), Some(7));
        assert_eq!(py_int(&json!(22.7)), Some(22));
        assert_eq!(py_int(&json!("1__0")), None);
        assert_eq!(py_int(&json!("x")), None);
        assert_eq!(py_int(&json!(null)), None);
        assert!(
            is_valid_language_code("zh-Hant-TW")
                && !is_valid_language_code("de-abcdefghi")
                && !is_valid_language_code("Deutsch")
        );
    }
}
