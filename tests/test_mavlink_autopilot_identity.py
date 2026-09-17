"""Which flight stack is on the other end, and what Corvus may claim about it.

Two things used to be wrong here, and both were the same mistake: naming the
autopilot something it is not.

MAV_AUTOPILOT_MAP was written with MAV_AUTOPILOT_RESERVED (1) skipped, so every
value from 2 up was shifted by one. An ArduPilot vehicle (3) was reported as
"PPZ", and OpenPilot (4) as "ARDUPILOTMEGA". That field is the one place the
aircraft says what it runs, and it is what any per-stack behaviour would have
to key off.

The mode list had the mirrored problem. pymavlink answers mode_mapping() in two
shapes: PX4 gets triples DO_SET_MODE can send, every other stack gets a flat
custom_mode integer it cannot. Corvus published the names either way, so an
ArduPilot pilot saw a full selector in which every entry came back refused —
and when the mapping was missing entirely, PX4's own mode list was offered to
whatever was actually connected.
"""
from __future__ import annotations

from types import SimpleNamespace as NS

import pytest
from pymavlink import mavutil as mv

from corvus.mavlink_bridge import (
    MAV_AUTOPILOT_MAP,
    PX4_AVAILABLE_MODES,
    MavlinkBridge,
)
from corvus.state_store import VehicleStateStore


@pytest.fixture
def bridge() -> MavlinkBridge:
    return MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")


def _with_mapping(bridge: MavlinkBridge, mapping: object) -> None:
    """Build the mode tables from what pymavlink would report for a vehicle."""
    bridge._conn = NS(mode_mapping=lambda: mapping)
    bridge._build_mode_mapping()


# ---------------------------------------------------------------------------
# MAV_AUTOPILOT_MAP
# ---------------------------------------------------------------------------

def test_every_autopilot_id_is_named_what_the_dialect_names_it() -> None:
    """The table is hand-written so a rename cannot silently follow a dialect
    change — which also means nothing but this test keeps it honest."""
    official = {
        getattr(mv.mavlink, name): name[len("MAV_AUTOPILOT_"):]
        for name in dir(mv.mavlink)
        if name.startswith("MAV_AUTOPILOT_")
        and name != "MAV_AUTOPILOT_ENUM_END"
        and isinstance(getattr(mv.mavlink, name), int)
    }
    wrong = {
        value: (named, official.get(value))
        for value, named in MAV_AUTOPILOT_MAP.items()
        if official.get(value) != named
    }
    assert not wrong, f"value: (Corvus says, dialect says) -> {wrong}"


def test_an_ardupilot_vehicle_is_called_ardupilot() -> None:
    """The regression itself, spelled out: 3 is ArduPilot, not PPZ."""
    assert MAV_AUTOPILOT_MAP[mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA] == "ARDUPILOTMEGA"
    assert MAV_AUTOPILOT_MAP[mv.mavlink.MAV_AUTOPILOT_PX4] == "PX4"
    assert MAV_AUTOPILOT_MAP[mv.mavlink.MAV_AUTOPILOT_PPZ] == "PPZ"


# ---------------------------------------------------------------------------
# Which modes get offered
# ---------------------------------------------------------------------------

def test_px4_modes_come_from_the_live_mapping(bridge: MavlinkBridge) -> None:
    _with_mapping(bridge, dict(mv.px4_map))
    offered = bridge.get_available_modes()
    assert "MANUAL" in offered and "MISSION" in offered
    assert bridge._mode_values["MANUAL"] == mv.px4_map["MANUAL"]


def test_a_missing_mapping_still_falls_back_to_px4(bridge: MavlinkBridge) -> None:
    """BUG 9's fallback: PX4 is the target, and a link that answers nothing
    must not leave the selector empty."""
    _with_mapping(bridge, None)
    assert bridge.get_available_modes() == PX4_AVAILABLE_MODES
    assert bridge._mode_values, "set_mode needs values, not just names"


def test_an_ardupilot_mapping_offers_no_modes_rather_than_unusable_ones(
    bridge: MavlinkBridge,
) -> None:
    """ArduPilot copter reports 27 real modes as flat integers. Every one of
    them would be refused by set_mode, so listing them fills the selector with
    buttons that cannot work."""
    ardupilot = mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR)
    assert ardupilot["AUTO"] == 3, "guard: pymavlink still reports flat ints"
    _with_mapping(bridge, ardupilot)
    assert bridge.get_available_modes() == []
    assert bridge._mode_values == {}


def test_an_ardupilot_vehicle_is_never_offered_px4s_modes(
    bridge: MavlinkBridge,
) -> None:
    """The worse half of the same bug: falling through to the PX4 list would
    name modes the connected aircraft has never heard of."""
    _with_mapping(bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR))
    assert "POSCTL" not in bridge.get_available_modes()


def test_reconnecting_to_px4_clears_the_unsupported_latch(
    bridge: MavlinkBridge,
) -> None:
    """The flag is per-link. A PX4 vehicle after an ArduPilot one must get its
    modes back."""
    _with_mapping(bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR))
    assert bridge.get_available_modes() == []
    _with_mapping(bridge, dict(mv.px4_map))
    assert "MANUAL" in bridge.get_available_modes()


# ---------------------------------------------------------------------------
# What set_mode says when it cannot
# ---------------------------------------------------------------------------

def test_a_refused_mode_on_a_non_px4_stack_says_why(bridge: MavlinkBridge) -> None:
    """"Unknown or unsupported PX4 mode: AUTO" is not true of a vehicle whose
    AUTO mode exists and is simply not sendable from here."""
    _with_mapping(bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR))
    bridge._connection_ready = lambda: True  # type: ignore[method-assign]
    assert bridge.set_mode("AUTO") is False
    error = bridge.get_last_command_error()
    assert "not supported on this autopilot" in error
    assert "Unknown" not in error


def test_an_unknown_name_on_px4_still_says_unknown(bridge: MavlinkBridge) -> None:
    """The other branch must keep its own message: a typo is not a stack
    mismatch."""
    _with_mapping(bridge, dict(mv.px4_map))
    bridge._connection_ready = lambda: True  # type: ignore[method-assign]
    assert bridge.set_mode("NOT_A_MODE") is False
    assert "Unknown or unsupported PX4 mode" in bridge.get_last_command_error()
