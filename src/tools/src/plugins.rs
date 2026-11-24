//! Plugin packages: `plugin-pack`, `plugin-unpack` and `plugin-index`.
//!
//! A plugin crate that sets `loadable = true` under `[package.metadata.corvus]` is built as a
//! shared library and published instead of being compiled into the program. Each platform's job
//! runs `plugin-pack` over its own build, which leaves one library and one `.entry.json` per
//! plugin; the release job runs `plugin-index` over all of them and merges the platforms into
//! one list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use corvus_package::{Entry, File, Index};
use serde_json::{json, Value};

/// The folders plugin crates live in, relative to the repository root.
const ROOTS: [&str; 1] = ["plugins"];

/// A plugin crate that is published as a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crate {
    pub id: String,
    pub package: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub version: String,
}

/// The plugin crates of the repository that are shipped as libraries, by id.
pub fn loadable_crates(root: &Path) -> Vec<Crate> {
    let mut out = Vec::new();
    for r in ROOTS {
        for e in std::fs::read_dir(root.join(r)).into_iter().flatten().flatten() {
            let Some(id) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(e.path().join("Cargo.toml")) else {
                continue;
            };
            let Ok(manifest) = text.parse::<toml::Table>() else {
                continue;
            };
            let Some(package) = manifest.get("package").and_then(|p| p.as_table()) else {
                continue;
            };
            let Some(meta) = package
                .get("metadata")
                .and_then(|m| m.get("corvus"))
                .and_then(|c| c.as_table())
                .filter(|c| c.get("loadable").and_then(|l| l.as_bool()) == Some(true))
            else {
                continue;
            };
            let text = |k: &str| meta.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            out.push(Crate {
                package: package.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                name: if text("name").is_empty() {
                    id.clone()
                } else {
                    text("name")
                },
                description: text("description"),
                icon: text("icon"),
                version: text("version"),
                id,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The file cargo writes for a library crate on this computer.
pub fn built_library(target: &Path, package: &str) -> PathBuf {
    target.join(corvus_package::built_name(package))
}

/// Copies one built library to `out`, makes it loadable on this system (ad hoc code signature on
/// macOS), and writes its `.entry.json`. Returns the library's published name.
pub fn pack_one(c: &Crate, library: &Path, out: &Path, platform: &str) -> Result<String, String> {
    let bytes = std::fs::read(library).map_err(|e| format!("{}: {e} (build it first)", library.display()))?;
    let abi = corvus_package::abi_of(&bytes)
        .ok_or_else(|| format!("{} is no plugin library (export_plugin! is missing)", library.display()))?
        .to_string();
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let raw = out.join(format!("{}-{platform}.packing", c.id));
    std::fs::write(&raw, &bytes).map_err(|e| format!("{}: {e}", raw.display()))?;
    if cfg!(target_os = "macos") {
        let ok = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&raw)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err(format!("codesign failed for {}", raw.display()));
        }
    }
    // Hash what is published, after signing changed the bytes.
    let published = std::fs::read(&raw).map_err(|e| format!("{}: {e}", raw.display()))?;
    let sha256 = corvus_package::sha256_hex(&published);
    let file = corvus_package::published_name(&c.id, platform, &sha256);
    let path = out.join(&file);
    std::fs::rename(&raw, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut files = BTreeMap::new();
    files.insert(
        platform.to_string(),
        File {
            file: file.clone(),
            sha256,
            size: published.len() as u64,
        },
    );
    let entry = Entry {
        id: c.id.clone(),
        name: c.name.clone(),
        description: c.description.clone(),
        icon: c.icon.clone(),
        version: c.version.clone(),
        files,
    };
    let fragment = json!({"abi": abi, "entry": entry.to_json()});
    let frag_path = out.join(format!("{}-{platform}.entry.json", c.id));
    std::fs::write(&frag_path, serde_json::to_vec_pretty(&fragment).unwrap_or_default())
        .map_err(|e| format!("{}: {e}", frag_path.display()))?;
    Ok(file)
}

/// Builds the program and every library in one cargo call, as the platform build scripts do. Apart
/// from each other cargo compiles the crates they share differently (features, link time
/// optimisation), and the program refuses a library whose types are not its own.
pub fn build(root: &Path) -> Result<(), String> {
    let status = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(root)
        .args(["build", "--release", "--locked", "--bin", "corvus-gcs", "--lib"])
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("cargo build failed ({s})")),
        Err(e) => Err(format!("cargo: {e}")),
    }
}

/// Packs every loadable crate of the repository from `target`, or only the plugin `only`.
pub fn pack(root: &Path, target: &Path, out: &Path, only: Option<&str>) -> Result<Vec<String>, String> {
    let platform = corvus_package::platform();
    let mut crates = loadable_crates(root);
    crates.retain(|c| only.is_none_or(|id| c.id == id));
    if crates.is_empty() {
        return Err(match only {
            Some(id) => format!("plugins/{id} is no plugin crate that sets loadable = true"),
            None => "no plugin crate sets loadable = true".into(),
        });
    }
    crates
        .iter()
        .map(|c| pack_one(c, &built_library(target, &c.package), out, &platform))
        .collect()
}

/// Installs the plugins that `pack` left in `out` for this platform into `to`, as the store does,
/// for a check that the finished program loads them (`corvus-gcs --check-plugins`).
pub fn unpack(out: &Path, to: &Path) -> Result<usize, String> {
    let platform = corvus_package::platform();
    let mut n = 0;
    let index = merge(out)?;
    for e in &index.plugins {
        let Some(f) = e.file_for(&platform) else { continue };
        let folder = to.join(&e.id);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).map_err(|er| format!("{}: {er}", folder.display()))?;
        std::fs::copy(out.join(&f.file), folder.join(&f.file)).map_err(|er| format!("{}: {er}", f.file))?;
        let manifest = json!({"id": e.id, "version": e.version, "library": f.file, "sha256": f.sha256});
        std::fs::write(folder.join("manifest.json"), manifest.to_string()).map_err(|er| er.to_string())?;
        n += 1;
    }
    if n == 0 {
        return Err(format!("{} holds no plugin for {platform}", out.display()));
    }
    Ok(n)
}

/// Merges the `.entry.json` files of every platform in `dir` into one list.
pub fn merge(dir: &Path) -> Result<Index, String> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".entry.json"))
        .collect();
    names.sort();
    let mut abi: Option<String> = None;
    let mut plugins: BTreeMap<String, Entry> = BTreeMap::new();
    for p in names {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        let this = v.get("abi").and_then(Value::as_str).unwrap_or("").to_string();
        match &abi {
            Some(a) if *a != this => {
                return Err(format!(
                    "{} was built for {this}, the others for {a}: every platform must build the same source with the same compiler",
                    p.display()
                ))
            }
            None => abi = Some(this),
            _ => {}
        }
        let entry = Entry::from_json(v.get("entry").unwrap_or(&Value::Null))?;
        match plugins.get_mut(&entry.id) {
            Some(have) => {
                if have.version != entry.version {
                    return Err(format!(
                        "{} has two versions: {} and {}",
                        entry.id, have.version, entry.version
                    ));
                }
                have.files.extend(entry.files);
            }
            None => {
                plugins.insert(entry.id.clone(), entry);
            }
        }
    }
    let abi = abi.ok_or("no .entry.json files found")?;
    Ok(Index {
        abi,
        plugins: plugins.into_values().collect(),
    })
}

/// Writes the list into `dir`, after checking that every file it names is the one that was packed.
pub fn publish_index(dir: &Path) -> Result<Index, String> {
    let index = merge(dir)?;
    for e in &index.plugins {
        for f in e.files.values() {
            let p = dir.join(&f.file);
            let have = corvus_package::sha256_file(&p).map_err(|er| format!("{}: {er}", p.display()))?;
            if have != f.sha256 {
                return Err(format!("{} changed after it was packed", p.display()));
            }
        }
    }
    std::fs::write(dir.join(corvus_package::INDEX_FILE), index.to_bytes()).map_err(|e| e.to_string())?;
    Ok(index)
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

    fn lib_of(abi: &str, rest: &str) -> Vec<u8> {
        format!("\0CORVUS-PLUGIN\0{abi}\0{rest}").into_bytes()
    }

    fn crate_() -> Crate {
        Crate {
            id: "vibration".into(),
            package: "corvus_plugin_vibration".into(),
            name: "Vibration Monitor".into(),
            description: "d".into(),
            icon: "activity".into(),
            version: "1.0.0".into(),
        }
    }

    #[test]
    fn the_repositorys_loadable_plugins_are_found_with_their_details() {
        let found = loadable_crates(&crate::repo_root());
        let v = found
            .iter()
            .find(|c| c.id == "vibration")
            .expect("vibration is loadable");
        assert_eq!(v.package, "corvus_plugin_vibration");
        assert_eq!(v.name, "Vibration Monitor");
        assert!(!v.description.is_empty() && !v.icon.is_empty() && !v.version.is_empty());
    }

    #[test]
    fn two_platforms_become_one_list() {
        let (work, lib) = (tmp("merge"), tmp("lib"));
        std::fs::write(lib.join("a"), lib_of("1|rustc", "mac library")).unwrap();
        std::fs::write(lib.join("b"), lib_of("1|rustc", "linux library")).unwrap();
        pack_one(&crate_(), &lib.join("a"), &work, "macos-aarch64").unwrap();
        pack_one(&crate_(), &lib.join("b"), &work, "linux-x86_64").unwrap();
        let index = publish_index(&work).unwrap();
        assert_eq!(index.plugins.len(), 1);
        assert_eq!(index.plugins[0].files.len(), 2);
        let bytes = std::fs::read(work.join(corvus_package::INDEX_FILE)).unwrap();
        assert_eq!(corvus_package::Index::parse(&bytes).unwrap(), index);
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn platforms_built_by_different_compilers_are_not_merged() {
        let (work, lib) = (tmp("abi"), tmp("abilib"));
        std::fs::write(lib.join("a"), lib_of("1|rustc 1.99", "")).unwrap();
        std::fs::write(lib.join("b"), lib_of("1|rustc 1.98", "")).unwrap();
        pack_one(&crate_(), &lib.join("a"), &work, "macos-aarch64").unwrap();
        pack_one(&crate_(), &lib.join("b"), &work, "linux-x86_64").unwrap();
        assert!(merge(&work).unwrap_err().contains("same compiler"));
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn a_file_changed_after_packing_is_not_published() {
        let (work, lib) = (tmp("late"), tmp("latelib"));
        std::fs::write(lib.join("a"), lib_of("1|rustc", "x")).unwrap();
        let file = pack_one(&crate_(), &lib.join("a"), &work, "linux-x86_64").unwrap();
        std::fs::write(work.join(file), b"changed").unwrap();
        assert!(publish_index(&work).unwrap_err().contains("changed after"));
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&lib);
    }
}
