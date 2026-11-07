use super::*;

impl MissionPage {
    pub(super) fn commit_circle(&mut self, pts: &[[f64; 2]]) {
        if pts.len() < 2 {
            return;
        }
        let radius = geometry::distance_ll(pts[0], pts[1])
            .max(geometry::PATTERN_CIRCLE_MIN_M)
            .clamp(plan::RADIUS_MIN_M, plan::RADIUS_MAX_M);
        let Some(i) = self
            .plan
            .add_item(Kind::LoiterTurns, Some((pts[0][0], pts[0][1])), None)
        else {
            return;
        };
        let it = &mut self.plan.items[i];
        it.radius = crate::data::units::js_round(radius);
        it.turns = 1.0;
        it.direction = 1.0;
        self.set_tool(Tool::Select);
        self.hint =
            "Circle added. Drag its rim to change the radius, and set the turns and the direction in the panel.".into();
        self.refresh_keep_hint();
    }

    /// `refresh` without losing the sentence a command just put in the hint.
    pub(super) fn refresh_keep_hint(&mut self) {
        let keep = self.hint.clone();
        self.refresh();
        self.hint = keep;
    }

    pub(super) fn pattern_update_hint(&mut self) {
        if matches!(self.tool, Tool::Shape(s) if s != Shape::Circle) && self.pattern_ready() {
            self.hint = ready_hint(crate::app::touch::on()).into();
        } else {
            self.update_hint();
        }
    }

    /// What the pointer did on the map.
    pub(super) fn apply_map(&mut self, out: Out) {
        if let Some(at) = out.hover {
            if matches!(self.tool, Tool::Shape(_)) && !self.draft.pts.is_empty() {
                self.draft.cursor = Some(at);
            }
        }
        if let Some(id) = out.select {
            if self.plan.selected != Some(id) {
                self.plan.selected = Some(id);
            }
        }
        if let Some((id, at)) = out.moved {
            if let Some(it) = self.plan.item_mut(id) {
                it.lon = at[0];
                it.lat = at[1];
            }
        }
        if let Some(at) = out.start_moved {
            let following = self.plan.start_at_vehicle;
            self.plan.start_at_vehicle = false;
            self.plan.place_start(at[0], at[1], true);
            if following {
                self.hint = "The start stays where you put it. AIRCRAFT ties it to the aircraft again.".into();
            }
        }
        if out.dropped || out.start_dropped || out.radius_dropped || out.pattern_dropped {
            self.refresh_keep_hint();
            if !self.hint.is_empty() && !(out.start_dropped && self.plan.start_at_vehicle) {
                self.update_hint_if_idle();
            }
        }
        if let Some((id, r)) = out.radius {
            if let Some(it) = self.plan.item_mut(id) {
                it.radius = r;
            }
        }
        if let Some(c) = out.pattern_grab {
            let group = self
                .plan
                .selected_item()
                .and_then(|i| i.pattern)
                .map(|g| {
                    self.plan
                        .items
                        .iter()
                        .filter(|i| i.pattern == Some(g))
                        .map(|i| (i.id, i.lat, i.lon))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            self.pattern_origin = Some((group, c));
        }
        if let (Some(to), Some((group, c))) = (out.pattern_to, self.pattern_origin.as_ref()) {
            let (d_lon, d_lat) = (to[0] - c[0], to[1] - c[1]);
            let moved: Vec<(u32, f64, f64)> = group
                .iter()
                .map(|(id, lat, lon)| (*id, lat + d_lat, lon + d_lon))
                .collect();
            for (id, lat, lon) in moved {
                if let Some(it) = self.plan.item_mut(id) {
                    it.lat = lat;
                    it.lon = lon;
                }
            }
        }
        if out.pattern_dropped {
            self.pattern_origin = None;
        }
        if let Some(id) = out.remove {
            self.plan.remove_item(id);
            self.refresh();
        }
        if out.start_remove {
            self.plan.remove_start();
            self.refresh();
        }
        if let Some(at) = out.clicked {
            self.map_click(at);
        }
        if out.double_clicked && matches!(self.tool, Tool::Shape(s) if s != Shape::Circle) && self.pattern_ready() {
            self.commit_pattern();
        }
        if out.secondary.is_some() {
            self.map_secondary();
        }
    }

    pub(super) fn update_hint_if_idle(&mut self) {
        if self.tool == Tool::Select {
            self.update_hint();
        }
    }

    /// `onMapClick`.
    pub(super) fn map_click(&mut self, at: [f64; 2]) {
        match self.tool {
            Tool::Select | Tool::Pattern | Tool::VehicleStart => {}
            Tool::Home => {
                self.plan.start_at_vehicle = false;
                let first = self.plan.home.is_none();
                self.plan.place_start(at[0], at[1], false);
                self.set_tool(if first {
                    Tool::Item(Kind::Waypoint)
                } else {
                    Tool::Select
                });
                self.refresh();
            }
            Tool::Item(kind) => {
                if let Some(why) = self.plan.tool_blocked(self.tool, &self.link) {
                    self.set_tool(Tool::Select);
                    self.hint = why;
                    return;
                }
                if self.plan.add_item(kind, Some((at[0], at[1])), None).is_none() {
                    return;
                }
                if kind.ends() {
                    self.set_tool(Tool::Select);
                }
                self.refresh();
            }
            Tool::Shape(shape) => {
                if let Some(why) = self.plan.tool_blocked(self.tool, &self.link) {
                    self.set_tool(Tool::Select);
                    self.hint = why;
                    return;
                }
                self.draft.pts.push(at);
                if shape == Shape::Circle && self.draft.pts.len() >= 2 {
                    self.commit_pattern();
                    return;
                }
                self.pattern_update_hint();
            }
        }
    }

    /// A right click on empty map: undo the last corner, else the newest item.
    pub(super) fn map_secondary(&mut self) {
        if matches!(self.tool, Tool::Shape(_)) && !self.draft.pts.is_empty() {
            self.draft.pts.pop();
            self.pattern_update_hint();
            return;
        }
        let Some(newest) = self.plan.items.iter().map(|i| i.id).max() else {
            return;
        };
        if self.plan.start_takeoff().map(|t| t.id) == Some(newest) {
            self.plan.remove_start();
        } else {
            self.plan.remove_item(newest);
        }
        self.refresh();
    }

    pub(super) fn keys(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Dialog::None) || ctx.egui_wants_keyboard_input() {
            return;
        }
        let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        if pressed(egui::Key::Escape) {
            if self.side.row_drag.is_some() {
                self.side.end_row_drag();
            } else if self.tools.pattern_pinned {
                self.tools.pattern_pinned = false;
            } else if !self.draft.pts.is_empty() {
                self.draft.pts.clear();
                self.draft.cursor = None;
                self.update_hint();
            } else if self.tool != Tool::Select {
                self.set_tool(Tool::Select);
            }
        }
        if pressed(egui::Key::Enter)
            && matches!(self.tool, Tool::Shape(s) if s != Shape::Circle)
            && self.pattern_ready()
        {
            self.commit_pattern();
        }
        if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
            if let Some(id) = self.plan.selected {
                self.plan.remove_item(id);
                self.refresh();
            }
        }
    }

    /// `planBounds`: the box around the plan, orbits widened by their radius.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        let points = geometry::stations(&self.plan.items, self.plan.home.as_ref());
        if points.is_empty() {
            return None;
        }
        let (mut w, mut e, mut s, mut n) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for st in &points {
            let item = self.plan.items.iter().find(|i| i.id == st.id);
            let radius = item.filter(|i| self.plan.ring_flown(i)).map_or(0.0, |i| i.radius);
            let d_lat = (radius / geometry::EARTH_RADIUS_M).to_degrees();
            let d_lon = d_lat / st.lat.to_radians().cos().max(0.01);
            w = w.min(st.lon - d_lon);
            e = e.max(st.lon + d_lon);
            s = s.min(st.lat - d_lat);
            n = n.max(st.lat + d_lat);
        }
        [w, s, e, n].iter().all(|v| v.is_finite()).then_some([w, s, e, n])
    }

    /// `notePlanOffscreen` and `clearOffscreenNotice`: a plan that is nowhere
    /// on screen is said out loud (the page opens on the view the operator
    /// last aimed, and framing the plan over it would undo that), and the
    /// line comes down once the plan is in view, however it got there.
    pub(super) fn check_offscreen(&mut self, map: &Map, size: egui::Vec2) {
        let Some((lng, lat, zoom)) = map.camera() else { return };
        let view = view_box((lng, lat), zoom, (size.x, size.y));
        let plan = self.bounds();
        if std::mem::take(&mut self.check_view) {
            if let Some(b) = plan {
                if !boxes_overlap(b, view) {
                    let name = if self.plan.name.is_empty() {
                        "The mission"
                    } else {
                        self.plan.name.as_str()
                    };
                    let text = format!("{name} is outside this view. Use Fit to frame it.");
                    self.say(Level::Warn, text);
                    self.offscreen = true;
                }
            }
        } else if self.offscreen && plan.is_none_or(|b| boxes_overlap(b, view)) {
            self.clear_status();
            self.offscreen = false;
        }
    }
}
