"""Tests for MAV_CMD_DO_SET_HOME — the map context menu's "Set home here".

Two things are pinned here that nothing else in the suite covers:

* the command goes out as **COMMAND_INT**, not COMMAND_LONG. COMMAND_LONG's
  params are float32, which holds ~7 significant digits: a latitude sent that
  way lands tens of metres from where the operator pointed. Every assertion
  about ``command_ints`` rather than ``commands`` is protecting that.
* the altitude is the **existing home reference**, never the click. A map
  gives no terrain height, and PX4 reads this command's z as AMSL, so a
  guessed value would move home vertically as a side effect of moving it
  sideways.

Mirrors the fakes in tests/test_mavlink_takeoff.py.
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Any, Callable

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)


class FakeMav:
    """Records both command wrappers so a test can assert which one was used."""

    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.commands: list[tuple] = []
        self.command_ints: list[tuple] = []
        self.on_send = on_send

    def command_long_send(self, *args: Any) -> None:
        self.commands.append(args)
        if self.on_send:
            self.on_send(args)

    def command_int_send(self, *args: Any) -> None:
        self.command_ints.append(args)
        if self.on_send:
            # args = (sys, comp, frame, cmd, current, autocontinue, p1..p4, x, y, z);
            # normalize to the (.., cmd at [2], ..) shape the COMMAND_LONG
            # callbacks use so one on_send can serve both wrappers.
            self.on_send((args[0], args[1], args[3]) + args[4:])


class FakeConnection:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.mav = FakeMav(on_send)
        self.source_system = 255
        self.source_component = 190


def ack(command: int, result: int, target_system: int = 255) -> FakeMessage:
    return FakeMessage(
        message_type="COMMAND_ACK",
        command=command,
        result=result,
        target_system=target_system,
        target_component=190,
        source_system=1,
    )


def ready_bridge(on_send: Callable[[tuple], None] | None = None) -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection(on_send)
    return bridge


def accepting_bridge(home_alt: float | None = 512.0) -> MavlinkBridge:
    """A bridge that ACKs every command, with a home altitude reference."""
    holder: dict[str, MavlinkBridge] = {}

    def on_send(args: tuple) -> None:
        holder["bridge"]._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge = ready_bridge(on_send)
    holder["bridge"] = bridge
    if home_alt is not None:
        bridge._home_alt_amsl = home_alt
    return bridge


# ---------------------------------------------------------------------------
# The wire format
# ---------------------------------------------------------------------------

def test_set_home_sends_command_int_not_command_long() -> None:
    bridge = accepting_bridge()

    assert bridge.set_home(48.0812345, 11.6405678)

    assert bridge._conn.mav.commands == [], (
        "COMMAND_LONG's float32 params cannot carry a coordinate accurately"
    )
    assert len(bridge._conn.mav.command_ints) == 1


def test_set_home_carries_the_exact_coordinate_as_deg_e7() -> None:
    bridge = accepting_bridge()

    assert bridge.set_home(48.0812345, 11.6405678)

    sent = bridge._conn.mav.command_ints[0]
    # (sys, comp, frame, cmd, current, autocontinue, p1..p4, x, y, z)
    assert sent[2] == mavutil.mavlink.MAV_FRAME_GLOBAL
    assert sent[3] == mavutil.mavlink.MAV_CMD_DO_SET_HOME
    assert sent[6] == 0.0, "param1=0 means 'use the specified location'"
    assert sent[10] == 480812345, "latitude survives as int32 deg x 1e7"
    assert sent[11] == 116405678
    # Round-tripping through float32 would lose the last digits entirely.
    assert sent[10] / 1e7 == pytest.approx(48.0812345, abs=1e-7)


def test_set_home_keeps_the_existing_home_altitude() -> None:
    bridge = accepting_bridge(home_alt=512.0)

    assert bridge.set_home(48.1, 11.6)

    assert bridge._conn.mav.command_ints[0][12] == pytest.approx(512.0), (
        "moving home sideways must not move it vertically"
    )


def test_set_home_falls_back_to_the_position_derived_reference() -> None:
    """No HOME_POSITION yet: AMSL minus AGL is the same reference takeoff uses."""
    bridge = accepting_bridge(home_alt=None)
    bridge._dispatch(FakeMessage(
        message_type="GLOBAL_POSITION_INT",
        lat=48_1000000, lon=11_6000000,
        alt=128_400,            # 128.4 m AMSL
        relative_alt=3_400,     # 3.4 m AGL
        hdg=0,
    ))

    assert bridge.set_home(48.2, 11.7)
    assert bridge._conn.mav.command_ints[0][12] == pytest.approx(125.0)


# ---------------------------------------------------------------------------
# Refusals
# ---------------------------------------------------------------------------

def test_set_home_refused_without_any_altitude_reference() -> None:
    bridge = accepting_bridge(home_alt=None)

    assert bridge.set_home(48.1, 11.6) is False
    assert "altitude reference" in bridge.get_last_command_error()
    assert bridge._conn.mav.command_ints == [], "nothing left the client"


@pytest.mark.parametrize("lat,lon", [
    (91.0, 11.6),
    (-90.5, 11.6),
    (48.1, 180.5),
    (48.1, -181.0),
    (float("nan"), 11.6),
    (48.1, float("inf")),
    ("48.1", 11.6),
    (None, 11.6),
    (True, 11.6),          # bool is an int in Python; must not fly as lat 1.0
    (48.1, False),
])
def test_set_home_rejects_bad_coordinates_without_sending(lat: Any, lon: Any) -> None:
    bridge = accepting_bridge()

    assert bridge.set_home(lat, lon) is False
    assert bridge._conn.mav.command_ints == []
    assert bridge._conn.mav.commands == []


def test_set_home_reports_a_px4_rejection() -> None:
    holder: dict[str, MavlinkBridge] = {}

    def on_send(args: tuple) -> None:
        holder["bridge"]._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge = ready_bridge(on_send)
    holder["bridge"] = bridge
    bridge._home_alt_amsl = 512.0

    assert bridge.set_home(48.1, 11.6) is False
    assert "DENIED" in bridge.get_last_command_error()


def test_set_home_requires_a_live_connection() -> None:
    bridge = accepting_bridge()
    bridge._conn = None

    assert bridge.set_home(48.1, 11.6) is False
    assert bridge.get_last_command_error()


# ---------------------------------------------------------------------------
# Armed behaviour — deliberately allowed
# ---------------------------------------------------------------------------

def test_set_home_is_allowed_while_armed() -> None:
    """Relocating home is how RTL is redirected in flight.

    The armed refusal applies to the preflight actions (set_param, calibrate,
    autotune), not to the in-flight commands — see
    tests/test_regression_armed_safety.py.
    """
    bridge = accepting_bridge()
    bridge._store.update(armed=True)

    assert bridge.set_home(48.1, 11.6)
    assert len(bridge._conn.mav.command_ints) == 1
