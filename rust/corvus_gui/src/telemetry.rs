//! The vehicle state, pushed by the server over `/api/telemetry`.
//!
//! One reader thread holds the stream and keeps the latest state; the
//! interface reads it each frame and is asked to repaint when a new one
//! lands. A dropped stream marks the state disconnected at once (stale
//! instruments that still look live are the worst failure a ground station
//! has) and is reopened after a second, for as long as the app runs.

use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use crate::http::{self, Server};
use crate::sse;

/// The state before the first frame, as the web interface starts it.
/// `-1` battery figures mean nobody has said; `prearm_ok: null` means the
/// firmware does not report a verdict.
const INITIAL_STATE: &str = r#"{
  "connected": false, "armed": false, "prearm_ok": null, "prearm_reasons": [],
  "vehicle_type": "", "autopilot": "", "mode": "", "px4_version": "",
  "position": [0, 0], "altitude_amsl": 0, "altitude_agl": 0, "heading": 0,
  "groundspeed": 0, "airspeed": 0, "vspeed": 0, "pitch": 0, "roll": 0, "yaw": 0,
  "battery_percent": 0, "battery_voltage": 0, "battery_current": 0,
  "battery_percent_fc": -1, "battery_percent_est": -1, "battery_source": "autopilot",
  "battery_cells": 0, "battery_cell_voltage": 0, "battery_cell_voltages": [],
  "battery_consumed_mah": 0, "battery_temperature": null, "battery_time_remaining": 0,
  "gps_fix": "", "gps_satellites": 0, "gps_hdop": 99,
  "rtk_state": "", "rtk_source": "", "rtk_accuracy": 0, "rtk_progress": 0,
  "uplink": 0, "time": "", "warnings": [], "home": [0, 0], "mission": []
}"#;

/// How long one read may block before the thread looks at its stop flag.
const POLL: Duration = Duration::from_millis(250);
/// The wait before a dropped stream is opened again.
const RECONNECT: Duration = Duration::from_secs(1);

/// The fresh state a new connection starts from.
pub fn initial_state() -> Value {
    serde_json::from_str(INITIAL_STATE).expect("the initial state is valid JSON")
}

#[derive(Debug)]
struct Shared {
    state: Value,
    stream_up: bool,
    /// Said once per drop while a vehicle was linked, as telemetry.js
    /// publishTransportError does; reset by the next good frame.
    error_reported: bool,
    errors: Vec<String>,
}

/// The reader thread and what it has seen.
pub struct Telemetry {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Telemetry {
    /// Starts reading from `server`; `changed` is called after every update.
    pub fn start(server: Server, changed: impl Fn() + Send + 'static) -> Telemetry {
        let shared = Arc::new(Mutex::new(Shared {
            state: initial_state(),
            stream_up: false,
            error_reported: false,
            errors: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("telemetry".into())
                .spawn(move || run(&server, &shared, &stop, &changed))
                .expect("the telemetry thread starts")
        };
        Telemetry {
            shared,
            stop,
            thread: Some(thread),
        }
    }

    /// The latest state.
    pub fn state(&self) -> Value {
        self.shared
            .lock()
            .map(|s| s.state.clone())
            .unwrap_or_else(|_| initial_state())
    }

    /// Takes the transport errors raised since the last call.
    pub fn take_errors(&self) -> Vec<String> {
        self.shared
            .lock()
            .map(|mut s| std::mem::take(&mut s.errors))
            .unwrap_or_default()
    }

    /// Whether the stream itself is open (the vehicle may still be absent).
    #[cfg(test)]
    pub fn stream_up(&self) -> bool {
        self.shared.lock().map(|s| s.stream_up).unwrap_or(false)
    }

    /// Stops the thread and waits for it; at most one poll interval.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(server: &Server, shared: &Mutex<Shared>, stop: &AtomicBool, changed: &dyn Fn()) {
    while !stop.load(Ordering::SeqCst) {
        let _ = read_stream(server, shared, stop, changed);
        let was_connected = {
            let mut s = shared.lock().expect("telemetry lock");
            s.stream_up = false;
            let linked = s.state.get("connected") == Some(&Value::Bool(true));
            let was = s.state.get("connected") != Some(&Value::Bool(false));
            if was {
                s.state["connected"] = Value::Bool(false);
            }
            // With no vehicle on the link a dropped stream loses nothing.
            if linked && !s.error_reported && !stop.load(Ordering::SeqCst) {
                s.error_reported = true;
                s.errors.push("Telemetry connection interrupted; reconnecting".into());
            }
            was
        };
        if was_connected {
            changed();
        }
        sleep_unless_stopped(RECONNECT, stop);
    }
}

fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) {
    let mut waited = Duration::ZERO;
    while waited < total && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(POLL.min(total - waited));
        waited += POLL;
    }
}

fn read_stream(server: &Server, shared: &Mutex<Shared>, stop: &AtomicBool, changed: &dyn Fn()) -> io::Result<()> {
    let mut response = http::request(server, "GET", "/api/telemetry", None, "text/event-stream", POLL)?;
    let is_stream = response
        .header("content-type")
        .is_some_and(|t| t.starts_with("text/event-stream"));
    if response.status != 200 || !is_stream {
        return Err(io::Error::other(format!(
            "telemetry stream answered {}",
            response.status
        )));
    }
    shared.lock().expect("telemetry lock").stream_up = true;
    let mut parser = sse::Parser::default();
    let mut line = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        match response.body.read_until(b'\n', &mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
        if line.last() != Some(&b'\n') {
            continue;
        }
        line.pop();
        let text = String::from_utf8_lossy(&line).into_owned();
        line.clear();
        let Some(event) = parser.line(&text) else { continue };
        if event.name != "state" {
            continue;
        }
        // A frame that does not parse is dropped; the next one replaces it.
        if let Ok(next) = serde_json::from_str::<Value>(&event.data) {
            let mut s = shared.lock().expect("telemetry lock");
            s.state = next;
            s.error_reported = false;
            drop(s);
            changed();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    /// A server that sends one state and then holds the stream open.
    fn one_frame_server() -> (Server, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body = "event: state\ndata: {\"connected\": true, \"mode\": \"HOLD\"}\n\n";
            s.write_all(format!("HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}").as_bytes())
                .unwrap();
            let _ = s.read(&mut buf);
        });
        (Server::local(port), t)
    }

    #[test]
    fn a_state_frame_arrives_and_stop_returns_quickly() {
        let (server, srv) = one_frame_server();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let mut t = Telemetry::start(server, move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while t.state()["mode"] != "HOLD" && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(t.state()["connected"], true);
        assert!(t.stream_up());
        assert!(calls.load(Ordering::SeqCst) >= 1);
        let begun = Instant::now();
        t.stop();
        assert!(
            begun.elapsed() < Duration::from_secs(2),
            "stop took {:?}",
            begun.elapsed()
        );
        srv.join().unwrap();
    }

    #[test]
    fn with_no_server_the_state_stays_disconnected() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut t = Telemetry::start(Server::local(port), || {});
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(t.state()["connected"], false);
        assert!(!t.stream_up());
        t.stop();
    }
}
