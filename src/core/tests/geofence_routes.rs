//! The geofence routes against a scripted vehicle over real UDP: the area is
//! drawn, put on the vehicle as an inclusion polygon of the FENCE mission type,
//! compared with what was drawn, and removed again. The vehicle is a strict
//! one: it loses the first MISSION_COUNT, it asks for every corner under the
//! fence type only, and it lets a late acknowledgement of a mission transfer
//! through first, which must not be taken for the fence's. It also gives the
//! fence back when asked, so the station can learn what a vehicle holds that
//! it did not put there itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::link::plain::connection::{open, Options};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::{CoreBuilder, Options as CoreOptions, VehicleModule, VehicleService};
use serde_json::{json, Value as Json};

const FENCE: i64 = 1;
const VERTEX_INCLUSION: i64 = 5001;
const VERTEX_EXCLUSION: i64 = 5002;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[derive(Default)]
struct Seen {
    /// Every MISSION_COUNT as (count, mission_type).
    counts: Vec<(i64, i64)>,
    /// Every item of the transfer in flight, as the vehicle would store it.
    items: Vec<Json>,
    /// What the vehicle holds once a transfer ended with its ACK.
    stored: Option<Vec<Json>>,
}

struct Vehicle {
    stop: Arc<AtomicBool>,
    refuse: Arc<AtomicBool>,
    seen: Arc<Mutex<Seen>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Vehicle {
    fn start(port: u16, autopilot: i64, custom_mode: i64) -> Vehicle {
        let stop = Arc::new(AtomicBool::new(false));
        let refuse = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let (s, r, log) = (Arc::clone(&stop), Arc::clone(&refuse), Arc::clone(&seen));
        let thread = std::thread::spawn(move || {
            let opts = Options {
                source_system: 1,
                source_component: 1,
                input: false,
                ..Options::default()
            };
            let conn = open(&format!("udpout:127.0.0.1:{port}"), &opts).unwrap();
            let int = |v: i64| Value::Int(i128::from(v));
            let to_station = |name: &str| {
                Message::named(name, Wire::V2)
                    .unwrap()
                    .with("target_system", int(254))
                    .with("target_component", int(190))
            };
            let ack = |result: i64, kind: i64| {
                to_station("MISSION_ACK")
                    .with("type", int(result))
                    .with("mission_type", int(kind))
            };
            let request = |seq: i64| {
                to_station("MISSION_REQUEST_INT")
                    .with("seq", int(seq))
                    .with("mission_type", int(FENCE))
            };
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut total = 0;
            let mut lost_one = false;
            while !s.load(Ordering::SeqCst) {
                if last.elapsed() >= Duration::from_millis(200) {
                    last = Instant::now();
                    let hb = Message::named("HEARTBEAT", Wire::V2)
                        .unwrap()
                        .with("type", int(2))
                        .with("autopilot", int(autopilot))
                        .with("base_mode", int(81))
                        .with("custom_mode", int(custom_mode))
                        .with("system_status", int(4))
                        .with("mavlink_version", int(3));
                    let _ = conn.send(&hb);
                }
                let Ok(frames) = conn.recv(Duration::from_millis(20)) else {
                    break;
                };
                for f in frames {
                    let Ok(msg) = f.message else { continue };
                    let field = |name: &str| msg.get(name).and_then(Value::as_i64).unwrap_or(0);
                    match msg.name() {
                        "MISSION_COUNT" => {
                            let kind = field("mission_type");
                            log.lock().unwrap().counts.push((field("count"), kind));
                            if kind != FENCE {
                                continue;
                            }
                            total = field("count");
                            if r.load(Ordering::SeqCst) {
                                let _ = conn.send(&ack(4, FENCE));
                                continue;
                            }
                            if total == 0 {
                                log.lock().unwrap().stored = Some(Vec::new());
                                let _ = conn.send(&ack(0, FENCE));
                                continue;
                            }
                            if !lost_one {
                                // A radio loses a frame: the station has to say it again.
                                lost_one = true;
                                continue;
                            }
                            log.lock().unwrap().items.clear();
                            // The end of an earlier mission transfer, arriving late.
                            let _ = conn.send(&ack(0, 0));
                            let _ = conn.send(&request(0));
                        }
                        "MISSION_ITEM_INT" => {
                            if field("mission_type") != FENCE {
                                continue;
                            }
                            let seq = field("seq");
                            let mut l = log.lock().unwrap();
                            if seq != l.items.len() as i64 {
                                continue;
                            }
                            l.items.push(json!({
                                "seq": seq,
                                "frame": field("frame"),
                                "command": field("command"),
                                "corners": msg.get("param1").and_then(Value::as_f64),
                                "lat": field("x"),
                                "lon": field("y"),
                            }));
                            if seq + 1 < total {
                                drop(l);
                                // Slower than the station polls, so an ACK it
                                // took too early shows as corners that never arrive.
                                std::thread::sleep(Duration::from_millis(30));
                                let _ = conn.send(&request(seq + 1));
                            } else {
                                l.stored = Some(l.items.clone());
                                drop(l);
                                let _ = conn.send(&ack(0, FENCE));
                            }
                        }
                        "MISSION_REQUEST_LIST" => {
                            if field("mission_type") != FENCE {
                                continue;
                            }
                            let held = log.lock().unwrap().stored.as_ref().map_or(0, Vec::len);
                            let count = to_station("MISSION_COUNT")
                                .with("count", int(held as i64))
                                .with("mission_type", int(FENCE));
                            let _ = conn.send(&count);
                        }
                        "MISSION_REQUEST_INT" => {
                            if field("mission_type") != FENCE {
                                continue;
                            }
                            let held = log.lock().unwrap().stored.clone().unwrap_or_default();
                            let Some(item) = held.get(field("seq") as usize) else {
                                continue;
                            };
                            let of = |name: &str| item[name].as_i64().unwrap_or(0);
                            let reply = to_station("MISSION_ITEM_INT")
                                .with("seq", int(of("seq")))
                                .with("frame", int(of("frame")))
                                .with("command", int(of("command")))
                                .with("param1", Value::Float(item["corners"].as_f64().unwrap_or(0.0)))
                                .with("x", int(of("lat")))
                                .with("y", int(of("lon")))
                                .with("mission_type", int(FENCE));
                            let _ = conn.send(&reply);
                        }
                        "COMMAND_LONG" => {
                            let reply = Message::named("COMMAND_ACK", Wire::V2)
                                .unwrap()
                                .with("command", int(field("command")))
                                .with("result", int(0));
                            let _ = conn.send(&reply);
                        }
                        _ => {}
                    }
                }
            }
        });
        Vehicle {
            stop,
            refuse,
            seen,
            thread: Some(thread),
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

fn the_fence_goes_up_and_comes_down(tag: &str, autopilot: i64, custom_mode: i64) {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("corvus-geofence-routes-{tag}-{}", std::process::id()));
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
    let call = |method: &str, path: &str, body: Json| core.internal_call(method, path, body.as_object().cloned());

    // Before there is a vehicle: the area can be drawn, and nothing can be sent.
    let (status, answer) = call("POST", "/api/geofence/upload", json!({}));
    assert_eq!(
        (status, answer["error"].as_str()),
        (400, Some("no area has been drawn"))
    );
    let bow_tie = json!([[11.0, 48.0], [11.1, 48.1], [11.1, 48.0], [11.0, 48.1]]);
    let (status, answer) = call("POST", "/api/geofence/save", json!({"polygon": bow_tie}));
    assert_eq!(
        (status, answer["error"].as_str()),
        (400, Some("the edges of the area cross each other"))
    );
    let area = json!([
        [11.0, 48.0],
        [11.1234567, 48.0],
        [11.1, 48.1],
        [11.0, 48.1],
        [11.0, 48.0]
    ]);
    let (status, answer) = call("POST", "/api/geofence/save", json!({"polygon": area}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        answer["polygon"].as_array().unwrap().len(),
        4,
        "the closing corner is dropped"
    );
    assert_eq!(answer["on_vehicle"], Json::Null, "nothing is known of the vehicle yet");
    let (status, answer) = call("POST", "/api/geofence/upload", json!({}));
    assert_eq!(status, 503, "no vehicle: {answer}");

    svc.active().bridge.start();
    let vehicle = Vehicle::start(port, autopilot, custom_mode);
    wait_for("the vehicle", Duration::from_secs(15), || {
        core.store().get("connected").is_some_and(|v| v == true)
    });

    // A fresh link knows nothing of the vehicle until it asks.
    let (_, answer) = call("GET", "/api/geofence", json!({}));
    assert_eq!(answer["vehicle_has_fence"], Json::Null);
    let (status, answer) = call("POST", "/api/geofence/read", json!({}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        (
            &answer["on_vehicle"],
            &answer["vehicle_has_fence"],
            &answer["vehicle_polygon"]
        ),
        (&json!(false), &json!(false), &json!([]))
    );

    // A request that names a vehicle acts on that one or is refused.
    for path in ["/api/geofence/upload", "/api/geofence/clear", "/api/geofence/read"] {
        let (status, answer) = call("POST", path, json!({"vehicle": "main:2"}));
        assert_eq!(
            (status, answer["error"].as_str()),
            (409, Some("this link flies system 1")),
            "{path}"
        );
    }
    assert!(
        vehicle.seen.lock().unwrap().counts.is_empty(),
        "a refused request sends nothing"
    );

    let (status, answer) = call("POST", "/api/geofence/upload", json!({"vehicle": "main:1"}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        (&answer["on_vehicle"], &answer["vehicle_has_fence"]),
        (&json!(true), &json!(true))
    );
    {
        let seen = vehicle.seen.lock().unwrap();
        assert_eq!(
            seen.counts,
            vec![(4, FENCE), (4, FENCE)],
            "the lost MISSION_COUNT is said again, and never as a mission"
        );
        let stored = seen
            .stored
            .as_ref()
            .expect("the route answered only after the vehicle had every corner");
        let corners = [
            (480_000_000, 110_000_000),
            (480_000_000, 111_234_567),
            (481_000_000, 111_000_000),
            (481_000_000, 110_000_000),
        ];
        assert_eq!(stored.len(), corners.len());
        for (seq, (item, (lat, lon))) in stored.iter().zip(corners).enumerate() {
            assert_eq!(
                item,
                &json!({"seq": seq, "frame": 0, "command": VERTEX_INCLUSION, "corners": 4.0, "lat": lat, "lon": lon}),
            );
        }
    }
    let (_, answer) = call("GET", "/api/geofence", json!({}));
    assert_eq!(answer["on_vehicle"], true);

    // The link is opened again: what the station knew is gone, and the vehicle says it again.
    let (status, answer) = call(
        "POST",
        "/api/mavlink/connect",
        json!({"connection": format!("udp:127.0.0.1:{port}")}),
    );
    assert_eq!(status, 200, "{answer}");
    wait_for("the link to come back", Duration::from_secs(15), || {
        let (_, answer) = call("GET", "/api/geofence", json!({}));
        answer["on_vehicle"].is_null() && core.store().get("connected").is_some_and(|v| v == true)
    });
    let (status, answer) = call("POST", "/api/geofence/read", json!({}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        (&answer["on_vehicle"], &answer["vehicle_has_fence"]),
        (&json!(true), &json!(true)),
        "{answer}"
    );
    assert_eq!(
        answer["vehicle_polygon"],
        json!([[11.0, 48.0], [11.1234567, 48.0], [11.1, 48.1], [11.0, 48.1]])
    );

    // Another station adds a zone this one cannot draw: the fence is no longer the drawing.
    let own = vehicle.seen.lock().unwrap().stored.clone().unwrap();
    {
        let mut seen = vehicle.seen.lock().unwrap();
        let held = seen.stored.as_mut().unwrap();
        for seq in 4..7 {
            held.push(
                json!({"seq": seq, "frame": 0, "command": VERTEX_EXCLUSION, "corners": 3.0,
                "lat": 480_500_000 + seq, "lon": 110_500_000 + seq * seq}),
            );
        }
    }
    let (status, answer) = call("POST", "/api/geofence/read", json!({}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        (
            &answer["on_vehicle"],
            &answer["vehicle_has_fence"],
            &answer["vehicle_polygon"]
        ),
        (&json!(false), &json!(true), &Json::Null),
        "{answer}"
    );
    vehicle.seen.lock().unwrap().stored = Some(own);
    let (_, answer) = call("POST", "/api/geofence/read", json!({}));
    assert_eq!(answer["on_vehicle"], true, "{answer}");

    // The drawing moves on: the vehicle still holds a fence, but not this one.
    let moved = json!([[11.0, 48.0], [11.2, 48.0], [11.2, 48.1]]);
    let (status, answer) = call("POST", "/api/geofence/save", json!({"polygon": moved}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        (&answer["on_vehicle"], &answer["vehicle_has_fence"]),
        (&json!(false), &json!(true))
    );

    // A vehicle that says no: the reason reaches the operator and nothing is claimed.
    vehicle.refuse.store(true, Ordering::SeqCst);
    let (status, answer) = call("POST", "/api/geofence/upload", json!({}));
    assert_eq!(status, 409, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().starts_with("Geofence upload failed"),
        "{answer}"
    );
    let (status, answer) = call("POST", "/api/geofence/clear", json!({}));
    assert_eq!(status, 409, "{answer}");
    let (_, answer) = call("GET", "/api/geofence", json!({}));
    assert_eq!(
        (&answer["on_vehicle"], &answer["vehicle_has_fence"]),
        (&json!(false), &json!(true)),
        "a refused transfer changes nothing the station believes"
    );
    vehicle.refuse.store(false, Ordering::SeqCst);

    let (status, answer) = call("POST", "/api/geofence/clear", json!({}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["vehicle_has_fence"], false);
    assert_eq!(answer["polygon"].as_array().unwrap().len(), 3, "the drawing is kept");
    {
        let seen = vehicle.seen.lock().unwrap();
        assert_eq!(seen.counts.last(), Some(&(0, FENCE)));
        assert_eq!(seen.stored.as_deref(), Some(&[][..]));
        assert!(
            seen.counts.iter().all(|(_, kind)| *kind == FENCE),
            "no fence step ever touches the stored mission: {:?}",
            seen.counts
        );
    }

    core.stop();
    drop(vehicle);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_px4_vehicle_takes_the_fence_and_gives_it_up() {
    // MAV_AUTOPILOT_PX4, POSCTL.
    the_fence_goes_up_and_comes_down("px4", 12, 196_608);
}

#[test]
fn an_ardupilot_vehicle_takes_the_fence_without_a_home_slot() {
    // MAV_AUTOPILOT_ARDUPILOTMEGA, LOITER: the mission's home slot at sequence
    // 0 is not part of a fence, so the same four corners go up.
    the_fence_goes_up_and_comes_down("ardupilot", 3, 5);
}
