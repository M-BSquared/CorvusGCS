//! The dialogs open over the page, and the company logo card whose picker is
//! one of them (`companyLogoCard` of `src/js/sidenav.js`).

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::{refused, Dialog, SettingsPage};
use crate::ui::form::{self, Form, Tone};
use crate::ui::kit::{Button, Variant};
use crate::ui::style::{self, Weight};

/// The PNG signature the backend checks too.
const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Why a file cannot be the company logo, before any round trip.
pub(super) fn logo_problem(name: &str, bytes: &[u8]) -> Option<&'static str> {
    if !name.to_ascii_lowercase().ends_with(".png") || !bytes.starts_with(&PNG) {
        Some("Choose a PNG file.")
    } else {
        None
    }
}

impl SettingsPage {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(open) = self.dialog.as_ref() else {
            return;
        };
        let close = match open {
            Dialog::Tokens => self.tokens_dialog(ctx),
            Dialog::DemRename { .. } | Dialog::DemRemove { .. } | Dialog::DemFiles { .. } => self.dem_dialogs(ctx),
            Dialog::Ssh { .. } | Dialog::SshRemove { .. } => self.ssh_dialogs(ctx),
            Dialog::LogoFile => self.logo_dialog(ctx),
            Dialog::McpPerms | Dialog::McpConnect { .. } => self.mcp_dialogs(ctx),
            Dialog::Credits => self.credits_dialog(ctx),
            Dialog::Export | Dialog::Import | Dialog::Reset | Dialog::Notice { .. } => self.transfer_dialogs(ctx),
        };
        if close {
            if matches!(self.dialog, Some(Dialog::Tokens)) {
                self.texts.retain(|k, _| !k.starts_with("token_"));
            }
            self.dialog = None;
        }
    }

    pub(super) fn logo_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let name = super::model::text(&self.cfg, "branding", "logo").to_string();
        if !name.is_empty() && self.logo.is_none() && self.logo_load.is_none() {
            let server = self.server.clone();
            let rev = self.logo_rev;
            self.logo_load = Some(crate::net::bg::Pending::spawn(ui.ctx(), move || {
                crate::net::http::get_bytes(&server, &format!("/api/branding/logo?v={rev}"))
            }));
        }
        if let Some(r) = self.logo_load.as_ref().and_then(|p| p.take()) {
            self.logo_load = None;
            self.logo = r;
        }
        let msg = self.msgs.get("logo").cloned();
        let busy = self.is_busy("logo") || self.is_busy("logo_remove");
        let uri = format!("bytes://company-logo-{}.png", self.logo_rev);
        let bytes = self.logo.clone();
        let (mut choose, mut remove) = (false, false);
        form::card(ui, f, "Company logo", |ui, f| {
            form::field(
                ui,
                f,
                "Top-right logo",
                "",
                "Optional PNG at the top right of the status bar; the Corvus mark keeps the left. Transparent artwork works best, max 4 MB.",
                |ui, x, y, _w| {
                    let tile = Rect::from_min_size(Pos2::new(x, y), Vec2::new(168.0, 60.0));
                    paint_checker(ui, tile);
                    match (&bytes, name.is_empty()) {
                        (Some(b), false) => {
                            egui::Image::from_bytes(uri.clone(), b.clone())
                                .fit_to_exact_size(Vec2::new(148.0, 48.0))
                                .maintain_aspect_ratio(true)
                                .paint_at(ui, tile.shrink2(Vec2::new(10.0, 6.0)));
                        }
                        (None, false) => {}
                        _ => {
                            let g = style::galley(ui, "No company logo", 11.0, Weight::W400, style::text_3(), 0.0);
                            ui.painter().galley(tile.center() - g.size() / 2.0, g, style::text_3());
                        }
                    }
                    let sx = tile.right() + 12.0;
                    let label = if name.is_empty() { "Corvus mark only" } else { &name };
                    let g = style::galley_mono(ui, label, 11.5, style::text_2());
                    let gh = g.size().y;
                    ui.painter().galley(Pos2::new(sx, y), g, style::text_2());
                    let by = y + gh + 8.0;
                    let b1 = Button::new("Choose PNG\u{2026}", Variant::Secondary).icon("image-plus").enabled(!busy);
                    let w1 = b1.width(ui);
                    let h = b1.height();
                    choose = b1.show(ui, Rect::from_min_size(Pos2::new(sx, by), Vec2::new(w1, h)), "logochoose");
                    let b2 = Button::new("Remove", Variant::Secondary)
                        .icon("trash-2")
                        .enabled(!name.is_empty() && !busy);
                    let w2 = b2.width(ui);
                    remove = b2.show(ui, Rect::from_min_size(Pos2::new(sx + w1 + 8.0, by), Vec2::new(w2, h)), "logoremove");
                    (by + h - y).max(60.0)
                },
            );
            if let Some((t, tone)) = &msg {
                f.skip(10.0);
                form::message(ui, f, t, *tone);
            }
        });
        if choose {
            self.hush("logo");
            self.text("logo_path").clear();
            self.dialog = Some(Dialog::LogoFile);
        }
        if remove {
            self.say("logo", "Removing\u{2026}", Tone::Plain);
            self.act(ui.ctx(), "logo_remove", "", "/api/branding/logo/remove", json!({}));
        }
    }

    fn logo_dialog(&mut self, ctx: &egui::Context) -> bool {
        let (mut go, mut close) = (false, false);
        let msg = self.msgs.get("logo_dialog").cloned();
        let buf = self.texts.entry("logo_path").or_default();
        let closed = crate::ui::modal::show(
            ctx,
            "logo",
            crate::ui::modal::Size::Md,
            "Company logo",
            |ui, x, y, w| {
                let mut f = Form::new(x, y, w);
                let resp = form::input(
                ui,
                &mut f,
                "PNG file",
                "",
                "The path of a PNG on this computer. Optional artwork for the top right of the status bar, max 4 MB.",
                buf,
                "~/Pictures/logo.png",
                true,
                false,
                "logopath",
            );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                if let Some((t, tone)) = &msg {
                    f.skip(10.0);
                    form::message(ui, &mut f, t, *tone);
                }
                f.skip(18.0);
                let (hit, bottom) = crate::ui::modal::actions(
                    ui,
                    f.x,
                    f.y,
                    f.w,
                    vec![
                        Button::new("CANCEL", Variant::Secondary),
                        Button::new("UPLOAD", Variant::Primary).icon("upload"),
                    ],
                );
                match hit {
                    Some(0) => close = true,
                    Some(1) => go = true,
                    _ => {}
                }
                bottom
            },
        );
        if go {
            let raw = self.text("logo_path").trim().trim_matches('"').to_string();
            let path = raw.strip_prefix("~/").map_or(raw.clone(), |r| {
                format!("{}/{r}", std::env::var("HOME").unwrap_or_default())
            });
            let name = std::path::Path::new(&path)
                .file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned());
            match std::fs::read(&path) {
                Err(_) => self.fail("logo_dialog", "Could not read the file."),
                Ok(bytes) => match logo_problem(&name, &bytes) {
                    Some(p) => self.fail("logo_dialog", p),
                    None => {
                        self.dialog = None;
                        self.say("logo", "Uploading\u{2026}", Tone::Plain);
                        let q = super::map_cards::pct(&name);
                        self.act_bytes(ctx, "logo", &name, &format!("/api/branding/logo?name={q}"), bytes);
                    }
                },
            }
        }
        closed || close
    }

    pub(super) fn logo_done(&mut self, _ctx: &egui::Context, name: &'static str, answer: Result<Value, String>) {
        let removing = name == "logo_remove";
        let fallback = if removing {
            "Could not remove the logo."
        } else {
            "Could not upload the logo."
        };
        if let Some(e) = refused(&answer, fallback) {
            self.fail("logo", &e);
            return;
        }
        self.logo = None;
        self.logo_rev += 1;
        if removing {
            super::model::merge(&mut self.cfg, &json!({"branding": {"logo": ""}}));
            self.say("logo", "Company logo removed.", Tone::Ok);
        } else {
            let file = answer
                .as_ref()
                .ok()
                .and_then(|v| v.get("logo"))
                .and_then(Value::as_str)
                .unwrap_or("logo.png")
                .to_string();
            super::model::merge(&mut self.cfg, &json!({"branding": {"logo": file}}));
            self.say("logo", "Company logo updated.", Tone::Ok);
        }
    }
}

/// The transparency checker behind the preview
/// (`repeating-conic-gradient(surface-2 0% 25%, surface-1 0% 50%) / 12px`).
fn paint_checker(ui: &egui::Ui, tile: Rect) {
    let p = ui.painter();
    p.rect_filled(tile, style::RADIUS_BTN, style::surface_1());
    let clip = p.with_clip_rect(tile);
    let mut y = tile.top();
    let mut row = 0;
    while y < tile.bottom() {
        let mut x = tile.left();
        let mut col = 0;
        while x < tile.right() {
            if (row + col) % 2 == 0 {
                clip.rect_filled(
                    Rect::from_min_size(Pos2::new(x, y), Vec2::splat(6.0)),
                    0,
                    style::surface_2(),
                );
                clip.rect_filled(
                    Rect::from_min_size(Pos2::new(x + 6.0, y + 6.0), Vec2::splat(6.0)),
                    0,
                    style::surface_2(),
                );
            }
            x += 12.0;
            col += 1;
        }
        y += 12.0;
        row += 1;
    }
    p.rect_stroke(
        tile,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::border()),
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_real_png_is_offered_to_the_backend() {
        let mut png = PNG.to_vec();
        png.extend([0, 0, 0, 0]);
        assert_eq!(logo_problem("crest.PNG", &png), None);
        assert_eq!(logo_problem("crest.jpg", &png), Some("Choose a PNG file."));
        assert_eq!(logo_problem("crest.png", b"\xff\xd8\xff"), Some("Choose a PNG file."));
    }
}
