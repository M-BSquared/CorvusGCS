//! The altitude profile under the map (`drawProfile`, `profileRange`,
//! `hitTest` and the altitude drag of `mission.js`; `.mission-profile*` of
//! `mission.css`): the route seen from the side, with the ground under it and
//! each point's height draggable.

use crate::ui::tip::HoverTr;
use eframe::egui::{self, Color32, Pos2, Rect, Shape as Paint, Stroke, Vec2};

use crate::mission::geometry::{
    interpolate_at, landing_approach, profile_shape, stations, Ground, Mark, MarkKind, StationKind,
};
use crate::mission::issues::{degrees_text, length_text, GLIDE_MAX_DEG};
use crate::mission::plan::{Aircraft, Kind, AIRCRAFT, APPROACH_MIN_M, DEFAULT_ALT_M};
use crate::mission::MissionPage;
use crate::ui::card::{self, Look};
use crate::ui::kit::{self, Choice};
use crate::ui::style::{self, Weight};

const MARGIN: (f32, f32, f32, f32) = (60.0, 18.0, 12.0, 40.0);
const HEAD_H: f32 = 44.0;
const GRAB: f32 = 18.0;

pub struct Profile {
    pub collapsed: bool,
    drag: Option<(u32, bool)>,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            collapsed: true,
            drag: None,
        }
    }
}

impl Profile {
    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    /// `openProfileForEnding`: the profile opens the first time the mission
    /// gets an ending, which is when its landing needs checking.
    pub fn open_for_ending(&mut self) {
        self.collapsed = false;
    }

    /// The panel's height, for the map's own credit to sit above.
    pub fn height(&self, narrow: bool) -> f32 {
        14.0 + 8.0 + self.panel_h(narrow, false)
    }

    /// The height of the band between the map and the sidebar when the page
    /// is stacked: the grid row is `--mission-profile-h` (210 px below 1280)
    /// unless the profile is folded, when it is `auto`.
    pub fn band_height(&self) -> f32 {
        if self.collapsed {
            HEAD_H
        } else {
            210.0
        }
    }

    fn panel_h(&self, narrow: bool, drives: bool) -> f32 {
        if self.collapsed || drives {
            HEAD_H
        } else if narrow {
            210.0
        } else {
            240.0
        }
    }
}

/// `profileRange`: the y range that shows everything, with headroom.
pub fn profile_range(alts: &[f64], ground: Option<&Ground>) -> (f64, f64) {
    let (mut low, mut high) = (0.0f64, DEFAULT_ALT_M);
    for a in alts {
        low = low.min(*a);
        high = high.max(*a);
    }
    if let Some(g) = ground {
        for e in &g.elevations {
            low = low.min(*e);
            high = high.max(*e);
        }
    }
    let pad = 12.0f64.max((high - low) * 0.18);
    (low - pad, high + pad)
}

/// Round tick values for `lo..hi` (in the display unit, `per_si` per metre),
/// returned in metres (`unitTicks`).
pub fn nice_ticks(lo: f64, hi: f64, per_si: f64) -> Vec<f64> {
    let (a, b) = (lo * per_si, hi * per_si);
    if b <= a || !a.is_finite() || !b.is_finite() {
        return Vec::new();
    }
    let raw = (b - a) / 5.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|k| k * mag)
        .find(|k| *k >= raw)
        .unwrap_or(mag * 10.0);
    let mut out = Vec::new();
    let mut v = (a / step).ceil() * step;
    while v <= b + step * 1e-9 {
        out.push(v / per_si);
        v += step;
    }
    out
}

/// A value in a range placed in a pixel span, and back.
#[derive(Debug, Clone, Copy)]
pub struct Axis {
    pub lo: f64,
    pub hi: f64,
    pub from: f32,
    pub to: f32,
}

impl Axis {
    pub fn px(&self, v: f64) -> f32 {
        self.from + ((v - self.lo) / (self.hi - self.lo)) as f32 * (self.to - self.from)
    }

    pub fn value(&self, px: f32) -> f64 {
        self.lo + f64::from((px - self.from) / (self.to - self.from)) * (self.hi - self.lo)
    }
}

fn mark_colour(m: &Mark) -> Color32 {
    match m.kind {
        MarkKind::Home => style::healthy(),
        MarkKind::Descent | MarkKind::Item(Kind::Land) | MarkKind::Item(Kind::Rtl) => style::accent(),
        MarkKind::Item(Kind::Takeoff) => style::healthy(),
        MarkKind::Item(Kind::LoiterTurns) | MarkKind::Item(Kind::LoiterTime) => style::warning(),
        MarkKind::Item(Kind::Waypoint) => style::nav(),
    }
}

fn hover_text(page: &MissionPage, m: &Mark, index: usize, ground: Option<&Ground>) -> String {
    let units = &page.units;
    let item = page.plan.items.iter().find(|i| i.id == m.id);
    let descent = m.kind == MarkKind::Descent;
    let label = match (m.kind, item) {
        (MarkKind::Home, _) => "Start".to_string(),
        (_, Some(i)) => i.display_name().to_string(),
        (MarkKind::Item(k), None) => k.label().to_string(),
        _ => "Land".to_string(),
    };
    let dive = descent && m.alt <= APPROACH_MIN_M;
    let mut parts = vec![
        format!(
            "{}{label}{}",
            if index == 0 {
                String::new()
            } else {
                format!("{index}. ")
            },
            if descent {
                if dive {
                    ": straight in"
                } else {
                    ": descent starts"
                }
            } else {
                ""
            }
        ),
        format!("{} above home", length_text(units, m.alt)),
    ];
    if m.kind == MarkKind::Item(Kind::Rtl) {
        let read = page.plan.return_profile.is_some_and(|r| r.known);
        parts.push(
            match (page.plan.aircraft, read) {
                (Some(Aircraft::FixedWing), true) => "circles here, height read from the vehicle",
                (Some(Aircraft::FixedWing), false) => "circles here, at its return height",
                (_, true) => "heights read from the vehicle",
                _ => "return height is set on the vehicle",
            }
            .to_string(),
        );
    }
    if let Some(under) = ground.and_then(|g| interpolate_at(g, m.distance)) {
        parts.push(format!("{} over ground", length_text(units, m.alt - under)));
    }
    parts.push(format!("{} along", units.format_distance(m.distance)));
    parts.join("\n")
}

fn resting_note(drives: bool) -> &'static str {
    if drives {
        "A rover or a boat drives the route at ground level. Heights are not used."
    } else {
        "Drag a point up or down to set its height."
    }
}

impl MissionPage {
    /// Draws the profile panel along the bottom of the map, or, stacked
    /// (`band`), as a full width band of its own under the map.
    pub(crate) fn show_profile(&mut self, ui: &mut egui::Ui, map: Rect, narrow: bool, band: bool) {
        let drives = self.plan.drives;
        let h = self.profile.panel_h(narrow, drives);
        let panel = if band {
            map
        } else {
            Rect::from_min_size(
                Pos2::new(map.left() + 14.0, map.bottom() - 14.0 - h),
                Vec2::new(map.width() - 28.0, h),
            )
        };
        let painter = ui.painter().clone();
        if band {
            painter.rect_filled(panel, 0, style::glass(0.55));
            painter.hline(panel.x_range(), panel.top() + 0.5, Stroke::new(1.0, style::tint(0.06)));
        } else {
            style::glass_panel(&painter, "missionprofile", panel, style::RADIUS, 0.55);
        }
        ui.interact(
            panel,
            ui.scope_id().with("missionprofile"),
            egui::Sense::click_and_drag(),
        );

        // head
        let open = !self.profile.collapsed && !drives;
        let mut x = panel.left() + 14.0 - 6.0;
        let cy = panel.top() + if open { 6.0 + 14.0 } else { HEAD_H / 2.0 };
        if !drives {
            let t = Rect::from_center_size(Pos2::new(x + 14.0, cy), Vec2::splat(28.0));
            if kit::icon_btn(
                ui,
                t,
                if open { "chevron-up" } else { "chevron-down" },
                "Show altitude profile",
                false,
                "mprofiletoggle",
            ) {
                self.profile.collapsed = !self.profile.collapsed;
            }
        }
        x += 28.0 + if drives { 6.0 } else { 0.0 } + 4.0;
        let title = if drives { "GROUND ROUTE" } else { "ALTITUDE PROFILE" };
        let tg = style::galley(ui, title, 10.0, Weight::W400, style::text_3(), 0.9);
        painter.galley(Pos2::new(x, cy - tg.size().y / 2.0), tg.clone(), style::text_3());
        x += tg.size().x + 12.0;

        // the vehicle picker at the right
        let picker_w = 118.0;
        let label = style::galley(ui, "VEHICLE", 9.5, Weight::W600, style::text_3(), 0.9);
        let sel = Rect::from_center_size(
            Pos2::new(panel.right() - 14.0 - picker_w / 2.0, cy),
            Vec2::new(picker_w, 25.0),
        );
        let lx = sel.left() - 8.0 - label.size().x;
        let show_picker = open || drives;
        if show_picker {
            painter.galley(Pos2::new(lx, cy - label.size().y / 2.0), label, style::text_3());
            let current = self.plan.aircraft.map_or("", |a| a.value());
            let mut choices = vec![Choice::new("", "Not set")];
            choices.extend(AIRCRAFT.iter().map(|a| Choice::new(a.value(), a.label())));
            if self.plan.aircraft_locked {
                let label = choices
                    .iter()
                    .find(|c| c.value == current)
                    .map_or("", |c| c.label.as_str());
                card::paint(
                    &painter,
                    sel,
                    Look::custom(
                        style::input(0.6).gamma_multiply(0.5),
                        style::border(),
                        style::RADIUS_BTN,
                    ),
                );
                let g = style::galley(ui, label, 11.0, Weight::W400, style::text_3(), 0.0);
                painter.galley(Pos2::new(sel.left() + 8.0, cy - g.size().y / 2.0), g, style::text_3());
                ui.interact(sel, ui.scope_id().with("maircraftlocked"), egui::Sense::hover())
                    .on_hover_tr("The connected vehicle sets this.");
            } else if let Some(v) = kit::select(ui, sel, &choices, current, "Not set", false, "", "maircraft") {
                if self.plan.choose_aircraft(&v) {
                    self.refresh();
                }
            }
            popover_hint(ui, Pos2::new(lx - 8.0 - 8.0, cy));
        }
        if open || drives {
            let note = self.profile_note(drives);
            let room = (lx - 30.0 - x).max(0.0);
            let g = style::galley(ui, &note, 11.0, Weight::W400, style::text_3(), 0.0);
            let clip = Rect::from_min_size(Pos2::new(x, cy - 10.0), Vec2::new(room, 20.0));
            ui.painter_at(clip)
                .galley(Pos2::new(x, cy - g.size().y / 2.0), g, style::text_3());
        }
        if open {
            let plot = Rect::from_min_max(
                Pos2::new(panel.left(), panel.top() + 6.0 + 28.0 + 2.0),
                Pos2::new(panel.right() - 6.0, panel.bottom() - 4.0),
            );
            self.plot(ui, plot);
        }
    }

    fn profile_note(&self, drives: bool) -> String {
        if let Some((id, descent)) = self.profile.drag {
            if let Some(item) = self.plan.items.iter().find(|i| i.id == id) {
                let height = if descent {
                    item.approach_alt.unwrap_or_else(|| self.plan.height_before(id))
                } else {
                    item.alt
                };
                let mut s = format!(
                    "{}: {}{} above home",
                    item.display_name(),
                    if descent { "descends from " } else { "" },
                    length_text(&self.units, height)
                );
                let under = stations(&self.plan.items, self.plan.home.as_ref())
                    .iter()
                    .find(|st| st.id == id)
                    .and_then(|st| self.ground.as_ref().and_then(|g| interpolate_at(g, st.distance)));
                if let Some(u) = under {
                    s += &format!("  ·  {} over ground", length_text(&self.units, height - u));
                }
                return s;
            }
        }
        resting_note(drives).to_string()
    }

    fn plot(&mut self, ui: &mut egui::Ui, area: Rect) {
        let units = self.units;
        let painter = ui.painter_at(area);
        let points = stations(&self.plan.items, self.plan.home.as_ref());
        let shape = profile_shape(&points, self.plan.aircraft, self.plan.return_profile.as_ref());
        let alts: Vec<f64> = points.iter().map(|p| p.alt).collect();
        let (lo, hi) = profile_range(&alts, self.ground.as_ref());
        let last = shape
            .x
            .iter()
            .copied()
            .chain(self.ground.iter().flat_map(|g| g.distances.iter().copied()))
            .fold(0.0f64, f64::max);
        let far = if last > 0.0 { last * 1.1 } else { 6000.0 };
        let pa = Rect::from_min_max(
            Pos2::new(area.left() + MARGIN.0, area.top() + MARGIN.2),
            Pos2::new(area.right() - MARGIN.1, area.bottom() - MARGIN.3),
        );
        let ax = Axis {
            lo: 0.0,
            hi: far,
            from: pa.left(),
            to: pa.right(),
        };
        let ay = Axis {
            lo,
            hi,
            from: pa.bottom(),
            to: pa.top(),
        };
        let grid = style::text_3().gamma_multiply(0.22);
        let tick_font = |ui: &egui::Ui, s: &str| style::galley_mono(ui, s, 10.0, style::text_3());
        // y grid and labels
        for v in nice_ticks(lo, hi, units.length.per_si) {
            let y = ay.px(v);
            if !(pa.top() - 0.5..=pa.bottom() + 0.5).contains(&y) {
                continue;
            }
            let zero = v.abs() < 1e-9;
            painter.hline(
                pa.x_range(),
                y,
                Stroke::new(
                    1.0,
                    if zero {
                        style::text_3().gamma_multiply(0.6)
                    } else {
                        grid
                    },
                ),
            );
            let g = tick_font(
                ui,
                &crate::data::units::js_number(((v * units.length.per_si) * 1e6).round() / 1e6),
            );
            painter.galley(
                Pos2::new(pa.left() - 8.0 - g.size().x, y - g.size().y / 2.0),
                g,
                style::text_3(),
            );
        }
        // x ticks
        for v in nice_ticks(0.0, far, units.distance.per_si) {
            let x = ax.px(v);
            painter.vline(
                x,
                egui::Rangef::new(pa.bottom(), pa.bottom() + 4.0),
                Stroke::new(1.0, style::border()),
            );
            let g = tick_font(
                ui,
                &crate::data::units::js_number(((v * units.distance.per_si) * 1e6).round() / 1e6),
            );
            painter.galley(Pos2::new(x - g.size().x / 2.0, pa.bottom() + 6.0), g, style::text_3());
        }
        let xt = style::galley(
            ui,
            &format!("Distance ({})", units.distance.symbol),
            10.0,
            Weight::W400,
            style::text_3(),
            0.0,
        );
        painter.galley(
            Pos2::new(pa.center().x - xt.size().x / 2.0, area.bottom() - 2.0 - xt.size().y),
            xt,
            style::text_3(),
        );
        let yt = style::galley(
            ui,
            &format!("{} above home", units.length.symbol),
            10.0,
            Weight::W400,
            style::text_3(),
            0.0,
        );
        painter.galley(Pos2::new(area.left() + 6.0, pa.top() - 10.0), yt, style::text_3());

        let clip = ui.painter_at(pa.expand2(Vec2::new(6.0, 6.0)));
        // the ground
        if let Some(g) = &self.ground {
            let top: Vec<Pos2> = g
                .distances
                .iter()
                .zip(&g.elevations)
                .map(|(d, e)| Pos2::new(ax.px(*d), ay.px(*e)))
                .collect();
            if top.len() >= 2 {
                let mut mesh = egui::Mesh::default();
                let fill = style::text_3().gamma_multiply(0.25);
                for p in &top {
                    mesh.colored_vertex(*p, fill);
                    mesh.colored_vertex(Pos2::new(p.x, pa.bottom()), fill);
                }
                for i in 0..top.len() as u32 - 1 {
                    let k = i * 2;
                    mesh.add_triangle(k, k + 1, k + 2);
                    mesh.add_triangle(k + 1, k + 3, k + 2);
                }
                clip.add(mesh);
                clip.add(Paint::line(top, Stroke::new(1.5, style::text_3())));
            }
        }
        // the route
        let line: Vec<Pos2> = shape
            .x
            .iter()
            .zip(&shape.y)
            .map(|(x, y)| Pos2::new(ax.px(*x), ay.px(*y)))
            .collect();
        if line.len() >= 2 {
            let base = ay.px(0.0).min(pa.bottom());
            let mut mesh = egui::Mesh::default();
            let fill = style::accent().gamma_multiply(0.1);
            for p in &line {
                mesh.colored_vertex(*p, fill);
                mesh.colored_vertex(Pos2::new(p.x, base), fill);
            }
            for i in 0..line.len() as u32 - 1 {
                let k = i * 2;
                mesh.add_triangle(k, k + 1, k + 2);
                mesh.add_triangle(k + 1, k + 3, k + 2);
            }
            clip.add(mesh);
            clip.add(Paint::line(line, Stroke::new(2.5, style::accent())));
        }
        // what the profile writes on itself: the glide into a fixed wing's
        // landing, and where its return circles
        if self.plan.aircraft == Some(Aircraft::FixedWing) {
            let note = |text: &str, at: Pos2, colour: Color32, right: bool| {
                let g = style::galley_mono(ui, text, 10.0, colour);
                let pos = if right {
                    Pos2::new(at.x - 7.0 - g.size().x, at.y - 4.0 - g.size().y)
                } else {
                    Pos2::new(at.x + 5.0, at.y - 3.0 - g.size().y)
                };
                clip.galley(pos, g, colour);
            };
            if let (Some(a), Some(i)) = (
                landing_approach(&points),
                points.iter().position(|s| s.kind == StationKind::Item(Kind::Land)),
            ) {
                let mid = Pos2::new(
                    ax.px((points[i - 1].distance + points[i].distance) / 2.0),
                    ay.px((points[i - 1].alt + points[i].alt) / 2.0),
                );
                let colour = if a.degrees > GLIDE_MAX_DEG {
                    style::warning()
                } else {
                    style::text_2()
                };
                note(&format!("{} glide", degrees_text(a.degrees)), mid, colour, false);
            }
            if let Some(m) = shape.marks.iter().find(|m| m.kind == MarkKind::Item(Kind::Rtl)) {
                note(
                    "circles here",
                    Pos2::new(ax.px(m.distance), ay.px(m.alt)),
                    style::warning(),
                    true,
                );
            }
        }
        // the marks
        let hover = ui.input(|i| i.pointer.hover_pos());
        let mut best: Option<(f32, usize)> = None;
        let marks = &shape.marks;
        for (k, m) in marks.iter().enumerate() {
            let c = Pos2::new(ax.px(m.distance), ay.px(m.alt));
            if let Some(h) = hover {
                let d = h.distance(c);
                if d < GRAB && best.is_none_or(|b| d < b.0) {
                    best = Some((d, k));
                }
            }
        }
        for m in marks.iter() {
            let c = Pos2::new(ax.px(m.distance), ay.px(m.alt));
            if self.plan.selected == Some(m.id) && m.id != 0 {
                clip.circle_stroke(c, 8.5, Stroke::new(2.5, style::text_1()));
            }
            clip.circle_filled(c, 5.0, style::bg());
            clip.circle_filled(c, 3.0, mark_colour(m));
        }
        // pointer: press selects and, on a point with a height, drags it
        let id = ui.scope_id().with("missionplot");
        let resp = ui.interact(pa, id, egui::Sense::click_and_drag());
        if resp.drag_started() {
            if let Some((_, k)) = best {
                let m = &marks[k];
                if m.id != 0 {
                    self.plan.selected = Some(m.id);
                }
                let descent = m.kind == MarkKind::Descent;
                let draggable = descent
                    || !(m.kind == MarkKind::Home
                        || m.kind == MarkKind::Item(Kind::Land)
                        || m.kind == MarkKind::Item(Kind::Rtl));
                if draggable && m.id != 0 {
                    self.profile.drag = Some((m.id, descent));
                }
            }
        } else if resp.clicked() {
            if let Some((_, k)) = best {
                if marks[k].id != 0 {
                    self.plan.selected = Some(marks[k].id);
                }
            }
        }
        if let Some((did, descent)) = self.profile.drag {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            if let Some(p) = resp.interact_pointer_pos().or(hover) {
                let alt = ay.value(p.y);
                if descent {
                    self.plan.set_approach(did, Some(alt));
                } else {
                    self.plan.set_altitude(did, alt);
                }
            }
            if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                self.profile.drag = None;
                self.refresh();
            }
        } else if let Some((_, k)) = best {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            let text = hover_text(self, &marks[k], k, self.ground.as_ref());
            resp.on_hover_tr(text);
        }
    }
}

fn popover_hint(ui: &mut egui::Ui, centre: Pos2) {
    crate::ui::popover::info(
        ui,
        centre,
        "Vehicle",
        "What flies this plan. The planner only tells an aircraft that hovers from one that circles: a quadcopter, hexacopter or octocopter is a multicopter, and all of them are planned the same way. A multicopter hovers at a Hold where a fixed wing circles it, so a Hold's ring, radius and direction are shown only for an aircraft that flies them. A rover or a boat drives the route on the ground: no takeoff, no landing and no heights. While a vehicle is connected, it sets this and the list names it.",
        "maircrafthint",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_range_has_room_over_the_highest_point_and_under_the_lowest() {
        let (lo, hi) = profile_range(&[0.0, 30.0, 40.0], None);
        assert_eq!((lo, hi), (-12.0, 52.0));
        let (lo, hi) = profile_range(&[0.0, 300.0], None);
        assert!((lo - -54.0).abs() < 1e-9 && (hi - 354.0).abs() < 1e-9, "{lo} {hi}");
        let g = Ground {
            distances: vec![0.0, 1.0],
            elevations: vec![-20.0, 80.0],
        };
        let (lo, hi) = profile_range(&[0.0, 30.0], Some(&g));
        assert!((lo - -38.0).abs() < 1e-9 && (hi - 98.0).abs() < 1e-9, "{lo} {hi}");
    }

    #[test]
    fn ticks_are_round_numbers_in_the_display_unit_placed_in_metres() {
        let t = nice_ticks(-12.0, 52.0, 1.0);
        assert_eq!(t, [0.0, 20.0, 40.0], "a step of 20 is the first round one over 12.8");
        let ft = nice_ticks(0.0, 100.0, 1.0 / 0.3048);
        assert!(
            (ft[1] - 100.0 * 0.3048).abs() < 1e-9,
            "100 ft is the first round step over 66 ft: {ft:?}"
        );
        assert!(nice_ticks(5.0, 5.0, 1.0).is_empty());
    }

    #[test]
    fn a_pixel_and_a_height_map_into_each_other() {
        let a = Axis {
            lo: -10.0,
            hi: 90.0,
            from: 200.0,
            to: 100.0,
        };
        assert_eq!(a.px(-10.0), 200.0);
        assert_eq!(a.px(90.0), 100.0);
        assert_eq!(a.px(40.0), 150.0);
        assert!((a.value(150.0) - 40.0).abs() < 1e-9);
        assert!((a.value(a.px(12.5)) - 12.5).abs() < 1e-4);
    }

    #[test]
    fn the_profile_starts_folded_and_opens_for_an_ending() {
        let mut p = Profile::default();
        assert!(p.collapsed);
        assert_eq!(p.panel_h(false, false), HEAD_H);
        p.open_for_ending();
        assert_eq!(p.panel_h(false, false), 240.0);
        assert_eq!(p.panel_h(true, false), 210.0);
        assert_eq!(p.panel_h(false, true), HEAD_H, "a plan that drives has no chart");
    }
}
