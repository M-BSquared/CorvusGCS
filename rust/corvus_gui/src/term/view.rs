//! The terminal as a widget: the screen of an `SshSession` drawn in a
//! rectangle, with the keyboard, the mouse wheel, selection and paste
//! (`.ssh-term`, xterm.js in `src/js/ssh-term.js`).

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::term::emu::{Cell, Colour, Screen};
use crate::term::session::{key_bytes, SshSession};
use crate::ui::style;

pub const FONT: f32 = 12.0;
/// `lineHeight: 1.2` of a 12 px mono font as the web measures it.
pub const CELL_H: f32 = 19.0;

/// What the view remembers between frames.
#[derive(Default)]
pub struct TermState {
    scroll: usize,
    selection: Option<((usize, usize), (usize, usize))>,
}

/// `--ssh-bg` of the theme in force.
pub fn background() -> Color32 {
    if style::is_dark() {
        style::rgba(0x090C0F, 1.0)
    } else {
        style::rgba(0xF7F8FA, 1.0)
    }
}

/// `themeFromTokens`: the 16 colours, then the 6 by 6 by 6 cube and the greys.
pub fn ansi(i: u8) -> Color32 {
    let fg = style::terminal_text();
    match i {
        0 => style::surface_2(),
        1 | 9 => style::critical(),
        2 | 10 => style::healthy(),
        3 | 11 => style::warning(),
        4 | 12 => style::nav(),
        5 => style::rgba(0xC48BE0, 1.0),
        6 => style::accent(),
        7 => fg,
        8 => style::text_3(),
        13 => style::rgba(0xDCA8F0, 1.0),
        14 => style::rgba(0x7BE0C0, 1.0),
        15 => style::text_1(),
        16..=231 => {
            let n = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

fn resolve(c: Colour, default: Color32) -> Color32 {
    match c {
        Colour::Default => default,
        Colour::Index(i) => ansi(i),
        Colour::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

fn colours(cell: &Cell, bg_default: Color32) -> (Color32, Color32) {
    let fg = resolve(cell.fg, style::terminal_text());
    let bg = resolve(cell.bg, bg_default);
    let (mut fg, bg) = if cell.inverse { (bg, fg) } else { (fg, bg) };
    if cell.dim {
        fg = fg.gamma_multiply(0.6);
    }
    (fg, bg)
}

/// The width of one character cell.
pub fn cell_width(ui: &egui::Ui) -> f32 {
    ui.painter()
        .layout_no_wrap("W".repeat(20), style::mono(FONT), Color32::WHITE)
        .size()
        .x
        / 20.0
}

/// The text between two cells, lines joined with newlines.
fn selected_text(screen: &Screen, a: (usize, usize), b: (usize, usize)) -> String {
    let (s, e) = if a <= b { (a, b) } else { (b, a) };
    let sb = screen.scrollback.len();
    let mut out = Vec::new();
    for abs in s.0..=e.0 {
        let line = if abs < sb {
            &screen.scrollback[abs]
        } else {
            let y = (abs - sb).min(screen.rows - 1);
            screen.line_at(0, y)
        };
        let from = if abs == s.0 { s.1 } else { 0 };
        let to = if abs == e.0 { e.1 + 1 } else { line.len() };
        let seg: String = line
            .iter()
            .skip(from)
            .take(to.saturating_sub(from))
            .map(|c| c.ch)
            .collect();
        out.push(if abs == e.0 && to < line.len() {
            seg
        } else {
            seg.trim_end().to_string()
        });
    }
    out.join("\n")
}

/// Draws the terminal in `rect` (the box with its padding) and handles its
/// input. `frame` fills and strokes the box when true.
pub fn show(ui: &mut egui::Ui, rect: Rect, session: &mut SshSession, st: &mut TermState, id: &str, frame: bool) {
    let painter = ui.painter().clone();
    let bg_default = background();
    if frame {
        painter.rect_filled(rect, style::RADIUS_BTN, bg_default);
        painter.rect_stroke(
            rect,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::border()),
            egui::StrokeKind::Inside,
        );
    }
    let (pt, pr, pb, pl) = (8.0, 4.0, 8.0, 10.0);
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + pl, rect.top() + pt),
        Pos2::new(rect.right() - pr, rect.bottom() - pb),
    );
    let cw = cell_width(ui);
    let cols = ((inner.width() / cw).floor() as usize).max(1);
    let rows = ((inner.height() / CELL_H).floor() as usize).max(1);
    session.resize(cols, rows);

    let rid = ui.scope_id().with(("term", id));
    let resp = ui.interact(rect, rid, egui::Sense::click_and_drag());
    if resp.clicked() || resp.drag_started() {
        ui.memory_mut(|m| m.request_focus(rid));
    }
    let focused = ui.memory(|m| m.has_focus(rid));
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                rid,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }

    let handle = session.emu.clone();
    let guard = match handle.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let screen = &guard.screen;
    st.scroll = st.scroll.min(screen.scrollback.len());
    let (app_cursor, bracketed, alt) = (screen.app_cursor, screen.bracketed_paste, screen.on_alt_screen());

    if resp.hovered() && !alt {
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            let lines = (wheel / CELL_H).round() as i64;
            st.scroll = (st.scroll as i64 + lines).clamp(0, screen.scrollback.len() as i64) as usize;
        }
    }

    let cell_at = |p: Pos2| -> (usize, usize) {
        let x = (((p.x - inner.left()) / cw).floor().max(0.0) as usize).min(cols - 1);
        let y = (((p.y - inner.top()) / CELL_H).floor().max(0.0) as usize).min(rows - 1);
        let abs = screen.scrollback.len() - st.scroll.min(screen.scrollback.len()) + y;
        (abs, x)
    };
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.drag_started() {
            let c = cell_at(p);
            st.selection = Some((c, c));
        } else if resp.dragged() {
            if let Some((a, _)) = st.selection {
                st.selection = Some((a, cell_at(p)));
            }
        } else if resp.clicked() {
            st.selection = None;
        }
    }

    let clip = ui.painter_at(rect);
    let sel = st.selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let sb = screen.scrollback.len();
    let top_abs = sb - st.scroll.min(sb);
    for y in 0..rows.min(screen.rows) {
        let line = screen.line_at(st.scroll, y);
        let ly = inner.top() + y as f32 * CELL_H;
        let abs = top_abs + y;
        let mut x = 0;
        while x < line.len().min(cols) {
            let cell = &line[x];
            let selected = sel.is_some_and(|(s, e)| (abs, x) >= s && (abs, x) <= e);
            let key = |c: &Cell| (c.fg, c.bg, c.bold, c.dim, c.inverse, c.underline);
            let mut end = x + 1;
            while end < line.len().min(cols) {
                let s2 = sel.is_some_and(|(s, e)| (abs, end) >= s && (abs, end) <= e);
                if key(&line[end]) == key(cell) && s2 == selected {
                    end += 1;
                } else {
                    break;
                }
            }
            let (mut fg, mut bg) = colours(cell, bg_default);
            if selected {
                bg = style::border();
                fg = style::terminal_text();
            }
            let run_rect = Rect::from_min_size(
                Pos2::new(inner.left() + x as f32 * cw, ly),
                Vec2::new((end - x) as f32 * cw, CELL_H),
            );
            if bg != bg_default && (cell.bg != Colour::Default || cell.inverse || selected) {
                clip.rect_filled(run_rect, 0, bg);
            }
            let text: String = line[x..end].iter().map(|c| c.ch).collect();
            if text.trim().is_empty() && !cell.underline {
                x = end;
                continue;
            }
            let g = painter.layout_no_wrap(text, style::mono(FONT), fg);
            let ty = ly + (CELL_H - g.size().y) / 2.0;
            clip.galley(Pos2::new(run_rect.left(), ty), g.clone(), fg);
            if cell.bold {
                clip.galley(Pos2::new(run_rect.left() + 0.5, ty), g, fg);
            }
            if cell.underline {
                clip.hline(run_rect.x_range(), ly + CELL_H - 2.0, Stroke::new(1.0, fg));
            }
            x = end;
        }
    }
    if screen.cursor_visible && st.scroll == 0 && screen.y < rows && screen.x < cols {
        let cr = Rect::from_min_size(
            Pos2::new(
                inner.left() + screen.x as f32 * cw,
                inner.top() + screen.y as f32 * CELL_H,
            ),
            Vec2::new(cw, CELL_H),
        );
        if focused {
            clip.rect_filled(cr, 0, style::accent());
            let ch = screen.cell(screen.x, screen.y).ch;
            if ch != ' ' {
                let g = painter.layout_no_wrap(ch.to_string(), style::mono(FONT), bg_default);
                clip.galley(Pos2::new(cr.left(), cr.center().y - g.size().y / 2.0), g, bg_default);
            }
        } else {
            clip.rect_stroke(
                cr.shrink(0.5),
                0,
                Stroke::new(1.0, style::accent()),
                egui::StrokeKind::Inside,
            );
        }
    }
    let copy_text = st.selection.map(|(a, b)| selected_text(screen, a, b));
    drop(guard);

    if focused {
        let mut out = String::new();
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::Text(t) => out.push_str(&t),
                egui::Event::Paste(t) => {
                    if bracketed {
                        out.push_str(&format!("\x1b[200~{t}\x1b[201~"));
                    } else {
                        out.push_str(&t.replace("\r\n", "\r").replace('\n', "\r"));
                    }
                }
                egui::Event::Copy => {
                    if let Some(t) = &copy_text {
                        ui.ctx().copy_text(t.clone());
                    } else {
                        out.push('\x03');
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(b) = key_bytes(key, modifiers, app_cursor) {
                        out.push_str(&b);
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            st.scroll = 0;
            st.selection = None;
            session.send(out);
        }
    }
}
