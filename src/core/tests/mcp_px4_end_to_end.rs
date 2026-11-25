//! An AI client over real HTTP against the whole core and a scripted PX4 over
//! real UDP: every built in MCP tool is called through `POST /mcp`, and what
//! the vehicle received on the wire is what PX4 expects (the mode triple, the
//! AMSL takeoff altitude, the byte exact INT32 parameter, the command list of
//! the capabilities). A plugin tool goes out to a window and its answer comes
//! back to the client.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::link::plain::connection::{open, Options};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value};
use corvus_core::mcp::McpService;
use corvus_core::{CoreBuilder, Http, HttpInfo, HttpOptions, Options as CoreOptions, VehicleModule, VehicleService};
use serde_json::{json, Value as Json};

const HOME_AMSL_M: f64 = 560.0;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One received message the test looks at: its name and its numeric fields.
#[derive(Clone, Debug)]
struct Seen {
    name: String,
    fields: Vec<(String, f64)>,
    text: String,
}

impl Seen {
    fn f(&self, key: &str) -> f64 {
        self.fields.iter().find(|(k, _)| k == key).map_or(f64::NAN, |(_, v)| *v)
    }
}

struct Vehicle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// name, MAV_PARAM_TYPE, the four bytes of the float field
    params: Arc<Mutex<Vec<(String, i64, u32)>>>,
}

fn int(v: i64) -> Value {
    Value::Int(i128::from(v))
}

fn param_value(name: &str, ty: i64, bits: u32, index: i64, count: i64) -> Message {
    let mut id = name.as_bytes().to_vec();
    id.resize(16, 0);
    Message::named("PARAM_VALUE", Wire::V2)
        .unwrap()
        .with("param_id", Value::Bytes(id))
        .with("param_value", Value::Float(f64::from(f32::from_bits(bits))))
        .with("param_type", int(ty))
        .with("param_count", int(count))
        .with("param_index", int(index))
}

impl Vehicle {
    fn start(port: u16) -> Vehicle {
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        // PX4 carries an INT32 as its own bytes in the float field.
        let params = Arc::new(Mutex::new(vec![
            ("COM_RC_IN_MODE".to_string(), 6, 1u32),
            ("MPC_XY_VEL_MAX".to_string(), 9, 12.0f32.to_bits()),
            ("NAV_DLL_ACT".to_string(), 6, 0u32),
            ("SYS_AUTOSTART".to_string(), 6, 4001u32),
        ]));
        let (s, log, table) = (Arc::clone(&stop), Arc::clone(&seen), Arc::clone(&params));
        let thread = std::thread::spawn(move || {
            let opts = Options {
                source_system: 1,
                source_component: 1,
                input: false,
                ..Options::default()
            };
            let conn = open(&format!("udpout:127.0.0.1:{port}"), &opts).unwrap();
            let mut last = Instant::now() - Duration::from_secs(1);
            let (mut armed, mut custom_mode, mut total, mut airborne) = (false, 196_608i64, 0i64, false);
            let ack = |command: i64, result: i64| {
                Message::named("COMMAND_ACK", Wire::V2)
                    .unwrap()
                    .with("command", int(command))
                    .with("result", int(result))
            };
            let mission_ack = |kind: f64| {
                Message::named("MISSION_ACK", Wire::V2)
                    .unwrap()
                    .with("mission_type", int(kind as i64))
                    .with("target_system", int(254))
                    .with("target_component", int(190))
                    .with("type", int(0))
            };
            let request = |seq: i64, kind: f64| {
                Message::named("MISSION_REQUEST_INT", Wire::V2)
                    .unwrap()
                    .with("mission_type", int(kind as i64))
                    .with("target_system", int(254))
                    .with("target_component", int(190))
                    .with("seq", int(seq))
            };
            while !s.load(Ordering::SeqCst) {
                if last.elapsed() >= Duration::from_millis(200) {
                    last = Instant::now();
                    let base = if armed { 81 | 128 } else { 81 };
                    let _ = conn.send(
                        &Message::named("HEARTBEAT", Wire::V2)
                            .unwrap()
                            .with("type", int(2))
                            .with("autopilot", int(12))
                            .with("base_mode", int(base))
                            .with("custom_mode", int(custom_mode))
                            .with("system_status", int(if armed { 4 } else { 3 }))
                            .with("mavlink_version", int(3)),
                    );
                    let _ = conn.send(
                        &Message::named("GLOBAL_POSITION_INT", Wire::V2)
                            .unwrap()
                            .with("lat", int(480_795_000))
                            .with("lon", int(116_385_000))
                            .with(
                                "alt",
                                int((HOME_AMSL_M * 1000.0) as i64 + if airborne { 25_000 } else { 0 }),
                            )
                            .with("relative_alt", int(if airborne { 25_000 } else { 0 }))
                            .with("hdg", int(9_000)),
                    );
                    let _ = conn.send(
                        &Message::named("HOME_POSITION", Wire::V2)
                            .unwrap()
                            .with("latitude", int(480_795_000))
                            .with("longitude", int(116_385_000))
                            .with("altitude", int((HOME_AMSL_M * 1000.0) as i64)),
                    );
                    let _ = conn.send(
                        &Message::named("GPS_RAW_INT", Wire::V2)
                            .unwrap()
                            .with("fix_type", int(3))
                            .with("lat", int(480_795_000))
                            .with("lon", int(116_385_000))
                            .with("alt", int((HOME_AMSL_M * 1000.0) as i64))
                            .with("satellites_visible", int(14)),
                    );
                    let _ = conn.send(
                        &Message::named("EXTENDED_SYS_STATE", Wire::V2)
                            .unwrap()
                            .with("landed_state", int(if airborne { 2 } else { 1 })),
                    );
                }
                let Ok(frames) = conn.recv(Duration::from_millis(20)) else {
                    break;
                };
                for r in frames {
                    let Ok(msg) = r.message else { continue };
                    let name = msg.name();
                    if name == "HEARTBEAT" {
                        continue;
                    }
                    let fields: Vec<(String, f64)> = msg
                        .def()
                        .fields
                        .iter()
                        .filter_map(|f| Some((f.name.to_string(), msg.get(f.name)?.as_f64()?)))
                        .collect();
                    let text = msg.get("param_id").and_then(Value::as_text).unwrap_or_default();
                    let seen = Seen {
                        name: name.to_string(),
                        fields,
                        text,
                    };
                    log.lock().unwrap().push(seen.clone());
                    match name {
                        "COMMAND_LONG" | "COMMAND_INT" => {
                            let command = seen.f("command") as i64;
                            match command {
                                400 => armed = seen.f("param1") == 1.0,
                                176 => {
                                    custom_mode = ((seen.f("param2") as i64) << 16) | ((seen.f("param3") as i64) << 24)
                                }
                                22 => {
                                    custom_mode = (4 << 16) | (2 << 24);
                                    airborne = true;
                                }
                                21 => custom_mode = (4 << 16) | (6 << 24),
                                20 => custom_mode = (4 << 16) | (5 << 24),
                                192 => custom_mode = (4 << 16) | (3 << 24),
                                _ => {}
                            }
                            if (command == 512 && seen.f("param1") == 148.0) || command == 520 {
                                let _ = conn.send(
                                    &Message::named("AUTOPILOT_VERSION", Wire::V2)
                                        .unwrap()
                                        .with("capabilities", int(1 | 2 | 8 | 32 | 128 | 256 | 512 | 8192))
                                        .with("flight_sw_version", int((1 << 24) | (16 << 16) | 255)),
                                );
                            }
                            // PX4 answers a command it does not know with UNSUPPORTED.
                            let known = [
                                400, 176, 22, 21, 20, 192, 512, 520, 300, 179, 178, 195, 34, 189, 187, 203, 206, 211,
                                224, 1000, 3000, 511, 519, 2500, 2800, 245, 246, 252,
                            ];
                            let _ = conn.send(&ack(command, if known.contains(&command) { 0 } else { 3 }));
                        }
                        "PARAM_REQUEST_LIST" => {
                            let t = table.lock().unwrap().clone();
                            for (i, (n, ty, bits)) in t.iter().enumerate() {
                                let _ = conn.send(&param_value(n, *ty, *bits, i as i64, t.len() as i64));
                            }
                        }
                        "PARAM_REQUEST_READ" => {
                            let t = table.lock().unwrap().clone();
                            if let Some((i, (n, ty, bits))) = t.iter().enumerate().find(|(_, p)| p.0 == seen.text) {
                                let _ = conn.send(&param_value(n, *ty, *bits, i as i64, t.len() as i64));
                            }
                        }
                        "PARAM_SET" => {
                            // The four bytes as they travelled: a float read back would quiet a signalling NaN pattern.
                            let mut four = [0u8; 4];
                            for (d, s) in four.iter_mut().zip(r.frame.payload.iter()) {
                                *d = *s;
                            }
                            let bits = u32::from_le_bytes(four);
                            let mut t = table.lock().unwrap();
                            let count = t.len() as i64;
                            if let Some((i, p)) = t.iter_mut().enumerate().find(|(_, p)| p.0 == seen.text) {
                                // PX4 keeps its own type and takes the bytes as they came.
                                p.2 = bits;
                                let _ = conn.send(&param_value(&p.0, p.1, p.2, i as i64, count));
                            }
                        }
                        "MISSION_COUNT" => {
                            total = seen.f("count") as i64;
                            let _ = conn.send(&if total == 0 {
                                mission_ack(seen.f("mission_type"))
                            } else {
                                request(0, seen.f("mission_type"))
                            });
                        }
                        "MISSION_ITEM_INT" => {
                            let seq = seen.f("seq") as i64;
                            let _ = conn.send(&if seq + 1 < total {
                                request(seq + 1, seen.f("mission_type"))
                            } else {
                                mission_ack(seen.f("mission_type"))
                            });
                        }
                        "MISSION_CLEAR_ALL" => {
                            total = 0;
                            let _ = conn.send(&mission_ack(seen.f("mission_type")));
                        }
                        "MISSION_REQUEST_LIST" => {
                            let _ = conn.send(
                                &Message::named("MISSION_COUNT", Wire::V2)
                                    .unwrap()
                                    .with("target_system", int(254))
                                    .with("target_component", int(190))
                                    .with("count", int(0)),
                            );
                        }
                        _ => {}
                    }
                }
            }
        });
        Vehicle {
            stop,
            thread: Some(thread),
            seen,
            params,
        }
    }

    /// What arrived since the last call, without the streams nobody asked about.
    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    fn param_bits(&self, name: &str) -> u32 {
        self.params
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.0 == name)
            .map_or(0, |p| p.2)
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

/// One HTTP request: `(status, headers lowercased, body as JSON)`.
fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &str,
    body: Option<&Json>,
) -> (u16, Vec<(String, String)>, Json) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let payload = body.map(Json::to_string).unwrap_or_default();
    let mut raw = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{headers}",
        addr.port()
    );
    if body.is_some() {
        raw.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    raw.push_str("\r\n");
    raw.push_str(&payload);
    s.write_all(raw.as_bytes()).unwrap();
    let mut all = Vec::new();
    let _ = s.read_to_end(&mut all);
    let split = all.windows(4).position(|w| w == b"\r\n\r\n").expect("a head");
    let head = String::from_utf8_lossy(&all[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    let body = serde_json::from_slice(&all[split + 4..]).unwrap_or(Json::Null);
    (status, headers, body)
}

/// The AI client: a session on `/mcp` with the bearer token.
struct Client {
    addr: SocketAddr,
    auth: String,
    next: i64,
}

impl Client {
    fn rpc(&mut self, method: &str, params: Json) -> (u16, Json) {
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        let (status, _, body) = http(self.addr, "POST", "/mcp", &self.auth, Some(&msg));
        (status, body)
    }

    /// `tools/call`: `(is an error, the tool's answer)`.
    fn call(&mut self, name: &str, args: Json) -> (bool, Json) {
        let (status, body) = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        assert_eq!(status, 200, "{name}: {body}");
        let result = &body["result"];
        assert!(result.is_object(), "{name}: {body}");
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        let value = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
        (result["isError"] == true, value)
    }

    fn ok(&mut self, name: &str, args: Json) -> Json {
        let (is_error, value) = self.call(name, args.clone());
        assert!(!is_error, "{name} {args} was refused: {value}");
        value
    }

    fn refused(&mut self, name: &str, args: Json) -> String {
        let (is_error, value) = self.call(name, args.clone());
        assert!(is_error, "{name} {args} should have been refused: {value}");
        value.as_str().unwrap_or("").to_string()
    }
}

fn commands(seen: &[Seen]) -> Vec<&Seen> {
    seen.iter()
        .filter(|s| s.name == "COMMAND_LONG" || s.name == "COMMAND_INT")
        .collect()
}

fn command(seen: &[Seen], id: i64) -> Seen {
    commands(seen)
        .into_iter()
        .find(|s| s.f("command") as i64 == id)
        .unwrap_or_else(|| panic!("MAV_CMD {id} did not reach the vehicle: {:?}", commands(seen)))
        .clone()
}

#[test]
fn an_mcp_client_flies_a_px4_over_http() {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("corvus-mcp-px4-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        json!({"mavlink_connection": format!("udp:127.0.0.1:{port}"), "tlog_dir": dir.join("tlogs")}).to_string(),
    )
    .unwrap();
    let core = CoreBuilder::new(CoreOptions::in_dir(&dir))
        .module(VehicleModule::new())
        .module(corvus_core::PluginsModule::new())
        .module(corvus_core::McpModule::new())
        .module(corvus_core::SetupSchemasModule::new())
        .module(Http::new(HttpOptions {
            port: 0,
            ..HttpOptions::default()
        }))
        .build()
        .unwrap();
    core.start().unwrap();
    let addr = core.service::<HttpInfo>().unwrap().addr().unwrap();
    core.service::<VehicleService>().unwrap().active().bridge.start();
    let vehicle = Vehicle::start(port);
    wait_for("the vehicle", Duration::from_secs(15), || {
        core.store().get("connected").is_some_and(|v| v == true)
    });

    // ---- the gate: off by default, then the token --------------------------
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    let (status, _, body) = http(addr, "POST", "/mcp", "", Some(&ping));
    assert_eq!(status, 403, "MCP is off until the operator switches it on: {body}");
    let (status, _, card) = http(addr, "GET", "/api/mcp", "", None);
    assert_eq!(status, 200);
    let token = card["token"].as_str().unwrap().to_string();
    assert!(token.len() >= 32, "a real token");
    let (status, _, body) = http(addr, "POST", "/api/mcp/settings", "", Some(&json!({"enabled": true})));
    assert_eq!(status, 200, "{body}");
    let (status, _, _) = http(addr, "POST", "/mcp", "", Some(&ping));
    assert_eq!(status, 401, "no token, no answer");
    let (status, _, _) = http(addr, "POST", "/mcp", "Authorization: Bearer wrong\r\n", Some(&ping));
    assert_eq!(status, 401);

    let auth = format!("Authorization: Bearer {token}\r\n");
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "test client", "version": "1"}}});
    let (status, headers, body) = http(addr, "POST", "/mcp", &auth, Some(&init));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["serverInfo"]["version"], corvus_core::version::version());
    let session = headers
        .iter()
        .find(|(k, _)| k == "mcp-session-id")
        .map(|(_, v)| v.clone())
        .expect("a session id");
    let mut ai = Client {
        addr,
        auth: format!("{auth}Mcp-Session-Id: {session}\r\n"),
        next: 1,
    };

    // ---- with the defaults an agent reads and changes nothing --------------
    let listed = |ai: &mut Client| -> Vec<String> {
        let (_, body) = ai.rpc("tools/list", json!({}));
        body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    let mut names = listed(&mut ai);
    for later in ["wait_for", "draw_on_map"] {
        assert!(
            names.iter().any(|n| n == later),
            "reading and drawing need no switch: {later}"
        );
    }
    for closed in ["orbit", "set_geofence", "take_photo", "fly_pattern"] {
        assert!(
            !names.iter().any(|n| n == closed),
            "{closed} is off until it is allowed"
        );
    }
    names.retain(|n| !corvus_core::mcp::tools::is_extension(n));
    assert_eq!(
        names,
        [
            "get_vehicle_state",
            "list_vehicles",
            "get_capabilities",
            "list_flight_modes",
            "download_mission",
            "list_parameters",
            "get_parameters",
            "download_parameters"
        ]
    );
    let why = ai.refused("arm", json!({}));
    assert!(why.contains("not allowed"), "{why}");
    assert!(commands(&vehicle.take()).iter().all(|c| c.f("command") != 400.0));

    let state = ai.ok("get_vehicle_state", json!({}));
    assert_eq!(state["connected"], true, "{state}");
    assert_eq!(state["armed"], false);
    let caps = ai.ok("get_capabilities", json!({}));
    assert_eq!(
        (caps["stack"].as_str(), caps["velocity_mode"].as_str()),
        (Some("px4"), Some("OFFBOARD"))
    );
    assert_eq!(caps["takeoff_frame"], "amsl", "{caps}");
    let offered: Vec<String> = caps["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert!(
        offered.contains(&"DO_ORBIT".to_string()) && !offered.contains(&"DO_SET_SERVO".to_string()),
        "{offered:?}"
    );
    let modes = ai.ok("list_flight_modes", json!({}));
    let modes: Vec<&str> = modes.as_array().unwrap().iter().filter_map(Json::as_str).collect();
    assert!(modes.contains(&"POSCTL") && modes.contains(&"OFFBOARD"), "{modes:?}");
    let fleet = ai.ok("list_vehicles", json!({}));
    assert_eq!(fleet["vehicles"][0]["id"], "main:1");

    // ---- parameters: read, type exact --------------------------------------
    let (is_error, hint) = ai.call("list_parameters", json!({}));
    if is_error {
        assert!(hint.as_str().unwrap().contains("download_parameters"), "{hint}");
        ai.ok("download_parameters", json!({}));
    }
    wait_for("the parameters", Duration::from_secs(15), || {
        !ai.call("list_parameters", json!({})).0
    });
    let all = ai.ok("list_parameters", json!({}));
    assert_eq!(all["count"], 4, "{all}");
    let got = ai.ok(
        "get_parameters",
        json!({"names": ["COM_RC_IN_MODE", "SYS_AUTOSTART", "MPC_XY_VEL_MAX", "NOPE"]}),
    );
    let value = |name: &str| {
        got["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        value("SYS_AUTOSTART")["value"].as_f64(),
        Some(4001.0),
        "an INT32 is read from its bytes: {got}"
    );
    assert_eq!(value("COM_RC_IN_MODE")["value"].as_f64(), Some(1.0));
    assert_eq!(value("MPC_XY_VEL_MAX")["value"].as_f64(), Some(12.0));
    assert_eq!(got["missing"], json!(["NOPE"]));
    let mpc = ai.ok("list_parameters", json!({"prefix": "mpc_"}));
    assert_eq!(mpc["count"], 1);

    // ---- everything that changes the vehicle, allowed without a question ---
    let all_on = json!({
        "permissions": {"params_write": true, "mission": true, "arm": true, "flight": true, "goto": true,
                        "control": true, "command": true, "plugins": true},
        "confirm": {"params_write": false, "mission": false, "arm": false, "flight": false, "control": false,
                    "command": false},
    });
    let (status, _, body) = http(addr, "POST", "/api/mcp/settings", "", Some(&all_on));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        listed(&mut ai).len(),
        corvus_core::mcp::tools::builtin()
            .iter()
            .filter(|t| t.permission == "map" || !corvus_core::mcp::settings::EXTENSIONS.contains(&t.permission))
            .count(),
        "every built in tool the switches above open is listed"
    );

    // ---- parameters: write --------------------------------------------------
    vehicle.take();
    ai.ok("set_parameter", json!({"name": "COM_RC_IN_MODE", "value": 4}));
    assert_eq!(
        vehicle.param_bits("COM_RC_IN_MODE"),
        4,
        "the INT32 went out as its bytes, not as 4.0"
    );
    let set = vehicle
        .take()
        .into_iter()
        .find(|s| s.name == "PARAM_SET")
        .expect("a PARAM_SET");
    assert_eq!(set.f("param_type"), 6.0, "written with the type PX4 reported");
    ai.ok("set_parameter", json!({"name": "MPC_XY_VEL_MAX", "value": 8.5}));
    assert_eq!(vehicle.param_bits("MPC_XY_VEL_MAX"), 8.5f32.to_bits());
    let back = ai.ok("get_parameters", json!({"names": ["COM_RC_IN_MODE", "MPC_XY_VEL_MAX"]}));
    assert_eq!(back["params"][0]["value"].as_f64(), Some(4.0), "{back}");
    assert_eq!(back["params"][1]["value"].as_f64(), Some(8.5));
    // A negative INT32 is a NaN pattern in the float field: its bytes survive, and a pattern a float
    // cannot carry unchanged (a signalling NaN) is refused instead of written wrong.
    ai.ok("set_parameter", json!({"name": "NAV_DLL_ACT", "value": -1}));
    assert_eq!(
        vehicle.param_bits("NAV_DLL_ACT") as i32,
        -1,
        "INT32 -1 went out byte exact"
    );
    for v in [-5_000_000, 2_140_000_000] {
        let why = ai.refused("set_parameter", json!({"name": "NAV_DLL_ACT", "value": v}));
        assert!(why.contains("cannot be sent exactly"), "{why}");
        assert_eq!(
            vehicle.param_bits("NAV_DLL_ACT") as i32,
            -1,
            "and the value on the vehicle stays"
        );
    }
    let why = ai.refused("set_parameter", json!({"name": "ARMING_CHECK", "value": 0}));
    assert!(why.contains("not a parameter the connected firmware reported"), "{why}");
    let (is_error, why) = ai.call("set_parameter", json!({"name": "COM_RC_IN_MODE", "value": 1.5}));
    assert!(is_error, "a fraction is no INT32: {why}");
    assert_eq!(vehicle.param_bits("COM_RC_IN_MODE"), 4, "and nothing was written");
    assert!(vehicle
        .take()
        .iter()
        .all(|s| s.name != "PARAM_SET" || s.text != "ARMING_CHECK"));

    // ---- flight commands ------------------------------------------------------
    ai.ok("set_flight_mode", json!({"mode": "POSCTL"}));
    let mode = command(&vehicle.take(), 176);
    assert_eq!(
        (mode.f("param1") as i64 & 1, mode.f("param2"), mode.f("param3")),
        (1, 3.0, 0.0),
        "POSCTL is main mode 3, custom mode flag set"
    );
    let why = ai.refused("set_flight_mode", json!({"mode": "GUIDED"}));
    assert!(!why.is_empty(), "an ArduPilot mode is refused on PX4");
    assert!(
        commands(&vehicle.take()).iter().all(|c| c.f("command") != 176.0),
        "and never sent"
    );

    ai.ok("arm", json!({}));
    assert_eq!(command(&vehicle.take(), 400).f("param1"), 1.0);
    wait_for("armed", Duration::from_secs(5), || {
        ai.ok("get_vehicle_state", json!({}))["armed"] == true
    });
    ai.ok("disarm", json!({}));
    assert_eq!(command(&vehicle.take(), 400).f("param1"), 0.0);
    wait_for("disarmed", Duration::from_secs(5), || {
        ai.ok("get_vehicle_state", json!({}))["armed"] == false
    });

    // On the ground one point is a mission of a takeoff and a waypoint, started: PX4 arms on MISSION_START.
    ai.ok("fly_to", json!({"points": [{"lat": 48.081, "lon": 11.641, "alt": 30}]}));
    let seen = vehicle.take();
    let sent: Vec<f64> = seen
        .iter()
        .filter(|s| s.name == "MISSION_ITEM_INT")
        .map(|s| s.f("command"))
        .collect();
    assert_eq!(sent, [22.0, 16.0], "from the ground: take off, then the point");
    let start = command(&seen, 300);
    assert_eq!((start.f("param1"), start.f("param2")), (0.0, 1.0));

    ai.ok("takeoff", json!({"altitude": 25}));
    let seen = vehicle.take();
    let order: Vec<i64> = commands(&seen)
        .iter()
        .map(|c| c.f("command") as i64)
        .filter(|c| [22, 400].contains(c))
        .collect();
    assert_eq!(order, [22, 400], "PX4: the takeoff command, then arm");
    let takeoff = command(&seen, 22);
    assert!(
        (takeoff.f("param7") - (HOME_AMSL_M + 25.0)).abs() < 0.5,
        "param7 is AMSL on PX4: {}",
        takeoff.f("param7")
    );

    ai.ok("hold", json!({}));
    let hold = command(&vehicle.take(), 176);
    assert_eq!((hold.f("param2"), hold.f("param3")), (4.0, 3.0), "AUTO LOITER");
    ai.ok("land", json!({}));
    let seen = vehicle.take();
    assert!(
        commands(&seen)
            .iter()
            .any(|c| c.f("command") == 21.0 || (c.f("command") == 176.0 && c.f("param3") == 6.0)),
        "{:?}",
        commands(&seen)
    );
    ai.ok("return_to_launch", json!({}));
    let seen = vehicle.take();
    assert!(
        commands(&seen)
            .iter()
            .any(|c| c.f("command") == 20.0 || (c.f("command") == 176.0 && c.f("param3") == 5.0)),
        "{:?}",
        commands(&seen)
    );

    ai.ok("set_home", json!({"lat": 48.08, "lon": 11.64}));
    let home = command(&vehicle.take(), 179);
    assert_eq!(home.f("param1"), 0.0, "the given position, not the current one");
    let (lat, lon) = if home.name == "COMMAND_INT" {
        (home.f("x") / 1e7, home.f("y") / 1e7)
    } else {
        (home.f("param5"), home.f("param6"))
    };
    assert!((lat - 48.08).abs() < 1e-5 && (lon - 11.64).abs() < 1e-5, "{home:?}");

    wait_for("airborne", Duration::from_secs(5), || {
        ai.ok("get_vehicle_state", json!({}))["altitude_agl"]
            .as_f64()
            .unwrap_or(0.0)
            > 20.0
    });
    ai.ok("fly_to", json!({"points": [{"lat": 48.081, "lon": 11.641, "alt": 30}]}));
    let go = command(&vehicle.take(), 192);
    assert_eq!(go.name, "COMMAND_INT", "a position travels as integers");
    assert!(
        (go.f("x") / 1e7 - 48.081).abs() < 1e-6 && (go.f("y") / 1e7 - 11.641).abs() < 1e-6,
        "{go:?}"
    );
    let amsl = if go.f("frame") == 0.0 || go.f("frame") == 5.0 {
        go.f("z")
    } else {
        go.f("z") + HOME_AMSL_M
    };
    assert!(
        (amsl - (HOME_AMSL_M + 30.0)).abs() < 0.5,
        "30 m above home, whatever the frame: {go:?}"
    );

    // ---- send_command: the list of the capabilities and nothing else --------
    ai.ok(
        "send_command",
        json!({"command": "DO_CHANGE_SPEED", "params": [1, 7.5, -1]}),
    );
    let speed = command(&vehicle.take(), 178);
    assert_eq!(
        (speed.f("param1"), speed.f("param2"), speed.f("param3")),
        (1.0, 7.5, -1.0)
    );
    ai.ok(
        "send_command",
        json!({"command": "MAV_CMD_DO_ORBIT", "params": [20, 2], "lat": 48.08, "lon": 11.64, "alt": 40}),
    );
    let orbit = command(&vehicle.take(), 34);
    assert_eq!((orbit.f("param1"), orbit.f("param2")), (20.0, 2.0), "{orbit:?}");
    for forbidden in [
        "COMPONENT_ARM_DISARM",
        "NAV_TAKEOFF",
        "DO_SET_MODE",
        "DO_FLIGHTTERMINATION",
        "PREFLIGHT_CALIBRATION",
        "PREFLIGHT_REBOOT_SHUTDOWN",
        "DO_MOTOR_TEST",
        "ACTUATOR_TEST",
        "DO_SET_SERVO",
        "DO_SET_PARAMETER",
        "PREFLIGHT_STORAGE",
    ] {
        let why = ai.refused("send_command", json!({"command": forbidden, "params": [1]}));
        assert!(!why.is_empty());
    }
    let why = ai.refused("send_command", json!({"command": 400, "params": [1, 21196]}));
    assert!(!why.is_empty(), "a number gets no further than a name");
    ai.refused("send_command", json!({"command": "NOT_A_COMMAND"}));
    assert!(
        commands(&vehicle.take()).is_empty(),
        "none of the refused commands reached the vehicle"
    );

    // ---- missions ---------------------------------------------------------------
    let plan = json!({"name": "Circuit", "speed": 5, "items": [
        {"type": "takeoff", "lat": 48.0795, "lon": 11.6385, "alt": 20},
        {"type": "waypoint", "lat": 48.081, "lon": 11.641, "alt": 30},
        {"type": "land", "lat": 48.0795, "lon": 11.6385}
    ]});
    let up = ai.ok("upload_mission", json!({"plan": plan}));
    let seen = vehicle.take();
    let items: Vec<&Seen> = seen.iter().filter(|s| s.name == "MISSION_ITEM_INT").collect();
    assert_eq!(up["items"].as_u64(), Some(items.len() as u64), "{up}");
    let sent: Vec<f64> = items.iter().map(|s| s.f("command")).collect();
    assert_eq!(
        sent,
        [178.0, 22.0, 16.0, 21.0],
        "PX4: no home item in front; the plan speed, then the route"
    );
    assert!(
        commands(&seen).iter().all(|c| c.f("command") != 300.0),
        "start false does not start"
    );
    ai.ok("start_mission", json!({"items": items.len()}));
    let start = command(&vehicle.take(), 300);
    assert_eq!(
        (start.f("param1"), start.f("param2")),
        (0.0, (items.len() - 1) as f64),
        "first and last item"
    );
    ai.ok("download_mission", json!({}));
    ai.ok("clear_mission", json!({}));
    assert!(vehicle
        .take()
        .iter()
        .any(|s| s.name == "MISSION_CLEAR_ALL" || (s.name == "MISSION_COUNT" && s.f("count") == 0.0)));

    // ---- direct control -------------------------------------------------------------
    // PX4 enters OFFBOARD only while setpoints already stream, so the stream leads and the mode follows.
    ai.ok(
        "set_velocity",
        json!({"vx": 1.5, "vy": 0, "vz": -0.5, "duration_s": 1.5}),
    );
    wait_for("the velocity stream", Duration::from_secs(3), || {
        vehicle
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.name == "SET_POSITION_TARGET_LOCAL_NED")
            .count()
            >= 3
    });
    ai.ok("set_flight_mode", json!({"mode": "OFFBOARD"}));
    wait_for("hold after the stream", Duration::from_secs(5), || {
        commands(&vehicle.seen.lock().unwrap())
            .iter()
            .any(|c| c.f("command") == 176.0 && c.f("param2") == 4.0 && c.f("param3") == 3.0)
    });
    let seen = vehicle.take();
    let offboard = commands(&seen)
        .into_iter()
        .find(|c| c.f("command") == 176.0)
        .unwrap()
        .clone();
    assert_eq!(offboard.f("param2"), 6.0, "OFFBOARD is main mode 6");
    let first_setpoint = seen
        .iter()
        .position(|s| s.name == "SET_POSITION_TARGET_LOCAL_NED")
        .unwrap();
    let mode_at = seen
        .iter()
        .position(|s| s.name == "COMMAND_LONG" && s.f("command") == 176.0)
        .unwrap();
    assert!(
        first_setpoint < mode_at,
        "PX4 refuses OFFBOARD without a setpoint stream, so the setpoints lead"
    );
    let sp = &seen[first_setpoint];
    assert_eq!((sp.f("vx"), sp.f("vz"), sp.f("coordinate_frame")), (1.5, -0.5, 1.0));
    assert_eq!(sp.f("type_mask") as i64 & 0b111_000, 0, "velocity is not masked out");

    ai.ok(
        "manual_control",
        json!({"x": 0.5, "y": 0, "z": 0.5, "r": 0, "duration_s": 0.5}),
    );
    wait_for("the stick stream", Duration::from_secs(3), || {
        vehicle
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.name == "MANUAL_CONTROL")
            .count()
            >= 2
    });
    let stick = vehicle
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.name == "MANUAL_CONTROL")
        .cloned()
        .unwrap();
    assert_eq!((stick.f("x"), stick.f("z")), (500.0, 500.0));
    let stopped = ai.ok("stop_control", json!({}));
    assert_eq!(stopped["ok"], true);
    let why = ai.refused("set_velocity", json!({"vx": 1, "duration_s": 60}));
    assert!(why.contains("duration_s"), "{why}");
    vehicle.take();

    // ---- one vehicle unless the operator opens the fleet ---------------------------------
    let why = ai.refused("arm", json!({"vehicle": "main:1"}));
    assert!(why.contains("Leave vehicle out"), "{why}");

    // ---- a confirmation: asked in a window, answered by the operator ---------------------
    let svc = core.service::<McpService>().unwrap();
    let events: Arc<Mutex<Vec<Json>>> = Arc::default();
    let sink = Arc::clone(&events);
    svc.add_listener(Arc::new(move |e: &Json| sink.lock().unwrap().push(e.clone())));
    http(
        addr,
        "POST",
        "/api/mcp/settings",
        "",
        Some(&json!({"confirm": {"arm": true}})),
    );
    let why = ai.refused("arm", json!({}));
    assert!(why.contains("no Corvus window is open"), "{why}");
    let tools = json!([{"plugin": "schwalby", "plugin_name": "Schwalby", "name": "press_button", "title": "Press a button",
        "description": "Presses one of the operator's buttons.",
        "input_schema": {"type": "object", "properties": {"label": {"type": "string"}}, "required": ["label"]}}]);
    let (status, _, body) = http(
        addr,
        "POST",
        "/api/mcp/window",
        "",
        Some(&json!({"id": "w1", "tools": tools})),
    );
    assert_eq!((status, &body["tools"]), (200, &json!(1)), "{body}");
    for (allow, armed) in [(false, false), (true, true)] {
        events.lock().unwrap().clear();
        let ev = Arc::clone(&events);
        let operator = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                let id = ev
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|e| e["type"] == "confirm")
                    .map(|e| e["id"].clone());
                if let Some(id) = id {
                    return http(
                        addr,
                        "POST",
                        "/api/mcp/confirm",
                        "",
                        Some(&json!({"id": id, "allow": allow})),
                    )
                    .0;
                }
                assert!(Instant::now() < end, "no question reached the window");
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let (is_error, answer) = ai.call("arm", json!({}));
        assert_eq!(operator.join().unwrap(), 200);
        assert_eq!(is_error, !allow, "{answer}");
        assert_eq!(commands(&vehicle.take()).iter().any(|c| c.f("command") == 400.0), armed);
    }
    http(
        addr,
        "POST",
        "/api/mcp/settings",
        "",
        Some(&json!({"confirm": {"arm": false}})),
    );
    ai.ok("disarm", json!({}));

    // ---- a plugin tool: out to the window, back to the client -------------------------------
    let names = listed(&mut ai);
    assert!(names.contains(&"schwalby_press_button".to_string()), "{names:?}");
    for (ok, result) in [(true, json!({"pressed": "Lights"})), (false, json!(null))] {
        events.lock().unwrap().clear();
        let ev = Arc::clone(&events);
        let window = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                let call = ev.lock().unwrap().iter().find(|e| e["type"] == "call").cloned();
                if let Some(call) = call {
                    let body =
                        json!({"id": call["id"], "ok": ok, "result": result, "error": "No button has that label."});
                    http(addr, "POST", "/api/mcp/result", "", Some(&body));
                    return call;
                }
                assert!(Instant::now() < end, "no call reached the window");
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let (is_error, answer) = ai.call("schwalby_press_button", json!({"label": "Lights"}));
        let call = window.join().unwrap();
        assert_eq!(
            (call["window"].as_str(), call["plugin"].as_str(), call["tool"].as_str()),
            (Some("w1"), Some("schwalby"), Some("press_button"))
        );
        assert_eq!(call["args"], json!({"label": "Lights"}));
        assert_eq!(is_error, !ok);
        if ok {
            assert_eq!(answer, json!({"pressed": "Lights"}));
        } else {
            assert_eq!(answer, json!("No button has that label."));
        }
    }
    http(
        addr,
        "POST",
        "/api/mcp/settings",
        "",
        Some(&json!({"plugins": {"schwalby": false}})),
    );
    assert!(
        !listed(&mut ai).contains(&"schwalby_press_button".to_string()),
        "the plugin's own switch hides its tools"
    );
    let why = ai.refused("schwalby_press_button", json!({"label": "Lights"}));
    assert!(why.contains("not allowed"), "{why}");

    // ---- Stop takes every permission that changes anything away ------------------------------
    vehicle.take();
    let (status, _, _) = http(addr, "POST", "/api/mcp/stop", "", Some(&json!({})));
    assert_eq!(status, 200);
    let mut left = listed(&mut ai);
    left.retain(|n| !corvus_core::mcp::tools::is_extension(n));
    assert_eq!(left.len(), 8);
    ai.refused("arm", json!({}));
    ai.refused("send_command", json!({"command": "DO_CHANGE_SPEED", "params": [1, 5]}));
    assert!(commands(&vehicle.take())
        .iter()
        .all(|c| ![400.0, 178.0].contains(&c.f("command"))));

    let began = Instant::now();
    core.stop();
    assert!(
        began.elapsed() < Duration::from_secs(8),
        "stop joined everything: {:?}",
        began.elapsed()
    );
    drop(vehicle);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A core with the map drawings, a scripted PX4 on it, and the AI Chat's way in.
struct Station {
    core: corvus_core::Core,
    addr: SocketAddr,
    vehicle: Vehicle,
    dir: std::path::PathBuf,
}

impl Station {
    fn start(tag: &str) -> Station {
        let port = free_port();
        let dir = std::env::temp_dir().join(format!("corvus-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            json!({"mavlink_connection": format!("udp:127.0.0.1:{port}"), "tlog_dir": dir.join("tlogs")}).to_string(),
        )
        .unwrap();
        let core = CoreBuilder::new(CoreOptions::in_dir(&dir))
            .module(VehicleModule::new())
            .module(corvus_core::PluginsModule::new())
            .module(corvus_core::McpModule::new())
            .module(corvus_core::DrawingsModule::new())
            .module(corvus_core::SetupSchemasModule::new())
            .module(Http::new(HttpOptions {
                port: 0,
                ..HttpOptions::default()
            }))
            .build()
            .unwrap();
        core.start().unwrap();
        let addr = core.service::<HttpInfo>().unwrap().addr().unwrap();
        core.service::<VehicleService>().unwrap().active().bridge.start();
        let vehicle = Vehicle::start(port);
        wait_for("the vehicle", Duration::from_secs(15), || {
            core.store().get("connected").is_some_and(|v| v == true)
        });
        wait_for("the position", Duration::from_secs(5), || {
            core.store().get("position").is_some_and(|v| v[1] != 0.0)
        });
        Station {
            core,
            addr,
            vehicle,
            dir,
        }
    }

    /// Signs in with `token`, as AI Chat does.
    fn sign_in(&self, token: &str) -> Client {
        let auth = format!("Authorization: Bearer {token}\r\n");
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "Corvus AI Chat"}}});
        let (status, headers, body) = http(self.addr, "POST", "/mcp", &auth, Some(&init));
        assert_eq!(status, 200, "{body}");
        let session = headers.iter().find(|(k, _)| k == "mcp-session-id").unwrap().1.clone();
        Client {
            addr: self.addr,
            auth: format!("{auth}Mcp-Session-Id: {session}\r\n"),
            next: 1,
        }
    }

    fn chat_token(&self) -> String {
        let (_, _, card) = http(self.addr, "GET", "/api/mcp", "", None);
        card["assistant"]["token"].as_str().unwrap().to_string()
    }

    fn set(&self, patch: Json) {
        let (status, _, body) = http(self.addr, "POST", "/api/mcp/settings", "", Some(&patch));
        assert_eq!(status, 200, "{body}");
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        self.core.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn all_allowed() -> Json {
    let ids = [
        "params_write",
        "mission",
        "arm",
        "flight",
        "goto",
        "control",
        "command",
        "geofence",
        "camera",
    ];
    let on: serde_json::Map<String, Json> = ids.iter().map(|i| (i.to_string(), json!(true))).collect();
    let quiet: serde_json::Map<String, Json> = ids.iter().map(|i| (i.to_string(), json!(false))).collect();
    json!({"permissions": on, "confirm": quiet})
}

#[test]
fn the_chat_signs_in_by_itself_and_is_asked_for_what_is_not_allowed() {
    let st = Station::start("chat");
    let mut chat = st.sign_in(&st.chat_token());
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    let (status, _, _) = http(st.addr, "POST", "/mcp", "Authorization: Bearer nope\r\n", Some(&ping));
    assert_eq!(status, 403, "the server for other programs stays off");
    assert_eq!(chat.ok("get_vehicle_state", json!({}))["connected"], true);
    let (_, body) = chat.rpc("tools/list", json!({}));
    let offered = body["result"]["tools"].as_array().unwrap().len();
    assert_eq!(
        offered,
        corvus_core::mcp::tools::builtin().len(),
        "everything is offered, what is off is asked for"
    );

    // With no window to ask in, nothing goes out.
    let why = chat.refused("arm", json!({}));
    assert!(why.contains("no Corvus window is open"), "{why}");
    assert!(commands(&st.vehicle.take()).iter().all(|c| c.f("command") != 400.0));

    // With one, the operator's answer decides.
    let svc = st.core.service::<McpService>().unwrap();
    for (allow, reached) in [(false, 0), (true, 1)] {
        svc.window_heartbeat(&json!("w1"), &json!([])).unwrap();
        st.vehicle.take();
        let mut c = Client {
            addr: st.addr,
            auth: chat.auth.clone(),
            next: 50,
        };
        let t = std::thread::spawn(move || c.call("arm", json!({})));
        wait_for("the question", Duration::from_secs(5), || {
            !svc.status()["pending"].as_array().unwrap().is_empty()
        });
        let q = svc.status()["pending"][0].clone();
        assert_eq!(
            (q["client"].as_str(), q["summary"].as_str()),
            (Some("Corvus AI Chat"), Some("arm"))
        );
        svc.answer(&q["id"], &json!(allow));
        let (is_error, _) = t.join().unwrap();
        assert_eq!(is_error, !allow);
        let arms = commands(&st.vehicle.take())
            .iter()
            .filter(|c| c.f("command") == 400.0)
            .count();
        assert_eq!(arms, reached);
    }

    // The sessions of the two sides are not interchangeable.
    st.set(json!({"enabled": true}));
    let (_, _, card) = http(st.addr, "GET", "/api/mcp", "", None);
    let session = chat.auth.split("Mcp-Session-Id: ").nth(1).unwrap();
    let mixed = format!(
        "Authorization: Bearer {}\r\nMcp-Session-Id: {session}",
        card["token"].as_str().unwrap()
    );
    let (status, _, _) = http(st.addr, "POST", "/mcp", &mixed, Some(&ping));
    assert_eq!(status, 404, "another program cannot borrow the chat's session");
    let mut other = st.sign_in(card["token"].as_str().unwrap());
    let why = other.refused("arm", json!({}));
    assert!(why.contains("not allowed"), "asking is the chat's alone: {why}");
    st.set(json!({"enabled": false}));
    assert_eq!(chat.ok("get_vehicle_state", json!({}))["connected"], true);

    // Stop takes the asking away, and switching the chat off shuts it out.
    let (status, _, _) = http(st.addr, "POST", "/api/mcp/stop", "", Some(&json!({})));
    assert_eq!(status, 200);
    let why = chat.refused("arm", json!({}));
    assert!(why.contains("not allowed"), "{why}");
    assert_eq!(
        chat.ok("get_vehicle_state", json!({}))["connected"],
        true,
        "reading stays"
    );
    st.set(json!({"assistant": false}));
    let (status, _, _) = http(st.addr, "POST", "/mcp", &chat.auth, Some(&ping));
    assert_eq!(status, 403);
}

#[test]
fn closing_does_not_wait_for_an_unanswered_question() {
    let st = Station::start("closing");
    let chat = st.sign_in(&st.chat_token());
    let svc = st.core.service::<McpService>().unwrap();
    svc.window_heartbeat(&json!("w1"), &json!([])).unwrap();
    let mut c = Client {
        addr: st.addr,
        auth: chat.auth.clone(),
        next: 9,
    };
    let t = std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.call("arm", json!({}))));
    });
    wait_for("the question", Duration::from_secs(5), || {
        !svc.status()["pending"].as_array().unwrap().is_empty()
    });
    let started = Instant::now();
    st.core.stop();
    assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    let _ = t.join();
}

#[test]
fn a_question_goes_with_the_chat_that_asked_it() {
    let st = Station::start("gone");
    let chat = st.sign_in(&st.chat_token());
    let svc = st.core.service::<McpService>().unwrap();
    svc.window_heartbeat(&json!("w1"), &json!([])).unwrap();
    let mut c = Client {
        addr: st.addr,
        auth: chat.auth.clone(),
        next: 9,
    };
    let t = std::thread::spawn(move || c.call("arm", json!({})));
    wait_for("the question", Duration::from_secs(5), || {
        !svc.status()["pending"].as_array().unwrap().is_empty()
    });
    let id = svc.status()["pending"][0]["id"].clone();
    // Stop in the chat ends the session while the dialog is still open.
    let (status, _, _) = http(st.addr, "DELETE", "/mcp", &chat.auth, None);
    assert_eq!(status, 200);
    assert!(!svc.answer(&id, &json!(true)), "Allow comes too late");
    let (is_error, why) = t.join().unwrap();
    assert!(is_error, "{why}");
    assert!(commands(&st.vehicle.take()).iter().all(|c| c.f("command") != 400.0));
}

#[test]
fn the_chat_draws_measures_and_shows() {
    let st = Station::start("draw");
    let mut ai = st.sign_in(&st.chat_token());
    let ids = ai.ok(
        "draw_on_map",
        json!({"shapes": [
            {"kind": "arrow", "id": "way", "points": [{"from": "vehicle"}, {"from": "vehicle", "forward_m": 200}], "color": "red"},
            {"kind": "circle", "from": "home", "radius_m": 50, "text": "Keep out"},
            {"kind": "marker", "lat": 48.08, "lon": 11.64, "icon": "flag"}
        ]}),
    );
    assert_eq!(ids["ids"].as_array().unwrap().len(), 3);
    let drawn = ai.ok("list_drawings", json!({}));
    let way = &drawn["shapes"][0];
    assert_eq!(way["color"], "#FF5A52");
    let (a, b) = (&way["points"][0], &way["points"][1]);
    assert!((a["lat"].as_f64().unwrap() - 48.0795).abs() < 1e-6);
    assert!(
        b["lon"].as_f64().unwrap() > 11.640 && (b["lat"].as_f64().unwrap() - 48.0795).abs() < 1e-4,
        "the nose points east: {b}"
    );
    let m = ai.ok(
        "measure",
        json!({"points": [{"from": "vehicle"}, {"from": "vehicle", "north_m": 100}]}),
    );
    assert!((m["total_m"].as_f64().unwrap() - 100.0).abs() < 0.5, "{m}");
    let p = ai.ok(
        "resolve_place",
        json!({"place": {"from": "home", "bearing_deg": 90, "distance_m": 500}}),
    );
    assert!(
        (p["from_vehicle"]["distance_m"].as_f64().unwrap() - 500.0).abs() < 1.0,
        "{p}"
    );
    ai.ok("show_on_map", json!({"ids": ["way"]}));
    ai.refused("show_on_map", json!({"ids": ["nope"]}));
    ai.refused(
        "draw_on_map",
        json!({"shapes": [{"kind": "line", "points": [{"from": "moon"}]}]}),
    );
    assert_eq!(ai.ok("remove_drawings", json!({"ids": ["way"]}))["removed"], 1);
    assert_eq!(ai.ok("remove_drawings", json!({"all": true}))["removed"], 2);
    let said: Arc<Mutex<Vec<Json>>> = Arc::default();
    let sink = said.clone();
    st.core
        .service::<McpService>()
        .unwrap()
        .add_listener(Arc::new(move |e: &Json| sink.lock().unwrap().push(e.clone())));
    ai.ok("notify_operator", json!({"text": "Look left", "level": "warning"}));
    assert!(said
        .lock()
        .unwrap()
        .contains(&json!({"type": "say", "text": "Look left", "level": "warning"})));
    let seen = st.vehicle.take();
    assert!(
        commands(&seen)
            .iter()
            .all(|c| ![192.0, 16.0, 300.0].contains(&c.f("command")))
            && seen.iter().all(|s| s.name != "MISSION_ITEM_INT"),
        "a drawing never reaches the vehicle"
    );
}

#[test]
fn the_chat_steers_relative_to_the_vehicle() {
    let st = Station::start("steer");
    st.set(all_allowed());
    let mut ai = st.sign_in(&st.chat_token());
    let why = ai.refused("move_by", json!({"north_m": 10}));
    assert!(why.contains("not flying"), "{why}");
    ai.refused("orbit", json!({"center": {"from": "vehicle"}, "radius_m": 20}));
    let seen = st.vehicle.take();
    assert!(
        commands(&seen)
            .iter()
            .all(|c| ![34.0, 192.0, 400.0].contains(&c.f("command"))),
        "a move on the ground starts nothing"
    );
    ai.ok("takeoff", json!({"altitude": 25}));
    let up = ai.ok("wait_for", json!({"until": "airborne", "timeout_s": 10}));
    assert_eq!(up["reached"], true, "{up}");
    let late = ai.ok("wait_for", json!({"until": "altitude", "altitude": 90, "timeout_s": 1}));
    assert_eq!(late["reached"], false, "{late}");
    wait_for("the height", Duration::from_secs(5), || {
        ai.ok("get_vehicle_state", json!({}))["altitude_agl"].as_f64().unwrap() > 20.0
    });
    st.vehicle.take();

    let moved = ai.ok("move_by", json!({"forward_m": 40, "up_m": 5}));
    let go = command(&st.vehicle.take(), 192);
    assert!((go.f("y") / 1e7 - moved["lon"].as_f64().unwrap()).abs() < 1e-6);
    assert!(go.f("y") / 1e7 > 11.6389, "forward is east: {go:?}");
    assert!((go.f("z") - (HOME_AMSL_M + 30.0)).abs() < 0.6, "{go:?}");

    ai.ok("change_altitude", json!({"altitude": 40}));
    assert!((command(&st.vehicle.take(), 192).f("z") - (HOME_AMSL_M + 40.0)).abs() < 0.6);
    ai.refused("change_altitude", json!({"altitude": 4000}));
    st.vehicle.take();

    ai.ok("set_speed", json!({"speed": 6}));
    let speed = command(&st.vehicle.take(), 178);
    assert_eq!((speed.f("param1"), speed.f("param2")), (1.0, 6.0));

    ai.ok("set_heading", json!({"heading": 270}));
    let turn = command(&st.vehicle.take(), 192);
    assert!(
        (turn.f("param4") + std::f64::consts::FRAC_PI_2).abs() < 1e-3,
        "west, in radians: {turn:?}"
    );
    assert!((turn.f("x") / 1e7 - 48.0795).abs() < 1e-6, "and it stays where it is");
    ai.ok("face_place", json!({"place": {"from": "vehicle", "north_m": 100}}));
    assert!(command(&st.vehicle.take(), 192).f("param4").abs() < 1e-3);

    ai.ok("orbit", json!({"center": {"from": "vehicle", "north_m": 30}, "radius_m": 30, "speed": 3, "clockwise": false, "turns": 2}));
    let orbit = command(&st.vehicle.take(), 34);
    assert_eq!((orbit.f("param1"), orbit.f("param2")), (-30.0, 3.0));
    assert!((orbit.f("param4") - 2.0 * std::f64::consts::TAU).abs() < 1e-3);

    ai.ok("look_at", json!({"place": {"lat": 48.08, "lon": 11.64}}));
    let roi = command(&st.vehicle.take(), 195);
    assert!((roi.f("x") / 1e7 - 48.08).abs() < 1e-6, "{roi:?}");
    ai.call("stop_looking", json!({}));
    ai.ok("take_photo", json!({}));
    assert_eq!(command(&st.vehicle.take(), 203).f("param5"), 1.0);
    ai.ok("set_photo_distance", json!({"distance_m": 15}));
    assert_eq!(command(&st.vehicle.take(), 206).f("param1"), 15.0);
    ai.ok("point_gimbal", json!({"pitch_deg": -90, "yaw_frame": "north"}));
    let g = command(&st.vehicle.take(), 1000);
    assert_eq!((g.f("param1"), g.f("param5")), (-90.0, 28.0));
    ai.refused("point_gimbal", json!({"pitch_deg": -120}));
    ai.ok("set_mission_item", json!({"item": 3}));
    assert_eq!(command(&st.vehicle.take(), 224).f("param1"), 3.0);
    ai.ok("resume_mission", json!({}));
    let mode = command(&st.vehicle.take(), 176);
    assert_eq!((mode.f("param2"), mode.f("param3")), (4.0, 4.0), "AUTO MISSION");
}

#[test]
fn the_chat_plans_patterns_and_flies_drawings() {
    let st = Station::start("pattern");
    st.set(all_allowed());
    let mut ai = st.sign_in(&st.chat_token());
    let area = json!([
        {"from": "home", "north_m": 50, "east_m": 0},
        {"from": "home", "north_m": 250, "east_m": 0},
        {"from": "home", "north_m": 250, "east_m": 100},
        {"from": "home", "north_m": 50, "east_m": 100}
    ]);
    let grid = json!({"pattern": "grid", "points": area, "spacing_m": 25, "altitude": 40, "speed": 5});
    let plan = ai.ok("plan_pattern", grid.clone());
    assert_eq!(plan["points"], 8, "{plan}");
    let seen = st.vehicle.take();
    assert!(
        seen.iter().all(|s| s.name != "MISSION_COUNT"),
        "a preview sends nothing"
    );
    let drawn = ai.ok("list_drawings", json!({}));
    let ids: Vec<&str> = drawn["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["pattern-area", "pattern-path", "pattern-start"]);

    let up = ai.ok("fly_pattern", grid);
    assert_eq!(up["started"], false, "{up}");
    let sent: Vec<f64> = st
        .vehicle
        .take()
        .iter()
        .filter(|s| s.name == "MISSION_ITEM_INT")
        .map(|s| s.f("command"))
        .collect();
    assert_eq!(sent[..2], [178.0, 22.0], "the speed, then the takeoff");
    assert_eq!(sent.len(), 11);
    assert_eq!(sent[10], 20.0, "and home at the end");
    let why = ai.refused(
        "plan_pattern",
        json!({"pattern": "grid", "points": area, "spacing_m": 1, "angle_deg": 90, "altitude": 40}),
    );
    assert!(why.contains("spacing_m"), "{why}");
    ai.ok(
        "plan_pattern",
        json!({"pattern": "spiral", "center": {"from": "home"}, "radius_m": 60, "spacing_m": 20, "altitude": 30}),
    );
    ai.ok(
        "plan_pattern",
        json!({"pattern": "circle", "center": {"from": "home"}, "radius_m": 60, "altitude": 30}),
    );

    ai.ok("draw_on_map", json!({"clear": true, "shapes": [{"kind": "arrow", "id": "leg", "points": [
        {"from": "home", "north_m": 20}, {"from": "home", "north_m": 80}, {"from": "home", "north_m": 80, "east_m": 60}]}]}));
    ai.ok("fly_drawing", json!({"id": "leg", "altitude": 20}));
    let seen = st.vehicle.take();
    let sent: Vec<f64> = seen
        .iter()
        .filter(|s| s.name == "MISSION_ITEM_INT")
        .map(|s| s.f("command"))
        .collect();
    assert_eq!(sent, [22.0, 16.0, 16.0, 16.0]);
    command(&seen, 300);
    ai.refused("fly_drawing", json!({"id": "nope", "altitude": 20}));

    // ---- the geofence and the saved missions ------------------------------
    ai.ok("set_geofence", json!({"points": area}));
    let fence = ai.ok("get_geofence", json!({}));
    assert_eq!(fence["polygon"].as_array().unwrap().len(), 4, "{fence}");
    assert!(st
        .vehicle
        .take()
        .iter()
        .any(|s| s.name == "MISSION_ITEM_INT" && s.f("command") == 5001.0));
    let crossed = json!([{"from": "home"}, {"from": "home", "north_m": 100, "east_m": 100},
                         {"from": "home", "north_m": 100}, {"from": "home", "east_m": 100}]);
    let why = ai.refused("set_geofence", json!({"points": crossed}));
    assert!(why.contains("cross"), "{why}");
    ai.ok("clear_geofence", json!({}));
    let plan = json!({"items": [{"type": "waypoint", "lat": 48.08, "lon": 11.64, "alt": 20}]});
    ai.ok("save_mission", json!({"name": "Field", "plan": plan}));
    let saved = ai.ok("list_saved_missions", json!({}));
    assert!(saved.to_string().contains("Field"), "{saved}");
    assert_eq!(
        ai.ok("get_saved_mission", json!({"name": "Field"}))["plan"]["items"][0]["alt"],
        20.0
    );
}
