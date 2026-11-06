//! The map's own controls (python/src/js/map.js buildControls, map.css
//! `.map-controls`, `.tiles-trigger`, `.map-search`): the rail at the top
//! right with zoom, centre and the layer list, and the search and offline
//! map buttons beside it.
//!
//! Zoom, centre (which resumes following) and the layers work. Downloaded
//! areas, 3D, measuring, place search and the offline map download are
//! drawn where the web map has them and come with their own steps.

use crate::ui::tip::HoverTr;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::map::Map;
use crate::ui::card::{self, Look};
use crate::ui::icons;
use crate::ui::menu;
use crate::ui::style;

/// `--map-rail-inset`, `--map-rail-gap`, and a button's side.
const INSET: f32 = 14.0;
const GAP: f32 = 8.0;
const BTN: f32 = 34.0;

/// A rail button's side: 34 px, or `--touch-target` in tablet mode.
pub fn btn() -> f32 {
    if crate::app::touch::on() {
        crate::app::touch::TARGET
    } else {
        BTN
    }
}

/// One slot of the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    ZoomIn,
    ZoomOut,
    /// The planner's: frame the whole mission.
    Fit,
    Center,
    Divider,
    Layers,
    Regions,
    ThreeD,
    Measure,
    /// The camera windows; there once a camera is set up.
    Camera,
}

impl Item {
    fn icon(self) -> (&'static str, &'static str) {
        match self {
            Item::ZoomIn => ("plus", "Zoom in"),
            Item::ZoomOut => ("minus", "Zoom out"),
            Item::Fit => ("scan", "Fit the whole mission"),
            Item::Center => ("crosshair", "Center on vehicle (resume following)"),
            Item::Layers => ("layers", "Map layers"),
            Item::Regions => ("frame", "Show downloaded areas"),
            Item::ThreeD => ("box", "3D mode"),
            Item::Measure => ("ruler", "Measure a distance (click two points)"),
            Item::Camera => ("video", "Camera windows (set up under Setup, Video)"),
            Item::Divider => ("", ""),
        }
    }

    fn works(self) -> bool {
        matches!(
            self,
            Item::ZoomIn
                | Item::ZoomOut
                | Item::Fit
                | Item::Center
                | Item::Layers
                | Item::Regions
                | Item::Measure
                | Item::ThreeD
                | Item::Camera
        )
    }
}

/// The rail, top to bottom; the ruler only while `ui.measure_tool` is on.
/// The camera group stays hidden until cameras are set up.
pub fn items(measure: bool, planner: bool, camera: bool) -> Vec<Item> {
    let mut v = items_all(measure, planner, camera);
    if crate::app::touch::on() {
        // Zooming is two fingers on the map itself.
        v.retain(|i| !matches!(i, Item::ZoomIn | Item::ZoomOut));
    }
    v
}

fn items_all(measure: bool, planner: bool, camera: bool) -> Vec<Item> {
    if planner {
        return vec![
            Item::ZoomIn,
            Item::ZoomOut,
            Item::Fit,
            Item::Divider,
            Item::Center,
            Item::Layers,
            Item::Regions,
        ];
    }
    let mut v = vec![
        Item::ZoomIn,
        Item::ZoomOut,
        Item::Center,
        Item::Divider,
        Item::Layers,
        Item::Regions,
        Item::ThreeD,
    ];
    if measure {
        v.push(Item::Measure);
    }
    if camera {
        v.extend([Item::Divider, Item::Camera]);
    }
    v
}

/// The rail's state.
#[derive(Default)]
pub struct MapRail {
    search: crate::map::search_card::SearchCard,
    tiles: crate::map::tiles_card::TilesCard,
    layers_open: bool,
    three_panel: bool,
    three_hover: Option<f64>,
    pub measure: bool,
    /// The mission planner's rail: Fit instead of 3D and the ruler, and the
    /// centre button goes to the aircraft without following it.
    pub planner: bool,
    /// Set by the Fit button; the page that owns the plan takes it.
    pub fit_requested: bool,
    /// A word for the operator, set when the centre button had no fix to go to.
    pub notice: Option<&'static str>,
    /// The cameras the button opens, and which have a window.
    pub cameras: crate::map::cameras::Cameras,
    pub open_cameras: Vec<String>,
}

/// The aircraft's position, when it has a real fix.
fn aircraft_fix(state: &Value) -> Option<(f64, f64)> {
    let a = state.get("position")?.as_array()?;
    let (lng, lat) = (a.first()?.as_f64()?, a.get(1)?.as_f64()?);
    (lng.is_finite() && lat.is_finite() && (lng != 0.0 || lat != 0.0)).then_some((lng, lat))
}

fn glass_box(painter: &egui::Painter, key: (&str, usize), r: Rect) {
    style::glass_panel(painter, key, r, style::RADIUS, 0.55);
}

impl MapRail {
    /// Draws the rail and the two buttons inside `area` (the map view).
    pub fn show(&mut self, ui: &mut egui::Ui, area: Rect, map: &mut Map, state: &Value) {
        let painter = ui.painter().clone();
        let list = items(self.measure, self.planner, !self.cameras.list.is_empty());
        let h = 12.0
            + list
                .iter()
                .map(|i| if *i == Item::Divider { 1.0 + 4.0 } else { btn() })
                .sum::<f32>()
            + 6.0 * (list.len() as f32 - 1.0);
        let rail = Rect::from_min_size(
            Pos2::new(area.right() - INSET - (btn() + 12.0), area.top() + INSET),
            Vec2::new(btn() + 12.0 + 2.0, h + 2.0),
        );
        glass_box(&painter, ("maprail", 0), rail);
        ui.interact(rail, ui.scope_id().with("mapctl"), egui::Sense::click_and_drag());
        let mut y = rail.top() + 7.0;
        let x = rail.left() + 7.0;
        let mut layers_anchor = None;
        let mut three_anchor = None;
        let mut camera_anchor = None;
        let mut camera_hover = false;
        for item in list {
            if item == Item::Divider {
                painter.hline(
                    egui::Rangef::new(x + 4.0, x + btn() - 4.0),
                    y + 2.0 + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                y += 5.0 + 6.0;
                continue;
            }
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(btn()));
            let (icon, title) = item.icon();
            let works = item.works();
            let title = if item == Item::Center && self.planner {
                "Go to the aircraft".to_string()
            } else if item == Item::Camera {
                self.cameras.title()
            } else if works {
                title.to_string()
            } else {
                format!("{title} (not in this build yet)")
            };
            let sense = if works {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let resp = ui
                .interact(r, ui.scope_id().with(("mc", icon)), sense)
                .on_hover_tr(title);
            let active = (item == Item::Center && map.follow && !self.planner)
                || (item == Item::Layers && self.layers_open)
                || (item == Item::Regions && map.regions_visible)
                || (item == Item::Measure && map.measure.active)
                || (item == Item::ThreeD && map.three_d())
                || (item == Item::Camera && !self.open_cameras.is_empty());
            let (fill, color) = if active {
                (style::accent_dark(), style::text_1())
            } else if resp.hovered() && works {
                (style::surface_hover(), style::text_1())
            } else {
                (Color32::TRANSPARENT, style::text_2())
            };
            let alpha = if works { 1.0 } else { 0.4 };
            painter.rect_filled(r, style::RADIUS_BTN, fill);
            icons::paint(
                ui,
                icon,
                Rect::from_center_size(r.center(), Vec2::splat(17.0)),
                color.gamma_multiply(alpha),
            );
            if resp.clicked() {
                match item {
                    Item::ZoomIn => map.zoom(1),
                    Item::ZoomOut => map.zoom(-1),
                    Item::Fit => self.fit_requested = true,
                    Item::Center if self.planner => match aircraft_fix(state) {
                        Some((lng, lat)) => map.go_to(lng, lat, 16.0),
                        None => self.notice = Some("The aircraft has not reported a position."),
                    },
                    Item::Center => map.recenter(state),
                    Item::Layers => self.layers_open = !self.layers_open,
                    Item::ThreeD => {
                        let on = !map.three_d();
                        map.set_three_d(on);
                    }
                    Item::Measure => {
                        let on = !map.measure.active;
                        map.measure.set_active(on);
                    }
                    Item::Regions => {
                        let on = !map.regions_visible;
                        map.set_regions_visible(on);
                    }
                    Item::Camera => self.cameras.press(),
                    _ => {}
                }
            }
            if item == Item::Layers {
                layers_anchor = Some(r);
            }
            if item == Item::Camera {
                camera_anchor = Some(r);
                camera_hover = resp.hovered();
                if resp.secondary_clicked() {
                    self.cameras.list_open = self.cameras.list.len() > 1;
                }
            }
            if item == Item::ThreeD {
                three_anchor = Some(r);
                let now = ui.input(|i| i.time);
                if resp.hovered() {
                    let since = *self.three_hover.get_or_insert(now);
                    if now - since > 0.6 {
                        self.three_panel = true;
                    } else {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                    }
                } else {
                    self.three_hover = None;
                }
                if resp.secondary_clicked() {
                    self.three_panel = true;
                }
            }
            y += btn() + 6.0;
        }

        if let Some(anchor) = camera_anchor {
            let now = ui.input(|i| i.time);
            let open = self.open_cameras.clone();
            let list_rect = self.cameras.menu(ui, anchor, &open);
            let over_list = list_rect.is_some_and(|r| ui.rect_contains_pointer(r));
            self.cameras.hover(camera_hover, over_list, now, ui.ctx());
        }
        // .tiles-trigger right of the search, both left of the rail
        let clear = INSET + btn() + 14.0 + GAP;
        let download = Rect::from_min_size(
            Pos2::new(area.right() - clear - btn(), area.top() + INSET),
            Vec2::splat(btn()),
        );
        glass_box(&painter, ("trigger", 0), download);
        let dresp = ui
            .interact(download, ui.scope_id().with(("trigger", 0)), egui::Sense::click())
            .on_hover_tr("Download offline map");
        let (dfill, dcolor) = if self.tiles.open {
            (style::accent_dark(), style::accent_bright())
        } else if dresp.hovered() {
            (style::surface_hover(), style::text_1())
        } else {
            (Color32::TRANSPARENT, style::text_2())
        };
        painter.rect_filled(download, style::RADIUS, dfill);
        icons::paint(
            ui,
            "download",
            Rect::from_center_size(download.center(), Vec2::splat(17.0)),
            dcolor,
        );
        if dresp.clicked() {
            if self.tiles.open {
                self.tiles.close_dialog();
            } else {
                self.tiles.open(ui.ctx(), map, area);
            }
        }
        let search = download.translate(Vec2::new(-(btn() + GAP), 0.0));
        let units = map.measure.units;
        self.search.show(ui, search, area, map, &units);

        if self.three_panel {
            if let Some(anchor) = three_anchor {
                let ctx = ui.ctx().clone();
                let (t, sources) = map.three_settings(&ctx);
                let out = crate::map3d::panel::show(ui, anchor, t, sources);
                if out.close {
                    self.three_panel = false;
                }
            }
        }
        if !self.planner {
            self.track_clear(ui, area, map);
        }
        self.tiles.show(ui.ctx(), area, map);

        if self.layers_open {
            if let Some(anchor) = layers_anchor {
                self.layers_menu(ui, anchor, map);
            }
        }
    }

    /// `.track-clear`: a quiet 25 px disc above the attribution, only while
    /// there is a track to clear.
    fn track_clear(&mut self, ui: &mut egui::Ui, area: Rect, map: &mut Map) {
        if map.has_any_track() && clear_disc(ui, area, 0.0, "Clear the flown track") {
            map.clear_tracks();
        }
    }
}

/// `.track-clear`: a quiet 25 px disc above the attribution, `lift` above
/// where it usually sits. True when pressed.
pub fn clear_disc(ui: &mut egui::Ui, area: Rect, lift: f32, title: &str) -> bool {
    {
        let r = Rect::from_min_size(
            Pos2::new(area.left() + INSET, area.bottom() - lift - INSET - 30.0 - 25.0),
            Vec2::splat(25.0),
        );
        let resp = ui
            .interact(r, ui.scope_id().with("trackclear"), egui::Sense::click())
            .on_hover_tr(title);
        let hot = resp.hovered();
        let painter = ui.painter().clone();
        style::glass_panel(&painter, "trackclear", r, 255u8, 0.75);
        let (color, alpha) = if hot {
            (style::critical(), 1.0)
        } else {
            (style::text_2(), 0.85)
        };
        if hot {
            card::paint(
                &painter,
                r,
                Look::custom(
                    style::critical().gamma_multiply(0.14),
                    style::critical().gamma_multiply(0.45),
                    255u8,
                ),
            );
        }
        icons::paint(
            ui,
            "trash-2",
            Rect::from_center_size(r.center(), Vec2::splat(14.0)),
            color.gamma_multiply(alpha),
        );
        resp.clicked()
    }
}

impl MapRail {
    /// The layer list, opened to the left of its button: a head per map
    /// service and a row per layer (`.ui-menu`, `.option-item`).
    fn layers_menu(&mut self, ui: &mut egui::Ui, anchor: Rect, map: &mut Map) {
        let items = layer_items(&map.layers());
        let id = ui.scope_id().with("layers");
        let out = menu::Menu::new(id, menu::Anchor::beside_rail(anchor), &menu::Look::LAYERS, &items)
            .selected(&map.source)
            .show(ui.ctx());
        if let Some(id) = out.picked {
            map.set_source(ui.ctx(), &id);
            self.layers_open = false;
        } else if out.dismissed {
            self.layers_open = false;
        }
    }
}

/// `renderLayerRows`: the caption, and a heading above each service's layers
/// when there is more than one service.
fn layer_items(layers: &[(String, String, String)]) -> Vec<menu::Item> {
    let mut providers: Vec<&str> = Vec::new();
    for (_, _, p) in layers {
        if !providers.contains(&p.as_str()) {
            providers.push(p);
        }
    }
    let mut items = vec![menu::Item::Head("Map layers".into())];
    for prov in &providers {
        if providers.len() > 1 {
            items.push(menu::Item::Section((*prov).to_string()));
        }
        for (id, label, _) in layers.iter().filter(|(_, _, p)| p == prov) {
            items.push(menu::Row::new(id.clone(), label.clone()).into());
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rail_as_map_js_builds_it() {
        assert_eq!(items(true, false, false).len(), 8);
        assert_eq!(*items(false, false, false).last().unwrap(), Item::ThreeD);
        assert_eq!(items(true, false, false)[3], Item::Divider);
        assert_eq!(
            items(true, true, false),
            [
                Item::ZoomIn,
                Item::ZoomOut,
                Item::Fit,
                Item::Divider,
                Item::Center,
                Item::Layers,
                Item::Regions
            ],
            "the planner's rail (mission.js MAP_CONTROLS)"
        );
        assert!(Item::Layers.works() && Item::Regions.works() && Item::Measure.works() && Item::ThreeD.works());
    }

    #[test]
    fn the_layer_list_has_a_caption_and_headings_only_with_several_services() {
        let l = |id: &str, p: &str| (id.to_string(), id.to_uppercase(), p.to_string());
        let one = layer_items(&[l("a", "Esri"), l("b", "Esri")]);
        assert_eq!(one.len(), 3);
        assert_eq!(one[0], menu::Item::Head("Map layers".into()));
        let two = layer_items(&[l("a", "Esri"), l("c", "Google"), l("b", "Esri")]);
        assert_eq!(two.len(), 1 + 2 + 3);
        assert_eq!(two[1], menu::Item::Section("Esri".into()));
        assert!(matches!(&two[2], menu::Item::Row(r) if r.value == "a"));
        assert!(
            matches!(&two[3], menu::Item::Row(r) if r.value == "b"),
            "grouped by service"
        );
        assert_eq!(two[4], menu::Item::Section("Google".into()));
    }
}
