//! Simulated companion computers behind one SSH port, for the pictures with a
//! terminal: each account is a machine with a banner, a prompt and answers to
//! the commands the pictures type. The server is the one of the core's SSH
//! tests (`src/core/tests/peripherals_ssh.rs`), and so is its throwaway key.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use russh::keys::PrivateKey;
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};

pub const PORT: u16 = 2222;
pub const PASSWORD: &str = "corvus-scene";
/// The saved connections of a scene: the name and the account behind it.
pub const ACCOUNTS: [(&str, &str); 3] = [
    ("Companion computer", "jetson"),
    ("Payload computer", "pi"),
    ("Mission computer", "uav"),
];
const KEY: &str = include_str!("../../core/tests/ssh_keys/host1");
const GREEN: &str = "\x1b[01;32m";
const BLUE: &str = "\x1b[01;34m";
const PLAIN: &str = "\x1b[00m";

struct Machine {
    user: &'static str,
    host: &'static str,
    banner: &'static str,
    /// Typed as soon as a session opens.
    typed: &'static [&'static str],
    answers: &'static [(&'static str, &'static str)],
    /// Commands that keep running until Ctrl-C.
    running: &'static [&'static str],
}

const MACHINES: [Machine; 3] = [
    Machine {
        user: "jetson",
        host: "companion",
        banner: "Ubuntu 22.04.4 LTS, Jetson Orin Nano\nAutopilot on /dev/ttyTHS1, 921600 baud\n\nLast login: Thu Sep 24 09:12:40 2026\n",
        typed: &["uptime -p", "pgrep -l mavlink", "ip -br addr"],
        answers: &[
            ("uptime -p", "up 38 minutes"),
            ("pgrep -l mavlink", "912 mavlink-routerd"),
            (
                "ip -br addr",
                "lo     UNKNOWN  127.0.0.1/8\neth0   UP       192.168.144.20/24\nwlan0  DOWN",
            ),
        ],
        running: &[],
    },
    Machine {
        user: "pi",
        host: "payload",
        banner: "Linux payload 6.6.51+rpt-rpi-2712 #1 SMP PREEMPT Debian 1:6.6.51-1+rpt3 aarch64\n\nLast login: Thu Sep 24 09:58:03 2026 from 192.168.144.11\n",
        typed: &["ls payload", "python3 payload/camera.py --status", "vcgencmd measure_temp"],
        answers: &[
            ("ls payload", "camera.py  gimbal.py  config.yaml  README.md"),
            (
                "python3 payload/camera.py --status",
                "camera   IMX477 12.3 MP   streaming   rtsp://0.0.0.0:8554/main   30 fps\ngimbal   SIYI A8 mini     locked      pitch -45.0\u{b0}  yaw 0.0\u{b0}\nstorage  /media/pi/SD     212 images  18.4 GB free",
            ),
            ("vcgencmd measure_temp", "temp=52.6'C"),
        ],
        running: &[],
    },
    Machine {
        user: "uav",
        host: "companion",
        banner: "Ubuntu 22.04.4 LTS, Jetson Orin Nano\n",
        typed: &[],
        answers: &[
            (
                "./start_mission.sh",
                "[mission] loading missions/campus_hop.yaml\n[mission] 6 items, 1.1 km, 40 m AGL\n[mission] waiting for \x1b[01;32moffboard ready\x1b[00m from PX4 ...",
            ),
            (
                "python3 record.py --out /data/flights",
                "[recorder] camera IMX477 at 30 fps, rosbag on /data/flights\n[recorder] \x1b[01;32mrecording\x1b[00m 2026-09-24_10-41-07.bag",
            ),
            (
                "ros2 launch payload gimbal.launch.py",
                "[INFO] [gimbal_node-1]: process started with pid [2417]\n[gimbal_node-1] [INFO] SIYI A8 mini on 192.168.144.25, locked",
            ),
            (
                "df -h /data",
                "Filesystem      Size  Used Avail Use% Mounted on\n/dev/nvme0n1p2  465G  112G  353G  25% /data",
            ),
        ],
        running: &[
            "./start_mission.sh",
            "python3 record.py --out /data/flights",
            "ros2 launch payload gimbal.launch.py",
        ],
    },
];

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

/// One shell: the line being typed, the folder, and whether a program runs.
struct Shell {
    machine: &'static Machine,
    line: String,
    folder: String,
    running: bool,
}

impl Shell {
    fn new(machine: &'static Machine) -> Shell {
        Shell {
            machine,
            line: String::new(),
            folder: "~".into(),
            running: false,
        }
    }

    fn prompt(&self) -> String {
        let m = self.machine;
        format!("{GREEN}{}@{}{PLAIN}:{BLUE}{}{PLAIN}$ ", m.user, m.host, self.folder)
    }

    /// What a session shows as it opens.
    fn hello(&mut self) -> String {
        let mut out = crlf(self.machine.banner) + &self.prompt();
        for c in self.machine.typed {
            out += &self.feed(&format!("{c}\r"));
        }
        out
    }

    fn run(&mut self, command: &str) -> String {
        // A launcher types `cd -- <folder> && <command>`.
        let mut command = command.trim();
        while let Some(rest) = command.strip_prefix("cd ") {
            let (folder, next) = rest.split_once("&&").unwrap_or((rest, ""));
            let folder = folder.trim().trim_start_matches("--").trim().trim_matches(['\'', '"']);
            self.folder = folder
                .replace("$HOME", "~")
                .replace(&format!("/home/{}", self.machine.user), "~");
            command = next.trim();
        }
        if command.is_empty() {
            return String::new();
        }
        self.running = self.machine.running.contains(&command);
        match self.machine.answers.iter().find(|a| a.0 == command) {
            Some((_, answer)) => crlf(answer) + "\r\n",
            None => format!(
                "-bash: {}: command not found\r\n",
                command.split(' ').next().unwrap_or("")
            ),
        }
    }

    fn feed(&mut self, typed: &str) -> String {
        let mut out = String::new();
        for c in typed.chars() {
            match c {
                '\x03' => {
                    self.running = false;
                    self.line.clear();
                    out += "^C\r\n";
                    out += &self.prompt();
                }
                _ if self.running => {}
                '\r' | '\n' => {
                    out += "\r\n";
                    let line = std::mem::take(&mut self.line);
                    out += &self.run(&line);
                    if !self.running {
                        out += &self.prompt();
                    }
                }
                '\x7f' | '\x08' => {
                    if self.line.pop().is_some() {
                        out += "\x08 \x08";
                    }
                }
                c if c >= ' ' => {
                    self.line.push(c);
                    out.push(c);
                }
                _ => {}
            }
        }
        out
    }
}

#[derive(Clone, Default)]
struct Sshd {
    user: String,
    shells: Arc<Mutex<HashMap<ChannelId, Shell>>>,
}

impl Sshd {
    fn machine(&self) -> Option<&'static Machine> {
        MACHINES.iter().find(|m| m.user == self.user)
    }
}

impl server::Server for Sshd {
    type Handler = Sshd;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Sshd {
        Sshd::default()
    }
}

impl server::Handler for Sshd {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.user = user.to_string();
        Ok(if self.machine().is_some() && password == PASSWORD {
            Auth::Accept
        } else {
            Auth::reject()
        })
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
        _term: &str,
        _cols: u32,
        _rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let Some(machine) = self.machine() else { return Ok(()) };
        let mut shell = Shell::new(machine);
        let hello = shell.hello();
        if let Ok(mut s) = self.shells.lock() {
            s.insert(channel, shell);
        }
        session.data(channel, hello.into_bytes())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        let typed = String::from_utf8_lossy(data).into_owned();
        let out = self
            .shells
            .lock()
            .ok()
            .and_then(|mut s| s.get_mut(&channel).map(|s| s.feed(&typed)));
        match out {
            Some(t) if !t.is_empty() => session.data(channel, t.into_bytes()),
            _ => Ok(()),
        }
    }

    /// A program started in the background: it is, and that is all.
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)
    }
}

pub struct Companion {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Companion {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Serves the machines on `127.0.0.1:PORT`.
pub fn start() -> Result<Companion, String> {
    let key = PrivateKey::from_openssh(KEY).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let (tell, ready) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tell.send(Err(e.to_string()));
                return;
            }
        };
        rt.block_on(async move {
            let config = Arc::new(server::Config {
                keys: vec![key],
                ..Default::default()
            });
            let socket = match tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = tell.send(Err(format!("port {PORT}: {e}")));
                    return;
                }
            };
            let _ = tell.send(Ok(()));
            let mut sshd = Sshd::default();
            let running = sshd.run_on_socket(config, &socket);
            let handle = running.handle();
            tokio::spawn(async move {
                while !flag.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                handle.shutdown("the picture is taken".into());
            });
            let _ = running.await;
        });
    });
    ready
        .recv_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())
        .and_then(|r| r)?;
    Ok(Companion {
        stop,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_answers_runs_and_stops() {
        let mut s = Shell::new(&MACHINES[2]);
        assert!(s.hello().ends_with("$ "));
        let out = s.feed("cd -- ~/mission && ./start_mission.sh\r");
        assert!(out.contains("campus_hop") && !out.ends_with("$ ") && s.running);
        assert_eq!(s.folder, "~/mission");
        assert_eq!(s.feed("ls\r"), "", "it keeps running");
        assert!(s.feed("\x03").ends_with("$ ") && !s.running);
        assert!(s.feed("nope\r").contains("command not found"));
        assert!(Shell::new(&MACHINES[0]).hello().contains("192.168.144.20"));
    }
}
