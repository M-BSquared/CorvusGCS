//! The Firmware page run against a backend that is only a few dozen lines of
//! Python's behaviour: it answers the status and the catalogue, accepts a flash
//! request, and streams the progress the test queues. No board, no bootloader:
//! what is checked is what the page does with each answer, and that closing it
//! takes its event stream down.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;

struct Request {
    method: String,
    path: String,
    body: String,
}

struct Fake {
    server: Server,
    requests: Arc<Mutex<Vec<Request>>>,
    status: Arc<Mutex<Value>>,
    events: Arc<Mutex<Vec<String>>>,
    streams_open: Arc<Mutex<i32>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn catalog() -> Value {
    let board = |n: &str| {
        json!({"name": n, "key": n, "label": n, "title": "Kakute H7", "vendor": "Holybro",
            "variant": "default", "size": 1_500_000, "cached": false, "peripheral": false})
    };
    json!({"releases": [{"tag": "v1.17.0", "name": "v1.17.0", "prerelease": false, "vendor": "px4",
            "boards": [board("holybro_kakuteh7_default.px4")]}],
        "error": "", "dir": "/fw", "detected": {"px4": null, "ardupilot": null},
        "suggested": {"stack": "px4", "vehicle": ""}})
}

impl Fake {
    fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(Mutex::new(json!({
            "state": "idle", "transport": "usb", "device": "/dev/ttyACM0",
            "can_flash": true, "armed": false, "progress": 0, "message": ""})));
        let events = Arc::new(Mutex::new(Vec::new()));
        let streams_open = Arc::new(Mutex::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = (
            requests.clone(),
            status.clone(),
            events.clone(),
            streams_open.clone(),
            stop.clone(),
        );
        let thread = std::thread::spawn(move || {
            let (requests, status, events, streams_open, stop) = shared;
            let mut handlers = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        let (rq, st, ev, so, sp) = (
                            requests.clone(),
                            status.clone(),
                            events.clone(),
                            streams_open.clone(),
                            stop.clone(),
                        );
                        handlers.push(std::thread::spawn(move || handle(conn, rq, st, ev, so, sp)));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            for h in handlers {
                let _ = h.join();
            }
        });
        Fake {
            server,
            requests,
            status,
            events,
            streams_open,
            stop,
            thread: Some(thread),
        }
    }

    fn bodies(&self, method: &str, path: &str) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.path.split('?').next() == Some(path))
            .map(|r| r.body.clone())
            .collect()
    }

    fn set_status(&self, v: Value) {
        *self.status.lock().unwrap() = v;
    }

    fn send(&self, v: Value) {
        self.events.lock().unwrap().push(v.to_string());
    }

    fn streams(&self) -> i32 {
        *self.streams_open.lock().unwrap()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn handle(
    mut conn: std::net::TcpStream,
    requests: Arc<Mutex<Vec<Request>>>,
    status: Arc<Mutex<Value>>,
    events: Arc<Mutex<Vec<String>>>,
    streams_open: Arc<Mutex<i32>>,
    stop: Arc<AtomicBool>,
) {
    conn.set_nonblocking(false).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 2048];
    let (head_end, len) = loop {
        let n = conn.read(&mut buf).unwrap_or(0);
        if n == 0 {
            return;
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
                break (at + 4, len);
            }
        }
    };
    let head = String::from_utf8_lossy(&got[..head_end]).to_string();
    let mut first = head.lines().next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("").to_string();
    let body = String::from_utf8_lossy(&got[head_end..head_end + len]).to_string();
    requests.lock().unwrap().push(Request {
        method: method.clone(),
        path: path.clone(),
        body,
    });
    let reply = |conn: &mut std::net::TcpStream, v: &Value| {
        let text = v.to_string();
        let _ = write!(
            conn,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
    };
    match (method.as_str(), path.split('?').next().unwrap_or("")) {
        ("GET", "/api/firmware/status") => reply(&mut conn, &status.lock().unwrap().clone()),
        ("GET", "/api/firmware/catalog") => reply(&mut conn, &catalog()),
        ("POST", "/api/firmware/flash") => reply(&mut conn, &json!({"ok": true, "state": "downloading"})),
        ("POST", "/api/firmware/cancel") => reply(&mut conn, &json!({"ok": true})),
        ("GET", "/api/events") => {
            *streams_open.lock().unwrap() += 1;
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n"
            );
            while !stop.load(Ordering::SeqCst) {
                let queued: Vec<String> = std::mem::take(&mut *events.lock().unwrap());
                let mut alive = true;
                for e in queued {
                    alive &= write!(conn, "event: firmware\ndata: {e}\n\n").is_ok();
                }
                alive &= conn.write_all(b": keep\n\n").is_ok();
                if !alive {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            *streams_open.lock().unwrap() -= 1;
        }
        _ => {
            let _ = write!(conn, "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{{}}");
        }
    }
}

/// Runs the page's frame work until `done` holds.
fn pump(page: &mut FirmwarePage, ctx: &egui::Context, state: &Value, what: &str, done: impl Fn(&FirmwarePage) -> bool) {
    let t = Instant::now();
    while !done(page) {
        assert!(t.elapsed() < Duration::from_secs(8), "timed out waiting for {what}");
        page.take_answers(ctx, state);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait(what: &str, cond: impl Fn() -> bool) {
    let t = Instant::now();
    while !cond() {
        assert!(t.elapsed() < Duration::from_secs(8), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_download_and_flash_runs_from_the_request_to_the_notice_and_closes_its_stream() {
    let fake = Fake::start();
    let ctx = egui::Context::default();
    let state = json!({"connected": true, "armed": false});
    let mut page = FirmwarePage::new(fake.server.clone(), &ctx);

    pump(&mut page, &ctx, &state, "the status and the catalogue", |p| {
        p.status.is_some() && p.picker.catalog.is_some()
    });
    assert_eq!(page.picker.release, "v1.17.0");
    assert!(!page.can_start(&state), "no board is chosen yet");

    page.picker.pick_vendor("Holybro");
    page.picker.pick_board("holybro_kakuteh7_default.px4");
    assert!(page.can_start(&state));
    assert!(
        !page.can_start(&json!({"connected": true, "armed": true})),
        "the live armed flag closes the gate before a status refetch"
    );

    page.start_flash(&ctx);
    assert!(!page.can_start(&state), "a second start while one is going out");
    pump(&mut page, &ctx, &state, "the flash request to be answered", |p| {
        p.awaiting
    });
    assert_eq!(
        serde_json::from_str::<Value>(&fake.bodies("POST", "/api/firmware/flash")[0]).unwrap(),
        json!({"release": "v1.17.0", "board": "holybro_kakuteh7_default.px4"})
    );
    assert!(page.cancel_visible);
    assert!(
        !page.can_start(&state),
        "nothing starts a second flash while one is accepted"
    );
    assert_eq!(page.line, ("Downloading firmware\u{2026}".to_string(), false));
    wait("the stream to open", || fake.streams() == 1);

    fake.send(json!({"state": "flashing", "percent": 40, "message": "Programming"}));
    pump(&mut page, &ctx, &state, "the progress event", |p| p.bar.0 == 40);
    assert_eq!(page.bar.1, "40% \u{b7} Programming");
    assert_eq!(page.log.lines.last().unwrap().text, "Programming");
    assert!(page.cancel_visible);
    assert!(!page.awaiting);

    page.cancel(&ctx);
    pump(&mut page, &ctx, &state, "the cancel answer", |p| p.cancel_req.is_none());
    assert_eq!(fake.bodies("POST", "/api/firmware/cancel").len(), 1);

    fake.set_status(
        json!({"state": "done", "transport": "usb", "device": "/dev/ttyACM0", "can_flash": true,
        "armed": false, "progress": 100, "message": "Firmware flash complete"}),
    );
    fake.send(json!({"state": "done", "percent": 100, "message": "Firmware flash complete"}));
    pump(&mut page, &ctx, &state, "the terminal status", |p| {
        p.status.as_ref().is_some_and(|s| s.state == "done")
    });
    assert_eq!(page.take_notes(), ["Firmware flash complete"]);
    assert!(!page.cancel_visible);
    assert!(page.take_errors().is_empty());
    page.take_answers(&ctx, &state);
    assert!(page.take_notes().is_empty(), "a terminal state is announced once");
    wait("the catalogue to be read again after a finished download", || {
        fake.bodies("GET", "/api/firmware/catalog").len() >= 2
    });

    let t = Instant::now();
    drop(page);
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "closing the page waits for its stream"
    );
    wait("the stream to close", || fake.streams() == 0);
}

#[test]
fn a_refused_start_is_said_in_the_line_the_log_and_the_notice_board() {
    let fake = Fake::start();
    let ctx = egui::Context::default();
    let state = json!({"connected": true, "armed": false});
    let mut page = FirmwarePage::new(fake.server.clone(), &ctx);
    page.source = Source::File;
    page.file_path = "/no/such/fw.px4".into();
    page.catalog_asked = true;
    pump(&mut page, &ctx, &state, "the status", |p| p.status.is_some());
    page.start_flash(&ctx);
    pump(&mut page, &ctx, &state, "the upload to fail", |p| p.action.is_none());
    assert!(page.line.1);
    assert!(
        page.line.0.starts_with("Could not read /no/such/fw.px4"),
        "{}",
        page.line.0
    );
    assert_eq!(page.log.lines.last().unwrap().level, "error");
    assert_eq!(page.take_errors().len(), 1);
    assert!(page.can_start(&state), "the button is back once the refusal is shown");
}

#[test]
fn a_flash_going_on_elsewhere_is_followed_and_blocks_a_second() {
    let fake = Fake::start();
    let ctx = egui::Context::default();
    let state = json!({"connected": true, "armed": false});
    fake.set_status(
        json!({"state": "flashing", "transport": "usb", "can_flash": true, "armed": false,
        "progress": 12, "message": "Erasing"}),
    );
    let mut page = FirmwarePage::new(fake.server.clone(), &ctx);
    pump(&mut page, &ctx, &state, "the status", |p| p.status.is_some());
    assert!(page.cancel_visible);
    assert_eq!(page.bar, (12, "12% \u{b7} Erasing".to_string()));
    wait("the stream to open", || fake.streams() == 1);
    page.picker.load(Catalog::from_json(&catalog()));
    page.picker.pick_vendor("Holybro");
    page.picker.pick_board("holybro_kakuteh7_default.px4");
    assert!(!page.can_start(&state));
    assert!(page.status.as_ref().is_some_and(Status::busy));
}

#[test]
fn the_status_is_read_again_when_the_link_or_the_armed_flag_changes() {
    let fake = Fake::start();
    let ctx = egui::Context::default();
    let mut page = FirmwarePage::new(fake.server.clone(), &ctx);
    let a = json!({"connected": true, "armed": false});
    pump(&mut page, &ctx, &a, "the first status", |p| p.status.is_some());
    let reads = || fake.bodies("GET", "/api/firmware/status").len();
    let first = reads();
    for _ in 0..5 {
        page.take_answers(&ctx, &json!({"connected": true, "armed": false, "altitude": 3}));
    }
    assert_eq!(reads(), first, "telemetry frames alone read nothing");
    let armed = json!({"connected": true, "armed": true});
    pump(&mut page, &ctx, &armed, "a second read", |_| reads() > first);
    let waiting = json!({"connected": false, "link_bootloader": {"device": "/dev/ttyACM0"}});
    pump(&mut page, &ctx, &waiting, "a third read", |_| reads() > first + 1);
}
