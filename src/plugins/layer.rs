//! The plugins' shapes on the flat map: painted in two passes (areas under
//! the track, marks over everything) and answering the pointer for markers,
//! buttons and `pick_point`.

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
    pub fn marks(&mut self, ui: &mut egui::Ui, response: &egui::Response, projector: &Projector, rect: Rect) {
        let project = |p: [f64; 2]| Some(to_screen(projector, rect, p));
        let mut hot: Option<String> = None;
        let mut taken = false;
        for s in self.shapes.iter().filter(|s| s.visible) {
            let (at, area, draggable, clickable) = match &s.kind {
                Kind::Marker {
                    at,
                    size,
                    draggable,
                    clickable,
                    ..
                } => (
                    *at,
                    shapes::marker_rect(to_screen(projector, rect, *at), *size),
                    *draggable,
                    *clickable,
                ),
                Kind::Button {
                    at,
                    text,
                    size,
                    disabled,
                    ..
                } if !disabled => (
                    *at,
                    shapes::button_rect(ui, to_screen(projector, rect, *at), text, *size),
                    false,
                    true,
                ),
                _ => continue,
            };
            let _ = at;
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
                if let Some(p) = h.interact_pointer_pos() {
                    let c = projector.unproject(p.to_vec2());
                    self.events
                        .push((s.owner, MapEvent::Drag(s.key.clone(), [c.x(), c.y()])));
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                taken = true;
            }
            if h.drag_stopped() {
                if let Some(p) = h
                    .interact_pointer_pos()
                    .or_else(|| ui.input(|i| i.pointer.latest_pos()))
                {
                    let c = projector.unproject(p.to_vec2());
                    self.events
                        .push((s.owner, MapEvent::DragEnd(s.key.clone(), [c.x(), c.y()])));
                }
                taken = true;
            } else if h.clicked() {
                self.events.push((s.owner, MapEvent::Click(s.key.clone())));
                taken = true;
            }
        }
        shapes::paint_marks(ui, &ui.painter_at(rect), &project, &self.shapes, hot.as_deref());
        if let Some(owner) = self.pick {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if response.secondary_clicked() || escape {
                self.pick = None;
                self.events.push((owner, MapEvent::PickCancelled));
            } else if response.clicked() && !taken {
                if let Some(p) = response.interact_pointer_pos() {
                    let c = projector.unproject(p.to_vec2());
                    self.pick = None;
                    self.events.push((owner, MapEvent::Picked([c.x(), c.y()])));
                }
            }
        }
    }
}

fn to_screen(projector: &Projector, _rect: Rect, p: [f64; 2]) -> Pos2 {
    let v = projector.project(walkers::lon_lat(p[0], p[1]));
    v.to_pos2().clamp(Pos2::new(-1e6, -1e6), Pos2::new(1e6, 1e6))
}
