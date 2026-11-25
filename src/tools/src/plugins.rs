//! Plugin packages: `plugin-pack`, `plugin-unpack` and `plugin-index`.
//!
//! A plugin is a crate under `plugins/` with a `plugin.json`, built into one
//! WebAssembly module that every system runs. `plugin-pack` leaves each module
//! under the name it is published as, with an `.entry.json` beside it;
//! `plugin-index` writes the list a release carries from those, and keeps from
//! the list that was there before what an older Corvus GCS still needs;
//! `plugin-unpack` installs packed modules as the store does, for the check that
//! a finished program loads them.

use std::path::{Path, PathBuf};

use corvus_package::{Entry, Index, Manifest, INDEX_FILE};
use serde_json::{json, Value};

/// The folder plugin crates live in, relative to the repository root.
const ROOT: &str = "plugins";
const ENTRY: &str = ".entry.json";
const TARGET: &str = "wasm32-wasip1";
const PROFILE: &str = "plugin";

/// A plugin crate of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crate {
    pub manifest: Manifest,
    pub package: String,
}

/// The plugin crates of the repository, by id. A `plugin.json` that cannot be read is an error,
/// never a plugin that goes missing.
pub fn crates(root: &Path) -> Result<Vec<Crate>, String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root.join(ROOT)).into_iter().flatten().flatten() {
        let path = e.path().join("plugin.json");
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let manifest = Manifest::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if e.file_name().to_str() != Some(manifest.id.as_str()) {
            return Err(format!("{}: the id must be the name of the folder", path.display()));
        }
        out.push(Crate {
            package: format!("corvus_plugin_{}", manifest.id.replace('-', "_")),
            manifest,
        });
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(out)
}

fn chosen(root: &Path, only: Option<&str>) -> Result<Vec<Crate>, String> {
    let mut list = crates(root)?;
    list.retain(|c| only.is_none_or(|id| c.manifest.id == id));
    if list.is_empty() {
        return Err(match only {
            Some(id) => format!("{ROOT}/{id} is no plugin: it has no plugin.json"),
            None => "there is no plugin with a plugin.json".into(),
        });
    }
    Ok(list)
}

/// Builds the modules into `target`.
pub fn build(root: &Path, target: &Path, only: Option<&str>) -> Result<(), String> {
    let mut cargo = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo
        .current_dir(root)
        .args(["build", "--locked", "--target", TARGET, "--profile", PROFILE])
        .arg("--target-dir")
        .arg(target);
    for c in chosen(root, only)? {
        cargo.args(["-p", &c.package]);
    }
    match cargo.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("cargo build failed ({s})")),
        Err(e) => Err(format!("cargo: {e}")),
    }
}

/// Where cargo leaves the modules in `target`.
pub fn built_in(target: &Path) -> PathBuf {
    target.join(TARGET).join(PROFILE)
}

/// Copies the built `module` of `c` to `out` under its published name, with its `.entry.json`.
/// Returns that name. A module that says something else than `plugin.json` was not built from
/// this source and is refused.
pub fn pack_one(c: &Crate, module: &Path, out: &Path) -> Result<String, String> {
    let bytes = std::fs::read(module).map_err(|e| format!("{}: {e} (build it first)", module.display()))?;
    let inside = Manifest::of_module(&bytes).map_err(|e| format!("{}: {e}", module.display()))?;
    if inside != c.manifest {
        return Err(format!(
            "{} was built from another plugin.json: build it again",
            module.display()
        ));
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let sha256 = corvus_package::sha256_hex(&bytes);
    let file = corvus_package::published_name(&c.manifest.id, &c.manifest.version, &sha256);
    let path = out.join(&file);
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let entry = Entry {
        manifest: inside,
        file: file.clone(),
        sha256,
        size: bytes.len() as u64,
    };
    let beside = out.join(format!("{}{ENTRY}", c.manifest.id));
    std::fs::write(&beside, serde_json::to_vec_pretty(&entry.to_json()).unwrap_or_default())
        .map_err(|e| format!("{}: {e}", beside.display()))?;
    Ok(file)
}

/// Packs every plugin of the repository from `target`, or only the plugin `only`.
pub fn pack(root: &Path, target: &Path, out: &Path, only: Option<&str>) -> Result<Vec<String>, String> {
    chosen(root, only)?
        .iter()
        .map(|c| {
            let module = built_in(target).join(corvus_package::built_name(&c.package));
            pack_one(c, &module, out)
        })
        .collect()
}

/// The entries `pack` left in `dir`.
fn packed(dir: &Path) -> Result<Vec<Entry>, String> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(ENTRY))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for p in names {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        let entry = Entry::from_json(&v)?;
        let module = dir.join(&entry.file);
        let have = corvus_package::sha256_file(&module).map_err(|e| format!("{}: {e}", module.display()))?;
        if have != entry.sha256 {
            return Err(format!("{} changed after it was packed", module.display()));
        }
        out.push(entry);
    }
    if out.is_empty() {
        return Err(format!("{} holds no packed plugin", dir.display()));
    }
    Ok(out)
}

/// The list of a release: what is new, and of what was listed `before` the newest module for
/// each minimum version that nothing newer serves. A station that has not updated Corvus GCS
/// goes on getting the newest plugin it can run. A module published again under the same
/// version and minimum takes the place of the old one.
pub fn merge(new: Vec<Entry>, before: Option<Index>) -> Index {
    let key = |e: &Entry| (e.manifest.id.clone(), e.manifest.min_corvus.clone());
    let mut plugins = new;
    let mut old = before.map(|i| i.plugins).unwrap_or_default();
    // The newest first, so the first of a minimum is the one to keep.
    old.sort_by(|a, b| {
        corvus_package::plugin_version(&b.manifest.version).cmp(&corvus_package::plugin_version(&a.manifest.version))
    });
    for e in old {
        if !plugins.iter().any(|have| key(have) == key(&e)) {
            plugins.push(e);
        }
    }
    plugins.sort_by(|a, b| {
        (&a.manifest.id, corvus_package::corvus_version(&a.manifest.min_corvus))
            .cmp(&(&b.manifest.id, corvus_package::corvus_version(&b.manifest.min_corvus)))
    });
    Index { plugins }
}

/// Writes the list into `dir` from what was packed there and `before`, the list published
/// until now.
pub fn publish_index(dir: &Path, before: Option<&Path>) -> Result<Index, String> {
    let before = match before {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Some(Index::parse(&bytes)?)
        }
        None => None,
    };
    let index = merge(packed(dir)?, before);
    std::fs::write(dir.join(INDEX_FILE), index.to_bytes()).map_err(|e| e.to_string())?;
    Ok(index)
}

/// Installs the plugins that `pack` left in `out` into `to`, as the store does, for a check
/// that a finished program loads them (`corvus-gcs --check-plugins`).
pub fn unpack(out: &Path, to: &Path) -> Result<usize, String> {
    let entries = packed(out)?;
    for e in &entries {
        let folder = to.join(&e.manifest.id);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).map_err(|er| format!("{}: {er}", folder.display()))?;
        std::fs::copy(out.join(&e.file), folder.join(&e.file)).map_err(|er| format!("{}: {er}", e.file))?;
        let manifest = json!({
            "id": e.manifest.id, "version": e.manifest.version, "module": e.file, "sha256": e.sha256,
            "permissions": e.manifest.permissions,
        });
        std::fs::write(folder.join("manifest.json"), manifest.to_string()).map_err(|er| er.to_string())?;
    }
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn manifest(version: &str, min: &str) -> Manifest {
        Manifest {
            id: "vibration".into(),
            name: "Vibration Monitor".into(),
            description: "d".into(),
            icon: "activity".into(),
            version: version.into(),
            order: 10,
            can_tab: false,
            min_corvus: min.into(),
            permissions: vec!["telemetry.rates".into()],
        }
    }

    fn module(m: &Manifest) -> Vec<u8> {
        let mut section = vec![corvus_package::MANIFEST_SECTION.len() as u8];
        section.extend_from_slice(corvus_package::MANIFEST_SECTION.as_bytes());
        section.extend_from_slice(m.to_json().to_string().as_bytes());
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        out.push(0);
        let len = section.len();
        out.push((len & 0x7f) as u8 | 0x80);
        out.push((len >> 7) as u8);
        out.extend(section);
        out
    }

    fn krate(m: Manifest) -> Crate {
        Crate {
            manifest: m,
            package: "corvus_plugin_vibration".into(),
        }
    }

    fn entry(version: &str, min: &str) -> Entry {
        let sha256 = corvus_package::sha256_hex(format!("{version} {min}").as_bytes());
        Entry {
            file: corvus_package::published_name("vibration", version, &sha256),
            sha256,
            size: 1,
            manifest: manifest(version, min),
        }
    }

    #[test]
    fn every_plugin_of_the_repository_has_a_manifest_that_reads() {
        let found = crates(&crate::repo_root()).unwrap();
        assert!(found.len() >= 8, "{}", found.len());
        let v = found.iter().find(|c| c.manifest.id == "vibration").expect("vibration");
        assert_eq!(v.package, "corvus_plugin_vibration");
        assert!(!v.manifest.description.is_empty() && !v.manifest.icon.is_empty());
    }

    #[test]
    fn what_is_packed_is_listed_installed_and_must_not_change() {
        let (work, built) = (tmp("work"), tmp("built"));
        let m = manifest("2.0.0", "2026.11.94");
        std::fs::write(built.join("m.wasm"), module(&m)).unwrap();
        let file = pack_one(&krate(m.clone()), &built.join("m.wasm"), &work).unwrap();
        let index = publish_index(&work, None).unwrap();
        assert_eq!(index.plugins.len(), 1);
        let bytes = std::fs::read(work.join(INDEX_FILE)).unwrap();
        assert_eq!(Index::parse(&bytes).unwrap(), index);
        let to = tmp("to");
        assert_eq!(unpack(&work, &to).unwrap(), 1);
        assert!(to.join("vibration/manifest.json").is_file() && to.join("vibration").join(&file).is_file());
        std::fs::write(work.join(&file), b"changed").unwrap();
        assert!(publish_index(&work, None).unwrap_err().contains("changed after"));
        let mut other = m;
        other.version = "2.0.1".into();
        let why = pack_one(&krate(other), &built.join("m.wasm"), &work).unwrap_err();
        assert!(why.contains("another plugin.json"), "{why}");
        for d in [work, built, to] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn an_older_corvus_goes_on_getting_the_newest_plugin_it_can_run() {
        let first = merge(vec![entry("2.0.0", "2026.11.94")], None);
        let second = merge(vec![entry("2.1.0", "2026.11.94")], Some(first));
        let versions = |i: &Index| {
            i.plugins
                .iter()
                .map(|e| format!("{} {}", e.manifest.version, e.manifest.min_corvus))
                .collect::<Vec<_>>()
        };
        assert_eq!(versions(&second), ["2.1.0 2026.11.94"]);
        let third = merge(vec![entry("3.0.0", "2027.02.01")], Some(second));
        assert_eq!(versions(&third), ["2.1.0 2026.11.94", "3.0.0 2027.02.01"]);
        assert_eq!(third.best("vibration", "2026.12.05").unwrap().manifest.version, "2.1.0");
        assert_eq!(third.best("vibration", "2027.02.01").unwrap().manifest.version, "3.0.0");
        // Published again as it is: one entry, the new module.
        let mut again = entry("3.0.0", "2027.02.01");
        again.sha256 = corvus_package::sha256_hex(b"rebuilt");
        let fourth = merge(vec![again.clone()], Some(third));
        assert_eq!(fourth.plugins.len(), 2);
        assert!(fourth.plugins.contains(&again));
    }
}
