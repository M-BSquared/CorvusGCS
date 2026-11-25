//! `corvus-scene`: the pictures of the README and the website, made again the
//! same way each time (`src/scene/README.md`).
//!
//! ```text
//! corvus-scene list
//! corvus-scene run ID [--set key=value ...]           stand it up and hold it
//! corvus-scene shoot [ID ...] [--set ...] [--out DIR] [--publish]
//! corvus-scene publish [ID ...] [--out DIR]           pictures taken before
//! ```

mod companion;
mod data;
mod flight;
mod model;
mod runner;
mod scene;
mod vehicle;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use scene::Scene;

fn usage() -> String {
    "usage: corvus-scene list | run ID | shoot [ID ...] | publish [ID ...]\n\
     [--set key=value] [--out DIR] [--publish] [--binary FILE] [--window WxH] [--isolated-tiles]"
        .into()
}

fn root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&here).unwrap_or(here)
}

fn cargo(root: &Path, args: &[&str]) -> Result<(), String> {
    let ok = Command::new("cargo").args(args).current_dir(root).status();
    match ok {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("cargo {}: {s}", args.join(" "))),
        Err(e) => Err(format!("cargo: {e}")),
    }
}

/// Builds the program and its plugins, and unpacks the plugins for the sandboxes.
fn prepare(root: &Path) -> Result<(PathBuf, PathBuf), String> {
    cargo(root, &["build", "--locked", "--bin", "corvus-gcs", "--lib"])?;
    let (packed, plugins) = (root.join("build/scene-packed"), root.join("build/scene-plugins"));
    for d in [&packed, &plugins] {
        let _ = std::fs::remove_dir_all(d);
    }
    let (from, to) = (packed.to_string_lossy(), plugins.to_string_lossy());
    let tool = [
        "run",
        "-q",
        "--locked",
        "-p",
        "corvus_tools",
        "--bin",
        "corvus-build",
        "--",
    ];
    cargo(
        root,
        &[&tool[..], &["plugin-pack", "--target", "target/debug", "--out", &from]].concat(),
    )?;
    cargo(root, &[&tool[..], &["plugin-unpack", &from, &to]].concat())?;
    let exe = format!("corvus-gcs{}", std::env::consts::EXE_SUFFIX);
    Ok((root.join("target/debug").join(exe), plugins))
}

fn main() -> ExitCode {
    match go() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn go() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let root = root();
    let (mut ids, mut sets) = (Vec::new(), Vec::new());
    let (mut out, mut binary) = (root.join("build/scenes"), None);
    let (mut publish, mut isolated, mut window) = (false, false, (1280, 800));
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{a} wants a value"));
        match a.as_str() {
            "--set" => sets.push(value()?),
            "--out" => out = PathBuf::from(value()?),
            "--binary" => binary = Some(PathBuf::from(value()?)),
            "--publish" => publish = true,
            "--isolated-tiles" => isolated = true,
            "--window" => {
                let v = value()?;
                let two = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                window = two.ok_or("--window wants WxH")?;
            }
            id if !id.starts_with("--") => ids.push(id.to_string()),
            _ => return Err(usage()),
        }
    }
    let mut scenes = scene::all();
    if let Some(bad) = ids.iter().find(|id| !scenes.iter().any(|s| s.id() == **id)) {
        return Err(format!("there is no scene called {bad}; corvus-scene list names them"));
    }
    if command == "list" {
        for s in &scenes {
            let note = if s.text("skip").is_empty() {
                ""
            } else {
                "  (not made: see skip)"
            };
            println!("{:26} {:24} {}{note}", s.id(), s.text("web"), s.text("title"));
        }
        return Ok(());
    }
    scenes.retain(|s| {
        if ids.is_empty() {
            s.text("skip").is_empty()
        } else {
            ids.iter().any(|i| i == s.id())
        }
    });
    for s in &mut scenes {
        for pair in &sets {
            s.set(pair)?;
        }
    }
    match command.as_str() {
        "publish" => {
            for s in &scenes {
                say_published(s, &out.join(format!("{}.png", s.id())), &root)?;
            }
            Ok(())
        }
        "run" | "shoot" => {
            let hold = command == "run";
            if hold && scenes.len() != 1 {
                return Err("run wants one scene".into());
            }
            let (built, plugins) = prepare(&root)?;
            let o = runner::Options {
                binary: binary.unwrap_or(built),
                plugins: Some(plugins),
                out,
                isolated_tiles: isolated,
                hold,
                window,
                root,
            };
            let mut failed = Vec::new();
            for s in &scenes {
                println!("{:26} {}", s.id(), s.text("title"));
                match runner::run(s, &o) {
                    Ok(Some(png)) if publish => say_published(s, &png, &o.root)?,
                    Ok(Some(png)) => println!("{:26} {}", "", png.display()),
                    Ok(None) => {}
                    Err(e) => failed.push(e),
                }
            }
            if failed.is_empty() {
                Ok(())
            } else {
                Err(failed.join("\n"))
            }
        }
        _ => Err(usage()),
    }
}

fn say_published(s: &Scene, png: &Path, root: &Path) -> Result<(), String> {
    for p in runner::publish(s, png, root)? {
        println!("{:26} {}", "", p.display());
    }
    Ok(())
}
