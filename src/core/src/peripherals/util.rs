//! Small helpers the peripherals share: Python's `str.strip`, `shutil.which`
//! and the text of an OS error as `exc.strerror` reads it.

use std::path::PathBuf;

/// `str.strip()`: Unicode white space, which in Python also covers `\x1c` to `\x1f`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// `shutil.which(name)` over `PATH` (on Windows also with the `PATHEXT` suffixes).
pub fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        let e = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        std::iter::once(String::new())
            .chain(e.split(';').filter(|x| !x.is_empty()).map(str::to_string))
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&paths) {
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

/// `exc.strerror or exc`: the system's wording without Rust's ` (os error N)` tail.
pub fn os_error_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(i) if text.ends_with(')') => text[..i].to_string(),
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_matches_python() {
        assert_eq!(py_strip(" \t a b\n"), "a b");
        assert_eq!(py_strip("\x1ca\x1f"), "a");
        assert_eq!(py_strip(""), "");
    }

    #[test]
    fn os_error_has_no_code() {
        let e = std::io::Error::from_raw_os_error(2);
        assert_eq!(os_error_text(&e), "No such file or directory");
        assert_eq!(os_error_text(&std::io::Error::other("custom")), "custom");
    }
}
