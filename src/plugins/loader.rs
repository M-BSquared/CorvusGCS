//! Plugins the operator installed: WebAssembly modules under
//! `~/.corvus/plugins-wasm/<id>/`, the same file on every system.
//!
//! A package folder holds `manifest.json` (`{"id", "version", "module", "sha256"}`)
//! and the module it names, beside which its compiled form is kept. Before a
//! module is started its checksum is compared and its own manifest read: **one
//! that needs a newer Corvus GCS than this is left out, and nothing else about
//! the program is compared.** A refused or broken package is a line for the
//! operator and never stops the program; a plugin runs in a sandbox
//! ([`super::wasm`]) and cannot take it down.
//!
//! Up to 2026.11.93 plugins were native libraries under
//! `~/.corvus/plugin-packages`. This program does not load those and leaves the
//! folder as it is, so going back to that version finds them; it only reads which
//! plugins were there ([`before`]), for the setup to offer them again.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use corvus_package::Manifest;
use serde_json::Value;

use super::wasm::{runtime, Sandbox, WasmPlugin};

pub const MANIFEST: &str = "manifest.json";

/// The folder in `~/.corvus` that installed plugins are kept in.
const PACKAGES: &str = "plugins-wasm";
/// Where the native libraries of earlier versions were.
const NATIVE_PACKAGES: &str = "plugin-packages";

/// The most a module may be.
pub const MODULE_CAP: usize = 64 * 1024 * 1024;

/// What is left over in the package folder is cleared away only when it is this old: another
/// window may be installing right now.
const LEFTOVER_AGE: Duration = Duration::from_secs(600);

static OFF: AtomicBool = AtomicBool::new(false);

/// Loads no packages in this run (`--no-plugins`).
pub fn switch_off() {
    OFF.store(true, Ordering::Relaxed);
}

/// A package that did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub id: String,
    /// In plain words, without the full stop.
    pub why: String,
    /// It needs a newer Corvus GCS than this one.
    pub needs_newer: bool,
}

/// A plugin that loaded, and the checksum of the module it came from.
pub struct Package {
    pub plugin: WasmPlugin,
    pub manifest: Manifest,
    pub sha256: String,
}

/// What `installed` found: the plugins that loaded and why the others did not.
#[derive(Default)]
pub struct Found {
    pub packages: Vec<Package>,
    pub refused: Vec<Refused>,
}

fn home() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".corvus"))
}

/// Where the packages are kept: `CORVUS_PLUGIN_PACKAGES`, else `~/.corvus/plugins-wasm`.
/// Nowhere for a test that names no folder, which must not run what the developer installed.
pub fn packages_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CORVUS_PLUGIN_PACKAGES").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    home().map(|h| h.join(PACKAGES))
}

/// Where an earlier version kept its native plugins.
pub fn native_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    home().map(|h| h.join(NATIVE_PACKAGES))
}

/// The plugins that were installed in `dir` as native libraries, by 2026.11.92 or 93.
pub fn before(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join(MANIFEST).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|id| corvus_package::is_valid_id(id) && !super::PARTS.contains(&id.as_str()))
        .collect()
}

/// Loads every package under `dir` but those in `taken`, which the program has already.
pub fn installed(sandbox: &Arc<Sandbox>, dir: &Path, taken: &[&str]) -> Found {
    let mut found = Found::default();
    if OFF.load(Ordering::Relaxed) {
        return found;
    }
    tidy(dir);
    let Ok(read) = std::fs::read_dir(dir) else {
        return found;
    };
    let mut ids: Vec<String> = read
        .flatten()
        .filter(|e| e.path().join(MANIFEST).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| corvus_package::is_valid_id(n))
        .collect();
    ids.sort();
    for id in ids {
        // A copy from the time it was a plugin: nothing to load and nothing to tell.
        if super::PARTS.contains(&id.as_str()) {
            continue;
        }
        if taken.contains(&id.as_str()) {
            found.refused.push(Refused {
                id,
                why: "this plugin is part of this version of Corvus GCS, so the installed one is not needed".into(),
                needs_newer: false,
            });
            continue;
        }
        match load_package(sandbox, dir, &id) {
            Ok(p) => found.packages.push(p),
            Err(r) => found.refused.push(r),
        }
    }
    found
}

/// Clears away what an interrupted install or a removal left behind: a module no manifest names
/// and what was compiled of it, a half written file, a folder with no manifest.
fn tidy(dir: &Path) {
    let old = |p: &Path| {
        std::fs::symlink_metadata(p)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|e| e > LEFTOVER_AGE))
    };
    for folder in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()) {
        let valid = folder
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(corvus_package::is_valid_id);
        if !valid || !folder.is_dir() {
            continue;
        }
        let named = read_manifest(&folder).ok();
        if named.is_none() && folder.join(MANIFEST).exists() {
            // A manifest that does not read is for the operator to see, not to lose.
            continue;
        }
        let module = named.as_ref().map(|m| m.module.as_str());
        let compiled_of = named.as_ref().and_then(|m| m.sha256.get(..16));
        for e in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let ending = |x: &str| e.path().extension().is_some_and(|y| y == x);
            let left_over = if ending("tmp") {
                true
            } else if ending(corvus_package::EXTENSION) {
                module != Some(name.as_str())
            } else if ending(runtime::COMPILED) {
                !compiled_of.is_some_and(|sum| name.starts_with(sum))
            } else {
                false
            };
            if left_over && old(&e.path()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        if named.is_none() {
            // Only an empty folder goes.
            let _ = std::fs::remove_dir(&folder);
        }
    }
}

/// What the program wrote down about an installed package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub version: String,
    pub module: String,
    pub sha256: String,
    /// What the module may do, as the operator saw it when it was installed.
    pub permissions: Vec<String>,
    /// The file beside the program it was installed from, and what that looked like
    /// (`store::follow_beside`); empty for one from a release or from anywhere else.
    pub origin: String,
    pub origin_stamp: String,
}

pub fn read_manifest(folder: &Path) -> Result<Installed, String> {
    let text =
        std::fs::read_to_string(folder.join(MANIFEST)).map_err(|e| format!("manifest.json cannot be read: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("manifest.json is not valid: {e}"))?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let module = s("module");
    if !corvus_package::is_plain_file_name(&module) {
        return Err("manifest.json names no module".into());
    }
    Ok(Installed {
        id: s("id"),
        version: s("version"),
        module,
        sha256: s("sha256").to_ascii_lowercase(),
        permissions: v
            .get("permissions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect(),
        origin: s("origin"),
        origin_stamp: s("origin_stamp"),
    })
}

/// What stands in the way of this program running a module with this manifest.
pub fn needs_newer(manifest: &Manifest) -> Option<String> {
    let have = corvus_core::version::version();
    (!manifest.fits(have)).then(|| format!("it needs Corvus GCS {} or newer, this is {have}", manifest.min_corvus))
}

/// Loads the package `id` of `dir`: its module, checked, started in the sandbox.
pub fn load_package(sandbox: &Arc<Sandbox>, dir: &Path, id: &str) -> Result<Package, Refused> {
    let refuse = |why: String| Refused {
        id: id.to_string(),
        why,
        needs_newer: false,
    };
    if OFF.load(Ordering::Relaxed) {
        return Err(refuse("plugins are switched off for this run (--no-plugins)".into()));
    }
    let folder = dir.join(id);
    let installed = read_manifest(&folder).map_err(refuse)?;
    if installed.id != id {
        return Err(refuse(format!(
            "manifest.json is of {:?}, not of this folder",
            installed.id
        )));
    }
    let path = folder.join(&installed.module);
    let bytes = std::fs::read(&path).map_err(|e| refuse(format!("the module cannot be read: {e}")))?;
    let sha256 = corvus_package::sha256_hex(&bytes);
    // A development build may put a module in place by hand, without a checksum.
    if installed.sha256 != sha256 && !(installed.sha256.is_empty() && cfg!(debug_assertions)) {
        return Err(refuse(
            "the module is not the file that was installed (its checksum differs)".into(),
        ));
    }
    let mut manifest = Manifest::of_module(&bytes).map_err(refuse)?;
    if manifest.id != id {
        return Err(refuse(format!("the module is the plugin {:?}", manifest.id)));
    }
    if let Some(why) = needs_newer(&manifest) {
        return Err(Refused {
            id: id.to_string(),
            why,
            needs_newer: true,
        });
    }
    // No more than the operator saw when it was installed, whatever the module asks for.
    manifest
        .permissions
        .retain(|p| installed.permissions.contains(p));
    let plugin = WasmPlugin::load(sandbox, &manifest, &bytes, Some(&folder))
        .map_err(|e| refuse(format!("it could not be started: {e}")))?;
    Ok(Package {
        plugin,
        manifest,
        sha256,
    })
}

/// `--check-plugins DIR`: whether every package of `dir` loads in this program. For the build,
/// which runs it with the finished program and the modules it made.
pub fn check(dir: &Path) -> Result<String, String> {
    use corvus_api::plugin::Plugin;
    let sandbox = Sandbox::new()?;
    let found = installed(&sandbox, dir, &[]);
    for p in &found.packages {
        println!("plugin {} {}: loads", p.manifest.id, p.plugin.info().version);
    }
    if found.refused.is_empty() && !found.packages.is_empty() {
        return Ok(format!("{} plugins load", found.packages.len()));
    }
    let why: Vec<String> = found.refused.iter().map(|r| format!("{}: {}", r.id, r.why)).collect();
    Err(if why.is_empty() {
        format!("no plugin packages in {}", dir.display())
    } else {
        why.join("\n")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::wasm::testlib::{self, empty, package};
    use serde_json::json;

    fn probe(dir: &Path) -> PathBuf {
        package(dir, "probe", "corvus_plugin_probe")
    }

    fn refused(dir: &Path) -> Refused {
        let found = installed(&testlib::sandbox(), dir, &[]);
        assert!(found.packages.is_empty());
        found.refused.into_iter().next().expect("one refused")
    }

    #[test]
    fn an_installed_package_loads_and_keeps_what_was_compiled() {
        let dir = empty("loads");
        let folder = probe(&dir);
        let compiled = || {
            std::fs::read_dir(&folder)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == runtime::COMPILED))
                .map(|e| e.path())
                .collect::<Vec<_>>()
        };
        assert!(compiled().is_empty());
        assert_eq!(installed(&testlib::sandbox(), &dir, &[]).refused, []);
        let kept = compiled();
        assert_eq!(kept.len(), 1);
        assert_eq!(installed(&testlib::sandbox(), &dir, &[]).packages.len(), 1);
        // A damaged one is made again and not run.
        let mut bytes = std::fs::read(&kept[0]).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&kept[0], &bytes).unwrap();
        assert_eq!(installed(&testlib::sandbox(), &dir, &[]).packages.len(), 1);
        assert_ne!(std::fs::read(&kept[0]).unwrap(), bytes);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_package_that_cannot_load_is_refused_with_a_reason() {
        let dir = empty("refused");
        let folder = probe(&dir);
        let good = std::fs::read(folder.join(MANIFEST)).unwrap();
        let mut v: Value = serde_json::from_slice(&good).unwrap();
        v["sha256"] = json!("00");
        std::fs::write(folder.join(MANIFEST), v.to_string()).unwrap();
        assert!(refused(&dir).why.contains("checksum"));
        std::fs::write(folder.join(MANIFEST), "{").unwrap();
        assert!(refused(&dir).why.contains("not valid"));
        v["module"] = json!("../x.wasm");
        std::fs::write(folder.join(MANIFEST), v.to_string()).unwrap();
        assert!(refused(&dir).why.contains("names no module"));
        std::fs::write(folder.join(MANIFEST), &good).unwrap();
        std::fs::rename(&folder, dir.join("other")).unwrap();
        assert!(refused(&dir).why.contains("not of this folder"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_package_gets_no_more_than_was_seen_when_it_was_installed() {
        let dir = empty("grant");
        let folder = probe(&dir);
        let mut v: Value = serde_json::from_slice(&std::fs::read(folder.join(MANIFEST)).unwrap()).unwrap();
        v["permissions"] = json!(["ai.tools", "local"]);
        std::fs::write(folder.join(MANIFEST), v.to_string()).unwrap();
        let found = installed(&testlib::sandbox(), &dir, &[]);
        assert_eq!(found.packages[0].manifest.permissions, ["ai.tools"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_plugin_the_program_has_is_not_loaded_a_second_time() {
        let dir = empty("taken");
        probe(&dir);
        let found = installed(&testlib::sandbox(), &dir, &["probe"]);
        assert!(found.packages.is_empty());
        assert!(found.refused[0].why.contains("part of this version"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_was_installed_as_a_native_library_is_known_and_left_alone() {
        let dir = empty("native");
        for id in ["tak", "schwalby", "ssh-launcher", "half"] {
            std::fs::create_dir_all(dir.join(id)).unwrap();
        }
        for id in ["tak", "schwalby", "ssh-launcher"] {
            std::fs::write(dir.join(id).join(MANIFEST), r#"{"library": "x.dylib"}"#).unwrap();
        }
        assert_eq!(before(&dir).into_iter().collect::<Vec<_>>(), ["schwalby", "tak"]);
        assert!(dir.join("tak").join(MANIFEST).exists());
        assert!(before(&dir.join("none")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_packages_folder_is_no_packages() {
        let found = installed(&testlib::sandbox(), Path::new("/no/such/folder"), &[]);
        assert!(found.packages.is_empty() && found.refused.is_empty());
    }
}
