"""The Rust NSH shell engine against the Python shell it replaced.

``tests/reference/mavlink_shell_python.py`` keeps the shell as it was in
Python. Each test runs the same SERIAL_CONTROL streams, commands, terminal
writes and releases through a bridge on the reference and a bridge on
``corvus_link.shell.ShellEngine``, and compares the console lines, what the
terminal received, the backlog, every frame sent and the command errors.
"""
from __future__ import annotations

import random
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_shell_python as reference  # noqa: E402

M = mavutil.mavlink


class ReferenceBridge(reference.ShellMixin, MavlinkBridge):
    """A bridge whose shell is the Python reference."""


for _name in ("_shell_active", "_shell_raw", "_shell_buffer", "_shell_backlog"):
    setattr(ReferenceBridge, _name, None)


class Mav:
    def __init__(self, fail: bool = False) -> None:
        self.sent: list[tuple] = []
        self.fail = fail

    def serial_control_send(self, *args: Any) -> None:
        if self.fail:
            raise OSError("radio gone")
        self.sent.append(tuple(bytes(a) if isinstance(a, (bytes, bytearray)) else a for a in args))


def make(kind: type, stack: str = "px4", fail: bool = False) -> tuple[Any, list, Mav]:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    autopilot = M.MAV_AUTOPILOT_ARDUPILOTMEGA if stack == "ardupilot" else M.MAV_AUTOPILOT_PX4
    bridge._latch_dialect(autopilot, M.MAV_TYPE_QUADROTOR)
    mav = Mav(fail)
    bridge._conn = SimpleNamespace(mav=mav, source_system=255, source_component=190)
    console: list[tuple] = []
    bridge._console_publish = lambda name, text, level: console.append((name, text, level))
    return bridge, console, mav


def chunk(data: bytes, count: int | None = None, device: int = 10, **extra: Any) -> SimpleNamespace:
    payload = data[:70]
    fields = {"target_system": 255, "target_component": 190, **extra}
    return SimpleNamespace(
        get_type=lambda: "SERIAL_CONTROL", get_srcSystem=lambda: 1, get_srcComponent=lambda: 1,
        device=device, count=len(payload) if count is None else count,
        data=list(payload.ljust(70, b"\x00")), **fields)


def _stream(seed: int) -> list[SimpleNamespace]:
    rng = random.Random(seed)
    text = ("nsh> ls\r\n/bin\r\n/dev\r\ntop\b\bp\r\nüber €\r\n" * 6).encode()
    text += b"\xff\xfe broken \xe2\x82 cut\r\n" + b"x" * 300 + b"\r\n"
    out = []
    pos = 0
    while pos < len(text):
        step = rng.randint(1, 70)
        out.append(chunk(text[pos:pos + step]))
        pos += step
    out.insert(3, chunk(b"other device", device=3))
    out.insert(5, chunk(b"bad count", count=0))
    out.insert(7, chunk(b"too many", count=71))
    out.insert(9, chunk(b"elsewhere", target_system=9))
    return out


@pytest.fixture(autouse=True)
def small_limits(monkeypatch: pytest.MonkeyPatch) -> None:
    from corvus import mavlink_shell as rust_side
    for module in (reference, rust_side):
        monkeypatch.setattr(module, "SHELL_LINE_MAX_CHARS", 64)
        monkeypatch.setattr(module, "SHELL_BACKLOG_CHARS", 90)
        monkeypatch.setattr(module, "SHELL_WRITE_MAX_CHARS", 200)


@pytest.mark.parametrize("seed", range(5))
def test_the_console_reassembles_identically(seed: int) -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, console, mav = make(kind)
        assert bridge.send_shell_command("ls") is True
        for msg in _stream(seed):
            bridge._handle_serial_control(msg)
        results.append((console, bridge._shell_buffer, mav.sent, bridge.get_last_command_error()))
    assert results[1] == results[0]
    assert len(results[0][0]) > 10


@pytest.mark.parametrize("seed", range(5))
def test_the_terminal_gets_identical_text_and_backlog(seed: int) -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, console, mav = make(kind)
        heard: list[str] = []
        assert bridge.add_shell_sub(lambda entry, out=heard: out.append(entry["text"])) == ""
        assert bridge.write_shell("top\n") is True
        for msg in _stream(seed):
            bridge._handle_serial_control(msg)
        late = bridge.add_shell_sub(lambda entry: None)
        results.append((heard, late, bridge._shell_backlog, console, mav.sent))
    assert results[1] == results[0]


def test_switching_between_console_and_terminal_matches() -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, console, mav = make(kind)
        heard: list[str] = []
        bridge.add_shell_sub(lambda entry, out=heard: out.append(entry["text"]))
        bridge.send_shell_command("free")
        bridge._handle_serial_control(chunk(b"half a li"))
        bridge.write_shell("\x03")
        bridge._handle_serial_control(chunk(b"ne\r\nraw\x1b[2J"))
        bridge.send_shell_command("dmesg\n")
        bridge._handle_serial_control(chunk(b"boot\r\n"))
        bridge.stop_shell()
        bridge._handle_serial_control(chunk(b"after stop\r\n"))
        results.append((console, heard, bridge._shell_backlog, bridge._shell_active, bridge._shell_raw, mav.sent))
    assert results[1] == results[0]


@pytest.mark.parametrize("call", [
    ("send_shell_command", "x" * 150),
    ("send_shell_command", 42),
    ("send_shell_command", "ü\ud800 surrogate"),
    ("write_shell", ""),
    ("write_shell", "y" * 201),
    ("write_shell", "y" * 150),
    ("write_shell", None),
])
@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("fail", [False, True])
def test_sends_and_refusals_match(call: tuple, stack: str, fail: bool) -> None:
    method, argument = call
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, console, mav = make(kind, stack, fail)
        ok = getattr(bridge, method)(argument)
        results.append((ok, mav.sent, console, bridge.get_last_command_error(), bridge._shell_active))
    assert results[1] == results[0]


def test_a_shell_command_without_a_link_matches() -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, console, mav = make(kind)
        bridge._connection_ready = lambda: False
        results.append((bridge.send_shell_command("ls"), bridge.write_shell("ls\n"),
                        bridge.get_last_command_error(), mav.sent))
        bridge._conn = None
        bridge.stop_shell()
    assert results[1] == results[0]
