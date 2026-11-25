//! Every plugin of this repository, built as the store installs it, doing its own
//! work against the whole backend and a simulated PX4 over real UDP. No window,
//! no hardware: the vehicle and the language model here are scripts.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::link::plain::connection::{open, Options};
use corvus_core::link::plain::defs::Wire;
use corvus_core::link::plain::{Message, Value as Field};
use eframe::egui;
use serde_json::{json, Value};

use super::wasm::{runtime, testlib, REFUSED};
use super::Registry;
use crate::net::http::{self, Server};

/// Every plugin of `plugins/`.
const ALL: [(&str, &str); 8] = [
    ("ai-chat", "corvus_plugin_ai_chat"),
    ("map-entities", "corvus_plugin_map_entities"),
    ("nuttx-console", "corvus_plugin_nuttx_console"),
    ("remote-id-check", "corvus_plugin_remote_id_check"),
    ("schwalby", "corvus_plugin_schwalby"),
    ("tak", "corvus_plugin_tak"),
    ("trajectory-viewer", "corvus_plugin_trajectory_viewer"),
    ("vibration", "corvus_plugin_vibration"),
];

#[derive(Default)]
struct Heard {
    /// The lines that reached the shell.
    shell: Vec<String>,
    /// The intervals asked for VIBRATION, in microseconds.
    vibration: Vec<f32>,
}

/// A PX4 with an NSH that answers each line with one of its own.
struct Vehicle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    heard: Arc<Mutex<Heard>>,
}

fn int(v: i64) -> Field {
    Field::Int(i128::from(v))
}

fn message(name: &str) -> Message {
    Message::named(name, Wire::V2).expect("a message of the dialect")
}

fn shell_reply(text: &str) -> Vec<Message> {
    text.as_bytes()
        .chunks(70)
        .map(|chunk| {
            let mut data: Vec<Field> = chunk.iter().map(|b| int(i64::from(*b))).collect();
            data.resize(70, int(0));
            message("SERIAL_CONTROL")
                .with("device", int(10))
                .with("flags", int(1))
                .with("count", int(chunk.len() as i64))
                .with("data", Field::Array(data))
        })
        .collect()
}

impl Vehicle {
    fn start(port: u16) -> Vehicle {
        let stop = Arc::new(AtomicBool::new(false));
        let heard = Arc::new(Mutex::new(Heard::default()));
        let (asked_to_stop, log) = (stop.clone(), heard.clone());
        let thread = std::thread::spawn(move || {
            let options = Options {
                source_system: 1,
                source_component: 1,
                input: false,
                ..Options::default()
            };
            let conn = open(&format!("udpout:127.0.0.1:{port}"), &options).expect("the vehicle's socket");
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut line = Vec::new();
            let mut vibrating = false;
            while !asked_to_stop.load(Ordering::SeqCst) {
                if last.elapsed() >= Duration::from_millis(200) {
                    last = Instant::now();
                    let _ = conn.send(
                        &message("HEARTBEAT")
                            .with("type", int(2))
                            .with("autopilot", int(12))
                            .with("base_mode", int(81))
                            .with("custom_mode", int(196_608))
                            .with("system_status", int(3))
                            .with("mavlink_version", int(3)),
                    );
                    let _ = conn.send(
                        &message("GLOBAL_POSITION_INT")
                            .with("lat", int(480_762_000))
                            .with("lon", int(116_432_000))
                            .with("alt", int(520_000)),
                    );
                    if vibrating {
                        let _ = conn.send(
                            &message("VIBRATION")
                                .with("vibration_x", Field::Float(12.5))
                                .with("vibration_y", Field::Float(7.25))
                                .with("vibration_z", Field::Float(3.5)),
                        );
                    }
                }
                let Ok(frames) = conn.recv(Duration::from_millis(30)) else {
                    break;
                };
                for r in frames {
                    let Ok(msg) = r.message else { continue };
                    let whole = |k: &str| msg.get(k).and_then(Field::as_i64).unwrap_or(0);
                    match msg.name() {
                        "SERIAL_CONTROL" => {
                            let count = whole("count").clamp(0, 70) as usize;
                            let Some(Field::Array(data)) = msg.get("data") else {
                                continue;
                            };
                            for b in data.iter().take(count).filter_map(Field::as_i64) {
                                if b != 10 && b != 13 {
                                    line.push(b as u8);
                                    continue;
                                }
                                let typed = String::from_utf8_lossy(&line).into_owned();
                                line.clear();
                                if typed.is_empty() {
                                    continue;
                                }
                                log.lock().unwrap().shell.push(typed.clone());
                                for reply in shell_reply(&format!("nsh> {typed}\r\nsimulated: {typed}\r\nnsh> ")) {
                                    let _ = conn.send(&reply);
                                }
                            }
                        }
                        "COMMAND_LONG" => {
                            let command = whole("command");
                            let number = |k: &str| msg.get(k).and_then(Field::as_f64).unwrap_or(0.0) as f32;
                            if command == 511 && number("param1") == 241.0 {
                                let interval = number("param2");
                                vibrating = interval > 0.0;
                                log.lock().unwrap().vibration.push(interval);
                            }
                            let _ = conn.send(
                                &message("COMMAND_ACK")
                                    .with("command", int(command))
                                    .with("result", int(0)),
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
            heard,
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

/// The backend as the program starts it, in a folder of its own, with the
/// simulated vehicle connected.
struct Station {
    core: corvus_core::Core,
    server: Server,
    vehicle: Vehicle,
    dir: PathBuf,
}

impl Station {
    fn start(tag: &str) -> Station {
        let dir = testlib::empty(&format!("e2e-{tag}"));
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        std::fs::write(
            dir.join("config.json"),
            json!({"mavlink_connection": format!("udp:127.0.0.1:{port}"), "tlog_dir": dir.join("tlogs")}).to_string(),
        )
        .unwrap();
        let core = corvus_core::CoreBuilder::new(corvus_core::Options::in_dir(&dir))
            .module(corvus_core::VehicleModule::with_options(corvus_core::VehicleOptions {
                autostart: true,
                connection: None,
            }))
            .module(corvus_core::PeripheralsModule::new())
            .module(corvus_core::LogsModule::new())
            .module(corvus_core::FirmwareModule::new())
            .module(corvus_core::PluginsModule::new())
            .module(corvus_core::McpModule::new())
            .module(corvus_core::TakModule::new())
            .module(corvus_core::DrawingsModule::new())
            .module(corvus_core::PlannerModule::new())
            .module(corvus_core::SetupSchemasModule::new())
            .module(corvus_core::MapDataModule::new())
            .module(corvus_core::Http::new(corvus_core::HttpOptions {
                port: 0,
                ..corvus_core::HttpOptions::default()
            }))
            .build()
            .unwrap();
        core.start().unwrap();
        let at = core
            .service::<corvus_core::HttpInfo>()
            .and_then(|i| i.addr())
            .map(|a| a.port())
            .unwrap();
        let mut server = Server::local(at);
        server.key = core
            .service::<corvus_core::ApiKey>()
            .and_then(|k| http::Key::new(k.secret()));
        let vehicle = Vehicle::start(port);
        let station = Station {
            core,
            server,
            vehicle,
            dir,
        };
        wait_for("the vehicle", || station.state()["connected"] == true);
        station
    }

    fn state(&self) -> Value {
        serde_json::to_value(&*self.core.store().snapshot()).unwrap()
    }

    fn save(&self, id: &str, settings: Value) {
        http::post_json(
            &self.server,
            "/api/plugins/settings",
            &json!({"id": id, "settings": settings}),
        )
        .unwrap();
    }

    fn get(&self, path: &str) -> Value {
        http::get_json(&self.server, path).unwrap()
    }

    /// What a terminal session has printed, read from its stream until `until`.
    #[cfg(unix)]
    fn session_output(&self, name: &str, until: &str) -> String {
        let path = format!("/api/ssh/stream?name={}", name.replace(' ', "%20"));
        let mut stream = TcpStream::connect(("127.0.0.1", self.server.port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let key = self.core.service::<corvus_core::ApiKey>().unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{}: {}\r\n\r\n",
            self.server.port,
            http::KEY_HEADER,
            key.secret()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        let (mut heard, mut buf) = (Vec::new(), [0u8; 4096]);
        while Instant::now() < end && !String::from_utf8_lossy(&heard).contains(until) {
            if let Ok(n) = stream.read(&mut buf) {
                heard.extend_from_slice(&buf[..n]);
            }
        }
        String::from_utf8_lossy(&heard).into_owned()
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        self.core.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// No module of these tests was told no. The probe is told no on purpose.
fn nothing_was_refused() {
    let refused: Vec<String> = REFUSED
        .lock()
        .unwrap()
        .iter()
        .filter(|r| ALL.iter().any(|(id, _)| r.starts_with(&format!("{id}: "))))
        .cloned()
        .collect();
    assert_eq!(refused, Vec::<String>::new());
}

/// The registry with `ids` installed, in a window of its own.
struct Window {
    ctx: egui::Context,
    reg: Registry,
    /// The window's half of MCP, when a test connects it.
    mcp: Option<crate::mcp::Mcp>,
    time: f64,
    calls: u32,
}

impl Window {
    fn new(station: &Station, ids: &[&str]) -> Window {
        let folder = station.dir.join("plugins-wasm");
        for (id, krate) in ALL.iter().filter(|(id, _)| ids.contains(id)) {
            testlib::package(&folder, id, krate);
        }
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::fonts::install(&ctx);
        crate::ui::style::apply(&ctx);
        let mut reg = Registry::with_packages(
            &testlib::sandbox(),
            station.server.clone(),
            &ctx,
            &json!({}),
            Vec::new(),
            &folder,
        );
        reg.finish_preparing();
        assert_eq!(reg.refused, []);
        Window {
            ctx,
            reg,
            mcp: None,
            time: 0.0,
            calls: 0,
        }
    }

    fn connect_mcp(&mut self, station: &Station) {
        self.mcp = Some(crate::mcp::Mcp::start(station.server.clone(), &self.ctx));
    }

    fn frame(&mut self, station: &Station) {
        self.input(station, Vec::new());
    }

    fn click(&mut self, station: &Station, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.input(station, vec![egui::Event::PointerMoved(at)]);
        self.input(station, vec![egui::Event::PointerMoved(at)]);
        self.input(station, vec![button(true)]);
        self.input(station, vec![button(false)]);
        for _ in 0..3 {
            self.frame(station);
        }
    }

    fn type_line(&mut self, station: &Station, text: &str) {
        self.input(station, vec![egui::Event::Text(text.into())]);
        self.frame(station);
        for pressed in [true, false] {
            let enter = egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            self.input(station, vec![enter]);
        }
    }

    fn input(&mut self, station: &Station, events: Vec<egui::Event>) {
        let state = station.state();
        self.reg.update(&self.ctx, &state);
        // As the program does it each frame (`app`).
        if let Some(mcp) = &mut self.mcp {
            mcp.set_tools(self.reg.mcp_tools(&self.ctx, &state));
            for (id, plugin, tool, args) in mcp.take_calls() {
                self.reg.mcp_call(&id, &plugin, &tool, &args, &self.ctx, &state);
            }
            for (id, result) in self.reg.mcp_answers(&self.ctx, &state) {
                mcp.reply(&id, result);
            }
        }
        self.time += 1.0 / 60.0;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))),
            time: Some(self.time),
            events,
            ..egui::RawInput::default()
        };
        let ctx = self.ctx.clone();
        let (reg, mcp) = (&mut self.reg, &mut self.mcp);
        let mut out = ctx.run_ui(raw, |ui| {
            reg.show_cards(ui, testlib::AREA, &state);
            if let Some(mcp) = mcp {
                mcp.show(ui.ctx(), None, 0.0);
            }
        });
        out.textures_delta.set.clear();
        out.textures_delta.free.clear();
        std::thread::sleep(Duration::from_millis(5));
    }

    fn pump(&mut self, station: &Station, what: &str, mut done: impl FnMut(&mut Window) -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            self.frame(station);
            if done(self) {
                return;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}");
        }
    }

    fn run_for(&mut self, station: &Station, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            self.frame(station);
        }
    }

    fn index(&self, id: &str) -> usize {
        self.reg.index_of(id).unwrap_or_else(|| panic!("{id} is not there"))
    }

    fn allow(&mut self, station: &Station, id: &str, option: &str) {
        let (i, state) = (self.index(id), station.state());
        self.reg.set_option(&self.ctx, &state, i, option, &json!(true)).unwrap();
    }

    fn tools(&mut self, station: &Station, id: &str) -> Vec<String> {
        let state = station.state();
        self.reg
            .mcp_tools(&self.ctx, &state)
            .iter()
            .filter(|t| t["plugin"] == id)
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect()
    }

    /// An AI client's call, to its answer.
    fn call(&mut self, station: &Station, id: &str, tool: &str, args: Value) -> Result<Value, String> {
        self.calls += 1;
        let call = format!("call-{}", self.calls);
        let state = station.state();
        self.reg.mcp_call(&call, id, tool, &args, &self.ctx, &state);
        let mut answer = None;
        self.pump(station, &format!("the answer of {id} {tool}"), |w| {
            let state = station.state();
            for (of, result) in w.reg.mcp_answers(&w.ctx, &state) {
                if of == call {
                    answer = Some(result);
                }
            }
            answer.is_some()
        });
        answer.expect("pumped until it came")
    }
}

#[test]
fn every_plugin_opens_on_the_whole_station_and_asks_for_nothing_it_may_not_have() {
    let station = Station::start("all");
    let ids: Vec<&str> = ALL.iter().map(|(id, _)| *id).collect();
    let mut w = Window::new(&station, &ids);
    assert_eq!(w.reg.len(), ALL.len());
    w.run_for(&station, Duration::from_millis(500));
    for id in &ids {
        let (i, state) = (w.index(id), station.state());
        w.reg.open_card(i, &w.ctx, &state);
        w.run_for(&station, Duration::from_millis(900));
        assert_eq!(w.reg.failure(i), None, "{id}");
        let state = station.state();
        w.reg.close_card(&w.ctx, &state);
        w.run_for(&station, Duration::from_millis(200));
        assert_eq!(w.reg.failure(i), None, "{id} after closing");
    }
    let state = station.state();
    w.reg.shut_down(&w.ctx, &state);
    nothing_was_refused();
}

#[test]
fn the_nuttx_console_runs_a_command_in_the_vehicles_shell() {
    let station = Station::start("nuttx");
    let mut w = Window::new(&station, &["nuttx-console"]);
    w.run_for(&station, Duration::from_millis(500));
    assert_eq!(w.tools(&station, "nuttx-console"), Vec::<String>::new());
    w.allow(&station, "nuttx-console", "mcp_read");
    w.allow(&station, "nuttx-console", "mcp_run");
    assert_eq!(w.tools(&station, "nuttx-console"), ["read_shell", "run_shell"]);
    let ran = w
        .call(&station, "nuttx-console", "run_shell", json!({"command": "ver all"}))
        .unwrap();
    assert!(ran.to_string().contains("simulated: ver all"), "{ran}");
    assert!(station
        .vehicle
        .heard
        .lock()
        .unwrap()
        .shell
        .contains(&"ver all".to_string()));
    let read = w
        .call(&station, "nuttx-console", "read_shell", json!({"command": "free"}))
        .unwrap();
    assert!(read.to_string().contains("simulated: free"), "{read}");
    nothing_was_refused();
}

// The commands are a Unix shell's.
#[cfg(unix)]
#[test]
fn schwalby_runs_its_buttons_on_this_computer() {
    let station = Station::start("schwalby");
    let made = station.dir.join("pressed");
    station.save(
        "schwalby",
        json!({"buttons": [
            {"id": "bg", "label": "Touch", "target": "local", "mode": "background",
             "command": format!("touch '{}'", made.display())},
            {"id": "term", "label": "Say", "target": "local", "mode": "terminal",
             "command": "echo e2e-$((40+2))"},
        ]}),
    );
    let mut w = Window::new(&station, &["schwalby"]);
    w.run_for(&station, Duration::from_millis(800));
    w.allow(&station, "schwalby", "mcp_run");
    w.allow(&station, "schwalby", "mcp_edit");
    let list = w.call(&station, "schwalby", "list_buttons", json!({})).unwrap();
    assert_eq!(list["buttons"].as_array().map(Vec::len), Some(2), "{list}");

    w.call(&station, "schwalby", "press_button", json!({"id": "bg"}))
        .unwrap();
    w.pump(&station, "the file the button makes", |_| made.exists());

    w.call(&station, "schwalby", "press_button", json!({"id": "term"}))
        .unwrap();
    let sessions = station.get("/api/ssh/sessions");
    let name = sessions["sessions"][0]["name"]
        .as_str()
        .unwrap_or_else(|| panic!("no session: {sessions}"))
        .to_string();
    let output = station.session_output(&name, "e2e-42");
    assert!(
        output.contains("e2e-42"),
        "the command did not run in the shell: {output}"
    );
    w.call(&station, "schwalby", "stop_button", json!({"id": "term"}))
        .unwrap();
    w.pump(&station, "the session to end", |_| {
        !station.get("/api/ssh/sessions").to_string().contains(&name)
    });

    let added = w
        .call(
            &station,
            "schwalby",
            "add_button",
            json!({"label": "From the model", "target": "local", "command": "true"}),
        )
        .unwrap();
    assert_eq!(added["waiting_for_operator"], true, "{added}");
    let refused = w.call(&station, "schwalby", "press_button", json!({"id": added["id"]}));
    assert_eq!(
        refused,
        Err("The operator has not allowed this button yet.".to_string())
    );
    let kept = station.get("/api/plugins");
    assert!(
        kept["settings"]["schwalby"].to_string().contains("From the model"),
        "{kept}"
    );
    nothing_was_refused();
}

#[test]
fn map_entities_places_changes_and_removes_what_a_model_asks_for() {
    let station = Station::start("entities");
    let mut w = Window::new(&station, &["map-entities"]);
    w.run_for(&station, Duration::from_millis(500));
    w.allow(&station, "map-entities", "mcp_read");
    w.allow(&station, "map-entities", "mcp_edit");
    let _ = w.reg.take_shapes();
    let tree = w
        .call(
            &station,
            "map-entities",
            "add_entity",
            json!({"kind": "tree", "lat": 48.0762, "lon": 11.6432, "height": 20, "name": "Oak"}),
        )
        .unwrap();
    let id = tree["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id: {tree}"))
        .to_string();
    let mut drawn = Vec::new();
    w.pump(&station, "the tree on the map", |w| {
        drawn = w.reg.take_shapes().unwrap_or_default();
        !drawn.is_empty()
    });
    let kept = station.get("/api/plugins");
    assert!(kept["settings"]["map-entities"].to_string().contains("Oak"), "{kept}");
    w.call(
        &station,
        "map-entities",
        "update_entity",
        json!({"id": id, "name": "Old oak"}),
    )
    .unwrap();
    let list = w.call(&station, "map-entities", "list_entities", json!({})).unwrap();
    assert!(list.to_string().contains("Old oak"), "{list}");
    w.call(&station, "map-entities", "remove_entity", json!({"id": id}))
        .unwrap();
    let list = w.call(&station, "map-entities", "list_entities", json!({})).unwrap();
    assert!(!list.to_string().contains("Old oak"), "{list}");
    nothing_was_refused();
}

#[test]
fn the_vibration_monitor_asks_the_vehicle_for_its_stream_and_gives_it_back() {
    let station = Station::start("vibration");
    let mut w = Window::new(&station, &["vibration"]);
    w.run_for(&station, Duration::from_millis(500));
    let (i, state) = (w.index("vibration"), station.state());
    w.reg.open_card(i, &w.ctx, &state);
    let heard = station.vehicle.heard.clone();
    w.pump(&station, "the request for the stream", |_| {
        heard.lock().unwrap().vibration.iter().any(|v| *v > 0.0)
    });
    w.pump(&station, "vibration in the state", |_| {
        station.state()["vibration_x"].as_f64() == Some(12.5)
    });
    w.run_for(&station, Duration::from_millis(500));
    let before = heard.lock().unwrap().vibration.len();
    let state = station.state();
    w.reg.close_card(&w.ctx, &state);
    w.pump(&station, "the stream given back", |_| {
        heard.lock().unwrap().vibration.len() > before
    });
    assert!(heard.lock().unwrap().vibration.last().is_some_and(|v| *v <= 0.0));
    assert_eq!(w.reg.failure(i), None);
    nothing_was_refused();
}

/// One request of an HTTP client, read whole: the request line and the body.
fn read_request(stream: &mut TcpStream) -> (String, String) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let text = String::from_utf8_lossy(&request).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let want = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ")?.parse().ok())
                .unwrap_or(0);
            if body.len() >= want {
                return (head.lines().next().unwrap_or("").to_string(), body.to_string());
            }
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return (String::new(), String::new()),
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
}

/// A language model that asks for one tool, then answers. Keeps
/// what it was sent.
fn model_server(tool: &'static str, args: Value) -> (u16, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (line, body) = read_request(&mut stream);
            let reply = if line.starts_with("POST /v1/chat/completions") {
                let mut log = log.lock().unwrap();
                log.push(serde_json::from_str::<Value>(&body).unwrap_or(Value::Null));
                if log.len() == 1 {
                    json!({"choices": [{"message": {"content": "Looking.", "tool_calls": [
                        {"id": "c1", "type": "function", "function": {"name": tool, "arguments": args.to_string()}}
                    ]}}]})
                } else {
                    json!({"choices": [{"message": {"content": "Done."}}]})
                }
            } else {
                json!({"data": [{"id": "scripted"}]})
            };
            let reply = reply.to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            );
        }
    });
    (port, asked)
}

fn rig(krate: &str, server: Server) -> testlib::Rig {
    let (_, plugin) = testlib::load(krate);
    let mut rig = testlib::Rig::new(plugin);
    rig.backend.server = server;
    rig
}

fn typed(rig: &mut testlib::Rig, text: &str) {
    rig.frame(vec![egui::Event::Text(text.into())]);
    rig.frames(2);
    for pressed in [true, false] {
        rig.frame(vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
    }
}

#[test]
fn ai_chat_answers_with_the_help_of_the_stations_own_tools() {
    let station = Station::start("chat");
    let (port, asked) = model_server("get_telemetry", json!({}));
    let mut r = rig("corvus_plugin_ai_chat", station.server.clone());
    r.state = station.state();
    for (key, value) in [
        ("provider", json!("openai")),
        ("url", json!(format!("http://127.0.0.1:{port}"))),
        ("model", json!("scripted")),
        ("mcp", json!(true)),
        ("offer", json!("all")),
    ] {
        r.options.insert(key.into(), value);
    }
    r.with(|p, h| {
        use corvus_api::plugin::Plugin;
        p.start(h);
        p.options_changed(h);
        p.open(h);
    });
    for _ in 0..40 {
        r.frames(1);
        std::thread::sleep(Duration::from_millis(10));
    }
    // The field lies at the bottom of the view; the send button is right of it.
    r.click(egui::pos2(testlib::AREA.left() + 60.0, testlib::AREA.bottom() - 24.0));
    assert!(r.wants_keyboard, "the click did not reach the chat's field");
    typed(&mut r, "How is the aircraft?");
    let end = Instant::now() + Duration::from_secs(30);
    while asked.lock().unwrap().len() < 2 {
        assert!(
            Instant::now() < end,
            "the turn did not finish: {:?}",
            asked.lock().unwrap()
        );
        r.frames(1);
        std::thread::sleep(Duration::from_millis(10));
    }
    let asked = asked.lock().unwrap();
    let first = &asked[0];
    assert!(first.to_string().contains("How is the aircraft?"), "{first}");
    assert!(first["tools"].to_string().contains("get_telemetry"), "{first}");
    let answered = asked[1]["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "tool"))
        .unwrap_or_else(|| panic!("no answer of the tool: {}", asked[1]));
    let content = answered["content"].to_string();
    assert!(content.contains("POSCTL") && content.contains("48.0762"), "{content}");
    nothing_was_refused();
}

#[test]
fn the_trajectory_viewer_reads_the_file_the_operator_picks_and_draws_it() {
    let station = Station::start("trajectory");
    let file = station.dir.join("path.csv");
    std::fs::write(&file, "x,y,z\n0,0,0\n10,0,2\n10,10,4\n0,10,6\n").unwrap();
    let mut r = rig("corvus_plugin_trajectory_viewer", station.server.clone());
    r.state = station.state();
    r.with(|p, h| {
        use corvus_api::plugin::Plugin;
        p.start(h);
        p.open(h);
    });
    r.frames(20);
    runtime::TEST_PICKS.lock().unwrap().push(file);
    // Down the view until the button that asks for a file is under the pointer.
    let mut y = testlib::AREA.top() + 8.0;
    while !runtime::TEST_PICKS.lock().unwrap().is_empty() {
        assert!(y < testlib::AREA.bottom(), "no button asked for a file");
        r.click(egui::pos2(testlib::AREA.left() + 60.0, y));
        y += 14.0;
    }
    r.frames(20);
    assert_eq!(r.saved["file"], "path.csv", "{}", r.saved);
    // Further down: the switch that puts the line on the Home map.
    while r.shapes.shapes().is_empty() {
        assert!(y < testlib::AREA.bottom(), "nothing was drawn: {}", r.saved);
        r.click(egui::pos2(testlib::AREA.right() - 19.0, y));
        y += 14.0;
    }
    r.frames(10);
    assert_eq!(r.saved["drawn"], true, "{}", r.saved);
    nothing_was_refused();
}

// ---- MCP: from an AI client through the server and the window to a plugin ----

/// One request of an MCP client: the status, the session id and the JSON answer.
fn mcp_http(port: u16, token: &str, session: &str, body: &Value) -> (u16, String, Value) {
    let body = body.to_string();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    let session_line = if session.is_empty() {
        String::new()
    } else {
        format!("Mcp-Session-Id: {session}\r\n")
    };
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n{session_line}\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer);
    let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let session = head
        .lines()
        .find_map(|l| l.strip_prefix("Mcp-Session-Id: "))
        .unwrap_or("")
        .to_string();
    (status, session, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// A program of someone else's that talks to `/mcp`, as an AI client does.
struct Client {
    port: u16,
    token: String,
    session: String,
}

impl Client {
    /// A request, while the window runs: a plugin's tool is answered by it.
    fn rpc(&self, w: &mut Window, station: &Station, method: &str, params: Value) -> Value {
        let (port, token, session) = (self.port, self.token.clone(), self.session.clone());
        let body = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
        let sent = std::thread::spawn(move || mcp_http(port, &token, &session, &body));
        w.pump(station, &format!("the answer to {method}"), |_| sent.is_finished());
        let (status, _, answer) = sent.join().unwrap();
        assert_eq!(status, 200, "{method}: {answer}");
        answer
    }

    fn tools(&self, w: &mut Window, station: &Station) -> Vec<String> {
        self.rpc(w, station, "tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect()
    }

    /// A tool call: its text, and whether the station says it failed.
    fn call(&self, w: &mut Window, station: &Station, tool: &str, args: Value) -> (String, bool) {
        let answer = self.rpc(w, station, "tools/call", json!({"name": tool, "arguments": args}));
        outcome(&answer)
    }
}

fn outcome(answer: &Value) -> (String, bool) {
    let result = &answer["result"];
    assert!(!result.is_null(), "{answer}");
    let text = result["content"][0]["text"].as_str().unwrap_or("").to_string();
    (text, result["isError"] == true)
}

/// Waits until Corvus asks the operator, answers, and returns what was asked.
fn operator_allows(w: &mut Window, station: &Station) -> String {
    let mut asked = Value::Null;
    w.pump(station, "the question to the operator", |_| {
        asked = station.get("/api/mcp")["status"]["pending"][0].clone();
        !asked.is_null()
    });
    http::post_json(
        &station.server,
        "/api/mcp/confirm",
        &json!({"id": asked["id"], "allow": true}),
    )
    .unwrap();
    asked.to_string()
}

#[test]
fn an_ai_client_reads_the_aircraft_and_uses_the_plugins_tools_over_mcp() {
    let station = Station::start("mcp");
    station.save(
        "schwalby",
        json!({"buttons": [{"id": "bg", "label": "Touch", "target": "local", "mode": "background",
            "command": format!("touch '{}'", station.dir.join("by-mcp").display())}]}),
    );
    let ids: Vec<&str> = ALL.iter().map(|(id, _)| *id).collect();
    let mut w = Window::new(&station, &ids);
    w.connect_mcp(&station);
    w.run_for(&station, Duration::from_millis(500));
    for (id, option) in [
        ("map-entities", "mcp_read"),
        ("map-entities", "mcp_edit"),
        ("nuttx-console", "mcp_read"),
        ("nuttx-console", "mcp_run"),
        ("schwalby", "mcp_edit"),
        ("schwalby", "mcp_run"),
        ("tak", "ai"),
    ] {
        w.allow(&station, id, option);
    }
    let post = |path: &str, body: Value| {
        http::post_json(&station.server, path, &body).unwrap();
    };
    post("/api/mcp/settings", json!({"enabled": true}));
    let mut client = Client {
        port: station.server.port,
        token: station.get("/api/mcp")["token"].as_str().unwrap().to_string(),
        session: String::new(),
    };
    let (status, session, hello) = mcp_http(
        client.port,
        &client.token,
        "",
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "1"}}}),
    );
    assert_eq!(status, 200, "{hello}");
    client.session = session;

    // The station's own tools answer with the aircraft's data.
    let (telemetry, failed) = client.call(&mut w, &station, "get_telemetry", json!({}));
    assert!(
        !failed && telemetry.contains("POSCTL") && telemetry.contains("48.0762"),
        "{telemetry}"
    );
    let (state, failed) = client.call(&mut w, &station, "get_vehicle_state", json!({}));
    assert!(!failed && state.contains("11.6432"), "{state}");

    // The plugins' tools wait for the operator's switch.
    let tools = client.tools(&mut w, &station);
    assert!(tools.iter().any(|t| t == "get_telemetry"));
    assert!(!tools.iter().any(|t| t.starts_with("map-entities_")), "{tools:?}");
    post(
        "/api/mcp/settings",
        json!({"permissions": {"plugins": true, "tak_read": true}}),
    );
    let wanted = [
        "map-entities_add_entity",
        "map-entities_list_entities",
        "nuttx-console_run_shell",
        "nuttx-console_read_shell",
        "schwalby_press_button",
        "tak_status",
        "tak_set_map_filter",
    ];
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let tools = client.tools(&mut w, &station);
        if wanted.iter().all(|n| tools.iter().any(|t| t == n)) {
            break;
        }
        assert!(Instant::now() < end, "not all tools came: {tools:?}");
    }

    let _ = w.reg.take_shapes();
    let (placed, failed) = client.call(
        &mut w,
        &station,
        "map-entities_add_entity",
        json!({"kind": "tree", "lat": 48.0765, "lon": 11.6440, "name": "Over MCP"}),
    );
    assert!(!failed && placed.contains("Over MCP"), "{placed}");
    let mut drawn = Vec::new();
    w.pump(&station, "the tree on the map", |w| {
        drawn = w.reg.take_shapes().unwrap_or_default();
        !drawn.is_empty()
    });
    let (list, _) = client.call(&mut w, &station, "map-entities_list_entities", json!({}));
    assert!(list.contains("Over MCP"), "{list}");

    let (read, failed) = client.call(&mut w, &station, "nuttx-console_read_shell", json!({"command": "free"}));
    assert!(!failed && read.contains("simulated: free"), "{read}");

    // This one the operator is asked about, each time.
    let (port, token, session) = (client.port, client.token.clone(), client.session.clone());
    let running = std::thread::spawn(move || {
        let body = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {
            "name": "nuttx-console_run_shell", "arguments": {"command": "ver all"}}});
        mcp_http(port, &token, &session, &body)
    });
    let question = operator_allows(&mut w, &station);
    assert!(question.contains("ver all"), "{question}");
    w.pump(&station, "the shell's answer", |_| running.is_finished());
    let (ran, failed) = outcome(&running.join().unwrap().2);
    assert!(!failed && ran.contains("simulated: ver all"), "{ran}");

    // The button's command is a Unix shell's.
    if cfg!(unix) {
        let (pressed, failed) = client.call(&mut w, &station, "schwalby_press_button", json!({"id": "bg"}));
        assert!(!failed, "{pressed}");
        let made = station.dir.join("by-mcp");
        w.pump(&station, "the file the button makes", |_| made.exists());
    }

    let (tak, failed) = client.call(&mut w, &station, "tak_status", json!({}));
    assert!(!failed, "{tak}");
    let (filter, failed) = client.call(
        &mut w,
        &station,
        "tak_set_map_filter",
        json!({"affiliations": ["friend"]}),
    );
    assert!(
        !failed && filter.contains("friend") && !filter.contains("hostile"),
        "{filter}"
    );

    // Stop takes the plugins away again; reading stays.
    post("/api/mcp/stop", json!({}));
    let tools = client.tools(&mut w, &station);
    assert!(!tools.iter().any(|t| t.starts_with("map-entities_")), "{tools:?}");
    let (_, failed) = client.call(&mut w, &station, "get_telemetry", json!({}));
    assert!(!failed);
    for id in ids {
        assert_eq!(w.reg.failure(w.index(id)), None, "{id}");
    }
    nothing_was_refused();
}

#[test]
fn the_ai_chat_places_an_entity_through_the_other_plugin_once_the_operator_allows_it() {
    let station = Station::start("chat-entities");
    let (port, asked) = model_server(
        "map-entities_add_entity",
        json!({"kind": "tree", "lat": 48.0766, "lon": 11.6441, "name": "From the chat"}),
    );
    let mut w = Window::new(&station, &["ai-chat", "map-entities"]);
    w.connect_mcp(&station);
    w.run_for(&station, Duration::from_millis(500));
    w.allow(&station, "map-entities", "mcp_read");
    w.allow(&station, "map-entities", "mcp_edit");
    for (key, value) in [
        ("provider", json!("openai")),
        ("url", json!(format!("http://127.0.0.1:{port}"))),
        ("model", json!("scripted")),
        ("offer", json!("all")),
    ] {
        let (i, state) = (w.index("ai-chat"), station.state());
        w.reg.set_option(&w.ctx, &state, i, key, &value).unwrap();
    }
    w.pump(&station, "the plugin's tools at the server", |_| {
        station.get("/api/mcp")["status"].to_string().contains("add_entity")
            || station.get("/api/mcp").to_string().contains("add_entity")
    });
    let (i, state) = (w.index("ai-chat"), station.state());
    w.reg.open_card(i, &w.ctx, &state);
    w.run_for(&station, Duration::from_millis(500));
    // The field lies at the bottom of the opened card.
    let mut up = 30.0;
    while !w.ctx.egui_wants_keyboard_input() {
        assert!(up < 120.0, "no click reached the chat's field");
        w.click(
            &station,
            egui::pos2(testlib::AREA.left() + 80.0, testlib::AREA.bottom() - up),
        );
        up += 12.0;
    }
    let _ = w.reg.take_shapes();
    w.type_line(&station, "Put a tree north of me.");
    let question = operator_allows(&mut w, &station);
    assert!(question.contains("From the chat"), "{question}");
    w.pump(&station, "the model's second turn", |_| {
        asked.lock().unwrap().len() >= 2
    });
    let mut drawn = Vec::new();
    w.pump(&station, "the tree on the map", |w| {
        drawn.extend(w.reg.take_shapes().unwrap_or_default());
        !drawn.is_empty()
    });
    let asked = asked.lock().unwrap();
    let answered = asked[1]["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "tool"))
        .unwrap_or_else(|| panic!("no answer of the tool: {}", asked[1]));
    assert!(answered["content"].to_string().contains("From the chat"), "{answered}");
    let kept = station.get("/api/plugins");
    assert!(
        kept["settings"]["map-entities"].to_string().contains("From the chat"),
        "{kept}"
    );
    nothing_was_refused();
}
