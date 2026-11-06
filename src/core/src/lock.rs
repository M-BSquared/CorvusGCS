//! One ground station per machine, unless the operator says otherwise
//! (`instance_lock.py`).
//!
//! Two processes on one serial port each get half the bytes and neither gets
//! an error, so the second launch is stopped before it opens anything and is
//! told where the first listens. `CORVUS_ALLOW_MULTI=1` allows more.
//!
//! The lock is held by the file handle: the operating system drops it when
//! the process ends however it ends, so there is no stale lock to clean up.
//! The file's contents (pid, port, start time) are only the message for the
//! next launch, never the decision. The file is the same
//! `~/.corvus/corvus.lock` the Python program locks, so the two exclude each
//! other. Windows locks a byte range mandatorily, so the byte taken is one
//! far past the record.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::json::Map;

pub const ALLOW_MULTI_ENV: &str = "CORVUS_ALLOW_MULTI";

pub fn allow_multi() -> bool {
    allow_multi_value(std::env::var(ALLOW_MULTI_ENV).ok().as_deref())
}

/// The switch's rule on its own, so a test does not touch the environment.
pub fn allow_multi_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    file: Option<File>,
    held: bool,
}

impl InstanceLock {
    pub fn new(path: impl Into<PathBuf>) -> InstanceLock {
        InstanceLock {
            path: path.into(),
            file: None,
            held: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn held(&self) -> bool {
        self.held
    }

    /// Takes the lock; true when this process now owns it. False means
    /// another live Corvus holds it (see `peer`). A file system that cannot
    /// lock degrades to "allowed": refusing to start a ground station over a
    /// locking quirk is worse than the race the lock guards.
    pub fn acquire(&mut self, port: Option<u16>) -> bool {
        if self.held {
            self.write_port(port);
            return true;
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o644);
        let file = match opts.open(&self.path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "corvus: instance lock unavailable at {} ({e}); not enforcing single instance",
                    self.path.display()
                );
                return true;
            }
        };
        match sys::try_lock(&file) {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                eprintln!(
                    "corvus: instance lock not enforceable on {} ({e}); continuing without it",
                    self.path.display()
                );
                return true;
            }
        }
        self.file = Some(file);
        self.held = true;
        self.write_port(port);
        true
    }

    /// Records pid, port and start time for the next launch to read.
    /// Best effort: the lock is the contract, this is the message.
    pub fn write_port(&mut self, port: Option<u16>) {
        let Some(f) = self.file.as_mut() else { return };
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let port = port.map_or("null".to_string(), |p| p.to_string());
        let record = format!(
            "{{\"pid\": {}, \"port\": {port}, \"started\": {started}}}\n",
            std::process::id()
        );
        let _ = f
            .set_len(0)
            .and_then(|_| f.seek(SeekFrom::Start(0)))
            .and_then(|_| f.write_all(record.as_bytes()));
    }

    /// The holder's advertised `{pid, port, started}`; empty when the file
    /// is missing, empty or unreadable. Every field is a hint, not a fact.
    pub fn peer(&self) -> Map {
        let mut text = String::new();
        let read = File::open(&self.path).and_then(|mut f| f.read_to_string(&mut text));
        if read.is_err() {
            return Map::new();
        }
        match serde_json::from_str::<serde_json::Value>(if text.trim().is_empty() { "{}" } else { &text }) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    /// Drops the lock and closes the handle. Idempotent.
    pub fn release(&mut self) {
        self.held = false;
        if let Some(f) = self.file.take() {
            sys::unlock(&f);
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// One line naming where the running instance is.
pub fn describe_peer(peer: &Map) -> String {
    let int = |k: &str| peer.get(k).and_then(serde_json::Value::as_i64).filter(|n| *n != 0);
    let where_ = int("port").map_or("(port unknown)".to_string(), |p| format!("http://localhost:{p}/"));
    let who = int("pid").map_or(String::new(), |p| format!(" (pid {p})"));
    format!("already running{who} at {where_}")
}

#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    pub fn try_lock(f: &File) -> std::io::Result<bool> {
        // SAFETY: flock only takes the descriptor and a flag.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) | Some(libc::EACCES) => Ok(false),
            _ => Err(e),
        }
    }

    pub fn unlock(f: &File) {
        // SAFETY: as above.
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(windows)]
mod sys {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    const LOCK_BYTE: u32 = 1 << 30;
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 1;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 2;
    const ERROR_LOCK_VIOLATION: i32 = 33;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut core::ffi::c_void,
    }

    extern "system" {
        fn LockFileEx(
            h: *mut core::ffi::c_void,
            flags: u32,
            reserved: u32,
            low: u32,
            high: u32,
            o: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(h: *mut core::ffi::c_void, reserved: u32, low: u32, high: u32, o: *mut Overlapped) -> i32;
    }

    fn overlapped() -> Overlapped {
        Overlapped {
            internal: 0,
            internal_high: 0,
            offset: LOCK_BYTE,
            offset_high: 0,
            event: core::ptr::null_mut(),
        }
    }

    pub fn try_lock(f: &File) -> std::io::Result<bool> {
        let mut o = overlapped();
        // SAFETY: the handle is a live file handle and `o` outlives the call.
        let ok = unsafe {
            LockFileEx(
                f.as_raw_handle().cast(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut o,
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(e)
        }
    }

    pub fn unlock(f: &File) {
        let mut o = overlapped();
        // SAFETY: as above.
        unsafe { UnlockFileEx(f.as_raw_handle().cast(), 0, 1, 0, &mut o) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-core-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_second_holder_is_refused_and_told_where_the_first_is() {
        let d = dir("two");
        let path = d.join("corvus.lock");
        let mut first = InstanceLock::new(&path);
        assert!(first.acquire(Some(8123)));
        assert!(first.held());
        let mut second = InstanceLock::new(&path);
        assert!(!second.acquire(Some(9000)));
        assert!(!second.held());
        let peer = second.peer();
        assert_eq!(peer["port"], 8123);
        assert_eq!(peer["pid"], std::process::id());
        assert_eq!(
            describe_peer(&peer),
            format!("already running (pid {}) at http://localhost:8123/", std::process::id())
        );
        first.release();
        assert!(second.acquire(Some(9000)));
        assert_eq!(second.peer()["port"], 9000);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn acquiring_twice_refreshes_the_port_and_dropping_releases() {
        let d = dir("again");
        let path = d.join("corvus.lock");
        {
            let mut l = InstanceLock::new(&path);
            assert!(l.acquire(None));
            assert!(l.acquire(Some(1234)));
            assert_eq!(l.peer()["port"], 1234);
        }
        assert!(InstanceLock::new(&path).acquire(None));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn describe_peer_copes_with_nothing() {
        assert_eq!(describe_peer(&Map::new()), "already running at (port unknown)");
        let peer = serde_json::from_str::<serde_json::Value>(r#"{"pid": 0, "port": null}"#).unwrap();
        assert_eq!(
            describe_peer(peer.as_object().unwrap()),
            "already running at (port unknown)"
        );
    }

    #[test]
    fn an_unwritable_place_is_allowed() {
        let mut l = InstanceLock::new("/proc/definitely/not/here/corvus.lock");
        assert!(l.acquire(None));
        assert!(!l.held());
    }

    #[test]
    fn the_multi_switch_is_truthy_words_only() {
        assert!(!allow_multi_value(None));
        assert!(allow_multi_value(Some(" Yes ")));
        assert!(allow_multi_value(Some("1")));
        assert!(!allow_multi_value(Some("0")));
        assert!(!allow_multi_value(Some("")));
    }
}
