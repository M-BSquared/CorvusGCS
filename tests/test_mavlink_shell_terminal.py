"""The PX4 NSH shell for a terminal: the NuttX Console plugin's backend.

``write_shell`` types into the shell exactly as given, and the replies reach
``add_shell_sub`` subscribers unchanged (escape sequences, carriage returns, a
prompt with no newline after it) instead of the MAVLink console. The console's
own ``send_shell_command`` keeps working and takes the shell back when used.

Also covers the HTTP side (``/api/mavlink/shell/send``, ``/close``) and the
``shell`` topic of ``/api/events``, including the shutdown path: the topic's
subscriber is removed when the stream ends, and ``stop_shell`` (which
``stop()`` calls) drops the terminal's backlog with the shell.
"""
from __future__ import annotations

import sys
from typing import Any
from collections.abc import Callable

from pymavlink import mavutil

from corvus import mavlink_shell
from corvus.mavlink_bridge import MavlinkBridge
from corvus.server import CorvusHandler, _BoundedSseBuffer
from corvus.state_store import VehicleStateStore

_TESTS_DIR = __import__("pathlib").Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

from test_mavlink_shell import (  # noqa: E402
    DEV_SHELL,
    FLAG_RESPOND_EXCLUSIVE_MULTI,
    FakeShellConn,
    _subscribe,
    ready_bridge,
    shell_reply,
)


def terminal_bridge() -> tuple[MavlinkBridge, list[str]]:
    """A connected PX4 bridge with a terminal attached; returns what it hears."""
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    heard: list[str] = []
    bridge.add_shell_sub(lambda entry: heard.append(entry["text"]))
    return bridge, heard


def ardupilot_bridge() -> MavlinkBridge:
    bridge = ready_bridge()
    bridge._latch_dialect(
        mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA, mavutil.mavlink.MAV_TYPE_QUADROTOR,
    )
    bridge._conn = FakeShellConn()
    return bridge


# ---------------------------------------------------------------------------
# write_shell: what goes out
# ---------------------------------------------------------------------------

def test_write_shell_sends_the_text_as_typed() -> None:
    bridge, _heard = terminal_bridge()

    assert bridge.write_shell("ver all\n") is True

    (device, flags, timeout, baudrate, count, data), = bridge._conn.mav.serial_sends
    assert device == DEV_SHELL
    assert flags == FLAG_RESPOND_EXCLUSIVE_MULTI
    assert (timeout, baudrate) == (0, 0)
    assert count == len(b"ver all\n")
    assert data == b"ver all\n".ljust(70, b"\x00")


def test_write_shell_adds_no_newline_so_ctrl_c_reaches_a_running_program() -> None:
    bridge, _heard = terminal_bridge()

    assert bridge.write_shell("\x03") is True

    (_d, _f, _t, _b, count, data), = bridge._conn.mav.serial_sends
    assert count == 1
    assert data[:1] == b"\x03"


def test_write_shell_splits_a_paste_into_70_byte_packets() -> None:
    bridge, _heard = terminal_bridge()
    text = "x" * 150

    assert bridge.write_shell(text) is True

    counts = [send[4] for send in bridge._conn.mav.serial_sends]
    assert counts == [70, 70, 10]
    assert b"".join(send[5][:send[4]] for send in bridge._conn.mav.serial_sends) == text.encode()


def test_write_shell_refuses_on_a_stack_without_a_shell() -> None:
    bridge = ardupilot_bridge()

    assert bridge.write_shell("ls\n") is False

    error = bridge.get_last_command_error()
    assert "ArduPilot" in error and "no NuttX shell" in error
    assert bridge._conn.mav.serial_sends == []


def test_write_shell_refuses_when_nothing_is_connected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())

    assert bridge.write_shell("ls\n") is False

    assert bridge.get_last_command_error() == "not connected"
    assert bridge._shell_active is False


def test_write_shell_refuses_empty_and_oversized_input() -> None:
    bridge, _heard = terminal_bridge()

    assert bridge.write_shell("") is False
    assert bridge.write_shell("x" * (mavlink_shell.SHELL_WRITE_MAX_CHARS + 1)) is False
    assert "too much at once" in bridge.get_last_command_error()
    assert bridge._conn.mav.serial_sends == []


def test_a_failed_send_reports_why() -> None:
    bridge, _heard = terminal_bridge()

    def broken(*_args: Any) -> None:
        raise OSError("serial port gone")

    bridge._conn.mav.serial_control_send = broken

    assert bridge.write_shell("ls\n") is False
    assert "serial port gone" in bridge.get_last_command_error()


# ---------------------------------------------------------------------------
# What comes back
# ---------------------------------------------------------------------------

def test_replies_reach_the_terminal_unchanged_and_not_the_console() -> None:
    bridge, heard = terminal_bridge()
    console = _subscribe(bridge)
    assert bridge.write_shell("top\n")

    bridge._dispatch(shell_reply(b"\x1b[2J\x1b[H\r\nnsh> "))

    assert heard == ["\x1b[2J\x1b[H\r\nnsh> "]
    assert [r for r in console if r["name"] == "SHELL"] == []


def test_a_character_split_across_two_packets_arrives_whole() -> None:
    bridge, heard = terminal_bridge()
    assert bridge.write_shell("\n")
    euro = "€".encode()

    bridge._dispatch(shell_reply(euro[:1]))
    bridge._dispatch(shell_reply(euro[1:]))

    assert "".join(heard) == "€"


def test_a_late_terminal_gets_the_backlog_and_nothing_twice() -> None:
    bridge, _heard = terminal_bridge()
    assert bridge.write_shell("\n")
    bridge._dispatch(shell_reply(b"nsh> "))

    late: list[str] = []
    backlog = bridge.add_shell_sub(lambda entry: late.append(entry["text"]))
    bridge._dispatch(shell_reply(b"ver all\r\n"))

    assert backlog == "nsh> "
    assert late == ["ver all\r\n"]


def test_the_backlog_is_trimmed_at_a_line_break() -> None:
    bridge, _heard = terminal_bridge()
    assert bridge.write_shell("\n")
    line = b"0123456789" * 6 + b"\r\n"          # 62 bytes, fits one packet
    for _ in range(mavlink_shell.SHELL_BACKLOG_CHARS // len(line) + 20):
        bridge._dispatch(shell_reply(line))

    backlog = bridge.add_shell_sub(lambda entry: None)

    assert len(backlog) <= mavlink_shell.SHELL_BACKLOG_CHARS
    assert backlog.startswith("0123456789")
    assert backlog.endswith("\r\n")


def test_one_failing_subscriber_does_not_cost_the_others() -> None:
    bridge, heard = terminal_bridge()

    def broken(_entry: dict[str, Any]) -> None:
        raise RuntimeError("closed tab")

    bridge.add_shell_sub(broken)
    assert bridge.write_shell("\n")

    bridge._dispatch(shell_reply(b"nsh> "))

    assert heard == ["nsh> "]


def test_a_removed_subscriber_hears_nothing_more() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    heard: list[str] = []

    def listener(entry: dict[str, Any]) -> None:
        heard.append(entry["text"])

    bridge.add_shell_sub(listener)
    bridge.remove_shell_sub(listener)
    bridge.remove_shell_sub(listener)            # twice is fine
    assert bridge.write_shell("\n")
    bridge._dispatch(shell_reply(b"nsh> "))

    assert heard == []


# ---------------------------------------------------------------------------
# Sharing the shell with the MAVLink console
# ---------------------------------------------------------------------------

def test_the_console_takes_the_shell_back_and_gets_lines_again() -> None:
    bridge, heard = terminal_bridge()
    console = _subscribe(bridge)
    assert bridge.write_shell("\n")
    bridge._dispatch(shell_reply(b"nsh> "))

    assert bridge.send_shell_command("free")
    bridge._dispatch(shell_reply(b"total used\r\n"))

    assert heard == ["nsh> "]
    assert [r["text"] for r in console if r["name"] == "SHELL"] == ["total used"]


def test_the_terminal_takes_the_shell_without_the_consoles_half_line() -> None:
    bridge, heard = terminal_bridge()
    console = _subscribe(bridge)
    assert bridge.send_shell_command("free")
    bridge._dispatch(shell_reply(b"half a li"))

    assert bridge.write_shell("\n")
    bridge._dispatch(shell_reply(b"nsh> "))
    assert bridge.send_shell_command("ver")
    bridge._dispatch(shell_reply(b"v1.16\n"))

    assert heard == ["nsh> "]
    assert [r["text"] for r in console if r["name"] == "SHELL"] == ["v1.16"]


# ---------------------------------------------------------------------------
# Closing (and the shutdown path)
# ---------------------------------------------------------------------------

def test_stop_shell_releases_and_forgets_the_terminal_session() -> None:
    bridge, heard = terminal_bridge()
    assert bridge.write_shell("\n")
    bridge._dispatch(shell_reply(b"nsh> "))
    bridge._conn.mav.serial_sends.clear()

    bridge.stop_shell()

    (_d, flags, _t, _b, count, _data), = bridge._conn.mav.serial_sends
    assert (flags, count) == (0, 0)
    assert bridge._shell_raw is False
    assert bridge.add_shell_sub(lambda entry: None) == ""
    bridge._dispatch(shell_reply(b"late\r\n"))
    assert heard == ["nsh> "]


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------

class FakeTerminalBridge:
    """Stand-in bridge recording what the shell endpoints and topic ask of it."""

    def __init__(self, result: bool = True, error: str = "", backlog: str = "") -> None:
        self.result = result
        self.error = error
        self.backlog = backlog
        self.writes: list[str] = []
        self.stops = 0
        self.subs: list[Callable[[dict[str, Any]], None]] = []

    def write_shell(self, data: str) -> bool:
        self.writes.append(data)
        return self.result

    def stop_shell(self) -> None:
        self.stops += 1

    def get_last_command_error(self) -> str:
        return self.error

    def add_shell_sub(self, fn: Callable[[dict[str, Any]], None]) -> str:
        self.subs.append(fn)
        return self.backlog

    def remove_shell_sub(self, fn: Callable[[dict[str, Any]], None]) -> None:
        self.subs.remove(fn)


def handler_with(bridge: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def test_send_endpoint_types_the_data_unchanged() -> None:
    bridge = FakeTerminalBridge()
    handler, responses = handler_with(bridge)

    handler._api_mavlink_shell_send({"data": "listener sensor_combined\n"})

    assert bridge.writes == ["listener sensor_combined\n"]
    assert responses == [({"ok": True}, 200)]


def test_send_endpoint_rejects_anything_but_text() -> None:
    bridge = FakeTerminalBridge()
    handler, responses = handler_with(bridge)

    for payload in ({}, {"data": ""}, {"data": 3}, {"data": ["ls"]}):
        handler._api_mavlink_shell_send(payload)

    assert bridge.writes == []
    assert [status for _body, status in responses] == [400, 400, 400, 400]


def test_send_endpoint_says_why_it_was_refused() -> None:
    handler, responses = handler_with(FakeTerminalBridge(False, "not connected"))
    handler._api_mavlink_shell_send({"data": "ls\n"})
    handler, refused = handler_with(FakeTerminalBridge(False, "ArduPilot has no NuttX shell."))
    handler._api_mavlink_shell_send({"data": "ls\n"})
    handler, no_bridge = handler_with(None)
    handler._api_mavlink_shell_send({"data": "ls\n"})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]
    assert refused == [({"ok": False, "error": "ArduPilot has no NuttX shell."}, 409)]
    assert no_bridge[0][1] == 503


def test_close_endpoint_releases_the_shell_and_is_fine_without_a_bridge() -> None:
    bridge = FakeTerminalBridge()
    handler, responses = handler_with(bridge)
    handler._api_mavlink_shell_close({})
    handler, quiet = handler_with(None)
    handler._api_mavlink_shell_close({})

    assert bridge.stops == 1
    assert responses == [({"ok": True}, 200)]
    assert quiet == [({"ok": True}, 200)]


def test_the_shell_topic_replays_then_streams_and_lets_go() -> None:
    bridge = FakeTerminalBridge(backlog="nsh> ")
    handler, _responses = handler_with(bridge)
    buf = _BoundedSseBuffer(16)

    unbind, initial = handler._bind_shell(buf)
    bridge.subs[0]({"text": "ver all\r\n"})

    assert initial == {"text": "nsh> ", "replay": True}
    assert buf.get(timeout=0) == {"text": "ver all\r\n"}
    unbind()
    assert bridge.subs == []


def test_the_shell_topic_is_on_the_multiplexed_stream() -> None:
    handler, _responses = handler_with(FakeTerminalBridge())
    assert "shell" in handler._sse_topics
    handler.mavlink = None
    unbind, initial = handler._bind_shell(_BoundedSseBuffer(4))
    unbind()
    assert initial == {"text": "", "replay": True}
