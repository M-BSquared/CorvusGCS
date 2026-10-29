//! Corvus GCS, the native desktop interface.
//!
//!     corvus-gcs                         start the backend here and show it
//!     corvus-gcs --server http://h:8080  connect to a backend elsewhere
//!     corvus-gcs --root DIR --python PY  where serve.py is, what runs it
//!
//! Locally the backend (`serve.py`) is a child of this process and is taken
//! down on every way out; see `net::backend`.

mod app;
mod data;
mod home;
mod map;
mod map3d;
mod mission;
mod net;
mod notify;
mod panel;
mod setup;
mod term;
mod ui;

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
                println!("usage: corvus-gcs [--server http://host:port] [--root DIR] [--python PATH] [--page setup|parameters|safety|safety:SENSOR|motors] [--scroll PX]");
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
            let start = args.root.clone().unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(PathBuf::from))
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            // A bundle carries its backend: frozen beside the executable on
            // Windows. A checkout, or flags that say where it is, use serve.py.
            let bundled = (args.root.is_none() && args.python.is_none())
                .then(|| net::backend::Launch::bundled(&start))
                .flatten();
            let found = match bundled {
                Some(launch) => Ok(launch),
                None => net::backend::Launch::find(&start, args.python.clone()),
            };
            let started = found.and_then(|launch| {
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
    // backend, SIGTERM first.
    drop(local);
    code
}
