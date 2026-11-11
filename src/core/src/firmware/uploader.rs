//! PX4 USB bootloader uploader (`firmware_uploader.py`, `corvus_link.flash`).
//!
//! Speaks the PX4 NuttX bootloader protocol over a direct USB CDC ACM link and
//! writes a firmware image to the flight controller's flash. The upload runs
//! synchronously in the caller's thread and reports progress through a
//! callback. It is cancellable and never leaks the serial handle.
//!
//! Safety (never bricks the board): on any protocol error or cancellation the
//! uploader reports `failed` or `cancelled` and does **not** send REBOOT,
//! leaving the controller in its bootloader so the operator can retry. REBOOT
//! goes out only after a clean program and verify.
//!
//! The protocol bytes and the CRC are those of PX4/Bootloader `bl.c` and
//! PX4-Autopilot `Tools/px_uploader.py`: `INSYNC` is 0x12 (0x14 is
//! `BAD_SILICON`), and the bootloader CRCs the whole flashable area (the image
//! padded with 0xFF to the board's `fw_maxsize`) with CRC-32 started at 0 and
//! no final XOR.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use super::image::{board_ids_compatible, MAX_FIRMWARE_IMAGE_BYTES};
use crate::protocol::common::Event;

pub const INSYNC: u8 = 0x12;
pub const EOC: u8 = 0x20;
pub const OK: u8 = 0x10;
pub const BAD_SILICON: u8 = 0x14;
pub const GET_SYNC: u8 = 0x21;
pub const GET_DEVICE: u8 = 0x22;
pub const CHIP_ERASE: u8 = 0x23;
pub const PROG_MULTI: u8 = 0x27;
pub const GET_CRC: u8 = 0x29;
pub const GET_CHIP: u8 = 0x2c;
pub const REBOOT: u8 = 0x30;
pub const INFO_BOARD_ID: u8 = 0x02;
pub const INFO_FLASH_SIZE: u8 = 0x04;
/// The bootloader needs `count % 4 == 0`, at most its 256 byte buffer.
pub const PROG_MULTI_MAX: usize = 252;
/// A full chip erase can take several seconds on large flashes.
pub const ERASE_TIMEOUT_S: f64 = 30.0;
/// Short enough that the cancel flag is re-checked promptly during an erase.
pub const READ_TIMEOUT_S: f64 = 1.0;
pub const MAX_BOOTLOADER_FLASH_BYTES: i64 = 64 * 1024 * 1024;
pub const INFO_TIMEOUT_S: f64 = 2.0;
pub const DEFAULT_BAUD: u32 = 115_200;

static CRC32_TABLE: [u32; 256] = crc32_table();

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

/// The bootloader's CRC-32: the reflected table, started at `state`, no final XOR.
pub fn bl_crc32(data: &[u8], state: u32) -> u32 {
    data.iter().fold(state, |crc, &b| {
        CRC32_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8)
    })
}

/// An open serial port.
pub trait SerialIo: Send + Sync {
    /// Up to `n` bytes; waits for the first for the port's read timeout.
    fn read(&self, n: usize) -> io::Result<Vec<u8>>;
    fn write(&self, data: &[u8]) -> io::Result<()>;
    fn flush(&self) -> io::Result<()>;
    fn reset_input(&self) -> io::Result<()>;
    fn close(&self);
}

/// What the uploader takes from the machine it runs on: the clock, the serial
/// ports and the log. A test hands in a scripted bootloader and a fake clock.
pub trait UploaderEnv: Send + Sync {
    fn monotonic(&self) -> f64;
    fn sleep(&self, seconds: f64);
    /// Has the bootloader's serial device re-enumerated yet?
    fn device_present(&self, device: &str) -> bool;
    fn open(&self, device: &str, baud: u32, timeout: f64) -> io::Result<Arc<dyn SerialIo>>;
    fn log(&self, level: &str, message: &str);
    /// A log line with an exception behind it (`exc_info` in Python).
    fn log_exc(&self, level: &str, message: &str, detail: &str);
}

/// One progress report: `{state, percent, message}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub state: String,
    pub percent: i64,
    pub message: String,
}

pub type ProgressFn = Arc<dyn Fn(&Progress) + Send + Sync>;

struct Inner {
    state: String,
    percent: i64,
    message: String,
    fw_maxsize: Option<i64>,
    board_id: Option<i64>,
    image_board_id: Option<i64>,
}

/// PX4 USB bootloader uploader.
pub struct FirmwareUploader {
    env: Arc<dyn UploaderEnv>,
    on_progress: Option<ProgressFn>,
    baud: u32,
    connect_timeout_s: f64,
    cancel: Arc<Event>,
    inner: Mutex<Inner>,
    serial: Mutex<Option<Arc<dyn SerialIo>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FirmwareUploader {
    pub fn new(
        env: Arc<dyn UploaderEnv>,
        on_progress: Option<ProgressFn>,
        cancel: Option<Arc<Event>>,
    ) -> FirmwareUploader {
        FirmwareUploader::with_options(env, on_progress, cancel, DEFAULT_BAUD, 10.0)
    }

    pub fn with_options(
        env: Arc<dyn UploaderEnv>,
        on_progress: Option<ProgressFn>,
        cancel: Option<Arc<Event>>,
        baud: u32,
        connect_timeout_s: f64,
    ) -> FirmwareUploader {
        FirmwareUploader {
            env,
            on_progress,
            baud,
            connect_timeout_s,
            cancel: cancel.unwrap_or_default(),
            inner: Mutex::new(Inner {
                state: "idle".into(),
                percent: 0,
                message: String::new(),
                fw_maxsize: None,
                board_id: None,
                image_board_id: None,
            }),
            serial: Mutex::new(None),
        }
    }

    /// The latest `{state, percent, message}`.
    pub fn status(&self) -> Progress {
        let g = lock(&self.inner);
        Progress {
            state: g.state.clone(),
            percent: g.percent,
            message: g.message.clone(),
        }
    }

    /// The board's maximum firmware size, once identified.
    pub fn fw_maxsize(&self) -> Option<i64> {
        lock(&self.inner).fw_maxsize
    }

    /// The board id the bootloader reported.
    pub fn board_id(&self) -> Option<i64> {
        lock(&self.inner).board_id
    }

    /// Is the serial handle still held?
    pub fn is_open(&self) -> bool {
        lock(&self.serial).is_some()
    }

    /// Requests cancellation of a running upload.
    pub fn cancel(&self) {
        self.cancel.set();
    }

    /// Idempotent teardown: cancel, then close the serial handle.
    pub fn shutdown(&self) {
        self.cancel.set();
        self.close_serial();
    }

    /// Runs the whole erase, program, verify and reset sequence. `board_id` is
    /// the board the image was built for; when it and the bootloader's own are
    /// both known and not compatible, nothing is erased. True on `done`.
    pub fn run(&self, device: &str, firmware: &[u8], board_id: Option<i64>) -> bool {
        if firmware.is_empty() {
            self.set("failed", 0, "firmware image is empty or invalid");
            return false;
        }
        if firmware.len() > MAX_FIRMWARE_IMAGE_BYTES {
            self.set("failed", 0, "firmware image exceeds size limit");
            return false;
        }
        {
            let mut g = lock(&self.inner);
            g.fw_maxsize = None;
            g.board_id = None;
            g.image_board_id = board_id;
        }
        // PROG_MULTI counts must be a multiple of 4; the padding is the erased
        // flash value, which is also what the verify CRC pads with.
        let mut image = firmware.to_vec();
        while !image.len().is_multiple_of(4) {
            image.push(0xFF);
        }
        let ok = self.run_sequence(device, &image);
        self.close_serial();
        ok
    }

    fn set(&self, state: &str, percent: i64, message: &str) {
        {
            let mut g = lock(&self.inner);
            g.state = state.into();
            g.percent = percent;
            g.message = message.into();
        }
        if let Some(f) = &self.on_progress {
            f(&Progress {
                state: state.into(),
                percent,
                message: message.into(),
            });
        }
    }

    fn percent(&self) -> i64 {
        lock(&self.inner).percent
    }

    fn close_serial(&self) {
        let port = lock(&self.serial).take();
        if let Some(p) = port {
            p.close();
        }
    }

    fn port(&self) -> Option<Arc<dyn SerialIo>> {
        lock(&self.serial).clone()
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_set()
    }

    /// `serial.write(bytes); serial.flush()`.
    fn send(&self, data: &[u8]) -> io::Result<()> {
        let port = self.port().ok_or_else(|| io::Error::other("port closed"))?;
        port.write(data)?;
        port.flush()
    }

    /// Reads exactly `n` bytes within `timeout_s`, honouring cancel; `None` on
    /// timeout, cancel or a read error.
    fn recv_exact(&self, n: usize, timeout_s: f64) -> Option<Vec<u8>> {
        let deadline = self.env.monotonic() + timeout_s;
        let mut buf: Vec<u8> = Vec::new();
        while buf.len() < n {
            if self.cancelled() {
                return None;
            }
            if self.env.monotonic() >= deadline {
                break;
            }
            let port = self.port()?;
            match port.read(n - buf.len()) {
                Ok(chunk) => buf.extend(chunk),
                Err(_) => return None,
            }
        }
        (buf.len() == n).then_some(buf)
    }

    /// Reads a 2 byte `INSYNC <status>` reply; true on OK.
    fn expect_insync_ok(&self, timeout_s: f64) -> bool {
        let Some(reply) = self.recv_exact(2, timeout_s) else {
            return false;
        };
        if reply.len() < 2 || reply[0] != INSYNC {
            return false;
        }
        if reply[1] == BAD_SILICON {
            self.env.log("ERROR", "bootloader reports bad silicon (F4 < Rev 3)");
            return false;
        }
        reply[1] == OK
    }

    /// Discards pending input so the next command starts in sync.
    fn drain(&self) {
        if let Some(port) = self.port() {
            let _ = port.reset_input();
        }
    }

    /// Polls for the device, then opens it, retrying on a transient busy.
    fn open_device(&self, device: &str) -> bool {
        let deadline = self.env.monotonic() + self.connect_timeout_s;
        while self.env.monotonic() < deadline && !self.cancelled() {
            if self.env.device_present(device) {
                for _ in 0..5 {
                    if self.cancelled() {
                        return false;
                    }
                    match self.env.open(device, self.baud, READ_TIMEOUT_S) {
                        Ok(port) => {
                            *lock(&self.serial) = Some(port);
                            return true;
                        }
                        Err(_) => self.env.sleep(0.2),
                    }
                }
                return false;
            }
            self.env.sleep(0.1);
        }
        false
    }

    /// Establishes sync with the bootloader (GET_SYNC, retried a few times).
    fn sync(&self) -> bool {
        for _ in 0..5 {
            if self.cancelled() {
                return false;
            }
            self.drain();
            if self.send(&[GET_SYNC, EOC]).is_err() {
                return false;
            }
            if self.expect_insync_ok(1.0) {
                return true;
            }
            self.env.sleep(0.1);
        }
        false
    }

    /// One GET_DEVICE query: the 4 byte value, or `None` when not answered. A
    /// refused, short or missing reply drains the input.
    fn get_device_info(&self, info_id: u8) -> Option<i64> {
        let attempt = (|| -> Option<i64> {
            self.send(&[GET_DEVICE, info_id, EOC]).ok()?;
            let raw = self.recv_exact(4, INFO_TIMEOUT_S)?;
            if self.expect_insync_ok(1.0) {
                return Some(i64::from(u32::from_le_bytes(raw.try_into().ok()?)));
            }
            None
        })();
        if attempt.is_none() {
            self.drain();
        }
        attempt
    }

    /// Fetches board info for the board check and the verify CRC. Every failure
    /// is swallowed and the input drained, so a partial reply cannot desync the
    /// CHIP_ERASE that follows.
    fn identify_best_effort(&self) {
        match self.get_device_info(INFO_BOARD_ID) {
            None => self
                .env
                .log("WARNING", "bootloader did not report a board id; board check skipped"),
            Some(id) => {
                lock(&self.inner).board_id = Some(id);
                self.env.log("INFO", &format!("bootloader board id: {id}"));
            }
        }
        if let Some(size) = self.get_device_info(INFO_FLASH_SIZE) {
            if size > 0 && size <= MAX_BOOTLOADER_FLASH_BYTES {
                lock(&self.inner).fw_maxsize = Some(size);
            } else {
                self.env
                    .log("ERROR", &format!("bootloader reported invalid flash size: {size}"));
            }
        }
        let _ = (|| -> Option<()> {
            self.send(&[GET_CHIP, EOC]).ok()?;
            if let Some(chip) = self.recv_exact(4, 1.0) {
                self.recv_exact(2, 0.5);
                let hex: String = chip.iter().map(|b| format!("{b:02x}")).collect();
                self.env.log("INFO", &format!("bootloader chip id: {hex}"));
            }
            Some(())
        })();
        self.drain();
    }

    /// The operator message when the image is for another board, else `None`.
    /// Two boards with the same MCU accept each other's image, and it boots and
    /// drives the wrong pins, so the bootloader's own id is the only reliable answer.
    fn board_refusal(&self) -> Option<String> {
        let (board, wanted) = {
            let g = lock(&self.inner);
            (g.board_id, g.image_board_id)
        };
        let (board, wanted) = (board?, wanted?);
        if board_ids_compatible(board, wanted) {
            if board != wanted {
                self.env.log(
                    "INFO",
                    &format!("board id {board} accepts firmware for board id {wanted}"),
                );
            }
            return None;
        }
        self.env.log(
            "ERROR",
            &format!("firmware board id {wanted} does not match bootloader board id {board}"),
        );
        Some(format!(
            "This firmware is for board id {wanted}, but the connected bootloader reports board id {board}. Nothing was erased."
        ))
    }

    /// Sends CHIP_ERASE and waits for INSYNC OK.
    fn erase(&self) -> bool {
        if self.send(&[CHIP_ERASE, EOC]).is_err() {
            return false;
        }
        self.expect_insync_ok(ERASE_TIMEOUT_S)
    }

    /// Streams the image in PROG_MULTI blocks, checking cancel between each.
    fn program(&self, image: &[u8]) -> bool {
        let total = image.len();
        let (mut offset, mut last_pct) = (0usize, -1i64);
        while offset < total {
            if self.cancelled() {
                self.set("cancelled", self.percent(), "Flash cancelled");
                return false;
            }
            let block = &image[offset..(offset + PROG_MULTI_MAX).min(total)];
            let mut frame = vec![PROG_MULTI, block.len() as u8];
            frame.extend_from_slice(block);
            frame.push(EOC);
            let failed = |at: usize| format!("program failed at offset {at}");
            if self.send(&frame).is_err() {
                self.set("failed", self.percent(), &failed(offset));
                return false;
            }
            if !self.expect_insync_ok(5.0) {
                if self.cancelled() {
                    self.set("cancelled", self.percent(), "Flash cancelled");
                } else {
                    self.set("failed", self.percent(), &failed(offset));
                }
                return false;
            }
            offset += block.len();
            let pct = (100.0 * offset as f64 / total as f64).round_ties_even() as i64;
            if pct != last_pct {
                last_pct = pct;
                self.set("programming", pct, &format!("Programming\u{2026} {pct}%"));
            }
        }
        true
    }

    /// The bootloader CRC over the image padded with 0xFF to fw_maxsize.
    fn compute_expected_crc(&self, image: &[u8]) -> u32 {
        let mut state = bl_crc32(image, 0);
        if let Some(max) = lock(&self.inner).fw_maxsize {
            if max > image.len() as i64 {
                let mut remaining = max as u64 - image.len() as u64;
                let padding = [0xFFu8; 65536];
                while remaining > 0 {
                    let n = remaining.min(65536) as usize;
                    state = bl_crc32(&padding[..n], state);
                    remaining -= n as u64;
                }
            }
        }
        state
    }

    /// Compares the bootloader's flash CRC to the expected one: `Some(true)` on
    /// a match, `Some(false)` on a mismatch, `None` when it could not be checked.
    fn verify(&self, image: &[u8]) -> Option<bool> {
        if self.send(&[GET_CRC, EOC]).is_err() {
            return None;
        }
        let Some(crc) = self.recv_exact(4, 5.0) else {
            self.env
                .log("WARNING", "bootloader did not return a CRC; skipping verify");
            return None;
        };
        if !self.expect_insync_ok(2.0) {
            self.env.log("WARNING", "GET_CRC missing INSYNC/OK; skipping verify");
            return None;
        }
        let max = lock(&self.inner).fw_maxsize;
        if max.is_none_or(|m| m <= 0) {
            self.env.log("WARNING", "board fw_maxsize unknown; skipping CRC verify");
            return None;
        }
        let reported = u32::from_le_bytes(crc.try_into().ok()?);
        let expected = self.compute_expected_crc(image);
        if reported == expected {
            return Some(true);
        }
        self.env.log(
            "ERROR",
            &format!("firmware CRC mismatch: got 0x{reported:08x} expected 0x{expected:08x}"),
        );
        Some(false)
    }

    /// Sends REBOOT; whether the command reached the serial link.
    fn reboot(&self) -> bool {
        if self.send(&[REBOOT, EOC]).is_err() {
            return false;
        }
        self.expect_insync_ok(2.0);
        true
    }

    /// The cancel check of the sequence: reports `cancelled` when it is set.
    fn cancelled_report(&self) -> bool {
        if self.cancelled() {
            self.set("cancelled", 0, "Flash cancelled");
            return true;
        }
        false
    }

    fn run_sequence(&self, device: &str, image: &[u8]) -> bool {
        if self.cancelled_report() {
            return false;
        }
        self.set("connecting", 0, &format!("Waiting for bootloader on {device}\u{2026}"));
        if !self.open_device(device) {
            if !self.cancelled_report() {
                self.set("failed", 0, &format!("bootloader device not available: {device}"));
            }
            return false;
        }
        self.set("syncing", 0, "Synchronising with bootloader\u{2026}");
        if !self.sync() {
            self.set("failed", 0, "bootloader sync failed");
            return false;
        }
        self.identify_best_effort();
        if let Some(refusal) = self.board_refusal() {
            self.set("failed", 0, &refusal);
            return false;
        }
        let Some(max) = lock(&self.inner).fw_maxsize else {
            self.set("failed", 0, "could not determine bootloader flash size");
            return false;
        };
        if image.len() as i64 > max {
            self.set(
                "failed",
                0,
                &format!(
                    "firmware image ({} bytes) exceeds board flash limit ({max} bytes)",
                    image.len()
                ),
            );
            return false;
        }
        if self.cancelled_report() {
            return false;
        }
        self.set("erasing", 0, "Erasing\u{2026}");
        if !self.erase() {
            if !self.cancelled_report() {
                self.set("failed", 0, "erase failed");
            }
            return false;
        }
        self.set("programming", 0, "Programming\u{2026}");
        if !self.program(image) {
            return false;
        }
        self.set("verifying", self.percent(), "Verifying\u{2026}");
        let verdict = self.verify(image);
        if verdict != Some(true) {
            let message = if verdict == Some(false) {
                "firmware CRC mismatch, not booting"
            } else {
                "firmware CRC verification unavailable, not booting"
            };
            self.set("failed", self.percent(), message);
            return false;
        }
        if self.cancelled() {
            self.set("cancelled", self.percent(), "Flash cancelled");
            return false;
        }
        self.set("booting", 100, "Booting new firmware\u{2026}");
        if !self.reboot() {
            self.set("failed", self.percent(), "bootloader reboot command failed");
            return false;
        }
        self.set("done", 100, "Firmware flashed successfully");
        true
    }
}

/// The real machine: the monotonic clock, the file system and the serial ports.
pub struct SystemEnv;

struct SystemPort {
    port: corvus_link::plain::transport::Serial,
    timeout: f64,
}

impl SerialIo for SystemPort {
    fn read(&self, n: usize) -> io::Result<Vec<u8>> {
        self.port.read_timeout(n, self.timeout)
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        self.port.write_timeout(data, 30.0).map(|_| ())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    fn reset_input(&self) -> io::Result<()> {
        self.port.reset_input()
    }

    fn close(&self) {
        self.port.close();
    }
}

impl UploaderEnv for SystemEnv {
    fn monotonic(&self) -> f64 {
        crate::clock::monotonic()
    }

    fn sleep(&self, seconds: f64) {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds.max(0.0)));
    }

    /// A POSIX device is a file system node, so its existence is the question.
    /// A Windows COM port is not: the port list is the only honest answer, and
    /// where it cannot be read the port itself is left to say no.
    fn device_present(&self, device: &str) -> bool {
        if !crate::serial_ports::is_windows_com_port(device) {
            return std::path::Path::new(device).exists();
        }
        let wanted = device.trim().to_lowercase();
        crate::serial_ports::list_serial_ports()
            .iter()
            .any(|p| p.device.trim().to_lowercase() == wanted)
    }

    fn open(&self, device: &str, baud: u32, timeout: f64) -> io::Result<Arc<dyn SerialIo>> {
        let port = corvus_link::plain::transport::Serial::open_direct(device, baud)
            .map_err(crate::peripherals::util::serial_open_error)?;
        Ok(Arc::new(SystemPort { port, timeout }))
    }

    fn log(&self, level: &str, message: &str) {
        if level != "INFO" {
            eprintln!("corvus: firmware {}: {message}", level.to_lowercase());
        }
    }

    fn log_exc(&self, level: &str, message: &str, detail: &str) {
        if level != "DEBUG" {
            eprintln!("corvus: firmware {}: {message}: {detail}", level.to_lowercase());
        }
    }
}
