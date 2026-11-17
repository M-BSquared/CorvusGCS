//! One SSH session as the terminal sees it: the shell's output from
//! `/api/ssh/stream` into a screen, the operator's keystrokes out through
//! `/api/ssh/send` and the size through `/api/ssh/resize`
//! (`python/src/js/ssh-term.js`, which prefers a WebSocket and falls back to these).
//!
//! Keystrokes leave on one thread, one request at a time, joined while they
//! wait: sent in parallel they could arrive scrambled, and a shell that gets
//! "sl" for "ls" is worse than a slow one. A stream that drops is opened
//! again and the backend replays its buffer, so the screen is cleared first
//! rather than printing the session twice.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use eframe::egui;
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::net::stream::Stream;
use crate::term::emu::Emulator;

enum Msg {
    Data(String),
    Resize(usize, usize),
    Stop,
}

/// A connection the backend holds, drawn into a screen.
pub struct SshSession {
    pub emu: Arc<Mutex<Emulator>>,
    closed: Arc<AtomicBool>,
    stream: Option<Stream>,
    tx: Sender<Msg>,
    sender: Option<JoinHandle<()>>,
    size: (usize, usize),
}

/// `POST /api/ssh/connect`: what the backend answers, or why not.
pub fn connect(ctx: &egui::Context, server: &Server, name: &str) -> Pending<Result<Value, String>> {
    let (s, n) = (server.clone(), name.to_string());
    Pending::spawn(ctx, move || {
        http::post_json(&s, "/api/ssh/connect", &json!({ "name": n }))
    })
}

/// `POST /api/ssh/disconnect`.
pub fn disconnect(ctx: &egui::Context, server: &Server, name: &str) -> Pending<Result<Value, String>> {
    let (s, n) = (server.clone(), name.to_string());
    Pending::spawn(ctx, move || {
        http::post_json(&s, "/api/ssh/disconnect", &json!({ "name": n }))
    })
}

/// The text of an `output` event.
pub fn output_text(data: &str) -> Option<String> {
    serde_json::from_str::<Value>(data)
        .ok()?
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
}

impl SshSession {
    /// Attaches to the session `name`, drawn on a grid of `cols` by `rows`.
    pub fn attach(ctx: &egui::Context, server: &Server, name: &str, cols: usize, rows: usize) -> SshSession {
        let emu = Arc::new(Mutex::new(Emulator::new(cols, rows)));
        let closed = Arc::new(AtomicBool::new(false));
        let (e2, c2, ctx2) = (emu.clone(), closed.clone(), ctx.clone());
        let stream = Stream::start(
            server.clone(),
            format!("/api/ssh/stream?name={}", encode(name)),
            move |ev| {
                match ev.name.as_str() {
                    "output" => {
                        if let (Some(t), Ok(mut e)) = (output_text(&ev.data), e2.lock()) {
                            e.feed(t.as_bytes());
                        }
                    }
                    "closed" => c2.store(true, Ordering::SeqCst),
                    "error" => {
                        if let Ok(mut e) = e2.lock() {
                            e.reset();
                        }
                    }
                    _ => {}
                }
                ctx2.request_repaint();
            },
        );
        let (tx, rx) = mpsc::channel();
        let (s, n) = (server.clone(), name.to_string());
        let sender = std::thread::Builder::new()
            .name("ssh-send".into())
            .spawn(move || send_loop(s, n, rx))
            .ok();
        let session = SshSession {
            emu,
            closed,
            stream: Some(stream),
            tx,
            sender,
            size: (cols, rows),
        };
        let _ = session.tx.send(Msg::Resize(cols, rows));
        session
    }

    /// True once the shell has ended.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Keystrokes for the remote shell.
    pub fn send(&self, data: String) {
        if !data.is_empty() {
            let _ = self.tx.send(Msg::Data(data));
        }
    }

    /// The grid the window shows now, told to the remote pty when it changed.
    /// Sizes below the smallest worth telling a shell are a layout in
    /// progress and are not sent.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if (cols, rows) == self.size || cols < 20 || rows < 4 {
            return;
        }
        self.size = (cols, rows);
        if let Ok(mut e) = self.emu.lock() {
            e.screen.resize(cols, rows);
        }
        let _ = self.tx.send(Msg::Resize(cols, rows));
    }

    /// Stops the threads; the session on the backend stays up.
    pub fn close(&mut self) {
        if let Some(mut s) = self.stream.take() {
            s.stop();
        }
        let _ = self.tx.send(Msg::Stop);
        if let Some(t) = self.sender.take() {
            let _ = t.join();
        }
    }
}

impl corvus_api::ui::term::Shell for SshSession {
    fn emulator(&self) -> Arc<Mutex<Emulator>> {
        self.emu.clone()
    }

    fn send(&mut self, data: String) {
        SshSession::send(self, data);
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        SshSession::resize(self, cols, rows);
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        self.close();
    }
}

fn send_loop(server: Server, name: String, rx: Receiver<Msg>) {
    let mut resize: Option<(usize, usize)> = None;
    loop {
        let first = match rx.recv() {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut data = String::new();
        let mut stop = false;
        let mut take = |m: Msg, data: &mut String, resize: &mut Option<(usize, usize)>| match m {
            Msg::Data(d) => data.push_str(&d),
            Msg::Resize(c, r) => *resize = Some((c, r)),
            Msg::Stop => stop = true,
        };
        take(first, &mut data, &mut resize);
        while let Ok(m) = rx.try_recv() {
            take(m, &mut data, &mut resize);
        }
        if stop {
            return;
        }
        if let Some((cols, rows)) = resize.take() {
            let _ = http::post_json(
                &server,
                "/api/ssh/resize",
                &json!({ "name": name, "cols": cols, "rows": rows }),
            );
        }
        if !data.is_empty() {
            let _ = http::post_json(&server, "/api/ssh/send", &json!({ "name": name, "data": data }));
        }
    }
}

/// `encodeURIComponent` for a session name.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_encoded_like_the_web_does() {
        assert_eq!(encode("my pi/1"), "my%20pi%2F1");
        assert_eq!(
            output_text(r#"{"text":"hi\r\n","name":"x"}"#).as_deref(),
            Some("hi\r\n")
        );
    }
}
