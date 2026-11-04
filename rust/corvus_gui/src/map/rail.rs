//! The map's own controls (src/js/map.js buildControls, map.css
//! `.map-controls`, `.tiles-trigger`, `.map-search`): the rail at the top
//! right with zoom, centre and the layer list, and the search and offline
//! map buttons beside it.
//!
//! Zoom, centre (which resumes following) and the layers work. Downloaded
//! areas, 3D, measuring, place search and the offline map download are
//! drawn where the web map has them and come with their own steps.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::map::Map;
use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// `--map-rail-inset`, `--map-rail-gap`, and a button's side.
const INSET: f32 = 14.0;
const GAP: f32 = 8.0;
const BTN: f32 = 34.0;

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
        )
    }
}

/// The rail, top to bottom; the ruler only while `ui.measure_tool` is on.
/// The camera group stays hidden until cameras are set up.
pub fn items(measure: bool, planner: bool) -> Vec<Item> {
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
        let list = items(self.measure, self.planner);
        let h = 12.0
            + list
                .iter()
                .map(|i| if *i == Item::Divider { 1.0 + 4.0 } else { BTN })
                .sum::<f32>()
            + 6.0 * (list.len() as f32 - 1.0);
        let rail = Rect::from_min_size(
            Pos2::new(area.right() - INSET - (BTN + 12.0), area.top() + INSET),
            Vec2::new(BTN + 12.0 + 2.0, h + 2.0),
        );
        glass_box(&painter, ("maprail", 0), rail);
        ui.interact(rail, ui.scope_id().with("mapctl"), egui::Sense::click_and_drag());
        let mut y = rail.top() + 7.0;
        let x = rail.left() + 7.0;
        let mut layers_anchor = None;
        let mut three_anchor = None;
        for item in list {
            if item == Item::Divider {
                painter.hline(
                    egui::Rangef::new(x + 4.0, x + BTN - 4.0),
                    y + 2.0 + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                y += 5.0 + 6.0;
                continue;
            }
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(BTN));
            let (icon, title) = item.icon();
            let works = item.works();
            let title = if item == Item::Center && self.planner {
                "Go to the aircraft".to_string()
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
                .on_hover_text(title);
            let active = (item == Item::Center && map.follow && !self.planner)
                || (item == Item::Layers && self.layers_open)
                || (item == Item::Regions && map.regions_visible)
                || (item == Item::Measure && map.measure.active)
                || (item == Item::ThreeD && map.three_d());
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
                    _ => {}
                }
            }
            if item == Item::Layers {
                layers_anchor = Some(r);
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
            y += BTN + 6.0;
        }

        // .tiles-trigger right of the search, both left of the rail
        let clear = INSET + 48.0 + GAP;
        let download = Rect::from_min_size(
            Pos2::new(area.right() - clear - BTN, area.top() + INSET),
            Vec2::splat(BTN),
        );
        glass_box(&painter, ("trigger", 0), download);
        let dresp = ui
            .interact(download, ui.scope_id().with(("trigger", 0)), egui::Sense::click())
            .on_hover_text("Download offline map");
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
        let search = download.translate(Vec2::new(-(BTN + GAP), 0.0));
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
        if map.track.has_line() && clear_disc(ui, area, 0.0, "Clear the flown track") {
            map.track.clear();
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
            .on_hover_text(title);
        let hot = resp.hovered();
        let painter = ui.painter().clone();
        style::glass_panel(&painter, "trackclear", r, 255u8, 0.75);
        let (color, alpha) = if hot {
            (style::critical(), 1.0)
        } else {
            (style::text_2(), 0.85)
        };
        if hot {
            painter.rect_filled(r, 255u8, style::critical().gamma_multiply(0.14));
            painter.rect_stroke(
                r,
                255u8,
                Stroke::new(1.0, style::critical().gamma_multiply(0.45)),
                egui::StrokeKind::Inside,
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
        let layers = map.layers();
        let row_h = 7.0 * 2.0 + 12.0 * 1.21;
        let head_h = 4.0 + 10.0 * 1.21 + 6.0;
        let mut providers: Vec<String> = Vec::new();
        for (_, _, p) in &layers {
            if !providers.contains(p) {
                providers.push(p.clone());
            }
        }
        let h = 10.0 + providers.len() as f32 * head_h + layers.len() as f32 * row_h;
        let w = 220.0;
        let menu = Rect::from_min_size(
            Pos2::new(anchor.left() - 7.0 - 8.0 - w, anchor.top() - 7.0),
            Vec2::new(w, h),
        );
        let ctx = ui.ctx().clone();
        let mut picked = None;
        let mut close = false;
        egui::Area::new(ui.scope_id().with("layers"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.min)
            .show(&ctx, |ui| {
                let painter = ui.painter().clone();
                style::glass_panel(&painter, "layers", menu, style::RADIUS, 0.6);
                ui.allocate_rect(menu, egui::Sense::hover());
                let mut y = menu.top() + 5.0;
                for prov in &providers {
                    let g = style::galley(ui, &prov.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.8);
                    painter.galley(Pos2::new(menu.left() + 5.0 + 6.0, y + 4.0), g, style::text_3());
                    y += head_h;
                    for (id, label, p) in layers.iter().filter(|(_, _, p)| p == prov) {
                        let _ = p;
                        let r = Rect::from_min_size(Pos2::new(menu.left() + 5.0, y), Vec2::new(w - 10.0, row_h));
                        let resp = ui.interact(r, ui.scope_id().with(("layer", id.as_str())), egui::Sense::click());
                        let active = *id == map.source;
                        if active {
                            painter.rect_filled(r, style::RADIUS_BTN, style::accent_dark());
                        } else if resp.hovered() {
                            painter.rect_filled(r, style::RADIUS_BTN, style::tint(0.06));
                        }
                        let color = if active || resp.hovered() {
                            style::text_1()
                        } else {
                            style::text_2()
                        };
                        let g = style::galley(ui, label, 12.0, Weight::W400, color, 0.0);
                        painter.galley(Pos2::new(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, color);
                        if resp.clicked() {
                            picked = Some(id.clone());
                        }
                        y += row_h;
                    }
                }
                let outside = ui.input(|i| i.pointer.any_pressed())
                    && !ui
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|p| menu.contains(p) || anchor.contains(p));
                close = outside || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        if let Some(id) = picked {
            map.set_source(&ctx, &id);
            self.layers_open = false;
        } else if close {
            self.layers_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rail_as_map_js_builds_it() {
        assert_eq!(items(true, false).len(), 8);
        assert_eq!(*items(false, false).last().unwrap(), Item::ThreeD);
        assert_eq!(items(true, false)[3], Item::Divider);
        assert_eq!(
            items(true, true),
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
}
