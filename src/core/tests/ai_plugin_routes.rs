//! The plugin routes, in process: the list, a plugin's own files, its saved
//! settings and the operator's folder.

mod common_ai;

use std::sync::{Arc, Mutex};

use common_ai::Tmp;
use corvus_core::plugins::PluginDirs;
use corvus_core::{Core, CoreBuilder, Options, PluginsModule};
use serde_json::{json, Map, Value};

struct Rig {
    tmp: Tmp,
    core: Core,
    opened: Arc<Mutex<Vec<String>>>,
}

fn rig(name: &str, config: Option<Value>) -> Rig {
    let tmp = Tmp::new(name);
    if let Some(c) = config {
        std::fs::write(tmp.0.join("config.json"), c.to_string()).unwrap();
    }
    let opened = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&opened);
    let dirs = PluginDirs {
        user: Some(tmp.0.join("plugins")),
        bundled: Some(tmp.0.join("bundled")),
        opener: Arc::new(move |p| {
            seen.lock().unwrap().push(p.to_string());
            (true, String::new())
        }),
        ..PluginDirs::default()
    };
    let core = CoreBuilder::new(Options::in_dir(&tmp.0))
        .module(PluginsModule::with_dirs(dirs))
        .build()
        .unwrap();
    core.start().unwrap();
    Rig { tmp, core, opened }
}

fn body(v: Value) -> Option<Map<String, Value>> {
    v.as_object().cloned()
}

#[test]
fn the_list_carries_the_plugins_the_roots_and_every_plugins_settings() {
    let r = rig("list", Some(json!({"plugins": {"old": {"a": 1}, "both": {"x": 1}}})));
    let dir = r.tmp.0.join("plugins/mine");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.json"), r#"{"name": "Mine", "order": 3}"#).unwrap();
    std::fs::write(dir.join("mine.js"), "x").unwrap();
    std::fs::create_dir_all(r.tmp.0.join("plugins/both")).unwrap();
    std::fs::write(r.tmp.0.join("plugins/both/config.json"), r#"{"y": 2}"#).unwrap();

    let (status, list) = r.core.internal_call("GET", "/api/plugins", None);
    assert_eq!(status, 200);
    let ids: Vec<&str> = list["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["mine", "vibration", "ssh-launcher", "schwalby", "map-entities"]);
    assert_eq!(list["user_dir"], json!(r.tmp.0.join("plugins").to_string_lossy()));
    assert_eq!(list["settings"], json!({"old": {"a": 1}, "both": {"y": 2}}));
}

#[test]
fn a_plugin_serves_its_own_files_and_nothing_else() {
    let r = rig("asset", None);
    let dir = r.tmp.0.join("plugins/mine");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("plugin.json"), "{}").unwrap();
    std::fs::write(dir.join("mine.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("sub/p.css"), "a{}").unwrap();
    std::fs::write(dir.join("secret.py"), "x").unwrap();
    std::fs::write(r.tmp.0.join("plugins/outside.js"), "no").unwrap();
    let get = |path: &str| {
        let mut req = corvus_core::Request::new(corvus_core::Method::Get, path);
        req.peer = Some("127.0.0.1".parse().unwrap());
        r.core.dispatch(req)
    };
    let ok = get("/api/plugins/asset/mine/mine.js");
    assert_eq!(ok.status, 200);
    assert_eq!(ok.header("content-type"), Some("text/javascript"));
    assert_eq!(ok.header("cache-control"), Some("no-cache"));
    assert_eq!(
        get("/api/plugins/asset/mine/sub/p.css").header("content-type"),
        Some("text/css")
    );
    for no in [
        "/api/plugins/asset/mine/secret.py",
        "/api/plugins/asset/mine/../outside.js",
        "/api/plugins/asset/nobody/mine.js",
        "/api/plugins/asset/bad%20id/mine.js",
        "/api/plugins/asset/mine/missing.js",
        "/api/plugins/asset/mine/plugin.json/..",
    ] {
        let resp = get(no);
        assert_eq!(
            (resp.status, resp.json_body()),
            (404, Some(json!({"error": "not found"}))),
            "{no}"
        );
    }
}

#[test]
fn settings_are_saved_merged_and_replaced_in_the_plugins_own_file() {
    let r = rig(
        "settings",
        Some(json!({"plugins": {"p": {"legacy": 1, "keep": 1}, "other": {"z": 1}}})),
    );
    let call = |payload: Value| r.core.internal_call("POST", "/api/plugins/settings", body(payload));
    assert_eq!(
        call(json!({})),
        (400, json!({"error": "id must be a non-empty string"}))
    );
    assert_eq!(call(json!({"id": 5})).0, 400);
    assert_eq!(
        call(json!({"id": "a b", "settings": {}})),
        (400, json!({"error": "id is not a valid plugin id"}))
    );
    assert_eq!(
        call(json!({"id": "p", "settings": []})),
        (400, json!({"error": "settings must be an object"}))
    );
    assert_eq!(
        call(json!({"id": "p", "settings": {}, "replace": "no"})),
        (400, json!({"error": "replace must be a boolean"}))
    );

    let (status, out) = call(json!({"id": "p", "settings": {"new": 2, "keep": 5}}));
    assert_eq!(
        (status, &out),
        (
            200,
            &json!({"ok": true, "settings": {"legacy": 1, "keep": 5, "new": 2}})
        )
    );
    let file = std::fs::read_to_string(r.tmp.0.join("plugins/p/config.json")).unwrap();
    assert_eq!(file, "{\n  \"legacy\": 1,\n  \"keep\": 5,\n  \"new\": 2\n}\n");
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(r.tmp.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        cfg["plugins"],
        json!({"other": {"z": 1}}),
        "the old copy left the main config"
    );

    let (_, out) = call(json!({"id": "p", "settings": {"only": true}, "replace": true}));
    assert_eq!(out["settings"], json!({"only": true}));
    let (_, out) = call(json!({"id": "p", "settings": {"more": 1}}));
    assert_eq!(out["settings"], json!({"only": true, "more": 1}));
    let (_, out) = call(json!({"id": "other", "settings": {"y": 1}}));
    assert_eq!(out["settings"], json!({"z": 1, "y": 1}));
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(r.tmp.0.join("config.json")).unwrap()).unwrap();
    assert!(cfg.get("plugins").is_none(), "the last old entry took the key with it");
}

#[test]
fn the_folder_route_opens_the_operators_folder_and_says_where_it_is() {
    let r = rig("folder", None);
    let (status, out) = r.core.internal_call("POST", "/api/plugins/folder", None);
    let path = r.tmp.0.join("plugins").to_string_lossy().into_owned();
    assert_eq!(
        (status, out),
        (200, json!({"ok": true, "path": path.clone(), "error": ""}))
    );
    assert_eq!(*r.opened.lock().unwrap(), vec![path]);
}
