"""Virtual joystick: MANUAL_CONTROL on the wire and the HTTP frame in front of it.

The joystick is the only path in the app where an operator gesture becomes
aircraft movement without a COMMAND_ACK to check afterwards, so the two things
worth pinning are exactly the two that have no runtime feedback:

* the axis scaling that turns a normalized stick into int16 ticks, including
  what happens to values the browser should never have sent (NaN, out of
  range, the wrong type);
* the endpoint's refusal to forward any of those, and its neutral defaults,
  so a truncated payload flies a hover rather than a dive.

Also covered: ``controls.virtual_joystick`` round-trips through the config
file and the POST /api/config merge, since that switch is what puts the stick
on screen at all.
"""
from __future__ import annotations

import json
import pathlib
from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus import config as config_mod
from corvus.mavlink_bridge import MavlinkBridge
from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Bridge fakes (same shape as tests/test_mavlink_vibration.py)
# ---------------------------------------------------------------------------

class FakeMav:
    def __init__(self) -> None:
        self.frames: list[tuple] = []

    def manual_control_send(self, *args: int) -> None:
        self.frames.append(args)


class FakeConnection:
    def __init__(self) -> None:
        self.mav = FakeMav()
        self.source_system = 255
        self.source_component = 190


def ready_bridge() -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    return bridge


def sent(bridge: MavlinkBridge) -> list[tuple]:
    return bridge._conn.mav.frames


# ---------------------------------------------------------------------------
# Axis scaling
# ---------------------------------------------------------------------------

def test_manual_control_scales_axes_to_int16_ticks() -> None:
    bridge = ready_bridge()

    assert bridge.manual_control(x=1.0, y=-1.0, z=1.0, r=0.5) is True

    assert sent(bridge) == [(1, 1000, -1000, 1000, 500, 0)]


def test_manual_control_neutral_frame_is_centred_sticks_at_hover_thrust() -> None:
    bridge = ready_bridge()

    bridge.manual_control(x=0.0, y=0.0, z=0.5, r=0.0)

    assert sent(bridge) == [(1, 0, 0, 500, 0, 0)]


@pytest.mark.parametrize(("axis", "value", "expected"), [
    ("x", 4.0, 1000),          # over the top of the range
    ("x", -4.0, -1000),
    ("z", 9.0, 1000),          # thrust is 0..1, not -1..1
    ("z", -9.0, 0),
    ("r", float("nan"), 0),    # NaN would otherwise become an arbitrary int16
    ("y", "bogus", 0),
    ("y", None, 0),
])
def test_manual_control_clamps_rather_than_rejecting(axis: str, value: Any, expected: int) -> None:
    """A stick that overshoots by a rounding error must still fly."""
    bridge = ready_bridge()
    index = {"x": 1, "y": 2, "z": 3, "r": 4}[axis]
    # Every axis defaults to 0 here, thrust included: this asserts the clamp,
    # not the endpoint's neutral defaults.
    bridge.manual_control(**{axis: value})
    assert sent(bridge)[0][index] == expected


def test_manual_control_masks_buttons_and_ignores_non_integers() -> None:
    bridge = ready_bridge()

    bridge.manual_control(buttons=0x1FFFF)
    bridge.manual_control(buttons="all of them")   # type: ignore[arg-type]
    bridge.manual_control(buttons=True)            # bool is an int subclass

    assert [frame[5] for frame in sent(bridge)] == [0xFFFF, 0, 0]


def test_manual_control_refuses_to_send_while_disconnected() -> None:
    bridge = ready_bridge()
    bridge._conn = None

    assert bridge.manual_control(x=1.0) is False
    # Same wording as every other command failure, so the HTTP layer's
    # DISCONNECTED -> 503 mapping holds for this path too.
    assert "DISCONNECTED" in bridge.get_last_command_error()


def test_manual_control_reports_a_send_failure_without_raising() -> None:
    bridge = ready_bridge()

    def boom(*_args: int) -> None:
        raise OSError("link went away mid-frame")

    bridge._conn.mav.manual_control_send = boom  # type: ignore[method-assign]

    assert bridge.manual_control(x=0.2) is False
    assert "link went away mid-frame" in bridge.get_last_command_error()


def test_manual_control_does_not_wait_for_an_ack() -> None:
    """MANUAL_CONTROL has no ACK; a stream that waited for one would stall."""
    bridge = ready_bridge()

    def refuse(*_args: Any, **_kwargs: Any) -> int:
        raise AssertionError("manual control must not go through the ACK path")

    bridge._send_command_and_wait = refuse  # type: ignore[method-assign]

    assert bridge.manual_control(x=0.5) is True


def test_manual_control_is_not_a_command_long() -> None:
    bridge = ready_bridge()
    bridge.manual_control(x=0.5)
    assert not hasattr(bridge._conn.mav, "commands")


# ---------------------------------------------------------------------------
# POST /api/mavlink/manual
# ---------------------------------------------------------------------------

class FakeBridge:
    def __init__(self, result: bool = True, error: str = "") -> None:
        self.result = result
        self.error = error
        self.calls: list[dict[str, Any]] = []

    def manual_control(self, **kwargs: Any) -> bool:
        self.calls.append(kwargs)
        return self.result

    def get_last_command_error(self) -> str:
        return self.error


def handler_with_bridge(bridge: FakeBridge | None) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def test_manual_endpoint_forwards_a_full_frame() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_manual({"x": 0.25, "y": -0.5, "z": 0.75, "r": 1.0, "buttons": 3})

    assert bridge.calls == [{"x": 0.25, "y": -0.5, "z": 0.75, "r": 1.0, "buttons": 3}]
    assert responses == [({"ok": True}, 200)]


def test_manual_endpoint_defaults_a_missing_axis_to_neutral() -> None:
    """A truncated payload must fly a hover, never a dive: z defaults to 0.5."""
    bridge = FakeBridge()
    handler, _responses = handler_with_bridge(bridge)

    handler._api_mavlink_manual({})

    assert bridge.calls == [{"x": 0.0, "y": 0.0, "z": 0.5, "r": 0.0, "buttons": 0}]


@pytest.mark.parametrize("payload", [
    {"x": 1.5}, {"x": -1.5}, {"y": 2}, {"r": -9},
    {"z": -0.1}, {"z": 1.1},
    {"x": "0.5"}, {"x": None}, {"x": True}, {"x": float("nan")}, {"x": float("inf")},
])
def test_manual_endpoint_rejects_bad_axes(payload: dict[str, Any]) -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_manual(payload)

    assert bridge.calls == []
    assert responses[0][1] == 400


@pytest.mark.parametrize("buttons", [-1, 0x10000, "1", 1.0, True, None])
def test_manual_endpoint_rejects_bad_button_masks(buttons: Any) -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_manual({"buttons": buttons})

    assert bridge.calls == []
    assert responses[0][1] == 400


def test_manual_endpoint_maps_disconnected_to_503() -> None:
    handler, responses = handler_with_bridge(
        FakeBridge(False, "Manual control failed: DISCONNECTED"))

    handler._api_mavlink_manual({"x": 0.1})

    assert responses == [({"ok": False, "error": "Manual control failed: DISCONNECTED"}, 503)]


def test_manual_endpoint_maps_a_send_failure_to_409() -> None:
    handler, responses = handler_with_bridge(FakeBridge(False, "Manual control send failed: boom"))

    handler._api_mavlink_manual({"x": 0.1})

    assert responses[0][1] == 409


def test_manual_endpoint_without_a_bridge_reports_not_connected() -> None:
    handler, responses = handler_with_bridge(None)

    handler._api_mavlink_manual({"x": 0.1})

    assert responses == [({"error": "not connected"}, 400)]


# ---------------------------------------------------------------------------
# controls.virtual_joystick in the config
# ---------------------------------------------------------------------------

def test_controls_round_trip_through_the_config_file(tmp_path: pathlib.Path) -> None:
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"controls": {"virtual_joystick": True}}), encoding="utf-8")

    cfg = config_mod.load_config(str(path))

    assert cfg.controls == {"virtual_joystick": True}
    assert config_mod.to_public_dict(cfg)["controls"] == {"virtual_joystick": True}


def test_controls_defaults_to_absent(tmp_path: pathlib.Path) -> None:
    path = tmp_path / "config.json"
    path.write_text("{}", encoding="utf-8")

    cfg = config_mod.load_config(str(path))

    assert cfg.controls is None
    assert "controls" not in config_mod.to_public_dict(cfg)


@pytest.mark.parametrize("raw", [
    {"virtual_joystick": "true"},   # a truthy string must NOT arm the stick
    {"virtual_joystick": 1},
    {"virtual_joystick": None},
    {"arrow_keys": "yes"},
    {"wasd_keys": 1},               # nor a truthy int the throttle keys
    {"unknown_control": True},
    {},
    "on",
    None,
])
def test_only_a_real_boolean_enables_a_control(raw: Any) -> None:
    assert config_mod._coerce_controls(raw) is None


def test_every_control_switch_round_trips_independently(tmp_path: pathlib.Path) -> None:
    path = tmp_path / "config.json"
    stored = {"virtual_joystick": False, "arrow_keys": True, "wasd_keys": True}
    path.write_text(json.dumps({"controls": stored}), encoding="utf-8")

    cfg = config_mod.load_config(str(path))

    assert cfg.controls == stored


def test_a_control_key_that_is_absent_stays_absent() -> None:
    """Turning one surface on must not write a decision about the other."""
    assert config_mod._coerce_controls({"arrow_keys": True}) == {"arrow_keys": True}


def test_config_post_merges_controls_without_disturbing_the_rest() -> None:
    handler = object.__new__(CorvusHandler)
    handler.config = config_mod.CorvusConfig(theme={"name": "blue"})
    handler.config_path = None
    handler._live_config = lambda: handler.config  # type: ignore[method-assign]
    handler._save_live_config = lambda: None  # type: ignore[method-assign]

    public, error = handler._apply_config_partial({"controls": {"virtual_joystick": True}})

    assert error is None
    assert public is not None
    assert public["controls"] == {"virtual_joystick": True}
    assert public["theme"] == {"name": "blue"}


def test_config_post_merges_one_control_switch_without_clearing_the_other() -> None:
    """Each switch is independent, and the UI toggles one at a time.

    theme/map replace wholesale; controls must not, or turning the arrow keys
    on would silently switch the joystick off.
    """
    handler = object.__new__(CorvusHandler)
    handler.config = config_mod.CorvusConfig(controls={"virtual_joystick": True})
    handler.config_path = None
    handler._live_config = lambda: handler.config  # type: ignore[method-assign]
    handler._save_live_config = lambda: None  # type: ignore[method-assign]

    public, error = handler._apply_config_partial({"controls": {"arrow_keys": True}})

    assert error is None
    assert public is not None
    assert public["controls"] == {"virtual_joystick": True, "arrow_keys": True}


def test_config_post_can_still_switch_a_control_back_off() -> None:
    handler = object.__new__(CorvusHandler)
    handler.config = config_mod.CorvusConfig(
        controls={"virtual_joystick": True, "arrow_keys": True})
    handler.config_path = None
    handler._live_config = lambda: handler.config  # type: ignore[method-assign]
    handler._save_live_config = lambda: None  # type: ignore[method-assign]

    public, error = handler._apply_config_partial({"controls": {"arrow_keys": False}})

    assert error is None
    assert public is not None
    assert public["controls"] == {"virtual_joystick": True, "arrow_keys": False}


def test_config_post_rejects_a_non_object_controls_value() -> None:
    handler = object.__new__(CorvusHandler)
    handler.config = config_mod.CorvusConfig()
    handler.config_path = None
    handler._live_config = lambda: handler.config  # type: ignore[method-assign]

    public, error = handler._apply_config_partial({"controls": "on"})

    assert public is None
    assert error == "controls must be an object"
