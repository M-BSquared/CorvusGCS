//! The shelf, the companion strip and the editor drawn (`.schw-*` of
//! `schwalby.css`).

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use super::companion::{CompDialog, POWER_ACTIONS};
use super::model::{
    self, derived_name, missing_connections, new_connection_error, preview_line, preview_of, readable_on, session_name,
    Button, State, Target, BUTTON_COLORS, MAX_BUTTONS, NEW_CONNECTION,
};
use super::{After, Confirm, Editor, Schwalby};
use crate::plugins::ssh_launcher::model::{connection_options, Mode};
use crate::plugins::Host;
use crate::ui::card::{self, Look};
use crate::ui::form::{self, Form};
use crate::ui::kit::{self, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::segmented::{self, Segment};
use crate::ui::style::{self, Weight};
use crate::ui::{icons, menu, modal, popover};

const FIELD_GAP: f32 = 16.0;
/// One row of the shelf: the 32 px button between its 5 px paddings and the hairline.
const ROW_PITCH: f32 = 5.0 + 32.0 + 5.0 + 1.0;
const BTN_H: f32 = 32.0;

/// A row being carried to another place.
#[derive(Debug, Clone)]
pub struct RowDrag {
    pub id: String,
    /// From the row's top to where it was grabbed.
    pub grab: f32,
    /// The pointer's height.
    pub y: f32,
}

pub fn show(p: &mut Schwalby, ui: &mut egui::Ui, area: Rect, h: &mut Host) -> f32 {
    let mut f = Form::new(area.left(), area.top(), area.width());
    companion_strip(p, ui, &mut f, h);
    if p.editor.is_some() {
        editor(p, ui, &mut f, h);
    } else {
        shelf(p, ui, &mut f, h);
        if let Some((text, kind)) = p.status.clone() {
            f.skip(10.0);
            status_line(ui, &mut f, &text, tone(kind));
        }
    }
    if let Some(out) = p.output.clone() {
        f.skip(12.0);
        let g = kit::wrapped_weight(ui, &out, 12.0, Weight::W400, style::text_2(), f.w - 26.0, 1.5);
        let r = Rect::from_min_size(Pos2::new(f.x, f.y), Vec2::new(f.w, g.size().y.min(200.0) + 22.0));
        ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
        ui.painter().rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .with_clip_rect(r)
            .galley(Pos2::new(r.left() + 13.0, r.top() + 11.0), g, style::text_2());
        f.y = r.bottom();
    }
    f.y - area.top()
}

fn tone(kind: &str) -> Tone {
    match kind {
        "err" => Tone::Err,
        "warn" => Tone::Warn,
        "info" => Tone::Plain,
        _ => Tone::Ok,
    }
}

/// `.schw-status`: a slim one-liner, never a block.
fn status_line(ui: &egui::Ui, f: &mut Form, text: &str, tone: Tone) {
    let fg = match tone {
        Tone::Ok => style::healthy(),
        Tone::Err => style::critical(),
        Tone::Warn => style::warning(),
        Tone::Plain => style::text_2(),
    };
    let line = 11.5 * 1.4;
    let h = line + 6.0 + 2.0;
    let rect = f.rect(h);
    let inner_left = if tone == Tone::Plain { 10.0 } else { 1.0 + 10.0 };
    if tone != Tone::Plain {
        card::paint(ui.painter(), rect, Look::Tone(tone));
        ui.painter()
            .circle_filled(Pos2::new(rect.left() + 1.0 + 10.0 + 3.5, rect.center().y), 3.5, fg);
    }
    let left = rect.left() + inner_left + if tone == Tone::Plain { 0.0 } else { 15.0 };
    let shown = fit_text(ui, text, 11.5, Weight::W400, rect.right() - 10.0 - left);
    let g = style::galley(ui, &shown, 11.5, Weight::W400, fg, 0.0);
    ui.painter()
        .galley(Pos2::new(left, rect.center().y - g.size().y / 2.0), g, fg);
    ui.interact(rect, ui.scope_id().with(("schwstatus", text)), egui::Sense::hover())
        .on_hover_text(text);
    f.y = rect.bottom();
}

/// `text-overflow: ellipsis` for `width` points.
fn fit_text(ui: &egui::Ui, text: &str, size: f32, weight: Weight, width: f32) -> String {
    let w = |s: &str| style::galley(ui, s, size, weight, Color32::WHITE, 0.0).size().x;
    if w(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 0 {
        let s: String = chars[..n].iter().collect::<String>() + "\u{2026}";
        if w(&s) <= width {
            return s;
        }
        n -= 1;
    }
    "\u{2026}".into()
}

// ---- the companion computer ----------------------------------------------------

fn state_color(s: State) -> Color32 {
    match s {
        State::Online => style::healthy(),
        State::Offline => style::critical(),
        State::Checking | State::Error => style::warning(),
        State::Unset => style::text_3(),
    }
}

fn companion_strip(p: &mut Schwalby, ui: &mut egui::Ui, f: &mut Form, h: &mut Host) {
    let state = p.companion_now(h.now);
    let rect = f.rect(30.0);
    p.strip = rect;
    let arrow_entry = if state == State::Online {
        p.companion_arrow()
    } else {
        None
    };
    let right = rect.right() - 1.0 - 4.0;
    let arrow = arrow_entry
        .as_ref()
        .map(|_| Rect::from_min_size(Pos2::new(right - 28.0, rect.top() + 1.0), Vec2::splat(28.0)));
    let main_right = arrow.map_or(right, |a| a.left() - 4.0);
    let main = Rect::from_min_max(
        Pos2::new(rect.left() + 1.0, rect.top() + 1.0),
        Pos2::new(main_right, rect.bottom() - 1.0),
    );
    let resp = ui.interact(main, ui.scope_id().with("schwcomp"), egui::Sense::click());
    let hover = ui.rect_contains_pointer(rect);
    let border = match state {
        State::Online => style::healthy(),
        State::Offline => style::critical(),
        State::Checking | State::Error => style::warning(),
        State::Unset => style::border_soft(),
    };
    card::paint(
        ui.painter(),
        rect,
        Look::custom(
            if hover { style::surface_1() } else { style::surface_2() },
            border,
            style::RADIUS_BTN,
        ),
    );
    let painter = ui.painter().clone();
    // chip icon, then the name and the address on one line
    let icon_color = match state {
        State::Online => style::healthy(),
        State::Offline => style::critical(),
        _ => style::text_3(),
    };
    let cy = rect.center().y;
    let x0 = main.left() + 10.0;
    icons::paint(
        ui,
        "cpu",
        Rect::from_center_size(Pos2::new(x0 + 7.0, cy), Vec2::splat(14.0)),
        icon_color,
    );
    let text_x = x0 + 14.0 + 8.0;
    // the state at the right: a dot and a word
    let word = if state == State::Online && p.probe.rtt.is_some() {
        format!(
            "{} \u{b7} {}",
            state.text(),
            model::format_rtt(p.probe.rtt.unwrap_or(0.0))
        )
    } else {
        state.text().to_string()
    };
    let wg = style::galley(ui, &word, 11.0, Weight::W600, style::text_2(), 0.3);
    let state_right = main.right() - 6.0;
    let word_x = state_right - wg.size().x;
    painter.galley(Pos2::new(word_x, cy - wg.size().y / 2.0), wg, style::text_2());
    kit::dot(&painter, Pos2::new(word_x - 6.0 - 3.5, cy), state_color(state), false);
    let state_left = word_x - 6.0 - 7.0;
    // name first; the address gives way when the panel is narrow
    let (name, host) = match &p.companion {
        Some(c) => (c.display_name().to_string(), c.host.clone()),
        None => ("Companion computer".to_string(), "Press to set".to_string()),
    };
    let room = (state_left - 8.0 - text_x).max(0.0);
    let name_g = style::galley(ui, &name, 12.0, Weight::W600, style::text_1(), 0.0);
    let name_w = name_g.size().x.min(room);
    let clip = Rect::from_min_max(Pos2::new(text_x, rect.top()), Pos2::new(text_x + name_w, rect.bottom()));
    let shown = if name_g.size().x > room {
        fit_text(ui, &name, 12.0, Weight::W600, room)
    } else {
        name.clone()
    };
    let shown_g = style::galley(ui, &shown, 12.0, Weight::W600, style::text_1(), 0.0);
    painter
        .with_clip_rect(clip)
        .galley(Pos2::new(text_x, cy - shown_g.size().y / 2.0), shown_g, style::text_1());
    let host_x = text_x + name_w + 8.0;
    let host_room = (state_left - 8.0 - host_x).max(0.0);
    if host_room > 8.0 {
        let hs = if p.companion.is_some() {
            fit_mono(ui, &host, host_room)
        } else {
            fit_text(ui, &host, 11.0, Weight::W400, host_room)
        };
        let hg = if p.companion.is_some() {
            style::galley_mono(ui, &hs, 11.0, style::text_3())
        } else {
            style::galley(ui, &hs, 11.0, Weight::W400, style::text_3(), 0.0)
        };
        painter.galley(Pos2::new(host_x, cy - hg.size().y / 2.0 + 1.0), hg, style::text_3());
    }
    let title = if state == State::Error {
        p.probe.error.clone()
    } else if let Some(c) = &p.companion {
        format!(
            "Pinged every {} s, offline after {} s without a reply",
            c.interval_s, c.timeout_s
        )
    } else {
        "Set the companion computer to watch".to_string()
    };
    let resp = resp.on_hover_text(title);
    if resp.clicked() {
        p.dialog = Some(CompDialog::new(p.companion.as_ref(), &p.connections));
    }
    // a ping-only companion has no login to send a command through
    if resp.secondary_clicked() && p.companion.as_ref().is_some_and(|c| !c.connection.is_empty()) {
        p.power_menu = true;
    }
    // The arrow only once the ping answers: a terminal to a computer that is
    // not there would just wait for its connection to time out.
    if let (Some(entry), Some(a)) = (arrow_entry, arrow) {
        let session = session_name(&entry.id);
        let up = p.live.contains(&session);
        let busy = p.opening(&session);
        let title = if up {
            "Open its SSH terminal window".to_string()
        } else {
            format!("Connect to {} and open a terminal", entry.connection)
        };
        let r = ui
            .interact(a, ui.scope_id().with("schwcomparrow"), egui::Sense::click())
            .on_hover_text(title);
        if r.hovered() {
            painter.rect_filled(a, style::RADIUS_BTN, style::surface_hover());
        }
        icons::paint(
            ui,
            "chevron-right",
            Rect::from_center_size(a.center(), Vec2::splat(15.0)),
            style::healthy().gamma_multiply(if busy { 0.35 } else { 1.0 }),
        );
        if r.clicked() && !busy {
            if up {
                p.show_terminal(&entry, h);
            } else {
                p.connect_and_open(&entry, h);
            }
        }
    }
    f.y = rect.bottom() + 10.0;
}

fn fit_mono(ui: &egui::Ui, text: &str, width: f32) -> String {
    let w = |s: &str| style::galley_mono(ui, s, 11.0, Color32::WHITE).size().x;
    if w(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 0 {
        let s: String = chars[..n].iter().collect::<String>() + "\u{2026}";
        if w(&s) <= width {
            return s;
        }
        n -= 1;
    }
    "\u{2026}".into()
}

// ---- the shelf -------------------------------------------------------------------

/// What a press on a row did.
enum Hit {
    Launch,
    Terminal,
    Connect,
    Stop,
    Edit,
}

fn shelf(p: &mut Schwalby, ui: &mut egui::Ui, f: &mut Form, h: &mut Host) {
    let mut act: Option<(usize, Hit)> = None;
    let mut add = false;
    let mut toggle = false;
    let mut order_commit: Option<(String, usize)> = None;
    let vh = ui.ctx().content_rect().height();
    if p.buttons.is_empty() {
        p.edit_mode = false;
    }
    form::card(ui, f, "", |ui, f| {
        let inner_top = f.y;
        if p.buttons.is_empty() {
            form::desc(
                ui,
                f,
                "No launch buttons yet. Add one for each program you start before a flight, on this computer or on a companion computer.",
            );
            f.skip(14.0);
        } else {
            let n = p.buttons.len();
            let base = f.y;
            // while a row is carried, the others close up around the slot it would drop into
            let mut order: Vec<usize> = (0..n).collect();
            let mut carried: Option<(usize, usize)> = None;
            if let Some(d) = &p.drag {
                if let Some(from) = p.buttons.iter().position(|b| b.id == d.id) {
                    let others: Vec<usize> = (0..n).filter(|i| *i != from).collect();
                    let centre = d.y - d.grab + ROW_PITCH / 2.0 - 5.0;
                    let slot = (((centre - base) / ROW_PITCH).floor().max(0.0) as usize).min(others.len());
                    order = others;
                    order.insert(slot, from);
                    carried = Some((from, slot));
                }
            }
            for (k, &i) in order.iter().enumerate() {
                let is_carried = carried.is_some_and(|c| c.0 == i);
                let mut top = base + k as f32 * ROW_PITCH + 5.0 - if k == 0 { 5.0 } else { 0.0 };
                if is_carried {
                    if let Some(d) = &p.drag {
                        top = (d.y - d.grab).clamp(base - 5.0, base + (n as f32 - 1.0) * ROW_PITCH);
                    }
                }
                let last = k + 1 == n;
                let out = row(p, ui, f.x, f.w, top, i, is_carried, last);
                if let Some(hit) = out.hit {
                    act = Some((i, hit));
                }
                if let Some(grab) = out.grab {
                    p.drag = Some(RowDrag {
                        id: p.buttons[i].id.clone(),
                        grab,
                        y: out.y,
                    });
                }
                if out.moved {
                    if let Some(d) = p.drag.as_mut() {
                        d.y = out.y;
                    }
                }
                if let Some(step) = out.step {
                    let at = i as isize + step;
                    if at >= 0 && (at as usize) < n {
                        order_commit = Some((p.buttons[i].id.clone(), at as usize));
                    }
                }
                if out.dropped {
                    if let Some((_, slot)) = carried {
                        order_commit = Some((p.buttons[i].id.clone(), slot));
                    }
                    p.drag = None;
                }
            }
            f.y = base + n as f32 * ROW_PITCH - 1.0 + 5.0 - 5.0;
            f.skip(14.0);
            if p.drag.is_some() {
                ui.ctx().request_repaint();
            }
        }
        // a shelf copied from another computer names connections this one has not saved
        let missing = if p.conn_loaded() {
            missing_connections(&p.buttons, &p.connections)
        } else {
            Vec::new()
        };
        let note = if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|n| format!("\u{201C}{n}\u{201D}")).collect();
            format!(
                "Not set up on this computer yet: {}. Press a button that uses one to enter its address and login once.",
                names.join(", ")
            )
        } else if p.buttons.iter().any(|b| b.target == Target::Ssh) {
            p.conn_error.clone()
        } else {
            String::new()
        };
        if !note.is_empty() {
            f.skip(-4.0);
            form::desc(ui, f, &note);
            f.skip(10.0);
        }
        // the card is a tall column with its bar on the bottom edge
        let bar_h = kit::Button::new("x", Variant::Secondary).icon("plus").height();
        let want_bottom = inner_top + 0.55 * vh - 2.0 * 19.0;
        if f.y + bar_h < want_bottom {
            f.y = want_bottom - bar_h;
        }
        let has = !p.buttons.is_empty();
        let toggle_w = if has { 38.0 } else { 0.0 };
        let add_w = f.w - if has { toggle_w + 8.0 } else { 0.0 };
        let b = kit::Button::new("Add button", Variant::Secondary)
            .icon("plus")
            .enabled(p.buttons.len() < MAX_BUTTONS);
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(f.x, f.y), Vec2::new(add_w, bar_h)),
            "schw-add",
        ) {
            add = true;
        }
        if has {
            let r = Rect::from_min_size(Pos2::new(f.x + f.w - toggle_w, f.y), Vec2::new(toggle_w, bar_h));
            let title = if p.edit_mode {
                "Done editing"
            } else {
                "Edit the buttons"
            };
            let pressed = if p.edit_mode {
                // `setActive`: the accent's dark fill and its bright glyph
                let resp = ui
                    .interact(r, ui.scope_id().with("schw-edit-toggle"), egui::Sense::click())
                    .on_hover_text(title);
                ui.painter().rect_filled(r, style::RADIUS_BTN, style::accent_dark());
                icons::paint(
                    ui,
                    "pencil",
                    Rect::from_center_size(r.center(), Vec2::splat(16.0)),
                    style::accent_bright(),
                );
                resp.clicked()
            } else {
                kit::Button::new("", Variant::Secondary)
                    .icon("pencil")
                    .title(title)
                    .show(ui, r, "schw-edit-toggle")
            };
            if pressed {
                toggle = true;
            }
        }
        f.y += bar_h;
    });
    if toggle {
        p.edit_mode = !p.edit_mode;
        p.drag = None;
    }
    if let Some((id, slot)) = order_commit {
        p.commit_order(&id, slot, h);
    }
    if add {
        let draft = Button::blank(p.buttons.last(), &p.first_connection());
        p.open_editor(draft, true);
    }
    if let Some((i, hit)) = act {
        let b = p.buttons[i].clone();
        match hit {
            Hit::Launch => {
                if b.pending {
                    p.confirm = Some(Confirm {
                        title: format!("Allow \"{}\"?", b.label),
                        lines: vec![
                            "An AI client added this button. Pressing it will run:".into(),
                            preview_of(&b),
                            "Allow it only if you would run that yourself.".into(),
                        ],
                        label: "Allow".into(),
                        icon: "check",
                        danger: false,
                        then: After::Approve(b.id.clone()),
                    });
                } else {
                    p.launch(&b, h);
                }
            }
            Hit::Terminal => p.show_terminal(&b, h),
            Hit::Connect => p.connect_and_open(&b, h),
            Hit::Stop => p.stop(&b, h),
            Hit::Edit => p.open_editor(b, false),
        }
    }
}

#[derive(Default)]
struct RowOut {
    hit: Option<Hit>,
    /// The grip was grabbed this far from the row's top.
    grab: Option<f32>,
    moved: bool,
    dropped: bool,
    y: f32,
    /// An arrow key on the focused grip.
    step: Option<isize>,
}

/// One row: the grip in edit mode, the launch button, and its tools.
#[allow(clippy::too_many_arguments)]
fn row(p: &Schwalby, ui: &mut egui::Ui, x: f32, w: f32, top: f32, i: usize, carried: bool, last: bool) -> RowOut {
    let b = &p.buttons[i];
    let mut out = RowOut::default();
    let session = session_name(&b.id);
    let running = p.live.contains(&session);
    let stopping = p.closing(&session);
    let failure = p.errors.get(&b.id);
    let reordering = p.drag.is_some();
    let row_rect = Rect::from_min_size(Pos2::new(x, top - 5.0), Vec2::new(w, BTN_H + 10.0));
    if carried {
        ui.painter()
            .add(style::shadow_pop().as_shape(row_rect, style::RADIUS_BTN));
        ui.painter()
            .rect_filled(row_rect, style::RADIUS_BTN, style::surface_2());
    }
    let hover = ui.rect_contains_pointer(row_rect);
    let strong = (hover || running || failure.is_some()) && !reordering;
    let cy = top + BTN_H / 2.0;
    let mut left = x;
    // the grip: drag a row up or down by it, or focus it and press the arrow keys
    if p.edit_mode && p.buttons.len() > 1 {
        let g = Rect::from_min_size(Pos2::new(x - 4.0, cy - 14.0), Vec2::new(22.0, 28.0));
        let resp = ui.interact(
            g,
            ui.scope_id().with(("schwgrip", &b.id)),
            egui::Sense::click_and_drag(),
        );
        let resp = resp.on_hover_text("Drag to move up or down");
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                out.grab = Some(pos.y - (top - 5.0));
                out.y = pos.y;
            }
        }
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                out.moved = true;
                out.y = pos.y;
            }
        }
        if resp.drag_stopped() {
            out.dropped = true;
        }
        if resp.has_focus() {
            let step = ui.input(|inp| {
                if inp.key_pressed(egui::Key::ArrowUp) {
                    -1
                } else if inp.key_pressed(egui::Key::ArrowDown) {
                    1
                } else {
                    0
                }
            });
            if step != 0 {
                out.step = Some(step);
            }
        }
        icons::paint(
            ui,
            "grip-vertical",
            Rect::from_center_size(g.center(), Vec2::splat(15.0)),
            if resp.hovered() {
                style::text_1()
            } else {
                style::text_3()
            },
        );
        left = g.right() + 8.0 - 0.0;
    }
    // tools, from the right: stop or pencil, the live dot, the warning
    let mut right = x + w;
    let mut tools = false;
    ui.set_opacity(if strong {
        1.0
    } else if reordering {
        0.0
    } else {
        0.55
    });
    if !reordering {
        let last_tool = Rect::from_min_size(Pos2::new(right - 28.0, cy - 14.0), Vec2::splat(28.0));
        if running || stopping {
            let label = if stopping {
                "Stopping"
            } else if b.target == Target::Local {
                "Stop the program with Ctrl-C and close its terminal"
            } else {
                "Stop the program with Ctrl-C and close its terminal connection"
            };
            if tool(ui, last_tool, "square", label, style::critical(), ("schw-stop", &b.id)) && !stopping {
                out.hit = Some(Hit::Stop);
            }
            right -= 28.0 + 2.0;
            tools = true;
        } else if p.edit_mode {
            if tool(ui, last_tool, "pencil", "Edit", style::text_3(), ("schw-edit", &b.id)) {
                out.hit = Some(Hit::Edit);
            }
            right -= 28.0 + 2.0;
            tools = true;
        }
        if running {
            let c = Pos2::new(right - 4.0 - 3.5, cy);
            ui.painter()
                .circle_filled(c, 5.5, style::healthy().gamma_multiply(0.25));
            ui.painter().circle_filled(c, 3.5, style::healthy());
            right -= 15.0 + 2.0;
            tools = true;
        }
        if let Some(why) = failure {
            let r = Rect::from_min_size(Pos2::new(right - 28.0, cy - 14.0), Vec2::splat(28.0));
            let id = ui.scope_id().with(("schw-error", &b.id));
            let resp = ui.interact(r, id, egui::Sense::click());
            let open = popover::is_open(ui.ctx(), id);
            let color = if resp.hovered() || open {
                style::critical_bright()
            } else {
                style::critical()
            };
            if resp.hovered() || open {
                ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_hover());
            }
            icons::paint(
                ui,
                "triangle-alert",
                Rect::from_center_size(r.center(), Vec2::splat(15.0)),
                color,
            );
            popover::drive(
                ui.ctx(),
                id,
                &resp,
                &popover::Content::hint(&b.label, why),
                popover::Look::Hint,
            );
            right -= 28.0 + 2.0;
            tools = true;
        }
    }
    ui.set_opacity(1.0);
    let btn_right = if tools { right - 8.0 + 2.0 } else { x + w };
    let rect = Rect::from_min_max(Pos2::new(left, top), Pos2::new(btn_right.max(left + 40.0), top + BTN_H));
    launch_button(p, ui, rect, b, running, &mut out);
    if !last && !carried {
        ui.painter().hline(
            egui::Rangef::new(x, x + w),
            top + BTN_H + 5.5,
            Stroke::new(1.0, style::border_soft()),
        );
    }
    out
}

/// The launch button: the label, where it runs and the arrow into its terminal.
fn launch_button(p: &Schwalby, ui: &mut egui::Ui, rect: Rect, b: &Button, running: bool, out: &mut RowOut) {
    let busy = p.pressing(&b.id);
    let id = ui.scope_id().with(("schw-launch", &b.id));
    let sense = if p.drag.is_some() {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    let resp = ui.interact(rect, id, sense);
    let hover = resp.hovered() && p.drag.is_none();
    let (fill, text, stroke_dashed) = if b.pending {
        (Color32::TRANSPARENT, style::warning(), true)
    } else if !b.color.is_empty() {
        let c = hex_color(&b.color);
        let c = if hover { brighten(c) } else { c };
        (c, hex_color(readable_on(&b.color)), false)
    } else {
        (
            if hover { style::accent_hover() } else { style::accent() },
            style::text_inverted(),
            false,
        )
    };
    let fade = if busy { 0.7 } else { 1.0 };
    ui.painter().rect_filled(rect, style::RADIUS_BTN, fill);
    if stroke_dashed {
        dashed_border(ui.painter(), rect, style::warning());
    }
    // from the right: the arrow, then where it runs
    let mut right = rect.right() - 1.0 - 11.0;
    let mut arrow_rect = None;
    if b.mode != Mode::Background {
        let a = Rect::from_min_size(Pos2::new(right + 4.0 - 20.0, rect.center().y - 10.0), Vec2::splat(20.0));
        arrow_rect = Some(a);
        right = a.left() - 7.0;
    }
    let (where_icon, where_title) = if b.target == Target::Local {
        ("laptop", "Runs on this computer".to_string())
    } else {
        (
            "server",
            format!(
                "Runs on {} over SSH",
                if b.connection.is_empty() {
                    "an SSH connection"
                } else {
                    &b.connection
                }
            ),
        )
    };
    let where_r = Rect::from_min_size(Pos2::new(right - 16.0, rect.center().y - 8.0), Vec2::splat(16.0));
    icons::paint(ui, where_icon, where_r, text.gamma_multiply(0.75 * fade));
    ui.interact(where_r, id.with("where"), egui::Sense::hover())
        .on_hover_text(where_title);
    let mut x = rect.left() + 1.0 + 11.0;
    if b.by_mcp {
        let g = style::galley(ui, "AI", 9.0, Weight::W700, text.gamma_multiply(0.85 * fade), 0.4);
        let mark = Rect::from_min_size(
            Pos2::new(x, rect.center().y - 8.0),
            Vec2::new(g.size().x + 8.0 + 2.0, 16.0),
        );
        ui.painter().rect_stroke(
            mark,
            3,
            Stroke::new(1.0, text.gamma_multiply(0.85 * fade)),
            egui::StrokeKind::Inside,
        );
        ui.painter().galley(
            Pos2::new(mark.left() + 5.0, mark.center().y - g.size().y / 2.0),
            g,
            text,
        );
        ui.interact(mark, id.with("ai"), egui::Sense::hover())
            .on_hover_text(if b.pending {
                "Added by an AI client, waiting for you to allow it"
            } else {
                "Added by an AI client"
            });
        x = mark.right() + 6.0;
    }
    let room = (where_r.left() - 8.0 - 7.0 - x).max(0.0);
    let label = fit_text_w(ui, &b.label, room);
    let g = style::galley(ui, &label, 11.0, Weight::W600, text.gamma_multiply(fade), 0.1);
    ui.painter()
        .galley(Pos2::new(x, rect.center().y - g.size().y / 2.0), g, text);
    let preview = {
        let line = preview_of(b);
        if line.is_empty() {
            b.command.clone()
        } else {
            line
        }
    };
    let title = if b.pending {
        format!("Added by an AI client. Press to see what it runs and allow it.\n{preview}")
    } else if running {
        format!(
            "{} in the terminal it is already in.\n{preview}",
            if model::restarts(b) {
                "Stop it with Ctrl-C and start it again"
            } else {
                "Run it again"
            }
        )
    } else {
        preview
    };
    let resp = resp.on_hover_text(title);
    if resp.clicked() && !busy {
        out.hit = Some(Hit::Launch);
    }
    if let Some(a) = arrow_rect {
        let r = ui.interact(a, id.with("arrow"), sense).on_hover_text(if running {
            "Open its terminal window to watch it, or press Ctrl-C to stop it"
        } else {
            "Open its terminal and connect. The program starts with the button."
        });
        let opening = p.opening(&session_name(&b.id));
        if r.hovered() && !opening {
            ui.painter()
                .rect_filled(a, style::RADIUS_BTN, Color32::from_white_alpha(46));
        }
        icons::paint(
            ui,
            "chevron-right",
            Rect::from_center_size(a.center(), Vec2::splat(16.0)),
            text.gamma_multiply(if opening {
                0.35
            } else if r.hovered() {
                1.0
            } else {
                0.85
            }),
        );
        if r.clicked() && !opening {
            out.hit = Some(if running { Hit::Terminal } else { Hit::Connect });
        }
    }
}

fn fit_text_w(ui: &egui::Ui, text: &str, width: f32) -> String {
    fit_text(ui, text, 11.0, Weight::W600, width)
}

fn hex_color(hex: &str) -> Color32 {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// `filter: brightness(1.1)`.
fn brighten(c: Color32) -> Color32 {
    let f = |v: u8| (f32::from(v) * 1.1).min(255.0) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

fn dashed_border(painter: &egui::Painter, rect: Rect, color: Color32) {
    let r = rect.shrink(0.5);
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.0, color), 4.0, 3.0));
}

fn tool(
    ui: &mut egui::Ui,
    r: Rect,
    icon: &'static str,
    title: &str,
    color: Color32,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let resp = ui
        .interact(r, ui.scope_id().with(id), egui::Sense::click())
        .on_hover_text(title);
    if resp.hovered() {
        ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_hover());
    }
    let c = if resp.hovered() && color == style::text_3() {
        style::text_1()
    } else {
        color
    };
    icons::paint(ui, icon, Rect::from_center_size(r.center(), Vec2::splat(15.0)), c);
    resp.clicked()
}

// ---- the editor ------------------------------------------------------------------

const LOCAL_FOLDER: &str = "Entered before the program starts. Leave empty to run in your home folder.";
const SSH_FOLDER: &str = "Entered before the program starts. Leave empty to run in the account's home directory.";
const LOCAL_PROGRAM: &str = "Run by your own login shell on this computer, so a pipeline or arguments work as typed. It runs as you and can do whatever your account can do.";
const SSH_PROGRAM: &str = "Run by the remote login shell, so a pipeline or arguments work as typed. It runs as the connection's own user and can do whatever that account can do.";
const LOCAL_TERMINAL: &str = "On: the program runs in a terminal of this button's own on this computer, and pressing the button again runs it again in that same terminal. The arrow beside the button opens the window to watch it in. Read its output there, and stop it with Ctrl-C, the window's disconnect button, or the stop button that takes the place of the pencil while it runs. Off: the program starts in the background, with nothing to watch. Either way it is stopped when Corvus closes.";
const SSH_TERMINAL: &str = "On: the program runs in an SSH session of this button's own, and pressing the button again runs it again in that same terminal. The arrow beside the button opens the window to watch it in. Read its output there, and stop it with Ctrl-C, the window's disconnect button, or the stop button that takes the place of the pencil while it runs. The session lasts as long as Corvus does. Off: the program is started with nohup and detached, so it survives Corvus closing, but there is nothing to watch and nothing to stop from here.";

fn editor(p: &mut Schwalby, ui: &mut egui::Ui, f: &mut Form, h: &mut Host) {
    let mut save = false;
    let mut cancel = false;
    let mut delete = false;
    let taken: Vec<String> = p
        .connections
        .iter()
        .filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let options = connection_options(
        &p.connections,
        &p.editor.as_ref().map_or(String::new(), |e| e.draft.connection.clone()),
    );
    let running = p
        .editor
        .as_ref()
        .is_some_and(|e| p.live.contains(&session_name(&e.draft.id)));
    let no_terminal = p.local_terminal_reason();
    let Some(e) = p.editor.as_mut() else {
        return;
    };
    let ssh = e.draft.target == Target::Ssh;
    // the mode the operator chose; a terminal that cannot be had overrides it while it is the target
    let blocked = !ssh && !no_terminal.is_empty();
    e.draft.mode = if blocked { Mode::Background } else { e.wanted };
    let picked_new = ssh && e.choice == NEW_CONNECTION;
    let target_name = if !ssh {
        String::new()
    } else if picked_new {
        let n = e.conn.name.trim();
        if n.is_empty() {
            derived_name(&e.conn)
        } else {
            n.to_string()
        }
    } else {
        e.choice.clone()
    };
    let line = preview_line(
        e.draft.target,
        &target_name,
        &e.draft.directory,
        &e.draft.command,
        e.draft.mode,
    );
    let conn_error = if picked_new {
        new_connection_error(&e.conn, &taken)
    } else if ssh && target_name.is_empty() {
        "Pick a connection to run it on.".to_string()
    } else {
        String::new()
    };
    let can_save = !line.is_empty() && conn_error.is_empty() && e.saving.is_none();
    if conn_error.contains("already saved") {
        e.message = Some((conn_error.clone(), "warn"));
    } else if e.message.as_ref().is_some_and(|m| m.1 == "warn") {
        e.message = None;
    }
    let message = e.message.clone();
    form::card(ui, f, "", |ui, f| {
        if let Some((m, k)) = &message {
            form::message(ui, f, m, tone(k));
            f.skip(FIELD_GAP);
        }
        form::input(
            ui,
            f,
            "Button",
            "",
            "What the button says. Leave empty to use the command itself.",
            &mut e.draft.label,
            "Start mission",
            false,
            false,
            "schw-label",
        );
        f.skip(FIELD_GAP);
        let color = e.draft.color.clone();
        form::field(
            ui,
            f,
            "Colour",
            "",
            "Default follows the theme. Pick another to tell this button apart on the shelf.",
            |ui, x, y, w| {
                if let Some(c) = color_strip(ui, x, y, w, &color) {
                    e.draft.color = c;
                }
                color_chip_size(w)
            },
        );
        f.skip(FIELD_GAP);
        let segs = [Segment::new("Local").icon("laptop"), Segment::new("SSH").icon("server")];
        let selected = usize::from(ssh);
        let mut pressed = None;
        form::field(ui, f, "Runs on", "", "", |ui, x, y, w| {
            let (r, hit) = segmented::show(
                ui,
                Pos2::new(x, y),
                &segs,
                selected,
                "schw-target",
                segmented::Opts::stretched(w),
            );
            pressed = hit;
            r.height()
        });
        if let Some(i) = pressed {
            e.draft.target = if i == 1 { Target::Ssh } else { Target::Local };
        }
        if ssh {
            f.skip(FIELD_GAP);
            let choices: Vec<Choice> = options
                .iter()
                .map(|(v, l)| Choice::new(v.as_str(), l.as_str()))
                .collect();
            let selected = e.choice.clone();
            form::field(
                ui, f, "Connection",
                "One of the saved SSH connections, or a new one typed in below. Either way the password is kept by the backend. This plugin only ever names the connection.",
                "",
                |ui, x, y, w| {
                    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, form::CONTROL_H));
                    if let Some(v) = kit::select(ui, r, &choices, &selected, "", false, "", "schw-conn") {
                        e.choice = v;
                    }
                    form::CONTROL_H
                },
            );
            if e.choice == NEW_CONNECTION {
                new_connection(ui, f, &mut e.conn);
            }
        }
        f.skip(FIELD_GAP);
        form::input(
            ui,
            f,
            "Folder",
            "",
            if ssh { SSH_FOLDER } else { LOCAL_FOLDER },
            &mut e.draft.directory,
            if ssh { "/home/pilot/mission" } else { "~/mission" },
            true,
            false,
            "schw-dir",
        );
        f.skip(FIELD_GAP);
        form::input(
            ui,
            f,
            "Program",
            if ssh { SSH_PROGRAM } else { LOCAL_PROGRAM },
            "",
            &mut e.draft.command,
            "./start.sh",
            true,
            false,
            "schw-cmd",
        );
        f.skip(FIELD_GAP);
        let terminal = e.draft.mode != Mode::Background;
        if let Some(on) = form::switch_field(
            ui,
            f,
            "schw-terminal",
            form::Switch::new("Run in a terminal", &no_terminal_hint(blocked, &no_terminal), terminal)
                .info(if ssh { SSH_TERMINAL } else { LOCAL_TERMINAL })
                .enabled(!blocked, &no_terminal),
            style::text_1(),
            12.0,
        ) {
            e.wanted = if on { Mode::Terminal } else { Mode::Background };
        }
        if e.draft.mode != Mode::Background {
            f.skip(FIELD_GAP);
            if let Some(on) = form::switch_field(
                ui,
                f,
                "schw-restart",
                form::Switch::new("Restart on press", "", e.draft.restart).info(
                    "On: pressing the button while its program is still running first sends Ctrl-C to stop it, waits a second, and then starts it again in the same terminal. Off: the line is typed into the terminal as it is, so a program still in the foreground gets it as input.",
                ),
                style::text_1(),
                12.0,
            ) {
                e.draft.restart = on;
            }
        }
        f.skip(FIELD_GAP);
        preview_box(ui, f, &line);
        f.skip(FIELD_GAP + 4.0);
        let buttons: Vec<kit::Button<'_>> = {
            let mut v = vec![kit::Button::new("Save", Variant::Primary)
                .icon("check")
                .enabled(can_save)
                .title(if can_save {
                    ""
                } else if !conn_error.is_empty() {
                    &conn_error
                } else {
                    "Name a program first."
                })];
            if !e.is_new {
                v.push(
                    kit::Button::new("Delete", Variant::Danger)
                        .icon("trash-2")
                        .title("Remove this button"),
                );
            }
            v.push(kit::Button::new("Cancel", Variant::Ghost));
            v
        };
        let n = buttons.len();
        match form::actions(ui, f, buttons) {
            Some(0) => save = true,
            Some(i) if i == n - 1 => cancel = true,
            Some(_) => delete = true,
            None => {}
        }
    });
    if save {
        if picked_new {
            let body = model::new_connection_body(&e.conn);
            e.saving = Some(h.post_any("/api/ssh/connections", body));
        } else {
            let mut d = e.draft.clone();
            if ssh {
                d.connection = e.choice.clone();
            }
            let is_new = e.is_new;
            p.commit(d, is_new, h);
        }
    } else if cancel {
        p.editor = None;
    } else if delete {
        let d = p.editor.as_ref().map(|e| e.draft.clone());
        if let Some(d) = d {
            if running {
                p.confirm = Some(Confirm {
                    title: format!("Remove \"{}\"?", d.label),
                    lines: vec!["Its session will be closed, which stops what it is running.".into()],
                    label: "Remove".into(),
                    icon: "trash-2",
                    danger: true,
                    then: After::Remove(d),
                });
            } else {
                p.remove(&d, h);
                p.editor = None;
            }
        }
    }
}

/// Why the switch is off stays under it: the state of this computer.
fn no_terminal_hint(blocked: bool, reason: &str) -> String {
    if blocked {
        reason.to_string()
    } else {
        String::new()
    }
}

fn new_connection(ui: &mut egui::Ui, f: &mut Form, c: &mut super::model::NewConn) {
    // a glass inset, 6 px under the select, 12 px inside, 12 px between fields
    f.skip(6.0);
    let slot = ui.painter().add(egui::Shape::Noop);
    let top = f.y;
    let mut inner = Form::new(f.x + 12.0, top + 12.0, f.w - 24.0);
    let host_hint = derived_name(c);
    let name_hint = if host_hint.is_empty() {
        "Optional. Left empty, it is saved as user@host.".to_string()
    } else if c.name.trim().is_empty() {
        format!("Optional. Left empty, it is saved as {host_hint}.")
    } else {
        String::new()
    };
    form::input(ui, &mut inner, "Name", "The name this connection is saved under. It joins the SSH connections in Settings, so the next button can simply pick it.", &name_hint, &mut c.name, "CORVUS-01", false, false, "schw-nc-name");
    inner.skip(12.0);
    form::input(
        ui,
        &mut inner,
        "Host",
        "",
        "",
        &mut c.host,
        "192.168.2.10",
        true,
        false,
        "schw-nc-host",
    );
    inner.skip(12.0);
    form::input(
        ui,
        &mut inner,
        "Port",
        "",
        "",
        &mut c.port,
        "",
        true,
        false,
        "schw-nc-port",
    );
    inner.skip(12.0);
    form::input(
        ui,
        &mut inner,
        "User",
        "",
        "",
        &mut c.username,
        "corvus",
        false,
        false,
        "schw-nc-user",
    );
    inner.skip(12.0);
    form::input(
        ui,
        &mut inner,
        "Password",
        "",
        "",
        &mut c.password,
        "optional, or use a key file",
        true,
        true,
        "schw-nc-pass",
    );
    inner.skip(12.0);
    form::path(
        ui,
        &mut inner,
        "Key file",
        "",
        "",
        &crate::ui::pathfield::PathField::new(crate::ui::pathfield::Mode::File(None), "Private key")
            .placeholder("No key file chosen")
            .optional()
            .backend()
            .opens_in("~/.ssh"),
        &mut c.key_path,
        "schw-nc-key",
    );
    let rect = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, inner.y + 12.0));
    ui.painter().set(
        slot,
        egui::Shape::Vec(vec![
            egui::Shape::rect_filled(rect, style::RADIUS, style::glass(0.96)),
            egui::Shape::rect_stroke(
                rect,
                style::RADIUS,
                Stroke::new(1.0, style::tint(0.06)),
                egui::StrokeKind::Inside,
            ),
        ]),
    );
    f.y = rect.bottom();
}

/// `.schw-preview`: what the button will run, in an inset box.
fn preview_box(ui: &mut egui::Ui, f: &mut Form, line: &str) {
    let (text, color, mono) = if line.is_empty() {
        ("Name a program to see the command.", style::text_3(), false)
    } else {
        (line, style::text_2(), true)
    };
    let g = if mono {
        let mut job = egui::text::LayoutJob::simple(text.to_string(), style::mono(12.0), color, f.w - 26.0);
        job.wrap.break_anywhere = true;
        ui.painter().layout_job(job)
    } else {
        kit::wrapped_weight(ui, text, 12.0, Weight::W400, color, f.w - 26.0, 1.5)
    };
    let r = f.rect(g.size().y + 22.0);
    ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
    ui.painter().rect_stroke(
        r,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::border_soft()),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(Pos2::new(r.left() + 13.0, r.top() + 11.0), g, color);
    f.y = r.bottom();
}

/// The size of one chip: nine in a row sharing the width, 24 px at most.
fn color_chip_size(w: f32) -> f32 {
    24.0f32.min((w - 8.0 * 4.0) / 9.0)
}

/// `.schw-colors`: the theme's own first, then the presets; round, the chosen
/// one ringed. Returns the colour picked.
fn color_strip(ui: &mut egui::Ui, x: f32, y: f32, w: f32, current: &str) -> Option<String> {
    let s = color_chip_size(w);
    let step = s + (w - 9.0 * s) / 8.0;
    let mut picked = None;
    let items: Vec<&str> = std::iter::once("").chain(BUTTON_COLORS.iter().copied()).collect();
    for (i, value) in items.iter().enumerate() {
        let c = Pos2::new(x + s / 2.0 + i as f32 * step, y + s / 2.0);
        let r = Rect::from_center_size(c, Vec2::splat(s));
        let resp = ui
            .interact(r, ui.scope_id().with(("schw-chip", i)), egui::Sense::click())
            .on_hover_text(if value.is_empty() {
                "Default, the theme colour"
            } else {
                value
            });
        let selected = *value == current;
        let scale = if resp.hovered() { 1.1 } else { 1.0 };
        let radius = s / 2.0 * scale;
        let fill = if value.is_empty() {
            style::accent()
        } else {
            hex_color(value)
        };
        let p = ui.painter();
        if selected {
            p.circle_stroke(c, radius + 1.0, Stroke::new(2.0, style::accent_bright()));
        } else {
            p.circle_stroke(c, radius + 0.5, Stroke::new(1.0, style::tint(0.12)));
        }
        p.circle_filled(c, radius, fill);
        p.circle_stroke(c, radius - 1.0, Stroke::new(2.0, Color32::from_white_alpha(235)));
        if resp.clicked() {
            picked = Some((*value).to_string());
        }
    }
    picked
}

// ---- dialogs ---------------------------------------------------------------------

/// The questions, the companion's popup and the power menu.
pub fn dialogs(p: &mut Schwalby, ctx: &egui::Context, h: &mut Host) {
    confirm(p, ctx, h);
    companion_dialog(p, ctx, h);
    power_menu(p, ctx);
}

fn confirm(p: &mut Schwalby, ctx: &egui::Context, h: &mut Host) {
    let Some(c) = &p.confirm else {
        return;
    };
    let mut yes = false;
    let mut no = false;
    let title = c.title.clone();
    let lines = c.lines.clone();
    let (label, icon, danger) = (c.label.clone(), c.icon, c.danger);
    let closed = modal::show(ctx, "schw-confirm", modal::Size::Sm, &title, |ui, x, y, w| {
        let mut y = y;
        for (i, l) in lines.iter().enumerate() {
            let (weight, color) = if i == 0 {
                (Weight::W600, style::text_1())
            } else {
                (Weight::W400, style::text_2())
            };
            let g = kit::wrapped_weight(ui, l, 13.0, weight, color, w, 1.5);
            ui.painter().galley(Pos2::new(x, y), g.clone(), color);
            y += g.size().y + 8.0;
        }
        let (hit, bottom) = modal::actions(
            ui,
            x,
            y + 8.0,
            w,
            vec![
                kit::Button::new("Cancel", Variant::Secondary),
                kit::Button::new(&label, if danger { Variant::Danger } else { Variant::Primary }).icon(icon),
            ],
        );
        match hit {
            Some(0) => no = true,
            Some(1) => yes = true,
            _ => {}
        }
        bottom
    });
    if yes || no || closed {
        if let Some(c) = p.confirm.take() {
            p.answer(c, yes, h);
        }
    }
}

fn companion_dialog(p: &mut Schwalby, ctx: &egui::Context, h: &mut Host) {
    if p.dialog.is_none() {
        return;
    }
    let connections = p.connections.clone();
    let has_companion = p.companion.is_some();
    let mut save = false;
    let mut clear = false;
    let mut exit = false;
    let mut d = p.dialog.take().expect("checked above");
    let problem = d.problem(&connections);
    let closed = modal::show(
        ctx,
        "schw-companion",
        modal::Size::Sm,
        "Companion computer",
        |ui, x, y, w| {
            let mut f = Form::new(x, y, w);
            let segs = [
                Segment::new("Ping").icon("activity"),
                Segment::new("SSH").icon("server"),
            ];
            let mut pressed = None;
            {
                let (r, hit) = segmented::show(
                    ui,
                    Pos2::new(f.x, f.y),
                    &segs,
                    usize::from(d.ssh),
                    "schw-comp-kind",
                    segmented::Opts::stretched(f.w),
                );
                pressed = hit.or(pressed);
                f.y = r.bottom();
            }
            if let Some(i) = pressed {
                d.ssh = i == 1;
            }
            if !d.ssh {
                f.skip(FIELD_GAP);
                form::input(
                    ui,
                    &mut f,
                    "Name",
                    "",
                    "Optional. Shown at the top of Schwalby.",
                    &mut d.name,
                    "Companion computer",
                    false,
                    false,
                    "schw-comp-name",
                );
                f.skip(FIELD_GAP);
                let host_hint = if d.host.trim().is_empty() {
                    String::new()
                } else {
                    model::host_error(&d.host).to_string()
                };
                form::input(
                    ui,
                    &mut f,
                    "IP address",
                    "",
                    &host_hint,
                    &mut d.host,
                    "192.168.2.10",
                    true,
                    false,
                    "schw-comp-host",
                );
            } else {
                f.skip(FIELD_GAP);
                let mut options: Vec<(String, String)> =
                    connection_options(&connections, &d.choice).into_iter().collect();
                options.retain(|_| true);
                let choices: Vec<Choice> = options
                    .iter()
                    .map(|(v, l)| Choice::new(v.as_str(), l.as_str()))
                    .collect();
                let selected = d.choice.clone();
                form::field(
                ui, &mut f, "SSH connection",
                "Pick a saved SSH connection to ping its host and get an arrow beside the status that opens a terminal on it, or type a new one in. The password stays with the backend.",
                "",
                |ui, x, y, w| {
                    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, form::CONTROL_H));
                    if let Some(v) = kit::select(ui, r, &choices, &selected, "", false, "", "schw-comp-conn") {
                        d.choice = v;
                    }
                    form::CONTROL_H
                },
            );
                if d.picked_new() {
                    new_connection(ui, &mut f, &mut d.conn);
                }
            }
            if let Some((m, k)) = &d.message {
                f.skip(FIELD_GAP);
                form::message(ui, &mut f, m, tone(k));
            }
            f.skip(FIELD_GAP);
            let steps = |list: &[u32]| list.iter().map(|n| format!("{n} s")).collect::<Vec<_>>();
            let isteps = steps(&model::INTERVAL_STEPS);
            let tsteps = steps(&model::TIMEOUT_STEPS);
            let icur = model::INTERVAL_STEPS.iter().position(|s| *s == d.interval).unwrap_or(1);
            let tcur = model::TIMEOUT_STEPS.iter().position(|s| *s == d.timeout).unwrap_or(2);
            form::ruled(ui, &mut f);
            form::field(
                ui,
                &mut f,
                "Ping every",
                "How often this computer sends the companion one ping.",
                "",
                |ui, x, y, w| {
                    let (bottom, out) =
                        crate::ui::slider::show(ui, x + 8.0, y + 2.0, w - 16.0, "schw-interval", &isteps, icur);
                    if let Some((i, _)) = out {
                        d.interval = model::INTERVAL_STEPS[i];
                        if d.timeout <= d.interval {
                            d.timeout = model::timeout_for(d.interval);
                        }
                    }
                    bottom - y
                },
            );
            f.skip(FIELD_GAP);
            form::field(
            ui, &mut f, "Offline after",
            "How long without a single reply before the companion is shown as offline. Always longer than the ping interval, so one late reply does not count as a lost companion.",
            "",
            |ui, x, y, w| {
                let (bottom, out) = crate::ui::slider::show(ui, x + 8.0, y + 2.0, w - 16.0, "schw-timeout", &tsteps, tcur);
                if let Some((i, _)) = out {
                    d.timeout = model::TIMEOUT_STEPS[i];
                    if d.interval >= d.timeout {
                        d.interval = model::interval_for(d.timeout);
                    }
                }
                bottom - y
            },
        );
            let (hit, bottom) = modal::actions(
                ui,
                x,
                f.y + 16.0,
                w,
                vec![
                    kit::Button::new("Save", Variant::Primary)
                        .icon("check")
                        .enabled(problem.is_empty() && d.saving.is_none())
                        .title(&problem),
                    kit::Button::new("Clear", Variant::Secondary)
                        .icon("eraser")
                        .enabled(has_companion)
                        .title("Forget the companion computer and stop pinging it"),
                    kit::Button::new("Exit", Variant::Ghost),
                ],
            );
            match hit {
                Some(0) => save = true,
                Some(1) => clear = true,
                Some(_) => exit = true,
                None => {}
            }
            bottom
        },
    );
    p.dialog = Some(d);
    if save {
        p.save_dialog(h);
    } else if clear {
        p.clear_companion(h);
    } else if exit || closed {
        p.dialog = None;
    }
}

/// The right click on the companion: reboot or shut it down.
fn power_menu(p: &mut Schwalby, ctx: &egui::Context) {
    if !p.power_menu {
        return;
    }
    let Some(c) = p.companion.clone().filter(|c| !c.connection.is_empty()) else {
        p.power_menu = false;
        return;
    };
    let mut items = vec![menu::Item::Head(c.display_name().to_string())];
    items.extend(
        POWER_ACTIONS
            .iter()
            .map(|a| menu::Row::new(a.id, a.label).icon(a.icon).note(a.note).into()),
    );
    let id = egui::Id::NULL.with("schw-power-menu");
    let out = menu::Menu::new(id, menu::Anchor::below(p.strip), &menu::Look::ACTIONS, &items).show(ctx);
    if let Some(a) = out
        .picked
        .and_then(|v| POWER_ACTIONS.iter().find(|a| a.id == v).copied())
    {
        p.power_menu = false;
        p.power_asked(a);
    } else if out.dismissed {
        p.power_menu = false;
    }
}

#[allow(dead_code)]
fn _editor_type(_: &Editor) {}
