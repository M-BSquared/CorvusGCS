//! Windows taken out of the app into windows of their own
//! (`python/src/js/popout.js`, `popout-page.js`).
//!
//! A frame that floats inside the app can go anywhere in the app's window and
//! nowhere else. That is right for a camera over the map and wrong for one on
//! a second monitor. So by default a camera or a terminal opens straight away
//! in a native window of its own, drawn with the very same frame; with
//! Settings, Layout, "Open inside the Corvus window" on, it opens as a frame
//! in the app instead. Either way the frame has a button to go out, and a
//! window out has a button to come back.
//!
//! Where the platform cannot give a second window (egui falls back to
//! embedded viewports), the frame stays in the app.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Vec2};

use crate::ui::style;

static IN_APP: AtomicBool = AtomicBool::new(false);

/// `ui.windows_in_app`: new windows open inside the app.
pub fn in_app() -> bool {
    IN_APP.load(Ordering::Relaxed)
}

static SOLID_TERMINALS: AtomicBool = AtomicBool::new(false);

/// `ui.solid_terminals`: terminal windows keep a solid ground.
pub fn solid_terminals() -> bool {
    SOLID_TERMINALS.load(Ordering::Relaxed)
}

pub fn set_solid_terminals(on: bool) {
    SOLID_TERMINALS.store(on, Ordering::Relaxed);
}

/// Whether a terminal in a window of its own is solid: asked for, or the
/// desktop cannot blur what is behind it.
pub fn solid_window() -> bool {
    solid_terminals() || !super::vibrancy::blurs()
}

pub fn set_in_app(on: bool) {
    IN_APP.store(on, Ordering::Relaxed);
}

/// What the window around a popped-out frame did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Popped {
    /// Its own close button was pressed.
    pub close: bool,
    /// The platform could not make a window: the frame belongs in the app.
    pub embedded: bool,
}

/// The stable id of a window out of the app.
pub fn viewport_id(key: &str) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("corvus-popout", key))
}

/// Draws `body` in a native window of its own, titled `title`. `body` gets
/// the context of that window, whose `content_rect` is the window.
pub fn show<R>(
    ctx: &egui::Context,
    key: &str,
    title: &str,
    size: Vec2,
    mut body: impl FnMut(&egui::Context) -> R,
) -> (R, Popped) {
    let builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size)
        .with_min_inner_size([320.0, 160.0]);
    ctx.show_viewport_immediate(viewport_id(key), builder, |ui, class| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(style::bg()))
            .show(ui, |_| {});
        let r = body(ui.ctx());
        let close = ui.ctx().input(|i| i.viewport().close_requested());
        (
            r,
            Popped {
                close,
                embedded: class == egui::ViewportClass::EmbeddedWindow,
            },
        )
    })
}

/// A window whose title bar is the frame's own bar: the frame draws its
/// tools in it. On macOS the native traffic lights stay and the content
/// runs under the title bar; elsewhere the window has no native frame, so the
/// frame draws the window buttons and `resize_edges` makes it resizable.
pub fn show_titlebar<R>(
    ctx: &egui::Context,
    key: &str,
    title: &str,
    size: Vec2,
    mut body: impl FnMut(&egui::Context) -> R,
) -> (R, Popped) {
    let mut builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size)
        .with_min_inner_size([320.0, 160.0])
        .with_transparent(!solid_window());
    builder = if cfg!(target_os = "macos") {
        builder
            .with_fullsize_content_view(true)
            .with_title_shown(false)
            .with_titlebar_shown(false)
    } else {
        builder.with_decorations(false)
    };
    ctx.show_viewport_immediate(viewport_id(key), builder, |ui, class| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(if solid_window() {
                style::bg()
            } else {
                egui::Color32::TRANSPARENT
            }))
            .show(ui, |_| {});
        if !solid_window() {
            super::vibrancy::apply(title, style::is_dark(), 12.0);
        }
        let r = body(ui.ctx());
        let close = ui.ctx().input(|i| i.viewport().close_requested());
        (
            r,
            Popped {
                close,
                embedded: class == egui::ViewportClass::EmbeddedWindow,
            },
        )
    })
}

/// Drag and resize of a window without a native frame: a press in the 5 px
/// band at the edge starts a resize in that direction.
pub fn resize_edges(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as D;
    use egui::CursorIcon as C;
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
        return;
    }
    let rect = ctx.content_rect();
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    if !rect.contains(p) {
        return;
    }
    let band = 5.0;
    let w = p.x - rect.left() < band;
    let e = rect.right() - p.x < band;
    let n = p.y - rect.top() < band;
    let so = rect.bottom() - p.y < band;
    let (dir, cursor) = match (n, so, w, e) {
        (true, _, true, _) => (D::NorthWest, C::ResizeNorthWest),
        (true, _, _, true) => (D::NorthEast, C::ResizeNorthEast),
        (_, true, true, _) => (D::SouthWest, C::ResizeSouthWest),
        (_, true, _, true) => (D::SouthEast, C::ResizeSouthEast),
        (true, ..) => (D::North, C::ResizeNorth),
        (_, true, ..) => (D::South, C::ResizeSouth),
        (_, _, true, _) => (D::West, C::ResizeWest),
        (_, _, _, true) => (D::East, C::ResizeEast),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
    }
}

/// The pin of a window out of the app (`popout-page.js`): keeps it above the
/// Corvus window. Draws the button at `rect`, flips `pinned` on a press and
/// tells the window's viewport. Returns true when pressed.
pub fn pin_button(ui: &mut egui::Ui, rect: egui::Rect, pinned: &mut bool, id: &str) -> bool {
    let title = if *pinned {
        "Kept above the Corvus window. Click to stop"
    } else {
        "Keep above the Corvus window. Other programs can still cover it"
    };
    let hit = crate::ui::kit::icon_btn(ui, rect, "pin", title, *pinned, ("pin", id));
    if hit {
        *pinned = !*pinned;
        let level = if *pinned {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        };
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_has_one_id_per_key() {
        assert_eq!(viewport_id("term:a"), viewport_id("term:a"));
        assert_ne!(viewport_id("term:a"), viewport_id("video:a"));
    }

    #[test]
    fn new_windows_open_out_of_the_app_unless_asked_not_to() {
        set_in_app(false);
        assert!(!in_app());
        set_in_app(true);
        assert!(in_app());
        set_in_app(false);
    }
}
