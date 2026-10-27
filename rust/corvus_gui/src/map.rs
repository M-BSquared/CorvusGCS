//! The map: tiles from the backend's tile proxy, the aircraft, home and the
//! flown track, drawn as the web map draws them (src/js/map.js, map.css).
//!
//! Tiles come through `/api/tiles/<source>/z/x/y.png`, the same proxy the
//! web map uses, so the backend's offline cache serves both and the app
//! itself never talks to a tile server. The source is `satellite` (Esri), the
//! web map's first layer; picking another comes with the map controls.

use eframe::egui::{self, Color32, Pos2, Shape, Stroke, Vec2};
use serde_json::Value;
use walkers::sources::{Attribution, TileSource};
use walkers::{HttpOptions, HttpTiles, MapMemory, Plugin, Projector, TileId};

use crate::http::Server;
use crate::style;
use crate::track::{self, Point, Track};

/// The web map's start: where it opens before any aircraft is heard.
const DEFAULT_CENTER: (f64, f64) = (11.640969, 48.080217);
const DEFAULT_ZOOM: f64 = 13.0;
/// The zoom the map goes to on the first fix.
const FIRST_FIX_ZOOM: f64 = 16.0;
/// The follow dead zone, as a share of the map: the aircraft moves freely
/// inside it and the map pans only once it leaves.
const FOLLOW_BOX_FRACTION: f32 = 0.4;
const DEFAULT_SOURCE: &str = "satellite";

struct CorvusTiles {
    base: String,
    id: String,
    attribution: &'static str,
    max_zoom: u8,
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
fn describe(sources: &Value, id: &str) -> (String, u8) {
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
}

fn real_fix(v: Option<&Value>) -> Option<(f64, f64)> {
    let a = v?.as_array()?;
    let lng = a.first()?.as_f64()?;
    let lat = a.get(1)?.as_f64()?;
    (lng.is_finite() && lat.is_finite() && !(lng == 0.0 && lat == 0.0)).then_some((lng, lat))
}

impl Map {
    pub fn new(ctx: &egui::Context, server: &Server, sources: &Value) -> Map {
        let mut memory = MapMemory::default();
        let _ = memory.set_zoom(DEFAULT_ZOOM);
        memory.center_at(walkers::lon_lat(DEFAULT_CENTER.0, DEFAULT_CENTER.1));
        let mut map = Map {
            server: server.clone(),
            sources: sources.clone(),
            source: String::new(),
            tiles: None,
            memory,
            attribution: "",
            track: Track::default(),
            first_fix: true,
            follow: true,
            heading: None,
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

    /// Draws the map into the whole of `ui`.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &Value) {
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
        let layers = Layers {
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
        .double_click_to_zoom(true);
        map = map.zoom_speed(2.0);
        let response = ui.add(map);
        if response.dragged() {
            self.follow = false;
        }
        if let Some((lng, lat)) = vehicle {
            if self.follow {
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
        let g = style::galley(ui, self.attribution, 10.0, style::Weight::W400, style::TEXT_2, 0.0);
        let size = Vec2::new(g.size().x + 4.0 + 9.0, 10.0 * 1.4 + 8.0);
        let pill = egui::Rect::from_min_size(Pos2::new(rect.left() + 10.0, rect.bottom() - 10.0 - size.y), size);
        let painter = ui.painter_at(rect);
        painter.add(style::shadow_pop().as_shape(pill, egui::CornerRadius::same(255)));
        painter.rect(
            pill,
            egui::CornerRadius::same(255),
            style::glass(0.62),
            style::glass_stroke(),
            egui::StrokeKind::Inside,
        );
        painter.galley(
            Pos2::new(pill.left() + 9.0, pill.center().y - g.size().y / 2.0),
            g,
            style::TEXT_2,
        );
    }

    /// Lets go of the tile downloader and joins its thread.
    pub fn shut_down(&mut self) {
        self.tiles = None;
    }
}

struct Layers<'a> {
    track: &'a Track,
    home: Option<(f64, f64)>,
    vehicle: Option<(f64, f64)>,
    heading: f32,
}

fn screen(projector: &Projector, _rect: egui::Rect, p: &Point) -> Pos2 {
    let v = projector.project(walkers::lon_lat(p.lng, p.lat));
    v.to_pos2().clamp(Pos2::new(-1e6, -1e6), Pos2::new(1e6, 1e6))
}

/// One flight as the web map draws it: a soft glow, a dark casing and the
/// coloured core, so the line reads on grass and on a red roof alike.
fn paint_flight(painter: &egui::Painter, pts: &[Pos2], color: Color32, past: bool) {
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
fn paint_vehicle(painter: &egui::Painter, c: Pos2, heading: f32) {
    for (r, a) in [(17.0, 0.07), (14.0, 0.07), (11.0, 0.08)] {
        painter.circle_filled(c, r, style::rgba(style::VEHICLE_RGB, a));
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
    let fill = style::rgba(style::VEHICLE_RGB, 0.62);
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
    painter.circle_filled(c, 9.0, style::VEHICLE);
    painter.circle_filled(c, 2.4, Color32::WHITE);
}

/// The home marker (buildHomeMarker, 32 px): crosshair ticks, a white ring,
/// a green pad and an H in strokes.
fn paint_home(painter: &egui::Painter, c: Pos2) {
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
    let h = style::HEALTHY;
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

impl Plugin for Layers<'_> {
    fn run(self: Box<Self>, ui: &mut egui::Ui, response: &egui::Response, projector: &Projector, _memory: &MapMemory) {
        let rect = response.rect;
        let painter = ui.painter_at(rect);
        for flight in &self.track.past {
            let pts: Vec<Pos2> = track::smooth(flight)
                .iter()
                .map(|p| screen(projector, rect, p))
                .collect();
            paint_flight(&painter, &pts, style::TRACK_PAST, true);
        }
        let pts: Vec<Pos2> = track::smooth(&self.track.current)
            .iter()
            .map(|p| screen(projector, rect, p))
            .collect();
        paint_flight(&painter, &pts, style::TRACK, false);
        if let Some((lng, lat)) = self.home {
            paint_home(&painter, screen(projector, rect, &Point { lng, lat, alt: None }));
        }
        if let Some((lng, lat)) = self.vehicle {
            paint_vehicle(
                &painter,
                screen(projector, rect, &Point { lng, lat, alt: None }),
                self.heading,
            );
        }
    }
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
            sources: Value::Null,
            source: String::new(),
            tiles: None,
            memory: MapMemory::default(),
            attribution: "",
            track: Track::default(),
            first_fix: true,
            follow: true,
            heading: None,
        };
        let _ = ctx;
        m.update(&json!({"connected": true, "armed": false, "position": [11.6, 48.1]}));
        assert!(!m.first_fix);
        assert_eq!(m.track.current.len(), 1);
        m.update(&json!({"connected": false, "position": [11.7, 48.2]}));
        assert_eq!(m.track.current.len(), 1, "a disconnected state draws nothing");
    }
}
