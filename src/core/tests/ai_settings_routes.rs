//! Settings export, import, reset and the logo, in process.

mod common_ai;

use common_ai::Tmp;
use corvus_core::plugins::settings_routes::{safe_filename, slugify};
use corvus_core::plugins::PluginDirs;
use corvus_core::{Core, CoreBuilder, Options, PluginsModule};
use serde_json::{json, Map, Value};

fn rig(name: &str, config: Value) -> (Tmp, Core) {
    let tmp = Tmp::new(name);
    std::fs::write(tmp.0.join("config.json"), config.to_string()).unwrap();
    let dirs = PluginDirs {
        user: Some(tmp.0.join("plugins")),
        bundled: Some(tmp.0.join("bundled")),
        ..PluginDirs::default()
    };
    let core = CoreBuilder::new(Options::in_dir(&tmp.0))
        .module(PluginsModule::with_dirs(dirs))
        .build()
        .unwrap();
    core.start().unwrap();
    (tmp, core)
}

fn post(core: &Core, path: &str, body: Value) -> (u16, Value) {
    core.internal_call("POST", path, body.as_object().cloned().map(|m: Map<_, _>| m))
}

#[test]
fn a_file_name_is_made_safe_as_python_makes_it() {
    let f = |v: Value| safe_filename(Some(&v), "fallback.json", ".json");
    assert_eq!(f(json!("../../etc/x")), "x.json");
    assert_eq!(f(json!("My: file?.JSON")), "My file.json");
    assert_eq!(f(json!("..")), "fallback.json");
    assert_eq!(f(json!(".json")), "fallback.json.json");
    assert_eq!(f(json!(42)), "fallback.json");
    assert_eq!(f(json!("a".repeat(200))), format!("{}.json", "a".repeat(115)));
    assert_eq!(slugify("My Laptop_2"), "my-laptop-2");
    assert_eq!(slugify("--"), "");
}

#[test]
fn an_armed_vehicle_blocks_an_import_and_a_reset() {
    let (_tmp, core) = rig("armed", json!({"theme": {"name": "blue"}}));
    core.store().update([("armed", json!(true))]);
    assert_eq!(
        post(&core, "/api/settings/import", json!({"bundle": {}})),
        (
            409,
            json!({"ok": false, "error": "Settings cannot be imported while the vehicle is armed."})
        )
    );
    assert_eq!(
        post(&core, "/api/settings/reset", json!({})),
        (
            409,
            json!({"ok": false, "error": "Settings cannot be reset while the vehicle is armed."})
        )
    );
    assert_eq!(core.settings().get().block("theme").unwrap()["name"], "blue");
}

#[test]
fn an_export_is_imported_back_and_a_reset_forgets_it() {
    let (tmp, core) = rig(
        "roundtrip",
        json!({"theme": {"name": "blue"}, "http_port": 9000, "ssh_connections": [{"name": "a", "host": "h", "username": "u", "password": "secret"}]}),
    );
    let out = tmp.0.join("exports");
    let (status, answer) = post(
        &core,
        "/api/settings/export",
        json!({"dir": out.to_string_lossy(), "filename": "mine", "sections": ["interface", "connections", "folders"]}),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["filename"], "mine.json");
    assert_eq!(answer["secrets"], false);
    let written: Value = serde_json::from_str(&std::fs::read_to_string(out.join("mine.json")).unwrap()).unwrap();
    assert!(written["config"]["ssh_connections"][0].get("password").is_none());
    assert_eq!(written["sections"], json!(["interface", "connections", "folders"]));

    post(
        &core,
        "/api/config",
        json!({"theme": {"name": "red"}, "http_port": 9100}),
    );
    let (status, got) = post(&core, "/api/settings/import", json!({"bundle": written}));
    assert_eq!(status, 200, "{got}");
    assert_eq!(got["restart"], json!(["http_port"]));
    assert_eq!(got["warnings"], json!([]));
    assert_eq!(core.settings().get().block("theme").unwrap()["name"], "blue");
    assert_eq!(
        core.settings().get().ssh_connections()[0]["password"],
        "secret",
        "a file without secrets keeps the stored one"
    );

    std::fs::write(tmp.0.join("geofence.json"), r#"{"polygon": [[1, 2], [3, 4], [5, 6]]}"#).unwrap();
    std::fs::write(tmp.0.join("update.json"), "{}").unwrap();
    std::fs::write(tmp.0.join("mcp-token"), "keep").unwrap();
    let (status, got) = post(&core, "/api/settings/reset", json!({}));
    assert_eq!((status, got["ok"].clone()), (200, json!(true)));
    assert!(!tmp.0.join("config.json").exists());
    assert!(
        !tmp.0.join("geofence.json").exists(),
        "the fence area is part of the reset"
    );
    assert!(!tmp.0.join("update.json").exists());
    assert!(tmp.0.join("mcp-token").exists(), "a key is not a setting");
    assert!(core.settings().first_start_pending());
    assert!(core.settings().get().block("theme").is_none());
}

#[test]
fn a_logo_is_kept_beside_the_config_and_named_in_it() {
    let (tmp, core) = rig("logo", json!({}));
    let png: Vec<u8> = b"\x89PNG\r\n\x1a\n\0\0\0\0".to_vec();
    let upload = |body: &[u8], query: &str| {
        let mut req = corvus_core::Request::new(corvus_core::Method::Post, &format!("/api/branding/logo{query}"));
        req.peer = Some("127.0.0.1".parse().unwrap());
        req.body = body.to_vec();
        let r = core.dispatch(req);
        (r.status, r.json_body().unwrap())
    };
    assert_eq!(upload(b"", ""), (400, json!({"ok": false, "error": "empty logo body"})));
    assert_eq!(
        upload(b"GIF89a", ""),
        (400, json!({"ok": false, "error": "logo must be a PNG image"}))
    );
    let (status, got) = upload(&png, "?name=C%3A%5Cdir%5Cmark.png");
    assert_eq!((status, &got["logo"]), (200, &json!("mark.png")));
    assert_eq!(std::fs::read(tmp.0.join("branding/logo.png")).unwrap(), png);
    assert_eq!(core.settings().get().block("branding").unwrap()["logo"], "mark.png");
    assert_eq!(upload(&png, "").1["logo"], "logo.png");
    let (status, _) = post(&core, "/api/branding/logo/remove", json!({}));
    assert_eq!(status, 200);
    assert!(!tmp.0.join("branding/logo.png").exists());
    assert!(core.settings().get().block("branding").is_none());
}
