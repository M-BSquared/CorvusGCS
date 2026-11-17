//! Lists the plugins. Every folder of `src/plugins` (the bundled ones) and of
//! `plugins` (the ones that are repositories of their own) that holds a `Cargo.toml`
//! is a plugin crate named `corvus_plugin_<folder>` (dashes as underscores)
//! with a `plugin()` that makes one and a `LANG` with its language files;
//! the list the registry is built from is made here, so adding a plugin is
//! adding a folder and the one dependency line that Cargo needs. A folder
//! without that line fails the build rather than going missing silently.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // The bundled plugins, and the ones that are repositories of their own.
    let roots = ["src/plugins", "plugins"];
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

    let mut folders: Vec<(String, &str)> = roots
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
    folders.sort();
    let mut code = String::from(
        "/// The compiled-in plugins, in the order of their `order`.\npub fn builtin() -> Vec<Box<dyn Plugin>> {\n    let mut all: Vec<Box<dyn Plugin>> = Vec::new();\n",
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
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(Path::new(&out).join("plugin_list.rs"), code).expect("write plugin_list.rs");
}
