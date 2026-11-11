//! The SSH transport against a simulated server (the `russh` server on a
//! loopback port: no real sshd, no hardware). The keys in `tests/ssh_keys` are
//! throwaway keys made for these tests; they open nothing anywhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::peripherals::shell::session::{Terminal, Terminals};
use corvus_core::peripherals::ssh::client::Login;
use corvus_core::peripherals::ssh::commands::{compose_power_command, compose_remote_command, shell_quote};
use corvus_core::peripherals::ssh::hostkeys::{decide, Decision, HostKeyFiles};
use corvus_core::peripherals::ssh::run::{fmt_g, run_command, truncate, RunOptions, OUTPUT_LIMIT};
use corvus_core::peripherals::ssh::SshSession;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use serde_json::{json, Value};

const HOST1: &str = include_str!("ssh_keys/host1");
const HOST2: &str = include_str!("ssh_keys/host2");

#[derive(Default)]
struct Log {
    password_attempts: Vec<(String, String)>,
    key_logins: Vec<String>,
    pty: Vec<(String, u32, u32)>,
    resizes: Vec<(u32, u32)>,
    shell_input: String,
    closed: usize,
    commands: Vec<String>,
}

type Pending = Arc<Mutex<HashMap<ChannelId, (String, Vec<u8>)>>>;

#[derive(Clone)]
struct Sshd {
    log: Arc<Mutex<Log>>,
    stdin: Pending,
    #[allow(dead_code)]
    guard: Option<Arc<Guard>>,
}

/// Counts a connection as closed once its handler is gone.
struct Guard(Arc<Mutex<Log>>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.lock().unwrap().closed += 1;
    }
}

impl server::Server for Sshd {
    type Handler = Sshd;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Sshd {
        Sshd {
            guard: Some(Arc::new(Guard(Arc::clone(&self.log)))),
            log: Arc::clone(&self.log),
            stdin: Arc::clone(&self.stdin),
        }
    }
}

impl server::Handler for Sshd {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.log
            .lock()
            .unwrap()
            .password_attempts
            .push((user.to_string(), password.to_string()));
        Ok(if user == "pilot" && password == "hunter2" {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(&mut self, user: &str, _key: server::VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        self.log.lock().unwrap().key_logins.push(user.to_string());
        Ok(if user == "pilot" { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.log.lock().unwrap().pty.push((term.to_string(), cols, rows));
        session.channel_success(channel)
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, "welcome \u{e4}\u{f6}\r\n$ ".as_bytes().to_vec())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.log.lock().unwrap().resizes.push((cols, rows));
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if let Some((_, buf)) = self.stdin.lock().unwrap().get_mut(&channel) {
            buf.extend_from_slice(data);
            return Ok(());
        }
        let text = String::from_utf8_lossy(data).into_owned();
        self.log.lock().unwrap().shell_input.push_str(&text);
        if text.contains("exit") {
            session.data(channel, b"bye\r\n".to_vec())?;
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
        } else {
            session.data(channel, format!("echo:{text}").into_bytes())?;
        }
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let pending = self.stdin.lock().unwrap().remove(&channel);
        if let Some((command, buf)) = pending {
            let given = String::from_utf8_lossy(&buf).into_owned();
            let ok = given == "hunter2\n";
            if command.contains("sudo -S") && !ok {
                session.extended_data(channel, 1, b"Sorry, try again.\n".to_vec())?;
                session.exit_status_request(channel, 1)?;
            } else {
                session.data(channel, format!("stdin={given:?}").into_bytes())?;
                session.exit_status_request(channel, 0)?;
            }
            session.eof(channel)?;
            session.close(channel)?;
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.log.lock().unwrap().commands.push(command.clone());
        session.channel_success(channel)?;
        if command.contains("sudo -S") || command == "read-stdin" {
            self.stdin.lock().unwrap().insert(channel, (command, Vec::new()));
            return Ok(());
        }
        if command.contains("sudo -n") {
            session.extended_data(channel, 1, b"sudo: a password is required\n".to_vec())?;
            session.exit_status_request(channel, 1)?;
        } else if command == "fail" {
            session.extended_data(channel, 1, b"boom\n".to_vec())?;
            session.exit_status_request(channel, 3)?;
        } else if command == "big" {
            session.data(channel, vec![b'a'; 100_000])?;
            session.extended_data(channel, 1, vec![b'e'; 70_000])?;
            session.exit_status_request(channel, 0)?;
        } else if command == "slow" {
            let handle = session.handle();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let _ = handle.close(channel).await;
            });
            return Ok(());
        } else {
            session.data(channel, format!("ran:{command}\n").into_bytes())?;
            session.exit_status_request(channel, 0)?;
        }
        session.eof(channel)?;
        session.close(channel)
    }
}

struct Server {
    port: u16,
    log: Arc<Mutex<Log>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(host_key: &str) -> Server {
        Server::on(host_key, 0)
    }

    fn on(host_key: &str, port: u16) -> Server {
        let key = PrivateKey::from_openssh(host_key).unwrap();
        let log: Arc<Mutex<Log>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (l, s) = (Arc::clone(&log), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let config = Arc::new(server::Config {
                    keys: vec![key],
                    auth_rejection_time: Duration::from_millis(10),
                    auth_rejection_time_initial: Some(Duration::from_millis(0)),
                    ..Default::default()
                });
                let socket = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
                ready_tx.send(socket.local_addr().unwrap().port()).unwrap();
                let mut sshd = Sshd {
                    log: l,
                    stdin: Arc::default(),
                    guard: None,
                };
                let running = sshd.run_on_socket(config, &socket);
                let handle = running.handle();
                tokio::spawn(async move {
                    while !s.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    handle.shutdown("test over".into());
                });
                let _ = running.await;
            });
        });
        let port = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Server {
            port,
            log,
            stop,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("corvus-ssh-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn files(d: &Path, strict: bool) -> HostKeyFiles {
    HostKeyFiles {
        system: Some(d.join("system_known_hosts")),
        corvus: d.join("known_hosts"),
        strict,
    }
}

fn login(password: &str) -> Login {
    Login {
        username: "pilot".into(),
        password: Some(password.into()),
        key_path: None,
        ssh_dir: Some(std::env::temp_dir().join("corvus-ssh-test-no-keys")),
    }
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn collect(t: &Arc<dyn Terminal>) -> Arc<Mutex<String>> {
    let seen: Arc<Mutex<String>> = Arc::default();
    let s = Arc::clone(&seen);
    t.output()
        .add_sub(Arc::new(move |text: &str| s.lock().unwrap().push_str(text)), true);
    seen
}

fn open(terminals: &Terminals, name: &str, port: u16, login: Login, files: &HostKeyFiles) -> bool {
    let (l, f) = (login, files.clone());
    terminals.open(name, &|| {
        SshSession::new(name, "127.0.0.1", port, l.clone(), f.clone()) as Arc<dyn Terminal>
    })
}

#[test]
fn a_shell_is_opened_streamed_resized_and_closed() {
    let d = dir("shell");
    let server = Server::start(HOST1);
    let terminals = Terminals::new();
    assert!(open(
        &terminals,
        "nav",
        server.port,
        login("hunter2"),
        &files(&d, false)
    ));
    let session = terminals.get("nav").unwrap();
    assert!(session.connected());
    assert_eq!(session.username(), "pilot");
    let seen = collect(&session);
    until("the greeting", || {
        seen.lock().unwrap().contains("welcome \u{e4}\u{f6}\r\n$ ")
    });
    assert!(seen
        .lock()
        .unwrap()
        .starts_with(&format!("Connected to pilot@127.0.0.1:{}\r\n", server.port)));
    assert_eq!(
        server.log.lock().unwrap().pty,
        vec![("xterm-256color".to_string(), 80, 24)]
    );

    assert!(terminals.send("nav", "ls\r"));
    until("the echo", || seen.lock().unwrap().contains("echo:ls\r"));
    assert!(terminals.resize("nav", 132, 43));
    until("the resize", || server.log.lock().unwrap().resizes == vec![(132, 43)]);
    assert!(terminals.resize("nav", 5000, 0), "sizes are clamped, not refused");
    until("the clamped resize", || {
        server.log.lock().unwrap().resizes.last() == Some(&(1000, 1))
    });

    assert!(terminals.send("nav", "exit\r"));
    until("the remote end to close", || !session.connected());
    until("the closing line", || {
        seen.lock().unwrap().ends_with("\r\n[Connection closed]\r\n")
    });
    assert!(seen.lock().unwrap().contains("bye\r\n"));
    assert!(!terminals.send("nav", "x"), "nothing is sent to a closed session");
    terminals.shutdown();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_new_connection_under_the_same_name_replaces_the_old_one() {
    let d = dir("replace");
    let server = Server::start(HOST1);
    let terminals = Terminals::new();
    assert!(open(
        &terminals,
        "nav",
        server.port,
        login("hunter2"),
        &files(&d, false)
    ));
    let first = terminals.get("nav").unwrap();
    assert!(open(
        &terminals,
        "nav",
        server.port,
        login("hunter2"),
        &files(&d, false)
    ));
    assert!(!first.connected(), "the first one was closed");
    assert!(terminals.get("nav").unwrap().connected());
    until("the first connection to end at the server", || {
        server.log.lock().unwrap().closed >= 1
    });
    terminals.shutdown();
    until("the second connection to end at the server", || {
        server.log.lock().unwrap().closed >= 2
    });
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_refused_login_says_so_and_is_marked_as_one_more_typing_can_fix() {
    let d = dir("auth");
    let server = Server::start(HOST1);
    let terminals = Terminals::new();
    assert!(!open(&terminals, "nav", server.port, login("wrong"), &files(&d, false)));
    assert_eq!(terminals.connect_error("nav"), "Authentication failed.");
    assert!(terminals.connect_auth_failed("nav"));
    assert!(terminals.get("nav").is_none(), "a failed session is not kept");

    let none = Login {
        username: "pilot".into(),
        password: None,
        key_path: None,
        ssh_dir: Some(d.join("no-keys")),
    };
    assert!(!open(&terminals, "nav", server.port, none, &files(&d, false)));
    assert!(terminals.connect_auth_failed("nav"));
    assert!(server
        .log
        .lock()
        .unwrap()
        .password_attempts
        .iter()
        .all(|(_, p)| !p.is_empty()));
    terminals.shutdown();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_key_file_logs_in_and_a_missing_one_is_an_error_not_a_hang() {
    let d = dir("key");
    let server = Server::start(HOST1);
    let key = format!("{}/tests/ssh_keys/client", env!("CARGO_MANIFEST_DIR"));
    let terminals = Terminals::new();
    let by_key = Login {
        username: "pilot".into(),
        password: Some("ignored".into()),
        key_path: Some(key),
        ssh_dir: None,
    };
    assert!(open(&terminals, "nav", server.port, by_key, &files(&d, false)));
    assert_eq!(server.log.lock().unwrap().key_logins, vec!["pilot".to_string()]);
    assert!(
        server.log.lock().unwrap().password_attempts.is_empty(),
        "a key login sends no password"
    );
    let missing = Login {
        username: "pilot".into(),
        password: None,
        key_path: Some("/no/such/key".into()),
        ssh_dir: None,
    };
    assert!(!open(&terminals, "gone", server.port, missing, &files(&d, false)));
    assert!(!terminals.connect_error("gone").is_empty());
    terminals.shutdown();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_refused_port_is_named_with_its_address() {
    let d = dir("refused");
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let terminals = Terminals::new();
    assert!(!open(&terminals, "nav", port, login("x"), &files(&d, false)));
    assert_eq!(
        terminals.connect_error("nav"),
        format!("Connection refused (127.0.0.1:{port}).")
    );
    assert!(!terminals.connect_auth_failed("nav"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_connect_in_progress_is_cancelled_by_shutdown() {
    let d = dir("cancel");
    // A listener that accepts and says nothing: the banner never comes.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let terminals = Arc::new(Terminals::new());
    let t = Arc::clone(&terminals);
    let f = files(&d, false);
    let attempt = std::thread::spawn(move || open(&t, "nav", port, login("x"), &f));
    std::thread::sleep(Duration::from_millis(300));
    let began = Instant::now();
    terminals.shutdown();
    assert!(!attempt.join().unwrap());
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "shutdown waited {:?}",
        began.elapsed()
    );
    drop(silent);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn host_keys_are_trusted_on_first_use_and_refused_when_they_change() {
    let d = dir("hostkeys");
    let f = files(&d, false);
    let mut first = Server::start(HOST1);
    let port = first.port;
    let terminals = Terminals::new();
    assert!(open(&terminals, "nav", port, login("hunter2"), &f));
    terminals.disconnect("nav");
    let recorded = std::fs::read_to_string(&f.corvus).unwrap();
    let line = recorded.lines().next().unwrap();
    assert!(line.starts_with(&format!("[127.0.0.1]:{port} ssh-ed25519 ")), "{line}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&f.corvus).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(
        open(&terminals, "nav", port, login("hunter2"), &f),
        "the same key is accepted again"
    );
    terminals.disconnect("nav");
    assert_eq!(
        std::fs::read_to_string(&f.corvus).unwrap(),
        recorded,
        "a known key is not written twice"
    );
    first.stop();

    let second = Server::on(HOST2, port);
    assert!(!open(&terminals, "nav", port, login("hunter2"), &f));
    let why = terminals.connect_error("nav");
    assert!(
        why.starts_with("Host key for server '127.0.0.1' does not match: got '"),
        "{why}"
    );
    assert!(why.contains("', expected '"), "{why}");
    assert!(
        why.contains(&f.corvus.display().to_string()),
        "the hint names the file to edit: {why}"
    );
    assert!(
        why.contains("either the companion computer was reinstalled")
            || why.contains("Either the companion computer was reinstalled")
    );
    assert!(!terminals.connect_auth_failed("nav"));
    assert!(
        second.log.lock().unwrap().password_attempts.is_empty(),
        "no password went to the wrong host"
    );
    terminals.shutdown();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_strict_policy_refuses_a_host_it_does_not_know() {
    let d = dir("strict");
    let server = Server::start(HOST1);
    let terminals = Terminals::new();
    assert!(!open(
        &terminals,
        "nav",
        server.port,
        login("hunter2"),
        &files(&d, true)
    ));
    assert_eq!(
        terminals.connect_error("nav"),
        format!("Server '[127.0.0.1]:{}' not found in known_hosts", server.port)
    );
    assert!(!files(&d, true).corvus.exists() || std::fs::read_to_string(files(&d, true).corvus).unwrap().is_empty());
    assert!(server.log.lock().unwrap().password_attempts.is_empty());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_operators_own_file_decides_before_the_corvus_one() {
    let d = dir("system");
    let key1 = PrivateKey::from_openssh(HOST1).unwrap().public_key().clone();
    let key2 = PrivateKey::from_openssh(HOST2).unwrap().public_key().clone();
    let f = files(&d, false);
    let line = |k: &PublicKey| format!("[127.0.0.1]:2222 {}\n", k.to_openssh().unwrap());
    std::fs::write(&f.corvus, line(&key1)).unwrap();
    assert_eq!(decide(&f, "127.0.0.1", 2222, &key1), Decision::Known);
    assert!(matches!(
        decide(&f, "127.0.0.1", 2222, &key2),
        Decision::Mismatch { .. }
    ));
    assert_eq!(
        decide(&f, "127.0.0.1", 2223, &key1),
        Decision::New,
        "another port is another host"
    );
    std::fs::write(f.system.as_ref().unwrap(), line(&key2)).unwrap();
    assert_eq!(decide(&f, "127.0.0.1", 2222, &key2), Decision::Known);
    assert!(
        matches!(decide(&f, "127.0.0.1", 2222, &key1), Decision::Mismatch { .. }),
        "the system file knows the host, so the Corvus file is not asked"
    );
    let strict = HostKeyFiles {
        strict: true,
        ..f.clone()
    };
    assert!(
        matches!(decide(&strict, "other", 22, &key1), Decision::Rejected(m) if m == "Server 'other' not found in known_hosts")
    );
    let _ = std::fs::remove_dir_all(d);
    let _ = Algorithm::Ed25519;
}

fn opts(port: u16, command: &str, password: &str) -> RunOptions {
    RunOptions {
        host: "127.0.0.1".into(),
        command: command.into(),
        port,
        login: login(password),
        timeout: 20.0,
        stdin_data: None,
    }
}

#[test]
fn a_command_returns_its_output_and_exit_status() {
    let d = dir("run");
    let server = Server::start(HOST1);
    let stop = AtomicBool::new(false);
    let f = files(&d, false);

    let ok = run_command(&opts(server.port, "echo hi", "hunter2"), &f, &stop);
    assert_eq!(
        ok,
        json!({"ok": true, "exit_status": 0, "stdout": "ran:echo hi\n", "stderr": "", "error": "", "auth_failed": false})
    );
    let failed = run_command(&opts(server.port, "fail", "hunter2"), &f, &stop);
    assert_eq!(
        failed,
        json!({"ok": false, "exit_status": 3, "stdout": "", "stderr": "boom\n",
               "error": "command exited with status 3", "auth_failed": false})
    );
    let mut with_stdin = opts(server.port, "read-stdin", "hunter2");
    with_stdin.stdin_data = Some("secret\n".into());
    let got = run_command(&with_stdin, &f, &stop);
    assert_eq!(got["stdout"], "stdin=\"secret\\n\"");
    assert!(
        !server.log.lock().unwrap().commands.iter().any(|c| c.contains("secret")),
        "standard input never rides on the command line"
    );

    let wrong = run_command(&opts(server.port, "echo hi", "nope"), &f, &stop);
    assert_eq!(wrong["ok"], false);
    assert_eq!(wrong["auth_failed"], true);
    assert_eq!(wrong["error"], "Authentication failed.");
    assert_eq!(wrong["exit_status"], Value::Null);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_large_output_is_cut_and_marked() {
    let d = dir("big");
    let server = Server::start(HOST1);
    let stop = AtomicBool::new(false);
    let r = run_command(&opts(server.port, "big", "hunter2"), &files(&d, false), &stop);
    let out = r["stdout"].as_str().unwrap();
    assert_eq!(
        out.chars().count(),
        OUTPUT_LIMIT + "\n… output truncated".chars().count()
    );
    assert!(out.ends_with("\n… output truncated"));
    assert!(r["stderr"].as_str().unwrap().ends_with("\n… output truncated"));
    assert_eq!(truncate("short"), "short");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_command_that_outlives_its_timeout_is_ended() {
    let d = dir("slow");
    let server = Server::start(HOST1);
    let stop = AtomicBool::new(false);
    let mut o = opts(server.port, "slow", "hunter2");
    o.timeout = 0.4;
    let began = Instant::now();
    let r = run_command(&o, &files(&d, false), &stop);
    assert_eq!(r["error"], "remote command exceeded 0.4s timeout");
    assert_eq!(r["ok"], false);
    assert!(began.elapsed() < Duration::from_secs(5));
    assert_eq!(fmt_g(20.0), "20");
    assert_eq!(fmt_g(2.5), "2.5");

    let stop = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&stop);
    let f = files(&d, false);
    let port = server.port;
    let worker = std::thread::spawn(move || {
        let o = opts(port, "slow", "hunter2");
        run_command(&o, &f, &s)
    });
    std::thread::sleep(Duration::from_millis(400));
    stop.store(true, Ordering::SeqCst);
    let r = worker.join().unwrap();
    assert_eq!(
        r["error"], "Corvus is shutting down.",
        "the core's stop ends a command that is waiting"
    );
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_command_to_a_changed_host_is_refused_without_a_password() {
    let d = dir("runkey");
    let mut first = Server::start(HOST1);
    let port = first.port;
    let stop = AtomicBool::new(false);
    let f = files(&d, false);
    assert_eq!(run_command(&opts(port, "echo hi", "hunter2"), &f, &stop)["ok"], true);
    first.stop();
    let second = Server::on(HOST2, port);
    let r = run_command(&opts(port, "echo hi", "hunter2"), &f, &stop);
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("does not match"), "{r}");
    assert!(second.log.lock().unwrap().password_attempts.is_empty());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_refused_port_is_worded_the_way_python_words_it() {
    let d = dir("runrefused");
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let stop = AtomicBool::new(false);
    let r = run_command(&opts(port, "x", "x"), &files(&d, false), &stop);
    assert_eq!(
        r["error"],
        format!("[Errno None] Unable to connect to port {port} on 127.0.0.1")
    );
    assert_eq!(r["auth_failed"], false);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_launcher_and_power_lines_are_composed_as_python_composes_them() {
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("/home/pi/app-1.0"), "/home/pi/app-1.0");
    assert_eq!(shell_quote("my dir"), "'my dir'");
    assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    assert_eq!(shell_quote("a$b"), "'a$b'");
    assert_eq!(compose_remote_command("", "ls -l", false), "ls -l");
    assert_eq!(
        compose_remote_command("/opt/my app", "./run.sh --x", false),
        "cd -- '/opt/my app' && ./run.sh --x"
    );
    assert_eq!(
        compose_remote_command("/opt/a", "./run.sh", true),
        "cd -- /opt/a && { nohup ./run.sh >/dev/null 2>&1 & }; echo $!"
    );
    assert_eq!(
        compose_power_command("reboot", true),
        "if [ \"$(id -u)\" -eq 0 ]; then shutdown -r now; else sudo -S -p '' shutdown -r now; fi"
    );
    assert_eq!(
        compose_power_command("shutdown", false),
        "if [ \"$(id -u)\" -eq 0 ]; then shutdown -h now; else sudo -n shutdown -h now; fi"
    );
}

mod routes {
    use super::*;
    use corvus_core::{CoreBuilder, Options as CoreOptions, Peripherals, PeripheralsModule, VehicleModule};

    fn core_in(d: &Path) -> corvus_core::Core {
        CoreBuilder::new(CoreOptions::in_dir(d))
            .module(VehicleModule::new())
            .module(PeripheralsModule::new())
            .build()
            .unwrap()
    }

    #[test]
    fn saved_connections_connect_run_power_and_stop_with_the_core() {
        let d = dir("routes");
        let server = Server::start(HOST1);
        let core = core_in(&d);
        core.start().unwrap();
        let call = |path: &str, body: Value| core.internal_call("POST", path, body.as_object().cloned());
        let port = server.port;

        let (s, a) = call(
            "/api/ssh/connections",
            json!({"name": "nav", "host": "127.0.0.1", "port": port, "username": "pilot", "password": "hunter2"}),
        );
        assert_eq!(s, 200, "{a}");
        assert_eq!(a["connections"][0]["has_password"], true);
        assert_eq!(a["connections"][0]["connected"], false);
        assert!(a["connections"][0].get("password").is_none());
        let (_, a) = call(
            "/api/ssh/connections",
            json!({"name": "nav", "host": "127.0.0.1", "port": port, "username": "pilot"}),
        );
        assert_eq!(
            a["connections"][0]["has_password"], true,
            "an omitted password keeps the stored one"
        );
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!(saved["ssh_connections"][0]["password"], "hunter2");
        assert_eq!(
            call("/api/ssh/connections", json!({"name": "x", "host": "h", "port": true})).0,
            400
        );
        assert_eq!(
            call("/api/ssh/connections", json!({"name": "x", "host": "h", "port": 70000})).1["error"],
            "port must be between 0 and 65535"
        );
        let (_, a) = call(
            "/api/ssh/connections",
            json!({"name": "second", "host": "h", "original_name": "nav"}),
        );
        assert_eq!(a["connections"][0]["name"], "second", "a rename keeps its place");
        assert_eq!(a["connections"][0]["has_password"], true);
        let (_, a) = call(
            "/api/ssh/connections",
            json!({"name": "nav", "host": "127.0.0.1", "port": port, "username": "pilot", "original_name": "second"}),
        );
        assert_eq!(a["connections"][0]["name"], "nav");
        call("/api/ssh/connections", json!({"name": "second", "host": "h"}));
        let (s, a) = call(
            "/api/ssh/connections",
            json!({"name": "second", "host": "h", "original_name": "nav"}),
        );
        assert_eq!(
            (s, a["error"].as_str()),
            (400, Some("\"second\" is already saved. Pick another name."))
        );
        call("/api/ssh/connections/remove", json!({"name": "second"}));

        let (s, a) = call("/api/ssh/connect", json!({"name": "nav"}));
        assert_eq!(s, 200, "{a}");
        assert_eq!(a["connected"], true, "{a}");
        assert_eq!(a["username"], "pilot");
        let (_, list) = core.internal_call("GET", "/api/ssh/connections", None);
        assert_eq!(list["connections"][0]["connected"], true);
        let (_, sessions) = core.internal_call("GET", "/api/ssh/sessions", None);
        assert_eq!(sessions["sessions"][0]["name"], "nav");
        let (_, a) = call("/api/ssh/send", json!({"name": "nav", "data": "ls\r"}));
        assert_eq!(a["ok"], true);
        let known = std::fs::read_to_string(d.join("known_hosts")).unwrap();
        assert!(known.contains(&format!("[127.0.0.1]:{port} ssh-ed25519 ")), "{known}");

        let (_, a) = call("/api/ssh/connect", json!({"name": "ghost"}));
        assert_eq!(a["error"], "no host");
        assert_eq!(a["needs"], "connection");
        let (_, a) = call("/api/ssh/connect", json!({"name": "x", "from": "ghost"}));
        assert_eq!(a["error"], "no saved connection named 'ghost'");
        let (_, a) = call(
            "/api/ssh/connect",
            json!({"name": "wrong", "from": "nav", "password": "bad"}),
        );
        assert_eq!(
            (a["connected"].clone(), a["error"].clone()),
            (json!(false), json!("Authentication failed."))
        );
        assert_eq!(
            (a["needs"].clone(), a["connection"].clone()),
            (json!("credentials"), json!("nav"))
        );
        let (s, a) = call(
            "/api/ssh/connect",
            json!({"name": "x", "host": "127.0.0.1", "username": ""}),
        );
        assert_eq!((s, a["error"].as_str()), (400, Some("username is required")));
        assert_eq!(call("/api/ssh/connect", json!({"name": 5})).0, 400);
        assert_eq!(
            call("/api/ssh/connect", json!({"name": "x", "host": "h", "port": "abc"})).1["error"],
            "port must be an integer"
        );

        let (s, a) = call(
            "/api/ssh/run",
            json!({"name": "nav", "command": " ls ", "directory": " /opt/my app ", "detach": true}),
        );
        assert_eq!(s, 200, "{a}");
        assert_eq!(
            a["command"],
            "cd -- '/opt/my app' && { nohup ls >/dev/null 2>&1 & }; echo $!"
        );
        assert_eq!(
            (a["host"].clone(), a["port"].clone(), a["username"].clone()),
            (json!("127.0.0.1"), json!(port), json!("pilot"))
        );
        assert!(a.get("auth_failed").is_none());
        assert_eq!(a["ok"], true);
        assert_eq!(call("/api/ssh/run", json!({"command": ""})).0, 400);
        assert_eq!(
            call("/api/ssh/run", json!({"command": "ls", "detach": "yes"})).1["error"],
            "detach must be a boolean"
        );
        let (_, a) = call("/api/ssh/run", json!({"command": "ls", "name": "ghost"}));
        assert_eq!(
            (a["needs"].clone(), a["connection"].clone()),
            (json!("connection"), json!("ghost"))
        );
        let (_, a) = call("/api/ssh/run", json!({"command": "ls", "host": "127.0.0.1"}));
        assert_eq!(a["error"], "username is required");
        assert!(
            a.get("needs").is_none(),
            "an ad hoc target has no saved connection to fix"
        );

        let (s, a) = call("/api/ssh/power", json!({"name": "nav", "action": "reboot"}));
        assert_eq!(s, 200, "{a}");
        assert_eq!(a["ok"], true, "the saved password is the sudo password: {a}");
        assert!(a.get("stdout").is_none() && a.get("auth_failed").is_none());
        assert_eq!(
            call("/api/ssh/power", json!({"name": "nav", "action": "format"})).1["error"],
            "action must be reboot or shutdown"
        );
        assert_eq!(
            call("/api/ssh/power", json!({"action": "reboot"})).1["error"],
            "name must be a non-empty string"
        );
        let (_, a) = call("/api/ssh/power", json!({"name": "ghost", "action": "reboot"}));
        assert_eq!(a["error"], "no saved connection named 'ghost'");
        call(
            "/api/ssh/connections",
            json!({"name": "keyless", "host": "127.0.0.1", "port": port, "username": "pilot"}),
        );
        let (_, a) = call("/api/ssh/power", json!({"name": "keyless", "action": "shutdown"}));
        assert_eq!(
            a["error"],
            "sudo needs a password on this computer. Save the password with the connection or allow passwordless sudo.",
            "{a}"
        );

        let (_, a) = call("/api/ssh/connections/remove", json!({"name": "nav"}));
        assert!(a["connections"].as_array().unwrap().iter().all(|c| c["name"] != "nav"));
        let (_, sessions) = core.internal_call("GET", "/api/ssh/sessions", None);
        assert_eq!(
            sessions["sessions"],
            json!([]),
            "removing a connection closes its session"
        );

        let (s, _) = call(
            "/api/ssh/connect",
            json!({"name": "again", "host": "127.0.0.1", "port": port, "username": "pilot", "password": "hunter2"}),
        );
        assert_eq!(s, 200);
        let before = server.log.lock().unwrap().closed;
        core.stop();
        until("the session to end at the server", || {
            server.log.lock().unwrap().closed > before
        });
        let terminals = core.service::<Peripherals>().unwrap();
        assert!(terminals.terminals.list().is_empty());
        let _ = std::fs::remove_dir_all(d);
        let _ = (AtomicUsize::new(0), HashMap::<u8, u8>::new());
    }
}
