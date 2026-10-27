//! A request on a thread of its own, its answer picked up by the frame that
//! finds it there. The interface never blocks on the network.

use std::sync::{Arc, Mutex};

use eframe::egui;

/// An answer still on its way.
pub struct Pending<T>(Arc<Mutex<Option<T>>>);

impl<T: Send + 'static> Pending<T> {
    /// Runs `f` on a thread and repaints when it has answered.
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Pending<T> {
        let slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let v = f();
            if let Ok(mut s) = out.lock() {
                *s = Some(v);
            }
            ctx.request_repaint();
        });
        Pending(slot)
    }

    /// The answer, once, when it is there.
    pub fn take(&self) -> Option<T> {
        self.0.lock().ok().and_then(|mut s| s.take())
    }
}
