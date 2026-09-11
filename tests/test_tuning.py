"""The Setup -> PID Tuning schema, endpoint, and setpoint telemetry.

Three jobs, three risk profiles.

:mod:`corvus.tuning_config` owns which PX4 parameters make up each loop of the
control cascade. Its whole contract is graceful degradation: a firmware that
does not answer for a parameter loses a field, not the page, and the same
schema has to serve a multicopter and a fixed wing.

``GET /api/tuning`` must never break the page — a missing bridge, a bridge that
answers nothing, and a bridge that raises all come back as a renderable 200.

The setpoint telemetry is what makes the page readable at all: a response trace
with nothing to compare it against cannot tell an operator whether a gain is
too low or too high.
"""
from __future__ import annotations

import math
from typing import Any

import pytest

pytest.importorskip("pymavlink")

from pymavlink import mavutil  # noqa: E402

from corvus import tuning_config  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge, _quaternion_to_euler_deg  # noqa: E402
from corvus.server import CorvusHandler  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402


class FakeTuningBridge:
    """Bridge stand-in exposing only what the tuning endpoints touch."""

    def __init__(self, values: dict[str, float] | None = None,
                 error: str = "", raises: Exception | None = None,
                 stream_ok: bool = True) -> None:
        self.values = dict(values or {})
        self.error = error
        self.raises = raises
        self.stream_ok = stream_ok
        self.requested: list[list[str]] = []
        self.stream_calls: list[tuple[bool, int]] = []

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.requested.append(list(names))
        if self.raises is not None:
            raise self.raises
        return {n: v for n, v in self.values.items() if n in set(names)}

    def set_tuning_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        self.stream_calls.append((enabled, rate_hz))
        return self.stream_ok

    def get_last_command_error(self) -> str:
        return self.error


def _handler(bridge: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _multicopter_values() -> dict[str, float]:
    """A multicopter's answers: every MC_/MPC_ parameter, no FW_ ones."""
    return {
        name: 1.0 for name in tuning_config.param_names()
        if name.startswith(("MC_", "MPC_"))
    }


def _fixedwing_values() -> dict[str, float]:
    return {
        name: 1.0 for name in tuning_config.param_names()
        if name.startswith("FW_")
    }


def _groups(doc: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {g["id"]: g for g in doc["groups"]}


def _params_of(group: dict[str, Any]) -> set[str]:
    return {f["param"] for s in group["sections"] for f in s["fields"]}


# ---------------------------------------------------------------------------
# Schema: the parameter list
# ---------------------------------------------------------------------------

def test_param_names_are_unique() -> None:
    """The batched read must not ask twice for a parameter two groups share."""
    names = tuning_config.param_names()
    assert len(names) == len(set(names))


def test_param_names_cover_every_field_the_schema_can_emit() -> None:
    """Anything build() can render has to have been asked for.

    A field whose parameter is missing from the read list would never appear,
    however complete the schema looked.
    """
    names = set(tuning_config.param_names())
    doc = tuning_config.build({n: 1.0 for n in names})
    for group in doc["groups"]:
        for section in group["sections"]:
            for field in section["fields"]:
                assert field["param"] in names


# ---------------------------------------------------------------------------
# Schema: the split into loops
# ---------------------------------------------------------------------------

def test_a_multicopter_gets_the_whole_cascade_innermost_first() -> None:
    doc = tuning_config.build(_multicopter_values())
    assert [g["id"] for g in doc["groups"]] == [
        "rate", "attitude", "velocity", "position", "autotune",
    ]


def test_the_rate_group_is_split_per_axis() -> None:
    """Roll, pitch and yaw are three separate decisions, not one list of 18."""
    rate = _groups(tuning_config.build(_multicopter_values()))["rate"]
    assert [s["id"] for s in rate["sections"]] == ["roll", "pitch", "yaw"]
    assert _params_of(rate) >= {
        "MC_ROLLRATE_P", "MC_ROLLRATE_I", "MC_ROLLRATE_D",
        "MC_PITCHRATE_P", "MC_YAWRATE_P",
    }


def test_every_controller_group_is_editable_by_hand() -> None:
    """The point of the split: each loop carries its own writable gains.

    Autotune tunes two of these four loops and nothing tunes the other two, so
    a page that only offered an autotune button could not tune a vehicle.
    """
    groups = _groups(tuning_config.build(_multicopter_values()))
    for group_id in ("rate", "attitude", "velocity", "position"):
        fields = [f for s in groups[group_id]["sections"] for f in s["fields"]]
        assert fields, f"{group_id} has no editable field"
        assert all(f["param"] and f["kind"] == "number" for f in fields)


def test_a_fixed_wing_gets_its_own_rate_and_attitude_controllers() -> None:
    """One schema, two airframes: the FW parameters answer, the MC ones do not."""
    groups = _groups(tuning_config.build(_fixedwing_values()))
    assert set(groups) == {"rate", "attitude", "autotune"}
    assert "FW_RR_P" in _params_of(groups["rate"])
    assert not any(p.startswith("MC_") for p in _params_of(groups["rate"]))
    # No multicopter position/velocity controller on a fixed wing.
    assert "velocity" not in groups and "position" not in groups


def test_a_firmware_that_answers_nothing_yields_no_groups() -> None:
    doc = tuning_config.build({})
    assert doc["groups"] == []
    assert doc["received"] == 0


def test_a_missing_parameter_costs_one_field_not_the_group() -> None:
    values = _multicopter_values()
    del values["MC_ROLLRATE_D"]
    rate = _groups(tuning_config.build(values))["rate"]
    params = _params_of(rate)
    assert "MC_ROLLRATE_D" not in params
    assert "MC_ROLLRATE_P" in params


def test_an_empty_section_is_dropped_rather_than_rendered_blank() -> None:
    values = {n: 1.0 for n in _multicopter_values()
              if not n.startswith("MC_YAWRATE") and n != "MC_YR_INT_LIM"}
    rate = _groups(tuning_config.build(values))["rate"]
    assert [s["id"] for s in rate["sections"]] == ["roll", "pitch"]


# ---------------------------------------------------------------------------
# Schema: charts
# ---------------------------------------------------------------------------

def test_every_controller_chart_pairs_a_setpoint_with_a_response() -> None:
    """A lone response trace cannot tell a lagging gain from a ringing one."""
    groups = _groups(tuning_config.build(_multicopter_values()))
    for group_id in ("rate", "attitude", "velocity"):
        charts = groups[group_id]["charts"]
        assert charts
        for chart in charts:
            assert chart["actual"] and chart["setpoint"]


def test_chart_fields_exist_in_the_telemetry_snapshot() -> None:
    """The frontend reads these straight off the state snapshot by name."""
    snapshot = VehicleStateStore().get_snapshot()
    for group in tuning_config.build(_multicopter_values())["groups"]:
        for chart in group["charts"]:
            assert chart["actual"] in snapshot
            if chart["setpoint"]:
                assert chart["setpoint"] in snapshot


def test_charts_are_copies_so_a_caller_cannot_edit_the_module_table() -> None:
    first = tuning_config.build(_multicopter_values())
    _groups(first)["rate"]["charts"][0]["title"] = "mutated"
    second = tuning_config.build(_multicopter_values())
    assert _groups(second)["rate"]["charts"][0]["title"] == "Roll rate"


# ---------------------------------------------------------------------------
# Schema: autotune
# ---------------------------------------------------------------------------

def test_the_autotune_group_states_its_in_flight_preconditions() -> None:
    """PX4's own refusal says nothing useful; the page has to say it instead."""
    autotune = _groups(tuning_config.build(_multicopter_values()))["autotune"]
    assert autotune["kind"] == "autotune"
    assert autotune["family"] == "MC"
    joined = " ".join(autotune["steps"]).lower()
    assert "hover" in joined and "take off" in joined


def test_the_autotune_group_reports_whether_the_module_is_switched_on() -> None:
    values = _multicopter_values()
    values["MC_AT_EN"] = 0.0
    assert _groups(tuning_config.build(values))["autotune"]["enabled"] is False
    values["MC_AT_EN"] = 1.0
    assert _groups(tuning_config.build(values))["autotune"]["enabled"] is True


def test_a_firmware_without_the_autotune_module_offers_no_autotune() -> None:
    """Compiled-out autotune shows as absent, not as a button that always fails."""
    values = {n: v for n, v in _multicopter_values().items() if "_AT_" not in n}
    assert "autotune" not in _groups(tuning_config.build(values))


def test_only_the_fixed_wing_autotune_offers_an_axis_selection() -> None:
    """PX4's multicopter autotune always tunes all three axes."""
    mc = _groups(tuning_config.build(_multicopter_values()))["autotune"]
    fw = _groups(tuning_config.build(_fixedwing_values()))["autotune"]
    assert "FW_AT_AXES" not in _params_of(mc)
    assert "FW_AT_AXES" in _params_of(fw)


def test_an_unknown_enum_value_is_preserved_rather_than_snapped() -> None:
    """Rewriting a real aircraft's apply-policy on an unrelated edit is worse
    than showing a number this build does not recognise."""
    values = _multicopter_values()
    values["MC_AT_APPLY"] = 9.0
    autotune = _groups(tuning_config.build(values))["autotune"]
    field = next(f for s in autotune["sections"] for f in s["fields"]
                 if f["param"] == "MC_AT_APPLY")
    assert any(int(o["value"]) == 9 for o in field["options"])


# ---------------------------------------------------------------------------
# GET /api/tuning
# ---------------------------------------------------------------------------

def test_a_connected_multicopter_returns_the_whole_cascade() -> None:
    bridge = FakeTuningBridge(_multicopter_values())
    handler, responses = _handler(bridge)

    handler._api_tuning()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is True
    assert [g["id"] for g in payload["groups"]] == [
        "rate", "attitude", "velocity", "position", "autotune",
    ]
    assert bridge.requested == [tuning_config.param_names()]


def test_no_bridge_still_renders_the_page() -> None:
    handler, responses = _handler(None)

    handler._api_tuning()

    assert responses == [({"connected": False, "groups": [], "received": 0}, 200)]


def test_a_bridge_that_answers_nothing_reports_the_reason() -> None:
    bridge = FakeTuningBridge({}, error="not connected")
    handler, responses = _handler(bridge)

    handler._api_tuning()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert payload["error"] == "not connected"


def test_a_raising_bridge_never_500s_the_page() -> None:
    bridge = FakeTuningBridge(raises=RuntimeError("link exploded"))
    handler, responses = _handler(bridge)

    handler._api_tuning()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert "link exploded" in payload["error"]


# ---------------------------------------------------------------------------
# POST /api/tuning/stream
# ---------------------------------------------------------------------------

def test_the_tuning_stream_is_enabled_at_the_requested_rate() -> None:
    bridge = FakeTuningBridge()
    handler, responses = _handler(bridge)

    handler._api_tuning_stream({"enabled": True, "rate_hz": 25})

    assert bridge.stream_calls == [(True, 25)]
    assert responses == [({"ok": True, "enabled": True, "rate_hz": 25}, 200)]


def test_disabling_the_tuning_stream_hands_the_rate_back() -> None:
    bridge = FakeTuningBridge()
    handler, responses = _handler(bridge)

    handler._api_tuning_stream({"enabled": False})

    assert bridge.stream_calls == [(False, 20)]


@pytest.mark.parametrize("payload", [
    {},
    {"enabled": "yes"},
    {"enabled": True, "rate_hz": 0},
    {"enabled": True, "rate_hz": 51},
    {"enabled": True, "rate_hz": True},
    {"enabled": True, "rate_hz": 2.5},
])
def test_the_tuning_stream_validates_before_it_reaches_the_link(payload: dict) -> None:
    bridge = FakeTuningBridge()
    handler, responses = _handler(bridge)

    handler._api_tuning_stream(payload)

    assert bridge.stream_calls == []
    assert responses[0][1] == 400


def test_the_tuning_stream_without_a_bridge_returns_503() -> None:
    handler, responses = _handler(None)

    handler._api_tuning_stream({"enabled": True})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


# ---------------------------------------------------------------------------
# Setpoint telemetry
# ---------------------------------------------------------------------------

class _FakeMessage:
    def __init__(self, message_type: str, **fields: Any) -> None:
        self._type = message_type
        for key, value in fields.items():
            setattr(self, key, value)

    def get_type(self) -> str:
        return self._type


def _bridge() -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    return bridge


def _connected_bridge() -> MavlinkBridge:
    """A bridge the stream routes will talk to.

    set_tuning_stream refuses before it sends anything unless a link is up, the
    same way set_rc_stream does, so a test that stubs set_message_interval on a
    link-less bridge would be asserting against a path the page never reaches.
    The object only has to be truthy: nothing here sends on it.
    """
    bridge = _bridge()
    bridge._conn = object()
    return bridge


def test_attitude_target_publishes_body_rate_setpoints_in_deg_s() -> None:
    bridge = _bridge()
    bridge._dispatch(_FakeMessage(
        "ATTITUDE_TARGET",
        q=[1.0, 0.0, 0.0, 0.0],
        body_roll_rate=1.0,          # 1 rad/s ≈ 57.3 deg/s
        body_pitch_rate=-0.5,
        body_yaw_rate=0.25,
    ))
    snap = bridge._store.get_snapshot()
    assert snap["rollspeed_sp"] == pytest.approx(57.3, abs=0.1)
    assert snap["pitchspeed_sp"] == pytest.approx(-28.6, abs=0.1)
    assert snap["yawspeed_sp"] == pytest.approx(14.3, abs=0.1)
    assert snap["setpoints_live"] is True


def test_attitude_target_publishes_the_attitude_setpoint_in_the_same_frame() -> None:
    """Setpoint and response share a scale, or the graph compares nothing."""
    bridge = _bridge()
    roll, pitch, yaw = math.radians(20.0), math.radians(-10.0), math.radians(45.0)
    cr, sr = math.cos(roll / 2), math.sin(roll / 2)
    cp, sp = math.cos(pitch / 2), math.sin(pitch / 2)
    cy, sy = math.cos(yaw / 2), math.sin(yaw / 2)
    quat = [
        cr * cp * cy + sr * sp * sy,
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
    ]
    bridge._dispatch(_FakeMessage(
        "ATTITUDE_TARGET", q=quat,
        body_roll_rate=0.0, body_pitch_rate=0.0, body_yaw_rate=0.0,
    ))
    snap = bridge._store.get_snapshot()
    assert snap["roll_sp"] == pytest.approx(20.0, abs=0.2)
    assert snap["pitch_sp"] == pytest.approx(-10.0, abs=0.2)
    assert snap["yaw_sp"] == pytest.approx(45.0, abs=0.2)


def test_a_short_quaternion_still_yields_the_rate_setpoints() -> None:
    """Half a message is better than none, and better than a guessed angle."""
    bridge = _bridge()
    bridge._dispatch(_FakeMessage(
        "ATTITUDE_TARGET", q=[1.0, 0.0],
        body_roll_rate=1.0, body_pitch_rate=0.0, body_yaw_rate=0.0,
    ))
    snap = bridge._store.get_snapshot()
    assert snap["rollspeed_sp"] == pytest.approx(57.3, abs=0.1)
    assert snap["roll_sp"] == 0.0


def test_a_malformed_attitude_target_is_dropped_not_guessed_at() -> None:
    bridge = _bridge()
    bridge._dispatch(_FakeMessage("ATTITUDE_TARGET", q=[1.0, 0.0, 0.0, 0.0]))
    assert bridge._store.get_snapshot()["setpoints_live"] is False


def test_an_unnormalised_quaternion_does_not_raise() -> None:
    """A lossy link can deliver a quaternion a hair outside the asin domain."""
    roll, pitch, yaw = _quaternion_to_euler_deg(0.7072, 0.0, 0.7072, 0.0)
    assert pitch == pytest.approx(90.0, abs=0.1)
    assert math.isfinite(roll) and math.isfinite(yaw)


def test_position_target_publishes_the_velocity_setpoint() -> None:
    bridge = _bridge()
    bridge._dispatch(_FakeMessage(
        "POSITION_TARGET_LOCAL_NED", vx=1.5, vy=-2.25, vz=0.5))
    snap = bridge._store.get_snapshot()
    assert (snap["vx_sp"], snap["vy_sp"], snap["vz_sp"]) == (1.5, -2.25, 0.5)
    assert snap["setpoints_live"] is True


def test_global_position_publishes_velocity_components_beside_groundspeed() -> None:
    """The controller commands components; groundspeed is only a magnitude."""
    bridge = _bridge()
    bridge._dispatch(_FakeMessage(
        "GLOBAL_POSITION_INT",
        lat=481000000, lon=116000000, alt=128400, relative_alt=3400, hdg=0,
        vx=150, vy=-225, vz=50,      # cm/s
    ))
    snap = bridge._store.get_snapshot()
    assert (snap["vx"], snap["vy"], snap["vz"]) == (1.5, -2.25, 0.5)


def test_a_truncated_global_position_costs_a_trace_not_the_position() -> None:
    bridge = _bridge()
    bridge._dispatch(_FakeMessage(
        "GLOBAL_POSITION_INT",
        lat=481000000, lon=116000000, alt=128400, relative_alt=3400, hdg=0,
    ))
    snap = bridge._store.get_snapshot()
    assert snap["altitude_agl"] == 3.4
    assert snap["vx"] == 0.0


def test_the_tuning_stream_raises_every_message_the_page_plots() -> None:
    bridge = _connected_bridge()
    sent: list[tuple[int, int]] = []
    bridge.set_message_interval = (  # type: ignore[method-assign]
        lambda msg_id, interval_us: sent.append((msg_id, interval_us)) or True)

    assert bridge.set_tuning_stream(True, 20) is True

    assert [m for m, _i in sent] == list(MavlinkBridge._TUNING_MSG_IDS)
    assert {i for _m, i in sent} == {50_000}


def test_disabling_the_tuning_stream_restores_the_firmware_default() -> None:
    bridge = _connected_bridge()
    sent: list[tuple[int, int]] = []
    bridge.set_message_interval = (  # type: ignore[method-assign]
        lambda msg_id, interval_us: sent.append((msg_id, interval_us)) or True)
    bridge._store.update(setpoints_live=True)

    assert bridge.set_tuning_stream(False) is True

    # 0 hands the rate back to PX4 rather than to a number this build picked.
    assert {i for _m, i in sent} == {0}
    assert bridge._store.get_snapshot()["setpoints_live"] is False


def test_the_tuning_stream_says_not_connected_rather_than_failing_vaguely() -> None:
    """A dead link is the common case for this route, so it names itself.

    set_message_interval clears the command error and then returns False on its
    own readiness check without setting one, so without the guard in
    set_tuning_stream the route reads an empty error and answers 409 "tuning
    stream request failed" — a conflict — for what is simply no vehicle. The
    server turns "not connected" into a 503.
    """
    bridge = _bridge()                      # deliberately no link
    sent: list[tuple[int, int]] = []
    bridge.set_message_interval = (  # type: ignore[method-assign]
        lambda msg_id, interval_us: sent.append((msg_id, interval_us)) or True)

    assert bridge.set_tuning_stream(True, 20) is False
    assert sent == []
    assert bridge.get_last_command_error() == "not connected"
    assert bridge._store.get_snapshot()["setpoints_live"] is False


@pytest.mark.parametrize("rate", [0, 51, True, "20"])
def test_the_tuning_stream_rejects_a_rate_it_cannot_honour(rate: Any) -> None:
    bridge = _bridge()
    sent: list[tuple[int, int]] = []
    bridge.set_message_interval = (  # type: ignore[method-assign]
        lambda msg_id, interval_us: sent.append((msg_id, interval_us)) or True)

    assert bridge.set_tuning_stream(True, rate) is False
    assert sent == []


def test_the_tuning_stream_is_not_refused_while_armed() -> None:
    """It is used in flight — that is the only time a setpoint trace exists."""
    bridge = _connected_bridge()
    bridge._store.update(
        armed=True, landed_state=mavutil.mavlink.MAV_LANDED_STATE_IN_AIR)
    bridge.set_message_interval = lambda msg_id, interval_us: True  # type: ignore[method-assign]

    assert bridge.set_tuning_stream(True, 20) is True
    assert "armed" not in bridge.get_last_command_error()
