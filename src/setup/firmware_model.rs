//! What the Firmware page says and decides (`python/src/js/setup-firmware.js`): the
//! catalogue read into releases and boards, the picker that narrows a flight
//! stack to one image (stack, release, manufacturer, board), the gate that
//! decides whether Flash is allowed, the words for the transport and the
//! progress, and the request bodies. The page itself only draws these.
//!
//! The picker follows the web page's rules to the letter, because each of them
//! exists to stop one wrong flash: the first option of a list is never a
//! board, a board nobody chose is never selected, a pre-release is never the
//! release a list opens on, and whatever is selected stays reachable.

use std::cmp::Ordering;

use serde_json::{json, Value};

use crate::data::units::{js_number, js_round, js_to_fixed};

/// The flash log keeps this many lines.
pub const LOG_CAP: usize = 50;

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn flag(v: &Value, k: &str) -> bool {
    match v.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}

/// One flashable image of a release.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    /// The file name, which is what the flash request names.
    pub name: String,
    /// What names the same board in every release of one stack.
    pub key: String,
    pub label: String,
    pub title: String,
    pub vendor: String,
    pub variant: String,
    pub size: f64,
    pub cached: bool,
    pub peripheral: bool,
}

impl Board {
    pub fn from_json(v: &Value) -> Board {
        Board {
            name: text(v, "name"),
            key: text(v, "key"),
            label: text(v, "label"),
            title: text(v, "title"),
            vendor: text(v, "vendor"),
            variant: text(v, "variant"),
            size: v.get("size").and_then(Value::as_f64).unwrap_or(0.0),
            cached: flag(v, "cached"),
            peripheral: flag(v, "peripheral"),
        }
    }

    /// `keyOf`.
    pub fn key_of(&self) -> &str {
        if self.key.is_empty() {
            &self.name
        } else {
            &self.key
        }
    }

    /// `vendorOf`: a board with no manufacturer still has to live somewhere.
    pub fn vendor_of(&self) -> &str {
        if self.vendor.is_empty() {
            "Other"
        } else {
            &self.vendor
        }
    }

    /// Whether this is a developer build (`variant` other than `default`).
    pub fn is_variant(&self) -> bool {
        !self.variant.is_empty() && self.variant != "default"
    }
}

/// One release: PX4's tag or an ArduPilot vehicle and channel.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Release {
    pub tag: String,
    pub name: String,
    pub prerelease: bool,
    /// The flight stack (`px4` or `ardupilot`).
    pub stack: String,
    pub vehicle: String,
    pub boards: Vec<Board>,
}

impl Release {
    pub fn from_json(v: &Value) -> Release {
        let stack = text(v, "vendor");
        Release {
            tag: text(v, "tag"),
            name: text(v, "name"),
            prerelease: flag(v, "prerelease"),
            stack: if stack.is_empty() { "px4".into() } else { stack },
            vehicle: text(v, "vehicle"),
            boards: v
                .get("boards")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(Board::from_json).collect())
                .unwrap_or_default(),
        }
    }
}

/// The board the backend recognised on the USB port.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detected {
    pub key: String,
    pub name: String,
    pub label: String,
    pub source: String,
}

impl Detected {
    fn from_json(v: Option<&Value>) -> Option<Detected> {
        let v = v.filter(|v| v.is_object())?;
        let d = Detected {
            key: text(v, "key"),
            name: text(v, "name"),
            label: text(v, "label"),
            source: text(v, "source"),
        };
        (!d.key.is_empty() || !d.name.is_empty()).then_some(d)
    }

    fn id(&self) -> &str {
        if self.key.is_empty() {
            &self.name
        } else {
            &self.key
        }
    }
}

/// `GET /api/firmware/catalog`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub releases: Vec<Release>,
    pub error: String,
    pub dir: String,
    pub detected_px4: Option<Detected>,
    pub detected_ardupilot: Option<Detected>,
    pub suggested_stack: String,
    pub suggested_vehicle: String,
}

impl Catalog {
    pub fn from_json(v: &Value) -> Catalog {
        let detected = v.get("detected");
        let suggested = v.get("suggested");
        let sg = |k: &str| suggested.map(|s| text(s, k)).unwrap_or_default();
        Catalog {
            releases: v
                .get("releases")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(Release::from_json).collect())
                .unwrap_or_default(),
            error: text(v, "error"),
            dir: text(v, "dir"),
            detected_px4: Detected::from_json(detected.and_then(|d| d.get("px4"))),
            detected_ardupilot: Detected::from_json(detected.and_then(|d| d.get("ardupilot"))),
            suggested_stack: sg("stack"),
            suggested_vehicle: sg("vehicle"),
        }
    }
}

/// `String.prototype.localeCompare` for the names in a catalogue: letters
/// without regard to case, a lower case letter ahead of its capital.
pub fn locale_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| b.cmp(a))
}

/// `formatSize`: kilobytes, or megabytes from 1024 KB; nothing for no size.
pub fn format_size(bytes: f64) -> String {
    let kb = bytes / 1024.0;
    if !kb.is_finite() || kb <= 0.0 {
        return String::new();
    }
    if kb >= 1024.0 {
        format!("{} MB", js_to_fixed(kb / 1024.0, 1))
    } else {
        format!("{} KB", js_number(js_round(kb)))
    }
}

/// What a board is called in the board dropdown: the label itself says what a
/// closed select cannot show (developer build, peripheral, already here).
pub fn board_option_label(b: &Board) -> String {
    let mut label = if !b.title.is_empty() {
        b.title.clone()
    } else if !b.label.is_empty() {
        b.label.clone()
    } else {
        b.name.clone()
    };
    if b.is_variant() {
        label += &format!("  \u{b7}  {}", b.variant);
    }
    if b.peripheral {
        label += "  \u{b7}  peripheral";
    }
    if b.cached {
        label += "  \u{b7}  downloaded";
    }
    label
}

/// The picker: which stack, release, manufacturer and board are chosen, and
/// what each list holds because of that.
#[derive(Debug, Clone, Default)]
pub struct Picker {
    pub catalog: Option<Catalog>,
    /// `px4` or `ardupilot`.
    pub stack: String,
    pub stack_touched: bool,
    pub release: String,
    pub boards: Vec<Board>,
    pub vendor: String,
    /// The selected board's file name, empty for none.
    pub board: String,
    pub board_key: String,
    pub board_touched: bool,
    pub show_variants: bool,
}

impl Picker {
    pub fn new() -> Picker {
        Picker {
            stack: "px4".into(),
            ..Picker::default()
        }
    }

    fn detected(&self) -> Option<&Detected> {
        let c = self.catalog.as_ref()?;
        if self.stack == "ardupilot" {
            c.detected_ardupilot.as_ref()
        } else {
            c.detected_px4.as_ref()
        }
    }

    /// The releases of the stack on screen.
    pub fn stack_releases(&self) -> Vec<&Release> {
        self.catalog
            .as_ref()
            .map(|c| c.releases.iter().filter(|r| r.stack == self.stack).collect())
            .unwrap_or_default()
    }

    /// `vendorRank`: 0 the vendor of the board thought to be plugged in, 1 PX4,
    /// 2 the rest, 3 the boards no manufacturer could be read from.
    fn vendor_rank(&self, vendor: &str) -> u8 {
        let detected = self.detected().and_then(|d| {
            self.boards
                .iter()
                .find(|b| b.key_of() == d.id())
                .map(|b| b.vendor.as_str())
                .filter(|v| !v.is_empty())
        });
        if detected == Some(vendor) {
            0
        } else if vendor == "Other" {
            3
        } else if vendor == "PX4" {
            1
        } else {
            2
        }
    }

    /// `compareBoards`: a vendor is one block; inside it autopilots come
    /// before peripherals, the plain build before its developer variants.
    fn compare(&self, a: &Board, b: &Board) -> Ordering {
        if a.vendor != b.vendor {
            return self
                .vendor_rank(&a.vendor)
                .cmp(&self.vendor_rank(&b.vendor))
                .then_with(|| locale_cmp(&a.vendor, &b.vendor));
        }
        if a.peripheral != b.peripheral {
            return if a.peripheral {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        a.is_variant().cmp(&b.is_variant()).then_with(|| {
            let name = |x: &Board| {
                if x.title.is_empty() {
                    x.name.clone()
                } else {
                    x.title.clone()
                }
            };
            locale_cmp(&name(a), &name(b))
        })
    }

    /// `visibleBoards`: developer builds only on request, and whatever is
    /// selected right now stays reachable whatever else changes.
    pub fn visible_boards(&self) -> Vec<&Board> {
        let mut out: Vec<&Board> = self
            .boards
            .iter()
            .filter(|b| (!self.board.is_empty() && b.name == self.board) || self.show_variants || !b.is_variant())
            .collect();
        out.sort_by(|a, b| self.compare(a, b));
        out
    }

    /// `visibleVendors`, in the order of their boards.
    pub fn visible_vendors(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for b in self.visible_boards() {
            let v = b.vendor_of().to_string();
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
        seen
    }

    pub fn boards_of_vendor(&self, vendor: &str) -> Vec<&Board> {
        self.visible_boards()
            .into_iter()
            .filter(|b| b.vendor_of() == vendor)
            .collect()
    }

    /// `preferredBoard`: the key the picker should land on, the operator's
    /// pick or else the detected board.
    fn preferred_board(&self) -> String {
        if self.board_touched && !self.board_key.is_empty() {
            return self.board_key.clone();
        }
        if let Some(d) = self.detected() {
            return d.id().to_string();
        }
        if self.board.is_empty() {
            String::new()
        } else {
            self.board_key.clone()
        }
    }

    /// `defaultRelease`: never a pre-release; for ArduPilot the connected
    /// aircraft's vehicle, so re-flashing a plane does not open on the copter.
    fn default_release(&self, list: &[&Release]) -> String {
        let stable: Vec<&&Release> = list.iter().filter(|r| !r.prerelease).collect();
        let vehicle = self
            .catalog
            .as_ref()
            .map(|c| c.suggested_vehicle.as_str())
            .unwrap_or("");
        let own = (!vehicle.is_empty())
            .then(|| stable.iter().find(|r| r.vehicle == vehicle))
            .flatten();
        own.or(stable.first())
            .map(|r| r.tag.clone())
            .or_else(|| list.first().map(|r| r.tag.clone()))
            .unwrap_or_default()
    }

    /// The catalogue arrived: open on the stack the aircraft runs, until the
    /// operator says otherwise, and fill every list.
    pub fn load(&mut self, catalog: Catalog) {
        let suggested = catalog.suggested_stack.clone();
        self.catalog = Some(catalog);
        let stack = if !self.stack_touched && (suggested == "px4" || suggested == "ardupilot") {
            suggested
        } else {
            self.stack.clone()
        };
        self.set_stack(&stack);
    }

    /// `setStack`. A target name means nothing to the other stack, so an
    /// explicit pick does not follow the operator across the switch.
    pub fn set_stack(&mut self, id: &str) {
        let next = if id == "ardupilot" { "ardupilot" } else { "px4" };
        if next != self.stack {
            self.board_touched = false;
            self.board_key.clear();
            self.board.clear();
        }
        self.stack = next.into();
        if self.catalog.is_some() {
            self.render_releases();
        }
    }

    /// The operator pressed a stack button.
    pub fn pick_stack(&mut self, id: &str) {
        self.stack_touched = true;
        self.set_stack(id);
    }

    /// `renderReleases`: the release stays when it is still on offer.
    fn render_releases(&mut self) {
        let tags: Vec<String> = self.stack_releases().iter().map(|r| r.tag.clone()).collect();
        if tags.is_empty() {
            self.boards.clear();
            self.board.clear();
            self.vendor.clear();
            self.release.clear();
            self.render_boards();
            return;
        }
        let wanted = if tags.contains(&self.release) {
            self.release.clone()
        } else {
            let list = self.stack_releases();
            self.default_release(&list)
        };
        self.release = wanted.clone();
        self.select_release(&wanted);
    }

    /// `selectRelease`: swap the board list to the release's. A board keeps
    /// its key across the releases of one stack, so a chosen or detected board
    /// survives a change.
    pub fn select_release(&mut self, tag: &str) {
        let boards = {
            let list = self.stack_releases();
            list.iter()
                .find(|r| r.tag == tag)
                .or(list.first())
                .map(|r| r.boards.clone())
                .unwrap_or_default()
        };
        self.boards = boards;
        self.render_boards();
    }

    /// The operator picked a release.
    pub fn pick_release(&mut self, tag: &str) {
        self.release = tag.to_string();
        self.select_release(tag);
    }

    /// `renderBoards`: only ever land on a board somebody chose, the
    /// operator's own pick or the one detected on the USB port.
    fn render_boards(&mut self) {
        let wanted = self.preferred_board();
        let found = (!wanted.is_empty())
            .then(|| {
                self.visible_boards()
                    .into_iter()
                    .find(|b| b.key_of() == wanted)
                    .map(|b| (b.name.clone(), b.vendor_of().to_string()))
            })
            .flatten();
        match found {
            Some((name, vendor)) => {
                self.board = name;
                self.vendor = vendor;
            }
            None => self.board.clear(),
        }
        self.render_vendors();
    }

    fn render_vendors(&mut self) {
        if self.boards.is_empty() || !self.visible_vendors().contains(&self.vendor) {
            self.vendor.clear();
        }
        self.render_board_options();
    }

    /// A select adopts its first option the moment it is filled, so the first
    /// option is a placeholder and never a board: nothing is flashable until
    /// somebody picks it.
    fn render_board_options(&mut self) {
        let keep = !self.vendor.is_empty()
            && self
                .boards_of_vendor(&self.vendor.clone())
                .iter()
                .any(|b| b.name == self.board);
        let name = if keep { self.board.clone() } else { String::new() };
        self.select_board(&name);
    }

    /// `selectBoard`.
    fn select_board(&mut self, name: &str) {
        self.board = name.to_string();
        if let Some(key) = self
            .boards
            .iter()
            .find(|b| !name.is_empty() && b.name == name)
            .map(|b| b.key_of().to_string())
        {
            self.board_key = key;
        }
    }

    /// Changing manufacturer drops the board: the one selected belongs to the
    /// vendor that is no longer shown.
    pub fn pick_vendor(&mut self, vendor: &str) {
        self.vendor = vendor.to_string();
        self.board.clear();
        self.render_board_options();
    }

    /// Once the operator picks, detection stops moving the selection.
    pub fn pick_board(&mut self, name: &str) {
        self.board_touched = true;
        self.select_board(name);
    }

    pub fn toggle_variants(&mut self) {
        self.show_variants = !self.show_variants;
        self.render_boards();
    }

    /// The release dropdown's rows: value and label. The label is the
    /// release's own name for ArduPilot, because its tag is a path that routes
    /// the request and was never meant to be read.
    pub fn release_rows(&self) -> Vec<(String, String)> {
        self.stack_releases()
            .iter()
            .map(|r| {
                let label = if r.stack == "ardupilot" {
                    if r.name.is_empty() {
                        r.tag.clone()
                    } else {
                        r.name.clone()
                    }
                } else {
                    format!("{}{}", r.tag, if r.prerelease { "  \u{b7}  pre-release" } else { "" })
                };
                (r.tag.clone(), label)
            })
            .collect()
    }

    /// The manufacturer dropdown's rows after the prompt: `Name  (count)`.
    pub fn vendor_rows(&self) -> Vec<(String, String)> {
        self.visible_vendors()
            .into_iter()
            .map(|v| {
                let n = self.boards_of_vendor(&v).len();
                let label = format!("{v}  ({n})");
                (v, label)
            })
            .collect()
    }

    /// The board dropdown's rows for the chosen manufacturer.
    pub fn board_rows(&self) -> Vec<(String, String)> {
        if self.vendor.is_empty() {
            return Vec::new();
        }
        self.boards_of_vendor(&self.vendor)
            .into_iter()
            .map(|b| (b.name.clone(), board_option_label(b)))
            .collect()
    }

    /// The selected board.
    pub fn selected(&self) -> Option<&Board> {
        (!self.board.is_empty())
            .then(|| self.boards.iter().find(|b| b.name == self.board))
            .flatten()
    }

    /// `renderBoardCount`: "150 targets from 24 manufacturers, 1 already
    /// downloaded", the answer neither dropdown can show.
    pub fn board_count(&self) -> String {
        if self.boards.is_empty() {
            return String::new();
        }
        let shown = self.visible_boards().len();
        let vendors = self.visible_vendors().len();
        let downloaded = self.boards.iter().filter(|b| b.cached).count();
        let targets = if shown == self.boards.len() {
            format!("{} targets", self.boards.len())
        } else {
            format!("{shown} of {} targets", self.boards.len())
        };
        let mut parts = vec![format!(
            "{targets} from {vendors} {}",
            if vendors == 1 { "manufacturer" } else { "manufacturers" }
        )];
        if downloaded > 0 {
            parts.push(format!("{downloaded} already downloaded"));
        }
        parts.join(" \u{b7} ")
    }

    /// `renderDetected`: what the backend recognised, and how.
    pub fn detected_text(&self) -> Option<String> {
        let d = self.detected()?;
        let name = if d.label.is_empty() { &d.name } else { &d.label };
        let source = if d.source.is_empty() {
            String::new()
        } else {
            format!(" ({})", d.source)
        };
        Some(format!(
            "Detected {name} on the USB port{source}. Change it below if that is not your board."
        ))
    }

    /// `renderCatalogNote`: where the list came from, or what to do about
    /// there not being one. The second value is true for a warning.
    pub fn catalog_note(&self) -> (String, bool) {
        let Some(c) = self.catalog.as_ref() else {
            return (String::new(), false);
        };
        if self.stack_releases().is_empty() {
            let name = if self.stack == "ardupilot" { "ArduPilot" } else { "PX4" };
            let ext = if self.stack == "ardupilot" { ".apj" } else { ".px4" };
            let lead = if c.error.is_empty() {
                format!("No {name} releases downloaded yet. ")
            } else {
                format!("{}. ", c.error)
            };
            return (
                format!("{lead}Press Refresh once this machine is online, or flash a local {ext} file instead."),
                true,
            );
        }
        if c.error.is_empty() {
            let dir = if c.dir.is_empty() {
                "the firmware folder"
            } else {
                &c.dir
            };
            (
                format!("Images are cached in {dir}, so a repeat flash needs no network."),
                false,
            )
        } else {
            (format!("{}. Showing the cached release list.", c.error), true)
        }
    }

    /// Whether a release and a board are chosen.
    pub fn have_image(&self) -> bool {
        !self.release.is_empty() && !self.board.is_empty()
    }
}

/// Where the image comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Catalog,
    File,
}

/// `transportLabel`.
pub fn transport_label(t: &str) -> &'static str {
    match t {
        "usb" => "USB (direct)",
        "sik" => "SiK Radio",
        "udp" => "UDP",
        "tcp" => "TCP",
        _ => "Unknown",
    }
}

/// `GET /api/firmware/status`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub state: String,
    pub transport: String,
    pub device: String,
    pub can_flash: bool,
    pub armed: bool,
    pub bootloader: String,
    pub progress: f64,
    pub message: String,
}

impl Status {
    pub fn from_json(v: &Value) -> Status {
        Status {
            state: text(v, "state"),
            transport: text(v, "transport"),
            device: text(v, "device"),
            can_flash: flag(v, "can_flash"),
            armed: flag(v, "armed"),
            bootloader: text(v, "bootloader"),
            progress: v.get("progress").and_then(Value::as_f64).unwrap_or(0.0),
            message: text(v, "message"),
        }
    }

    pub fn busy(&self) -> bool {
        self.state == "downloading" || self.state == "flashing"
    }

    /// The Transport row: a board waiting in its bootloader has no link, so
    /// no transport, and "Unknown" would be wrong.
    pub fn transport_row(&self) -> String {
        if !self.bootloader.is_empty() {
            "USB bootloader".into()
        } else {
            transport_label(if self.transport.is_empty() {
                "unknown"
            } else {
                &self.transport
            })
            .into()
        }
    }

    pub fn device_row(&self) -> String {
        if !self.bootloader.is_empty() {
            self.bootloader.clone()
        } else if !self.device.is_empty() {
            self.device.clone()
        } else {
            "\u{2014}".into()
        }
    }

    /// The amber text when flashing is not allowed.
    pub fn gate_text(&self) -> String {
        let t = if self.transport.is_empty() {
            "unknown"
        } else {
            &self.transport
        };
        format!(
            "Firmware flash requires a direct USB connection to the flight controller. SiK radio, UDP, and TCP cannot be used for flashing. (Current transport: {}.)",
            transport_label(t)
        )
    }

    /// Armed by the snapshot or by the live telemetry flag: either says so.
    pub fn armed_now(&self, telemetry_armed: bool) -> bool {
        self.armed || telemetry_armed
    }

    /// `progress` as the bar's share, 0 to 100.
    pub fn percent(&self) -> u32 {
        percent(self.progress)
    }

    /// The label under the bar.
    pub fn progress_label(&self) -> String {
        let pct = self.percent();
        if self.busy() {
            let fallback = if self.state == "downloading" {
                "Downloading\u{2026}"
            } else {
                "Flashing\u{2026}"
            };
            let m = if self.message.is_empty() {
                fallback
            } else {
                &self.message
            };
            format!("{pct}% \u{b7} {m}")
        } else {
            self.message.clone()
        }
    }
}

/// `Math.max(0, Math.min(100, Math.round(v)))`.
pub fn percent(v: f64) -> u32 {
    js_round(v).clamp(0.0, 100.0) as u32
}

/// `recomputeUploadGate`: the button is enabled only when the backend says
/// `can_flash`, nothing is running, the vehicle is not armed (the live flag is
/// the real time signal, the snapshot a fallback) and an image is chosen.
pub fn can_start(status: &Status, telemetry_armed: bool, have_image: bool) -> bool {
    status.can_flash && !status.busy() && !status.armed_now(telemetry_armed) && have_image
}

/// The button says what it will do: a catalogue flash downloads first, and a
/// button labelled Flash that spends two minutes fetching reads as a hang.
pub fn flash_label(source: Source) -> &'static str {
    match source {
        Source::File => "Flash Firmware",
        Source::Catalog => "Download & Flash",
    }
}

/// The body of `POST /api/firmware/flash`: a release and a board by name,
/// never a URL.
pub fn flash_body(release: &str, board: &str) -> Value {
    json!({"release": release, "board": board})
}

/// `encodeURIComponent`.
pub fn uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The path of `POST /api/firmware/upload`.
pub fn upload_path(file_name: &str) -> String {
    if file_name.is_empty() {
        "/api/firmware/upload".into()
    } else {
        format!("/api/firmware/upload?name={}", uri_component(file_name))
    }
}

/// The last component of a path typed or pasted by the operator.
pub fn file_name_of(path: &str) -> String {
    path.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// A flash log line: what it said and the state it was said in.
#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub level: String,
    pub text: String,
}

/// One event of the `firmware` topic.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub percent: u32,
    pub label: String,
    pub message: String,
    pub state: String,
}

impl Progress {
    pub fn from_json(d: &Value) -> Progress {
        let state = text(d, "state");
        let message = text(d, "message");
        let pct = percent(d.get("percent").and_then(Value::as_f64).unwrap_or(0.0));
        let tail = if message.is_empty() { &state } else { &message };
        Progress {
            percent: pct,
            label: format!("{pct}% \u{b7} {tail}"),
            message,
            state,
        }
    }

    /// Cancel follows the stream's state, the live source while flashing.
    pub fn cancellable(&self) -> bool {
        self.state == "flashing" || self.state == "downloading"
    }

    pub fn terminal(&self) -> bool {
        matches!(self.state.as_str(), "done" | "failed" | "cancelled")
    }
}

/// The flash log, capped like the guidance list.
#[derive(Debug, Clone, Default)]
pub struct Log {
    pub lines: Vec<LogLine>,
}

impl Log {
    pub fn push(&mut self, message: &str, level: &str) {
        if message.is_empty() {
            return;
        }
        self.lines.push(LogLine {
            level: level.into(),
            text: message.into(),
        });
        if self.lines.len() > LOG_CAP {
            self.lines.remove(0);
        }
    }
}

/// What a terminal state says to the notification board: its level and text.
pub fn terminal_note(status: &Status) -> Option<(&'static str, String)> {
    let say = |fallback: &str| {
        if status.message.is_empty() {
            fallback.to_string()
        } else {
            status.message.clone()
        }
    };
    match status.state.as_str() {
        "done" => Some(("info", say("Firmware flash complete"))),
        "failed" => Some(("critical", say("Firmware flash failed"))),
        "cancelled" => Some(("warning", say("Firmware flash cancelled"))),
        _ => None,
    }
}

/// The telemetry fields the page follows: a status refetch happens only when
/// one of them changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Signature {
    pub connected: bool,
    pub armed: bool,
    pub bootloader: String,
}

impl Signature {
    /// A controller plugged in with no firmware appears as a bootloader port,
    /// not as a link, so that is part of the signature too.
    pub fn of(state: &Value) -> Signature {
        Signature {
            connected: flag(state, "connected"),
            armed: flag(state, "armed"),
            bootloader: state
                .get("link_bootloader")
                .map(|b| text(b, "device"))
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(name: &str, vendor: &str, variant: &str) -> Value {
        json!({"name": name, "key": name, "label": name, "title": name, "vendor": vendor,
               "variant": variant, "size": 1_500_000, "cached": false, "peripheral": false})
    }

    fn px4_release(tag: &str, pre: bool, boards: Vec<Value>) -> Value {
        json!({"tag": tag, "name": tag, "prerelease": pre, "vendor": "px4", "boards": boards})
    }

    fn catalog() -> Catalog {
        Catalog::from_json(&json!({
            "releases": [
                px4_release("v1.18.0-beta1", true, vec![board("px4_fmu-v6x_default.px4", "PX4", "default")]),
                px4_release("v1.17.0", false, vec![
                    board("px4_fmu-v6x_default.px4", "PX4", "default"),
                    board("px4_fmu-v6x_bootloader.px4", "PX4", "bootloader"),
                    board("holybro_kakuteh7_default.px4", "Holybro", "default"),
                    board("holybro_pix32v6_default.px4", "Holybro", "default"),
                ]),
                {"tag": "ardupilot:Copter/stable", "name": "ArduCopter stable", "vendor": "ardupilot",
                 "vehicle": "copter", "prerelease": false, "boards": [
                    {"name": "CubeOrange", "key": "CubeOrange", "title": "Cube Orange", "vendor": "CubePilot", "variant": "default"}]},
                {"tag": "ardupilot:Plane/stable", "name": "ArduPlane stable", "vendor": "ardupilot",
                 "vehicle": "plane", "prerelease": false, "boards": [
                    {"name": "CubeOrange", "key": "CubeOrange", "title": "Cube Orange", "vendor": "CubePilot", "variant": "default"}]},
            ],
            "error": "", "dir": "/home/a/firmware",
            "detected": {"px4": null, "ardupilot": null},
            "suggested": {"stack": "", "vehicle": ""},
        }))
    }

    #[test]
    fn a_pre_release_is_never_the_one_a_list_opens_on() {
        let mut p = Picker::new();
        p.load(catalog());
        assert_eq!(p.release, "v1.17.0");
        let rows = p.release_rows();
        assert_eq!(
            rows[0],
            ("v1.18.0-beta1".into(), "v1.18.0-beta1  \u{b7}  pre-release".into())
        );
    }

    #[test]
    fn nothing_is_selected_until_somebody_picks_it() {
        let mut p = Picker::new();
        p.load(catalog());
        assert_eq!(p.board, "");
        assert_eq!(p.vendor, "");
        assert!(!p.have_image());
        assert!(p.board_rows().is_empty());
        p.pick_vendor("Holybro");
        assert_eq!(p.board, "");
        assert_eq!(p.board_rows().len(), 2);
        p.pick_board("holybro_kakuteh7_default.px4");
        assert!(p.have_image());
        assert_eq!(p.board_key, "holybro_kakuteh7_default.px4");
    }

    #[test]
    fn developer_builds_are_set_aside_until_asked_for() {
        let mut p = Picker::new();
        p.load(catalog());
        assert_eq!(p.visible_boards().len(), 3);
        assert_eq!(p.board_count(), "3 of 4 targets from 2 manufacturers");
        p.toggle_variants();
        assert_eq!(p.visible_boards().len(), 4);
        assert_eq!(p.board_count(), "4 targets from 2 manufacturers");
    }

    #[test]
    fn the_selected_board_stays_reachable_when_developer_builds_are_hidden_again() {
        let mut p = Picker::new();
        p.load(catalog());
        p.toggle_variants();
        p.pick_vendor("PX4");
        p.pick_board("px4_fmu-v6x_bootloader.px4");
        p.show_variants = false;
        assert_eq!(p.visible_boards().len(), 4);
        assert_eq!(p.board, "px4_fmu-v6x_bootloader.px4");
    }

    #[test]
    fn px4_leads_and_the_rest_follow_alphabetically() {
        let mut p = Picker::new();
        p.load(catalog());
        assert_eq!(p.visible_vendors(), ["PX4", "Holybro"]);
        let rows = p.vendor_rows();
        assert_eq!(rows[0].1, "PX4  (1)");
        assert_eq!(rows[1].1, "Holybro  (2)");
    }

    #[test]
    fn a_picked_board_survives_a_change_of_release_by_its_key() {
        let mut p = Picker::new();
        p.load(catalog());
        p.pick_vendor("PX4");
        p.pick_board("px4_fmu-v6x_default.px4");
        p.pick_release("v1.18.0-beta1");
        assert_eq!(p.board, "px4_fmu-v6x_default.px4");
        assert_eq!(p.vendor, "PX4");
        p.pick_release("v1.17.0");
        assert_eq!(p.board, "px4_fmu-v6x_default.px4");
    }

    #[test]
    fn a_change_of_manufacturer_drops_the_board() {
        let mut p = Picker::new();
        p.load(catalog());
        p.pick_vendor("Holybro");
        p.pick_board("holybro_kakuteh7_default.px4");
        p.pick_vendor("PX4");
        assert_eq!(p.board, "");
        assert!(!p.have_image());
    }

    #[test]
    fn a_detected_board_is_preselected_with_its_vendor_first() {
        let mut c = catalog();
        c.detected_px4 = Some(Detected {
            key: "holybro_pix32v6_default.px4".into(),
            name: "holybro_pix32v6_default.px4".into(),
            label: "Holybro Pix32 v6".into(),
            source: "USB id".into(),
        });
        let mut p = Picker::new();
        p.load(c);
        assert_eq!(p.board, "holybro_pix32v6_default.px4");
        assert_eq!(p.vendor, "Holybro");
        assert_eq!(p.visible_vendors(), ["Holybro", "PX4"]);
        assert_eq!(
            p.detected_text().unwrap(),
            "Detected Holybro Pix32 v6 on the USB port (USB id). Change it below if that is not your board."
        );
    }

    #[test]
    fn once_the_operator_picks_detection_stops_moving_the_selection() {
        let mut c = catalog();
        c.detected_px4 = Some(Detected {
            key: "holybro_pix32v6_default.px4".into(),
            ..Detected::default()
        });
        let mut p = Picker::new();
        p.load(c);
        p.pick_vendor("Holybro");
        p.pick_board("holybro_kakuteh7_default.px4");
        p.pick_release("v1.18.0-beta1");
        p.pick_release("v1.17.0");
        assert_eq!(p.board, "holybro_kakuteh7_default.px4");
    }

    #[test]
    fn the_stack_follows_the_aircraft_until_the_operator_chooses() {
        let mut c = catalog();
        c.suggested_stack = "ardupilot".into();
        c.suggested_vehicle = "plane".into();
        let mut p = Picker::new();
        p.load(c.clone());
        assert_eq!(p.stack, "ardupilot");
        assert_eq!(p.release, "ardupilot:Plane/stable");
        assert_eq!(p.release_rows()[0].1, "ArduCopter stable");
        p.pick_stack("px4");
        p.load(c);
        assert_eq!(p.stack, "px4");
    }

    #[test]
    fn switching_stack_drops_the_pick() {
        let mut p = Picker::new();
        p.load(catalog());
        p.pick_vendor("PX4");
        p.pick_board("px4_fmu-v6x_default.px4");
        p.pick_stack("ardupilot");
        assert_eq!(p.board, "");
        assert!(!p.board_touched);
        assert_eq!(p.release, "ardupilot:Copter/stable");
        assert!(!p.have_image());
    }

    #[test]
    fn a_stack_with_no_releases_says_what_to_do() {
        let mut c = catalog();
        c.releases.retain(|r| r.stack == "px4");
        let mut p = Picker::new();
        p.load(c.clone());
        p.pick_stack("ardupilot");
        assert!(p.release_rows().is_empty());
        assert_eq!(
            p.catalog_note(),
            (
                "No ArduPilot releases downloaded yet. Press Refresh once this machine is online, or flash a local .apj file instead."
                    .to_string(),
                true
            )
        );
        c.error = "could not reach the PX4 release server".into();
        p.catalog = Some(c);
        assert!(p
            .catalog_note()
            .0
            .starts_with("could not reach the PX4 release server. Press Refresh"));
    }

    #[test]
    fn the_catalog_note_says_where_images_are_cached() {
        let mut p = Picker::new();
        p.load(catalog());
        assert_eq!(
            p.catalog_note(),
            (
                "Images are cached in /home/a/firmware, so a repeat flash needs no network.".to_string(),
                false
            )
        );
        let mut c = catalog();
        c.error = "offline".into();
        p.catalog = Some(c);
        assert_eq!(
            p.catalog_note(),
            ("offline. Showing the cached release list.".to_string(), true)
        );
    }

    #[test]
    fn a_board_is_labelled_with_what_a_closed_select_cannot_show() {
        let mut b = Board::from_json(&board("x_y_bootloader.px4", "X", "bootloader"));
        b.title = "Y".into();
        b.peripheral = true;
        b.cached = true;
        assert_eq!(
            board_option_label(&b),
            "Y  \u{b7}  bootloader  \u{b7}  peripheral  \u{b7}  downloaded"
        );
    }

    #[test]
    fn sizes_are_kilobytes_then_megabytes() {
        assert_eq!(format_size(0.0), "");
        assert_eq!(format_size(f64::NAN), "");
        assert_eq!(format_size(500_000.0), "488 KB");
        assert_eq!(format_size(1_500_000.0), "1.4 MB");
    }

    #[test]
    fn names_sort_the_way_localecompare_does() {
        assert_eq!(locale_cmp("apple", "Banana"), Ordering::Less);
        assert_eq!(locale_cmp("a", "A"), Ordering::Less);
        assert_eq!(locale_cmp("same", "same"), Ordering::Equal);
    }

    fn status(v: Value) -> Status {
        Status::from_json(&v)
    }

    #[test]
    fn flashing_needs_the_backend_the_vehicle_and_an_image() {
        let ok = status(json!({"state": "idle", "can_flash": true, "transport": "usb"}));
        assert!(can_start(&ok, false, true));
        assert!(!can_start(&ok, false, false));
        assert!(!can_start(&ok, true, true));
        assert!(!can_start(
            &status(json!({"state": "idle", "can_flash": false})),
            false,
            true
        ));
        assert!(!can_start(
            &status(json!({"state": "flashing", "can_flash": true})),
            false,
            true
        ));
        assert!(!can_start(
            &status(json!({"state": "idle", "can_flash": true, "armed": true})),
            false,
            true
        ));
    }

    #[test]
    fn the_button_says_what_it_will_do() {
        assert_eq!(flash_label(Source::Catalog), "Download & Flash");
        assert_eq!(flash_label(Source::File), "Flash Firmware");
    }

    #[test]
    fn a_board_in_its_bootloader_is_not_an_unknown_transport() {
        let s = status(json!({"bootloader": "/dev/ttyACM0", "transport": "unknown"}));
        assert_eq!(s.transport_row(), "USB bootloader");
        assert_eq!(s.device_row(), "/dev/ttyACM0");
        let s = status(json!({"transport": "sik", "device": ""}));
        assert_eq!(s.transport_row(), "SiK Radio");
        assert_eq!(s.device_row(), "\u{2014}");
        assert_eq!(status(json!({})).transport_row(), "Unknown");
    }

    #[test]
    fn the_gate_names_the_transport_in_the_way_the_row_does() {
        let s = status(json!({"transport": "udp"}));
        assert_eq!(
            s.gate_text(),
            "Firmware flash requires a direct USB connection to the flight controller. SiK radio, UDP, and TCP cannot be used for flashing. (Current transport: UDP.)"
        );
    }

    #[test]
    fn the_label_under_the_bar_follows_the_state() {
        let s = status(json!({"state": "flashing", "progress": 41.6, "message": "Programming"}));
        assert_eq!(s.progress_label(), "42% \u{b7} Programming");
        let s = status(json!({"state": "downloading", "progress": 3}));
        assert_eq!(s.progress_label(), "3% \u{b7} Downloading\u{2026}");
        let s = status(json!({"state": "failed", "message": "Board not found"}));
        assert_eq!(s.progress_label(), "Board not found");
        assert_eq!(status(json!({"state": "idle"})).progress_label(), "");
        assert_eq!(percent(250.0), 100);
        assert_eq!(percent(-4.0), 0);
    }

    #[test]
    fn a_progress_event_drives_the_bar_the_label_and_cancel() {
        let p = Progress::from_json(&json!({"state": "flashing", "percent": 12.4, "message": "Erasing"}));
        assert_eq!((p.percent, p.label.as_str()), (12, "12% \u{b7} Erasing"));
        assert!(p.cancellable());
        assert!(!p.terminal());
        let p = Progress::from_json(&json!({"state": "done", "percent": 100}));
        assert_eq!(p.label, "100% \u{b7} done");
        assert!(p.terminal());
        assert!(!p.cancellable());
    }

    #[test]
    fn the_log_keeps_fifty_lines() {
        let mut l = Log::default();
        l.push("", "info");
        assert!(l.lines.is_empty());
        for i in 0..60 {
            l.push(&format!("line {i}"), "info");
        }
        assert_eq!(l.lines.len(), LOG_CAP);
        assert_eq!(l.lines[0].text, "line 10");
    }

    #[test]
    fn a_finished_flash_is_announced_at_its_level() {
        let n = |v: Value| terminal_note(&status(v));
        assert_eq!(
            n(json!({"state": "done"})),
            Some(("info", "Firmware flash complete".into()))
        );
        assert_eq!(
            n(json!({"state": "failed", "message": "No bootloader"})),
            Some(("critical", "No bootloader".into()))
        );
        assert_eq!(
            n(json!({"state": "cancelled"})),
            Some(("warning", "Firmware flash cancelled".into()))
        );
        assert_eq!(n(json!({"state": "idle"})), None);
    }

    #[test]
    fn the_request_names_a_release_and_a_board_and_never_a_url() {
        assert_eq!(
            flash_body("v1.17.0", "holybro_kakuteh7_default.px4"),
            json!({"release": "v1.17.0", "board": "holybro_kakuteh7_default.px4"})
        );
    }

    #[test]
    fn an_upload_names_the_file_as_encodeuricomponent_does() {
        assert_eq!(
            upload_path("px4 fmu+v6.px4"),
            "/api/firmware/upload?name=px4%20fmu%2Bv6.px4"
        );
        assert_eq!(upload_path(""), "/api/firmware/upload");
        assert_eq!(file_name_of("/home/a/b/fw.apj"), "fw.apj");
        assert_eq!(file_name_of("\"C:\\Users\\a\\fw.px4\""), "fw.px4");
        assert_eq!(file_name_of("  fw.bin "), "fw.bin");
    }

    #[test]
    fn the_status_is_refetched_only_when_the_link_changes() {
        let a = Signature::of(&json!({"connected": true, "armed": false}));
        let b = Signature::of(&json!({"connected": true, "armed": false, "altitude": 3}));
        assert_eq!(a, b);
        let c = Signature::of(&json!({"connected": false, "link_bootloader": {"device": "/dev/ttyACM0"}}));
        assert_ne!(a, c);
        assert_eq!(c.bootloader, "/dev/ttyACM0");
    }
}
