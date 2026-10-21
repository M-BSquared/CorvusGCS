"""The downward distance sensor behind the top bar's altitude card.

It is opt-in all the way down: nothing asks the vehicle for DISTANCE_SENSOR
until the operator switches the reading on, only the downward sensor is a
height, a reading outside the sensor's own range is "nothing" and not a
distance, and a sensor that goes quiet stops being shown.
"""
from __future__ import annotations

import pytest
from pymavlink import mavutil

from corvus import mavlink_bridge
from corvus.mavlink_bridge import _rangefinder_distance_m
from test_mavlink_rc import FakeMessage, _accepting_bridge, ready_bridge


def distance_msg(current: int, orientation: int = 25, low: int = 20,
                 high: int = 1200) -> FakeMessage:
    return FakeMessage(
        message_type="DISTANCE_SENSOR", current_distance=current,
        orientation=orientation, min_distance=low, max_distance=high,
        source_system=1,
    )


def test_a_downward_reading_is_published_in_metres() -> None:
    bridge = ready_bridge()
    bridge._dispatch(distance_msg(1134))
    assert bridge._store.get_snapshot()["rangefinder_distance"] == 11.34


def test_a_sensor_that_does_not_look_down_is_not_a_height() -> None:
    bridge = ready_bridge()
    bridge._dispatch(distance_msg(500, orientation=0))
    assert bridge._store.get_snapshot()["rangefinder_distance"] is None


@pytest.mark.parametrize("current", [5, 1200, 5000])
def test_a_reading_outside_the_sensor_range_is_nothing(current: int) -> None:
    assert _rangefinder_distance_m(distance_msg(current)) is None


def test_a_quiet_sensor_stops_being_shown(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    clock = [100.0]
    monkeypatch.setattr(mavlink_bridge.time, "monotonic", lambda: clock[0])
    bridge._dispatch(distance_msg(300))
    assert bridge._store.get_snapshot()["rangefinder_distance"] == 3.0
    clock[0] += mavlink_bridge._RANGEFINDER_STALE_S + 1
    bridge._dispatch(FakeMessage(
        message_type="HEARTBEAT", type=2, autopilot=12, base_mode=0, custom_mode=0,
        system_status=4, source_system=1, get_srcComponent=lambda: 1))
    assert bridge._store.get_snapshot()["rangefinder_distance"] is None


def test_nothing_is_requested_until_asked() -> None:
    bridge = _accepting_bridge()
    ids = bridge._message_intervals()
    assert mavutil.mavlink.MAVLINK_MSG_ID_DISTANCE_SENSOR not in ids
    assert bridge._conn.mav.commands == []


def test_enabling_asks_for_two_hertz_and_disabling_hands_it_back() -> None:
    bridge = _accepting_bridge()
    assert bridge.set_rangefinder_stream(True) is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[4] == pytest.approx(float(mavutil.mavlink.MAVLINK_MSG_ID_DISTANCE_SENSOR))
    assert cmd[5] == pytest.approx(500000.0)
    bridge._dispatch(distance_msg(300))
    assert bridge.set_rangefinder_stream(False) is True
    assert bridge._conn.mav.commands[-1][5] == pytest.approx(0.0)
    assert bridge._store.get_snapshot()["rangefinder_distance"] is None


def test_a_dead_link_is_an_error_not_a_request() -> None:
    bridge = ready_bridge()
    bridge._conn = None
    assert bridge.set_rangefinder_stream(True) is False
    assert "not connected" in bridge.get_last_command_error()
