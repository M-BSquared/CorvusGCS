//! "Check values" (setup-shared.js `checkButton`, `checkCard`,
//! `summarizeCheck`): read every field back from the vehicle, write again what
//! did not stick, and show what the vehicle holds now.
//!
//! The page keeps what the operator asked for in `FormState::wanted`; a check
//! sends that with the names to read back (`POST /api/params/verify`), drops
//! what the vehicle confirmed, and the page re-reads itself fresh.

use std::collections::BTreeMap;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::bg::Pending;
use crate::http::{self, Server};
use crate::pagekit;
use crate::schema_form::format_number;
use crate::style::{self, Weight};

/// `POST /api/params/verify` refuses more names than this in one request.
pub const MAX_NAMES: usize = 200;

/// One value the operator set, as the check found it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub wanted: Option<f64>,
    pub after: Option<f64>,
    pub ok: bool,
    pub rewritten: bool,
    pub error: String,
}

/// The last check: a verdict class (`ok` or `err`), one sentence and the rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub cls: &'static str,
    pub text: String,
    pub rows: Vec<Row>,
}

/// `summarizeCheck`.
pub fn summarize(res: &Value) -> Summary {
    let rows: Vec<Row> = res
        .get("results")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| Row {
                    name: r.get("name").and_then(Value::as_str).unwrap_or("").into(),
                    wanted: r.get("wanted").and_then(Value::as_f64),
                    after: r.get("after").and_then(Value::as_f64),
                    ok: r.get("ok") == Some(&Value::Bool(true)),
                    rewritten: r.get("rewritten") == Some(&Value::Bool(true)),
                    error: r.get("error").and_then(Value::as_str).unwrap_or("").into(),
                })
                .collect()
        })
        .unwrap_or_default();
    let read = res.get("values").and_then(Value::as_object).map_or(0, |o| o.len());
    let missing: Vec<&str> = res
        .get("missing")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let failed: Vec<&Row> = rows.iter().filter(|r| !r.ok).collect();
    let rewritten = rows.iter().filter(|r| r.ok && r.rewritten).count();
    let mut cls = "ok";
    let mut text;
    if rows.is_empty() {
        text = format!("No changes to confirm on this page. {read} values read back from the vehicle.");
    } else if failed.is_empty() {
        text = if rows.len() == 1 {
            format!("{} is on the vehicle.", rows[0].name)
        } else {
            format!("All {} changes are on the vehicle.", rows.len())
        };
        if rewritten > 0 {
            text += &if rewritten == 1 && rows.len() == 1 {
                " It had to be written again.".to_string()
            } else {
                format!(" {rewritten} had to be written again.")
            };
        }
    } else {
        cls = "err";
        let names = failed.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ");
        text = if rows.len() == 1 {
            format!("{names} is not on the vehicle.")
        } else {
            format!(
                "{} of {} changes are not on the vehicle: {names}.",
                failed.len(),
                rows.len()
            )
        };
        text += " Press Check values again to retry.";
    }
    if !missing.is_empty() {
        cls = "err";
        text += &format!(" No answer for {}.", missing.join(", "));
    }
    Summary { cls, text, rows }
}

/// `formatChecked`.
pub fn format_checked(v: Option<f64>) -> String {
    v.map_or("no answer".to_string(), format_number)
}

/// `checkOutcome`.
pub fn outcome(r: &Row) -> String {
    if r.ok {
        if r.rewritten {
            "written again, confirmed".into()
        } else {
            "confirmed".into()
        }
    } else if r.error.is_empty() {
        "not applied".into()
    } else {
        r.error.clone()
    }
}

/// A check on its way, and the answer of the last one.
#[derive(Default)]
pub struct Check {
    run: Option<Pending<Result<Value, String>>>,
    pub result: Option<Summary>,
}

impl Check {
    pub fn busy(&self) -> bool {
        self.run.is_some()
    }

    /// Sends `wanted` and `names` to the vehicle for confirmation. The names
    /// are capped to what one request may carry.
    pub fn start(&mut self, ctx: &egui::Context, server: &Server, wanted: &BTreeMap<String, f64>, names: Vec<String>) {
        let targets: Vec<Value> = wanted.iter().map(|(n, v)| json!({"name": n, "value": v})).collect();
        let room = MAX_NAMES.saturating_sub(targets.len());
        let names: Vec<String> = names.into_iter().take(room).collect();
        let s = server.clone();
        self.run = Some(Pending::spawn(ctx, move || {
            http::post_json(&s, "/api/params/verify", &json!({"params": targets, "names": names}))
        }));
    }

    /// The finished check, when it is: the confirmed names are dropped from
    /// `wanted`, and the summary is kept as the last result.
    pub fn take(&mut self, wanted: &mut BTreeMap<String, f64>) -> Option<Summary> {
        let r = self.run.as_ref()?.take()?;
        self.run = None;
        let summary = match r {
            Ok(v) => {
                if let Some(rows) = v.get("results").and_then(Value::as_array) {
                    for row in rows {
                        if row.get("ok") == Some(&Value::Bool(true)) {
                            if let Some(n) = row.get("name").and_then(Value::as_str) {
                                wanted.remove(n);
                            }
                        }
                    }
                }
                summarize(&v)
            }
            Err(e) => Summary {
                cls: "err",
                text: if e.is_empty() {
                    "The check could not run".into()
                } else {
                    e
                },
                rows: Vec::new(),
            },
        };
        self.result = Some(summary.clone());
        Some(summary)
    }
}

/// `checkCard`: the last check, one row per value the operator set, in the
/// column `x .. x + w` from `y`. Returns the bottom of the card.
pub fn card(ui: &egui::Ui, x: f32, y: f32, w: f32, check: &Summary) -> f32 {
    let painter = ui.painter().clone();
    let bg = painter.add(egui::Shape::Noop);
    let (ix, iw) = (x + 19.0, w - 38.0);
    let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Check result");
    let color = if check.cls == "ok" {
        style::healthy()
    } else {
        style::critical()
    };
    let g = crate::kit::wrapped_weight(ui, &check.text, 12.0, Weight::W400, color, iw, 1.4);
    painter.galley(Pos2::new(ix, cy), g.clone(), color);
    cy += g.size().y;
    if !check.rows.is_empty() {
        cy += 10.0;
        let total = 1.4 + 0.8 + 0.8 + 1.4;
        let col = |i: usize| -> (f32, f32) {
            let fr = [1.4, 0.8, 0.8, 1.4];
            let avail = iw - 3.0 * 8.0;
            let start: f32 = fr[..i].iter().sum::<f32>() / total * avail + 8.0 * i as f32;
            (ix + start, fr[i] / total * avail)
        };
        let head = ["PARAMETER", "WANTED", "VEHICLE NOW", "RESULT"];
        let hh = 10.5 * 1.4;
        for (i, t) in head.iter().enumerate() {
            let g = style::galley(ui, t, 10.5, Weight::W400, style::text_3(), 0.3);
            painter.galley(Pos2::new(col(i).0, cy + 5.0), g, style::text_3());
        }
        cy += hh + 10.0;
        painter.hline(
            egui::Rangef::new(ix, ix + iw),
            cy - 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        for r in &check.rows {
            let cells = [
                r.name.clone(),
                format_checked(r.wanted),
                format_checked(r.after),
                outcome(r),
            ];
            let mut row_h = 0.0f32;
            let mut gs = Vec::new();
            for (i, t) in cells.iter().enumerate() {
                let c = if i == 3 {
                    if r.ok {
                        style::healthy()
                    } else {
                        style::critical()
                    }
                } else {
                    style::text_1()
                };
                let g = if i < 3 {
                    ui.painter().layout(t.clone(), style::mono(11.5), c, col(i).1)
                } else {
                    crate::kit::wrapped_weight(ui, t, 11.5, Weight::W400, c, col(i).1, 1.4)
                };
                row_h = row_h.max(g.size().y);
                gs.push((g, c));
            }
            for (i, (g, c)) in gs.into_iter().enumerate() {
                painter.galley(Pos2::new(col(i).0, cy + 5.0), g, c);
            }
            cy += row_h + 10.0;
            painter.hline(
                egui::Rangef::new(ix, ix + iw),
                cy - 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
        }
    }
    let rect = Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0));
    painter.set(bg, egui::Shape::rect_filled(rect, style::RADIUS, style::glass(0.82)));
    painter.rect_stroke(
        rect,
        style::RADIUS,
        Stroke::new(1.0, style::tint(0.09)),
        egui::StrokeKind::Inside,
    );
    let _ = Vec2::ZERO;
    rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_with_nothing_set_says_how_many_values_it_read() {
        let s = summarize(&json!({"results": [], "values": {"A": 1, "B": 2}}));
        assert_eq!(s.cls, "ok");
        assert_eq!(
            s.text,
            "No changes to confirm on this page. 2 values read back from the vehicle."
        );
    }

    #[test]
    fn a_confirmed_change_names_itself_and_says_when_it_was_written_again() {
        let one = summarize(&json!({"results": [{"name": "X", "ok": true, "rewritten": true}]}));
        assert_eq!(one.text, "X is on the vehicle. It had to be written again.");
        let two = summarize(&json!({"results": [
            {"name": "X", "ok": true, "rewritten": true}, {"name": "Y", "ok": true}]}));
        assert_eq!(two.text, "All 2 changes are on the vehicle. 1 had to be written again.");
    }

    #[test]
    fn a_refused_change_turns_the_verdict_red_and_names_it() {
        let s = summarize(&json!({"results": [
            {"name": "X", "ok": false, "wanted": 1.0, "after": 2.0}, {"name": "Y", "ok": true}],
            "missing": ["Z"]}));
        assert_eq!(s.cls, "err");
        assert_eq!(
            s.text,
            "1 of 2 changes are not on the vehicle: X. Press Check values again to retry. No answer for Z."
        );
        assert_eq!(outcome(&s.rows[0]), "not applied");
        assert_eq!(format_checked(None), "no answer");
    }

    #[test]
    fn only_confirmed_values_leave_the_wanted_map() {
        let mut wanted: BTreeMap<String, f64> = [("X".to_string(), 1.0), ("Y".to_string(), 2.0)].into();
        let mut c = Check {
            run: Some(Pending::ready(Ok(json!({"results": [
                {"name": "X", "ok": true}, {"name": "Y", "ok": false}]})))),
            ..Default::default()
        };
        let s = c.take(&mut wanted).expect("answer");
        assert_eq!(s.cls, "err");
        assert_eq!(wanted.keys().collect::<Vec<_>>(), ["Y"]);
    }
}
