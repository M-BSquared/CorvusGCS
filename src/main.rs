//! Corvus GCS, the native desktop interface.
//!
//!     corvus-gcs                         run the backend in this process and show it
//!     corvus-gcs --server http://h:8080  connect to a backend elsewhere
//!     corvus-gcs --root DIR --python PY  run the Python backend (serve.py) as a child instead
//!
//! The backend is `corvus_core`, started in this process and stopped on every
//! way out; see `net::backend`.

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

use std::path::PathBuf;
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
    root: Option<PathBuf>,
    python: Option<PathBuf>,
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
            "--root" => args.root = Some(PathBuf::from(value("--root")?)),
            "--python" => args.python = Some(PathBuf::from(value("--python")?)),
            "--page" => args.page = Some(value("--page")?),
            "--scroll" => args.scroll = value("--scroll")?.parse().ok(),
            "-h" | "--help" => {
                println!("usage: corvus-gcs [--server http://host:port] [--root DIR] [--python PATH] (the last two run the Python backend) [--page setup|TILE|safety:SENSOR|mission|mission:NAME|settings|logs|analysis] [--scroll PX]");
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
        // does not in time, the backend still must not outlive us.
        std::thread::spawn(|| {
            std::thread::sleep(SIGNAL_DEADLINE);
            net::backend::kill_from_signal();
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
        None if args.root.is_none() && args.python.is_none() => match net::backend::Backend::start_core() {
            Ok(b) => {
                let s = b.server().clone();
                local = Some(b);
                s
            }
            Err(e) => {
                eprintln!("corvus-gcs: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            let start = args.root.clone().unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(PathBuf::from))
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            let started = net::backend::Launch::find(&start, args.python.clone()).and_then(|launch| {
                let port = net::backend::free_port().map_err(|e| format!("no free port: {e}"))?;
                net::backend::Backend::start(&launch, port)
            });
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
    };

    let code = app::run(server, &CONTEXT, args.page.as_deref(), args.scroll);
    // The interface (and with it the telemetry thread) is gone; now the
    // backend: the core is stopped and its threads joined, a Python child
    // gets SIGTERM first.
    drop(local);
    code
}
