//! The Corvus backend (`serve.py`) as a child of this app.
//!
//! The field laptop is rebooted between flights, so the app must never leave
//! a server behind: on every way out (window closed, SIGINT, SIGTERM, a
//! panic unwinding through `main`) the child gets SIGTERM, which runs its own
//! teardown (links, tlogs, bridges), then SIGKILL if it has not gone within
//! the grace period. `serve.py` handles SIGTERM itself; it is told nothing
//! else.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::net::http::{self, Server};

/// How long the server may take to answer `/api/version` after starting.
const STARTUP: Duration = Duration::from_secs(45);
/// How long it may take to stop after SIGTERM before it is killed.
const GRACE: Duration = Duration::from_secs(8);

/// The child's pid, for a signal handler that has to act without the
/// `Backend` itself. 0 when there is none.
static CHILD_PID: AtomicU32 = AtomicU32::new(0);

/// Where the backend comes from.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The repository (or bundle) root that holds `serve.py`.
    pub root: PathBuf,
    /// The Python interpreter to run it with.
    pub python: PathBuf,
    /// A frozen backend that runs by itself (the Windows bundle's
    /// `backend/corvus-backend.exe`), which takes the place of `python` and
    /// `serve.py`.
    pub frozen: Option<PathBuf>,
}

impl Launch {
    /// The root found from `start` upwards, and its `.venv` Python when it
    /// has one (else `python3` on PATH). `python` overrides the interpreter.
    pub fn find(start: &Path, python: Option<PathBuf>) -> Result<Launch, String> {
        let root = start
            .ancestors()
            .find(|d| d.join("serve.py").is_file() && d.join("corvus").is_dir())
            .ok_or_else(|| format!("no serve.py above {}; pass --root", start.display()))?
            .to_path_buf();
        let python = python.unwrap_or_else(|| {
            let venv = if cfg!(windows) {
                root.join(".venv/Scripts/python.exe")
            } else {
                root.join(".venv/bin/python")
            };
            if venv.is_file() {
                venv
            } else {
                PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
            }
        });
        Ok(Launch {
            root,
            python,
            frozen: None,
        })
    }

    /// The frozen backend beside the executable, when this is a bundle that
    /// carries one: `<dir>/backend/corvus-backend` (`.exe` on Windows).
    pub fn bundled(exe_dir: &Path) -> Option<Launch> {
        let root = exe_dir.join("backend");
        let exe = root.join(if cfg!(windows) {
            "corvus-backend.exe"
        } else {
            "corvus-backend"
        });
        exe.is_file().then(|| Launch {
            root,
            python: PathBuf::new(),
            frozen: Some(exe),
        })
    }
}

/// A running backend.
pub struct Backend {
    child: Option<Child>,
    server: Server,
}

/// A port nothing listens on right now, from the operating system.
pub fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

impl Backend {
    /// Starts `serve.py <port>` and waits until it answers.
    pub fn start(launch: &Launch, port: u16) -> Result<Backend, String> {
        let mut command = match &launch.frozen {
            Some(exe) => Command::new(exe),
            None => {
                let mut c = Command::new(&launch.python);
                c.arg(launch.root.join("serve.py"));
                c
            }
        };
        let program = launch.frozen.as_ref().unwrap_or(&launch.python);
        let child = command
            .arg(port.to_string())
            .current_dir(&launch.root)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", program.display()))?;
        CHILD_PID.store(child.id(), Ordering::SeqCst);
        let mut backend = Backend {
            child: Some(child),
            server: Server::local(port),
        };
        backend.wait_ready()?;
        Ok(backend)
    }

    /// The server to talk to.
    pub fn server(&self) -> &Server {
        &self.server
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + STARTUP;
        loop {
            if let Some(child) = self.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    self.child = None;
                    CHILD_PID.store(0, Ordering::SeqCst);
                    return Err(format!(
                        "the backend exited at startup ({status}); see its output above"
                    ));
                }
            }
            if http::get_json(&self.server, "/api/version").is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.stop();
                return Err(format!(
                    "the backend did not answer on {} within {}s",
                    self.server.url(),
                    STARTUP.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// SIGTERM, wait up to the grace period, then SIGKILL. Idempotent.
    pub fn stop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        CHILD_PID.store(0, Ordering::SeqCst);
        terminate(child.id());
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Asks process `pid` to stop the polite way.
fn terminate(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill(2) with a pid this process spawned and has not reaped yet.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        // Windows has no SIGTERM for a console child; the kill is all there is.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

/// Last resort from a signal handler when the interface did not close in
/// time: kill the child outright. Does nothing when there is none.
pub fn kill_from_signal() {
    let pid = CHILD_PID.swap(0, Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    // SAFETY: as in `terminate`.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    terminate(pid);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_is_found_from_below_it() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let launch = Launch::find(here, Some(PathBuf::from("/bin/true"))).unwrap();
        assert!(launch.root.join("serve.py").is_file());
        assert_eq!(launch.python, PathBuf::from("/bin/true"));
        assert!(Launch::find(Path::new("/"), None).is_err());
    }

    #[test]
    fn a_frozen_backend_beside_the_executable_is_found() {
        let dir = std::env::temp_dir().join(format!("corvus-bundled-{}", std::process::id()));
        let backend = dir.join("backend");
        std::fs::create_dir_all(&backend).unwrap();
        assert!(Launch::bundled(&dir).is_none(), "nothing there yet");
        let name = if cfg!(windows) {
            "corvus-backend.exe"
        } else {
            "corvus-backend"
        };
        std::fs::write(backend.join(name), "").unwrap();
        let launch = Launch::bundled(&dir).expect("found");
        assert_eq!(launch.frozen, Some(backend.join(name)));
        assert_eq!(launch.root, backend);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_ignores_sigterm_is_killed_after_the_grace_period() {
        // sh traps TERM and keeps sleeping; stop() must still end it.
        let child = Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 60"])
            .spawn()
            .unwrap();
        // Let sh set its trap first, or the TERM beats it.
        std::thread::sleep(Duration::from_millis(300));
        let mut b = Backend {
            child: Some(child),
            server: Server::local(1),
        };
        let begun = Instant::now();
        b.stop();
        assert!(b.child.is_none());
        assert!(begun.elapsed() >= GRACE - Duration::from_millis(100));
        assert!(begun.elapsed() < GRACE + Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_honours_sigterm_stops_at_once() {
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let mut b = Backend {
            child: Some(child),
            server: Server::local(1),
        };
        let begun = Instant::now();
        b.stop();
        assert!(begun.elapsed() < Duration::from_secs(1));
    }
}
