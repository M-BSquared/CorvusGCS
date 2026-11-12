//! Corvus GCS, the native desktop interface.
//!
//!     corvus-gcs                         run `corvus_core` in this process and show it (no Python)
//!     corvus-gcs --server http://h:8080  connect to a backend elsewhere
//!
//! A running Corvus on this computer is joined, not started twice. The
//! backend is stopped on every way out; see `net::backend`.

mod analysis;
mod app;
mod control;
mod crash;
mod data;
mod fleet;
mod home;
mod logs;
mod map;
mod map3d;
mod mcp;
mod mission;
mod net;
mod notify;
mod panel;
mod plugins;
mod settings;
mod setup;
mod term;
mod update;
mod welcome;

pub use corvus_api::{i18n, ui};

use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Duration;

use eframe::egui;

/// How long the interface has to close after SIGINT/SIGTERM before the
/// backend is killed outright and the process exits.
const SIGNAL_DEADLINE: Duration = Duration::from_secs(10);

/// The interface's context, for the signal handler to ask it to close.
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

#[derive(Debug, Default)]
struct Args {
    server: Option<String>,
    page: Option<String>,
    scroll: Option<f32>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--server" => args.server = Some(value("--server")?),
            "--core" => {}
            "--page" => args.page = Some(value("--page")?),
            "--scroll" => args.scroll = value("--scroll")?.parse().ok(),
            "-h" | "--help" => {
                println!("usage: corvus-gcs [--server http://host:port] [--page setup|TILE|safety:SENSOR|mission|mission:NAME|settings|logs|analysis] [--scroll PX]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

fn install_signal_handler() {
    let result = ctrlc::set_handler(|| {
        if let Some(ctx) = CONTEXT.get() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
        }
        // The window normally closes and main tears everything down. If it
        // does not in time, the process ends and the core's threads with it.
        std::thread::spawn(|| {
            std::thread::sleep(SIGNAL_DEADLINE);
            std::process::exit(130);
        });
    });
    if let Err(e) = result {
        eprintln!("corvus-gcs: no signal handler ({e}); close the window to quit");
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("corvus-gcs: {e}");
            return ExitCode::from(2);
        }
    };
    crash::install();
    install_signal_handler();

    let mut local = None;
    let server = match &args.server {
        Some(url) => match net::http::Server::parse(url) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("corvus-gcs: {e}");
                return ExitCode::from(2);
            }
        },
        None => {
            if let Some(peer) = net::backend::running_peer() {
                eprintln!("corvus-gcs: Corvus is already running, joining it at {}", peer.url());
                peer
            } else {
                let started = net::backend::Backend::start_core();
                match started {
                    Ok(b) => {
                        let s = b.server().clone();
                        local = Some(b);
                        s
                    }
                    Err(e) => {
                        eprintln!("corvus-gcs: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
    };

    let code = app::run(server, &CONTEXT, args.page.as_deref(), args.scroll);
    // The interface (and with it the telemetry thread) is gone; now the
    // backend: the core is stopped and its threads joined, a Python child
    // gets SIGTERM first.
    drop(local);
    code
}
