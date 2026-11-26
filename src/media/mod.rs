//! The media cards: a picture or a video of a file on this computer, in a frame
//! over the window. A plugin asks for one with `Host::show_file` (Map Entities,
//! for what was recorded at a place); the program reads the file, the plugin
//! never sees its bytes.

pub mod exif;
pub mod scan;
pub mod source;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use self::source::{Kind, Player, Start};
use crate::net::http::{self, Server};
use crate::net::lock::Relock;
use crate::ui::card;
use crate::ui::kit;
use crate::ui::style::{self, Weight};
use crate::ui::tip::HoverTr;

const DEF_SIZE: Vec2 = Vec2::new(640.0, 450.0);
const MIN: Vec2 = Vec2::new(320.0, 220.0);
const BAR: f32 = 34.0;
const CONTROLS: f32 = 38.0;
/// The most cards at once; one more closes the oldest. Each may hold a large picture.
pub const CARDS_MAX: usize = 4;
/// The longest side of a picture on the graphics card.
const TEXTURE_SIDE_MAX: usize = 4096;

pub const OTHER_KIND: &str =
    "Corvus GCS shows pictures and videos. This file is of another kind: open its folder to use it.";
pub const LOADING: &str = "Loading…";
pub const FOLDER_FAILED: &str = "The folder could not be opened.";
pub const SYSTEM_FAILED: &str = "The system could not open the file.";

type Loaded = Arc<Mutex<Option<Result<egui::ColorImage, &'static str>>>>;

/// Where a video stands.
#[derive(Default)]
pub struct Video {
    player: Option<Player>,
    /// Where it was paused or stopped.
    at: f64,
    duration: Option<f64>,
    pub playing: bool,
    ended: bool,
}

enum Content {
    Picture {
        loaded: Loaded,
        thread: Option<JoinHandle<()>>,
    },
    Video(Video),
    /// A file Corvus does not show.
    Other,
    Failed(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Act {
    Close,
    Maximize,
    Folder,
    System,
    PlayPause,
    Seek(f64),
}

pub struct MediaCard {
    serial: u64,
    /// The path as it was asked for: the same one again raises this card.
    asked: String,
    path: Option<PathBuf>,
    title: String,
    content: Content,
    texture: Option<egui::TextureHandle>,
    /// What a button of the bar could not do.
    note: Arc<Mutex<Option<&'static str>>>,
    pos: Pos2,
    size: Vec2,
    maximized: bool,
    raise: bool,
}

/// `m:ss`, with hours when there are any.
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn finder(server: &Server) -> source::Find {
    let server = server.clone();
    Box::new(move || {
        // The ffmpeg the operator set for the cameras, or the one found.
        let configured = http::get_json(&server, "/api/video/status")
            .ok()
            .and_then(|v| v.pointer("/ffmpeg/configured")?.as_str().map(str::to_string))
            .unwrap_or_default();
        corvus_core::peripherals::video::ffmpeg::resolve_ffmpeg(&configured).0
    })
}

impl MediaCard {
    /// A player from where `v` stands; one picture only while paused.
    fn player(ctx: &egui::Context, server: &Server, path: &std::path::Path, v: &Video) -> Player {
        let start = Start {
            at: v.at,
            still: !v.playing,
            duration: v.duration,
        };
        Player::start(ctx, path, finder(server), start)
    }

    fn new(ctx: &egui::Context, server: &Server, asked: &str, title: &str, serial: u64, nth: usize) -> MediaCard {
        let found = source::local_file(asked);
        let content = match &found {
            Err(why) => Content::Failed(why),
            Ok(path) => match source::kind_of(path) {
                Kind::Picture => {
                    let loaded: Loaded = Arc::default();
                    let side = ctx.input(|i| i.max_texture_side).min(TEXTURE_SIDE_MAX) as u32;
                    let (out, path, ctx) = (loaded.clone(), path.clone(), ctx.clone());
                    let thread = std::thread::Builder::new()
                        .name("media-picture".into())
                        .spawn(move || {
                            let result = crate::crash::guarded(|| source::picture(&path, side))
                                .unwrap_or(Err(source::UNREADABLE));
                            *out.relock() = Some(result);
                            ctx.request_repaint();
                        })
                        .ok();
                    Content::Picture { loaded, thread }
                }
                kind @ (Kind::Video | Kind::OtherPicture) => {
                    let mut v = Video {
                        playing: kind == Kind::Video,
                        ..Video::default()
                    };
                    v.player = Some(MediaCard::player(ctx, server, path, &v));
                    Content::Video(v)
                }
                Kind::Other => Content::Other,
            },
        };
        let path = found.ok();
        let name = path
            .as_deref()
            .map(corvus_api::ui::filepicker::file_name)
            .unwrap_or_default();
        let title = match (title.trim(), name.as_str()) {
            ("", "") => asked.to_string(),
            ("", n) => n.to_string(),
            (t, "") => t.to_string(),
            (t, n) => format!("{t} \u{b7} {n}"),
        };
        let screen = ctx.content_rect();
        let size = DEF_SIZE.min(screen.size() - Vec2::splat(24.0)).max(MIN);
        let step = 28.0 * nth as f32;
        MediaCard {
            serial,
            asked: asked.to_string(),
            path,
            title,
            content,
            texture: None,
            note: Arc::default(),
            pos: Pos2::new(
                (screen.center().x - size.x / 2.0 + step).max(8.0),
                (screen.center().y - size.y / 2.0 + step).max(8.0),
            ),
            size,
            maximized: false,
            raise: true,
        }
    }

    /// Takes in what the threads made.
    fn poll(&mut self, ctx: &egui::Context) {
        let mut fresh = None;
        let read = match &self.content {
            Content::Picture { loaded, .. } => loaded.relock().take(),
            _ => None,
        };
        match read {
            Some(Ok(img)) => fresh = Some(img),
            Some(Err(why)) => self.content = Content::Failed(why),
            None => {}
        }
        match &mut self.content {
            Content::Video(v) => {
                if let Some(player) = &v.player {
                    let (ended, error, duration) = {
                        let mut p = player.shared.relock();
                        fresh = p.frame.take();
                        (p.ended, p.error, p.duration)
                    };
                    v.duration = duration.or(v.duration);
                    if let Some(why) = error {
                        self.content = Content::Failed(why);
                    } else if ended {
                        if v.playing {
                            v.at = v.duration.unwrap_or_else(|| player.position());
                            v.playing = false;
                            v.ended = true;
                        }
                        v.player = None;
                    }
                }
            }
            Content::Picture { .. } | Content::Other | Content::Failed(_) => {}
        }
        if let Some(img) = fresh {
            match &mut self.texture {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture(format!("media-{}", self.serial), img, egui::TextureOptions::LINEAR));
                }
            }
        }
    }

    /// Play, pause and seek.
    fn control(&mut self, ctx: &egui::Context, server: &Server, act: Act) {
        let (Content::Video(v), Some(path)) = (&mut self.content, &self.path) else {
            return;
        };
        match act {
            Act::PlayPause if v.playing => {
                if let Some(p) = v.player.take() {
                    v.at = p.position();
                }
                v.playing = false;
            }
            Act::PlayPause => {
                if v.ended {
                    v.at = 0.0;
                }
                v.ended = false;
                v.playing = true;
                v.player = None;
                v.player = Some(MediaCard::player(ctx, server, path, v));
            }
            Act::Seek(at) => {
                // The very end has no picture to show.
                v.at = at.clamp(0.0, (v.duration.unwrap_or(at) - 0.1).max(0.0));
                v.ended = false;
                v.player = None;
                v.player = Some(MediaCard::player(ctx, server, path, v));
            }
            _ => {}
        }
    }

    /// In a thread: a system's opener may take a moment.
    fn hand_over(&self, ctx: &egui::Context, folder: bool) -> Option<JoinHandle<()>> {
        let path = self.path.clone()?;
        let (note, ctx) = (self.note.clone(), ctx.clone());
        let thread = std::thread::Builder::new().name("media-open".into()).spawn(move || {
            use corvus_core::plugins::file_manager;
            let failed = if folder {
                let parent = path
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (!file_manager::open_folder(&parent).0).then_some(FOLDER_FAILED)
            } else {
                (!file_manager::open_file(&path)).then_some(SYSTEM_FAILED)
            };
            *note.relock() = failed;
            ctx.request_repaint();
        });
        thread.ok()
    }

    /// Draws the card; false once it is closed. A thread it starts goes to `threads`.
    fn show(&mut self, ctx: &egui::Context, server: &Server, threads: &mut Vec<JoinHandle<()>>) -> bool {
        self.poll(ctx);
        let screen = ctx.content_rect();
        let rect = if self.maximized {
            screen.shrink(12.0)
        } else {
            let size = self.size.min(screen.size()).max(MIN);
            let x = self.pos.x.clamp(screen.left() + 64.0 - size.x, screen.right() - 64.0);
            let y = self
                .pos
                .y
                .clamp(screen.top(), (screen.bottom() - BAR).max(screen.top()));
            Rect::from_min_size(Pos2::new(x, y), size)
        };
        let id = egui::Id::NULL.with(("media-card", self.serial));
        let layer = egui::LayerId::new(egui::Order::Middle, id);
        if std::mem::take(&mut self.raise) {
            ctx.move_to_top(layer);
        }
        let mut acts = Vec::new();
        let mut dragged = Vec2::ZERO;
        let mut resized = Vec2::ZERO;
        egui::Area::new(id)
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.set_min_size(rect.size());
                ui.set_clip_rect(screen);
                // Presses on the card stay with it.
                ui.interact(rect, ui.scope_id().with("mediaback"), egui::Sense::click_and_drag());
                let painter = ui.painter().clone();
                painter.add(card::window("mediawin", rect, 0.97, style::border_strong()));
                let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), BAR));
                painter.hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, style::border()));
                let bar_resp = ui.interact(bar, ui.scope_id().with("mediabar"), egui::Sense::click_and_drag());
                if bar_resp.dragged() && !self.maximized {
                    dragged = bar_resp.drag_delta();
                }
                if bar_resp.double_clicked() {
                    acts.push(Act::Maximize);
                }
                crate::ui::icons::paint(
                    ui,
                    "file-image",
                    Rect::from_center_size(Pos2::new(bar.left() + 17.0, bar.center().y), Vec2::splat(14.0)),
                    style::text_2(),
                );
                let mut bx = bar.right() - 6.0 - 24.0;
                let size = if self.maximized {
                    ("minimize-2", "Restore")
                } else {
                    ("maximize-2", "Maximize")
                };
                let mut tools = vec![
                    ("x", "Close the file window", Act::Close),
                    (size.0, size.1, Act::Maximize),
                ];
                if self.path.is_some() {
                    tools.push(("folder-open", "Show the file's folder", Act::Folder));
                }
                if self.path.as_deref().is_some_and(source::system_may_open) {
                    tools.push((
                        "square-arrow-out-up-right",
                        "Open with the system's program",
                        Act::System,
                    ));
                }
                for (icon, title, act) in tools {
                    let r = Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0));
                    if kit::icon_btn(ui, r, icon, title, false, ("mediatool", icon)) {
                        acts.push(act);
                    }
                    bx -= 25.0;
                }
                let tx = bar.left() + 32.0;
                let room = Rect::from_min_max(Pos2::new(tx, bar.top()), Pos2::new(bx + 18.0, bar.bottom()));
                // A file's name is not a text of the interface: it stays as it is.
                let g = painter.layout_no_wrap(self.title.clone(), style::font(12.0, Weight::W600), style::text_1());
                painter.with_clip_rect(room).galley(
                    Pos2::new(tx, bar.center().y - g.size().y / 2.0),
                    g,
                    style::text_1(),
                );
                self.body(ui, rect, &mut acts);
                if !self.maximized {
                    let grip = Rect::from_min_size(rect.right_bottom() - Vec2::splat(16.0), Vec2::splat(16.0));
                    let gr = ui.interact(grip, ui.scope_id().with("mediagrip"), egui::Sense::drag());
                    if gr.hovered() || gr.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                    }
                    if gr.dragged() {
                        resized = gr.drag_delta();
                    }
                    for k in [0.0, 5.0] {
                        ui.painter().line_segment(
                            [
                                Pos2::new(rect.right() - 3.0 - k, rect.bottom() - 13.0),
                                Pos2::new(rect.right() - 13.0, rect.bottom() - 3.0 - k),
                            ],
                            Stroke::new(1.5, style::border_strong()),
                        );
                    }
                }
            });
        if !self.maximized && (dragged != Vec2::ZERO || resized != Vec2::ZERO) {
            self.pos = rect.min + dragged;
            self.size = (rect.size() + resized).max(MIN);
        }
        for act in acts {
            match act {
                Act::Close => return false,
                Act::Maximize => self.maximized = !self.maximized,
                Act::Folder => threads.extend(self.hand_over(ctx, true)),
                Act::System => threads.extend(self.hand_over(ctx, false)),
                Act::PlayPause | Act::Seek(_) => self.control(ctx, server, act),
            }
        }
        true
    }

    /// The picture, what is said instead of one, and a video's controls.
    fn body(&self, ui: &mut egui::Ui, rect: Rect, acts: &mut Vec<Act>) {
        let painter = ui.painter().clone();
        let video = match &self.content {
            Content::Video(v) if v.duration.is_some() => Some(v),
            _ => None,
        };
        let foot = if video.is_some() { CONTROLS } else { 0.0 };
        let body = Rect::from_min_max(
            Pos2::new(rect.left() + 1.0, rect.top() + BAR),
            Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0 - foot),
        );
        let low = if foot == 0.0 { 11 } else { 0 };
        let corners = egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: low,
            se: low,
        };
        painter.rect_filled(body, corners, Color32::BLACK);
        let shows_picture = !matches!(self.content, Content::Failed(_) | Content::Other);
        if let (Some(t), true) = (&self.texture, shows_picture) {
            let ts = t.size_vec2();
            if ts.x > 0.0 && ts.y > 0.0 {
                let scale = (body.width() / ts.x).min(body.height() / ts.y);
                let at = Rect::from_center_size(body.center(), ts * scale);
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                painter.image(t.id(), at, uv, Color32::WHITE);
            }
        }
        let note = *self.note.relock();
        let said = match &self.content {
            Content::Failed(why) => Some(*why),
            Content::Other => Some(OTHER_KIND),
            _ => note.or_else(|| self.texture.is_none().then_some(LOADING)),
        };
        if let Some(said) = said {
            let g = kit::wrapped_weight(
                ui,
                said,
                12.0,
                Weight::W400,
                Color32::WHITE,
                (body.width() - 56.0).max(40.0),
                1.45,
            );
            let box_ = Rect::from_center_size(body.center(), g.size() + Vec2::new(24.0, 12.0));
            painter.rect_filled(box_, style::RADIUS_BTN, Color32::from_black_alpha(170));
            painter.galley(box_.min + Vec2::new(12.0, 6.0), g, Color32::WHITE);
        }
        let Some(v) = video else { return };
        let duration = v.duration.unwrap_or(0.0);
        let at = match &v.player {
            Some(p) if v.playing => p.position(),
            _ => v.at,
        }
        .min(duration);
        let row = Rect::from_min_max(
            Pos2::new(body.left(), body.bottom()),
            Pos2::new(body.right(), rect.bottom()),
        );
        let button = Rect::from_min_size(Pos2::new(row.left() + 6.0, row.center().y - 14.0), Vec2::splat(28.0));
        let (icon, title) = if v.playing {
            ("pause", "Pause")
        } else {
            ("play", "Play")
        };
        if kit::icon_btn(ui, button, icon, title, false, "mediaplay") {
            acts.push(Act::PlayPause);
        }
        let time = format!("{} / {}", clock(at), clock(duration));
        let tg = style::galley_mono(ui, &time, 11.0, style::text_2());
        // Room for the grip in the corner.
        let time_x = row.right() - 20.0 - tg.size().x;
        painter.galley(
            Pos2::new(time_x, row.center().y - tg.size().y / 2.0),
            tg,
            style::text_2(),
        );
        let track = Rect::from_min_max(
            Pos2::new(button.right() + 10.0, row.center().y - 9.0),
            Pos2::new(time_x - 10.0, row.center().y + 9.0),
        );
        if track.width() < 20.0 {
            return;
        }
        let resp = ui
            .interact(track, ui.scope_id().with("mediaseek"), egui::Sense::click_and_drag())
            .on_hover_tr("Jump to a time");
        let under = resp
            .interact_pointer_pos()
            .map(|p| f64::from(((p.x - track.left()) / track.width()).clamp(0.0, 1.0)) * duration);
        // One jump when the pointer is let go, not one ffmpeg a frame of the drag.
        if resp.clicked() || resp.drag_stopped() {
            if let Some(to) = under {
                acts.push(Act::Seek(to));
            }
        }
        let shown = under.filter(|_| resp.dragged()).unwrap_or(at);
        let line = Rect::from_center_size(track.center(), Vec2::new(track.width(), 4.0));
        painter.rect_filled(line, 2.0, style::border_strong());
        let x = line.left() + line.width() * (shown / duration.max(0.001)) as f32;
        painter.rect_filled(
            Rect::from_min_max(line.min, Pos2::new(x, line.bottom())),
            2.0,
            style::accent(),
        );
        painter.circle_filled(Pos2::new(x, line.center().y), 6.0, style::accent());
    }
}

/// The media cards that are open.
pub struct MediaCards {
    server: Server,
    cards: Vec<MediaCard>,
    made: u64,
    /// Threads still at work, such as a picture being read for a card that was closed:
    /// joined once done.
    closing: Vec<JoinHandle<()>>,
}

impl MediaCards {
    pub fn new(server: Server) -> MediaCards {
        MediaCards {
            server,
            cards: Vec::new(),
            made: 0,
            closing: Vec::new(),
        }
    }

    /// Shows the file at `path`, headed `title`; the card it has already is raised.
    pub fn open(&mut self, ctx: &egui::Context, path: &str, title: &str) {
        if let Some(i) = self.cards.iter().position(|c| c.asked == path) {
            // A file that could not be shown is tried again.
            if matches!(self.cards[i].content, Content::Failed(_)) {
                self.close_at(i);
            } else {
                self.cards[i].raise = true;
                ctx.request_repaint();
                return;
            }
        }
        while self.cards.len() >= CARDS_MAX {
            self.close_at(0);
        }
        self.made += 1;
        let card = MediaCard::new(ctx, &self.server, path, title, self.made, self.cards.len());
        self.cards.push(card);
        ctx.request_repaint();
    }

    fn close_at(&mut self, i: usize) {
        if let Content::Picture { thread, .. } = &mut self.cards[i].content {
            self.closing.extend(thread.take());
        }
        self.cards.remove(i);
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let mut gone = Vec::new();
        for (i, c) in self.cards.iter_mut().enumerate() {
            if !c.show(ctx, &self.server, &mut self.closing) {
                gone.push(i);
            }
        }
        for i in gone.into_iter().rev() {
            self.close_at(i);
        }
        let (done, going): (Vec<_>, Vec<_>) = std::mem::take(&mut self.closing)
            .into_iter()
            .partition(JoinHandle::is_finished);
        for t in done {
            let _ = t.join();
        }
        self.closing = going;
    }

    /// Ends every player and waits for every thread.
    pub fn shut_down(&mut self) {
        while !self.cards.is_empty() {
            self.close_at(0);
        }
        for t in self.closing.drain(..) {
            let _ = t.join();
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    #[cfg(test)]
    pub fn titles(&self) -> Vec<&str> {
        self.cards.iter().map(|c| c.title.as_str()).collect()
    }
}

impl Drop for MediaCards {
    fn drop(&mut self) {
        self.shut_down();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn frame(cards: &mut MediaCards, ctx: &egui::Context, time: &mut f64) {
        *time += 1.0 / 60.0;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
            time: Some(*time),
            ..egui::RawInput::default()
        };
        let mut out = ctx.run_ui(raw, |ui| cards.show(ui.ctx()));
        out.textures_delta.set.clear();
        out.textures_delta.free.clear();
    }

    fn cards() -> (MediaCards, egui::Context) {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::fonts::install(&ctx);
        crate::ui::style::apply(&ctx);
        // Nothing listens there: the ffmpeg that is found is the one used.
        (MediaCards::new(Server::local(9)), ctx)
    }

    #[test]
    fn the_clock_has_hours_only_when_needed() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(75.9), "1:15");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(-1.0), "0:00");
    }

    #[test]
    fn a_picture_comes_up_in_its_card_and_the_same_file_again_raises_it() {
        let file = std::env::temp_dir().join(format!("corvus-card-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(64, 32, image::Rgba([200, 10, 30, 255]))
            .save(&file)
            .unwrap();
        let path = file.to_string_lossy().into_owned();
        let (mut cards, ctx) = cards();
        let mut time = 0.0;
        cards.open(&ctx, &path, "Gate 1");
        let end = Instant::now() + Duration::from_secs(10);
        while cards.cards[0].texture.is_none() {
            assert!(Instant::now() < end, "the picture did not come");
            frame(&mut cards, &ctx, &mut time);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(cards.cards[0].texture.as_ref().unwrap().size(), [64, 32]);
        assert!(cards.titles()[0].starts_with("Gate 1 \u{b7} corvus-card-"));
        cards.open(&ctx, &path, "Gate 1");
        assert_eq!(cards.len(), 1);
        for n in 0..CARDS_MAX + 2 {
            // A full path on every system, to a file that is not there.
            let gone = std::env::temp_dir().join(format!("corvus-no-such-file-{n}.png"));
            cards.open(&ctx, &gone.to_string_lossy(), "");
            frame(&mut cards, &ctx, &mut time);
        }
        assert_eq!(cards.len(), CARDS_MAX);
        assert!(matches!(cards.cards[0].content, Content::Failed(source::NOT_THERE)));
        cards.shut_down();
        assert_eq!(cards.len(), 0);
        std::fs::remove_file(file).unwrap();
    }

    /// With the ffmpeg of the computer the tests run on.
    #[test]
    fn a_video_plays_pauses_and_jumps() {
        use corvus_core::peripherals::video::ffmpeg;
        let (found, _) = ffmpeg::resolve_ffmpeg("");
        if found.is_empty() {
            eprintln!("no ffmpeg here: playing a video is not tested");
            return;
        }
        let file = std::env::temp_dir().join(format!("corvus-card-{}.mp4", std::process::id()));
        let made = ffmpeg::command(&found)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=25:duration=3",
            ])
            .arg(&file)
            .status()
            .unwrap();
        assert!(made.success());
        let (mut cards, ctx) = cards();
        let mut time = 0.0;
        cards.open(&ctx, &file.to_string_lossy(), "");
        let server = cards.server.clone();
        let mut until = |cards: &mut MediaCards, what: &str, done: &dyn Fn(&MediaCard) -> bool| {
            let end = Instant::now() + Duration::from_secs(20);
            while !done(&cards.cards[0]) {
                assert!(Instant::now() < end, "timed out waiting for {what}");
                frame(cards, &ctx, &mut time);
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let video = |c: &MediaCard| match &c.content {
            Content::Video(v) => (v.playing, v.ended, v.at, v.duration, v.player.is_some()),
            _ => panic!("not a video any more"),
        };
        until(&mut cards, "a picture", &|c| {
            c.texture.is_some() && video(c).3.is_some()
        });
        cards.cards[0].control(&ctx, &server, Act::PlayPause);
        let (playing, _, paused_at, _, running) = video(&cards.cards[0]);
        assert!(!playing && !running && paused_at < 2.5, "{paused_at}");
        cards.cards[0].control(&ctx, &server, Act::Seek(2.0));
        until(&mut cards, "the picture at two seconds", &|c| !video(c).4);
        assert_eq!(video(&cards.cards[0]).2, 2.0);
        cards.cards[0].control(&ctx, &server, Act::PlayPause);
        until(&mut cards, "the end", &|c| video(c).1);
        let (playing, ended, at, duration, running) = video(&cards.cards[0]);
        assert!(!playing && ended && !running);
        assert_eq!(Some(at), duration);
        // Play at the end starts over.
        cards.cards[0].control(&ctx, &server, Act::PlayPause);
        assert_eq!(video(&cards.cards[0]).2, 0.0);
        cards.shut_down();
        std::fs::remove_file(file).unwrap();
    }
}
