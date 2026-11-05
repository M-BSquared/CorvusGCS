//! The sidebar (`buildSidebar`, `renderList`, `renderDetail`,
//! `renderSummary`, `renderIssues`, `renderVehicle`, `buildActions` of
//! `mission.js`; `.mission-side*` of `mission.css`): the plan's name, the
//! summary, what is wrong with the plan, the list of items in the order they
//! are flown, the editor for the selected item, and the buttons that put the
//! plan on the vehicle or in a file.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::data::units::js_number;
use crate::mission::backend::Job;
use crate::mission::issues::{
    describe_item, length_from_shown, problems, shown_length, shown_speed, speed_from_shown, summary, EMPTY_PLAN,
};
use crate::mission::plan::{
    clamp_number, clean_point_name, Aircraft, Item, Kind, Param, POINT_NAME_MAX, SPEED_MAX_MS, SPEED_MIN_MS,
};
use crate::mission::progress::vehicle_line;
use crate::mission::{Dialog, Level, MissionPage};
use crate::ui::card::{self, Look};
use crate::ui::field;
use crate::ui::icons;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::listrow::{self, Mark};
use crate::ui::popover;
use crate::ui::style::{self, Weight};

const PAD: f32 = 14.0;
const GAP: f32 = 10.0;
const ROW_H: f32 = 28.6;
const ROW_GAP: f32 = 2.0;
const FIELD_H: f32 = 26.5;
const LABEL_H: f32 = 12.0;

/// A row being carried to a new place in the plan.
#[derive(Debug, Clone, Copy)]
pub struct RowDrag {
    pub id: u32,
    /// From the row's top to where it was grabbed.
    pub grab: f32,
    /// The pointer's height.
    pub y: f32,
}

#[derive(Default)]
pub struct SideState {
    pub row_drag: Option<RowDrag>,
    name_for: Option<u32>,
    name_buf: String,
}

impl SideState {
    pub fn end_row_drag(&mut self) {
        self.row_drag = None;
    }
}

/// `dropSlot`: which slot a dragged row lands in, given the boxes of the
/// others (top, height) and the middle of the dragged row.
pub fn drop_slot(others: &[(f32, f32)], centre: f32) -> usize {
    others.iter().filter(|(top, h)| top + h / 2.0 < centre).count()
}

#[derive(Clone, Copy, PartialEq)]
enum Fid {
    Lat,
    Lon,
    Alt,
    Speed,
    Descent,
    Param(Param),
}

struct Fld {
    id: Fid,
    label: String,
}

/// The editor's fields as rows of one or two, in the order the web page puts them.
fn detail_rows(page: &MissionPage, item: &Item) -> Vec<Vec<Fld>> {
    let u = &page.units;
    let plan = &page.plan;
    let speed = |u: &crate::data::units::Units| Fld {
        id: Fid::Speed,
        label: format!("Speed ({})", u.speed.symbol),
    };
    let mut rows: Vec<Vec<Fld>> = Vec::new();
    if item.kind.position() {
        rows.push(vec![
            Fld {
                id: Fid::Lat,
                label: "Latitude".into(),
            },
            Fld {
                id: Fid::Lon,
                label: "Longitude".into(),
            },
        ]);
        if plan.drives {
            rows.push(vec![speed(u)]);
        } else if item.kind != Kind::Land {
            rows.push(vec![
                Fld {
                    id: Fid::Alt,
                    label: format!(
                        "{} ({})",
                        if item.kind == Kind::Takeoff {
                            "Climb to (above start)"
                        } else {
                            "Altitude above home"
                        },
                        u.length.symbol
                    ),
                },
                speed(u),
            ]);
        } else if plan.aircraft == Some(Aircraft::Multirotor) {
            rows.push(vec![
                Fld {
                    id: Fid::Descent,
                    label: format!("Descend from ({})", u.length.symbol),
                },
                speed(u),
            ]);
        } else {
            rows.push(vec![speed(u)]);
        }
    } else {
        rows.push(vec![speed(u)]);
    }
    let mut pending: Vec<Fld> = Vec::new();
    for p in item.kind.params() {
        if !plan.param_applies(item, *p) {
            continue;
        }
        let r = p.rule();
        let wide = *p == Param::Direction;
        let unit = if r.unit == "m" { u.length.symbol } else { r.unit };
        let fld = Fld {
            id: Fid::Param(*p),
            label: if unit.is_empty() || wide {
                r.label.to_string()
            } else {
                format!("{} ({unit})", r.label)
            },
        };
        if wide {
            if !pending.is_empty() {
                rows.push(std::mem::take(&mut pending));
            }
            rows.push(vec![fld]);
        } else {
            pending.push(fld);
            if pending.len() == 2 {
                rows.push(std::mem::take(&mut pending));
            }
        }
    }
    if !pending.is_empty() {
        rows.push(pending);
    }
    rows
}

fn rows_height(rows: &[Vec<Fld>]) -> f32 {
    rows.len() as f32 * (LABEL_H + 3.0 + FIELD_H) + rows.len().saturating_sub(1) as f32 * 8.0
}

impl MissionPage {
    /// The text a field reads as, and its placeholder.
    fn field_text(&self, item: &Item, id: Fid) -> (String, String) {
        let u = &self.units;
        match id {
            Fid::Lat => (js_number((item.lat * 1e7).round() / 1e7), String::new()),
            Fid::Lon => (js_number((item.lon * 1e7).round() / 1e7), String::new()),
            Fid::Alt => (js_number(shown_length(u, item.alt)), String::new()),
            Fid::Speed => (
                item.speed.map(|s| js_number(shown_speed(u, s))).unwrap_or_default(),
                self.plan
                    .speed_into(item.id)
                    .map_or("airframe".to_string(), |s| js_number(shown_speed(u, s))),
            ),
            Fid::Descent => (
                item.approach_alt
                    .map(|a| js_number(shown_length(u, a)))
                    .unwrap_or_default(),
                js_number(shown_length(u, self.plan.height_before(item.id))),
            ),
            Fid::Param(p) => {
                let v = item.get(p);
                let text = if p.rule().unit == "m" {
                    js_number(shown_length(u, v))
                } else {
                    js_number(v)
                };
                (text, String::new())
            }
        }
    }

    /// An edit typed into a field of the item `id`.
    fn commit_field(&mut self, id: u32, fid: Fid, text: &str) {
        let text = text.trim();
        let u = self.units;
        let number = text.parse::<f64>().ok();
        let Some(item) = self.plan.items.iter().find(|i| i.id == id).cloned() else {
            return;
        };
        match fid {
            Fid::Speed => {
                let speed = match number {
                    _ if text.is_empty() => None,
                    None => return,
                    Some(n) => Some(clamp_number(
                        speed_from_shown(&u, n),
                        SPEED_MIN_MS,
                        SPEED_MAX_MS,
                        SPEED_MIN_MS,
                    )),
                };
                if let Some(it) = self.plan.item_mut(id) {
                    it.speed = speed;
                }
            }
            Fid::Descent => {
                let v = match number {
                    _ if text.is_empty() => None,
                    None => return,
                    Some(n) => Some(length_from_shown(&u, n)),
                };
                self.plan.set_approach(id, v);
            }
            Fid::Lat | Fid::Lon => {
                let Some(n) = number else { return };
                let (lat, lon) = if fid == Fid::Lat {
                    (clamp_number(n, -90.0, 90.0, item.lat), item.lon)
                } else {
                    (item.lat, clamp_number(n, -180.0, 180.0, item.lon))
                };
                if self.plan.start_takeoff().map(|t| t.id) == Some(id) {
                    self.plan.place_start(lon, lat, true);
                } else if let Some(it) = self.plan.item_mut(id) {
                    it.lat = lat;
                    it.lon = lon;
                }
            }
            Fid::Alt => {
                let Some(n) = number else { return };
                self.plan.set_altitude(id, length_from_shown(&u, n));
            }
            Fid::Param(p) => {
                let Some(n) = number else { return };
                let r = p.rule();
                let si = if r.unit == "m" { length_from_shown(&u, n) } else { n };
                if let Some(it) = self.plan.item_mut(id) {
                    it.set(p, clamp_number(si, r.min, r.max, r.def));
                    it.snap_direction();
                }
            }
        }
        self.refresh();
    }

    /// Draws the sidebar into `rect`.
    pub(crate) fn show_side(&mut self, ui: &mut egui::Ui, rect: Rect, narrow: bool) {
        let _ = narrow;
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0, style::surface_1());
        painter.vline(rect.left() + 0.5, rect.y_range(), Stroke::new(1.0, style::border()));
        ui.interact(rect, ui.scope_id().with("missionside"), egui::Sense::hover());
        let inner = Rect::from_min_max(
            Pos2::new(rect.left() + 1.0 + PAD, rect.top() + PAD),
            Pos2::new(rect.right() - PAD, rect.bottom() - PAD),
        );
        let w = inner.width();

        // The bottom stack first: its height decides what the list gets.
        let busy_up = self.calls.busy(|j| matches!(j, Job::Upload { .. }));
        let busy_read = self.calls.busy(|j| matches!(j, Job::Read));
        let block = |label: &'static str, icon, variant| Button::new(label, variant).icon(icon);
        let bh = block("x", "upload", Variant::Primary).height();
        let sm_h = Button::new("x", Variant::Secondary).small().height();
        let actions_h = bh * 2.0 + sm_h + 12.0;
        let status_h = match &self.status {
            Some((_, t)) => {
                kit::wrapped_weight(ui, t, 11.0, Weight::W400, style::text_2(), w, 1.4)
                    .size()
                    .y
            }
            None => 0.0,
        };
        let status_block = if status_h > 0.0 { status_h + GAP } else { 0.0 };
        let selected = self.plan.selected_item().cloned();
        let rows = selected.as_ref().map(|i| detail_rows(self, i)).unwrap_or_default();
        let content_h = if selected.is_some() {
            28.0 + 8.0 + rows_height(&rows)
        } else {
            16.0
        };
        let max_detail = (inner.height() * 0.42).max(120.0);
        let detail_h = content_h.min(max_detail);
        let detail_block = 1.0 + 10.0 + detail_h;

        let actions_top = inner.bottom() - actions_h;
        let status_top = actions_top - status_block;
        let detail_top = status_top - GAP - detail_block;
        let scroll = Rect::from_min_max(
            Pos2::new(rect.left() + 1.0, inner.top()),
            Pos2::new(rect.right(), (detail_top - GAP).max(inner.top() + 80.0)),
        );

        self.list_area(ui, &ctx, scroll, w);

        // The editor of the selected item.
        painter.hline(
            Rect::from_min_size(Pos2::new(inner.left(), detail_top), Vec2::new(w, 1.0)).x_range(),
            detail_top + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        let det = Rect::from_min_size(Pos2::new(inner.left(), detail_top + 11.0), Vec2::new(w, detail_h));
        self.detail(ui, det, selected.as_ref(), &rows, content_h);

        // status
        if let Some((level, text)) = self.status.clone() {
            let colour = match level {
                Level::Pending => style::nav(),
                Level::Ok => style::healthy(),
                Level::Warn => style::warning(),
                Level::Err => style::critical(),
            };
            let g = kit::wrapped_weight(ui, &text, 11.0, Weight::W400, colour, w, 1.4);
            painter.galley(Pos2::new(inner.left(), status_top), g, colour);
        }

        // actions
        let mut y = actions_top;
        let fly_label = if self.plan.drives {
            "Upload and drive"
        } else {
            "Upload and fly"
        };
        let r = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(w, bh));
        if block("Upload to vehicle", "upload", Variant::Primary)
            .enabled(!busy_up)
            .show(ui, r, "mup")
        {
            self.upload_plan(&ctx, false);
        }
        y += bh + 6.0;
        let r = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(w, bh));
        if block(fly_label, "play", Variant::Secondary)
            .enabled(!busy_up)
            .show(ui, r, "mfly")
        {
            self.upload_plan(&ctx, true);
        }
        y += bh + 6.0;
        let cw = (w - 12.0) / 4.0;
        let specs: [(&str, &'static str, Variant, &str); 4] = [
            ("Save", "save", Variant::Secondary, ""),
            ("Open", "folder-open", Variant::Secondary, ""),
            (
                "Read",
                "download",
                Variant::Secondary,
                "Read the mission the vehicle holds into the planner",
            ),
            ("New", "file-plus", Variant::Ghost, ""),
        ];
        let buttons: Vec<Button> = specs
            .into_iter()
            .enumerate()
            .map(|(k, (label, icon, variant, title))| {
                Button::new(label, variant)
                    .icon(icon)
                    .small()
                    .title(title)
                    .enabled(!(k == 2 && busy_read))
            })
            .collect();
        // Four equal columns, unless a longer word needs more: then each
        // column takes its text's share, as a grid track does (`1fr` never
        // goes below the content).
        let natural: Vec<f32> = buttons.iter().map(|b| b.width(ui)).collect();
        let widths: Vec<f32> = if natural.iter().any(|n| *n > cw) {
            let total: f32 = natural.iter().sum();
            let room = w - 12.0;
            natural.iter().map(|n| n * room / total).collect()
        } else {
            vec![cw; 4]
        };
        let mut x = inner.left();
        for (k, b) in buttons.into_iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(widths[k], sm_h));
            x += widths[k] + 4.0;
            if b.show(ui, r, ("mfile", k)) {
                match k {
                    0 => self.save_plan(&ctx),
                    1 => self.send(&ctx, Job::ListPlans),
                    2 => self.read_vehicle(&ctx),
                    _ => self.confirm_clear(),
                }
            }
        }
    }

    pub(crate) fn upload_plan(&mut self, ctx: &egui::Context, fly: bool) {
        if self.plan.items.is_empty() {
            self.say(Level::Warn, "Nothing to upload. The mission is empty.");
            return;
        }
        let warnings: Vec<_> = problems(&self.plan, &self.units, self.ground.as_ref(), self.link.start)
            .into_iter()
            .filter(|i| i.text != EMPTY_PLAN)
            .collect();
        if fly || !warnings.is_empty() {
            self.dialog = Dialog::Upload { fly, warnings };
        } else {
            self.send_plan(ctx, false);
        }
    }

    pub(crate) fn send_plan(&mut self, ctx: &egui::Context, fly: bool) {
        self.say(
            Level::Pending,
            if fly {
                "Uploading and starting…"
            } else {
                "Uploading…"
            },
        );
        self.send(ctx, Job::Upload { fly });
    }

    fn save_plan(&mut self, ctx: &egui::Context) {
        if self.plan.items.is_empty() {
            self.say(Level::Warn, "Nothing to save. The mission is empty.");
            return;
        }
        self.say(Level::Pending, "Saving…");
        self.send(ctx, Job::Save);
    }

    fn read_vehicle(&mut self, ctx: &egui::Context) {
        if self.plan.items.is_empty() {
            self.start_read(ctx);
        } else {
            self.dialog = Dialog::Read;
        }
    }

    pub(crate) fn start_read(&mut self, ctx: &egui::Context) {
        self.say(Level::Pending, "Reading the mission from the vehicle…");
        self.send(ctx, Job::Read);
    }

    pub(crate) fn confirm_clear(&mut self) {
        if self.plan.items.is_empty() && self.plan.home.is_none() {
            self.clear_plan();
        } else {
            self.dialog = Dialog::Clear;
        }
    }

    pub(crate) fn clear_plan(&mut self) {
        self.plan.clear();
        self.ground = None;
        self.clear_status();
        self.set_tool(crate::mission::plan::Tool::Select);
        self.refresh();
    }

    /// Everything above the editor, which scrolls as one.
    fn list_area(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, scroll: Rect, w: f32) {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(scroll));
        child.set_clip_rect(scroll);
        let x = scroll.left() + PAD;
        let units = self.units;
        let mut act: Option<ListAct> = None;
        egui::ScrollArea::vertical()
            .id_salt("missionside-scroll")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let top = ui.max_rect().top();
                let clip_top = ui.clip_rect().top();
                let painter = ui.painter().clone();
                let mut y = top;

                // the plan's name beside its caption
                let cap = style::galley(ui, "Mission", 13.0, Weight::W700, style::text_1(), 0.0);
                let in_h = 30.0;
                painter.galley(
                    Pos2::new(x, y + (in_h - cap.size().y) / 2.0),
                    cap.clone(),
                    style::text_1(),
                );
                let name_r = Rect::from_min_size(
                    Pos2::new(x + cap.size().x + 10.0, y),
                    Vec2::new(w - cap.size().x - 10.0, in_h),
                );
                let name = self.plan.name.clone();
                if let Some(t) = field::committed(ui, name_r, "planname", &name, "Mission", false) {
                    let t = t.trim();
                    self.plan.name = if t.is_empty() { "Mission".into() } else { t.to_string() };
                }
                y += in_h + GAP;

                // summary
                let cells = summary(&self.plan, &units, self.ground.as_ref());
                let cw = (w - 5.0) / 2.0;
                let ch = 24.5;
                for (k, (label, value, warn)) in cells.iter().enumerate() {
                    let r = Rect::from_min_size(
                        Pos2::new(x + (k % 2) as f32 * (cw + 5.0), y + (k / 2) as f32 * (ch + 5.0)),
                        Vec2::new(cw, ch),
                    );
                    painter.rect_filled(r, style::RADIUS_BTN, style::surface_2());
                    let line = if *warn {
                        mix(style::warning(), style::border_soft(), 0.55)
                    } else {
                        style::border_soft()
                    };
                    painter.rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, line), egui::StrokeKind::Inside);
                    let lg = style::galley(ui, &label.to_uppercase(), 9.0, Weight::W400, style::text_3(), 0.7);
                    painter.galley(
                        Pos2::new(r.left() + 9.0, r.center().y - lg.size().y / 2.0),
                        lg.clone(),
                        style::text_3(),
                    );
                    let vc = if *warn { style::warning() } else { style::text_1() };
                    let vg = style::galley_mono(ui, value, 12.0, vc);
                    painter.galley(
                        Pos2::new(r.right() - 9.0 - vg.size().x, r.center().y - vg.size().y / 2.0),
                        vg,
                        vc,
                    );
                }
                y += (cells.len().div_ceil(2)) as f32 * (ch + 5.0) - 5.0 + GAP;

                // the vehicle's side of it
                let (text, level, offer) = vehicle_line(&self.tracker, &self.plan);
                if !text.is_empty() {
                    let btn = Button::new("Read it", Variant::Secondary).icon("download").small();
                    let bw = if offer { btn.width(ui) } else { 0.0 };
                    let tw = (w - 18.0 - if offer { bw + 8.0 } else { 0.0 }).max(80.0);
                    let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_2(), tw, 1.4);
                    let h = (g.size().y.max(if offer { btn.height() } else { 0.0 })) + 14.0 + 2.0;
                    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
                    painter.rect_filled(r, style::RADIUS_BTN, style::surface_2());
                    let line = match level {
                        "live" => mix(style::healthy(), style::border(), 0.55),
                        "warn" => mix(style::warning(), style::border(), 0.55),
                        _ => style::border(),
                    };
                    painter.rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, line), egui::StrokeKind::Inside);
                    painter.galley(Pos2::new(r.left() + 10.0, r.top() + 8.0), g, style::text_2());
                    if offer {
                        let b = Rect::from_min_size(
                            Pos2::new(r.right() - 9.0 - bw, r.center().y - btn.height() / 2.0),
                            Vec2::new(bw, btn.height()),
                        );
                        if btn.show(ui, b, "mreadit") {
                            act = Some(ListAct::Read);
                        }
                    }
                    y += h + GAP;
                }

                // what is wrong with the plan
                let issues: Vec<_> = problems(&self.plan, &units, self.ground.as_ref(), self.link.start)
                    .into_iter()
                    .filter(|i| i.text != EMPTY_PLAN)
                    .collect();
                if !issues.is_empty() {
                    for (k, issue) in issues.iter().enumerate() {
                        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 27.0));
                        card::paint(
                            &painter,
                            r,
                            Look::custom(
                                style::warning().gamma_multiply(0.10),
                                mix(style::warning(), style::border_soft(), 0.45),
                                style::RADIUS_BTN,
                            ),
                        );
                        icons::paint(
                            ui,
                            "triangle-alert",
                            Rect::from_center_size(Pos2::new(r.left() + 8.0 + 7.0, r.center().y), Vec2::splat(12.0)),
                            style::warning(),
                        );
                        let room = w - 8.0 - 14.0 - 6.0 - if issue.info.is_empty() { 8.0 } else { 16.0 + 6.0 + 8.0 };
                        let g = style::galley(ui, &issue.text, 11.0, Weight::W400, style::text_2(), 0.0);
                        let clip = Rect::from_min_size(
                            Pos2::new(r.left() + 8.0 + 14.0 + 6.0, r.top()),
                            Vec2::new(room, r.height()),
                        );
                        ui.painter_at(clip).galley(
                            Pos2::new(clip.left(), r.center().y - g.size().y / 2.0),
                            g,
                            style::text_2(),
                        );
                        if !issue.info.is_empty() {
                            popover::info(
                                ui,
                                Pos2::new(r.right() - 8.0 - 8.0, r.center().y),
                                "",
                                &issue.info,
                                ("missue", k),
                            );
                        }
                        y += 27.0 + 4.0;
                    }
                    y += GAP - 4.0;
                }

                // the list
                let head_natural = y;
                y += 24.0 + 3.0 + 1.0 + GAP - 6.0;
                let list_top = y;
                let list_end = self.rows(ui, x, y, w, &mut act);
                y = list_end;
                // the head stays in reach on a plan longer than the panel
                let hy = head_natural.max(clip_top);
                let head = Rect::from_min_size(Pos2::new(scroll.left(), hy), Vec2::new(scroll.width(), 28.0));
                painter.rect_filled(head, 0, style::surface_1());
                painter.hline(
                    head.x_range(),
                    head.bottom() - 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                let tg = style::galley(ui, "ITEMS", 10.0, Weight::W400, style::text_3(), 0.9);
                painter.galley(Pos2::new(x, head.top() + 12.0 - tg.size().y / 2.0), tg, style::text_3());
                let tr = Rect::from_min_size(Pos2::new(x + w - 24.0, head.top()), Vec2::splat(24.0));
                if kit::icon_btn(ui, tr, "trash-2", "Clear the whole plan", false, "mclear") {
                    act = Some(ListAct::Clear);
                }
                let _ = list_top;
                ui.allocate_space(Vec2::new(scroll.width(), (y - top).max(1.0) + 4.0));
            });
        match act {
            Some(ListAct::Read) => self.start_read(ctx),
            Some(ListAct::Clear) => self.confirm_clear(),
            Some(ListAct::Select(id)) => self.plan.selected = Some(id),
            Some(ListAct::Remove(id)) => {
                self.plan.remove_item(id);
                self.refresh();
            }
            Some(ListAct::Move(id, d)) => {
                self.plan.move_item(id, d);
                self.refresh();
            }
            Some(ListAct::Drop(id, to)) => {
                self.plan.move_item_to(id, to as isize);
                self.refresh();
            }
            None => {}
        }
    }

    /// The items, one row each; returns the bottom.
    fn rows(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, act: &mut Option<ListAct>) -> f32 {
        let painter = ui.painter().clone();
        if self.plan.items.is_empty() {
            let text = if self.plan.home.is_some() {
                "Nothing planned yet. Add points from the tool bar.".to_string()
            } else {
                format!(
                    "Nothing planned yet. Press START and click where the {} {}.",
                    self.plan.craft(),
                    if self.plan.drives {
                        "starts driving"
                    } else {
                        "takes off"
                    }
                )
            };
            let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_3(), w - 16.0, 1.4);
            painter.galley(Pos2::new(x + 8.0, y + 8.0), g.clone(), style::text_3());
            return y + g.size().y + 16.0;
        }
        // the order they are drawn in: while a row is carried, the others close
        // up around the slot it would drop into
        let n = self.plan.items.len();
        let mut order: Vec<usize> = (0..n).collect();
        let pitch = ROW_H + ROW_GAP;
        let mut carried = None;
        if let Some(d) = self.side.row_drag {
            if let Some(from) = self.plan.index_of(d.id) {
                let others: Vec<usize> = (0..n).filter(|i| *i != from).collect();
                let boxes: Vec<(f32, f32)> = others
                    .iter()
                    .enumerate()
                    .map(|(k, _)| (y + k as f32 * pitch, ROW_H))
                    .collect();
                let centre = d.y - d.grab + ROW_H / 2.0;
                let (lo, hi) = self.plan.slot_range();
                let slot = drop_slot(&boxes, centre).clamp(lo.min(n - 1), hi.max(lo).min(n - 1));
                order = others;
                order.insert(slot.min(order.len()), from);
                carried = Some((from, slot, d));
            }
        }
        let units = self.units;
        let mut dropped = None;
        for (k, &index) in order.iter().enumerate() {
            let item = self.plan.items[index].clone();
            let is_carried = carried.is_some_and(|c| c.0 == index);
            let mut top = y + k as f32 * pitch;
            if is_carried {
                if let Some((_, _, d)) = carried {
                    top = (d.y - d.grab).clamp(y - 4.0, y + (n as f32 - 1.0) * pitch + 4.0);
                }
            }
            let r = Rect::from_min_size(Pos2::new(x, top), Vec2::new(w, ROW_H));
            let selected = self.plan.selected == Some(item.id);
            let current = self.tracker.current_index() == Some(index);
            let done = index < self.tracker.done_before(n);
            let id = ui.scope_id().with(("mrow", item.id));
            let resp = ui.interact(r, id, egui::Sense::click());
            let hover = resp.hovered() && self.side.row_drag.is_none();
            if is_carried {
                painter.add(style::shadow_pop().as_shape(r, style::RADIUS_BTN));
                card::paint(
                    &painter,
                    r,
                    Look::custom(
                        style::surface_2(),
                        mix(style::nav(), style::border(), 0.5),
                        style::RADIUS_BTN,
                    ),
                );
            } else if selected {
                card::paint(
                    &painter,
                    r,
                    Look::custom(
                        style::surface_2(),
                        mix(style::nav(), style::border(), 0.5),
                        style::RADIUS_BTN,
                    ),
                );
            } else if hover {
                listrow::paint(&painter, r, style::RADIUS_BTN, Mark::Hover);
            }
            if current {
                painter.rect_filled(
                    Rect::from_min_size(r.min, Vec2::new(3.0, r.height())),
                    0,
                    style::healthy(),
                );
            }
            let held = self.plan.pinned(index as isize);
            let spec_pos = item.kind.position();
            let number = self.plan.position_number(index);
            let ig = style::galley_mono(
                ui,
                &if spec_pos {
                    number.to_string()
                } else {
                    "•".to_string()
                },
                10.5,
                style::text_3(),
            );
            let ix = r.left() + 6.0 + 1.0;
            painter.galley(
                Pos2::new(ix + 18.0 - ig.size().x, r.center().y - ig.size().y / 2.0),
                ig,
                style::text_3(),
            );
            let grab = Rect::from_min_size(Pos2::new(ix - 2.0, r.top()), Vec2::new(24.0, r.height()));
            if !held {
                let dh = ui.interact(grab, id.with("grab"), egui::Sense::drag());
                if dh.hovered() || dh.dragged() {
                    ui.ctx().set_cursor_icon(if dh.dragged() {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    });
                }
                if dh.drag_started() {
                    if let Some(p) = dh.interact_pointer_pos() {
                        self.side.row_drag = Some(RowDrag {
                            id: item.id,
                            grab: p.y - r.top(),
                            y: p.y,
                        });
                    }
                    *act = Some(ListAct::Select(item.id));
                }
                if dh.dragged() {
                    if let (Some(p), Some(d)) = (dh.interact_pointer_pos(), self.side.row_drag.as_mut()) {
                        d.y = p.y;
                    }
                }
                if dh.drag_stopped() {
                    if let Some((_, slot, _)) = carried {
                        dropped = Some((item.id, slot));
                    }
                    self.side.row_drag = None;
                }
                dh.on_hover_text("Drag to reorder");
            }
            let tint = |c: Color32| if done { style::text_3() } else { c };
            let kind_colour = match item.kind {
                Kind::Takeoff => style::healthy(),
                Kind::LoiterTurns | Kind::LoiterTime => style::warning(),
                Kind::Land | Kind::Rtl => style::accent(),
                Kind::Waypoint => style::text_2(),
            };
            icons::paint(
                ui,
                item.kind.icon(),
                Rect::from_center_size(Pos2::new(ix + 18.0 + 7.0 + 8.0, r.center().y), Vec2::splat(13.0)),
                kind_colour,
            );
            let show_tools = (hover || selected) && !is_carried && self.side.row_drag.is_none();
            let tools_w = if show_tools { 3.0 * 20.0 + 2.0 } else { 0.0 };
            let text_left = ix + 18.0 + 7.0 + 16.0 + 7.0;
            let text_right = r.right() - 6.0 - 62.0 - 7.0;
            let lg = style::galley(ui, item.display_name(), 12.0, Weight::W400, tint(style::text_1()), 0.0);
            let value = describe_item(&self.plan, &item, &units);
            let vg = style::galley_mono(ui, &value, 10.5, tint(style::text_3()));
            let label_w = lg.size().x.min((text_right - text_left - 4.0 * 10.5 * 1.2).max(30.0));
            let clip = Rect::from_min_size(Pos2::new(text_left, r.top()), Vec2::new(label_w, r.height()));
            ui.painter_at(clip).galley(
                Pos2::new(text_left, r.center().y - lg.size().y / 2.0),
                lg,
                tint(style::text_1()),
            );
            let vclip = Rect::from_min_max(
                Pos2::new(text_left + label_w + 7.0, r.top()),
                Pos2::new(text_right, r.bottom()),
            );
            let vw = vg.size().x.min(vclip.width().max(0.0));
            ui.painter_at(vclip).galley(
                Pos2::new((vclip.right() - vw).max(vclip.left()), r.center().y - vg.size().y / 2.0),
                vg,
                tint(style::text_3()),
            );
            if show_tools {
                let _ = tools_w;
                let bx = r.right() - 6.0 - 62.0;
                let specs: [(&'static str, &str, bool); 3] = [
                    ("chevron-up", "Move up", held || self.plan.pinned(index as isize - 1)),
                    (
                        "chevron-down",
                        "Move down",
                        held || self.plan.pinned(index as isize + 1),
                    ),
                    ("x", "Remove", false),
                ];
                for (j, (icon, title, off)) in specs.into_iter().enumerate() {
                    let br =
                        Rect::from_min_size(Pos2::new(bx + j as f32 * 21.0, r.center().y - 10.0), Vec2::splat(20.0));
                    if off {
                        icons::paint(
                            ui,
                            icon,
                            Rect::from_center_size(br.center(), Vec2::splat(12.0)),
                            style::text_3().gamma_multiply(0.4),
                        );
                    } else if kit::icon_btn(ui, br, icon, title, false, ("mrowtool", item.id, j)) {
                        *act = Some(match j {
                            0 => ListAct::Move(item.id, -1),
                            1 => ListAct::Move(item.id, 1),
                            _ => ListAct::Remove(item.id),
                        });
                    }
                }
            }
            if resp.clicked() && act.is_none() {
                *act = Some(ListAct::Select(item.id));
            }
            if held && hover {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
            }
        }
        if let Some((id, slot)) = dropped {
            *act = Some(ListAct::Drop(id, slot));
        }
        y + n as f32 * pitch - ROW_GAP
    }

    /// The selected item's editor.
    fn detail(&mut self, ui: &mut egui::Ui, rect: Rect, item: Option<&Item>, rows: &[Vec<Fld>], content_h: f32) {
        let painter = ui.painter().clone();
        let Some(item) = item else {
            let g = style::galley(
                ui,
                "Select an item to edit it.",
                12.0,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            painter.galley(rect.min + Vec2::new(8.0, 0.0), g, style::text_3());
            return;
        };
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect.expand2(Vec2::new(4.0, 4.0)));
        let id = item.id;
        let hint = self.plan.item_hint(item);
        let units = self.units;
        let mut commits: Vec<(Fid, String)> = Vec::new();
        let mut remove = false;
        let mut set_dir: Option<f64> = None;
        let mut name_change: Option<String> = None;
        egui::ScrollArea::vertical()
            .id_salt("missiondetail")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let top = ui.max_rect().top();
                let x = rect.left();
                let w = rect.width();
                let painter = ui.painter().clone();
                // head: icon, title, hint, name, trash
                let head_h = 28.0;
                let cy = top + head_h / 2.0;
                icons::paint(
                    ui,
                    item.kind.icon(),
                    Rect::from_center_size(Pos2::new(x + 7.0, cy), Vec2::splat(14.0)),
                    style::text_1(),
                );
                let tg = style::galley(ui, item.kind.label(), 12.0, Weight::W700, style::text_1(), 0.0);
                painter.galley(
                    Pos2::new(x + 14.0 + 7.0, cy - tg.size().y / 2.0),
                    tg.clone(),
                    style::text_1(),
                );
                let mut nx = x + 14.0 + 7.0 + tg.size().x + 7.0;
                if !hint.is_empty() {
                    popover::info(
                        ui,
                        Pos2::new(nx + 8.0, cy),
                        item.kind.label(),
                        hint,
                        ("mdetailhint", id),
                    );
                    nx += 16.0 + 7.0;
                }
                nx += 3.0;
                let trash = Rect::from_min_size(Pos2::new(x + w - 28.0, top), Vec2::splat(28.0));
                let name_r = Rect::from_min_max(
                    Pos2::new(nx, cy - FIELD_H / 2.0),
                    Pos2::new(trash.left() - 7.0, cy + FIELD_H / 2.0),
                );
                if self.side.name_for != Some(id) && !ui.memory(|m| m.focused().is_some()) {
                    self.side.name_for = Some(id);
                    self.side.name_buf = item.name.clone();
                }
                let before = self.side.name_buf.clone();
                let mut buf = std::mem::take(&mut self.side.name_buf);
                crate::ui::field::TextField::new(&mut buf)
                    .hint("Name")
                    .pad(8.0, 8.0)
                    .show(ui, name_r, ("mname", id));
                if buf.chars().count() > POINT_NAME_MAX {
                    buf = buf.chars().take(POINT_NAME_MAX).collect();
                }
                if buf != before {
                    name_change = Some(clean_point_name(&buf));
                }
                self.side.name_buf = buf;
                if kit::icon_btn(
                    ui,
                    trash,
                    "trash-2",
                    &format!("Delete this {}", item.kind.label().to_lowercase()),
                    false,
                    ("mdelete", id),
                ) {
                    remove = true;
                }
                let mut y = top + head_h + 8.0;
                for (r, row) in rows.iter().enumerate() {
                    let cw = if row.len() == 1 { w } else { (w - 8.0) / 2.0 };
                    for (c, f) in row.iter().enumerate() {
                        let fx = x + c as f32 * (cw + 8.0);
                        let upper = f.label.to_uppercase();
                        let mut lg = style::galley(ui, &upper, 10.0, Weight::W600, style::text_3(), 0.7);
                        if lg.size().x > cw {
                            let size = (10.0 * cw / lg.size().x * 0.95).max(8.0);
                            lg = style::galley(ui, &upper, size, Weight::W600, style::text_3(), 0.7);
                        }
                        let clip = Rect::from_min_size(Pos2::new(fx, y), Vec2::new(cw, LABEL_H));
                        ui.painter_at(clip).galley(Pos2::new(fx, y), lg, style::text_3());
                        let input = Rect::from_min_size(Pos2::new(fx, y + LABEL_H + 3.0), Vec2::new(cw, FIELD_H));
                        if let Fid::Param(Param::Direction) = f.id {
                            let choices = [Choice::new("1", "Clockwise"), Choice::new("-1", "Counter-clockwise")];
                            let cur = if item.direction < 0.0 { "-1" } else { "1" };
                            if let Some(v) = kit::select(ui, input, &choices, cur, "", false, "", ("mdir", id)) {
                                set_dir = v.parse().ok();
                            }
                            continue;
                        }
                        let (shown, placeholder) = self.field_text(item, f.id);
                        if let Some(t) = field::committed(ui, input, ("mf", id, r, c), &shown, &placeholder, true) {
                            commits.push((f.id, t));
                        }
                    }
                    y += LABEL_H + 3.0 + FIELD_H + 8.0;
                }
                ui.allocate_space(Vec2::new(w, content_h));
            });
        let _ = (&painter, &units);
        if let Some(n) = name_change {
            if let Some(it) = self.plan.item_mut(id) {
                it.name = n;
            }
        }
        if let Some(d) = set_dir {
            if let Some(it) = self.plan.item_mut(id) {
                it.direction = d;
                it.snap_direction();
            }
            self.refresh();
        }
        for (fid, text) in commits {
            self.commit_field(id, fid, &text);
        }
        if remove {
            self.plan.remove_item(id);
            self.refresh();
        }
    }
}

enum ListAct {
    Read,
    Clear,
    Select(u32),
    Remove(u32),
    Move(u32, isize),
    Drop(u32, usize),
}

/// `color-mix(in srgb, a share%, b)`.
fn mix(a: Color32, b: Color32, share: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) * share + f32::from(y) * (1.0 - share)).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<(f32, f32)> {
        (0..n).map(|i| (i as f32 * 26.0, 24.0)).collect()
    }

    #[test]
    fn a_row_dropped_over_a_top_half_goes_above_that_row() {
        let others = rows(5);
        // the middle of the third row (top 52, height 24) is 64
        assert_eq!(drop_slot(&others, 63.0), 2);
        assert_eq!(drop_slot(&others, 65.0), 3);
    }

    #[test]
    fn a_row_dragged_past_either_end_stops_at_the_end() {
        let others = rows(5);
        assert_eq!(drop_slot(&others, -400.0), 0, "above the list");
        assert_eq!(drop_slot(&others, 4000.0), others.len(), "below the last row");
    }

    #[test]
    fn mixing_follows_color_mix() {
        let c = mix(Color32::from_rgb(200, 100, 0), Color32::from_rgb(0, 0, 100), 0.5);
        assert_eq!((c.r(), c.g(), c.b()), (100, 50, 50));
    }
}
