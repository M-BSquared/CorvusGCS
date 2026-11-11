//! The vehicle routes against a simulated vehicle over real UDP: the mission is
//! lowered for PX4 and handed over, the vehicle list shows the aircraft, and a
//! request that names a vehicle acts on that one or is refused.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::link::plain::connection::{open, Options};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::{CoreBuilder, Options as CoreOptions, VehicleModule, VehicleService};
use serde_json::{json, Value as Json};

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Vehicle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    counts: Arc<Mutex<Vec<i64>>>,
}

impl Vehicle {
    fn start(port: u16) -> Vehicle {
        let stop = Arc::new(AtomicBool::new(false));
        let counts = Arc::new(Mutex::new(Vec::new()));
        let (s, seen) = (Arc::clone(&stop), Arc::clone(&counts));
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
            let mut total = 0;
            while !s.load(Ordering::SeqCst) {
                if last.elapsed() >= Duration::from_millis(200) {
                    last = Instant::now();
                    let hb = Message::named("HEARTBEAT", Wire::V2)
                        .unwrap()
                        .with("type", int(2))
                        .with("autopilot", int(12))
                        .with("base_mode", int(81))
                        .with("custom_mode", int(196_608))
                        .with("system_status", int(4))
                        .with("mavlink_version", int(3));
                    let _ = conn.send(&hb);
                }
                // A busy stream, as a real autopilot has: the heartbeat wait must read past it.
                let _ = conn.send(
                    &Message::named("GLOBAL_POSITION_INT", Wire::V2)
                        .unwrap()
                        .with("lat", int(481_234_567)),
                );
                let Ok(frames) = conn.recv(Duration::from_millis(50)) else {
                    break;
                };
                for r in frames {
                    let Ok(msg) = r.message else { continue };
                    let request = |seq: i64| {
                        Message::named("MISSION_REQUEST_INT", Wire::V2)
                            .unwrap()
                            .with("target_system", int(254))
                            .with("target_component", int(190))
                            .with("seq", int(seq))
                    };
                    match msg.name() {
                        "MISSION_COUNT" => {
                            total = msg.get("count").and_then(Value::as_i64).unwrap();
                            seen.lock().unwrap().push(total);
                            let reply = if total == 0 {
                                Message::named("MISSION_ACK", Wire::V2)
                                    .unwrap()
                                    .with("target_system", int(254))
                                    .with("target_component", int(190))
                                    .with("type", int(0))
                            } else {
                                request(0)
                            };
                            let _ = conn.send(&reply);
                        }
                        "MISSION_ITEM_INT" => {
                            let seq = msg.get("seq").and_then(Value::as_i64).unwrap();
                            let reply = if seq + 1 < total {
                                request(seq + 1)
                            } else {
                                Message::named("MISSION_ACK", Wire::V2)
                                    .unwrap()
                                    .with("target_system", int(254))
                                    .with("target_component", int(190))
                                    .with("type", int(0))
                            };
                            let _ = conn.send(&reply);
                        }
                        "COMMAND_LONG" => {
                            let command = msg.get("command").and_then(Value::as_i64).unwrap();
                            let ack = Message::named("COMMAND_ACK", Wire::V2)
                                .unwrap()
                                .with("command", int(command))
                                .with("result", int(0));
                            let _ = conn.send(&ack);
                        }
                        _ => {}
                    }
                }
            }
        });
        Vehicle {
            stop,
            thread: Some(thread),
            counts,
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

fn plan() -> Json {
    json!({"name": "Circuit", "aircraft": "multirotor", "speed": 5, "items": [
        {"type": "takeoff", "lat": 47.5, "lon": 8.5, "alt": 20},
        {"type": "loiter_turns", "lat": 47.502, "lon": 8.505, "alt": 30, "turns": 2, "radius": 30, "direction": -1},
        {"type": "land", "lat": 47.5, "lon": 8.5}
    ]})
}

#[test]
fn the_vehicle_routes_fly_a_simulated_vehicle() {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("corvus-vehicle-routes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        json!({"mavlink_connection": format!("udp:127.0.0.1:{port}"), "tlog_dir": dir.join("tlogs")}).to_string(),
    )
    .unwrap();
    let core = CoreBuilder::new(CoreOptions::in_dir(&dir))
        .module(VehicleModule::new())
        .build()
        .unwrap();
    core.start().unwrap();
    let svc = core.service::<VehicleService>().expect("the vehicle service");
    svc.active().bridge.start();
    let vehicle = Vehicle::start(port);
    wait_for("the vehicle", Duration::from_secs(15), || {
        core.store().get("connected").is_some_and(|v| v == true)
    });

    let (status, fleet) = core.internal_call("GET", "/api/fleet", None);
    assert_eq!(status, 200);
    assert_eq!(fleet["vehicles"][0]["id"], "main:1");
    assert_eq!(fleet["vehicles"][0]["type"], "QUADROTOR");
    assert_eq!(fleet["vehicles"][0]["controlled"], true);
    assert_eq!(fleet["active"], "main:1");

    let call = |method: &str, path: &str, body: Json| core.internal_call(method, path, body.as_object().cloned());
    let (status, answer) = call("POST", "/api/fleet/select", json!({"id": "main:1"}));
    assert_eq!((status, &answer["ok"]), (200, &json!(true)));
    let (status, answer) = call("POST", "/api/fleet/select", json!({"id": "main:2"}));
    assert_eq!(
        (status, answer["error"].as_str()),
        (400, Some("that vehicle is not on the list"))
    );

    let (status, answer) = call("POST", "/api/mission/clear", json!({"vehicle": "main:2"}));
    assert_eq!(
        (status, answer["error"].as_str()),
        (409, Some("this link flies system 1"))
    );
    let (status, answer) = call("POST", "/api/mission/clear", json!({"vehicle": "link-3:1"}));
    assert_eq!(
        (status, answer["error"].as_str()),
        (409, Some("that vehicle's link is not open"))
    );
    let (status, _) = call("POST", "/api/mission/clear", json!({"vehicle": "main:1"}));
    assert_eq!(status, 200);

    let (status, answer) = call(
        "POST",
        "/api/mission/upload",
        json!({"plan": plan(), "start": false, "vehicle": "main:1"}),
    );
    assert_eq!(status, 200, "{answer}");
    let items = answer["items"].as_u64().unwrap();
    assert!(items > 6, "the Circle went up as waypoints for PX4 ({items} items)");
    assert_eq!(answer["started"], false);
    assert_eq!(vehicle.counts.lock().unwrap().last().copied(), Some(items as i64));

    let (status, answer) = call("POST", "/api/mission/start", json!({"items": items}));
    assert_eq!((status, &answer["ok"]), (200, &json!(true)));

    let (status, answer) = call("POST", "/api/mission/upload", json!({"plan": {"items": []}}));
    assert_eq!((status, answer["error"].as_str()), (400, Some("the mission is empty")));

    let (status, caps) = core.internal_call("GET", "/api/mavlink/capabilities", None);
    assert_eq!(
        (status, caps["stack"].as_str(), caps["vehicle_type"].as_i64()),
        (200, Some("px4"), Some(2))
    );
    let (status, modes) = core.internal_call("GET", "/api/mavlink/modes", None);
    assert_eq!(status, 200);
    assert!(
        modes["modes"].as_array().unwrap().iter().any(|m| m == "POSCTL"),
        "{modes}"
    );
    assert_eq!(modes["labels"]["POSCTL"], "POSITION");
    let (status, answer) = call("POST", "/api/mavlink/arm", json!({"arm": true}));
    assert_eq!((status, &answer["ok"]), (200, &json!(true)), "{answer}");
    let (status, answer) = call("POST", "/api/mavlink/manual", json!({"x": 0.5, "z": 0.5}));
    assert_eq!((status, &answer["ok"]), (200, &json!(true)), "{answer}");
    let (status, answer) = call("POST", "/api/mavlink/arm", json!({"arm": "x"}));
    assert_eq!((status, answer["error"].as_str()), (400, Some("arm must be boolean")));
    let (status, _) = call("POST", "/api/mavlink/disconnect", json!({}));
    assert_eq!(status, 200);
    let (_, auto) = core.internal_call("GET", "/api/mavlink/auto", None);
    assert_eq!(auto["auto"]["manual_override"], true);

    core.stop();
    drop(vehicle);
    let _ = std::fs::remove_dir_all(&dir);
}
