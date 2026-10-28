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
import sys
import threading
import time  # noqa: F401 - the engine reads the clock here
import zipfile
import zlib
from types import SimpleNamespace
from typing import Any, NamedTuple
from collections.abc import Callable

from corvus_link import flash as _flash
from corvus_link import rtk as _rtk

from .mavlink_bridge import MavlinkBridge
from .serial_ports import is_windows_com_port

logger = logging.getLogger("corvus.firmware")

# pyserial is a hard dependency (the serial port listing uses it), but import lazily with a
# clear sentinel so the module imports cleanly even in environments where it is
# temporarily unavailable (e.g. a unit test that monkeypatches the serial layer).
# The serial port the uploader opens: corvus_link's own, straight at the
# bootloader's baud rate. A test replaces this to hand in a fake port.
_serial: Any = SimpleNamespace(Serial=_rtk.SerialPort)


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

# Current PX4 flight-controller images are only a few MiB. 32 MiB leaves
# ample room for future boards while preventing a compressed upload from
# expanding until it exhausts the field laptop's memory.
MAX_FIRMWARE_IMAGE_BYTES = 32 * 1024 * 1024
MAX_BOOTLOADER_FLASH_BYTES = 64 * 1024 * 1024

# How long one GET_DEVICE query may take before the board is treated as not
# answering it.
INFO_TIMEOUT_S = 2.0

# Bootloader board id -> the one other firmware board id it runs. Taken from
# ArduPilot Tools/scripts/uploader.py ``compatible_IDs``; PX4's px_uploader.py
# (v1.16, v1.17) has no exceptions and requires an exact match.
COMPATIBLE_BOARD_IDS: dict[int, int] = {
    33: 9,  # AUAVX2.1 runs fmu-v2/v3 firmware
}


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
    return _flash.bl_crc32(data, state)


class FirmwareImage(NamedTuple):
    """A decoded firmware file: the bytes to flash and the board they are for.

    ``board_id`` comes from the ``.px4``/``.apj`` container and is ``None`` for
    a raw binary, which carries no board information.
    """

    image: bytes
    board_id: int | None = None


def board_ids_compatible(bootloader_id: int, firmware_id: int) -> bool:
    """May firmware built for *firmware_id* run on a board reporting *bootloader_id*?"""
    return bootloader_id == firmware_id or COMPATIBLE_BOARD_IDS.get(bootloader_id) == firmware_id


def parse_firmware(data: bytes) -> bytes:
    """Return the raw firmware binary from *data*.

    See :func:`parse_firmware_image`, which also returns the board id.
    """
    return parse_firmware_image(data).image


def parse_firmware_image(data: bytes) -> FirmwareImage:
    """Decode *data* into the firmware binary and its target board id.

    Real PX4 ``.px4`` files (as produced by the v1.16/v1.17 build and consumed
    by ``px_uploader.py``) are a JSON object whose ``image`` field is
    base64-encoded, zlib-compressed binary — not a ZIP. ArduPilot's ``.apj``
    uses the same container. We therefore handle three shapes, in order:

    1. JSON ``.px4``/``.apj`` (the canonical format) — decode ``image`` via
       base64 + zlib, and read ``board_id``.
    2. ZIP (``PK\\x03\\x04`` magic) — extract the binary image entry (prefer one
       named ``firmware.px4``/``image.px4``, else the largest ``.px4``/``.bin``
       entry, else the largest entry). Kept as a defensive fallback for any
       toolchain that wraps the image in a ZIP. No board id.
    3. Raw binary (``.bin``) — returned unchanged. No board id.

    Raises ``ValueError`` on an empty or invalid payload.
    """
    if not isinstance(data, bytes) or not data:
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
        encoded = desc["image"]
        if not isinstance(encoded, str):
            raise ValueError("firmware JSON 'image' must be a base64 string")
        board_id = desc.get("board_id")
        if board_id is not None and (
            isinstance(board_id, bool) or not isinstance(board_id, int)
            or not 0 <= board_id <= 0xFFFFFFFF
        ):
            raise ValueError("firmware JSON 'board_id' must be an unsigned integer")
        try:
            import base64
            raw = base64.b64decode(encoded.encode("ascii"), validate=True)
            decoder = zlib.decompressobj()
            image = decoder.decompress(raw, MAX_FIRMWARE_IMAGE_BYTES + 1)
            if len(image) > MAX_FIRMWARE_IMAGE_BYTES or decoder.unconsumed_tail:
                raise ValueError("decoded firmware image exceeds size limit")
            room = MAX_FIRMWARE_IMAGE_BYTES + 1 - len(image)
            if room > 0:
                image += decoder.flush(room)
            if len(image) > MAX_FIRMWARE_IMAGE_BYTES:
                raise ValueError("decoded firmware image exceeds size limit")
            if not decoder.eof:
                raise ValueError("firmware image has an incomplete zlib stream")
        except ValueError:
            raise
        except Exception as exc:  # base64/zlib errors
            raise ValueError(f"could not decode firmware image: {exc}") from exc
        if not image:
            raise ValueError("firmware image is empty")
        return FirmwareImage(image, board_id)

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
                info = zf.getinfo(preferred)
                if info.file_size > MAX_FIRMWARE_IMAGE_BYTES:
                    raise ValueError("firmware archive image exceeds size limit")
                blob = zf.read(info)
                if not blob:
                    raise ValueError(f"firmware archive entry {preferred!r} is empty")
                return FirmwareImage(blob)
        candidates = [n for n in names if n.lower().endswith((".px4", ".bin"))]
        pool = candidates or names
        best = max(pool, key=lambda n: zf.getinfo(n).file_size)
        info = zf.getinfo(best)
        if info.file_size > MAX_FIRMWARE_IMAGE_BYTES:
            raise ValueError("firmware archive image exceeds size limit")
        blob = zf.read(info)
        if not blob:
            raise ValueError("firmware archive contains an empty image")
        return FirmwareImage(blob)

    # 3. Raw binary (.bin).
    if len(data) > MAX_FIRMWARE_IMAGE_BYTES:
        raise ValueError("firmware image exceeds size limit")
    return FirmwareImage(data)


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
        # The board id the bootloader reports, and the one the image was built
        # for. Either may stay None; the check needs both.
        self._board_id: int | None = None
        self._image_board_id: int | None = None

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

    def run(self, device: str, firmware_bytes: bytes, board_id: int | None = None) -> bool:
        """Run the full erase/program/verify/reset sequence synchronously.

        *board_id* is the board the image was built for (from its container).
        When it and the bootloader's own board id are both known and are not
        compatible, the upload stops before anything is erased.

        Returns ``True`` on ``done``, ``False`` on ``failed``/``cancelled``.
        The serial handle is closed in a ``finally`` so it is never leaked.
        RESET is only sent after a clean program + verify; any error or
        cancellation returns ``False`` without resetting (no bricking).
        """
        if not isinstance(firmware_bytes, bytes) or not firmware_bytes:
            self._set("failed", 0, "firmware image is empty or invalid")
            return False
        if len(firmware_bytes) > MAX_FIRMWARE_IMAGE_BYTES:
            self._set("failed", 0, "firmware image exceeds size limit")
            return False
        self._fw_maxsize = None
        self._board_id = None
        self._image_board_id = board_id
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
        return _flash_engine.recv_exact(self, n, timeout_s)

    def _expect_insync_ok(self, timeout_s: float) -> bool:
        """Read a 2-byte ``INSYNC <status>`` reply and return True on OK."""
        return _flash_engine.expect_insync_ok(self, timeout_s)

    def _drain(self) -> None:
        """Discard any pending input so the next command starts in sync."""
        _flash_engine.drain(self)

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
        if not is_windows_com_port(device):
            return os.path.exists(device)
        try:
            wanted = device.strip().lower()
            return any(
                str(p.get("device", "")).strip().lower() == wanted
                for p in MavlinkBridge.list_serial_ports()
            )
        except Exception:  # noqa: BLE001 - enumeration must never abort a flash
            # Let pyserial be the judge instead of refusing on our own guess: a
            # board sitting in its bootloader should not be stranded because we
            # could not read the port list.
            return True

    def _open_device(self, device: str) -> bool:
        """Poll for the device then open it, retrying on transient busy."""
        return _flash_engine.open_device(self, device)

    def _sync(self) -> bool:
        """Establish sync with the bootloader (GETSYNC, retried a few times)."""
        return _flash_engine.sync(self)

    def _get_device_info(self, info_id: int) -> int | None:
        """One GET_DEVICE query: the 4-byte value, or None when not answered.

        A refused, short or missing reply drains the input so its leftovers
        cannot be read as the answer to the next command.
        """
        return _flash_engine.get_device_info(self, info_id)

    def _identify_best_effort(self) -> None:
        """Fetch board info for the board check and the verify CRC.

        Sends GET_DEVICE(INFO_BOARD_ID) for the board check,
        GET_DEVICE(INFO_FLASH_SIZE) to learn ``fw_maxsize`` (needed by the
        verify step) and a best-effort GET_CHIP for the log message. All failures
        are swallowed and the input buffer is drained afterwards so a partial
        reply cannot desync the subsequent CHIP_ERASE.
        """
        _flash_engine.identify_best_effort(self)

    def _board_refusal(self) -> str | None:
        """Operator message when the image is for another board, else None.

        Two boards with the same MCU accept each other's image, and it boots
        and drives the wrong pins, so the bootloader's own id is the only
        reliable answer. Unknown on either side keeps the old behaviour.
        """
        return _flash_engine.board_refusal(self)

    def _erase(self) -> bool:
        """Send CHIP_ERASE and wait (up to ERASE_TIMEOUT_S) for INSYNC OK."""
        return _flash_engine.erase(self)

    def _program(self, image: bytes) -> bool:
        """Stream the image in PROG_MULTI blocks, checking cancel between each."""
        return _flash_engine.program(self, image)

    def _compute_expected_crc(self, image: bytes) -> int:
        """Bootloader CRC over the image padded with 0xFF to fw_maxsize."""
        return _flash_engine.compute_expected_crc(self, image)

    def _verify(self, image: bytes) -> bool | None:
        """Compare the bootloader's flash CRC to the expected value.

        Returns ``True`` on match, ``False`` on mismatch (corruption — do NOT
        boot), ``None`` when the check could not be performed (no CRC returned,
        no INSYNC/OK, or fw_maxsize unknown). Reply framing is
        ``<crc:4 LE> INSYNC OK``
        (data first, then the status pair) per ``bl.c`` PROTO_GET_CRC.
        """
        return _flash_engine.verify(self, image)

    def _reboot(self) -> bool:
        """Send REBOOT and report whether the command reached the serial link."""
        return _flash_engine.reboot(self)

    # ------------------------------------------------------------------
    # Orchestrator
    # ------------------------------------------------------------------

    def _run(self, device: str, image: bytes) -> bool:
        return _flash_engine.run(self, device, image)


_flash_engine = _flash.UploaderEngine(sys.modules[__name__])
