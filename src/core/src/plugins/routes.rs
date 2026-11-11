//! The plugin routes: the list, a plugin's own files, its saved settings and
//! the operator's folder (`routes` of `server.py` that name a plugin).
//!
//! Plugin settings are files of their own under `~/.corvus/plugins/<id>`
//! ([`super::config`]); the main config's old `plugins` key only fills in for a
//! plugin that has no file yet, and is emptied as plugins are saved.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use super::config;
use super::file_manager;
use super::registry::{is_valid_id, resolve_asset, PluginRegistry};
use crate::core::Core;
use crate::http::static_files::content_type;
use crate::json::Map;
use crate::module::Registry;
use crate::request::Request;
use crate::response::Response;

/// What opens a folder for the operator; the file manager unless a test says otherwise.
pub type Opener = Arc<dyn Fn(&str) -> (bool, String) + Send + Sync>;

/// Where the plugin folders are. `None` is the real place: `~/.corvus/plugins`
/// of the core's state folder, and `plugins` beside the program.
#[derive(Clone)]
pub struct PluginDirs {
    pub user: Option<PathBuf>,
    pub bundled: Option<PathBuf>,
    pub opener: Opener,
    /// Where the update check reads the release list from; GitHub when `None`.
    pub update_url: Option<String>,
    /// What opens a web page; the system browser when `None`.
    pub browser: Option<super::update_check::BrowserOpener>,
}

impl Default for PluginDirs {
    fn default() -> Self {
        PluginDirs {
            user: None,
            bundled: None,
            opener: Arc::new(file_manager::open_folder),
            update_url: None,
            browser: None,
        }
    }
}

/// The bundled plugins' folder: `$CORVUS_PLUGINS_DIR`, else `plugins` beside the program.
fn default_bundled_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CORVUS_PLUGINS_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("plugins")))
        .unwrap_or_else(|| PathBuf::from("plugins"))
}

/// The registry over the folders this core uses.
pub fn registry(core: &Core) -> PluginRegistry {
    let dirs = core.service::<PluginDirs>();
    let user = dirs
        .as_ref()
        .and_then(|d| d.user.clone())
        .unwrap_or_else(|| core.paths().join("plugins"));
    let bundled = dirs
        .as_ref()
        .and_then(|d| d.bundled.clone())
        .unwrap_or_else(default_bundled_dir);
    PluginRegistry::new(user, bundled)
}

/// Every plugin's saved settings by id: each plugin's own file, over whatever
/// is still under the main config's old `plugins` key.
pub fn settings_all(core: &Core, user_dir: &std::path::Path) -> Map {
    let cfg = core.settings().get();
    let mut out = cfg.block("plugins").cloned().unwrap_or_default();
    for (k, v) in config::load_all(user_dir) {
        out.insert(k, v);
    }
    out
}

pub(super) fn register(reg: &mut Registry, dirs: PluginDirs) {
    reg.service(Arc::new(dirs));
    // The plugin folders whose `lang/<code>.json` files join a language, the
    // bundled ones first so a user plugin's text wins (`plugin_language_roots`).
    reg.language_roots(|core| {
        let reg = registry(core);
        vec![reg.bundled_dir().to_path_buf(), reg.user_dir().to_path_buf()]
    });
    reg.get("/api/plugins", api_plugins);
    reg.get_pattern("/api/plugins/asset/{id}/{rest*}", api_plugin_asset);
    reg.post("/api/plugins/settings", api_plugins_settings);
    reg.post("/api/plugins/folder", api_plugins_folder);
}

fn path_text(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

fn api_plugins(core: &Core, _req: &Request) -> Response {
    let reg = registry(core);
    Response::json(json!({
        "plugins": reg.public_list(),
        "user_dir": path_text(reg.user_dir()),
        "bundled_dir": path_text(reg.bundled_dir()),
        "settings": settings_all(core, reg.user_dir()),
    }))
}

fn api_plugin_asset(core: &Core, req: &Request) -> Response {
    let not_found = || Response::error(404, "not found");
    let (Some(id), Some(rel)) = (req.param("id"), req.param("rest")) else {
        return not_found();
    };
    if !is_valid_id(id) || id.ends_with('\n') {
        return not_found();
    }
    let Some(dir) = registry(core).find_dir(id) else {
        return not_found();
    };
    let Some(target) = resolve_asset(&dir, rel) else {
        return not_found();
    };
    match std::fs::read(&target) {
        Ok(body) => {
            Response::bytes(200, content_type(&target.to_string_lossy()), body).with_header("Cache-Control", "no-cache")
        }
        Err(e) => {
            eprintln!("corvus: plugin asset {id}/{rel} unreadable: {e}");
            not_found()
        }
    }
}

fn api_plugins_settings(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let id = match payload.get("id") {
        Some(Value::String(s)) if !s.is_empty() => s.as_str(),
        _ => return Response::error(400, "id must be a non-empty string"),
    };
    if !is_valid_id(id) {
        return Response::error(400, "id is not a valid plugin id");
    }
    let Some(Value::Object(settings)) = payload.get("settings") else {
        return Response::error(400, "settings must be an object");
    };
    let replace = match payload.get("replace") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Response::error(400, "replace must be a boolean"),
    };
    let user_dir = registry(core).user_dir().to_path_buf();
    let legacy = core.settings().get().block("plugins").cloned().unwrap_or_default();
    let mut settings = settings.clone();
    // A merge onto a plugin whose settings were never moved out of the main
    // config starts from those, not from nothing.
    if !replace && legacy.contains_key(id) && config::load(id, &user_dir).is_none() {
        if let Some(Value::Object(base)) = legacy.get(id) {
            let mut merged = base.clone();
            for (k, v) in &settings {
                merged.insert(k.clone(), v.clone());
            }
            settings = merged;
        }
    }
    let saved = match config::update(id, &settings, replace, &user_dir) {
        Ok(saved) => saved,
        Err(e) => {
            eprintln!("corvus: plugin {id}: saving settings failed: {e}");
            return Response::error(500, &format!("could not save settings: {e}"));
        }
    };
    if legacy.contains_key(id) {
        let id = id.to_string();
        let _ = core.settings().update(core, move |cfg| {
            let mut rest = cfg.block("plugins").cloned().unwrap_or_default();
            rest.shift_remove(&id);
            cfg.set("plugins", (!rest.is_empty()).then_some(Value::Object(rest)));
        });
    }
    Response::json(json!({"ok": true, "settings": saved}))
}

fn api_plugins_folder(core: &Core, _req: &Request) -> Response {
    let reg = registry(core);
    let dirs = core.service::<PluginDirs>();
    // The folder is made here and not at startup, so an operator who never
    // opens this never gets a directory they did not ask for. A folder the
    // caller named is theirs and is left as it is.
    let path = match dirs.as_ref().and_then(|d| d.user.clone()) {
        Some(user) => user,
        None => reg.ensure_user_dir(),
    };
    let path = path_text(&path);
    let (ok, error) = match dirs.as_ref() {
        Some(d) => (d.opener)(&path),
        None => file_manager::open_folder(&path),
    };
    // The path is worth returning either way: on a headless machine the
    // operator still needs to know where to copy the folder to.
    Response::json_status(
        json!({"ok": ok, "path": path, "error": error}),
        if ok { 200 } else { 500 },
    )
}

/// The config was replaced as a whole (an import, a reset): whoever runs from
/// it re-reads it. The AI server's switches are the one such reader that keeps
/// state of its own, and it is told here once it exists.
pub(super) fn settings_reloaded(_core: &Core) {}
