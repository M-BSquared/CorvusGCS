//! The window.

use std::process::ExitCode;
use std::sync::OnceLock;

use eframe::egui;
use serde_json::Value;

use crate::http::{self, Server};
use crate::telemetry::Telemetry;
use crate::theme::Theme;
use crate::topbar;
use crate::units::Units;

struct App {
    server: Server,
    telemetry: Telemetry,
    theme: Theme,
    units: Units,
}

impl App {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let state: Value = self.telemetry.state();
        let blocks = topbar::blocks(&state, &self.units);
        egui::Panel::top("topbar")
            .frame(
                egui::Frame::new()
                    .fill(self.theme.surface)
                    .stroke(egui::Stroke::new(1.0, self.theme.border))
                    .inner_margin(egui::Margin::symmetric(0, 8)),
            )
            .show(ui, |ui| topbar::show(ui, &self.theme, &blocks));
        egui::CentralPanel::default().show(ui, |ui| {
            let stream = if self.telemetry.stream_up() {
                "open"
            } else {
                "reconnecting"
            };
            ui.label(
                egui::RichText::new(format!("Backend {}, telemetry stream {stream}", self.server.url()))
                    .color(self.theme.text_muted),
            );
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn on_exit(&mut self) {
        self.telemetry.stop();
    }
}

/// The window title, from the version the backend serves; the app carries
/// no version of its own.
fn title(server: &Server) -> String {
    match http::get_json(server, "/api/version") {
        Ok(v) => {
            let product = v.get("product").and_then(Value::as_str).unwrap_or("Corvus GCS");
            match v.get("version").and_then(Value::as_str) {
                Some(ver) => format!("{product} v{ver}"),
                None => product.to_string(),
            }
        }
        Err(_) => "Corvus GCS".into(),
    }
}

/// Opens the window on `server` and runs until it is closed.
pub fn run(server: Server, context: &'static OnceLock<egui::Context>) -> ExitCode {
    let units = http::get_json(&server, "/api/config")
        .map(|c| Units::from_config(&c))
        .unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title(&server))
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Corvus GCS",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let _ = context.set(ctx.clone());
            let theme = Theme::light();
            theme.apply(&ctx);
            let telemetry = Telemetry::start(server.clone(), move || ctx.request_repaint());
            Ok(Box::new(App {
                server,
                telemetry,
                theme,
                units,
            }))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("corvus-gcs: {e}");
            ExitCode::FAILURE
        }
    }
}
