//! The CONSOLE tab (index.html `[data-panel="console"]`, python/src/js/panel.js,
//! link.css `.console-*`, panel.css `.con-line`): the toolbar with filter,
//! pause, copy, save and clear; the stream; the command row; the footer.
//!
//! Lines come from the `console` topic of `/api/events` and are kept as
//! panel.js keeps them: 2000 at most, held while paused, each with the time
//! it arrived and a colour by level. A command is normalised as panel.js
//! normalises it, `?` prints the list without a round trip, and the answer
//! is turned into a line the same way.

use crate::ui::tip::HoverTr;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::net::lock::Relock;
use crate::net::stream::Stream;
use crate::ui::card::{self, Look};
use crate::ui::field::{Surface, TextField};
use crate::ui::icons;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

const NEW_LINE: &str = "{a} new line";
const NEW_LINES: &str = "{a} new lines";
const MAX_LINES: usize = 2000;

const COMMANDS: &[(&str, &str, &str, bool)] = &[
    (
        "arm",
        "",
        "Arm the vehicle (refused unless the autopilot is happy)",
        false,
    ),
    ("disarm", "", "Disarm the vehicle", false),
    ("mode", "<MODE>", "Set flight mode, e.g. mode HOLD", false),
    ("takeoff", "[ALT]", "Take off to ALT metres AGL (default 10)", false),
    ("land", "", "Land at the current position", false),
    ("rtl", "", "Return to launch", false),
    (
        "listener",
        "<topic>",
        "Stream a uORB topic, e.g. listener sensor_combined",
        true,
    ),
    ("top", "", "PX4 task/CPU listing", true),
    ("free", "", "Free memory", true),
    ("dmesg", "", "PX4 boot/kernel log", true),
    ("tasks", "", "Running tasks", true),
    ("perf", "", "Performance counters", true),
    ("boot_log", "", "Boot log", true),
    ("hrt", "", "High-resolution timer info", true),
    ("shell", "<cmd>", "Send a raw NSH command (case preserved)", true),
    ("help", "", "Ask the backend what it accepts", false),
];

/// One line of the console.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub ts: String,
    pub level: String,
    pub text: String,
}

/// One record as a transcript line (`formatLine` of `python/src/js/panel.js`):
/// timestamp, level tag, message.
pub fn format_line(l: &Line) -> String {
    let tag = if l.level.is_empty() {
        String::new()
    } else {
        format!("[{}] ", l.level)
    };
    format!("{}  {tag}{}", l.ts, l.text)
}

/// panel.js normalizeCommand: newlines and NULs to spaces, the verb lower
/// case, the rest's runs of space collapsed except for a raw shell line.
pub fn normalize_command(text: &str) -> String {
    let raw: String = text
        .chars()
        .map(|c| if matches!(c, '\r' | '\n' | '\0') { ' ' } else { c })
        .collect();
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let (verb, rest) = match raw.find(char::is_whitespace) {
        Some(i) => (&raw[..i], raw[i..].trim()),
        None => (raw, ""),
    };
    let verb = verb.to_lowercase();
    if rest.is_empty() {
        return verb;
    }
    if verb == "shell" || verb == "nsh" {
        return format!("{verb} {rest}");
    }
    format!("{verb} {}", rest.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The line class a record level draws in (LEVEL_CLASS).
pub fn level_class(level: &str) -> &'static str {
    match level {
        "critical" | "error" => "error",
        "warning" => "warning",
        "success" => "success",
        "info" => "info",
        "cmd" => "cmd",
        "shell" => "shell",
        "nav" => "nav",
        _ => "",
    }
}

fn now_ts() -> String {
    // Local wall clock, HH:MM:SS, as the browser's Date gives it.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let offset = local_offset();
    let t = (secs + offset).rem_euclid(86400);
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

pub use crate::data::clock::local_offset;

#[derive(Default)]
struct Buffer {
    lines: Vec<Line>,
    pending: Vec<Line>,
    paused: bool,
    interrupted: bool,
    /// The command whose answer is awaited, so the answer can name it.
    last_cmd: Option<String>,
    /// Lines ever shown, for the "new lines" count of the jump chip.
    added: u64,
}

impl Buffer {
    fn add(&mut self, level: &str, text: &str) {
        let rec = Line {
            ts: now_ts(),
            level: level.into(),
            text: text.into(),
        };
        if self.paused {
            self.pending.push(rec);
            if self.pending.len() > MAX_LINES {
                self.pending.remove(0);
            }
            return;
        }
        self.added += 1;
        self.lines.push(rec);
        if self.lines.len() > MAX_LINES {
            self.lines.remove(0);
        }
    }
}

/// The tab's state.
pub struct ConsoleTab {
    server: Server,
    buffer: Arc<Mutex<Buffer>>,
    stream: Option<Stream>,
    filter: String,
    input: String,
    sending: Option<Pending<Result<Value, String>>>,
    history: Vec<String>,
    hist_idx: usize,
}

impl ConsoleTab {
    pub fn new(server: Server, ctx: &egui::Context) -> ConsoleTab {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        {
            let mut b = buffer.relock();
            b.add("", "Corvus GCS MAVLink console.");
            b.add("", "Listening for live MAVLink messages \u{2026}");
            b.add("", "Type \"?\" for the command list, Tab to complete.");
        }
        let sink = buffer.clone();
        let repaint = ctx.clone();
        let stream = Stream::start(server.clone(), "/api/events?topics=console".into(), move |ev| {
            let mut b = sink.relock();
            if ev.name == "error" {
                if !b.interrupted {
                    b.interrupted = true;
                    b.add("warning", "Console stream interrupted; reconnecting\u{2026}");
                }
            } else if ev.name == "console" {
                let Ok(entry) = serde_json::from_str::<Value>(&ev.data) else {
                    return;
                };
                if entry.get("name").and_then(Value::as_str) == Some("ping") {
                    return;
                }
                if b.interrupted {
                    b.interrupted = false;
                    b.add("success", "Console stream reconnected.");
                }
                let name = entry.get("name").and_then(Value::as_str).unwrap_or("");
                let level = if name == "SHELL" {
                    "shell"
                } else {
                    entry.get("level").and_then(Value::as_str).unwrap_or("")
                };
                let text = entry
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .unwrap_or(name);
                b.add(level, text);
            }
            drop(b);
            repaint.request_repaint();
        });
        ConsoleTab {
            server,
            buffer,
            stream: Some(stream),
            filter: String::new(),
            input: String::new(),
            sending: None,
            history: Vec::new(),
            hist_idx: 0,
        }
    }

    /// One line from outside the stream (a plugin's `api.console`).
    pub fn add_line(&self, level: &str, text: &str) {
        self.buffer.relock().add(level, text);
    }

    /// Every line of the buffer as a transcript, a filter typed into the tab
    /// notwithstanding (`consoleText({all: true})`).
    pub fn transcript(&self) -> String {
        let b = self.buffer.relock();
        b.lines.iter().map(format_line).collect::<Vec<_>>().join("\n")
    }

    /// Ends the console stream.
    pub fn shut_down(&mut self) {
        if let Some(mut s) = self.stream.take() {
            s.stop();
        }
    }

    fn print_commands(&self) {
        let mut b = self.buffer.relock();
        b.add("info", "Commands:");
        for (name, args, help, _) in COMMANDS {
            let sig = if args.is_empty() {
                (*name).to_string()
            } else {
                format!("{name} {args}")
            };
            b.add("", &format!("  {sig:<18}{help}"));
        }
    }

    fn send(&mut self, ctx: &egui::Context) {
        if self.sending.is_some() {
            return;
        }
        let v = normalize_command(&self.input);
        if v.is_empty() {
            return;
        }
        self.input.clear();
        if v == "?" {
            self.print_commands();
            return;
        }
        self.history.push(v.clone());
        if self.history.len() > 100 {
            self.history.remove(0);
        }
        self.hist_idx = self.history.len();
        let server = self.server.clone();
        let cmd = v.clone();
        self.sending = Some(Pending::spawn(ctx, move || {
            http::post_json(&server, "/api/console/command", &json!({ "command": cmd }))
        }));
        self.buffer.relock().last_cmd = Some(v);
    }

    fn take_answer(&mut self) {
        let Some(r) = self.sending.as_ref().and_then(Pending::take) else {
            return;
        };
        self.sending = None;
        let v = self.buffer.relock().last_cmd.take().unwrap_or_default();
        match r {
            Ok(res) => {
                if res
                    .get("help")
                    .is_some_and(|h| h.as_bool() != Some(false) && !h.is_null())
                {
                    self.print_commands();
                } else if let Some(e) = res.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()) {
                    self.buffer.relock().add("error", e);
                } else if res.get("shell").is_some_and(|s| s.as_bool() == Some(true)) {
                    self.buffer
                        .relock()
                        .add("info", "Shell command sent, output streaming\u{2026}");
                } else if res.get("ok") == Some(&Value::Bool(true)) {
                    self.buffer.relock().add("success", &format!("Command sent: {v}"));
                }
            }
            Err(e) => self.buffer.relock().add("error", &format!("Command failed: {e}")),
        }
    }

    /// Draws the tab into `rect`.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        self.take_answer();
        let painter = ui.painter().clone();
        // .console-toolbar: padding 10 12, a rule, 8 px under it
        let field_h: f32 = 2.0 + 10.0 + 11.5 * 1.21;
        let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 20.0 + field_h.max(26.0) + 1.0));
        painter.hline(
            bar.x_range(),
            bar.bottom() - 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        let tools_w = 4.0 * 26.0 + 3.0 * 2.0;
        let search = Rect::from_min_size(
            Pos2::new(rect.left() + 12.0, bar.center().y - field_h / 2.0),
            Vec2::new(rect.width() - 24.0 - 8.0 - tools_w, field_h),
        );
        self.filter_field(ui, search);
        let paused = self.buffer.lock().map(|b| b.paused).unwrap_or(false);
        let mut tx = search.right() + 8.0;
        let tools: [(&'static str, &str); 4] = [
            (
                if paused { "play" } else { "pause" },
                if paused {
                    "Resume the stream"
                } else {
                    "Pause the stream"
                },
            ),
            ("copy", "Copy visible lines"),
            ("download", "Save log to a file"),
            ("rotate-ccw", "Clear console"),
        ];
        for (i, (icon, title)) in tools.into_iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(tx, bar.center().y - 13.0), Vec2::splat(26.0));
            let resp = ui
                .interact(r, ui.scope_id().with(("ctool", i)), egui::Sense::click())
                .on_hover_tr(title);
            let active = i == 0 && paused;
            let (fill, color) = if active {
                (style::warning_dark(), style::warning())
            } else if resp.hovered() {
                (style::surface_hover(), style::text_1())
            } else {
                (Color32::TRANSPARENT, style::text_3())
            };
            painter.rect_filled(r, 5, fill);
            icons::paint(ui, icon, Rect::from_center_size(r.center(), Vec2::splat(13.0)), color);
            if resp.clicked() {
                self.tool(i, &ctx);
            }
            tx += 28.0;
        }

        // the footer and the command row from the bottom up
        let footer_h = 5.0 + 15.0;
        let row_h = 9.0 * 2.0 + 34.0 + 1.0;
        let footer = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - footer_h), rect.max);
        let row = Rect::from_min_max(
            Pos2::new(rect.left(), footer.top() - row_h),
            Pos2::new(rect.right(), footer.top()),
        );
        let stream = Rect::from_min_max(
            Pos2::new(rect.left() + 12.0, bar.bottom() + 8.0),
            Pos2::new(rect.right() - 12.0, row.top()),
        );
        self.paint_stream(ui, stream);

        painter.rect_filled(row, 0, style::surface_1());
        painter.hline(row.x_range(), row.top() + 0.5, Stroke::new(1.0, style::border_soft()));
        let prompt = style::galley_mono(ui, ">>", 13.0, style::terminal_cmd());
        painter.galley(
            Pos2::new(row.left() + 12.0, row.center().y - prompt.size().y / 2.0),
            prompt.clone(),
            style::terminal_cmd(),
        );
        let send = Rect::from_min_size(
            Pos2::new(row.right() - 12.0 - 34.0, row.center().y - 17.0),
            Vec2::splat(34.0),
        );
        let input = Rect::from_min_max(
            Pos2::new(row.left() + 12.0 + prompt.size().x + 8.0, row.center().y - 16.5),
            Pos2::new(send.left() - 8.0, row.center().y + 16.5),
        );
        let resp = kit::text_field(
            ui,
            input,
            &mut self.input,
            "Command (Tab completes, \u{2191} history, ? for help)",
            true,
            "cinput",
        );
        if resp.has_focus() {
            if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) && self.hist_idx > 0 {
                self.hist_idx -= 1;
                self.input = self.history[self.hist_idx].clone();
            }
            if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) && self.hist_idx < self.history.len() {
                self.hist_idx += 1;
                self.input = self.history.get(self.hist_idx).cloned().unwrap_or_default();
            }
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.send(&ctx);
            resp.request_focus();
        }
        let sresp = ui
            .interact(send, ui.scope_id().with("csend"), egui::Sense::click())
            .on_hover_tr("Send");
        let busy = self.sending.is_some();
        let fill = if sresp.hovered() && !busy {
            style::accent_hover()
        } else {
            style::accent()
        };
        painter.circle_filled(send.center(), 17.0, fill.gamma_multiply(if busy { 0.45 } else { 1.0 }));
        icons::paint(
            ui,
            "send",
            Rect::from_center_size(send.center(), Vec2::splat(15.0)),
            style::text_inverted(),
        );
        if sresp.clicked() && !busy {
            self.send(&ctx);
        }

        let count = self.count_text();
        let paused = self.buffer.lock().map(|b| b.paused).unwrap_or(false);
        let cg = style::galley(
            ui,
            &count,
            10.0,
            if paused { Weight::W600 } else { Weight::W400 },
            if paused { style::warning() } else { style::text_3() },
            0.0,
        );
        painter.galley(Pos2::new(footer.left() + 12.0, footer.top() + 5.0), cg, style::text_3());
    }

    fn filter_field(&mut self, ui: &mut egui::Ui, r: Rect) {
        TextField::new(&mut self.filter)
            .surface(Surface::Glass)
            .hint("Filter")
            .font(11.5, Weight::W400)
            .icon("search", 12.0, 7.0, 6.0)
            .pad(25.0, 8.0)
            .show(ui, r, "cfilter");
    }

    fn visible(&self) -> Vec<Line> {
        let f = self.filter.to_lowercase();
        self.buffer
            .lock()
            .map(|b| {
                b.lines
                    .iter()
                    .filter(|l| f.is_empty() || l.text.to_lowercase().contains(&f))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The footer count: lines (or shown / all under a filter), held, paused.
    pub fn count_text(&self) -> String {
        let b = self.buffer.relock();
        let mut parts = Vec::new();
        if self.filter.is_empty() {
            parts.push(b.lines.len().to_string());
        } else {
            let f = self.filter.to_lowercase();
            let shown = b.lines.iter().filter(|l| l.text.to_lowercase().contains(&f)).count();
            parts.push(format!("{shown} / {}", b.lines.len()));
        }
        if !b.pending.is_empty() {
            parts.push(format!("{} held", b.pending.len()));
        }
        if b.paused {
            parts.push("paused".into());
        }
        parts.join("  \u{00b7}  ")
    }

    fn tool(&mut self, i: usize, ctx: &egui::Context) {
        match i {
            0 => {
                let mut b = self.buffer.relock();
                b.paused = !b.paused;
                if !b.paused {
                    let held = std::mem::take(&mut b.pending);
                    b.lines.extend(held);
                    let over = b.lines.len().saturating_sub(MAX_LINES);
                    b.lines.drain(..over);
                }
            }
            1 => ctx.copy_text(
                self.visible()
                    .iter()
                    .map(|l| format!("{} >> {}", l.ts, l.text))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            2 => {
                let text: String = self
                    .visible()
                    .iter()
                    .map(|l| format!("{} >> {}\n", l.ts, l.text))
                    .collect();
                let dir = std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join("Downloads"))
                    .filter(|d| d.is_dir());
                let mut b = self.buffer.relock();
                match dir {
                    Some(d) => {
                        let path = d.join(format!("corvus-console-{}.log", now_ts().replace(':', "")));
                        match std::fs::write(&path, text) {
                            Ok(()) => b.add("success", &format!("Saved to {}", path.display())),
                            Err(e) => b.add("error", &format!("Could not save the log: {e}")),
                        }
                    }
                    None => b.add("error", "Could not save the log: no Downloads folder"),
                }
            }
            _ => {
                let mut b = self.buffer.relock();
                b.lines.clear();
                b.pending.clear();
            }
        }
    }

    fn paint_stream(&self, ui: &mut egui::Ui, r: Rect) {
        let painter = ui.painter().clone();
        {
            card::paint(&painter, r, Look::Console);
        }
        let inner = r.shrink2(Vec2::new(12.0, 8.0));
        let lines = self.visible();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        let memory = ui.scope_id().with("console-jump");
        let (mut seen, mut missed, jump): (u64, usize, bool) = ui.data(|d| d.get_temp(memory)).unwrap_or_default();
        let added = self.buffer.relock().added;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("constream")
            .stick_to_bottom(true)
            .auto_shrink([false, false]);
        if jump {
            area = area.vertical_scroll_offset(f32::INFINITY);
        }
        let out = area.show(&mut child, |ui| {
            let width = inner.width() - 9.0;
            for l in &lines {
                let class = level_class(&l.level);
                let (msg, arrow, rail) = match class {
                    "success" => (style::healthy(), style::terminal_cmd(), Some(style::healthy())),
                    "warning" => (style::warning(), style::warning(), Some(style::warning())),
                    "error" => (style::critical(), style::critical(), Some(style::critical())),
                    "nav" => (style::nav(), style::terminal_cmd(), None),
                    "cmd" => (
                        style::terminal_cmd(),
                        style::terminal_cmd(),
                        Some(style::terminal_cmd()),
                    ),
                    "shell" => (style::nav_bright(), style::terminal_cmd(), Some(style::nav_bright())),
                    _ => (style::terminal_text(), style::terminal_cmd(), None),
                };
                let mut job = egui::text::LayoutJob::default();
                let fmt = |c: Color32| egui::TextFormat {
                    font_id: style::mono(12.0),
                    color: c,
                    line_height: Some(12.0 * 1.55),
                    ..Default::default()
                };
                job.append(&l.ts, 0.0, fmt(style::terminal_muted()));
                job.append(" ", 0.0, fmt(msg));
                job.append(">>", 0.0, fmt(arrow));
                job.append(" ", 0.0, fmt(msg));
                job.append(&l.text, 0.0, fmt(msg));
                job.wrap.max_width = width;
                job.wrap.break_anywhere = true;
                let g = ui.painter().layout_job(job);
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(inner.width(), g.size().y + 2.0), egui::Sense::hover());
                if let Some(c) = rail {
                    ui.painter()
                        .vline(rect.left() + 1.0, rect.y_range(), Stroke::new(2.0, c));
                }
                ui.painter()
                    .galley(Pos2::new(rect.left() + 9.0, rect.top() + 1.0), g, msg);
            }
        });
        let at_end = jump || out.state.clamped_offset().y + out.inner_rect.height() >= out.content_size.y - 1.0;
        missed = if at_end {
            0
        } else {
            missed + added.saturating_sub(seen) as usize
        };
        seen = added;
        let pressed = !at_end && self.jump_chip(ui, r, missed);
        ui.data_mut(|d| d.insert_temp(memory, (seen, missed, pressed)));
    }

    /// `.console-jump`: appears once the stream is scrolled away from its live
    /// end and says how much has come since. True when pressed.
    fn jump_chip(&self, ui: &mut egui::Ui, r: Rect, missed: usize) -> bool {
        let label = match missed {
            0 => crate::i18n::tr("Jump to live").into_owned(),
            1 => crate::i18n::tr_fmt(NEW_LINE, &[("a", "1")]),
            n => crate::i18n::tr_fmt(NEW_LINES, &[("a", &n.to_string())]),
        };
        let g = style::galley(ui, &label, 10.5, Weight::W600, style::text_inverted(), 0.3);
        let size = Vec2::new(10.0 + 12.0 + 5.0 + g.size().x + 10.0, 4.0 + g.size().y.max(12.0) + 4.0);
        let rect = Rect::from_min_size(Pos2::new(r.center().x - size.x / 2.0, r.bottom() - 10.0 - size.y), size);
        let resp = ui
            .interact(rect, ui.scope_id().with("console-jump-chip"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let fill = if resp.hovered() {
            style::accent_hover()
        } else {
            style::accent()
        };
        let painter = ui.painter();
        painter.add(style::shadow_pop().as_shape(rect, 255u8));
        painter.rect_filled(rect, 255u8, fill);
        icons::paint(
            ui,
            "arrow-down",
            Rect::from_center_size(Pos2::new(rect.left() + 10.0 + 6.0, rect.center().y), Vec2::splat(12.0)),
            style::text_inverted(),
        );
        painter.galley(
            Pos2::new(rect.left() + 10.0 + 12.0 + 5.0, rect.center().y - g.size().y / 2.0),
            g,
            style::text_inverted(),
        );
        resp.clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_keeps_the_newest_lines_and_counts_every_one() {
        let mut b = Buffer::default();
        for i in 0..(MAX_LINES + 500) {
            b.add("info", &format!("line {i}"));
        }
        assert_eq!(b.lines.len(), MAX_LINES);
        assert_eq!(b.lines[0].text, "line 500");
        assert_eq!(b.added, (MAX_LINES + 500) as u64);
        b.paused = true;
        for i in 0..(MAX_LINES + 10) {
            b.add("info", &format!("held {i}"));
        }
        assert_eq!(b.pending.len(), MAX_LINES, "a paused stream is bounded too");
    }

    #[test]
    fn a_transcript_line_is_timestamp_level_and_text() {
        let l = |level: &str| Line {
            ts: "12:00:01".into(),
            level: level.into(),
            text: "Armed".into(),
        };
        assert_eq!(format_line(&l("info")), "12:00:01  [info] Armed");
        assert_eq!(format_line(&l("")), "12:00:01  Armed");
    }

    #[test]
    fn commands_are_normalised_as_panel_js_does() {
        assert_eq!(normalize_command("  ARM  "), "arm");
        assert_eq!(normalize_command("MODE   hold"), "mode hold");
        assert_eq!(normalize_command("shell  ls   /fs"), "shell ls   /fs");
        assert_eq!(normalize_command("takeoff\n20"), "takeoff 20");
        assert_eq!(normalize_command("   "), "");
    }

    #[test]
    fn levels_draw_as_their_class() {
        assert_eq!(level_class("critical"), "error");
        assert_eq!(level_class("shell"), "shell");
        assert_eq!(level_class("unknown"), "");
    }

    #[test]
    fn pause_holds_lines_and_resume_lets_them_in() {
        let mut b = Buffer::default();
        b.add("", "one");
        b.paused = true;
        b.add("", "two");
        assert_eq!((b.lines.len(), b.pending.len()), (1, 1));
        let mut b2 = Buffer::default();
        for i in 0..2100 {
            b2.add("", &i.to_string());
        }
        assert_eq!(b2.lines.len(), MAX_LINES);
        assert_eq!(b2.lines[0].text, "100");
    }
}
