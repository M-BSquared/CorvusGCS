//! Discovery of plugin folders (`plugin_registry.py`).
//!
//! A plugin is a folder with a `plugin.json` manifest. Two roots are read, in
//! this order: the bundled one (the plugins that ship with Corvus, read only and
//! replaced by an update) and the operator's own, `~/.corvus/plugins`, which
//! survives updates. A user plugin whose id collides with a bundled one wins:
//! that is how an operator patches a shipped plugin.
//!
//! The bundled plugins of this program are Rust crates in `src/plugins/<id>`,
//! so their manifests come from [`bundled::manifests`] and not from files; the
//! user folder is scanned with the rules Python uses, because that folder is
//! shared with the Python program. Nothing here runs plugin code. A scan never
//! fails: an unreadable folder, a malformed manifest or a plugin with no script
//! is skipped.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const MANIFEST_NAME: &str = "plugin.json";

/// Where a plugin lands in the grid when its manifest does not say.
pub const DEFAULT_ORDER: i64 = 100;

/// What a plugin folder may serve: what a plugin needs and nothing that would
/// turn the asset route into a general file server (`ASSET_SUFFIXES`).
pub const ASSET_SUFFIXES: [&str; 20] = [
    ".js", ".mjs", ".css", ".json", ".map", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".ico", ".woff",
    ".woff2", ".ttf", ".otf", ".html", ".md", ".txt", ".csv",
];

/// Written into a freshly created user plugin folder (`_README` of Python), so
/// the folder both programs share explains itself.
pub const README: &str = include_str!("user_readme.md");

/// One plugin as the registry knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub order: i64,
    pub icon: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub scripts: Vec<String>,
    pub styles: Vec<String>,
    /// `"bundled"` or `"user"`.
    pub source: &'static str,
    pub dir: PathBuf,
}

impl Manifest {
    /// The object `GET /api/plugins` lists: the folder is the server's business.
    pub fn public(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "order": self.order,
            "icon": self.icon,
            "description": self.description,
            "version": self.version,
            "author": self.author,
            "scripts": self.scripts,
            "styles": self.styles,
            "source": self.source,
        })
    }
}

/// Whether `id` is safe as a folder name and in a URL: `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`
/// (Python's `$` lets one trailing newline pass, so does this).
pub fn is_valid_id(id: &str) -> bool {
    let id = id.strip_suffix('\n').unwrap_or(id);
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.chars().count() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// A stripped string, or `default` for anything that is not a non empty one.
fn clean_str(raw: Option<&Value>, default: &str) -> String {
    match raw {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => default.to_string(),
    }
}

/// The manifest's `order`, or [`DEFAULT_ORDER`] for anything that is not a
/// number. A boolean is not one (`True` would silently mean first).
fn clean_order(raw: Option<&Value>) -> i64 {
    match raw {
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => i,
            None => match n.as_f64() {
                Some(f) if f.is_finite() => f as i64,
                _ => DEFAULT_ORDER,
            },
        },
        _ => DEFAULT_ORDER,
    }
}

/// `rel` inside `plugin_dir`, or `None` when it is not servable. The one
/// containment check behind the manifest parser and the asset route: inside the
/// folder after symlinks are resolved, an allowed suffix, a regular file.
pub fn resolve_asset(plugin_dir: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return None;
    }
    let bytes = rel.as_bytes();
    if rel.starts_with(['/', '\\']) || (rel.chars().nth(1) == Some(':') && bytes.len() > 1) {
        return None;
    }
    let root = std::fs::canonicalize(plugin_dir).ok()?;
    // pathlib drops a trailing slash and "." steps before the path is resolved.
    let joined = Path::new(rel)
        .components()
        .fold(root.clone(), |p, c| p.join(c.as_os_str()));
    let target = std::fs::canonicalize(joined).ok()?;
    target.strip_prefix(&root).ok()?;
    let name = target.file_name()?.to_string_lossy().to_lowercase();
    let suffix = match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => &name[i..],
        _ => "",
    };
    if !ASSET_SUFFIXES.contains(&suffix) || !target.is_file() {
        return None;
    }
    Some(target)
}

fn clean_file_list(raw: Option<&Value>, plugin_dir: &Path) -> Vec<String> {
    let Some(Value::Array(entries)) = raw else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        let rel = clean_str(Some(entry), "");
        if rel.is_empty() || resolve_asset(plugin_dir, &rel).is_none() {
            continue;
        }
        let mut normalized = rel.replace('\\', "/");
        while let Some(rest) = normalized.strip_prefix("./") {
            normalized = rest.to_string();
        }
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

fn read_manifest(plugin_dir: &Path, source: &'static str) -> Option<Manifest> {
    let text = std::fs::read_to_string(plugin_dir.join(MANIFEST_NAME)).ok()?;
    let Value::Object(data) = serde_json::from_str::<Value>(&text).ok()? else {
        return None;
    };
    let dir_name = plugin_dir.file_name()?.to_string_lossy().into_owned();
    let id = clean_str(data.get("id"), &dir_name);
    if !is_valid_id(&id) {
        return None;
    }
    let mut scripts = clean_file_list(data.get("scripts"), plugin_dir);
    if scripts.is_empty() {
        scripts = clean_file_list(Some(&json!([format!("{id}.js")])), plugin_dir);
    }
    if scripts.is_empty() {
        return None;
    }
    Some(Manifest {
        name: clean_str(data.get("name"), &id),
        order: clean_order(data.get("order")),
        icon: clean_str(data.get("icon"), "puzzle"),
        description: clean_str(data.get("description"), ""),
        version: clean_str(data.get("version"), ""),
        author: clean_str(data.get("author"), ""),
        scripts,
        styles: clean_file_list(data.get("styles"), plugin_dir),
        source,
        dir: plugin_dir.to_path_buf(),
        id,
    })
}

/// Every plugin folder directly under `root`; never fails.
pub fn scan_root(root: &Path, source: &'static str) -> Vec<Manifest> {
    let Ok(read) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    let mut out = Vec::new();
    for entry in entries {
        // A hidden folder is never a plugin: an import stages a plugin in one
        // before swapping it in, and one left behind by a crash must not load
        // as a second copy.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if !entry.path().is_dir() {
            continue;
        }
        if let Some(m) = read_manifest(&entry.path(), source) {
            out.push(m);
        }
    }
    out
}

/// The plugins of this program and where the operator's own are.
#[derive(Debug, Clone)]
pub struct PluginRegistry {
    user_dir: PathBuf,
    bundled_dir: PathBuf,
    bundled: Bundled,
}

#[derive(Debug, Clone)]
enum Bundled {
    /// The Rust plugin crates of this program, listed by [`bundled::manifests`].
    Compiled,
    /// A folder of `plugin.json` plugins, scanned like the user's (tests).
    Scanned,
}

impl PluginRegistry {
    /// The registry of a running program: the compiled-in plugins and the
    /// operator's folder.
    pub fn new(user_dir: impl Into<PathBuf>, bundled_dir: impl Into<PathBuf>) -> PluginRegistry {
        PluginRegistry {
            user_dir: user_dir.into(),
            bundled_dir: bundled_dir.into(),
            bundled: Bundled::Compiled,
        }
    }

    /// A registry that reads `bundled_dir` the way it reads the user folder, as
    /// `plugin_registry.discover` does. What the parity tests compare.
    pub fn scanning(user_dir: impl Into<PathBuf>, bundled_dir: impl Into<PathBuf>) -> PluginRegistry {
        PluginRegistry {
            user_dir: user_dir.into(),
            bundled_dir: bundled_dir.into(),
            bundled: Bundled::Scanned,
        }
    }

    pub fn user_dir(&self) -> &Path {
        &self.user_dir
    }

    pub fn bundled_dir(&self) -> &Path {
        &self.bundled_dir
    }

    /// Every installed plugin, sorted by `order` then name, so which root a
    /// plugin came from never decides where it appears. A user plugin replaces
    /// a bundled one with the same id, wherever the bundled one sat.
    pub fn discover(&self) -> Vec<Manifest> {
        let mut merged = match self.bundled {
            Bundled::Compiled => bundled::manifests(&self.bundled_dir),
            Bundled::Scanned => scan_root(&self.bundled_dir, "bundled"),
        };
        for manifest in scan_root(&self.user_dir, "user") {
            match merged.iter().position(|m| m.id == manifest.id) {
                Some(i) => merged[i] = manifest,
                None => merged.push(manifest),
            }
        }
        merged.sort_by_key(|m| (m.order, m.name.to_lowercase()));
        merged
    }

    /// The folder a discovered plugin was loaded from. Goes through
    /// [`discover`](Self::discover) so the asset route reaches only a folder
    /// that parsed as a plugin.
    pub fn find_dir(&self, id: &str) -> Option<PathBuf> {
        if !is_valid_id(id) {
            return None;
        }
        self.discover().into_iter().find(|m| m.id == id).map(|m| m.dir)
    }

    pub fn public_list(&self) -> Vec<Value> {
        self.discover().iter().map(Manifest::public).collect()
    }

    /// Creates the user folder with its README and returns it. Idempotent and
    /// silent about failure: a read only home costs the folder, not the program.
    /// The README is only written when absent.
    pub fn ensure_user_dir(&self) -> PathBuf {
        if std::fs::create_dir_all(&self.user_dir).is_ok() {
            let readme = self.user_dir.join("README.md");
            if !readme.exists() {
                let _ = std::fs::write(readme, README);
            }
        }
        self.user_dir.clone()
    }
}

/// The plugins that ship with this program: one Rust crate each in
/// `src/plugins/<id>`. The values are the `plugin.json` of the web plugin the
/// crate replaces, so both programs list the same plugins with the same fields
/// (held by `tests/ai_plugins.rs` against the Python registry).
pub mod bundled {
    use super::*;

    struct Entry {
        id: &'static str,
        name: &'static str,
        icon: &'static str,
        description: &'static str,
        version: &'static str,
        order: i64,
        script: &'static str,
        style: &'static str,
    }

    const AUTHOR: &str = "Maximilian Böck";

    const ENTRIES: [Entry; 4] = [
        Entry {
            id: "vibration",
            name: "Vibration Monitor",
            icon: "activity",
            description: "Live PX4 vibration metrics and accelerometer clipping",
            version: "1.0.0",
            order: 10,
            script: "vibration.js",
            style: "vibration.css",
        },
        Entry {
            id: "ssh-launcher",
            name: "SSH Launcher",
            icon: "rocket",
            description: "One-press buttons that start programs on a companion computer",
            version: "1.3.0",
            order: 20,
            script: "ssh-launcher.js",
            style: "ssh-launcher.css",
        },
        Entry {
            id: "schwalby",
            name: "Schwalby",
            icon: "bird",
            description: "One-press buttons that start programs on this computer or over SSH",
            version: "1.1.0",
            order: 30,
            script: "schwalby.js",
            style: "schwalby.css",
        },
        Entry {
            id: "map-entities",
            name: "Map Entities",
            icon: "map-pin",
            description: "Gates, trees, people and devices with a range, placed exactly on the Home map",
            version: "1.0.0",
            order: 35,
            script: "map-entities.js",
            style: "map-entities.css",
        },
    ];

    /// The bundled manifests; `dir` is `<bundled_dir>/<id>`.
    pub fn manifests(bundled_dir: &Path) -> Vec<Manifest> {
        ENTRIES
            .iter()
            .map(|e| Manifest {
                id: e.id.into(),
                name: e.name.into(),
                order: e.order,
                icon: e.icon.into(),
                description: e.description.into(),
                version: e.version.into(),
                author: AUTHOR.into(),
                scripts: vec![e.script.into()],
                styles: vec![e.style.into()],
                source: "bundled",
                dir: bundled_dir.join(e.id),
            })
            .collect()
    }
}
