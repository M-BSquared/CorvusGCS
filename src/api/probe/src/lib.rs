//! A plugin for the tests of the loader (`src/plugins/loader.rs`), built as a library like a real
//! one: what it is told in a message it does, on its own side of the boundary. Never shipped.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use corvus_api::plugin::{Host, Info, Plugin};
use corvus_api::ui::style::{self, Weight};
use eframe::egui;
use serde_json::{json, Value};

pub const LANG: &[(&str, &str)] = &[("de", r#"{"strings": {"Probe text": "Sondentext"}}"#)];

const TEXT: &str = "Probe text";
const UNKNOWN: &str = "Text nobody translated";

/// A character that is in no font of ours, so one is looked for while the plugin draws.
pub const FOREIGN: &str = "\u{416}";

pub fn plugin() -> Box<dyn Plugin> {
    Box::<Probe>::default()
}

corvus_api::export_plugin!(plugin, LANG);

#[derive(Default)]
struct Probe {
    stop: Arc<AtomicBool>,
    rounds: Arc<AtomicU64>,
    threads: Vec<JoinHandle<()>>,
    drop_panics: bool,
}

/// What every thread that asks for a repaint or reads the input does: take the context's lock.
pub fn knock(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        let v = d.get_temp_mut_or_default::<u64>(egui::Id::NULL);
        *v = v.wrapping_add(1);
    });
}

impl Plugin for Probe {
    fn info(&self) -> Info {
        Info {
            id: "probe",
            name: "Probe",
            icon: "activity",
            description: "",
            version: "1.0.0",
            order: 99,
            can_tab: false,
        }
    }

    fn tick(&mut self, h: &mut Host) {
        h.publish("rounds", json!(self.rounds.load(Ordering::Relaxed)));
    }

    fn message(&mut self, _from: &str, msg: &Value, h: &mut Host) {
        match msg.as_str().unwrap_or_default() {
            "panic" => panic!("the probe was told to"),
            "drop panics" => self.drop_panics = true,
            "knock" => {
                for _ in 0..2 {
                    let (ctx, stop, n) = (h.ctx.clone(), self.stop.clone(), self.rounds.clone());
                    self.threads.push(std::thread::spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            knock(&ctx);
                            n.fetch_add(1, Ordering::Relaxed);
                        }
                    }));
                }
            }
            "look" => {
                h.publish(
                    "look",
                    json!({
                        "bg": style::palette().bg,
                        "touch": style::touch(),
                        "text": corvus_api::i18n::tr(TEXT),
                        "none": corvus_api::i18n::tr(UNKNOWN),
                    }),
                );
            }
            _ => {}
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, _h: &mut Host) -> f32 {
        let g = style::galley(ui, FOREIGN, 14.0, Weight::W400, style::text_1(), 0.0);
        ui.painter().galley(area.min, g, style::text_1());
        for _ in 0..50 {
            knock(ui.ctx());
        }
        20.0
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        assert!(!self.drop_panics, "the probe was told to panic in its drop");
    }
}
