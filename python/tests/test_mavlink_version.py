"""AUTOPILOT_VERSION parsing and version-retry lifecycle tests.

Covers ``_decode_git_hash`` (PX4 ``flight_custom_version`` -> hex git hash)
and the ``_schedule_version_retry`` daemon-thread guard logic: it must not
spawn when ``_running`` is clear, must not double-spawn, must stay silent
after ``stop()``, and must skip when the version already arrived.
"""
from __future__ import annotations

import threading
from types import SimpleNamespace
from unittest.mock import patch

import pytest

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore


class _FakeMav:
    def __init__(self) -> None:
        self.version_requests = 0
        self.commands: list[tuple] = []

    def autopilot_version_request_send(self, target_system: int, target_component: int) -> None:
        self.version_requests += 1

    def command_long_send(self, *args) -> None:
        self.commands.append(args)


class _FakeConn:
    def __init__(self) -> None:
        self.mav = _FakeMav()


class _FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type


def _commanded(mav) -> set[int]:
    """The MAV_CMD ids sent as COMMAND_LONG (command is arg index 2)."""
    return {int(call[2]) for call in mav.commands}


# ---------------------------------------------------------------------------
# _request_version — asking in a way PX4 actually answers
# ---------------------------------------------------------------------------

def test_request_version_uses_the_commands_px4_answers() -> None:
    """The bare AUTOPILOT_VERSION_REQUEST message is not enough.

    PX4 does not handle message 183 at all — it is an ArduPilot-era legacy — so
    a GCS that only sends that never learns the firmware version. v1.16-v1.18
    answer MAV_CMD_REQUEST_MESSAGE, and older builds the now-deprecated
    MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES; both must go out.
    """
    from pymavlink import mavutil

    bridge = MavlinkBridge(VehicleStateStore())
    bridge._conn = _FakeConn()
    with patch.object(MavlinkBridge, "_schedule_version_retry", lambda self: None):
        bridge._request_version()

    commanded = _commanded(bridge._conn.mav)
    assert mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE in commanded
    assert mavutil.mavlink.MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES in commanded
    # REQUEST_MESSAGE must name AUTOPILOT_VERSION (148) in param1, or PX4
    # answers with some other message and the version stays blank.
    request = next(
        call for call in bridge._conn.mav.commands
        if int(call[2]) == mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE
    )
    assert int(request[4]) == 148
    # The legacy message still goes out for anything that only speaks that.
    assert bridge._conn.mav.version_requests == 1


def test_request_version_does_not_need_a_parameter_download() -> None:
    """The firmware version must not be coupled to the parameter set.

    Parameters are downloaded lazily, only when the operator opens the
    Parameters page — so anything that waits for them leaves the version blank
    for the whole flight. _request_version touches no parameter state.
    """
    store = VehicleStateStore()
    bridge = MavlinkBridge(store)
    bridge._conn = _FakeConn()
    with patch.object(MavlinkBridge, "_schedule_version_retry", lambda self: None):
        bridge._request_version()

    assert bridge._conn.mav.commands, "a version request went out"
    assert store.get_snapshot().get("params_received", 0) in (0, None), (
        "no parameter download was triggered"
    )


def test_one_failing_request_does_not_suppress_the_others() -> None:
    """A firmware that rejects one form must still be asked the other ways."""
    bridge = MavlinkBridge(VehicleStateStore())
    conn = _FakeConn()

    def boom(*_args, **_kwargs):
        raise RuntimeError("unsupported")

    conn.mav.command_long_send = boom  # type: ignore[method-assign]
    bridge._conn = conn
    with patch.object(MavlinkBridge, "_schedule_version_retry", lambda self: None):
        bridge._request_version()

    assert conn.mav.version_requests == 1, "the legacy request still went out"


# ---------------------------------------------------------------------------
# _decode_git_hash
# ---------------------------------------------------------------------------

def test_decode_git_hash_px4_le_wire_layout() -> None:
    raw = bytes([0, 0, 0, 0x97, 0x2e, 0x61, 0x49, 0x68])
    assert MavlinkBridge._decode_git_hash(raw) == "6849612e97"


def test_decode_git_hash_list_form() -> None:
    assert MavlinkBridge._decode_git_hash([0, 0, 0, 0x97, 0x2e, 0x61, 0x49, 0x68]) == "6849612e97"


def test_decode_git_hash_bytearray() -> None:
    raw = bytearray([0, 0, 0, 0x97, 0x2e, 0x61, 0x49, 0x68])
    assert MavlinkBridge._decode_git_hash(raw) == "6849612e97"


def test_decode_git_hash_big_endian_with_trailing_nul() -> None:
    raw = b"\x68\x49\x61\x2e\x97\x00\x00\x00"
    assert MavlinkBridge._decode_git_hash(raw) == "6849612e97"


def test_decode_git_hash_str_input() -> None:
    assert MavlinkBridge._decode_git_hash("AB") == "4142"


@pytest.mark.parametrize(
    "raw",
    [b"", b"\x00", b"\x00\x00", [], [0], [0] * 8, b"\x00" * 8, None, 42, 3.14],
)
def test_decode_git_hash_empty_or_all_zero_returns_empty(raw: object) -> None:
    assert MavlinkBridge._decode_git_hash(raw) == ""


# ---------------------------------------------------------------------------
# _schedule_version_retry — lifecycle guards
# ---------------------------------------------------------------------------

def test_version_retry_no_spawn_when_not_running() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert not bridge._running.is_set()
    bridge._schedule_version_retry()
    assert bridge._version_retry_thread is None


def test_version_retry_spawns_and_sends_when_running() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._running.set()
    bridge._conn = _FakeConn()
    with patch("corvus.mavlink_bridge.time.sleep", lambda _: None):
        bridge._schedule_version_retry()
        bridge._version_retry_thread.join(timeout=2)
    assert bridge._version_retry_thread is not None
    assert not bridge._version_retry_thread.is_alive()
    assert bridge._conn.mav.version_requests == 1


def test_version_retry_no_double_spawn() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._running.set()
    bridge._conn = _FakeConn()
    gate = threading.Event()
    with patch("corvus.mavlink_bridge.time.sleep", lambda _: gate.wait(timeout=2)):
        bridge._schedule_version_retry()
        first = bridge._version_retry_thread
        assert first is not None and first.is_alive()
        bridge._schedule_version_retry()
        assert bridge._version_retry_thread is first
        gate.set()
        first.join(timeout=2)
    assert not first.is_alive()


def test_version_retry_quiets_on_stop() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._running.set()
    bridge._conn = _FakeConn()

    def fake_sleep(_: float) -> None:
        bridge._running.clear()

    with patch("corvus.mavlink_bridge.time.sleep", fake_sleep):
        bridge._schedule_version_retry()
        bridge._version_retry_thread.join(timeout=2)
    assert not bridge._version_retry_thread.is_alive()
    assert bridge._conn.mav.version_requests == 0


def test_version_retry_skips_when_version_already_set() -> None:
    store = VehicleStateStore()
    store.update(px4_version="v1.18.0")
    bridge = MavlinkBridge(store)
    bridge._running.set()
    bridge._conn = _FakeConn()
    with patch("corvus.mavlink_bridge.time.sleep", lambda _: None):
        bridge._schedule_version_retry()
        bridge._version_retry_thread.join(timeout=2)
    assert bridge._conn.mav.version_requests == 0


def test_version_retry_no_send_when_conn_is_none() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._running.set()
    bridge._conn = None
    with patch("corvus.mavlink_bridge.time.sleep", lambda _: None):
        bridge._schedule_version_retry()
        bridge._version_retry_thread.join(timeout=2)
    assert not bridge._version_retry_thread.is_alive()


# ---------------------------------------------------------------------------
# AUTOPILOT_VERSION dispatch
# ---------------------------------------------------------------------------

def test_autopilot_version_dispatch_sets_both_fields() -> None:
    store = VehicleStateStore()
    bridge = MavlinkBridge(store)
    sw = (1 << 24) | (18 << 16) | (0 << 8)
    msg = _FakeMessage(
        message_type="AUTOPILOT_VERSION",
        flight_sw_version=sw,
        flight_custom_version=[0, 0, 0, 0x97, 0x2e, 0x61, 0x49, 0x68],
    )
    bridge._dispatch(msg)
    snap = store.get_snapshot()
    assert snap["px4_version"] == "v1.18.0"
    assert snap["px4_version_detail"] == "6849612e97"


def test_autopilot_version_dispatch_with_missing_custom_version() -> None:
    store = VehicleStateStore()
    bridge = MavlinkBridge(store)
    sw = (1 << 24) | (16 << 16) | (2 << 8)
    msg = _FakeMessage(
        message_type="AUTOPILOT_VERSION",
        flight_sw_version=sw,
    )
    bridge._dispatch(msg)
    snap = store.get_snapshot()
    assert snap["px4_version"] == "v1.16.2"
    assert snap["px4_version_detail"] == ""
