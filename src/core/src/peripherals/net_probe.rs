//! Is a host on the network answering? One ICMP echo through the system
//! `ping` (`net_probe.py`).
//!
//! A raw socket needs privileges the operator's account does not have, so this
//! asks the operating system's own `ping` for exactly one echo and reads its
//! answer. The host is checked before it goes anywhere near a command line and
//! is passed as its own argument, never through a shell. Every probe is a child
//! of Corvus, bounded by its own deadline and ended by [`Pinger::shutdown`].

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::child_env::child_env;
use super::util::{os_error_text, py_strip, which};

pub const MAX_HOST: usize = 253;
pub const MIN_WAIT_S: f64 = 0.5;
pub const MAX_WAIT_S: f64 = 10.0;
/// Past the echo's own wait: what a slow ping gets to start and print.
pub const GRACE_S: f64 = 2.0;
/// Probes in flight at once.
pub const MAX_CONCURRENT: usize = 4;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `(host, "")` for a usable host name or address, else `("", why)`.
pub fn clean_host(raw: &str) -> (String, String) {
    let host = py_strip(raw);
    if host.is_empty() {
        return (String::new(), "Enter an IP address or host name.".into());
    }
    if host.chars().count() > MAX_HOST || !host_shape(host) {
        return (String::new(), "That is not an IP address or host name.".into());
    }
    (host.to_string(), String::new())
}

/// `clean_host` of a JSON value, as `str(raw if raw is not None else "")` reads it.
pub fn clean_host_value(raw: &Value) -> (String, String) {
    match raw {
        Value::Null => clean_host(""),
        Value::String(s) => clean_host(s),
        Value::Bool(b) => clean_host(if *b { "True" } else { "False" }),
        other => clean_host(&other.to_string()),
    }
}

/// `^[A-Za-z0-9:](?:[A-Za-z0-9._:%-]*[A-Za-z0-9])?$`.
fn host_shape(host: &str) -> bool {
    let b = host.as_bytes();
    let first = |c: u8| c.is_ascii_alphanumeric() || c == b':';
    if !first(b[0]) {
        return false;
    }
    if b.len() == 1 {
        return true;
    }
    b[1..b.len() - 1]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'%' | b'-'))
        && b[b.len() - 1].is_ascii_alphanumeric()
}

/// The platform name Python's `sys.platform` gives.
pub fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// The argv for one echo to `host`, waiting at most `wait_s` for the reply.
pub fn ping_command(ping: &str, host: &str, wait_s: f64, platform: &str) -> Vec<String> {
    let seconds = (wait_s.ceil() as i64).max(1).to_string();
    let s = |x: &str| x.to_string();
    match platform {
        "win32" => vec![
            s(ping),
            s("-n"),
            s("1"),
            s("-w"),
            ((wait_s * 1000.0) as i64).max(1).to_string(),
            s(host),
        ],
        "darwin" => {
            // macOS' ping is IPv4 only; ping6 has no overall timeout, so the
            // process deadline is what bounds it.
            if host.contains(':') {
                let ping6 = which("ping6")
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s("ping6"));
                vec![ping6, s("-c"), s("1"), s("-n"), s(host)]
            } else {
                vec![s(ping), s("-c"), s("1"), s("-n"), s("-t"), seconds, s(host)]
            }
        }
        _ => vec![s(ping), s("-c"), s("1"), s("-n"), s("-W"), seconds, s(host)],
    }
}

/// `[=<]\s*([0-9]+(?:[.,][0-9]+)?)\s*ms\b`, case insensitive: the first time.
fn round_trip(output: &str) -> Option<f64> {
    let chars: Vec<char> = output.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != '=' && *c != '<' {
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let start = j;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j == start {
            continue;
        }
        let mut number: String = chars[start..j].iter().collect();
        if j + 1 < chars.len() && (chars[j] == '.' || chars[j] == ',') && chars[j + 1].is_ascii_digit() {
            let mut k = j + 1;
            while k < chars.len() && chars[k].is_ascii_digit() {
                k += 1;
            }
            let mut tail = k;
            while tail < chars.len() && chars[tail].is_whitespace() {
                tail += 1;
            }
            if ms_at(&chars, tail) {
                number = format!("{number}.{}", chars[j + 1..k].iter().collect::<String>());
                return number.parse().ok();
            }
        }
        let mut tail = j;
        while tail < chars.len() && chars[tail].is_whitespace() {
            tail += 1;
        }
        if ms_at(&chars, tail) {
            return number.parse().ok();
        }
    }
    None
}

fn ms_at(chars: &[char], at: usize) -> bool {
    at + 1 < chars.len()
        && chars[at].eq_ignore_ascii_case(&'m')
        && chars[at + 1].eq_ignore_ascii_case(&'s')
        && (at + 2 == chars.len() || !(chars[at + 2].is_alphanumeric() || chars[at + 2] == '_'))
}

/// Whether `output` shows an echo reply, and its round trip in ms. Windows
/// answers "Destination host unreachable" from a router with exit code 0, so a
/// reply there also needs the TTL an IPv4 echo carries.
pub fn parse_reply(returncode: i32, output: &str, host: &str, platform: &str) -> (bool, Option<f64>) {
    if returncode != 0 {
        return (false, None);
    }
    let rtt = round_trip(output);
    if platform == "win32" {
        if host.contains(':') {
            return (rtt.is_some(), rtt);
        }
        return (output.to_lowercase().contains("ttl="), rtt);
    }
    (true, rtt)
}

/// One-shot pings, each a child of Corvus that ends with it.
pub struct Pinger {
    ping: String,
    state: Mutex<State>,
    slots: AtomicUsize,
    next: AtomicU64,
}

#[derive(Default)]
struct State {
    closed: bool,
    procs: HashMap<u64, Arc<Mutex<Child>>>,
}

impl Pinger {
    /// `ping` is the program to run; `None` looks `ping` up on the `PATH`.
    pub fn new(ping: Option<&str>) -> Pinger {
        let ping = match ping {
            Some(p) => p.to_string(),
            None => which("ping")
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        Pinger {
            ping,
            state: Mutex::new(State::default()),
            slots: AtomicUsize::new(0),
            next: AtomicU64::new(1),
        }
    }

    /// Whether there is a `ping` to ask.
    pub fn available(&self) -> bool {
        !self.ping.is_empty()
    }

    /// Sends one echo and waits for it. `{"ok": true, "host", "reachable",
    /// "rtt_ms"}` once ping has answered either way, `{"ok": false, "error"}`
    /// when the probe could not be made.
    pub fn ping(&self, raw_host: &Value, wait_s: f64) -> Value {
        let (host, problem) = clean_host_value(raw_host);
        if !problem.is_empty() {
            return json!({"ok": false, "error": problem});
        }
        if self.ping.is_empty() {
            return json!({"ok": false, "error": "There is no ping program on this computer."});
        }
        let wait = wait_s.clamp(MIN_WAIT_S, MAX_WAIT_S);
        if self.slots.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT {
            self.slots.fetch_sub(1, Ordering::SeqCst);
            return json!({"ok": false, "error": "Too many pings at once."});
        }
        let answer = self.probe(&host, wait);
        self.slots.fetch_sub(1, Ordering::SeqCst);
        answer
    }

    fn probe(&self, host: &str, wait: f64) -> Value {
        let argv = ping_command(&self.ping, host, wait, platform());
        let (id, child) = {
            let mut st = lock(&self.state);
            if st.closed {
                return json!({"ok": false, "error": "Corvus is shutting down."});
            }
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env_clear()
                .envs(child_env());
            no_window(&mut cmd);
            let child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    return json!({"ok": false, "error": format!("ping could not be started: {}", os_error_text(&e))});
                }
            };
            let id = self.next.fetch_add(1, Ordering::SeqCst);
            let child = Arc::new(Mutex::new(child));
            st.procs.insert(id, Arc::clone(&child));
            (id, child)
        };
        let (out, err) = {
            let mut c = lock(&child);
            (c.stdout.take(), c.stderr.take())
        };
        let reader = |pipe: Option<Box<dyn Read + Send>>| {
            thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                buf
            })
        };
        let out_t = reader(out.map(|p| Box::new(p) as Box<dyn Read + Send>));
        let err_t = reader(err.map(|p| Box::new(p) as Box<dyn Read + Send>));
        let deadline = Instant::now() + Duration::from_secs_f64(wait + GRACE_S);
        let mut timed_out = false;
        let status = loop {
            if let Ok(Some(status)) = lock(&child).try_wait() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let mut c = lock(&child);
                let _ = c.kill();
                let _ = c.wait();
                break None;
            }
            thread::sleep(Duration::from_millis(10));
        };
        lock(&self.state).procs.remove(&id);
        let mut bytes = out_t.join().unwrap_or_default();
        bytes.extend(err_t.join().unwrap_or_default());
        if timed_out {
            return json!({"ok": true, "host": host, "reachable": false, "rtt_ms": null});
        }
        let code = status.and_then(|s| s.code()).unwrap_or(-1);
        let (reachable, rtt) = parse_reply(code, &String::from_utf8_lossy(&bytes), host, platform());
        json!({"ok": true, "host": host, "reachable": reachable, "rtt_ms": if reachable { rtt.map(Value::from).unwrap_or(Value::Null) } else { Value::Null }})
    }

    /// Ends every probe in flight and refuses new ones. Never fails.
    pub fn shutdown(&self) {
        let procs: Vec<Arc<Mutex<Child>>> = {
            let mut st = lock(&self.state);
            st.closed = true;
            st.procs.values().cloned().collect()
        };
        for p in procs {
            let mut c = lock(&p);
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}
