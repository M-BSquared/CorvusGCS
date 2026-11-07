//! Which plan the page opens on next time (`rememberLastPlan`, `forgetLastPlan`,
//! `loadLastPlan` of `mission.js`): a per machine convenience kept in
//! `~/.corvus/gui-mission-last.txt`. A failure is silent and means "start empty".
use std::path::{Path, PathBuf};

pub fn path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus").join("gui-mission-last.txt"))
}

pub fn read_at(path: &Path) -> Option<String> {
    let name = std::fs::read_to_string(path).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

pub fn remember_at(path: &Path, name: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, name);
}

pub fn forget_at(path: &Path, name: &str) {
    if read_at(path).as_deref() == Some(name) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn read() -> Option<String> {
    read_at(&path()?)
}

pub fn remember(name: &str) {
    if let Some(p) = path() {
        remember_at(&p, name);
    }
}

pub fn forget(name: &str) {
    if let Some(p) = path() {
        forget_at(&p, name);
    }
}
