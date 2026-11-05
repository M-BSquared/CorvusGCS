//! The floating camera windows (`python/src/js/video-window.js`, `.video-win-*` of
//! panel.css), one per camera, in the same frame as the terminal windows.
//!
//! An RTSP camera is decoded by ffmpeg in the backend and this window asks
//! `GET /api/video/frame` for one JPEG at a time: the next request goes out
//! once the last frame is in, which keeps the decoder running, and closing
//! the window stops asking so the backend stops it a few seconds later. A
//! WebRTC camera is played by the browser's own engine in the web interface;
//! the native interface has none, so its window says so instead of showing a
//! picture it cannot decode.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::net::http::{self, Server};
use crate::setup::video_model::Local;
use crate::ui::card;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// A 16:9 picture under the 34 px bar.
const DEF_SIZE: Vec2 = Vec2::new(560.0, 349.0);
const MIN: Vec2 = Vec2::new(320.0, 160.0);
const RETRY_MS: u64 = 1000;
const MAX_RETRY_MS: u64 = 8000;
/// A frame request waits up to 2 s on the backend; one that has not answered
/// in this long is dead, not slow.
const FRAME_TIMEOUT: Duration = Duration::from_secs(12);

pub const WEBRTC_MESSAGE: &str =
    "WebRTC is not played by the native interface yet. Use an RTSP address for this camera, or the web interface.";

/// Pause after the `failures`th failed attempt: doubled each time.
pub fn backoff_ms(failures: u32) -> u64 {
    (RETRY_MS << failures.saturating_sub(1).min(8)).min(MAX_RETRY_MS)
}

/// The pill's text and tone for a state (`PILL`).
pub fn pill(state: &str) -> (&'static str, &'static str) {
    match state {
        "live" => ("LIVE", "on"),
        "starting" | "idle" => ("CONNECTING", "wait"),
        "stalled" => ("NO PICTURE", "wait"),
        _ => ("OFFLINE", ""),
    }
}

/// The line over a picture that is not live.
pub fn overlay_text(state: &str, message: &str) -> String {
    if state == "live" {
        return String::new();
    }
    if !message.is_empty() {
        return message.to_string();
    }
    if pill(state).1 == "wait" {
        "Connecting to the camera.".into()
    } else {
        "The camera is not sending.".into()
    }
}

#[derive(Default)]
struct Shared {
    frame: Option<egui::ColorImage>,
    local: Local,
    has_frame: bool,
    frame_at: Option<Instant>,
}

impl Shared {
    fn show_state(&mut self, state: &str, message: &str) {
        self.local.state = state.to_string();
        self.local.message = message.to_string();
    }

    /// Records a decoded frame and moves the frame rate (`noteFrameRate`).
    fn push_frame(&mut self, img: egui::ColorImage, now: Instant) {
        self.local.width = img.size[0] as u32;
        self.local.height = img.size[1] as u32;
        if let Some(prev) = self.frame_at {
            let dt = now.duration_since(prev).as_secs_f64();
            if dt > 0.0 {
                self.local.fps = if self.local.fps > 0.0 {
                    self.local.fps * 0.9 + (1.0 / dt) * 0.1
                } else {
                    1.0 / dt
                };
            }
        }
        self.frame_at = Some(now);
        self.frame = Some(img);
        self.has_frame = true;
        if self.local.state != "live" {
            self.show_state("live", "");
        }
    }
}

/// One answer to a frame request.
#[derive(Debug, PartialEq)]
pub enum Answer {
    /// A picture and its frame number.
    Frame(Vec<u8>, Option<u64>),
    /// The camera is gone: stop asking.
    Gone,
    /// A state the backend reports, with the message.
    State(String, String),
    /// Something went wrong: say what, wait, ask again.
    Retry(String),
}

/// Reads one answer of `GET /api/video/frame`.
pub fn ask(server: &Server, id: &str, after: u64) -> Answer {
    let path = format!("/api/video/frame?id={}&after={after}", encode(id));
    let mut r = match http::request(server, "GET", &path, None, "*/*", FRAME_TIMEOUT) {
        Ok(r) => r,
        Err(e) => {
            return Answer::Retry(
                if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
                    "Corvus did not answer in time.".into()
                } else {
                    "Corvus is not answering.".into()
                },
            )
        }
    };
    let is_image = r.header("content-type").is_some_and(|t| t.starts_with("image/"));
    let seq = r.header("x-video-seq").and_then(|s| s.trim().parse().ok());
    let status = r.status;
    let mut body = Vec::new();
    if std::io::Read::read_to_end(&mut r.body, &mut body).is_err() {
        return Answer::Retry("Corvus did not answer in time.".into());
    }
    if (200..300).contains(&status) && is_image {
        return Answer::Frame(body, seq);
    }
    if status == 404 {
        return Answer::Gone;
    }
    let data: Option<Value> = serde_json::from_slice(&body).ok();
    let said = data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !(200..300).contains(&status) || data.is_none() {
        return Answer::Retry(if said.is_empty() {
            "Corvus could not read the camera.".into()
        } else {
            said.into()
        });
    }
    let d = data.unwrap_or(Value::Null);
    let state = d.get("state").and_then(Value::as_str).unwrap_or("error");
    let state = if pill(state).0 == "OFFLINE" && state != "error" && state != "unavailable" {
        "error"
    } else {
        state
    };
    Answer::State(
        state.into(),
        d.get("message").and_then(Value::as_str).unwrap_or("").into(),
    )
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

fn sleep_unless_stopped(stop: &AtomicBool, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while !stop.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Asks for frames until `stop`: one request in flight, always, repeated
/// failures back off.
fn pump(server: Server, id: String, shared: Arc<Mutex<Shared>>, stop: Arc<AtomicBool>, ctx: egui::Context) {
    let mut failures = 0u32;
    let mut after = 0u64;
    let set = |state: &str, message: &str| {
        if let Ok(mut s) = shared.lock() {
            s.show_state(state, message);
        }
        ctx.request_repaint();
    };
    while !stop.load(Ordering::Relaxed) {
        match ask(&server, &id, after) {
            _ if stop.load(Ordering::Relaxed) => return,
            Answer::Frame(bytes, seq) => {
                if let Some(seq) = seq {
                    after = seq;
                }
                let Some(img) = decode(&bytes) else {
                    continue;
                };
                failures = 0;
                if let Ok(mut s) = shared.lock() {
                    s.push_frame(img, Instant::now());
                }
                ctx.request_repaint();
            }
            Answer::Gone => {
                set("error", "This camera is no longer set up.");
                return;
            }
            Answer::Retry(message) => {
                failures += 1;
                set("error", &message);
                sleep_unless_stopped(&stop, backoff_ms(failures));
            }
            Answer::State(state, message) => {
                set(&state, &message);
                if state == "unavailable" {
                    sleep_unless_stopped(&stop, MAX_RETRY_MS);
                }
            }
        }
    }
}

/// One camera's window.
pub struct VideoWindow {
    pub id: String,
    title: String,
    address: String,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    texture: Option<egui::TextureHandle>,
    pos: Pos2,
    size: Vec2,
    maximized: bool,
    /// In a native window of its own, not a frame in the app.
    pub popped: bool,
}

impl Drop for VideoWindow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// What the window asked for this frame.
#[derive(Debug, PartialEq)]
pub enum Out {
    None,
    Close,
}

impl VideoWindow {
    fn new(ctx: &egui::Context, server: &Server, camera: &Value, nth: usize) -> VideoWindow {
        let text = |k: &str| camera.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let id = text("id");
        let name = text("name");
        let address = text("address");
        let title = if !name.is_empty() {
            name
        } else if !address.is_empty() {
            address.clone()
        } else {
            "Camera".into()
        };
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let webrtc = text("kind") == "webrtc";
        if let Ok(mut s) = shared.lock() {
            if webrtc {
                s.show_state("error", WEBRTC_MESSAGE);
            } else {
                s.show_state("starting", "");
            }
        }
        if !webrtc {
            let (server, id, shared, stop, ctx) =
                (server.clone(), id.clone(), shared.clone(), stop.clone(), ctx.clone());
            std::thread::spawn(move || pump(server, id, shared, stop, ctx));
        }
        let screen = ctx.content_rect();
        VideoWindow {
            id,
            title,
            address,
            shared,
            stop,
            texture: None,
            pos: Pos2::new(
                (screen.center().x - DEF_SIZE.x / 2.0 + 28.0 * nth as f32).max(8.0),
                (screen.center().y - DEF_SIZE.y / 2.0 + 28.0 * nth as f32).max(8.0),
            ),
            size: DEF_SIZE,
            maximized: false,
            popped: !crate::app::popout::in_app(),
        }
    }

    /// What the window is showing, for the Video page's rows.
    pub fn local(&self) -> Local {
        self.shared.lock().map(|s| s.local.clone()).unwrap_or_default()
    }

    /// The frame, in the app or in a window of its own.
    fn show(&mut self, ctx: &egui::Context) -> Out {
        if !self.popped {
            return self.frame(ctx, false);
        }
        let key = format!("video:{}", self.id);
        let (title, size) = (self.title.clone(), self.size);
        let (out, popped) = crate::app::popout::show(ctx, &key, &title, size, |c| self.frame(c, true));
        if popped.embedded {
            self.popped = false;
        }
        if popped.close && out == Out::None {
            return Out::Close;
        }
        out
    }

    fn frame(&mut self, ctx: &egui::Context, popped: bool) -> Out {
        let screen = ctx.content_rect();
        let mut out = Out::None;
        let (pos, size) = if popped {
            (screen.min, screen.size())
        } else if self.maximized {
            (screen.min + Vec2::splat(12.0), screen.size() - Vec2::splat(24.0))
        } else {
            let size = self.size.max(MIN).min(screen.size());
            let pos = Pos2::new(
                self.pos.x.clamp(screen.left() - size.x + 80.0, screen.right() - 80.0),
                self.pos.y.clamp(screen.top(), screen.bottom() - 34.0),
            );
            (pos, size)
        };
        let rect = Rect::from_min_size(pos, size);
        let (local, has_frame) = {
            let Ok(mut s) = self.shared.lock() else {
                return Out::None;
            };
            if let Some(img) = s.frame.take() {
                match &mut self.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => {
                        self.texture =
                            Some(ctx.load_texture(format!("video-{}", self.id), img, egui::TextureOptions::LINEAR))
                    }
                }
            }
            (s.local.clone(), s.has_frame)
        };
        let mut dragged = Vec2::ZERO;
        let mut resized = Vec2::ZERO;
        let mut toggle = false;
        egui::Area::new(egui::Id::NULL.with(("videowin", &self.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.set_clip_rect(rect.expand(24.0));
                let painter = ui.painter().clone();
                let bg = painter.add(egui::Shape::Noop);
                let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 34.0));
                let body = Rect::from_min_max(Pos2::new(rect.left() + 1.0, bar.bottom()), rect.max - Vec2::splat(1.0));
                painter.rect_filled(
                    body,
                    egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 11,
                        se: 11,
                    },
                    Color32::BLACK,
                );
                if let Some(t) = &self.texture {
                    let ts = t.size_vec2();
                    if ts.x > 0.0 && ts.y > 0.0 {
                        let scale = (body.width() / ts.x).min(body.height() / ts.y);
                        let img = Rect::from_center_size(body.center(), ts * scale);
                        let tint = if local.state == "live" || !has_frame {
                            Color32::WHITE
                        } else {
                            Color32::from_gray(90)
                        };
                        painter.image(t.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
                    }
                }
                let line = overlay_text(&local.state, &local.message);
                if !line.is_empty() {
                    let g = kit::wrapped_weight(
                        ui,
                        &line,
                        12.0,
                        Weight::W400,
                        Color32::WHITE,
                        body.width() - 32.0 - 24.0,
                        1.45,
                    );
                    let box_rect = Rect::from_center_size(body.center(), g.size() + Vec2::new(24.0, 12.0));
                    painter.rect_filled(box_rect, style::RADIUS_BTN, Color32::from_black_alpha(153));
                    painter.galley(box_rect.min + Vec2::new(12.0, 6.0), g, Color32::WHITE);
                }
                painter.rect_filled(
                    bar,
                    egui::CornerRadius {
                        nw: 12,
                        ne: 12,
                        sw: 0,
                        se: 0,
                    },
                    style::glass(0.6),
                );
                painter.hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, style::border()));
                let bar_resp = ui.interact(bar, ui.scope_id().with("videobar"), egui::Sense::click_and_drag());
                if bar_resp.dragged() && !self.maximized && !popped {
                    dragged = bar_resp.drag_delta();
                }
                if bar_resp.double_clicked() && !popped {
                    toggle = true;
                }
                crate::ui::icons::paint(
                    ui,
                    "video",
                    Rect::from_center_size(Pos2::new(bar.left() + 10.0 + 7.0, bar.center().y), Vec2::splat(14.0)),
                    style::text_2(),
                );
                let tx = bar.left() + 10.0 + 14.0 + 8.0;
                let tools_w = if popped { 2.0 } else { 3.0 } * 24.0 + 1.0 + 6.0;
                let (pill_text, tone) = pill(&local.state);
                let sc = match tone {
                    "on" => style::healthy(),
                    "wait" => style::warning(),
                    _ => style::text_3(),
                };
                let sg = style::galley(ui, pill_text, 10.0, Weight::W600, sc, 0.7);
                let sx = bar.right() - tools_w - 8.0 - sg.size().x;
                painter.galley(Pos2::new(sx, bar.center().y - sg.size().y / 2.0), sg, sc);
                kit::dot(&painter, Pos2::new(sx - 6.0 - 3.5, bar.center().y), sc, tone == "on");
                let info = ui.painter_at(Rect::from_min_max(
                    Pos2::new(tx, bar.top()),
                    Pos2::new(sx - 20.0, bar.bottom()),
                ));
                let ng = style::galley(ui, &self.title, 12.0, Weight::W600, style::text_1(), 0.0);
                info.galley(
                    Pos2::new(tx, bar.center().y - ng.size().y / 2.0),
                    ng.clone(),
                    style::text_1(),
                );
                if !self.address.is_empty() {
                    let hg = style::galley_mono(ui, &self.address, 10.0, style::text_3());
                    info.galley(
                        Pos2::new(tx + ng.size().x + 8.0, bar.center().y - hg.size().y / 2.0 + 1.0),
                        hg,
                        style::text_3(),
                    );
                }
                let mut bx = bar.right() - 6.0 - 24.0;
                if kit::icon_btn(
                    ui,
                    Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0)),
                    "x",
                    "Close the camera window",
                    false,
                    "videoclose",
                ) {
                    out = Out::Close;
                }
                bx -= 25.0;
                if popped {
                    if kit::icon_btn(
                        ui,
                        Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0)),
                        "minimize-2",
                        "Back in the Corvus window",
                        false,
                        "videodock",
                    ) {
                        self.popped = false;
                    }
                } else {
                    if kit::icon_btn(
                        ui,
                        Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0)),
                        if self.maximized { "minimize-2" } else { "maximize-2" },
                        if self.maximized { "Restore" } else { "Maximize" },
                        false,
                        "videomax",
                    ) {
                        toggle = true;
                    }
                    bx -= 25.0;
                    if kit::icon_btn(
                        ui,
                        Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0)),
                        "square-arrow-out-up-right",
                        "Open in a window of its own",
                        false,
                        "videoout",
                    ) {
                        self.popped = true;
                    }
                }
                if !self.maximized && !popped {
                    let grip = Rect::from_min_size(rect.right_bottom() - Vec2::splat(16.0), Vec2::splat(16.0));
                    let gr = ui.interact(grip, ui.scope_id().with("videogrip"), egui::Sense::drag());
                    if gr.hovered() || gr.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                    }
                    if gr.dragged() {
                        resized = gr.drag_delta();
                    }
                    for k in [0.0, 5.0] {
                        painter.line_segment(
                            [
                                Pos2::new(rect.right() - 3.0 - k, rect.bottom() - 13.0),
                                Pos2::new(rect.right() - 13.0, rect.bottom() - 3.0 - k),
                            ],
                            Stroke::new(1.5, style::border_strong()),
                        );
                    }
                }
                painter.set(bg, card::window(rect, 0.97, style::border_strong()));
            });
        if toggle {
            self.maximized = !self.maximized;
        }
        if !self.maximized && !popped {
            self.pos += dragged;
            self.size = (self.size + resized).max(MIN);
        }
        out
    }
}

/// The camera windows, drawn over the whole app.
#[derive(Default)]
pub struct VideoWindows {
    windows: Vec<VideoWindow>,
}

impl VideoWindows {
    /// Opens a camera's window, or raises the one it has.
    pub fn open(&mut self, ctx: &egui::Context, server: &Server, camera: &Value) {
        let id = camera.get("id").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() || self.has(id) {
            return;
        }
        let nth = self.windows.len();
        self.windows.push(VideoWindow::new(ctx, server, camera, nth));
    }

    pub fn close(&mut self, id: &str) {
        self.windows.retain(|w| w.id != id);
    }

    pub fn has(&self, id: &str) -> bool {
        self.windows.iter().any(|w| w.id == id)
    }

    /// The cameras that have a window.
    pub fn ids(&self) -> Vec<String> {
        self.windows.iter().map(|w| w.id.clone()).collect()
    }

    pub fn close_all(&mut self) {
        self.windows.clear();
    }

    /// What a camera's window shows, or none when it has no window.
    pub fn local(&self, id: &str) -> Option<Local> {
        self.windows.iter().find(|w| w.id == id).map(VideoWindow::local)
    }

    /// Draws every window.
    pub fn show(&mut self, ctx: &egui::Context) {
        let mut closed = Vec::new();
        for w in &mut self.windows {
            if w.show(ctx) == Out::Close {
                closed.push(w.id.clone());
            }
        }
        for id in closed {
            self.close(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn the_pause_between_attempts_doubles_to_a_cap() {
        assert_eq!(backoff_ms(1), 1000);
        assert_eq!(backoff_ms(2), 2000);
        assert_eq!(backoff_ms(3), 4000);
        assert_eq!(backoff_ms(4), 8000);
        assert_eq!(backoff_ms(20), 8000);
    }

    #[test]
    fn the_pill_and_the_line_over_the_picture() {
        assert_eq!(pill("live"), ("LIVE", "on"));
        assert_eq!(pill("idle"), ("CONNECTING", "wait"));
        assert_eq!(pill("stalled"), ("NO PICTURE", "wait"));
        assert_eq!(pill("unavailable"), ("OFFLINE", ""));
        assert_eq!(overlay_text("live", "x"), "");
        assert_eq!(overlay_text("starting", ""), "Connecting to the camera.");
        assert_eq!(overlay_text("error", ""), "The camera is not sending.");
        assert_eq!(overlay_text("error", "Refused"), "Refused");
    }

    #[test]
    fn ids_are_percent_encoded() {
        assert_eq!(encode("cam-1_a.b"), "cam-1_a.b");
        assert_eq!(encode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn the_frame_rate_is_smoothed() {
        let mut s = Shared::default();
        let t0 = Instant::now();
        let img = || egui::ColorImage::filled([4, 2], Color32::RED);
        s.push_frame(img(), t0);
        assert_eq!(s.local.fps, 0.0);
        assert_eq!((s.local.width, s.local.height), (4, 2));
        assert_eq!(s.local.state, "live");
        s.push_frame(img(), t0 + Duration::from_millis(100));
        assert!((s.local.fps - 10.0).abs() < 1e-9);
        s.push_frame(img(), t0 + Duration::from_millis(300));
        assert!((s.local.fps - (10.0 * 0.9 + 5.0 * 0.1)).abs() < 1e-9);
    }

    fn serve(answers: Vec<Vec<u8>>) -> (Server, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(l.local_addr().unwrap().port());
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for a in answers {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                seen.push(req.lines().next().unwrap_or("").to_string());
                s.write_all(&a).unwrap();
            }
            seen
        });
        (server, h)
    }

    fn jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([200, 30, 30]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    fn http(status: &str, ctype: &str, extra: &str, body: &[u8]) -> Vec<u8> {
        let mut v = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn a_frame_a_state_and_a_vanished_camera_are_told_apart() {
        let jp = jpeg();
        let (server, h) = serve(vec![
            http("200 OK", "image/jpeg", "X-Video-Seq: 7\r\n", &jp),
            http(
                "200 OK",
                "application/json",
                "",
                br#"{"state":"stalled","message":"No picture for 4 s.","seq":7}"#,
            ),
            http(
                "200 OK",
                "application/json",
                "",
                br#"{"state":"unavailable","message":"x"}"#,
            ),
            http(
                "404 Not Found",
                "application/json",
                "",
                br#"{"error":"no such camera"}"#,
            ),
            http("500 Error", "application/json", "", br#"{"error":"boom"}"#),
        ]);
        match ask(&server, "a b", 3) {
            Answer::Frame(bytes, Some(7)) => {
                let img = decode(&bytes).unwrap();
                assert_eq!(img.size, [8, 6]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            ask(&server, "a b", 7),
            Answer::State("stalled".into(), "No picture for 4 s.".into())
        );
        assert_eq!(ask(&server, "x", 0), Answer::State("unavailable".into(), "x".into()));
        assert_eq!(ask(&server, "x", 0), Answer::Gone);
        assert_eq!(ask(&server, "x", 0), Answer::Retry("boom".into()));
        let seen = h.join().unwrap();
        assert_eq!(seen[0], "GET /api/video/frame?id=a%20b&after=3 HTTP/1.1");
    }

    #[test]
    fn a_dead_server_is_a_retry() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        assert_eq!(
            ask(&Server::local(port), "x", 0),
            Answer::Retry("Corvus is not answering.".into())
        );
    }

    #[test]
    fn the_pump_shows_the_frame_and_stops_when_told() {
        let jp = jpeg();
        let ctx = egui::Context::default();
        let (server, h) = serve(vec![http("200 OK", "image/jpeg", "X-Video-Seq: 1\r\n", &jp)]);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let t = {
            let (s, sh, st) = (server.clone(), shared.clone(), stop.clone());
            std::thread::spawn(move || pump(s, "cam".into(), sh, st, ctx))
        };
        h.join().unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while shared.lock().unwrap().frame.is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        {
            let s = shared.lock().unwrap();
            assert!(s.has_frame);
            assert_eq!((s.local.width, s.local.height), (8, 6));
        }
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap();
    }
}
