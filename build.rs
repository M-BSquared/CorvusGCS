//! Lists the plugins. Every folder of `plugins` that holds a `Cargo.toml` is a
//! plugin crate named `corvus_plugin_<folder>` (dashes as underscores), each a
//! repository of its own. One with `loadable = true` is built as a shared
//! library and installed by the operator: it goes on the list of what there
//! is. One without is compiled in: the list the registry is built from is made
//! here from its `plugin()` and its `LANG`, and it needs a dependency line. A
//! folder without that line fails the build rather than going missing silently.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// `[package.metadata.corvus]` of a plugin folder's manifest and its repository, when it says
/// `loadable = true`: that plugin is built as a shared library and installed by the operator, the
/// program only loads it.
fn loadable(manifest: &Path) -> Option<(toml::Table, String)> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let table: toml::Table = text.parse().ok()?;
    let package = table.get("package")?;
    let meta = package.get("metadata")?.get("corvus")?.as_table()?.clone();
    let repository = package.get("repository").and_then(|r| r.as_str()).unwrap_or("");
    (meta.get("loadable")?.as_bool()?).then(|| (meta, repository.trim_end_matches('/').to_string()))
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let roots = ["plugins"];
    for r in roots {
        println!("cargo:rerun-if-changed={}", root.join(r).display());
    }
    println!("cargo:rerun-if-changed={}", root.join("Cargo.toml").display());
    let manifest: toml::Table = std::fs::read_to_string(root.join("Cargo.toml"))
        .expect("read Cargo.toml")
        .parse()
        .expect("parse Cargo.toml");
    let deps = manifest
        .get("dependencies")
        .and_then(|d| d.as_table())
        .cloned()
        .unwrap_or_default();

    let all: Vec<(String, &str)> = roots
        .iter()
        .flat_map(|r| {
            std::fs::read_dir(root.join(r))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().join("Cargo.toml").is_file())
                .filter_map(|e| e.file_name().into_string().ok())
                .map(move |name| (name, *r))
        })
        .collect();
    let mut folders = Vec::new();
    let mut offered = Vec::new();
    for (f, at) in all {
        let manifest = root.join(at).join(&f).join("Cargo.toml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        match loadable(&manifest) {
            Some((meta, repository)) => {
                assert!(
                    repository.starts_with("https://github.com/"),
                    "{at}/{f}/Cargo.toml: [package] repository names no GitHub repository to install it from"
                );
                offered.push((f, meta, repository));
            }
            None => folders.push((f, at)),
        }
    }
    folders.sort();
    offered.sort_by(|a, b| a.0.cmp(&b.0));
    let mut code = String::from(
        "/// The compiled-in plugins, in the order of their `order`.\npub fn builtin() -> Vec<Box<dyn Plugin>> {\n    #[allow(unused_mut)]\n    let mut all: Vec<Box<dyn Plugin>> = Vec::new();\n",
    );
    for (f, at) in &folders {
        let krate = format!("corvus_plugin_{}", f.replace('-', "_"));
        assert!(
            deps.contains_key(&krate),
            "{at}/{f} is a plugin crate: add `{krate} = {{ path = \"{at}/{f}\" }}` to [dependencies] in Cargo.toml"
        );
        writeln!(
            code,
            "    corvus_api::i18n::register_plugin_files({krate}::LANG);\n    all.push({krate}::plugin());"
        )
        .unwrap();
    }
    code.push_str(
        "    all.sort_by(|a, b| {\n        let (a, b) = (a.info(), b.info());\n        (a.order, a.name.to_lowercase()).cmp(&(b.order, b.name.to_lowercase()))\n    });\n    all\n}\n",
    );
    code.push_str("\n/// The plugins of this source that are installed by the operator.\npub const OFFERED: &[store::Offered] = &[\n");
    for (f, meta, repository) in &offered {
        let text = |k: &str| meta.get(k).and_then(|v| v.as_str()).unwrap_or("");
        writeln!(
            code,
            "    store::Offered {{ id: {f:?}, package: {:?}, name: {:?}, description: {:?}, version: {:?}, repository: {repository:?} }},",
            format!("corvus_plugin_{}", f.replace('-', "_")),
            text("name"),
            text("description"),
            text("version"),
        )
        .unwrap();
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(Path::new(&out).join("plugin_list.rs"), code).expect("write plugin_list.rs");
}
