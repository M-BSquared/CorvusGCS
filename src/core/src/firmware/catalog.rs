//! Firmware release catalogue and image cache for both flight stacks
//! (`firmware_catalog.py`).
//!
//! The field laptop has no internet and the app never needs it: nothing here
//! runs at startup, every fetch is written to `<firmware_dir>/catalog.json`
//! and every image to `<firmware_dir>/<asset name>`, and with no network the
//! catalogue is served from that cache. Downloads happen here, server side,
//! so the one network egress point is bounded by a timeout and a size cap.
//!
//! Only the release list is trusted from the network. A flash request names a
//! release and a board, never a URL: the download target is re-derived from a
//! fixed template and checked against [`ALLOWED_HOSTS`].

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::ardupilot;
use super::net::{read_up_to, Net};
use super::pyjson;
use super::tables::{BOARD_LABELS, VENDOR_LABELS};
use super::util::{int_or_zero, py_str, py_title, quote, text_or_empty, truthy, urlparse};
use crate::protocol::common::Event;

pub const RELEASES_URL: &str = "https://api.github.com/repos/PX4/PX4-Autopilot/releases";
/// Download URLs are built from this template, never read out of the catalogue.
pub const ASSET_URL: &str = "https://github.com/PX4/PX4-Autopilot/releases/download/{tag}/{name}";
/// The only hosts a firmware image may be fetched from.
pub const ALLOWED_HOSTS: [&str; 4] = [
    "github.com",
    "objects.githubusercontent.com",
    "api.github.com",
    "release-assets.githubusercontent.com",
];
pub const CATALOG_FILE: &str = "catalog.json";
pub const ARDUPILOT_CATALOG_FILE: &str = "catalog-ardupilot.json";
/// PX4 has years of tags; the operator wants the current one and a couple to roll back to.
pub const MAX_RELEASES: usize = 8;
pub const FETCH_TIMEOUT_S: f64 = 20.0;
/// A PX4 image is a few MB; the cap stops a wrong URL from filling the disk.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
/// The release list is a few MB; this is far above it.
const MAX_CATALOG_BYTES: usize = 64 * 1024 * 1024;
/// Assets that are not flight controller firmware; flashing a bootloader image
/// bricks the board until it is recovered over SWD.
pub const NON_FIRMWARE_SUFFIXES: [&str; 3] = ["_bootloader.px4", "_canbootloader.px4", "_cannode.px4"];
pub const DEFAULT_VARIANT: &str = "default";

const PERIPHERAL_BOARDS: [&str; 3] = ["cannode", "dist", "h-flow"];
const PERIPHERAL_PREFIXES: [&str; 3] = ["can-", "gnss-", "io-v"];
const PERIPHERAL_SUFFIXES: [&str; 2] = ["-gps", "-io"];

/// `(substring in the USB descriptor, token that appears in the target name)`.
const DESCRIPTOR_TOKENS: [(&str, &str); 12] = [
    ("cubeorangeplus", "cubeorangeplus"),
    ("cube orange+", "cubeorangeplus"),
    ("cubeorange", "cubeorange"),
    ("cube orange", "cubeorange"),
    ("cubeyellow", "cubeyellow"),
    ("cubeblack", "cubeblack"),
    ("kakuteh7v2", "kakuteh7v2"),
    ("kakuteh7", "kakuteh7"),
    ("pixracer", "fmu-v4"),
    ("durandal", "durandal"),
    ("matek", "matek"),
    ("modalai", "modalai"),
];
/// USB VID:PID of boards whose descriptor is too generic to identify them.
const USB_IDS: [((i64, i64), &str); 2] = [((0x26AC, 0x0011), "fmu-v2"), ((0x26AC, 0x0016), "fmu-v4")];

/// True when the board token names a peripheral rather than an autopilot.
pub fn is_peripheral_board(board: &str) -> bool {
    let token = board.to_lowercase();
    PERIPHERAL_BOARDS.contains(&token.as_str())
        || PERIPHERAL_PREFIXES.iter().any(|p| token.starts_with(p))
        || PERIPHERAL_SUFFIXES.iter().any(|s| token.ends_with(s))
}

fn stem_of(asset_name: &str) -> &str {
    asset_name.strip_suffix(".px4").unwrap_or(asset_name)
}

fn known_label(stem: &str) -> Option<&'static str> {
    BOARD_LABELS.iter().find(|(k, _)| *k == stem).map(|(_, v)| *v)
}

/// Operator facing name for a `<target>.px4` asset.
pub fn board_label(asset_name: &str) -> String {
    let stem = stem_of(asset_name);
    known_label(stem).unwrap_or(stem).to_string()
}

/// Splits a build target into the parts a human reads it by:
/// `{vendor, board, variant, peripheral, title}`. A name that does not follow
/// `<vendor>_<board>_<variant>` still comes back whole.
pub fn describe_board(asset_name: &str) -> Map<String, Value> {
    let stem = stem_of(asset_name);
    let parts: Vec<&str> = stem.splitn(3, '_').collect();
    let (vendor, board, variant) = match parts.as_slice() {
        [v, b, var] => (*v, *b, *var),
        [v, b] => (*v, *b, DEFAULT_VARIANT),
        _ => (stem, stem, DEFAULT_VARIANT),
    };
    let lower = vendor.to_lowercase();
    let vendor_label = VENDOR_LABELS
        .iter()
        .find(|(k, _)| *k == lower)
        .map_or_else(|| py_title(&vendor.replace('-', " ")), |(_, v)| (*v).to_string());
    let mut out = Map::new();
    out.insert("vendor".into(), json!(vendor_label));
    out.insert("board".into(), json!(board));
    out.insert("variant".into(), json!(variant));
    out.insert("peripheral".into(), json!(is_peripheral_board(board)));
    out.insert("title".into(), json!(known_label(stem).unwrap_or(board)));
    out
}

/// True for a `.px4` image that is flight controller firmware.
pub fn is_flashable_asset(name: &str) -> bool {
    name.ends_with(".px4") && !NON_FIRMWARE_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// The download URL for one release asset, built from the fixed template.
pub fn asset_url(tag: &str, name: &str) -> String {
    ASSET_URL.replace("{tag}", &quote(tag)).replace("{name}", &quote(name))
}

/// A filename that cannot escape the cache directory.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or("");
    let mut out = String::new();
    let mut in_run = false;
    for c in base.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    out.chars().take(120).collect()
}

/// Is `url` an https URL on a host firmware is published from?
pub fn host_allowed(url: &str) -> bool {
    match urlparse(url) {
        Some(u) => u.scheme == "https" && u.hostname().is_some_and(|h| ALLOWED_HOSTS.contains(&h.as_str())),
        None => false,
    }
}

/// `VID:PID=xxxx:xxxx` out of a hardware id string.
fn hex_ids(hwid: &str) -> Option<(i64, i64)> {
    let b = hwid.as_bytes();
    let tag = b"VID:PID=";
    let mut i = 0;
    while i + tag.len() + 9 <= b.len() {
        if &b[i..i + tag.len()] == tag {
            let at = i + tag.len();
            let hex4 = |s: &[u8]| s.len() == 4 && s.iter().all(u8::is_ascii_hexdigit);
            if hex4(&b[at..at + 4]) && b[at + 4] == b':' && hex4(&b[at + 5..at + 9]) {
                let parse = |s: &[u8]| i64::from_str_radix(std::str::from_utf8(s).unwrap_or("0"), 16).unwrap_or(0);
                return Some((parse(&b[at..at + 4]), parse(&b[at + 5..at + 9])));
            }
        }
        i += 1;
    }
    None
}

/// Guesses which of `boards` is plugged in, from USB and MAVLink identity
/// hints. A suggestion: the caller preselects it and the operator can always
/// override, because flashing the wrong image is worse than picking from a list.
pub fn detect_board(hints: &Value, boards: &[Value]) -> Option<Value> {
    if boards.is_empty() {
        return None;
    }
    let description = text_or_empty(hints.get("description"));
    let hwid = text_or_empty(hints.get("hwid"));
    let haystack = format!("{description} {hwid}").to_lowercase().replace(['_', ' '], "");
    let find = |token: &str, source: &str| -> Option<Value> {
        let needle = token.to_lowercase();
        let mut candidates: Vec<&Value> = boards
            .iter()
            .filter(|b| {
                let name = match b.get("name") {
                    None => String::new(),
                    some => py_str(some),
                };
                name.to_lowercase().contains(&needle)
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(|b| {
            let name = py_str(b.get("name"));
            (u8::from(!name.contains("_default.")), name.chars().count())
        });
        let best = candidates[0];
        Some(json!({
            "name": best.get("name").cloned().unwrap_or(Value::Null),
            "label": best.get("label").cloned().unwrap_or(Value::Null),
            "token": token,
            "source": source,
        }))
    };
    let mut ids = hex_ids(&hwid);
    if ids.is_none() {
        let int_of = |k: &str| match hints.get(k) {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::Bool(b)) => Some(i64::from(*b)),
            _ => None,
        };
        if let (Some(v), Some(p)) = (int_of("vendor_id"), int_of("product_id")) {
            if v != 0 && p != 0 {
                ids = Some((v, p));
            }
        }
    }
    if let Some(ids) = ids {
        if let Some((_, token)) = USB_IDS.iter().find(|(k, _)| *k == ids) {
            if let Some(found) = find(token, "USB device id") {
                return Some(found);
            }
        }
    }
    for (needle, token) in DESCRIPTOR_TOKENS {
        if haystack.contains(&needle.replace(' ', "")) {
            if let Some(found) = find(token, "USB descriptor") {
                return Some(found);
            }
        }
    }
    if let Some(generation) = ardupilot::fmu_generation(&format!("{description} {hwid}")) {
        if let Some(found) = find(&format!("fmu-v{generation}"), "USB descriptor") {
            return Some(found);
        }
    }
    None
}

/// The connected board, matched once per stack: `{"px4": ..., "ardupilot": ...}`.
/// Per stack because the same board is a different build on each.
pub fn detect_boards(hints: &Value, releases: &[Value]) -> Value {
    let mut px4_found = Value::Null;
    let px4: Vec<&Value> = releases
        .iter()
        .filter(|r| r.get("vendor").map_or("px4".to_string(), |v| py_str(Some(v))) == "px4")
        .collect();
    if !px4.is_empty() {
        let stable = px4.iter().find(|r| !truthy(r.get("prerelease"))).unwrap_or(&px4[0]);
        let boards = stable
            .get("boards")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(mut found) = detect_board(hints, &boards) {
            let name = found.get("name").cloned().unwrap_or(Value::Null);
            found["key"] = name;
            px4_found = found;
        }
    }
    let mut boards: Vec<(String, Value)> = Vec::new();
    for release in releases {
        if release.get("vendor").map(|v| py_str(Some(v))).as_deref() != Some("ardupilot") {
            continue;
        }
        for board in release.get("boards").and_then(Value::as_array).into_iter().flatten() {
            let key = text_or_empty(board.get("board"));
            if !boards.iter().any(|(k, _)| *k == key) {
                boards.push((key, board.clone()));
            }
        }
    }
    let mut ap_found = Value::Null;
    if !boards.is_empty() {
        let list: Vec<Value> = boards.into_iter().map(|(_, b)| b).collect();
        ap_found = ardupilot::detect_board(hints, &list).unwrap_or(Value::Null);
    }
    json!({"px4": px4_found, "ardupilot": ap_found})
}

/// Reduces the GitHub release payload to what the UI needs. Untrusted input:
/// every field is coerced and only `.px4` assets on an allowed host survive.
pub fn parse_releases(payload: &Value) -> Result<Vec<Value>, String> {
    let Some(items) = payload.as_array() else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Value> = Vec::new();
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        if truthy(item.get("draft")) {
            continue;
        }
        let mut boards: Vec<(bool, String, Value)> = Vec::new();
        for asset in item.get("assets").and_then(Value::as_array).into_iter().flatten() {
            let Some(asset) = asset.as_object() else {
                continue;
            };
            let name = text_or_empty(asset.get("name"));
            let url = text_or_empty(asset.get("browser_download_url"));
            if !is_flashable_asset(&name) || !host_allowed(&url) {
                continue;
            }
            let label = board_label(&name);
            let size = int_or_zero(asset.get("size"))?;
            let plain = label == name[..name.len() - 4];
            boards.push((
                plain,
                label.to_lowercase(),
                json!({"name": name, "label": label, "size": size}),
            ));
        }
        if boards.is_empty() {
            continue;
        }
        // Named boards first: the operator's own hardware is almost always one of those.
        boards.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let tag = item.get("tag_name");
        out.push(json!({
            "tag": text_or_empty(tag),
            "name": text_or_empty(item.get("name").filter(|v| truthy(Some(v))).or(tag)),
            "prerelease": truthy(item.get("prerelease")),
            "published": text_or_empty(item.get("published_at")),
            "boards": boards.into_iter().map(|(_, _, b)| b).collect::<Vec<_>>(),
        }));
        if out.len() >= MAX_RELEASES {
            break;
        }
    }
    Ok(out)
}

/// What Python's `UnicodeDecodeError` says for bytes that are not UTF-8.
fn utf8_error_text(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let at = e.valid_up_to();
    let first = bytes[at];
    let (span, reason) = match e.error_len() {
        None => (bytes.len() - at, "unexpected end of data"),
        Some(n) => {
            let start = !(0xC2..=0xF4).contains(&first);
            (
                n,
                if n == 1 && start {
                    "invalid start byte"
                } else {
                    "invalid continuation byte"
                },
            )
        }
    };
    if span <= 1 {
        format!("'utf-8' codec can't decode byte 0x{first:02x} in position {at}: {reason}")
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {at}-{}: {reason}",
            at + span - 1
        )
    }
}

/// Release list plus image cache, backed by a directory on disk. Thread safe
/// for the one thing that matters: a download runs on the flash worker thread
/// while HTTP handlers read the catalogue.
pub struct FirmwareCatalog {
    pub dir: String,
    lock: Mutex<()>,
    net: Arc<dyn Net>,
}

impl FirmwareCatalog {
    pub fn new(dir: impl Into<String>, net: Arc<dyn Net>) -> FirmwareCatalog {
        FirmwareCatalog {
            dir: dir.into(),
            lock: Mutex::new(()),
            net,
        }
    }

    fn path(&self, file: &str) -> std::path::PathBuf {
        Path::new(&self.dir).join(file)
    }

    fn load(&self, file: &str) -> Vec<Value> {
        let Ok(text) = std::fs::read_to_string(self.path(file)) else {
            return Vec::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(mut o)) => match o.remove("releases") {
                Some(Value::Array(a)) => a,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn store(&self, file: &str, releases: &[Value]) {
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let path = self.path(file);
            let tmp = self.path(&format!("{file}.tmp"));
            std::fs::write(&tmp, serde_json::to_vec(&json!({"releases": releases}))?)?;
            std::fs::rename(tmp, path)
        };
        let _ = write();
    }

    /// Releases from the on-disk cache; empty when there is no usable cache.
    pub fn load_cached(&self) -> Vec<Value> {
        self.load(CATALOG_FILE)
    }

    pub fn load_cached_ardupilot(&self) -> Vec<Value> {
        self.load(ARDUPILOT_CATALOG_FILE)
    }

    /// Fetches the release list from GitHub as `(releases, error)`. On a network
    /// failure the cached copy comes back with the error, so the page can say
    /// "showing what is cached" rather than going blank. `Err` is a payload
    /// Python's `int()` refuses.
    pub fn fetch(&self) -> Result<(Vec<Value>, String), String> {
        let reach = |why: String| {
            (
                self.load_cached(),
                format!("could not reach the PX4 release server ({why})"),
            )
        };
        let body = match self
            .net
            .get(RELEASES_URL, "application/vnd.github+json", FETCH_TIMEOUT_S)
            .and_then(|mut r| read_up_to(r.as_mut(), MAX_CATALOG_BYTES))
        {
            Ok(b) => b,
            Err(e) => return Ok(reach(e)),
        };
        let text = match std::str::from_utf8(&body) {
            Ok(t) => t,
            Err(e) => return Ok(reach(utf8_error_text(&body, &e))),
        };
        if let Some(problem) = pyjson::check(text) {
            return Ok(reach(problem));
        }
        let Ok(payload) = serde_json::from_str::<Value>(text) else {
            return Ok(reach("could not read the release list".into()));
        };
        let releases = parse_releases(&payload)?;
        if !releases.is_empty() {
            self.store(CATALOG_FILE, &releases);
        }
        Ok((releases, String::new()))
    }

    /// ArduPilot's release list as `(releases, error)`, the cache when it cannot be read.
    pub fn fetch_ardupilot(&self) -> (Vec<Value>, String) {
        let (releases, error) = ardupilot::fetch_releases(self.net.as_ref());
        if !releases.is_empty() {
            self.store(ARDUPILOT_CATALOG_FILE, &releases);
            return (releases, error);
        }
        (self.load_cached_ardupilot(), error)
    }

    /// The catalogue as the API serves it: both stacks, one release list.
    /// `refresh` forces a fetch; without it the caches are used when they have
    /// anything. A failure of one source is reported but never empties the other.
    pub fn catalog(&self, refresh: bool) -> Result<Value, String> {
        let mut errors: Vec<String> = Vec::new();
        let mut releases = if refresh { Vec::new() } else { self.load_cached() };
        let mut ardupilot_releases = if refresh {
            Vec::new()
        } else {
            self.load_cached_ardupilot()
        };
        // ArduPilot's list costs one request per vehicle and channel, so it is
        // fetched on a refresh and on a genuinely empty first run, never on an
        // ordinary page open that a cache can already answer.
        let fetch_ardupilot = refresh || !(!ardupilot_releases.is_empty() || !releases.is_empty());
        if refresh || releases.is_empty() {
            let (fetched, error) = self.fetch()?;
            releases = fetched;
            if !error.is_empty() {
                errors.push(error);
            }
        }
        if fetch_ardupilot {
            let (fetched, error) = self.fetch_ardupilot();
            ardupilot_releases = fetched;
            if !error.is_empty() {
                errors.push(error);
            }
        }
        let cached = self.list_cached_images();
        let cached_names: Vec<&str> = cached.iter().filter_map(|c| c["name"].as_str()).collect();
        for release in releases.iter_mut().filter_map(Value::as_object_mut) {
            release.entry("vendor").or_insert_with(|| json!("px4"));
            for board in release
                .get_mut("boards")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                let Some(board) = board.as_object_mut() else {
                    continue;
                };
                let name = board.get("name").cloned();
                board.insert(
                    "cached".into(),
                    json!(name
                        .as_ref()
                        .and_then(Value::as_str)
                        .is_some_and(|n| cached_names.contains(&n))),
                );
                for (k, v) in describe_board(&text_or_empty(name.as_ref())) {
                    board.insert(k, v);
                }
                board.insert("key".into(), name.unwrap_or(Value::Null));
            }
        }
        for release in ardupilot_releases.iter_mut().filter_map(Value::as_object_mut) {
            release.entry("vendor").or_insert_with(|| json!("ardupilot"));
            let parsed = ardupilot::parse_tag(&text_or_empty(release.get("tag")));
            release
                .entry("vehicle")
                .or_insert_with(|| json!(parsed.map_or(String::new(), |p| p.0)));
            for board in release
                .get_mut("boards")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                let Some(board) = board.as_object_mut() else {
                    continue;
                };
                let name = board.get("name").cloned();
                board.insert(
                    "cached".into(),
                    json!(name
                        .as_ref()
                        .and_then(Value::as_str)
                        .is_some_and(|n| cached_names.contains(&n))),
                );
                for (k, v) in ardupilot::describe_board(&text_or_empty(board.get("board"))) {
                    board.insert(k, v);
                }
                let key = board.get("board").cloned().unwrap_or(Value::Null);
                board.insert("key".into(), key);
            }
        }
        // PX4 first: it is the primary target.
        releases.extend(ardupilot_releases);
        Ok(json!({
            "releases": releases,
            "cached": cached,
            "error": errors.join("; "),
            "dir": self.dir,
        }))
    }

    /// Images already on disk: what can be flashed with no network.
    pub fn list_cached_images(&self) -> Vec<Value> {
        let Ok(read) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = read
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut out = Vec::new();
        for name in names {
            if !(name.ends_with(".px4") || ardupilot::is_flashable_asset(&name)) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(self.path(&name)) else {
                continue;
            };
            out.push(json!({"name": name, "size": meta.len(), "label": board_label(&name)}));
        }
        out
    }

    /// Finds one board's asset in the catalogue; `None` when it is not there.
    /// The URL is derived here rather than accepted from the client. An
    /// ArduPilot tag carries its own prefix, so the routing is a property of
    /// the request.
    pub fn resolve(&self, release_tag: &str, board_name: &str) -> Option<Value> {
        if ardupilot::parse_tag(release_tag).is_some() {
            return ardupilot::resolve(&self.load_cached_ardupilot(), release_tag, board_name);
        }
        for release in self.load_cached() {
            if py_str(release.get("tag")) != release_tag {
                continue;
            }
            for board in release.get("boards").and_then(Value::as_array).into_iter().flatten() {
                let name = py_str(board.get("name"));
                if name != board_name || !is_flashable_asset(&name) {
                    continue;
                }
                let tag = text_or_empty(release.get("tag"));
                let mut found = board.as_object().cloned().unwrap_or_default();
                found.insert("url".into(), json!(asset_url(&tag, &name)));
                found.insert("tag".into(), json!(tag));
                return Some(Value::Object(found));
            }
        }
        None
    }

    /// Path of an already downloaded image, or `None`.
    pub fn cached_path(&self, name: &str) -> Option<String> {
        let path = self.path(&safe_name(name));
        path.is_file().then(|| path.to_string_lossy().into_owned())
    }

    /// Downloads one image, caches it and returns its bytes. The error is an
    /// operator readable message; the flash service turns it into the status.
    pub fn download(
        &self,
        entry: &Value,
        on_progress: Option<&dyn Fn(i64, i64)>,
        cancel: Option<&Event>,
    ) -> Result<Vec<u8>, String> {
        let name = safe_name(&text_or_empty(entry.get("name")));
        let url = text_or_empty(entry.get("url"));
        // The extension and the host are checked together: a `.px4` may only
        // come from GitHub and an `.apj` only from ArduPilot's own server.
        if name.ends_with(".px4") {
            if !host_allowed(&url) {
                return Err("firmware download refused: unexpected host".into());
            }
        } else if ardupilot::is_flashable_asset(&name) {
            if !ardupilot::host_allowed(&url) {
                return Err("firmware download refused: unexpected host".into());
            }
        } else {
            return Err("not a firmware image (.px4 or .apj)".into());
        }
        if let Some(cached) = self.cached_path(&name) {
            if let Ok(bytes) = std::fs::read(cached) {
                return Ok(bytes);
            }
        }
        let expected = int_or_zero(entry.get("size"))?;
        let mut reply = self
            .net
            .get(&url, "application/vnd.github+json", FETCH_TIMEOUT_S)
            .map_err(|e| format!("firmware download failed ({e})"))?;
        let total = if expected != 0 {
            expected
        } else {
            reply.content_length().unwrap_or(0)
        };
        let mut buf: Vec<u8> = Vec::new();
        loop {
            if cancel.is_some_and(Event::is_set) {
                return Err("download cancelled".into());
            }
            let chunk = reply
                .read(CHUNK)
                .map_err(|e| format!("firmware download failed ({e})"))?;
            if chunk.is_empty() {
                break;
            }
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_IMAGE_BYTES {
                return Err("firmware image is implausibly large".into());
            }
            if let Some(report) = on_progress {
                report(buf.len() as i64, total);
            }
        }
        if buf.is_empty() {
            return Err("firmware download returned an empty file".into());
        }
        {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let write = || -> std::io::Result<()> {
                std::fs::create_dir_all(&self.dir)?;
                let path = self.path(&name);
                let tmp = self.path(&format!("{name}.part"));
                std::fs::write(&tmp, &buf)?;
                std::fs::rename(tmp, path)
            };
            // A full or read only disk must not fail the flash: the bytes are in hand.
            let _ = write();
        }
        Ok(buf)
    }
}
