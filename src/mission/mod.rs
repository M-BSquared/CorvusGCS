//! The Mission page, the planner (`python/src/js/mission.js`, `mission-geometry.js`,
//! `python/src/css/mission.css`).
//!
//! A flight drawn before it is flown: a start, waypoints, orbits and a
//! landing on the map, with the whole route's altitude underneath as a
//! profile whose points can be dragged up and down. Nothing here talks to the
//! aircraft until Upload is pressed, through the backend's `/api/mission`
//! endpoints. The map is the Home map (`crate::map::Map`) with the plan drawn
//! over it as an `Overlay`, so the layers, the downloaded areas, the search
//! and the offline dialog are the ones the Home page has.
//!
//! * `plan`: the item table, bounds and editing rules.
//! * `geometry`: distances, stations, the profile line, sweeps.
//! * `issues`: the warnings, a row's text and the display units.
//! * `backend`, `progress`: the calls, and which leg the vehicle is flying.
//! * `mapview`: the plan on the map and the pointer on it.
//! * `toolbar`: the tool bar, its hint and the pattern card.
//! * `side`: the sidebar, with the list, the editor and the actions.
//! * `profile`: the altitude profile.
//! * `dialogs`: the confirmations and the list of saved plans.

pub mod backend;
mod dialogs;
mod edit;
pub mod geometry;
pub mod issues;
pub mod mapview;
pub mod plan;
mod profile;
pub mod progress;
mod side;
mod terrain;
mod toolbar;

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::Value;

use crate::data::units::Units;
use crate::map::rail::MapRail;
use crate::map::Map;
use crate::mission::backend::{Calls, Job};
use crate::mission::geometry::Ground;
use crate::mission::mapview::{Draft, Out, PlanLayer};
use crate::mission::plan::{Kind, Link, Plan, Shape, Tool, DEFAULT_ALT_M, START_OFFSET_WARN_M};
use crate::mission::progress::Tracker;
use crate::net::http::Server;
use crate::ui::style;

/// `--mission-side-w`, 280 below 1280 px.
pub const SIDE_W: f32 = 320.0;
pub const SIDE_W_NARROW: f32 = 280.0;

/// `updatePatternHint`: what a finished shape says, for a finger or a mouse.
pub fn ready_hint(by_finger: bool) -> &'static str {
    if by_finger {
        "Press FINISH to cover it. Tap for more points, hold the map to take one back."
    } else {
        "Enter or a double click covers it. Click for more points, right click undoes one."
    }
}
mod last;

/// The map's row when the page is stacked: `minmax(320px, 55vh)`.
pub fn stacked_map_height(vh: f32) -> f32 {
    (0.55 * vh).max(320.0)
}

/// What the status line under the editor says, and in which colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Pending,
    Ok,
    Warn,
    Err,
}

/// The dialogs the planner opens.
pub(crate) enum Dialog {
    None,
    Upload {
        fly: bool,
        warnings: Vec<issues::Issue>,
    },
    Read,
    Clear,
    Open {
        plans: Vec<backend::Saved>,
        dir: String,
        armed: Option<(String, f64)>,
    },
}

/// A pattern being moved: its points as grabbed (id, lat, lon) and its centre.
type PatternGrab = (Vec<(u32, f64, f64)>, [f64; 2]);

pub struct MissionPage {
    pub(crate) calls: Calls,
    pub(crate) plan: Plan,
    pub(crate) tracker: Tracker,
    pub(crate) tool: Tool,
    pub(crate) draft: Draft,
    pub(crate) hint: String,
    pub(crate) status: Option<(Level, String)>,
    pub(crate) dialog: Dialog,
    pub(crate) link: Link,
    pub(crate) vehicle: Option<(f64, f64)>,
    pub(crate) vehicle_id: String,
    pub(crate) units: Units,
    pub(crate) ground: Option<Ground>,
    pub(crate) profile: profile::Profile,
    pub(crate) side: side::SideState,
    /// The stacked sidebar's height as last drawn, which the page reserves.
    pub(crate) side_height: f32,
    pub(crate) tools: toolbar::ToolState,
    pub(crate) notices: Vec<(&'static str, String)>,
    sampler: terrain::Sampler,
    pattern_origin: Option<PatternGrab>,
    return_token: u32,
    was_connected: bool,
    refusal_stack: Option<String>,
    had_ending: bool,
    fit_pending: bool,
    last_plan: Option<String>,
    first: bool,
    open_first: Option<String>,
    /// The plan has not been looked for on the map yet (`notePlanOffscreen`),
    /// and whether the status line says it is outside the view.
    check_view: bool,
    offscreen: bool,
}

/// The ground a map of `size` pixels shows around its centre at `zoom`, as
/// `[west, south, east, north]`.
pub fn view_box(centre: (f64, f64), zoom: f64, size: (f32, f32)) -> [f64; 4] {
    let metres_per_px = 156_543.033_9 * centre.1.to_radians().cos() / 2f64.powf(zoom);
    let d_lat = f64::from(size.1) / 2.0 * metres_per_px / 111_320.0;
    let d_lon = f64::from(size.0) / 2.0 * metres_per_px / (111_320.0 * centre.1.to_radians().cos().max(0.01));
    [centre.0 - d_lon, centre.1 - d_lat, centre.0 + d_lon, centre.1 + d_lat]
}

/// `boxesOverlap`: two `[w, s, e, n]` boxes share some ground.
pub fn boxes_overlap(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

/// `lngLatPoint`: a telemetry `[lon, lat]`, or None for none or 0/0.
fn lng_lat(v: Option<&Value>) -> Option<(f64, f64)> {
    let a = v?.as_array()?;
    let (lon, lat) = (a.first()?.as_f64()?, a.get(1)?.as_f64()?);
    (lat.is_finite() && lon.is_finite() && !(lat == 0.0 && lon == 0.0) && lat.abs() <= 90.0 && lon.abs() <= 180.0)
        .then_some((lon, lat))
}

/// `vehicleStart`: where the connected aircraft takes off from. Armed, that
/// is its home, where the mission's heights are measured from.
pub fn vehicle_start(state: &Value) -> Option<(f64, f64)> {
    if state.get("connected").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if state.get("armed").and_then(Value::as_bool) == Some(true) {
        return lng_lat(state.get("home")).or_else(|| lng_lat(state.get("position")));
    }
    lng_lat(state.get("position"))
}

impl MissionPage {
    pub fn new(server: Server, sources: Value) -> MissionPage {
        MissionPage {
            sampler: terrain::Sampler::new(server.clone(), sources),
            calls: Calls::new(server),
            plan: Plan::default(),
            tracker: Tracker::default(),
            tool: Tool::Select,
            draft: Draft {
                spacing: geometry::PATTERN_SPACING_M.def,
                width: geometry::PATTERN_WIDTH_M.def,
                ..Draft::default()
            },
            hint: String::new(),
            status: None,
            dialog: Dialog::None,
            link: Link::default(),
            vehicle: None,
            vehicle_id: String::new(),
            units: Units::default(),
            ground: None,
            profile: profile::Profile::default(),
            side: side::SideState::default(),
            side_height: 600.0,
            tools: toolbar::ToolState::default(),
            notices: Vec::new(),
            pattern_origin: None,
            return_token: 0,
            was_connected: false,
            refusal_stack: None,
            had_ending: false,
            fit_pending: false,
            last_plan: None,
            first: true,
            check_view: false,
            offscreen: false,
            open_first: None,
        }
    }

    /// Opens this saved plan when the page is first drawn (`--page mission:NAME`).
    pub fn open_saved(&mut self, name: &str) {
        self.open_first = Some(name.to_string());
    }

    /// Lines for the notification board: (level, text).
    pub fn take_notices(&mut self) -> Vec<(&'static str, String)> {
        let mut out = std::mem::take(&mut self.notices);
        out.extend(self.plan.notices.drain(..).map(|n| ("warning", n)));
        out
    }

    /// Leaving the page: whatever was half done is let go of.
    pub fn suspend(&mut self) {
        self.check_view = true;
        self.draft.pts.clear();
        self.draft.cursor = None;
        self.side.end_row_drag();
        self.profile.end_drag();
        self.pattern_origin = None;
        self.tools.pattern_pinned = false;
    }

    pub(crate) fn say(&mut self, level: Level, text: impl Into<String>) {
        self.status = Some((level, text.into()));
    }

    pub(crate) fn clear_status(&mut self) {
        self.status = None;
    }

    pub(crate) fn send(&mut self, ctx: &egui::Context, job: Job) {
        let plan = self.plan.to_json();
        let name = self.plan.name.clone();
        self.calls.start(ctx, job, &plan, &name);
    }

    /// Everything a telemetry frame can change on this page.
    fn follow_telemetry(&mut self, ctx: &egui::Context, state: &Value, units: &Units) {
        self.units = *units;
        let connected = state.get("connected").and_then(Value::as_bool) == Some(true);
        self.link = Link {
            connected,
            start: vehicle_start(state),
        };
        self.vehicle = connected.then(|| lng_lat(state.get("position"))).flatten();
        self.vehicle_id = state
            .get("vehicle_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let was_locked = self.plan.aircraft_locked;
        let vehicle_type = connected
            .then(|| state.get("vehicle_type").and_then(Value::as_str))
            .flatten();
        let adopted = self.plan.adopt_vehicle_aircraft(vehicle_type);
        if connected != self.was_connected || self.plan.aircraft_locked != was_locked {
            self.was_connected = connected;
            self.load_return(ctx);
        }
        let kind_changed = self.plan.sync_drives(true);
        let moved = self.plan.follow_vehicle_start(self.link.start, false);
        let stack = if connected {
            state
                .get("autopilot_stack")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        if self.refusal_stack.as_deref() != Some(stack.as_str()) {
            self.refusal_stack = Some(stack.clone());
            self.plan.refusals.clear();
            if !stack.is_empty() {
                self.send(ctx, Job::Capabilities { stack });
            }
        }
        let progress = self.tracker.on_frame(&self.plan, state);
        if moved || adopted || kind_changed || progress {
            self.refresh();
        }
        if kind_changed {
            self.set_tool(Tool::Select);
        }
    }

    fn load_return(&mut self, ctx: &egui::Context) {
        self.return_token += 1;
        if !self.link.connected {
            self.plan.return_profile = None;
            return;
        }
        let token = self.return_token;
        self.send(ctx, Job::Return { token });
    }

    /// The answers of the requests on their way.
    fn take_answers(&mut self, ctx: &egui::Context) {
        for (job, answer) in self.calls.poll() {
            match (job, answer) {
                (Job::Upload { fly }, Ok(res)) => {
                    let revision = res.get("revision").and_then(Value::as_i64);
                    self.tracker.sync_with(&self.plan, revision, &self.vehicle_id);
                    self.say(Level::Ok, backend::upload_report(&res, fly));
                }
                (Job::Upload { .. }, Err(e)) => {
                    self.say(
                        Level::Err,
                        if e.is_empty() {
                            "The vehicle refused the mission.".to_string()
                        } else {
                            e
                        },
                    );
                }
                (Job::Read, Ok(res)) => match res.get("plan").filter(|p| p.is_object()) {
                    None => {
                        let why = res
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("The vehicle's mission cannot be shown as a plan.");
                        self.say(Level::Err, why);
                    }
                    Some(p) => {
                        self.plan.load_json(p, self.vehicle_type(), self.link.start);
                        self.refresh();
                        let revision = res.get("revision").and_then(Value::as_i64);
                        self.tracker.sync_with(&self.plan, revision, &self.vehicle_id);
                        if !self.plan.items.is_empty() {
                            self.fit_pending = true;
                        }
                        let (text, warn) = backend::read_report(&res);
                        self.say(if warn { Level::Warn } else { Level::Ok }, text);
                    }
                },
                (Job::Read, Err(e)) => {
                    self.say(
                        Level::Err,
                        if e.is_empty() {
                            "Could not read the mission from the vehicle.".to_string()
                        } else {
                            e
                        },
                    );
                }
                (Job::Save, Ok(res)) => {
                    let name = res
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&self.plan.name)
                        .to_string();
                    self.last_plan = Some(name.clone());
                    last::remember(&name);
                    self.say(Level::Ok, format!("Saved as {name}."));
                }
                (Job::Save, Err(e)) => {
                    self.say(
                        Level::Err,
                        if e.is_empty() {
                            "Could not save the mission.".to_string()
                        } else {
                            e
                        },
                    );
                }
                (Job::ListPlans, Ok(res)) => {
                    let (plans, dir) = backend::read_plans(&res);
                    self.dialog = Dialog::Open {
                        plans,
                        dir,
                        armed: None,
                    };
                }
                (Job::ListPlans, Err(e)) => {
                    self.say(
                        Level::Err,
                        if e.is_empty() {
                            "Could not list the saved missions.".to_string()
                        } else {
                            e
                        },
                    );
                }
                (Job::LoadPlan { name, quiet }, Ok(res)) => {
                    if let Some(p) = res.get("plan").filter(|p| p.is_object()) {
                        self.plan.load_json(p, self.vehicle_type(), self.link.start);
                        let shown = res.get("name").and_then(Value::as_str).unwrap_or(&name).to_string();
                        self.last_plan = Some(shown.clone());
                        last::remember(&shown);
                        self.refresh();
                        if !quiet {
                            self.fit_pending = true;
                            self.say(Level::Ok, format!("Opened {shown}."));
                        } else {
                            self.check_view = true;
                        }
                    }
                }
                (Job::LoadPlan { name, quiet }, Err(e)) => {
                    if !quiet {
                        self.say(
                            Level::Err,
                            if e.is_empty() {
                                format!("Could not open {name}.")
                            } else {
                                e
                            },
                        );
                    }
                }
                (Job::RemovePlan { name }, Ok(_)) => {
                    if self.last_plan.as_deref() == Some(name.as_str()) {
                        self.last_plan = None;
                    }
                    last::forget(&name);
                    if let Dialog::Open { plans, .. } = &mut self.dialog {
                        plans.retain(|p| p.name != name);
                    }
                }
                (Job::RemovePlan { name }, Err(e)) => {
                    self.say(
                        Level::Err,
                        if e.is_empty() {
                            format!("Could not delete {name}.")
                        } else {
                            e
                        },
                    );
                }
                (Job::Return { token }, answer) => {
                    if token == self.return_token {
                        self.plan.return_profile = answer.ok().and_then(|v| plan::ReturnProfile::from_json(&v));
                        self.refresh();
                    }
                }
                (Job::Capabilities { stack }, Ok(caps)) => {
                    if self.refusal_stack.as_deref() == Some(stack.as_str()) {
                        self.plan.refusals = backend::read_refusals(&caps).into_iter().collect();
                        self.refresh();
                    }
                }
                (Job::Capabilities { .. }, Err(_)) => {}
            }
        }
        let _ = ctx;
    }

    /// The ground under the route, once its tiles are in.
    fn sample_ground(&mut self, ctx: &egui::Context, now: f64) {
        let points = geometry::stations(&self.plan.items, self.plan.home.as_ref());
        if let Some(found) = self.sampler.update(ctx, now, &points, self.plan.drives) {
            self.ground = found.ground;
            if let (Some(base), Some(h)) = (found.base, self.plan.home.as_mut()) {
                h.elevation = Some(crate::data::units::js_round(base * 10.0) / 10.0);
            }
        }
    }

    fn vehicle_type(&self) -> Option<&'static str> {
        self.plan
            .aircraft
            .filter(|_| self.plan.aircraft_locked)
            .and_then(|a| a.types().first().copied())
    }

    /// `refreshPlan`: whatever follows from the plan having changed.
    pub(crate) fn refresh(&mut self) {
        self.tracker.apply(&self.plan);
        self.update_hint();
        if self.plan.end_index().is_some() && !self.had_ending {
            self.profile.open_for_ending();
        }
        self.had_ending = self.plan.end_index().is_some();
    }

    /// The hint under the bar: what the next step is, in the order a mission
    /// is drawn in.
    pub(crate) fn update_hint(&mut self) {
        self.hint = toolbar::hint_for(&self.plan, self.tool, &self.link, &self.draft, crate::app::touch::on());
    }

    pub(crate) fn set_tool(&mut self, next: Tool) {
        self.tool = next;
        if self.plan.tool_blocked(self.tool, &self.link).is_some() {
            self.tool = Tool::Select;
        }
        if !matches!(self.tool, Tool::Shape(_) | Tool::Pattern) {
            self.tools.pattern_pinned = false;
            self.tools.pattern_until = 0.0;
        }
        self.draft.pts.clear();
        self.draft.cursor = None;
        self.update_hint();
    }

    /// A tool bar button was pressed.
    pub(crate) fn press_tool(&mut self, tool: Tool) {
        if let Some(why) = self.plan.tool_blocked(tool, &self.link) {
            self.hint = why;
            return;
        }
        match tool {
            Tool::Item(Kind::Rtl) => {
                self.plan.add_item(Kind::Rtl, None, None);
                self.set_tool(Tool::Select);
                self.refresh();
            }
            Tool::Pattern => {
                self.tools.pattern_pinned = !self.tools.pattern_pinned;
                self.tools.pattern_until = 0.0;
            }
            Tool::VehicleStart => {
                self.tools.pattern_pinned = false;
                let on = !self.plan.start_at_vehicle;
                self.plan.set_start_at_vehicle(on, self.link.start);
                self.set_tool(Tool::Select);
                self.refresh();
                if on && self.plan.home.is_some() {
                    self.fit_pending = true;
                }
            }
            other => {
                self.tools.pattern_pinned = false;
                self.set_tool(other);
            }
        }
    }

    /// Drawing a pattern: is there enough of a shape to cover?
    pub(crate) fn pattern_ready(&self) -> bool {
        let need = if self.tool == Tool::Shape(Shape::Area) { 3 } else { 2 };
        mapview::distinct_points(&self.draft.pts).len() >= need
    }

    /// `commitPattern`: the drawn shape becomes the mission, before its ending.
    pub(crate) fn commit_pattern(&mut self) {
        let Tool::Shape(shape) = self.tool else { return };
        let pts = mapview::distinct_points(&self.draft.pts);
        if shape == Shape::Circle {
            self.commit_circle(&pts);
            return;
        }
        let route = geometry::pattern_waypoints(shape, &pts, self.draft.spacing, self.draft.width);
        if route.is_empty() {
            self.hint = "That shape is too small to cover. Draw it larger or reduce the line spacing.".into();
            return;
        }
        let room = plan::MAX_ITEMS.saturating_sub(self.plan.items.len());
        if route.len() > room {
            let text = format!(
                "That pattern needs {} points and the mission has room for {room}. Increase the line spacing.",
                route.len()
            );
            self.notices.push(("warning", text.clone()));
            self.hint = text;
            return;
        }
        let group = self.plan.next_pattern;
        self.plan.next_pattern += 1;
        for p in &route {
            if let Some(i) = self.plan.add_item(Kind::Waypoint, Some((p[0], p[1])), None) {
                let at = self.plan.end_index().map_or(i, |e| e.min(i));
                let _ = at;
                self.plan.items[i].pattern = Some(group);
            }
        }
        self.set_tool(Tool::Select);
        self.hint = format!(
            "Pattern added: {} points. Drag the move handle to shift the whole pattern, or a point to move just that one.",
            route.len()
        );
        self.refresh_keep_hint();
    }

    /// `fitMission`.
    fn fit(&mut self, map: &mut Map, size: egui::Vec2, lift: f32) {
        let Some([w, s, e, n]) = self.bounds() else { return };
        if e - w < 1e-9 && n - s < 1e-9 {
            map.go_to(w, s, 16.0);
            return;
        }
        let corners = [[w, s], [e, s], [e, n], [w, n]];
        // the profile covers the bottom of the map: frame the plan above it
        let room = (size.y - lift).max(120.0);
        if let Some((zoom, c)) = crate::setup::geofence::fit(&corners, (size.x, room), 80.0, 18.0) {
            let metres_per_px = 156_543.033_9 * c[1].to_radians().cos() / 2f64.powf(zoom);
            let shift = f64::from(lift) / 2.0 * metres_per_px / 111_320.0;
            map.go_to(c[0], c[1] - shift, zoom);
        }
    }

    /// The map with its tool bar, its rail and the plan on it, and where the
    /// profile overlays it (`band` is the profile's own rect when stacked).
    #[allow(clippy::too_many_arguments)]
    fn show_map_region(
        &mut self,
        ui: &mut egui::Ui,
        map_rect: Rect,
        state: &Value,
        map: &mut Map,
        rail: &mut MapRail,
        narrow: bool,
        lift: f32,
        band: Option<Rect>,
    ) {
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        map.bottom_lift = lift;
        let mut out = Out::default();
        {
            let mut map_ui = ui.new_child(egui::UiBuilder::new().max_rect(map_rect));
            map_ui.set_clip_rect(map_rect.intersect(ui.clip_rect()));
            let mut layer = PlanLayer {
                plan: &self.plan,
                tool: self.tool,
                draft: &self.draft,
                leg: progress::current_leg(&self.tracker, &self.plan),
                current: self.tracker.current_index(),
                done_before: self.tracker.done_before(self.plan.items.len()),
                out: &mut out,
            };
            map.show_with(&mut map_ui, state, Some(&mut layer));
            rail.show(&mut map_ui, map_rect, map, state);
            if (!self.plan.items.is_empty() || self.plan.home.is_some())
                && crate::map::rail::clear_disc(&mut map_ui, map_rect, lift, "Delete the planned route")
            {
                self.confirm_clear();
            }
            self.show_toolbar(&mut map_ui, map_rect, now, narrow);
            if band.is_none() {
                self.show_profile(&mut map_ui, map_rect, narrow, false);
            }
        }
        if let Some(b) = band {
            let mut prof_ui = ui.new_child(egui::UiBuilder::new().max_rect(b));
            prof_ui.set_clip_rect(b.intersect(ui.clip_rect()));
            self.show_profile(&mut prof_ui, b, narrow, true);
        }
        if let Some(n) = rail.notice.take() {
            self.notices.push(("warning", n.to_string()));
        }
        if std::mem::take(&mut rail.fit_requested) || std::mem::take(&mut self.fit_pending) {
            let lift = if band.is_some() { 0.0 } else { lift - 8.0 };
            self.fit(map, map_rect.size(), lift);
        }
        self.apply_map(out);
        self.check_offscreen(map, map_rect.size());
        let _ = ctx;
    }

    /// The page under 960 px (`max-959` of `mission.css`): the map, the
    /// profile as a band, then the sidebar at its natural height, all in one
    /// column that scrolls. The map keeps 55 percent of the window height and
    /// never less than 320 px.
    fn show_stacked(
        &mut self,
        ui: &mut egui::Ui,
        main: Rect,
        state: &Value,
        map: &mut Map,
        rail: &mut MapRail,
        narrow: bool,
    ) {
        let vh = ui.ctx().content_rect().height();
        let map_h = stacked_map_height(vh);
        let band_h = self.profile.band_height();
        let side_min = (main.height() - map_h - band_h).max(0.0);
        let side_h = self.side_height.max(side_min);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(main));
        child.set_clip_rect(main.intersect(ui.clip_rect()));
        child.painter().rect_filled(main, 0, style::bg());
        let area = egui::ScrollArea::vertical().id_salt("missionstacked");
        area.auto_shrink([false, false])
            .scroll_source(egui::scroll_area::ScrollSource::MOUSE_WHEEL | egui::scroll_area::ScrollSource::SCROLL_BAR)
            .show(&mut child, |ui| {
                let top = ui.max_rect().top();
                let map_rect = Rect::from_min_size(Pos2::new(main.left(), top), Vec2::new(main.width(), map_h));
                let band = Rect::from_min_size(
                    Pos2::new(main.left(), map_rect.bottom()),
                    Vec2::new(main.width(), band_h),
                );
                let side = Rect::from_min_size(Pos2::new(main.left(), band.bottom()), Vec2::new(main.width(), side_h));
                self.show_map_region(ui, map_rect, state, map, rail, narrow, 0.0, Some(band));
                let used = self.show_side(ui, side, narrow, true);
                if (used - self.side_height).abs() > 0.5 {
                    self.side_height = used;
                    ui.ctx().request_repaint();
                }
                ui.allocate_space(Vec2::new(main.width(), map_h + band_h + side_h));
            });
    }

    /// Draws the page into `main`: the map with its tool bar and profile on
    /// the left, the sidebar on the right.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        main: Rect,
        state: &Value,
        units: &Units,
        map: &mut Map,
        rail: &mut MapRail,
        narrow: bool,
        stacked: bool,
    ) {
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        self.follow_telemetry(&ctx, state, units);
        self.take_answers(&ctx);
        self.sample_ground(&ctx, now);
        if self.first {
            self.first = false;
            self.check_view = true;
            self.update_hint();
            if let Some(name) = self.open_first.take() {
                self.send(&ctx, Job::LoadPlan { name, quiet: false });
            } else if let Some(name) = last::read() {
                // the plan the operator was drawing last time; no framing, and
                // no complaint when it is gone
                self.send(&ctx, Job::LoadPlan { name, quiet: true });
            }
        }
        map.planner = true;
        rail.planner = true;
        map.set_three_d(false);
        if stacked {
            self.show_stacked(ui, main, state, map, rail, narrow);
        } else {
            let side_w = (if narrow { SIDE_W_NARROW } else { SIDE_W }).min(main.width());
            let map_rect = Rect::from_min_max(main.min, egui::pos2(main.right() - side_w, main.bottom()));
            let side_rect = Rect::from_min_max(egui::pos2(map_rect.right(), main.top()), main.max);
            let lift = self.profile.height(narrow);
            self.show_map_region(ui, map_rect, state, map, rail, narrow, lift, None);
            self.show_side(ui, side_rect, narrow, false);
        }
        self.show_dialogs(&ctx);
        self.keys(&ctx);
        let _ = (DEFAULT_ALT_M, START_OFFSET_WARN_M);
    }
}

#[cfg(test)]
mod tests;
