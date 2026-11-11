//! Show a folder to the operator in the host's own file manager
//! (`file_manager.py`), and a web address in the system browser.
//!
//! The path is an argv element of a fixed executable, never part of a shell
//! line, so a folder name cannot turn into a command. Callers pass a directory
//! Corvus decided on, and anything that is not an existing directory is refused.
//!
//! Both start the launcher with the operator's environment, not the one a
//! packaged Corvus runs with ([`child_env`]): on a KDE desktop `xdg-open` is a
//! Qt program, and with the AppImage's library paths it loads the bundle's
//! libraries instead of its own and dies before it opens anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for the launcher to report back. The file manager keeps
/// running; these commands only hand the path over and exit.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Lists of directories in which an entry inside this Corvus is removed.
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

/// Only ever meaningful to the bundled interpreter or an embedded Chromium.
const ALWAYS_DROP: [&str; 5] = [
    "PYTHONEXECUTABLE",
    "__PYVENV_LAUNCHER__",
    "QTWEBENGINE_CHROMIUM_FLAGS",
    "CORVUS_LAUNCHER_CHROMIUM_FLAGS",
    "_MEIPASS2",
];

/// Set by the AppImage runtime for the AppImage itself.
const APPIMAGE_VARS: [&str; 4] = ["APPDIR", "APPIMAGE", "ARGV0", "OWD"];

fn norm(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn inside(path: &str, root: &Path) -> bool {
    if path.is_empty() || root.as_os_str().is_empty() {
        return false;
    }
    norm(Path::new(path)).starts_with(norm(root))
}

/// The folders that belong to this Corvus and to nothing else: the folder the
/// program lives in, the AppImage mount it is inside, the `.app` bundle it is
/// inside. Never a system prefix such as `/usr`.
fn bundle_dirs(env: &HashMap<String, String>, root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    if let Some(appdir) = env.get("APPDIR").filter(|a| !a.is_empty()) {
        if inside(&root.to_string_lossy(), Path::new(appdir)) {
            dirs.push(PathBuf::from(appdir));
        }
    }
    let mut head = norm(root);
    loop {
        if head
            .file_name()
            .is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(".app"))
        {
            dirs.push(head);
            break;
        }
        if !head.pop() {
            break;
        }
    }
    dirs
}

/// A copy of `env` fit for a program that is not Corvus: only entries pointing
/// into this Corvus are taken out, whatever the operator's own environment had
/// stays (`child_env.py`).
pub fn clean_env(env: &HashMap<String, String>, root: &Path) -> HashMap<String, String> {
    let dirs = bundle_dirs(env, root);
    let ours = |p: &str| dirs.iter().any(|d| inside(p, d));
    let mut out = env.clone();
    for key in ALWAYS_DROP {
        out.remove(key);
    }
    out.retain(|k, _| !k.starts_with("_PYI_"));
    if out.get("PYTHONHOME").is_some_and(|h| !h.is_empty() && ours(h)) {
        out.remove("PYTHONHOME");
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    for key in PATH_LISTS {
        let Some(value) = out.get(key) else { continue };
        let kept: Vec<&str> = value.split(sep).filter(|p| !p.is_empty() && !ours(p)).collect();
        if kept.is_empty() {
            out.remove(key);
        } else {
            let joined = kept.join(&sep.to_string());
            out.insert(key.into(), joined);
        }
    }
    if let Some(defaulted) = out.remove("CORVUS_QPA_DEFAULT") {
        if out.get("QT_QPA_PLATFORM") == Some(&defaulted) {
            out.remove("QT_QPA_PLATFORM");
        }
    }
    if env
        .get("APPDIR")
        .is_some_and(|a| dirs.iter().any(|d| d == Path::new(a)))
    {
        for key in APPIMAGE_VARS {
            out.remove(key);
        }
    }
    out
}

/// The environment for a launcher: this process' own, cleaned.
pub fn child_env() -> HashMap<String, String> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let root = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    clean_env(&env, &root)
}

fn command(program: &str, arg: &str) -> Command {
    let mut c = Command::new(program);
    c.arg(arg).env_clear().envs(child_env());
    c
}

/// Opens `path` in the host's file manager; `(ok, error)`. The error is an
/// empty string on success and a sentence for the operator otherwise. Never
/// panics: a machine with no desktop session has nothing to open, and that is
/// a message, not a failed request.
pub fn open_folder(path: &str) -> (bool, String) {
    if path.is_empty() {
        return (false, "no folder given".into());
    }
    if !Path::new(path).is_dir() {
        return (false, format!("{path} does not exist"));
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        // explorer.exe returns a non zero exit code even when it opened the
        // window, so it is judged by whether it started at all.
        let mut c = command("explorer", &norm(Path::new(path)).to_string_lossy());
        return match c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => (true, String::new()),
            Err(e) => {
                eprintln!("corvus: could not open {path} in Explorer: {e}");
                (false, "could not open the folder".into())
            }
        };
    } else {
        "xdg-open"
    };
    let mut child = match command(program, path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (false, format!("{program} is not available on this system"));
        }
        Err(e) => {
            eprintln!("corvus: could not open {path}: {e}");
            return (false, "could not open the folder".into());
        }
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() < LAUNCH_TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, "could not open the folder".into());
            }
            Err(_) => return (false, "could not open the folder".into()),
        }
    };
    if status.success() {
        (true, String::new())
    } else {
        eprintln!("corvus: {program} exited {status} for {path}");
        (false, "could not open the folder".into())
    }
}

/// The scheme of `url` as `urlsplit` reads it (lowercase), or `None` when
/// `urlsplit` would raise or finds none.
fn scheme_of(url: &str) -> Option<String> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let (scheme, rest) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() || !chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
        return None;
    }
    if let Some(netloc) = rest.strip_prefix("//") {
        let netloc = netloc.split(['/', '?', '#']).next().unwrap_or("");
        if netloc.contains('[') != netloc.contains(']') {
            return None;
        }
    }
    Some(scheme.to_ascii_lowercase())
}

/// Opens a web page in the system browser; whether a launcher was started.
/// `http` and `https` only: the opener of every desktop handles far more
/// schemes than those, and the scheme decides which program runs.
pub fn open_url(url: &str) -> bool {
    if !matches!(scheme_of(url).as_deref(), Some("http" | "https")) {
        return false;
    }
    let mut c = if cfg!(target_os = "macos") {
        command("open", url)
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler")
            .arg(url)
            .env_clear()
            .envs(child_env());
        c
    } else {
        command("xdg-open", url)
    };
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    match c.spawn() {
        Ok(mut child) => {
            // The launcher is not waited for (some browsers keep xdg-open until
            // their window closes), but it is reaped so no zombie is left.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(e) => {
            eprintln!("corvus: could not open {url} in a browser: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn only_entries_inside_this_corvus_are_taken_out() {
        let root = Path::new("/mnt/AppDir/usr/bin");
        let e = env(&[
            ("APPDIR", "/mnt/AppDir"),
            ("APPIMAGE", "/x/Corvus.AppImage"),
            ("LD_LIBRARY_PATH", "/mnt/AppDir/usr/lib:/opt/mine:/mnt/AppDirOther"),
            ("QT_PLUGIN_PATH", "/mnt/AppDir/usr/plugins"),
            ("PYTHONHOME", "/mnt/AppDir/usr"),
            ("PATH", "/usr/bin"),
            ("_PYI_X", "1"),
        ]);
        let out = clean_env(&e, root);
        assert_eq!(out["LD_LIBRARY_PATH"], "/opt/mine:/mnt/AppDirOther");
        for gone in ["QT_PLUGIN_PATH", "PYTHONHOME", "APPDIR", "APPIMAGE", "_PYI_X"] {
            assert!(!out.contains_key(gone), "{gone}");
        }
        assert_eq!(out["PATH"], "/usr/bin");
    }

    #[test]
    fn a_folder_that_is_not_there_is_refused_in_words() {
        assert_eq!(open_folder(""), (false, "no folder given".into()));
        let (ok, why) = open_folder("/definitely/not/here");
        assert!(!ok);
        assert_eq!(why, "/definitely/not/here does not exist");
    }

    #[test]
    fn only_web_addresses_are_opened() {
        for no in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://x",
            "",
            "x",
            "http://[::1",
        ] {
            assert!(!open_url(no), "{no}");
        }
        assert_eq!(scheme_of("HTTPS://a.b/c").as_deref(), Some("https"));
        assert_eq!(scheme_of("  http://a").as_deref(), Some("http"));
    }
}
