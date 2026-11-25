//! Real plugin modules for the tests, and a window to run one in. The modules
//! are built by cargo the first time a test asks for one.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use corvus_api::plugin::bus::Bus;
use corvus_api::plugin::shapes::ShapeSet;
use corvus_api::plugin::{Host, Outbox, Plugin};
use corvus_package::Manifest;
use eframe::egui;
use serde_json::{json, Map, Value};

use super::{Sandbox, WasmPlugin};
use crate::net::http::Server;

fn target_dir() -> PathBuf {
    // The test binary lies in `<target>/<profile>/deps`.
    let exe = std::env::current_exe().expect("the test binary");
    exe.ancestors().nth(3).expect("the target folder").to_path_buf()
}

/// The module of crate `package`, built now if the sources changed.
pub fn module(package: &str) -> Arc<Vec<u8>> {
    static BUILT: OnceLock<Mutex<BTreeMap<String, Arc<Vec<u8>>>>> = OnceLock::new();
    let built = BUILT.get_or_init(Mutex::default);
    let mut built = built.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(bytes) = built.get(package) {
        return bytes.clone();
    }
    let target = target_dir().join("plugin-tests");
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--target",
            "wasm32-wasip1",
            "--profile",
            "plugin",
            "-p",
            package,
        ])
        .arg("--target-dir")
        .arg(&target)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("cargo runs");
    assert!(status.success(), "{package} does not build for wasm32-wasip1");
    let file = target
        .join("wasm32-wasip1/plugin")
        .join(corvus_package::built_name(package));
    let bytes = Arc::new(std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display())));
    built.insert(package.to_string(), bytes.clone());
    bytes
}

/// One engine for all tests of the process.
pub fn sandbox() -> Arc<Sandbox> {
    static SANDBOX: OnceLock<Arc<Sandbox>> = OnceLock::new();
    SANDBOX
        .get_or_init(|| Sandbox::new().expect("the sandbox starts"))
        .clone()
}

pub fn load(package: &str) -> (Manifest, WasmPlugin) {
    let bytes = module(package);
    let manifest = Manifest::of_module(&bytes).expect("the module has a manifest");
    let plugin = WasmPlugin::load(&sandbox(), &manifest, &bytes, None).expect("the module loads");
    (manifest, plugin)
}

/// A backend that answers every request and keeps what it was asked.
pub struct Backend {
    pub server: Server,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Backend {
    pub fn new() -> Backend {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let server = Server::local(listener.local_addr().expect("its address").port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = stream.read(&mut buf) {
                        request.extend_from_slice(&buf[..n]);
                        if n == 0 || whole(&request) {
                            break;
                        }
                    }
                    let line = String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(" HTTP/1.1")
                        .to_string();
                    log.lock().unwrap_or_else(|p| p.into_inner()).push(line);
                    let body = r#"{"ok":true}"#;
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                });
            }
        });
        Backend { server, seen }
    }

    /// The request lines so far, without the protocol.
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

fn whole(request: &[u8]) -> bool {
    let text = String::from_utf8_lossy(request);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let want = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: ")?.parse().ok())
        .unwrap_or(0);
    body.len() >= want
}

/// Where the view of a plugin lies in the window of the tests.
pub const AREA: egui::Rect = egui::Rect {
    min: egui::Pos2 { x: 600.0, y: 100.0 },
    max: egui::Pos2 { x: 960.0, y: 700.0 },
};

/// What the registry keeps for a plugin, and a window to draw it in.
pub struct Rig {
    pub ctx: egui::Context,
    pub backend: Backend,
    pub state: Value,
    pub shapes: ShapeSet,
    pub saved: Value,
    pub options: Map<String, Value>,
    pub out: Outbox,
    pub bus: Bus,
    pub plugin: WasmPlugin,
    pub height: f32,
    /// Whether the program thought a text field had the keyboard after the last frame.
    pub wants_keyboard: bool,
    time: f64,
}

impl Rig {
    pub fn new(plugin: WasmPlugin) -> Rig {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::fonts::install(&ctx);
        crate::ui::style::apply(&ctx);
        let id = plugin.info().id;
        let mut bus = Bus::default();
        bus.set_live([id, "other"].into_iter());
        Rig {
            ctx,
            backend: Backend::new(),
            state: json!({}),
            shapes: ShapeSet::new(id),
            saved: json!({}),
            options: Map::new(),
            out: Outbox::default(),
            bus,
            plugin,
            height: 0.0,
            wants_keyboard: false,
            time: 0.0,
        }
    }

    pub fn with<R>(&mut self, f: impl FnOnce(&mut WasmPlugin, &mut Host) -> R) -> R {
        let info = self.plugin.info();
        let mut host = Host::new(
            &self.ctx,
            &self.backend.server,
            &self.state,
            info,
            &mut self.shapes,
            &mut self.saved,
            &self.options,
            &mut self.out,
            &mut self.bus,
        );
        f(&mut self.plugin, &mut host)
    }

    pub fn tick(&mut self) {
        self.with(|p, h| p.tick(h));
    }

    /// Tells the probe to do something.
    pub fn tell(&mut self, what: &str, arg: &str) {
        self.with(|p, h| p.message("test", &json!({"do": what, "arg": arg}), h));
    }

    pub fn published(&self, name: &str) -> Option<Value> {
        self.bus.read(&format!("{}/{name}", self.plugin.info().id)).cloned()
    }

    /// Ticks until the plugin has published `name`.
    pub fn await_published(&mut self, name: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.tick();
            if let Some(v) = self.published(name) {
                return v;
            }
            assert!(Instant::now() < deadline, "nothing published as {name}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// One frame of the window with these events, the plugin's view in it.
    pub fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 1.0 / 60.0;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))),
            time: Some(self.time),
            events,
            ..egui::RawInput::default()
        };
        let ctx = self.ctx.clone();
        let mut height = 0.0;
        let mut full = ctx.run_ui(raw, |root| {
            let mut ui = root.new_child(egui::UiBuilder::new().max_rect(AREA));
            ui.set_clip_rect(AREA);
            height = self.with(|p, h| p.show(&mut ui, AREA, h));
        });
        full.textures_delta.set.clear();
        full.textures_delta.free.clear();
        self.height = height;
        self.wants_keyboard = ctx.egui_wants_keyboard_input();
        self.tick();
    }

    pub fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(Vec::new());
        }
    }

    pub fn click(&mut self, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![egui::Event::PointerMoved(at)]);
        self.frame(vec![egui::Event::PointerMoved(at)]);
        self.frame(vec![button(true)]);
        self.frame(vec![button(false)]);
        self.frames(3);
    }
}

/// A fresh folder for a test.
pub fn empty(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-plugins-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder for the test");
    dir
}

/// Installs the module of crate `krate` as package `id` of `dir`, as the store does.
pub fn package(dir: &std::path::Path, id: &str, krate: &str) -> PathBuf {
    let bytes = module(krate);
    let manifest = Manifest::of_module(&bytes).expect("the module has a manifest");
    let folder = dir.join(id);
    std::fs::create_dir_all(&folder).expect("its folder");
    std::fs::write(folder.join("module.wasm"), bytes.as_slice()).expect("its module");
    let said = json!({
        "id": id, "version": manifest.version, "module": "module.wasm",
        "sha256": corvus_package::sha256_hex(&bytes), "permissions": manifest.permissions,
    });
    std::fs::write(folder.join(crate::plugins::loader::MANIFEST), said.to_string()).expect("its manifest");
    folder
}
