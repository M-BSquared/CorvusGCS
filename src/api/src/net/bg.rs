//! A request under way, its answer picked up by the frame that finds it there.
//! The interface never blocks on the network. In the program a thread does the
//! work; in a plugin the program does it and delivers the answer.

use std::sync::{Arc, Mutex};

/// An answer still on its way.
pub struct Pending<T>(Arc<Mutex<Option<T>>>);

/// The other end of a [`Pending`] that no thread of its own fills.
pub struct Filler<T>(Arc<Mutex<Option<T>>>);

impl<T> Filler<T> {
    pub fn fill(self, value: T) {
        if let Ok(mut s) = self.0.lock() {
            *s = Some(value);
        }
    }
}

impl<T: Send + 'static> Pending<T> {
    /// Runs `f` on a thread and repaints when it has answered.
    #[cfg(not(target_family = "wasm"))]
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Pending<T> {
        let (pending, filler) = Pending::slot();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            filler.fill(f());
            ctx.request_repaint();
        });
        pending
    }

    /// An answer someone else brings.
    pub fn slot() -> (Pending<T>, Filler<T>) {
        let slot = Arc::new(Mutex::new(None));
        (Pending(slot.clone()), Filler(slot))
    }

    /// An answer that is already here (tests build pages with it).
    pub fn ready(v: T) -> Pending<T> {
        Pending(Arc::new(Mutex::new(Some(v))))
    }

    /// The answer, once, when it is there.
    pub fn take(&self) -> Option<T> {
        self.0.lock().ok().and_then(|mut s| s.take())
    }
}

/// The wait for a [`Pending`] in a [`Task`](crate::task::Task), which polls again after every
/// delivery, so nobody is woken. A type of its own: an answer nobody waits for is dropped like
/// any other value.
pub struct Wait<T>(Pending<T>);

impl<T: Send + 'static> Pending<T> {
    pub fn wait(self) -> Wait<T> {
        Wait(self)
    }
}

impl<T: Send + 'static> std::future::Future for Wait<T> {
    type Output = T;

    fn poll(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<T> {
        match self.0.take() {
            Some(v) => std::task::Poll::Ready(v),
            None => std::task::Poll::Pending,
        }
    }
}
