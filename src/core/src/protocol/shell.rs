//! The MAVLink shell (PX4's NSH over SERIAL_CONTROL): lines typed by an
//! operator go out in 70 byte chunks, what the vehicle prints comes back as
//! console lines, or, once a terminal has taken the stream (`write`), as raw
//! text for every subscriber with a backlog for one that attaches late.
//!
//! The twin of `ShellEngine` in `corvus_link` (`protocol/shell.rs`).

use std::sync::Mutex;

use corvus_link::plain::defs::{constant_or, Wire};
use corvus_link::plain::shell_text::{assemble_lines, trim_backlog, Utf8Stream, CHUNK};
use corvus_link::plain::{Message, Value};

use super::RequestHost;

/// The limits `corvus.mavlink_shell` sets.
#[derive(Debug, Clone, Copy)]
pub struct ShellLimits {
    /// A line without an end is flushed as a line of its own at this length.
    pub line_max_chars: usize,
    /// What a terminal attaching late is handed of what the shell printed.
    pub backlog_chars: usize,
    /// One write from a terminal.
    pub write_max_chars: usize,
}

impl Default for ShellLimits {
    fn default() -> Self {
        ShellLimits {
            line_max_chars: 8192,
            backlog_chars: 64 * 1024,
            write_max_chars: 4096,
        }
    }
}

/// What the shell asks of its bridge beyond `RequestHost`.
pub trait ShellHost: RequestHost {
    /// Whether this stack has a MAVLink shell (PX4 does, ArduPilot does not).
    fn supports_shell(&self) -> bool;
    /// The stack's name for the operator.
    fn label(&self) -> String;
    /// A command that could not run: sets the error, tells the console and the
    /// warning board. Returns false.
    fn command_failure(&self, action: &str, result: i64) -> bool;
    /// Whether a SERIAL_CONTROL answer is for this station.
    fn ack_is_for_us(&self, msg: &Message) -> bool;
    /// The view of the message set the link speaks. SERIAL_CONTROL is addressed
    /// to the vehicle only where the dialect has its target fields (an extension
    /// in MAVLink 2, absent in MAVLink 1), as the Python engine checks.
    fn wire(&self) -> Wire {
        Wire::V2
    }
}

type Subscriber = Box<dyn Fn(&str) + Send + Sync>;

struct State {
    active: bool,
    raw: bool,
    buffer: String,
    decoder: Utf8Stream,
    backlog: String,
    subs: Vec<(u64, std::sync::Arc<Subscriber>)>,
    next_sub: u64,
}

/// A subscriber to the raw stream, for `remove_sub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubId(u64);

pub struct ShellEngine {
    limits: ShellLimits,
    st: Mutex<State>,
}

fn dev_shell() -> i64 {
    constant_or("SERIAL_CONTROL_DEV_SHELL", 10)
}

fn flags_stream() -> i64 {
    constant_or("SERIAL_CONTROL_FLAG_RESPOND", 2)
        | constant_or("SERIAL_CONTROL_FLAG_EXCLUSIVE", 4)
        | constant_or("SERIAL_CONTROL_FLAG_MULTI", 16)
}

impl ShellEngine {
    pub fn new(limits: ShellLimits) -> ShellEngine {
        ShellEngine {
            limits,
            st: Mutex::new(State {
                active: false,
                raw: false,
                buffer: String::new(),
                decoder: Utf8Stream::default(),
                backlog: String::new(),
                subs: Vec::new(),
                next_sub: 0,
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn active(&self) -> bool {
        self.state().active
    }

    pub fn raw(&self) -> bool {
        self.state().raw
    }

    pub fn buffer(&self) -> String {
        self.state().buffer.clone()
    }

    pub fn backlog(&self) -> String {
        self.state().backlog.clone()
    }

    fn reset_locked(&self) {
        let mut st = self.state();
        st.active = false;
        st.buffer.clear();
        st.decoder.reset();
    }

    /// Forgets a half assembled line and ends the session.
    pub fn reset(&self) {
        self.reset_locked();
    }

    fn serial_control(host: &dyn ShellHost, flags: i64, count: usize, data: &[u8]) -> Result<(), String> {
        let wire = host.wire();
        let mut m = Message::named("SERIAL_CONTROL", wire).ok_or("no SERIAL_CONTROL")?;
        let mut padded = data.to_vec();
        padded.resize(CHUNK, 0);
        let set = |m: &mut Message, f: &str, v: Value| m.set(f, v).map_err(|e| e.to_string());
        set(&mut m, "device", Value::Int(dev_shell() as i128))?;
        set(&mut m, "flags", Value::Int(flags as i128))?;
        set(&mut m, "timeout", Value::Int(0))?;
        set(&mut m, "baudrate", Value::Int(0))?;
        set(&mut m, "count", Value::Int(count as i128))?;
        set(
            &mut m,
            "data",
            Value::Array(padded.iter().map(|b| Value::Int(*b as i128)).collect()),
        )?;
        if m.get("target_system").is_some() && m.get("target_component").is_some() {
            let t = host.target();
            set(&mut m, "target_system", Value::Int(t.0 as i128))?;
            set(&mut m, "target_component", Value::Int(t.1 as i128))?;
        }
        match host.send(m) {
            Ok(true) => Ok(()),
            Ok(false) => Err("link closed".into()),
            Err(e) => Err(e),
        }
    }

    fn send_chunks(host: &dyn ShellHost, payload: &[u8]) -> Result<(), String> {
        for chunk in payload.chunks(CHUNK) {
            Self::serial_control(host, flags_stream(), chunk.len(), chunk)?;
        }
        Ok(())
    }

    /// Types one line into the shell, which takes it as a command whose
    /// output arrives as console lines. The caller holds the send lock.
    pub fn send_command(&self, host: &dyn ShellHost, text: &str) -> bool {
        host.set_error("");
        if !host.supports_shell() {
            host.set_error(&format!(
                "{} has no MAVLink shell. The console is a PX4 feature (NSH over SERIAL_CONTROL)",
                host.label()
            ));
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Shell", -2);
        }
        let line = if text.ends_with('\n') {
            text.to_string()
        } else {
            format!("{text}\n")
        };
        self.reset_locked();
        {
            let mut st = self.state();
            st.active = true;
            st.raw = false;
        }
        if let Err(e) = Self::send_chunks(host, line.as_bytes()) {
            self.reset_locked();
            host.log("error", &format!("shell command send failed: {e}"));
            host.set_error(&format!("shell send failed: {e}"));
            return false;
        }
        true
    }

    /// Writes raw text to the shell, for a terminal that owns the stream.
    pub fn write(&self, host: &dyn ShellHost, text: &str) -> bool {
        host.set_error("");
        if text.is_empty() {
            host.set_error("nothing to send to the shell");
            return false;
        }
        let max = self.limits.write_max_chars;
        let length = text.chars().count();
        if length > max {
            host.set_error(&format!(
                "too much at once for the shell ({length} characters, at most {max})"
            ));
            return false;
        }
        if !host.supports_shell() {
            host.set_error(&format!(
                "{} has no NuttX shell. It is a PX4 feature (NSH over SERIAL_CONTROL)",
                host.label()
            ));
            return false;
        }
        if !host.connection_ready() {
            host.set_error("not connected");
            return false;
        }
        {
            let mut st = self.state();
            if !st.raw {
                st.buffer.clear();
                st.decoder.reset();
            }
            st.raw = true;
            st.active = true;
        }
        if let Err(e) = Self::send_chunks(host, text.as_bytes()) {
            host.log("error", &format!("shell write failed: {e}"));
            host.set_error(&format!("shell send failed: {e}"));
            return false;
        }
        true
    }

    /// Subscribes to the raw stream; returns the id and what the shell
    /// already printed (the backlog) for the subscriber to draw first.
    pub fn add_sub(&self, f: impl Fn(&str) + Send + Sync + 'static) -> (SubId, String) {
        let mut st = self.state();
        st.next_sub += 1;
        let id = st.next_sub;
        st.subs.push((id, std::sync::Arc::new(Box::new(f))));
        (SubId(id), st.backlog.clone())
    }

    pub fn remove_sub(&self, id: SubId) {
        self.state().subs.retain(|(i, _)| *i != id.0);
    }

    /// Ends the session: a SERIAL_CONTROL with no flags tells NSH to let go.
    pub fn stop(&self, host: &dyn ShellHost) {
        self.reset_locked();
        {
            let mut st = self.state();
            st.raw = false;
            st.backlog.clear();
        }
        let _ = Self::serial_control(host, 0, 0, &[0u8; CHUNK]);
    }

    /// A SERIAL_CONTROL message from the vehicle. Output of another device,
    /// or while no session is open, is not the shell's.
    pub fn handle(&self, host: &dyn ShellHost, msg: &Message) {
        let device = msg.get("device").and_then(Value::as_i64).unwrap_or(-1);
        if device != dev_shell() {
            return;
        }
        if !self.state().active || !host.ack_is_for_us(msg) {
            return;
        }
        let (lines, subs, text) = {
            let mut st = self.state();
            if !st.active {
                return;
            }
            let count = msg.get("count").and_then(Value::as_i64).unwrap_or(0);
            if count <= 0 || count > CHUNK as i64 {
                return;
            }
            let raw: Vec<u8> = match msg.get("data") {
                Some(Value::Array(a)) => a.iter().map(|v| v.as_i64().unwrap_or(0) as u8).collect(),
                _ => return,
            };
            if count as usize > raw.len() {
                return;
            }
            let text = st.decoder.decode(&raw[..count as usize]);
            if st.raw {
                let mut subs = Vec::new();
                if !text.is_empty() {
                    let joined = format!("{}{}", st.backlog, text);
                    st.backlog = trim_backlog(&joined, self.limits.backlog_chars);
                    subs = st.subs.iter().map(|(_, f)| f.clone()).collect();
                }
                (Vec::new(), subs, text)
            } else {
                let lines = assemble_lines(&mut st.buffer, &text, self.limits.line_max_chars);
                (lines, Vec::new(), text)
            }
        };
        for sub in subs {
            sub(&text);
        }
        for line in lines {
            host.console("SHELL", &line, "info");
        }
    }
}
