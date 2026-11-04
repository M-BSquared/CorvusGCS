//! The credits dialog behind Settings, About (`python/src/js/credits.js`). The list
//! of what ships is the native interface's own: its crates in place of the
//! web interface's vendored scripts, the same typefaces and icons, the same
//! backend, services and references.

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::Value;

use crate::ui::card::{self, Look};
use crate::ui::form::Form;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

pub struct Entry {
    pub name: &'static str,
    pub version: &'static str,
    pub license: &'static str,
    pub copyright: &'static str,
    pub role: &'static str,
    pub url: &'static str,
}

const fn e(name: &'static str, license: &'static str, role: &'static str, url: &'static str) -> Entry {
    Entry {
        name,
        version: "",
        license,
        copyright: "",
        role,
        url,
    }
}

pub const AUTHOR: &str = "Maximilian B\u{f6}ck";
pub const ORG: &str = "with Universit\u{e4}t der Bundeswehr M\u{fc}nchen";
pub const CHAIR: &str = "In the context of the work at Chair LRT 1.1 of Prof. Dr. Matthias Gerdts.";
pub const NOTE: &str = "Corvus GCS is designed, built and maintained by Maximilian B\u{f6}ck. Built for real field use with PX4 and ArduPilot aircraft.";
pub const LICENSE: &str = "Sustainable Use License 1.0. See LICENSE.md in the application folder: internal business, non-commercial and personal use; not for resale.";
pub const FOOTER: &str = "Every typeface and icon above is cut out of the files under python/src/vendor/ when the interface is built. The interface requests nothing from the internet. Map tiles are fetched by the backend and cached for offline use.";

/// The crates the native interface is built from.
pub const LIBRARIES: [Entry; 9] = [
    e(
        "egui and eframe",
        "MIT or Apache 2.0",
        "The native interface: windows, input and painting.",
        "https://github.com/emilk/egui",
    ),
    e(
        "egui_extras",
        "MIT or Apache 2.0",
        "Draws the SVG icons.",
        "https://github.com/emilk/egui",
    ),
    e(
        "walkers",
        "MIT",
        "The flat map, on the backend's tile proxy.",
        "https://github.com/podusowski/walkers",
    ),
    e(
        "image",
        "MIT or Apache 2.0",
        "Reads the logos and the map's PNG and WebP tiles.",
        "https://github.com/image-rs/image",
    ),
    e(
        "flate2",
        "MIT or Apache 2.0",
        "Unpacks the building cells the backend stores gzipped.",
        "https://github.com/rust-lang/flate2-rs",
    ),
    e(
        "serde_json",
        "MIT or Apache 2.0",
        "Reads and writes the backend's JSON.",
        "https://github.com/serde-rs/json",
    ),
    e(
        "vte",
        "MIT or Apache 2.0",
        "The terminal emulator behind the SSH tab.",
        "https://github.com/alacritty/vte",
    ),
    e(
        "ctrlc",
        "MIT or Apache 2.0",
        "Ends the app cleanly on SIGINT and SIGTERM.",
        "https://github.com/Detegr/rust-ctrlc",
    ),
    e(
        "Lucide",
        "ISC",
        "The icon set used throughout the interface.",
        "https://lucide.dev/",
    ),
];

pub const FONTS: [Entry; 2] = [
    Entry {
        copyright: "Copyright The Inter Project Authors (Rasmus Andersson).",
        ..e(
            "Inter",
            "SIL Open Font License 1.1",
            "Interface typeface.",
            "https://rsms.me/inter/",
        )
    },
    Entry {
        copyright: "Copyright The JetBrains Mono Project Authors (JetBrains s.r.o.).",
        ..e(
            "JetBrains Mono",
            "SIL Open Font License 1.1",
            "Monospace typeface for telemetry values, console, coordinates.",
            "https://www.jetbrains.com/lp/mono/",
        )
    },
];

pub const RUNTIME: [Entry; 16] = [
    e("Python", "PSF License", "The backend runs on it; the desktop build bundles its own CPython, including the libraries its standard library is built with, such as OpenSSL, SQLite, XZ Utils and zlib.", "https://www.python.org/"),
    e("MAVLink message definitions", "MIT", "The common and dialect message sets, compiled into Corvus' MAVLink module.", "https://mavlink.io/"),
    e("Rust standard library", "MIT / Apache 2.0", "The runtime of corvus_link, the MAVLink module written in Rust.", "https://www.rust-lang.org/"),
    e("PyO3", "MIT / Apache 2.0", "Connects corvus_link to the Python backend.", "https://pyo3.rs/"),
    e("RustCrypto sha2", "MIT / Apache 2.0", "SHA-256 for MAVLink 2 message signing.", "https://github.com/RustCrypto/hashes"),
    e("socket2, libc, windows-sys, indexmap", "MIT / Apache 2.0", "Sockets and system calls under the links, and the ordered parameter cache.", "https://github.com/rust-lang/socket2"),
    e("serialport-rs", "MPL 2.0", "Serial ports on Windows. Unmodified; source at the link.", "https://github.com/serialport/serialport-rs"),
    e("paramiko", "LGPL 2.1", "SSH sessions to the companion computer.", "https://www.paramiko.org/"),
    e("cryptography", "Apache 2.0 or BSD 3-Clause", "Ciphers and key handling under paramiko. Ships its own OpenSSL (Apache 2.0).", "https://cryptography.io/"),
    e("bcrypt", "Apache 2.0", "Key derivation for encrypted SSH keys, used by paramiko.", "https://github.com/pyca/bcrypt"),
    e("PyNaCl", "Apache 2.0", "Ed25519 keys for SSH, used by paramiko. Includes libsodium (ISC).", "https://github.com/pyca/pynacl"),
    e("invoke", "BSD 2-Clause", "Command execution helper required by paramiko.", "https://www.pyinvoke.org/"),
    e("cffi / pycparser", "MIT or MIT-0 / BSD 3-Clause", "Native bindings under cryptography, bcrypt and PyNaCl.", "https://cffi.readthedocs.io/"),
    e("pyserial", "BSD 3-Clause", "Serial-port enumeration and the radio link.", "https://github.com/pyserial/pyserial"),
    e("PyInstaller", "GPL v2 with bootloader exception", "Packs the Windows build. Its bootloader is part of that build only.", "https://pyinstaller.org/"),
    e("AppImage runtime", "MIT", "The small starter at the front of the Linux AppImage that mounts and runs it.", "https://appimage.org/"),
];

pub const SERVICES: [Entry; 6] = [
    e("OpenStreetMap", "ODbL 1.0", "Building footprints and heights for the 3D map. \u{a9} OpenStreetMap contributors.", "https://www.openstreetmap.org/copyright"),
    e("Overpass API", "Service of the OpenStreetMap community", "Serves the OpenStreetMap building data the backend requests and caches.", "https://overpass-api.de/"),
    e("Nominatim", "ODbL 1.0 (OpenStreetMap data)", "Place search on the map. Search text is sent to the public Nominatim service, so it needs a connection. \u{a9} OpenStreetMap contributors.", "https://nominatim.org/"),
    e("PX4 Autopilot releases", "BSD 3-Clause", "Firmware images listed and downloaded from GitHub releases when you flash PX4. Nothing is bundled.", "https://px4.io/"),
    e("ArduPilot firmware server", "GPL v3", "Firmware images and board lists fetched when you flash ArduPilot. Nothing is bundled.", "https://firmware.ardupilot.org/"),
    e("GitHub", "Public API, no account", "The update check reads the list of published Corvus GCS releases. Nothing is downloaded and nothing about the computer is sent. It can be switched off in Settings.", "https://github.com/M-BSquared/CorvusGCS/releases"),
];

pub const REFERENCES: [Entry; 5] = [
    e("PX4 Bootloader and px_uploader.py", "BSD 3-Clause", "The USB bootloader protocol and its CRC, which the firmware uploader speaks. One board exception comes from ArduPilot's uploader.py.", "https://github.com/PX4/PX4-Bootloader"),
    e("PX4 GPS drivers", "BSD 3-Clause", "The u-blox configuration keys and survey-in steps behind the RTK base station setup.", "https://github.com/PX4/PX4-GPSDrivers"),
    e("PX4 Flight Review", "BSD 3-Clause", "The model for the Flight Review page, which is a smaller implementation of its own.", "https://github.com/PX4/flight_review"),
    e("pyulog", "BSD 3-Clause", "The reference the built-in ULog reader was checked against.", "https://github.com/PX4/pyulog"),
    e("QGroundControl", "Apache 2.0 or GPL v3", "The .params file format, the MAVLink shell framing and the second station defaults that Corvus stays compatible with.", "https://qgroundcontrol.com/"),
];

/// The distinct credit lines of each map service, in the order the registry
/// lists its layers (`mapDataGroups`).
pub fn map_credits(data: &Value) -> Vec<(String, String, String)> {
    let sources = data
        .get("sources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let providers = data
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for p in &providers {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("");
        let mut seen: Vec<String> = Vec::new();
        for s in sources
            .iter()
            .filter(|s| s.get("provider").and_then(Value::as_str) == Some(id))
        {
            if let Some(a) = s.get("attribution").and_then(Value::as_str).filter(|a| !a.is_empty()) {
                if !seen.iter().any(|x| x == a) {
                    seen.push(a.to_string());
                }
            }
        }
        if seen.is_empty() {
            continue;
        }
        out.push((
            p.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
            if seen.len() == 1 {
                String::new()
            } else {
                format!("{} sources", seen.len())
            },
            seen.join(" \u{b7} "),
        ));
    }
    out
}

/// The elevation models that carry a credit.
pub fn terrain_credits(data: &Value) -> Vec<(String, String)> {
    data.get("terrain")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    let credit = t.get("attribution").and_then(Value::as_str).filter(|c| !c.is_empty())?;
                    Some((
                        t.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
                        credit.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn entry(ui: &egui::Ui, f: &mut Form, name: &str, license: &str, lines: &[&str]) {
    let n = style::galley(ui, name, 12.5, Weight::W600, style::text_1(), 0.0);
    let l = style::galley(ui, license, 11.0, Weight::W400, style::text_3(), 0.0);
    let inner_w = f.w - 22.0;
    let mut texts = Vec::new();
    for t in lines.iter().filter(|t| !t.is_empty()) {
        texts.push(kit::wrapped_weight(
            ui,
            t,
            11.0,
            Weight::W400,
            style::text_2(),
            inner_w,
            1.45,
        ));
    }
    let head_h = n.size().y.max(l.size().y);
    let h = 18.0 + head_h + texts.iter().map(|g| 2.0 + g.size().y).sum::<f32>();
    let r = f.rect(h);
    card::paint(ui.painter(), r, Look::Inset);
    let mut y = r.top() + 9.0;
    ui.painter().galley(Pos2::new(r.left() + 11.0, y), n, style::text_1());
    ui.painter()
        .galley(Pos2::new(r.right() - 11.0 - l.size().x, y + 1.0), l, style::text_3());
    y += head_h;
    for g in texts {
        y += 2.0;
        let gh = g.size().y;
        ui.painter().galley(Pos2::new(r.left() + 11.0, y), g, style::text_2());
        y += gh;
    }
    f.y = r.bottom() + 8.0;
}

fn group(ui: &egui::Ui, f: &mut Form, title: &str, entries: &[Entry]) {
    caption(ui, f, title);
    for e in entries {
        let name = if e.version.is_empty() {
            e.name.to_string()
        } else {
            format!("{} {}", e.name, e.version)
        };
        entry(ui, f, &name, e.license, &[e.copyright, e.role, e.url]);
    }
}

fn caption(ui: &egui::Ui, f: &mut Form, text: &str) {
    let g = style::galley(ui, &text.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let h = g.size().y;
    ui.painter().galley(f.rect(h).min, g, style::text_3());
    f.y += h + 8.0;
}

/// Draws the dialog's content from `y`; returns the bottom.
pub fn draw(ui: &egui::Ui, x: f32, y: f32, w: f32, version: &str, tiles: Option<&Value>) -> f32 {
    let mut f = Form::new(x, y, w);
    let product = if version.is_empty() {
        "Corvus GCS".to_string()
    } else {
        format!("Corvus GCS {version}")
    };
    let lines: [(String, f32, Weight, egui::Color32); 4] = [
        (product, 16.0, Weight::W700, style::text_1()),
        (format!("by {AUTHOR}"), 13.0, Weight::W600, style::accent_bright()),
        (ORG.to_string(), 11.5, Weight::W400, style::text_3()),
        (CHAIR.to_string(), 11.5, Weight::W400, style::text_3()),
    ];
    for (t, size, weight, color) in lines {
        let g = style::galley(ui, &t, size, weight, color, 0.0);
        let h = g.size().y;
        ui.painter().galley(f.rect(h).min, g, color);
        f.y += h + 2.0;
    }
    f.y += 6.0;
    for (t, color) in [(NOTE, style::text_2()), (LICENSE, style::text_3())] {
        let g = kit::wrapped_weight(ui, t, 11.5, Weight::W400, color, w, 1.5);
        let h = g.size().y;
        ui.painter().galley(f.rect(h).min, g, color);
        f.y += h + 8.0;
    }
    f.y += 8.0;
    group(ui, &mut f, "Libraries", &LIBRARIES);
    f.y += 8.0;
    group(ui, &mut f, "Typefaces", &FONTS);
    f.y += 8.0;
    group(ui, &mut f, "Backend & runtime", &RUNTIME);
    f.y += 8.0;
    group(ui, &mut f, "Data & services", &SERVICES);
    f.y += 8.0;
    caption(ui, &mut f, "Map data");
    match tiles.map(map_credits).filter(|c| !c.is_empty()) {
        Some(list) => {
            for (name, lic, role) in list {
                entry(ui, &mut f, &name, &lic, &[&role]);
            }
        }
        None => note(
            ui,
            &mut f,
            if tiles.is_some() {
                "Map attributions unavailable. The backend did not respond."
            } else {
                "Loading map attributions\u{2026}"
            },
        ),
    }
    f.y += 8.0;
    caption(ui, &mut f, "Elevation data");
    match tiles.map(terrain_credits).filter(|c| !c.is_empty()) {
        Some(list) => {
            for (name, role) in list {
                entry(ui, &mut f, &name, "", &[&role]);
            }
        }
        None => note(
            ui,
            &mut f,
            if tiles.is_some() {
                "Elevation attributions unavailable. The backend did not respond."
            } else {
                "Loading elevation attributions\u{2026}"
            },
        ),
    }
    f.y += 8.0;
    group(ui, &mut f, "Reference implementations", &REFERENCES);
    let g = kit::wrapped_weight(ui, FOOTER, 11.0, Weight::W400, style::text_3(), w, 1.5);
    let h = g.size().y;
    ui.painter().galley(f.rect(h).min, g, style::text_3());
    f.y + h
}

fn note(ui: &egui::Ui, f: &mut Form, text: &str) {
    let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, style::text_3(), f.w, 1.5);
    let h = g.size().y;
    ui.painter().galley(f.rect(h).min, g, style::text_3());
    f.y += h + 8.0;
}

/// A scrolling box under the dialog's title, `height` tall.
pub fn scroll(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    height: f32,
    content: impl FnOnce(&mut egui::Ui, f32, f32) -> f32,
) -> f32 {
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, height));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.set_clip_rect(rect);
    egui::ScrollArea::vertical()
        .id_salt("creditsscroll")
        .max_height(height)
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            let top = ui.max_rect().top();
            let bottom = content(ui, ui.max_rect().left(), top);
            ui.allocate_space(Vec2::new(w - 12.0, bottom - top));
        });
    rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_map_credits_list_each_line_of_a_service_once() {
        let data = json!({
            "providers": [{"id": "esri", "label": "Esri"}, {"id": "osm", "label": "OpenStreetMap"}, {"id": "none", "label": "Bare"}],
            "sources": [
                {"provider": "esri", "attribution": "Esri, Maxar"},
                {"provider": "esri", "attribution": "Esri, Maxar"},
                {"provider": "osm", "attribution": "OSM contributors"},
                {"provider": "osm", "attribution": "OSM tiles"}],
            "terrain": [{"label": "Copernicus", "attribution": "ESA"}, {"label": "Silent"}],
        });
        assert_eq!(
            map_credits(&data),
            [
                ("Esri".to_string(), String::new(), "Esri, Maxar".to_string()),
                (
                    "OpenStreetMap".to_string(),
                    "2 sources".to_string(),
                    "OSM contributors \u{b7} OSM tiles".to_string()
                )
            ]
        );
        assert_eq!(terrain_credits(&data), [("Copernicus".to_string(), "ESA".to_string())]);
    }

    #[test]
    fn the_attribution_follows_the_house_wording() {
        assert!(ORG.starts_with("with "));
        assert!(!ORG.contains(" at "));
        for e in LIBRARIES
            .iter()
            .chain(&FONTS)
            .chain(&RUNTIME)
            .chain(&SERVICES)
            .chain(&REFERENCES)
        {
            assert!(
                !e.role.contains('\u{2014}') && !e.name.contains('\u{2014}'),
                "{}",
                e.name
            );
        }
    }
}
