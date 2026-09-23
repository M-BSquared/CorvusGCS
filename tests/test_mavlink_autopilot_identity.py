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
custom_mode integer. Corvus published the names either way, so an ArduPilot
pilot saw a full selector in which every entry came back refused — and when the
mapping was missing entirely, PX4's own mode list was offered to whatever was
actually connected.

The first fix for that was to offer an ArduPilot pilot *nothing*, which was
honest and useless. The flat integer is not unsendable; it goes in param2 of
DO_SET_MODE with MAV_MODE_FLAG_CUSTOM_MODE_ENABLED in param1, and which table
it is read against depends on MAV_TYPE, because mode 4 is GUIDED on a copter
and ACRO on a plane. So the rule these tests now hold to is narrower and
stronger: a vehicle is offered its own modes, never another stack's, and never
a name it would refuse.
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


def _with_mapping(
    bridge: MavlinkBridge,
    mapping: object,
    autopilot: int = mv.mavlink.MAV_AUTOPILOT_PX4,
    mav_type: int = mv.mavlink.MAV_TYPE_QUADROTOR,
) -> None:
    """Build the mode tables as a heartbeat from that vehicle would build them.

    The autopilot id is half the input: the same flat mapping means one thing
    from an ArduPilot heartbeat and nothing at all from a stack Corvus has
    never met.
    """
    bridge._latch_dialect(autopilot, mav_type)
    bridge._conn = NS(mode_mapping=lambda: mapping)
    bridge._build_mode_mapping()


_APM = mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA
_CUSTOM = mv.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED


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


def test_the_live_px4_modes_keep_the_dialects_order(bridge: MavlinkBridge) -> None:
    """Manual first, automatic last: an alphabetical list opened with ACRO."""
    _with_mapping(bridge, dict(mv.px4_map))
    offered = bridge.get_available_modes()
    assert offered[0] == "MANUAL"
    known = [m for m in PX4_AVAILABLE_MODES if m in mv.px4_map]
    assert offered[:len(known)] == known
    assert offered[len(known):] == sorted(set(mv.px4_map) - set(known))


def test_a_live_mode_the_dialect_does_not_know_is_still_offered(
    bridge: MavlinkBridge,
) -> None:
    _with_mapping(bridge, dict(mv.px4_map, ZZ_CUSTOM=mv.px4_map["MANUAL"]))
    assert bridge.get_available_modes()[-1] == "ZZ_CUSTOM"


def test_a_missing_mapping_still_falls_back_to_px4(bridge: MavlinkBridge) -> None:
    """BUG 9's fallback: PX4 is the target, and a link that answers nothing
    must not leave the selector empty."""
    _with_mapping(bridge, None)
    assert bridge.get_available_modes() == PX4_AVAILABLE_MODES
    assert bridge._mode_values, "set_mode needs values, not just names"


def test_an_ardupilot_vehicle_is_offered_its_own_modes(
    bridge: MavlinkBridge,
) -> None:
    """The flat table is usable, and these are the modes an ArduPilot pilot
    actually flies."""
    ardupilot = mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR)
    assert ardupilot["AUTO"] == 3, "guard: pymavlink still reports flat ints"
    _with_mapping(bridge, ardupilot, autopilot=_APM)
    offered = bridge.get_available_modes()
    assert {"AUTO", "GUIDED", "LOITER", "RTL", "STABILIZE"} <= set(offered)
    assert bridge._mode_values["AUTO"] == (_CUSTOM, 3, 0)


def test_an_ardupilot_vehicle_is_never_offered_px4s_modes(
    bridge: MavlinkBridge,
) -> None:
    """The worse half of the original bug: falling through to the PX4 list
    would name modes the connected aircraft has never heard of."""
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR),
        autopilot=_APM,
    )
    offered = bridge.get_available_modes()
    assert "POSCTL" not in offered
    assert "ALTCTL" not in offered


def test_a_px4_vehicle_is_never_offered_ardupilots_modes(
    bridge: MavlinkBridge,
) -> None:
    _with_mapping(bridge, dict(mv.px4_map))
    assert "GUIDED" not in bridge.get_available_modes()


def test_an_ardupilot_mode_is_read_against_its_own_vehicle_table(
    bridge: MavlinkBridge,
) -> None:
    """custom_mode 4 is GUIDED on a copter and ACRO on a plane. One number,
    two aircraft, two different answers — which is the whole reason MAV_TYPE
    has to reach the decoder."""
    hb = NS(custom_mode=4, base_mode=_CUSTOM, type=mv.mavlink.MAV_TYPE_QUADROTOR)
    bridge._latch_dialect(_APM, mv.mavlink.MAV_TYPE_QUADROTOR)
    assert bridge._decode_mode(hb) == "GUIDED"
    plane = NS(custom_mode=4, base_mode=_CUSTOM, type=mv.mavlink.MAV_TYPE_FIXED_WING)
    bridge._latch_dialect(_APM, mv.mavlink.MAV_TYPE_FIXED_WING)
    assert bridge._decode_mode(plane) == "ACRO"


def test_an_ardupilot_mapping_survives_a_vehicle_with_no_built_in_table(
    bridge: MavlinkBridge,
) -> None:
    """A MAV_TYPE this module has never heard of still gets real modes, as long
    as pymavlink recognised the vehicle."""
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_SUBMARINE),
        autopilot=_APM, mav_type=mv.mavlink.MAV_TYPE_SUBMARINE,
    )
    assert "SURFACE" in bridge.get_available_modes()


def test_an_unrecognised_stack_offers_nothing_rather_than_a_guess(
    bridge: MavlinkBridge,
) -> None:
    """The case that is still genuinely unsupported: a flat mapping from a
    stack whose mode numbering Corvus does not know. Offering PX4's names, or
    ArduPilot's, would be a guess about somebody's aircraft."""
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR),
        autopilot=mv.mavlink.MAV_AUTOPILOT_GENERIC,
    )
    assert bridge.get_available_modes() == []
    assert bridge._mode_values == {}


def test_reconnecting_to_px4_clears_the_unsupported_latch(
    bridge: MavlinkBridge,
) -> None:
    """The flag is per-link. A PX4 vehicle after an unknown one must get its
    modes back."""
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR),
        autopilot=mv.mavlink.MAV_AUTOPILOT_GENERIC,
    )
    assert bridge.get_available_modes() == []
    _with_mapping(bridge, dict(mv.px4_map))
    assert "MANUAL" in bridge.get_available_modes()


# ---------------------------------------------------------------------------
# What set_mode sends, and what it says when it cannot
# ---------------------------------------------------------------------------

def test_an_ardupilot_mode_goes_out_as_a_flat_custom_mode(
    bridge: MavlinkBridge,
) -> None:
    """param1 carries the custom-mode flag and param2 the number. PX4's packed
    main/sub words would reach ArduPilot as mode 4-and-something and be
    refused."""
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR),
        autopilot=_APM,
    )
    sent: list[tuple[int, list[float]]] = []

    def _send(command: int, params: list[float] | None = None, **_: object) -> int:
        sent.append((command, list(params or [])))
        return mv.mavlink.MAV_RESULT_ACCEPTED

    bridge._connection_ready = lambda: True   # type: ignore[method-assign]
    bridge._send_command_and_wait = _send     # type: ignore[method-assign]
    assert bridge.set_mode("RTL") is True
    command, params = sent[0]
    assert command == mv.mavlink.MAV_CMD_DO_SET_MODE
    assert params[0] == float(_CUSTOM)
    assert params[1] == 6.0        # ArduPilot copter RTL
    assert params[2] == 0.0


def test_a_refused_mode_on_an_unknown_stack_says_why(bridge: MavlinkBridge) -> None:
    """"Unknown or unsupported PX4 mode: AUTO" is not true of a vehicle whose
    AUTO mode exists and is simply not sendable from here."""
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR),
        autopilot=mv.mavlink.MAV_AUTOPILOT_GENERIC,
    )
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


def test_an_unknown_name_on_ardupilot_is_named_for_ardupilot(
    bridge: MavlinkBridge,
) -> None:
    """The message used to say PX4 at an operator flying something else."""
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR),
        autopilot=_APM,
    )
    bridge._connection_ready = lambda: True  # type: ignore[method-assign]
    assert bridge.set_mode("NOT_A_MODE") is False
    error = bridge.get_last_command_error()
    assert "ArduPilot" in error and "PX4" not in error
