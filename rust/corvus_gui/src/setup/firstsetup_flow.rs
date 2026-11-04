//! First Setup run against a stand-in backend that answers `GET
//! /api/first-setup` and `POST /api/first-setup/airframe` with what a test says
//! (and `{}` for everything else, which is all the hosted pages ask of it): the
//! step it opens on, what writing an aircraft type sends, what a refusal does,
//! what Finish remembers, and how a reboot is followed.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::setup::firstsetup_model::MEMORY_TEST;

struct Fake {
    server: Server,
    routes: Arc<Mutex<HashMap<String, (u16, String)>>>,
    seen: Arc<Mutex<Vec<(String, String, String)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fake {
    fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        let routes: Arc<Mutex<HashMap<String, (u16, String)>>> = Arc::new(Mutex::new(HashMap::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s, st) = (routes.clone(), seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !st.load(AtomicOrdering::SeqCst) {
                let Ok((mut conn, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                conn.set_nonblocking(false).unwrap();
                conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut got = Vec::new();
                let mut buf = [0u8; 2048];
                loop {
                    let n = conn.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                    if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&got[..at]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if got.len() >= at + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&got).to_string();
                let mut first = text.lines().next().unwrap_or("").split(' ');
                let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                let key = path.split('?').next().unwrap_or("").to_string();
                s.lock().unwrap().push((method.to_string(), key.clone(), body));
                let (code, answer) = r.lock().unwrap().get(&key).cloned().unwrap_or((200, "{}".to_string()));
                let _ = write!(
                    conn,
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
            }
        });
        Fake {
            server,
            routes,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn answer(&self, path: &str, code: u16, body: Value) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.into(), (code, body.to_string()));
    }

    fn count(&self, method: &str, path: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == method && p == path)
            .count()
    }

    fn bodies(&self, method: &str, path: &str) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == method && p == path)
            .map(|(_, _, b)| b.clone())
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, AtomicOrdering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn payload(extra: Value) -> Value {
    let mut base = json!({"connected": true, "stack": "px4", "vehicle": "Copter", "transport": "usb",
        "facts": {"airframe": "todo", "accel": "todo", "gyro": "done", "mag": "done"},
        "reasons": ["No aircraft type is set."], "needs_setup": true, "prompt": true,
        "presets": [{"id": 4001, "group": "Multirotor", "label": "Quadcopter X"},
                    {"id": 5001, "group": "Multirotor", "label": "Quadcopter +"}],
        "preset_current": null, "fingerprint": "px4|1|2|abc"});
    base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    base
}

fn pump(
    page: &mut FirstSetupPage,
    ctx: &egui::Context,
    state: &Value,
    now: f64,
    what: &str,
    done: impl Fn(&FirstSetupPage) -> bool,
) {
    let t = Instant::now();
    while !done(page) {
        assert!(t.elapsed() < Duration::from_secs(8), "timed out waiting for {what}");
        page.take_answers(ctx, state, now);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn open(fake: &Fake, step: Option<&str>) -> (FirstSetupPage, egui::Context) {
    let ctx = egui::Context::default();
    let env = Env::new(Value::Null);
    let mut page = FirstSetupPage::new(fake.server.clone(), env, &ctx, step);
    page.quiet_path = None;
    (page, ctx)
}

fn up() -> Value {
    json!({"connected": true, "armed": false})
}

#[test]
fn a_fresh_controller_opens_on_the_step_that_needs_work_and_the_page_counts_as_open() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    model::memory().forget();
    let fake = Fake::start();
    fake.answer("/api/first-setup", 200, payload(json!({})));
    let before = OPEN.load(Ordering::SeqCst);
    let (mut page, ctx) = open(&fake, None);
    assert!(page.looking, "it looks at the controller before it picks");
    assert_eq!(OPEN.load(Ordering::SeqCst), before + 1);
    pump(&mut page, &ctx, &up(), 0.0, "the first look", |p| !p.looking);
    assert_eq!(page.current, "aircraft");
    assert_eq!(model::memory().current, "aircraft");
    drop(page);
    assert_eq!(OPEN.load(Ordering::SeqCst), before);
    model::memory().forget();
}

#[test]
fn a_controller_that_does_not_answer_opens_on_the_firmware_step() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    model::memory().forget();
    let fake = Fake::start();
    fake.answer(
        "/api/first-setup",
        200,
        json!({"connected": false, "facts": {}, "presets": []}),
    );
    let (mut page, ctx) = open(&fake, None);
    pump(&mut page, &ctx, &up(), 0.0, "the first look", |p| !p.looking);
    assert_eq!(page.current, "firmware");
    assert!(
        matches!(page.hosted, Some(Hosted::Firmware(_))),
        "the firmware page is the step's"
    );
    model::memory().forget();
}

#[test]
fn a_named_step_and_the_sessions_memory_open_without_looking_first() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    model::memory().forget();
    let fake = Fake::start();
    let (page, _ctx) = open(&fake, Some("battery"));
    assert!(!page.looking);
    assert_eq!(page.current, "battery");
    assert!(matches!(page.hosted, Some(Hosted::Battery(_))));
    drop(page);
    let (page, _ctx) = open(&fake, None);
    assert_eq!(page.current, "battery", "left where the session was");
    drop(page);
    model::memory().forget();
}

#[test]
fn next_marks_the_step_done_and_skip_does_not() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    model::memory().forget();
    let fake = Fake::start();
    fake.answer("/api/first-setup", 200, payload(json!({})));
    let (mut page, ctx) = open(&fake, Some("aircraft"));
    page.go(1, true, &ctx);
    assert_eq!(page.current, "frame");
    assert!(model::memory().visited.contains("aircraft"));
    page.go(1, false, &ctx);
    assert_eq!(page.current, "calibration");
    assert!(!model::memory().visited.contains("frame"));
    page.go(-1, false, &ctx);
    assert_eq!(page.current, "frame");
    page.show_step("finish", &ctx);
    page.go(1, true, &ctx);
    assert_eq!(page.current, "finish", "there is no step after the last");
    pump(&mut page, &ctx, &up(), 0.0, "a read", |_| {
        fake.count("GET", "/api/first-setup") >= 1
    });
    model::memory().forget();
}

#[test]
fn choosing_an_aircraft_writes_the_preset_and_says_to_reboot() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    fake.answer("/api/first-setup", 200, payload(json!({})));
    fake.answer(
        "/api/first-setup/airframe",
        200,
        json!({"ok": true, "written": ["SYS_AUTOSTART"], "reboot": true}),
    );
    let (mut page, ctx) = open(&fake, Some("aircraft"));
    pump(&mut page, &ctx, &up(), 0.0, "the payload", |p| p.payload.is_some());
    page.chosen = Some(5001);
    page.write(&ctx);
    pump(&mut page, &ctx, &up(), 0.0, "the write", |p| p.write_req.is_none());
    assert_eq!(
        serde_json::from_str::<Value>(&fake.bodies("POST", "/api/first-setup/airframe")[0]).unwrap(),
        json!({"preset": 5001})
    );
    assert!(page.written);
    assert_eq!(
        page.take_notes(),
        ["Aircraft type written. Reboot the controller to apply it."]
    );
    let view = model::aircraft_view(page.payload.as_ref(), false, page.written, false, page.chosen);
    assert!(view.show_reboot && !view.apply_enabled);
    model::memory().forget();
}

#[test]
fn a_refused_write_says_why_and_leaves_the_choice_open() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    fake.answer("/api/first-setup", 200, payload(json!({})));
    fake.answer(
        "/api/first-setup/airframe",
        409,
        json!({"ok": false, "error": "disarm before choosing an airframe"}),
    );
    let (mut page, ctx) = open(&fake, Some("aircraft"));
    page.chosen = Some(4001);
    page.write(&ctx);
    pump(&mut page, &ctx, &up(), 0.0, "the refusal", |p| p.write_req.is_none());
    assert!(!page.written);
    assert_eq!(
        page.take_errors(),
        ["Could not set the aircraft type: disarm before choosing an airframe"]
    );
    model::memory().forget();
}

#[test]
fn a_warning_from_the_backend_goes_to_the_warnings() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    fake.answer(
        "/api/first-setup/airframe",
        200,
        json!({"ok": true, "warning": "SYS_AUTOCONFIG not written"}),
    );
    let (mut page, ctx) = open(&fake, Some("aircraft"));
    page.chosen = Some(4001);
    page.write(&ctx);
    pump(&mut page, &ctx, &up(), 0.0, "the write", |p| p.write_req.is_none());
    assert_eq!(
        page.take_warnings(),
        ["The aircraft type is set, but: SYS_AUTOCONFIG not written"]
    );
    assert!(page.written);
    model::memory().forget();
}

#[test]
fn the_reboot_is_followed_down_and_up_and_the_controller_read_again() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    fake.answer("/api/first-setup", 200, payload(json!({})));
    fake.answer("/api/mavlink/reboot", 200, json!({"ok": true}));
    let (mut page, ctx) = open(&fake, Some("aircraft"));
    pump(&mut page, &ctx, &up(), 0.0, "the payload", |p| p.payload.is_some());
    page.written = true;
    page.start_reboot(&ctx);
    pump(&mut page, &ctx, &up(), 0.0, "the reboot", |p| p.reboot_req.is_none());
    assert!(page.reboot.active());
    assert_eq!(
        page.take_notes(),
        ["The autopilot is rebooting. The link returns in a few seconds."]
    );
    let reads = fake.count("GET", "/api/first-setup");
    page.take_answers(&ctx, &json!({"connected": false}), 1000.0);
    page.take_answers(&ctx, &json!({"connected": true}), 5000.0);
    page.take_answers(&ctx, &json!({"connected": true}), 7000.0);
    assert_eq!(
        fake.count("GET", "/api/first-setup"),
        reads,
        "not before the pause is over"
    );
    assert!(page.written);
    pump(
        &mut page,
        &ctx,
        &json!({"connected": true}),
        7600.0,
        "the new read",
        |_| fake.count("GET", "/api/first-setup") > reads,
    );
    assert!(!page.written && !page.reboot.active());
    model::memory().forget();
}

#[test]
fn finishing_forgets_the_session_silences_this_controller_and_leaves_the_page() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    fake.answer("/api/first-setup", 200, payload(json!({})));
    let dir = std::env::temp_dir().join(format!("corvus-fs-finish-{}", std::process::id()));
    let path = dir.join("quiet.json");
    let (mut page, ctx) = open(&fake, Some("finish"));
    page.quiet_path = Some(path.clone());
    pump(&mut page, &ctx, &up(), 0.0, "the payload", |p| p.payload.is_some());
    model::memory().visited.insert("firmware");
    page.finish_setup();
    assert!(page.finished());
    assert!(model::memory().visited.is_empty() && model::memory().current.is_empty());
    assert_eq!(page.take_notes(), ["First setup finished."]);
    assert!(model::Quiet::load(Some(path.clone())).is_silenced("px4|1|2|abc"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn leaving_the_page_closes_the_hosted_page_and_its_streams() {
    let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    let (mut page, ctx) = open(&fake, Some("firmware"));
    assert!(matches!(page.hosted, Some(Hosted::Firmware(_))));
    page.show_step("finish", &ctx);
    assert!(page.hosted.is_none(), "a step with no page drops the one it had");
    let t = Instant::now();
    drop(page);
    assert!(t.elapsed() < Duration::from_secs(2));
    model::memory().forget();
}
