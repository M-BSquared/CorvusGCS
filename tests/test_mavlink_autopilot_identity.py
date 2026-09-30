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

from corvus import autopilot as autopilot_dialect
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
    known = [m for m in PX4_AVAILABLE_MODES if m in offered]
    assert offered == known


def test_a_live_mode_the_dialect_does_not_know_is_still_offered(
    bridge: MavlinkBridge,
) -> None:
    _with_mapping(bridge, dict(mv.px4_map, ZZ_CUSTOM=mv.px4_map["MANUAL"]))
    assert bridge.get_available_modes()[-1] == "ZZ_CUSTOM"


def test_a_missing_mapping_still_falls_back_to_px4(bridge: MavlinkBridge) -> None:
    """BUG 9's fallback: PX4 is the target, and a link that answers nothing
    must not leave the selector empty."""
    _with_mapping(bridge, None)
    offered = bridge.get_available_modes()
    assert offered == [m for m in PX4_AVAILABLE_MODES if m in offered]
    assert {"MANUAL", "POSCTL", "LOITER", "MISSION", "RTL"} <= set(offered)
    assert set(bridge._mode_values) == set(offered), "set_mode needs values, not just names"


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


# ---------------------------------------------------------------------------
# PX4 modes the connected release will not fly, checked against
# px4_custom_mode.h and Commander.cpp at v1.16.2, v1.17.0 and v1.18.0-rc1
# ---------------------------------------------------------------------------

class _Message(NS):
    def get_type(self) -> str:
        return self.message_type


def _report_px4(bridge: MavlinkBridge, major: int, minor: int, patch: int) -> None:
    """AUTOPILOT_VERSION as PX4 packs it: one byte each, type in the low byte."""
    bridge._dispatch(_Message(
        message_type="AUTOPILOT_VERSION",
        flight_sw_version=(major << 24) | (minor << 16) | (patch << 8) | 0xFF,
    ))


def _capture_commands(bridge: MavlinkBridge) -> list[tuple[int, list[float]]]:
    sent: list[tuple[int, list[float]]] = []

    def _send(command: int, params: list[float] | None = None, **_: object) -> int:
        sent.append((command, list(params or [])))
        return mv.mavlink.MAV_RESULT_ACCEPTED

    bridge._connection_ready = lambda: True   # type: ignore[method-assign]
    bridge._send_command_and_wait = _send     # type: ignore[method-assign]
    return sent


@pytest.mark.parametrize("name", ["RTGS", "RATTITUDE"])
def test_px4_modes_pymavlink_still_lists_are_neither_offered_nor_sent(
    bridge: MavlinkBridge, name: str,
) -> None:
    """px4_map still carries both. RTGS is refused on every target; RATTITUDE
    is ACKed by v1.16 and v1.17 and changes nothing, so an operator reaching
    for it would be told the mode change worked."""
    assert name in mv.px4_map, "guard: pymavlink still lists it"
    _with_mapping(bridge, dict(mv.px4_map))
    _report_px4(bridge, 1, 18, 0)
    assert name not in bridge.get_available_modes()
    sent = _capture_commands(bridge)
    assert bridge.set_mode(name) is False
    assert sent == []


def test_altitude_cruise_waits_for_a_release_that_has_it(bridge: MavlinkBridge) -> None:
    """Main mode 11 arrives in v1.17. v1.16's handler has no branch for it and
    ACKs the request unchanged, so it is offered only once the version says so,
    and refused before that without anything being sent."""
    _with_mapping(bridge, dict(mv.px4_map))
    sent = _capture_commands(bridge)
    assert "ALTITUDE_CRUISE" not in bridge.get_available_modes()
    assert bridge.set_mode("ALTITUDE_CRUISE") is False

    _report_px4(bridge, 1, 16, 2)
    assert "ALTITUDE_CRUISE" not in bridge.get_available_modes()
    assert bridge.set_mode("ALTITUDE_CRUISE") is False
    assert sent == []

    _report_px4(bridge, 1, 17, 0)
    offered = bridge.get_available_modes()
    assert offered.index("ALTITUDE_CRUISE") == offered.index("ALTCTL") + 1
    assert bridge.get_mode_labels()["ALTITUDE_CRUISE"] == "ALTITUDE CRUISE"
    assert bridge.set_mode("ALTITUDE_CRUISE") is True
    command, params = sent[0]
    assert command == mv.mavlink.MAV_CMD_DO_SET_MODE
    assert params[:3] == [81.0, 11.0, 0.0]


def test_position_slow_goes_out_as_posctl_sub_mode_two(bridge: MavlinkBridge) -> None:
    """Before v1.15 the same request lands in plain POSCTL and is ACKed."""
    _with_mapping(bridge, dict(mv.px4_map))
    _report_px4(bridge, 1, 14, 3)
    assert "POSITION_SLOW" not in bridge.get_available_modes()

    _report_px4(bridge, 1, 16, 0)
    assert bridge.get_mode_labels()["POSITION_SLOW"] == "POSITION SLOW"
    sent = _capture_commands(bridge)
    assert bridge.set_mode("POSITION_SLOW") is True
    assert sent[0][1][:3] == [81.0, 3.0, 2.0]


def test_a_new_link_forgets_the_last_vehicles_release(bridge: MavlinkBridge) -> None:
    """A PX4 v1.17 on the bench, then an ArduPilot, then a PX4 of unknown
    release: ArduPilot's 4.x must not unlock v1.17 modes on the second PX4."""
    _with_mapping(bridge, dict(mv.px4_map))
    _report_px4(bridge, 1, 17, 0)
    assert "ALTITUDE_CRUISE" in bridge.get_available_modes()
    _with_mapping(
        bridge, mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR), autopilot=_APM,
    )
    _report_px4(bridge, 4, 5, 7)
    _with_mapping(bridge, dict(mv.px4_map))
    assert "ALTITUDE_CRUISE" not in bridge.get_available_modes()


@pytest.mark.parametrize(("sub", "mode", "label"), [
    (1, "ORBIT", "ORBIT"),
    (2, "POSITION_SLOW", "POSITION SLOW"),
])
def test_a_posctl_sub_mode_reaches_the_top_bar_by_name(
    bridge: MavlinkBridge, sub: int, mode: str, label: str,
) -> None:
    """An orbiting vehicle used to read POSITION: the sub mode was dropped."""
    store = bridge._store
    bridge._latch_dialect(mv.mavlink.MAV_AUTOPILOT_PX4, mv.mavlink.MAV_TYPE_QUADROTOR)
    bridge._conn = NS(mode_mapping=lambda: dict(mv.px4_map))
    hb = _Message(
        message_type="HEARTBEAT", type=mv.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mv.mavlink.MAV_AUTOPILOT_PX4, base_mode=_CUSTOM | 0x80,
        custom_mode=(3 << 16) | (sub << 24), system_status=4,
    )
    bridge._dispatch(hb)
    snap = store.get_snapshot()
    assert snap["mode"] == mode
    assert snap["mode_label"] == label


def test_the_dialect_is_what_decides_px4_mode_support() -> None:
    """One place knows which PX4 release flies which mode. The bridge only
    hands it the version."""
    assert autopilot_dialect.px4_mode_supported("ALTITUDE_CRUISE", (1, 17, 0))
    assert not autopilot_dialect.px4_mode_supported("ALTITUDE_CRUISE", (1, 16, 9))
    assert not autopilot_dialect.px4_mode_supported("RTGS", (1, 18, 0))
