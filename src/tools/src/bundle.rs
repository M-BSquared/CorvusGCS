//! A finished bundle (AppDir, `.app`, Windows folder) holds no Python.
//!
//! Corvus GCS ships as one native program with its backend inside it. This
//! walks a bundle and names every offender: a Python interpreter or its
//! library, a package folder, Python sources or bytecode, and a native file
//! built against Python (it carries `PyInit_` or `Py_Initialize`, as a PyO3
//! extension or an embedding program does). Files the bundle must hold are
//! checked too.

use std::io::Read;
use std::path::Path;

use crate::appimage::{files, glob};

/// Names a Python install would carry. Globs on the file or folder name.
pub const FORBIDDEN_NAMES: &[&str] = &[
    "python",
    "python[0-9]*",
    "pythonw*",
    "libpython*",
    "Python",
    "Python.framework",
    "Python.app",
    "pyvenv.cfg",
    "site-packages",
    "dist-packages",
    "lib-dynload",
    "pip",
    "pip3*",
    "PySide*",
    "corvus_link*",
    "*.pyd",
    "*.py",
    "*.pyc",
    "*.pyo",
    "serve.py",
];

const PYTHON_SYMBOLS: &[&[u8]] = &[b"PyInit_", b"Py_Initialize"];
const NATIVE_SUFFIXES: &[&str] = &["so", "dylib", "dll", "exe", "node"];

fn names_python(name: &str) -> bool {
    FORBIDDEN_NAMES.iter().any(|p| {
        // `python[0-9]*`: python followed by a digit.
        if let Some(rest) = p.strip_prefix("python[0-9]") {
            return name
                .strip_prefix("python")
                .and_then(|r| {
                    r.chars()
                        .next()
                        .filter(char::is_ascii_digit)
                        .map(|c| &r[c.len_utf8()..])
                })
                .is_some_and(|tail| glob(rest, tail));
        }
        glob(p, name)
    })
}

fn is_native(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => NATIVE_SUFFIXES.contains(&ext.to_ascii_lowercase().as_str()),
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                false
            }
        }
    }
}

fn built_against_python(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let keep = PYTHON_SYMBOLS.iter().map(|s| s.len()).max().unwrap_or(1) - 1;
    let mut tail: Vec<u8> = Vec::new();
    let mut block = vec![0u8; 1 << 20];
    loop {
        let n = match f.read(&mut block) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        let mut data = std::mem::take(&mut tail);
        data.extend_from_slice(&block[..n]);
        if PYTHON_SYMBOLS.iter().any(|s| data.windows(s.len()).any(|w| w == *s)) {
            return true;
        }
        tail = data[data.len().saturating_sub(keep)..].to_vec();
    }
}

/// What is wrong with the bundle at `root`; empty when it is clean.
pub fn check(root: &Path, require: &[&str]) -> Vec<String> {
    if !root.is_dir() {
        return vec![format!("{} is not a folder", root.display())];
    }
    let mut problems = Vec::new();
    let mut dirs = Vec::new();
    collect_dirs(root, &mut dirs);
    for path in dirs.iter().chain(files(root).iter()) {
        let rel = path.strip_prefix(root).unwrap_or(path).display().to_string();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if names_python(name) {
            problems.push(format!("{rel}: a Python file or folder"));
        } else if is_native(path) && built_against_python(path) {
            problems.push(format!("{rel}: built against Python (PyO3 or an embedded interpreter)"));
        }
    }
    for needed in require {
        if !root.join(needed).exists() {
            problems.push(format!("{needed}: required and missing"));
        }
    }
    problems.sort();
    problems
}

fn collect_dirs(root: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
            out.push(p.clone());
            collect_dirs(&p, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_names_are_recognised() {
        for bad in [
            "python",
            "python3",
            "python3.12",
            "libpython3.12.so",
            "site-packages",
            "x.py",
            "x.pyc",
            "corvus_link.abi3.so",
            "PySide6",
            "pyvenv.cfg",
        ] {
            assert!(names_python(bad), "{bad}");
        }
        for good in [
            "corvus-gcs",
            "VERSION",
            "LICENSE.md",
            "pythonic.txt",
            "libGL.so.1",
            "Corvus GCS.exe",
        ] {
            assert!(!names_python(good), "{good}");
        }
    }

    #[test]
    fn a_clean_bundle_passes_and_a_dirty_one_names_each_offender() {
        let root = std::env::temp_dir().join(format!("corvus-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/app.exe"), b"MZ native code").unwrap();
        std::fs::write(root.join("VERSION"), b"2026.11.01").unwrap();
        assert_eq!(check(&root, &["VERSION", "bin/app.exe"]), Vec::<String>::new());
        std::fs::create_dir_all(root.join("lib/site-packages")).unwrap();
        std::fs::write(root.join("serve.py"), b"print()").unwrap();
        std::fs::write(root.join("bin/ext.so"), b"\0\0PyInit_corvus_link\0").unwrap();
        let problems = check(&root, &["LICENSE.md"]);
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("site-packages")));
        assert!(problems.iter().any(|p| p.contains("ext.so: built against Python")));
        assert!(problems.iter().any(|p| p == "LICENSE.md: required and missing"));
        let _ = std::fs::remove_dir_all(root);
    }
}
