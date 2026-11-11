//! The MAVLink console (`routes/console.py`): the command line of the
//! console page, the transcript save and the console stream.
//!
//! The `console` topic fans the main bridge's console lines (STATUSTEXT, the
//! results of commands, shell output) out to every connected stream through
//! one listener, because a bridge listener cannot be taken off again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::buffer::Sink;
use crate::core::Core;
use crate::events::{Binding, Topic};
use crate::fleet::MAIN_LINK;
use crate::module::Registry;
use crate::peripherals::util::py_strip;
use crate::protocol::common::{py_float_of, TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M};
use crate::request::Request;
use crate::response::Response;
use crate::vehicle::VehicleService;

/// How many console lines a slow reader may hold (`CONSOLE_SSE_CAPACITY`).
pub const CONSOLE_SSE_CAPACITY: usize = 100;

/// PX4 NuttShell builtins with no MAVLink command equivalent: routed to the
/// serial control debug shell.
const SHELL_COMMANDS: [&str; 8] = ["listener", "top", "free", "dmesg", "tasks", "perf", "boot_log", "hrt"];

/// The streams listening to the console, and the id the next one gets.
#[derive(Default)]
pub struct ConsoleHub {
    sinks: Mutex<Vec<(u64, Sink<Value>)>>,
    next: AtomicU64,
}

impl ConsoleHub {
    fn publish(&self, name: &str, text: &str, level: &str) {
        let sinks: Vec<Sink<Value>> = self
            .sinks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        for s in sinks {
            s.put_console(json!({"name": name, "text": text, "level": level}));
        }
    }

    fn add(&self, sink: Sink<Value>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.sinks.lock().unwrap_or_else(|e| e.into_inner()).push((id, sink));
        id
    }

    fn remove(&self, id: u64) {
        self.sinks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(i, _)| *i != id);
    }

    /// Feeds the hub from the main bridge's console, once.
    pub fn attach(self: &Arc<Self>, vehicle: &VehicleService) {
        if let Some(h) = vehicle.handles(MAIN_LINK) {
            let hub = Arc::clone(self);
            h.bridge
                .add_console_listener(move |name, text, level| hub.publish(name, text, level));
        }
    }
}

pub fn register(reg: &mut Registry, hub: Arc<ConsoleHub>) {
    reg.topic(Topic::new("console", CONSOLE_SSE_CAPACITY, move |_core, sink, _job| {
        let id = hub.add(sink);
        let hub = Arc::clone(&hub);
        Binding {
            unbind: Box::new(move || hub.remove(id)),
            initial: None,
        }
    }));
    reg.post("/api/console/command", api_command);
    reg.post("/api/console/save", api_save);
    reg.get("/api/console/stream", api_stream);
}

/// `_parse_takeoff_altitude`.
fn parse_takeoff_altitude(value: &str) -> Result<f64, String> {
    let altitude = py_float_of(&Value::String(value.to_string())).ok_or("takeoff altitude must be a number")?;
    if !altitude.is_finite() {
        return Err("takeoff altitude must be finite".into());
    }
    if !(TAKEOFF_ALTITUDE_MIN_M..=TAKEOFF_ALTITUDE_MAX_M).contains(&altitude) {
        return Err(format!(
            "takeoff altitude must be between {TAKEOFF_ALTITUDE_MIN_M:.0} and {TAKEOFF_ALTITUDE_MAX_M:.0} m AGL"
        ));
    }
    Ok(altitude)
}

fn shell_failure(error: String, cmd: &str) -> Response {
    let error = if error.is_empty() {
        format!("shell command failed: {cmd}")
    } else {
        error
    };
    let lower = error.to_lowercase();
    let status = if lower.contains("disconnected") || lower.contains("not connected") {
        503
    } else {
        409
    };
    Response::json_status(json!({"ok": false, "error": error}), status)
}

fn api_command(core: &Core, req: &Request) -> Response {
    let cmd = match req.payload().get("command") {
        Some(Value::String(s)) => py_strip(s).to_string(),
        _ => String::new(),
    };
    let Some(svc) = core.service::<VehicleService>() else {
        return Response::error(400, "no command or not connected");
    };
    if cmd.is_empty() {
        return Response::error(400, "no command or not connected");
    }
    let bridge = svc.active().bridge;
    let lower = cmd.to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    let ok = match parts[0] {
        "arm" => bridge.arm(true),
        "disarm" => bridge.arm(false),
        "mode" if parts.len() > 1 => bridge.set_mode(&parts[1].to_uppercase()),
        "takeoff" => {
            if parts.len() > 2 {
                return Response::error(400, "usage: takeoff [altitude_agl]");
            }
            let altitude = match parts.get(1) {
                Some(text) => parse_takeoff_altitude(text),
                None => Ok(10.0),
            };
            match altitude {
                Ok(a) => bridge.takeoff(a),
                Err(e) => return Response::error(400, &e),
            }
        }
        "land" => bridge.land(),
        "rtl" => bridge.rtl(),
        "help" => return Response::json(json!({"ok": true, "help": true})),
        "shell" | "nsh" => {
            // The prefix comes off the original text so the rest keeps its
            // case: NSH is case sensitive.
            let rest = cmd
                .split_once(char::is_whitespace)
                .map(|(_, r)| py_strip(r))
                .unwrap_or("");
            if rest.is_empty() {
                return Response::error(400, &format!("usage: {} <command>", parts[0]));
            }
            return if bridge.shell_command(rest) {
                Response::json(json!({"ok": true, "shell": true}))
            } else {
                shell_failure(bridge.last_error(), &cmd)
            };
        }
        name if SHELL_COMMANDS.contains(&name) => {
            return if bridge.shell_command(&cmd) {
                Response::json(json!({"ok": true, "shell": true}))
            } else {
                shell_failure(bridge.last_error(), &cmd)
            };
        }
        _ => return Response::error(400, &format!("unknown command: {cmd}")),
    };
    if ok {
        return Response::json(json!({"ok": true}));
    }
    let error = bridge.last_error();
    let error = if error.is_empty() {
        format!("command failed: {cmd}")
    } else {
        error
    };
    let status = if error.contains("DISCONNECTED") { 503 } else { 409 };
    Response::json_status(json!({"ok": false, "error": error}), status)
}

/// `_safe_filename`: one safe file name, or `fallback`.
fn safe_filename(raw: Option<&Value>, fallback: &str, suffix: &str) -> String {
    let Some(Value::String(raw)) = raw else {
        return fallback.to_string();
    };
    let stripped = py_strip(raw);
    let base = if cfg!(windows) {
        stripped.rsplit(['/', '\\']).next().unwrap_or("")
    } else {
        stripped.rsplit('/').next().unwrap_or("")
    };
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, '\0'..='\x1f' | '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect();
    let mut name = py_strip(&cleaned).to_string();
    if name.is_empty() || name == "." || name == ".." {
        return fallback.to_string();
    }
    if name.to_lowercase().ends_with(&suffix.to_lowercase()) {
        let keep = name.chars().count() - suffix.chars().count();
        name = name.chars().take(keep).collect();
    }
    let stem: String = name.chars().take(115).collect();
    let stem = stem.trim_end().to_string();
    let stem = if stem.is_empty() { fallback.to_string() } else { stem };
    format!("{stem}{suffix}")
}

/// `os.path.expanduser` for a leading `~`.
fn expand_user(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return Path::new(&home).join(path.trim_start_matches("~").trim_start_matches('/'));
        }
    }
    PathBuf::from(path)
}

fn write_private(dir: &Path, filename: &str, text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    static N: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(dir)?;
    let target = dir.join(filename);
    let temp = dir.join(format!(
        "{filename}.{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        drop(file);
        std::fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map(|()| target)
}

/// Writes the console transcript beside the tlogs and answers where: server
/// side for the same reason parameter export is, since a desktop webview drops
/// an `<a download>`.
fn api_save(core: &Core, req: &Request) -> Response {
    let text = match req.payload().get("text") {
        Some(Value::String(s)) if !py_strip(s).is_empty() => s.as_str(),
        _ => return Response::not_ok(400, "text must be a non-empty string"),
    };
    let cfg = core.settings().get();
    let configured = py_strip(cfg.str_field("tlog_dir"));
    let target_dir = if configured.is_empty() {
        core.paths().join("logs")
    } else {
        expand_user(configured)
    };
    let now = crate::clock::local_datetime();
    let fallback = format!(
        "corvus-console_{:04}-{:02}-{:02}_{:02}-{:02}.log",
        now.year, now.month, now.day, now.hour, now.minute
    );
    let filename = safe_filename(req.payload().get("filename"), &fallback, ".log");
    match write_private(&target_dir, &filename, text) {
        Ok(path) => Response::json(json!({
            "ok": true,
            "path": path.to_string_lossy(),
            "dir": target_dir.to_string_lossy(),
            "filename": filename,
        })),
        Err(e) => Response::not_ok(
            400,
            &format!(
                "could not write to {}: {}",
                target_dir.display(),
                crate::peripherals::util::os_error_text(&e)
            ),
        ),
    }
}

/// STATUSTEXT as its own stream, under the event name `message`.
fn api_stream(core: &Core, _req: &Request) -> Response {
    Response::sse(core.topic_stream(&[("console", "message")], None, &[]))
}
