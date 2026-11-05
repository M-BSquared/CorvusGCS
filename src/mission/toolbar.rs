//! The tool bar across the top of the map, the line under it saying what the
//! next click does, and the card under PATTERN (`buildTools`, `updateHint`,
//! `buildPatternCard` of `mission.js`; `.mission-tools`, `.mission-hint`,
//! `.pattern-card` of `mission.css`). The bar is the Home flight bar's
//! `.fa-btn` row, fitted to the room the same way.

use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::home::flight::fit_bar;
use crate::mission::mapview::Draft;
use crate::mission::plan::{clamp_number, Kind, Link, Plan, Shape, Tool};
use crate::mission::{geometry, MissionPage};
use crate::ui::fabar;
use crate::ui::field;
use crate::ui::style::{self, Weight};

const INSET: f32 = 14.0;
const BAR_PAD: f32 = 7.0;
const BAR_GAP: f32 = 7.0;
/// `--map-trigger-clear`: where the bar's room ends.
const TRIGGER_CLEAR: f32 = 154.0;

#[derive(Default)]
pub struct ToolState {
    pub pattern_pinned: bool,
    /// The card stays open until this time after the pointer left it.
    pub pattern_until: f64,
}

/// One tool of the bar.
struct Entry {
    tool: Tool,
    icon: &'static str,
    label: &'static str,
    drive_label: Option<&'static str>,
    title: &'static str,
    drive_title: Option<&'static str>,
}

/// `TOOLS`, with `divider` between the groups.
fn entries(drives: bool) -> Vec<Option<Entry>> {
    let e = |tool, icon, label, drive_label, title, drive_title| {
        Some(Entry {
            tool,
            icon,
            label,
            drive_label,
            title,
            drive_title,
        })
    };
    let mut v = vec![
        e(
            Tool::Select,
            "mouse-pointer-2",
            "SELECT",
            None,
            "Select and move points",
            None,
        ),
        e(
            Tool::Home,
            "plane-takeoff",
            "START",
            None,
            "Set the start: the aircraft takes off here",
            Some("Set the start: the vehicle starts driving here"),
        ),
        e(
            Tool::VehicleStart,
            "locate-fixed",
            "AIRCRAFT",
            Some("VEHICLE"),
            "Start where the connected aircraft stands, and follow it",
            Some("Start where the connected vehicle stands, and follow it"),
        ),
        None,
        e(
            Tool::Item(Kind::Waypoint),
            "map-pin",
            "POINT",
            None,
            "Add a waypoint",
            None,
        ),
        e(
            Tool::Item(Kind::LoiterTime),
            "timer",
            "HOLD",
            None,
            "Add a timed hold",
            None,
        ),
        e(
            Tool::Pattern,
            "waypoints",
            "PATTERN",
            None,
            "Fly a circle, or cover a corridor or an area in a back and forth sweep",
            None,
        ),
    ];
    if !drives {
        v.push(e(
            Tool::Item(Kind::Land),
            "plane-landing",
            "LAND",
            None,
            "End the mission with a landing",
            None,
        ));
    }
    v.push(None);
    v.push(e(
        Tool::Item(Kind::Rtl),
        "house",
        "RETURN",
        None,
        "End the mission with a return to the start",
        None,
    ));
    v
}

/// `PATTERNS`: the three shapes of the card.
pub const SHAPES: [(Shape, &str, &str, &str); 3] = [
    (
        Shape::Circle,
        "rotate-cw",
        "Circle",
        "Click the centre, then click the edge of the circle to fly.",
    ),
    (
        Shape::Corridor,
        "route",
        "Corridor",
        "Click along the centre line of the corridor. Enter or a double click finishes.",
    ),
    (
        Shape::Area,
        "vector-square",
        "Area",
        "Click the corners of the area. Enter or a double click finishes.",
    ),
];

/// `updateHint`: the line under the bar for the tool and the plan as they stand.
pub fn hint_for(plan: &Plan, tool: Tool, link: &Link, draft: &Draft, touch: bool) -> String {
    let _ = draft;
    let aboard = link.start.is_some();
    let drives = plan.drives;
    let craft = plan.craft();
    let switch = if drives { "VEHICLE" } else { "AIRCRAFT" };
    let starts = if drives { "starts driving" } else { "takes off" };
    match tool {
        Tool::Home => {
            let there = if plan.home.is_some() {
                format!("Click the map to move the start. The {craft} {starts} there.")
            } else {
                format!("Click the map where the {craft} {starts}.")
            };
            if aboard {
                format!("{there} {switch} puts it where the {craft} stands.")
            } else {
                there
            }
        }
        Tool::Shape(s) => {
            // What Enter and a double click do with a mouse, a finger does
            // with the FINISH button (`touchHint`).
            if touch {
                let text = match s {
                    Shape::Circle => "Tap the centre, then tap the edge of the circle to fly.",
                    Shape::Corridor => "Tap along the centre line of the corridor, then press FINISH.",
                    Shape::Area => "Tap the corners of the area, then press FINISH.",
                };
                return format!("{text} Hold the map to take a point back.");
            }
            let text = SHAPES.iter().find(|x| x.0 == s).map_or("", |x| x.3);
            format!("{text} Right click undoes a point. Esc to stop.")
        }
        Tool::Item(kind) => {
            let before = match plan.end_index() {
                Some(e) if !kind.ends() => format!(" before {}", plan.items[e].kind.end_name()),
                _ => String::new(),
            };
            format!(
                "Click the map to add a {}{before}. Esc to stop.",
                kind.label().to_lowercase()
            )
        }
        Tool::Select | Tool::Pattern | Tool::VehicleStart => {
            if plan.home.is_none() {
                if aboard {
                    format!("Press {switch} to start where the connected {craft} stands, or START to click a place.")
                } else {
                    format!("Press START and click where the {craft} {starts}.")
                }
            } else if !plan.items.iter().any(|i| i.kind.position() && i.kind != Kind::Takeoff) {
                if drives {
                    "Now add points with POINT, HOLD or PATTERN, then end with RETURN or at the last point.".into()
                } else {
                    "Now add points with POINT, HOLD or PATTERN, then end with LAND or RETURN.".into()
                }
            } else if plan.end_index().is_none() {
                if drives {
                    "End with RETURN, or the vehicle stops at the last point. Drag a point to move it, right-click to delete it.".into()
                } else {
                    "End the mission with LAND or RETURN. Drag a point to move it, right-click to delete it.".into()
                }
            } else {
                "Drag a point to move it, right-click to delete it. New points go in before the end.".into()
            }
        }
    }
}

impl MissionPage {
    fn tool_title(&self, e: &Entry) -> String {
        if e.tool == Tool::VehicleStart && self.plan.start_at_vehicle {
            return format!(
                "The start follows the connected {}. Press to let go of it.",
                self.plan.craft()
            );
        }
        if self.plan.drives {
            if let Some(t) = e.drive_title {
                return t.to_string();
            }
        }
        e.title.to_string()
    }

    fn tool_active(&self, tool: Tool) -> bool {
        tool == self.tool
            || (tool == Tool::VehicleStart && self.plan.start_at_vehicle)
            || (tool == Tool::Pattern && matches!(self.tool, Tool::Shape(_)))
    }

    /// Draws the bar, its hint and the pattern card over the map.
    pub(crate) fn show_toolbar(&mut self, ui: &mut egui::Ui, map: Rect, now: f64, _narrow: bool) {
        let list = entries(self.plan.drives);
        let n = list.iter().filter(|e| e.is_some()).count() as f32;
        let dividers = list.iter().filter(|e| e.is_none()).count() as f32;
        let room = map.width() - INSET - TRIGGER_CLEAR;
        let mut longest = 0.0f32;
        for e in list.iter().flatten() {
            for c in [Some(e.label), e.drive_label].into_iter().flatten() {
                longest = longest.max(style::galley(ui, c, 10.0, Weight::W600, style::text_2(), 0.6).size().x);
            }
        }
        // padding both sides, the 1 px border, a 7 px gap between all elements
        // and 5 px for each divider
        let gaps = (n + dividers - 1.0) * BAR_GAP;
        let others = BAR_PAD * 2.0 + 2.0 + dividers * 5.0 + gaps;
        let fit = fit_bar(longest, others, n, room);
        let caption_h = style::galley(ui, "ARM", 10.0, Weight::W600, style::text_2(), 0.6)
            .size()
            .y;
        let btn_h = if fit.compact {
            1.0 + 9.0 + 17.0 + 9.0 + 1.0
        } else {
            1.0 + 9.0 + 17.0 + 3.0 + caption_h + 9.0 + 1.0
        };
        let bar_w = others + n * fit.width;
        let bar_h = BAR_PAD * 2.0 + btn_h + 2.0;
        let bar = Rect::from_min_size(map.min + Vec2::splat(INSET), Vec2::new(bar_w, bar_h));
        let painter = ui.painter().clone();
        style::glass_panel(&painter, "missionbar", bar, style::RADIUS, 0.6);
        ui.interact(bar, ui.scope_id().with("missionbar"), egui::Sense::click_and_drag());

        let mut x = bar.left() + BAR_PAD + 1.0;
        let y = bar.top() + BAR_PAD + 1.0;
        let mut pressed = None;
        let mut pattern_btn = None;
        let drives = self.plan.drives;
        for (k, e) in list.iter().enumerate() {
            let Some(e) = e else {
                painter.vline(
                    x + 2.0,
                    egui::Rangef::new(bar.center().y - 17.0, bar.center().y + 17.0),
                    egui::Stroke::new(1.0, style::border_soft()),
                );
                x += 5.0 + BAR_GAP;
                continue;
            };
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(fit.width, btn_h));
            let blocked = self.plan.tool_blocked(e.tool, &self.link);
            let caption = if drives {
                e.drive_label.unwrap_or(e.label)
            } else {
                e.label
            };
            let open = e.tool == Tool::Pattern && (self.tools.pattern_pinned || now < self.tools.pattern_until);
            let icon = if open { "chevron-up" } else { e.icon };
            let title = blocked.clone().unwrap_or_else(|| self.tool_title(e));
            let look = fabar::Look {
                active: self.tool_active(e.tool),
                blocked: blocked.is_some(),
                next: e.tool == Tool::Home && self.plan.home.is_none(),
                ..Default::default()
            };
            if fabar::button(ui, r, icon, caption, &title, look, (fit.scale, fit.compact), ("mt", k)) {
                pressed = Some(e.tool);
            }
            if e.tool == Tool::Pattern {
                pattern_btn = Some(r);
            }
            x += fit.width + BAR_GAP;
        }
        if let Some(t) = pressed {
            self.press_tool(t);
        }

        // the line under the bar
        let mut below = bar.bottom() + 8.0;
        if !self.hint.is_empty() {
            let w_max = 360.0f32.min(map.width() - INSET - TRIGGER_CLEAR);
            let g =
                crate::ui::kit::wrapped_weight(ui, &self.hint, 11.5, Weight::W400, style::text_2(), w_max - 24.0, 1.45);
            let size = Vec2::new(g.size().x + 24.0, g.size().y + 14.0 + 2.0);
            let hint = Rect::from_min_size(Pos2::new(bar.left(), below), size);
            style::glass_panel(&painter, "missionhint", hint, style::RADIUS_BTN, 0.55);
            painter.galley(hint.min + Vec2::new(12.0, 8.0), g, style::text_2());
            below = hint.bottom() + 8.0;
        }
        // FINISH: tablet mode only, and only while a shape can be finished.
        if crate::app::touch::on()
            && matches!(self.tool, Tool::Shape(Shape::Corridor | Shape::Area))
            && self.pattern_ready()
        {
            let b = crate::ui::kit::Button::new("FINISH", crate::ui::kit::Variant::Primary)
                .icon("check")
                .small();
            let r = Rect::from_min_size(Pos2::new(bar.left(), below), Vec2::new(b.width(ui), b.height()));
            if b.show(ui, r, "patterndone") {
                self.commit_pattern();
            }
        }
        if let Some(btn) = pattern_btn {
            self.pattern_card(ui, bar, btn, btn_h, now);
        }
    }

    /// The frosted card under PATTERN: the three shapes and the two numbers
    /// that decide the sweep. It opens on hover; a press pins it open.
    fn pattern_card(&mut self, ui: &mut egui::Ui, bar: Rect, btn: Rect, btn_h: f32, now: f64) {
        const W: f32 = 244.0;
        let open = self.tools.pattern_pinned || now < self.tools.pattern_until;
        let hover_pt = ui.input(|i| i.pointer.hover_pos());
        let over_btn = hover_pt.is_some_and(|p| btn.contains(p));
        if !open {
            if over_btn {
                self.tools.pattern_until = now + 0.16;
                ui.ctx().request_repaint();
            }
            return;
        }
        let top = bar.top() + BAR_PAD + 1.0 + btn_h + 16.0;
        let left = btn.left().min(bar.left().max(btn.left()));
        let fields_h = 9.0 + 12.0 + 6.0 + 28.0 + 2.0 + 1.0;
        let h = 7.0 + btn_h + 7.0 + fields_h + 7.0;
        let card = Rect::from_min_size(Pos2::new(left, top), Vec2::new(W, h));
        let painter = ui.painter().clone();
        style::glass_panel(&painter, "patterncard", card, style::RADIUS_BTN, 0.6);
        ui.interact(card, ui.scope_id().with("patterncard"), egui::Sense::click_and_drag());
        if hover_pt.is_some_and(|p| card.contains(p) || btn.contains(p)) {
            self.tools.pattern_until = now + 0.16;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        let bw = (W - 22.0 + 8.0 - 14.0) / 3.0;
        let mut x = card.left() + 11.0 - 4.0;
        let y = card.top() + 7.0;
        let mut pick = None;
        for (shape, icon, label, hint) in SHAPES {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(bw, btn_h));
            let tool = Tool::Shape(shape);
            let blocked = self.plan.tool_blocked(tool, &self.link);
            let look = fabar::Look {
                active: self.tool == tool,
                blocked: blocked.is_some(),
                next: false,
                ..Default::default()
            };
            let title = blocked.unwrap_or_else(|| hint.to_string());
            if fabar::button(
                ui,
                r,
                icon,
                &label.to_uppercase(),
                &title,
                look,
                (1.0, false),
                ("mp", label),
            ) {
                pick = Some(tool);
            }
            x += bw + 7.0;
        }
        if let Some(t) = pick {
            if let Some(why) = self.plan.tool_blocked(t, &self.link) {
                self.hint = why;
            } else {
                self.set_tool(t);
            }
            self.tools.pattern_pinned = false;
            self.tools.pattern_until = 0.0;
        }
        let fy = y + btn_h + 7.0;
        painter.hline(card.x_range(), fy, egui::Stroke::new(1.0, style::border_soft()));
        let col = (W - 22.0 - 8.0) / 2.0;
        for (k, (label, range, value)) in [
            ("SPACING (M)", geometry::PATTERN_SPACING_M, self.draft.spacing),
            ("WIDTH (M)", geometry::PATTERN_WIDTH_M, self.draft.width),
        ]
        .into_iter()
        .enumerate()
        {
            let fx = card.left() + 11.0 + k as f32 * (col + 8.0);
            let g = style::galley(ui, label, 10.0, Weight::W600, style::text_3(), 0.7);
            painter.galley(Pos2::new(fx, fy + 10.0), g, style::text_3());
            let input = Rect::from_min_size(Pos2::new(fx, fy + 9.0 + 12.0 + 4.0), Vec2::new(col, 26.0));
            let shown = crate::data::units::js_number(value);
            if let Some(text) = field::committed(ui, input, ("patfield", k), &shown, "", true) {
                if let Ok(v) = text.trim().parse::<f64>() {
                    let v = clamp_number(v, range.min, range.max, range.def);
                    if k == 0 {
                        self.draft.spacing = v;
                    } else {
                        self.draft.width = v;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> Plan {
        let mut p = Plan::default();
        p.place_start(11.0, 48.0, false);
        p
    }

    fn hint(p: &Plan, t: Tool, link: &Link) -> String {
        hint_for(p, t, link, &Draft::default(), false)
    }

    #[test]
    fn the_hint_follows_the_order_a_mission_is_drawn_in() {
        let link = Link::default();
        let mut p = Plan::default();
        assert_eq!(
            hint(&p, Tool::Select, &link),
            "Press START and click where the aircraft takes off."
        );
        let near = Link {
            connected: true,
            start: Some((11.0, 48.0)),
        };
        assert_eq!(
            hint(&p, Tool::Select, &near),
            "Press AIRCRAFT to start where the connected aircraft stands, or START to click a place."
        );
        assert_eq!(
            hint(&p, Tool::Home, &link),
            "Click the map where the aircraft takes off."
        );
        p = started();
        assert_eq!(
            hint(&p, Tool::Home, &near),
            "Click the map to move the start. The aircraft takes off there. AIRCRAFT puts it where the aircraft stands."
        );
        assert_eq!(
            hint(&p, Tool::Select, &link),
            "Now add points with POINT, HOLD or PATTERN, then end with LAND or RETURN."
        );
        p.add_item(Kind::Waypoint, Some((11.001, 48.0)), None);
        assert_eq!(
            hint(&p, Tool::Select, &link),
            "End the mission with LAND or RETURN. Drag a point to move it, right-click to delete it."
        );
        p.add_item(Kind::Land, Some((11.002, 48.0)), None);
        assert_eq!(
            hint(&p, Tool::Select, &link),
            "Drag a point to move it, right-click to delete it. New points go in before the end."
        );
        assert_eq!(
            hint(&p, Tool::Item(Kind::Waypoint), &link),
            "Click the map to add a waypoint before a landing. Esc to stop."
        );
        assert_eq!(
            hint(&p, Tool::Shape(Shape::Area), &link),
            "Click the corners of the area. Enter or a double click finishes. Right click undoes a point. Esc to stop."
        );
    }

    #[test]
    fn a_plan_that_drives_is_spoken_to_in_its_own_words() {
        let link = Link::default();
        let mut p = Plan::default();
        p.choose_aircraft("rover");
        assert_eq!(
            hint(&p, Tool::Select, &link),
            "Press START and click where the vehicle starts driving."
        );
        p.place_start(11.0, 48.0, false);
        assert_eq!(
            hint(&p, Tool::Select, &link),
            "Now add points with POINT, HOLD or PATTERN, then end with RETURN or at the last point."
        );
        let list = entries(true);
        assert!(
            !list.iter().flatten().any(|e| e.label == "LAND"),
            "a rover does not land"
        );
        assert!(entries(false).iter().flatten().any(|e| e.label == "LAND"));
    }

    #[test]
    fn the_bar_has_the_web_pages_tools_in_its_order() {
        let labels: Vec<&str> = entries(false)
            .iter()
            .map(|e| e.as_ref().map_or("|", |e| e.label))
            .collect();
        assert_eq!(
            labels,
            ["SELECT", "START", "AIRCRAFT", "|", "POINT", "HOLD", "PATTERN", "LAND", "|", "RETURN"]
        );
        let driving: Vec<&str> = entries(true).iter().flatten().filter_map(|e| e.drive_label).collect();
        assert_eq!(driving, ["VEHICLE"]);
    }
}
