//! The map: tiles from the backend's tile proxy, the aircraft, home and the
//! flown track, drawn as the web map draws them (python/src/js/map.js, map.css).
//!
//! Tiles come through `/api/tiles/<source>/z/x/y.png`, the same proxy the
//! web map uses, so the backend's offline cache serves both and the app
//! itself never talks to a tile server. The source is `satellite` (Esri), the
//! web map's first layer; picking another comes with the map controls.

pub mod measure;
pub mod rail;
pub mod regions;
pub mod search;
pub mod search_card;
pub mod tiles_card;
pub mod tiles_model;

use eframe::egui::{self, Color32, Pos2, Shape, Stroke, Vec2};
use serde_json::Value;
use walkers::sources::{Attribution, TileSource};
use walkers::{HttpOptions, HttpTiles, MapMemory, Plugin, Projector, TileId};

use crate::data::track::{self, Point, Track};
use crate::map3d::{Layers3d, Map3d};
use crate::net::http::Server;
use crate::ui::card::{self, Look};
use crate::ui::style;

/// The web map's start: where it opens before any aircraft is heard.
const DEFAULT_CENTER: (f64, f64) = (11.640969, 48.080217);
const DEFAULT_ZOOM: f64 = 13.0;
/// The zoom the map goes to on the first fix.
const FIRST_FIX_ZOOM: f64 = 16.0;
/// The follow dead zone, as a share of the map: the aircraft moves freely
/// inside it and the map pans only once it leaves.
const FOLLOW_BOX_FRACTION: f32 = 0.4;
const DEFAULT_SOURCE: &str = "satellite";

pub(crate) struct CorvusTiles {
    pub(crate) base: String,
    pub(crate) id: String,
    pub(crate) attribution: &'static str,
    pub(crate) max_zoom: u8,
}

impl TileSource for CorvusTiles {
    fn tile_url(&self, tile: TileId) -> String {
        format!(
            "{}/api/tiles/{}/{}/{}/{}.png",
            self.base, self.id, tile.zoom, tile.x, tile.y
        )
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            text: self.attribution,
            url: "",
            logo_light: None,
            logo_dark: None,
        }
    }

    fn max_zoom(&self) -> u8 {
        self.max_zoom
    }
}

/// A source's credit and top zoom from `/api/tiles/sources`.
pub(crate) fn describe(sources: &Value, id: &str) -> (String, u8) {
    let entry = sources
        .get("sources")
        .and_then(Value::as_array)
        .and_then(|list| list.iter().find(|s| s.get("id").and_then(Value::as_str) == Some(id)));
    let attribution = entry
        .and_then(|e| e.get("attribution"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let max_zoom = entry
        .and_then(|e| e.get("maxzoom"))
        .and_then(Value::as_u64)
        .unwrap_or(19)
        .min(22) as u8;
    (attribution, max_zoom)
}

/// The map and what it remembers between frames.
pub struct Map {
    server: Server,
    env: crate::setup::geofence::Env,
    sources: Value,
    /// The base layer shown, a source id of `/api/tiles/sources`.
    pub source: String,
    tiles: Option<HttpTiles>,
    memory: MapMemory,
    attribution: &'static str,
    pub track: Track,
    first_fix: bool,
    /// Follows the aircraft until the operator drags the map.
    pub follow: bool,
    /// The marker's heading, eased like the web marker.
    heading: Option<f32>,
    /// The downloaded areas, outlined while `regions_visible`.
    regions: Vec<crate::map::regions::Region>,
    pub regions_visible: bool,
    regions_load: Option<crate::net::bg::Pending<Result<Value, String>>>,
    regions_asked: f64,
    /// The distance tool.
    pub measure: crate::map::measure::Measure,
    /// Where the place search landed.
    pub pin: Option<[f64; 2]>,
    /// 3D mode: the tilted view, made when it is first switched on.
    three: Option<Map3d>,
    three_d: bool,
    /// Longitude, latitude and zoom of the flat view's centre, as last drawn.
    view: Option<(f64, f64, f64)>,
    /// The mission planner is on this map: no flown track and no home mark
    /// (the plan draws its own start), and the map does not follow the aircraft.
    pub planner: bool,
    /// Room the page keeps along the bottom, which the credit moves above.
    pub bottom_lift: f32,
    /// What the plugins draw on it.
    pub plugins: crate::plugins::layer::Layer,
}

/// Something a page draws on the flat map in its own pixels, over the
/// vehicle, and answers the pointer for: the planner's route and points.
pub trait Overlay {
    /// Called with the map's response, its projector and its zoom.
    fn run(&mut self, ui: &mut egui::Ui, response: &egui::Response, projector: &Projector, zoom: f64);
    /// Whether a double click belongs to the overlay rather than zooming.
    fn takes_double_click(&self) -> bool {
        false
    }
}

fn real_fix(v: Option<&Value>) -> Option<(f64, f64)> {
    let a = v?.as_array()?;
    let lng = a.first()?.as_f64()?;
    let lat = a.get(1)?.as_f64()?;
    (lng.is_finite() && lat.is_finite() && !(lng == 0.0 && lat == 0.0)).then_some((lng, lat))
}

impl Map {
    pub fn new(ctx: &egui::Context, server: &Server, sources: &Value, env: crate::setup::geofence::Env) -> Map {
        let mut memory = MapMemory::default();
        let _ = memory.set_zoom(DEFAULT_ZOOM);
        memory.center_at(walkers::lon_lat(DEFAULT_CENTER.0, DEFAULT_CENTER.1));
        let mut map = Map {
            server: server.clone(),
            env,
            sources: sources.clone(),
            source: String::new(),
            tiles: None,
            memory,
            attribution: "",
            track: Track::default(),
            first_fix: true,
            follow: true,
            heading: None,
            regions: Vec::new(),
            regions_visible: false,
            regions_load: None,
            regions_asked: f64::NEG_INFINITY,
            measure: crate::map::measure::Measure::default(),
            pin: None,
            three: None,
            three_d: false,
            view: None,
            planner: false,
            bottom_lift: 0.0,
            plugins: Default::default(),
        };
        map.set_source(ctx, DEFAULT_SOURCE);
        map
    }

    /// The base layers to offer: (id, label, provider label).
    pub fn layers(&self) -> Vec<(String, String, String)> {
        let providers = self
            .sources
            .get("providers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let provider_label = |id: &str| {
            providers
                .iter()
                .find(|p| p.get("id").and_then(Value::as_str) == Some(id))
                .and_then(|p| p.get("label").and_then(Value::as_str))
                .unwrap_or(id)
                .to_string()
        };
        self.sources
            .get("sources")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|s| {
                        let id = s.get("id")?.as_str()?.to_string();
                        let label = s.get("label").and_then(Value::as_str).unwrap_or(&id).to_string();
                        let prov = provider_label(s.get("provider").and_then(Value::as_str).unwrap_or(""));
                        Some((id, label, prov))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the catalogue lists a base layer `id`.
    pub fn has_layer(&self, id: &str) -> bool {
        self.sources
            .get("sources")
            .and_then(Value::as_array)
            .is_some_and(|l| l.iter().any(|s| s.get("id").and_then(Value::as_str) == Some(id)))
    }

    /// A fresh catalogue (a key was set, an elevation model came or went).
    pub fn set_sources(&mut self, sources: Value) {
        self.sources = sources;
    }

    /// Shows the source `id`, through the backend's proxy like every other.
    pub fn set_source(&mut self, ctx: &egui::Context, id: &str) {
        let (attribution, max_zoom) = describe(&self.sources, id);
        // Leaked once per layer picked: walkers wants a 'static credit.
        let attribution: &'static str = Box::leak(attribution.into_boxed_str());
        let source = CorvusTiles {
            base: self.server.url(),
            id: id.into(),
            attribution,
            max_zoom,
        };
        // No HTTP cache of walkers' own: the backend's tile cache is the one.
        let options = HttpOptions {
            cache: None,
            ..Default::default()
        };
        self.tiles = Some(HttpTiles::with_options(source, options, ctx.clone()));
        self.attribution = attribution;
        self.source = id.to_string();
    }

    /// Whether the map is in 3D mode.
    #[allow(dead_code)]
    pub fn three_d(&self) -> bool {
        self.three_d
    }

    /// Switches 3D mode on or off; the view stays where it is looking.
    pub fn set_three_d(&mut self, on: bool) {
        if on == self.three_d {
            return;
        }
        self.three_d = on;
        if on {
            if let (Some(t), Some((lng, lat, zoom))) = (self.three.as_mut(), self.view) {
                t.start_at(lng, lat, zoom, 0.0);
            }
        } else if let Some(t) = &self.three {
            self.memory.center_at(walkers::lon_lat(t.cam.lng, t.cam.lat));
            let _ = self.memory.set_zoom(t.cam.zoom);
            self.follow = false;
        }
    }

    /// The 3D view and the source catalogue, for the panel that sets it up.
    pub fn three_settings(&mut self, ctx: &egui::Context) -> (&mut Map3d, &Value) {
        self.ensure_three(ctx);
        let t = self.three.as_mut().expect("the 3D view was just made");
        t.follow_sources(&self.sources);
        (t, &self.sources)
    }

    /// Makes the 3D view when it is first needed, looking where the flat map does.
    fn ensure_three(&mut self, ctx: &egui::Context) {
        if self.three.is_some() {
            return;
        }
        let (_, max_zoom) = describe(&self.sources, &self.source);
        let mut t = Map3d::new(ctx, &self.server, &self.source, max_zoom);
        if let Some((lng, lat, zoom)) = self.view {
            t.start_at(lng, lat, zoom, 0.0);
        }
        self.three = Some(t);
    }

    /// Draws the tilted view into the whole of `ui`.
    fn show_3d(
        &mut self,
        ui: &mut egui::Ui,
        state: &Value,
        vehicle: Option<(f64, f64)>,
        home: Option<(f64, f64)>,
        heading: f32,
    ) {
        let rect = ui.max_rect();
        let (_, max_zoom) = describe(&self.sources, &self.source);
        self.ensure_three(ui.ctx());
        let follow = self.follow;
        let Some(t) = self.three.as_mut() else {
            return;
        };
        t.set_source(&self.source, max_zoom);
        t.follow_sources(&self.sources);
        if let (true, Some((lng, lat))) = (follow, vehicle) {
            t.cam.lng = lng;
            t.cam.lat = lat;
        }
        let height = state.get("altitude_agl").and_then(Value::as_f64).unwrap_or(0.0);
        let input = t.show(
            ui,
            rect,
            &Layers3d {
                track: &self.track,
                home,
                vehicle,
                heading,
                height,
                shapes: self.plugins.shapes(),
            },
        );
        if input.dragged {
            self.follow = false;
        }
        if std::env::var_os("CORVUS_GUI_3D_PANEL").is_some() {
            let anchor =
                egui::Rect::from_center_size(Pos2::new(rect.right() - 36.0, rect.top() + 249.0), Vec2::splat(34.0));
            let ctx = ui.ctx().clone();
            let (t, sources) = self.three_settings(&ctx);
            crate::map3d::panel::show(ui, anchor, t, sources);
        }
        self.paint_attribution(ui, rect);
    }

    /// Zooms one step in or out.
    pub fn zoom(&mut self, by: i32) {
        let _ = if by > 0 {
            self.memory.zoom_in()
        } else {
            self.memory.zoom_out()
        };
    }

    /// Centres on the aircraft and follows it again.
    pub fn recenter(&mut self, state: &Value) {
        self.follow = true;
        if let Some((lng, lat)) = real_fix(state.get("position")) {
            self.memory.center_at(walkers::lon_lat(lng, lat));
        }
    }

    /// Feeds one telemetry state: the track, and the first-fix jump.
    pub fn update(&mut self, state: &Value) {
        if state.get("connected") != Some(&Value::Bool(true)) {
            return;
        }
        if self
            .track
            .check_for_takeoff(state.get("armed").and_then(Value::as_bool))
        {
            self.track.start_new_flight();
        }
        if let Some((lng, lat)) = real_fix(state.get("position")) {
            let h_acc = state.get("gps_h_acc").and_then(Value::as_f64);
            let alt = state.get("altitude_agl").and_then(Value::as_f64);
            self.track.record(lng, lat, h_acc, alt);
            if self.first_fix {
                self.first_fix = false;
                let _ = self.memory.set_zoom(FIRST_FIX_ZOOM);
                self.memory.center_at(walkers::lon_lat(lng, lat));
            }
        }
    }

    /// The downloaded areas last read.
    pub fn regions(&self) -> &[crate::map::regions::Region] {
        &self.regions
    }

    /// Longitude, latitude and zoom of the flat view's centre, as last drawn.
    pub fn camera(&self) -> Option<(f64, f64, f64)> {
        self.view
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Looks at a place, no longer following the aircraft.
    pub fn go_to(&mut self, lng: f64, lat: f64, zoom: f64) {
        self.follow = false;
        let _ = self.memory.set_zoom(zoom);
        self.memory.center_at(walkers::lon_lat(lng, lat));
    }

    /// Shows or hides the outlines of the downloaded areas; showing asks the
    /// backend for them again.
    pub fn set_regions_visible(&mut self, on: bool) {
        self.regions_visible = on;
        self.regions_asked = f64::NEG_INFINITY;
    }

    /// The ids of the elevation sources, whose areas are not outlined: an
    /// elevation download rides along with an imagery one over the same ground.
    fn terrain_ids(&self) -> Vec<String> {
        self.sources
            .get("terrain")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Reads the areas while they are shown: every 5 s, every 2 s while one
    /// is downloading.
    fn poll_regions(&mut self, ctx: &egui::Context, now: f64) {
        if let Some(r) = self.regions_load.as_ref().and_then(crate::net::bg::Pending::take) {
            self.regions_load = None;
            if let Ok(d) = r {
                self.regions = crate::map::regions::read(&d);
            }
        }
        if !self.regions_visible && self.regions_asked.is_finite() {
            return;
        }
        let every = if self.regions.iter().any(|r| r.running) {
            2.0
        } else {
            5.0
        };
        if self.regions_load.is_none() && now - self.regions_asked >= every {
            self.regions_asked = now;
            let s = self.server.clone();
            self.regions_load = Some(crate::net::bg::Pending::spawn(ctx, move || {
                crate::net::http::get_json(&s, "/api/tiles/regions")
            }));
        }
        if self.regions_visible {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    /// Draws the map into the whole of `ui`.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &Value) {
        self.show_with(ui, state, None);
    }

    /// `show` with a page's own layer drawn over the aircraft.
    pub fn show_with(&mut self, ui: &mut egui::Ui, state: &Value, overlay: Option<&mut (dyn Overlay + '_)>) {
        let now = ui.input(|i| i.time);
        self.poll_regions(&ui.ctx().clone(), now);
        let terrain = self.terrain_ids();
        let regions = if self.regions_visible {
            crate::map::regions::merge(&self.regions, &|s| terrain.iter().any(|t| t == s))
        } else {
            Vec::new()
        };
        let vehicle = (state.get("connected") == Some(&Value::Bool(true)))
            .then(|| real_fix(state.get("position")))
            .flatten();
        let home = real_fix(state.get("home"));
        let target = state.get("heading").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let heading = match self.heading {
            Some(h) => {
                let delta = (target - h).rem_euclid(360.0);
                let delta = if delta > 180.0 { delta - 360.0 } else { delta };
                let next = h + delta * (1.0 - (-dt / 0.18f32).exp());
                if (next - target).abs() > 0.05 {
                    ui.ctx().request_repaint();
                }
                next
            }
            None => target,
        };
        self.heading = Some(heading);

        let my_position = vehicle.map_or(walkers::lon_lat(DEFAULT_CENTER.0, DEFAULT_CENTER.1), |(lng, lat)| {
            walkers::lon_lat(lng, lat)
        });
        if !self.three_d && self.view.is_some() && std::env::var_os("CORVUS_GUI_3D").is_some() && !self.three.is_some()
        {
            self.ensure_three(ui.ctx());
            self.set_three_d(true);
            if let Some(v) = std::env::var("CORVUS_GUI_3D").ok().filter(|v| v.contains(',')) {
                let n: Vec<f64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if let (3..=5, Some(t)) = (n.len(), self.three.as_mut()) {
                    t.start_at(n[0], n[1], n[2], n.get(3).copied().unwrap_or(0.0));
                    if let Some(p) = n.get(4) {
                        t.cam.pitch = *p;
                    }
                    self.follow = false;
                }
            }
        }
        if self.three_d {
            self.show_3d(ui, state, vehicle, home, heading);
            return;
        }
        let fence = self.env.get();
        let outline = (fence.show_on_map && fence.polygon.len() >= 3).then_some(fence.polygon);
        let double_click_zoom = !overlay.as_ref().is_some_and(|o| o.takes_double_click());
        let layers = Layers {
            planner: self.planner,
            plugins: &mut self.plugins,
            overlay,
            pin: &mut self.pin,
            measure: &mut self.measure,
            regions,
            outline,
            track: &self.track,
            home,
            vehicle,
            heading,
        };
        let mut map = walkers::Map::new(
            self.tiles.as_mut().map(|t| t as &mut dyn walkers::Tiles),
            &mut self.memory,
            my_position,
        )
        .with_plugin(layers)
        .zoom_with_ctrl(false)
        .double_click_to_zoom(double_click_zoom);
        map = map.zoom_speed(2.0);
        let response = ui.add(map);
        {
            let p = Projector::new(response.rect, &self.memory, my_position);
            let c = p.unproject(response.rect.center().to_vec2());
            self.view = Some((c.x(), c.y(), self.memory.zoom()));
        }
        if response.dragged() {
            self.follow = false;
        }
        if let Some((lng, lat)) = vehicle {
            if self.follow && !self.planner {
                self.keep_in_view(response.rect, lng, lat);
            }
        }
        self.paint_attribution(ui, response.rect);
    }

    /// Pans when the aircraft leaves the dead zone in the middle of the map.
    fn keep_in_view(&mut self, rect: egui::Rect, lng: f64, lat: f64) {
        let projector = Projector::new(rect, &self.memory, walkers::lon_lat(lng, lat));
        let at = projector.project(walkers::lon_lat(lng, lat));
        let p = at.to_pos2();
        let zone = egui::Rect::from_center_size(rect.center(), rect.size() * FOLLOW_BOX_FRACTION);
        if !zone.contains(p) {
            self.memory.center_at(walkers::lon_lat(lng, lat));
        }
    }

    /// The tile credit, bottom left, in the pill main.css draws it in.
    fn paint_attribution(&self, ui: &egui::Ui, rect: egui::Rect) {
        if self.attribution.is_empty() {
            return;
        }
        let g = style::galley(ui, self.attribution, 10.0, style::Weight::W400, style::text_2(), 0.0);
        let size = Vec2::new(g.size().x + 4.0 + 9.0, 25.0);
        let pill = egui::Rect::from_min_size(
            Pos2::new(rect.left() + 14.0, rect.bottom() - self.bottom_lift - 14.0 - size.y),
            size,
        );
        let painter = ui.painter_at(rect);
        style::glass_panel(&painter, "attribution", pill, egui::CornerRadius::same(255), 0.38);
        painter.galley(
            Pos2::new(pill.left() + 9.0, pill.center().y - g.size().y / 2.0),
            g,
            style::text_2(),
        );
    }

    /// Lets go of the tile downloader and joins its thread.
    pub fn shut_down(&mut self) {
        self.tiles = None;
    }
}

struct Layers<'a, 'o> {
    planner: bool,
    plugins: &'a mut crate::plugins::layer::Layer,
    overlay: Option<&'a mut (dyn Overlay + 'o)>,
    pin: &'a mut Option<[f64; 2]>,
    measure: &'a mut crate::map::measure::Measure,
    regions: Vec<crate::map::regions::Merged>,
    /// The geofence area, when the Home map is asked to show it.
    outline: Option<Vec<[f64; 2]>>,
    track: &'a Track,
    home: Option<(f64, f64)>,
    vehicle: Option<(f64, f64)>,
    heading: f32,
}

/// An outlined downloaded area (`offline-regions-fill` and `-line`, #4CC9FF)
/// with its name chip at the centre (`.region-label`).
fn paint_region(painter: &egui::Painter, projector: &Projector, rect: egui::Rect, r: &crate::map::regions::Merged) {
    let b = r.bounds;
    let at = |lng: f64, lat: f64| screen(projector, rect, &Point { lng, lat, alt: None });
    let ring = [at(b.w, b.s), at(b.e, b.s), at(b.e, b.n), at(b.w, b.n)];
    let blue = Color32::from_rgb(0x4C, 0xC9, 0xFF);
    painter.rect_filled(egui::Rect::from_two_pos(ring[0], ring[2]), 0, blue.gamma_multiply(0.06));
    let mut closed = ring.to_vec();
    closed.push(ring[0]);
    painter.extend(Shape::dashed_line(
        &closed,
        Stroke::new(1.4, blue.gamma_multiply(0.85)),
        5.6,
        4.2,
    ));
    let (cx, cy) = b.centre();
    let c = at(cx, cy);
    let name = painter.layout_no_wrap(r.name(), style::font(11.0, style::Weight::W600), style::text_1());
    let mc = if r.running { style::warning() } else { style::text_3() };
    let meta = painter.layout_no_wrap(r.meta(), style::font(9.0, style::Weight::W400), mc);
    let w = name.size().x.max(meta.size().x) + 16.0 + 2.0;
    let h = name.size().y + 1.0 + meta.size().y + 8.0 + 2.0;
    let chip = egui::Rect::from_center_size(c, Vec2::new(w, h));
    card::paint(
        painter,
        chip,
        Look::custom(
            style::glass(0.72),
            if r.running {
                style::warning()
            } else {
                blue.gamma_multiply(0.45)
            },
            style::RADIUS_BTN,
        ),
    );
    painter.galley(
        Pos2::new(c.x - name.size().x / 2.0, chip.top() + 5.0),
        name.clone(),
        style::text_1(),
    );
    painter.galley(
        Pos2::new(c.x - meta.size().x / 2.0, chip.top() + 5.0 + name.size().y + 1.0),
        meta,
        mc,
    );
}

fn screen(projector: &Projector, _rect: egui::Rect, p: &Point) -> Pos2 {
    let v = projector.project(walkers::lon_lat(p.lng, p.lat));
    v.to_pos2().clamp(Pos2::new(-1e6, -1e6), Pos2::new(1e6, 1e6))
}

/// One flight as the web map draws it: a soft glow, a dark casing and the
/// coloured core, so the line reads on grass and on a red roof alike.
pub(crate) fn paint_flight(painter: &egui::Painter, pts: &[Pos2], color: Color32, past: bool) {
    if pts.len() < 2 {
        return;
    }
    let (glow, glow_a, casing, casing_a, core, core_a) = if past {
        (9.0, 0.18, 4.6, 0.45, 2.2, 0.85)
    } else {
        (11.0, 0.3, 5.5, 0.55, 2.6, 0.95)
    };
    let with = |c: Color32, a: f32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0) as u8);
    // line-blur spreads the glow out; three widening strokes stand in for it.
    for (w, share) in [(glow, 0.5), (glow * 0.75, 0.3), (glow * 0.5, 0.2)] {
        painter.add(Shape::line(
            pts.to_vec(),
            Stroke::new(w, with(color, glow_a * share * 1.6)),
        ));
    }
    painter.add(Shape::line(
        pts.to_vec(),
        Stroke::new(casing, with(Color32::BLACK, casing_a)),
    ));
    painter.add(Shape::line(pts.to_vec(), Stroke::new(core, with(color, core_a))));
    for p in [pts[0], pts[pts.len() - 1]] {
        painter.circle_filled(p, core / 2.0, with(color, core_a));
    }
}

/// The vehicle marker (map.js buildVehicleMarker, 48 px): a blurred halo,
/// the heading cone, a white ring, the red body and a white centre dot.
pub(crate) fn paint_vehicle(painter: &egui::Painter, c: Pos2, heading: f32) {
    for (r, a) in [(17.0, 0.07), (14.0, 0.07), (11.0, 0.08)] {
        painter.circle_filled(c, r, style::rgba(style::vehicle_rgb(), a));
    }
    // The SVG's 48 px box, centred, rotated by the heading.
    let rot = egui::emath::Rot2::from_angle(heading.to_radians());
    let at = |x: f32, y: f32| c + rot * Vec2::new(x - 24.0, y - 24.0);
    // M24 1 L31.5 15.5 A16 16 0 0 0 16.5 15.5 Z: with sweep 0 and the small
    // arc, the circle's centre lies below the chord, so the base curves up
    // towards the tip.
    let mut cone = vec![at(24.0, 1.0), at(31.5, 15.5)];
    let half: f32 = 7.5;
    let centre_y = 15.5 + (16.0f32 * 16.0 - half * half).sqrt();
    let a0 = (15.5 - centre_y).atan2(half);
    let a1 = (15.5 - centre_y).atan2(-half);
    for i in 1..10 {
        let t = a0 + (a1 - a0) * i as f32 / 10.0;
        cone.push(at(24.0 + 16.0 * t.cos(), centre_y + 16.0 * t.sin()));
    }
    cone.push(at(16.5, 15.5));
    // The base curves in, so the outline is not convex: filled as a fan
    // from the tip, which sees every point of it.
    let fill = style::rgba(style::vehicle_rgb(), 0.62);
    let mut mesh = egui::Mesh::default();
    for p in &cone {
        mesh.colored_vertex(*p, fill);
    }
    for i in 1..cone.len() as u32 - 1 {
        mesh.add_triangle(0, i, i + 1);
    }
    painter.add(mesh);
    painter.add(Shape::closed_line(
        cone,
        Stroke::new(0.9, Color32::from_rgba_unmultiplied(255, 255, 255, 140)),
    ));
    painter.circle_filled(c, 10.5, Color32::WHITE);
    painter.circle_filled(c, 9.0, style::vehicle());
    painter.circle_filled(c, 2.4, Color32::WHITE);
}

/// The home marker (buildHomeMarker, 32 px): crosshair ticks, a white ring,
/// a green pad and an H in strokes.
pub(crate) fn paint_home(painter: &egui::Painter, c: Pos2) {
    let at = |x: f32, y: f32| c + Vec2::new(x - 16.0, y - 16.0);
    let tick = Stroke::new(1.6, Color32::from_rgba_unmultiplied(15, 139, 80, 204));
    for (a, b) in [
        ((16.0, 0.5), (16.0, 4.5)),
        ((16.0, 27.5), (16.0, 31.5)),
        ((0.5, 16.0), (4.5, 16.0)),
        ((27.5, 16.0), (31.5, 16.0)),
    ] {
        painter.line_segment([at(a.0, a.1), at(b.0, b.1)], tick);
    }
    painter.circle_filled(c, 10.0, Color32::WHITE);
    // color-mix(in srgb, var(--healthy) 88%, #000)
    let h = style::healthy();
    let mix = |v: u8| (f32::from(v) * 0.88).round() as u8;
    painter.circle_filled(c, 8.6, Color32::from_rgb(mix(h.r()), mix(h.g()), mix(h.b())));
    let glyph = Stroke::new(2.0, Color32::WHITE);
    for (a, b) in [
        ((12.4, 11.6), (12.4, 20.4)),
        ((19.6, 11.6), (19.6, 20.4)),
        ((12.4, 16.0), (19.6, 16.0)),
    ] {
        painter.line_segment([at(a.0, a.1), at(b.0, b.1)], glyph);
    }
}

impl Plugin for Layers<'_, '_> {
    fn run(
        mut self: Box<Self>,
        ui: &mut egui::Ui,
        response: &egui::Response,
        projector: &Projector,
        memory: &MapMemory,
    ) {
        let rect = response.rect;
        let painter = ui.painter_at(rect);
        if let Some(poly) = &self.outline {
            // setPolygonOverlay: --warning, 3 px, dashed 2 to 1.5, fill at 6 percent
            let pts: Vec<Pos2> = poly
                .iter()
                .map(|c| {
                    screen(
                        projector,
                        rect,
                        &Point {
                            lng: c[0],
                            lat: c[1],
                            alt: None,
                        },
                    )
                })
                .collect();
            let warning = style::warning();
            let mut mesh = egui::Mesh::default();
            for p in &pts {
                mesh.colored_vertex(*p, warning.gamma_multiply(0.06));
            }
            for t in crate::setup::geofence_card::triangulate(&pts) {
                mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
            }
            painter.add(mesh);
            let mut closed = pts.clone();
            closed.push(pts[0]);
            painter.extend(Shape::dashed_line(&closed, Stroke::new(3.0, warning), 6.0, 4.5));
        }
        for r in &self.regions {
            paint_region(&painter, projector, rect, r);
        }
        self.plugins.paint_areas(&painter, projector, rect);
        let flown = !self.planner;
        for flight in self.track.past.iter().filter(|_| flown) {
            let pts: Vec<Pos2> = track::smooth(flight)
                .iter()
                .map(|p| screen(projector, rect, p))
                .collect();
            paint_flight(&painter, &pts, style::track_past(), true);
        }
        if flown {
            let pts: Vec<Pos2> = track::smooth(&self.track.current)
                .iter()
                .map(|p| screen(projector, rect, p))
                .collect();
            paint_flight(&painter, &pts, style::track(), false);
        }
        if let Some((lng, lat)) = self.home.filter(|_| flown) {
            paint_home(&painter, screen(projector, rect, &Point { lng, lat, alt: None }));
        }
        if let Some((lng, lat)) = self.vehicle {
            paint_vehicle(
                &painter,
                screen(projector, rect, &Point { lng, lat, alt: None }),
                self.heading,
            );
        }
        self.plugins.marks(ui, response, projector, rect);
        if let Some(p) = *self.pin {
            let c = screen(
                projector,
                rect,
                &Point {
                    lng: p[0],
                    lat: p[1],
                    alt: None,
                },
            );
            paint_pin(&painter, c);
            if response.clicked()
                && response.interact_pointer_pos().is_some_and(|q| q.distance(c) <= 11.0)
                && !self.measure.active
            {
                *self.pin = None;
            }
        }
        if self.measure.active {
            drive_measure(ui, response, projector, self.measure);
            paint_measure(&painter, ui, projector, rect, self.measure);
        }
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.run(ui, response, projector, memory.zoom());
        }
    }
}

/// `.map-search-pin`: a ring and a dot in #33D6FF with a dark casing.
fn paint_pin(painter: &egui::Painter, c: Pos2) {
    let cyan = Color32::from_rgb(0x33, 0xD6, 0xFF);
    let ink = Color32::from_black_alpha(160);
    painter.circle_filled(c + Vec2::new(0.0, 1.0), 10.5, ink.gamma_multiply(0.35));
    painter.circle_stroke(c, 9.5, Stroke::new(1.0, ink));
    painter.circle_filled(c, 8.0, cyan.gamma_multiply(0.2));
    painter.circle_stroke(c, 8.0, Stroke::new(2.0, cyan));
    painter.circle_filled(c, 2.5, cyan);
}

/// Clicks, the pointer and the keys of the distance tool.
fn drive_measure(
    ui: &egui::Ui,
    response: &egui::Response,
    projector: &Projector,
    m: &mut crate::map::measure::Measure,
) {
    let at = |p: Pos2| {
        let a = projector.unproject(p.to_vec2());
        [a.x(), a.y()]
    };
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        if let Some(p) = response.hover_pos() {
            m.hover(at(p));
        }
    }
    if response.clicked() {
        if let Some(p) = response.interact_pointer_pos() {
            m.click(at(p));
        }
    }
    if response.secondary_clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        m.back();
    }
}

/// `measure-casing`, `-line` and `-points`, and the label on the line
/// (`.map-measure-label`).
fn paint_measure(
    painter: &egui::Painter,
    ui: &egui::Ui,
    projector: &Projector,
    rect: egui::Rect,
    m: &crate::map::measure::Measure,
) {
    let at = |p: [f64; 2]| {
        screen(
            projector,
            rect,
            &Point {
                lng: p[0],
                lat: p[1],
                alt: None,
            },
        )
    };
    let accent = style::accent();
    if let Some(line) = m.line() {
        let (a, b) = (at(line.from), at(line.to));
        painter.line_segment([a, b], Stroke::new(6.0, Color32::from_black_alpha(140)));
        painter.line_segment([a, b], Stroke::new(3.0, accent));
        let (dist, heading) = m.label(&line);
        let dg = painter.layout_no_wrap(dist, style::font(12.0, style::Weight::W600), style::text_1());
        let hg = painter.layout_no_wrap(heading.clone(), style::font(11.0, style::Weight::W500), style::text_2());
        let gap = if heading.is_empty() { 0.0 } else { 6.0 };
        let w = dg.size().x + gap + hg.size().x + 16.0 + 2.0;
        let h = dg.size().y.max(hg.size().y) + 6.0 + 2.0;
        let mid = at(crate::map::measure::midpoint(line.from, line.to));
        let chip = egui::Rect::from_min_size(Pos2::new(mid.x - w / 2.0, mid.y - 10.0 - h), Vec2::new(w, h));
        card::paint(
            painter,
            chip,
            Look::custom(style::glass(0.85), style::tint(0.1), style::RADIUS_BTN),
        );
        let base = chip.bottom() - 1.0 - 3.0;
        painter.galley(
            Pos2::new(chip.left() + 9.0, base - dg.size().y),
            dg.clone(),
            style::text_1(),
        );
        painter.galley(
            Pos2::new(chip.left() + 9.0 + dg.size().x + gap, base - hg.size().y),
            hg,
            style::text_2(),
        );
    }
    for p in m.points() {
        let c = at(*p);
        painter.circle_filled(c, 5.5 + 1.25, accent);
        painter.circle_filled(c, 5.5 - 1.25, Color32::WHITE);
    }
    let _ = ui;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_source_is_described_from_the_backend() {
        let sources = json!({"sources": [
            {"id": "osm", "attribution": "OSM", "maxzoom": 19},
            {"id": "satellite", "attribution": "© Esri, Maxar, Earthstar Geographics", "maxzoom": 19}
        ]});
        assert_eq!(
            describe(&sources, "satellite"),
            ("© Esri, Maxar, Earthstar Geographics".to_string(), 19)
        );
        assert_eq!(describe(&json!({}), "satellite"), (String::new(), 19));
    }

    #[test]
    fn a_tile_url_goes_through_the_backend_proxy() {
        let s = CorvusTiles {
            base: "http://127.0.0.1:8080".into(),
            id: "satellite".into(),
            attribution: "",
            max_zoom: 19,
        };
        assert_eq!(
            s.tile_url(TileId { x: 3, y: 5, zoom: 7 }),
            "http://127.0.0.1:8080/api/tiles/satellite/7/3/5.png"
        );
    }

    #[test]
    fn only_a_real_fix_counts() {
        assert_eq!(real_fix(Some(&json!([11.6, 48.1]))), Some((11.6, 48.1)));
        assert_eq!(real_fix(Some(&json!([0, 0]))), None);
        assert_eq!(real_fix(Some(&json!([]))), None);
        assert_eq!(real_fix(None), None);
    }

    #[test]
    fn the_first_fix_records_a_track_point() {
        let ctx = egui::Context::default();
        let mut m = Map {
            server: Server::local(1),
            env: crate::setup::geofence::Env::new(Value::Null),
            sources: Value::Null,
            source: String::new(),
            tiles: None,
            memory: MapMemory::default(),
            attribution: "",
            track: Track::default(),
            first_fix: true,
            follow: true,
            heading: None,
            regions: Vec::new(),
            regions_visible: false,
            regions_load: None,
            regions_asked: f64::NEG_INFINITY,
            measure: crate::map::measure::Measure::default(),
            pin: None,
            three: None,
            three_d: false,
            view: None,
            planner: false,
            bottom_lift: 0.0,
            plugins: Default::default(),
        };
        let _ = ctx;
        m.update(&json!({"connected": true, "armed": false, "position": [11.6, 48.1]}));
        assert!(!m.first_fix);
        assert_eq!(m.track.current.len(), 1);
        m.update(&json!({"connected": false, "position": [11.7, 48.2]}));
        assert_eq!(m.track.current.len(), 1, "a disconnected state draws nothing");
    }
}
