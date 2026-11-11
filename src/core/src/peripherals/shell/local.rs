//! Programs on this computer (`local_shell.py`): an interactive shell in a
//! terminal window on a pseudo terminal, or one run in the background.
//!
//! `LocalSession` lives in the terminal registry beside the SSH sessions, so
//! everything a terminal window does with a session (the output stream and its
//! replay, typed input, resizes, the disconnect button) works on it by name.
//! `LocalRunner` starts a program without a window; unlike an SSH launcher's
//! background mode it is Corvus' own child and is stopped with it (SIGTERM,
//! then SIGKILL), because the ground station must shut down leaving nothing
//! behind. Neither inherits the variables a packaged Corvus runs with.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::session::{Output, Terminal};
use crate::peripherals::child_env::child_env;
use crate::peripherals::util::{os_error_text, which};

pub const LOCAL_HOST: &str = "this computer";
pub const MAX_COMMAND: usize = 8192;
/// How long a background start waits to see whether the program failed at once.
pub const EARLY_EXIT_S: f64 = 1.0;
/// How much of a background program's output is kept for that answer.
pub const OUTPUT_TAIL: usize = 4096;
pub const TERM_TIMEOUT_S: f64 = 2.0;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The operator's login shell, or the system's.
pub fn default_shell() -> String {
    if cfg!(windows) {
        return std::env::var("COMSPEC")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "cmd.exe".into());
    }
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() && is_executable_file(Path::new(&shell)) {
            return shell;
        }
    }
    which("bash")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/bin/sh".into())
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

/// How the login shell is asked to run `text`: `<shell> -l -c <text>` as an
/// argument list on POSIX, one command line for cmd.exe on Windows (a list
/// would go through quote escaping that cmd does not read).
pub fn shell_command(text: &str, shell: &str, platform: &str) -> Vec<String> {
    let shell = if shell.is_empty() {
        default_shell()
    } else {
        shell.to_string()
    };
    if platform == "win32" {
        return vec![format!("\"{shell}\" /d /s /c \"{text}\"")];
    }
    vec![shell, "-l".into(), "-c".into(), text.to_string()]
}

pub fn have_pty() -> bool {
    cfg!(unix)
}

/// What running things on this computer can do here.
pub fn capabilities() -> Value {
    json!({
        "terminal": have_pty(),
        "reason": if have_pty() { "" } else {
            "A terminal on this computer needs a pseudo-terminal, which Windows does not offer here. Run the program in the background instead."
        },
        "shell": default_shell(),
        "host": LOCAL_HOST,
    })
}

/// The environment the terminal's shell starts with: the operator's, a terminal
/// type xterm.js answers to, and a UTF-8 character set when the session named
/// none.
pub fn shell_env(env: &BTreeMap<String, String>, platform: &str) -> BTreeMap<String, String> {
    let mut out = crate::peripherals::child_env::child_env_from(env, &crate::peripherals::child_env::app_root());
    out.insert("TERM".into(), "xterm-256color".into());
    out.insert("COLORTERM".into(), "truecolor".into());
    if !["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .any(|k| out.get(*k).is_some_and(|v| !v.is_empty()))
    {
        out.insert(
            "LC_CTYPE".into(),
            if platform == "darwin" { "UTF-8" } else { "C.UTF-8" }.into(),
        );
    }
    out
}

fn platform() -> &'static str {
    crate::peripherals::net_probe::platform()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn normpath(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `(path, "")` for a folder that exists, or `("", why)`. Empty means home, and
/// a relative folder is read from home, the way a login shell reads it.
pub fn clean_directory(directory: &str) -> (String, String) {
    let text = crate::peripherals::util::py_strip(directory);
    let home = home_dir();
    if text.is_empty() {
        return (home.to_string_lossy().into_owned(), String::new());
    }
    let expanded = if text == "~" || text.starts_with("~/") {
        home.join(text.trim_start_matches('~').trim_start_matches('/'))
    } else {
        PathBuf::from(text)
    };
    let path = normpath(&home.join(expanded));
    if !path.is_dir() {
        return (String::new(), format!("There is no folder {text} on this computer."));
    }
    (path.to_string_lossy().into_owned(), String::new())
}

// ----------------------------------------------------------- stopping ----

/// The process groups of every process still running in session `sid`. Never
/// fails. Zombies are left out. Read from `/proc` where there is one, from `ps`
/// and `getsid` elsewhere.
#[cfg(unix)]
pub fn session_groups(sid: i32) -> std::collections::BTreeSet<i32> {
    let mut groups = std::collections::BTreeSet::new();
    if Path::new("/proc/self/stat").is_file() {
        let Ok(read) = std::fs::read_dir("/proc") else {
            return groups;
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(raw) = std::fs::read(format!("/proc/{name}/stat")) else {
                continue;
            };
            let Some(at) = raw.iter().rposition(|&b| b == b')') else {
                continue;
            };
            let rest = String::from_utf8_lossy(&raw[at + 1..]).into_owned();
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() > 3 && fields[0] != "Z" {
                if let (Ok(pgrp), Ok(session)) = (fields[2].parse::<i32>(), fields[3].parse::<i32>()) {
                    if session == sid {
                        groups.insert(pgrp);
                    }
                }
            }
        }
        return groups;
    }
    let Ok(out) = Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,pgid=,stat="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .envs(child_env())
        .output()
    else {
        return groups;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 || parts[2].starts_with('Z') {
            continue;
        }
        if let (Ok(pid), Ok(pgid)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) {
            // SAFETY: getsid only reads kernel state for the given pid.
            if unsafe { libc::getsid(pid) } == sid {
                groups.insert(pgid);
            }
        }
    }
    groups
}

#[cfg(unix)]
fn signal_groups(groups: &std::collections::BTreeSet<i32>, sig: i32) {
    for &g in groups {
        // SAFETY: killpg with a group number is always sound; failures are ignored.
        unsafe {
            libc::killpg(g, sig);
        }
    }
}

fn wait_child(child: &mut Child, seconds: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Signals everything in the session `child` leads, then makes sure it is
/// gone. The session, not only the process's own group: a shell with job
/// control puts every background job in a group of its own, and whether a
/// hangup is passed on to them is the shell's choice. What is still in the
/// session after [`TERM_TIMEOUT_S`] is killed. A program that left the session
/// on purpose is not Corvus' to stop. Asked even when the child has ended: an
/// `exit` typed in the shell leaves its background jobs running.
#[cfg(unix)]
pub fn stop_session(child: &mut Child, first: i32) {
    let sid = child.id() as i32;
    let mut all = session_groups(sid);
    all.insert(sid);
    signal_groups(&all, first);
    // A stopped job acts on nothing but SIGKILL until it is continued.
    signal_groups(&session_groups(sid), libc::SIGCONT);
    let deadline = Instant::now() + Duration::from_secs_f64(TERM_TIMEOUT_S);
    wait_child(child, TERM_TIMEOUT_S);
    let mut left = session_groups(sid);
    while !left.is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
        left = session_groups(sid);
    }
    if left.is_empty() && matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    left.insert(sid);
    signal_groups(&left, libc::SIGKILL);
    wait_child(child, TERM_TIMEOUT_S);
}

#[cfg(windows)]
pub fn taskkill_command(pid: u32, system_root: Option<&str>) -> Vec<String> {
    let root = system_root
        .map(str::to_string)
        .or_else(|| std::env::var("SystemRoot").ok())
        .unwrap_or_default();
    let exe = if root.is_empty() {
        "taskkill".to_string()
    } else {
        format!("{root}\\System32\\taskkill.exe")
    };
    vec![exe, "/F".into(), "/T".into(), "/PID".into(), pid.to_string()]
}

#[cfg(windows)]
pub fn stop_session(child: &mut Child, _first: i32) {
    use std::os::windows::process::CommandExt;
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let argv = taskkill_command(child.id(), None);
    let _ = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .status();
    if !wait_child(child, TERM_TIMEOUT_S) {
        let _ = child.kill();
        wait_child(child, TERM_TIMEOUT_S);
    }
}

// ------------------------------------------------------------ session ----

/// Decodes UTF-8 across reads, a bad byte becoming U+FFFD.
#[derive(Default)]
pub struct Utf8Decoder {
    held: Vec<u8>,
}

impl Utf8Decoder {
    pub fn decode(&mut self, data: &[u8]) -> String {
        self.held.extend_from_slice(data);
        let mut out = String::new();
        let mut at = 0;
        loop {
            match std::str::from_utf8(&self.held[at..]) {
                Ok(s) => {
                    out.push_str(s);
                    at = self.held.len();
                    break;
                }
                Err(e) => {
                    let valid = at + e.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.held[at..valid]).unwrap_or(""));
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{FFFD}');
                            at = valid + n;
                        }
                        None => {
                            at = valid;
                            break;
                        }
                    }
                }
            }
        }
        self.held.drain(..at);
        out
    }
}

#[cfg(unix)]
mod pty {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    /// A master side of a pty: closed when the last reference goes.
    pub struct Master(pub OwnedFd);

    impl Master {
        pub fn fd(&self) -> RawFd {
            self.0.as_raw_fd()
        }
    }

    pub fn open(rows: u16, cols: u16) -> std::io::Result<(Master, OwnedFd)> {
        let (mut m, mut s) = (-1, -1);
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty fills the two descriptors; ws is a valid winsize.
        let rc = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>() as _,
                &ws as *const libc::winsize as _,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both descriptors are open and ours.
        unsafe {
            libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(s, libc::F_SETFD, libc::FD_CLOEXEC);
            Ok((Master(OwnedFd::from_raw_fd(m)), OwnedFd::from_raw_fd(s)))
        }
    }

    pub fn set_size(fd: RawFd, rows: u16, cols: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads a winsize; a failure is ignored.
        unsafe {
            libc::ioctl(fd, libc::TIOCSWINSZ as _, &ws);
        }
    }

    /// Waits up to `ms` for `fd` to be readable.
    pub fn readable(fd: RawFd, ms: i32) -> bool {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut p, 1, ms) };
        rc > 0 && p.revents != 0
    }
}

struct LocalState {
    child: Option<Child>,
    #[cfg(unix)]
    master: Option<Arc<pty::Master>>,
    reader: Option<JoinHandle<()>>,
}

struct Local {
    name: String,
    directory: String,
    shell: String,
    username: String,
    output: Output,
    connected: AtomicBool,
    running: AtomicBool,
    error: Mutex<String>,
    size: Mutex<(u16, u16)>,
    state: Mutex<LocalState>,
}

/// A login shell on this computer, on a pseudo terminal.
pub struct LocalSession(Arc<Local>);

fn username() -> String {
    for k in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    #[cfg(unix)]
    {
        // SAFETY: getpwuid returns a pointer into static storage or null.
        unsafe {
            let pw = libc::getpwuid(libc::getuid());
            if !pw.is_null() {
                return std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned();
            }
        }
    }
    String::new()
}

impl LocalSession {
    pub fn new(name: &str, directory: &str, shell: &str) -> Arc<LocalSession> {
        Arc::new(LocalSession(Arc::new(Local {
            name: name.to_string(),
            directory: directory.to_string(),
            shell: if shell.is_empty() {
                default_shell()
            } else {
                shell.to_string()
            },
            username: username(),
            output: Output::default(),
            connected: AtomicBool::new(false),
            running: AtomicBool::new(false),
            error: Mutex::new(String::new()),
            size: Mutex::new((80, 24)),
            state: Mutex::new(LocalState {
                child: None,
                #[cfg(unix)]
                master: None,
                reader: None,
            }),
        })))
    }
}

impl Local {
    fn set_error(&self, text: &str) {
        *lock(&self.error) = text.to_string();
    }

    /// Stops the shell and every job in its session. The pty is let go by the
    /// last user of it, never closed under the reader.
    fn close_transport(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        let child = {
            let mut st = lock(&self.state);
            #[cfg(unix)]
            {
                st.master = None;
            }
            st.child.take()
        };
        if let Some(mut child) = child {
            // SIGHUP first: what closing a terminal sends, to every job, since
            // not every shell passes it on.
            #[cfg(unix)]
            stop_session(&mut child, libc::SIGHUP);
            #[cfg(not(unix))]
            stop_session(&mut child, 0);
            let _ = child.wait();
        }
    }

    #[cfg(not(unix))]
    fn connect(self: &Arc<Self>) -> bool {
        let reason = capabilities()["reason"].as_str().unwrap_or("").to_string();
        self.set_error(&reason);
        self.output.publish(&format!("{reason}\r\n"));
        false
    }

    #[cfg(unix)]
    fn connect(self: &Arc<Self>) -> bool {
        use std::os::unix::process::CommandExt;
        let (cwd, problem) = clean_directory(&self.directory);
        if !problem.is_empty() {
            self.set_error(&problem);
            return false;
        }
        let (cols, rows) = *lock(&self.size);
        let fail = |this: &Local, e: &std::io::Error| {
            let text = format!("The shell could not be started: {}", os_error_text(e));
            this.set_error(&text);
            this.output.publish(&format!("{text}\r\n"));
            false
        };
        let (master, slave) = match pty::open(rows, cols) {
            Ok(p) => p,
            Err(e) => return fail(self, &e),
        };
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let stdio = |fd: &std::os::fd::OwnedFd| fd.try_clone().map(Stdio::from);
        let (a, b, c) = match (stdio(&slave), stdio(&slave), stdio(&slave)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return fail(self, &e),
        };
        let mut cmd = Command::new(&self.shell);
        cmd.arg("-l")
            .current_dir(&cwd)
            .env_clear()
            .envs(shell_env(&env, platform()))
            .stdin(a)
            .stdout(b)
            .stderr(c);
        // SAFETY: only async signal safe calls between fork and exec: a new
        // session, then the pty (already fd 0) as its controlling terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return fail(self, &e),
        };
        drop(cmd);
        drop(slave);
        let master = Arc::new(master);
        {
            let mut st = lock(&self.state);
            st.master = Some(Arc::clone(&master));
            st.child = Some(child);
        }
        self.connected.store(true, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        let me = Arc::clone(self);
        let reader = thread::Builder::new()
            .name(format!("local-{}", self.name))
            .spawn(move || me.read_loop(master));
        match reader {
            Ok(h) => lock(&self.state).reader = Some(h),
            Err(e) => {
                self.close_transport();
                return fail(self, &e);
            }
        }
        let base = Path::new(&self.shell)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.output.publish(&format!("Shell on this computer ({base})\r\n"));
        true
    }

    #[cfg(unix)]
    fn read_loop(self: Arc<Self>, master: Arc<pty::Master>) {
        let mut decoder = Utf8Decoder::default();
        let mut buf = vec![0u8; 65536];
        while self.running.load(Ordering::SeqCst) {
            if !pty::readable(master.fd(), 500) {
                continue;
            }
            // SAFETY: reads into a live buffer from a descriptor the Arc keeps open.
            let n = unsafe { libc::read(master.fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            if n < 0 {
                let kind = std::io::Error::last_os_error().kind();
                if matches!(kind, std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) {
                    continue;
                }
                break; // EIO: the shell and everything on its pty ended
            }
            if n == 0 {
                break;
            }
            self.output.publish(&decoder.decode(&buf[..n as usize]));
        }
        drop(master);
        self.close_transport();
        self.output.publish("\r\n[Shell ended]\r\n");
    }
}

impl Terminal for LocalSession {
    fn name(&self) -> String {
        self.0.name.clone()
    }

    fn host(&self) -> String {
        LOCAL_HOST.into()
    }

    fn port(&self) -> i64 {
        0
    }

    fn username(&self) -> String {
        self.0.username.clone()
    }

    fn connected(&self) -> bool {
        self.0.connected.load(Ordering::SeqCst)
    }

    fn error(&self) -> String {
        lock(&self.0.error).clone()
    }

    fn output(&self) -> &Output {
        &self.0.output
    }

    fn connect(&self) -> bool {
        self.0.connect()
    }

    fn send(&self, data: &str) {
        #[cfg(unix)]
        {
            let master = {
                let st = lock(&self.0.state);
                if !self.connected() {
                    return;
                }
                st.master.clone()
            };
            let Some(master) = master else { return };
            let mut payload = data.as_bytes();
            while !payload.is_empty() {
                // SAFETY: writes from a live buffer to a descriptor kept open by the Arc.
                let n = unsafe { libc::write(master.fd(), payload.as_ptr() as *const _, payload.len()) };
                if n < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                payload = &payload[n as usize..];
            }
        }
        #[cfg(not(unix))]
        let _ = data;
    }

    fn resize(&self, cols: i64, rows: i64) -> bool {
        let cols = cols.clamp(2, 1000) as u16;
        let rows = rows.clamp(1, 1000) as u16;
        *lock(&self.0.size) = (cols, rows);
        #[cfg(unix)]
        {
            let master = lock(&self.0.state).master.clone();
            let Some(master) = master else { return false };
            pty::set_size(master.fd(), rows, cols);
            true
        }
        #[cfg(not(unix))]
        false
    }

    fn disconnect(&self) {
        self.0.close_transport();
        let reader = lock(&self.0.state).reader.take();
        if let Some(h) = reader {
            if h.thread().id() != thread::current().id() {
                let end = Instant::now() + Duration::from_secs(2);
                while !h.is_finished() && Instant::now() < end {
                    thread::sleep(Duration::from_millis(10));
                }
                if h.is_finished() {
                    let _ = h.join();
                }
            }
        }
    }
}

// ------------------------------------------------------------- runner ----

struct Entry {
    child: Arc<Mutex<Child>>,
    reader: JoinHandle<()>,
}

/// Programs started on this computer without a window, stopped with Corvus.
/// Each gets a reader thread that keeps the tail of its output (so its pipe
/// never fills and blocks it) and ends with it.
#[derive(Default)]
pub struct LocalRunner {
    running: Mutex<indexmap::IndexMap<u32, Entry>>,
    closed: AtomicBool,
}

impl LocalRunner {
    pub fn new() -> LocalRunner {
        LocalRunner::default()
    }

    /// Starts `command` in the login shell and says whether it failed at once:
    /// `{ok: true, pid}` while it runs, `{ok, exited: true, code, output}` when
    /// it ended within [`EARLY_EXIT_S`], `{ok: false, error}` when it could not
    /// start.
    pub fn run(&self, command: &str, directory: &str) -> Value {
        let text = crate::peripherals::util::py_strip(command).to_string();
        if text.is_empty() {
            return json!({"ok": false, "error": "There is no command to run."});
        }
        if text.chars().count() > MAX_COMMAND || text.contains('\0') {
            return json!({"ok": false, "error": "That command is too long."});
        }
        let (cwd, problem) = clean_directory(directory);
        if !problem.is_empty() {
            return json!({"ok": false, "error": problem});
        }
        if self.closed.load(Ordering::SeqCst) {
            return json!({"ok": false, "error": "Corvus is shutting down."});
        }
        self.reap();
        let argv = shell_command(&text, "", platform());
        #[cfg(windows)]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("cmd.exe");
            c.raw_arg("/d /s /c")
                .raw_arg(format!("\"{text}\""))
                .creation_flags(0x0800_0000 | 0x0000_0200);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        };
        let (mut pipe_reader, pipe_writer) = match std::io::pipe() {
            Ok(p) => p,
            Err(e) => return json!({"ok": false, "error": format!("It could not be started: {}", os_error_text(&e))}),
        };
        let both = match pipe_writer.try_clone() {
            Ok(w) => w,
            Err(e) => return json!({"ok": false, "error": format!("It could not be started: {}", os_error_text(&e))}),
        };
        cmd.current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(both)
            .stderr(pipe_writer)
            .env_clear()
            .envs(child_env());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async signal safe; its own session so the whole
            // tree can be stopped.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        let child = cmd.spawn();
        drop(cmd);
        let child = match child {
            Ok(c) => c,
            Err(e) => return json!({"ok": false, "error": format!("It could not be started: {}", os_error_text(&e))}),
        };
        let pid = child.id();
        let tail: Arc<Mutex<std::collections::VecDeque<Vec<u8>>>> = Arc::default();
        let reader = {
            let tail = Arc::clone(&tail);
            thread::Builder::new().name(format!("local-run-{pid}")).spawn(move || {
                use std::io::Read;
                let mut buf = [0u8; 4096];
                while let Ok(n) = pipe_reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut t = lock(&tail);
                    t.push_back(buf[..n].to_vec());
                    let mut total: usize = t.iter().map(Vec::len).sum();
                    while total > OUTPUT_TAIL && t.len() > 1 {
                        if let Some(first) = t.pop_front() {
                            total -= first.len();
                        }
                    }
                }
            })
        };
        let reader = match reader {
            Ok(r) => r,
            Err(e) => {
                let mut child = child;
                let _ = child.kill();
                return json!({"ok": false, "error": format!("It could not be started: {}", os_error_text(&e))});
            }
        };
        let child = Arc::new(Mutex::new(child));
        lock(&self.running).insert(
            pid,
            Entry {
                child: Arc::clone(&child),
                reader,
            },
        );
        let end = Instant::now() + Duration::from_secs_f64(EARLY_EXIT_S);
        let code = loop {
            if let Ok(Some(status)) = lock(&child).try_wait() {
                break Some(status.code().unwrap_or(-1));
            }
            if Instant::now() >= end {
                break None;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let Some(code) = code else {
            return json!({"ok": true, "pid": pid});
        };
        let entry = lock(&self.running).shift_remove(&pid);
        if let Some(e) = entry {
            let end = Instant::now() + Duration::from_secs_f64(TERM_TIMEOUT_S);
            while !e.reader.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(10));
            }
            if e.reader.is_finished() {
                let _ = e.reader.join();
            }
        }
        let bytes: Vec<u8> = lock(&tail).iter().flatten().copied().collect();
        let text = String::from_utf8_lossy(&bytes);
        let chars: Vec<char> = text.chars().collect();
        let start = chars.len().saturating_sub(OUTPUT_TAIL);
        let output: String = chars[start..].iter().collect();
        json!({"ok": code == 0, "exited": true, "code": code, "output": output.trim()})
    }

    fn reap(&self) {
        let done: Vec<Entry> = {
            let mut r = lock(&self.running);
            let pids: Vec<u32> = r
                .iter()
                .filter(|(_, e)| matches!(lock(&e.child).try_wait(), Ok(Some(_))))
                .map(|(p, _)| *p)
                .collect();
            pids.into_iter().filter_map(|p| r.shift_remove(&p)).collect()
        };
        for e in done {
            let end = Instant::now() + Duration::from_millis(500);
            while !e.reader.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(10));
            }
            if e.reader.is_finished() {
                let _ = e.reader.join();
            }
        }
    }

    /// How many programs started here are still running.
    pub fn count(&self) -> usize {
        self.reap();
        lock(&self.running).len()
    }

    /// Stops every program started here and joins its reader. Never fails.
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let entries: Vec<Entry> = lock(&self.running).drain(..).map(|(_, e)| e).collect();
        for e in &entries {
            let mut child = lock(&e.child);
            #[cfg(unix)]
            stop_session(&mut child, libc::SIGTERM);
            #[cfg(not(unix))]
            stop_session(&mut child, 0);
        }
        for e in entries {
            let end = Instant::now() + Duration::from_secs_f64(TERM_TIMEOUT_S);
            while !e.reader.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(10));
            }
            if e.reader.is_finished() {
                let _ = e.reader.join();
            }
        }
    }
}
