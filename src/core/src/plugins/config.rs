//! Per plugin config files, kept apart from the application's own config
//! (`plugin_config.py`).
//!
//! Each plugin's saved settings live in `~/.corvus/plugins/<id>/config.json`
//! and nowhere else, so a plugin set up on one laptop is deployed on the next by
//! copying one file, and resetting the application's config does not wipe a
//! plugin's shelf. The file holds exactly the object the plugin saved, as
//! indented JSON, even for a plugin that ships inside the program: the bundled
//! folder is read only. The Python program reads and writes the same files.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use super::registry::is_valid_id;
use crate::json::Map;

pub const CONFIG_NAME: &str = "config.json";

/// The options the operator set from a plugin's gear. They share the plugin's
/// file so they travel with it, but they are not the plugin's to write: a
/// replace keeps them.
pub const OPTIONS_KEY: &str = "_options";

/// One writer at a time: a merge is read, modify, write.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Why a config was not saved.
#[derive(Debug)]
pub enum SaveError {
    /// The id is not one (`ValueError` in Python).
    InvalidId(String),
    Io(std::io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::InvalidId(id) => write!(f, "invalid plugin id {}", py_repr(id)),
            SaveError::Io(e) => write!(f, "{}", io_text(e)),
        }
    }
}

/// Python's `repr` of a string, for the one message that quotes an id.
pub fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// What Python prints for an `OSError`: `[Errno N] text: 'path'` is more than a
/// client needs; the routes only ever show `strerror`, which is Rust's own text
/// without its " (os error N)" tail.
pub fn io_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// Where `id` keeps its config, or `None` for an id that is not one. The id is
/// checked before it is joined onto a path, so a request cannot name a file
/// outside the plugin folder.
pub fn config_path(id: &str, user_dir: &Path) -> Option<PathBuf> {
    is_valid_id(id).then(|| user_dir.join(id).join(CONFIG_NAME))
}

/// The saved config of `id`, or `None` when it has none. A file that is
/// unreadable or not a JSON object is treated as absent: a plugin starting from
/// empty settings is recoverable, a station that fails over a typo is not.
pub fn load(id: &str, user_dir: &Path) -> Option<Map> {
    let path = config_path(id, user_dir)?;
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Every plugin config under the plugin folder, keyed by folder name. A config
/// whose plugin is not installed is returned too: it is what makes a config
/// copied ahead of its plugin work once the plugin arrives.
pub fn load_all(user_dir: &Path) -> Map {
    let Ok(read) = std::fs::read_dir(user_dir) else {
        return Map::new();
    };
    let mut names: Vec<String> = read
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = Map::new();
    for name in names {
        if let Some(data) = load(&name, user_dir) {
            out.insert(name, Value::Object(data));
        }
    }
    out
}

/// Python's `json.dumps(obj, indent=2, ensure_ascii=False) + "\n"`.
pub fn dump(obj: &Map) -> String {
    let mut text = serde_json::to_string_pretty(obj).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    text
}

fn save_unlocked(id: &str, settings: &Map, user_dir: &Path) -> Result<(), SaveError> {
    let path = config_path(id, user_dir).ok_or_else(|| SaveError::InvalidId(id.to_string()))?;
    let text = dump(settings);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(SaveError::Io)?;
    let tmp = tmp_file(dir, CONFIG_NAME);
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(SaveError::Io(e));
    }
    Ok(())
}

/// A temp file name beside the target that nothing else uses.
pub(super) fn tmp_file(dir: &Path, name: &str) -> PathBuf {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    dir.join(format!(
        "{name}.{:08x}.tmp",
        u32::from_le_bytes(bytes) ^ std::process::id()
    ))
}

/// Writes `settings` as the whole config of `id`, atomically: a temp file beside
/// the target renamed over it, so a crash leaves the old file or the new one.
pub fn save(id: &str, settings: &Map, user_dir: &Path) -> Result<(), SaveError> {
    let _w = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    save_unlocked(id, settings, user_dir)
}

/// Merges `patch` into the config of `id` (or replaces it) and saves; returns
/// the object now on disk. Read and write happen under one lock, so two merges
/// cannot each drop the other's key. A replace replaces what the plugin saved,
/// not the operator's `_options`.
pub fn update(id: &str, patch: &Map, replace: bool, user_dir: &Path) -> Result<Map, SaveError> {
    let _w = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let existing = load(id, user_dir);
    let merged = if replace {
        let mut merged = patch.clone();
        if let Some(opts) = existing.as_ref().and_then(|e| e.get(OPTIONS_KEY)) {
            if !merged.contains_key(OPTIONS_KEY) {
                merged.insert(OPTIONS_KEY.into(), opts.clone());
            }
        }
        merged
    } else {
        match existing.filter(|e| !e.is_empty()) {
            Some(mut base) => {
                for (k, v) in patch {
                    base.insert(k.clone(), v.clone());
                }
                base
            }
            None => patch.clone(),
        }
    };
    save_unlocked(id, &merged, user_dir)?;
    Ok(merged)
}

/// Moves settings out of the main config's old `plugins` key into files. A
/// plugin that already has a file keeps it, even one that does not parse.
/// Returns `true` when every entry is accounted for, so the caller may drop the
/// key; `false` when a write failed and nothing may be lost.
pub fn migrate(legacy: Option<&Value>, user_dir: &Path) -> bool {
    let Some(Value::Object(legacy)) = legacy else {
        return true;
    };
    let mut complete = true;
    for (id, settings) in legacy {
        let Value::Object(settings) = settings else { continue };
        let Some(path) = config_path(id, user_dir) else {
            continue;
        };
        if path.exists() {
            continue;
        }
        if let Err(e) = save(id, settings, user_dir) {
            eprintln!("corvus: plugin {id}: could not move settings out of the main config: {e}");
            complete = false;
        }
    }
    complete
}
