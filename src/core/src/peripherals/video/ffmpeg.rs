//! The ffmpeg boundary (`video.py`): finding the program, the arguments that
//! turn one camera into JPEG frames on a pipe, the ffconcat list that keeps a
//! password off the command line, and the cutting of the byte stream into
//! images. ffmpeg stays a child process; nothing here decodes video.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::model::{DEFAULT_TRANSPORT, TRANSPORT_TCP};
use super::url;
use crate::peripherals::child_env::child_env;
use crate::peripherals::util::which;

pub const MAX_WIDTH: u32 = 1280;
pub const JPEG_QUALITY: u32 = 5;
pub const SOCKET_TIMEOUT_S: f64 = 8.0;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const FFMPEG_ENV: &str = "CORVUS_FFMPEG";

const COMMON_FFMPEG_PATHS: [&str; 5] = [
    "/opt/homebrew/bin/ffmpeg",
    "/usr/local/bin/ffmpeg",
    "/usr/bin/ffmpeg",
    "/snap/bin/ffmpeg",
    "/opt/local/bin/ffmpeg",
];

/// Where winget, Scoop and Chocolatey put ffmpeg on Windows.
pub fn windows_ffmpeg_paths(env: &dyn Fn(&str) -> String) -> Vec<String> {
    let join = |base: String, parts: &[&str]| {
        let mut path = std::path::PathBuf::from(base);
        for p in parts {
            path.push(p);
        }
        path.to_string_lossy().into_owned()
    };
    let mut out = Vec::new();
    let local = env("LOCALAPPDATA");
    if !local.is_empty() {
        out.push(join(local, &["Microsoft", "WinGet", "Links", "ffmpeg.exe"]));
    }
    let profile = env("USERPROFILE");
    if !profile.is_empty() {
        out.push(join(profile, &["scoop", "shims", "ffmpeg.exe"]));
    }
    let data = env("ProgramData");
    if !data.is_empty() {
        out.push(join(data, &["chocolatey", "bin", "ffmpeg.exe"]));
    }
    out
}

pub fn is_executable(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let p = Path::new(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn expand_user(path: &str) -> String {
    if path == "~" || path.starts_with("~/") || (cfg!(windows) && path.starts_with("~\\")) {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        if !home.is_empty() {
            return format!("{home}{}", &path[1..]);
        }
    }
    path.to_string()
}

/// `(path, where it came from)`, or `("", "")` when there is no ffmpeg. A
/// configured path that does not exist is not silently replaced by another
/// one: the operator asked for that binary.
pub fn resolve_ffmpeg(configured: &str) -> (String, String) {
    let configured = crate::peripherals::util::py_strip(configured);
    if !configured.is_empty() {
        let path = expand_user(configured);
        if is_executable(&path) {
            return (path, "configured".into());
        }
        if cfg!(windows) && !path.to_lowercase().ends_with(".exe") && is_executable(&format!("{path}.exe")) {
            return (format!("{path}.exe"), "configured".into());
        }
        return (String::new(), String::new());
    }
    let env = std::env::var(FFMPEG_ENV).unwrap_or_default();
    let env = crate::peripherals::util::py_strip(&env);
    if !env.is_empty() && is_executable(&expand_user(env)) {
        return (expand_user(env), "environment".into());
    }
    if let Some(found) = which("ffmpeg") {
        return (found.to_string_lossy().into_owned(), "path".into());
    }
    let candidates: Vec<String> = if cfg!(windows) {
        windows_ffmpeg_paths(&|k| std::env::var(k).unwrap_or_default())
    } else {
        COMMON_FFMPEG_PATHS.iter().map(|s| s.to_string()).collect()
    };
    for c in candidates {
        if is_executable(&c) {
            return (c, "path".into());
        }
    }
    (String::new(), String::new())
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}
#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

/// The command for one ffmpeg run: the cleaned environment, no window.
pub fn command(path: &str) -> Command {
    let mut cmd = Command::new(path);
    cmd.env_clear().envs(child_env());
    no_window(&mut cmd);
    cmd
}

/// `"7.1"` from `ffmpeg -version`, or `""` when it cannot be read.
pub fn ffmpeg_version(path: &str) -> String {
    let mut cmd = command(path);
    cmd.args(["-hide_banner", "-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return String::new();
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(out) = stdout.as_mut() {
            let _ = std::io::Read::read_to_end(out, &mut buf);
        }
        buf
    });
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    let first = text.lines().next().unwrap_or("");
    let words: Vec<&str> = first.split_whitespace().collect();
    if words.len() >= 3 && words[0] == "ffmpeg" && words[1] == "version" {
        words[2].to_string()
    } else {
        String::new()
    }
}

/// The major version from `"8.1.2"`, `"n7.1"` or `"4.4.2-0ubuntu1"`; `None`
/// for a git build, which is newer than any release this has to tell apart.
pub fn ffmpeg_major(version: &str) -> Option<u32> {
    let v = crate::peripherals::util::py_strip(version);
    let v = v.strip_prefix('n').unwrap_or(v);
    let digits = v.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || !v[digits..].starts_with('.') {
        return None;
    }
    let minor = &v[digits + 1..];
    if !minor.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    v[..digits].parse().ok()
}

/// Whether this ffmpeg can take the address from a file (ffconcat `option`, 5.0+).
pub fn hides_password(version: &str) -> bool {
    ffmpeg_major(version).is_none_or(|m| m >= 5)
}

fn protocols(scheme: &str) -> &'static str {
    match scheme {
        "rtsp" => "rtsp,tcp,udp,rtp",
        "rtsps" => "rtsps,rtsp,tcp,tls,udp,rtp",
        "udp" => "udp,rtp",
        "srt" => "srt",
        "http" | "https" => "http,https,tcp,tls,crypto",
        _ => "tcp",
    }
}

/// The options ffmpeg opens one camera with, in order.
pub fn input_options(url: &str, transport: &str, version: &str) -> Vec<(String, String)> {
    let scheme = url::urlsplit(url).map(|p| p.scheme.to_lowercase()).unwrap_or_default();
    let timeout_us = ((SOCKET_TIMEOUT_S * 1_000_000.0) as i64).to_string();
    let mut opts = vec![("protocol_whitelist".to_string(), protocols(&scheme).to_string())];
    if scheme == "rtsp" || scheme == "rtsps" {
        opts.push((
            "rtsp_transport".into(),
            if scheme == "rtsps" { TRANSPORT_TCP } else { transport }.to_string(),
        ));
        let old = ffmpeg_major(version).is_some_and(|m| m < 5);
        opts.push((if old { "stimeout" } else { "timeout" }.into(), timeout_us));
    } else if scheme == "udp" || scheme == "srt" {
        opts.push(("timeout".into(), timeout_us));
    } else {
        opts.push(("rw_timeout".into(), timeout_us));
    }
    for (k, v) in [
        ("fflags", "nobuffer"),
        ("analyzeduration", "1000000"),
        ("probesize", "1000000"),
    ] {
        opts.push((k.into(), v.into()));
    }
    opts
}

fn ffconcat_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The ffconcat file that hands ffmpeg the address, password included.
pub fn concat_list(url: &str, transport: &str, version: &str) -> String {
    let mut lines = vec![
        "ffconcat version 1.0".to_string(),
        format!("file {}", ffconcat_quote(url)),
    ];
    for (k, v) in input_options(url, transport, version) {
        lines.push(format!("option {k} {}", ffconcat_quote(&v)));
    }
    lines.join("\n") + "\n"
}

fn output_args() -> Vec<String> {
    [
        "-an".to_string(),
        "-sn".into(),
        "-dn".into(),
        "-vf".into(),
        format!("scale='min({MAX_WIDTH},iw)':-2"),
        "-f".into(),
        "mjpeg".into(),
        "-q:v".into(),
        JPEG_QUALITY.to_string(),
        "pipe:1".into(),
    ]
    .to_vec()
}

/// The argv that turns one camera into JPEG frames on stdout. With
/// `list_path` the address (and its password) is in that ffconcat file and the
/// argv carries only the file's name.
pub fn ffmpeg_command(ffmpeg: &str, url: &str, transport: &str, list_path: &str, version: &str) -> Vec<String> {
    let mut cmd: Vec<String> = [ffmpeg, "-hide_banner", "-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if !list_path.is_empty() {
        for a in [
            "-protocol_whitelist",
            "file",
            "-f",
            "concat",
            "-safe",
            "0",
            "-analyzeduration",
            "1000000",
            "-probesize",
            "1000000",
            "-flags",
            "low_delay",
            "-i",
            list_path,
        ] {
            cmd.push(a.to_string());
        }
    } else {
        for (k, v) in input_options(url, transport, version) {
            cmd.push(format!("-{k}"));
            cmd.push(v);
        }
        for a in ["-flags", "low_delay", "-i", url] {
            cmd.push(a.to_string());
        }
    }
    cmd.extend(output_args());
    cmd
}

/// The default transport, for callers that have none.
pub fn default_transport() -> &'static str {
    DEFAULT_TRANSPORT
}

/// Cuts ffmpeg's MJPEG byte stream into whole JPEG images: everything from a
/// start-of-image marker to the next end-of-image marker.
pub struct JpegSplitter {
    buf: Vec<u8>,
    scan: usize,
    max: usize,
}

const SOI: [u8; 2] = [0xff, 0xd8];
const EOI: [u8; 2] = [0xff, 0xd9];

fn find(hay: &[u8], needle: &[u8; 2], from: usize) -> Option<usize> {
    if hay.len() < 2 || from + 1 > hay.len() {
        return None;
    }
    (from..hay.len() - 1).find(|&i| hay[i] == needle[0] && hay[i + 1] == needle[1])
}

impl JpegSplitter {
    pub fn new(max_bytes: usize) -> JpegSplitter {
        JpegSplitter {
            buf: Vec::new(),
            scan: 0,
            max: max_bytes,
        }
    }

    /// Adds `data`; returns every image it completed, oldest first.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            let Some(start) = find(&self.buf, &SOI, 0) else {
                // A trailing FF may be the first half of a marker.
                let keep = self.buf.len().saturating_sub(1);
                self.buf.drain(..keep);
                self.scan = 0;
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
                self.scan = 0;
            }
            let Some(end) = find(&self.buf, &EOI, self.scan.max(2)) else {
                if self.buf.len() > self.max {
                    self.buf.clear();
                    self.scan = 0;
                } else {
                    self.scan = self.buf.len().saturating_sub(1).max(2);
                }
                break;
            };
            frames.push(self.buf[..end + 2].to_vec());
            self.buf.drain(..end + 2);
            self.scan = 0;
        }
        frames
    }
}

/// `(width, height)` from a JPEG's frame header.
pub fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    let n = data.len();
    while i + 9 < n {
        if data[i] != 0xff {
            return None;
        }
        let marker = data[i + 1];
        if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            i += 2;
            continue;
        }
        let seg = ((data[i + 2] as usize) << 8) | data[i + 3] as usize;
        if matches!(marker, 0xc0..=0xc2) {
            let height = ((data[i + 5] as u32) << 8) | data[i + 6] as u32;
            let width = ((data[i + 7] as u32) << 8) | data[i + 8] as u32;
            return Some((width, height));
        }
        i += 2 + seg;
    }
    None
}

/// Creates `path` readable by this account only, and never over another file.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(path)?;
    f.write_all(text.as_bytes())
}

/// Removes `path`; whether it is gone now. Windows refuses while ffmpeg holds
/// the file, so the caller tries again once it has exited.
pub fn unlink(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}
