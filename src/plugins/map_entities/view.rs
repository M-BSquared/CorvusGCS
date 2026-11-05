//! The plugin's view (`.ent-*` of `map-entities.css`): the summary, the
//! palette, the list and the editor of the selected entity.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::model::{
    bearing_deg, distance_m, format_degrees, kind_of, log_text, metres, parse_coordinate_pair, parse_degrees,
    plain_name, points_of, row_detail, status_color, Entity, COLORS, KINDS, NAME_MAX, NOTE_MAX,
};
use super::{Aim, MapEntities};
use crate::plugins::shapes::{hex, paint_glyph};
use crate::plugins::Host;
use crate::ui::field;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::style::{self, Weight};
use crate::ui::{formfield, icons};

const GAP: f32 = 10.0;

pub fn show(p: &mut MapEntities, ui: &mut egui::Ui, area: Rect, h: &mut Host) -> f32 {
    let (x, w) = (area.left(), area.width());
    let mut y = area.top();
    y = summary(p, ui, x, y, w) + GAP;
    y = head(ui, x, y, w, "Place", |_, _| false).0 + GAP;
    y = palette(p, ui, x, y, w, h) + GAP;
    if let Some(pl) = p.placing.clone() {
        y = hint(p, ui, x, y, w, &pl, h) + GAP;
    }
    let (bottom, tools) = head(ui, x, y, w, "On the map", |ui, right| tools(ui, right, y));
    y = bottom + GAP;
    match tools {
        Some(0) => p.reset_results(h),
        Some(1) => {
            if !p.entities.is_empty() {
                h.fit(p.all_points());
            }
        }
        Some(2) => p.remove_all(h),
        _ => {}
    }
    if p.undo.is_some() {
        y = undo_bar(p, ui, x, y, w, h) + GAP;
    }
    if p.entities.is_empty() {
        let g = kit::wrapped_weight(
            ui,
            "Nothing placed yet. Pick a gate or a tree above, then click the map.",
            11.5,
            Weight::W400,
            style::text_3(),
            w - 4.0,
            1.5,
        );
        ui.painter()
            .galley(Pos2::new(x + 2.0, y + 4.0), g.clone(), style::text_3());
        y += g.size().y + 8.0 + GAP;
    } else {
        y = list(p, ui, x, y, w, h) + GAP;
    }
    if let Some(e) = p.selected.as_ref().and_then(|id| p.find(id)).cloned() {
        y = detail(p, ui, x, y, w, e, h);
    } else {
        y -= GAP;
    }
    y - area.top()
}

/// A color the entity's saved `#RRGGBB` stands for.
fn color_of(s: &str) -> Color32 {
    hex(s).unwrap_or(Color32::WHITE)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// `.ent-badge`: a 26 px disc tinted with the colour, its rim and glyph pulled
/// towards the text colour so yellow and white read on either theme.
fn badge(ui: &egui::Ui, at: Pos2, icon: &str, color: Color32, glyph: f32) {
    let r = Rect::from_min_size(at, Vec2::splat(26.0));
    let p = ui.painter();
    p.circle_filled(r.center(), 13.0, mix(style::surface_1(), color, 0.16));
    p.circle_stroke(r.center(), 12.25, Stroke::new(1.5, mix(color, style::text_1(), 0.42)));
    paint_glyph(
        ui,
        icon,
        Rect::from_center_size(r.center(), Vec2::splat(glyph)),
        mix(color, style::text_1(), 0.52),
        2.0,
    );
}

fn summary(p: &MapEntities, ui: &egui::Ui, x: f32, y: f32, w: f32) -> f32 {
    let gates: Vec<&Entity> = p.entities.iter().filter(|e| e.is_gate()).collect();
    let passed = gates
        .iter()
        .filter(|g| p.results.get(&g.id).is_some_and(|r| r.passes > 0))
        .count();
    let hits = p
        .entities
        .iter()
        .filter(|e| e.is_tree() && p.results.get(&e.id).is_some_and(|r| r.hits > 0))
        .count();
    let inside = p
        .entities
        .iter()
        .filter(|e| !e.is_gate() && p.results.get(&e.id).is_some_and(|r| r.inside))
        .count();
    let cells = [
        ("Entities", p.entities.len().to_string(), ""),
        ("Gates", format!("{passed} / {}", gates.len()), ""),
        ("Trees hit", hits.to_string(), if hits > 0 { "bad" } else { "" }),
        ("Inside area", inside.to_string(), if inside > 0 { "warn" } else { "" }),
    ];
    let cw = (w - 5.0) / 2.0;
    let mut bottom = y;
    for (i, (label, value, level)) in cells.iter().enumerate() {
        let (cx, cy) = (x + (i % 2) as f32 * (cw + 5.0), y + (i / 2) as f32 * (26.0 + 5.0));
        let r = Rect::from_min_size(Pos2::new(cx, cy), Vec2::new(cw, 26.0));
        let (line, ink) = match *level {
            "warn" => (mix(style::border_soft(), style::warning(), 0.55), style::warning()),
            "bad" => (mix(style::border_soft(), style::critical(), 0.55), style::critical()),
            _ => (style::border_soft(), style::text_1()),
        };
        ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
        ui.painter()
            .rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, line), StrokeKind::Inside);
        let lg = style::galley(ui, &label.to_uppercase(), 9.5, Weight::W400, style::text_2(), 0.7);
        let vg = style::galley_mono(ui, value, 13.0, ink);
        ui.painter().galley(
            Pos2::new(r.left() + 10.0, r.center().y - lg.size().y / 2.0),
            lg.clone(),
            style::text_2(),
        );
        let room = r.width() - 20.0 - lg.size().x - 8.0;
        let clip = Rect::from_min_size(Pos2::new(r.right() - 10.0 - room, r.top()), Vec2::new(room, r.height()));
        let vx = (r.right() - 10.0 - vg.size().x).max(clip.left());
        ui.painter_at(clip)
            .galley(Pos2::new(vx, r.center().y - vg.size().y / 2.0), vg, ink);
        bottom = bottom.max(r.bottom());
    }
    bottom
}

/// `.ent-list-head`: a small caps title over a hairline, tools at the right.
fn head<T>(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    tools: impl FnOnce(&mut egui::Ui, f32) -> T,
) -> (f32, T) {
    let g = style::galley(ui, &title.to_uppercase(), 10.0, Weight::W400, style::text_3(), 0.9);
    let row_h = g.size().y.max(24.0);
    ui.painter()
        .galley(Pos2::new(x, y + (row_h - g.size().y) / 2.0), g, style::text_3());
    let out = tools(ui, x + w);
    let line = y + row_h + 3.0;
    ui.painter().hline(
        egui::Rangef::new(x, x + w),
        line + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    (line + 1.0, out)
}

fn tools(ui: &mut egui::Ui, right: f32, y: f32) -> Option<usize> {
    let mut hit = None;
    for (i, (icon, title)) in [
        ("rotate-ccw", "Reset results"),
        ("maximize", "Show all on the map"),
        ("trash-2", "Delete all"),
    ]
    .into_iter()
    .enumerate()
    .rev()
    {
        let r = Rect::from_min_size(Pos2::new(right - (3 - i) as f32 * 25.0 + 1.0, y), Vec2::splat(24.0));
        if small_tool(ui, r, icon, title, i == 2, ("entools", i)) {
            hit = Some(i);
        }
    }
    hit
}

/// A square icon button; `remove` turns critical on hover (`.ent-remove`).
fn small_tool(
    ui: &mut egui::Ui,
    r: Rect,
    icon: &'static str,
    title: &str,
    remove: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let resp = ui
        .interact(r, ui.scope_id().with(id), egui::Sense::click())
        .on_hover_text(title);
    let hot = resp.hovered();
    let (fill, ink) = if hot && remove {
        (style::critical().gamma_multiply(0.16), style::critical())
    } else if hot {
        (style::surface_hover(), style::text_1())
    } else {
        (Color32::TRANSPARENT, style::text_3())
    };
    ui.painter().rect_filled(r, style::RADIUS_BTN, fill);
    icons::paint(
        ui,
        icon,
        Rect::from_center_size(r.center(), Vec2::splat(if r.width() < 24.0 { 13.0 } else { 14.0 })),
        ink,
    );
    resp.clicked()
}

/// The grid's column count for the container's width (`@container` rules).
pub fn palette_columns(w: f32) -> usize {
    if w >= 520.0 {
        6
    } else if w <= 210.0 {
        2
    } else if w <= 330.0 {
        3
    } else {
        4
    }
}

fn palette(p: &mut MapEntities, ui: &mut egui::Ui, x: f32, y: f32, w: f32, h: &mut Host) -> f32 {
    let mut pressed: Option<&'static str> = None;
    let active = p.placing.as_ref().map(|pl| pl.kind);
    let main_w = (w - 5.0) / 2.0;
    for (i, k) in KINDS.iter().take(2).enumerate() {
        let r = Rect::from_min_size(Pos2::new(x + i as f32 * (main_w + 5.0), y), Vec2::new(main_w, 48.0));
        if kind_button(ui, r, k.id, k.label, k.color, true, active == Some(k.id)) {
            pressed = Some(k.id);
        }
    }
    let rest_y = y + 48.0 + 5.0;
    let cols = palette_columns(w);
    let cw = (w - 5.0 * (cols as f32 - 1.0)) / cols as f32;
    let others = &KINDS[2..];
    for (i, k) in others.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(
                x + (i % cols) as f32 * (cw + 5.0),
                rest_y + (i / cols) as f32 * (59.0 + 5.0),
            ),
            Vec2::new(cw, 59.0),
        );
        if kind_button(ui, r, k.id, k.label, k.color, false, active == Some(k.id)) {
            pressed = Some(k.id);
        }
    }
    if let Some(id) = pressed {
        if active == Some(id) {
            p.stop_placing(h);
        } else {
            p.start_placing(id, h);
        }
    }
    let rows = others.len().div_ceil(cols);
    rest_y + rows as f32 * 59.0 + (rows as f32 - 1.0) * 5.0
}

fn kind_button(
    ui: &mut egui::Ui,
    r: Rect,
    id: &'static str,
    label: &str,
    color: &str,
    main: bool,
    active: bool,
) -> bool {
    let resp = ui
        .interact(r, ui.scope_id().with(("enkind", id)), egui::Sense::click())
        .on_hover_text(format!("Place: {label}"));
    let (fill, line) = if active {
        (
            mix(style::surface_2(), style::nav(), 0.14),
            mix(style::border(), style::nav(), 0.7),
        )
    } else if resp.hovered() {
        (style::surface_hover(), style::border_soft())
    } else {
        (style::surface_2(), style::border_soft())
    };
    ui.painter().rect_filled(r, style::RADIUS_BTN, fill);
    ui.painter()
        .rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, line), StrokeKind::Inside);
    let ink = if main || active || resp.hovered() {
        style::text_1()
    } else {
        style::text_2()
    };
    let c = color_of(color);
    if main {
        let g = style::galley(ui, label, 12.0, Weight::W600, ink, 0.0);
        let total = 26.0 + 8.0 + g.size().x;
        let x0 = r.center().x - total / 2.0;
        badge(ui, Pos2::new(x0, r.center().y - 13.0), id, c, 18.0);
        ui.painter()
            .galley(Pos2::new(x0 + 34.0, r.center().y - g.size().y / 2.0), g, ink);
    } else {
        badge(ui, Pos2::new(r.center().x - 13.0, r.top() + 8.0), id, c, 15.0);
        let g = kit::wrapped_weight(ui, label, 11.0, Weight::W400, ink, r.width() - 8.0, 1.2);
        let clip = ui.painter_at(r.shrink2(Vec2::new(4.0, 0.0)));
        let one = style::galley(ui, label, 11.0, Weight::W400, ink, 0.0);
        let _ = g;
        let tx = (r.center().x - one.size().x / 2.0).max(r.left() + 4.0);
        clip.galley(Pos2::new(tx, r.top() + 8.0 + 26.0 + 4.0), one, ink);
    }
    resp.clicked()
}

fn hint(p: &mut MapEntities, ui: &mut egui::Ui, x: f32, y: f32, w: f32, pl: &super::Placing, h: &mut Host) -> f32 {
    let kind = kind_of(pl.kind).expect("placing a kind");
    let text = if kind.gate {
        if pl.step == 0 {
            "Click the map for post A of the gate.".to_string()
        } else {
            "Click the map for post B of the gate.".to_string()
        }
    } else {
        format!("Click the map to place: {}.", kind.label)
    };
    let done = Button::new("Done", Variant::Secondary).small();
    let dw = done.width(ui);
    let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_1(), w - 20.0 - dw - 8.0, 1.2);
    let sub = style::galley(
        ui,
        "Esc or right click also stops.",
        10.5,
        Weight::W400,
        style::text_3(),
        0.0,
    );
    let row = g.size().y.max(done.height());
    let r = Rect::from_min_size(
        Pos2::new(x, y),
        Vec2::new(w, 7.0 + row + 8.0 + sub.size().y + 7.0 + 2.0),
    );
    ui.painter()
        .rect_filled(r, style::RADIUS_BTN, mix(style::bg(), style::nav(), 0.10));
    ui.painter().rect_stroke(
        r,
        style::RADIUS_BTN,
        Stroke::new(1.0, mix(style::border(), style::nav(), 0.55)),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        Pos2::new(r.left() + 10.0, r.top() + 8.0 + (row - g.size().y) / 2.0),
        g,
        style::text_1(),
    );
    if done.show(
        ui,
        Rect::from_min_size(Pos2::new(r.right() - 10.0 - dw, r.top() + 8.0), Vec2::new(dw, 28.0)),
        "enhintdone",
    ) {
        p.stop_placing(h);
    }
    ui.painter().galley(
        Pos2::new(r.left() + 10.0, r.top() + 8.0 + row + 8.0),
        sub,
        style::text_3(),
    );
    r.bottom()
}

fn undo_bar(p: &mut MapEntities, ui: &mut egui::Ui, x: f32, y: f32, w: f32, h: &mut Host) -> f32 {
    let Some(u) = &p.undo else { return y };
    let many = u.items.len() > 1;
    let name = u.items.first().map(|i| i.0.name.clone()).unwrap_or_default();
    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 38.0));
    ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
    ui.painter().rect_stroke(
        r,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::border()),
        StrokeKind::Inside,
    );
    let t = style::galley(
        ui,
        if many { "Deleted all" } else { "Deleted" },
        12.0,
        Weight::W400,
        style::text_2(),
        0.0,
    );
    ui.painter().galley(
        Pos2::new(r.left() + 10.0, r.center().y - t.size().y / 2.0),
        t.clone(),
        style::text_2(),
    );
    let undo = Button::new("Undo", Variant::Secondary).small();
    let uw = undo.width(ui);
    if !many {
        let n = style::galley(ui, &name, 12.0, Weight::W600, style::text_1(), 0.0);
        let room = r.width() - 20.0 - t.size().x - 8.0 - uw - 8.0;
        let nr = Rect::from_min_size(
            Pos2::new(r.left() + 10.0 + t.size().x + 8.0, r.top()),
            Vec2::new(room.max(0.0), r.height()),
        );
        ui.painter_at(nr).galley(
            Pos2::new(nr.left(), r.center().y - n.size().y / 2.0),
            n,
            style::text_1(),
        );
    }
    if undo.show(
        ui,
        Rect::from_min_size(
            Pos2::new(r.right() - 10.0 - uw, r.center().y - 14.0),
            Vec2::new(uw, 28.0),
        ),
        "enundo",
    ) {
        p.undo_delete(h);
    }
    r.bottom()
}

fn list(p: &mut MapEntities, ui: &mut egui::Ui, x: f32, y: f32, w: f32, h: &mut Host) -> f32 {
    const ROW: f32 = 38.0;
    let total = p.entities.len() as f32 * ROW + (p.entities.len() as f32 - 1.0) * 2.0;
    let shown = total.min(280.0);
    let box_ = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, shown));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(box_));
    child.set_clip_rect(box_);
    let mut select: Option<String> = None;
    let mut locate: Option<usize> = None;
    let mut delete: Option<String> = None;
    egui::ScrollArea::vertical()
        .id_salt("entlist")
        .auto_shrink([false, false])
        .max_height(280.0)
        .show(&mut child, |ui| {
            let top = ui.max_rect().top();
            let rw = w - if total > 280.0 { 10.0 } else { 0.0 };
            for (i, e) in p.entities.iter().enumerate() {
                let r = Rect::from_min_size(Pos2::new(x, top + i as f32 * (ROW + 2.0)), Vec2::new(rw, ROW));
                let on = p.selected.as_deref() == Some(e.id.as_str());
                let resp = ui.interact(r, ui.scope_id().with(("enrow", &e.id)), egui::Sense::click());
                if on {
                    ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
                    ui.painter().rect_stroke(
                        r,
                        style::RADIUS_BTN,
                        Stroke::new(1.0, mix(style::border(), style::nav(), 0.5)),
                        StrokeKind::Inside,
                    );
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_hover());
                }
                let color = color_of(status_color(e, p.results.get(&e.id)));
                badge(
                    ui,
                    Pos2::new(r.left() + 6.0, r.center().y - 13.0),
                    e.kind.id,
                    color,
                    14.0,
                );
                let (value, level) = row_detail(e, p.results.get(&e.id));
                let vink = match level {
                    "good" => style::healthy(),
                    "warn" => style::warning(),
                    "bad" => style::critical(),
                    _ => style::text_2(),
                };
                let vg = style::galley_mono(ui, &value, 11.0, vink);
                let strong = on || resp.hovered();
                let tools_w = 2.0 * 24.0 + 2.0;
                let vx = r.right() - 6.0 - tools_w - 7.0 - vg.size().x;
                let ng = style::galley(ui, &e.name, 12.5, Weight::W500, style::text_1(), 0.0);
                let name_room = (vx - 7.0 - (r.left() + 6.0 + 26.0 + 7.0)).max(0.0);
                ui.painter_at(Rect::from_min_size(
                    Pos2::new(r.left() + 39.0, r.top()),
                    Vec2::new(name_room, r.height()),
                ))
                .galley(
                    Pos2::new(r.left() + 39.0, r.center().y - ng.size().y / 2.0),
                    ng,
                    style::text_1(),
                );
                ui.painter()
                    .galley(Pos2::new(vx, r.center().y - vg.size().y / 2.0), vg, vink);
                ui.set_opacity(if strong { 1.0 } else { 0.7 });
                let t0 = r.right() - 6.0 - tools_w;
                if small_tool(
                    ui,
                    Rect::from_min_size(Pos2::new(t0, r.center().y - 12.0), Vec2::splat(24.0)),
                    "locate-fixed",
                    "Show on the map",
                    false,
                    ("enloc", &e.id),
                ) {
                    locate = Some(i);
                }
                if small_tool(
                    ui,
                    Rect::from_min_size(Pos2::new(t0 + 26.0, r.center().y - 12.0), Vec2::splat(24.0)),
                    "trash-2",
                    "Delete",
                    true,
                    ("endel", &e.id),
                ) {
                    delete = Some(e.id.clone());
                }
                ui.set_opacity(1.0);
                if resp.clicked() && locate.is_none() && delete.is_none() {
                    select = Some(e.id.clone());
                }
            }
            ui.allocate_space(Vec2::new(w, total));
        });
    if let Some(i) = locate {
        h.fit(points_of(&p.entities[i]));
    }
    if let Some(id) = delete {
        p.remove(&id, h);
    } else if let Some(id) = select {
        p.select(Some(id), h);
    }
    y + shown
}

/// A caption over a control; returns the control's rect.
fn caption(ui: &mut egui::Ui, x: f32, y: f32, w: f32, text: &str, h: f32) -> Rect {
    let c = formfield::caption(ui, x, y, text, "", "");
    Rect::from_min_size(Pos2::new(x, y + c + 3.0), Vec2::new(w, h))
}

const CONTROL: f32 = 28.0;

fn text_box(
    ui: &mut egui::Ui,
    r: Rect,
    shown: &str,
    mono: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> Option<String> {
    field::committed(ui, r, id, shown, "", mono)
}

fn detail(p: &mut MapEntities, ui: &mut egui::Ui, x: f32, y: f32, w: f32, e: Entity, h: &mut Host) -> f32 {
    let id = e.id.clone();
    let mut y = y;
    ui.painter().hline(
        egui::Rangef::new(x, x + w),
        y + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    y += 1.0 + 10.0;
    let key = |s: &str| (id.clone(), s.to_string());
    let col_w = (w - 8.0) / 2.0;
    // head: badge, name, locate, delete
    badge(ui, Pos2::new(x, y), e.kind.id, color_of(&e.color), 16.0);
    let name_r = Rect::from_min_size(Pos2::new(x + 33.0, y - 1.0), Vec2::new(w - 33.0 - 2.0 * 35.0, CONTROL));
    if let Some(t) = text_box(ui, name_r, &e.name, false, key("name")) {
        let kind = e.kind;
        let n = plain_name(&t, NAME_MAX);
        p.update(&id, h, |e| e.name = if n.is_empty() { kind.label.into() } else { n });
    }
    if small_btn(
        ui,
        Rect::from_min_size(Pos2::new(x + w - 70.0 + 7.0, y - 1.0), Vec2::splat(28.0)),
        "locate-fixed",
        "Show on the map",
        false,
        key("loc"),
    ) {
        h.fit(points_of(&e));
    }
    if small_btn(
        ui,
        Rect::from_min_size(Pos2::new(x + w - 35.0 + 7.0, y - 1.0), Vec2::splat(28.0)),
        "trash-2",
        "Delete",
        true,
        key("del"),
    ) {
        p.remove(&id, h);
        return y + 40.0;
    }
    y += 30.0 + 8.0;
    let mut g = Grid {
        x,
        w,
        col_w,
        y,
        col: 0,
        row_bottom: y,
    };
    if e.is_gate() {
        point_row(p, ui, &mut g, &e, 'a', "Post A", h);
        point_row(p, ui, &mut g, &e, 'b', "Post B", h);
        number(
            p,
            ui,
            &mut g,
            &e,
            "Opening from (m above takeoff)",
            e.base,
            -1000.0,
            10000.0,
            |e, v| e.base = v,
            h,
        );
        number(
            p,
            ui,
            &mut g,
            &e,
            "Opening height (m)",
            e.height,
            0.0,
            10000.0,
            |e, v| e.height = v,
            h,
        );
        let (a, b) = ([e.lng, e.lat], [e.lng2, e.lat2]);
        let facts = [
            ("Width", metres(distance_m(a, b))),
            ("Heading", format!("{}\u{b0}", bearing_deg(a, b).round() as i64)),
        ];
        let r = g.place(true, 26.0);
        let cw = (r.width() - 5.0) / 2.0;
        for (i, (l, v)) in facts.iter().enumerate() {
            let c = Rect::from_min_size(
                Pos2::new(r.left() + i as f32 * (cw + 5.0), r.top()),
                Vec2::new(cw, 26.0),
            );
            ui.painter().rect_filled(c, style::RADIUS_BTN, style::surface_2());
            ui.painter().rect_stroke(
                c,
                style::RADIUS_BTN,
                Stroke::new(1.0, style::border_soft()),
                StrokeKind::Inside,
            );
            let lg = style::galley(ui, &l.to_uppercase(), 9.5, Weight::W400, style::text_2(), 0.7);
            let vg = style::galley_mono(ui, v, 13.0, style::text_1());
            ui.painter().galley(
                Pos2::new(c.left() + 10.0, c.center().y - lg.size().y / 2.0),
                lg,
                style::text_2(),
            );
            ui.painter().galley(
                Pos2::new(c.right() - 10.0 - vg.size().x, c.center().y - vg.size().y / 2.0),
                vg,
                style::text_1(),
            );
        }
    } else {
        point_row(p, ui, &mut g, &e, 'a', "Position", h);
        let label = if e.is_tree() {
            "Crown radius (m)"
        } else {
            "Radius (m, 0 for none)"
        };
        number(
            p,
            ui,
            &mut g,
            &e,
            label,
            e.radius,
            0.0,
            100_000.0,
            |e, v| e.radius = v,
            h,
        );
        if e.is_tree() {
            number(
                p,
                ui,
                &mut g,
                &e,
                "Height (m)",
                e.height,
                0.0,
                1000.0,
                |e, v| e.height = v,
                h,
            );
        } else {
            number(
                p,
                ui,
                &mut g,
                &e,
                "Angle (degrees, 360 for a full circle)",
                e.angle,
                1.0,
                360.0,
                |e, v| e.angle = v,
                h,
            );
            if e.angle < 360.0 {
                let r = g.place(true, 11.0 + 3.0 + CONTROL);
                let cr = caption(
                    ui,
                    r.left(),
                    r.top(),
                    r.width(),
                    "Direction (degrees, 0 is north)",
                    CONTROL,
                );
                let boxr = Rect::from_min_size(cr.min, Vec2::new(cr.width() - 34.0, CONTROL));
                if let Some(t) = text_box(
                    ui,
                    boxr,
                    &crate::plugins::view::number_text(e.bearing),
                    true,
                    key("bearing"),
                ) {
                    if let Ok(v) = t.trim().parse::<f64>() {
                        p.update(&id, h, |e| e.bearing = v.clamp(0.0, 360.0) % 360.0);
                    }
                }
                if small_btn(
                    ui,
                    Rect::from_min_size(Pos2::new(cr.right() - 28.0, cr.top()), Vec2::splat(28.0)),
                    "crosshair",
                    "Aim on the map",
                    false,
                    key("aim"),
                ) {
                    p.aim_at(Aim::Bearing(id.clone()), h);
                }
            }
        }
        let r = g.place(true, 18.0);
        let mut on = e.alert;
        let text = if e.is_tree() {
            "Warn when the aircraft is in the crown"
        } else {
            "Warn when the aircraft enters"
        };
        if kit::checkbox(ui, r.min, r.width(), text, &mut on, false, key("alert")) {
            p.update(&id, h, |e| e.alert = on);
        }
    }
    // colour and note
    let r = g.place(false, 11.0 + 3.0 + CONTROL);
    let cr = caption(ui, r.left(), r.top(), r.width(), "Colour", CONTROL);
    let mut choices: Vec<Choice> = COLORS.iter().map(|(l, c)| Choice::new(*c, *l)).collect();
    if !COLORS.iter().any(|(_, c)| c.eq_ignore_ascii_case(&e.color)) {
        choices.push(Choice::new(e.color.as_str(), "Custom"));
    }
    if let Some(v) = kit::select(ui, cr, &choices, &e.color, "", false, "", key("color")) {
        p.update(&id, h, |e| e.color = v);
    }
    let r = g.place(true, 11.0 + 3.0 + CONTROL);
    let nr = caption(ui, r.left(), r.top(), r.width(), "Note", CONTROL);
    if let Some(t) = text_box(ui, nr, &e.note, false, key("note")) {
        let n = plain_name(&t, NOTE_MAX);
        p.update(&id, h, |e| e.note = n);
    }
    let mut y = g.finish();
    // results
    if let Some(r) = p.results.get(&id).filter(|r| !r.log.is_empty()) {
        y += 8.0;
        let t = style::galley(ui, "RESULTS", 10.0, Weight::W400, style::text_3(), 0.9);
        ui.painter().galley(Pos2::new(x, y), t.clone(), style::text_3());
        y += t.size().y + 3.0;
        ui.painter().hline(
            egui::Rangef::new(x, x + w),
            y + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        y += 1.0 + 2.0;
        for entry in r.log.iter().rev().take(14) {
            let stamp = clock_text(entry.t);
            let sg = style::galley_mono(ui, &stamp, 11.0, style::text_3());
            let ink = if entry.ok { style::text_2() } else { style::warning() };
            let tg = style::galley(ui, &log_text(&e, entry), 11.0, Weight::W400, ink, 0.0);
            ui.painter().galley(Pos2::new(x, y + 2.0), sg.clone(), style::text_3());
            ui.painter()
                .galley(Pos2::new(x + sg.size().x + 10.0, y + 2.0), tg.clone(), ink);
            y += tg.size().y.max(sg.size().y) + 4.0;
        }
    }
    y
}

fn small_btn(
    ui: &mut egui::Ui,
    r: Rect,
    icon: &'static str,
    title: &str,
    remove: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    small_tool(ui, r, icon, title, remove, id)
}

/// `toLocaleTimeString`, as 24 hour clock text.
pub fn clock_text(secs: f64) -> String {
    let (_, _, _, hh, mm, ss) = crate::data::clock::civil(secs as i64 + crate::panel::console::local_offset());
    format!("{hh:02}:{mm:02}:{ss:02}")
}

/// A two column grid of fields: a wide one takes the row.
struct Grid {
    x: f32,
    w: f32,
    col_w: f32,
    y: f32,
    col: usize,
    row_bottom: f32,
}

impl Grid {
    fn place(&mut self, wide: bool, h: f32) -> Rect {
        if wide && self.col == 1 {
            self.y = self.row_bottom + 8.0;
            self.col = 0;
        }
        let r = if wide {
            Rect::from_min_size(Pos2::new(self.x, self.y), Vec2::new(self.w, h))
        } else {
            Rect::from_min_size(
                Pos2::new(self.x + self.col as f32 * (self.col_w + 8.0), self.y),
                Vec2::new(self.col_w, h),
            )
        };
        self.row_bottom = if self.col == 0 {
            r.bottom()
        } else {
            self.row_bottom.max(r.bottom())
        };
        if wide || self.col == 1 {
            self.y = self.row_bottom + 8.0;
            self.col = 0;
        } else {
            self.col = 1;
        }
        r
    }

    fn finish(&self) -> f32 {
        if self.col == 1 {
            self.row_bottom
        } else {
            self.y - 8.0
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn number(
    p: &mut MapEntities,
    ui: &mut egui::Ui,
    g: &mut Grid,
    e: &Entity,
    label: &str,
    value: f64,
    min: f64,
    max: f64,
    set: impl FnOnce(&mut Entity, f64),
    h: &mut Host,
) {
    let r = g.place(false, 11.0 + 3.0 + CONTROL);
    let cr = caption(ui, r.left(), r.top(), r.width(), label, CONTROL);
    let shown = crate::plugins::view::number_text((value * 100.0).round() / 100.0);
    if let Some(t) = text_box(ui, cr, &shown, true, (e.id.clone(), label.to_string())) {
        if let Ok(v) = t.trim().parse::<f64>() {
            if v.is_finite() {
                p.update(&e.id, h, |e| set(e, v.clamp(min, max)));
            }
        }
    }
}

/// `pointRow`: latitude and longitude as text, and a crosshair to click the map.
fn point_row(p: &mut MapEntities, ui: &mut egui::Ui, g: &mut Grid, e: &Entity, which: char, title: &str, h: &mut Host) {
    let (lng, lat) = if which == 'b' { (e.lng2, e.lat2) } else { (e.lng, e.lat) };
    let r = g.place(true, 22.0 + 4.0 + 11.0 + 3.0 + CONTROL);
    let hy = formfield::caption(ui, r.left(), r.top() + 4.0, title, "", "");
    if small_btn(
        ui,
        Rect::from_min_size(Pos2::new(r.right() - 22.0, r.top()), Vec2::splat(22.0)),
        "crosshair",
        "Place on the map",
        false,
        (e.id.clone(), format!("pt{which}")),
    ) {
        p.aim_at(Aim::Post(e.id.clone(), which), h);
    }
    let _ = hy;
    let top = r.top() + 22.0 + 4.0;
    let cw = (r.width() - 8.0) / 2.0;
    let la = caption(ui, r.left(), top, cw, "Latitude", CONTROL);
    let lo = caption(ui, r.left() + cw + 8.0, top, cw, "Longitude", CONTROL);
    let shown_lat = format_degrees(lat);
    let shown_lng = format_degrees(lng);
    let got_lat = text_box(ui, la, &shown_lat, true, (e.id.clone(), format!("lat{which}")));
    let got_lng = text_box(ui, lo, &shown_lng, true, (e.id.clone(), format!("lng{which}")));
    if got_lat.is_some() || got_lng.is_some() {
        let lat_text = got_lat.unwrap_or(shown_lat);
        let lng_text = got_lng.unwrap_or(shown_lng);
        let (next_lat, next_lng) = match parse_coordinate_pair(&lat_text) {
            Some((a, b)) => (Some(a), Some(b)),
            None => (parse_degrees(&lat_text, 90.0), parse_degrees(&lng_text, 180.0)),
        };
        if let (Some(a), Some(b)) = (next_lat, next_lng) {
            p.update(&e.id, h, |e| {
                if which == 'b' {
                    e.lat2 = a;
                    e.lng2 = b;
                } else {
                    e.lat = a;
                    e.lng = b;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_follows_the_container_width() {
        assert_eq!(palette_columns(600.0), 6);
        assert_eq!(palette_columns(520.0), 6);
        assert_eq!(palette_columns(400.0), 4);
        assert_eq!(palette_columns(330.0), 3);
        assert_eq!(palette_columns(300.0), 3);
        assert_eq!(palette_columns(210.0), 2);
        assert_eq!(palette_columns(150.0), 2);
    }

    #[test]
    fn the_grid_puts_a_wide_field_on_a_row_of_its_own() {
        let mut g = Grid {
            x: 0.0,
            w: 100.0,
            col_w: 46.0,
            y: 0.0,
            col: 0,
            row_bottom: 0.0,
        };
        let a = g.place(false, 10.0);
        let b = g.place(false, 20.0);
        assert_eq!((a.left(), b.left(), b.top()), (0.0, 54.0, 0.0));
        let c = g.place(true, 5.0);
        assert_eq!((c.left(), c.top(), c.width()), (0.0, 28.0, 100.0));
        let d = g.place(false, 10.0);
        let w = g.place(true, 5.0);
        assert_eq!(d.top(), 41.0);
        assert_eq!(w.top(), 59.0, "a wide field after a lone one drops under it");
    }

    #[test]
    fn the_time_of_a_result_is_a_clock() {
        let s = clock_text(3600.0 * 5.0 + 61.0);
        assert_eq!(s.len(), 8);
        assert_eq!(&s[2..3], ":");
    }
}
