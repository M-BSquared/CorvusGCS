//! Settings export, import and reset, and the company logo
//! (`routes/settings.py` and the branding routes of `server.py`).
//!
//! The bundle format itself is [`crate::bundle`]; this module reads and writes
//! the files around it: the plugins' own files and settings, the logo under
//! `branding/` beside the config, and the export file.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::config::{self, io_text};
use super::files;
use super::registry::{is_valid_id, PluginRegistry};
use super::routes::{registry, settings_all};
use crate::bundle::{self, BuildOptions};
use crate::config::{truthy, CorvusConfig};
use crate::core::Core;
use crate::json::Map;
use crate::module::Registry;
use crate::request::Request;
use crate::response::Response;
use crate::router::{RouteOptions, MAX_SETTINGS_BODY};

/// The plugins one export may carry together, before base64.
pub const MAX_EXPORT_PLUGIN_BYTES: usize = 16 * 1024 * 1024;

/// Config keys read once at startup (the listening socket, the tile cache, the
/// tlog recorder, the firmware catalog): an import that changes one says a
/// restart is needed rather than pretending it took effect.
const IMPORT_RESTART_KEYS: [&str; 4] = ["http_port", "tile_cache_dir", "tlog_dir", "firmware_dir"];

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

pub(super) fn register(reg: &mut Registry) {
    reg.get("/api/settings/export/target", api_export_target);
    reg.post("/api/settings/export", api_export);
    reg.post_with(
        "/api/settings/import",
        RouteOptions::json(MAX_SETTINGS_BODY),
        api_import,
    );
    reg.post("/api/settings/reset", api_reset);
    reg.get("/api/branding/logo", api_logo);
    reg.post_with(
        "/api/branding/logo",
        RouteOptions::raw(MAX_SETTINGS_BODY),
        api_logo_upload,
    );
    reg.post("/api/branding/logo/remove", api_logo_remove);
}

// ---------------------------------------------------------------- paths

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `os.path.expanduser`: a leading `~` is the home folder.
fn expand_user(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || (cfg!(windows) && rest.starts_with('\\')) => {
            format!("{}{rest}", home_dir().display())
        }
        _ => path.to_string(),
    }
}

/// Where a settings export goes by default: Downloads, else the home folder.
/// Not under `~/.corvus`: this file is made to be carried to another machine,
/// and a hidden folder is where it gets lost.
pub fn export_dir() -> String {
    let home = home_dir();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
        .to_string_lossy()
        .into_owned()
}

/// Lowercase, filename safe slug of `value` ("" when there is nothing).
pub fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes at most buf.len() bytes into the buffer.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
        String::new()
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// `corvus-settings_<host>_<YYYY-MM-DD_HH-MM>.json`: which station, and when.
pub fn default_filename() -> String {
    let t = crate::clock::local_datetime();
    let stamp = format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}",
        t.year, t.month, t.day, t.hour, t.minute
    );
    let host = slugify(hostname().split('.').next().unwrap_or(""));
    if host.is_empty() {
        format!("corvus-settings_{stamp}.json")
    } else {
        format!("corvus-settings_{host}_{stamp}.json")
    }
}

/// `_safe_filename`: one safe file name, or `fallback`. A directory part is
/// dropped, the empty and dot names are refused and `suffix` is forced; the stem
/// is cut, not the suffix.
pub fn safe_filename(raw: Option<&Value>, fallback: &str, suffix: &str) -> String {
    let Some(Value::String(raw)) = raw else {
        return fallback.to_string();
    };
    let trimmed = raw.trim();
    let base = if cfg!(windows) {
        trimmed.rsplit(['/', '\\']).next().unwrap_or("")
    } else {
        trimmed.rsplit('/').next().unwrap_or("")
    };
    let cleaned: String = base
        .chars()
        .filter(|c| !((*c as u32) < 0x20 || "<>:\"/\\|?*".contains(*c)))
        .collect();
    let mut name = cleaned.trim().to_string();
    if name.is_empty() || name == "." || name == ".." {
        return fallback.to_string();
    }
    if name.to_lowercase().ends_with(&suffix.to_lowercase()) {
        let keep = name.chars().count() - suffix.chars().count();
        name = name.chars().take(keep).collect();
    }
    let cut: String = name.chars().take(115).collect();
    let cut = cut.trim_end();
    let stem = if cut.is_empty() { fallback } else { cut };
    format!("{stem}{suffix}")
}

fn logo_path(core: &Core) -> PathBuf {
    let config = core.settings().path().to_path_buf();
    config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("branding")
        .join("logo.png")
}

/// Writes `data` to `target` through a temp file beside it, mode 0600 as
/// `mkstemp` makes it, then renames it over.
fn write_atomic(target: &Path, prefix: &str, data: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = config::tmp_file(dir, prefix);
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        drop(f);
        std::fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn armed(core: &Core) -> bool {
    core.store().get("armed").is_some_and(|v| truthy(&v))
}

fn refusal(status: u16, message: impl AsRef<str>) -> Response {
    Response::not_ok(status, message.as_ref())
}

// --------------------------------------------------------------- export

fn api_export_target(_core: &Core, _req: &Request) -> Response {
    Response::json(json!({"dir": export_dir(), "filename": default_filename()}))
}

/// The files of each plugin named in `raw`, or the sentence that says why not.
/// Only plugins from the operator's folder: a bundled one ships with Corvus,
/// and a name that is not installed has no files to take.
fn export_plugin_files(
    reg: &PluginRegistry,
    raw: Option<&Value>,
    wanted: bool,
) -> Result<BTreeMap<String, BTreeMap<String, Vec<u8>>>, String> {
    let Some(raw) = raw.filter(|r| !r.is_null() && wanted) else {
        return Ok(BTreeMap::new());
    };
    let ids: Vec<&str> = match raw {
        Value::Array(a) if a.iter().all(Value::is_string) => a.iter().filter_map(Value::as_str).collect(),
        _ => return Err("plugin_files must be a list of plugin ids".into()),
    };
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let installed = reg.discover();
    let mut out = BTreeMap::new();
    let mut total = 0usize;
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        let Some(manifest) = installed.iter().find(|m| m.id == id).filter(|m| m.source == "user") else {
            return Err(format!(
                "The plugin {id} is not in the plugin folder, so its files cannot be exported."
            ));
        };
        let collected = match files::collect(&manifest.dir) {
            Ok(f) => f,
            Err(files::FilesError::Value(why)) => {
                return Err(format!(
                    "The plugin {} is {why}, too much for a settings file. Copy its folder instead.",
                    manifest.name
                ))
            }
            Err(files::FilesError::Io(e)) => {
                return Err(format!(
                    "The plugin {} could not be read: {}",
                    manifest.name,
                    io_text(&e)
                ))
            }
        };
        total += collected.values().map(Vec::len).sum::<usize>();
        if total > MAX_EXPORT_PLUGIN_BYTES {
            return Err(format!(
                "The chosen plugins are larger than {} MB together. Take fewer of them, or copy their folders instead.",
                MAX_EXPORT_PLUGIN_BYTES / (1024 * 1024)
            ));
        }
        out.insert(id.to_string(), collected.into_iter().collect());
    }
    Ok(out)
}

fn api_export(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let include_secrets = match payload.get("include_secrets") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return refusal(400, "include_secrets must be true or false"),
    };
    let browser = match payload.get("browser") {
        None => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return refusal(400, "browser must be an object"),
    };
    let sections = match bundle::normalize_sections(payload.get("sections")) {
        Ok(s) => s,
        Err(e) => return refusal(400, e),
    };
    if sections.is_empty() {
        return refusal(400, "Choose at least one part to export.");
    }
    let reg = registry(core);
    let plugin_files = match export_plugin_files(
        &reg,
        Some(payload.get("plugin_files").unwrap_or(&json!([]))),
        sections.contains("plugins"),
    ) {
        Ok(f) => f,
        Err(e) => return refusal(400, e),
    };
    let target_dir = match payload.get("dir") {
        Some(Value::String(d)) if !d.trim().is_empty() => d.clone(),
        _ => export_dir(),
    };
    let target_dir = expand_user(target_dir.trim());
    let filename = safe_filename(payload.get("filename"), &default_filename(), ".json");

    let cfg = core.settings().get();
    let config_dict = cfg.to_dict();
    let mut plugins = settings_all(core, reg.user_dir());
    let available: Vec<&str> = plugins.keys().map(String::as_str).collect();
    let chosen = match bundle::normalize_ids(payload.get("plugin_settings"), &available, "plugin_settings") {
        Ok(c) => c,
        Err(e) => return refusal(400, e),
    };
    plugins.retain(|k, _| chosen.contains(k));
    let logo = cfg
        .block("branding")
        .filter(|b| b.get("logo").is_some_and(truthy))
        .and_then(|_| std::fs::read(logo_path(core)).ok())
        .filter(|l| !l.is_empty());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut order: Vec<String> = sections.iter().cloned().collect();
    order.sort();
    let built = bundle::build(
        &config_dict,
        &BuildOptions {
            sections: Some(order),
            plugins,
            plugin_files,
            logo,
            browser,
            include_secrets,
            exported_at: bundle::utc_iso(now),
            version: crate::version::version().to_string(),
        },
    );
    let text = format!("{}\n", config::dump(&built));
    let path = Path::new(&target_dir).join(&filename);
    // The file is as sensitive as the config itself with the secrets in it:
    // write_atomic makes it 0600.
    if let Err(e) = write_atomic(&path, &format!("{filename}."), text.as_bytes()) {
        return refusal(400, format!("could not write to {target_dir}: {}", io_text(&e)));
    }
    let sections_out = built.get("sections").cloned().unwrap_or(json!([]));
    Response::json(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "dir": target_dir,
        "filename": filename,
        "secrets": include_secrets,
        "sections": sections_out,
    }))
}

// --------------------------------------------------------------- import

fn api_import(core: &Core, req: &Request) -> Response {
    if armed(core) {
        return refusal(409, "Settings cannot be imported while the vehicle is armed.");
    }
    let payload = req.payload();
    let check = |f: BTreeMap<String, Vec<u8>>| {
        files::check(&f.into_iter().collect())
            .map(|ok| ok.into_iter().collect())
            .map_err(|e| e.to_string())
    };
    let bundle = match bundle::parse(payload.get("bundle").unwrap_or(&Value::Null), &check) {
        Ok(b) => b,
        Err(e) => return refusal(400, e),
    };
    let mut sections = match bundle::normalize_sections(payload.get("sections")) {
        Ok(s) => s,
        Err(e) => return refusal(400, e),
    };
    let setting_ids: Vec<&str> = bundle.plugins.keys().map(String::as_str).collect();
    let mut take_settings = match bundle::normalize_ids(payload.get("plugin_settings"), &setting_ids, "plugin_settings")
    {
        Ok(s) => s,
        Err(e) => return refusal(400, e),
    };
    let file_ids: Vec<&str> = bundle.plugin_files.keys().map(String::as_str).collect();
    let mut take_files = match bundle::normalize_ids(payload.get("plugin_files"), &file_ids, "plugin_files") {
        Ok(s) => s,
        Err(e) => return refusal(400, e),
    };
    sections.retain(|s| bundle.sections.contains(s));
    if !sections.contains("plugins") {
        take_settings.clear();
        take_files.clear();
    }
    if sections.is_empty() {
        return refusal(400, "Choose at least one part to import.");
    }

    let settings = core.settings();
    let coercers = settings.coercers();
    let user_dir = registry(core).user_dir().to_path_buf();
    let logo_file = logo_path(core);
    let mut warnings: Vec<String> = Vec::new();
    let (mut written, mut installed) = (0usize, 0usize);
    let mut before = Map::new();
    let mut after = Map::new();
    let mut ordered: Vec<&str> = bundle::SECTIONS
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| sections.contains(*n))
        .collect();
    ordered.sort();

    let saved = settings.update(core, |cfg| {
        before = cfg.to_dict();
        let mut merged = bundle::merge(&before, &bundle.config, &ordered, bundle.secrets);
        let interface = sections.contains("interface");
        let logo = if interface { bundle.logo.as_ref() } else { None };
        if interface && logo.is_none() {
            // A logo name with no picture behind it would draw a broken image.
            if let Some(Value::Object(branding)) = merged.get("branding") {
                let mut branding = branding.clone();
                branding.shift_remove("logo");
                if branding.is_empty() {
                    merged.shift_remove("branding");
                } else {
                    merged.insert("branding".into(), Value::Object(branding));
                }
            }
        }
        let mut next = crate::config::build(&merged, &coercers);
        if interface {
            let stored = match logo {
                Some(bytes) => write_atomic(&logo_file, "logo.png.", bytes),
                None => match std::fs::remove_file(&logo_file) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                    _ => Ok(()),
                },
            };
            if let Err(e) = stored {
                eprintln!("corvus: settings import: company logo not stored: {e}");
                warnings.push("The company logo could not be stored.".into());
                next.set("branding", cfg.get("branding").cloned());
            }
        }
        next.set("plugins", cfg.get("plugins").cloned());
        if sections.contains("plugins") {
            // Files before settings: an install keeps the settings the
            // installed copy had, and the file's own settings, when chosen,
            // then replace them.
            let mut ids: Vec<&String> = take_files.iter().collect();
            ids.sort();
            for id in ids {
                let files: files::Files = bundle.plugin_files[id].clone().into_iter().collect();
                match files::install(id, &files, &user_dir) {
                    Ok(_) => installed += 1,
                    Err(e) => {
                        eprintln!("corvus: settings import: plugin {id} not installed: {e}");
                        warnings.push(format!("The plugin {id} could not be installed."));
                    }
                }
            }
            let mut legacy = cfg.block("plugins").cloned().unwrap_or_default();
            for (id, plugin_settings) in &bundle.plugins {
                if !take_settings.contains(id) || !is_valid_id(id) {
                    continue;
                }
                let Some(plugin_settings) = plugin_settings.as_object() else {
                    continue;
                };
                match config::save(id, plugin_settings, &user_dir) {
                    Ok(()) => {
                        written += 1;
                        legacy.shift_remove(id);
                    }
                    Err(e) => {
                        eprintln!("corvus: settings import: plugin {id} not stored: {e}");
                        warnings.push(format!("The settings of plugin {id} could not be stored."));
                    }
                }
            }
            next.set("plugins", (!legacy.is_empty()).then_some(Value::Object(legacy)));
        }
        after = next.to_dict();
        *cfg = next;
    });
    if let Err(e) = saved {
        eprintln!("corvus: settings import: config not saved: {e}");
        warnings.push("Applied for this session but not saved.".into());
    }
    super::routes::settings_reloaded(core);
    let restart: Vec<&str> = IMPORT_RESTART_KEYS
        .iter()
        .copied()
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    settings.finish_first_start();
    let mut sections_out: Vec<String> = sections.into_iter().collect();
    sections_out.sort();
    Response::json(json!({
        "ok": true,
        "config": settings.get().to_public(),
        "sections": sections_out,
        "plugins": written,
        "plugins_installed": installed,
        "restart": restart,
        "warnings": warnings,
    }))
}

// ---------------------------------------------------------------- reset

fn api_reset(core: &Core, _req: &Request) -> Response {
    if armed(core) {
        return refusal(409, "Settings cannot be reset while the vehicle is armed.");
    }
    let settings = core.settings();
    let mut warnings: Vec<String> = Vec::new();
    let before = settings.get().to_dict();
    // The listeners hear the defaults (the forwarder, the battery estimate, the
    // Remote ID identity all follow them) ...
    let _ = settings.update(core, |cfg| *cfg = CorvusConfig::default());
    let after = settings.get().to_dict();
    // ... and the file goes, so the next start is a first start as well.
    match std::fs::remove_file(settings.path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("corvus: settings reset: config file not removed: {e}");
            warnings.push("The config file could not be removed. The next start may skip the setup.".into());
        }
        _ => {}
    }
    match std::fs::remove_file(logo_path(core)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("corvus: settings reset: company logo not removed: {e}");
            warnings.push("The company logo could not be removed.".into());
        }
        _ => {}
    }
    let user_dir = registry(core).user_dir().to_path_buf();
    for (id, _) in config::load_all(&user_dir) {
        let Some(path) = config::config_path(&id, &user_dir) else {
            continue;
        };
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("corvus: settings reset: plugin {id} settings not removed: {e}");
                warnings.push(format!("The settings of plugin {id} could not be removed."));
            }
            _ => {}
        }
    }
    super::routes::settings_reloaded(core);
    let restart: Vec<&str> = IMPORT_RESTART_KEYS
        .iter()
        .copied()
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    settings.load();
    Response::json(json!({
        "ok": true,
        "config": settings.get().to_public(),
        "restart": restart,
        "warnings": warnings,
    }))
}

// ----------------------------------------------------------------- logo

fn api_logo(core: &Core, _req: &Request) -> Response {
    match std::fs::read(logo_path(core)) {
        Ok(blob) if !blob.is_empty() => {
            Response::bytes(200, "image/png", blob).with_header("Cache-Control", "no-store")
        }
        _ => Response::error(404, "no company logo"),
    }
}

fn api_logo_upload(core: &Core, req: &Request) -> Response {
    let raw = &req.body;
    if raw.is_empty() {
        return refusal(400, "empty logo body");
    }
    if raw.len() > bundle::MAX_LOGO_BYTES {
        return refusal(413, "logo too large (max 4 MB)");
    }
    if !raw.starts_with(PNG_MAGIC) {
        return refusal(400, "logo must be a PNG image");
    }
    // Only the basename is kept: this is display metadata, never a path.
    let name = req.query.get("name").unwrap_or("").replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or("").trim().to_string();
    let name: String = name.chars().take(120).collect();
    let name = if name.is_empty() { "logo.png".to_string() } else { name };
    if let Err(e) = write_atomic(&logo_path(core), "logo.png.", raw) {
        eprintln!("corvus: company logo write failed: {e}");
        return refusal(500, "could not store logo");
    }
    let partial: Map = json!({"branding": {"logo": name}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    match core.settings().apply_partial(core, &partial) {
        Ok(public) => Response::json(json!({"ok": true, "logo": name, "config": public})),
        Err(e) => refusal(400, e),
    }
}

fn api_logo_remove(core: &Core, _req: &Request) -> Response {
    match std::fs::remove_file(logo_path(core)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("corvus: company logo delete failed: {e}");
            return refusal(500, "could not remove logo");
        }
        _ => {}
    }
    let partial: Map = json!({"branding": {}}).as_object().cloned().unwrap_or_default();
    match core.settings().apply_partial(core, &partial) {
        Ok(public) => Response::json(json!({"ok": true, "config": public})),
        Err(e) => refusal(400, e),
    }
}
