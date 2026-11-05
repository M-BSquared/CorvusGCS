//! "A newer release exists" (`python/src/js/update.js`), backed entirely by
//! `GET /api/update`, which compares the running version with the project's
//! GitHub releases.
//!
//! Three rules keep it out of the operator's way in the field: the check runs
//! in the background a few seconds after start and a failure is silent; the
//! dialog is raised once per release ("Skip this version" is written to the
//! config); and it is never raised while the vehicle is armed, only once it
//! disarms.

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::kit::{self, Button, Variant};
use crate::ui::modal::{self, Size};
use crate::ui::style::{self, Weight};

/// Long enough that the map, the telemetry stream and the panels have
/// settled: the dialog must never be the first thing that paints.
const AUTO_DELAY: f64 = 4.0;
/// Release notes are a summary here, not a changelog viewer.
const NOTES_MAX_LINES: usize = 12;

/// What `GET /api/update` answers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub update_available: bool,
    pub current: String,
    pub latest: String,
    pub published: String,
    pub notes: String,
    pub url: String,
    pub skipped: String,
}

impl Status {
    pub fn parse(v: &Value) -> Status {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        Status {
            enabled: v.get("enabled") != Some(&Value::Bool(false)),
            update_available: v.get("update_available") == Some(&Value::Bool(true)),
            current: s("current"),
            latest: s("latest"),
            published: s("published"),
            notes: s("notes"),
            url: s("url"),
            skipped: s("skipped"),
        }
    }

    /// The version the quiet marker points at: an update the operator has not
    /// skipped, while the check is on.
    pub fn worth(&self) -> Option<&str> {
        (self.enabled && self.update_available && !self.latest.is_empty() && self.skipped != self.latest)
            .then_some(self.latest.as_str())
    }

    /// The automatic check does not raise a version the operator skipped.
    pub fn skipped_by_operator(&self) -> bool {
        !self.skipped.is_empty() && self.skipped == self.latest
    }
}

/// The notes as typed, at most twelve lines: GitHub release bodies are short
/// and the raw text reads fine, while rendering markdown would mean trusting
/// network content as markup.
pub fn notes_text(notes: &str) -> String {
    let lines: Vec<&str> = notes.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= NOTES_MAX_LINES {
        lines.join("\n")
    } else {
        format!("{}\n\u{2026}", lines[..NOTES_MAX_LINES].join("\n"))
    }
}

/// An ISO timestamp as its date, or "" when it is not one.
pub fn published_date(iso: &str) -> String {
    let d: String = iso.chars().take(10).collect();
    let ok = d.len() == 10
        && d.char_indices()
            .all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    if ok {
        d
    } else {
        String::new()
    }
}

#[derive(Default)]
pub struct Update {
    started: Option<f64>,
    check: Option<Pending<Option<Status>>>,
    /// A status held back because the vehicle was armed.
    held: Option<Status>,
    dialog: Option<Status>,
    /// The version the marker points at, or None.
    pub available: Option<String>,
    note: Option<(String, bool)>,
    open: Option<Pending<Result<bool, String>>>,
    scheduled: bool,
}

impl Update {
    pub fn new() -> Update {
        Update::default()
    }

    /// One step: starts the background check after its delay, raises the
    /// dialog when it is allowed, and draws it.
    pub fn show(&mut self, ctx: &egui::Context, server: &Server, state: &Value, now_s: f64, enabled: bool) {
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let started = *self.started.get_or_insert(now_s);
        if enabled && !self.scheduled && now_s - started >= AUTO_DELAY {
            self.scheduled = true;
            let s = server.clone();
            self.check = Some(Pending::spawn(ctx, move || {
                http::get_json(&s, "/api/update").ok().map(|v| Status::parse(&v))
            }));
        }
        if let Some(Some(status)) = self.check.as_ref().and_then(Pending::take) {
            self.check = None;
            self.available = status.worth().map(String::from);
            if status.update_available && !status.skipped_by_operator() && status.enabled {
                self.held = Some(status);
            }
        }
        if !armed && self.dialog.is_none() {
            if let Some(s) = self.held.take() {
                self.note = None;
                self.dialog = Some(s);
            }
        }
        if let Some(status) = self.dialog.clone() {
            self.draw(ctx, server, &status);
        }
    }

    fn draw(&mut self, ctx: &egui::Context, server: &Server, status: &Status) {
        if let Some(r) = self.open.as_ref().and_then(Pending::take) {
            self.open = None;
            self.note = Some(match r {
                Ok(true) => ("Opened in your browser.".to_string(), true),
                Ok(false) => {
                    ctx.copy_text(status.url.clone());
                    ("No browser available. Link copied to the clipboard.".to_string(), false)
                }
                Err(_) => ("Could not open the release page.".to_string(), false),
            });
        }
        let mut hit = None;
        let note = self.note.clone();
        let closed = modal::show(ctx, "update", Size::Lg, "Update available", |ui, x, y, w| {
            let mut y = y;
            let lead = kit::wrapped(
                ui,
                "A newer version of Corvus GCS has been published. Updating is manual: download the release for this platform and replace the installed app.",
                12.0,
                style::text_2(),
                w,
            );
            ui.painter().galley(Pos2::new(x, y), lead.clone(), style::text_2());
            y += lead.size().y + 12.0;
            let published = published_date(&status.published);
            let mut rows = vec![
                ("Installed", status.current.as_str()),
                ("Available", status.latest.as_str()),
            ];
            if !published.is_empty() {
                rows.push(("Published", published.as_str()));
            }
            for (label, value) in rows {
                let value = if value.is_empty() { "\u{2014}" } else { value };
                let l = style::galley(ui, label, 12.0, Weight::W400, style::text_3(), 0.0);
                let v = style::galley(ui, value, 12.0, Weight::W500, style::text_1(), 0.0);
                ui.painter().galley(Pos2::new(x, y), l, style::text_3());
                ui.painter()
                    .galley(Pos2::new(x + w - v.size().x, y), v, style::text_1());
                y += 24.0;
                ui.painter()
                    .hline(x..=x + w, y - 4.5, egui::Stroke::new(1.0, style::border_soft()));
            }
            let notes = notes_text(&status.notes);
            if !notes.is_empty() {
                y += 10.0;
                let cap = style::galley(ui, "Release notes", 12.0, Weight::W600, style::text_2(), 0.0);
                ui.painter().galley(Pos2::new(x, y), cap.clone(), style::text_2());
                y += cap.size().y + 5.0;
                let g = kit::wrapped(ui, &notes, 11.0, style::text_2(), w - 20.0);
                let box_ = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 16.0));
                ui.painter().rect_filled(box_, style::RADIUS_BTN, style::surface_2());
                ui.painter().galley(box_.min + Vec2::new(10.0, 8.0), g, style::text_2());
                y = box_.bottom();
            }
            y += 10.0;
            let cap = style::galley(ui, "Release page", 12.0, Weight::W600, style::text_2(), 0.0);
            ui.painter().galley(Pos2::new(x, y), cap.clone(), style::text_2());
            y += cap.size().y + 5.0;
            let url = style::galley_mono(ui, &status.url, 11.0, style::text_1());
            ui.painter().galley(Pos2::new(x, y), url.clone(), style::text_1());
            y += url.size().y + 8.0;
            if let Some((t, ok)) = &note {
                let c = if *ok { style::healthy() } else { style::warning() };
                let g = style::galley(ui, t, 12.0, Weight::W500, c, 0.0);
                ui.painter().galley(Pos2::new(x, y), g.clone(), c);
                y += g.size().y + 8.0;
            }
            let (h, bottom) = modal::actions(
                ui,
                x,
                y + 8.0,
                w,
                vec![
                    Button::new("Open release", Variant::Primary).icon("download"),
                    Button::new("Copy link", Variant::Secondary),
                    Button::new("Skip this version", Variant::Secondary),
                    Button::new("Later", Variant::Secondary),
                ],
            );
            hit = h;
            bottom
        });
        match hit {
            Some(0) => {
                let s = server.clone();
                self.open = Some(Pending::spawn(ctx, move || {
                    http::post_json(&s, "/api/update/open", &json!({})).map(|v| v.get("ok") == Some(&Value::Bool(true)))
                }));
            }
            Some(1) => {
                ctx.copy_text(status.url.clone());
                self.note = Some(("Link copied to the clipboard.".to_string(), true));
            }
            Some(2) => {
                // Fire and forget: the dialog closes either way, because a
                // failed write must not trap the operator in a dialog they
                // dismissed.
                let (s, v) = (server.clone(), status.latest.clone());
                std::thread::spawn(move || {
                    let _ = http::post_json(&s, "/api/update/skip", &json!({"version": v}));
                });
                self.available = None;
                self.dialog = None;
            }
            Some(_) => self.dialog = None,
            None if closed => self.dialog = None,
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(v: Value) -> Status {
        Status::parse(&v)
    }

    #[test]
    fn a_new_release_is_worth_a_marker_until_it_is_skipped() {
        let s =
            status(json!({"enabled": true, "update_available": true, "latest": "2026.12.01", "current": "2026.11.13"}));
        assert_eq!(s.worth(), Some("2026.12.01"));
        let skipped = status(json!({"update_available": true, "latest": "2026.12.01", "skipped": "2026.12.01"}));
        assert_eq!(skipped.worth(), None);
        assert!(skipped.skipped_by_operator());
        let other = status(json!({"update_available": true, "latest": "2026.12.02", "skipped": "2026.12.01"}));
        assert_eq!(other.worth(), Some("2026.12.02"));
        assert!(!other.skipped_by_operator());
        assert_eq!(
            status(json!({"enabled": false, "update_available": true, "latest": "x"})).worth(),
            None
        );
        assert_eq!(status(json!({"update_available": false, "latest": "x"})).worth(), None);
        assert_eq!(status(json!({"update_available": true})).worth(), None);
    }

    #[test]
    fn the_notes_are_cut_at_twelve_lines() {
        assert_eq!(notes_text(""), "");
        assert_eq!(notes_text("a\n\n b \n"), "a\n b ");
        let long: String = (1..=15).map(|i| format!("line {i}\n")).collect();
        let cut = notes_text(&long);
        assert_eq!(cut.lines().count(), 13);
        assert!(cut.ends_with("line 12\n\u{2026}"));
    }

    #[test]
    fn the_published_date_is_the_day() {
        assert_eq!(published_date("2026-11-05T09:30:00Z"), "2026-11-05");
        assert_eq!(published_date("yesterday"), "");
        assert_eq!(published_date(""), "");
    }
}
