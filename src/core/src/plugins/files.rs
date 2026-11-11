//! A plugin's own files, carried inside a settings file (`plugin_files.py`).
//!
//! A settings export can take an operator's plugin along with its settings.
//! Only plugins from the operator's folder travel this way; a bundled one ships
//! with Corvus. What is collected is what the asset route could serve, from
//! inside the folder only: symlinks, hidden files and folders and the plugin's
//! `config.json` stay behind (the config travels as the plugin's settings).
//!
//! Installing stages the files in a hidden folder beside the target and swaps
//! that in, so a failure halfway leaves the installed copy as it was; the saved
//! settings in the same folder are carried over. Discovery skips hidden folders,
//! so a staging folder left by a crash is never loaded.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use super::config::{SaveError, CONFIG_NAME};
use super::registry::{is_valid_id, ASSET_SUFFIXES, MANIFEST_NAME};

pub const MAX_PLUGIN_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FILES: usize = 200;
pub const MAX_DEPTH: usize = 8;
pub const MAX_PATH_CHARS: usize = 240;

const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
const BAD_CHARS: &str = "<>:\"|?*";

/// A plugin's files by relative path, in the order they were found.
pub type Files = IndexMap<String, Vec<u8>>;

/// Why files were not collected or installed. `Value` carries the sentence of
/// Python's `ValueError`, `Io` the disk's refusal.
#[derive(Debug)]
pub enum FilesError {
    Value(String),
    Io(std::io::Error),
}

impl std::fmt::Display for FilesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilesError::Value(s) => write!(f, "{s}"),
            FilesError::Io(e) => write!(f, "{}", super::config::io_text(e)),
        }
    }
}

impl From<std::io::Error> for FilesError {
    fn from(e: std::io::Error) -> Self {
        FilesError::Io(e)
    }
}

/// `rel` as a safe path inside a plugin folder, or `None`: forward slashes,
/// relative, no `.` or `..` step, no hidden part, a suffix the asset route
/// serves and a name every supported platform can write. The plugin's own
/// `config.json` at the top is refused.
pub fn clean_path(rel: &str) -> Option<String> {
    if rel.is_empty() || rel.chars().count() > MAX_PATH_CHARS {
        return None;
    }
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || rel.chars().nth(1) == Some(':') {
        return None;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() > MAX_DEPTH {
        return None;
    }
    for part in &parts {
        if matches!(*part, "" | "." | "..") || part.starts_with('.') || part.ends_with([' ', '.']) {
            return None;
        }
        if part.chars().any(|c| BAD_CHARS.contains(c) || (c as u32) < 32) {
            return None;
        }
        let stem = part.split('.').next().unwrap_or("").to_uppercase();
        if WINDOWS_RESERVED.contains(&stem.as_str()) {
            return None;
        }
    }
    if parts == [CONFIG_NAME] {
        return None;
    }
    let name = parts.last()?;
    let suffix = match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => name[i..].to_lowercase(),
        _ => String::new(),
    };
    ASSET_SUFFIXES.contains(&suffix.as_str()).then_some(rel)
}

fn too_big() -> FilesError {
    FilesError::Value(format!("larger than {} MB", MAX_PLUGIN_BYTES / (1024 * 1024)))
}

fn walk(dir: &Path, root: &Path, out: &mut Files, total: &mut usize) -> Result<(), FilesError> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let ftype = entry.file_type()?;
        // os.walk follows a symlink to a folder as a folder name; it is dropped here.
        if ftype.is_symlink() {
            if entry.path().is_dir() {
                continue;
            }
            files.push(name);
        } else if ftype.is_dir() {
            if !name.starts_with('.') && name != "__pycache__" {
                dirs.push(name);
            }
        } else {
            files.push(name);
        }
    }
    files.sort();
    dirs.sort();
    for name in files {
        let full = dir.join(&name);
        let Ok(rel) = full.strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let Some(rel) = clean_path(&rel) else { continue };
        let is_link = std::fs::symlink_metadata(&full).is_ok_and(|m| m.file_type().is_symlink());
        if is_link || !full.is_file() {
            continue;
        }
        let data = std::fs::read(&full)?;
        *total += data.len();
        if *total > MAX_PLUGIN_BYTES {
            return Err(too_big());
        }
        out.insert(rel, data);
        if out.len() > MAX_FILES {
            return Err(FilesError::Value(format!("made of more than {MAX_FILES} files")));
        }
    }
    for name in dirs {
        walk(&dir.join(name), root, out, total)?;
    }
    Ok(())
}

/// Every file of the plugin in `plugin_dir` that travels, by relative path.
/// `Value` when the plugin is larger than [`MAX_PLUGIN_BYTES`] or holds more
/// than [`MAX_FILES`] files, `Io` when the folder cannot be read.
pub fn collect(plugin_dir: &Path) -> Result<Files, FilesError> {
    let mut out = Files::new();
    let mut total = 0;
    walk(plugin_dir, plugin_dir, &mut out, &mut total)?;
    Ok(out)
}

/// `files` if they are a plugin that may be installed: every path clean, a
/// `plugin.json` among them, within the limits [`collect`] applies.
pub fn check(files: &Files) -> Result<Files, FilesError> {
    if files.is_empty() {
        return Err(FilesError::Value("no files".into()));
    }
    if files.len() > MAX_FILES {
        return Err(FilesError::Value(format!("more than {MAX_FILES} files")));
    }
    let mut total = 0;
    let mut out = Files::new();
    for (rel, data) in files {
        match clean_path(rel) {
            Some(clean) if clean == *rel => {
                total += data.len();
                out.insert(clean, data.clone());
            }
            _ => {
                return Err(FilesError::Value(format!(
                    "unusable file {}",
                    super::config::py_repr(rel)
                )))
            }
        }
    }
    if total > MAX_PLUGIN_BYTES {
        return Err(too_big());
    }
    if !out.contains_key(MANIFEST_NAME) {
        return Err(FilesError::Value(format!("no {MANIFEST_NAME}")));
    }
    Ok(out)
}

fn unique_suffix() -> String {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    format!("{:08x}", u32::from_le_bytes(bytes))
}

fn swap_in(staging: &Path, target: &Path) -> std::io::Result<()> {
    let mut old: Option<PathBuf> = None;
    let exists = std::fs::symlink_metadata(target).is_ok();
    if exists {
        if !target.is_dir() {
            return Err(std::io::Error::other(format!(
                "{} exists and is not a folder",
                target.display()
            )));
        }
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let aside = target.with_file_name(format!(".{name}.old-{}", unique_suffix()));
        std::fs::rename(target, &aside)?;
        old = Some(aside);
    }
    if let Err(e) = std::fs::rename(staging, target) {
        if let Some(aside) = &old {
            let _ = std::fs::rename(aside, target);
        }
        return Err(e);
    }
    if let Some(aside) = old {
        // A symlinked plugin (a developer's working copy) loses the link, never
        // the folder it points at.
        let is_link = std::fs::symlink_metadata(&aside).is_ok_and(|m| m.file_type().is_symlink());
        if is_link {
            let _ = std::fs::remove_file(&aside);
        } else {
            let _ = std::fs::remove_dir_all(&aside);
        }
    }
    Ok(())
}

/// Installs `files` as plugin `id` in the operator's folder, replacing an
/// installed copy with the same id and keeping its saved settings. Returns the
/// plugin's folder. Either way an installed copy is left as it was on failure.
pub fn install(id: &str, files: &Files, user_dir: &Path) -> Result<PathBuf, FilesError> {
    if !is_valid_id(id) {
        return Err(FilesError::Value(format!(
            "invalid plugin id {}",
            super::config::py_repr(id)
        )));
    }
    let files = check(files)?;
    std::fs::create_dir_all(user_dir)?;
    let target = user_dir.join(id);
    let staging = user_dir.join(format!(".{id}.import-{}", unique_suffix()));
    std::fs::create_dir(&staging)?;
    let result = (|| -> std::io::Result<()> {
        for (rel, data) in &files {
            let path = rel.split('/').fold(staging.clone(), |p, part| p.join(part));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, data)?;
        }
        let settings = target.join(CONFIG_NAME);
        if settings.is_file() {
            std::fs::copy(&settings, staging.join(CONFIG_NAME))?;
        }
        swap_in(&staging, &target)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(FilesError::Io(e));
    }
    Ok(target)
}

impl From<SaveError> for FilesError {
    fn from(e: SaveError) -> Self {
        match e {
            SaveError::InvalidId(id) => FilesError::Value(format!("invalid plugin id {}", super::config::py_repr(&id))),
            SaveError::Io(e) => FilesError::Io(e),
        }
    }
}
