//! The confirmations and the list of saved plans (`confirmUpload`,
//! `readFromVehicle`, `confirmClear`, `openPlanDialog` of `mission.js`).

use crate::ui::tip::HoverTr;
use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::mission::backend::{Job, Saved};
use crate::mission::issues::{distance_text, Issue};
use crate::mission::{Dialog, MissionPage};
use crate::ui::card::{self, Look};
use crate::ui::kit::{self, Button, Variant};
use crate::ui::modal::{self, Size};
use crate::ui::style::{self, Weight};

/// What a dialog asked for.
enum Pick {
    Close,
    Upload(bool),
    Read,
    Clear,
    Open(String),
    Remove(String),
    Arm(String),
}

impl MissionPage {
    pub(crate) fn show_dialogs(&mut self, ctx: &egui::Context) {
        let mut pick: Option<Pick> = None;
        let name = self.plan.name.clone();
        let count = self.plan.items.len();
        let drives = self.plan.drives;
        let units = self.units;
        let now = ctx.input(|i| i.time);
        match &self.dialog {
            Dialog::None => return,
            Dialog::Upload { fly, warnings } => {
                let (fly, warnings) = (*fly, warnings.clone());
                let title = match (fly, drives) {
                    (true, true) => "Drive this mission?",
                    (true, false) => "Fly this mission?",
                    _ => "Upload this mission?",
                };
                let text = match (fly, drives) {
                    (true, true) => format!(
                        "{name} will be uploaded, the vehicle switched to its mission mode and armed. It will start driving."
                    ),
                    (true, false) => format!(
                        "{name} will be uploaded, the vehicle switched to MISSION and armed. It will take off."
                    ),
                    (false, true) => format!("{name} will be written to the vehicle. It will not drive until you start it."),
                    (false, false) => format!("{name} will be written to the vehicle. It will not fly until you start it."),
                };
                let close = modal::show(ctx, "mission-upload", Size::Md, title, |ui, x, y, w| {
                    let mut cy = y;
                    let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_2(), w, 1.5);
                    ui.painter().galley(Pos2::new(x, cy), g.clone(), style::text_2());
                    cy += g.size().y + 12.0;
                    cy = warning_list(ui, x, cy, w, &warnings) + 6.0;
                    let confirm = Button::new(
                        match (fly, drives) {
                            (true, true) => "Upload and drive",
                            (true, false) => "Upload and fly",
                            _ => "Upload",
                        },
                        if fly { Variant::Danger } else { Variant::Primary },
                    )
                    .icon(if fly { "play" } else { "upload" });
                    let (hit, bottom) =
                        modal::actions(ui, x, cy, w, vec![Button::new("Cancel", Variant::Secondary), confirm]);
                    match hit {
                        Some(0) => pick = Some(Pick::Close),
                        Some(_) => pick = Some(Pick::Upload(fly)),
                        None => {}
                    }
                    bottom
                });
                if close && pick.is_none() {
                    pick = Some(Pick::Close);
                }
            }
            Dialog::Read => {
                let text = format!(
                    "{name} on screen is replaced by the mission the vehicle holds. What is saved is not touched."
                );
                let close = modal::show(
                    ctx,
                    "mission-read",
                    Size::Md,
                    "Read the mission from the vehicle?",
                    |ui, x, y, w| {
                        let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_2(), w, 1.5);
                        ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
                        let (hit, bottom) = modal::actions(
                            ui,
                            x,
                            y + g.size().y + 16.0,
                            w,
                            vec![
                                Button::new("Keep this plan", Variant::Secondary),
                                Button::new("Replace it", Variant::Primary).icon("download"),
                            ],
                        );
                        match hit {
                            Some(0) => pick = Some(Pick::Close),
                            Some(_) => pick = Some(Pick::Read),
                            None => {}
                        }
                        bottom
                    },
                );
                if close && pick.is_none() {
                    pick = Some(Pick::Close);
                }
            }
            Dialog::Clear => {
                let text = format!(
                    "{name} holds {count} item{}. Clearing it here does not touch what is already saved or on the vehicle.",
                    if count == 1 { "" } else { "s" }
                );
                let close = modal::show(ctx, "mission-clear", Size::Md, "Clear this mission?", |ui, x, y, w| {
                    let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_2(), w, 1.5);
                    ui.painter().galley(Pos2::new(x, y), g.clone(), style::text_2());
                    let (hit, bottom) = modal::actions(
                        ui,
                        x,
                        y + g.size().y + 16.0,
                        w,
                        vec![
                            Button::new("Keep it", Variant::Secondary),
                            Button::new("Clear the plan", Variant::Danger).icon("trash-2"),
                        ],
                    );
                    match hit {
                        Some(0) => pick = Some(Pick::Close),
                        Some(_) => pick = Some(Pick::Clear),
                        None => {}
                    }
                    bottom
                });
                if close && pick.is_none() {
                    pick = Some(Pick::Close);
                }
            }
            Dialog::Open { plans, dir, armed } => {
                let (plans, dir, armed) = (plans.clone(), dir.clone(), armed.clone());
                let close = modal::show(ctx, "mission-open", Size::Lg, "Open a mission", |ui, x, y, w| {
                    let mut cy = y;
                    if plans.is_empty() {
                        let text = format!("No saved missions yet. They are kept in {dir}.");
                        let g = kit::wrapped_weight(ui, &text, 12.0, Weight::W400, style::text_3(), w, 1.5);
                        ui.painter().galley(Pos2::new(x, cy), g.clone(), style::text_3());
                        cy += g.size().y;
                    }
                    for (k, p) in plans.iter().enumerate() {
                        cy = saved_row(
                            ui,
                            x,
                            cy,
                            w,
                            k,
                            p,
                            &units,
                            armed.as_ref().is_some_and(|a| a.0 == p.name && now - a.1 < 4.0),
                            &mut pick,
                        );
                        cy += 4.0;
                    }
                    let (hit, bottom) =
                        modal::actions(ui, x, cy + 12.0, w, vec![Button::new("Close", Variant::Secondary)]);
                    if hit == Some(0) {
                        pick = Some(Pick::Close);
                    }
                    bottom
                });
                if close && pick.is_none() {
                    pick = Some(Pick::Close);
                }
                if armed.as_ref().is_some_and(|a| now - a.1 >= 4.0) {
                    if let Dialog::Open { armed, .. } = &mut self.dialog {
                        *armed = None;
                    }
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
        match pick {
            None => {}
            Some(Pick::Close) => self.dialog = Dialog::None,
            Some(Pick::Upload(fly)) => {
                self.dialog = Dialog::None;
                self.send_plan(ctx, fly);
            }
            Some(Pick::Read) => {
                self.dialog = Dialog::None;
                self.start_read(ctx);
            }
            Some(Pick::Clear) => {
                self.dialog = Dialog::None;
                self.clear_plan();
            }
            Some(Pick::Open(name)) => {
                self.dialog = Dialog::None;
                self.send(ctx, Job::LoadPlan { name, quiet: false });
            }
            Some(Pick::Arm(name)) => {
                if let Dialog::Open { armed, .. } = &mut self.dialog {
                    *armed = Some((name, now));
                }
            }
            Some(Pick::Remove(name)) => {
                if let Dialog::Open { armed, .. } = &mut self.dialog {
                    *armed = None;
                }
                self.send(ctx, Job::RemovePlan { name });
            }
        }
    }
}

/// The warnings of the plan in the upload dialog: one line each.
fn warning_list(ui: &egui::Ui, x: f32, y: f32, w: f32, warnings: &[Issue]) -> f32 {
    let mut cy = y;
    for issue in warnings {
        let g = kit::wrapped_weight(ui, &issue.text, 11.0, Weight::W400, style::text_2(), w - 36.0, 1.4);
        let h = g.size().y + 12.0;
        let r = Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, h));
        let painter = ui.painter();
        card::paint(
            painter,
            r,
            Look::custom(
                style::warning().gamma_multiply(0.10),
                style::warning().gamma_multiply(0.45),
                style::RADIUS_BTN,
            ),
        );
        crate::ui::icons::paint(
            ui,
            "triangle-alert",
            Rect::from_center_size(Pos2::new(r.left() + 15.0, r.center().y), Vec2::splat(12.0)),
            style::warning(),
        );
        painter.galley(Pos2::new(r.left() + 28.0, r.top() + 6.0), g, style::text_2());
        cy += h + 4.0;
    }
    cy
}

/// One saved plan: its name, its size, and a trash that asks twice.
#[allow(clippy::too_many_arguments)]
fn saved_row(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    k: usize,
    p: &Saved,
    units: &crate::data::units::Units,
    armed: bool,
    pick: &mut Option<Pick>,
) -> f32 {
    let h = 7.0 * 2.0 + 12.5 * 1.21 + 2.0 + 10.5 * 1.21 + 2.0;
    let name = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w - 34.0, h));
    let resp = ui.interact(name, ui.scope_id().with(("msaved", k)), egui::Sense::click());
    let painter = ui.painter();
    card::paint(
        painter,
        name,
        Look::custom(
            if resp.hovered() {
                style::surface_hover()
            } else {
                style::surface_2()
            },
            style::border_soft(),
            style::RADIUS_BTN,
        ),
    );
    let tg = style::galley(ui, &p.title, 12.5, Weight::W400, style::text_1(), 0.0);
    painter.galley(
        Pos2::new(name.left() + 9.0, name.top() + 8.0),
        tg.clone(),
        style::text_1(),
    );
    let meta = format!("{} items · {}", p.items, distance_text(units, p.distance_m));
    let mg = style::galley_mono(ui, &meta, 10.5, style::text_3());
    painter.galley(
        Pos2::new(name.left() + 9.0, name.top() + 8.0 + tg.size().y + 2.0),
        mg,
        style::text_3(),
    );
    if resp.clicked() {
        *pick = Some(Pick::Open(p.name.clone()));
    }
    let trash = Rect::from_min_size(Pos2::new(x + w - 28.0, y + (h - 28.0) / 2.0), Vec2::splat(28.0));
    let title = if armed {
        format!("Press again to delete {}", p.name)
    } else {
        format!("Delete {}", p.name)
    };
    if armed {
        painter.rect_filled(trash, style::RADIUS_BTN, style::critical().gamma_multiply(0.18));
    }
    let tr = ui
        .interact(trash, ui.scope_id().with(("msavedrm", k)), egui::Sense::click())
        .on_hover_tr(title);
    let colour = if armed || tr.hovered() {
        style::critical()
    } else {
        style::text_3()
    };
    crate::ui::icons::paint(
        ui,
        "trash-2",
        Rect::from_center_size(trash.center(), Vec2::splat(15.0)),
        colour,
    );
    if tr.clicked() {
        *pick = Some(if armed {
            Pick::Remove(p.name.clone())
        } else {
            Pick::Arm(p.name.clone())
        });
    }
    y + h
}
