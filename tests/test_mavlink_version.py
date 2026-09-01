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

    def autopilot_version_request_send(self, target_system: int, target_component: int) -> None:
        self.version_requests += 1


class _FakeConn:
    def __init__(self) -> None:
        self.mav = _FakeMav()


class _FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type


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
