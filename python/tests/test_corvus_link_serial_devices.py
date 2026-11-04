"""The serial devices Corvus talks to, played by software on a pseudo terminal.

No hardware is needed or used. A pty pair gives the real Rust serial port
(`corvus_link.rtk.SerialPort`, the one the SiK service, the bootloader
flasher and the RTK base session open) a device node to open, and a thread
on the other end plays the device: a SiK radio that wants ``+++`` after a
second of silence and answers the AT dialect, a PX4 bootloader that fails on
request, a u-blox base station that is configured and then streams RTCM.
What reaches each device, what the services report and what is left behind
(the radio's EEPROM, the receiver out of base mode, the serial port closed)
are asserted.
"""
from __future__ import annotations

import os
import threading
import time
from typing import Any
from collections.abc import Callable

import pytest

pytest.importorskip("corvus_link")
if os.name == "nt":
    pytest.skip("pseudo-terminals are POSIX", allow_module_level=True)

import pty  # noqa: E402
import tty  # noqa: E402

from corvus import firmware_uploader as fu  # noqa: E402
from corvus import rtk, rtk_service, sik_config, sik_service  # noqa: E402
from corvus_link import rtk as link_rtk  # noqa: E402
from tests.test_flash_parity import Bootloader  # noqa: E402
from tests.test_rtk_service import FakeBridge, FakeReceiver, FakeStore  # noqa: E402
from tests.test_sik_service import ATI5_LOCAL, ATI5_REMOTE  # noqa: E402


class PtyDevice:
    """A device node whose far end is a function and an optional ticker."""

    def __init__(self, handle: Callable[[bytes], bytes], tick: Callable[[], bytes] | None = None) -> None:
        self.master, self.slave = pty.openpty()
        tty.setraw(self.slave)
        self.path = os.ttyname(self.slave)
        self.handle, self.tick = handle, tick
        self.stop = threading.Event()
        os.set_blocking(self.master, False)
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self) -> None:
        while not self.stop.is_set():
            out = b""
            try:
                data = os.read(self.master, 65536)
                if data:
                    out += self.handle(data)
            except BlockingIOError:
                time.sleep(0.003)
            except OSError:
                break
            if self.tick is not None:
                out += self.tick()
            if out:
                try:
                    os.write(self.master, out)
                except OSError:
                    break

    def close(self) -> None:
        self.stop.set()
        self.thread.join(timeout=3)
        for fd in (self.master, self.slave):
            try:
                os.close(fd)
            except OSError:
                pass


def pty_device(handle: Callable[[bytes], bytes], tick: Callable[[], bytes] | None = None) -> Any:
    device = PtyDevice(handle, tick)
    return device


@pytest.fixture
def devices() -> Any:
    made: list[PtyDevice] = []

    def make(handle: Callable[[bytes], bytes] = lambda data: b"", tick: Callable[[], bytes] | None = None) -> PtyDevice:
        device = PtyDevice(handle, tick)
        made.append(device)
        return device

    yield make
    for device in made:
        device.close()


# ------------------------------------------------------------------ the port ----

def test_the_serial_port_reads_writes_and_times_out(devices: Any) -> None:
    received = bytearray()
    device = devices(lambda data: (received.extend(data), b"")[1])
    port = link_rtk.SerialPort(device.path, 57600, timeout=0.2)
    try:
        started = time.monotonic()
        assert port.read(8) == b""
        assert 0.15 < time.monotonic() - started < 1.0
        os.write(device.master, b"hello")
        assert port.read(10) == b"hello"
        os.write(device.master, b"abcdef")
        deadline = time.monotonic() + 2
        while port.in_waiting < 6 and time.monotonic() < deadline:
            time.sleep(0.01)
        assert port.in_waiting == 6
        port.reset_input_buffer()
        assert port.in_waiting == 0
        assert port.write(b"to the device") == 13
        port.flush()
        deadline = time.monotonic() + 2
        while len(received) < 13 and time.monotonic() < deadline:
            time.sleep(0.01)
        assert bytes(received) == b"to the device"
    finally:
        port.close()
    with pytest.raises(OSError):
        port.read(1)
    port.close()


def test_the_serial_port_refuses_what_is_not_there() -> None:
    with pytest.raises(OSError) as raised:
        link_rtk.SerialPort("/dev/corvus-no-such-device", 57600)
    assert raised.value.errno == 2


def test_the_serial_port_hands_a_large_write_to_the_far_end(devices: Any) -> None:
    received = bytearray()
    device = devices(lambda data: (received.extend(data), b"")[1])
    port = link_rtk.SerialPort(device.path, 115200, timeout=0.2, write_timeout=5.0)
    try:
        payload = bytes(range(256)) * 400
        assert port.write(payload) == len(payload)
        deadline = time.monotonic() + 5
        while len(received) < len(payload) and time.monotonic() < deadline:
            time.sleep(0.01)
        assert bytes(received) == payload
    finally:
        port.close()


# ----------------------------------------------------------------- SiK radio ----

class Radio:
    """A SiK radio: command mode after a guarded +++, an AT dialect, an EEPROM."""

    def __init__(self, escape_after: int = 0, silent: bool = False) -> None:
        self.local = {k: v["value"] for k, v in sik_config.parse_registers(ATI5_LOCAL).items()}
        self.remote = {k: v["value"] for k, v in sik_config.parse_registers(ATI5_REMOTE).items()}
        self.numbers = {k: v["number"] for k, v in sik_config.parse_registers(ATI5_LOCAL).items()}
        self.eeprom_local: dict | None = None
        self.eeprom_remote: dict | None = None
        self.commands: list[str] = []
        self.escapes = 0
        self.escape_after = escape_after
        self.silent = silent
        self.last = time.monotonic()
        self.buffer = b""
        self.command_mode = False

    def table(self, regs: dict) -> str:
        return "".join(f"S{self.numbers[n]}: {n}={v}\r\n" for n, v in regs.items()) + "OK\r\n"

    def handle(self, data: bytes) -> bytes:
        now = time.monotonic()
        quiet = now - self.last > 0.2
        self.last = now
        if self.silent:
            return b""
        if data == b"+++":
            self.escapes += 1
            if quiet and self.escapes > self.escape_after:
                self.command_mode = True
                return b"OK\r\n"
            return b""
        if not self.command_mode:
            return b""
        self.buffer += data
        out = b""
        while b"\r\n" in self.buffer:
            line, self.buffer = self.buffer.split(b"\r\n", 1)
            out += self.answer(line.decode().strip().upper())
        return out

    def answer(self, command: str) -> bytes:
        self.commands.append(command)
        remote = command.startswith("RT")
        regs = self.remote if remote else self.local
        body = command[2:]
        if body == "I5":
            return self.table(regs).encode()
        if body in ("I", "I2"):
            return b"SiK 2.0 on HM-TRP\r\nOK\r\n"
        if body == "I7":
            return b"L/R RSSI: 208/205  L/R noise: 41/38 pkts: 20 txe=0 rxe=0\r\n"
        if body.startswith("S") and "=" in body:
            number, value = body[1:].split("=")
            for name, n in self.numbers.items():
                if n == int(number):
                    regs[name] = int(value)
            return b"OK\r\n"
        if body == "&W":
            if remote:
                self.eeprom_remote = dict(regs)
            else:
                self.eeprom_local = dict(regs)
            return b"OK\r\n"
        if body == "&F":
            for name in regs:
                regs[name] = 0
            return b"OK\r\n"
        if body == "Z":
            if not remote:
                self.command_mode = False
            return b"OK\r\n"
        if body == "O":
            self.command_mode = False
            return b"OK\r\n"
        return b"OK\r\n"


@pytest.fixture
def quick_radio(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sik_service, "GUARD_S", 0.3)
    monkeypatch.setattr(sik_service, "IDLE_S", 0.1)
    monkeypatch.setattr(sik_service, "REBOOT_S", 0.1)
    monkeypatch.setattr(sik_service, "LOCAL_TIMEOUT_S", 1.0)
    monkeypatch.setattr(sik_service, "REMOTE_TIMEOUT_S", 2.0)


class Store:
    def get_snapshot(self) -> dict:
        return {"armed": False}


def test_a_sik_radio_is_read_through_the_real_port(devices: Any, quick_radio: None) -> None:
    radio = Radio()
    device = devices(radio.handle)
    loaded = sik_service.SikService(None, Store()).load(device.path, 57600)
    assert loaded["baud"] == 57600 and loaded["remote_reachable"] is True
    assert [(m["name"], m["local"], m["remote"]) for m in loaded["mismatches"]] == [("NETID", 25, 30)]
    assert loaded["local"]["version"] == "SiK 2.0 on HM-TRP"
    assert loaded["link"]["local_dbm"] == pytest.approx(-17.5, abs=0.1)
    assert radio.commands[-1] == "ATO"


def test_a_sik_save_writes_the_far_radio_first_and_commits_both(devices: Any, quick_radio: None) -> None:
    radio = Radio()
    device = devices(radio.handle)
    result = sik_service.SikService(None, Store()).save(
        device.path, 57600, local={"NETID": 31, "TXPOWER": 14}, remote={"NETID": 31})
    assert result["applied"]["remote"] == [{"name": "NETID", "register": 3, "value": 31}]
    assert [w["name"] for w in result["applied"]["local"]] == ["NETID", "TXPOWER"]
    writes = [c for c in radio.commands if c.startswith(("RTS", "ATS", "RT&", "AT&", "RTZ", "ATZ"))]
    assert writes == ["RTS3=31", "RT&W", "RTZ", "ATS3=31", "ATS4=14", "AT&W", "ATZ"]
    assert radio.eeprom_remote["NETID"] == 31 and radio.eeprom_local["NETID"] == 31
    assert radio.eeprom_local["TXPOWER"] == 14


def test_a_sik_reset_asks_for_the_defaults_of_one_end_only(devices: Any, quick_radio: None) -> None:
    radio = Radio()
    device = devices(radio.handle)
    assert sik_service.SikService(None, Store()).reset(device.path, 57600, "local")["target"] == "local"
    assert "AT&F" in radio.commands and "RT&F" not in radio.commands
    assert radio.eeprom_local is not None and radio.eeprom_remote is None


def test_a_sik_radio_that_needs_a_second_escape_is_still_reached(devices: Any, quick_radio: None) -> None:
    radio = Radio(escape_after=1)
    device = devices(radio.handle)
    loaded = sik_service.SikService(None, Store()).load(device.path, 57600)
    assert radio.escapes >= 2 and loaded["local"]["values"]["NETID"] == 25


def test_a_silent_sik_port_is_reported_as_unreachable(devices: Any, quick_radio: None, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sik_config, "BAUD_CANDIDATES", (57600,))
    device = devices(Radio(silent=True).handle)
    with pytest.raises(sik_service.SikError, match="could not reach a radio"):
        sik_service.SikService(None, Store()).load(device.path, 57600)


# ------------------------------------------------------------ the bootloader ----

SIZES = {0x21: 2, 0x22: 3, 0x23: 2, 0x29: 2, 0x2C: 2, 0x30: 2}


def bootloader_on_pty(devices: Any, **options: Any) -> tuple[PtyDevice, Bootloader]:
    bootloader = Bootloader(**options)
    pending = bytearray()

    def handle(data: bytes) -> bytes:
        pending.extend(data)
        out = b""
        while pending:
            cmd = pending[0]
            need = SIZES.get(cmd) or (pending[1] + 3 if cmd == 0x27 and len(pending) > 1 else None)
            if need is None or len(pending) < need:
                break
            bootloader.write(bytes(pending[:need]))
            del pending[:need]
            out += bytes(bootloader.rbuf)
            bootloader.rbuf.clear()
        return out

    return devices(handle), bootloader


IMAGE = bytes((i * 31 + 7) & 0xFF for i in range(20000))


def flash(device: PtyDevice, image: bytes = IMAGE, board_id: int | None = 9, cancel: threading.Event | None = None) -> tuple[bool, fu.FirmwareUploader, list[str]]:
    states: list[str] = []
    uploader = fu.FirmwareUploader(on_progress=lambda p: states.append(p["state"]), connect_timeout_s=5.0, cancel=cancel)
    ok = uploader.run(device.path, image, board_id=board_id)
    return ok, uploader, [s for i, s in enumerate(states) if i == 0 or s != states[i - 1]]


def test_a_firmware_image_is_flashed_through_the_real_port(devices: Any) -> None:
    device, bootloader = bootloader_on_pty(devices, fw_maxsize=1 << 20, board_id=9)
    ok, uploader, states = flash(device)
    assert ok and uploader.status()["state"] == "done"
    assert states == ["connecting", "syncing", "erasing", "programming", "verifying", "booting", "done"]
    assert bytes(bootloader.programmed)[:len(IMAGE)] == IMAGE
    assert bootloader.rebooted and bootloader.erased == 1 and bootloader.block == 80
    assert uploader._serial is None


def test_a_firmware_for_another_board_is_refused_before_anything_is_erased(devices: Any) -> None:
    device, bootloader = bootloader_on_pty(devices, board_id=77)
    ok, uploader, _ = flash(device, board_id=9)
    assert not ok and bootloader.erased == 0 and not bootloader.rebooted
    assert "Nothing was erased" in uploader.status()["message"]


def test_a_firmware_that_fails_its_crc_is_not_booted(devices: Any) -> None:
    device, bootloader = bootloader_on_pty(devices, crc="bad")
    ok, uploader, _ = flash(device)
    assert not ok and not bootloader.rebooted
    assert uploader.status()["message"] == "firmware CRC mismatch, not booting"


def test_a_failing_erase_leaves_the_board_in_its_bootloader(devices: Any) -> None:
    device, bootloader = bootloader_on_pty(devices, erase="fail")
    ok, uploader, _ = flash(device)
    assert not ok and bootloader.block == 0 and not bootloader.rebooted
    assert uploader.status()["message"] == "erase failed"


def test_a_cancel_during_programming_never_reboots(devices: Any) -> None:
    cancel = threading.Event()
    device, bootloader = bootloader_on_pty(devices, cancel=cancel, cancel_at="block5")
    ok, uploader, _ = flash(device, cancel=cancel)
    assert not ok and uploader.status()["state"] == "cancelled"
    assert not bootloader.rebooted and 5 <= bootloader.block < 80


def test_a_firmware_larger_than_the_flash_is_refused(devices: Any) -> None:
    device, bootloader = bootloader_on_pty(devices, fw_maxsize=4096)
    ok, uploader, _ = flash(device)
    assert not ok and bootloader.erased == 0
    assert "exceeds board flash limit" in uploader.status()["message"]


def test_a_device_that_is_not_there_is_reported(tmp_path: Any) -> None:
    uploader = fu.FirmwareUploader(connect_timeout_s=0.5)
    assert not uploader.run(str(tmp_path / "ttyACM9"), IMAGE)
    assert "bootloader device not available" in uploader.status()["message"]


# ----------------------------------------------------------------- RTK base ----

def receiver_on_pty(devices: Any, **options: Any) -> tuple[PtyDevice, FakeReceiver]:
    receiver = FakeReceiver(**options)

    def handle(data: bytes) -> bytes:
        receiver.write(data)
        return b""

    def tick() -> bytes:
        time.sleep(0.02)
        return receiver.read(4096)

    return devices(handle, tick), receiver


def base_service(device: PtyDevice, settings: dict | None = None) -> tuple[rtk_service.RtkService, FakeBridge]:
    bridge = FakeBridge()
    service = rtk_service.RtkService(
        bridge, FakeStore(), {"enabled": True, **(settings or {})},
        list_ports=lambda: [{"device": device.path, "description": "u-blox GNSS receiver", "hwid": "USB VID:PID=1546:01A9"}],
    )
    return service, bridge


def wait_for(predicate: Callable[[], bool], timeout: float = 10.0) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(0.02)
    return False


def test_an_rtk_base_on_a_real_port_is_surveyed_streamed_and_put_back(devices: Any) -> None:
    device, receiver = receiver_on_pty(devices)
    service, bridge = base_service(device)
    service.start()
    try:
        assert wait_for(lambda: service.status()["state"] == rtk_service.STATE_ACTIVE), service.status()["message"]
        assert wait_for(lambda: len(bridge.injected) >= 4)
        status = service.status()
        assert status["device"] == device.path and status["baud"] == rtk.BAUD_CANDIDATES[0]
        assert status["receiver"]["protocol"] == 27.11
        assert receiver.mode == 1 and receiver.streaming
        assert all(frame.startswith(b"\xd3") for frame in bridge.injected)
    finally:
        service.stop(timeout=5.0)
    assert not service.is_alive() and service.status()["state"] == "off"
    assert receiver.mode == 0, "the receiver was left in base mode"


def test_an_rtk_base_at_a_fixed_position_skips_the_survey(devices: Any) -> None:
    device, receiver = receiver_on_pty(devices)
    service, bridge = base_service(device, {"mode": "fixed", "fixed": {"latitude": 48.08, "longitude": 11.64, "altitude": 545.2, "accuracy": 0.02}})
    service.start()
    try:
        assert wait_for(lambda: service.status()["state"] == rtk_service.STATE_ACTIVE)
        assert receiver.mode == 2
        assert receiver.valset_items()[rtk.KEY_TMODE_LAT] == 480800000
    finally:
        service.stop(timeout=5.0)
    assert receiver.mode == 0


def test_a_base_that_never_answers_is_listened_to_and_reported_quiet(devices: Any, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(rtk_service, "LISTEN_TIMEOUT_S", 1.0)
    monkeypatch.setattr(rtk_service, "IDENTIFY_TIMEOUT_S", 0.3)
    device, receiver = receiver_on_pty(devices, identify=False)
    service, bridge = base_service(device, {"baud": 38400})
    service.start()
    try:
        assert wait_for(lambda: service.status()["state"] == rtk_service.STATE_ERROR, 10)
        assert "produced no corrections" in service.status()["error"]
        assert bridge.injected == []
    finally:
        service.stop(timeout=5.0)
    assert [w for w in receiver.writes if w[0] == rtk.UBX_CLASS_CFG] == []
