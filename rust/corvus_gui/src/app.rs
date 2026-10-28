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
use crate::notify::Board;
use crate::notify_ui;
use crate::panel::Panel;
use crate::setup::SetupPage;
use crate::telemetry::Telemetry;
use crate::units::Units;
use crate::{fonts, style, topbar, topbar_cards};

/// The mark ships in two cuts (themes.css `--logo-mark`): black for the
/// light themes, white for the dark ones.
static LOGO_FOR_LIGHT: &[u8] = include_bytes!("../../../assets/CorvusGCS_logo_inverted.png");
static LOGO_FOR_DARK: &[u8] = include_bytes!("../../../assets/CorvusGCS_logo.png");

struct App {
    server: Server,
    board: Board,
    telemetry: Telemetry,
    units: Units,
    top_speeds: topbar_cards::TopSpeeds,
    map: Map,
    hud: Hud,
    flight: FlightBar,
    logo: egui::ImageSource<'static>,
    page: Page,
    setup: SetupPage,
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
        self.board.update(&state, now);
        self.flight.settle(&mut self.board, now);
        for e in self.setup.take_errors().into_iter().chain(self.telemetry.take_errors()) {
            self.board.add_local("critical", &e, now);
        }
        if std::mem::take(&mut self.board.clear_remote) {
            // The backend's store owns the vehicle's lines; its next push is
            // what really removes them.
            let server = self.server.clone();
            std::thread::spawn(move || {
                let _ = http::post_json(&server, "/api/warnings/clear", &serde_json::json!({}));
            });
        }
        let summary = self.board.summary(now);

        let blocks = topbar::blocks(&state, &self.units);
        let cards = topbar_cards::all(&state, &self.units, &mut self.top_speeds);
        egui::Panel::top("topbar")
            .exact_size(topbar::HEIGHT)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                if topbar::show(ui, rect, &self.logo, &blocks, &cards, &summary, self.board.open) {
                    let open = !self.board.open;
                    self.board.set_open(open, now);
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(style::bg()))
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
                } else if self.page == Page::Setup {
                    self.setup.show(ui, main, &state, now);
                } else {
                    nav::placeholder(ui, main, self.page);
                }
                if let Some(page) = nav::show(ui, rail, self.mission_page, self.page) {
                    if page == Page::Setup && self.page == Page::Setup {
                        self.setup.reset();
                    }
                    self.page = page;
                }
                let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(panel));
                panel_ui.set_clip_rect(panel);
                self.panel.show(&mut panel_ui, panel, &state);
            });
        let screen = ui.ctx().content_rect();
        let below = notify_ui::board(ui.ctx(), screen, screen.top() + topbar::HEIGHT, &mut self.board, now);
        let toasts_top = below.map_or(screen.top() + topbar::HEIGHT + 10.0, |b| b + 10.0);
        notify_ui::toasts(ui.ctx(), screen, toasts_top, &mut self.board);
        // The flight clock ticks while airborne, and toasts expire.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

impl eframe::App for App {
    /// `CORVUS_GUI_HOVER=x,y` holds the pointer there, so a hover card or a
    /// hint can be looked at without a hand on the mouse.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(p) = std::env::var("CORVUS_GUI_HOVER").ok().and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
        }) {
            raw_input.events.push(egui::Event::PointerMoved(p));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let Some(gl) = gl {
            crate::frost::destroy(gl);
        }
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

/// `GET /api/config` answers `{"config": {...}}`; the settings are inside.
pub fn unwrap_config(answer: &Value) -> Value {
    answer.get("config").cloned().unwrap_or_else(|| answer.clone())
}

/// Opens the window on `server` and runs until it is closed.
pub fn run(
    server: Server,
    context: &'static OnceLock<egui::Context>,
    page: Option<&str>,
    scroll: Option<f32>,
) -> ExitCode {
    let config = http::get_json(&server, "/api/config")
        .map(|c| unwrap_config(&c))
        .unwrap_or(Value::Null);
    let units = Units::from_config(&config);
    // Before anything is built, so every colour of the first frame is the
    // theme's (js/theme.js fromConfig).
    style::set_theme(style::theme_from_config(&config));
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
    let start_page = match page {
        Some("setup") | Some("parameters") | Some("safety") | Some("motors") => Page::Setup,
        Some(p) if p.starts_with("safety:") => Page::Setup,
        _ => Page::Home,
    };
    let open_page = page
        .filter(|p| matches!(*p, "parameters" | "safety" | "motors") || p.starts_with("safety:"))
        .map(String::from);
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
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
                bytes: if style::is_dark() {
                    LOGO_FOR_DARK
                } else {
                    LOGO_FOR_LIGHT
                }
                .into(),
            };
            Ok(Box::new(App {
                server: server.clone(),
                board: Board::default(),
                telemetry,
                units,
                top_speeds: Default::default(),
                map,
                hud: Hud::default(),
                flight,
                logo,
                page: start_page,
                setup: {
                    let mut s = SetupPage::new(server.clone());
                    if let Some(p) = &open_page {
                        s.open(p, &ctx);
                    }
                    if let Some(px) = scroll {
                        s.scroll_to(px);
                    }
                    s
                },
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_settings_are_inside_the_answer() {
        let answer = json!({"config": {"ui": {"scale": 1.25}, "theme": {"name": "pink"}}});
        let cfg = unwrap_config(&answer);
        assert_eq!(cfg["ui"]["scale"], 1.25);
        assert_eq!(style::theme_from_config(&cfg), "pink");
        assert_eq!(unwrap_config(&json!({"ui": {}})), json!({"ui": {}}));
    }
}
