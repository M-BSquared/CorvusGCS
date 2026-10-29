//! One SSH session as the terminal sees it: the shell's output from
//! `/api/ssh/stream` into a screen, the operator's keystrokes out through
//! `/api/ssh/send` and the size through `/api/ssh/resize`
//! (`src/js/ssh-term.js`, which prefers a WebSocket and falls back to these).
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

use crate::bg::Pending;
use crate::http::{self, Server};
use crate::stream::Stream;
use crate::term_emu::Emulator;

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
            .expect("the ssh send thread starts");
        let session = SshSession {
            emu,
            closed,
            stream: Some(stream),
            tx,
            sender: Some(sender),
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
fn encode(s: &str) -> String {
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

/// The bytes a key sends: `None` for one that sends nothing.
pub fn key_bytes(key: egui::Key, mods: egui::Modifiers, app_cursor: bool) -> Option<String> {
    use egui::Key::*;
    let cursor = |c: char| {
        if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let s = match key {
        Enter => "\r".to_string(),
        Tab if mods.shift => "\x1b[Z".to_string(),
        Tab => "\t".to_string(),
        Backspace => "\x7f".to_string(),
        Escape => "\x1b".to_string(),
        ArrowUp => cursor('A'),
        ArrowDown => cursor('B'),
        ArrowRight => cursor('C'),
        ArrowLeft => cursor('D'),
        Home => cursor('H'),
        End => cursor('F'),
        Insert => "\x1b[2~".to_string(),
        Delete => "\x1b[3~".to_string(),
        PageUp => "\x1b[5~".to_string(),
        PageDown => "\x1b[6~".to_string(),
        F1 => "\x1bOP".to_string(),
        F2 => "\x1bOQ".to_string(),
        F3 => "\x1bOR".to_string(),
        F4 => "\x1bOS".to_string(),
        F5 => "\x1b[15~".to_string(),
        F6 => "\x1b[17~".to_string(),
        F7 => "\x1b[18~".to_string(),
        F8 => "\x1b[19~".to_string(),
        F9 => "\x1b[20~".to_string(),
        F10 => "\x1b[21~".to_string(),
        F11 => "\x1b[23~".to_string(),
        F12 => "\x1b[24~".to_string(),
        k if mods.ctrl && !mods.alt => {
            let name = k.name();
            let c = name.chars().next().filter(|_| name.len() == 1)?;
            match c.to_ascii_lowercase() {
                l @ 'a'..='z' => ((l as u8 - b'a' + 1) as char).to_string(),
                '[' => "\x1b".to_string(),
                '\\' => "\x1c".to_string(),
                ']' => "\x1d".to_string(),
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(if mods.alt && !s.starts_with('\x1b') {
        format!("\x1b{s}")
    } else {
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    #[test]
    fn keys_become_what_a_terminal_sends() {
        let none = Modifiers::NONE;
        assert_eq!(key_bytes(Key::Enter, none, false).as_deref(), Some("\r"));
        assert_eq!(key_bytes(Key::ArrowUp, none, false).as_deref(), Some("\x1b[A"));
        assert_eq!(key_bytes(Key::ArrowUp, none, true).as_deref(), Some("\x1bOA"));
        assert_eq!(key_bytes(Key::Delete, none, false).as_deref(), Some("\x1b[3~"));
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false).as_deref(), Some("\x03"));
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL, false).as_deref(), Some("\x04"));
        assert_eq!(key_bytes(Key::A, none, false), None);
    }

    #[test]
    fn names_are_encoded_like_the_web_does() {
        assert_eq!(encode("my pi/1"), "my%20pi%2F1");
        assert_eq!(
            output_text(r#"{"text":"hi\r\n","name":"x"}"#).as_deref(),
            Some("hi\r\n")
        );
    }
}
