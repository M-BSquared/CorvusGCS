//! Stands a scene up and takes its picture: a sandbox home, the aircraft on
//! UDP ports of its own, the real program with its window staged
//! (`CORVUS_GUI_SCENE`), and the window's own pixels.
//!
//! The operator's own Corvus is not touched: the sandbox is the program's
//! `HOME`, MAVLink is on 14571 and up, and only the tile cache is shared, so
//! the map shows the imagery that is on disk.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::companion;
use crate::data;
use crate::scene::{self, Scene, FRAME};
use crate::vehicle::Vehicle;

const MAV_PORT: u16 = 14571;
const HTTP_PORT: u16 = 8777;
/// The window is up and listening before the aircraft is.
const HEAD_START: f64 = 5.0;

pub struct Options {
    pub root: PathBuf,
    pub binary: PathBuf,
    /// Unpacked plugins for the sandbox.
    pub plugins: Option<PathBuf>,
    pub out: PathBuf,
    pub isolated_tiles: bool,
    /// Stay up until the window is closed instead of taking the picture.
    pub hold: bool,
    /// The window in points of the screen; the interface is laid out as the frame whatever it is.
    pub window: (u32, u32),
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(path, text))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn sandbox(s: &Scene, o: &Options) -> Result<PathBuf, String> {
    // The folder is on screen (the Analysis page), so it has a plain name.
    let base = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let home = base.join(format!("corvus-scene-{}", s.id()));
    let _ = std::fs::remove_dir_all(&home);
    let dot = home.join(".corvus");
    let own = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus/tiles"));
    let tiles = match own {
        Some(t) if !o.isolated_tiles => t,
        _ => dot.join("tiles"),
    };
    let mut config = json!({
        "mavlink_connection": format!("udpin:127.0.0.1:{MAV_PORT}"),
        "http_port": HTTP_PORT,
        "tile_cache_dir": tiles.to_string_lossy(),
        "theme": {"name": s.text("theme")},
        "map": {"provider": "esri", "base_layer": "esri_world_imagery",
                "terrain_source": if s.text("terrain").is_empty() { "auto" } else { s.text("terrain") }},
        "ui": {"scale": 1.0, "mission_page": true, "plugin_setup": 99, "welcome_done": true},
        "updates": {"check": false},
        "ssh_connections": companion::ACCOUNTS.iter().map(|(name, user)| json!({
            "name": name, "host": "127.0.0.1", "port": companion::PORT, "username": user,
            "password": companion::PASSWORD, "key_path": "",
        })).collect::<Vec<_>>(),
    });
    if !s.text("language").is_empty() {
        config["language"] = json!(s.text("language"));
    }
    if let Some(ui) = s.get("ui").as_object() {
        for (k, v) in ui {
            config["ui"][k] = v.clone();
        }
    }
    let fleet = s.fleet()?;
    if !fleet.is_empty() {
        let mut links = Vec::new();
        let mut names = json!({"main:1": {"name": s.text("vehicle_name")}});
        for (i, (name, color, _)) in fleet.iter().enumerate() {
            let id = format!("link-{}", i + 2);
            links.push(json!({"id": id, "connection": format!("udpin:127.0.0.1:{}", MAV_PORT + 1 + i as u16)}));
            names[format!("{id}:1")] = json!({"name": name, "color": color});
        }
        config["fleet"] = json!({"links": links, "vehicles": names});
    }
    write(&dot.join("config.json"), &config.to_string())?;
    // Unlocked and in full, as the pictures always showed the instruments.
    write(
        &dot.join("hud.json"),
        &json!({"pinned": false, "compact": false, "collapsed": false, "readouts": true}).to_string(),
    )?;
    // What this station learned of the transmitter: the control and its channel.
    let learned = json!({"mode": 2, "bindings": {
        "SA": {"channel": 5, "positions": 3}, "SB": {"channel": 9, "positions": 3},
        "SF": {"channel": 8, "positions": 2}, "SD": {"channel": 7, "positions": 2},
        "SC": {"channel": 10, "positions": 2}, "SH": {"channel": 6, "positions": 2},
        "S1": {"channel": 11}, "S2": {"channel": 12}}});
    write(&dot.join("gui-rc-transmitter.json"), &learned.to_string())?;
    write(&dot.join("missions/Campus hop.json"), &scene::mission().to_string())?;
    if s.get("geofence").is_array() {
        let fence = json!({"polygon": s.get("geofence"), "show_on_map": true});
        write(&dot.join("geofence.json"), &fence.to_string())?;
    }
    for (id, v) in s.get("plugin_settings").as_object().into_iter().flatten() {
        write(&dot.join("plugins").join(id).join("config.json"), &v.to_string())?;
    }
    if s.flag("review_log") {
        let log = data::flight_log();
        let dir = dot.join("flightlogs");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("log_009_2026-09-16_10-40.ulg"), log).map_err(|e| e.to_string())?;
    }
    if let Some(p) = &o.plugins {
        copy(p, &dot.join("plugin-packages")).map_err(|e| format!("plugins: {e}"))?;
    }
    Ok(home)
}

fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// Ends the program whatever happens.
struct Window(Child);

impl Drop for Window {
    fn drop(&mut self) {
        let end = Instant::now() + Duration::from_secs(12);
        while Instant::now() < end {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `s`; returns the picture, or nothing when it was only held.
pub fn run(s: &Scene, o: &Options) -> Result<Option<PathBuf>, String> {
    let home = sandbox(s, o)?;
    let shot_at = HEAD_START + s.number("settle") + 10.0;
    std::fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
    let png = o.out.join(format!("{}.png", s.id()));
    let _ = std::fs::remove_file(&png);
    let stage = home.join("scene.json");
    write(&stage, &s.staging(shot_at)?.to_string())?;
    let _companion = s.flag("ssh").then(companion::start).transpose()?;
    let _model = s.flag("model").then(crate::model::start).transpose()?;
    let mut cmd = Command::new(&o.binary);
    cmd.args(["--page", s.first_page()])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CORVUS_ALLOW_MULTI", "1")
        .env("CORVUS_GUI_SCENE", &stage)
        .env("CORVUS_GUI_SIZE", format!("{},{}", o.window.0, o.window.1))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !o.hold {
        cmd.env("CORVUS_GUI_SHOT", &png)
            .env("CORVUS_GUI_SHOT_AFTER", shot_at.to_string());
    }
    let mut window = Window(cmd.spawn().map_err(|e| format!("{}: {e}", o.binary.display()))?);
    std::thread::sleep(Duration::from_secs_f64(HEAD_START));
    let mut fleet = vec![Vehicle::start(MAV_PORT, s.vehicle()?)?];
    for (i, (_, _, v)) in s.fleet()?.into_iter().enumerate() {
        fleet.push(Vehicle::start(MAV_PORT + 1 + i as u16, v)?);
    }
    let end = Instant::now() + Duration::from_secs_f64(shot_at + 40.0);
    while o.hold || Instant::now() < end {
        if let Ok(Some(_)) = window.0.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    drop(fleet);
    drop(window);
    let _ = std::fs::remove_dir_all(&home);
    if o.hold {
        return Ok(None);
    }
    if png.is_file() {
        Ok(Some(png))
    } else {
        Err(format!("{}: the window took no picture", s.id()))
    }
}

/// The picture at the size and in the kind of file `to` names.
pub fn export(png: &Path, to: &Path, width: u32) -> Result<(), String> {
    let img = image::open(png).map_err(|e| format!("{}: {e}", png.display()))?;
    let height = (f64::from(width) * FRAME.1 / FRAME.0).round() as u32;
    let small = img
        .resize_exact(width, height, image::imageops::FilterType::Lanczos3)
        .to_rgb8();
    let mut bytes = Vec::new();
    if to.extension().is_some_and(|e| e == "png") {
        image::DynamicImage::ImageRgb8(small)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 86)
            .encode_image(&small)
            .map_err(|e| e.to_string())?;
    }
    std::fs::write(to, bytes).map_err(|e| format!("{}: {e}", to.display()))
}

/// Puts the picture of `s` where the README and the website have it.
pub fn publish(s: &Scene, png: &Path, root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut put = |to: PathBuf, width| -> Result<(), String> {
        export(png, &to, width)?;
        out.push(to);
        Ok(())
    };
    let images = root.join("docs/assets/images");
    if !s.text("web").is_empty() {
        put(images.join(format!("{}.jpg", s.text("web"))), FRAME.0 as u32)?;
        put(images.join(format!("{}-800.jpg", s.text("web"))), 800)?;
    }
    if !s.text("readme").is_empty() {
        put(root.join("assets").join(s.text("readme")), FRAME.0 as u32)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn a_picture_is_written_at_the_sizes_the_website_has() {
        let dir = std::env::temp_dir().join(format!("corvus-scene-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        image::RgbaImage::from_pixel(2560, 1600, image::Rgba([10, 20, 30, 255]))
            .save(&png)
            .unwrap();
        for (name, w) in [("b.jpg", 1600), ("c.png", 800)] {
            export(&png, &dir.join(name), w).unwrap();
            let got = image::open(dir.join(name)).unwrap();
            assert_eq!((got.width(), got.height()), (w, w * 10 / 16));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_home_holds_what_the_scene_says() {
        let s = scene::all().into_iter().find(|s| s.id() == "fleet").unwrap();
        let o = Options {
            root: PathBuf::new(),
            binary: PathBuf::new(),
            plugins: None,
            out: PathBuf::new(),
            isolated_tiles: true,
            hold: false,
            window: (1280, 800),
        };
        let home = sandbox(&s, &o).unwrap();
        let c: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".corvus/config.json")).unwrap()).unwrap();
        assert_eq!(c["fleet"]["links"].as_array().unwrap().len(), 2);
        assert_eq!(c["fleet"]["vehicles"]["link-2:1"]["name"], "Zippy");
        assert!(home.join(".corvus/hud.json").is_file());
        let _ = std::fs::remove_dir_all(&home);
    }
}
