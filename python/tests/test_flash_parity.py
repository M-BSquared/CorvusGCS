"""The Rust firmware flashing against the Python it replaced.

``tests/reference/firmware_uploader_python.py`` and
``flash_service_python.py`` keep the PX4 bootloader uploader and the flash
service as they were in Python. The tests compare the bootloader CRC on
random data, then run whole uploads against a scripted bootloader that can
fail in every way a real one does (silent, refused, bad silicon, a wrong
board, an erase or a block that fails, a wrong or missing CRC, a failing
write or reboot, a cancel at any step), on a fake clock, comparing the
progress, the final state, every byte written and every log line. The
service is run the same way on its worker threads: the gates, an upload
and a catalogue download, a board already in its bootloader, a cancel, a
shutdown, and the bridge stopped and restarted around each.
"""
from __future__ import annotations

import logging
import os
import random
import threading
import time
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import firmware_uploader as rust_uploader  # noqa: E402
from corvus import flash_service as rust_service  # noqa: E402
from tests.reference import firmware_uploader_python as reference_uploader  # noqa: E402
from tests.reference import flash_service_python as reference_service  # noqa: E402

UPLOADERS = (reference_uploader, rust_uploader)
SERVICES = (reference_service, rust_service)

INSYNC, OK, FAILED, INVALID, BAD_SILICON = 0x12, 0x10, 0x11, 0x13, 0x14
GET_SYNC, GET_DEVICE, CHIP_ERASE, PROG_MULTI, GET_CRC, GET_CHIP, REBOOT = 0x21, 0x22, 0x23, 0x27, 0x29, 0x2C, 0x30
INFO_BOARD_ID, INFO_FLASH_SIZE = 0x02, 0x04


def _plain(value: Any) -> Any:
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    return value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


# ------------------------------------------------------------------- the CRC

@pytest.mark.parametrize("seed", range(10))
def test_the_bootloader_crc_agrees(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(200):
        data = bytes(rng.randrange(256) for _ in range(rng.choice([0, 1, 3, 4, 100, 5000])))
        state = rng.choice([0, 1, 0xFFFFFFFF, 0x12345678, rng.randrange(2**32)])
        results = [outcome(m._bl_crc32, data, state) for m in UPLOADERS]
        assert results[1] == results[0]
        assert outcome(rust_uploader._bl_crc32, data) == outcome(reference_uploader._bl_crc32, data)
    for state in [-1, 2**40, -(2**40), 1.5, "x", None, True]:
        results = [outcome(m._bl_crc32, b"abc", state) for m in UPLOADERS]
        assert results[1][0] == results[0][0], state
    for data in [bytearray(b"abc"), b"", [1, 2, 3], "x", None]:
        results = [outcome(m._bl_crc32, data) for m in UPLOADERS]
        assert results[1][0] == results[0][0], data


# ------------------------------------------------------------ fake hardware

class Clock:
    def __init__(self) -> None:
        self.t = 1000.0

    def monotonic(self) -> float:
        self.t += 0.01
        return self.t

    def sleep(self, seconds: float) -> None:
        self.t += seconds


class Bootloader:
    """A scripted PX4 bootloader that fails on request."""

    def __init__(self, *, fw_maxsize: int = 1 << 20, board_id: int | None = 9, sync: str = "ok", info: str = "ok",
                 chip: bool = True, erase: str = "ok", prog: str = "ok", prog_at: int = 1, crc: str = "ok",
                 reboot: str = "ok", write_error: int | None = None, write_error_at: int = 0, cancel: Any = None,
                 cancel_at: str = "", read_error_after: int | None = None, on_command: Any = None) -> None:
        self.__dict__.update(locals())
        del self.__dict__["self"]
        self.rbuf = bytearray()
        self.programmed = bytearray()
        self.writes: list[bytes] = []
        self.block = 0
        self.closed = False
        self.reads = 0
        self.rebooted = False
        self.erased = 0

    def _reply(self, *chunks: bytes) -> None:
        for c in chunks:
            self.rbuf.extend(c)

    def write(self, data: bytes) -> int:
        self.writes.append(bytes(data))
        cmd = data[0]
        if self.write_error == cmd:
            self.write_error_at -= 1
            if self.write_error_at < 0:
                raise OSError(5, "Input/output error")
        if self.on_command is not None:
            self.on_command(cmd, self)
        if self.cancel is not None and self.cancel_at == {GET_SYNC: "sync", CHIP_ERASE: "erase", GET_CRC: "crc", REBOOT: "reboot"}.get(cmd):
            self.cancel.set()
        if cmd == GET_SYNC:
            if self.sync == "ok":
                self._reply(bytes([INSYNC, OK]))
            elif self.sync == "bad-silicon":
                self._reply(bytes([INSYNC, BAD_SILICON]))
            elif self.sync == "wrong":
                self._reply(bytes([0x14, OK]))
            elif self.sync == "failed":
                self._reply(bytes([INSYNC, FAILED]))
        elif cmd == GET_DEVICE:
            arg = data[1]
            if self.info == "silent":
                return len(data)
            if self.info == "invalid" or (arg == INFO_BOARD_ID and self.board_id is None):
                self._reply(bytes([INSYNC, INVALID]))
            elif self.info == "short":
                self._reply(b"\x01\x02")
            else:
                value = {INFO_BOARD_ID: self.board_id, INFO_FLASH_SIZE: self.fw_maxsize}.get(arg, 0)
                self._reply(value.to_bytes(4, "little"), bytes([INSYNC, OK]))
        elif cmd == GET_CHIP:
            if self.chip:
                self._reply((0x12345678).to_bytes(4, "little"), bytes([INSYNC, OK]))
        elif cmd == CHIP_ERASE:
            self.erased += 1
            if self.erase == "ok":
                self._reply(bytes([INSYNC, OK]))
            elif self.erase == "fail":
                self._reply(bytes([INSYNC, FAILED]))
        elif cmd == PROG_MULTI:
            count = data[1]
            self.block += 1
            if self.prog != "silent" or self.block != self.prog_at:
                self.programmed.extend(data[2:2 + count])
            if self.block == self.prog_at and self.prog == "fail":
                self._reply(bytes([INSYNC, FAILED]))
            elif self.block == self.prog_at and self.prog == "silent":
                pass
            else:
                self._reply(bytes([INSYNC, OK]))
            if self.cancel is not None and self.cancel_at == f"block{self.block}":
                self.cancel.set()
        elif cmd == GET_CRC:
            image = bytes(self.programmed)
            state = reference_uploader._bl_crc32(image, 0)
            if self.fw_maxsize > len(image):
                state = reference_uploader._bl_crc32(b"\xff" * (self.fw_maxsize - len(image)), state)
            if self.crc == "ok":
                self._reply(state.to_bytes(4, "little"), bytes([INSYNC, OK]))
            elif self.crc == "bad":
                self._reply(((state + 1) & 0xFFFFFFFF).to_bytes(4, "little"), bytes([INSYNC, OK]))
            elif self.crc == "no-insync":
                self._reply(state.to_bytes(4, "little"))
            elif self.crc == "failed":
                self._reply(state.to_bytes(4, "little"), bytes([INSYNC, FAILED]))
        elif cmd == REBOOT:
            self.rebooted = True
            if self.reboot == "ok":
                self._reply(bytes([INSYNC, OK]))
        return len(data)

    def read(self, n: int) -> bytes:
        self.reads += 1
        if self.read_error_after is not None and self.reads > self.read_error_after:
            raise OSError(5, "Input/output error")
        out = bytes(self.rbuf[:n])
        del self.rbuf[:n]
        return out

    def flush(self) -> None:
        pass

    def reset_input_buffer(self) -> None:
        self.rbuf.clear()

    def close(self) -> None:
        self.closed = True


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage(), bool(record.exc_info)))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    loggers = [logging.getLogger("corvus.firmware"), logging.getLogger("corvus.flash")]
    previous = [log.level for log in loggers]
    for log in loggers:
        log.addHandler(handler)
        log.setLevel(logging.DEBUG)
    yield handler
    for log, level in zip(loggers, previous):
        log.removeHandler(handler)
        log.setLevel(level)


def serial_module(fake: Any, opens: list, failures: int = 0) -> Any:
    class SerialModule:
        @staticmethod
        def Serial(device: str, **kwargs: Any) -> Any:  # noqa: N802 - pyserial's spelling
            opens.append((device, tuple(sorted(kwargs))))
            if len(opens) <= failures:
                raise OSError(16, "Device or resource busy")
            fake.closed = False
            return fake

    return SerialModule


IMAGE_SIZES = [1, 3, 4, 251, 252, 253, 1000, 5000]
BOARD_IDS = [None, 9, 33, 5]

CASES = [
    ("ok", {}),
    ("sync-bad-silicon", {"sync": "bad-silicon"}),
    ("sync-wrong", {"sync": "wrong"}),
    ("sync-silent", {"sync": "silent"}),
    ("info-silent", {"info": "silent"}),
    ("info-invalid", {"info": "invalid"}),
    ("info-short", {"info": "short"}),
    ("no-board-id", {"board_id": None}),
    ("no-chip", {"chip": False}),
    ("tiny-flash", {"fw_maxsize": 100}),
    ("huge-flash", {"fw_maxsize": 80 * 1024 * 1024}),
    ("erase-fail", {"erase": "fail"}),
    ("erase-silent", {"erase": "silent"}),
    ("prog-fail", {"prog": "fail", "prog_at": 2}),
    ("prog-silent", {"prog": "silent", "prog_at": 1}),
    ("crc-bad", {"crc": "bad"}),
    ("crc-none", {"crc": "none"}),
    ("crc-no-insync", {"crc": "no-insync"}),
    ("crc-failed", {"crc": "failed"}),
    ("reboot-silent", {"reboot": "silent"}),
    ("write-error-sync", {"write_error": GET_SYNC}),
    ("write-error-info", {"write_error": GET_DEVICE}),
    ("write-error-erase", {"write_error": CHIP_ERASE}),
    ("write-error-prog", {"write_error": PROG_MULTI, "write_error_at": 1}),
    ("write-error-crc", {"write_error": GET_CRC}),
    ("write-error-reboot", {"write_error": REBOOT}),
    ("read-error", {"read_error_after": 6}),
    ("read-error-late", {"read_error_after": 40}),
    ("cancel-sync", {"cancel_at": "sync"}),
    ("cancel-erase", {"cancel_at": "erase"}),
    ("cancel-block", {"cancel_at": "block2"}),
    ("cancel-crc", {"cancel_at": "crc"}),
    ("cancel-reboot", {"cancel_at": "reboot"}),
]


@pytest.mark.parametrize("name,options", CASES, ids=[c[0] for c in CASES])
@pytest.mark.parametrize("size", IMAGE_SIZES)
@pytest.mark.parametrize("image_board", BOARD_IDS)
def test_uploads_agree(name: str, options: dict, size: int, image_board: int | None, monkeypatch: pytest.MonkeyPatch, logs: Logs) -> None:
    results = []
    for module in UPLOADERS:
        logs.lines.clear()
        clock = Clock()
        monkeypatch.setattr(module, "time", clock)
        monkeypatch.setattr(os.path, "exists", lambda p: True)
        cancel = threading.Event()
        fake = Bootloader(cancel=cancel, **options)
        opens: list = []
        monkeypatch.setattr(module, "_serial", serial_module(fake, opens))
        progress: list = []
        uploader = module.FirmwareUploader(on_progress=progress.append, cancel=cancel)
        image = bytes((i * 7 + 1) & 0xFF for i in range(size))
        result = outcome(uploader.run, "/dev/ttyACM0", image, board_id=image_board)
        results.append((result, progress, uploader.status(), fake.writes, fake.closed, fake.rebooted, uploader._serial,
                        uploader._fw_maxsize, uploader._board_id, opens, logs.lines[:], clock.t))
    assert results[1] == results[0]


@pytest.mark.parametrize("failures", [0, 1, 4, 5, 9])
@pytest.mark.parametrize("present", [True, False])
def test_opening_the_device_agrees(failures: int, present: bool, monkeypatch: pytest.MonkeyPatch, logs: Logs) -> None:
    results = []
    for module in UPLOADERS:
        logs.lines.clear()
        clock = Clock()
        monkeypatch.setattr(module, "time", clock)
        monkeypatch.setattr(os.path, "exists", lambda p, _present=present: _present)
        fake = Bootloader()
        opens: list = []
        monkeypatch.setattr(module, "_serial", serial_module(fake, opens, failures))
        progress: list = []
        uploader = module.FirmwareUploader(on_progress=progress.append, connect_timeout_s=2.0)
        result = outcome(uploader.run, "/dev/ttyACM0", b"\x01" * 40)
        results.append((result, progress, uploader.status(), opens, fake.writes, logs.lines[:], clock.t))
    assert results[1] == results[0]


def test_a_missing_serial_layer_fails_alike(monkeypatch: pytest.MonkeyPatch) -> None:
    results = []
    for module in UPLOADERS:
        monkeypatch.setattr(module, "_serial", None)
        monkeypatch.setattr(os.path, "exists", lambda p: True)
        uploader = module.FirmwareUploader(connect_timeout_s=0.2)
        results.append((outcome(uploader.run, "/dev/ttyACM0", b"\x01" * 8), uploader.status()))
    assert results[1] == results[0] and results[0][0][0] == "error"


def test_bad_inputs_fail_alike() -> None:
    for image in [b"", None, "text", bytearray(b"abc"), b"x" * (reference_uploader.MAX_FIRMWARE_IMAGE_BYTES + 1)]:
        results = []
        for module in UPLOADERS:
            uploader = module.FirmwareUploader()
            results.append((outcome(uploader.run, "/dev/ttyACM0", image), uploader.status()))
        assert results[1] == results[0]


def test_the_cases_reach_every_ending(monkeypatch: pytest.MonkeyPatch) -> None:
    seen = set()
    for _name, options in CASES:
        clock = Clock()
        monkeypatch.setattr(rust_uploader, "time", clock)
        monkeypatch.setattr(os.path, "exists", lambda p: True)
        cancel = threading.Event()
        fake = Bootloader(cancel=cancel, **options)
        monkeypatch.setattr(rust_uploader, "_serial", serial_module(fake, []))
        uploader = rust_uploader.FirmwareUploader(cancel=cancel)
        uploader.run("/dev/ttyACM0", bytes(600), board_id=9)
        status = uploader.status()
        seen.add((status["state"], status["message"].split(" at ")[0].split(":")[0][:40]))
    states = {s for s, _ in seen}
    assert {"done", "failed", "cancelled"} <= states, seen
    messages = " | ".join(m for _, m in seen)
    for expected in ("bootloader sync failed", "erase failed", "firmware CRC mismatch", "program failed",
                     "could not determine bootloader flash size", "Flash cancelled", "exceeds board"):
        assert expected[:40] in messages, (expected, messages)


# ------------------------------------------------------------------ service

class Mavlink:
    def __init__(self, *, usb: bool = True, reboot: bool = True, conn: str = "serial:/dev/ttyACM0:115200",
                 device: str = "/dev/ttyACM0", rows: list | None = None, raises: str = "") -> None:
        self.usb, self.reboot, self.conn, self.device = usb, reboot, conn, device
        self.rows = rows if rows is not None else []
        self.raises = raises
        self.events: list = []

    def transport(self) -> str:
        return "usb" if self.usb else "sik"

    def is_direct_usb(self) -> bool:
        return self.usb

    def connection_string(self) -> str:
        return self.conn

    def serial_device(self) -> str:
        return self.device

    def reboot_to_bootloader(self) -> bool:
        self.events.append("reboot")
        return self.reboot

    def stop(self) -> None:
        self.events.append("stop")
        if self.raises == "stop":
            raise RuntimeError("already down")

    def set_connection(self, conn: str) -> None:
        self.events.append(("set", conn))
        if self.raises == "restart":
            raise RuntimeError("cannot restart")

    def start(self) -> None:
        self.events.append("start")

    def list_serial_ports(self) -> list:
        if self.raises == "list":
            raise OSError("no ports")
        return self.rows


class Store:
    def __init__(self, connected: bool = True, armed: bool = False) -> None:
        self.snap = {"connected": connected, "armed": armed}

    def get_snapshot(self) -> dict:
        return dict(self.snap)


class Catalog:
    def __init__(self, *, known: bool = True, cached: bool = False, fail: str = "", image: bytes | None = None, store: Store | None = None) -> None:
        self.known, self.cached, self.fail, self.store = known, cached, fail, store
        self.image = image if image is not None else bytes(range(256)) * 3
        self.calls: list = []

    def resolve(self, tag: str, board: str) -> dict | None:
        self.calls.append(("resolve", tag, board))
        return {"name": f"{board}.px4", "tag": tag} if self.known else None

    def cached_path(self, name: str) -> str | None:
        return "/cache/x" if self.cached else None

    def download(self, entry: dict, on_progress: Any = None, cancel: Any = None) -> bytes:
        self.calls.append(("download", entry["name"], on_progress is not None))
        if on_progress is not None:
            for done in (0, 1500, 3000, 4096):
                on_progress(done, 4096)
            on_progress(10, 0)
        if self.fail == "value":
            raise ValueError("bad release file")
        if self.fail == "crash":
            raise RuntimeError("network on fire")
        if self.fail == "cancel":
            cancel.set()
        if self.fail == "arm" and self.store is not None:
            self.store.snap["armed"] = True
        if self.fail == "unplug" and self.store is not None:
            self.store.snap["connected"] = True
        return self.image


PORT_ROWS = {
    "none": [],
    "one": [{"device": "/dev/ttyACM9", "description": "PX4 BL FMU v6X.x", "hwid": "USB VID:PID=26AC:0011"}],
    "two": [{"device": "/dev/ttyACM9", "description": "PX4 BL FMU v6X.x", "hwid": "USB VID:PID=26AC:0011"},
            {"device": "/dev/ttyACM8", "description": "PX4 BL FMU v5.x", "hwid": "USB VID:PID=26AC:0011"}],
    "firmware": [{"device": "/dev/ttyACM0", "description": "PX4 FMU v6X", "hwid": "USB VID:PID=26AC:0032"}],
}

PX4_IMAGE = bytes((i * 3) & 0xFF for i in range(700))

SERVICE_CASES = [
    # name, call, mavlink options, store options, ports, bootloader options, catalog options
    ("upload-ok", "upload", {}, {}, "none", {}, {}),
    ("upload-armed", "upload", {}, {"armed": True}, "none", {}, {}),
    ("upload-sik", "upload", {"usb": False}, {}, "none", {}, {}),
    ("upload-sik-bootloader", "upload", {"usb": False}, {"connected": False}, "one", {}, {}),
    ("upload-reboot-rejected", "upload", {"reboot": False}, {}, "none", {}, {}),
    ("upload-stop-raises", "upload", {"raises": "stop"}, {}, "none", {}, {}),
    ("upload-restart-raises", "upload", {"raises": "restart"}, {}, "none", {}, {}),
    ("upload-no-device", "upload", {"device": ""}, {}, "none", {}, {}),
    ("upload-bad-image", "upload-bad", {}, {}, "none", {}, {}),
    ("upload-crc-bad", "upload", {}, {}, "none", {"crc": "bad"}, {}),
    ("upload-wrong-board", "upload", {}, {}, "none", {"board_id": 77}, {}),
    ("upload-no-link", "upload", {"conn": ""}, {}, "none", {}, {}),
    ("boot-one", "upload", {}, {"connected": False}, "one", {}, {}),
    ("boot-two", "upload", {"usb": False}, {"connected": False}, "two", {}, {}),
    ("boot-lister-fails", "upload", {"raises": "list", "usb": False}, {"connected": False}, "none", {}, {}),
    ("release-ok", "release", {}, {}, "none", {}, {}),
    ("release-cached", "release", {}, {}, "none", {}, {"cached": True}),
    ("release-unknown", "release", {}, {}, "none", {}, {"known": False}),
    ("release-no-catalog", "release-none", {}, {}, "none", {}, {}),
    ("release-value-error", "release", {}, {}, "none", {}, {"fail": "value"}),
    ("release-crash", "release", {}, {}, "none", {}, {"fail": "crash"}),
    ("release-cancelled", "release", {}, {}, "none", {}, {"fail": "cancel"}),
    ("release-armed-meanwhile", "release", {}, {}, "none", {}, {"fail": "arm"}),
    ("release-bootloader", "release", {"usb": False}, {"connected": False}, "one", {}, {}),
    ("release-bootloader-left", "release", {"usb": False}, {"connected": False}, "one", {}, {"fail": "unplug"}),
    ("release-garbage", "release", {}, {}, "none", {}, {"image": b""}),
    ("busy-second-start", "busy", {}, {}, "none", {}, {}),
    ("cancel-idle", "cancel-idle", {}, {}, "none", {}, {}),
    ("cancel-erase", "cancel-erase", {}, {}, "none", {"erase": "silent"}, {}),
    ("shutdown-erase", "shutdown-erase", {}, {}, "none", {"erase": "silent"}, {}),
    ("shutdown-idle", "shutdown-idle", {}, {}, "none", {}, {}),
    ("start-after-shutdown", "after-shutdown", {}, {}, "none", {}, {}),
]


def wait_for(predicate: Any, timeout: float = 8.0) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(0.005)
    return False


def without_start_notice(events: list, call: str) -> list:
    """Drop the one notice `start()` sends after the worker is already running.

    Where it lands among the worker's own events depends on thread timing in
    the Python service and the Rust one alike, so it is not compared.
    """
    out = list(events)
    release = call.startswith("release")
    for i, entry in enumerate(out):
        message = entry.get("message", "")
        if (release and message.startswith("Downloading ") and message.endswith("\u2026") and entry.get("percent") == 0 and entry.get("state") == "downloading") or (
            not release and message == "Preparing to flash\u2026" and entry.get("state") == "flashing"
        ):
            del out[i]
            break
    return out


@pytest.mark.parametrize("case", SERVICE_CASES, ids=[c[0] for c in SERVICE_CASES])
def test_the_service_agrees(case: tuple, monkeypatch: pytest.MonkeyPatch, logs: Logs) -> None:
    name, call, mav_opts, store_opts, ports, boot_opts, cat_opts = case
    results = []
    for service_module, uploader_module in zip(SERVICES, UPLOADERS):
        logs.lines.clear()
        monkeypatch.setattr(os.path, "exists", lambda p: True)
        fake = Bootloader(**boot_opts)
        opens: list = []
        monkeypatch.setattr(uploader_module, "_serial", serial_module(fake, opens))
        rows = PORT_ROWS[ports]
        mavlink = Mavlink(rows=rows, **mav_opts)
        store = Store(**store_opts)
        catalog = None if call == "release-none" else Catalog(store=store, **cat_opts)
        svc = service_module.FlashService(mavlink, store, catalog)
        events: list = []
        svc.add_listener(events.append)
        first: Any = None
        second: Any = None
        if call in ("upload", "busy"):
            first = svc.start(PX4_IMAGE)
            if call == "busy":
                second = (svc.start(PX4_IMAGE), svc.last_error, svc.start_release("v1", "fmu-v6x"), svc.last_error)
        elif call == "upload-bad":
            first = svc.start(b"{not json")
        elif call.startswith("release"):
            first = svc.start_release("v1.16.0", "fmu-v6x")
        elif call == "cancel-idle":
            first = svc.cancel()
        elif call == "cancel-erase":
            first = svc.start(PX4_IMAGE)
            assert wait_for(lambda _f=fake: _f.erased)
            second = svc.cancel()
        elif call == "shutdown-erase":
            first = svc.start(PX4_IMAGE)
            assert wait_for(lambda _f=fake: _f.erased)
            svc.shutdown()
        elif call in ("shutdown-idle", "after-shutdown"):
            svc.shutdown()
            svc.shutdown()
            if call == "after-shutdown":
                first = svc.start(PX4_IMAGE)
        worker = svc._worker
        if worker is not None:
            worker.join(timeout=10)
        if call == "busy":
            pass
        svc.shutdown()
        status = svc.status()
        results.append((first, second, svc.last_error, status, without_start_notice(events, call), mavlink.events, fake.writes, fake.closed,
                        fake.rebooted, opens, logs.lines[:], svc._worker, svc._uploader, svc.catalog is None or catalog.calls))
    assert results[1] == results[0], name


def test_the_service_cases_reach_the_main_endings() -> None:
    seen = set()
    for _name, call, mav_opts, store_opts, ports, boot_opts, cat_opts in SERVICE_CASES:
        if call.startswith("shutdown") or call in ("after-shutdown", "cancel-erase", "busy"):
            continue
        fake = Bootloader(**boot_opts)
        opens: list = []
        old = rust_uploader._serial
        rust_uploader._serial = serial_module(fake, opens)
        orig_exists = os.path.exists
        os.path.exists = lambda p: True
        try:
            store = Store(**store_opts)
            mavlink = Mavlink(rows=PORT_ROWS[ports], **mav_opts)
            catalog = None if call == "release-none" else Catalog(store=store, **cat_opts)
            svc = rust_service.FlashService(mavlink, store, catalog)
            if call.startswith("release"):
                started = svc.start_release("v1", "fmu-v6x")
            elif call == "upload-bad":
                started = svc.start(b"{not json")
            elif call == "cancel-idle":
                started = svc.cancel()
            else:
                started = svc.start(PX4_IMAGE)
            if svc._worker is not None:
                svc._worker.join(timeout=10)
            seen.add((started, svc.status()["state"], svc.last_error[:30]))
        finally:
            rust_uploader._serial = old
            os.path.exists = orig_exists
    states = {s for _, s, _ in seen}
    assert {"done", "failed", "cancelled", "idle"} <= states, seen
    errors = " | ".join(e for _, _, e in seen)
    for expected in ("cannot flash while armed", "Firmware flash requires", "unknown firmware release", "firmware catalogue una"):
        assert expected in errors, (expected, errors)
