//! The Logs page (`renderLogsPage` of `python/src/js/sidenav.js`): the last lines of
//! the console, a button that writes the whole transcript to the log folder
//! and one that copies it.

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::form::{self, Form};
use crate::ui::kit::{Button, Variant};
use crate::ui::{pagekit, style};

/// `LOG_PREVIEW_LINES`.
const PREVIEW_LINES: usize = 30;

/// The section title and the lines shown (`renderLogsPage`).
pub fn preview(transcript: &str) -> (String, Vec<&str>) {
    let all: Vec<&str> = if transcript.is_empty() {
        Vec::new()
    } else {
        transcript.split('\n').collect()
    };
    let title = if all.len() > PREVIEW_LINES {
        format!("Console Output: last {PREVIEW_LINES} of {} lines", all.len())
    } else {
        "Recent Console Output".to_string()
    };
    let from = all.len().saturating_sub(PREVIEW_LINES);
    (title, all[from..].to_vec())
}

/// `YYYY-MM-DDTHH-MM-SS` of a Unix time in UTC, as the web names the file
/// (`toISOString` with `:` and `.` turned to `-`, cut at 19).
pub fn stamp(secs: u64) -> String {
    let (y, m, d, hh, mm, ss) = crate::data::clock::civil(secs as i64);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}-{mm:02}-{ss:02}")
}

/// The file the transcript is written to.
pub fn file_name(secs: u64) -> String {
    format!("corvus-console_{}.log", stamp(secs))
}

/// The page's state.
pub struct LogsPage {
    server: Server,
    export: Option<Pending<Result<Value, String>>>,
    note: (String, bool),
}

impl LogsPage {
    pub fn new(server: Server) -> LogsPage {
        LogsPage {
            server,
            export: None,
            note: (String::new(), false),
        }
    }

    /// Leaving the page forgets the note, as the web page is rebuilt.
    pub fn reset(&mut self) {
        self.note = (String::new(), false);
    }

    /// Draws the page into `rect`; `transcript` is the console's text.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, transcript: &str) {
        if let Some(r) = self.export.as_ref().and_then(Pending::take) {
            self.export = None;
            self.note = match r {
                Ok(v) if v.get("ok").and_then(Value::as_bool) != Some(false) => (
                    format!(
                        "Written to {}",
                        v.get("path").and_then(Value::as_str).unwrap_or("the log folder")
                    ),
                    false,
                ),
                Ok(v) => (
                    v.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Could not write the log")
                        .to_string(),
                    true,
                ),
                Err(e) => (
                    if e.is_empty() {
                        "Could not write the log".into()
                    } else {
                        e
                    },
                    true,
                ),
            };
        }
        pagekit::background(ui, rect);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect);
        egui::ScrollArea::vertical()
            .id_salt("logsscroll")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let top = ui.max_rect().top();
                let mut f = Form::new(
                    rect.left() + pagekit::PAD_X,
                    top + pagekit::PAD_Y,
                    rect.width() - 2.0 * pagekit::PAD_X,
                );
                f.y = pagekit::header(ui, f.x, f.y, "Logs", "Flight logs and MAVLink message history") + 28.0;
                self.body(ui, &mut f, transcript);
                ui.allocate_space(Vec2::new(rect.width() - 1.0, f.y + pagekit::PAD_Y - top));
            });
    }

    fn body(&mut self, ui: &mut egui::Ui, f: &mut Form, transcript: &str) {
        let (title, shown) = preview(transcript);
        let empty = shown.is_empty();
        let mut hit = None;
        let busy = self.export.is_some();
        let note = self.note.clone();
        form::section(ui, f, &title, |ui, f| {
            form::card(ui, f, "", |ui, f| {
                if empty {
                    form::desc(ui, f, "No log entries yet.");
                } else {
                    let mut job = egui::text::LayoutJob::single_section(
                        shown.join("\n"),
                        egui::TextFormat {
                            font_id: style::mono(11.0),
                            color: style::text_2(),
                            line_height: Some(16.5),
                            ..Default::default()
                        },
                    );
                    job.wrap.max_width = f.w;
                    let g = ui.painter().layout_job(job);
                    let h = g.size().y;
                    ui.painter().galley(Pos2::new(f.x, f.y), g, style::text_2());
                    f.y += h;
                }
                f.skip(12.0);
                hit = form::actions(
                    ui,
                    f,
                    vec![
                        Button::new("Export log", Variant::Primary)
                            .icon("download")
                            .small()
                            .enabled(!empty && !busy)
                            .title("Write the full console transcript to the log folder"),
                        Button::new("Copy", Variant::Secondary)
                            .icon("copy")
                            .small()
                            .enabled(!empty)
                            .title("Copy the full console transcript to the clipboard"),
                    ],
                );
                f.skip(10.0);
                let text = if busy {
                    "Writing the log\u{2026}"
                } else {
                    note.0.as_str()
                };
                let color = if note.1 && !busy {
                    style::critical()
                } else {
                    style::text_2()
                };
                let g = crate::ui::kit::wrapped_weight(ui, text, 11.5, style::Weight::W400, color, f.w, 1.5);
                let h = g.size().y.max(16.0);
                ui.painter().galley(Pos2::new(f.x, f.y), g, color);
                f.y += h;
            });
        });
        match hit {
            Some(0) => {
                let server = self.server.clone();
                let name = file_name(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs()),
                );
                let body = json!({ "filename": name, "text": transcript });
                self.note = (String::new(), false);
                self.export = Some(Pending::spawn(ui.ctx(), move || {
                    http::post_json(&server, "/api/console/save", &body)
                }));
            }
            Some(1) => {
                ui.ctx().copy_text(transcript.to_string());
                self.note = ("Copied to the clipboard.".into(), false);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_shows_the_last_thirty_lines_and_says_so() {
        let (t, lines) = preview("");
        assert_eq!((t.as_str(), lines.len()), ("Recent Console Output", 0));
        let few = "a\nb\nc";
        let (t, lines) = preview(few);
        assert_eq!((t.as_str(), lines.len()), ("Recent Console Output", 3));
        let many: String = (1..=45).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let (t, lines) = preview(&many);
        assert_eq!(t, "Console Output: last 30 of 45 lines");
        assert_eq!((lines[0], lines[29]), ("line 16", "line 45"));
        let exact: String = (1..=30).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        assert_eq!(preview(&exact).0, "Recent Console Output");
    }

    #[test]
    fn the_export_is_named_by_the_utc_second() {
        assert_eq!(stamp(0), "1970-01-01T00-00-00");
        assert_eq!(stamp(1_000_000_000), "2001-09-09T01-46-40");
        assert_eq!(stamp(1_782_345_678), "2026-06-25T00-01-18");
        assert_eq!(stamp(951_782_400 + 86_399), "2000-02-29T23-59-59");
        assert_eq!(file_name(1_000_000_000), "corvus-console_2001-09-09T01-46-40.log");
    }
}
