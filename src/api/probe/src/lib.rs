//! A plugin for the tests of the plugin runtime (`src/plugins/wasm`), built as a module like a
//! real one: what it is told in a message it does, on its own side of the sandbox. What it finds
//! it publishes. Never shipped.

use corvus_api::egui::{self, Pos2, Rect, Vec2};
use corvus_api::plugin::{Host, Info, McpTool, Plugin, Reply};
use corvus_api::ui::filepicker;
use corvus_api::ui::kit::{self, Choice};
use corvus_api::ui::style;
use serde_json::{json, Value};

pub const LANG: &[(&str, &str)] = &[("de", r#"{"strings": {"Probe text": "Sondentext"}}"#)];

const TEXT: &str = "Probe text";
const UNKNOWN: &str = "Text nobody translated";

pub fn plugin() -> Box<dyn Plugin> {
    Box::<Probe>::default()
}

corvus_api::export_plugin!(plugin, LANG);

#[derive(Default)]
struct Probe {
    typed: String,
    picked: String,
    asked: Vec<(String, Reply)>,
    stream: Option<corvus_api::net::stream::Stream>,
    mcp: Option<u64>,
}

impl Probe {
    fn ask(&mut self, h: &Host, name: &str, method: &str, path: &str) {
        let reply = if method == "GET" {
            h.get(path)
        } else {
            h.post(path, json!({}))
        };
        self.asked.push((name.to_string(), reply));
    }
}

#[allow(unconditional_recursion, clippy::only_used_in_recursion)]
fn deep(n: u64) -> u64 {
    deep(std::hint::black_box(n + 1)) + 1
}

impl Plugin for Probe {
    fn info(&self) -> Info {
        Info {
            id: "probe",
            name: "Probe",
            icon: "activity",
            description: "A plugin for the tests",
            version: "1.0.0",
            order: 99,
            can_tab: false,
        }
    }

    fn tick(&mut self, h: &mut Host) {
        let mut answered = Vec::new();
        self.asked.retain(|(name, reply)| match reply.take() {
            Some(r) => {
                answered.push((name.clone(), r));
                false
            }
            None => true,
        });
        for (name, r) in answered {
            let v = match r {
                Ok(v) => json!({"ok": v}),
                Err(e) => json!({"err": e}),
            };
            h.publish(&name, v);
        }
    }

    fn message(&mut self, _from: &str, msg: &Value, h: &mut Host) {
        let arg = msg.get("arg").and_then(Value::as_str).unwrap_or("").to_string();
        match msg.get("do").and_then(Value::as_str).unwrap_or_default() {
            "panic" => panic!("the probe was told to"),
            "loop" => loop {
                std::hint::black_box(0);
            },
            "memory" => {
                let mut hoard: Vec<Vec<u8>> = Vec::new();
                loop {
                    hoard.push(std::hint::black_box(vec![1u8; 16 << 20]));
                }
            }
            "stack" => {
                h.publish("deep", json!(deep(0)));
            }
            "get" => self.ask(h, "get", "GET", &arg),
            "post" => self.ask(h, "post", "POST", &arg),
            "events" => {
                self.stream = Some(h.events(&arg, |_, _| {}));
            }
            "pick" => {
                let path = filepicker::pick_file("Probe pick", None, None);
                let read = path.as_deref().map(|p| filepicker::read(p, 1 << 20));
                h.publish(
                    "pick",
                    match read {
                        Some(Ok(bytes)) => json!({"ok": String::from_utf8_lossy(&bytes)}),
                        Some(Err(e)) => json!({"err": e}),
                        None => json!({"err": "nothing picked"}),
                    },
                );
            }
            // A file nobody picked, and the disk behind the sandbox.
            "peek" => {
                let picked = filepicker::read(std::path::Path::new(&arg), 1 << 20);
                let direct = std::fs::read(&arg);
                h.publish("peek", json!({"picked": picked.is_ok(), "direct": direct.is_ok()}));
            }
            "net" => {
                let connected = std::net::TcpStream::connect(arg.as_str()).is_ok();
                let env: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
                h.publish("net", json!({"connected": connected, "env": env}));
            }
            "look" => {
                h.publish(
                    "look",
                    json!({
                        "bg": style::palette().bg,
                        "touch": style::touch(),
                        "text": corvus_api::i18n::tr(TEXT),
                        "none": corvus_api::i18n::tr(UNKNOWN),
                        "state": h.state,
                        "settings": h.settings(),
                        "offset": corvus_api::data::clock::local_offset(),
                    }),
                );
            }
            "save" => {
                let reply = h.save_settings(&json!({ "kept": arg }), false);
                self.asked.push(("saved".into(), reply));
            }
            "notify" => h.notify("warning", &arg),
            "draw" => {
                let style = corvus_api::plugin::shapes::Style::default();
                h.map().line("a", &[[11.0, 48.0], [11.1, 48.1]], style, None, true);
                h.map()
                    .marker("m", [11.0, 48.0], "tree", "T", "tip", style, None, false, true, true);
            }
            "publish" => {
                h.publish("said", json!(arg));
            }
            "send" => {
                let sent = h.send(&arg, json!({"do": "hello"}));
                h.publish("sent", json!(sent));
            }
            _ => {}
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, area: Rect, h: &mut Host) -> f32 {
        let field = Rect::from_min_size(area.min, Vec2::new(200.0, 30.0));
        let mut text = self.typed.clone();
        let edit = egui::TextEdit::singleline(&mut text).id(egui::Id::unique("probe-field"));
        ui.put(field, edit);
        if text != self.typed {
            self.typed = text;
            h.publish("typed", json!(self.typed));
        }
        let at = Rect::from_min_size(Pos2::new(area.left(), area.top() + 40.0), Vec2::new(200.0, 30.0));
        let choices = [Choice::new("a", "First"), Choice::new("b", "Second")];
        if let Some(v) = kit::select(ui, at, &choices, &self.picked, "Pick one", false, "", "probe-select") {
            self.picked = v;
            h.publish("picked", json!(self.picked));
        }
        80.0
    }

    fn mcp_tools(&self, _h: &Host) -> Vec<McpTool> {
        vec![McpTool {
            name: "echo".into(),
            title: "Echo".into(),
            description: "Says it back".into(),
            input_schema: json!({"type": "object"}),
            confirm: false,
            read_only: true,
        }]
    }

    fn mcp_call(&mut self, _name: &str, _args: &Value, token: u64, _h: &mut Host) {
        self.mcp = Some(token);
    }

    fn mcp_take(&mut self, token: u64) -> Option<Result<Value, String>> {
        (self.mcp == Some(token)).then(|| Ok(json!({"echo": true})))
    }
}
