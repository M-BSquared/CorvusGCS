"""One tlog per flight: a disarm after a flight closes the file and opens the next.

Two sorties flown on one connection used to share a single tlog, so a log could
not be picked by flight. The rule is an armed to disarmed edge seen on this
connect cycle; a vehicle that is already disarmed when the link comes up, or a
heartbeat that repeats the same state, must not produce an empty file.
"""
from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace as NS

import pytest
from pymavlink import mavutil as mv

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore

_ARMED = mv.mavlink.MAV_MODE_FLAG_SAFETY_ARMED


class _Message(NS):
    def get_type(self) -> str:
        return self.message_type


def _heartbeat(armed: bool) -> _Message:
    return _Message(
        message_type="HEARTBEAT", type=mv.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mv.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=mv.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED | (_ARMED if armed else 0),
        custom_mode=3 << 16, system_status=4,
    )


@pytest.fixture
def bridge(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(
        "corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path / "logs"),
    )
    b = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")
    b._conn = NS(mode_mapping=lambda: dict(mv.px4_map))
    b._latch_dialect(mv.mavlink.MAV_AUTOPILOT_PX4, mv.mavlink.MAV_TYPE_QUADROTOR)
    b.version_requests = 0
    b._request_version = lambda: setattr(b, "version_requests", b.version_requests + 1)
    b._running.set()
    b._start_tlog()
    yield b
    b._running.clear()
    b._stop_tlog()


def _tlogs(tmp_path: Path) -> list[Path]:
    return sorted((tmp_path / "logs").glob("*.tlog"))


def test_a_disarm_after_a_flight_starts_a_new_tlog(bridge, tmp_path) -> None:
    first = bridge._tlog
    for armed in (False, True, True, False):
        bridge._dispatch(_heartbeat(armed))
    assert bridge._tlog is not None and bridge._tlog is not first
    assert len(_tlogs(tmp_path)) == 2
    assert bridge.version_requests == 1


def test_every_flight_gets_its_own_tlog(bridge, tmp_path) -> None:
    for armed in (True, False, True, False, False):
        bridge._dispatch(_heartbeat(armed))
    assert len(_tlogs(tmp_path)) == 3


def test_a_vehicle_that_stays_disarmed_keeps_one_tlog(bridge, tmp_path) -> None:
    first = bridge._tlog
    for _ in range(3):
        bridge._dispatch(_heartbeat(False))
    assert bridge._tlog is first
    assert len(_tlogs(tmp_path)) == 1
    assert bridge.version_requests == 0


def test_arming_alone_does_not_split_the_tlog(bridge, tmp_path) -> None:
    first = bridge._tlog
    bridge._dispatch(_heartbeat(False))
    bridge._dispatch(_heartbeat(True))
    assert bridge._tlog is first


def test_no_tlog_is_opened_when_logging_is_off(bridge, tmp_path) -> None:
    bridge._stop_tlog()
    for armed in (True, False):
        bridge._dispatch(_heartbeat(armed))
    assert bridge._tlog is None
    assert len(_tlogs(tmp_path)) == 1
