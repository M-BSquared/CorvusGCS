//! The window, laid out as index.html lays out `.app`: the top bar, and
//! under it `.body` with the left rail, the page (the map with the flight
//! bar, HUD and map controls over it on HOME) and the right panel.

use std::process::ExitCode;
use std::sync::OnceLock;

use eframe::egui;
use serde_json::Value;

use crate::flight::FlightBar;
use crate::http::{self, Server};
use crate::hud::Hud;
use crate::map::Map;
use crate::maprail::MapRail;
use crate::nav::{self, Page};
use crate::panel::Panel;
use crate::telemetry::Telemetry;
use crate::units::Units;
use crate::{fonts, style, topbar};

static LOGO: &[u8] = include_bytes!("../../../assets/CorvusGCS_logo_inverted.png");

struct App {
    telemetry: Telemetry,
    units: Units,
    map: Map,
    hud: Hud,
    flight: FlightBar,
    logo: egui::ImageSource<'static>,
    page: Page,
    mission_page: bool,
    panel: Panel,
    rail: MapRail,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

impl App {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let state: Value = self.telemetry.state();
        let now = now_ms();
        self.map.update(&state);
        self.flight.drain_errors(now);

        let blocks = topbar::blocks(&state, &self.units);
        egui::Panel::top("topbar")
            .exact_size(topbar::HEIGHT)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                topbar::show(ui, rect, &self.logo, &blocks);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(style::BG))
            .show(ui, |ui| {
                let body = ui.max_rect();
                let rail = egui::Rect::from_min_size(body.min, egui::vec2(nav::WIDTH, body.height()));
                let panel_w = self.panel.current_width();
                let panel = egui::Rect::from_min_max(egui::pos2(body.right() - panel_w, body.top()), body.max);
                let main = egui::Rect::from_min_max(
                    egui::pos2(rail.right(), body.top()),
                    egui::pos2(panel.left(), body.bottom()),
                );
                if self.page == Page::Home {
                    let mut map_ui = ui.new_child(egui::UiBuilder::new().max_rect(main));
                    map_ui.set_clip_rect(main);
                    self.map.show(&mut map_ui, &state);
                    let corner = main.max - egui::vec2(16.0, 16.0);
                    self.hud.show(&mut map_ui, corner, &state, &self.units, now);
                    self.rail.show(&mut map_ui, main, &mut self.map, &state);
                    self.flight.show(&mut map_ui, main, &state, &self.units);
                } else {
                    nav::placeholder(ui, main, self.page);
                }
                if let Some(page) = nav::show(ui, rail, self.mission_page, self.page) {
                    self.page = page;
                }
                let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(panel));
                panel_ui.set_clip_rect(panel);
                self.panel.show(&mut panel_ui, panel, &state);
            });
        // The flight clock ticks while airborne, and toasts expire.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn on_exit(&mut self) {
        self.telemetry.stop();
        self.panel.shut_down();
        self.map.shut_down();
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
    let config = http::get_json(&server, "/api/config").unwrap_or(Value::Null);
    let units = Units::from_config(&config);
    let ui_flag = |k: &str, default: bool| {
        config
            .get("ui")
            .and_then(|u| u.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(default)
    };
    let mission_page = ui_flag("mission_page", false);
    let measure = ui_flag("measure_tool", true);
    // The interface size (Settings > Interface), as --ui-scale zooms the page.
    let scale = config
        .get("ui")
        .and_then(|u| u.get("scale"))
        .and_then(Value::as_f64)
        .filter(|s| (0.8..=1.5).contains(s))
        .unwrap_or(1.0) as f32;
    let sources = http::get_json(&server, "/api/tiles/sources").unwrap_or(Value::Null);
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
            egui_extras::install_image_loaders(&ctx);
            fonts::install(&ctx);
            style::apply(&ctx);
            let repaint = ctx.clone();
            let telemetry = Telemetry::start(server.clone(), move || repaint.request_repaint());
            let map = Map::new(&ctx, &server, &sources);
            let flight = FlightBar::new(server.clone(), &ctx);
            ctx.set_zoom_factor(scale);
            let panel = Panel::new(server.clone(), &ctx, &config);
            let mut rail = MapRail::default();
            rail.measure = measure;
            let logo = egui::ImageSource::Bytes {
                uri: "bytes://corvus-logo.png".into(),
                bytes: LOGO.into(),
            };
            Ok(Box::new(App {
                telemetry,
                units,
                map,
                hud: Hud::default(),
                flight,
                logo,
                page: Page::Home,
                mission_page,
                panel,
                rail,
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
