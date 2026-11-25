use corvus_api::plugin::Plugin;
use eframe::egui::{self, pos2};
use serde_json::json;

use super::testlib::{load, Rig, AREA};

fn probe() -> Rig {
    let mut rig = Rig::new(load("corvus_plugin_probe").1);
    rig.with(|p, h| p.start(h));
    rig
}

/// What the plugin was stopped for.
fn stopped(rig: &mut Rig, what: &str) -> String {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rig.tell(what, "")));
    let payload = caught.expect_err("the plugin should have been stopped");
    payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_else(|| "not a text".into())
}

fn field() -> egui::Pos2 {
    pos2(AREA.min.x + 20.0, AREA.min.y + 15.0)
}

fn dropdown() -> egui::Pos2 {
    pos2(AREA.min.x + 100.0, AREA.min.y + 55.0)
}

#[test]
fn a_module_says_who_it_is_and_what_it_needs_without_running() {
    let (manifest, plugin) = load("corvus_plugin_probe");
    assert_eq!(manifest.id, "probe");
    assert_eq!(manifest.min_corvus, "2026.11.94");
    assert!(manifest.fits("2026.11.94") && manifest.fits("2031.01.01"));
    assert!(!manifest.fits("2026.11.93"));
    assert_eq!(plugin.info().name, "Probe");
}

#[test]
fn text_typed_into_a_field_arrives_and_the_flight_keys_are_quiet_meanwhile() {
    let mut rig = probe();
    rig.frames(2);
    assert!(rig.height > 0.0);
    assert!(!rig.wants_keyboard);
    // Typed while nothing of the plugin was clicked: none of its business.
    rig.frame(vec![egui::Event::Text("lost".into())]);
    assert_eq!(rig.published("typed"), None);
    rig.click(field());
    assert!(
        rig.wants_keyboard,
        "while a field of a plugin has the keyboard, the program must see that a text is being typed"
    );
    rig.frame(vec![egui::Event::Text("w a s d \u{e4}".into())]);
    rig.frames(2);
    assert_eq!(rig.published("typed"), Some(json!("w a s d \u{e4}")));
    assert!(rig.wants_keyboard);
    // A click beside the plugin gives the keyboard back.
    rig.click(pos2(50.0, 50.0));
    assert!(!rig.wants_keyboard);
    rig.frame(vec![egui::Event::Text("more".into())]);
    rig.frames(2);
    assert_eq!(rig.published("typed"), Some(json!("w a s d \u{e4}")));
}

#[test]
fn a_dropdown_of_a_plugin_opens_over_the_window_and_can_be_picked_from() {
    let mut rig = probe();
    rig.frames(2);
    rig.click(dropdown());
    rig.frames(20);
    // The list lies under the box, over the window. Its second row is its lower half, whatever
    // height a row has (another test may switch tablet mode on meanwhile).
    let list = rig
        .plugin
        .popups()
        .into_iter()
        .find(|r| r.top() > dropdown().y)
        .expect("the list is open");
    rig.click(pos2(list.center().x, list.top() + list.height() * 0.72));
    assert_eq!(rig.published("picked"), Some(json!("b")));
    rig.frames(20);
    assert!(rig.plugin.popups().iter().all(|r| r.top() <= dropdown().y), "it closed");
}

#[test]
fn only_what_a_permission_covers_reaches_the_backend() {
    let mut rig = probe();
    rig.tell("get", "/api/config");
    assert_eq!(rig.await_published("get"), json!({"ok": {"ok": true}}));
    for path in [
        "/api/local/run",
        "/api/plugins/settings",
        "/api/config",
        "/api/ssh/../local/run",
    ] {
        rig.bus.retract_all("probe");
        rig.tell("post", path);
        let got = rig.await_published("post");
        assert!(got["err"].as_str().unwrap().contains("not allowed"), "{path}: {got}");
    }
    assert_eq!(rig.backend.seen(), ["GET /api/config"]);
    rig.tell("events", "shell");
    rig.frames(5);
    assert_eq!(rig.backend.seen(), ["GET /api/config"], "no stream was opened");
}

#[test]
fn a_module_has_no_disk_no_network_and_no_environment_of_its_own() {
    let secret = std::env::temp_dir().join(format!("corvus-secret-{}", std::process::id()));
    std::fs::write(&secret, b"secret").unwrap();
    let mut rig = probe();
    rig.tell("peek", &secret.to_string_lossy());
    assert_eq!(rig.await_published("peek"), json!({"picked": false, "direct": false}));
    let port = rig.backend.server.port;
    rig.tell("net", &format!("127.0.0.1:{port}"));
    assert_eq!(rig.await_published("net"), json!({"connected": false, "env": []}));
    std::fs::remove_file(secret).unwrap();
}

#[test]
fn a_plugin_reads_the_file_the_operator_picked() {
    let file = std::env::temp_dir().join(format!("corvus-picked-{}", std::process::id()));
    std::fs::write(&file, b"waypoints").unwrap();
    let mut rig = probe();
    let chosen = file.clone();
    rig.plugin.set_picker(Box::new(move |_| Some(chosen.clone())));
    rig.tell("pick", "");
    assert_eq!(rig.await_published("pick"), json!({"ok": "waypoints"}));
    std::fs::remove_file(file).unwrap();
}

#[test]
fn what_the_plugin_may_look_at_follows_the_program() {
    let mut rig = probe();
    rig.state = json!({"armed": true});
    rig.saved = json!({"kept": 1, "_options": {"x": 2}});
    rig.tell("look", "");
    let look = rig.await_published("look");
    assert_eq!(look["state"], json!({"armed": true}));
    assert_eq!(look["settings"], json!({"kept": 1}), "the options are the operator's");
    assert_eq!(look["bg"], json!(crate::ui::style::palette().bg));
    assert_eq!(look["offset"], json!(corvus_api::data::clock::local_offset()));
    assert_eq!(look["none"], json!("Text nobody translated"));
    rig.state = json!({"armed": false});
    rig.bus.retract_all("probe");
    rig.tell("look", "");
    assert_eq!(rig.await_published("look")["state"], json!({"armed": false}));
}

#[test]
fn what_a_plugin_saves_draws_says_and_sends_is_done_for_it() {
    let mut rig = probe();
    rig.saved = json!({"_options": {"x": 2}});
    rig.tell("save", "value");
    assert_eq!(rig.saved, json!({"kept": "value", "_options": {"x": 2}}));
    rig.await_published("saved");
    assert_eq!(rig.backend.seen(), ["POST /api/plugins/settings"]);
    rig.tell("draw", "");
    assert_eq!(rig.shapes.shapes().len(), 2);
    rig.tell("notify", "hello");
    assert_eq!(rig.out.notices, [("Probe".to_string(), "warning", "hello".to_string())]);
    rig.tell("send", "other");
    assert_eq!(rig.await_published("sent"), json!(true));
    assert!(rig.bus.has_mail("other"));
}

#[test]
fn a_plugin_offers_its_tools_and_answers_a_call() {
    let mut rig = probe();
    let tools = rig.with(|p, h| p.mcp_tools(h));
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    rig.with(|p, h| p.mcp_call("echo", &json!({}), 7, h));
    assert_eq!(rig.plugin.mcp_take(7), Some(Ok(json!({"echo": true}))));
    assert_eq!(rig.plugin.mcp_take(8), None);
}

#[test]
fn a_panic_ends_the_plugin_with_its_words() {
    let mut rig = probe();
    let why = stopped(&mut rig, "panic");
    assert!(why.contains("the probe was told to"), "{why}");
    let again = stopped(&mut rig, "look");
    assert!(
        again.contains("stopped"),
        "a stopped module is not called again: {again}"
    );
}

#[test]
fn a_loop_is_cut_off() {
    let mut rig = probe();
    let started = std::time::Instant::now();
    let why = stopped(&mut rig, "loop");
    assert!(why.contains("took too long"), "{why}");
    assert!(started.elapsed().as_secs() < 30);
}

#[test]
fn a_plugin_that_eats_memory_or_stack_is_stopped_and_the_program_goes_on() {
    for what in ["memory", "stack"] {
        let mut rig = probe();
        let why = stopped(&mut rig, what);
        assert!(why.starts_with("the plugin"), "{what}: {why}");
    }
    probe().frames(1);
}

/// The test plugin as built with the first version of the protocol (`fixtures/README.md`).
/// It is what an old, published plugin is to a later program.
#[test]
fn a_plugin_built_with_the_first_protocol_still_works() {
    use std::io::Read;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!("fixtures/protocol-1.wasm.gz")[..])
        .read_to_end(&mut bytes)
        .expect("the fixture unpacks");
    let manifest = corvus_package::Manifest::of_module(&bytes).expect("its manifest");
    assert!(manifest.fits(corvus_core::version::version()));
    let plugin = super::WasmPlugin::load(&super::testlib::sandbox(), &manifest, &bytes, None).expect("it loads");
    let mut rig = Rig::new(plugin);
    rig.with(|p, h| p.start(h));
    rig.state = json!({"armed": true});
    rig.saved = json!({"kept": 1});
    rig.tell("look", "");
    let look = rig.await_published("look");
    assert_eq!(
        (look["state"].clone(), look["settings"].clone()),
        (rig.state.clone(), rig.saved.clone())
    );
    assert_eq!(look["bg"], json!(crate::ui::style::palette().bg));
    rig.frames(2);
    assert!(rig.height > 0.0);
    rig.click(field());
    assert!(rig.wants_keyboard);
    rig.frame(vec![egui::Event::Text("old".into())]);
    rig.frames(2);
    assert_eq!(rig.published("typed"), Some(json!("old")));
    rig.tell("get", "/api/config");
    assert_eq!(rig.await_published("get"), json!({"ok": {"ok": true}}));
    rig.tell("post", "/api/local/run");
    assert!(rig.await_published("post")["err"].as_str().is_some());
    rig.tell("draw", "");
    assert_eq!(rig.shapes.shapes().len(), 2);
    assert_eq!(rig.with(|p, h| p.mcp_tools(h)).len(), 1);
}
