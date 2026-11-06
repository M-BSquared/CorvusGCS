//! A panic anywhere is written, with its backtrace, to `~/.corvus/gui-crash.log`
//! and never shown: a field laptop has nobody to read a console. A panic on the
//! main thread also takes the backend child down before the process ends, as
//! the lifecycle rules ask; a panic on a worker thread leaves the interface
//! running (every lock in it is poison tolerant).

use std::cell::Cell;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The log is cut to its newest half when it passes this many bytes.
const MAX_LOG: u64 = 256 * 1024;

pub fn log_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".corvus").join("gui-crash.log"))
}

/// A panic that repeats every frame is written once in this long.
const SAME_PANIC: Duration = Duration::from_secs(5);

thread_local! {
    static GUARDED: Cell<bool> = const { Cell::new(false) };
}

static LAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// Runs `f`; a panic inside it comes back as the message instead of ending
/// the program, and does not take the backend down with it. The frame that
/// panicked shows a notice and the next one tries again.
pub fn guarded<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    GUARDED.with(|g| g.set(true));
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    GUARDED.with(|g| g.set(false));
    out.map_err(|p| payload(p.as_ref()))
}

/// Whether this `place` and `message` was written less than `SAME_PANIC` ago.
fn repeated(key: &str, now: Instant) -> bool {
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, at)) = last.as_ref() {
        if k == key && now.duration_since(*at) < SAME_PANIC {
            return true;
        }
    }
    *last = Some((key.to_string(), now));
    false
}

/// Installs the hook once. Safe to call early: it reads nothing but the panic.
pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed").to_string();
        let place = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let message = payload(info.payload());
        if !repeated(&format!("{place} {message}"), Instant::now()) {
            let entry = format_entry(
                &name,
                &place,
                &message,
                &std::backtrace::Backtrace::force_capture().to_string(),
            );
            if let Some(path) = log_path() {
                let _ = append(&path, &entry);
            }
        }
        if name == "main" && !GUARDED.with(Cell::get) {
            crate::net::backend::kill_from_signal();
        }
        if std::env::var_os("CORVUS_GUI_PANIC_ECHO").is_some() {
            default(info);
        }
    }));
}

fn payload(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".into())
}

/// One entry of the log.
pub fn format_entry(thread: &str, location: &str, message: &str, backtrace: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut s = String::new();
    let _ = writeln!(s, "--- panic at unix time {secs} on thread {thread}");
    let _ = writeln!(s, "at {location}");
    let _ = writeln!(s, "{message}");
    let _ = writeln!(s, "{backtrace}");
    s
}

/// Appends `entry`, first cutting the file down to its newest part when it
/// has grown past `MAX_LOG`.
pub fn append(path: &Path, entry: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).map_or(0, |m| m.len()) > MAX_LOG {
        let old = std::fs::read(path).unwrap_or_default();
        let keep = &old[old.len() - (MAX_LOG as usize / 2).min(old.len())..];
        std::fs::write(path, keep)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(entry.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-crash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("gui-crash.log")
    }

    #[test]
    fn an_entry_carries_thread_place_and_message() {
        let e = format_entry("main", "src/a.rs:3", "boom", "frame 0");
        assert!(e.contains("thread main") && e.contains("src/a.rs:3") && e.contains("boom") && e.contains("frame 0"));
    }

    #[test]
    fn the_log_is_created_with_its_folder_and_appended_to() {
        let p = temp("append");
        append(&p, "one\n").unwrap();
        append(&p, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn the_log_never_grows_past_its_bound() {
        let p = temp("bound");
        let chunk = "x".repeat(10_000);
        for _ in 0..200 {
            append(&p, &chunk).unwrap();
        }
        assert!(std::fs::metadata(&p).unwrap().len() <= MAX_LOG + 10_000);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn a_panic_inside_the_guard_is_returned_not_thrown() {
        let r = guarded(|| -> u8 { panic!("a page broke") });
        assert_eq!(r.unwrap_err(), "a page broke");
        assert_eq!(guarded(|| 7).unwrap(), 7);
    }

    #[test]
    fn the_same_panic_is_written_once_in_a_while() {
        let t = Instant::now();
        assert!(!repeated("a", t));
        assert!(repeated("a", t + Duration::from_secs(1)));
        assert!(!repeated("b", t + Duration::from_secs(2)));
        assert!(!repeated("b", t + Duration::from_secs(20)));
    }

    #[test]
    fn a_payload_of_either_kind_reads_as_text() {
        assert_eq!(payload(&"a"), "a");
        assert_eq!(payload(&String::from("b")), "b");
        assert_eq!(payload(&5_u8), "a panic with no message");
    }
}
