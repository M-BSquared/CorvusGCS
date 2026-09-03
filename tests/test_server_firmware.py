"""Tests for the firmware-flash backend: uploader, FlashService, HTTP/SSE.

Hermetic — no real serial device, no real autopilot. The PX4 bootloader is
faked by a scripted pyserial stand-in that replies to the protocol commands;
the MAVLink bridge is faked for the FlashService gate tests; the HTTP layer
is exercised via the real ``CorvusHandler`` with ``_send_json``/``_send_sse``
captured, mirroring ``test_server_params.py``.
"""
from __future__ import annotations

import base64
import io
import json
import queue
import threading
import time
import zipfile
import zlib
from typing import Any
from unittest.mock import MagicMock

import pytest

from corvus.firmware_uploader import (
    FirmwareUploader,
    _bl_crc32,
    parse_firmware,
    BAD_SILICON,
    CHIP_ERASE,
    EOC,
    FAILED,
    GET_CHIP,
    GET_CRC,
    GET_DEVICE,
    GET_SYNC,
    INFO_FLASH_SIZE,
    INSYNC,
    INVALID,
    OK,
    PROG_MULTI,
    REBOOT,
)
from corvus.flash_service import FlashService, NON_USB_GATE_MESSAGE
from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Fake PX4 bootloader over pyserial
# ---------------------------------------------------------------------------

class FakeBootloaderSerial:
    """Scripted reply engine for the PX4 bootloader protocol.

    ``write`` parses the command opcode (the first byte) and queues the
    canonical reply into the read buffer; ``read`` drains it. Records every
    command so tests can assert the exact sequence (e.g. that REBOOT was/was
    not sent).
    """

    def __init__(self, fw_maxsize: int = 1 << 20) -> None:
        self._rbuf = bytearray()
        self.fw_maxsize = fw_maxsize
        self.programmed = bytearray()
        self.commands: list[int] = []
        self.rebooted = False
        self.closed = False
        self.timeout = 1.0
        self._lock = threading.Lock()

    def write(self, data: bytes) -> int:
        if not data:
            return 0
        cmd = data[0]
        with self._lock:
            self.commands.append(cmd)
            if cmd == GET_SYNC:
                self._rbuf.extend(bytes([INSYNC, OK]))
            elif cmd == GET_DEVICE:
                arg = data[1] if len(data) > 1 else 0
                self._rbuf.extend((self.fw_maxsize if arg == INFO_FLASH_SIZE else 0).to_bytes(4, "little"))
                self._rbuf.extend(bytes([INSYNC, OK]))
            elif cmd == GET_CHIP:
                self._rbuf.extend((0x12345678).to_bytes(4, "little"))
                self._rbuf.extend(bytes([INSYNC, OK]))
            elif cmd == CHIP_ERASE:
                self._rbuf.extend(bytes([INSYNC, OK]))
            elif cmd == PROG_MULTI:
                count = data[1] if len(data) > 1 else 0
                self.programmed.extend(data[2:2 + count])
                self._rbuf.extend(bytes([INSYNC, OK]))
            elif cmd == GET_CRC:
                img = bytes(self.programmed)
                while len(img) % 4 != 0:
                    img += b"\xff"
                state = _bl_crc32(img, 0)
                if self.fw_maxsize > len(img):
                    state = _bl_crc32(b"\xff" * (self.fw_maxsize - len(img)), state)
                self._rbuf.extend((state & 0xFFFFFFFF).to_bytes(4, "little"))
                self._rbuf.extend(bytes([INSYNC, OK]))
            elif cmd == REBOOT:
                self._rbuf.extend(bytes([INSYNC, OK]))
                self.rebooted = True
        return len(data)

    def read(self, n: int) -> bytes:
        with self._lock:
            if not self._rbuf:
                return b""
            out = bytes(self._rbuf[:n])
            del self._rbuf[:n]
            return out

    def flush(self) -> None:
        pass

    def reset_input_buffer(self) -> None:
        with self._lock:
            self._rbuf.clear()

    def close(self) -> None:
        self.closed = True


def _patch_bootloader(monkeypatch: pytest.MonkeyPatch, fake: FakeBootloaderSerial) -> None:
    """Route ``FirmwareUploader._open_device`` at the pyserial import."""
    import corvus.firmware_uploader as fu

    monkeypatch.setattr(fu, "_serial", MagicMock(Serial=lambda *a, **kw: fake))
    monkeypatch.setattr(fu.os.path, "exists", lambda p: True)


# ---------------------------------------------------------------------------
# parse_firmware
# ---------------------------------------------------------------------------

def test_parse_firmware_raw_binary_returned_unchanged() -> None:
    blob = bytes(range(256)) * 4
    assert parse_firmware(blob) == blob


def test_parse_firmware_json_px4_decodes_base64_zlib() -> None:
    image = bytes(range(256)) * 8  # 2 KiB
    desc = {"image": base64.b64encode(zlib.compress(image)).decode("ascii"),
            "board_id": 1, "image_size": len(image)}
    data = json.dumps(desc).encode("utf-8")
    assert parse_firmware(data) == image


def test_parse_firmware_zip_extracts_named_image_entry() -> None:
    image = bytes(range(256)) * 4
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr("metadata.json", "{}")
        zf.writestr("firmware.px4", image)
    assert parse_firmware(buf.getvalue()) == image


def test_parse_firmware_zip_falls_back_to_largest_entry() -> None:
    image = bytes(range(256)) * 4
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr("a.bin", b"tiny")
        zf.writestr("b.bin", image)
    assert parse_firmware(buf.getvalue()) == image


def test_parse_firmware_empty_raises() -> None:
    with pytest.raises(ValueError):
        parse_firmware(b"")


def test_parse_firmware_invalid_json_raises() -> None:
    with pytest.raises(ValueError):
        parse_firmware(b"{not json")


def test_parse_firmware_json_without_image_field_raises() -> None:
    with pytest.raises(ValueError):
        parse_firmware(b'{"board_id": 1}')


# ---------------------------------------------------------------------------
# FirmwareUploader.run
# ---------------------------------------------------------------------------

def test_run_success_programs_verifies_and_reboots(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = FakeBootloaderSerial(fw_maxsize=1 << 20)
    _patch_bootloader(monkeypatch, fake)
    image = bytes((i * 7) & 0xFF for i in range(4096))  # 4 KiB, multiple PROG_MULTI blocks

    uploader = FirmwareUploader(connect_timeout_s=2.0)
    ok = uploader.run("/dev/ttyACM0", image)

    assert ok is True
    assert uploader.status()["state"] == "done"
    assert fake.rebooted is True
    assert fake.programmed[:len(image)] == image
    # full protocol sequence observed
    assert GET_SYNC in fake.commands
    assert CHIP_ERASE in fake.commands
    assert PROG_MULTI in fake.commands
    assert GET_CRC in fake.commands
    assert REBOOT in fake.commands


def test_run_crc_mismatch_does_not_reboot(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = FakeBootloaderSerial(fw_maxsize=1 << 20)

    # Corrupt the GET_CRC reply so verify reports a wrong CRC.
    real_write = fake.write

    def bad_crc_write(data: bytes) -> int:
        if data and data[0] == GET_CRC:
            with fake._lock:
                fake._rbuf.extend((0xDEADBEEF).to_bytes(4, "little"))
                fake._rbuf.extend(bytes([INSYNC, OK]))
            fake.commands.append(GET_CRC)
            return len(data)
        return real_write(data)

    fake.write = bad_crc_write  # type: ignore[method-assign]
    _patch_bootloader(monkeypatch, fake)
    image = bytes((i * 7) & 0xFF for i in range(2048))

    uploader = FirmwareUploader(connect_timeout_s=2.0)
    ok = uploader.run("/dev/ttyACM0", image)

    assert ok is False
    assert uploader.status()["state"] == "failed"
    assert "CRC mismatch" in uploader.status()["message"]
    # Safety: no RESET/REBOOT sent on a verify failure (no bricking).
    assert REBOOT not in fake.commands
    assert fake.rebooted is False


def test_run_sync_failure_does_not_reboot(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = FakeBootloaderSerial()
    # Reply to GET_SYNC with INVALID instead of OK.
    real_write = fake.write

    def bad_sync(data: bytes) -> int:
        if data and data[0] == GET_SYNC:
            with fake._lock:
                fake._rbuf.extend(bytes([INSYNC, INVALID]))
                fake.commands.append(GET_SYNC)
            return len(data)
        return real_write(data)

    fake.write = bad_sync  # type: ignore[method-assign]
    _patch_bootloader(monkeypatch, fake)

    uploader = FirmwareUploader(connect_timeout_s=2.0)
    ok = uploader.run("/dev/ttyACM0", b"\x00" * 1024)

    assert ok is False
    assert uploader.status()["state"] == "failed"
    assert REBOOT not in fake.commands


def test_run_cancel_before_start_does_not_reboot(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = FakeBootloaderSerial()
    _patch_bootloader(monkeypatch, fake)
    cancel = threading.Event()
    cancel.set()
    uploader = FirmwareUploader(connect_timeout_s=2.0, cancel=cancel)
    ok = uploader.run("/dev/ttyACM0", b"\x00" * 1024)
    assert ok is False
    assert uploader.status()["state"] == "cancelled"
    assert REBOOT not in fake.commands
    assert fake.rebooted is False


def test_run_device_not_available_fails(monkeypatch: pytest.MonkeyPatch) -> None:
    import corvus.firmware_uploader as fu
    monkeypatch.setattr(fu.os.path, "exists", lambda p: False)
    uploader = FirmwareUploader(connect_timeout_s=0.3)
    ok = uploader.run("/dev/ttyACM99", b"\x00" * 16)
    assert ok is False
    assert uploader.status()["state"] == "failed"
    assert "not available" in uploader.status()["message"]


def test_run_closes_serial_handle_on_success(monkeypatch: pytest.MonkeyPatch) -> None:
    """The serial handle is closed in a ``finally`` — no leaked fd on the happy path."""
    fake = FakeBootloaderSerial(fw_maxsize=1 << 20)
    _patch_bootloader(monkeypatch, fake)
    uploader = FirmwareUploader(connect_timeout_s=2.0)
    assert uploader.run("/dev/ttyACM0", bytes((i * 7) & 0xFF for i in range(1024))) is True
    assert fake.closed is True, "serial handle closed after a successful run"


def test_run_closes_serial_handle_on_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    """The serial handle is closed even when the flash fails (CRC mismatch)."""
    fake = FakeBootloaderSerial(fw_maxsize=1 << 20)
    real_write = fake.write

    def bad_crc_write(data: bytes) -> int:
        if data and data[0] == GET_CRC:
            with fake._lock:
                fake._rbuf.extend((0xDEADBEEF).to_bytes(4, "little"))
                fake._rbuf.extend(bytes([INSYNC, OK]))
            fake.commands.append(GET_CRC)
            return len(data)
        return real_write(data)

    fake.write = bad_crc_write  # type: ignore[method-assign]
    _patch_bootloader(monkeypatch, fake)
    uploader = FirmwareUploader(connect_timeout_s=2.0)
    assert uploader.run("/dev/ttyACM0", b"\x00" * 512) is False
    assert fake.closed is True, "serial handle closed after a failed run (no fd leak)"
    assert REBOOT not in fake.commands


def test_uploader_shutdown_closes_serial_handle(monkeypatch: pytest.MonkeyPatch) -> None:
    """FirmwareUploader.shutdown() closes the serial handle (idempotent teardown)."""
    fake = FakeBootloaderSerial(fw_maxsize=1 << 20)
    _patch_bootloader(monkeypatch, fake)
    uploader = FirmwareUploader(connect_timeout_s=2.0)
    # Simulate an open handle (as if a flash were mid-flight) then shut down.
    uploader._serial = fake
    uploader.shutdown()
    assert fake.closed is True, "shutdown closed the serial handle"
    assert uploader._serial is None
    # Idempotent: a second shutdown on an already-closed handle does not raise.
    uploader.shutdown()


def test_run_mid_program_cancel_does_not_reboot(monkeypatch: pytest.MonkeyPatch) -> None:
    """Cancel during the program loop: a block boundary aborts without RESET."""
    fake = FakeBootloaderSerial(fw_maxsize=1 << 20)
    _patch_bootloader(monkeypatch, fake)
    cancel = threading.Event()
    # Large image -> many PROG_MULTI blocks so cancel lands mid-program.
    image = bytes((i * 3) & 0xFF for i in range(200_000))

    # Stall the first PROG_MULTI reply until cancel is set, so the program loop
    # is genuinely mid-flight when cancellation arrives.
    real_write = fake.write
    prog_seen = []

    def stalling_write(data: bytes) -> int:
        if data and data[0] == PROG_MULTI:
            prog_seen.append(1)
            if len(prog_seen) == 1:
                # Hold this reply; let the cancel signal arrive from main thread.
                return len(data)
        return real_write(data)

    fake.write = stalling_write  # type: ignore[method-assign]
    uploader = FirmwareUploader(connect_timeout_s=2.0, cancel=cancel)
    result: dict[str, Any] = {}

    def worker() -> None:
        result["ok"] = uploader.run("/dev/ttyACM0", image)

    t = threading.Thread(target=worker, daemon=True)
    t.start()
    # Wait until the program loop has started, then cancel.
    while not prog_seen:
        time.sleep(0.01)
    cancel.set()
    # Now unblock the stalled reply so the loop wakes and observes cancel.
    real_write(bytes([PROG_MULTI, 0]) + bytes([EOC]))
    t.join(timeout=5)
    assert not t.is_alive()
    assert result["ok"] is False
    assert uploader.status()["state"] == "cancelled"
    assert REBOOT not in fake.commands
    assert fake.rebooted is False


# ---------------------------------------------------------------------------
# FlashService gate
# ---------------------------------------------------------------------------

class FakeMavlink:
    """Stand-in for MavlinkBridge exposing the FlashService surface."""

    def __init__(
        self,
        transport: str = "usb",
        device: str = "/dev/ttyACM0",
        conn_str: str = "serial:/dev/ttyACM0:115200",
        armed: bool = False,
        reboot_ok: bool = True,
    ) -> None:
        self._transport = transport
        self._device = device
        self._conn = conn_str
        self._armed = armed
        self._reboot_ok = reboot_ok
        self.stopped = False
        self.started = False
        self.connection_set_to: str | None = None

    def transport(self) -> str:
        return self._transport

    def is_direct_usb(self) -> bool:
        return self._transport == "usb"

    def serial_device(self) -> str:
        return self._device

    def connection_string(self) -> str:
        return self._conn

    def reboot_to_bootloader(self) -> bool:
        return self._reboot_ok

    def stop(self) -> None:
        self.stopped = True

    def set_connection(self, conn: str) -> None:
        self.connection_set_to = conn

    def start(self) -> None:
        self.started = True


def _store(armed: bool = False) -> VehicleStateStore:
    s = VehicleStateStore()
    s.update(armed=armed)
    return s


def test_flash_start_refuses_non_usb_with_gate_message() -> None:
    fs = FlashService(FakeMavlink(transport="sik", device="/dev/ttyUSB0"), _store())
    assert fs.start(b"\x00" * 16) is False
    assert "direct USB" in fs.last_error
    assert "sik" in fs.last_error


def test_flash_start_refuses_udp_with_gate_message() -> None:
    fs = FlashService(FakeMavlink(transport="udp", device=""), _store())
    assert fs.start(b"\x00" * 16) is False
    assert "UDP" in fs.last_error
    assert fs.last_error == NON_USB_GATE_MESSAGE.format(transport="udp")


def test_flash_start_refuses_tcp_with_gate_message() -> None:
    fs = FlashService(FakeMavlink(transport="tcp", device=""), _store())
    assert fs.start(b"\x00" * 16) is False
    assert "TCP" in fs.last_error


def test_flash_start_refuses_when_armed() -> None:
    fs = FlashService(FakeMavlink(transport="usb"), _store(armed=True))
    assert fs.start(b"\x00" * 16) is False
    assert "armed" in fs.last_error


def test_flash_start_refuses_invalid_firmware_before_reboot() -> None:
    mav = FakeMavlink(transport="usb")
    fs = FlashService(mav, _store())
    # Invalid JSON .px4 -> parse fails synchronously, no reboot attempted.
    assert fs.start(b"{not json") is False
    assert "firmware" in fs.last_error.lower() or "json" in fs.last_error.lower()
    # The FC was NOT asked to reboot (gate/parse happens before the worker).
    assert mav.stopped is False


def test_flash_start_refuses_second_flash_while_in_progress() -> None:
    mav = FakeMavlink(transport="usb", reboot_ok=False)  # worker exits fast on reboot reject
    fs = FlashService(mav, _store())
    assert fs.start(b"\x00" * 1024) is True
    # Second start while the (short-lived) worker is running/in flashing state.
    # Give the worker a moment to set state, then attempt again.
    time.sleep(0.1)
    # Force the state back to flashing to deterministically test the guard.
    fs._state = "flashing"
    assert fs.start(b"\x00" * 1024) is False
    assert "already" in fs.last_error
    fs.shutdown()


def test_flash_status_can_flash_conjunction() -> None:
    # USB, idle, disarmed -> can_flash True.
    fs = FlashService(FakeMavlink(transport="usb"), _store(armed=False))
    st = fs.status()
    assert st["can_flash"] is True
    assert st["transport"] == "usb"
    assert st["device"] == "/dev/ttyACM0"
    assert st["state"] == "idle"
    assert st["armed"] is False

    # SiK -> can_flash False.
    fs2 = FlashService(FakeMavlink(transport="sik", device="/dev/ttyUSB0"), _store())
    assert fs2.status()["can_flash"] is False

    # USB but armed -> can_flash False.
    fs3 = FlashService(FakeMavlink(transport="usb"), _store(armed=True))
    assert fs3.status()["can_flash"] is False
    assert fs3.status()["armed"] is True


def test_flash_cancel_returns_false_when_not_flashing() -> None:
    fs = FlashService(FakeMavlink(transport="usb"), _store())
    assert fs.cancel() is False


def test_flash_shutdown_is_idempotent_and_never_raises() -> None:
    fs = FlashService(FakeMavlink(transport="usb"), _store())
    fs.shutdown()
    fs.shutdown()  # second call must not raise
    assert fs.status()["state"] == "idle"


def test_flash_reconnect_after_reboot_rejected(monkeypatch: pytest.MonkeyPatch) -> None:
    # reboot rejected -> worker sets failed and reconnects the bridge.
    mav = FakeMavlink(transport="usb", reboot_ok=False)
    fs = FlashService(mav, _store())
    assert fs.start(b"\x00" * 1024) is True
    # wait for worker to finish (it returns fast on reboot rejection)
    for _ in range(100):
        if fs.status()["state"] != "flashing":
            break
        time.sleep(0.01)
    assert fs.status()["state"] == "failed"
    # Bridge was reconnected (set_connection + start) after the failed reboot.
    assert mav.connection_set_to == "serial:/dev/ttyACM0:115200"
    assert mav.started is True
    fs.shutdown()


# ---------------------------------------------------------------------------
# HTTP endpoints
# ---------------------------------------------------------------------------

def _handler(flash: Any = None, mavlink: Any = None, store: Any = None):
    h = object.__new__(CorvusHandler)
    h.flash = flash
    h.mavlink = mavlink
    h.store = store
    responses: list[tuple[dict, int]] = []
    h._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return h, responses


def test_api_firmware_status_without_flash_returns_idle_defaults() -> None:
    h, responses = _handler(flash=None, mavlink=None, store=None)
    h._api_firmware_status()
    payload, status = responses[0]
    assert status == 200
    assert payload["state"] == "idle"
    assert payload["can_flash"] is False
    assert payload["transport"] == "unknown"


def test_api_firmware_status_with_flash_forwards_service_status() -> None:
    flash = MagicMock()
    flash.status.return_value = {
        "state": "idle", "transport": "usb", "device": "/dev/ttyACM0",
        "can_flash": True, "armed": False, "progress": 0, "message": "",
    }
    h, responses = _handler(flash=flash)
    h._api_firmware_status()
    assert responses[0][1] == 200
    assert responses[0][0]["can_flash"] is True


def test_api_firmware_upload_raw_empty_body_returns_400() -> None:
    h, responses = _handler(flash=MagicMock(), mavlink=MagicMock())
    h.headers = _Headers(0)
    h.rfile = _Reader(b"")
    h._api_firmware_upload_raw()
    assert responses[0] == ({"ok": False, "error": "empty firmware body"}, 400)


def test_api_firmware_upload_raw_no_service_returns_503() -> None:
    h, responses = _handler(flash=None, mavlink=None)
    h.headers = _Headers(8)
    h.rfile = _Reader(b"\x00" * 8)
    h._api_firmware_upload_raw()
    assert responses[0] == ({"ok": False, "error": "flash service unavailable"}, 503)


def test_api_firmware_upload_raw_gate_refusal_returns_409() -> None:
    flash = MagicMock()
    flash.start.return_value = False
    flash.last_error = "cannot flash while armed"
    h, responses = _handler(flash=flash, mavlink=MagicMock())
    body = b"\x00" * 16
    h.headers = _Headers(len(body))
    h.rfile = _Reader(body)
    h._api_firmware_upload_raw()
    assert responses[0] == ({"ok": False, "error": "cannot flash while armed"}, 409)


def test_api_firmware_upload_raw_accepted_returns_200_flashing() -> None:
    flash = MagicMock()
    flash.start.return_value = True
    h, responses = _handler(flash=flash, mavlink=MagicMock())
    body = b"\x00" * 16
    h.headers = _Headers(len(body))
    h.rfile = _Reader(body)
    h._api_firmware_upload_raw()
    assert responses[0] == ({"ok": True, "state": "flashing"}, 200)
    assert flash.start.call_args[0][0] == body  # raw bytes passed through


def test_api_firmware_cancel_ok() -> None:
    flash = MagicMock()
    flash.cancel.return_value = True
    h, responses = _handler(flash=flash)
    h._api_firmware_cancel({})
    assert responses[0] == ({"ok": True}, 200)


def test_api_firmware_cancel_not_in_progress() -> None:
    flash = MagicMock()
    flash.cancel.return_value = False
    h, responses = _handler(flash=flash)
    h._api_firmware_cancel({})
    assert responses[0] == ({"ok": False, "error": "no flash in progress"}, 200)


def test_api_firmware_cancel_no_service_returns_503() -> None:
    h, responses = _handler(flash=None)
    h._api_firmware_cancel({})
    assert responses[0] == ({"ok": False, "error": "flash service unavailable"}, 503)


def test_post_firmware_upload_bypasses_json_dispatcher() -> None:
    """The raw-binary endpoint must be handled before JSON parsing."""
    flash = MagicMock()
    flash.start.return_value = True
    h, responses = _handler(flash=flash, mavlink=MagicMock())
    body = b"\x00\x01\x02PKgarbage"  # NOT valid JSON; would 400 if json-parsed
    h.headers = _Headers(len(body))
    h.rfile = _Reader(body)
    h.path = "/api/firmware/upload"
    h.do_POST()
    assert responses[0] == ({"ok": True, "state": "flashing"}, 200)


class _Headers:
    def __init__(self, length: int) -> None:
        self._length = str(length)

    def get(self, name: str, default: Any = None) -> Any:
        return self._length if name == "Content-Length" else default


class _Reader:
    def __init__(self, data: bytes) -> None:
        self._data = data

    def read(self, n: int) -> bytes:
        return self._data[:n]


# ---------------------------------------------------------------------------
# SSE /api/firmware/progress
# ---------------------------------------------------------------------------

def test_sse_firmware_emits_initial_status_and_cleans_up_listener(monkeypatch: pytest.MonkeyPatch) -> None:
    flash = MagicMock()
    flash.status.return_value = {
        "state": "idle", "progress": 0, "message": "",
    }
    flash.add_listener = lambda fn: None
    removed: list[Any] = []
    flash.remove_listener = lambda fn: removed.append(fn)

    h = object.__new__(CorvusHandler)
    h.flash = flash
    sent: list[tuple[str, str]] = []
    h._send_sse = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    h.send_response = lambda status: None  # type: ignore[method-assign]
    h.send_header = lambda name, value: None  # type: ignore[method-assign]
    h.end_headers = lambda: None  # type: ignore[method-assign]

    # Break the loop on the first queue.get (simulate client disconnect).
    # Patched at the class level, so the bound call passes the instance first.
    def boom(*args: Any, **kwargs: Any) -> Any:
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(queue.Queue, "get", boom)

    h._sse_firmware()

    # Initial progress event sent before the loop broke.
    assert len(sent) == 1
    assert sent[0][0] == "progress"
    payload = json.loads(sent[0][1])
    assert payload == {"state": "idle", "percent": 0, "message": ""}
    # Listener was removed in the finally block.
    assert len(removed) == 1


def test_sse_firmware_without_flash_skips_listener(monkeypatch: pytest.MonkeyPatch) -> None:
    h = object.__new__(CorvusHandler)
    h.flash = None
    sent: list[tuple[str, str]] = []
    h._send_sse = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    h.send_response = lambda status: None  # type: ignore[method-assign]
    h.send_header = lambda name, value: None  # type: ignore[method-assign]
    h.end_headers = lambda: None  # type: ignore[method-assign]

    def boom(*args: Any, **kwargs: Any) -> Any:
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(queue.Queue, "get", boom)
    h._sse_firmware()
    # No initial event (no flash service); no listener registration attempted.
    assert sent == []


def test_sse_firmware_emits_ping_keepalive(monkeypatch: pytest.MonkeyPatch) -> None:
    """A queue.Empty in the SSE loop emits a ``ping`` keepalive before the next wait."""
    flash = MagicMock()
    flash.status.return_value = {"state": "idle", "progress": 0, "message": ""}
    flash.add_listener = lambda fn: None
    flash.remove_listener = lambda fn: None

    h = object.__new__(CorvusHandler)
    h.flash = flash
    sent: list[tuple[str, str]] = []
    h._send_sse = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    h.send_response = lambda status: None  # type: ignore[method-assign]
    h.send_header = lambda name, value: None  # type: ignore[method-assign]
    h.end_headers = lambda: None  # type: ignore[method-assign]

    # First queue.get -> queue.Empty -> ping keepalive; second -> BrokenPipeError
    # -> the outer except breaks the loop so the handler returns cleanly.
    states = iter(["empty", "pipe"])

    def fake_get(self: Any, timeout: Any = None) -> Any:
        s = next(states)
        if s == "empty":
            raise queue.Empty
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(queue.Queue, "get", fake_get)
    h._sse_firmware()

    events = [e for e, _ in sent]
    assert events[0] == "progress", "initial status event sent first"
    assert events[1] == "ping", "ping keepalive emitted on an empty queue"
    assert sent[1][1] == "{}"


# ---------------------------------------------------------------------------
# End-to-end HTTP gate: a real FlashService over a non-USB link returns 409
# with the full USB-only gate message (not just a generic refusal).
# ---------------------------------------------------------------------------

def test_api_firmware_upload_raw_usb_gate_409_message() -> None:
    mav = FakeMavlink(transport="sik", device="/dev/ttyUSB0")
    fs = FlashService(mav, _store())
    h, responses = _handler(flash=fs, mavlink=mav)
    body = b"\x00" * 16
    h.headers = _Headers(len(body))
    h.rfile = _Reader(body)
    h._api_firmware_upload_raw()

    payload, status = responses[0]
    assert status == 409
    assert payload["ok"] is False
    # The gate message is single-sourced (NON_USB_GATE_MESSAGE) and lists every
    # forbidden transport so the operator knows exactly why flashing is blocked.
    assert "direct USB" in payload["error"]
    assert "SiK" in payload["error"]
    assert "UDP" in payload["error"]
    assert "TCP" in payload["error"]
    # The FC was NOT asked to reboot (gate refused before the worker spawned).
    assert mav.stopped is False
    fs.shutdown()


# ---------------------------------------------------------------------------
# Shutdown: FlashService.shutdown joins a blocking worker (no zombie) and the
# _shutting_down guard prevents the bridge reconnect path during teardown.
# ---------------------------------------------------------------------------

class _BlockingUploader:
    """Fake uploader whose run() blocks until the shared cancel event is set.

    Mirrors the FirmwareUploader surface FlashService._run uses: constructed
    with on_progress + cancel, run(device, image) -> bool, shutdown().
    """

    def __init__(self, on_progress: Any = None, cancel: Any = None, **_kw: Any) -> None:
        self._cancel = cancel
        self.on_progress = on_progress
        self.run_called = False
        self.shutdown_called = False

    def run(self, device: str, image: bytes) -> bool:
        self.run_called = True
        # Block until FlashService.shutdown sets the cancel event (or a safety
        # timeout so a buggy test never hangs the suite).
        self._cancel.wait(timeout=10.0)
        return False

    def shutdown(self) -> None:
        self.shutdown_called = True


def test_flash_shutdown_joins_blocking_worker_and_skips_reconnect(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    mav = FakeMavlink(transport="usb", reboot_ok=True)
    fs = FlashService(mav, _store())
    monkeypatch.setattr("corvus.flash_service.FirmwareUploader", _BlockingUploader)

    assert fs.start(b"\x00" * 1024) is True

    # Wait until the worker has rebooted + stopped the bridge and reached the
    # uploader (run blocks there) — otherwise shutdown could join before the
    # worker starts.
    deadline = time.monotonic() + 2.0
    while not mav.stopped and time.monotonic() < deadline:
        time.sleep(0.01)
    assert mav.stopped, "worker did not reach the uploader (mavlink.stop not called)"

    worker = fs._worker
    assert worker is not None and worker.is_alive()

    fs.shutdown()

    # The worker thread was joined within the timeout — no zombie.
    worker.join(timeout=0.1)
    assert not worker.is_alive(), "flash worker thread survived shutdown (zombie)"
    assert fs._worker is None

    # The _shutting_down guard suppressed the bridge reconnect path: the worker
    # called mavlink.stop() (stopped=True) but never reconnected (started=False).
    assert mav.started is False, "bridge was reconnected during shutdown (guard failed)"
    assert fs.status()["state"] == "idle"

    # Idempotent: a second shutdown does not raise.
    fs.shutdown()


def test_flash_shutdown_idempotent_with_no_worker() -> None:
    fs = FlashService(FakeMavlink(transport="usb"), _store())
    fs.shutdown()
    fs.shutdown()
    assert fs.status()["state"] == "idle"
    assert fs._worker is None
