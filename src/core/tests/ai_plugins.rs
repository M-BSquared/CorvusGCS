//! The plugin registry, the plugin config files and the plugin files behave as
//! the Python modules do (`python/tools/dump_ai_cases.py plugins plugin_config
//! plugin_files`), on the same folders.

mod common_ai;

use std::path::Path;

use common_ai::{listing, materialize, Tmp};
use corvus_core::plugins::config;
use corvus_core::plugins::files::{self, FilesError};
use corvus_core::plugins::registry::{is_valid_id, resolve_asset, PluginRegistry};
use serde_json::{json, Value};

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[test]
fn the_registry_lists_the_same_plugins_as_python_from_the_same_folders() {
    let want = fixture(include_str!("data/ai_plugins.json"));
    let tmp = Tmp::new("registry");
    materialize(&tmp.0, want["tree"].as_object().unwrap());
    let reg = PluginRegistry::scanning(tmp.0.join("user"), tmp.0.join("bundled"));

    let rel = |dir: &Path| dir.strip_prefix(&tmp.0).unwrap().to_string_lossy().replace('\\', "/");
    let found: Vec<Value> = reg
        .discover()
        .iter()
        .map(|m| {
            let mut v = m.public();
            v["dir"] = json!(rel(&m.dir));
            v
        })
        .collect();
    assert_eq!(json!(found), want["discover"]);
    assert_eq!(json!(reg.public_list()), want["public"]);

    for (id, dir) in want["find_dir"].as_object().unwrap() {
        let got = reg.find_dir(id).map(|d| rel(&d));
        assert_eq!(json!(got), *dir, "find_dir {id:?}");
    }
    let bundled_only = PluginRegistry::scanning(tmp.0.join("none"), tmp.0.join("bundled"));
    let got: Vec<Value> = bundled_only
        .discover()
        .iter()
        .map(|m| {
            let mut v = m.public();
            v["dir"] = json!(rel(&m.dir));
            v
        })
        .collect();
    assert_eq!(json!(got), want["bundled_only"]);

    let zeta = tmp.0.join("bundled/zeta");
    let zeta_real = std::fs::canonicalize(&zeta).unwrap();
    for pair in want["assets"].as_array().unwrap() {
        let rel_in = pair[0].as_str().unwrap();
        let got =
            resolve_asset(&zeta, rel_in).map(|p| p.strip_prefix(&zeta_real).unwrap().to_string_lossy().into_owned());
        // A case insensitive disk answers with the name as it is spelled on
        // disk where Python keeps the caller's spelling; both name the file.
        let same = match (&got, pair[1].as_str()) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (None, None) => true,
            _ => false,
        };
        assert!(same, "asset {rel_in:?}: {got:?} against {}", pair[1]);
    }
    for pair in want["ids"].as_array().unwrap() {
        assert_eq!(json!(is_valid_id(pair[0].as_str().unwrap())), pair[1], "id {}", pair[0]);
    }
}

#[test]
fn the_user_readme_is_the_one_python_writes() {
    let want = fixture(include_str!("data/ai_plugins.json"));
    assert_eq!(corvus_core::plugins::registry::README, want["readme"].as_str().unwrap());
    let tmp = Tmp::new("readme");
    let reg = PluginRegistry::new(tmp.0.join("plugins"), tmp.0.join("bundled"));
    assert_eq!(reg.ensure_user_dir(), tmp.0.join("plugins"));
    assert_eq!(
        std::fs::read_to_string(tmp.0.join("plugins/README.md")).unwrap(),
        corvus_core::plugins::registry::README
    );
    std::fs::write(tmp.0.join("plugins/README.md"), "mine").unwrap();
    reg.ensure_user_dir();
    assert_eq!(
        std::fs::read_to_string(tmp.0.join("plugins/README.md")).unwrap(),
        "mine"
    );
}

#[test]
fn the_bundled_plugins_are_listed_as_python_lists_the_web_plugins() {
    let want = fixture(include_str!("data/ai_plugins_bundled.json"));
    let tmp = Tmp::new("bundled");
    let reg = PluginRegistry::new(tmp.0.join("user"), tmp.0.join("bundled"));
    assert_eq!(json!(reg.public_list()), want);
    let crates: Vec<String> = {
        let mut v: Vec<String> = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().join("Cargo.toml").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let mut listed: Vec<String> = reg.discover().iter().map(|m| m.id.clone()).collect();
    listed.sort();
    assert_eq!(
        listed, crates,
        "every plugin crate in src/plugins is listed, and no other"
    );
}

#[test]
fn plugin_config_files_are_read_and_written_as_python_does() {
    let want = fixture(include_str!("data/ai_plugin_config.json"));
    let tmp = Tmp::new("config");
    for step in want["ops"].as_array().unwrap() {
        let op = step["op"].as_array().unwrap();
        let name = |i: usize| op[i].as_str().unwrap_or("").to_string();
        let obj = |i: usize| op[i].as_object().cloned().unwrap_or_default();
        let got: Value = match op[0].as_str().unwrap() {
            "load" => json!(config::load(&name(1), &tmp.0)),
            "load_all" => json!(config::load_all(&tmp.0)),
            "save" => match config::save(&name(1), &obj(2), &tmp.0) {
                Ok(()) => json!("ok"),
                Err(e) => json!({"error": e.to_string()}),
            },
            "update" => match config::update(&name(1), &obj(2), op[3].as_bool().unwrap(), &tmp.0) {
                Ok(m) => json!(m),
                Err(e) => json!({"error": e.to_string()}),
            },
            "raw" => {
                std::fs::create_dir_all(tmp.0.join(name(1))).unwrap();
                std::fs::write(tmp.0.join(name(1)).join("config.json"), name(2)).unwrap();
                json!("ok")
            }
            "mkdir" => {
                std::fs::create_dir_all(tmp.0.join(name(1))).unwrap();
                json!("ok")
            }
            "migrate" => json!(config::migrate(Some(&op[1]).filter(|v| !v.is_null()), &tmp.0)),
            other => panic!("{other}"),
        };
        assert_eq!(got, step["result"], "op {op:?}");
    }
    let mut files = std::collections::BTreeMap::new();
    for (path, text) in listing(&tmp.0) {
        if path.ends_with("config.json") {
            files.insert(path, text);
        }
    }
    let want_files: std::collections::BTreeMap<String, String> = want["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    assert_eq!(files, want_files, "the files on disk, byte for byte");
}

#[test]
fn plugin_file_paths_are_cleaned_as_python_cleans_them() {
    let want = fixture(include_str!("data/ai_plugin_files.json"));
    for pair in want["clean"].as_array().unwrap() {
        let input = pair[0].as_str().unwrap();
        assert_eq!(json!(files::clean_path(input)), pair[1], "path {input:?}");
    }
    assert_eq!(want["limits"], json!([files::MAX_PLUGIN_BYTES, files::MAX_FILES]));
}

fn texts(f: &files::Files) -> Value {
    json!(f
        .iter()
        .map(|(k, v)| json!([k, String::from_utf8(v.clone()).unwrap()]))
        .collect::<Vec<_>>())
}

#[test]
fn plugin_folders_are_collected_as_python_collects_them() {
    let want = fixture(include_str!("data/ai_plugin_files.json"));
    let tmp = Tmp::new("collect");
    materialize(&tmp.0, want["tree"].as_object().unwrap());
    for name in ["p", "q", "r", "s"] {
        let got = match files::collect(&tmp.0.join(name)) {
            Ok(f) if name == "p" => json!({"files": texts(&f)}),
            Ok(f) => {
                let mut keys: Vec<&String> = f.keys().collect();
                keys.sort();
                let mut v: Vec<Value> = keys.iter().take(3).map(|k| json!(k)).collect();
                v.push(json!(f.len()));
                json!({"files": v})
            }
            Err(FilesError::Value(s)) => json!({"error": s}),
            Err(e) => panic!("{e}"),
        };
        assert_eq!(got, want["collected"][name], "plugin {name}");
    }
}

#[test]
fn a_set_of_files_is_checked_and_installed_as_python_does() {
    let want = fixture(include_str!("data/ai_plugin_files.json"));
    let big = vec![b'x'; files::MAX_PLUGIN_BYTES + 1];
    let fit = vec![b'x'; files::MAX_PLUGIN_BYTES];
    let many = |n: usize| {
        let mut f = files::Files::new();
        f.insert("plugin.json".into(), b"{}".to_vec());
        for i in 0..n {
            f.insert(format!("f{i}.js"), b"x".to_vec());
        }
        f
    };
    let one = |pairs: &[(&str, &[u8])]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_vec()))
            .collect::<files::Files>()
    };
    let sets: Vec<(&str, files::Files)> = vec![
        ("empty", files::Files::new()),
        ("none", files::Files::new()),
        ("manifest only", one(&[("plugin.json", b"{}")])),
        ("no manifest", one(&[("a.js", b"a")])),
        ("unclean", one(&[("plugin.json", b"{}"), ("../a.js", b"a")])),
        ("config", one(&[("plugin.json", b"{}"), ("config.json", b"{}")])),
        ("backslash", one(&[("plugin.json", b"{}"), ("a\\b.js", b"a")])),
        ("not bytes", one(&[("plugin.json", b"text")])),
        ("big", one(&[("plugin.json", b"{}"), ("a.txt", &big)])),
        ("just fits", one(&[("plugin.json", &fit)])),
        ("many", many(files::MAX_FILES)),
        ("just many", many(files::MAX_FILES - 1)),
    ];
    for (i, pair) in want["checks"].as_array().unwrap().iter().enumerate() {
        let (label, set) = &sets[i];
        assert_eq!(pair[0], *label);
        if *label == "not bytes" {
            continue;
        }
        let got = match files::check(set) {
            Ok(f) => {
                let mut keys: Vec<String> = f.keys().cloned().collect();
                keys.sort();
                json!({"ok": keys})
            }
            Err(e) => json!({"error": e.to_string()}),
        };
        assert_eq!(got, pair[1], "check {label}");
    }

    let tmp = Tmp::new("install");
    let user = tmp.0.join("plugins");
    let steps: Vec<(&str, files::Files)> = vec![
        ("bad id", one(&[("plugin.json", b"{}")])),
        ("no files", files::Files::new()),
        (
            "first",
            one(&[("plugin.json", b"{\"v\": 1}"), ("x.js", b"one"), ("sub/y.css", b"y")]),
        ),
        ("again", one(&[("plugin.json", b"{\"v\": 2}"), ("x.js", b"two")])),
    ];
    for (step, (label, set)) in want["installs"].as_array().unwrap().iter().zip(steps) {
        assert_eq!(step["step"], label);
        let id = if label == "bad id" { "bad id" } else { "x" };
        if label == "again" {
            std::fs::write(user.join("x/config.json"), "{\"saved\": 1}\n").unwrap();
        }
        let got = match files::install(id, &set, &user) {
            Ok(_) => json!({"step": label, "ok": true, "files": listing(&user)}),
            Err(e) => json!({"step": label, "error": e.to_string(), "files": listing(&user)}),
        };
        assert_eq!(got, *step, "install step {label}");
    }
    assert!(
        std::fs::read_dir(&user)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with('.')),
        "no staging folder is left behind"
    );
}
