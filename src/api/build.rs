//! Turns the interface's fonts and icons into what egui reads, so the app
//! looks like the web interface it replaces. Both live in `assets/`, copied
//! from the web interface's vendor folder so the Rust program does not build
//! from the Python program's files.
//!
//! * `assets/fonts/*.woff2` (Inter, JetBrains Mono; variable weight) are
//!   decoded to TrueType, which egui reads, into `$OUT_DIR`.
//! * The Lucide icons the interface draws are cut out of
//!   `assets/lucide.min.js` and written as SVG documents into
//!   `$OUT_DIR/icons.rs`, stroked as Lucide draws them (24 px box, width 2,
//!   round caps and joins).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const FONTS: &[&str] = &[
    "Inter-latin",
    "Inter-latin-ext",
    "JetBrainsMono-latin",
    "JetBrainsMono-latin-ext",
];

/// (the Lucide name the web interface uses, the export name in lucide.min.js)
const ICONS: &[(&str, &str)] = &[
    ("power", "Power"),
    ("drone", "Drone"),
    ("arrow-up", "ArrowUp"),
    ("arrow-down", "ArrowDown"),
    ("house", "House"),
    ("route", "Route"),
    ("x", "X"),
    ("chevron-down", "ChevronDown"),
    ("chevron-right", "ChevronRight"),
    ("chevron-left", "ChevronLeft"),
    ("sliders-horizontal", "SlidersHorizontal"),
    ("chart-column", "ChartColumn"),
    ("file-text", "FileText"),
    ("settings", "Settings"),
    ("plug", "Plug"),
    ("list-checks", "ListChecks"),
    ("heading", "Heading"),
    ("clipboard-check", "ClipboardCheck"),
    ("chevron-up", "ChevronUp"),
    ("circle-check", "CircleCheck"),
    ("maximize-2", "Maximize2"),
    ("pen-line", "PenLine"),
    ("circle-x", "CircleX"),
    ("cable", "Cable"),
    ("network", "Network"),
    ("refresh-cw", "RefreshCw"),
    ("share-2", "Share2"),
    ("search", "Search"),
    ("map-pin", "MapPin"),
    ("locate-fixed", "LocateFixed"),
    ("pause", "Pause"),
    ("play", "Play"),
    ("copy", "Copy"),
    ("save", "Save"),
    ("download", "Download"),
    ("rotate-ccw", "RotateCcw"),
    ("send", "Send"),
    ("plus", "Plus"),
    ("minus", "Minus"),
    ("server", "Server"),
    ("pencil", "Pencil"),
    ("puzzle", "Puzzle"),
    ("crosshair", "Crosshair"),
    ("layers", "Layers"),
    ("frame", "Frame"),
    ("box", "Box"),
    ("ruler", "Ruler"),
    ("info", "Info"),
    ("octagon-x", "OctagonX"),
    ("radar", "Radar"),
    ("check-check", "CheckCheck"),
    ("scan", "Scan"),
    ("scan-line", "ScanLine"),
    ("camera", "Camera"),
    ("check", "Check"),
    ("upload", "Upload"),
    ("wand-sparkles", "WandSparkles"),
    ("radio", "Radio"),
    ("activity", "Activity"),
    ("fan", "Fan"),
    ("shield", "Shield"),
    ("battery-charging", "BatteryCharging"),
    ("radio-tower", "RadioTower"),
    ("satellite-dish", "SatelliteDish"),
    ("id-card", "IdCard"),
    ("list", "List"),
    ("cpu", "Cpu"),
    ("video", "Video"),
    ("message-square", "MessageSquare"),
    ("octagon-alert", "OctagonAlert"),
    ("triangle-alert", "TriangleAlert"),
    ("trash-2", "Trash2"),
    ("square-terminal", "SquareTerminal"),
    ("minimize-2", "Minimize2"),
    ("square-arrow-out-up-right", "SquareArrowOutUpRight"),
    ("hourglass", "Hourglass"),
    ("rotate-3d", "Rotate3d"),
    ("hand", "Hand"),
    ("plug-zap", "PlugZap"),
    ("unplug", "Unplug"),
    ("wind", "Wind"),
    ("ban", "Ban"),
    ("move-3d", "Move3d"),
    ("compass", "Compass"),
    ("separator-horizontal", "SeparatorHorizontal"),
    ("gauge", "Gauge"),
    ("vibrate", "Vibrate"),
    ("zap", "Zap"),
    ("trees", "Trees"),
    ("magnet", "Magnet"),
    ("rotate-cw", "RotateCw"),
    ("eraser", "Eraser"),
    ("laptop", "Laptop"),
    ("grip-vertical", "GripVertical"),
    ("cloud-upload", "CloudUpload"),
    ("cloud-download", "CloudDownload"),
    ("folder-open", "FolderOpen"),
    ("circle-alert", "CircleAlert"),
    ("circle", "Circle"),
    ("database", "Database"),
    ("eye", "Eye"),
    ("eye-off", "EyeOff"),
    ("file-plus", "FilePlus"),
    ("gamepad-2", "Gamepad2"),
    ("image-plus", "ImagePlus"),
    ("key-round", "KeyRound"),
    ("map", "Map"),
    ("mountain", "Mountain"),
    ("palette", "Palette"),
    ("panels-top-left", "PanelsTopLeft"),
    ("arrow-up-from-line", "ArrowUpFromLine"),
    ("move-vertical", "MoveVertical"),
    ("plane", "Plane"),
    ("battery-medium", "BatteryMedium"),
    ("satellite", "Satellite"),
    ("folder", "Folder"),
    ("hard-drive-download", "HardDriveDownload"),
    ("file-clock", "FileClock"),
    ("arrow-right", "ArrowRight"),
    ("chart-spline", "ChartSpline"),
    ("spline", "Spline"),
    ("plane-takeoff", "PlaneTakeoff"),
    ("plane-landing", "PlaneLanding"),
    ("timer", "Timer"),
    ("waypoints", "Waypoints"),
    ("vector-square", "VectorSquare"),
    ("mouse-pointer-2", "MousePointer2"),
    ("maximize", "Maximize"),
    ("rocket", "Rocket"),
    ("bird", "Bird"),
    ("square", "Square"),
    ("move", "Move"),
    ("navigation", "Navigation"),
    ("grip-horizontal", "GripHorizontal"),
    ("arrow-left", "ArrowLeft"),
    ("pin", "Pin"),
    ("pin-off", "PinOff"),
    ("chevrons-down-up", "ChevronsDownUp"),
    ("chevrons-up-down", "ChevronsUpDown"),
    ("skip-forward", "SkipForward"),
    ("layout-grid", "LayoutGrid"),
    ("bot", "Bot"),
    ("wrench", "Wrench"),
];

include!("lock_closure.rs");

/// What a plugin library must carry to be loaded: the compiler, and a hash of everything that
/// decides how the types that cross the boundary are laid out and what they do: this crate, the
/// vendored crates, the toolchain and the part of `Cargo.lock` this crate is built from.
/// Not the program's version: a library stays good for as long as none of this changes.
fn abi_stamp() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    println!("cargo:rerun-if-env-changed=RUSTC");
    let lock = root.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    println!("cargo:rerun-if-changed=lock_closure.rs");
    let lock = std::fs::read_to_string(&lock).unwrap_or_else(|e| panic!("{}: {e}", lock.display()));
    let mut files = vec![root.join("rust-toolchain.toml")];
    for dir in ["src/api/src", "src/vendor"] {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
        source_files(&root.join(dir), &mut files);
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut add = |bytes: &[u8]| {
        // A checkout on Windows may end its lines differently; the stamp must not.
        for b in bytes.iter().filter(|b| **b != b'\r') {
            hash = (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    add(lock_closure(&lock, "corvus_api").as_bytes());
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        add(&std::fs::read(f).unwrap_or_default());
    }
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let compiler = std::process::Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=CORVUS_ABI=1|{}|{hash:016x}", compiler.trim());
}

/// The `.rs` and `.toml` files under `dir`, in an order that does not depend on the system.
fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut all: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    all.sort();
    for p in all {
        if p.is_dir() {
            source_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs" || x == "toml") {
            out.push(p);
        }
    }
}

fn main() {
    abi_stamp();
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    for name in FONTS {
        let path = web.join(format!("fonts/{name}.woff2"));
        println!("cargo:rerun-if-changed={}", path.display());
        let woff = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let ttf = wuff::decompress_woff2(&woff).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        std::fs::write(out.join(format!("{name}.ttf")), ttf).expect("write font");
    }

    let lucide_path = web.join("lucide.min.js");
    println!("cargo:rerun-if-changed={}", lucide_path.display());
    let lucide = std::fs::read_to_string(&lucide_path).expect("read lucide.min.js");
    let mut code = String::from("/// The SVG of a Lucide icon by its web name.\npub fn svg(name: &str) -> Option<&'static [u8]> {\n    Some(match name {\n");
    for (name, export) in ICONS {
        let nodes = icon_nodes(&lucide, export).unwrap_or_else(|| panic!("Lucide icon {export} not found"));
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" \
             stroke=\"#ffffff\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{nodes}</svg>"
        );
        writeln!(code, "        {name:?} => {svg:?}.as_bytes(),").unwrap();
    }
    code.push_str("        _ => return None,\n    })\n}\n");
    std::fs::write(out.join("icons.rs"), code).expect("write icons.rs");
}

/// The SVG elements of one icon: `,Power:om,` names the variable, and
/// `om=[["path",{d:"..."}],...]` holds its nodes.
fn icon_nodes(js: &str, export: &str) -> Option<String> {
    let key = format!(",{export}:");
    let at = js.find(&key)? + key.len();
    let var: String = js[at..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    let def = format!("{var}=[[");
    let mut start = None;
    let mut from = 0;
    while let Some(i) = js[from..].find(&def) {
        let i = from + i;
        let before = js[..i].chars().last();
        if !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.') {
            start = Some(i + var.len() + 1);
            break;
        }
        from = i + 1;
    }
    let body = bracketed(&js[start?..])?;
    let mut svg = String::new();
    let mut rest = &body[1..body.len() - 1];
    while let Some(open) = rest.find("[\"") {
        let node = bracketed(&rest[open..])?;
        rest = &rest[open + node.len()..];
        let tag_end = node[2..].find('"')? + 2;
        let tag = &node[2..tag_end];
        let attrs_start = node.find('{')?;
        let attrs_end = node.rfind('}')?;
        svg.push('<');
        svg.push_str(tag);
        for pair in split_attrs(&node[attrs_start + 1..attrs_end]) {
            let (k, v) = pair.split_once(':')?;
            let k = k.trim().trim_matches('"');
            let v = v.trim().trim_matches('"');
            write!(svg, " {k}=\"{v}\"").ok()?;
        }
        svg.push_str("/>");
    }
    Some(svg)
}

/// The text from an opening `[` to its matching `]`, strings respected.
fn bracketed(s: &str) -> Option<&str> {
    let mut depth = 0;
    let mut in_str = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '[' if !in_str => depth += 1,
            ']' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `d:"M1 2",cx:"3"` into its pairs, commas inside strings kept.
fn split_attrs(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_str = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_str = !in_str,
            ',' if !in_str => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}
