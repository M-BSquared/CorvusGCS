//! Helpers the AI area's tests share.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::Value;

/// A folder of its own under the system temp dir, removed on drop.
pub struct Tmp(pub PathBuf);

impl Tmp {
    pub fn new(name: &str) -> Tmp {
        let dir = std::env::temp_dir().join(format!("corvus-ai-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a `{path: text | {"repeat": n}}` tree under `root`.
pub fn materialize(root: &Path, tree: &serde_json::Map<String, Value>) {
    for (rel, content) in tree {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        match content {
            Value::String(s) => std::fs::write(&path, s).unwrap(),
            other => std::fs::write(&path, vec![b'x'; other["repeat"].as_u64().unwrap() as usize]).unwrap(),
        }
    }
}

/// Every file under `root` as `relative path -> text`, sorted.
pub fn listing(root: &Path) -> std::collections::BTreeMap<String, String> {
    fn walk(dir: &Path, root: &Path, out: &mut std::collections::BTreeMap<String, String>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"),
                    String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned(),
                );
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}
