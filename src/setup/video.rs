//! The Video page of Setup (python/src/js/setup-video.js, the `.video-*` rules of
//! setup.css): a list of cameras, each an address with an optional user and
//! password, and the ffmpeg that decodes the RTSP ones. Two views, never both:
//! the list with the decoder under it, and the editor of one camera.

use std::sync::Arc;

use eframe::egui::{self, Galley, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::setup::video_model::{self as model, Draft, Kind};
use crate::setup::video_window::VideoWindows;
use crate::ui::card;
use crate::ui::formfield as ff;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::{self, Kind as Msg};
use crate::ui::modal;
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

type Answer = Pending<Result<Value, String>>;

/// The editor of one camera.
struct Editor {
    draft: Draft,
    is_new: bool,
    message: Option<(Msg, String)>,
    saving: Option<Answer>,
    removing: Option<Answer>,
    confirm_remove: bool,
}

/// The page's state.
pub struct VideoPage {
    server: Server,
    status: Value,
    loaded: bool,
    read_failed: bool,
    poll: Option<Answer>,
    last_poll: f64,
    editor: Option<Editor>,
    notice: Option<(Msg, String)>,
    ffmpeg: String,
    ffmpeg_dirty: bool,
    ffmpeg_saving: Option<Answer>,
    errors: Vec<String>,
}

impl VideoPage {
    pub fn new(server: Server, _ctx: &egui::Context) -> VideoPage {
        VideoPage {
            server,
            status: Value::Null,
            loaded: false,
            read_failed: false,
            poll: None,
            last_poll: f64::MIN,
            editor: None,
            notice: None,
            ffmpeg: String::new(),
            ffmpeg_dirty: false,
            ffmpeg_saving: None,
            errors: Vec::new(),
        }
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    pub fn close_editor(&mut self) {
        self.editor = None;
        self.notice = None;
        self.last_poll = f64::MIN;
    }

    fn streams(&self) -> Vec<Value> {
        self.status
            .get("streams")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn paint(&mut self, data: Value) {
        self.status = data;
        self.loaded = true;
        self.read_failed = false;
        if !self.ffmpeg_dirty {
            self.ffmpeg = self
                .status
                .get("ffmpeg")
                .and_then(|f| f.get("configured"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
    }

    fn take_answers(&mut self, ctx: &egui::Context, windows: &mut VideoWindows) {
        if let Some(r) = self.poll.as_ref().and_then(Pending::take) {
            self.poll = None;
            match r {
                Ok(v) if self.editor.is_none() => self.paint(v),
                Ok(_) => {}
                Err(_) => self.read_failed = true,
            }
        }
        if let Some(r) = self.ffmpeg_saving.as_ref().and_then(Pending::take) {
            self.ffmpeg_saving = None;
            match r {
                Ok(v) => {
                    self.ffmpeg_dirty = false;
                    if let Some(s) = v.get("status").filter(|s| s.is_object()) {
                        self.paint(s.clone());
                    }
                    self.notice = Some((Msg::Ok, model::settings_notice(&self.ffmpeg).into()));
                }
                Err(e) => {
                    let e = if e.is_empty() {
                        "The path could not be saved.".into()
                    } else {
                        e
                    };
                    self.notice = Some((Msg::Err, e));
                }
            }
        }
        let mut finished = None;
        if let Some(ed) = &mut self.editor {
            if let Some(r) = ed.saving.as_ref().and_then(Pending::take) {
                ed.saving = None;
                match r {
                    Ok(v) => finished = Some(("saved", v, ed.is_new)),
                    Err(e) => {
                        ed.message = Some((
                            Msg::Err,
                            if e.is_empty() {
                                "The camera could not be saved.".into()
                            } else {
                                e
                            },
                        ));
                    }
                }
            }
            if let Some(r) = ed.removing.as_ref().and_then(Pending::take) {
                ed.removing = None;
                match r {
                    Ok(v) => finished = Some(("removed", v, false)),
                    Err(e) => {
                        ed.message = Some((
                            Msg::Err,
                            if e.is_empty() {
                                "The camera could not be removed.".into()
                            } else {
                                e
                            },
                        ));
                    }
                }
            }
        }
        if let Some((what, v, is_new)) = finished {
            let id = self.editor.as_ref().map(|e| e.draft.id.clone()).unwrap_or_default();
            self.close_editor();
            if let Some(s) = v.get("status").filter(|s| s.is_object()) {
                self.paint(s.clone());
            }
            if what == "removed" {
                windows.close(&id);
                self.notice = Some((Msg::Ok, "Camera removed.".into()));
            } else {
                let saved_id = v.get("id").and_then(Value::as_str).unwrap_or("");
                // A camera just added is one the operator wants to see: that
                // is the test of whether the address is right.
                if is_new {
                    if let Some(c) = self
                        .streams()
                        .iter()
                        .find(|s| s.get("id").and_then(Value::as_str) == Some(saved_id))
                    {
                        windows.open(ctx, &self.server, c);
                    }
                }
                self.notice = Some(
                    match v.get("warning").and_then(Value::as_str).filter(|w| !w.is_empty()) {
                        Some(w) => (Msg::Warn, w.into()),
                        None => (Msg::Ok, if is_new { "Camera added." } else { "Camera saved." }.into()),
                    },
                );
            }
        }
    }

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, now: f64, windows: &mut VideoWindows) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx, windows);
        if self.editor.is_none() && self.poll.is_none() && now - self.last_poll >= model::POLL_MS {
            self.last_poll = now;
            let s = self.server.clone();
            self.poll = Some(Pending::spawn(&ctx, move || http::get_json(&s, "/api/video/status")));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let mut y = pagekit::header(
            ui,
            x,
            y,
            "Video",
            "Cameras on the aircraft or the ground network, over RTSP or WebRTC, each in a window of its own",
        ) + 32.0;
        if self.editor.is_some() {
            return self.editor_card(ui, &ctx, x, y, w);
        }
        let streams = self.streams();
        let available = self.status.get("available") != Some(&Value::Bool(false));
        if self.read_failed {
            y = message::show(ui, x, y, w, "Could not read the camera list.", Msg::Err) + 12.0;
        } else if !available && model::needs_decoder(&streams) {
            let reason = self.status.get("reason").and_then(Value::as_str).unwrap_or("");
            let reason = if reason.is_empty() {
                "Video is unavailable."
            } else {
                reason
            };
            y = message::show(ui, x, y, w, reason, Msg::Warn) + 12.0;
        }
        y = self.cameras_card(ui, &ctx, x, y, w, &streams, now, windows) + 16.0;
        self.decoder_card(ui, &ctx, x, y, w)
    }

    #[allow(clippy::too_many_arguments)]
    fn cameras_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        x: f32,
        y: f32,
        w: f32,
        streams: &[Value],
        _now: f64,
        windows: &mut VideoWindows,
    ) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Cameras") + 12.0;
        if let Some((kind, text)) = &self.notice {
            cy = message::show(ui, ix, cy, iw, text, *kind) + 12.0;
        }
        if streams.is_empty() {
            let g = kit::wrapped_weight(
                ui,
                "No cameras yet. Add the RTSP address of the camera on the aircraft or on the ground network.",
                11.0,
                Weight::W400,
                style::text_3(),
                iw,
                1.5,
            );
            painter.galley(Pos2::new(ix, cy), g.clone(), style::text_3());
            cy += g.size().y + 12.0;
        }
        let narrow = crate::app::responsive::Vw::of(ctx).max600;
        let mut open_clicked = None;
        let mut edit_clicked = None;
        for (i, s) in streams.iter().enumerate() {
            let id = s.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let local = windows.local(&id);
            let shown = model::shown_state(s, local.as_ref());
            let state = model::known_state(shown.get("state").and_then(Value::as_str).unwrap_or("idle")).to_string();
            let top_pad = if i == 0 { 0.0 } else { 8.0 };
            let name = model::row_name(s);
            let kind = if s.get("kind").and_then(Value::as_str) == Some("webrtc") {
                "WebRTC"
            } else {
                "RTSP"
            };
            let badge_g = style::galley(ui, &kind.to_uppercase(), 9.0, Weight::W600, style::text_3(), 0.6);
            let open_btn = Button::new("Open", Variant::Primary).icon("play").small();
            let (ow, oh) = (open_btn.width(ui), open_btn.height());
            let state_g = style::galley(
                ui,
                model::state_text(&state),
                11.0,
                Weight::W600,
                state_color(&state),
                0.0,
            );
            let mut right = ix + iw - 28.0;
            let edit_rect_x = right;
            right -= 10.0 + ow;
            let open_x = right;
            let state_x = if narrow {
                right
            } else {
                right -= 10.0 + state_g.size().x;
                right
            };
            let text_x = ix + 7.0 + 10.0;
            let text_w = (right - 10.0 - text_x).max(40.0);
            let name_h: f32 = 16.0;
            let detail_line = model::detail_line(&shown);
            let detail_g = one_line(ui, &detail_line, 11.0, true, style::text_3(), text_w);
            let detail_h: f32 = 15.0;
            let content_h = (name_h + 2.0 + detail_h).max(oh).max(28.0);
            let row_top = cy + top_pad;
            let mid = row_top + content_h / 2.0;
            kit::dot(
                &painter,
                Pos2::new(ix + 3.5, mid),
                dot_color(model::state_level(&state)),
                state == "live",
            );
            let badge_w = badge_g.size().x + 10.0 + 2.0;
            let name_fit = one_line(
                ui,
                &name,
                13.0,
                false,
                style::text_1(),
                (text_w - badge_w - 6.0).max(30.0),
            );
            let name_top = mid - (name_h + 2.0 + detail_h) / 2.0;
            painter.galley(Pos2::new(text_x, name_top), name_fit.clone(), style::text_1());
            let bx = text_x + name_fit.size().x + 6.0;
            let badge = Rect::from_min_size(Pos2::new(bx, name_top + 2.0), Vec2::new(badge_w, 15.0));
            painter.rect_stroke(badge, 4, Stroke::new(1.0, style::border()), egui::StrokeKind::Inside);
            painter.galley(
                badge.min + Vec2::new(6.0, (15.0 - badge_g.size().y) / 2.0),
                badge_g,
                style::text_3(),
            );
            painter.galley(Pos2::new(text_x, name_top + name_h + 2.0), detail_g, style::text_3());
            if !narrow {
                painter.galley(
                    Pos2::new(state_x, mid - state_g.size().y / 2.0),
                    state_g,
                    state_color(&state),
                );
            }
            if open_btn.show(
                ui,
                Rect::from_min_size(Pos2::new(open_x, mid - oh / 2.0), Vec2::new(ow, oh)),
                ("videoopen", i),
            ) {
                open_clicked = Some(i);
            }
            if kit::icon_btn(
                ui,
                Rect::from_min_size(Pos2::new(edit_rect_x, mid - 14.0), Vec2::splat(28.0)),
                "pencil",
                "Edit",
                false,
                ("videoedit", i),
            ) {
                edit_clicked = Some(i);
            }
            cy = row_top + content_h + 8.0;
            if i + 1 < streams.len() {
                painter.hline(
                    egui::Rangef::new(ix, ix + iw),
                    cy + 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
                cy += 1.0;
            }
        }
        if !streams.is_empty() {
            cy += 12.0;
        }
        if let Some(i) = open_clicked {
            windows.open(ctx, &self.server, &streams[i]);
        }
        if let Some(i) = edit_clicked {
            self.editor = Some(Editor {
                draft: Draft::from_stream(&streams[i]),
                is_new: false,
                message: None,
                saving: None,
                removing: None,
                confirm_remove: false,
            });
        }
        // the actions
        let max = self
            .status
            .get("max_streams")
            .and_then(Value::as_u64)
            .map_or(model::MAX_STREAMS, |n| n as usize);
        let add = Button::new("Add camera", Variant::Secondary)
            .icon("plus")
            .enabled(streams.len() < max);
        let (aw, ah) = (add.width(ui), add.height());
        if add.show(
            ui,
            Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(aw, ah)),
            "videoadd",
        ) {
            self.editor = Some(Editor {
                draft: Draft::blank(),
                is_new: true,
                message: None,
                saving: None,
                removing: None,
                confirm_remove: false,
            });
            self.notice = None;
        }
        if streams.len() >= 2 {
            let all = Button::new("Open all", Variant::Ghost)
                .icon("video")
                .title("Open a window for every camera. The camera button on the map does the same.");
            let alw = all.width(ui);
            if all.show(
                ui,
                Rect::from_min_size(Pos2::new(ix + aw + 8.0, cy), Vec2::new(alw, ah)),
                "videoall",
            ) {
                for s in streams {
                    windows.open(ctx, &self.server, s);
                }
            }
        }
        cy += ah;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    fn decoder_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Decoder") + 12.0;
        let g = kit::wrapped_weight(
            ui,
            "RTSP cameras send H.264 or H.265, which Corvus hands to ffmpeg to decode. It runs only while a camera window is open, and stops a few seconds after the last one closes. WebRTC cameras need no ffmpeg: the window decodes them itself.",
            12.0,
            Weight::W400,
            style::text_2(),
            iw - 28.0,
            1.55,
        );
        let r = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, g.size().y + 24.0 + 2.0));
        painter.rect_filled(r, style::RADIUS_BTN, style::surface_2());
        painter.rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::border_soft()),
            egui::StrokeKind::Inside,
        );
        painter.galley(r.min + Vec2::new(15.0, 13.0), g, style::text_2());
        cy = r.bottom() + 12.0;
        if model::old_decoder(&self.status) {
            let g = kit::wrapped_weight(
                ui,
                "This ffmpeg is older than version 5, so camera passwords show in the process list, where other accounts on this computer can read them. Install ffmpeg 5 or later to keep them private.",
                12.0,
                Weight::W400,
                style::warning(),
                iw,
                1.45,
            );
            painter.galley(Pos2::new(ix, cy), g.clone(), style::warning());
            cy += g.size().y + 12.0;
        }
        let path = self
            .status
            .get("ffmpeg")
            .and_then(|f| f.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let rows = [
            (
                "ffmpeg",
                if self.loaded {
                    model::ffmpeg_version(&self.status)
                } else {
                    "\u{2026}".into()
                },
                false,
            ),
            (
                "Found at",
                if !self.loaded {
                    "\u{2026}".into()
                } else if path.is_empty() {
                    "\u{2014}".into()
                } else {
                    path.to_string()
                },
                true,
            ),
            ("WebRTC plays", "WebRTC is not available here".to_string(), false),
        ];
        for (i, (label, value, wrap)) in rows.iter().enumerate() {
            let lg = style::galley(ui, label, 13.0, Weight::W400, style::text_1(), 0.0);
            let vw = (iw - lg.size().x - 16.0).max(60.0);
            let vg = if *wrap {
                mono_wrapped(ui, value, 13.0, style::text_2(), vw)
            } else {
                style::galley_mono(ui, value, 13.0, style::text_2())
            };
            let h = lg.size().y.max(vg.size().y) + 20.0;
            let mid = cy + 10.0 + (h - 20.0) / 2.0;
            painter.galley(Pos2::new(ix, mid - lg.size().y / 2.0), lg, style::text_1());
            painter.galley(
                Pos2::new(ix + iw - vg.size().x, mid - vg.size().y / 2.0),
                vg,
                style::text_2(),
            );
            if i + 1 < rows.len() {
                painter.hline(
                    egui::Rangef::new(ix, ix + iw),
                    cy + h - 0.5,
                    Stroke::new(1.0, style::border_soft()),
                );
            }
            cy += h + 12.0;
        }
        let mut text = self.ffmpeg.clone();
        let mut changed = false;
        cy = ff::field(
            ui,
            ix,
            cy,
            iw,
            "ffmpeg path",
            "Leave it empty to find ffmpeg by itself: on the PATH, in the usual install folders, or from the imageio-ffmpeg package.",
            |ui, top| {
                let r = ff::input(ui, ix, top, iw, &mut text, "Found by itself", true, false, "videoffmpeg");
                changed = r.changed();
                ff::CONTROL_H
            },
        ) + 24.0;
        if changed {
            self.ffmpeg = text;
            self.ffmpeg_dirty = true;
        }
        let b = Button::new("Use this ffmpeg", Variant::Secondary)
            .small()
            .enabled(self.ffmpeg_saving.is_none());
        let (bw, bh) = (b.width(ui), b.height());
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(bw, bh)),
            "videoffsave",
        ) {
            let body = model::settings_body(&self.ffmpeg);
            let s = self.server.clone();
            self.ffmpeg_saving = Some(Pending::spawn(ctx, move || post(&s, "/api/video/settings", &body)));
        }
        cy += bh;
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    fn editor_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let Some(ed) = self.editor.as_mut() else {
            return y;
        };
        let mut cy =
            pagekit::section_title(ui, ix, y + 19.0, iw, if ed.is_new { "New camera" } else { "Camera" }) + 12.0;
        let busy = ed.saving.is_some() || ed.removing.is_some();

        let mut name = ed.draft.name.clone();
        cy = ff::field(
            ui,
            ix,
            cy,
            iw,
            "Name",
            "What the window's title bar says. Leave it empty to use the address.",
            |ui, top| {
                ff::input(ui, ix, top, iw, &mut name, "Gimbal camera", false, false, "videoname");
                ff::CONTROL_H
            },
        ) + 12.0;
        ed.draft.name = name;

        let kind = ed.draft.kind;
        let mut next_kind = None;
        cy = ff::field(
            ui,
            ix,
            cy,
            iw,
            "Type",
            "RTSP is what most cameras send. WebRTC needs a media server in between, and gives the lowest delay.",
            |ui, top| {
                let choices = [
                    Choice::new("rtsp", "RTSP (decoded by Corvus with ffmpeg)"),
                    Choice::new("webrtc", "WebRTC (WHEP, played by the window)"),
                ];
                next_kind = ff::select(ui, ix, top, iw, &choices, kind.id(), "videokind");
                ff::CONTROL_H
            },
        ) + 12.0;
        if let Some(k) = next_kind {
            ed.draft.kind = Kind::parse(&k);
        }
        let kind = ed.draft.kind;

        let mut url = ed.draft.url.clone();
        cy = ff::field(ui, ix, cy, iw, "Address", kind.address_hint(), |ui, top| {
            ff::input(ui, ix, top, iw, &mut url, kind.placeholder(), true, false, "videourl");
            ff::CONTROL_H
        }) + 12.0;
        ed.draft.url = url;

        let mut user = ed.draft.username.clone();
        cy = ff::field(ui, ix, cy, iw, "User", "", |ui, top| {
            ff::input(ui, ix, top, iw, &mut user, "optional", false, false, "videouser");
            ff::CONTROL_H
        }) + 12.0;
        ed.draft.username = user;

        let mut pass = ed.draft.password.clone();
        let mut pass_changed = false;
        let placeholder = if ed.draft.has_password {
            "Saved. Type to replace it."
        } else {
            "optional"
        };
        cy = ff::field(ui, ix, cy, iw, "Password", kind.password_hint(), |ui, top| {
            let r = ff::input(ui, ix, top, iw, &mut pass, placeholder, true, true, "videopass");
            pass_changed = r.changed();
            ff::CONTROL_H
        }) + 12.0;
        if pass_changed {
            ed.draft.password = pass;
            ed.draft.password_touched = true;
        }

        if kind == Kind::Rtsp {
            let transport = ed.draft.transport.clone();
            let mut next = None;
            cy = ff::field(
                ui,
                ix,
                cy,
                iw,
                "Transport",
                "How the RTSP video travels. TCP unless the camera only does UDP.",
                |ui, top| {
                    let choices = [
                        Choice::new("tcp", "TCP (works through packet loss)"),
                        Choice::new("udp", "UDP (lower delay on a clean link)"),
                    ];
                    next = ff::select(ui, ix, top, iw, &choices, &transport, "videotransport");
                    ff::CONTROL_H
                },
            ) + 12.0;
            if let Some(t) = next {
                ed.draft.transport = t;
            }
        }

        if let Some((kind, text)) = &ed.message {
            cy = message::show(ui, ix, cy, iw, text, *kind) + 12.0;
        }

        let problem = ed.draft.problem();
        let save = Button::new(
            if busy && ed.saving.is_some() { "Saving" } else { "Save" },
            Variant::Primary,
        )
        .icon("check")
        .enabled(problem.is_empty() && !busy)
        .title(problem);
        let delete = (!ed.is_new).then(|| {
            Button::new("Delete", Variant::Danger)
                .icon("trash-2")
                .title("Remove this camera")
                .enabled(!busy)
        });
        let cancel = Button::new("Cancel", Variant::Ghost);
        let mut bx = ix;
        let mut action = None;
        let mut buttons: Vec<(usize, Button)> = vec![(0, save)];
        if let Some(d) = delete {
            buttons.push((1, d));
        }
        buttons.push((2, cancel));
        let mut bh = 0.0f32;
        for (id, b) in buttons {
            let (bw, h) = (b.width(ui), b.height());
            bh = bh.max(h);
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(bx, cy), Vec2::new(bw, h)),
                ("videoed", id),
            ) {
                action = Some(id);
            }
            bx += bw + 8.0;
        }
        cy += bh;
        match action {
            Some(0) => {
                ed.message = None;
                let body = ed.draft.save_body();
                let s = self.server.clone();
                ed.saving = Some(Pending::spawn(ctx, move || post(&s, "/api/video/streams", &body)));
            }
            Some(1) => ed.confirm_remove = true,
            Some(2) => {
                self.close_editor();
                card::fill(
                    &painter,
                    bg,
                    Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
                    card::Look::Page,
                );
                return cy + 19.0;
            }
            _ => {}
        }
        if ed.confirm_remove {
            let label = if !ed.draft.name.is_empty() {
                ed.draft.name.clone()
            } else if !ed.draft.url.is_empty() {
                ed.draft.url.clone()
            } else {
                "this camera".into()
            };
            let mut choice = None;
            let closed = modal::show(
                ctx,
                "videoremove",
                modal::Size::Sm,
                "Remove camera",
                |ui, mx, my, mw| {
                    let g = kit::wrapped_weight(
                        ui,
                        &format!("Remove \"{label}\"?"),
                        13.0,
                        Weight::W400,
                        style::text_2(),
                        mw,
                        1.5,
                    );
                    ui.painter().galley(Pos2::new(mx, my), g.clone(), style::text_2());
                    let (hit, bottom) = modal::actions(
                        ui,
                        mx,
                        my + g.size().y + 16.0,
                        mw,
                        vec![
                            Button::new("Cancel", Variant::Ghost),
                            Button::new("Remove", Variant::Danger).icon("trash-2"),
                        ],
                    );
                    choice = hit;
                    bottom
                },
            );
            if closed || choice == Some(0) {
                ed.confirm_remove = false;
            } else if choice == Some(1) {
                ed.confirm_remove = false;
                let body = serde_json::json!({ "id": ed.draft.id });
                let s = self.server.clone();
                ed.removing = Some(Pending::spawn(ctx, move || {
                    post(&s, "/api/video/streams/remove", &body)
                }));
            }
        }
        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }
}

fn state_color(state: &str) -> egui::Color32 {
    match state {
        "live" => style::healthy(),
        "error" | "unavailable" => style::critical(),
        "starting" | "stalled" => style::warning(),
        _ => style::text_2(),
    }
}

fn dot_color(level: &str) -> egui::Color32 {
    match level {
        "healthy" => style::healthy(),
        "warning" => style::warning(),
        "critical" => style::critical(),
        _ => style::text_disabled(),
    }
}

/// One line of text cut with an ellipsis at `w` (`text-overflow: ellipsis`).
fn one_line(ui: &egui::Ui, text: &str, size: f32, mono: bool, color: egui::Color32, w: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: if mono {
                style::mono(size)
            } else {
                style::font(size, Weight::W600)
            },
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('\u{2026}');
    ui.painter().layout_job(job)
}

/// Mono text that breaks anywhere (`overflow-wrap: anywhere`).
fn mono_wrapped(ui: &egui::Ui, text: &str, size: f32, color: egui::Color32, w: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: style::mono(size),
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = w;
    job.wrap.break_anywhere = true;
    ui.painter().layout_job(job)
}

fn post(s: &Server, path: &str, body: &Value) -> Result<Value, String> {
    let d = http::post_json(s, path, body)?;
    if d.get("ok") == Some(&Value::Bool(false)) {
        Err(d.get("error").and_then(Value::as_str).unwrap_or("").to_string())
    } else {
        Ok(d)
    }
}
