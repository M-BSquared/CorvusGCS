//! The plugins' shapes on the map: painted in two passes (areas under the
//! track, marks over everything) and answering the pointer for markers,
//! buttons and `pick_point`, on the flat map and on the tilted one alike.

use eframe::egui::{self, Pos2, Rect};
use walkers::Projector;

use super::shapes::{self, Kind, Shape};
use super::MapEvent;

/// Every plugin's shapes, flat, with what the pointer has done to them.
#[derive(Default)]
pub struct Layer {
    shapes: Vec<Shape>,
    /// The plugin waiting for a click on the map.
    pub pick: Option<&'static str>,
    events: Vec<(&'static str, MapEvent)>,
}

impl Layer {
    pub fn set(&mut self, shapes: Vec<Shape>) {
        self.shapes = shapes;
    }

    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// What happened since the last call, for the registry to hand on.
    pub fn take_events(&mut self) -> Vec<(&'static str, MapEvent)> {
        std::mem::take(&mut self.events)
    }

    /// Lines and areas, under the flown track.
    pub fn paint_areas(&self, painter: &egui::Painter, projector: &Projector, rect: Rect) {
        let project = |p: [f64; 2]| Some(to_screen(projector, rect, p));
        shapes::paint_areas(painter, &project, &self.shapes);
    }

    /// Text, markers and buttons over the aircraft, and the pointer on them.
    /// True when a press on the map was meant for a plugin.
    pub fn marks(&mut self, ui: &mut egui::Ui, response: &egui::Response, projector: &Projector, rect: Rect) -> bool {
        let project = |p: [f64; 2]| Some(to_screen(projector, rect, p));
        let unproject = |p: Pos2| {
            let c = projector.unproject(p.to_vec2());
            Some([c.x(), c.y()])
        };
        let clicked = response.clicked().then(|| response.interact_pointer_pos()).flatten();
        self.marks_with(ui, rect, &project, &unproject, clicked, response.secondary_clicked())
    }

    /// The same on any map: `project` gives where a place is on the screen
    /// (`None` where it is not seen, behind a hill or the camera) and
    /// `unproject` the place under a screen point (`None` for the sky).
    /// `clicked` is where a press-and-release landed on the map itself.
    pub fn marks_with(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        project: shapes::Project<'_>,
        unproject: &dyn Fn(Pos2) -> Option<[f64; 2]>,
        clicked: Option<Pos2>,
        secondary_clicked: bool,
    ) -> bool {
        let mut hot: Option<String> = None;
        let mut taken = false;
        for s in self.shapes.iter().filter(|s| s.visible) {
            let (at, draggable, clickable) = match &s.kind {
                Kind::Marker {
                    at,
                    draggable,
                    clickable,
                    ..
                } => (*at, *draggable, *clickable),
                Kind::Button { at, disabled, .. } if !disabled => (*at, false, true),
                _ => continue,
            };
            let Some(on_screen) = project(at) else { continue };
            let area = match &s.kind {
                Kind::Marker { size, .. } => shapes::marker_rect(on_screen, *size),
                Kind::Button { text, size, .. } => shapes::button_rect(ui, on_screen, text, *size),
                _ => continue,
            };
            if !(draggable || clickable) || !rect.intersects(area) {
                continue;
            }
            let sense = if draggable {
                egui::Sense::click_and_drag()
            } else {
                egui::Sense::click()
            };
            let h = ui.interact(area, ui.scope_id().with(("plugin-shape", s.owner, &s.key)), sense);
            if h.hovered() {
                hot = Some(s.key.clone());
                ui.ctx().set_cursor_icon(if draggable {
                    egui::CursorIcon::Grab
                } else {
                    egui::CursorIcon::PointingHand
                });
            }
            if h.dragged() {
                if let Some(c) = h.interact_pointer_pos().and_then(unproject) {
                    self.events.push((s.owner, MapEvent::Drag(s.key.clone(), c)));
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                taken = true;
            }
            if h.drag_stopped() {
                // Let go over the sky: the marker stays where the last drag put it.
                let end = h
                    .interact_pointer_pos()
                    .or_else(|| ui.input(|i| i.pointer.latest_pos()))
                    .and_then(unproject)
                    .unwrap_or(at);
                self.events.push((s.owner, MapEvent::DragEnd(s.key.clone(), end)));
                taken = true;
            } else if h.clicked() {
                self.events.push((s.owner, MapEvent::Click(s.key.clone())));
                taken = true;
            }
        }
        shapes::paint_marks(ui, &ui.painter_at(rect), project, &self.shapes, hot.as_deref());
        if let Some(owner) = self.pick {
            if ui.rect_contains_pointer(rect) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if secondary_clicked || escape {
                self.pick = None;
                self.events.push((owner, MapEvent::PickCancelled));
                taken = true;
            } else if let (false, Some(c)) = (taken, clicked.and_then(unproject)) {
                self.pick = None;
                self.events.push((owner, MapEvent::Picked(c)));
                taken = true;
            }
        }
        taken
    }
}

fn to_screen(projector: &Projector, _rect: Rect, p: [f64; 2]) -> Pos2 {
    let v = projector.project(walkers::lon_lat(p[0], p[1]));
    v.to_pos2().clamp(Pos2::new(-1e6, -1e6), Pos2::new(1e6, 1e6))
}
