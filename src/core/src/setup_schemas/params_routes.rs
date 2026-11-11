//! The parameter routes of the Vehicle area (`/api/params*` of `server.py`): the
//! full list, the download, one write, the read back check, the upload of a
//! saved file, and the export of the list in the format the connected stack's
//! users exchange (`param_files.py`).
//!
//! A parameter is written only when the vehicle answers for it: `set_param` asks
//! the firmware for the name first, so a name the loaded firmware does not have
//! is refused rather than sent.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::param_metadata::ParamMetadataCache;
use super::routes::handles;
use crate::bridge::Bridge;
use crate::buffer::Sink;
use crate::core::Core;
use crate::events::{Binding, Topic};
use crate::module::Registry;
use crate::plugins::settings_routes::{safe_filename, slugify};
use crate::protocol::params::Status;
use crate::request::Request;
use crate::response::Response;
use crate::vehicle::{Handles, VehicleService};

/// How many progress events a slow reader may hold (`PARAMS_SSE_CAPACITY`).
pub const PARAMS_SSE_CAPACITY: usize = 16;

const FORMATS: [(&str, &str); 3] = [("qgc", ".params"), ("mission-planner", ".param"), ("json", ".json")];
const REAL32: i64 = 9;

type SinkList = Vec<(u64, usize, Sink<Value>)>;

/// Fans every bridge's parameter progress out to the streams that follow it. A
/// bridge listener cannot be taken off again, so each bridge gets one, once.
#[derive(Default)]
pub struct ParamHub {
    sinks: Mutex<SinkList>,
    attached: Mutex<Vec<usize>>,
    next: Mutex<u64>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn progress(status: &Status) -> Value {
    json!({"state": status.state, "count": status.count, "received": status.received})
}

impl ParamHub {
    fn publish(&self, key: usize, status: &Status) {
        let event = progress(status);
        for (_, k, sink) in lock(&self.sinks).iter() {
            if *k == key {
                sink.put_latest(event.clone());
            }
        }
    }

    fn attach(self: &Arc<Self>, h: &Handles) -> usize {
        let key = Arc::as_ptr(&h.store) as usize;
        let mut attached = lock(&self.attached);
        if !attached.contains(&key) {
            attached.push(key);
            let hub = Arc::clone(self);
            h.bridge.add_param_listener(move |s| hub.publish(key, s));
        }
        key
    }

    fn add(&self, key: usize, sink: Sink<Value>) -> u64 {
        let mut next = lock(&self.next);
        *next += 1;
        lock(&self.sinks).push((*next, key, sink));
        *next
    }

    fn remove(&self, id: u64) {
        lock(&self.sinks).retain(|(i, _, _)| *i != id);
    }
}

pub(super) fn register(reg: &mut Registry) {
    let hub = Arc::new(ParamHub::default());
    reg.topic(Topic::new("params", PARAMS_SSE_CAPACITY, move |core, sink, _job| {
        let Some(svc) = core.service::<VehicleService>() else {
            return Binding {
                unbind: Box::new(|| {}),
                initial: Some(json!({"state": "idle", "count": 0, "received": 0})),
            };
        };
        let h = svc.active();
        let key = hub.attach(&h);
        let id = hub.add(key, sink);
        let hub = Arc::clone(&hub);
        Binding {
            unbind: Box::new(move || hub.remove(id)),
            initial: Some(progress(&h.bridge.param_status())),
        }
    }));
    reg.get("/api/params", api_params);
    reg.get("/api/params/progress", api_params_progress);
    reg.post("/api/params/download", api_params_download);
    reg.get("/api/params/export/target", api_export_target);
    reg.post("/api/params/export", api_export);
    reg.post("/api/params/set", api_set);
    reg.post("/api/params/verify", api_verify);
    reg.post("/api/params/upload", api_upload);
    reg.get("/api/params/upload/result", api_upload_result);
    reg.get("/api/params/metadata", api_metadata);
    reg.post("/api/params/metadata", api_metadata_fetch);
    reg.get("/api/params/metadata/cache", api_metadata_cache);
    reg.post("/api/params/metadata/cache/clear", api_metadata_cache_clear);
}

fn cache_enabled(core: &Core) -> bool {
    core.settings()
        .get()
        .block("parameters")
        .and_then(|b| b.get("cache_defaults"))
        .is_some_and(|v| match v {
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64() != Some(0.0),
            Value::String(s) => !s.is_empty(),
            Value::Null => false,
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        })
}

fn metadata_cache(core: &Core) -> ParamMetadataCache {
    ParamMetadataCache::new(core.paths().join("param-metadata"))
}

fn api_metadata(core: &Core, _req: &Request) -> Response {
    match handles(core) {
        Some(h) => Response::json(h.bridge.param_metadata_status(true)),
        None => {
            Response::json(json!({"state": "idle", "error": "not connected", "received": 0, "size": 0, "count": 0}))
        }
    }
}

fn api_metadata_fetch(core: &Core, _req: &Request) -> Response {
    let Some(h) = handles(core) else {
        return refuse(503, "not connected");
    };
    let cache = cache_enabled(core).then(|| metadata_cache(core));
    let mut status = h.bridge.start_param_metadata(cache);
    status["ok"] = json!(true);
    Response::json(status)
}

fn api_metadata_cache(core: &Core, _req: &Request) -> Response {
    let mut info = metadata_cache(core).info();
    info.insert("enabled".into(), json!(cache_enabled(core)));
    Response::json(Value::Object(info))
}

fn api_metadata_cache_clear(core: &Core, _req: &Request) -> Response {
    let cache = metadata_cache(core);
    let removed = cache.clear();
    let mut info = cache.info();
    info.insert("ok".into(), json!(true));
    info.insert("removed".into(), json!(removed));
    info.insert("enabled".into(), json!(cache_enabled(core)));
    Response::json(Value::Object(info))
}

fn api_params_progress(core: &Core, _req: &Request) -> Response {
    Response::sse(core.topic_stream(&[("params", "progress")], None, &[]))
}

fn reason(bridge: &Bridge, fallback: &str) -> String {
    let error = bridge.last_error();
    if error.is_empty() {
        fallback.to_string()
    } else {
        error
    }
}

fn refuse(status: u16, error: &str) -> Response {
    Response::json_status(json!({"ok": false, "error": error}), status)
}

fn api_params(core: &Core, _req: &Request) -> Response {
    let Some(h) = handles(core) else {
        return Response::json(json!({"state": "idle", "count": 0, "received": 0, "complete": false, "params": []}));
    };
    let st = h.bridge.param_status();
    let complete = st.state == "complete";
    let params = if complete { h.bridge.params_json() } else { Vec::new() };
    Response::json(json!({
        "state": st.state, "count": st.count, "received": st.received,
        "complete": complete, "params": params,
    }))
}

fn api_params_download(core: &Core, _req: &Request) -> Response {
    let Some(h) = handles(core) else {
        return refuse(503, "not connected");
    };
    if h.bridge.request_param_list() {
        return Response::json(json!({"ok": true, "state": "downloading"}));
    }
    refuse(503, &reason(&h.bridge, "not connected"))
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn api_set(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let name = match payload.get("name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return refuse(400, "name must be a non-empty string"),
    };
    let Some(value) = number(payload.get("value")) else {
        return refuse(400, "value must be a number");
    };
    let Some(h) = handles(core) else {
        return refuse(503, "not connected");
    };
    if h.bridge.set_param(&name, value) {
        return Response::json(json!({"ok": true}));
    }
    let error = reason(&h.bridge, "parameter write failed");
    let lower = error.to_lowercase();
    let status = if lower.contains("disconnected") || lower.contains("not connected") {
        503
    } else {
        409
    };
    refuse(status, &error)
}

fn api_verify(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let empty = Value::Array(Vec::new());
    let (Some(params), Some(names)) = (
        payload.get("params").unwrap_or(&empty).as_array(),
        payload.get("names").unwrap_or(&empty).as_array(),
    ) else {
        return refuse(400, "params and names must be lists");
    };
    let mut read: Vec<String> = Vec::new();
    for n in names {
        match n {
            Value::String(s) if !s.is_empty() => read.push(s.clone()),
            _ => return refuse(400, "names must be non-empty strings"),
        }
    }
    let mut clean: Vec<(String, f64)> = Vec::new();
    for (index, entry) in params.iter().enumerate() {
        let name = entry.get("name").and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(name) = name else {
            return refuse(
                400,
                &format!("invalid parameter at index {index}: name must be a non-empty string"),
            );
        };
        let Some(value) = number(entry.get("value")) else {
            return refuse(
                400,
                &format!("invalid parameter at index {index}: value must be a number"),
            );
        };
        clean.push((name.to_string(), value));
    }
    let Some(h) = handles(core) else {
        return refuse(503, "not connected");
    };
    match h.bridge.verify_params(&clean, &read) {
        Some(mut result) => {
            result["ok"] = json!(true);
            Response::json(result)
        }
        None => {
            let error = reason(&h.bridge, "check failed");
            refuse(if error.contains("not connected") { 503 } else { 400 }, &error)
        }
    }
}

fn api_upload(core: &Core, req: &Request) -> Response {
    let Some(params) = req
        .payload()
        .get("params")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    else {
        return refuse(400, "params must be a non-empty list");
    };
    let mut clean: Vec<(String, f64)> = Vec::new();
    for (index, entry) in params.iter().enumerate() {
        let Some(entry) = entry.as_object() else {
            return refuse(400, &format!("invalid parameter at index {index}: must be an object"));
        };
        let name = match entry.get("name") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => {
                return refuse(
                    400,
                    &format!("invalid parameter at index {index}: name must be a non-empty string"),
                )
            }
        };
        let Some(value) = number(entry.get("value")) else {
            return refuse(
                400,
                &format!("invalid parameter at index {index}: value must be a number"),
            );
        };
        clean.push((name, value));
    }
    let Some(h) = handles(core) else {
        return refuse(503, "not connected");
    };
    let count = clean.len();
    if h.bridge.start_param_upload(clean) {
        return Response::json(json!({"ok": true, "state": "uploading", "count": count}));
    }
    let error = reason(&h.bridge, "parameter upload failed");
    refuse(if error.contains("not connected") { 503 } else { 409 }, &error)
}

fn api_upload_result(core: &Core, _req: &Request) -> Response {
    match handles(core) {
        Some(h) => Response::json(h.bridge.param_upload_result()),
        None => Response::json(json!({"state": "idle", "written": 0, "failed": 0, "errors": []})),
    }
}

// ---------------------------------------------------------------- export

fn export_dir(core: &Core) -> String {
    let configured = core.settings().get().str_field("params_dir").trim().to_string();
    if configured.is_empty() {
        core.paths().join("params").to_string_lossy().into_owned()
    } else {
        expand_user(&configured)
    }
}

fn expand_user(path: &str) -> String {
    let home = || {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default()
    };
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || (cfg!(windows) && rest.starts_with('\\')) => {
            format!("{}{rest}", home())
        }
        _ => path.to_string(),
    }
}

/// `corvus-params_<vehicle>_<YYYY-MM-DD_HH-MM><suffix>`.
fn default_filename(tag: &str, suffix: &str) -> String {
    let t = crate::clock::local_datetime();
    let stamp = format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}",
        t.year, t.month, t.day, t.hour, t.minute
    );
    let tag = slugify(tag);
    if tag.is_empty() {
        format!("corvus-params_{stamp}{suffix}")
    } else {
        format!("corvus-params_{tag}_{stamp}{suffix}")
    }
}

fn vehicle_tag(core: &Core) -> String {
    let snap = match handles(core) {
        Some(h) => h.store.snapshot(),
        None => core.store().snapshot(),
    };
    ["autopilot", "vehicle_type"]
        .iter()
        .filter_map(|k| snap.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .map(slugify)
        .collect::<Vec<_>>()
        .join("-")
}

fn file_info(core: &Core) -> Map<String, Value> {
    let mut info = handles(core)
        .map(|h| h.bridge.param_file_info())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    if !info
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|f| FORMATS.iter().any(|(k, _)| k == &f))
    {
        info.insert("format".into(), json!("qgc"));
    }
    info
}

fn suffix_of(format: &str) -> &'static str {
    FORMATS.iter().find(|(k, _)| *k == format).map_or(".json", |(_, s)| s)
}

fn api_export_target(core: &Core, _req: &Request) -> Response {
    let info = file_info(core);
    let format = info["format"].as_str().unwrap_or("qgc").to_string();
    Response::json(json!({
        "dir": export_dir(core),
        "filename": default_filename(&vehicle_tag(core), suffix_of(&format)),
        "format": format,
        "formats": FORMATS.iter().map(|(id, suffix)| json!({"id": id, "suffix": suffix})).collect::<Vec<_>>(),
    }))
}

/// Python's `f"{number:.{digits}g}"`.
fn format_g(number: f64, digits: usize) -> String {
    if number == 0.0 {
        return if number.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let sci = format!("{:.*e}", digits - 1, number);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if exp < -4 || exp >= digits as i32 {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", strip(mantissa), exp.abs())
    } else {
        let decimals = (digits as i32 - 1 - exp).max(0) as usize;
        strip(&format!("{number:.decimals$}"))
    }
}

/// `format_param_value`: the shortest text that reads back as the same float32.
fn format_param_value(value: f64, param_type: Option<i64>) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "nan".into()
        } else if value > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let kind = param_type.unwrap_or(REAL32);
    if kind != REAL32 && value == value.trunc() {
        return format!("{}", value as i128);
    }
    let target = value as f32;
    if target.is_infinite() {
        return format!("{value}");
    }
    for digits in 1..10 {
        let text = format_g(value, digits);
        if text.parse::<f64>().map(|v| v as f32) == Ok(target) {
            return text;
        }
    }
    format!("{value}")
}

fn type_of(entry: &Value) -> Option<i64> {
    entry
        .get("type")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite())
        .map(|t| t as i64)
}

fn sorted(params: &[Value]) -> Vec<&Value> {
    let mut v: Vec<&Value> = params.iter().collect();
    v.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    v
}

fn write_qgc(params: &[Value], info: &Map<String, Value>) -> String {
    let int = |k: &str| match info.get(k).and_then(Value::as_i64) {
        Some(v) if v != 0 => v,
        _ => 1,
    };
    let text = |k: &str| info.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let (system, component) = (int("system_id"), int("component_id"));
    let mut lines = vec![
        format!("# Onboard parameters for Vehicle {system}"),
        "#".to_string(),
        format!("# Stack: {}", text("stack")),
        format!("# Vehicle: {}", text("vehicle")),
        format!("# Version: {}", text("version")),
        format!("# Git Revision: {}", text("git_hash")),
        "#".to_string(),
        "# Vehicle-Id Component-Id Name Value Type".to_string(),
    ];
    for p in sorted(params) {
        let ptype = match type_of(p) {
            Some(t) if t != 0 => t,
            _ => REAL32,
        };
        lines.push(format!(
            "{system}\t{component}\t{}\t{}\t{ptype}",
            p["name"].as_str().unwrap_or(""),
            format_param_value(p["value"].as_f64().unwrap_or(0.0), Some(ptype)),
        ));
    }
    lines.join("\n") + "\n"
}

fn write_mission_planner(params: &[Value], header: &str) -> String {
    let mut lines: Vec<String> = header
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| format!("# {l}"))
        .collect();
    for p in sorted(params) {
        lines.push(format!(
            "{},{}",
            p["name"].as_str().unwrap_or(""),
            format_param_value(p["value"].as_f64().unwrap_or(0.0), type_of(p)),
        ));
    }
    lines.join("\n") + "\n"
}

fn write_file(dir: &Path, filename: &str, text: &str) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(filename);
    let tmp = dir.join(format!("{filename}.{}.tmp", std::process::id()));
    let outcome = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path));
    if outcome.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    outcome.map(|()| path)
}

fn api_export(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let Some(params) = payload
        .get("params")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    else {
        return refuse(400, "params must be a non-empty list");
    };
    let format = match payload.get("format") {
        None => "json".to_string(),
        Some(Value::String(f)) if FORMATS.iter().any(|(k, _)| k == f) => f.clone(),
        Some(other) => {
            let shown = match other {
                Value::String(s) => format!("'{s}'"),
                v => v.to_string(),
            };
            return refuse(400, &format!("unknown format {shown}"));
        }
    };
    for (index, entry) in params.iter().enumerate() {
        let name = entry.get("name").and_then(Value::as_str).filter(|s| !s.is_empty());
        if name.is_none() || number(entry.get("value")).is_none() {
            return refuse(400, &format!("invalid parameter at index {index}"));
        }
    }
    let target_dir = match payload.get("dir") {
        Some(Value::String(d)) if !d.trim().is_empty() => d.trim().to_string(),
        _ => export_dir(core),
    };
    let target_dir = expand_user(target_dir.trim());
    let suffix = suffix_of(&format);
    let tag = vehicle_tag(core);
    let filename = safe_filename(payload.get("filename"), &default_filename(&tag, suffix), suffix);
    let t = crate::clock::utc_datetime(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
    );
    let exported_at = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    let text = match format.as_str() {
        "qgc" => write_qgc(params, &file_info(core)),
        "mission-planner" => write_mission_planner(
            params,
            &format!(
                "Corvus GCS {} parameter export\nExported {exported_at}{}",
                crate::version::version(),
                if tag.is_empty() {
                    String::new()
                } else {
                    format!(", vehicle {tag}")
                }
            ),
        ),
        _ => {
            let doc = json!({
                "product": "Corvus GCS", "version": crate::version::version(),
                "exported_at": exported_at, "vehicle": tag,
                "param_count": params.len(), "params": params,
            });
            serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n"
        }
    };
    match write_file(Path::new(&target_dir), &filename, &text) {
        Ok(path) => Response::json(json!({
            "ok": true, "path": path.to_string_lossy(), "dir": target_dir,
            "filename": filename, "param_count": params.len(), "format": format,
        })),
        Err(e) => refuse(400, &format!("could not write to {target_dir}: {}", io_text(&e))),
    }
}

/// `exc.strerror`: the operating system's words without Rust's "(os error N)".
fn io_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_print_as_the_shortest_float32_text() {
        assert_eq!(format_param_value(0.100_000_001_490_116_12, Some(REAL32)), "0.1");
        assert_eq!(format_param_value(5.0, Some(REAL32)), "5");
        assert_eq!(format_param_value(5.0, Some(6)), "5");
        assert_eq!(format_param_value(1.0e-5, Some(REAL32)), "1e-05");
        assert_eq!(format_param_value(123_456_789.0, None), "1.2345679e+08");
        assert_eq!(format_param_value(2.345_678_91, Some(REAL32)), "2.3456789");
        assert_eq!(format_param_value(1.0e10, Some(REAL32)), "1e+10");
        assert_eq!(format_param_value(0.000_123_4, Some(REAL32)), "0.0001234");
        assert_eq!(format_param_value(-2.5, None), "-2.5");
        assert_eq!(format_param_value(f64::NAN, None), "nan");
    }
}
