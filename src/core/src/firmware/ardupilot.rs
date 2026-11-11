//! ArduPilot firmware releases, the second source of the catalogue
//! (`ardupilot_firmware.py`).
//!
//! ArduPilot publishes its builds under a path on its own server that *is* the
//! release (`https://firmware.ardupilot.org/<Vehicle>/<channel>/<board>/<binary>.apj`),
//! and the only index of it worth reading is the directory listing of each
//! vehicle and channel. A board name from that page is untrusted: it is
//! substituted into a URL and into a cache filename, so anything that is not a
//! plain directory token is dropped, and the URL of an image is built from a
//! template, never taken from the page or the cache.

use serde_json::{json, Map, Value};

use super::net::{read_up_to, Net};
use super::tables::{AP_FMU_BOARDS, AP_NOT_FLASHABLE, AP_VENDOR_PREFIXES};
use super::util::{py_split, py_str, py_strip, quote, text_or_empty, unquote, urlparse};
use crate::dialect::{vehicle_class, VehicleClass};

pub const BASE_URL: &str = "https://firmware.ardupilot.org";
/// The one host an `.apj` may be fetched from, checked on the resolved URL.
pub const ALLOWED_HOSTS: [&str; 1] = ["firmware.ardupilot.org"];
/// Release tags carry this prefix so a flash request routes back to its source.
pub const TAG_PREFIX: &str = "ardupilot:";
pub const FETCH_TIMEOUT_S: f64 = 12.0;
/// A listing is a page of links; 4 MB is far above any real one.
pub const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// One firmware an operator can pick, and where the server keeps it.
#[derive(Debug, Clone, Copy)]
pub struct Vehicle {
    pub key: &'static str,
    pub directory: &'static str,
    pub suffix: &'static str,
    pub binary: &'static str,
    pub label: &'static str,
}

/// The vehicles Corvus flies. AntennaTracker and Blimp are left out on purpose.
pub const VEHICLES: [Vehicle; 5] = [
    Vehicle {
        key: "Copter",
        directory: "Copter",
        suffix: "",
        binary: "arducopter",
        label: "ArduCopter",
    },
    Vehicle {
        key: "Heli",
        directory: "Copter",
        suffix: "-heli",
        binary: "arducopter-heli",
        label: "ArduCopter Heli",
    },
    Vehicle {
        key: "Plane",
        directory: "Plane",
        suffix: "",
        binary: "arduplane",
        label: "ArduPlane",
    },
    Vehicle {
        key: "Rover",
        directory: "Rover",
        suffix: "",
        binary: "ardurover",
        label: "Rover",
    },
    Vehicle {
        key: "Sub",
        directory: "Sub",
        suffix: "",
        binary: "ardusub",
        label: "ArduSub",
    },
];

/// `(directory, label, prerelease)`. `latest`, the per commit build, is not offered.
pub const CHANNELS: [(&str, &str, bool); 2] = [("stable", "stable", false), ("beta", "beta", true)];

const NOT_BOARDS: [&str; 5] = ["..", ".", "parent directory", "tools", "logs"];
const FILE_SUFFIXES: [&str; 10] = [
    ".apj", ".abin", ".elf", ".hex", ".bin", ".px4", ".txt", ".json", ".gz", ".html",
];
const FILE_SUFFIX_XML: &str = ".xml";
const NOT_FLASHABLE_PREFIXES: [&str; 4] = ["sitl", "apm1", "apm2", "bebop"];
const SIM_ON_HARDWARE: &str = "-simonhardware";
/// Shorter board names are too likely to turn up inside an unrelated descriptor.
const MIN_EMBEDDED_NAME: usize = 5;

fn vehicle(key: &str) -> Option<&'static Vehicle> {
    VEHICLES.iter().find(|v| v.key == key)
}

/// The `.apj` stem for one vehicle, or an empty string.
pub fn vehicle_binary(vehicle_key: &str) -> &'static str {
    vehicle(vehicle_key).map_or("", |v| v.binary)
}

/// The firmware a vehicle of this MAV_TYPE runs, or an empty string when none
/// fits; what the Firmware page opens on when an aircraft is connected.
pub fn vehicle_for_mav_type(mav_type: &Value) -> &'static str {
    let Some(t) = py_int_of(mav_type) else {
        return "";
    };
    if t == 4 {
        return "Heli";
    }
    match vehicle_class(t) {
        VehicleClass::Copter => "Copter",
        VehicleClass::Plane => "Plane",
        VehicleClass::Rover => "Rover",
        VehicleClass::Sub => "Sub",
        VehicleClass::Other => "",
    }
}

/// `int(value)`, `None` where Python raises.
fn py_int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        Value::String(s) => {
            let t = py_strip(s);
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                t.parse().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The catalogue tag for one vehicle and channel.
pub fn release_tag(vehicle_key: &str, channel: &str) -> String {
    format!("{TAG_PREFIX}{vehicle_key}/{channel}")
}

/// `ardupilot:Copter/stable` to `("Copter", "stable")`. Validated against the
/// tables rather than merely parsed: the two halves become path segments.
pub fn parse_tag(tag: &str) -> Option<(String, String)> {
    let rest = tag.strip_prefix(TAG_PREFIX)?;
    let (vehicle_key, channel) = rest.split_once('/')?;
    vehicle(vehicle_key)?;
    CHANNELS
        .iter()
        .any(|(c, _, _)| *c == channel)
        .then(|| (vehicle_key.to_string(), channel.to_string()))
}

/// The cache filename for one build. ArduPilot names every build after its
/// vehicle, so the distinguishing parts move into the name.
pub fn asset_name(vehicle_key: &str, channel: &str, board: &str) -> String {
    format!("{}-{channel}-{board}.apj", vehicle_binary(vehicle_key))
}

/// The download URL for one build, from the fixed template.
pub fn asset_url(vehicle_key: &str, channel: &str, board: &str) -> String {
    let found = vehicle(vehicle_key);
    let directory = found.map_or(vehicle_key, |v| v.directory);
    let suffix = found.map_or("", |v| v.suffix);
    format!(
        "{BASE_URL}/{}/{}/{}/{}.apj",
        quote(directory),
        quote(channel),
        quote(&format!("{board}{suffix}")),
        vehicle_binary(vehicle_key)
    )
}

pub fn index_url(directory: &str, channel: &str) -> String {
    format!("{BASE_URL}/{}/{}/", quote(directory), quote(channel))
}

pub fn is_flashable_asset(name: &str) -> bool {
    name.ends_with(".apj")
}

/// False for a board directory that holds no `.apj` to flash.
pub fn is_flashable_board(board: &str) -> bool {
    let mut token = board.to_lowercase();
    for v in &VEHICLES {
        if !v.suffix.is_empty() && token.ends_with(v.suffix) {
            token.truncate(token.len() - v.suffix.len());
            break;
        }
    }
    !(AP_NOT_FLASHABLE.contains(&token.as_str()) || NOT_FLASHABLE_PREFIXES.iter().any(|p| token.starts_with(p)))
}

/// Is `url` an https URL on a host firmware is published from?
pub fn host_allowed(url: &str) -> bool {
    match urlparse(url) {
        Some(u) => u.scheme == "https" && u.hostname().is_some_and(|h| ALLOWED_HOSTS.contains(&h.as_str())),
        None => false,
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$`, where `$` also matches before a final newline.
pub fn board_token(token: &str) -> bool {
    let body = token.strip_suffix('\n').unwrap_or(token);
    let mut chars = body.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && body.chars().count() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// Every `href="..."` of a page, as `re.finditer(r'href=["\']([^"\'>]+)["\']', html, re.I)` finds them.
fn hrefs(html: &str) -> Vec<&str> {
    let b = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 6 <= b.len() {
        if b[i..i + 5].eq_ignore_ascii_case(b"href=") && matches!(b[i + 5], b'"' | b'\'') {
            let mut j = i + 6;
            while j < b.len() && !matches!(b[j], b'"' | b'\'' | b'>') {
                j += 1;
            }
            if j > i + 6 && j < b.len() && matches!(b[j], b'"' | b'\'') {
                out.push(&html[i + 6..j]);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Board directory names out of one firmware.ardupilot.org listing. `base` is
/// the listing's own path; the server links every board by its absolute path
/// under it, without a trailing slash, and a plain relative `Board/` is accepted
/// too. Untrusted input: every candidate must be a whole directory token.
pub fn parse_index(html: &str, base: &str) -> Vec<String> {
    let prefix = if base.is_empty() || base.ends_with('/') {
        base.to_string()
    } else {
        format!("{base}/")
    };
    let mut names: Vec<String> = Vec::new();
    for raw in hrefs(html) {
        let decoded = unquote(raw);
        let mut href = py_strip(&decoded).to_string();
        let Some(parsed) = urlparse(&href) else {
            continue;
        };
        if !parsed.scheme.is_empty() || !parsed.netloc.is_empty() {
            if !parsed.hostname().is_some_and(|h| ALLOWED_HOSTS.contains(&h.as_str())) {
                continue;
            }
            href = parsed.path;
        }
        let token: String = if !prefix.is_empty() && href.starts_with(&prefix) {
            let rest = &href[prefix.len()..];
            let rest = rest.strip_suffix('/').unwrap_or(rest);
            let lower = rest.to_lowercase();
            if FILE_SUFFIXES.iter().any(|s| lower.ends_with(s)) || lower.ends_with(FILE_SUFFIX_XML) {
                continue;
            }
            rest.to_string()
        } else if href.ends_with('/') && !href.starts_with('/') {
            href[..href.len() - 1].to_string()
        } else {
            continue;
        };
        if token.contains('/') || NOT_BOARDS.contains(&token.to_lowercase().as_str()) {
            continue;
        }
        if !board_token(&token) || names.contains(&token) {
            continue;
        }
        names.push(token);
    }
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// One vehicle's flashable boards out of its directory's listing. Copter and
/// Heli share a directory; a heli build is the `-heli` directory with the
/// suffix taken off, so the same board has the same name under both.
pub fn vehicle_boards(v: &Vehicle, listing: &[String]) -> Vec<String> {
    let siblings: Vec<&str> = VEHICLES
        .iter()
        .filter(|o| o.directory == v.directory && !o.suffix.is_empty())
        .map(|o| o.suffix)
        .collect();
    let mut out = Vec::new();
    for name in listing {
        if !is_flashable_board(name) {
            continue;
        }
        if !v.suffix.is_empty() {
            if let Some(board) = name.strip_suffix(v.suffix) {
                if !board.is_empty() && board_token(board) {
                    out.push(board.to_string());
                }
            }
        } else if !siblings.iter().any(|s| name.ends_with(s)) {
            out.push(name.clone());
        }
    }
    out
}

/// Splits a board directory name into the parts a human reads it by.
pub fn describe_board(board: &str) -> Map<String, Value> {
    let lower = board.to_lowercase();
    let vendor = AP_VENDOR_PREFIXES
        .iter()
        .find(|(p, _)| lower.starts_with(p))
        .map_or("Other", |(_, label)| *label);
    let (mut variant, mut title) = ("default".to_string(), board.to_string());
    if lower.ends_with(SIM_ON_HARDWARE) {
        let cut = board.chars().count() - SIM_ON_HARDWARE.chars().count();
        variant = board.chars().skip(cut + 1).collect();
        title = board.chars().take(cut).collect();
    }
    let mut out = Map::new();
    out.insert("vendor".into(), json!(vendor));
    out.insert("board".into(), json!(board));
    out.insert("variant".into(), json!(variant));
    out.insert("peripheral".into(), json!(false));
    out.insert("title".into(), json!(title));
    out
}

/// A board name as a descriptor might spell it: case, spaces and dashes gone.
fn norm(text: &str) -> String {
    text.to_lowercase()
        .replace('+', "plus")
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

/// `fmu[\s_-]*v?(\d+[a-z]*)`, case insensitive: the generation named in a descriptor, lower case.
pub fn fmu_generation(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    if lower.len() != chars.len() {
        return None;
    }
    let mut i = 0;
    while i + 3 <= lower.len() {
        if lower[i..i + 3] == ['f', 'm', 'u'] {
            let mut j = i + 3;
            while j < lower.len() && (lower[j].is_whitespace() || lower[j] == '_' || lower[j] == '-') {
                j += 1;
            }
            if j < lower.len() && lower[j] == 'v' {
                // `v?` is greedy but the digits must follow either way.
                if j + 1 < lower.len() && lower[j + 1].is_ascii_digit() {
                    j += 1;
                }
            }
            if j < lower.len() && lower[j].is_ascii_digit() {
                let mut k = j;
                while k < lower.len() && lower[k].is_ascii_digit() {
                    k += 1;
                }
                while k < lower.len() && lower[k].is_ascii_lowercase() {
                    k += 1;
                }
                return Some(lower[j..k].iter().collect());
            }
        }
        i += 1;
    }
    None
}

/// Guesses which of `boards` is plugged in, from the USB descriptor; a
/// suggestion only. ArduPilot sets its USB product string to the board's own
/// name, and a board still on PX4 names its FMU generation instead.
pub fn detect_board(hints: &Value, boards: &[Value]) -> Option<Value> {
    let description = text_or_empty(hints.get("description"));
    if boards.is_empty() || py_strip(&description).is_empty() {
        return None;
    }
    let mut by_key: Vec<(String, &Value)> = Vec::new();
    for entry in boards {
        let key = norm(&text_or_empty(entry.get("board")));
        if !key.is_empty() && !by_key.iter().any(|(k, _)| *k == key) {
            by_key.push((key, entry));
        }
    }
    let found = |key: &str| -> Value {
        let entry = by_key.iter().find(|(k, _)| k == key).map(|(_, e)| *e).unwrap();
        let board = text_or_empty(entry.get("board"));
        let label = match entry.get("title") {
            Some(t) if super::util::truthy(Some(t)) => t.clone(),
            _ => json!(board),
        };
        json!({
            "name": entry.get("name").cloned().unwrap_or(Value::Null),
            "key": board,
            "label": label,
            "token": board,
            "source": "USB descriptor",
        })
    };
    let has = |key: &str| by_key.iter().any(|(k, _)| k == key);
    let whole = norm(&description);
    let words: Vec<String> = py_split(&description).iter().map(|w| norm(w)).collect();
    for candidate in std::iter::once(&whole).chain(words.iter()) {
        if has(candidate) {
            return Some(found(candidate));
        }
    }
    if let Some(generation) = fmu_generation(&description) {
        let wanted = format!("fmu-v{generation}");
        let target = AP_FMU_BOARDS
            .iter()
            .find(|(k, _)| *k == wanted)
            .map_or_else(String::new, |(_, v)| norm(v));
        if has(&target) {
            return Some(found(&target));
        }
    }
    let mut best: Option<&String> = None;
    for (key, _) in &by_key {
        if key.chars().count() >= MIN_EMBEDDED_NAME
            && whole.contains(key.as_str())
            && best.is_none_or(|b| key.chars().count() > b.chars().count())
        {
            best = Some(key);
        }
    }
    best.map(|k| found(k))
}

fn fetch_listing(net: &dyn Net, directory: &str, channel: &str) -> Option<Vec<String>> {
    let url = index_url(directory, channel);
    let mut reply = net.get(&url, "text/html", FETCH_TIMEOUT_S).ok()?;
    let raw = read_up_to(reply.as_mut(), MAX_INDEX_BYTES).ok()?;
    if raw.len() > MAX_INDEX_BYTES {
        return None;
    }
    let base = urlparse(&url)?.path;
    Some(parse_index(&String::from_utf8_lossy(&raw), &base))
}

/// Every vehicle and channel pair with its board list, as `(releases, error)`.
/// One request per directory and channel, in parallel. A pair whose listing
/// cannot be read contributes nothing and does not fail the rest; a server that
/// answers but yields no boards is an error, since that is what a changed
/// listing format looks like.
pub fn fetch_releases(net: &dyn Net) -> (Vec<Value>, String) {
    let mut directories: Vec<&str> = Vec::new();
    for v in &VEHICLES {
        if !directories.contains(&v.directory) {
            directories.push(v.directory);
        }
    }
    let pairs: Vec<(&str, &str)> = directories
        .iter()
        .flat_map(|d| CHANNELS.iter().map(move |(c, _, _)| (*d, *c)))
        .collect();
    let listings: Vec<Option<Vec<String>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = pairs
            .iter()
            .map(|(d, c)| scope.spawn(move || fetch_listing(net, d, c)))
            .collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let listing_of = |d: &str, c: &str| -> Option<&Vec<String>> {
        pairs
            .iter()
            .position(|(pd, pc)| *pd == d && *pc == c)
            .and_then(|i| listings[i].as_ref())
    };
    let mut releases = Vec::new();
    for (channel, channel_label, prerelease) in CHANNELS {
        for v in &VEHICLES {
            let empty = Vec::new();
            let boards = vehicle_boards(v, listing_of(v.directory, channel).unwrap_or(&empty));
            if boards.is_empty() {
                continue;
            }
            releases.push(json!({
                "tag": release_tag(v.key, channel),
                "name": format!("{} ({channel_label})", v.label),
                "prerelease": prerelease,
                "vendor": "ardupilot",
                "vehicle": v.key,
                "published": "",
                "boards": boards.iter().map(|b| json!({
                    "name": asset_name(v.key, channel, b),
                    "board": b,
                    "label": b,
                    "size": 0,
                    "url": asset_url(v.key, channel, b),
                })).collect::<Vec<_>>(),
            }));
        }
    }
    if !releases.is_empty() {
        return (releases, String::new());
    }
    if listings.iter().all(Option::is_none) {
        return (Vec::new(), "could not reach the ArduPilot firmware server".into());
    }
    (
        Vec::new(),
        "the ArduPilot firmware server answered, but no boards could be read from it".into(),
    )
}

/// Finds one board's build in a cached release list, re-deriving its URL: the
/// URL in the cache is ignored, so a tampered file can at worst name a
/// different board on the same server.
pub fn resolve(releases: &[Value], release_tag_: &str, board_name: &str) -> Option<Value> {
    let (vehicle_key, channel) = parse_tag(release_tag_)?;
    for release in releases {
        if py_str(release.get("tag")) != release_tag_ {
            continue;
        }
        for entry in release.get("boards").and_then(Value::as_array).into_iter().flatten() {
            if py_str(entry.get("name")) != board_name {
                continue;
            }
            let board = text_or_empty(entry.get("board"));
            if !board_token(&board) || !is_flashable_board(&board) {
                return None;
            }
            let mut found = entry.as_object().cloned().unwrap_or_default();
            found.insert("tag".into(), json!(release_tag_));
            found.insert("url".into(), json!(asset_url(&vehicle_key, &channel, &board)));
            return Some(Value::Object(found));
        }
    }
    None
}
