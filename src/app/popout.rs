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
