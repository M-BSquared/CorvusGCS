//! The window, laid out as index.html lays out `.app`: the top bar, and
//! under it `.body` with the left rail, the page (the map with the flight
//! bar, HUD and map controls over it on HOME) and the right panel.

pub mod nav;
pub mod responsive;

use std::process::ExitCode;
use std::sync::OnceLock;

use eframe::egui;
use serde_json::Value;

use crate::analysis::AnalysisPage;
use crate::app::nav::Page;
use crate::data::telemetry::Telemetry;
use crate::data::units::Units;
use crate::home::flight::FlightBar;
use crate::home::hud::Hud;
use crate::home::{topbar, topbar_cards};
use crate::logs::LogsPage;
use crate::map::rail::MapRail;
use crate::map::Map;
use crate::mission::MissionPage;
use crate::net::http::{self, Server};
use crate::notify::Board;
use crate::panel::Panel;
use crate::settings::{self, SettingsPage};
use crate::setup::geofence;
use crate::setup::SetupPage;
use crate::ui::{fonts, style};

/// The mark ships in two cuts (themes.css `--logo-mark`): black for the
/// light themes, white for the dark ones.
static LOGO_FOR_LIGHT: &[u8] = include_bytes!("../../assets/CorvusGCS_logo_inverted.png");
static LOGO_FOR_DARK: &[u8] = include_bytes!("../../assets/CorvusGCS_logo.png");

struct App {
    server: Server,
    control: crate::control::Control,
    welcome: crate::welcome::Welcome,
    caps: crate::data::capabilities::Cache,
    mcp: crate::mcp::Mcp,
    update: crate::update::Update,
    updates_on: bool,
    topbar_hidden: Vec<&'static str>,
    rangefinder: bool,
    rangefinder_asked: bool,
    rangefinder_retry: f64,
    rangefinder_failed: std::sync::Arc<std::sync::Mutex<bool>>,
    fleet: crate::fleet::Fleet,
    fleet_dialog: crate::fleet::dialog::Dialog,
    board: Board,
    telemetry: Telemetry,
    units: Units,
    top_speeds: topbar_cards::TopSpeeds,
    checklist: crate::setup::checklist::Checklist,
    shot_sent: bool,
    click_stage: u32,
    map: Map,
    hud: Hud,
    flight: FlightBar,
    logo: egui::ImageSource<'static>,
    page: Page,
    setup: SetupPage,
    first_setup: crate::setup::firstsetup_watch::Prompt,
    settings: SettingsPage,
    logs: LogsPage,
    analysis: AnalysisPage,
    mission: MissionPage,
    mission_page: bool,
    panel: Panel,
    rail: MapRail,
}

/// The id a dock ties the window to its launcher entry by: `corvus-gcs`, as the
/// AppImage's desktop entry names it, or `CORVUS_APP_ID` (the source checkout's
/// `corvus-gcs-dev`, which `run.sh` sets).
fn app_id() -> String {
    std::env::var("CORVUS_APP_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "corvus-gcs".to_string())
}

/// The mark for the theme in force; the uri carries the cut so a theme
/// switch loads the other one.
fn logo_source() -> egui::ImageSource<'static> {
    let dark = style::is_dark();
    egui::ImageSource::Bytes {
        uri: format!("bytes://corvus-logo-{}.png", if dark { "dark" } else { "light" }).into(),
        bytes: if dark { LOGO_FOR_DARK } else { LOGO_FOR_LIGHT }.into(),
    }
}

/// The window icon where the platform takes one from the window (Linux, and
/// Windows when run bare); the bundles carry their own.
fn window_icon() -> egui::IconData {
    image::load_from_memory(LOGO_FOR_DARK).map_or_else(
        |_| egui::IconData::default(),
        |img| {
            let img = img.to_rgba8();
            egui::IconData {
                width: img.width(),
                height: img.height(),
                rgba: img.into_raw(),
            }
        },
    )
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

impl App {
    /// What the Settings page changed that the rest of the window reads from
    /// the config: theme, size, units, the Mission entry, the distance tool,
    /// the side panel's tabs, the base layer.
    /// Puts what a config names in force: theme, size, units, the Mission
    /// entry, the tabs, the controls, the HUD.
    fn apply_effects(&mut self, ctx: &egui::Context, e: settings::Effects) {
        style::set_theme(e.theme);
        style::apply(ctx);
        self.logo = logo_source();
        if !e.base_layer.is_empty() && self.map.has_layer(&e.base_layer) {
            self.map.set_source(ctx, &e.base_layer);
        }
        ctx.set_zoom_factor(e.scale);
        self.units = e.units;
        self.mission_page = e.mission_page;
        self.rail.measure = e.measure_tool;
        self.panel.console_tab = e.console_tab;
        self.panel.ssh_tab = e.ssh_tab;
        self.control.set_config(e.controls);
        self.hud.visible = e.flight_hud;
        self.hud.square = e.hud_square;
    }

    /// What the map's camera button asked of the camera windows.
    fn apply_camera(&mut self, ctx: &egui::Context, cmd: crate::map::cameras::Cmd, now: f64) {
        use crate::map::cameras::Cmd;
        let server = self.server.clone();
        let list = self.rail.cameras.list.clone();
        let windows = self.setup.video_windows();
        match cmd {
            Cmd::Toggle(raw) => {
                let id = raw.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                if windows.has(&id) {
                    windows.close(&id);
                } else {
                    windows.open(ctx, &server, &raw);
                }
            }
            Cmd::CloseAll => windows.close_all(),
            Cmd::OpenAll => {
                if windows.ids().is_empty() {
                    if list.is_empty() {
                        self.board
                            .add_local("info", "No camera is set up yet. Add one under Setup, Video.", now);
                    }
                    self.rail.cameras.open_all(ctx, &server, self.setup.video_windows());
                } else {
                    windows.close_all();
                }
            }
        }
    }

    /// Asks for the distance sensor's stream, or hands it back, once per
    /// connection and change of mind. Nothing is sent while the altitude
    /// block is off the bar: its card could not be opened to read it.
    fn sync_rangefinder(&mut self, state: &Value, now: f64) {
        if state.get("connected") != Some(&Value::Bool(true)) {
            self.rangefinder_asked = false;
            return;
        }
        let want = self.rangefinder && !self.topbar_hidden.contains(&"Altitude");
        if want == self.rangefinder_asked || (want && now < self.rangefinder_retry) {
            return;
        }
        self.rangefinder_asked = want;
        let server = self.server.clone();
        let shared = self.rangefinder_failed.clone();
        std::thread::spawn(move || {
            let answer = http::post_json(
                &server,
                "/api/rangefinder/stream",
                &serde_json::json!({"enabled": want}),
            );
            let refused = match answer {
                Ok(d) => d.get("ok") == Some(&Value::Bool(false)),
                Err(_) => true,
            };
            if refused && want {
                if let Ok(mut f) = shared.lock() {
                    *f = true;
                }
            }
        });
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        if let Some(e) = self.settings.take_effects() {
            self.apply_effects(ctx, e);
        }
        if self.settings.take_sources_dirty() {
            if let Ok(sources) = http::get_json(&self.server, "/api/tiles/sources") {
                self.map.set_sources(sources);
            }
        }
        if let Some((lng, lat, zoom)) = self.settings.take_show_on_map() {
            self.page = Page::Home;
            self.map.go_to(lng, lat, zoom);
        }
        if let Some(conn) = self.settings.take_ssh_request() {
            self.panel.open_ssh(ctx, conn);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let state: Value = self.telemetry.state();
        let now = now_ms();
        self.map.update(&state);
        let caps = self.caps.get(ui.ctx(), &self.server, &state).clone();
        self.control
            .update(ui.ctx(), &state, &caps, self.page == Page::Home, now);
        for e in self.control.take_notes() {
            self.board.add_local("critical", &e, now);
        }
        let snap = self.fleet.snapshot();
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        if self.map.sync_fleet(&snap, &state, dt) {
            ui.ctx().request_repaint();
        }
        for (level, text) in self.fleet.take_notes() {
            self.board.add_local(&level, &text, now);
        }
        self.board.update(&state, now);
        self.flight.settle(&mut self.board, now);
        self.checklist.poll();
        if let Some(msg) = self.checklist.on_telemetry(&state) {
            self.board.add_local("warning", &msg, now);
        }
        for e in self
            .setup
            .take_errors()
            .into_iter()
            .chain(self.telemetry.take_errors())
            .chain(self.checklist.take_errors())
        {
            self.board.add_local("critical", &e, now);
        }
        for e in self.analysis.take_errors() {
            self.board.add_local("critical", &e, now);
        }
        for e in self.settings.take_errors() {
            self.board.add_local("critical", &e, now);
        }
        for n in self.setup.take_notes().into_iter().chain(self.settings.take_notes()) {
            self.board.add_local("info", &n, now);
        }
        for n in self.setup.take_warnings() {
            self.board.add_local("warning", &n, now);
        }
        self.apply_settings(ui.ctx());
        if std::mem::take(&mut self.board.clear_remote) {
            // The backend's store owns the vehicle's lines; its next push is
            // what really removes them.
            let server = self.server.clone();
            std::thread::spawn(move || {
                let _ = http::post_json(&server, "/api/warnings/clear", &serde_json::json!({}));
            });
        }
        let summary = self.board.summary(now);

        let vw = crate::app::responsive::Vw::of(ui.ctx());
        let mut blocks = topbar::blocks(&state, &self.units);
        if let Some(b) = blocks.first_mut() {
            b.value = crate::fleet::vehicle_label(&state, &snap, &b.value);
        }
        let own = state.get("vehicle_id").and_then(Value::as_str).unwrap_or("");
        let dot = topbar::FleetDot {
            color: snap
                .is_multi()
                .then(|| crate::fleet::parse_hex(snap.color_of(own)).unwrap_or_else(style::vehicle)),
        };
        let mut cards = topbar_cards::all(&state, &self.units, &mut self.top_speeds, self.rangefinder);
        // Blocks the operator switched off (Settings, Interface, Top bar).
        let keep: Vec<bool> = blocks.iter().map(|b| !self.topbar_hidden.contains(&b.label)).collect();
        let mut at = 0;
        blocks.retain(|_| {
            at += 1;
            keep[at - 1]
        });
        let mut at = 0;
        cards.retain(|_| {
            at += 1;
            keep.get(at - 1).copied().unwrap_or(true)
        });
        egui::Panel::top("topbar")
            .exact_size(vw.top_height())
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let clicked = topbar::show(
                    ui,
                    rect,
                    &self.logo,
                    &blocks,
                    &cards,
                    &summary,
                    self.board.open,
                    &vw,
                    dot,
                );
                if clicked.notifications {
                    let open = !self.board.open;
                    self.board.set_open(open, now);
                }
                if clicked.vehicles {
                    self.fleet_dialog.open(ui.ctx(), &self.server, None);
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(style::bg()))
            .show(ui, |ui| {
                let body = ui.max_rect();
                let rail = egui::Rect::from_min_size(body.min, egui::vec2(nav::WIDTH, body.height()));
                self.panel.follow_window(&vw);
                let panel_w = self.panel.current_width(&vw);
                let panel = egui::Rect::from_min_max(egui::pos2(body.right() - panel_w, body.top()), body.max);
                let main = egui::Rect::from_min_max(
                    egui::pos2(rail.right(), body.top()),
                    egui::pos2(panel.left(), body.bottom()),
                );
                if self.page != Page::Mission {
                    self.map.planner = false;
                    self.map.bottom_lift = 0.0;
                    self.rail.planner = false;
                }
                if self.page == Page::Home {
                    let mut map_ui = ui.new_child(egui::UiBuilder::new().max_rect(main));
                    map_ui.set_clip_rect(main);
                    self.map.measure.units = self.units;
                    self.map.show(&mut map_ui, &state);
                    let inset = if vw.max959 { 12.0 } else { 16.0 };
                    self.hud.show(
                        &mut map_ui,
                        main,
                        inset,
                        &state,
                        &self.units,
                        now,
                        vw.hud_width(),
                        vw.max600,
                    );
                    let t = ui.input(|i| i.time);
                    self.rail.cameras.poll(ui.ctx(), &self.server, t);
                    self.rail.open_cameras = self.setup.video_windows().ids();
                    self.rail.show(&mut map_ui, main, &mut self.map, &state);
                    for cmd in self.rail.cameras.take_cmds() {
                        self.apply_camera(ui.ctx(), cmd, now);
                    }
                    self.flight
                        .show(&mut map_ui, main, &state, &self.units, &mut self.map.tools);
                    let ground = crate::home::flight::is_ground(&state);
                    if let Some(action) = self
                        .map
                        .tools
                        .menu(&map_ui, &state, ground, &self.units, now / 1000.0, main)
                    {
                        let alt = self.map.tools.plan.alt_agl();
                        self.flight.run_action(ui.ctx(), action, ground, alt);
                    }
                    let below = self.flight.bar_bottom + 10.0;
                    self.checklist.window(&mut map_ui, ui.ctx(), main, below);
                    self.control.show(&mut map_ui, main, now);
                } else if self.page == Page::Mission {
                    self.mission
                        .show(ui, main, &state, &self.units, &mut self.map, &mut self.rail, vw.max1279);
                    for (level, text) in self.mission.take_notices() {
                        self.board.add_local(level, &text, now);
                    }
                } else if self.page == Page::Setup {
                    self.setup.units = self.units;
                    self.setup.show(ui, main, &state, now, &mut self.checklist);
                } else if self.page == Page::Analysis {
                    self.analysis.show(ui, main, &state, &self.units);
                } else if self.page == Page::Logs {
                    let transcript = self.panel.console_transcript();
                    self.logs.show(ui, main, &transcript);
                } else if self.page == Page::Settings {
                    self.settings.show(ui, main);
                } else {
                    nav::placeholder(ui, main, self.page);
                }
                if let Some(page) = nav::show(
                    ui,
                    rail,
                    self.mission_page,
                    self.page,
                    !vw.max1279,
                    self.update.available.as_deref(),
                ) {
                    if page == Page::Setup && self.page == Page::Setup {
                        self.setup.reset();
                    }
                    if self.page == Page::Analysis && page != Page::Analysis {
                        self.analysis.reset();
                    }
                    if page == Page::Logs && self.page != Page::Logs {
                        self.logs.reset();
                    }
                    if page == Page::Settings && self.page != Page::Settings {
                        self.settings.enter(ui.ctx());
                    }
                    if self.page == Page::Mission && page != Page::Mission {
                        self.mission.suspend();
                    }
                    self.page = page;
                }
                let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(panel));
                panel_ui.set_clip_rect(panel);
                self.panel.show(&mut panel_ui, panel, &state);
                self.panel.windows(ui.ctx());
                self.setup.windows(ui.ctx());
            });
        if let Some(id) = self.map.take_vehicle_click() {
            self.fleet_dialog.open(ui.ctx(), &self.server, Some(id));
        }
        self.fleet_dialog
            .show(ui.ctx(), &self.fleet, &self.server, &state, &self.units, &mut self.map);
        for id in self.fleet_dialog.take_removed() {
            self.map.forget_vehicle(&id);
        }
        for e in self.fleet.take_errors() {
            self.board.add_local("critical", &e, now);
        }
        match self.welcome.show(ui.ctx(), &self.server, &self.logo) {
            crate::welcome::Event::None => {}
            crate::welcome::Event::Preview(cfg) | crate::welcome::Event::Restore(cfg) => {
                self.apply_effects(ui.ctx(), settings::Effects::of(&cfg));
            }
            crate::welcome::Event::Done(cfg) => {
                self.apply_effects(ui.ctx(), settings::Effects::of(&cfg));
                self.board.add_local(
                    "info",
                    "Corvus GCS is set up. Everything can be changed under Settings.",
                    now,
                );
            }
            crate::welcome::Event::Import => {
                self.page = Page::Settings;
                self.settings.enter(ui.ctx());
                self.settings.open_import();
            }
        }
        let screen_rect = ui.ctx().content_rect();
        let main_rect = egui::Rect::from_min_max(
            egui::pos2(nav::WIDTH, screen_rect.top() + vw.top_height()),
            screen_rect.max,
        );
        let below = self.flight.bar_bottom + crate::home::flight::INSET;
        self.mcp
            .show(ui.ctx(), (self.page == Page::Home).then_some(main_rect), below);
        for (level, text) in self.mcp.take_notes() {
            self.board.add_local(&level, &text, now);
        }
        if self
            .rangefinder_failed
            .lock()
            .map(|mut f| std::mem::take(&mut *f))
            .unwrap_or(false)
        {
            self.rangefinder_asked = false;
            self.rangefinder_retry = now + 15_000.0;
        }
        self.sync_rangefinder(&state, now);
        let first_run = self.welcome.is_open();
        self.update.show(
            ui.ctx(),
            &self.server,
            &state,
            now / 1000.0,
            self.updates_on && !first_run,
        );
        self.checklist.dialog(ui.ctx());
        if let Some(step) =
            self.first_setup
                .update(ui.ctx(), &self.server, &state, now, crate::setup::first_setup_open())
        {
            self.page = Page::Setup;
            self.setup.open_first_setup(&step, ui.ctx());
        }
        let screen = ui.ctx().content_rect();
        let below =
            crate::notify::overlay::board(ui.ctx(), screen, screen.top() + vw.top_height(), &mut self.board, now);
        let toasts_top = below.map_or(screen.top() + vw.top_height() + 10.0, |b| b + 10.0);
        crate::notify::overlay::toasts(ui.ctx(), screen, toasts_top, &mut self.board);
        // The flight clock ticks while airborne, and toasts expire.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

impl App {
    /// `CORVUS_GUI_SHOT=file.png` saves the window's own pixels once
    /// `CORVUS_GUI_SHOT_AFTER` seconds (9 by default) have passed, and quits.
    fn shoot(&mut self, ctx: &egui::Context, raw_input: &egui::RawInput) {
        let Ok(path) = std::env::var("CORVUS_GUI_SHOT") else {
            return;
        };
        let after: f64 = std::env::var("CORVUS_GUI_SHOT_AFTER")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(9.0);
        if raw_input.time.unwrap_or(0.0) > after && !self.shot_sent {
            self.shot_sent = true;
            let ctx_done = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |image| {
                    let (w, h) = (image.width() as u32, image.height() as u32);
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(img) = image::RgbaImage::from_raw(w, h, bytes) {
                        let _ = img.save(&path);
                    }
                    // Close the window rather than exit, so main still takes the backend down.
                    ctx_done.send_viewport_cmd(egui::ViewportCommand::Close);
                },
            )));
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    /// `CORVUS_GUI_HOVER=x,y` holds the pointer there, so a hover card or a
    /// hint can be looked at without a hand on the mouse.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.shoot(ctx, raw_input);
        // CORVUS_GUI_CLICK="x,y;x,y;enter" presses at each point in turn, the
        // first at 6 s and one more every half second, for a menu or a drawn
        // shape to look at; `enter` presses the Enter key.
        // CORVUS_GUI_TEXT="..." types that text once the clicks are done.
        if let Ok(text) = std::env::var("CORVUS_GUI_TEXT") {
            let steps = std::env::var("CORVUS_GUI_CLICK").map_or(0, |l| l.split(';').count()) as u32;
            let due = raw_input.time.unwrap_or(0.0) > 7.0 + 0.5 * f64::from(steps);
            if self.click_stage == steps * 2 && due {
                self.click_stage += 1;
                raw_input.events.push(egui::Event::Text(text));
            }
        }
        if let Ok(list) = std::env::var("CORVUS_GUI_CLICK") {
            let steps: Vec<&str> = list.split(';').map(str::trim).collect();
            let t = raw_input.time.unwrap_or(0.0);
            let k = (self.click_stage / 2) as usize;
            if let Some(step) = steps.get(k) {
                let point = step
                    .split_once(',')
                    .and_then(|(x, y)| Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?)));
                if let Some(p) = point {
                    raw_input.events.push(egui::Event::PointerMoved(p));
                }
                let due = t > 6.0 + 0.5 * k as f64;
                if self.click_stage.is_multiple_of(2) && due {
                    self.click_stage += 1;
                    match point {
                        Some(p) => raw_input.events.push(egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: Default::default(),
                        }),
                        None => raw_input.events.push(egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: Some(egui::Key::Enter),
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        }),
                    }
                } else if self.click_stage % 2 == 1 {
                    self.click_stage += 1;
                    match point {
                        Some(p) => raw_input.events.push(egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: Default::default(),
                        }),
                        None => raw_input.events.push(egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: Some(egui::Key::Enter),
                            pressed: false,
                            repeat: false,
                            modifiers: Default::default(),
                        }),
                    }
                }
            }
        }
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
            crate::ui::frost::destroy(gl);
            crate::map3d::gpu::destroy(gl);
        }
        self.telemetry.stop();
        self.fleet.shut_down();
        self.control.shut_down();
        self.mcp.shut_down();
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
/// `CORVUS_GUI_SIZE=w,h` opens the window at that size, below the usual
/// floor too, to look at the narrow layouts.
fn window_size() -> Option<[f32; 2]> {
    let v = std::env::var("CORVUS_GUI_SIZE").ok()?;
    let (w, h) = v.split_once(',')?;
    Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
}

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
    let mission_page =
        ui_flag("mission_page", false) || page.is_some_and(|p| p == "mission" || p.starts_with("mission:"));
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
        Some(p) if p == "mission" || p.starts_with("mission:") => Page::Mission,
        Some(p) if p == "setup" || crate::setup::is_page(p) => Page::Setup,
        Some(p) if p == "settings" || p.starts_with("settings:") => Page::Settings,
        Some("logs") => Page::Logs,
        Some(p) if p == "analysis" || p.starts_with("analysis:") => Page::Analysis,
        _ => Page::Home,
    };
    let open_page = page.filter(|p| crate::setup::is_page(p)).map(String::from);
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        // The 3D map's depth test.
        depth_buffer: 24,
        viewport: egui::ViewportBuilder::default()
            .with_title(title(&server))
            .with_app_id(app_id())
            .with_icon(window_icon())
            .with_inner_size(window_size().unwrap_or([1440.0, 900.0]))
            .with_min_inner_size(if window_size().is_some() {
                [320.0, 300.0]
            } else {
                [800.0, 500.0]
            }),
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
            let env = geofence::Env::new(sources.clone());
            env.load(&ctx, &server);
            let mut map = Map::new(&ctx, &server, &sources, env.clone());
            if let Some(layer) = Some(settings::model::text(&config, "map", "base_layer")).filter(|l| !l.is_empty()) {
                if map.has_layer(layer) {
                    map.set_source(&ctx, layer);
                }
            }
            let mut flight = FlightBar::new(server.clone(), &ctx);
            {
                let e = settings::Effects::of(&config);
                flight.split = e.takeoff_land_split;
                flight.shrink = e.flight_bar_shrink;
            }
            ctx.set_zoom_factor(scale);
            let panel = Panel::new(server.clone(), &ctx, &config);
            let mut rail = MapRail::default();
            rail.measure = measure;
            let logo = logo_source();
            Ok(Box::new(App {
                server: server.clone(),
                control: crate::control::Control::new(
                    server.clone(),
                    crate::control::model::Config::from_config(&config),
                ),
                caps: Default::default(),
                mcp: crate::mcp::Mcp::start(server.clone(), &ctx),
                update: crate::update::Update::new(),
                topbar_hidden: settings::Effects::of(&config).topbar_hidden,
                rangefinder: settings::Effects::of(&config).rangefinder,
                rangefinder_asked: false,
                rangefinder_retry: 0.0,
                rangefinder_failed: Default::default(),
                updates_on: config.get("updates").and_then(|u| u.get("check")) != Some(&Value::Bool(false)),
                welcome: {
                    let mut w = crate::welcome::Welcome::start(&server, &ctx);
                    if page == Some("welcome") {
                        w.open_now(&server, &ctx, config.clone());
                    }
                    w
                },
                fleet: crate::fleet::Fleet::start(server.clone(), &ctx),
                fleet_dialog: Default::default(),
                board: Board::default(),
                telemetry,
                units,
                top_speeds: Default::default(),
                checklist: crate::setup::checklist::Checklist::new(server.clone(), &ctx),
                shot_sent: false,
                click_stage: 0,
                map,
                hud: {
                    let mut h = Hud::new();
                    let e = settings::Effects::of(&config);
                    h.visible = e.flight_hud;
                    h.square = e.hud_square;
                    h.nose_up = e.compass_nose_up;
                    h
                },
                flight,
                logo,
                page: start_page,
                setup: {
                    let mut s = SetupPage::new(server.clone(), env.clone());
                    if let Some(p) = &open_page {
                        s.open(p, &ctx);
                    }
                    if let Some(px) = scroll {
                        s.scroll_to(px);
                    }
                    s
                },
                first_setup: Default::default(),
                logs: LogsPage::new(server.clone()),
                analysis: {
                    let mut a = AnalysisPage::new(server.clone());
                    if let Some(view) = page.and_then(|p| p.strip_prefix("analysis:")) {
                        a.open(view);
                    }
                    if let Some(px) = scroll {
                        a.scroll(px);
                    }
                    a
                },
                settings: {
                    let mut s = SettingsPage::new(server.clone(), &config);
                    if let Some(tab) = page.and_then(|p| p.strip_prefix("settings:")) {
                        s.open_tab(tab);
                    }
                    s
                },
                mission: {
                    let mut m = MissionPage::new(server.clone(), sources.clone());
                    if let Some(name) = page.and_then(|p| p.strip_prefix("mission:")) {
                        m.open_saved(name);
                    }
                    m
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
