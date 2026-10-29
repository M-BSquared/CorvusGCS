//! Takes the web interface's own fonts and icons, so the two look the same
//! without a second copy of either in the repository.
//!
//! * `src/vendor/fonts/*.woff2` (Inter, JetBrains Mono; variable weight) are
//!   decoded to TrueType, which egui reads, into `$OUT_DIR`.
//! * The Lucide icons the interface draws are cut out of
//!   `src/vendor/lucide.min.js` and written as SVG documents into
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
    ("database", "Database"),
    ("eye", "Eye"),
    ("eye-off", "EyeOff"),
    ("file-plus", "FilePlus"),
    ("folder-open", "FolderOpen"),
    ("gamepad-2", "Gamepad2"),
    ("gauge", "Gauge"),
    ("image-plus", "ImagePlus"),
    ("key-round", "KeyRound"),
    ("map", "Map"),
    ("mountain", "Mountain"),
    ("palette", "Palette"),
    ("panels-top-left", "PanelsTopLeft"),
    ("save", "Save"),
    ("arrow-up-from-line", "ArrowUpFromLine"),
    ("move-vertical", "MoveVertical"),
    ("compass", "Compass"),
    ("plane", "Plane"),
    ("rotate-3d", "Rotate3d"),
    ("battery-medium", "BatteryMedium"),
    ("satellite", "Satellite"),
    ("folder", "Folder"),
    ("hard-drive-download", "HardDriveDownload"),
    ("file-clock", "FileClock"),
    ("eraser", "Eraser"),
    ("arrow-right", "ArrowRight"),
    ("chart-spline", "ChartSpline"),
];

fn main() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    for name in FONTS {
        let path = web.join(format!("vendor/fonts/{name}.woff2"));
        println!("cargo:rerun-if-changed={}", path.display());
        let woff = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let ttf = wuff::decompress_woff2(&woff).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        std::fs::write(out.join(format!("{name}.ttf")), ttf).expect("write font");
    }

    let lucide_path = web.join("vendor/lucide.min.js");
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
