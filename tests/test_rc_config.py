"""Tests for the Radio Control schema, its endpoints, and RC telemetry.

Three layers, in the order a channel value travels through them:

1. :mod:`corvus.rc_config` — the schema and, more importantly, the calibration
   validator. That validator is the last thing between a mis-measured sweep and
   a set of endpoints PX4 will happily fly with, so most of the assertions here
   are about what it *refuses*.
2. ``GET /api/rc`` / ``POST /api/rc/stream`` / ``POST /api/rc/calibrate``.
3. The bridge's ``RC_CHANNELS`` handling, which is where the numbers the wizard
   measures come from in the first place.
"""

from __future__ import annotations

from typing import Any

import pytest

from corvus import rc_config
from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

def _values(**extra: float) -> dict[str, float]:
    """A plausible v1.18 answer: four sticks, a mode switch, eight channels."""
    values: dict[str, float] = {
        "COM_RC_IN_MODE": 0.0,
        "NAV_RCL_ACT": 2.0,
        "COM_RC_LOSS_T": 0.5,
        "RC_CHAN_CNT": 8.0,
        "RC_MAP_ROLL": 1.0,
        "RC_MAP_PITCH": 2.0,
        "RC_MAP_THROTTLE": 3.0,
        "RC_MAP_YAW": 4.0,
        "RC_MAP_FLTMODE": 5.0,
        "RC_MAP_KILL_SW": 6.0,
        "COM_FLTMODE1": 0.0,
        "COM_FLTMODE2": 2.0,
        "COM_FLTMODE6": -1.0,
    }
    for channel in range(1, 9):
        values[f"RC{channel}_MIN"] = 1100.0
        values[f"RC{channel}_MAX"] = 1900.0
        values[f"RC{channel}_TRIM"] = 1500.0
        values[f"RC{channel}_REV"] = 1.0
    values.update(extra)
    return values


def _section(doc: dict[str, Any], section_id: str) -> dict[str, Any] | None:
    return next((s for s in doc["sections"] if s["id"] == section_id), None)


class FakeRcBridge:
    """Stand-in for ``MavlinkBridge`` covering only what the RC routes call."""

    def __init__(self, values: dict[str, float] | None = None, *,
                 set_ok: bool = True, stream_ok: bool = True,
                 error: str = "") -> None:
        self.values = values if values is not None else _values()
        self.set_ok = set_ok
        self.stream_ok = stream_ok
        self.error = error
        self.set_calls: list[tuple[str, float]] = []
        self.stream_calls: list[tuple[bool, int]] = []
        self.fetch_calls: int = 0

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.fetch_calls += 1
        return {n: self.values[n] for n in names if n in self.values}

    def set_param(self, name: str, value: float) -> bool:
        self.set_calls.append((name, value))
        return self.set_ok

    def set_rc_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        self.stream_calls.append((enabled, rate_hz))
        return self.stream_ok

    def get_last_command_error(self) -> str:
        return self.error


def _handler(bridge: Any, store: VehicleStateStore | None = None):
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    handler.store = store  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


# ---------------------------------------------------------------------------
# Schema
# ---------------------------------------------------------------------------

def test_param_names_are_unique_and_cover_every_family() -> None:
    names = rc_config.param_names()
    assert len(names) == len(set(names)), "the batched read must not ask twice"
    for expected in ("RC_MAP_ROLL", "RC_MAP_FLTMODE", "COM_FLTMODE6",
                     "COM_RC_IN_MODE", "NAV_RCL_ACT", "RC18_TRIM"):
        assert expected in names


def test_build_returns_the_documented_sections() -> None:
    doc = rc_config.build(_values(), 8)
    assert [s["id"] for s in doc["sections"]] == [
        "input", "sticks", "modes", "switches", "channels",
    ]
    # No AUX parameter answered, so the AUX section is absent rather than empty.
    assert _section(doc, "aux") is None
    assert _section(doc, "channels")["kind"] == "channels"


def test_a_firmware_without_a_parameter_loses_one_field_not_the_page() -> None:
    values = _values()
    del values["RC_MAP_KILL_SW"]
    doc = rc_config.build(values, 8)
    assert _section(doc, "switches") is None, "the only switch is gone, so is its section"
    assert _section(doc, "sticks") is not None, "the rest of the page survives"


def test_channel_pickers_are_capped_at_what_the_receiver_delivers() -> None:
    doc = rc_config.build(_values(), 6)
    roll = next(f for f in _section(doc, "sticks")["fields"] if f["param"] == "RC_MAP_ROLL")
    channels = [o["value"] for o in roll["options"]]
    assert channels == [0, 1, 2, 3, 4, 5, 6]
    assert doc["channel_limit"] == 6


def test_an_enum_value_the_build_never_heard_of_is_kept_not_snapped() -> None:
    doc = rc_config.build(_values(NAV_RCL_ACT=99.0), 8)
    action = next(f for f in _section(doc, "input")["fields"]
                  if f["param"] == "NAV_RCL_ACT")
    assert {"value": 99, "label": "Unknown (99)"} in action["options"]


def test_a_channel_at_the_px4_defaults_reports_itself_uncalibrated() -> None:
    values = _values(RC7_MIN=1500.0, RC7_MAX=1560.0)
    rows = _section(rc_config.build(values, 8), "channels")["rows"]
    calibrated = {row["channel"]: row["calibrated"] for row in rows}
    assert calibrated[1] is True
    assert calibrated[7] is False, "60 us of travel is not a calibrated channel"


def test_reverse_is_read_from_the_sign_not_the_magnitude() -> None:
    rows = _section(rc_config.build(_values(RC2_REV=-1.0), 8), "channels")["rows"]
    assert rows[1]["reversed"] is True
    assert rows[0]["reversed"] is False


def test_assignments_name_every_bound_channel() -> None:
    doc = rc_config.build(_values(), 8)
    assert doc["assignments"]["RC_MAP_ROLL"] == 1
    assert doc["assignments"]["RC_MAP_KILL_SW"] == 6
    assert "RC_MAP_AUX1" not in doc["assignments"]


# ---------------------------------------------------------------------------
# Calibration validation — what it refuses matters more than what it writes
# ---------------------------------------------------------------------------

def _sweep(channel: int = 1, **over: Any) -> dict[str, Any]:
    entry = {"channel": channel, "min": 1100, "max": 1900, "trim": 1500}
    entry.update(over)
    return entry


def test_calibration_writes_are_ordered_endpoints_then_count_then_mapping() -> None:
    writes = rc_config.calibration_writes(
        [_sweep(1, reversed=True)], 8, None, {"RC_MAP_ROLL": 1})
    assert [w["name"] for w in writes] == [
        "RC1_MIN", "RC1_MAX", "RC1_TRIM", "RC1_REV", "RC_CHAN_CNT", "RC_MAP_ROLL",
    ]
    assert writes[3]["value"] == rc_config.REV_REVERSED


def test_a_channel_that_barely_moved_is_refused_by_name() -> None:
    with pytest.raises(rc_config.CalibrationError) as exc:
        rc_config.calibration_writes([_sweep(3, min=1500, max=1560, trim=1520)])
    assert "channel 3" in str(exc.value)


def test_a_centre_outside_its_own_travel_is_refused() -> None:
    with pytest.raises(rc_config.CalibrationError):
        rc_config.calibration_writes([_sweep(trim=2000)])


def test_an_implausible_pulse_is_refused() -> None:
    with pytest.raises(rc_config.CalibrationError):
        rc_config.calibration_writes([_sweep(min=100)])


def test_a_channel_measured_twice_is_refused() -> None:
    with pytest.raises(rc_config.CalibrationError):
        rc_config.calibration_writes([_sweep(1), _sweep(1)])


def test_reverse_is_only_written_when_it_was_actually_measured() -> None:
    writes = rc_config.calibration_writes([_sweep(2)])
    assert not any(w["name"] == "RC2_REV" for w in writes), (
        "a channel nobody was asked to move has no measured direction")


def test_a_parameter_the_firmware_lacks_is_dropped_not_sent() -> None:
    known = {"RC1_MIN", "RC1_MAX", "RC1_TRIM"}
    writes = rc_config.calibration_writes([_sweep(1, reversed=False)], 8, known)
    assert [w["name"] for w in writes] == ["RC1_MIN", "RC1_MAX", "RC1_TRIM"]


def test_mapping_is_limited_to_what_the_wizard_can_have_measured() -> None:
    with pytest.raises(rc_config.CalibrationError):
        rc_config.calibration_writes([_sweep(1)], None, None, {"RC_MAP_KILL_SW": 1})


def test_an_empty_measurement_is_refused() -> None:
    with pytest.raises(rc_config.CalibrationError):
        rc_config.calibration_writes([])


# ---------------------------------------------------------------------------
# GET /api/rc
# ---------------------------------------------------------------------------

def test_api_rc_without_a_bridge_is_200_and_says_not_connected() -> None:
    handler, responses = _handler(None)
    handler._api_rc()
    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert payload["sections"] == []


def test_api_rc_caps_the_pickers_at_the_reported_channel_count() -> None:
    store = VehicleStateStore()
    store.update(rc_channel_count=6)
    handler, responses = _handler(FakeRcBridge(), store)
    handler._api_rc()
    payload, _status = responses[0]
    assert payload["connected"] is True
    assert payload["channel_limit"] == 6


def test_api_rc_falls_back_to_eighteen_when_no_rc_has_arrived() -> None:
    handler, responses = _handler(FakeRcBridge(), VehicleStateStore())
    handler._api_rc()
    assert responses[0][0]["channel_limit"] == rc_config.MAX_CHANNELS


def test_api_rc_reports_a_silent_vehicle_without_failing() -> None:
    bridge = FakeRcBridge({}, error="no parameters received")
    handler, responses = _handler(bridge, VehicleStateStore())
    handler._api_rc()
    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert payload["error"] == "no parameters received"


# ---------------------------------------------------------------------------
# POST /api/rc/stream
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("payload", [
    {"enabled": "yes", "rate_hz": 20},
    {"enabled": True, "rate_hz": True},
    {"enabled": True, "rate_hz": 0},
    {"enabled": True, "rate_hz": 51},
    {"enabled": True, "rate_hz": 12.5},
])
def test_rc_stream_rejects_a_bad_request_before_the_link(payload: dict) -> None:
    bridge = FakeRcBridge()
    handler, responses = _handler(bridge)
    handler._api_rc_stream(payload)
    assert responses[0][1] == 400
    assert bridge.stream_calls == []


def test_rc_stream_forwards_the_rate() -> None:
    bridge = FakeRcBridge()
    handler, responses = _handler(bridge)
    handler._api_rc_stream({"enabled": True, "rate_hz": 20})
    assert bridge.stream_calls == [(True, 20)]
    assert responses[0] == ({"ok": True, "enabled": True, "rate_hz": 20}, 200)


def test_rc_stream_without_a_bridge_is_503() -> None:
    handler, responses = _handler(None)
    handler._api_rc_stream({"enabled": False})
    assert responses[0][1] == 503


# ---------------------------------------------------------------------------
# POST /api/rc/calibrate
# ---------------------------------------------------------------------------

def _armed_store(armed: bool) -> VehicleStateStore:
    store = VehicleStateStore()
    store.update(armed=armed)
    return store


def test_calibrate_writes_every_parameter_in_order() -> None:
    bridge = FakeRcBridge()
    handler, responses = _handler(bridge, _armed_store(False))
    handler._api_rc_calibrate({
        "channels": [_sweep(1, reversed=True), _sweep(2)],
        "count": 8,
        "mapping": {"RC_MAP_ROLL": 1},
    })
    payload, status = responses[0]
    assert status == 200 and payload["ok"] is True
    assert [name for name, _v in bridge.set_calls] == [
        "RC1_MIN", "RC1_MAX", "RC1_TRIM", "RC1_REV",
        "RC2_MIN", "RC2_MAX", "RC2_TRIM",
        "RC_CHAN_CNT", "RC_MAP_ROLL",
    ]


def test_calibrate_is_refused_while_armed_and_writes_nothing() -> None:
    bridge = FakeRcBridge()
    handler, responses = _handler(bridge, _armed_store(True))
    handler._api_rc_calibrate({"channels": [_sweep(1)], "count": 8})
    payload, status = responses[0]
    assert status == 409
    assert "armed" in payload["error"]
    assert bridge.set_calls == []


def test_a_rejected_measurement_reaches_no_parameter_at_all() -> None:
    bridge = FakeRcBridge()
    handler, responses = _handler(bridge, _armed_store(False))
    handler._api_rc_calibrate({
        "channels": [_sweep(1), _sweep(2, min=1500, max=1520, trim=1510)],
        "count": 8,
    })
    assert responses[0][1] == 400
    assert bridge.set_calls == [], (
        "all-or-nothing: a half-written endpoint set looks calibrated and is not")


def test_a_failed_write_reports_what_did_land() -> None:
    bridge = FakeRcBridge(set_ok=False, error="parameter write not confirmed (timeout)")
    handler, responses = _handler(bridge, _armed_store(False))
    handler._api_rc_calibrate({"channels": [_sweep(1)], "count": 8})
    payload, status = responses[0]
    assert status == 409
    assert payload["failed"] == "RC1_MIN"
    assert payload["applied"] == []


def test_calibrate_without_a_bridge_is_503() -> None:
    handler, responses = _handler(None, _armed_store(False))
    handler._api_rc_calibrate({"channels": [_sweep(1)]})
    assert responses[0][1] == 503
