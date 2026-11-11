//! The routes of `routes/logs.py` and the log review upload of `server.py`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::flight_review::{normalize_sensitivity, ReviewError};
use super::service::{strerror, LogService};
use super::{flight_review, tlog_review, Logs};
use crate::core::Core;
use crate::module::Registry;
use crate::request::Request;
use crate::response::Response;
use crate::router::RouteOptions;

/// `MAX_ULOG_BODY_BYTES` of `server.py`.
const MAX_ULOG_BODY_BYTES: usize = 128 * 1024 * 1024;

pub(super) fn register(reg: &mut Registry) {
    reg.get("/api/logs/status", api_status);
    reg.post("/api/logs/refresh", api_refresh);
    reg.post("/api/logs/download", api_download);
    reg.get("/api/logs/review", api_review);
    reg.get("/api/logs/tlog-review", api_tlog_review);
    reg.post("/api/logs/erase", api_erase);
    reg.post("/api/logs/cancel", api_cancel);
    reg.post("/api/logs/dir", api_dir);
    reg.post_with(
        "/api/logs/review/upload",
        RouteOptions::raw(MAX_ULOG_BODY_BYTES),
        api_review_upload,
    );
}

fn service(core: &Core) -> Option<Arc<LogService>> {
    core.service::<Logs>().and_then(|l| l.service())
}

fn unavailable() -> Response {
    Response::not_ok(503, "log service unavailable")
}

/// `self.logs.<job>()` refused: 503 when there is no link, else 409.
fn refused(service: &LogService, fallback: &str) -> Response {
    let error = service.last_error();
    let error = if error.is_empty() { fallback.to_string() } else { error };
    let status = if error.contains("not connected") { 503 } else { 409 };
    Response::not_ok(status, &error)
}

fn api_status(core: &Core, _req: &Request) -> Response {
    match service(core) {
        Some(logs) => Response::json(logs.status()),
        None => Response::json(json!({
            "state": "idle", "message": "", "percent": 0, "current": null,
            "queued": [], "completed": [], "logs": [], "tlogs": [],
            "dir": "", "connected": false,
        })),
    }
}

fn api_refresh(core: &Core, _req: &Request) -> Response {
    let Some(logs) = service(core) else {
        return unavailable();
    };
    if logs.refresh() {
        return Response::json(json!({"ok": true, "state": "listing"}));
    }
    refused(&logs, "could not list logs")
}

fn api_download(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let Some(raw) = payload.get("ids").and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return Response::not_ok(400, "ids must be a non-empty list");
    };
    let mut ids: Vec<i64> = Vec::new();
    for item in raw {
        match item {
            Value::Number(n) => match n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)) {
                Some(i) => ids.push(i),
                None => return Response::not_ok(400, "ids must be numbers"),
            },
            _ => return Response::not_ok(400, "ids must be numbers"),
        }
    }
    let Some(logs) = service(core) else {
        return unavailable();
    };
    if logs.start_download(&ids) {
        return Response::json(json!({"ok": true, "state": "downloading", "queued": ids.len()}));
    }
    refused(&logs, "download refused")
}

/// The Flight Review sensitivity the operator set in Settings, read per
/// request so a change applies to the next review without a restart.
fn review_sensitivity(core: &Core) -> &'static str {
    let config = core.settings().get();
    let value = config
        .block("review")
        .and_then(|b| b.get("sensitivity"))
        .cloned()
        .unwrap_or(Value::Null);
    normalize_sensitivity(&value)
}

/// `os.path.realpath`: symlinks resolved, a missing tail kept as written.
fn realpath(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut tail = Vec::new();
    let mut head = absolute.as_path();
    while let Some(parent) = head.parent() {
        if let Some(name) = head.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(real) = std::fs::canonicalize(parent) {
            let mut out = real;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        head = parent;
    }
    absolute
}

/// `os.path.basename`.
fn basename(name: &str) -> &str {
    let cut = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    name.rsplit(cut).next().unwrap_or("")
}

/// Python's `str.strip()`.
fn strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
}

/// A file named in `?file=`, confined to `configured` the way the Python does:
/// the name is never a path, and the resolved target must stay inside.
enum Pick {
    Refused(Response),
    Target(PathBuf),
}

fn pick(configured: &str, name: &str, suffixes: &[&str], unconfigured: &str, unknown: &str) -> Pick {
    // Emptiness is tested before realpath: realpath("") is the working folder.
    if configured.is_empty() {
        return Pick::Refused(Response::not_ok(400, unconfigured));
    }
    let directory = realpath(Path::new(configured));
    let target = realpath(&directory.join(basename(name)));
    if !target.starts_with(&directory) {
        return Pick::Refused(Response::not_ok(400, unknown));
    }
    let text = target.to_string_lossy();
    if !suffixes.iter().any(|s| text.ends_with(s)) || !target.is_file() {
        return Pick::Refused(Response::not_ok(404, unknown));
    }
    Pick::Target(target)
}

fn with_ok(data: &Value) -> Response {
    let mut out = data.clone();
    if let Value::Object(map) = &mut out {
        map.insert("ok".into(), Value::Bool(true));
    }
    Response::json(out)
}

fn requested_file(req: &Request) -> String {
    strip(req.query.get("file").unwrap_or("")).to_string()
}

fn api_review(core: &Core, req: &Request) -> Response {
    let name = requested_file(req);
    if name.is_empty() {
        return Response::not_ok(400, "file is required");
    }
    let Some(logs) = service(core) else {
        return unavailable();
    };
    let configured = logs.status()["dir"].as_str().unwrap_or("").to_string();
    // .bin is accepted here and refused by the parser with a sentence that says
    // what the file actually is.
    let target = match pick(
        &configured,
        &name,
        &super::service::LOG_SUFFIXES,
        "no download folder configured",
        "unknown log file",
    ) {
        Pick::Refused(r) => return r,
        Pick::Target(t) => t,
    };
    let label = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sensitivity = review_sensitivity(core);
    let run = std::panic::catch_unwind(|| flight_review::review_file(&target, &label, sensitivity));
    match run {
        Ok(Ok(data)) => with_ok(&data),
        Ok(Err(ReviewError::Ulog(text))) => Response::not_ok(400, &text),
        Ok(Err(ReviewError::Io(text))) => Response::not_ok(400, &format!("could not read the log ({text})")),
        Err(_) => Response::not_ok(400, "could not analyse this log"),
    }
}

fn api_tlog_review(core: &Core, req: &Request) -> Response {
    let name = requested_file(req);
    if name.is_empty() {
        return Response::not_ok(400, "file is required");
    }
    let Some(logs) = service(core) else {
        return unavailable();
    };
    let configured = logs.resolve_tlog_dir();
    let target = match pick(
        &configured,
        &name,
        &[".tlog"],
        "no recording folder configured",
        "unknown recording",
    ) {
        Pick::Refused(r) => return r,
        Pick::Target(t) => t,
    };
    let label = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sensitivity = review_sensitivity(core);
    let run = std::panic::catch_unwind(|| tlog_review::review_file(&target, &label, sensitivity));
    match run {
        Ok(Ok(data)) => with_ok(&data),
        Ok(Err(e)) => Response::not_ok(400, &e.0),
        Err(_) => Response::not_ok(400, "could not analyse that recording"),
    }
}

fn api_erase(core: &Core, _req: &Request) -> Response {
    let Some(logs) = service(core) else {
        return unavailable();
    };
    if logs.erase() {
        return Response::json(json!({"ok": true, "state": "erasing"}));
    }
    refused(&logs, "erase refused")
}

fn api_cancel(core: &Core, _req: &Request) -> Response {
    let Some(logs) = service(core) else {
        return unavailable();
    };
    if logs.cancel() {
        Response::json(json!({"ok": true}))
    } else {
        Response::json(json!({"ok": false, "error": "no log operation in progress"}))
    }
}

/// `os.access(path, os.W_OK)`.
fn writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        match std::ffi::CString::new(path.as_os_str().as_bytes()) {
            // SAFETY: a valid NUL terminated path.
            Ok(c) => unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 },
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false)
    }
}

/// Sets the download folder and persists it for future connections.
fn api_dir(core: &Core, req: &Request) -> Response {
    let value = req
        .payload()
        .get("dir")
        .and_then(Value::as_str)
        .filter(|s| !strip(s).is_empty());
    let Some(value) = value else {
        return Response::not_ok(400, "dir must be a non-empty string");
    };
    let path = crate::mapdata::util::expand_user(strip(value));
    let made = std::fs::create_dir_all(&path).and_then(|_| {
        if writable(Path::new(&path)) {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "not writable",
            ))
        }
    });
    if let Err(e) = made {
        return Response::not_ok(400, &format!("cannot use {path} ({})", strerror(&e)));
    }
    let saved = core.settings().update(core, |cfg| {
        cfg.set("log_download_dir", Some(Value::String(path.clone())));
    });
    if saved.is_err() {
        return Response::json(json!({
            "ok": true, "dir": path,
            "warning": "folder set for this session but not saved",
        }));
    }
    Response::json(json!({"ok": true, "dir": path}))
}

/// Flight Review for a ULog the operator picked from anywhere on disk. The
/// bytes arrive as the request body, so the backend never gains the ability to
/// read an arbitrary path on request.
fn api_review_upload(core: &Core, req: &Request) -> Response {
    let raw = &req.body;
    if raw.is_empty() {
        return Response::not_ok(400, "empty log body");
    }
    let name = strip(req.query.get("name").unwrap_or(""));
    let name = match basename(name) {
        "" => "log.ulg",
        n => n,
    };
    let sensitivity = review_sensitivity(core);
    let run = std::panic::catch_unwind(|| flight_review::review_bytes(raw, name, sensitivity));
    match run {
        Ok(Ok(data)) => with_ok(&data),
        Ok(Err(ReviewError::Ulog(text))) => Response::not_ok(400, &text),
        Ok(Err(ReviewError::Io(text))) => Response::not_ok(400, &text),
        Err(_) => Response::not_ok(400, "could not analyse this log"),
    }
}
