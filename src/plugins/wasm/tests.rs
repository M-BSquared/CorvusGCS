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

/// A module follows Unix rules; the paths it is handed on Windows do not.
#[test]
fn a_plugin_names_a_file_of_windows_by_its_name_alone() {
    let mut rig = probe();
    rig.tell("name", r"C:\Users\pilot\AppData\Local\Temp\path.csv");
    assert_eq!(rig.await_published("name"), json!("path.csv"));
}

#[test]
fn a_plugin_reads_the_file_the_operator_picked() {
    let file = std::env::temp_dir().join(format!("corvus-picked-{}", std::process::id()));
    std::fs::write(&file, b"waypoints").unwrap();
    let mut rig = probe();
    let chosen = file.clone();
    let asked = std::sync::Arc::new(std::sync::Mutex::new(None));
    let into = asked.clone();
    rig.plugin.set_picker(Box::new(move |ask| {
        *into.lock().unwrap() = ask.start.clone();
        Some(chosen.clone())
    }));
    rig.tell("pick", "~/.ssh/id_ed25519");
    assert_eq!(rig.await_published("pick"), json!({"ok": "waypoints"}));
    assert_eq!(
        asked.lock().unwrap().as_deref(),
        Some("~/.ssh/id_ed25519"),
        "the value is handed on"
    );
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
    // A file to show, a double click on the map, and a folder the program reads.
    rig.tell("media", "/home/pilot/a.jpg");
    let shown = ("/home/pilot/a.jpg".to_string(), "From the probe".to_string());
    assert_eq!(rig.out.files, [shown]);
    rig.with(|p, h| p.map_event(corvus_api::plugin::MapEvent::DoubleClick("m".into()), h));
    assert_eq!(rig.await_published("event"), json!("DoubleClick(\"m\")"));
    let card = std::env::temp_dir().join(format!("corvus-frozen-card-{}", std::process::id()));
    std::fs::create_dir_all(&card).unwrap();
    let block = crate::media::exif::tests::block(
        true,
        ("N", [48, 4, 35040]),
        ("E", [11, 38, 34800]),
        5205,
        "2026:11:25 14:02:11",
    );
    let photo = card.join("a.jpg");
    std::fs::write(&photo, crate::media::exif::tests::jpeg(&block)).unwrap();
    let picked = card.clone();
    rig.plugin.set_picker(Box::new(move |_| Some(picked.clone())));
    rig.tell("scan", "");
    let got = rig.await_published("scanned");
    assert_eq!((&got["tag"], &got["found"]["files"]), (&json!(7), &json!(1)), "{got}");
    assert_eq!(got["found"]["first"][0], json!(photo.to_string_lossy()), "{got}");
    assert_eq!(got["found"]["first"][3], "2026-11-25 14:02:11", "{got}");
    std::fs::remove_dir_all(card).unwrap();
}

/// Every delay the program's window was asked to draw again after.
fn frames_asked_for(rig: &Rig) -> std::sync::Arc<std::sync::Mutex<Vec<std::time::Duration>>> {
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let into = asked.clone();
    rig.ctx
        .set_repaint_observer(move |info| into.lock().unwrap().push(info.delay));
    asked
}

#[test]
fn a_timer_a_plugin_sets_while_it_is_not_shown_wakes_the_window() {
    let mut rig = probe();
    rig.tick();
    let asked = frames_asked_for(&rig);
    let later = std::time::Duration::from_millis(4321);
    rig.tell("wake", "4321");
    assert!(asked.lock().unwrap().contains(&later), "{:?}", asked.lock().unwrap());
    // And again and again from its ticks, though no frame of its own is ever drawn.
    rig.tell("every", "1234");
    for _ in 0..3 {
        asked.lock().unwrap().clear();
        rig.tick();
        assert!(asked.lock().unwrap().contains(&std::time::Duration::from_millis(1234)));
    }
    // A longer wait than an earlier one is passed on as well.
    rig.tell("every", "60000");
    asked.lock().unwrap().clear();
    rig.tick();
    assert!(asked.lock().unwrap().contains(&std::time::Duration::from_secs(60)));
}

#[test]
fn a_plugin_that_puts_the_cursor_into_its_field_gets_the_keys_until_a_press_elsewhere() {
    let mut rig = probe();
    rig.with(|p, h| p.open(h));
    rig.frames(2);
    // As a console does once its shell is up: nothing of it was pressed.
    rig.tell("focus", "");
    rig.frames(3);
    assert!(rig.wants_keyboard);
    rig.frame(vec![egui::Event::Text("ls".into())]);
    rig.frames(2);
    assert_eq!(rig.published("typed"), Some(json!("ls")));
    assert_eq!(rig.published("focused"), Some(json!(true)));

    // A press elsewhere reaches the field too: it lets go, and may take what it holds.
    rig.click(pos2(50.0, 50.0));
    assert_eq!(rig.published("focused"), Some(json!(false)));
    assert!(!rig.wants_keyboard);

    // From then on it cannot take the keys by itself: they fly the vehicle.
    rig.tell("focus", "");
    rig.frames(4);
    assert!(!rig.wants_keyboard, "the operator's last press was elsewhere");
    rig.frame(vec![egui::Event::Text("w".into())]);
    rig.frames(2);
    assert_eq!(rig.published("typed"), Some(json!("ls")));

    // Closed and opened again, it is the operator's wish once more.
    rig.with(|p, h| p.close(h));
    rig.with(|p, h| p.open(h));
    rig.tell("focus", "");
    rig.frames(4);
    assert!(rig.wants_keyboard);
}

#[test]
fn a_terminal_window_opens_for_a_plugin_with_either_kind_of_session() {
    use corvus_package::Manifest;
    let bytes = super::testlib::module("corvus_plugin_probe");
    let base = Manifest::of_module(&bytes).unwrap();
    for (permission, opens) in [("ssh", true), ("local", true), ("shell", false)] {
        let mut manifest = base.clone();
        manifest.permissions = vec![permission.to_string()];
        let plugin = super::WasmPlugin::load(&super::testlib::sandbox(), &manifest, &bytes, None).unwrap();
        let mut rig = Rig::new(plugin);
        rig.tell("terminal", "this computer");
        assert_eq!(rig.out.terminals.len(), usize::from(opens), "{permission}");
        if opens {
            assert_eq!(rig.out.terminals[0].0["name"], json!("s1"));
        }
    }
}

#[test]
fn only_a_plugin_with_files_has_one_shown_in_a_card() {
    use corvus_package::Manifest;
    let bytes = super::testlib::module("corvus_plugin_probe");
    let base = Manifest::of_module(&bytes).unwrap();
    for (permission, shown) in [("files", true), ("settings.read", false)] {
        let mut manifest = base.clone();
        manifest.permissions = vec![permission.to_string()];
        let plugin = super::WasmPlugin::load(&super::testlib::sandbox(), &manifest, &bytes, None).unwrap();
        let mut rig = Rig::new(plugin);
        rig.tell("media", "/home/pilot/a.jpg");
        let want = shown.then(|| ("/home/pilot/a.jpg".to_string(), "From the probe".to_string()));
        assert_eq!(rig.out.files, Vec::from_iter(want), "{permission}");
    }
}

#[test]
fn a_folder_is_read_for_a_plugin_only_when_the_operator_picks_it_and_it_may_not_read_in_it() {
    use corvus_package::Manifest;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let card = std::env::temp_dir().join(format!("corvus-probe-card-{}", std::process::id()));
    std::fs::create_dir_all(&card).unwrap();
    let block = crate::media::exif::tests::block(
        true,
        ("N", [48, 4, 35040]),
        ("E", [11, 38, 34800]),
        5205,
        "2026:11:25 14:02:11",
    );
    let photo = card.join("a.jpg");
    std::fs::write(&photo, crate::media::exif::tests::jpeg(&block)).unwrap();
    std::fs::write(card.join("none.png"), b"x").unwrap();
    let bytes = super::testlib::module("corvus_plugin_probe");
    let base = Manifest::of_module(&bytes).unwrap();
    let rig_with = |permission: &str, answer: Option<std::path::PathBuf>| {
        let mut manifest = base.clone();
        manifest.permissions = vec![permission.to_string()];
        let mut plugin = super::WasmPlugin::load(&super::testlib::sandbox(), &manifest, &bytes, None).unwrap();
        let asked = Arc::new(AtomicUsize::new(0));
        let count = asked.clone();
        plugin.set_picker(Box::new(move |ask| {
            assert_eq!(ask.mode, "folder");
            count.fetch_add(1, Ordering::SeqCst);
            answer.clone()
        }));
        (Rig::new(plugin), asked)
    };

    // Without the permission no dialog opens at all.
    let (mut rig, asked) = rig_with("settings.read", Some(card.clone()));
    rig.tell("scan", "");
    assert_eq!(rig.await_published("scanned"), json!({"tag": 7, "found": null}));
    assert_eq!(asked.load(Ordering::SeqCst), 0);

    // The operator gives the dialog up.
    let (mut rig, asked) = rig_with("files", None);
    rig.tell("scan", "");
    assert_eq!(rig.await_published("scanned"), json!({"tag": 7, "found": null}));
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    let (mut rig, _) = rig_with("files", Some(card.clone()));
    rig.tell("scan", "");
    let got = rig.await_published("scanned");
    assert_eq!(got["found"]["files"], 1, "{got}");
    assert_eq!(got["found"]["without"], 1, "{got}");
    assert_eq!(got["found"]["first"][0], json!(photo.to_string_lossy()), "{got}");
    assert_eq!(got["found"]["first"][3], "2026-11-25 14:02:11", "{got}");
    // What the program read for it is still not the plugin's to read.
    rig.tell("peek", &photo.to_string_lossy());
    assert_eq!(rig.await_published("peek"), json!({"picked": false, "direct": false}));
    std::fs::remove_dir_all(card).unwrap();
}
