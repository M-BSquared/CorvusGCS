//! The shelf and the editor drawn (`.sshl-*` of `ssh-launcher.css`).

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use super::model::{
    connection_options, derived_name, missing_connections, new_connection_error, preview_line, session_name, Button,
    Mode, NewConn, MAX_BUTTONS, NEW_CONNECTION,
};
use super::{Editor, SshLauncher};
use crate::plugins::Host;
use crate::ui::form::{self, Form};
use crate::ui::kit::{self, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::popover;
use crate::ui::style::{self, Weight};
use crate::ui::{icons, modal};

const FIELD_GAP: f32 = 16.0;

pub fn show(p: &mut SshLauncher, ui: &mut egui::Ui, area: Rect, h: &mut Host) -> f32 {
    let mut f = Form::new(area.left(), area.top(), area.width());
    if let Some((text, kind)) = p.status.clone() {
        form::message(ui, &mut f, &text, tone(kind));
        f.skip(10.0);
    }
    if p.editor.is_some() {
        editor(p, ui, &mut f, h);
    } else {
        shelf(p, ui, &mut f, h);
    }
    if let Some(out) = p.output.clone() {
        f.skip(12.0);
        let g = kit::wrapped_weight(ui, &out, 12.0, Weight::W400, style::text_2(), f.w - 26.0, 1.5);
        let r = Rect::from_min_size(Pos2::new(f.x, f.y), Vec2::new(f.w, g.size().y.min(190.0) + 22.0));
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
        _ => Tone::Ok,
    }
}

fn shelf(p: &mut SshLauncher, ui: &mut egui::Ui, f: &mut Form, h: &mut Host) {
    let mut act: Option<(usize, &'static str)> = None;
    let mut add = false;
    form::card(ui, f, "", |ui, f| {
        if p.buttons.is_empty() {
            form::desc(
                ui,
                f,
                "No launch buttons yet. Add one for each program you start before a flight.",
            );
            f.skip(14.0);
        } else {
            let n = p.buttons.len();
            for i in 0..n {
                if let Some(a) = row(p, ui, f, i, i == 0, i + 1 == n) {
                    act = Some((i, a));
                }
            }
            f.skip(14.0);
        }
        let b = kit::Button::new("Add button", Variant::Secondary)
            .icon("plus")
            .enabled(p.buttons.len() < MAX_BUTTONS);
        let w = b.width(ui);
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(f.x, f.y), Vec2::new(w, 38.0)),
            "sshl-add",
        ) {
            add = true;
        }
        f.y += 38.0;
        let missing = if p.conn_loaded {
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
        } else {
            p.conn_error.clone()
        };
        if !note.is_empty() {
            f.skip(10.0);
            form::desc(ui, f, &note);
        }
    });
    if add {
        let last = p.buttons.last();
        let connection = last
            .map(|b| b.connection.clone())
            .filter(|c| !c.is_empty())
            .or_else(|| {
                p.connections
                    .first()
                    .and_then(|c| c.get("name").and_then(Value::as_str))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let draft = Button::blank(&connection, last.map_or("", |b| b.directory.as_str()));
        open_editor(p, draft, true);
    }
    if let Some((i, a)) = act {
        let b = p.buttons[i].clone();
        match a {
            "launch" => p.launch(&b, h),
            "terminal" => p.show_terminal(&b, h),
            "connect" => p.connect_and_open(&b, h),
            "stop" => p.stop(&b, h),
            _ => open_editor(p, b, false),
        }
    }
}

fn open_editor(p: &mut SshLauncher, draft: Button, is_new: bool) {
    let choice = if !p.connections.is_empty() || !draft.connection.is_empty() {
        draft.connection.clone()
    } else {
        NEW_CONNECTION.to_string()
    };
    p.status = None;
    p.editor = Some(Editor {
        draft,
        is_new,
        choice,
        conn: NewConn::blank(),
        saving: None,
        message: None,
    });
}

/// One shelf row: the launch button and its tools. Returns what was pressed.
fn row(p: &SshLauncher, ui: &mut egui::Ui, f: &mut Form, i: usize, first: bool, last: bool) -> Option<&'static str> {
    let b = &p.buttons[i];
    let session = session_name(&b.id);
    let running = p.live.contains(&session);
    let stopping = p.closing(&session);
    let failure = p.errors.get(&b.id);
    let top = f.y + if first { 0.0 } else { 7.0 };
    let launch = kit::Button::new("", Variant::Primary).icon("play").small();
    let h = launch.height().max(28.0);
    let row_rect = Rect::from_min_size(Pos2::new(f.x, top - 7.0), Vec2::new(f.w, h + 14.0));
    let hover = ui.rect_contains_pointer(row_rect);
    let strong = hover || running || failure.is_some();
    let mut hit = None;
    // tools, from the right: stop or pencil, the arrow, the dot, the warning
    let mut x = f.x + f.w;
    ui.set_opacity(if strong { 1.0 } else { 0.55 });
    x -= 28.0;
    let last_tool = Rect::from_min_size(Pos2::new(x, top + (h - 28.0) / 2.0), Vec2::splat(28.0));
    if running || stopping {
        let label = if stopping {
            "Stopping"
        } else {
            "Stop the program with Ctrl-C and close its terminal connection"
        };
        if tool(ui, last_tool, "square", label, style::critical(), ("sshl-stop", &b.id)) && !stopping {
            hit = Some("stop");
        }
    } else if tool(ui, last_tool, "pencil", "Edit", style::text_3(), ("sshl-edit", &b.id)) {
        hit = Some("edit");
    }
    if b.mode != Mode::Background {
        x -= 30.0;
        let r = Rect::from_min_size(Pos2::new(x, top + (h - 28.0) / 2.0), Vec2::splat(28.0));
        let (title, color) = if running {
            (
                "Open its terminal window to watch it, or press Ctrl-C to stop it",
                style::healthy(),
            )
        } else {
            (
                "Open its terminal and connect. The program starts with the button.",
                style::text_3(),
            )
        };
        if tool(ui, r, "chevron-right", title, color, ("sshl-open", &b.id)) {
            hit = Some(if running { "terminal" } else { "connect" });
        }
    }
    if running {
        x -= 2.0 + 7.0 + 8.0;
        let c = Pos2::new(x + 11.5, top + h / 2.0);
        ui.painter()
            .circle_filled(c, 5.5, style::healthy().gamma_multiply(0.25));
        ui.painter().circle_filled(c, 3.5, style::healthy());
    }
    if let Some(why) = failure {
        x -= 30.0;
        let r = Rect::from_min_size(Pos2::new(x, top + (h - 28.0) / 2.0), Vec2::splat(28.0));
        let id = ui.scope_id().with(("sshl-error", &b.id));
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
    }
    ui.set_opacity(1.0);
    // the launch button takes the rest of the row, its label cut with an ellipsis
    let avail = (x - 8.0 - f.x).max(40.0);
    let label = fit_label(ui, &b.label, avail - 2.0 - 22.0 - 13.0 - 7.0);
    let title = if running {
        format!(
            "{} in the terminal it is already in.\n{}",
            if super::model::restarts(b) {
                "Stop it with Ctrl-C and start it again"
            } else {
                "Run it again"
            },
            preview_of(b)
        )
    } else {
        preview_of(b)
    };
    let btn = kit::Button::new(&label, Variant::Primary)
        .icon("play")
        .small()
        .align_start()
        .title(&title);
    let busy = p
        .tasks
        .iter()
        .any(|t| t.button.id == b.id && matches!(t.kind, super::Kind::Launch | super::Kind::Detach));
    let btn = btn.enabled(!busy);
    let rect = Rect::from_min_size(
        Pos2::new(f.x, top + (h - launch.height()) / 2.0),
        Vec2::new(avail, launch.height()),
    );
    paint_left(ui, rect, btn, ("sshl-launch", &b.id), &mut hit);
    let bottom = top + h + if last { 0.0 } else { 7.0 };
    if !last {
        ui.painter().hline(
            egui::Rangef::new(f.x, f.x + f.w),
            bottom + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        f.y = bottom + 1.0;
    } else {
        f.y = bottom;
    }
    hit
}

/// A button whose label sits at the left (`.sshl-launch`), as the flexible item of its row.
fn paint_left(
    ui: &mut egui::Ui,
    rect: Rect,
    b: kit::Button<'_>,
    id: impl std::hash::Hash + std::fmt::Debug,
    hit: &mut Option<&'static str>,
) {
    // The shared button centres its content; draw it at its natural width at
    // the left and stretch only the surface.
    let natural = b.width(ui).min(rect.width());
    let _ = natural;
    if b.show(ui, rect, id) {
        *hit = Some("launch");
    }
}

fn preview_of(b: &Button) -> String {
    let line = preview_line(&b.connection, &b.directory, &b.command, b.mode);
    if line.is_empty() {
        b.command.clone()
    } else {
        line
    }
}

/// `text-overflow: ellipsis` for `width` points of 11 px type.
pub fn fit_label(ui: &egui::Ui, text: &str, width: f32) -> String {
    let w = |s: &str| style::galley(ui, s, 11.0, Weight::W600, Color32::WHITE, 0.1).size().x;
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

fn editor(p: &mut SshLauncher, ui: &mut egui::Ui, f: &mut Form, h: &mut Host) {
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
    let Some(e) = p.editor.as_mut() else {
        return;
    };
    let picked_new = e.choice == NEW_CONNECTION;
    let target = if picked_new {
        let n = e.conn.name.trim();
        if n.is_empty() {
            derived_name(&e.conn)
        } else {
            n.to_string()
        }
    } else {
        e.choice.clone()
    };
    let line = preview_line(&target, &e.draft.directory, &e.draft.command, e.draft.mode);
    let conn_error = if picked_new {
        new_connection_error(&e.conn, &taken)
    } else if target.is_empty() {
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
            "sshl-label",
        );
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
                if let Some(v) = kit::select(ui, r, &choices, &selected, "", false, "", "sshl-conn") {
                    e.choice = v;
                }
                form::CONTROL_H
            },
        );
        if e.choice == NEW_CONNECTION {
            f.skip(-10.0 + 10.0);
            new_connection(ui, f, &mut e.conn);
        }
        f.skip(FIELD_GAP);
        form::input(
            ui,
            f,
            "Folder",
            "",
            "Entered before the program starts. Leave empty to run in the account's home directory.",
            &mut e.draft.directory,
            "/home/pilot/mission",
            true,
            false,
            "sshl-dir",
        );
        f.skip(FIELD_GAP);
        form::input(
            ui, f, "Program",
            "Run by the remote login shell, so a pipeline or arguments work as typed. It runs as the connection's own user and can do whatever that account can do.",
            "", &mut e.draft.command, "./start.sh", true, false, "sshl-cmd",
        );
        f.skip(FIELD_GAP);
        let terminal = e.draft.mode != Mode::Background;
        if let Some(on) = form::switch_field(
            ui, f, "sshl-terminal",
            form::Switch::new("Run in a terminal", "", terminal).info(
                "On: the program runs in an SSH session of this button's own, and pressing the button again runs it again in that same terminal. The arrow beside the button opens the window to watch it in. Read its output there, and stop it with Ctrl-C, the window's disconnect button, or the stop button that takes the place of the pencil while it runs. The session lasts as long as Corvus does. Off: the program is started with nohup and detached, so it survives Corvus closing, but there is nothing to watch and nothing to stop from here.",
            ),
            style::text_1(), 12.0,
        ) {
            e.draft.mode = if on { Mode::Terminal } else { Mode::Background };
        }
        if e.draft.mode != Mode::Background {
            f.skip(FIELD_GAP);
            if let Some(on) = form::switch_field(
                ui, f, "sshl-restart",
                form::Switch::new("Restart on press", "", e.draft.restart).info(
                    "On: pressing the button while its program is still running first sends Ctrl-C to stop it, waits a second, and then starts it again in the same terminal. Off: the line is typed into the terminal as it is, so a program still in the foreground gets it as input.",
                ),
                style::text_1(), 12.0,
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
                .enabled(can_save)];
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
            let body = super::model::new_connection_body(&e.conn);
            e.saving = Some(h.post_any("/api/ssh/connections", body));
        } else {
            let mut d = e.draft.clone();
            d.connection = e.choice.clone();
            let is_new = e.is_new;
            p.commit(d, is_new, h);
        }
    } else if cancel {
        p.editor = None;
    } else if delete {
        let d = p.editor.as_ref().map(|e| e.draft.clone());
        if let Some(d) = d {
            if running {
                p.confirm_delete = Some(d);
            } else {
                p.remove(&d, h);
                p.editor = None;
            }
        }
    }
}

fn new_connection(ui: &mut egui::Ui, f: &mut Form, c: &mut NewConn) {
    // a glass inset, 6 px under the select, 12 px inside, 12 px between fields
    f.skip(6.0);
    let slot = ui.painter().add(egui::Shape::Noop);
    let top = f.y;
    let mut inner = Form::new(f.x + 12.0, top + 12.0, f.w - 24.0);
    let placeholder = derived_name(c);
    let name_hint = if placeholder.is_empty() {
        "pilot@10.0.0.7".to_string()
    } else {
        placeholder
    };
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
        "sshl-nc-host",
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
        "sshl-nc-port",
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
        "sshl-nc-user",
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
        "sshl-nc-pass",
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
        "sshl-nc-key",
    );
    inner.skip(12.0);
    form::input(
        ui, &mut inner, "Save as",
        "The name this connection is saved under. It joins the SSH connections in Settings, so the next button can simply pick it.",
        "Leave it empty to use user@host.", &mut c.name, &name_hint, false, false, "sshl-nc-name",
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

/// `.sshl-preview`: what the button will run, in an inset box.
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

/// The confirmation before removing a button whose program runs.
pub fn confirm(p: &mut SshLauncher, ctx: &egui::Context, h: &mut Host) {
    let Some(d) = p.confirm_delete.clone() else {
        return;
    };
    let mut yes = false;
    let mut no = false;
    let text = format!(
        "Remove \"{}\"?\n\nIts session will be closed, which stops what it is running.",
        d.label
    );
    let closed = modal::show(ctx, "sshl-remove", modal::Size::Sm, "Remove button", |ui, x, y, w| {
        let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_2(), w, 1.5);
        ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
        let (hit, bottom) = modal::actions(
            ui,
            x,
            y + g.size().y + 20.0,
            w,
            vec![
                kit::Button::new("CANCEL", Variant::Secondary),
                kit::Button::new("REMOVE", Variant::Danger),
            ],
        );
        match hit {
            Some(0) => no = true,
            Some(1) => yes = true,
            _ => {}
        }
        bottom
    });
    if yes {
        p.remove(&d, h);
        p.editor = None;
        p.confirm_delete = None;
    } else if no || closed {
        p.confirm_delete = None;
    }
}
