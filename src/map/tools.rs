//! What the Home map does with a click: the fly-to-points plan ("Punktabflug",
//! `setWaypointMode` of `python/src/js/map.js`, the PLAN button and panel of
//! `app.js`) and the menu that opens on a point (`openContextMenu`, the two
//! rows registered by `app.js`: fly to this point, set home here).

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::{json, Value};

use crate::data::units::Units;
use crate::ui::icons;
use crate::ui::menu;
use crate::ui::style::{self, Weight};

/// The slider of the plan panel: `min="1" max="50" step="1" value="10"`.
pub const ALT_MIN_M: f64 = 1.0;
pub const ALT_MAX_M: f64 = 50.0;
pub const ALT_DEFAULT_M: f64 = 10.0;
/// `--plan`: the amber of the route, the same one the Mission planner draws.
const PLAN: Color32 = Color32::from_rgb(0xFF, 0xC2, 0x1A);

fn connected(s: &Value) -> bool {
    s.get("connected") == Some(&Value::Bool(true))
}

fn armed(s: &Value) -> bool {
    s.get("armed") == Some(&Value::Bool(true))
}

/// The fly-to-points plan: the points the operator clicked, in entry order.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Planning mode: a click adds a point, a right click takes the last one
    /// away, Escape leaves (`PLAN` lit on the bar).
    pub active: bool,
    /// Whether the panel shows.
    pub panel: bool,
    /// Longitude and latitude, as `getWaypoints` keeps them.
    pub points: Vec<[f64; 2]>,
    /// Metres above the ground the points are flown at, a whole number.
    pub alt: f64,
}

impl Default for Plan {
    fn default() -> Plan {
        Plan {
            active: false,
            panel: false,
            points: Vec::new(),
            alt: ALT_DEFAULT_M,
        }
    }
}

impl Plan {
    /// Turns planning on or off (`setPlanMode`); on shows the panel.
    pub fn set_active(&mut self, on: bool) {
        self.active = on;
        if on {
            self.panel = true;
        }
    }

    /// A click in planning mode.
    pub fn add(&mut self, lng: f64, lat: f64) {
        self.points.push([lng, lat]);
        if self.active {
            self.panel = true;
        }
    }

    /// A right click in planning mode: only the tail goes, so the earlier
    /// numbers stay.
    pub fn pop(&mut self) {
        self.points.pop();
    }

    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// "0 POINTS", "1 POINT", "2 POINTS".
    pub fn count_text(&self) -> String {
        let n = self.points.len();
        format!("{n} POINT{}", if n == 1 { "" } else { "S" })
    }

    /// FLY needs a connected, armed vehicle and one point at least: the bridge
    /// arms and starts the mission, and a parked aircraft only earns a
    /// rejection to clear.
    pub fn fly_ready(&self, state: &Value) -> bool {
        connected(state) && armed(state) && !self.points.is_empty()
    }

    /// The slider value as the request sends it, whole metres in [1, 50].
    pub fn alt_agl(&self) -> f64 {
        if self.alt.is_finite() {
            self.alt.round().clamp(ALT_MIN_M, ALT_MAX_M)
        } else {
            ALT_DEFAULT_M
        }
    }

    /// The body of `POST /api/mavlink/gotopoints`; a ground vehicle gets no
    /// height.
    pub fn body(&self, ground: bool) -> Value {
        let alt = self.alt_agl();
        let points: Vec<Value> = self
            .points
            .iter()
            .map(|p| {
                if ground {
                    json!({"lat": p[1], "lon": p[0]})
                } else {
                    json!({"lat": p[1], "lon": p[0], "alt_agl": alt})
                }
            })
            .collect();
        json!({ "points": points })
    }

    /// The dashed route: from the aircraft when it has a fix, so the first
    /// leg shows even for one point; a lone point without a fix draws none.
    pub fn route(&self, vehicle: Option<(f64, f64)>) -> Vec<[f64; 2]> {
        if self.points.is_empty() {
            return Vec::new();
        }
        match vehicle {
            Some((lng, lat)) => std::iter::once([lng, lat]).chain(self.points.iter().copied()).collect(),
            None if self.points.len() >= 2 => self.points.clone(),
            None => Vec::new(),
        }
    }
}

/// What a row of the menu does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// `POST /api/mavlink/gotopoints` with this one point.
    Goto([f64; 2]),
    /// `POST /api/mavlink/sethome`.
    SetHome([f64; 2]),
}

/// One row of the menu: icon, label and the line under it.
#[derive(Debug, Clone, PartialEq)]
pub struct RowSpec {
    pub id: &'static str,
    pub icon: &'static str,
    pub label: String,
    pub note: String,
    pub enabled: bool,
}

/// "lat, lng" to six places, 0.1 m, finer than a click can say.
pub fn coords_text(p: [f64; 2]) -> String {
    format!("{:.6}, {:.6}", p[1], p[0])
}

/// The two rows `app.js` registers, with the same gates and the same notes.
pub fn rows(state: &Value, ground: bool, plan_alt: f64, units: &Units) -> Vec<RowSpec> {
    let conn = connected(state);
    let fly = conn && armed(state);
    let goto_note = if !fly {
        if conn {
            "Arm the vehicle first".to_string()
        } else {
            "Not connected".to_string()
        }
    } else if ground {
        String::new()
    } else {
        format!("{} AGL", units.format_length(plan_alt, 0, false, false))
    };
    vec![
        RowSpec {
            id: "goto",
            icon: "navigation",
            label: format!("{} to this point", crate::data::capabilities::move_verb(state)),
            note: goto_note,
            enabled: fly,
        },
        RowSpec {
            id: "sethome",
            icon: "house",
            label: "Set home here".into(),
            note: if conn {
                "Moves the return target"
            } else {
                "Not connected"
            }
            .into(),
            enabled: conn,
        },
    ]
}

/// The menu open on a point, and the pin that marks it.
#[derive(Debug, Default)]
pub struct Tools {
    pub plan: Plan,
    /// The point the menu is about.
    pub point: Option<[f64; 2]>,
    anchor: Option<Pos2>,
    copied_at: Option<f64>,
}

impl Tools {
    /// A click on the map. `secondary` is the right button. Returns true when
    /// it was taken; planning owns the click while it is on, so the menu
    /// stands down.
    pub fn click(&mut self, p: [f64; 2], secondary: bool, blocked: bool) -> bool {
        if blocked {
            return false;
        }
        if self.plan.active {
            if secondary {
                self.plan.pop();
            } else {
                self.plan.add(p[0], p[1]);
            }
            return true;
        }
        self.point = Some(p);
        self.copied_at = None;
        true
    }

    /// Closes the menu and takes the pin off.
    pub fn close_menu(&mut self) {
        self.point = None;
        self.anchor = None;
        self.copied_at = None;
    }

    /// Draws the route, the numbered points and the pin; `at` turns a point
    /// into a screen position, or None where it cannot be seen.
    pub fn paint(
        &mut self,
        painter: &egui::Painter,
        at: &dyn Fn([f64; 2]) -> Option<Pos2>,
        vehicle: Option<(f64, f64)>,
    ) {
        let route: Vec<Pos2> = self.plan.route(vehicle).into_iter().filter_map(at).collect();
        if route.len() >= 2 {
            painter.extend(Shape::dashed_line(
                &route,
                Stroke::new(2.0, PLAN.gamma_multiply(0.9)),
                6.0,
                4.0,
            ));
        }
        let warning = style::warning();
        for (i, p) in self.plan.points.iter().enumerate() {
            let Some(c) = at(*p) else { continue };
            for (r, a) in [(19.0, 0.05), (16.5, 0.08), (14.0, 0.12)] {
                painter.circle_filled(c, r, warning.gamma_multiply(a));
            }
            painter.circle_filled(c, 14.0, warning.gamma_multiply(0.18));
            painter.circle_stroke(c, 13.25, Stroke::new(1.5, warning));
            let g = painter.layout_no_wrap((i + 1).to_string(), style::font(12.0, Weight::W700), warning);
            painter.galley(c - g.size() / 2.0, g, warning);
        }
        self.anchor = self.point.and_then(at);
        if let Some(c) = self.anchor {
            painter.circle_filled(c + Vec2::new(0.0, 2.0), 9.0, Color32::from_black_alpha(46));
            painter.circle_filled(c, 8.0, Color32::WHITE);
            painter.circle_filled(c, 5.0, style::accent());
        }
    }

    /// The menu beside its pin. Returns the action chosen; any choice closes
    /// the menu, as does a press outside it or Escape.
    pub fn menu(
        &mut self,
        ui: &egui::Ui,
        state: &Value,
        ground: bool,
        units: &Units,
        now: f64,
        bounds: Rect,
    ) -> Option<Action> {
        let point = self.point?;
        let anchor = self.anchor?;
        let specs = rows(state, ground, self.plan.alt_agl(), units);
        let coords = coords_text(point);

        let head_g = style::galley_mono(ui, &coords, 11.0, style::text_1());
        let head_w = 8.0 + 14.0 + 8.0 + head_g.size().x + 8.0 + 22.0 + 3.0;
        let items: Vec<menu::Item> = specs
            .iter()
            .map(|r| {
                menu::Row::new(r.id, r.label.clone())
                    .icon(r.icon)
                    .note(r.note.clone())
                    .enabled(r.enabled)
                    .into()
            })
            .collect();
        let copied = self.copied_at.is_some_and(|t| now - t < 1.8);
        let mut copy_now = false;
        let id = ui.scope_id().with("map-context-menu");
        let out = menu::Menu::new(id, menu::Anchor::point(anchor, bounds), &menu::Look::CONTEXT, &items)
            .head(HEAD_H, head_w, |ui, rect| {
                copy_now = head(ui, rect, &coords, &head_g, copied);
            })
            .show(ui.ctx());
        if copy_now {
            self.copied_at = Some(now);
        }
        if copied {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        let chosen = out.picked.map(|id| match id.as_str() {
            "goto" => Action::Goto(point),
            _ => Action::SetHome(point),
        });
        if chosen.is_some() || out.dismissed {
            self.close_menu();
        }
        chosen
    }
}

/// `.map-context-head`: 2 above, the 22 px line, 6 below, the rule, 3 under it.
const HEAD_H: f32 = 2.0 + 22.0 + 6.0 + 1.0 + 3.0;

/// The head of the context menu: the pin, the coordinates, the copy button.
/// True when the button was pressed.
fn head(ui: &mut egui::Ui, rect: Rect, coords: &str, galley: &std::sync::Arc<egui::Galley>, copied: bool) -> bool {
    let painter = ui.painter().clone();
    let head = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEAD_H - 3.0));
    let line_y = head.center().y - 0.5;
    icons::paint(
        ui,
        "map-pin",
        Rect::from_center_size(Pos2::new(head.left() + 8.0 + 7.0, line_y - 1.5), Vec2::splat(14.0)),
        style::accent(),
    );
    painter.galley(
        Pos2::new(head.left() + 8.0 + 14.0 + 8.0, line_y - 1.5 - galley.size().y / 2.0),
        galley.clone(),
        style::text_1(),
    );
    painter.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, style::tint(0.1)));
    let copy = Rect::from_center_size(Pos2::new(head.right() - 3.0 - 11.0, line_y - 1.5), Vec2::splat(22.0));
    let copy_resp = ui.interact(copy, ui.scope_id().with("ctx-copy"), egui::Sense::click());
    if copy_resp.hovered() {
        painter.rect_filled(copy, 4, style::surface_hover());
    }
    icons::paint(
        ui,
        if copied { "check" } else { "copy" },
        copy.shrink(4.5),
        if copied {
            style::accent_bright()
        } else if copy_resp.hovered() {
            style::text_1()
        } else {
            style::text_3()
        },
    );
    let pressed = copy_resp
        .on_hover_text(if copied { "Copied" } else { "Copy coordinates" })
        .clicked();
    if pressed {
        ui.ctx().copy_text(coords.to_string());
    }
    pressed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fly_state() -> Value {
        json!({"connected": true, "armed": true})
    }

    #[test]
    fn the_count_reads_as_the_panel_does() {
        let mut p = Plan::default();
        assert_eq!(p.count_text(), "0 POINTS");
        p.add(11.0, 48.0);
        assert_eq!(p.count_text(), "1 POINT");
        p.add(11.1, 48.1);
        assert_eq!(p.count_text(), "2 POINTS");
    }

    #[test]
    fn a_right_click_takes_only_the_tail() {
        let mut p = Plan::default();
        p.add(1.0, 2.0);
        p.add(3.0, 4.0);
        p.pop();
        assert_eq!(p.points, vec![[1.0, 2.0]]);
        p.pop();
        p.pop();
        assert!(p.points.is_empty());
    }

    #[test]
    fn fly_needs_a_connected_armed_vehicle_and_a_point() {
        let mut p = Plan::default();
        assert!(!p.fly_ready(&fly_state()));
        p.add(1.0, 2.0);
        assert!(p.fly_ready(&fly_state()));
        assert!(!p.fly_ready(&json!({"connected": true, "armed": false})));
        assert!(!p.fly_ready(&json!({"connected": false, "armed": true})));
    }

    #[test]
    fn the_request_carries_lat_lon_and_a_clamped_height() {
        let mut p = Plan::default();
        p.add(11.5, 48.25);
        p.alt = 80.0;
        assert_eq!(
            p.body(false),
            json!({"points": [{"lat": 48.25, "lon": 11.5, "alt_agl": 50.0}]})
        );
        p.alt = 0.2;
        assert_eq!(p.body(false)["points"][0]["alt_agl"], 1.0);
        assert_eq!(p.body(true), json!({"points": [{"lat": 48.25, "lon": 11.5}]}));
    }

    #[test]
    fn the_route_starts_at_the_aircraft() {
        let mut p = Plan::default();
        assert!(p.route(Some((1.0, 1.0))).is_empty());
        p.add(2.0, 2.0);
        assert_eq!(p.route(Some((1.0, 1.0))), vec![[1.0, 1.0], [2.0, 2.0]]);
        assert!(p.route(None).is_empty());
        p.add(3.0, 3.0);
        assert_eq!(p.route(None), vec![[2.0, 2.0], [3.0, 3.0]]);
    }

    #[test]
    fn coordinates_are_latitude_first_to_six_places() {
        assert_eq!(coords_text([11.5, 48.25]), "48.250000, 11.500000");
        assert_eq!(coords_text([-0.1234567, -33.9]), "-33.900000, -0.123457");
    }

    #[test]
    fn the_rows_say_why_they_are_unavailable() {
        let u = Units::default();
        let off = rows(&json!({"connected": false}), false, 10.0, &u);
        assert!(!off[0].enabled && !off[1].enabled);
        assert_eq!(off[0].note, "Not connected");
        assert_eq!(off[1].note, "Not connected");
        let parked = rows(&json!({"connected": true, "armed": false}), false, 10.0, &u);
        assert!(!parked[0].enabled && parked[1].enabled);
        assert_eq!(parked[0].note, "Arm the vehicle first");
        assert_eq!(parked[1].note, "Moves the return target");
        let up = rows(&fly_state(), false, 10.0, &u);
        assert!(up[0].enabled);
        assert_eq!(up[0].label, "Fly to this point");
        assert_eq!(up[0].note, "10 m AGL");
        let rover = json!({"connected": true, "armed": true, "vehicle_type": "GROUND_ROVER"});
        let car = rows(&rover, true, 10.0, &u);
        assert_eq!(car[0].label, "Drive to this point");
        assert_eq!(car[0].note, "");
    }

    #[test]
    fn planning_owns_the_click() {
        let mut t = Tools::default();
        assert!(t.click([1.0, 2.0], false, false));
        assert_eq!(t.point, Some([1.0, 2.0]));
        t.close_menu();
        t.plan.set_active(true);
        assert!(t.click([3.0, 4.0], false, false));
        assert_eq!(t.point, None);
        assert_eq!(t.plan.points, vec![[3.0, 4.0]]);
        assert!(t.plan.panel);
        assert!(t.click([3.0, 4.0], true, false));
        assert!(t.plan.points.is_empty());
        assert!(!t.click([5.0, 6.0], false, true));
    }
}
