//! One command over its own short lived connection (`run_command` in
//! `ssh_bridge.py`): a real exit status and clean stdout and stderr, and
//! nothing left behind. It blocks its calling thread for at most the 8 s
//! connect plus the timeout and owns no state past the call; a caller that
//! wants a program to keep running composes it with `nohup`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use russh::ChannelMsg;
use serde_json::{json, Value};

use super::client::{self, Login};
use super::hostkeys::HostKeyFiles;

/// How much of a command's output is carried back (bytes read, then characters).
pub const OUTPUT_LIMIT: usize = 64 * 1024;
const MARK: &str = "\n… output truncated";

/// `_truncate`: cut to the limit, marked when something was dropped.
pub fn truncate(text: &str) -> String {
    if text.chars().count() <= OUTPUT_LIMIT {
        return text.to_string();
    }
    let cut: String = text.chars().take(OUTPUT_LIMIT).collect();
    format!("{cut}{MARK}")
}

/// `f"{x:g}"` for a timeout.
pub fn fmt_g(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x}");
        s
    }
}

pub struct RunOptions {
    pub host: String,
    pub command: String,
    pub port: u16,
    pub login: Login,
    pub timeout: f64,
    pub stdin_data: Option<String>,
}

fn failure(error: String, auth_failed: bool) -> Value {
    json!({
        "ok": false,
        "exit_status": Value::Null,
        "stdout": "",
        "stderr": "",
        "error": error,
        "auth_failed": auth_failed,
    })
}

fn append_limited(target: &mut Vec<u8>, data: &[u8]) -> bool {
    let room = OUTPUT_LIMIT.saturating_sub(target.len());
    target.extend_from_slice(&data[..data.len().min(room)]);
    data.len() > room
}

/// Runs the command. Never panics; every failure is a message, not an error.
/// `stop` ends the wait early on the core's shutdown path.
pub fn run_command(opts: &RunOptions, files: &HostKeyFiles, stop: &AtomicBool) -> Value {
    let timeout = if opts.timeout.is_finite() && opts.timeout > 0.0 {
        opts.timeout
    } else {
        20.0
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => return failure(e.to_string(), false),
    };
    runtime.block_on(async {
        let handle = match client::connect(&opts.host, opts.port, &opts.login, files).await {
            Ok(h) => h,
            Err(e) => return failure(e.plain, e.auth_failed),
        };
        let result = exec(&handle, opts, timeout, stop).await;
        let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        result
    })
}

async fn exec(
    handle: &russh::client::Handle<client::KeyHandler>,
    opts: &RunOptions,
    timeout: f64,
    stop: &AtomicBool,
) -> Value {
    let mut channel = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) => return failure(e.to_string(), false),
    };
    if let Err(e) = channel.exec(true, opts.command.clone()).await {
        return failure(e.to_string(), false);
    }
    if let Some(data) = &opts.stdin_data {
        if channel.data_bytes(data.clone().into_bytes()).await.is_err() || channel.eof().await.is_err() {
            return failure("Socket is closed".into(), false);
        }
    }
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_cut, mut err_cut) = (false, false);
    let mut status: Option<u32> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = channel.close().await;
            return failure(format!("remote command exceeded {}s timeout", fmt_g(timeout)), false);
        }
        if stop.load(Ordering::SeqCst) {
            let _ = channel.close().await;
            return failure("Corvus is shutting down.".into(), false);
        }
        match tokio::time::timeout(left.min(Duration::from_millis(50)), channel.wait()).await {
            Err(_) => {}
            Ok(None) => break,
            Ok(Some(ChannelMsg::Data { data })) => out_cut = append_limited(&mut out, &data) || out_cut,
            Ok(Some(ChannelMsg::ExtendedData { data, ext })) => {
                if ext == 1 {
                    err_cut = append_limited(&mut err, &data) || err_cut;
                }
            }
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => status = Some(exit_status),
            Ok(Some(ChannelMsg::Close)) => break,
            Ok(Some(_)) => {}
        }
    }
    let status = status.map(i64::from).unwrap_or(-1);
    let mut out_text = String::from_utf8_lossy(&out).into_owned();
    let mut err_text = String::from_utf8_lossy(&err).into_owned();
    if out_cut {
        out_text.push_str(MARK);
    }
    if err_cut {
        err_text.push_str(MARK);
    }
    json!({
        "ok": status == 0,
        "exit_status": status,
        "stdout": truncate(&out_text),
        "stderr": truncate(&err_text),
        "error": if status == 0 { String::new() } else { format!("command exited with status {status}") },
        "auth_failed": false,
    })
}
