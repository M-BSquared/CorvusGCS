//! The backend without the interface: `corvus_core` and its HTTP server in a
//! process of their own. The Python tests start it to hold the routes to the
//! Python server (`python/tools/route_harness.py`), and it is what a second
//! station or an AI client talks to when no window is open.
//!
//!     corvus-core [--port N] [--state-dir DIR] [--web-root DIR]
//!
//! Prints `listening http://127.0.0.1:PORT` once the server is up, and stops
//! (joining every thread) on SIGINT and SIGTERM.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;

use corvus_core::{CoreBuilder, Http, HttpInfo, HttpOptions, Options, PeripheralsModule, VehicleModule};

fn main() -> ExitCode {
    let mut port = 8000u16;
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let parsed = match a.as_str() {
            "--port" => value("--port").and_then(|v| v.parse().map(|p| port = p).map_err(|_| format!("bad port {v}"))),
            "--state-dir" => value("--state-dir").map(|v| options = Options::in_dir(PathBuf::from(v))),
            "--web-root" => value("--web-root").map(|v| options.web_root = Some(PathBuf::from(v))),
            "-h" | "--help" => {
                println!("usage: corvus-core [--port N] [--state-dir DIR] [--web-root DIR]");
                return ExitCode::SUCCESS;
            }
            other => Err(format!("unknown argument {other}")),
        };
        if let Err(e) = parsed {
            eprintln!("corvus-core: {e}");
            return ExitCode::from(2);
        }
    }
    let core = match CoreBuilder::new(options)
        .module(VehicleModule::new())
        .module(PeripheralsModule::new())
        .module(Http::new(HttpOptions {
            port,
            ..HttpOptions::default()
        }))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("corvus-core: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = core.claim_instance() {
        eprintln!("corvus-core: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = core.start() {
        eprintln!("corvus-core: {e}");
        return ExitCode::FAILURE;
    }
    let Some(url) = core.service::<HttpInfo>().and_then(|i| i.url()) else {
        eprintln!("corvus-core: no server address");
        return ExitCode::FAILURE;
    };
    println!("listening {url}");
    let (tx, rx) = mpsc::channel();
    if ctrlc::set_handler(move || {
        let _ = tx.send(());
    })
    .is_err()
    {
        eprintln!("corvus-core: no signal handler; stop it with kill");
    }
    let _ = rx.recv();
    core.stop();
    ExitCode::SUCCESS
}
