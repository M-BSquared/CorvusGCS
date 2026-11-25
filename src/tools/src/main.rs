//! `corvus-build`: the repository's tools. Run through cargo, so a build host
//! needs nothing but the Rust toolchain:
//!
//! ```text
//! cargo run -q --locked -p corvus_tools --bin corvus-build -- <command> ...
//! ```
//!
//! | command | what it does |
//! | --- | --- |
//! | `appimage-libs bundle\|check <AppDir>` | close an AppDir over the system libraries it links (Linux build hosts) |
//! | `icon <in.png> <out.png> <size>` | the logo fitted onto a transparent square |
//! | `png-size <file.png>` | `WIDTHxHEIGHT` of a PNG |
//! | `squashfs <AppImage> <out>` | the squashfs inside an AppImage, cut out |
//! | `elf-static <file>` | exit 1 when an ELF file asks for an interpreter |
//! | `bundle-check <bundle> [--require REL ...]` | fail when a bundle holds Python or misses a file |
//! | `guide [--check]` | rewrite (or check) the frame of every manual page |
//! | `i18n check\|fmt\|new CODE NAME NATIVE [--force]` | the language files |
//! | `soak [options]` | a soak run of the interface with memory samples |
//! | `plugin-pack [--build] [--only ID] [--target DIR] [--out DIR]` | the built plugin libraries of this platform (signed ad hoc on macOS, as the system asks), with their entries |
//! | `plugin-unpack OUT DIR` | install what `plugin-pack` left in OUT into DIR, for `corvus-gcs --check-plugins DIR` |
//! | `plugin-index [--out DIR]` | merge the platforms in DIR into `plugins-index.json` |
//! | `plugin-tag DIR` | print the release the libraries packed in DIR are published under |

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use corvus_tools::{appimage, bundle, elf, guide, i18n, icon, repo_root, soak};

fn usage() -> ExitCode {
    eprintln!(
        "usage: corvus-build appimage-libs bundle|check APPDIR | icon IN OUT SIZE | png-size FILE | \
         squashfs APPIMAGE OUT | elf-static FILE | bundle-check BUNDLE [--require REL ...] | \
         guide [--check] | i18n check|fmt|new CODE NAME NATIVE [--force] | \
         plugin-pack [--build] [--only ID] [--target DIR] [--out DIR] | plugin-unpack OUT DIR | plugin-index [--out DIR] | plugin-tag DIR | soak [--binary B] [--minutes M] \
         [--every S] [--step S] [--mode 1|3d] [--at LNG,LAT] [--home DIR] [--csv FILE] [--max-mb MB]"
    );
    ExitCode::from(2)
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("ERROR: {msg}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["appimage-libs", how @ ("bundle" | "check"), dir] => {
            let appdir = match std::fs::canonicalize(dir) {
                Ok(p) => p,
                Err(e) => return fail(format!("{dir}: {e}")),
            };
            let missing = if *how == "bundle" {
                let index = appimage::ldconfig_index();
                let find = |name: &str| appimage::find_host_library(name, &index, appimage::LIB_DIRS);
                match appimage::bundle(&appdir, &appdir.join("usr/lib"), &find, &appimage::elf_deps, &mut |l| {
                    println!("{l}")
                }) {
                    Ok((copied, missing)) => {
                        println!("    {} system libraries bundled", copied.len());
                        missing
                    }
                    Err(e) => return fail(e),
                }
            } else {
                appimage::unresolved(&appdir, &appimage::elf_deps)
            };
            match appimage::missing_message(&missing) {
                Some(m) => fail(m),
                None => ExitCode::SUCCESS,
            }
        }
        ["icon", input, output, size] => match size.parse() {
            Ok(size) => match icon::fit(Path::new(input), Path::new(output), size) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(e),
            },
            Err(_) => usage(),
        },
        ["png-size", file] => match icon::png_size(Path::new(file)) {
            Ok((w, h)) => {
                println!("{w}x{h}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        ["squashfs", image, out] => {
            let data = match std::fs::read(image) {
                Ok(d) => d,
                Err(e) => return fail(format!("{image}: {e}")),
            };
            match appimage::squashfs_offset(&data) {
                Some(off) => match std::fs::write(out, &data[off..]) {
                    Ok(()) => {
                        println!("OK {off}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => fail(format!("{out}: {e}")),
                },
                None => fail(format!("no valid squashfs superblock in {image}")),
            }
        }
        ["elf-static", file] => match elf::has_interpreter(Path::new(file)) {
            Ok(false) => ExitCode::SUCCESS,
            Ok(true) => fail(format!("{file} is dynamically linked")),
            Err(e) => fail(format!("{file}: {e}")),
        },
        ["bundle-check", dir, rest @ ..] => {
            let require: Vec<&str> = match rest {
                [] => Vec::new(),
                ["--require", files @ ..] => files.to_vec(),
                _ => return usage(),
            };
            let problems = bundle::check(Path::new(dir), &require);
            if problems.is_empty() {
                println!(
                    "    {}: no Python inside",
                    Path::new(dir)
                        .file_name()
                        .map_or(String::new(), |n| n.to_string_lossy().into_owned())
                );
                ExitCode::SUCCESS
            } else {
                eprintln!("ERROR: {dir} is not a Python free bundle:");
                for p in problems {
                    eprintln!("    {p}");
                }
                ExitCode::FAILURE
            }
        }
        ["guide", rest @ ..] => {
            let check = rest.contains(&"--check");
            match guide::build(&guide::guide_dir(), check) {
                Ok(stale) if check && !stale.is_empty() => {
                    println!("out of date, run corvus-build guide: {}", stale.join(", "));
                    ExitCode::FAILURE
                }
                Ok(stale) => {
                    for name in stale {
                        println!("wrote docs/guide/{name}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        ["i18n", "check"] => {
            if i18n::check(&repo_root(), &mut |l| println!("{l}")) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        ["i18n", "fmt"] => match i18n::fmt(&repo_root(), &mut |l| println!("{l}")) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        },
        ["i18n", "new", code, name, native, rest @ ..] => {
            match i18n::new_language(&repo_root(), code, name, native, rest.contains(&"--force")) {
                Ok(msg) => {
                    println!("{msg}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(2)
                }
            }
        }
        ["plugin-pack", rest @ ..] => {
            let (mut target, mut out) = (repo_root().join("target/release"), repo_root().join("dist/plugins"));
            let mut only = None;
            let build = rest.contains(&"--build");
            let rest: Vec<&&str> = rest.iter().filter(|f| **f != "--build").collect();
            let mut it = rest.iter();
            while let Some(flag) = it.next() {
                let Some(value) = it.next() else { return usage() };
                match **flag {
                    "--target" => target = PathBuf::from(value),
                    "--out" => out = PathBuf::from(value),
                    "--only" => only = Some(**value),
                    _ => return usage(),
                }
            }
            if build {
                if let Err(e) = corvus_tools::plugins::build(&repo_root()) {
                    return fail(e);
                }
            }
            match corvus_tools::plugins::pack(&repo_root(), &target, &out, only) {
                Ok(files) => {
                    for f in files {
                        println!("    {f}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        ["plugin-unpack", out, to] => match corvus_tools::plugins::unpack(Path::new(out), Path::new(to)) {
            Ok(n) => {
                println!("    {n} plugins in {to}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        ["plugin-index", rest @ ..] => {
            let mut out = repo_root().join("dist/plugins");
            match rest {
                [] => {}
                ["--out", dir] => out = PathBuf::from(dir),
                _ => return usage(),
            }
            match corvus_tools::plugins::publish_index(&out) {
                Ok(index) => {
                    println!(
                        "    {} plugins listed in {}",
                        index.plugins.len(),
                        corvus_package::INDEX_FILE
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        ["plugin-tag", dir] => match corvus_tools::plugins::merge(Path::new(dir)) {
            Ok(index) => {
                println!("{}", corvus_package::release_tag(&index.abi));
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        ["soak", rest @ ..] => {
            let mut o = soak::Options {
                binary: repo_root().join("target/release/corvus-gcs"),
                minutes: 15.0,
                every: 10.0,
                step: "4".into(),
                mode: "1".into(),
                at: None,
                home: None,
                csv: PathBuf::from("soak.csv"),
                max_mb: 0.0,
            };
            let mut it = rest.iter();
            while let Some(flag) = it.next() {
                let Some(value) = it.next() else { return usage() };
                let num = |v: &str| v.parse::<f64>().ok();
                match *flag {
                    "--binary" => o.binary = PathBuf::from(value),
                    "--minutes" => {
                        o.minutes = match num(value) {
                            Some(v) => v,
                            None => return usage(),
                        }
                    }
                    "--every" => {
                        o.every = match num(value) {
                            Some(v) => v,
                            None => return usage(),
                        }
                    }
                    "--step" => o.step = value.to_string(),
                    "--mode" if ["1", "3d"].contains(value) => o.mode = value.to_string(),
                    "--at" => o.at = Some(value.to_string()),
                    "--home" => o.home = Some(PathBuf::from(value)),
                    "--csv" => o.csv = PathBuf::from(value),
                    "--max-mb" => {
                        o.max_mb = match num(value) {
                            Some(v) => v,
                            None => return usage(),
                        }
                    }
                    _ => return usage(),
                }
            }
            match soak::run(&o) {
                0 => ExitCode::SUCCESS,
                code => ExitCode::from(code as u8),
            }
        }
        _ => usage(),
    }
}
