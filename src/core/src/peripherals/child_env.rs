//! The environment for a program Corvus starts that is not part of Corvus
//! (`child_env.py`).
//!
//! A packaged Corvus runs with variables its launcher set for its own use,
//! pointing inside the bundle: `LD_LIBRARY_PATH`, `QT_PLUGIN_PATH`, the
//! AppImage's `APPDIR`, the interpreter's `PYTHONHOME`. A child inherits every
//! one of them unless told otherwise, and for an operator's program that is
//! wrong. What is kept is exactly what the operator's environment had: only
//! entries pointing into this Corvus are taken out, and a list such as
//! `LD_LIBRARY_PATH` keeps whatever else it held.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Set by the launcher when it chose a Qt platform, so a child gets the
/// variable taken away only when Corvus put it there.
pub const QPA_DEFAULT_ENV: &str = "CORVUS_QPA_DEFAULT";

const PATH_LISTS: [&str; 12] = [
    "PYTHONPATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "QT_PLUGIN_PATH",
    "QT_QPA_PLATFORM_PLUGIN_PATH",
    "QML_IMPORT_PATH",
    "QML2_IMPORT_PATH",
    "QTWEBENGINE_RESOURCES_PATH",
    "QTWEBENGINE_DICTIONARIES_PATH",
    "QTWEBENGINEPROCESS_PATH",
];

const ALWAYS_DROP: [&str; 5] = [
    "PYTHONEXECUTABLE",
    "__PYVENV_LAUNCHER__",
    "QTWEBENGINE_CHROMIUM_FLAGS",
    "CORVUS_LAUNCHER_CHROMIUM_FLAGS",
    "_MEIPASS2",
];

const APPIMAGE_VARS: [&str; 4] = ["APPDIR", "APPIMAGE", "ARGV0", "OWD"];

/// `os.path.normpath(os.path.abspath(path))` without touching the file system.
fn norm(path: &str) -> PathBuf {
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if cfg!(windows) {
        PathBuf::from(out.to_string_lossy().to_lowercase())
    } else {
        out
    }
}

fn inside(path: &str, root: &str) -> bool {
    if path.is_empty() || root.is_empty() {
        return false;
    }
    let (p, r) = (norm(path), norm(root));
    p == r || p.starts_with(&r)
}

fn ours(path: &str, dirs: &[String]) -> bool {
    dirs.iter().any(|d| inside(path, d))
}

/// The folder holding the program: the folder of the running binary.
pub fn app_root() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// The folders that belong to this Corvus and to nothing else: the app folder,
/// the AppImage mount it is inside and the `.app` bundle it is inside.
pub fn bundle_dirs(env: &BTreeMap<String, String>, root: &str) -> Vec<String> {
    let mut dirs = vec![root.to_string()];
    if let Some(appdir) = env.get("APPDIR").filter(|a| !a.is_empty()) {
        if inside(root, appdir) {
            dirs.push(appdir.clone());
        }
    }
    let mut head = norm(root);
    loop {
        if head.to_string_lossy().to_lowercase().ends_with(".app") {
            dirs.push(head.to_string_lossy().into_owned());
            break;
        }
        match head.parent() {
            Some(parent) if parent != head => head = parent.to_path_buf(),
            _ => break,
        }
    }
    dirs
}

/// A copy of `env` fit for a program that is not Corvus.
pub fn child_env_from(env: &BTreeMap<String, String>, root: &str) -> BTreeMap<String, String> {
    let dirs = bundle_dirs(env, root);
    let mut out = env.clone();
    for key in ALWAYS_DROP {
        out.remove(key);
    }
    out.retain(|k, _| !k.starts_with("_PYI_"));
    if let Some(home) = out.get("PYTHONHOME").cloned() {
        if !home.is_empty() && ours(&home, &dirs) {
            out.remove("PYTHONHOME");
        }
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    for key in PATH_LISTS {
        let Some(value) = out.get(key).cloned() else {
            continue;
        };
        let kept: Vec<&str> = value.split(sep).filter(|p| !p.is_empty() && !ours(p, &dirs)).collect();
        if kept.is_empty() {
            out.remove(key);
        } else {
            out.insert(key.into(), kept.join(&sep.to_string()));
        }
    }
    if let Some(defaulted) = out.remove(QPA_DEFAULT_ENV) {
        if out.get("QT_QPA_PLATFORM") == Some(&defaulted) {
            out.remove("QT_QPA_PLATFORM");
        }
    }
    if let Some(appdir) = env.get("APPDIR") {
        if !appdir.is_empty() && dirs.iter().any(|d| d == appdir) {
            for key in APPIMAGE_VARS {
                out.remove(key);
            }
        }
    }
    out
}

/// This process's environment, cleaned for a child.
pub fn child_env() -> BTreeMap<String, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    child_env_from(&env, &app_root())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const SEP: &str = if cfg!(windows) { ";" } else { ":" };

    #[test]
    fn entries_inside_the_bundle_go_and_the_operators_stay() {
        let ld = ["/opt/corvus/lib", "/usr/local/lib", "/opt/corvus/lib/qt"].join(SEP);
        let e = env(&[
            ("LD_LIBRARY_PATH", ld.as_str()),
            ("QT_PLUGIN_PATH", "/opt/corvus/plugins"),
            ("PYTHONHOME", "/opt/corvus/py"),
            ("_PYI_X", "1"),
            ("HOME", "/home/me"),
            ("APPDIR", "/opt/corvus"),
            ("OWD", "/x"),
        ]);
        let out = child_env_from(&e, "/opt/corvus/app");
        assert_eq!(out.get("LD_LIBRARY_PATH").map(String::as_str), Some("/usr/local/lib"));
        assert!(!out.contains_key("QT_PLUGIN_PATH"));
        assert!(!out.contains_key("PYTHONHOME"));
        assert!(!out.contains_key("_PYI_X"));
        assert!(!out.contains_key("APPDIR") && !out.contains_key("OWD"));
        assert_eq!(out.get("HOME").map(String::as_str), Some("/home/me"));
    }

    #[test]
    fn a_platform_corvus_chose_is_taken_away_and_the_operators_kept() {
        let e = env(&[("CORVUS_QPA_DEFAULT", "xcb"), ("QT_QPA_PLATFORM", "xcb")]);
        let out = child_env_from(&e, "/opt/corvus/app");
        assert!(!out.contains_key("QT_QPA_PLATFORM") && !out.contains_key(QPA_DEFAULT_ENV));
        let e = env(&[("CORVUS_QPA_DEFAULT", "xcb"), ("QT_QPA_PLATFORM", "wayland")]);
        let out = child_env_from(&e, "/opt/corvus/app");
        assert_eq!(out.get("QT_QPA_PLATFORM").map(String::as_str), Some("wayland"));
    }

    #[test]
    fn the_app_bundle_folder_counts_as_ours() {
        let dyld = ["/Applications/Corvus GCS.app/Contents/Frameworks", "/opt/x"].join(SEP);
        let e = env(&[("DYLD_LIBRARY_PATH", dyld.as_str())]);
        let out = child_env_from(&e, "/Applications/Corvus GCS.app/Contents/Resources");
        assert_eq!(out.get("DYLD_LIBRARY_PATH").map(String::as_str), Some("/opt/x"));
    }
}
