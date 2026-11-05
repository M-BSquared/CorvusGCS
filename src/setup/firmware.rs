//! The Firmware page of Setup (python/src/js/setup-firmware.js, the `.firmware-*`
//! rules of setup.css): flash PX4 or ArduPilot over a direct USB cable.
//!
//! Two cards say what the link is and whether flashing is allowed; the third
//! picks an image (a release and a board from the catalogue, or a file on this
//! computer), starts the flash and follows it. The backend decides everything
//! that matters (`can_flash` is true only on a USB link with the vehicle idle
//! and disarmed); the page mirrors the gate, adds the live armed flag, and
//! names the reason when it is closed. A request names a release and a board,
//! never a URL.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::net::stream::Stream;
use crate::setup::firmware_io as io;
use crate::setup::firmware_model::{self as model, Catalog, Log, Picker, Progress, Signature, Source, Status};
use crate::ui::card;
use crate::ui::formfield as ff;
use crate::ui::icons;
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::{self, Tone};
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

/// `.page-header` leaves 28 px under it and `.setup-page` 4 px between its
/// children; `.page-section` leaves 24 px.
const SECTION_GAP_HEAD: f32 = 32.0;
const SECTION_GAP: f32 = 28.0;
/// `.firmware-link-grid { max-width: 900px }`.
const GRID_MAX_W: f32 = 900.0;
/// `.firmware-link-grid`: the columns are at least 300 px, 16 px apart.
const GRID_GAP: f32 = 16.0;
const GRID_MIN: f32 = 300.0;
/// A small button as the web fonts measure it.
const SMALL_H: f32 = 29.0;
/// `.firmware-card { gap: 12px }`.
const GAP: f32 = 12.0;
/// A row of the card that holds a small button beside a select.
const ROW_GAP: f32 = 8.0;
/// `.guidance-list { max-height: 200px }`.
const LOG_MAX_H: f32 = 200.0;

type Answer = Pending<Result<Value, String>>;

/// What the request in flight is, so its answer is told right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Download,
    Upload,
}

/// The page's state.
pub struct FirmwarePage {
    server: Server,
    picker: Picker,
    source: Source,
    file_path: String,
    status: Option<Status>,
    status_req: Option<Answer>,
    status_wanted: bool,
    sig: Option<Signature>,
    catalog_req: Option<Answer>,
    catalog_asked: bool,
    /// A line in place of the catalogue note: loading, or why it failed.
    catalog_msg: Option<(String, bool)>,
    action: Option<(Action, Answer)>,
    /// A flash was accepted and no progress or status has said so yet.
    awaiting: bool,
    cancel_req: Option<Answer>,
    cancel_flag: Arc<AtomicBool>,
    cancel_visible: bool,
    stream: Option<Stream>,
    inbox: Arc<Mutex<Vec<Value>>>,
    bar: (u32, String),
    line: (String, bool),
    log: Log,
    noted: String,
    errors: Vec<String>,
    notes: Vec<String>,
    warnings: Vec<String>,
}

impl FirmwarePage {
    pub fn new(server: Server, _ctx: &egui::Context) -> FirmwarePage {
        FirmwarePage {
            server,
            picker: Picker::new(),
            source: Source::Catalog,
            file_path: String::new(),
            status: None,
            status_req: None,
            status_wanted: true,
            sig: None,
            catalog_req: None,
            catalog_asked: false,
            catalog_msg: None,
            action: None,
            awaiting: false,
            cancel_req: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            cancel_visible: false,
            stream: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
            bar: (0, String::new()),
            line: (String::new(), false),
            log: Log::default(),
            noted: String::new(),
            errors: Vec::new(),
            notes: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    // ----- requests ------------------------------------------------------

    fn load_catalog(&mut self, ctx: &egui::Context, refresh: bool) {
        if self.catalog_req.is_some() {
            return;
        }
        self.catalog_asked = true;
        self.catalog_msg = Some((
            if refresh {
                "Checking for new releases\u{2026}"
            } else {
                "Loading releases\u{2026}"
            }
            .into(),
            false,
        ));
        let s = self.server.clone();
        self.catalog_req = Some(Pending::spawn(ctx, move || io::get_catalog(&s, refresh)));
    }

    fn want_status(&mut self) {
        self.status_wanted = true;
    }

    /// The `firmware` topic, opened when a flash starts or one is running.
    fn open_stream(&mut self, ctx: &egui::Context) {
        if self.stream.is_some() {
            return;
        }
        let sink = self.inbox.clone();
        let repaint = ctx.clone();
        self.stream = Some(Stream::start(
            self.server.clone(),
            "/api/events?topics=firmware".into(),
            move |ev| {
                if ev.name != "firmware" {
                    return;
                }
                if let Ok(d) = serde_json::from_str::<Value>(&ev.data) {
                    if let Ok(mut q) = sink.lock() {
                        q.push(d);
                    }
                    repaint.request_repaint();
                }
            },
        ));
    }

    fn start_flash(&mut self, ctx: &egui::Context) {
        if self.action.is_some() {
            return;
        }
        self.cancel_flag.store(false, Ordering::SeqCst);
        self.line = (String::new(), false);
        let s = self.server.clone();
        match self.source {
            Source::Catalog => {
                if !self.picker.have_image() {
                    return;
                }
                let (release, board) = (self.picker.release.clone(), self.picker.board.clone());
                self.line = ("Downloading firmware\u{2026}".into(), false);
                self.log
                    .push(&format!("Downloading {board} ({release})\u{2026}"), "info");
                let body = model::flash_body(&release, &board);
                self.action = Some((
                    Action::Download,
                    Pending::spawn(ctx, move || {
                        let d = http::post_json(&s, "/api/firmware/flash", &body)?;
                        if d.get("ok") == Some(&Value::Bool(false)) {
                            Err(d
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("flash refused")
                                .to_string())
                        } else {
                            Ok(d)
                        }
                    }),
                ));
            }
            Source::File => {
                let path = self.file_path.trim().to_string();
                if path.is_empty() {
                    return;
                }
                self.line = ("Uploading\u{2026}".into(), false);
                self.log
                    .push(&format!("Uploading {}\u{2026}", model::file_name_of(&path)), "info");
                let flag = self.cancel_flag.clone();
                self.action = Some((
                    Action::Upload,
                    Pending::spawn(ctx, move || io::upload_file(&s, &path, &flag)),
                ));
            }
        }
    }

    fn cancel(&mut self, ctx: &egui::Context) {
        if self.cancel_req.is_some() {
            return;
        }
        let s = self.server.clone();
        self.cancel_req = Some(Pending::spawn(ctx, move || {
            http::post_json(&s, "/api/firmware/cancel", &json!({}))
        }));
    }

    // ----- answers -------------------------------------------------------

    fn take_answers(&mut self, ctx: &egui::Context, state: &Value) {
        let sig = Signature::of(state);
        if self.sig.as_ref() != Some(&sig) {
            self.sig = Some(sig);
            self.want_status();
        }
        if !self.catalog_asked && self.source == Source::Catalog {
            self.load_catalog(ctx, false);
        }
        if let Some(r) = self.catalog_req.as_ref().and_then(Pending::take) {
            self.catalog_req = None;
            match r {
                Ok(v) => {
                    self.catalog_msg = None;
                    self.picker.load(Catalog::from_json(&v));
                }
                Err(e) => {
                    let msg = if e.is_empty() {
                        "Could not load the release list".to_string()
                    } else {
                        e
                    };
                    self.catalog_msg = Some((msg, true));
                }
            }
        }
        if self.status_req.is_none() && self.status_wanted {
            self.status_wanted = false;
            let s = self.server.clone();
            self.status_req = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/firmware/status")));
        }
        if let Some(r) = self.status_req.as_ref().and_then(Pending::take) {
            self.status_req = None;
            match r {
                Ok(v) => self.apply_status(ctx, Status::from_json(&v)),
                Err(e) => {
                    self.line = (
                        if e.is_empty() {
                            "Could not load firmware status".into()
                        } else {
                            e
                        },
                        true,
                    );
                    self.status = None;
                }
            }
        }
        if let Some((kind, req)) = self.action.as_ref() {
            if let Some(r) = req.take() {
                let kind = *kind;
                self.action = None;
                match r {
                    Ok(_) => {
                        self.awaiting = true;
                        self.cancel_visible = true;
                        if kind == Action::Upload {
                            self.line = ("Flashing\u{2026}".into(), false);
                        }
                        self.open_stream(ctx);
                        self.want_status();
                    }
                    Err(e) => {
                        let msg = if e.is_empty() {
                            match kind {
                                Action::Download => "Firmware download failed".to_string(),
                                Action::Upload => "Upload failed".to_string(),
                            }
                        } else {
                            e
                        };
                        self.line = (msg.clone(), true);
                        self.log.push(&msg, "error");
                        self.errors.push(msg);
                    }
                }
            }
        }
        if let Some(r) = self.cancel_req.as_ref().and_then(Pending::take) {
            self.cancel_req = None;
            if let Err(e) = r {
                self.log.push(if e.is_empty() { "Cancel failed" } else { &e }, "error");
            }
        }
        let events: Vec<Value> = self
            .inbox
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for d in events {
            self.apply_event(ctx, &Progress::from_json(&d));
        }
    }

    /// `applyStatus`.
    fn apply_status(&mut self, ctx: &egui::Context, st: Status) {
        self.bar = (st.percent(), st.progress_label());
        self.line = (st.message.clone(), st.state == "failed");
        self.cancel_visible = st.busy();
        if st.busy() || !st.state.is_empty() && st.state != "idle" {
            self.awaiting = false;
        }
        if st.busy() {
            self.open_stream(ctx);
        }
        match model::terminal_note(&st) {
            Some((level, text)) => {
                if self.noted != st.state {
                    self.noted = st.state.clone();
                    match level {
                        "critical" => self.errors.push(text),
                        "warning" => self.warnings.push(text),
                        _ => self.notes.push(text),
                    }
                }
            }
            None => self.noted.clear(),
        }
        self.status = Some(st);
    }

    /// One event of the stream: the bar, the label, the log and Cancel follow
    /// it, and a terminal one re-reads the authoritative status.
    fn apply_event(&mut self, ctx: &egui::Context, p: &Progress) {
        self.bar = (p.percent, p.label.clone());
        if !p.message.is_empty() {
            self.log.push(&p.message, &p.state);
        }
        self.cancel_visible = p.cancellable();
        self.awaiting = false;
        if p.terminal() {
            if p.state == "done" && self.source == Source::Catalog {
                self.load_catalog(ctx, false);
            }
            self.want_status();
        }
    }

    fn telemetry_armed(state: &Value) -> bool {
        state.get("armed") == Some(&Value::Bool(true))
    }

    fn can_start(&self, state: &Value) -> bool {
        let have = match self.source {
            Source::Catalog => self.picker.have_image(),
            Source::File => !self.file_path.trim().is_empty(),
        };
        self.status
            .as_ref()
            .is_some_and(|s| model::can_start(s, Self::telemetry_armed(state), have))
            && self.action.is_none()
            && !self.awaiting
    }

    // ----- drawing -------------------------------------------------------

    /// Draws the page from `y`; returns the bottom.
    pub fn show(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value, _now: f64) -> f32 {
        let ctx = ui.ctx().clone();
        self.take_answers(&ctx, state);
        let y = pagekit::header(
            ui,
            x,
            y,
            "Firmware Flash",
            "Flash PX4 or ArduPilot firmware over a direct USB connection only",
        ) + SECTION_GAP_HEAD;
        let y = self.link_cards(ui, x, y, w, state) + SECTION_GAP;
        self.firmware_card(ui, &ctx, x, y, w, state)
    }

    fn link_cards(&self, ui: &mut egui::Ui, x: f32, y: f32, w: f32, state: &Value) -> f32 {
        let gw = w.min(GRID_MAX_W);
        let cols = (((gw + GRID_GAP) / (GRID_MIN + GRID_GAP)).floor() as usize).clamp(1, 2);
        let cw = (gw - GRID_GAP * (cols as f32 - 1.0)) / cols as f32;
        let painter = ui.painter().clone();
        let slots = [card::reserve(&painter), card::reserve(&painter)];
        let armed = self.status.as_ref().map_or_else(
            || Self::telemetry_armed(state),
            |s| s.armed_now(Self::telemetry_armed(state)),
        );
        let tops: Vec<f32> = if cols == 2 { vec![y, y] } else { vec![y, 0.0] };
        let first = self.connection_card(ui, x, tops[0], cw);
        let second_y = if cols == 2 { y } else { first + GRID_GAP };
        let second_x = if cols == 2 { x + cw + GRID_GAP } else { x };
        let second = self.status_card(ui, second_x, second_y, cw, armed);
        let (b1, b2) = if cols == 2 {
            let b = first.max(second);
            (b, b)
        } else {
            (first, second)
        };
        card::fill(
            &painter,
            slots[0],
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + cw, b1)),
            card::Look::Page,
        );
        card::fill(
            &painter,
            slots[1],
            Rect::from_min_max(Pos2::new(second_x, second_y), Pos2::new(second_x + cw, b2)),
            card::Look::Page,
        );
        b1.max(b2)
    }

    /// `.page-row` pairs: a label at the left and a mono value at the right.
    fn info_row(ui: &egui::Ui, x: f32, y: f32, w: f32, label: &str, value: &str, last: bool) -> f32 {
        let p = ui.painter();
        let lg = style::galley(ui, label, 13.0, Weight::W400, style::text_1(), 0.0);
        let vg = style::galley_mono(ui, value, 13.0, style::text_2());
        let mid = y + 10.0 + 8.75;
        p.galley(Pos2::new(x, mid - lg.size().y / 2.0), lg, style::text_1());
        p.galley(
            Pos2::new(x + w - vg.size().x, mid - vg.size().y / 2.0),
            vg,
            style::text_2(),
        );
        if last {
            y + 37.5
        } else {
            p.hline(
                egui::Rangef::new(x, x + w),
                y + 38.0 - 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
            y + 38.5
        }
    }

    fn connection_card(&self, ui: &egui::Ui, x: f32, y: f32, w: f32) -> f32 {
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Connection");
        let (transport, device) = self
            .status
            .as_ref()
            .map_or(("\u{2014}".to_string(), "\u{2014}".to_string()), |s| {
                (s.transport_row(), s.device_row())
            });
        cy = Self::info_row(ui, ix, cy, iw, "Transport", &transport, false);
        cy = Self::info_row(ui, ix, cy, iw, "Device", &device, true);
        cy + 19.0
    }

    fn status_card(&self, ui: &egui::Ui, x: f32, y: f32, w: f32, armed: bool) -> f32 {
        let (ix, iw) = (x + 19.0, w - 38.0);
        // the title's margin and the first notice's collapse to the larger
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Status") + 2.0;
        let can_flash = self.status.as_ref().is_none_or(|s| s.can_flash);
        if can_flash && !armed {
            cy = message::boxed(ui, ix, cy, iw, "Ready to flash.", Tone::Plain) + 14.0;
        }
        if !can_flash {
            let text = self.status.as_ref().map(Status::gate_text).unwrap_or_default();
            cy = message::boxed(ui, ix, cy, iw, &text, Tone::Warn) + 14.0;
        }
        // the armed notice is the last child, so it keeps no bottom margin
        if armed {
            cy = message::boxed(ui, ix, cy, iw, "Cannot flash while armed. Disarm first.", Tone::Warn);
        }
        cy + 19.0
    }

    fn firmware_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32, state: &Value) -> f32 {
        let painter = ui.painter().clone();
        let bg = card::reserve(&painter);
        let (ix, iw) = (x + 19.0, w - 38.0);
        let mut cy = pagekit::section_title(ui, ix, y + 19.0, iw, "Firmware") + GAP;
        cy = message::boxed(
            ui,
            ix,
            cy,
            iw,
            "Pick PX4 or ArduPilot, a release and your board and Corvus downloads the image for you, or select a local file. Flashing reboots the autopilot into its bootloader and uploads over USB. Keep the USB cable connected.",
            Tone::Plain,
        ) + GAP;

        // the source switch
        let half = (iw - 6.0) / 2.0;
        let bh = SMALL_H;
        let mut next_source = None;
        for (i, (src, label, icon)) in [
            (Source::Catalog, "Download", "cloud-download"),
            (Source::File, "Local file", "folder-open"),
        ]
        .into_iter()
        .enumerate()
        {
            let r = Rect::from_min_size(Pos2::new(ix + i as f32 * (half + 6.0), cy), Vec2::new(half, bh));
            if toggle_button(ui, r, label, Some(icon), self.source == src, ("fwsource", i)) {
                next_source = Some(src);
            }
        }
        cy += bh + GAP;
        if let Some(s) = next_source {
            self.source = s;
            if s == Source::Catalog && !self.catalog_asked {
                self.load_catalog(ctx, false);
            }
        }

        match self.source {
            Source::Catalog => cy = self.catalog_box(ui, ctx, ix, cy, iw) + GAP,
            Source::File => cy = self.file_row(ui, ix, cy, iw) + GAP,
        }

        // Flash and Cancel
        let can = self.can_start(state);
        let label = model::flash_label(self.source);
        let mut flash = Button::new(label, Variant::Primary).icon("cloud-upload").enabled(can);
        flash.pad = Some((11.0, 18.0));
        flash.icon_gap = Some((15.0, 7.0));
        flash.weight = Weight::W700;
        let (fw, fh) = (flash.width(ui), flash.height());
        let mut start = false;
        let mut stop = false;
        let cancel = Button::new("Cancel", Variant::Secondary)
            .icon("x")
            .small()
            .enabled(self.cancel_req.is_none());
        let (cw, ch) = (cancel.width(ui), cancel.height());
        let row_h = if self.cancel_visible { fh.max(ch) } else { fh };
        if flash.show(
            ui,
            Rect::from_min_size(Pos2::new(ix, cy + (row_h - fh) / 2.0), Vec2::new(fw, fh)),
            "fwflash",
        ) {
            start = true;
        }
        if self.cancel_visible
            && cancel.show(
                ui,
                Rect::from_min_size(Pos2::new(ix + iw - cw, cy + (row_h - ch) / 2.0), Vec2::new(cw, ch)),
                "fwcancel",
            )
        {
            stop = true;
        }
        cy += row_h + GAP;
        if start {
            self.start_flash(ctx);
        }
        if stop {
            self.cancel(ctx);
        }

        // the last message
        if !self.line.0.is_empty() {
            let col = if self.line.1 {
                style::critical()
            } else {
                style::text_2()
            };
            let g = kit::wrapped_weight(ui, &self.line.0, 11.5, Weight::W400, col, iw, 1.21);
            let h = g.size().y;
            painter.galley(Pos2::new(ix, cy), g, col);
            cy += h + GAP;
        }

        // the bar and its label
        let bar = Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(iw, 8.0));
        card::paint(
            &painter,
            bar,
            card::Look::custom(style::surface_3(), style::border_soft(), 4),
        );
        let inner = bar.shrink(1.0);
        if self.bar.0 > 0 {
            painter.rect_filled(
                Rect::from_min_size(
                    inner.min,
                    Vec2::new(inner.width() * self.bar.0 as f32 / 100.0, inner.height()),
                ),
                3,
                style::accent(),
            );
        }
        cy += 8.0 + ROW_GAP;
        if !self.bar.1.is_empty() {
            let g = style::galley(ui, &self.bar.1, 12.0, Weight::W400, style::text_2(), 0.0);
            let h = g.size().y;
            painter.galley(Pos2::new(ix, cy), g, style::text_2());
            cy += h;
        }
        cy += GAP;

        // the flash log
        icons::paint(
            ui,
            "info",
            Rect::from_min_size(Pos2::new(ix, cy + 0.5), Vec2::splat(13.0)),
            style::nav(),
        );
        let tg = style::galley(ui, "FLASH LOG", 11.0, Weight::W600, style::text_2(), 0.5);
        let th = tg.size().y;
        painter.galley(Pos2::new(ix + 19.0, cy + (13.0 - th) / 2.0 + 0.5), tg, style::text_2());
        cy += th.max(13.0) + GAP;
        cy = self.log_box(ui, ix, cy, iw);

        card::fill(
            &painter,
            bg,
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + w, cy + 19.0)),
            card::Look::Page,
        );
        cy + 19.0
    }

    /// `.guidance-list`: the newest lines, as many as fit in 200 px.
    fn log_box(&self, ui: &egui::Ui, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let inner_w = w - 2.0 - 20.0;
        let galleys: Vec<_> = self
            .log
            .lines
            .iter()
            .map(|l| {
                let mut job = egui::text::LayoutJob::single_section(
                    l.text.clone(),
                    egui::TextFormat {
                        font_id: style::mono(11.5),
                        color: style::terminal_text(),
                        line_height: Some(11.5 * 1.5),
                        ..Default::default()
                    },
                );
                job.wrap.max_width = inner_w;
                painter.layout_job(job)
            })
            .collect();
        let content: f32 =
            galleys.iter().map(|g| g.size().y).sum::<f32>() + 2.0 * galleys.len().saturating_sub(1) as f32;
        let h = (content + 16.0).min(LOG_MAX_H) + 2.0;
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        card::paint(&painter, rect, card::Look::Console);
        // the list scrolls to its end, so the newest lines are what shows
        let clip = rect.shrink2(Vec2::new(1.0, 1.0));
        let p = painter.with_clip_rect(clip);
        let room = h - 2.0 - 16.0;
        let mut ty = clip.top() + 8.0 - (content - room).max(0.0);
        for g in galleys {
            let gh = g.size().y;
            p.galley(Pos2::new(x + 11.0, ty), g, style::terminal_text());
            ty += gh + 2.0;
        }
        rect.bottom()
    }

    fn file_row(&mut self, ui: &mut egui::Ui, x: f32, y: f32, w: f32) -> f32 {
        let iw = w.min(420.0);
        let resp = ff::input(
            ui,
            x,
            y,
            iw,
            &mut self.file_path,
            "Path to a .px4, .apj or .bin file",
            true,
            false,
            "fwfile",
        );
        let _ = resp;
        let name = if self.file_path.trim().is_empty() {
            "No file selected".to_string()
        } else {
            model::file_name_of(&self.file_path)
        };
        let g = style::galley_mono(ui, &name, 11.5, style::text_2());
        let h = g.size().y;
        ui.painter().galley(
            Pos2::new(x + iw + 10.0, y + (ff::CONTROL_H - h) / 2.0),
            g,
            style::text_2(),
        );
        y + ff::CONTROL_H
    }

    fn catalog_box(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, x: f32, y: f32, w: f32) -> f32 {
        let painter = ui.painter().clone();
        let mut cy = y;

        // the flight stack
        let stack = self.picker.stack.clone();
        let mut next_stack = None;
        cy = ff::field(ui, x, cy, w, "Flight stack", "", |ui, top| {
            let half = (w - 6.0) / 2.0;
            let bh = SMALL_H;
            for (i, (id, label)) in [("px4", "PX4"), ("ardupilot", "ArduPilot")].into_iter().enumerate() {
                let r = Rect::from_min_size(Pos2::new(x + i as f32 * (half + 6.0), top), Vec2::new(half, bh));
                if toggle_button(ui, r, label, None, stack == id, ("fwstack", i)) {
                    next_stack = Some(id);
                }
            }
            bh
        }) + GAP;
        if let Some(id) = next_stack {
            self.picker.pick_stack(id);
        }

        // the release, and Refresh beside it
        let rows = self.picker.release_rows();
        let loaded = self.picker.catalog.is_some();
        let mut refresh = false;
        let mut next_release = None;
        let release = self.picker.release.clone();
        let busy = self.catalog_req.is_some();
        cy = ff::field(ui, x, cy, w, "Release", "", |ui, top| {
            let b = Button::new("Refresh", Variant::Secondary)
                .icon("refresh-cw")
                .small()
                .enabled(!busy);
            let (bw, bh) = (b.width(ui), b.height());
            let sw = (w - ROW_GAP - bw).max(60.0);
            if rows.is_empty() {
                let text = if loaded {
                    "No releases available"
                } else {
                    "Loading releases\u{2026}"
                };
                disabled_select(
                    ui,
                    Rect::from_min_size(Pos2::new(x, top), Vec2::new(sw, ff::CONTROL_H)),
                    text,
                );
            } else {
                let choices: Vec<Choice> = rows.iter().map(|(v, l)| Choice::new(v.clone(), l.clone())).collect();
                next_release = ff::select(ui, x, top, sw, &choices, &release, "fwrelease");
            }
            if b.show(
                ui,
                Rect::from_min_size(
                    Pos2::new(x + w - bw, top + (ff::CONTROL_H - bh) / 2.0),
                    Vec2::new(bw, bh),
                ),
                "fwrefresh",
            ) {
                refresh = true;
            }
            ff::CONTROL_H
        }) + GAP;
        if let Some(tag) = next_release {
            self.picker.pick_release(&tag);
        }
        if refresh {
            self.load_catalog(ctx, true);
        }

        // the manufacturer, the board and the developer builds switch
        let mut next_vendor = None;
        let mut next_board = None;
        let mut toggle = false;
        let vendor = self.picker.vendor.clone();
        let board = self.picker.board.clone();
        let have_boards = !self.picker.boards.is_empty();
        let vendor_rows = self.picker.vendor_rows();
        let board_rows = self.picker.board_rows();
        let variants = self.picker.show_variants;
        let selected = self.picker.selected().cloned();
        let count = self.picker.board_count();
        let hint = "Every release has one image per flight controller. Pick your manufacturer, then the board it made.";
        let mut cy_inner = 0.0;
        cy = ff::field(ui, x, cy, w, "Board", hint, |ui, top| {
            let mut by = top;
            let vb = Button::new("Developer builds", Variant::Secondary).small();
            let vbw = vb.width(ui);
            let lines = flex_wrap(&[(150.0, 1.0), (210.0, 1.7), (vbw, 0.0)], w, ROW_GAP);
            let mut first_line = true;
            for line in &lines {
                if !first_line {
                    by += ROW_GAP;
                }
                first_line = false;
                for &(idx, ox, ow) in line {
                    match idx {
                        0 => {
                            if !have_boards {
                                let t = if loaded {
                                    "Pick a release first"
                                } else {
                                    "Loading manufacturers\u{2026}"
                                };
                                disabled_select(
                                    ui,
                                    Rect::from_min_size(Pos2::new(x + ox, by), Vec2::new(ow, ff::CONTROL_H)),
                                    t,
                                );
                            } else {
                                let mut ch = vec![Choice::new("", "Select manufacturer\u{2026}")];
                                ch.extend(vendor_rows.iter().map(|(v, l)| Choice::new(v.clone(), l.clone())));
                                next_vendor = ff::select(ui, x + ox, by, ow, &ch, &vendor, "fwvendor");
                            }
                        }
                        1 => {
                            if vendor.is_empty() {
                                let t = if have_boards {
                                    "Pick a manufacturer first"
                                } else {
                                    "No boards to choose from"
                                };
                                disabled_select(
                                    ui,
                                    Rect::from_min_size(Pos2::new(x + ox, by), Vec2::new(ow, ff::CONTROL_H)),
                                    t,
                                );
                            } else {
                                let mut ch = vec![Choice::new("", "Select board\u{2026}")];
                                ch.extend(board_rows.iter().map(|(v, l)| Choice::new(v.clone(), l.clone())));
                                next_board = ff::select(ui, x + ox, by, ow, &ch, &board, "fwboard");
                            }
                        }
                        _ => {
                            let vh = vb.height();
                            let r = Rect::from_min_size(
                                Pos2::new(x + ox, by + (ff::CONTROL_H - vh) / 2.0),
                                Vec2::new(vbw, vh),
                            );
                            if toggle_button(ui, r, "Developer builds", None, variants, "fwvariants") {
                                toggle = true;
                            }
                        }
                    }
                }
                by += ff::CONTROL_H;
            }
            // what the chosen image is
            if let Some(b) = &selected {
                by += ROW_GAP;
                by = board_detail(ui, x, by, w, b);
            }
            if !count.is_empty() {
                by += ROW_GAP;
                let g = style::galley(ui, &count, 11.0, Weight::W400, style::text_3(), 0.0);
                let h = g.size().y;
                ui.painter().galley(Pos2::new(x, by), g, style::text_3());
                by += h;
            }
            cy_inner = by - top;
            cy_inner
        }) + GAP;
        let _ = cy_inner;
        if let Some(v) = next_vendor {
            self.picker.pick_vendor(&v);
        }
        if let Some(b) = next_board {
            self.picker.pick_board(&b);
        }
        if toggle {
            self.picker.toggle_variants();
        }

        // what the backend recognised on the USB port
        if let Some(text) = self.picker.detected_text() {
            cy = detected_note(ui, x, cy, w, &text) + GAP;
        }

        // where the list came from
        let (note, warn) = match &self.catalog_msg {
            Some((m, w)) => (m.clone(), *w),
            None => self.picker.catalog_note(),
        };
        if !note.is_empty() {
            let col = if warn { style::warning() } else { style::text_3() };
            let g = kit::wrapped_weight(ui, &note, 11.0, Weight::W400, col, w, 1.5);
            let h = g.size().y;
            painter.galley(Pos2::new(x, cy), g, col);
            cy += h;
        } else {
            cy -= GAP;
        }
        cy
    }
}

/// `.firmware-detected`: the board the backend recognised, in the healthy
/// colours, with a check before the sentence.
fn detected_note(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = kit::wrapped_weight(
        ui,
        text,
        11.5,
        Weight::W400,
        style::healthy(),
        w - 2.0 - 20.0 - 13.0 - 7.0,
        1.5,
    );
    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 16.0 + 2.0));
    let p = ui.painter();
    card::paint(p, r, card::Look::Tone(crate::ui::message::Tone::Ok));
    icons::paint(
        ui,
        "circle-check",
        Rect::from_min_size(Pos2::new(r.left() + 11.0, r.top() + 9.0 + 1.0), Vec2::splat(13.0)),
        style::healthy(),
    );
    p.galley(
        Pos2::new(r.left() + 11.0 + 13.0 + 7.0, r.top() + 9.0),
        g,
        style::healthy(),
    );
    r.bottom()
}

/// `.firmware-board-detail`: the exact file, its size, and the chips that say
/// what a label alone cannot.
fn board_detail(ui: &egui::Ui, x: f32, y: f32, w: f32, b: &model::Board) -> f32 {
    let p = ui.painter();
    let size = model::format_size(b.size);
    let tg = style::galley_mono(ui, &b.name, 10.5, style::text_2());
    let sg = style::galley(ui, &size, 10.5, Weight::W400, style::text_3(), 0.0);
    let mut chips: Vec<(String, Color32, Color32)> = Vec::new();
    if b.is_variant() {
        chips.push((b.variant.clone(), style::text_3(), style::surface_3()));
    }
    if b.peripheral {
        chips.push(("peripheral".into(), style::warning(), style::warning_dark()));
    }
    if b.cached {
        chips.push(("downloaded".into(), style::healthy(), style::healthy_dark()));
    }
    let meta_h = tg.size().y.max(sg.size().y);
    let chip_h = 9.5 * 1.21 + 2.0;
    let h = 2.0 + 16.0 + meta_h + if chips.is_empty() { 0.0 } else { 5.0 + chip_h };
    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
    card::paint(p, r, card::Look::Well);
    let (ix, iw) = (r.left() + 11.0, w - 22.0);
    let top = r.top() + 9.0;
    // the size keeps its width and the file name gives way to it
    let size_w = sg.size().x;
    let room = (iw - size_w - if size_w > 0.0 { 10.0 } else { 0.0 }).max(20.0);
    p.with_clip_rect(Rect::from_min_size(Pos2::new(ix, top), Vec2::new(room, meta_h)))
        .galley(Pos2::new(ix, top + (meta_h - tg.size().y)), tg, style::text_2());
    if size_w > 0.0 {
        p.galley(
            Pos2::new(ix + iw - size_w, top + (meta_h - sg.size().y)),
            sg,
            style::text_3(),
        );
    }
    let mut cx = ix;
    let cy = top + meta_h + 5.0;
    for (text, fg, bgc) in chips {
        let g = style::galley(ui, &text.to_uppercase(), 9.5, Weight::W600, fg, 0.5);
        let cw = g.size().x + 10.0;
        let rect = Rect::from_min_size(Pos2::new(cx, cy), Vec2::new(cw, chip_h));
        p.rect_filled(rect, 255, bgc);
        p.galley(Pos2::new(cx + 5.0, cy + (chip_h - g.size().y) / 2.0), g, fg);
        cx += cw + 5.0;
    }
    r.bottom()
}

/// A `.btn.firmware-source-btn` / `.firmware-stack-btn`: the small secondary
/// button, and when it is the chosen one the accent border, the accent wash
/// and the icon in the bright accent. Returns true when pressed.
fn toggle_button(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    icon: Option<&'static str>,
    active: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    if !active {
        let mut b = Button::new(label, Variant::Secondary).small();
        b.icon = icon;
        return b.show(ui, rect, id);
    }
    let resp = ui.interact(rect, ui.scope_id().with(id), egui::Sense::click());
    let p = ui.painter();
    card::paint(p, rect, card::Look::Chosen);
    let g = style::galley(ui, label, 11.0, Weight::W600, style::text_1(), 0.1);
    let icon_w = if icon.is_some() { 13.0 + 7.0 } else { 0.0 };
    let x = rect.center().x - (icon_w + g.size().x) / 2.0;
    if let Some(name) = icon {
        icons::paint(
            ui,
            name,
            Rect::from_min_size(Pos2::new(x, rect.center().y - 6.5), Vec2::splat(13.0)),
            style::accent_bright(),
        );
    }
    p.galley(
        Pos2::new(x + icon_w, rect.center().y - g.size().y / 2.0),
        g,
        style::text_1(),
    );
    resp.clicked()
}

/// A `.field-select:disabled`: the select at half strength, saying why.
fn disabled_select(ui: &egui::Ui, rect: Rect, text: &str) {
    let p = ui.painter();
    card::paint(
        p,
        rect,
        card::Look::custom(
            style::input(0.6).gamma_multiply(0.5),
            style::border().gamma_multiply(0.5),
            style::RADIUS_BTN,
        ),
    );
    let c = style::text_3().gamma_multiply(0.5);
    let g = style::galley(ui, text, 12.0, Weight::W400, c, 0.0);
    let clip = Rect::from_min_max(rect.min, Pos2::new(rect.right() - 26.0, rect.bottom()));
    ui.painter_at(clip)
        .galley(Pos2::new(rect.left() + 11.0, rect.center().y - g.size().y / 2.0), g, c);
    icons::paint(
        ui,
        "chevron-down",
        Rect::from_center_size(Pos2::new(rect.right() - 14.0, rect.center().y), Vec2::splat(12.0)),
        style::text_3().gamma_multiply(0.5),
    );
}

/// CSS `display: flex; flex-wrap: wrap` for one axis: items of a basis and a
/// grow factor are placed in lines of width `w`, `gap` apart, each line's free
/// space handed out by grow. Returns, per line, `(index, offset, width)`.
pub fn flex_wrap(items: &[(f32, f32)], w: f32, gap: f32) -> Vec<Vec<(usize, f32, f32)>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, (basis, _)) in items.iter().enumerate() {
        let need = if lines.last().is_some_and(|l| !l.is_empty()) {
            gap + basis
        } else {
            *basis
        };
        match lines.last_mut() {
            Some(line) if !line.is_empty() && used + need <= w => {
                line.push(i);
                used += need;
            }
            _ => {
                lines.push(vec![i]);
                used = *basis;
            }
        }
    }
    lines
        .into_iter()
        .map(|line| {
            let bases: f32 = line.iter().map(|&i| items[i].0).sum::<f32>() + gap * (line.len() as f32 - 1.0);
            let grow: f32 = line.iter().map(|&i| items[i].1).sum();
            let free = (w - bases).max(0.0);
            let mut x = 0.0;
            line.into_iter()
                .map(|i| {
                    let width = items[i].0 + if grow > 0.0 { free * items[i].1 / grow } else { 0.0 };
                    let out = (i, x, width);
                    x += width + gap;
                    out
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
#[path = "firmware_flow.rs"]
mod flow;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_items_share_a_wide_row_by_their_grow_factors() {
        let lines = flex_wrap(&[(150.0, 1.0), (210.0, 1.7), (100.0, 0.0)], 800.0, 8.0);
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(l[2].2, 100.0);
        let free = 800.0 - (150.0 + 210.0 + 100.0 + 16.0);
        assert!((l[0].2 - (150.0 + free / 2.7)).abs() < 0.01);
        assert!((l[1].2 - (210.0 + free * 1.7 / 2.7)).abs() < 0.01);
        assert!((l[2].1 + l[2].2 - 800.0).abs() < 0.01);
    }

    #[test]
    fn a_narrow_row_wraps_the_switch_under_the_pair() {
        let lines = flex_wrap(&[(150.0, 1.0), (210.0, 1.7), (110.0, 0.0)], 400.0, 8.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 2);
        assert_eq!(lines[1].len(), 1);
        assert!((lines[0][1].1 + lines[0][1].2 - 400.0).abs() < 0.01);
        assert_eq!(lines[1][0].2, 110.0);
    }

    #[test]
    fn a_very_narrow_row_gives_each_item_its_own_line() {
        let lines = flex_wrap(&[(150.0, 1.0), (210.0, 1.7)], 200.0, 8.0);
        assert_eq!(lines.len(), 2);
    }
}
