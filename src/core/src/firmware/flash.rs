//! Firmware flash orchestration (`flash_service.py`, `corvus_link.flash`).
//!
//! The gate and the flow for flashing PX4 firmware over a **direct USB** link
//! only: a SiK radio, UDP and TCP are refused (the uploader must own the serial
//! device, and flashing over a telemetry radio is unsafe). The flow runs on one
//! worker thread:
//!
//! 1. reboot the flight controller into its USB bootloader (refused while armed),
//! 2. stop the MAVLink bridge so the uploader can own the serial device,
//! 3. erase, program, verify and reset through [`FirmwareUploader`],
//! 4. point the bridge back at the (newly flashed) controller. Skipped during
//!    shutdown, so a teardown never respawns the bridge.
//!
//! A controller with no firmware has no MAVLink to reboot, but it already sits
//! in its bootloader. When no vehicle is linked and exactly one board shows up
//! in its bootloader, step 1 is skipped and that port is flashed directly.

use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::image::parse_firmware_image;
use super::uploader::{FirmwareUploader, Progress, UploaderEnv};
use crate::protocol::common::Event;
use crate::serial_ports::{is_bootloader_port, PortRow};

/// The gate text when the link is not a direct USB connection to the controller.
pub const NON_USB_GATE_MESSAGE: &str = "Firmware flash requires a direct USB connection to the flight controller \
(current link: {transport}). SiK radio, UDP, and TCP are not permitted.";

/// States in which the service is busy and must refuse a second job. Downloading
/// counts: the operator has committed to a flash.
pub const BUSY_STATES: [&str; 2] = ["downloading", "flashing"];

/// What the service asks of the vehicle link and the state store.
pub trait FlashHost: Send + Sync {
    /// `usb`, `sik`, `udp`, `tcp` or `unknown`.
    fn transport(&self) -> String;
    fn is_direct_usb(&self) -> bool;
    fn connection_string(&self) -> String;
    fn serial_device(&self) -> String;
    fn reboot_to_bootloader(&self) -> bool;
    /// Releases the link so the uploader can own the serial device.
    fn stop(&self) -> Result<(), String>;
    fn set_connection(&self, conn: &str) -> Result<(), String>;
    fn start(&self) -> Result<(), String>;
    fn list_serial_ports(&self) -> Result<Vec<PortRow>, String>;
    /// The state store's `connected` and `armed`.
    fn connected(&self) -> bool;
    fn armed(&self) -> bool;
    /// What identifies the connected board for the firmware picker: the USB
    /// descriptor of the port and the ids AUTOPILOT_VERSION reported.
    fn board_identity(&self) -> Value {
        json!({})
    }
    /// `px4`, `ardupilot` or another stack's name.
    fn stack(&self) -> String {
        String::new()
    }
    fn vehicle_type_id(&self) -> i64 {
        0
    }
}

/// Why a download failed: a refusal with words for the operator, or something
/// nobody expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    Value(String),
    Crash(String),
}

/// Where release images come from: the firmware catalogue.
pub trait ImageSource: Send + Sync {
    fn resolve(&self, release_tag: &str, board_name: &str) -> Option<Value>;
    fn cached_path(&self, name: &str) -> Option<String>;
    fn download(
        &self,
        entry: &Value,
        on_progress: Option<&dyn Fn(i64, i64)>,
        cancel: &Event,
    ) -> Result<Vec<u8>, DownloadError>;
}

impl ImageSource for super::catalog::FirmwareCatalog {
    fn resolve(&self, release_tag: &str, board_name: &str) -> Option<Value> {
        super::catalog::FirmwareCatalog::resolve(self, release_tag, board_name)
    }

    fn cached_path(&self, name: &str) -> Option<String> {
        super::catalog::FirmwareCatalog::cached_path(self, name)
    }

    fn download(
        &self,
        entry: &Value,
        on_progress: Option<&dyn Fn(i64, i64)>,
        cancel: &Event,
    ) -> Result<Vec<u8>, DownloadError> {
        super::catalog::FirmwareCatalog::download(self, entry, on_progress, Some(cancel)).map_err(DownloadError::Value)
    }
}

type Listener = Arc<dyn Fn(&Value) + Send + Sync>;
/// A fetched image and the board it is built for; `None` when the job ended early.
type Fetched = Option<(Vec<u8>, Option<i64>)>;

struct Inner {
    state: String,
    percent: i64,
    message: String,
    last_error: String,
    listeners: Vec<(u64, Listener)>,
    next_listener: u64,
    uploader: Option<Arc<FirmwareUploader>>,
    worker: Option<JoinHandle<()>>,
    shutting_down: bool,
}

/// Orchestrates firmware flashing over a direct USB link only.
pub struct FlashService {
    host: Arc<dyn FlashHost>,
    catalog: Option<Arc<dyn ImageSource>>,
    env: Arc<dyn UploaderEnv>,
    me: Weak<FlashService>,
    inner: Mutex<Inner>,
    cancel: Arc<Event>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn entry(state: &str, percent: i64, message: &str) -> Value {
    json!({"state": state, "percent": percent, "message": message})
}

impl FlashService {
    pub fn new(
        host: Arc<dyn FlashHost>,
        catalog: Option<Arc<dyn ImageSource>>,
        env: Arc<dyn UploaderEnv>,
    ) -> Arc<FlashService> {
        Arc::new_cyclic(|me| FlashService {
            host,
            catalog,
            env,
            me: me.clone(),
            inner: Mutex::new(Inner {
                state: "idle".into(),
                percent: 0,
                message: String::new(),
                last_error: String::new(),
                listeners: Vec::new(),
                next_listener: 1,
                uploader: None,
                worker: None,
                shutting_down: false,
            }),
            cancel: Arc::new(Event::new()),
        })
    }

    /// Why the last refused call was refused.
    pub fn last_error(&self) -> String {
        lock(&self.inner).last_error.clone()
    }

    fn refuse(&self, text: &str) -> bool {
        lock(&self.inner).last_error = text.to_string();
        false
    }

    /// The link the service flashes through.
    pub fn host(&self) -> &Arc<dyn FlashHost> {
        &self.host
    }

    pub fn has_catalog(&self) -> bool {
        self.catalog.is_some()
    }

    fn busy(g: &Inner) -> bool {
        BUSY_STATES.contains(&g.state.as_str())
    }

    // ------------------------------------------ a board in its bootloader

    /// Serial ports whose descriptor says "flight controller in its bootloader".
    pub fn bootloader_ports(&self) -> Vec<PortRow> {
        match self.host.list_serial_ports() {
            Ok(rows) => rows
                .into_iter()
                .filter(|r| is_bootloader_port(&r.device, &r.hwid, &r.description))
                .collect(),
            Err(e) => {
                self.env.log_exc("DEBUG", "bootloader port enumeration failed", &e);
                Vec::new()
            }
        }
    }

    /// The one bootloader port that may be flashed without a link, or an empty
    /// string. Only while no vehicle is linked, and only when there is exactly
    /// one: with two boards waiting, guessing is how the wrong one gets erased.
    pub fn bootloader_device(&self) -> String {
        if self.host.connected() {
            return String::new();
        }
        let ports = self.bootloader_ports();
        if ports.len() != 1 {
            return String::new();
        }
        ports[0].device.clone()
    }

    // ------------------------------------------------------------- status

    /// Live status from the bridge, the store and the flash state.
    pub fn status(&self) -> Value {
        let transport = self.host.transport();
        let device = self.host.serial_device();
        let armed = self.host.armed();
        let bootloader = self.bootloader_device();
        let (state, percent, message) = {
            let g = lock(&self.inner);
            (g.state.clone(), g.percent, g.message.clone())
        };
        let busy = BUSY_STATES.contains(&state.as_str());
        json!({
            "state": state,
            "transport": transport,
            "device": device,
            // The conjunction: a USB link or a board waiting, idle, and disarmed.
            "can_flash": !busy && (!bootloader.is_empty() || (self.host.is_direct_usb() && !armed)),
            "bootloader": bootloader,
            "armed": armed,
            "progress": percent,
            "message": message,
        })
    }

    fn gate_message(&self) -> String {
        NON_USB_GATE_MESSAGE.replace("{transport}", &self.host.transport())
    }

    /// The refusal reason, or `None` when a flash may start. Shared by every
    /// entry point so a download then flash is refused on the same terms as a
    /// direct upload.
    fn gate(&self) -> Option<String> {
        if self.bootloader_device().is_empty() {
            if !self.host.is_direct_usb() {
                return Some(self.gate_message());
            }
            if self.host.armed() {
                return Some("cannot flash while armed".into());
            }
        }
        if Self::busy(&lock(&self.inner)) {
            return Some("flash already in progress".into());
        }
        None
    }

    /// The gate minus the busy check: this job is the busy one. A flash that
    /// started on a board in its bootloader resumes only while that same board
    /// is still the one waiting there.
    fn gate_for_resume(&self, bootloader: &str) -> Option<String> {
        if !bootloader.is_empty() {
            if self.bootloader_device() != bootloader {
                return Some("the controller left its bootloader".into());
            }
            return None;
        }
        if !self.host.is_direct_usb() {
            return Some(self.gate_message());
        }
        if self.host.armed() {
            return Some("cannot flash while armed".into());
        }
        None
    }

    // ------------------------------------------------------------- starts

    /// Gate, parse the image, then start the flash worker. False on refusal,
    /// with the reason in [`last_error`](Self::last_error).
    pub fn start(&self, firmware: &[u8]) -> bool {
        if let Some(refusal) = self.gate() {
            return self.refuse(&refusal);
        }
        let parsed = match parse_firmware_image(firmware) {
            Ok(p) => p,
            Err(e) => return self.refuse(&e),
        };
        if parsed.image.is_empty() {
            return self.refuse("empty firmware image");
        }
        let preparing = "Preparing to flash\u{2026}";
        {
            let mut g = lock(&self.inner);
            if g.shutting_down {
                g.last_error = "flash service is shutting down".into();
                return false;
            }
            if Self::busy(&g) {
                g.last_error = "flash already in progress".into();
                return false;
            }
            let conn = self.host.connection_string();
            let bootloader = self.bootloader_device();
            self.cancel.clear();
            g.state = "flashing".into();
            g.percent = 0;
            g.message = preparing.into();
            let Some(me) = self.me.upgrade() else {
                return false;
            };
            let (image, board_id) = (parsed.image, parsed.board_id);
            g.worker = std::thread::Builder::new()
                .name("firmware-flash".into())
                .spawn(move || me.run(&conn, &image, board_id, &bootloader))
                .ok();
        }
        self.notify(&entry("flashing", 0, preparing));
        true
    }

    /// Downloads one catalogue image, then flashes it. The client names a
    /// release and a board, never a URL: the target is resolved from the
    /// backend's own catalogue. Download and flash are one job on one worker
    /// with one progress stream and one cancel; the gate is re-checked after
    /// the download, because a download that finished into a vehicle that has
    /// since armed must not flash.
    pub fn start_release(&self, release_tag: &str, board_name: &str) -> bool {
        let Some(catalog) = &self.catalog else {
            return self.refuse("firmware catalogue unavailable");
        };
        if let Some(refusal) = self.gate() {
            return self.refuse(&refusal);
        }
        let Some(found) = catalog.resolve(release_tag, board_name) else {
            return self.refuse("unknown firmware release or board");
        };
        let name = super::util::py_str(Some(found.get("name").unwrap_or(&json!(""))));
        let message = format!("Downloading {name}\u{2026}");
        {
            let mut g = lock(&self.inner);
            if g.shutting_down {
                g.last_error = "flash service is shutting down".into();
                return false;
            }
            if Self::busy(&g) {
                g.last_error = "flash already in progress".into();
                return false;
            }
            let conn = self.host.connection_string();
            let bootloader = self.bootloader_device();
            self.cancel.clear();
            g.state = "downloading".into();
            g.percent = 0;
            g.message = message.clone();
            let Some(me) = self.me.upgrade() else {
                return false;
            };
            g.worker = std::thread::Builder::new()
                .name("firmware-download".into())
                .spawn(move || me.run_release(&conn, &found, &bootloader))
                .ok();
        }
        self.notify(&entry("downloading", 0, &message));
        true
    }

    /// Download worker: fetch (or reuse the cache), then hand off to `run`.
    fn run_release(&self, conn: &str, found: &Value, bootloader: &str) {
        let Some(catalog) = self.catalog.clone() else {
            return;
        };
        let name = super::util::py_str(Some(found.get("name").unwrap_or(&json!(""))));
        let fetched = (|| -> Result<Fetched, DownloadError> {
            let cached = catalog.cached_path(&name).is_some();
            if cached {
                self.set("downloading", 100, &format!("Using cached {name}"));
            }
            let report = |done: i64, total: i64| {
                let percent = if total > 0 { (done * 100) / total } else { 0 };
                self.set(
                    "downloading",
                    percent.min(99),
                    &format!("Downloading {name}: {} KB", done.div_euclid(1024)),
                );
            };
            let raw = catalog.download(found, if cached { None } else { Some(&report) }, &self.cancel)?;
            if self.cancel.is_set() {
                self.set("cancelled", 0, "Download cancelled");
                return Ok(None);
            }
            let parsed = parse_firmware_image(&raw).map_err(DownloadError::Value)?;
            if parsed.image.is_empty() {
                self.set("failed", 0, "empty firmware image");
                return Ok(None);
            }
            Ok(Some((parsed.image, parsed.board_id)))
        })();
        let (image, board_id) = match fetched {
            Ok(Some(pair)) => pair,
            Ok(None) => return,
            Err(DownloadError::Value(text)) => return self.set("failed", 0, &text),
            Err(DownloadError::Crash(e)) => {
                self.env.log_exc("ERROR", "firmware download worker crashed", &e);
                return self.set("failed", 0, "firmware download failed unexpectedly");
            }
        };
        if let Some(refusal) = self.gate_for_resume(bootloader) {
            return self.set("failed", 0, &refusal);
        }
        self.set("flashing", 0, "Preparing to flash\u{2026}");
        self.run(conn, &image, board_id, bootloader);
    }

    /// Upload worker: reboot, stop the bridge, flash, reconnect the bridge.
    /// `board_id` is the board the image was built for, `bootloader` the port of
    /// a board that is already in its bootloader (nothing to reboot).
    fn run(&self, conn: &str, image: &[u8], board_id: Option<i64>, bootloader: &str) {
        let mut uploader: Option<Arc<FirmwareUploader>> = None;
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !bootloader.is_empty() {
                self.set("flashing", 0, "Controller found in its bootloader\u{2026}");
            } else {
                self.set("flashing", 0, "Rebooting autopilot into bootloader\u{2026}");
                if !self.host.reboot_to_bootloader() {
                    return self.set("failed", 0, "Reboot to bootloader rejected. Is the vehicle armed?");
                }
            }
            if let Err(e) = self.host.stop() {
                self.env.log_exc("DEBUG", "mavlink.stop() during flash raised", &e);
            }
            let device = if bootloader.is_empty() {
                self.host.serial_device()
            } else {
                bootloader.to_string()
            };
            if device.is_empty() {
                return self.set("failed", 0, "no serial device for bootloader");
            }
            let me = self.me.clone();
            let up = Arc::new(FirmwareUploader::new(
                Arc::clone(&self.env),
                Some(Arc::new(move |p: &Progress| {
                    if let Some(svc) = me.upgrade() {
                        svc.on_uploader_progress(p);
                    }
                })),
                Some(Arc::clone(&self.cancel)),
            ));
            uploader = Some(Arc::clone(&up));
            lock(&self.inner).uploader = Some(Arc::clone(&up));
            let ok = up.run(&device, image, board_id);
            if self.cancel.is_set() {
                let percent = lock(&self.inner).percent;
                self.set("cancelled", percent, "Flash cancelled");
            } else if ok {
                self.set("done", 100, "Firmware flashed successfully");
            } else {
                let (percent, message) = {
                    let g = lock(&self.inner);
                    (g.percent, g.message.clone())
                };
                let message = if message.is_empty() {
                    "flash failed".to_string()
                } else {
                    message
                };
                self.set("failed", percent, &message);
            }
        }));
        if crashed.is_err() {
            self.env.log_exc("ERROR", "firmware flash worker crashed", "panic");
            let percent = lock(&self.inner).percent;
            self.set("failed", percent, "flash failed unexpectedly");
        }
        if let Some(up) = &uploader {
            up.shutdown();
            let mut g = lock(&self.inner);
            if g.uploader.as_ref().is_some_and(|u| Arc::ptr_eq(u, up)) {
                g.uploader = None;
            }
        }
        self.reconnect(conn);
    }

    /// Points the MAVLink bridge back at the flashed controller. Skipped during
    /// shutdown (the bridge is being torn down anyway).
    fn reconnect(&self, conn: &str) {
        if lock(&self.inner).shutting_down || conn.is_empty() {
            return;
        }
        let restarted = self.host.set_connection(conn).and_then(|_| self.host.start());
        if let Err(e) = restarted {
            self.env.log_exc("DEBUG", "mavlink reconnect after flash failed", &e);
        }
    }

    // -------------------------------------------- cancel and shutdown

    /// Cancels a running download or flash. False when nothing is running.
    pub fn cancel(&self) -> bool {
        if !Self::busy(&lock(&self.inner)) {
            return false;
        }
        self.cancel.set();
        true
    }

    /// Idempotent teardown: cancels the worker, joins it and resets the state.
    pub fn shutdown(&self) {
        let (uploader, worker) = {
            let mut g = lock(&self.inner);
            g.shutting_down = true;
            self.cancel.set();
            (g.uploader.clone(), g.worker.take())
        };
        if let Some(up) = uploader {
            up.shutdown();
        }
        if let Some(handle) = worker {
            let limit = Instant::now() + Duration::from_secs(3);
            while !handle.is_finished() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        lock(&self.inner).state = "idle".into();
    }

    /// The worker, so a test can wait for it.
    pub fn take_worker(&self) -> Option<JoinHandle<()>> {
        lock(&self.inner).worker.take()
    }

    /// Whether a worker or an uploader is still held.
    pub fn is_settled(&self) -> (bool, bool) {
        let g = lock(&self.inner);
        (g.worker.is_none(), g.uploader.is_none())
    }

    // ---------------------------------------------------------- listeners

    pub fn add_listener(&self, f: impl Fn(&Value) + Send + Sync + 'static) -> u64 {
        let mut g = lock(&self.inner);
        let id = g.next_listener;
        g.next_listener += 1;
        g.listeners.push((id, Arc::new(f)));
        id
    }

    pub fn remove_listener(&self, id: u64) {
        lock(&self.inner).listeners.retain(|(i, _)| *i != id);
    }

    fn notify(&self, event: &Value) {
        let listeners: Vec<Listener> = lock(&self.inner).listeners.iter().map(|(_, f)| Arc::clone(f)).collect();
        for f in listeners {
            f(event);
        }
    }

    /// Updates the flash level state and tells the listeners.
    fn set(&self, state: &str, percent: i64, message: &str) {
        {
            let mut g = lock(&self.inner);
            g.state = state.into();
            g.percent = percent;
            g.message = message.into();
        }
        self.notify(&entry(state, percent, message));
    }

    /// Forwards the uploader's progress: updates the percent and message that
    /// `status` reads, but not the flash level state, which stays `flashing`
    /// until done, failed or cancelled.
    fn on_uploader_progress(&self, p: &Progress) {
        {
            let mut g = lock(&self.inner);
            g.percent = p.percent;
            g.message = p.message.clone();
        }
        self.notify(&entry(&p.state, p.percent, &p.message));
    }
}
