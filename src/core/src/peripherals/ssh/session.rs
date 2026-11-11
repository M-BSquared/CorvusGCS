//! One interactive SSH session (`SshSession` in `ssh_bridge.py`): a shell on a
//! pty of the remote computer whose output is streamed byte for byte, escape
//! sequences included, to the subscribers of the terminal registry.
//!
//! The session owns one thread. It runs a private single threaded tokio
//! runtime, connects, opens the shell and then serves input, resizes and the
//! close request while it reads. `disconnect` asks it to close and waits for
//! it for two seconds; `connect` can be cancelled by a `disconnect` that comes
//! in meanwhile, so no connect attempt holds the core's shutdown for 8 s.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use russh::ChannelMsg;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use super::client::{self, Login};
use super::hostkeys::HostKeyFiles;
use crate::peripherals::shell::session::{Output, Terminal};
use crate::peripherals::video::stream::Task;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

enum Cmd {
    Input(Vec<u8>),
    Resize(u32, u32),
    Close,
}

struct Ready {
    ok: bool,
    error: String,
    auth_failed: bool,
}

#[derive(Default)]
struct State {
    thread: Option<Arc<Task>>,
    tx: Option<UnboundedSender<Cmd>>,
    error: String,
    auth_failed: bool,
}

/// What the session's thread publishes into and flips.
struct Shared {
    out: Output,
    connected: AtomicBool,
}

pub struct SshSession {
    name: String,
    host: String,
    port: u16,
    login: Login,
    files: HostKeyFiles,
    shared: Arc<Shared>,
    state: Mutex<State>,
    cancel: AtomicBool,
    cols: AtomicI64,
    rows: AtomicI64,
}

impl SshSession {
    pub fn new(name: &str, host: &str, port: u16, login: Login, files: HostKeyFiles) -> Arc<SshSession> {
        Arc::new(SshSession {
            name: name.to_string(),
            host: host.to_string(),
            port,
            login,
            files,
            shared: Arc::new(Shared {
                out: Output::default(),
                connected: AtomicBool::new(false),
            }),
            state: Mutex::new(State::default()),
            cancel: AtomicBool::new(false),
            cols: AtomicI64::new(80),
            rows: AtomicI64::new(24),
        })
    }

    fn close_transport(&self) {
        self.shared.connected.store(false, Ordering::SeqCst);
        let tx = lock(&self.state).tx.take();
        if let Some(tx) = tx {
            let _ = tx.send(Cmd::Close);
        }
    }
}

/// Decodes UTF-8 incrementally: a read ends wherever the channel's window did,
/// often inside a character, and decoding each read alone would turn every
/// split character into a replacement mark.
#[derive(Default)]
struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    fn push(&mut self, data: &[u8]) -> String {
        self.pending.extend_from_slice(data);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    return out;
                }
                Err(e) => {
                    let good = e.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.pending[..good]).unwrap_or(""));
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{fffd}');
                            self.pending.drain(..good + n);
                        }
                        None => {
                            self.pending.drain(..good);
                            return out;
                        }
                    }
                }
            }
        }
    }
}

impl Terminal for SshSession {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn host(&self) -> String {
        self.host.clone()
    }

    fn port(&self) -> i64 {
        i64::from(self.port)
    }

    fn username(&self) -> String {
        self.login.username.clone()
    }

    fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    fn connect(&self) -> bool {
        let (ready_tx, ready_rx) = mpsc::channel::<Ready>();
        let (cmd_tx, cmd_rx) = unbounded_channel::<Cmd>();
        let (host, port, login, files) = (self.host.clone(), self.port, self.login.clone(), self.files.clone());
        let (cols, rows) = (
            self.cols.load(Ordering::SeqCst).clamp(2, 1000) as u32,
            self.rows.load(Ordering::SeqCst).clamp(1, 1000) as u32,
        );
        let shared = Arc::clone(&self.shared);
        let task = Task::spawn(format!("ssh-{}", self.name), move || {
            serve(&shared, &host, port, &login, &files, cols, rows, cmd_rx, ready_tx);
        });
        let task = Arc::new(task);
        {
            let mut st = lock(&self.state);
            st.thread = Some(Arc::clone(&task));
            st.tx = Some(cmd_tx);
        }
        let ready = loop {
            match ready_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(r) => break r,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.cancel.load(Ordering::SeqCst) {
                        self.close_transport();
                        task.join(Duration::from_secs(3));
                        lock(&self.state).error = "Connection cancelled.".into();
                        return false;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Ready {
                        ok: false,
                        error: "The connection thread ended.".into(),
                        auth_failed: false,
                    }
                }
            }
        };
        if ready.ok {
            self.shared.connected.store(true, Ordering::SeqCst);
            self.shared.out.publish(&format!(
                "Connected to {}@{}:{}\r\n",
                self.login.username, self.host, self.port
            ));
            return true;
        }
        {
            let mut st = lock(&self.state);
            st.error = ready.error;
            st.auth_failed = ready.auth_failed;
            st.tx = None;
        }
        task.join(Duration::from_secs(3));
        false
    }

    fn error(&self) -> String {
        lock(&self.state).error.clone()
    }

    fn auth_failed(&self) -> bool {
        lock(&self.state).auth_failed
    }

    fn send(&self, data: &str) {
        if !self.connected() {
            return;
        }
        if let Some(tx) = &lock(&self.state).tx {
            let _ = tx.send(Cmd::Input(data.as_bytes().to_vec()));
        }
    }

    fn resize(&self, cols: i64, rows: i64) -> bool {
        let cols = cols.clamp(2, 1000);
        let rows = rows.clamp(1, 1000);
        self.cols.store(cols, Ordering::SeqCst);
        self.rows.store(rows, Ordering::SeqCst);
        if !self.connected() {
            return false;
        }
        match &lock(&self.state).tx {
            Some(tx) => tx.send(Cmd::Resize(cols as u32, rows as u32)).is_ok(),
            None => false,
        }
    }

    fn disconnect(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.close_transport();
        let task = lock(&self.state).thread.clone();
        if let Some(t) = task {
            if std::thread::current().name() != Some(&format!("ssh-{}", self.name)) {
                t.join(Duration::from_secs(2));
            }
        }
    }

    fn output(&self) -> &Output {
        &self.shared.out
    }
}

#[allow(clippy::too_many_arguments)]
fn serve(
    me: &Shared,
    host: &str,
    port: u16,
    login: &Login,
    files: &HostKeyFiles,
    cols: u32,
    rows: u32,
    mut cmds: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    ready: mpsc::Sender<Ready>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            let _ = ready.send(Ready {
                ok: false,
                error: e.to_string(),
                auth_failed: false,
            });
            return;
        }
    };
    runtime.block_on(async {
        let connecting = async {
            let handle = client::connect(host, port, login, files).await?;
            let channel = handle
                .channel_open_session()
                .await
                .map_err(|e| client::ConnectError::plain(e.to_string()))?;
            channel
                .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
                .await
                .map_err(|e| client::ConnectError::plain(e.to_string()))?;
            channel
                .request_shell(true)
                .await
                .map_err(|e| client::ConnectError::plain(e.to_string()))?;
            Ok::<_, client::ConnectError>((handle, channel))
        };
        // A close request that comes in while connecting ends the attempt.
        let cancelled = async {
            loop {
                match cmds.recv().await {
                    Some(Cmd::Close) | None => return,
                    Some(_) => {}
                }
            }
        };
        let opened = tokio::select! {
            r = connecting => r,
            () = cancelled => Err(client::ConnectError::plain("Connection cancelled.")),
        };
        let (handle, mut channel) = match opened {
            Ok(x) => x,
            Err(e) => {
                let text = if e.bad_host_key {
                    format!("Connection refused: {}\r\n", e.message.replace('\n', "\r\n"))
                } else {
                    format!("Connection failed: {}\r\n", e.message)
                };
                me.out.publish(&text);
                let _ = ready.send(Ready {
                    ok: false,
                    error: e.message,
                    auth_failed: e.auth_failed,
                });
                return;
            }
        };
        let _ = ready.send(Ready {
            ok: true,
            error: String::new(),
            auth_failed: false,
        });
        let mut decoder = Utf8Stream::default();
        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Input(bytes)) => {
                        if channel.data_bytes(bytes).await.is_err() {
                            break;
                        }
                    }
                    Some(Cmd::Resize(c, r)) => {
                        let _ = channel.window_change(c, r, 0, 0).await;
                    }
                    Some(Cmd::Close) | None => break,
                },
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) => me.out.publish(&decoder.push(&data)),
                    Some(ChannelMsg::ExtendedData { data, .. }) => me.out.publish(&decoder.push(&data)),
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    Some(_) => {}
                },
            }
        }
        me.connected.store(false, Ordering::SeqCst);
        let _ = channel.close().await;
        let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        me.out.publish("\r\n[Connection closed]\r\n");
    });
}
