//! The Landing card (`landingCard` of src/js/setup-safety.js, the `.slz*`
//! rules of setup-forms.css): the descent drawn as one vertical bar read from
//! the top down, the fields that set each stretch beside it, and a bracket
//! per stretch saying how many metres it spans, worked out from the height
//! fields as they are typed.

use std::collections::HashMap;

use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Shape, Stroke, Vec2};

use crate::http::Server;
use crate::kit::{self, Button, Variant};
use crate::popover;
use crate::safety::Section;
use crate::schema_form::{format_number, Field, FormState, Kind};
use crate::style::{self, Weight};

const DERIVED_SPAN_HINT: &str = "Calculated from the heights, so it cannot be changed here. \
The vehicle stores only the heights. Change them in the fields beside it and this value follows.";

/// `LANDING_ZONES`: the stretches, top to bottom, with the height parameter
/// that ends each and the speed parameters that apply in it.
const ZONES: [(&str, Option<&str>, &[&str]); 4] = [
    ("fast", Some("MPC_LAND_ALT1"), &["MPC_Z_VEL_MAX_DN", "MPC_Z_V_AUTO_DN"]),
    ("brake", Some("MPC_LAND_ALT2"), &[]),
    ("land", None, &["MPC_LAND_SPEED"]),
    ("crawl", Some("MPC_LAND_ALT3"), &["MPC_LAND_CRWL"]),
];

/// The text of each bracket from the heights typed so far.
pub fn spans(v: &HashMap<String, f64>) -> [String; 4] {
    let metres = |n: f64| format!("{} m", format_number((n * 100.0).round() / 100.0));
    let get = |k: &str| v.get(k).copied();
    let mut out: [String; 4] = Default::default();
    if let Some(a1) = get("MPC_LAND_ALT1") {
        out[0] = format!("> {}", metres(a1));
    }
    if let (Some(a1), Some(a2)) = (get("MPC_LAND_ALT1"), get("MPC_LAND_ALT2")) {
        if a1 >= a2 {
            out[1] = metres(a1 - a2);
        }
    }
    if let (Some(a2), Some(a3)) = (get("MPC_LAND_ALT2"), get("MPC_LAND_ALT3")) {
        if a2 >= a3 {
            out[2] = metres(a2 - a3);
        }
    }
    if let Some(a3) = get("MPC_LAND_ALT3") {
        out[3] = format!("< {}", metres(a3));
    }
    out
}

/// A field of the grid: label row over control, `.safety-landing-grid`.
struct Cell<'a> {
    f: &'a Field,
}

const CONTROL_H: f32 = 30.0;

/// How the label row of a cell lays out in `w`: the galley, whether the icon
/// shares its line, and the row's height.
fn label_row(ui: &egui::Ui, f: &Field, w: f32) -> (std::sync::Arc<egui::Galley>, bool, f32) {
    let g = kit::wrapped_weight(ui, &f.label, 11.0, Weight::W400, style::text_2(), w, 1.21);
    if f.hint.is_empty() {
        let h = g.size().y;
        return (g, false, h);
    }
    let inline = g.size().x + 6.0 + 16.0 <= w;
    let h = if inline {
        g.size().y.max(16.0)
    } else {
        g.size().y + 16.0
    };
    (g, inline, h)
}

fn cell_height(ui: &egui::Ui, f: &Field, w: f32) -> f32 {
    label_row(ui, f, w).2 + 3.0 + CONTROL_H
}

fn grid_height(ui: &egui::Ui, fields: &[&Field], w: f32) -> f32 {
    fields.iter().map(|f| cell_height(ui, f, w)).sum::<f32>() + 8.0 * (fields.len() as f32 - 1.0).max(0.0)
}

impl Cell<'_> {
    #[allow(clippy::too_many_arguments)]
    fn show(
        &self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        server: &Server,
        form: &mut FormState,
        x: f32,
        y: f32,
        w: f32,
        armed: bool,
    ) -> f32 {
        let f = self.f;
        let (g, inline, lh) = label_row(ui, f, w);
        ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
        if !f.hint.is_empty() {
            let (ix, iy) = if inline {
                (x + g.size().x + 6.0 + 8.0, y + lh.min(g.size().y.max(16.0)) / 2.0)
            } else {
                (x + 8.0, y + g.size().y + 8.0)
            };
            popover::info(ui, Pos2::new(ix, iy), &f.label, &f.hint, ("landhint", &f.param));
        }
        let top = y + lh + 3.0;
        let input_w = w.min(96.0);
        let status = form.status_of(&f.param);
        let end = if matches!(f.kind, Kind::Number) {
            form.control_with(
                ui,
                ctx,
                server,
                f,
                Pos2::new(x, top),
                input_w,
                CONTROL_H,
                armed,
                false,
                None,
                Some(input_w),
            )
        } else {
            form.control(ui, ctx, server, f, Pos2::new(x, top), w, CONTROL_H, armed, false)
        };
        let mut ux = end;
        if !f.unit.is_empty() && matches!(f.kind, Kind::Number) {
            let ug = style::galley(ui, &f.unit, 11.0, Weight::W400, style::text_3(), 0.0);
            ui.painter().galley(
                Pos2::new(ux + 6.0, top + (CONTROL_H - ug.size().y) / 2.0),
                ug.clone(),
                style::text_3(),
            );
            ux += 6.0 + ug.size().x;
        }
        if let Some((cls, text)) = status {
            let c = match cls {
                "ok" => style::healthy(),
                "err" => style::critical(),
                _ => style::nav(),
            };
            let sg = style::galley(ui, &text, 10.5, Weight::W400, c, 0.0);
            ui.painter()
                .galley(Pos2::new(ux + 6.0, top + (CONTROL_H - sg.size().y) / 2.0), sg, c);
        }
        lh + 3.0 + CONTROL_H
    }
}

#[allow(clippy::too_many_arguments)]
fn grid(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    server: &Server,
    form: &mut FormState,
    fields: &[&Field],
    x: f32,
    y: f32,
    w: f32,
    armed: bool,
) {
    let mut cy = y;
    for f in fields {
        cy += Cell { f }.show(ui, ctx, server, form, x, cy, w, armed) + 8.0;
    }
}

/// A vertical bar with rounded ends, its colour a function of the height
/// down it (0 at the top, 1 at the bottom).
fn pill(painter: &egui::Painter, r: Rect, colour: impl Fn(f32) -> Color32) {
    let radius = 7.0f32.min(r.width() / 2.0).min(r.height() / 2.0);
    let mut pts: Vec<Pos2> = Vec::new();
    let arc = |c: Pos2, a0: f32, pts: &mut Vec<Pos2>| {
        for k in 0..=6 {
            let a = a0 + std::f32::consts::FRAC_PI_2 * k as f32 / 6.0;
            pts.push(c + Vec2::new(a.cos(), a.sin()) * radius);
        }
    };
    arc(
        Pos2::new(r.right() - radius, r.top() + radius),
        -std::f32::consts::FRAC_PI_2,
        &mut pts,
    );
    arc(Pos2::new(r.right() - radius, r.bottom() - radius), 0.0, &mut pts);
    arc(
        Pos2::new(r.left() + radius, r.bottom() - radius),
        std::f32::consts::FRAC_PI_2,
        &mut pts,
    );
    arc(
        Pos2::new(r.left() + radius, r.top() + radius),
        std::f32::consts::PI,
        &mut pts,
    );
    let t = |p: Pos2| ((p.y - r.top()) / r.height().max(1.0)).clamp(0.0, 1.0);
    let mut mesh = Mesh::default();
    let centre = r.center();
    mesh.colored_vertex(centre, colour(0.5));
    for p in &pts {
        mesh.colored_vertex(*p, colour(t(*p)));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(Shape::mesh(mesh));
}

fn mix(a: Color32, alpha: f32) -> Color32 {
    a.gamma_multiply(alpha)
}

/// The bar of one stretch: `.slz-bar` and its per-zone background.
fn draw_bar(painter: &egui::Painter, id: &str, r: Rect) {
    let accent = style::accent();
    match id {
        "fast" => pill(painter, r, |_| accent),
        "brake" => pill(painter, r, |t| {
            let a = 1.0 - 0.6 * t;
            mix(accent, a)
        }),
        "land" => pill(painter, r, |_| mix(accent, 0.4)),
        _ => {
            // repeating-linear-gradient(accent 60% 0 3px, transparent 3px 6px)
            let radius = 7.0f32.min(r.width() / 2.0);
            let mut y = r.top();
            while y < r.bottom() {
                let y1 = (y + 3.0).min(r.bottom());
                let mid = (y + y1) / 2.0;
                let from_end = (mid - r.top()).min(r.bottom() - mid);
                let inset = if from_end < radius {
                    radius - (radius * radius - (radius - from_end).powi(2)).max(0.0).sqrt()
                } else {
                    0.0
                };
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(r.left() + inset, y), Pos2::new(r.right() - inset, y1)),
                    0,
                    mix(accent, 0.6),
                );
                y += 6.0;
            }
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(r.left() - 3.0, r.bottom() + 4.0 - 2.0),
                    Pos2::new(r.right() + 3.0, r.bottom() + 4.0),
                ),
                2,
                style::text_2(),
            );
        }
    }
}

/// `.slz-span`: a bracket 8 px wide, 1.5 px in the muted colour, open to the
/// left, with 6 px rounded corners.
fn draw_bracket(painter: &egui::Painter, r: Rect) {
    let rad = 6.0f32.min(r.height() / 2.0);
    let (x0, x1) = (r.left() + 0.75, r.right() - 0.75);
    let (t, b) = (r.top() + 0.75, r.bottom() - 0.75);
    let mut pts = vec![Pos2::new(x0, t)];
    for k in 0..=6 {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::FRAC_PI_2 * k as f32 / 6.0;
        pts.push(Pos2::new(x1 - rad, t + rad) + Vec2::new(a.cos(), a.sin()) * rad);
    }
    for k in 0..=6 {
        let a = std::f32::consts::FRAC_PI_2 * k as f32 / 6.0;
        pts.push(Pos2::new(x1 - rad, b - rad) + Vec2::new(a.cos(), a.sin()) * rad);
    }
    pts.push(Pos2::new(x0, b));
    painter.add(Shape::line(pts, Stroke::new(1.5, style::text_3())));
}

/// What the card did this frame.
pub struct Outcome {
    pub bottom: f32,
    pub apply: bool,
}

/// Draws the card body in `x .. x + w` from `y` (inside the card's padding).
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    server: &Server,
    form: &mut FormState,
    section: &Section,
    fields: &[Field],
    action_status: Option<&(&'static str, String)>,
    x: f32,
    y: f32,
    w: f32,
    armed: bool,
    busy: bool,
) -> Outcome {
    let y = y + 1.0;
    let painter = ui.painter().clone();
    // the head: the title with its hint, the preset button at the right
    let title = style::galley(
        ui,
        &section.title.to_uppercase(),
        13.0,
        Weight::W600,
        style::text_2(),
        0.6,
    );
    let mut apply = false;
    let btn = section.actions.first().map(|a| {
        let b = Button::new(&a.label, Variant::Primary)
            .icon("check-check")
            .small()
            .enabled(!armed && !busy && !a.writes.is_empty());
        (a, b)
    });
    let btn_h = btn.as_ref().map_or(0.0, |(_, b)| b.height());
    let head_h = title.size().y.max(16.0).max(btn_h);
    painter.galley(
        Pos2::new(x, y + (head_h - title.size().y) / 2.0),
        title.clone(),
        style::text_2(),
    );
    if !section.hint.is_empty() {
        popover::info(
            ui,
            Pos2::new(x + title.size().x + 6.0 + 8.0, y + head_h / 2.0),
            &section.title,
            &section.hint,
            "landtitle",
        );
    }
    if let Some((a, b)) = btn {
        let (bw, bh) = (b.width(ui), b.height());
        let by = y + (head_h - bh) / 2.0;
        let status_g = action_status.map(|(cls, t)| {
            let c = match *cls {
                "ok" => style::healthy(),
                "err" => style::critical(),
                _ => style::nav(),
            };
            (style::galley(ui, t, 10.5, Weight::W400, c, 0.0), c)
        });
        let sw = status_g.as_ref().map_or(0.0, |(g, _)| 8.0 + g.size().x);
        let bx = x + w - bw - sw;
        let lines: Vec<String> = a
            .writes
            .iter()
            .map(|(p, v)| {
                let f = fields.iter().find(|f| &f.param == p);
                let label = f.map_or(String::new(), |f| {
                    format!(
                        "{}{}",
                        f.label,
                        if f.unit.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", f.unit)
                        }
                    )
                });
                format!("{label} {}", format_number(*v))
            })
            .collect();
        let tip = [a.hint.clone(), lines.join(", ")]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let b = b.title(&tip);
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(bx, by), Vec2::new(bw, bh)),
            ("landaction", &a.id),
        ) {
            apply = true;
        }
        if let Some((g, c)) = status_g {
            painter.galley(Pos2::new(bx + bw + 8.0, by + (bh - g.size().y) / 2.0), g, c);
        }
    }
    // padding-bottom 8, the rule, margin-bottom 4, the card's 8 px gap, the
    // zones' 4 px padding
    let rule = y + head_h + 8.0;
    painter.hline(
        egui::Rangef::new(x, x + w),
        rule + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    let mut cy = rule + 1.0 + 4.0 + 8.0 + 4.0;

    let by_param: HashMap<&str, &Field> = fields.iter().map(|f| (f.param.as_str(), f)).collect();
    let mut used: Vec<&str> = Vec::new();
    let live: HashMap<String, f64> = fields
        .iter()
        .filter_map(|f| form.live_value(f).map(|v| (f.param.clone(), v)))
        .collect();
    let texts = spans(&live);
    let narrow = w <= 340.0;
    for (zi, (id, end, speeds)) in ZONES.iter().enumerate() {
        let end_f: Vec<&Field> = end.and_then(|p| by_param.get(p).copied()).into_iter().collect();
        let speed_f: Vec<&Field> = speeds.iter().filter_map(|p| by_param.get(p).copied()).collect();
        if end_f.is_empty() && speed_f.is_empty() {
            continue;
        }
        for f in end_f.iter().chain(speed_f.iter()) {
            used.push(f.param.as_str());
        }
        let row_h;
        if !narrow {
            let fr = ((w - 108.0) / 2.0).max(0.0);
            let col_w = fr.min(150.0);
            let (lh, rh) = (grid_height(ui, &speed_f, col_w), grid_height(ui, &end_f, col_w));
            row_h = 58.0f32.max(lh).max(rh);
            let bar_x = x + fr + 5.0;
            let bar = Rect::from_min_size(Pos2::new(bar_x, cy), Vec2::new(14.0, row_h));
            let span_r = Rect::from_min_max(
                Pos2::new(bar.right() + 5.0, cy + 3.0),
                Pos2::new(bar.right() + 5.0 + 8.0, cy + row_h - 3.0),
            );
            let cell_x = span_r.right() + 5.0;
            let right_x = cell_x + 66.0 + 5.0;
            grid(
                ui,
                ctx,
                server,
                form,
                &speed_f,
                x + fr - col_w,
                cy + (row_h - lh) / 2.0,
                col_w,
                armed,
            );
            draw_bar(&painter, id, bar);
            draw_bracket(&painter, span_r);
            span_cell(ui, &texts[zi], cell_x, cy, row_h);
            grid(
                ui,
                ctx,
                server,
                form,
                &end_f,
                right_x,
                cy + (row_h - rh) / 2.0,
                col_w,
                armed,
            );
        } else {
            let fw = (w - 14.0 - 8.0 - 66.0 - 15.0).max(0.0);
            let (lh, rh) = (grid_height(ui, &speed_f, fw), grid_height(ui, &end_f, fw));
            let gap = if !speed_f.is_empty() && !end_f.is_empty() {
                6.0
            } else {
                0.0
            };
            row_h = 58.0f32.max(4.0 + lh + gap + rh + 4.0);
            let bar = Rect::from_min_size(Pos2::new(x, cy), Vec2::new(14.0, row_h));
            let span_r = Rect::from_min_max(
                Pos2::new(bar.right() + 5.0, cy + 3.0),
                Pos2::new(bar.right() + 5.0 + 8.0, cy + row_h - 3.0),
            );
            let cell_x = span_r.right() + 5.0;
            let fx = cell_x + 66.0 + 5.0;
            draw_bar(&painter, id, bar);
            draw_bracket(&painter, span_r);
            span_cell(ui, &texts[zi], cell_x, cy, row_h);
            grid(ui, ctx, server, form, &speed_f, fx, cy + 4.0, fw, armed);
            grid(ui, ctx, server, form, &end_f, fx, cy + 4.0 + lh + gap, fw, armed);
        }
        cy += row_h + 6.0;
    }
    let rest: Vec<&Field> = fields.iter().filter(|f| !used.contains(&f.param.as_str())).collect();
    if !rest.is_empty() {
        let h = grid_height(ui, &rest, w);
        grid(ui, ctx, server, form, &rest, x, cy, w, armed);
        cy += h + 6.0;
    }
    Outcome {
        bottom: cy - 6.0,
        apply,
    }
}

/// `.slz-span-cell`: the span in mono with its hint icon, centred on the row.
fn span_cell(ui: &mut egui::Ui, text: &str, x: f32, y: f32, h: f32) {
    let cy = y + h / 2.0;
    let mut ex = x;
    if !text.is_empty() {
        let g = style::galley_mono(ui, text, 11.5, style::text_1());
        ui.painter()
            .galley(Pos2::new(x, cy - g.size().y / 2.0), g.clone(), style::text_1());
        ex += g.size().x;
    }
    popover::info(
        ui,
        Pos2::new(ex + 4.0 + 8.0, cy),
        "",
        DERIVED_SPAN_HINT,
        ("landspan", (x as i32, y as i32)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(k, x)| ((*k).to_string(), *x)).collect()
    }

    #[test]
    fn the_brackets_say_how_long_each_stretch_is() {
        let s = spans(&v(&[
            ("MPC_LAND_ALT1", 10.0),
            ("MPC_LAND_ALT2", 5.0),
            ("MPC_LAND_ALT3", 1.0),
        ]));
        assert_eq!(s, ["> 10 m", "5 m", "4 m", "< 1 m"]);
    }

    #[test]
    fn a_stretch_whose_heights_are_the_wrong_way_round_says_nothing() {
        let s = spans(&v(&[
            ("MPC_LAND_ALT1", 3.0),
            ("MPC_LAND_ALT2", 5.0),
            ("MPC_LAND_ALT3", 6.0),
        ]));
        assert_eq!(s, ["> 3 m", "", "", "< 6 m"]);
    }

    #[test]
    fn a_missing_height_leaves_its_brackets_empty() {
        let s = spans(&v(&[("MPC_LAND_ALT2", 5.0)]));
        assert_eq!(s, ["", "", "", ""]);
        let s = spans(&v(&[("MPC_LAND_ALT1", 10.126), ("MPC_LAND_ALT2", 5.0)]));
        assert_eq!(s[1], "5.13 m");
    }
}
