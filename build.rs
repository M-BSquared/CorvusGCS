//! Lists the plugins there are. Every folder of `plugins` that holds a
//! `plugin.json` is a plugin, each a repository of its own: what the manifest
//! says, with the repository its `Cargo.toml` names, goes on the list the store
//! shows. None of them is part of the program: each is a WebAssembly module the
//! operator installs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn repository(folder: &Path) -> String {
    let manifest = folder.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
    let table: toml::Table = text.parse().unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
    table
        .get("package")
        .and_then(|p| p.get("repository"))
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string()
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let plugins = root.join("plugins");
    println!("cargo:rerun-if-changed={}", plugins.display());
    let mut folders: Vec<String> = std::fs::read_dir(&plugins)
        .expect("the plugins folder")
        .flatten()
        .filter(|e| e.path().join("plugin.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    folders.sort();
    let mut code = String::from(
        "/// The plugins of this source, each installed by the operator.\npub const OFFERED: &[store::Offered] = &[\n",
    );
    for id in &folders {
        let folder = plugins.join(id);
        let path = folder.join("plugin.json");
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let manifest = corvus_package::Manifest::parse(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            manifest.id,
            *id,
            "{}: the id must be the name of the folder",
            path.display()
        );
        let repository = repository(&folder);
        assert!(
            repository.starts_with("https://github.com/"),
            "plugins/{id}/Cargo.toml: [package] repository names no GitHub repository to install it from"
        );
        writeln!(
            code,
            "    store::Offered {{ id: {id:?}, name: {:?}, description: {:?}, version: {:?}, min_corvus: {:?}, permissions: &{:?}, repository: {repository:?} }},",
            manifest.name, manifest.description, manifest.version, manifest.min_corvus, manifest.permissions,
        )
        .expect("a line");
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("plugin_list.rs"), code).expect("write plugin_list.rs");
}
