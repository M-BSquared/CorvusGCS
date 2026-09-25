"""The telemetry behind the top bar's GPS and Battery cards.

GPS: the accuracy and DOP fields GPS_RAW_INT carries past the fix, the
autopilot's own receiver health from SYS_STATUS, and the jamming and spoofing
verdicts from GNSS_INTEGRITY. That last one is not in the dialect pymavlink
loads, so it arrives as an UNKNOWN_441 frame and is decoded by hand; what is
pinned here is that an intact frame decodes, a damaged one does not, and a
frame from another aircraft is ignored.

Battery: Corvus's own time-to-empty, which exists only as a fallback for an
autopilot that publishes none, and only when there is enough of a flight to
fit a line through.
"""
from __future__ import annotations

import io
import os
from types import SimpleNamespace

import pytest

pytest.importorskip("pymavlink")
os.environ.setdefault("MAVLINK20", "1")

# Built from the MAVLink 2 dialect directly: another test may already have
# loaded mavutil with MAVLink 1, which has no h_acc/v_acc to set.
from pymavlink.dialects.v20 import ardupilotmega as mav2  # noqa: E402
from pymavlink.dialects.v20 import development  # noqa: E402

from corvus import battery  # noqa: E402
from corvus.mavlink_bridge import (  # noqa: E402
    MavlinkBridge,
    _gps_accuracy_m,
    _sensor_health,
)
from corvus.mavlink_telemetry import _decode_gnss_integrity  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402


def _bridge() -> tuple[MavlinkBridge, VehicleStateStore]:
    store = VehicleStateStore()
    return MavlinkBridge(store), store


def _integrity_frame(system: int = 1, jamming: int = 3, spoofing: int = 1,
                     signal: int = 7, receiver: int = 0) -> bytes:
    mav = development.MAVLink(io.BytesIO(), srcSystem=system, srcComponent=1)
    msg = development.MAVLink_gnss_integrity_message(
        receiver, 0, 0, jamming, spoofing, 0, 65535, 65535, 255, 255, signal, 255)
    return bytes(msg.pack(mav))


def _unknown(frame: bytes) -> SimpleNamespace:
    return SimpleNamespace(get_type=lambda: "UNKNOWN_441", data=frame,
                           get_srcSystem=lambda: 0, get_srcComponent=lambda: 0)


# ---------------------------------------------------------------- drain rate

def test_drain_endurance_needs_a_minute_and_a_point_of_drop() -> None:
    assert battery.drain_endurance([]) == -1
    # 30 s of samples: too short a line to read a rate off.
    assert battery.drain_endurance([(t, 80 - t / 10) for t in range(30)]) == -1
    # Two minutes, flat: not falling, so no answer rather than "forever".
    assert battery.drain_endurance([(t, 80.0) for t in range(120)]) == -1


def test_drain_endurance_extrapolates_the_recent_rate() -> None:
    # 1 % every 10 s from 80 %: 0.1 %/s, and 68 % left at t=120 -> 680 s.
    samples = [(float(t), 80 - t / 10) for t in range(121)]
    assert battery.drain_endurance(samples) == pytest.approx(680, abs=2)


def test_drain_endurance_forgets_what_is_older_than_the_window() -> None:
    # A steep climb out, then a gentle cruise. The cruise sets the rate.
    climb = [(float(t), 100 - t * 0.5) for t in range(60)]
    cruise = [(float(t), 70 - (t - 60) * 0.05) for t in range(60, 400)]
    seconds = battery.drain_endurance(climb + cruise)
    remaining = 70 - 339 * 0.05
    assert seconds == pytest.approx(remaining / 0.05, rel=0.02)


def test_endurance_only_while_armed_and_restarts_on_a_source_change() -> None:
    bridge, _store = _bridge()
    fields = {}
    for t in range(121):
        fields = bridge._battery_endurance_fields(80 - t / 10, "autopilot", True, now=float(t))
    assert fields["battery_endurance_est"] > 0

    # The operator switching to the voltage estimate starts a new series.
    after = bridge._battery_endurance_fields(60.0, "estimate", True, now=121.0)
    assert after["battery_endurance_est"] == -1

    # Disarmed: nothing, and the series is gone.
    assert bridge._battery_endurance_fields(60.0, "estimate", False, now=122.0) == {
        "battery_endurance_est": -1}
    assert not bridge._drain_samples


def test_sys_status_publishes_endurance_only_when_armed() -> None:
    bridge, store = _bridge()
    msg = mav2.MAVLink_sys_status_message(0, 0, 0, 0, 22200, 1000, 70, 0, 0, 0, 0, 0, 0)
    bridge._dispatch(msg)
    assert store.get_snapshot()["battery_endurance_est"] == -1


# ---------------------------------------------------------------- GPS

def test_gps_raw_int_publishes_accuracy_and_vdop() -> None:
    bridge, store = _bridge()
    msg = mav2.MAVLink_gps_raw_int_message(
        0, 3, 0, 0, 0, 80, 140, 0, 0, 14, 0, 900, 1500, 0, 0, 0)
    bridge._dispatch(msg)
    snap = store.get_snapshot()
    assert snap["gps_hdop"] == 0.8
    assert snap["gps_vdop"] == 1.4
    assert snap["gps_h_acc"] == 0.9
    assert snap["gps_v_acc"] == 1.5


def test_accuracy_zero_means_not_reported() -> None:
    assert _gps_accuracy_m(0) == -1
    assert _gps_accuracy_m(None) == -1
    assert _gps_accuracy_m(2500) == 2.5


def test_receiver_health_from_sys_status() -> None:
    bit = mav2.MAV_SYS_STATUS_SENSOR_GPS
    ok = SimpleNamespace(onboard_control_sensors_present=bit,
                         onboard_control_sensors_enabled=bit,
                         onboard_control_sensors_health=bit)
    bad = SimpleNamespace(onboard_control_sensors_present=bit,
                          onboard_control_sensors_enabled=bit,
                          onboard_control_sensors_health=0)
    absent = SimpleNamespace(onboard_control_sensors_present=0,
                             onboard_control_sensors_enabled=0,
                             onboard_control_sensors_health=0)
    assert _sensor_health(ok, bit) == "ok"
    assert _sensor_health(bad, bit) == "fault"
    assert _sensor_health(absent, bit) == ""


def test_gnss_integrity_frame_decodes() -> None:
    decoded = _decode_gnss_integrity(_integrity_frame(system=7))
    assert decoded == {"system": 7, "id": 0, "jamming_state": 3,
                       "spoofing_state": 1, "gnss_signal_quality": 7}


def test_a_damaged_gnss_integrity_frame_is_dropped() -> None:
    frame = bytearray(_integrity_frame())
    frame[-3] ^= 0xFF
    assert _decode_gnss_integrity(bytes(frame)) is None
    assert _decode_gnss_integrity(b"") is None
    assert _decode_gnss_integrity(b"\xfe" + bytes(20)) is None


def test_jamming_reaches_the_store_through_the_dispatch() -> None:
    bridge, store = _bridge()
    bridge._target_system = 1
    bridge._dispatch(_unknown(_integrity_frame(system=1, jamming=3, spoofing=2, signal=4)))
    snap = store.get_snapshot()
    assert snap["gps_jamming"] == "detected"
    assert snap["gps_spoofing"] == "mitigated"
    assert snap["gps_signal_quality"] == 4


def test_another_aircraft_or_receiver_does_not_speak_for_this_one() -> None:
    bridge, store = _bridge()
    bridge._target_system = 1
    bridge._dispatch(_unknown(_integrity_frame(system=2, jamming=3)))
    bridge._dispatch(_unknown(_integrity_frame(system=1, jamming=3, receiver=1)))
    assert store.get_snapshot()["gps_jamming"] == ""


def test_unknown_states_are_not_reported_rather_than_ok() -> None:
    bridge, store = _bridge()
    bridge._target_system = 1
    bridge._dispatch(_unknown(_integrity_frame(jamming=0, spoofing=0, signal=255)))
    snap = store.get_snapshot()
    assert snap["gps_jamming"] == ""
    assert snap["gps_spoofing"] == ""
    assert snap["gps_signal_quality"] == -1
