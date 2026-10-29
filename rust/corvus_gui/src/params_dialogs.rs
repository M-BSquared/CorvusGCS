//! The three dialogs of the Parameters page: the reboot confirmation, Export
//! and Import (src/js/setup-parameters.js, setup-shared.js `rebootButton`).
//! Each draws itself and reports what was pressed; the page does the work.
//!
//! The web asks the browser for the import file. A native window has no file
//! picker without a dependency the project does not take, so Import asks for
//! the path of a file on this machine instead.

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};

use crate::bg::Pending;
use crate::kit::{self, Button, Choice, Variant};
use crate::modal;
use crate::params_files::{self as files, Preview, FORMATS};
use crate::style::{self, Weight};

/// The Export dialog's fields.
pub struct ExportDlg {
    pub name: String,
    pub dir: String,
    pub format: String,
    pub only_modified: bool,
    pub message: Option<String>,
    pub target: Option<Pending<Result<serde_json::Value, String>>>,
    pub save: Option<Pending<Result<serde_json::Value, String>>>,
}

impl ExportDlg {
    pub fn new(target: Pending<Result<serde_json::Value, String>>) -> ExportDlg {
        ExportDlg {
            name: String::new(),
            dir: String::new(),
            format: "qgc".into(),
            only_modified: false,
            message: None,
            target: Some(target),
            save: None,
        }
    }
}

/// The Import dialog: the path first, then what the file would change.
pub struct ImportDlg {
    pub path: String,
    pub message: Option<String>,
    pub preview: Option<(Preview, String)>,
}

/// What a dialog's buttons did this frame.
#[derive(Debug, PartialEq)]
pub enum Out {
    None,
    Close,
    Go,
}

fn desc_box(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, style::text_2(), w - 28.0, 1.55);
    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 24.0));
    ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
    ui.painter().rect_stroke(
        r,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::border_soft()),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(r.min + Vec2::new(14.0, 12.0), g, style::text_2());
    r.bottom()
}

fn label(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x, y), g, style::text_3());
    y + h + 5.0
}

fn hint(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = kit::wrapped_weight(ui, text, 10.5, Weight::W400, style::text_3(), w, 1.4);
    ui.painter().galley(Pos2::new(x, y + 5.0), g.clone(), style::text_3());
    y + 5.0 + g.size().y
}

fn message(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let c = style::critical();
    let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, c, w, 1.4);
    ui.painter().galley(Pos2::new(x, y), g.clone(), c);
    y + g.size().y
}

const FIELD_H: f32 = 34.5;

/// `rebootButton`'s confirmation. Returns true when closed, and true again
/// as the second value when Reboot was pressed.
pub fn reboot(ctx: &egui::Context) -> (bool, bool) {
    let mut go = false;
    let close = modal::show(
        ctx,
        "paramsreboot",
        modal::Size::Sm,
        "Reboot the autopilot?",
        |ui, x, y, w| {
            let g = kit::wrapped_weight(
            ui,
            "The link to the vehicle drops while it restarts, and the parameters are read again afterwards. The vehicle must be disarmed.",
            12.0,
            Weight::W400,
            style::text_2(),
            w,
            1.5,
        );
            ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
            let (hit, bottom) = modal::actions(
                ui,
                x,
                y + g.size().y + 18.0,
                w,
                vec![
                    Button::new("Cancel", Variant::Secondary),
                    Button::new("Reboot", Variant::Primary).icon("power"),
                ],
            );
            go = hit == Some(1);
            if hit == Some(0) {
                go = false;
            }
            bottom
        },
    );
    (close || go, go)
}

/// Export. `modified` is how many parameters differ from their default, when
/// the defaults are known.
pub fn export(ctx: &egui::Context, dlg: &mut ExportDlg, total: usize, modified: Option<usize>) -> Out {
    let mut out = Out::None;
    let busy = dlg.save.is_some();
    let close = modal::show(
        ctx,
        "paramsexport",
        modal::Size::Md,
        "Export parameters",
        |ui, x, y, w| {
            let chosen = if dlg.only_modified {
                modified.unwrap_or(0)
            } else {
                total
            };
            let summary = format!(
                "{} will be written as a {} parameter file. Import reads it back.",
                files::plural(chosen, "parameter"),
                files::format(&dlg.format).kind
            );
            let mut cy = desc_box(ui, x, y, w, &summary) + 14.0;
            cy = label(ui, x, cy, "File name");
            let placeholder = format!("corvus-params{}", files::format(&dlg.format).suffix);
            kit::text_field(
                ui,
                Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, FIELD_H)),
                &mut dlg.name,
                &placeholder,
                true,
                "expname",
            );
            cy += FIELD_H + 14.0;
            cy = label(ui, x, cy, "Format");
            let choices: Vec<Choice> = FORMATS.iter().map(|f| Choice::new(f.id, f.label)).collect();
            if let Some(v) = kit::select(
                ui,
                Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, FIELD_H)),
                &choices,
                &dlg.format,
                "",
                false,
                "",
                "expformat",
            ) {
                dlg.name = files::with_suffix(&dlg.name, &v);
                dlg.format = v;
            }
            cy += FIELD_H + 14.0;
            cy = label(ui, x, cy, "Folder");
            kit::text_field(
                ui,
                Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, FIELD_H)),
                &mut dlg.dir,
                "~/.corvus/params",
                true,
                "expdir",
            );
            cy += FIELD_H;
            cy = hint(
                ui,
                x,
                cy,
                w,
                "On the machine running Corvus. Created if it does not exist.",
            ) + 14.0;
            let lg = style::galley(
                ui,
                "Only parameters changed from their default",
                12.0,
                Weight::W600,
                style::text_1(),
                0.0,
            );
            let enabled = modified.is_some();
            ui.painter().galley(
                Pos2::new(x, cy + (22.0 - lg.size().y) / 2.0),
                lg,
                if enabled { style::text_1() } else { style::text_3() },
            );
            if kit::switch(ui, Pos2::new(x + w - 38.0, cy), dlg.only_modified, enabled, "expmod") {
                dlg.only_modified = !dlg.only_modified;
            }
            cy += 22.0;
            let h = match modified {
                Some(n) => format!("{n} of {total} parameters differ from their default."),
                None => "Needs the defaults from the vehicle, which are not available.".into(),
            };
            cy = hint(ui, x, cy, w - 50.0, &h);
            if let Some(m) = &dlg.message {
                cy = message(ui, x, cy + 14.0, w, m);
            }
            let (hit, bottom) = modal::actions(
                ui,
                x,
                cy + 18.0,
                w,
                vec![
                    Button::new("Cancel", Variant::Secondary),
                    Button::new("Save", Variant::Primary).icon("download").enabled(!busy),
                ],
            );
            if hit == Some(1) {
                out = Out::Go;
            } else if hit == Some(0) {
                out = Out::Close;
            }
            bottom
        },
    );
    if close {
        Out::Close
    } else {
        out
    }
}

/// Import. `armed` keeps the write button off.
pub fn import(ctx: &egui::Context, dlg: &mut ImportDlg, armed: bool) -> Out {
    let mut out = Out::None;
    let max_list = (ctx.content_rect().height() * 0.4).min(320.0);
    let close = modal::show(
        ctx,
        "paramsimport",
        modal::Size::Md,
        "Import parameters",
        |ui, x, y, w| {
            let mut cy;
            match &dlg.preview {
                None => {
                    cy = desc_box(
                        ui,
                        x,
                        y,
                        w,
                        "Enter the path of a parameter file (.params, .param or .json) on the machine running Corvus.",
                    ) + 14.0;
                    cy = label(ui, x, cy, "File");
                    let r = kit::text_field(
                        ui,
                        Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, FIELD_H)),
                        &mut dlg.path,
                        "~/.corvus/params/corvus-params.params",
                        true,
                        "imppath",
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !dlg.path.trim().is_empty() {
                        out = Out::Go;
                    }
                    cy += FIELD_H;
                    if let Some(m) = &dlg.message {
                        cy = message(ui, x, cy + 14.0, w, m);
                    }
                    let (hit, bottom) = modal::actions(
                        ui,
                        x,
                        cy + 18.0,
                        w,
                        vec![
                            Button::new("Cancel", Variant::Secondary),
                            Button::new("Read file", Variant::Primary)
                                .icon("upload")
                                .enabled(!dlg.path.trim().is_empty()),
                        ],
                    );
                    if hit == Some(1) {
                        out = Out::Go;
                    } else if hit == Some(0) {
                        out = Out::Close;
                    }
                    cy = bottom;
                }
                Some((p, file)) => {
                    cy = desc_box(ui, x, y, w, &files::preview_text(p, file)) + 14.0;
                    if !p.changes.is_empty() {
                        cy = change_list(ui, x, cy, w, p, max_list) + 14.0;
                        if p.reboot_note {
                            cy = note(
                                ui,
                                x,
                                cy,
                                w,
                                "Some of these take effect only after the autopilot reboots.",
                            ) + 14.0;
                        }
                    }
                    if !p.unknown.is_empty() {
                        let shown = p.unknown.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
                        let more = if p.unknown.len() > 8 {
                            format!(" and {} more", p.unknown.len() - 8)
                        } else {
                            String::new()
                        };
                        cy = note(ui, x, cy, w, &format!("Not on this vehicle: {shown}{more}.")) + 14.0;
                    }
                    let n = p.to_write.len();
                    let (hit, bottom) = modal::actions(
                        ui,
                        x,
                        cy + 4.0,
                        w,
                        vec![
                            Button::new(if n > 0 { "Cancel" } else { "Close" }, Variant::Secondary),
                            Button::new(
                                &if n > 0 {
                                    format!("Write {}", files::plural(n, "parameter"))
                                } else {
                                    "Nothing to write".to_string()
                                },
                                Variant::Primary,
                            )
                            .icon("upload")
                            .enabled(n > 0 && !armed),
                        ],
                    );
                    if hit == Some(1) {
                        out = Out::Go;
                    } else if hit == Some(0) {
                        out = Out::Close;
                    }
                    cy = bottom;
                }
            }
            cy
        },
    );
    if close {
        Out::Close
    } else {
        out
    }
}

fn note(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, style::text_2(), w, 1.45);
    ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
    y + g.size().y
}

/// `.params-import-list`: name, old value, an arrow and the new value, in a
/// box that scrolls past `max_h`.
fn change_list(ui: &mut egui::Ui, x: f32, y: f32, w: f32, p: &Preview, max_h: f32) -> f32 {
    const SHOWN: usize = 300;
    let row_h = 5.0 * 2.0 + 11.5 * 1.21 + 1.0;
    let rows = p.changes.len().min(SHOWN);
    let extra = if p.changes.len() > SHOWN {
        20.0 + 11.0 * 1.21
    } else {
        0.0
    };
    let full = rows as f32 * row_h + extra;
    let h = full.min(max_h);
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(1.0)));
    egui::ScrollArea::vertical()
        .id_salt("importlist")
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            let inner = rect.width() - 2.0;
            ui.set_width(inner);
            let (area, _) = ui.allocate_exact_size(Vec2::new(inner, full - 2.0), egui::Sense::hover());
            let painter = ui.painter_at(area);
            for (i, c) in p.changes.iter().take(SHOWN).enumerate() {
                let top = area.top() + i as f32 * row_h;
                let to = style::galley_mono(ui, &crate::params::format_value(c.to, c.kind), 11.5, style::text_1());
                let from = style::galley_mono(ui, &crate::params::format_value(c.from, c.kind), 11.5, style::text_3());
                let arrow = style::galley_mono(ui, "\u{2192}", 11.5, style::text_3());
                let cy = top + (row_h - 1.0) / 2.0;
                let to_x = area.right() - 10.0 - to.size().x;
                let arrow_x = to_x - 8.0 - 14.0 + (14.0 - arrow.size().x) / 2.0;
                let from_x = to_x - 8.0 - 14.0 - 8.0 - from.size().x;
                let name = style::galley_mono(ui, &c.name, 11.5, style::text_1());
                painter.galley(
                    Pos2::new(area.left() + 10.0, cy - name.size().y / 2.0),
                    name,
                    style::text_1(),
                );
                painter.galley(Pos2::new(from_x, cy - from.size().y / 2.0), from, style::text_3());
                painter.galley(Pos2::new(arrow_x, cy - arrow.size().y / 2.0), arrow, style::text_3());
                painter.galley(Pos2::new(to_x, cy - to.size().y / 2.0), to, style::text_1());
                if i + 1 < rows || extra > 0.0 {
                    painter.hline(
                        area.x_range(),
                        top + row_h - 0.5,
                        Stroke::new(1.0, style::border_soft()),
                    );
                }
            }
            if extra > 0.0 {
                let t = format!("and {} more", p.changes.len() - SHOWN);
                let g = style::galley(ui, &t, 11.0, Weight::W400, style::text_3(), 0.0);
                painter.galley(
                    Pos2::new(
                        area.center().x - g.size().x / 2.0,
                        area.top() + rows as f32 * row_h + 10.0,
                    ),
                    g,
                    style::text_3(),
                );
            }
        });
    ui.painter().rect_stroke(
        rect,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::border_soft()),
        egui::StrokeKind::Inside,
    );
    rect.bottom()
}
