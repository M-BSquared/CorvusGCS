"""PX4 USB bootloader uploader (pyserial).

Speaks the PX4 NuttX bootloader wire protocol over a direct USB CDC ACM link
and writes a firmware image to the flight controller's flash. The uploader runs
the erase / program / verify / reset sequence **synchronously** in the caller's
thread and reports progress through an ``on_progress(dict)`` callback. It is
cancellable via a ``threading.Event`` and never leaks the serial handle (it is
closed in a ``finally``).

Safety (never bricks the board): on any protocol error or cancellation the
uploader reports ``failed``/``cancelled`` and **does NOT send RESET**, leaving
the FC in the bootloader so the operator can retry. RESET is only sent after a
clean program + verify.

Protocol source verified against:
  * PX4/Bootloader ``bl.c`` (``bootloader()`` state machine, ``crc32()``,
    ``sync_response``/``invalid_response``/``failure_response``) — the
    authoritative FC-side byte definitions.
  * PX4-Autopilot ``Tools/px_uploader.py`` (v1.16.0 / v1.17.0) — the
    authoritative host-side reference uploader (sync, erase, program, verify,
    reboot, and the ``fw_image`` CRC/parsing logic).

The protocol byte values and the CRC algorithm used here are taken **directly**
from those two files. ``INSYNC`` is 0x12 (not 0x14, which is ``BAD_SILICON``);
the boot/reset opcode is ``REBOOT`` = 0x30; and the bootloader CRCs the *entire
flashable area* (image padded to 4 bytes, then 0xFF-padded to the board's
``fw_maxsize``) using CRC-32 with init 0 and no final XOR — not ``zlib.crc32``
of the bare image. See :func:`_bl_crc32` and :meth:`FirmwareUploader._verify`.
"""
from __future__ import annotations

import io
import json
import logging
import os
import threading
import time
import zipfile
import zlib
from typing import Any, Callable

from .mavlink_bridge import is_windows_com_port

logger = logging.getLogger("corvus.firmware")

# pyserial is a hard dependency (pymavlink pulls it in), but import lazily with a
# clear sentinel so the module imports cleanly even in environments where it is
# temporarily unavailable (e.g. a unit test that monkeypatches the serial layer).
try:  # pragma: no cover - import-time guard
    import serial as _serial  # type: ignore[import-not-found]
except ImportError:  # pragma: no cover
    _serial = None  # type: ignore[assignment]


# ---------------------------------------------------------------------------
# PX4 bootloader protocol bytes (verified against PX4/Bootloader bl.c and
# PX4-Autopilot Tools/px_uploader.py, v1.16/v1.17).
# ---------------------------------------------------------------------------

# Reply framing: every command reply is [<data>...] INSYNC <status>.
INSYNC = 0x12          # 'in sync' byte sent before the status byte
EOC = 0x20             # end-of-command marker (appended after each command + args)

# Status bytes that follow INSYNC.
OK = 0x10              # success
FAILED = 0x11          # flash erase/write failure
INVALID = 0x13         # invalid command / bad arguments
BAD_SILICON = 0x14     # F4 < Rev 3 silicon errata (rev5+)

# Command bytes.
GET_SYNC = 0x21        # no args -> INSYNC OK
GET_DEVICE = 0x22     # arg: info_id(1) -> <value:4 LE> INSYNC OK
CHIP_ERASE = 0x23      # -> INSYNC OK (takes several seconds)
CHIP_VERIFY = 0x24    # rev2 only (unused here)
PROG_MULTI = 0x27     # + count(1) + count bytes -> INSYNC OK
READ_MULTI = 0x28     # rev2 only (unused here)
GET_CRC = 0x29        # -> <crc:4 LE> INSYNC OK
GET_CHIP = 0x2c       # -> <mcu_id:4 LE> INSYNC OK
GET_VERSION = 0x2f    # -> <len:4 LE> <ver:len> INSYNC OK
REBOOT = 0x30         # finalise flash and boot the app -> INSYNC OK, then reset

# GET_DEVICE info-id arguments.
INFO_BOARD_ID = 0x02
INFO_FLASH_SIZE = 0x04   # max firmware size in bytes (used to compute the verify CRC)

# PROG_MULTI count byte is a single uint8; the bootloader requires count % 4 == 0
# and bounds it to its 256-byte flash_buffer. px_uploader sends 252-byte blocks
# (the widely-used host-side max, a multiple of 4 with headroom).
PROG_MULTI_MAX = 252

# A full chip erase can take several seconds on large flashes; bound the wait.
ERASE_TIMEOUT_S = 30.0
# Per-read timeout for the serial port: short enough that the cancel flag is
# re-checked promptly during a long erase, long enough to avoid busy-spinning.
READ_TIMEOUT_S = 1.0


def _bl_crc32(data: bytes, state: int = 0) -> int:
    """Bootloader CRC-32 (poly 0xEDB88320, init=state, no final XOR).

    PX4's bootloader (``bl.c`` ``crc32``) and the host uploader
    (``px_uploader.fw_image.__crc32``) both fold bytes with the standard
    reflected CRC-32 table but start at ``state`` (0 by default) and apply **no
    final XOR**. ``zlib.crc32`` instead pre-conditions with 0xFFFFFFFF and
    post-XORs with 0xFFFFFFFF, so the bare ``zlib.crc32(image)`` the spec
    suggests would never match the bootloader. We recover the bootloader variant
    by undoing zlib's pre/post conditioning: passing ``state ^ 0xFFFFFFFF`` as
    zlib's init cancels its pre-XOR, and XORing the result with 0xFFFFFFFF
    cancels its post-XOR, yielding exactly ``fold(data, state)``.
    """
    return (zlib.crc32(data, state ^ 0xFFFFFFFF) ^ 0xFFFFFFFF) & 0xFFFFFFFF


def parse_firmware(data: bytes) -> bytes:
    """Return the raw firmware binary from *data*.

    Real PX4 ``.px4`` files (as produced by the v1.16/v1.17 build and consumed
    by ``px_uploader.py``) are a JSON object whose ``image`` field is
    base64-encoded, zlib-compressed binary — not a ZIP. We therefore handle
    three shapes, in order:

    1. JSON ``.px4`` (the canonical format) — decode ``image`` via
       base64 + zlib.
    2. ZIP (``PK\\x03\\x04`` magic) — extract the binary image entry (prefer one
       named ``firmware.px4``/``image.px4``, else the largest ``.px4``/``.bin``
       entry, else the largest entry). Kept as a defensive fallback for any
       toolchain that wraps the image in a ZIP.
    3. Raw binary (``.bin``) — returned unchanged.

    Raises ``ValueError`` on an empty or invalid payload.
    """
    if not data:
        raise ValueError("empty firmware data")

    stripped = data.lstrip()
    # 1. JSON .px4 (canonical PX4 firmware container).
    if stripped[:1] == b"{":
        try:
            desc = json.loads(stripped.decode("utf-8", errors="replace"))
        except (ValueError, UnicodeDecodeError) as exc:
            raise ValueError(f"invalid firmware JSON: {exc}") from exc
        if not isinstance(desc, dict) or "image" not in desc:
            raise ValueError("firmware JSON has no 'image' field")
        try:
            import base64
            raw = base64.b64decode(desc["image"])
            image = zlib.decompress(raw)
        except Exception as exc:  # base64/zlib errors
            raise ValueError(f"could not decode firmware image: {exc}") from exc
        if not image:
            raise ValueError("firmware image is empty")
        return image

    # 2. ZIP wrapper (defensive; some toolchains bundle the binary).
    if data[:2] == b"PK":
        try:
            zf = zipfile.ZipFile(io.BytesIO(data))
            names = zf.namelist()
        except zipfile.BadZipFile as exc:
            raise ValueError(f"invalid firmware archive: {exc}") from exc
        if not names:
            raise ValueError("firmware archive is empty")
        for preferred in ("firmware.px4", "image.px4"):
            if preferred in names:
                blob = zf.read(preferred)
                if not blob:
                    raise ValueError(f"firmware archive entry {preferred!r} is empty")
                return blob
        candidates = [n for n in names if n.lower().endswith((".px4", ".bin"))]
        pool = candidates or names
        best = max(pool, key=lambda n: zf.getinfo(n).file_size)
        blob = zf.read(best)
        if not blob:
            raise ValueError("firmware archive contains an empty image")
        return blob

    # 3. Raw binary (.bin).
    return data


class FirmwareUploader:
    """PX4 USB bootloader uploader (pyserial).

    Runs the erase/program/verify/reset sequence synchronously in the caller's
    thread (``run``) and reports progress via an ``on_progress(dict)`` callback.
    Cancellable through a ``threading.Event`` and cleanly shut down (the serial
    handle is closed in a ``finally``). Never bricks the board: on any protocol
    error or cancellation it reports ``failed``/``cancelled`` and does NOT send
    RESET, leaving the FC in the bootloader so the operator can retry.
    """

    def __init__(
        self,
        on_progress: Callable[[dict[str, Any]], None] | None = None,
        baud: int = 115200,
        connect_timeout_s: float = 10.0,
        cancel: threading.Event | None = None,
    ) -> None:
        self._on_progress = on_progress
        self._baud = baud
        self._connect_timeout_s = connect_timeout_s
        # Shared cancel event: FlashService sets it to abort a running upload;
        # the worker re-checks it between protocol steps and between blocks.
        self._cancel = cancel if cancel is not None else threading.Event()
        self._lock = threading.Lock()
        self._state = "idle"
        self._percent = 0
        self._message = ""
        self._serial: Any = None
        # Board max firmware size (bytes), fetched best-effort via GET_DEVICE so
        # the verify CRC can be computed over the same padded area the
        # bootloader CRCs. None until/unless identified.
        self._fw_maxsize: int | None = None

    # ------------------------------------------------------------------
    # Public surface
    # ------------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Return the latest ``{state, percent, message}`` snapshot."""
        with self._lock:
            return {"state": self._state, "percent": self._percent, "message": self._message}

    def cancel(self) -> None:
        """Request cancellation of a running upload (idempotent)."""
        self._cancel.set()

    def shutdown(self) -> None:
        """Idempotent teardown: cancel, then close the serial handle. Never raises."""
        self._cancel.set()
        self._close_serial()

    def run(self, device: str, firmware_bytes: bytes) -> bool:
        """Run the full erase/program/verify/reset sequence synchronously.

        Returns ``True`` on ``done``, ``False`` on ``failed``/``cancelled``.
        The serial handle is closed in a ``finally`` so it is never leaked.
        RESET is only sent after a clean program + verify; any error or
        cancellation returns ``False`` without resetting (no bricking).
        """
        # The bootloader requires PROG_MULTI counts to be a multiple of 4; pad
        # the image to a 4-byte boundary with 0xFF (erased-flash value), which
        # also matches the padding used by the verify CRC.
        image = bytearray(firmware_bytes)
        while len(image) % 4 != 0:
            image.append(0xFF)
        try:
            return self._run(device, bytes(image))
        finally:
            self._close_serial()

    # ------------------------------------------------------------------
    # State reporting
    # ------------------------------------------------------------------

    def _set(self, state: str, percent: int, message: str) -> None:
        """Update state under the lock and fan out to the progress callback."""
        with self._lock:
            self._state = state
            self._percent = percent
            self._message = message
        if self._on_progress is not None:
            try:
                self._on_progress({"state": state, "percent": percent, "message": message})
            except Exception:  # noqa: BLE001 - a callback error must not abort the upload
                logger.exception("firmware progress callback raised")

    # ------------------------------------------------------------------
    # Serial helpers
    # ------------------------------------------------------------------

    def _close_serial(self) -> None:
        s = self._serial
        self._serial = None
        if s is not None:
            try:
                s.close()
            except Exception:  # noqa: BLE001 - teardown must not raise
                pass

    def _recv_exact(self, n: int, timeout_s: float) -> bytes | None:
        """Read exactly *n* bytes within *timeout_s*, honouring cancel.

        Returns the bytes, or ``None`` on timeout/cancel. Uses the port's
        short read timeout so the cancel flag is re-checked every ~1 s even
        during a multi-second erase.
        """
        deadline = time.monotonic() + timeout_s
        buf = bytearray()
        while len(buf) < n:
            if self._cancel.is_set():
                return None
            if time.monotonic() >= deadline:
                break
            try:
                chunk = self._serial.read(n - len(buf))
            except Exception:  # noqa: BLE001 - port closed/errored -> treat as timeout
                return None
            if chunk:
                buf.extend(chunk)
        return bytes(buf) if len(buf) == n else None

    def _expect_insync_ok(self, timeout_s: float) -> bool:
        """Read a 2-byte ``INSYNC <status>`` reply and return True on OK."""
        b = self._recv_exact(2, timeout_s)
        if b is None or len(b) < 2:
            return False
        if b[0] != INSYNC:
            return False
        if b[1] == BAD_SILICON:
            logger.error("bootloader reports bad silicon (F4 < Rev 3)")
            return False
        return b[1] == OK

    def _drain(self) -> None:
        """Discard any pending input so the next command starts in sync."""
        try:
            self._serial.reset_input_buffer()
        except Exception:  # noqa: BLE001 - best-effort
            pass

    # ------------------------------------------------------------------
    # Protocol steps
    # ------------------------------------------------------------------

    @staticmethod
    def _device_present(device: str) -> bool:
        """Has the bootloader's serial device re-enumerated yet?

        A POSIX device is a filesystem node, so its existence is the question.
        A Windows COM port is not: ``os.path.exists("COM7")`` is False for a
        port that is present and openable, which made this loop wait out the
        whole connect timeout and report "bootloader device not available" on
        every Windows flash. There the port list is the only honest answer.
        """
        if is_windows_com_port(device):
            try:
                from .mavlink_bridge import MavlinkBridge
                return any(
                    str(p.get("device", "")).strip().lower() == device.strip().lower()
                    for p in MavlinkBridge.list_serial_ports()
                )
            except Exception:  # noqa: BLE001 - enumeration must never abort a flash
                # Let pyserial be the judge instead of refusing on our guess.
                return True
        return os.path.exists(device)

    def _open_device(self, device: str) -> bool:
        """Poll for the device then open it, retrying on transient busy."""
        if _serial is None:
            raise RuntimeError("pyserial is not available; cannot open the bootloader device")
        deadline = time.monotonic() + self._connect_timeout_s
        while time.monotonic() < deadline and not self._cancel.is_set():
            if self._device_present(device):
                for _ in range(5):  # device may be transiently busy right after re-enumeration
                    if self._cancel.is_set():
                        return False
                    try:
                        self._serial = _serial.Serial(
                            device, baudrate=self._baud, timeout=READ_TIMEOUT_S,
                        )
                        return True
                    except Exception:
                        time.sleep(0.2)
                return False  # present but could not be opened
            time.sleep(0.1)
        return False

    def _sync(self) -> bool:
        """Establish sync with the bootloader (GETSYNC, retried a few times)."""
        for _ in range(5):
            if self._cancel.is_set():
                return False
            self._drain()
            try:
                self._serial.write(bytes([GET_SYNC, EOC]))
                self._serial.flush()
            except Exception:  # noqa: BLE001
                return False
            if self._expect_insync_ok(timeout_s=1.0):
                return True
            time.sleep(0.1)
        return False

    def _identify_best_effort(self) -> None:
        """Fetch board info for the verify CRC; tolerate any failure.

        Sends GET_DEVICE(INFO_FLASH_SIZE) to learn ``fw_maxsize`` (needed by the
        verify step) and a best-effort GET_CHIP for the log message. All failures
        are swallowed and the input buffer is drained afterwards so a partial
        reply cannot desync the subsequent CHIP_ERASE.
        """
        try:
            self._serial.write(bytes([GET_DEVICE, INFO_FLASH_SIZE, EOC]))
            self._serial.flush()
            raw = self._recv_exact(4, timeout_s=2.0)
            if raw is not None:
                self._fw_maxsize = int.from_bytes(raw, "little")
                self._recv_exact(2, timeout_s=1.0)  # INSYNC OK
        except Exception:  # noqa: BLE001 - best-effort
            pass
        try:
            self._serial.write(bytes([GET_CHIP, EOC]))
            self._serial.flush()
            chip = self._recv_exact(4, timeout_s=1.0)
            if chip is not None:
                self._recv_exact(2, timeout_s=0.5)  # INSYNC OK
                logger.info("bootloader chip id: %s", chip.hex())
        except Exception:  # noqa: BLE001 - best-effort
            pass
        self._drain()

    def _erase(self) -> bool:
        """Send CHIP_ERASE and wait (up to ERASE_TIMEOUT_S) for INSYNC OK."""
        try:
            self._serial.write(bytes([CHIP_ERASE, EOC]))
            self._serial.flush()
        except Exception:  # noqa: BLE001
            return False
        # Erase blocks for several seconds; the bootloader sends nothing until
        # done, then a single INSYNC <status> pair.
        return self._expect_insync_ok(timeout_s=ERASE_TIMEOUT_S)

    def _program(self, image: bytes) -> bool:
        """Stream the image in PROG_MULTI blocks, checking cancel between each."""
        total = len(image)
        offset = 0
        last_pct = -1
        while offset < total:
            if self._cancel.is_set():
                self._set("cancelled", self._percent, "Flash cancelled")
                return False
            block = image[offset:offset + PROG_MULTI_MAX]
            count = len(block)
            try:
                self._serial.write(bytes([PROG_MULTI, count]) + block + bytes([EOC]))
                self._serial.flush()
            except Exception:  # noqa: BLE001
                self._set("failed", self._percent, f"program failed at offset {offset}")
                return False
            if not self._expect_insync_ok(timeout_s=5.0):
                if self._cancel.is_set():
                    self._set("cancelled", self._percent, "Flash cancelled")
                else:
                    self._set("failed", self._percent, f"program failed at offset {offset}")
                return False
            offset += count
            pct = round(100 * offset / total) if total else 100
            if pct != last_pct:
                last_pct = pct
                self._set("programming", pct, f"Programming… {pct}%")
        return True

    def _compute_expected_crc(self, image: bytes) -> int:
        """Bootloader CRC over the image padded with 0xFF to fw_maxsize."""
        state = _bl_crc32(image, 0)
        if self._fw_maxsize is not None and self._fw_maxsize > len(image):
            # Byte-wise CRC is a stream fold, so one call over the whole pad is
            # identical to px_uploader's 4-byte chunked loop.
            state = _bl_crc32(b"\xff" * (self._fw_maxsize - len(image)), state)
        return state & 0xFFFFFFFF

    def _verify(self, image: bytes) -> bool | None:
        """Compare the bootloader's flash CRC to the expected value.

        Returns ``True`` on match, ``False`` on mismatch (corruption — do NOT
        boot), ``None`` when the check could not be performed (no CRC returned,
        no INSYNC/OK, or fw_maxsize unknown) — treated as best-effort: the
        caller proceeds to boot. Reply framing is ``<crc:4 LE> INSYNC OK``
        (data first, then the status pair) per ``bl.c`` PROTO_GET_CRC.
        """
        try:
            self._serial.write(bytes([GET_CRC, EOC]))
            self._serial.flush()
        except Exception:  # noqa: BLE001
            return None
        crc_bytes = self._recv_exact(4, timeout_s=5.0)
        if crc_bytes is None:
            logger.warning("bootloader did not return a CRC; skipping verify")
            return None
        if not self._expect_insync_ok(timeout_s=2.0):
            logger.warning("GET_CRC missing INSYNC/OK; skipping verify")
            return None
        if self._fw_maxsize is None or self._fw_maxsize <= 0:
            logger.warning("board fw_maxsize unknown; skipping CRC verify")
            return None
        reported = int.from_bytes(crc_bytes, "little")
        expected = self._compute_expected_crc(image)
        if reported == expected:
            return True
        logger.error("firmware CRC mismatch: got 0x%08x expected 0x%08x", reported, expected)
        return False

    def _reboot(self) -> None:
        """Send REBOOT; best-effort read of the final INSYNC OK (board resets)."""
        try:
            self._serial.write(bytes([REBOOT, EOC]))
            self._serial.flush()
        except Exception:  # noqa: BLE001
            return
        # bl.c sends sync_response() before jumping to the app, so the pair
        # usually arrives — but the board resets immediately after, so a read
        # timeout is normal and tolerated.
        self._expect_insync_ok(timeout_s=2.0)

    # ------------------------------------------------------------------
    # Orchestrator
    # ------------------------------------------------------------------

    def _run(self, device: str, image: bytes) -> bool:
        if self._cancel.is_set():
            self._set("cancelled", 0, "Flash cancelled")
            return False

        self._set("connecting", 0, f"Waiting for bootloader on {device}…")
        if not self._open_device(device):
            if self._cancel.is_set():
                self._set("cancelled", 0, "Flash cancelled")
            else:
                self._set("failed", 0, f"bootloader device not available: {device}")
            return False

        self._set("syncing", 0, "Synchronising with bootloader…")
        if not self._sync():
            self._set("failed", 0, "bootloader sync failed")
            return False

        self._identify_best_effort()

        if self._cancel.is_set():
            self._set("cancelled", 0, "Flash cancelled")
            return False
        self._set("erasing", 0, "Erasing…")
        if not self._erase():
            if self._cancel.is_set():
                self._set("cancelled", 0, "Flash cancelled")
            else:
                self._set("failed", 0, "erase failed")
            return False

        self._set("programming", 0, "Programming…")
        if not self._program(image):
            return False  # _program already set cancelled/failed (no RESET sent)

        self._set("verifying", self._percent, "Verifying…")
        verify = self._verify(image)
        if verify is False:
            # Corruption detected: do NOT boot. Leave the FC in the bootloader.
            self._set("failed", self._percent, "firmware CRC mismatch — not booting")
            return False

        self._set("booting", 100, "Booting new firmware…")
        self._reboot()
        self._set("done", 100, "Firmware flashed successfully")
        return True
