//! The review's ground track over imagery (`trackMapView`, `showTrackMap`,
//! `addTrackLayers`, `toLngLat`, `imageryLayer` of `python/src/js/analysis.js`).
//!
//! The chart stays the primary reading; this is the same plot on the backend's
//! tile proxy, so it works offline like the Home map. The rules (the inverse of
//! the backend's projection, which layer opens, how the track is fitted) have
//! no `egui` in them and are held to values the web code answered.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;
use walkers::{HttpOptions, HttpTiles, MapMemory, Plugin, Projector};

use super::plot::{self, Plot};
use crate::map::{describe, CorvusTiles};
use crate::net::http::Server;
use crate::ui::style::{self, Weight};

/// WGS-84 mean radius, the constant the backend projected with.
pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// `padding: 36` of `fitBoundsOptions`.
const FIT_PADDING: f64 = 36.0;

/// What the page needs from the Home map: the base layer shown there and the
/// catalogue of layers.
pub struct Imagery<'a> {
    pub layer: &'a str,
    pub sources: &'a Value,
}

/// One local-frame point back on the globe: the inverse of the backend's
/// azimuthal equidistant projection about `origin` (latitude, longitude).
/// Returns (longitude, latitude), or `None` for a point that is not finite.
pub fn to_lng_lat(origin: (f64, f64), east: f64, north: f64) -> Option<(f64, f64)> {
    if !east.is_finite() || !north.is_finite() {
        return None;
    }
    let (olat, olon) = origin;
    let lat0 = olat.to_radians();
    let lon0 = olon.to_radians();
    let distance = (east * east + north * north).sqrt();
    if distance <= 1e-6 {
        return Some((olon, olat));
    }
    let c = distance / EARTH_RADIUS_M;
    let (sin_c, cos_c) = c.sin_cos();
    let (sin_lat0, cos_lat0) = lat0.sin_cos();
    let lat = (cos_c * sin_lat0 + (north * sin_c * cos_lat0) / distance).asin();
    let lon = lon0 + (east * sin_c).atan2(distance * cos_lat0 * cos_c - north * sin_c * sin_lat0);
    Some((lon.to_degrees(), lat.to_degrees()))
}

/// Whether a plot can be laid over imagery: it has an origin.
pub fn mappable(plot: &Plot) -> bool {
    plot.origin.is_some_and(|(lat, lon)| lat.is_finite() && lon.is_finite())
}

/// One series as a map path.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub name: String,
    /// The chart's colour slot for this series (same palette, same order).
    pub slot: usize,
    /// Longitude and latitude.
    pub coords: Vec<(f64, f64)>,
    /// Drawn as dots, not a line.
    pub points: bool,
}

/// The plot's series as map paths; a series with no placeable point is left out.
pub fn track_paths(plot: &Plot) -> Vec<Path> {
    let Some(origin) = plot.origin else {
        return Vec::new();
    };
    plot.series
        .iter()
        .enumerate()
        .filter_map(|(index, s)| {
            let coords: Vec<(f64, f64)> =
                s.x.iter()
                    .zip(&s.y)
                    .filter_map(|(e, n)| to_lng_lat(origin, *e, *n))
                    .collect();
            (!coords.is_empty()).then(|| Path {
                name: s.name.clone(),
                slot: index % 5,
                coords,
                points: s.markers,
            })
        })
        .collect()
}

/// One layer of the catalogue `GET /api/tiles/sources` gave.
fn layer_spec<'a>(sources: &'a Value, id: &str) -> Option<&'a Value> {
    sources
        .get("sources")?
        .as_array()?
        .iter()
        .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
}

/// Which base layer the view opens on: the operator's own when that is
/// imagery, else the same provider's satellite layer, else `satellite`.
pub fn imagery_layer(current: &str, sources: &Value) -> String {
    let spec = layer_spec(sources, current);
    let style_of = |s: &Value| s.get("style").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(s) = spec {
        if matches!(style_of(s).as_str(), "satellite" | "hybrid") {
            return current.to_string();
        }
    }
    let provider = spec
        .and_then(|s| s.get("provider"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let candidate = if !provider.is_empty() && provider != "esri" && provider != "osm" {
        format!("{provider}_satellite")
    } else {
        "satellite".to_string()
    };
    if layer_spec(sources, &candidate).is_some() {
        candidate
    } else {
        "satellite".to_string()
    }
}

/// The corners of the track: west, south, east, north.
pub fn bounds(paths: &[Path]) -> Option<[f64; 4]> {
    let mut b: Option<[f64; 4]> = None;
    for (lng, lat) in paths.iter().flat_map(|p| p.coords.iter().copied()) {
        b = Some(match b {
            None => [lng, lat, lng, lat],
            Some([w, s, e, n]) => [w.min(lng), s.min(lat), e.max(lng), n.max(lat)],
        });
    }
    b
}

fn merc_x(lng: f64) -> f64 {
    (lng + 180.0) / 360.0
}

fn merc_y(lat: f64) -> f64 {
    let lat = lat.clamp(-85.051_129, 85.051_129).to_radians();
    0.5 - (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln() / (2.0 * std::f64::consts::PI)
}

fn unmerc_x(x: f64) -> f64 {
    x * 360.0 - 180.0
}

fn unmerc_y(y: f64) -> f64 {
    let n = std::f64::consts::PI * (1.0 - 2.0 * y);
    n.sinh().atan().to_degrees()
}

/// `fitBounds` with `padding: 36` and `maxZoom: min(18, spec.maxzoom || 19)`:
/// the centre (longitude, latitude) and the zoom in MapLibre's terms, where
/// the world is 512 px wide at zoom 0.
pub fn fit(b: [f64; 4], width: f64, height: f64, max_zoom: f64) -> (f64, f64, f64) {
    let [w, s, e, n] = b;
    let dx = merc_x(e) - merc_x(w);
    let dy = merc_y(s) - merc_y(n);
    let avail_w = (width - 2.0 * FIT_PADDING).max(1.0);
    let avail_h = (height - 2.0 * FIT_PADDING).max(1.0);
    let zoom_for = |avail: f64, d: f64| {
        if d > 0.0 {
            (avail / (512.0 * d)).log2()
        } else {
            f64::INFINITY
        }
    };
    let zoom = zoom_for(avail_w, dx).min(zoom_for(avail_h, dy)).min(max_zoom).max(0.0);
    let cx = (merc_x(w) + merc_x(e)) / 2.0;
    let cy = (merc_y(s) + merc_y(n)) / 2.0;
    (unmerc_x(cx), unmerc_y(cy), zoom)
}

/// The deepest zoom the view fits to: `min(18, maxzoom || 19)`.
pub fn fit_max_zoom(source_max: u8) -> f64 {
    let m = if source_max == 0 { 19 } else { u32::from(source_max) };
    f64::from(m.min(18))
}

/// MapLibre's zoom 0 is 512 px across; walkers' is 256 px.
const WALKERS_ZOOM_OFFSET: f64 = 1.0;

/// How long the zoom hint stays full, and how long it takes to go.
const HINT_HOLD: f64 = 1.1;
const HINT_FADE: f64 = 1.0;

/// The hint's opacity `age` seconds after the last plain wheel turn.
pub fn hint_opacity(age: f64) -> f32 {
    if age < 0.0 {
        0.0
    } else if age <= HINT_HOLD {
        1.0
    } else {
        (1.0 - (age - HINT_HOLD) / HINT_FADE).clamp(0.0, 1.0) as f32
    }
}

/// The map of one plot, made when it is first asked for.
pub struct TrackMap {
    tiles: Option<HttpTiles>,
    memory: MapMemory,
    attribution: &'static str,
    credit: crate::map::credit::Credit,
    max_zoom: u8,
    paths: Vec<Path>,
    fitted: bool,
    hint_at: f64,
}

struct Lines<'a>(&'a [Path]);

impl Plugin for Lines<'_> {
    fn run(self: Box<Self>, ui: &mut egui::Ui, response: &egui::Response, projector: &Projector, _: &MapMemory) {
        let painter = ui.painter().with_clip_rect(response.rect);
        let casing = Color32::from_rgba_unmultiplied(0x0B, 0x0F, 0x14, (0.55 * 255.0) as u8);
        let dark = Color32::from_rgb(0x0B, 0x0F, 0x14);
        for path in self.0 {
            let color = plot::series_color(path.slot);
            let pts: Vec<Pos2> = path
                .coords
                .iter()
                .map(|(lng, lat)| projector.project(walkers::lon_lat(*lng, *lat)).to_pos2())
                .collect();
            if path.points {
                for p in &pts {
                    painter.circle(*p, 4.5, color, Stroke::new(1.5, dark));
                }
                continue;
            }
            painter.add(egui::Shape::line(pts.clone(), Stroke::new(5.0, casing)));
            let core = color.gamma_multiply(0.95);
            painter.add(egui::Shape::line(pts.clone(), Stroke::new(2.4, core)));
            for p in [pts.first(), pts.last()].into_iter().flatten() {
                painter.circle_filled(*p, 1.2, core);
            }
        }
    }
}

impl TrackMap {
    /// Opens the map on the imagery layer for `plot`'s track.
    pub fn new(ctx: &egui::Context, server: &Server, sources: &Value, base_layer: &str, plot: &Plot) -> TrackMap {
        let id = imagery_layer(base_layer, sources);
        let (attribution, max_zoom) = describe(sources, &id);
        let attribution: &'static str = crate::data::intern::intern(&attribution);
        let source = CorvusTiles {
            base: server.url(),
            id,
            attribution,
            max_zoom,
        };
        let options = HttpOptions {
            cache: None,
            ..Default::default()
        };
        TrackMap {
            tiles: Some(HttpTiles::with_options(source, options, ctx.clone())),
            memory: MapMemory::default(),
            attribution,
            credit: Default::default(),
            max_zoom,
            paths: track_paths(plot),
            fitted: false,
            hint_at: f64::NEG_INFINITY,
        }
    }

    /// The key under the map: a dot and a name per series, in the chart's colours.
    pub fn key(&self) -> Vec<(usize, &str)> {
        self.paths.iter().map(|p| (p.slot, p.name.as_str())).collect()
    }

    /// Draws the map into `rect`, the whole of `.review-track-map`.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, id: usize) {
        let radius = style::RADIUS_BTN;
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius), style::surface_2());
        let inner = rect.shrink(1.0);
        if self.paths.is_empty() {
            let g = style::galley(
                ui,
                "This track has no points that can be placed on a map.",
                13.0,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            ui.painter()
                .galley(Pos2::new(inner.left(), inner.top() + 4.0), g, style::text_3());
            return;
        }
        if !self.fitted {
            self.fitted = true;
            if let Some(b) = bounds(&self.paths) {
                let (lng, lat, zoom) = fit(
                    b,
                    f64::from(inner.width()),
                    f64::from(inner.height()),
                    fit_max_zoom(self.max_zoom),
                );
                let _ = self.memory.set_zoom(zoom + WALKERS_ZOOM_OFFSET);
                self.memory.center_at(walkers::lon_lat(lng, lat));
            }
        }
        let centre = walkers::lon_lat(0.0, 0.0);
        let map = walkers::Map::new(
            self.tiles.as_mut().map(|t| t as &mut dyn walkers::Tiles),
            &mut self.memory,
            centre,
        )
        .with_plugin(Lines(&self.paths))
        .zoom_with_ctrl(true)
        .panning(false)
        .double_click_to_zoom(true);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        child.set_clip_rect(inner);
        let response = child.add(map);
        let now = ui.input(|i| i.time);
        let over = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| inner.contains(p));
        let plain_wheel =
            ui.input(|i| i.smooth_scroll_delta != Vec2::ZERO && !(i.modifiers.ctrl || i.modifiers.command));
        if over && plain_wheel {
            self.hint_at = now;
        }
        let painter = ui.painter_at(inner);
        self.controls(ui, &painter, inner, id);
        if response.dragged() {
            self.credit.fold();
        }
        self.credit.show(ui, inner, self.attribution, 0.0, "trackmap");
        let opacity = hint_opacity(now - self.hint_at);
        if opacity > 0.0 {
            painter.rect_filled(inner, 0, Color32::from_black_alpha((0.4 * opacity * 255.0) as u8));
            let text = if cfg!(target_os = "macos") {
                "Use \u{2318} + scroll to zoom the map"
            } else {
                "Use Ctrl + scroll to zoom the map"
            };
            let g = style::galley(
                ui,
                text,
                19.6,
                Weight::W400,
                Color32::WHITE.gamma_multiply(opacity),
                0.0,
            );
            painter.galley(inner.center() - g.size() / 2.0, g, Color32::WHITE);
            ui.ctx().request_repaint();
        }
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(style::RADIUS_BTN),
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        if response.dragged() || self.memory.animating() {
            ui.ctx().request_repaint();
        }
    }

    /// The zoom buttons of MapLibre's navigation control, top right: 29 px
    /// white buttons in a group with a 2 px ring, 10 px from the corner.
    fn controls(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, inner: Rect, id: usize) {
        let group = Rect::from_min_size(
            Pos2::new(inner.right() - 10.0 - 29.0, inner.top() + 10.0),
            Vec2::new(29.0, 58.0),
        );
        painter.rect_stroke(
            group.expand(1.0),
            5,
            Stroke::new(2.0, Color32::from_black_alpha(26)),
            egui::StrokeKind::Outside,
        );
        painter.rect_filled(group, 4, Color32::WHITE);
        let ink = Color32::from_rgb(0x33, 0x33, 0x33);
        for (i, plus) in [true, false].into_iter().enumerate() {
            let r = Rect::from_min_size(
                Pos2::new(group.left(), group.top() + 29.0 * i as f32),
                Vec2::splat(29.0),
            );
            let resp = ui.interact(r, ui.scope_id().with(("trackzoom", id, i)), egui::Sense::click());
            if resp.hovered() {
                painter.rect_filled(
                    r,
                    if plus {
                        CornerRadius {
                            nw: 4,
                            ne: 4,
                            sw: 0,
                            se: 0,
                        }
                    } else {
                        CornerRadius {
                            nw: 0,
                            ne: 0,
                            sw: 4,
                            se: 4,
                        }
                    },
                    Color32::from_black_alpha(13),
                );
            }
            let bar = |x0: f32, y0: f32, x1: f32, y1: f32| {
                painter.rect_filled(
                    Rect::from_min_max(r.min + Vec2::new(x0, y0), r.min + Vec2::new(x1, y1)),
                    1.5,
                    ink,
                );
            };
            bar(8.5, 13.0, 20.5, 16.0);
            if plus {
                bar(13.0, 8.5, 16.0, 20.5);
            }
            if resp.clicked() {
                let _ = if plus {
                    self.memory.zoom_in()
                } else {
                    self.memory.zoom_out()
                };
            }
        }
        painter.hline(
            egui::Rangef::new(group.left(), group.right()),
            group.top() + 29.0,
            Stroke::new(1.0, Color32::from_rgb(0xDD, 0xDD, 0xDD)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} vs {b}");
    }

    fn catalogue() -> Value {
        json!({"sources": [
            {"id": "satellite", "provider": "esri", "style": "satellite", "maxzoom": 19},
            {"id": "streets", "provider": "esri", "style": "streets", "maxzoom": 19},
            {"id": "osm", "provider": "osm", "style": "streets", "maxzoom": 19},
            {"id": "google_streets", "provider": "google", "style": "streets", "maxzoom": 20},
            {"id": "google_satellite", "provider": "google", "style": "satellite", "maxzoom": 20},
            {"id": "google_hybrid", "provider": "google", "style": "hybrid", "maxzoom": 20},
            {"id": "maptiler_streets", "provider": "maptiler", "style": "streets", "maxzoom": 20}
        ]})
    }

    #[test]
    fn the_origin_maps_to_itself() {
        assert_eq!(to_lng_lat((47.4, 8.5), 0.0, 0.0), Some((8.5, 47.4)));
        assert_eq!(to_lng_lat((47.4, 8.5), f64::NAN, 1.0), None);
    }

    #[test]
    fn local_points_are_put_back_on_the_globe_as_the_web_does() {
        // values the web `toLngLat` answered for these inputs
        let (lng, lat) = to_lng_lat((47.397742, 8.545594), 100.0, 200.0).unwrap();
        close(lng, WEB_100_200.0, 1e-9);
        close(lat, WEB_100_200.1, 1e-9);
        let (lng, lat) = to_lng_lat((47.397742, 8.545594), -1500.0, 800.0).unwrap();
        close(lng, WEB_M1500_800.0, 1e-9);
        close(lat, WEB_M1500_800.1, 1e-9);
        let (lng, lat) = to_lng_lat((-33.9, 151.2), 30000.0, -42000.0).unwrap();
        close(lng, WEB_SYDNEY.0, 1e-9);
        close(lat, WEB_SYDNEY.1, 1e-9);
        let (lng, lat) = to_lng_lat((89.0, 10.0), 5000.0, -3000.0).unwrap();
        close(lng, WEB_POLAR.0, 1e-9);
        close(lat, WEB_POLAR.1, 1e-9);
    }

    const WEB_100_200: (f64, f64) = (8.546922624157325, 47.39954063553668);
    const WEB_M1500_800: (f64, f64) = (8.525662596764107, 47.40493484570709);
    const WEB_SYDNEY: (f64, f64) = (151.5265011798182, -34.277284922785846);
    const WEB_POLAR: (f64, f64) = (12.507216420575016, 88.97203650802317);

    #[test]
    fn an_imagery_choice_is_kept_and_a_street_map_gives_its_providers_satellite() {
        let c = catalogue();
        assert_eq!(imagery_layer("google_hybrid", &c), "google_hybrid");
        assert_eq!(imagery_layer("satellite", &c), "satellite");
        assert_eq!(imagery_layer("google_streets", &c), "google_satellite");
        assert_eq!(imagery_layer("streets", &c), "satellite");
        assert_eq!(imagery_layer("osm", &c), "satellite");
        // a provider with no satellite layer, and an id nobody knows
        assert_eq!(imagery_layer("maptiler_streets", &c), "satellite");
        assert_eq!(imagery_layer("nothing", &c), "satellite");
    }

    #[test]
    fn the_fit_leaves_the_padding_and_stops_at_the_deepest_tile() {
        let b = [8.54, 47.39, 8.55, 47.40];
        let (lng, lat, z) = fit(b, 800.0, 460.0, 18.0);
        close(lng, 8.545, 1e-9);
        close(lat, 47.395, 1e-3);
        assert!(z > 12.0 && z < 18.0, "{z}");
        // a single point sits at the cap
        let (_, _, z) = fit([8.5, 47.4, 8.5, 47.4], 800.0, 460.0, 18.0);
        close(z, 18.0, 1e-12);
        assert_eq!(fit_max_zoom(0), 18.0);
        assert_eq!(fit_max_zoom(17), 17.0);
        assert_eq!(fit_max_zoom(22), 18.0);
    }

    #[test]
    fn a_series_with_no_placeable_point_is_left_out_and_slots_follow_the_chart() {
        let plot = Plot {
            title: String::new(),
            group: String::new(),
            unit: String::new(),
            xlabel: "East".into(),
            ylabel: "North".into(),
            equal: true,
            threshold: None,
            note: String::new(),
            origin: Some((47.0, 8.0)),
            series: vec![
                plot::Series {
                    name: "Estimate".into(),
                    x: vec![0.0, 10.0],
                    y: vec![0.0, 10.0],
                    markers: false,
                    step: false,
                },
                plot::Series {
                    name: "Empty".into(),
                    x: vec![f64::NAN],
                    y: vec![f64::NAN],
                    markers: false,
                    step: false,
                },
                plot::Series {
                    name: "Setpoint".into(),
                    x: vec![5.0],
                    y: vec![5.0],
                    markers: true,
                    step: false,
                },
            ],
        };
        assert!(mappable(&plot));
        let paths = track_paths(&plot);
        assert_eq!(paths.len(), 2);
        assert_eq!((paths[0].slot, paths[1].slot), (0, 2));
        assert!(paths[1].points);
        let mut none = plot.clone();
        none.origin = None;
        assert!(!mappable(&none));
    }

    #[test]
    fn the_zoom_hint_holds_then_fades() {
        assert_eq!(hint_opacity(-1.0), 0.0);
        assert_eq!(hint_opacity(0.5), 1.0);
        assert!((hint_opacity(1.6) - 0.5).abs() < 1e-6);
        assert_eq!(hint_opacity(3.0), 0.0);
    }
}
