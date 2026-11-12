//! The built macOS bundle, run. Passes without a word unless a bundle is
//! there: `./build-macos-app.sh` puts one in `dist/`, and `CORVUS_APP_BUNDLE`
//! names another. A bundle left in `dist/` by an older build says nothing
//! about this tree, so only one of the current `VERSION` counts.
//!
//! It holds no Python of any kind, carries the version and the licence, links
//! only what macOS ships, starts and prints its usage, and its signature
//! still verifies afterwards (a run that wrote into the bundle would break the
//! seal an operator's Mac checks).

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;

use corvus_tools::{bundle, repo_root};

fn bundle_here() -> Option<PathBuf> {
    let want = std::fs::read_to_string(repo_root().join("VERSION"))
        .ok()?
        .trim()
        .to_string();
    if let Some(named) = std::env::var_os("CORVUS_APP_BUNDLE") {
        return Some(PathBuf::from(named));
    }
    let app = repo_root().join("dist").join("Corvus GCS.app");
    let have = std::fs::read_to_string(app.join("Contents/Resources/VERSION")).ok()?;
    if have.trim() != want {
        eprintln!(
            "skipped: {} is from {}, this tree is {want}",
            app.display(),
            have.trim()
        );
        return None;
    }
    Some(app)
}

#[test]
fn the_built_bundle_is_whole_native_and_sealed() {
    let Some(app) = bundle_here() else {
        eprintln!("skipped: no bundle of this version; build one with ./build-macos-app.sh");
        return;
    };
    assert!(app.is_dir(), "{}", app.display());
    let res = app.join("Contents/Resources");
    let exe = app.join("Contents/MacOS/corvus-gcs");

    let problems = bundle::check(
        &app,
        &[
            "Contents/MacOS/corvus-gcs",
            "Contents/Resources/VERSION",
            "Contents/Resources/LICENSE.md",
        ],
    );
    assert!(problems.is_empty(), "{problems:#?}");

    let version = std::fs::read_to_string(res.join("VERSION")).unwrap().trim().to_string();
    if std::env::var_os("CORVUS_APP_BUNDLE").is_none() {
        assert_eq!(
            version,
            std::fs::read_to_string(repo_root().join("VERSION")).unwrap().trim()
        );
    }
    assert!(res.join("assets/licenses").is_dir());
    assert!(std::fs::read_to_string(app.join("Contents/Info.plist"))
        .unwrap()
        .contains(&version));

    let otool = Command::new("otool").arg("-L").arg(&exe).output().expect("otool");
    let listing = String::from_utf8_lossy(&otool.stdout);
    let foreign: Vec<&str> = listing
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .filter(|p| !(p.starts_with("/System/") || p.starts_with("/usr/lib/") || p.starts_with('@')))
        .collect();
    assert!(foreign.is_empty(), "links outside macOS: {foreign:?}");

    let home = std::env::temp_dir().join(format!("corvus-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let help = Command::new(&exe)
        .arg("--help")
        .env("HOME", &home)
        .output()
        .expect("the program runs");
    let _ = std::fs::remove_dir_all(&home);
    assert!(help.status.success(), "{}", String::from_utf8_lossy(&help.stderr));
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("usage: corvus-gcs"));

    let verify = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app)
        .output()
        .expect("codesign");
    assert!(verify.status.success(), "{}", String::from_utf8_lossy(&verify.stderr));
}
