//! The bridge against a simulated vehicle over a real UDP socket: it finds
//! the vehicle, fills the store, writes a tlog, arms with an ACK confirmed
//! command, shows the vehicle's words on the console and shuts down clean.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::bridge::{Bridge, BridgeConfig};
use corvus_core::link::plain::connection::{open, Options};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::store::VehicleStateStore;

fn free_port() -> u16 {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap().port()
}

struct Vehicle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    armed: Arc<AtomicBool>,
    commands: Arc<Mutex<Vec<i64>>>,
}

impl Vehicle {
    fn start(port: u16) -> Vehicle {
        let stop = Arc::new(AtomicBool::new(false));
        let armed = Arc::new(AtomicBool::new(false));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let (s, a, c) = (Arc::clone(&stop), Arc::clone(&armed), Arc::clone(&commands));
        let thread = std::thread::spawn(move || {
            let opts = Options {
                source_system: 1,
                source_component: 1,
                input: false,
                ..Options::default()
            };
            let conn = open(&format!("udpout:127.0.0.1:{port}"), &opts).unwrap();
            let int = |v: i64| Value::Int(i128::from(v));
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut tick = 0;
            while !s.load(Ordering::SeqCst) {
                if last.elapsed() >= Duration::from_millis(200) {
                    last = Instant::now();
                    tick += 1;
                    let base = if a.load(Ordering::SeqCst) { 81 | 128 } else { 81 };
                    let hb = Message::named("HEARTBEAT", Wire::V2)
                        .unwrap()
                        .with("type", int(2))
                        .with("autopilot", int(12))
                        .with("base_mode", int(base))
                        .with("custom_mode", int(196_608))
                        .with("system_status", int(4))
                        .with("mavlink_version", int(3));
                    let _ = conn.send(&hb);
                    let pos = Message::named("GLOBAL_POSITION_INT", Wire::V2)
                        .unwrap()
                        .with("lat", int(481_234_567))
                        .with("lon", int(117_654_321))
                        .with("alt", int(520_500))
                        .with("relative_alt", int(30_250))
                        .with("hdg", int(18_050));
                    let _ = conn.send(&pos);
                    if tick == 2 {
                        let mut text = b"Takeoff detected".to_vec();
                        text.resize(50, 0);
                        let st = Message::named("STATUSTEXT", Wire::V2)
                            .unwrap()
                            .with("severity", int(6))
                            .with("text", Value::Bytes(text));
                        let _ = conn.send(&st);
                    }
                }
                let Ok(frames) = conn.recv(Duration::from_millis(50)) else {
                    break;
                };
                for r in frames {
                    let Ok(msg) = r.message else { continue };
                    if msg.name() != "COMMAND_LONG" {
                        continue;
                    }
                    let command = msg.get("command").and_then(Value::as_i64).unwrap();
                    c.lock().unwrap().push(command);
                    if command == 400 {
                        a.store(msg.get("param1").and_then(Value::as_f64) == Some(1.0), Ordering::SeqCst);
                    }
                    let ack = Message::named("COMMAND_ACK", Wire::V2)
                        .unwrap()
                        .with("command", int(command))
                        .with("result", int(0));
                    let _ = conn.send(&ack);
                }
            }
        });
        Vehicle {
            stop,
            thread: Some(thread),
            armed,
            commands,
        }
    }
}

impl Drop for Vehicle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn the_bridge_flies_a_simulated_vehicle_over_udp() {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("corvus-bridge-udp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(VehicleStateStore::new());
    let bridge = Bridge::new(
        Arc::clone(&store),
        BridgeConfig::new(&format!("udp:127.0.0.1:{port}"), dir.join("logs")),
    );
    let console = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&console);
    bridge.add_console_listener(move |name, text, _| sink.lock().unwrap().push(format!("{name}: {text}")));
    bridge.start();
    let vehicle = Vehicle::start(port);

    wait_for("the vehicle", Duration::from_secs(15), || {
        store.get("connected").is_some_and(|v| v == true) && store.get("mode").is_some_and(|v| v == "POSCTL")
    });
    assert_eq!(store.get("vehicle_type").unwrap(), "QUADROTOR");
    assert_eq!(store.get("autopilot").unwrap(), "PX4");
    assert_eq!(store.get("link_status").unwrap(), "connected");
    assert_eq!(bridge.target(), (1, 1));
    assert!(bridge.is_connected());

    wait_for("a position", Duration::from_secs(5), || {
        store.get("altitude_agl").is_some_and(|v| v.as_f64() == Some(30.2))
    });
    assert_eq!(store.get("heading").unwrap().as_f64(), Some(180.5));

    wait_for("the vehicle's words", Duration::from_secs(5), || {
        console
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "STATUSTEXT: Takeoff detected")
    });

    assert!(bridge.arm(true), "arm is ACK confirmed");
    assert!(vehicle.armed.load(Ordering::SeqCst));
    wait_for("armed in the store", Duration::from_secs(5), || {
        store.get("armed").is_some_and(|v| v == true)
    });
    assert!(vehicle.commands.lock().unwrap().contains(&400));
    assert!(bridge.arm(false));
    wait_for("disarmed", Duration::from_secs(5), || {
        store.get("armed").is_some_and(|v| v == false)
    });

    let tlog = bridge.tlog_path().expect("a tlog is open");
    let started = Instant::now();
    bridge.stop();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "stop took {:?}",
        started.elapsed()
    );
    assert_eq!(store.get("connected").unwrap(), false);
    assert_eq!(store.get("link_status").unwrap(), "disconnected");
    let data = std::fs::read(&tlog).unwrap();
    assert!(data.starts_with(b"#CORVUS-TLOG "), "the metadata line leads the file");
    assert!(data.len() > 200, "frames follow it ({} bytes)", data.len());
    drop(vehicle);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_bridge_can_be_stopped_and_started_again() {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("corvus-bridge-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(VehicleStateStore::new());
    let bridge = Bridge::new(
        Arc::clone(&store),
        BridgeConfig::new(&format!("udp:127.0.0.1:{port}"), dir.join("logs")),
    );
    let vehicle = Vehicle::start(port);
    bridge.start();
    bridge.start();
    wait_for("the vehicle", Duration::from_secs(15), || {
        store.get("connected").is_some_and(|v| v == true)
    });
    bridge.stop();
    bridge.stop();
    assert_eq!(store.get("connected").unwrap(), false);
    bridge.start();
    wait_for("the vehicle again", Duration::from_secs(15), || {
        store.get("connected").is_some_and(|v| v == true)
    });
    bridge.stop();
    drop(vehicle);
    let _ = std::fs::remove_dir_all(&dir);
}
