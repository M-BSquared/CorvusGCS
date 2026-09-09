"""Sharing one link with other MAVLink nodes — mavlink-router and friends.

On a direct radio the only thing talking is the autopilot, and every message
off the wire is by definition the aircraft's. Behind a router that stops being
true: the same link carries QGroundControl's heartbeat, a companion computer's
STATUSTEXT, a gimbal announcing itself, a second aircraft's telemetry. This
module pins that Corvus flies *its* vehicle and ignores the rest — and that
"the rest" specifically includes the two cases that used to break flying:
a ground station's heartbeat feeding the staleness timer of an aircraft that
had gone silent, and the command target latching onto whoever heartbeat first.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import GCS_SYSTEM_ID, MavlinkBridge, _is_vehicle_heartbeat
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Frames, built the way the wire builds them: get_srcSystem() only answers
# after a message has been packed, which is exactly what the filter reads.
# ---------------------------------------------------------------------------

def _from(msg, system: int, component: int = 1):
    """Stamp *msg* with a sender, as packing it for transmission would."""
    msg.pack(mavutil.mavlink.MAVLink(None, srcSystem=system, srcComponent=component))
    return msg


def _heartbeat(vtype: int, autopilot: int, *, armed: bool = False, custom: int = 0):
    base_mode = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
    if armed:
        base_mode |= mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED
    return mavutil.mavlink.MAVLink_heartbeat_message(
        type=vtype, autopilot=autopilot, base_mode=base_mode,
        custom_mode=custom, system_status=4, mavlink_version=3,
    )


def _vehicle_heartbeat(armed: bool = True):
    return _heartbeat(
        mavutil.mavlink.MAV_TYPE_QUADROTOR, mavutil.mavlink.MAV_AUTOPILOT_PX4,
        armed=armed, custom=(3 << 16),          # POSCTL
    )


def _gcs_heartbeat():
    """QGroundControl's own 1 Hz heartbeat, as a router hands it back."""
    return _heartbeat(
        mavutil.mavlink.MAV_TYPE_GCS, mavutil.mavlink.MAV_AUTOPILOT_INVALID,
    )


@pytest.fixture()
def bridge():
    store = VehicleStateStore()
    b = MavlinkBridge(store, "udp:0.0.0.0:14540")
    b._target_system = 1
    b._target_component = 1
    return b


# ---------------------------------------------------------------------------
# The armed flag, the flight mode, and who is allowed to set them
# ---------------------------------------------------------------------------

def test_a_second_ground_station_cannot_disarm_the_aircraft_on_screen(bridge) -> None:
    """The report this was found from: ARMED blinking off once a second.

    QGroundControl's heartbeat says type=GCS, autopilot=INVALID, base_mode=0.
    Taken as the aircraft's it wrote armed=False, mode="MODE_0" and
    vehicle_type="TYPE_6" over the real ones, 1 Hz, for as long as the second
    station was running.
    """
    bridge._dispatch(_from(_vehicle_heartbeat(armed=True), 1, 1))
    flying = bridge._store.get_snapshot()
    assert (flying["armed"], flying["mode"], flying["vehicle_type"]) == (
        True, "POSCTL", "QUADROTOR")

    bridge._dispatch(_from(_gcs_heartbeat(), 255, 190))

    after = bridge._store.get_snapshot()
    assert after["armed"] is True, "a ground station must never clear the armed flag"
    assert after["mode"] == "POSCTL"
    assert after["vehicle_type"] == "QUADROTOR"


def test_a_gimbal_on_the_same_airframe_does_not_speak_for_the_airframe(bridge) -> None:
    """Same system id, different component — the vehicle guard alone is not
    enough, which is why HEARTBEAT asks for the autopilot component."""
    bridge._dispatch(_from(_vehicle_heartbeat(armed=True), 1, 1))
    bridge._dispatch(_from(
        _heartbeat(mavutil.mavlink.MAV_TYPE_GIMBAL,
                   mavutil.mavlink.MAV_AUTOPILOT_INVALID), 1, 154))

    snap = bridge._store.get_snapshot()
    assert snap["armed"] is True
    assert snap["vehicle_type"] == "QUADROTOR"


def test_a_foreign_heartbeat_does_not_keep_a_lost_link_looking_alive(bridge) -> None:
    """The safety-relevant half.

    _receive_loop tears the link down and reconnects once the store goes stale
    past the drop timeout. Every HEARTBEAT used to feed that timer, so while a
    second station kept heartbeating through the router, an aircraft that had
    stopped transmitting still showed a green dot and never triggered a
    reconnect.
    """
    bridge._dispatch(_from(_vehicle_heartbeat(), 1, 1))
    bridge._store._last_heartbeat -= 20.0            # the aircraft went quiet
    assert bridge._store.is_stale(timeout=8.0)

    bridge._dispatch(_from(_gcs_heartbeat(), 255, 190))

    assert bridge._store.is_stale(timeout=8.0), (
        "somebody else's heartbeat must not reset this aircraft's staleness timer"
    )


# ---------------------------------------------------------------------------
# Telemetry from other systems
# ---------------------------------------------------------------------------

def test_a_second_aircraft_does_not_move_this_ones_marker(bridge) -> None:
    ours = mavutil.mavlink.MAVLink_global_position_int_message(
        time_boot_ms=1, lat=int(48.1 * 1e7), lon=int(11.5 * 1e7),
        alt=100_000, relative_alt=50_000, vx=0, vy=0, vz=0, hdg=0)
    bridge._dispatch(_from(ours, 1, 1))
    assert bridge._store.get_snapshot()["position"] == pytest.approx([11.5, 48.1])

    theirs = mavutil.mavlink.MAVLink_global_position_int_message(
        time_boot_ms=1, lat=0, lon=0, alt=0, relative_alt=0,
        vx=0, vy=0, vz=0, hdg=0)
    bridge._dispatch(_from(theirs, 2, 1))

    assert bridge._store.get_snapshot()["position"] == pytest.approx([11.5, 48.1])


def test_another_systems_parameters_never_reach_the_parameter_table(bridge) -> None:
    """A router carrying two vehicles used to merge both parameter sets."""
    param = mavutil.mavlink.MAVLink_param_value_message(
        param_id=b"MPC_XY_P", param_value=0.95, param_count=1, param_index=0,
        param_type=mavutil.mavlink.MAV_PARAM_TYPE_REAL32)
    bridge._dispatch(_from(param, 2, 1))
    assert bridge._params == {}


def test_the_radio_still_speaks_even_though_it_is_not_the_vehicle(bridge) -> None:
    """RADIO_STATUS comes from the SiK radio's own id (ord('3')/ord('D')), so
    it is served ahead of the guard — filtering it out would take link quality
    down with it on every serial link."""
    radio = mavutil.mavlink.MAVLink_radio_status_message(
        rssi=160, remrssi=150, txbuf=90, noise=20, remnoise=20, rxerrors=3, fixed=1)
    bridge._dispatch(_from(radio, ord("3"), ord("D")))

    snap = bridge._store.get_snapshot()
    assert snap["uplink"] > 0
    assert snap["uplink_rxerrors"] == 3


def test_a_message_with_no_sender_is_still_ours(bridge) -> None:
    """Source system 0 means the frame carries no sender id. The guard rejects
    an identified *other* node; it does not demand provenance."""
    bridge._dispatch(SimpleNamespace(
        get_type=lambda: "VFR_HUD",
        groundspeed=5.0, airspeed=5.0, heading=90, climb=0.5,
    ))
    assert bridge._store.get_snapshot()["groundspeed"] == 5.0


# ---------------------------------------------------------------------------
# Picking the aircraft out of the heartbeats on the link
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("hb, expected", [
    (_vehicle_heartbeat(), True),
    (_gcs_heartbeat(), False),
    (_heartbeat(mavutil.mavlink.MAV_TYPE_ONBOARD_CONTROLLER,
                mavutil.mavlink.MAV_AUTOPILOT_INVALID), False),
    (_heartbeat(mavutil.mavlink.MAV_TYPE_GIMBAL,
                mavutil.mavlink.MAV_AUTOPILOT_INVALID), False),
    # A ground station that fills the autopilot field in anyway.
    (_heartbeat(mavutil.mavlink.MAV_TYPE_GCS,
                mavutil.mavlink.MAV_AUTOPILOT_PX4), False),
    (_heartbeat(mavutil.mavlink.MAV_TYPE_FIXED_WING,
                mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA), True),
    # Nothing to judge by: accepted, so a dialect without the field still flies.
    (SimpleNamespace(type=2), True),
])
def test_only_an_autopilot_counts_as_the_vehicle(hb, expected) -> None:
    assert _is_vehicle_heartbeat(hb) is expected


def test_connect_waits_past_the_ground_stations_heartbeat_for_the_aircraft(bridge) -> None:
    """mavutil.wait_heartbeat matches on message type alone, so on a router it
    returns whichever heartbeat lands first. Latching onto QGroundControl's set
    the command target to system 255: arming, mode changes, parameter reads and
    mission uploads all then timed out against a ground station."""
    served = [_gcs_heartbeat(), _gcs_heartbeat(), _vehicle_heartbeat()]

    def fake_wait(blocking=True, timeout=None):
        return served.pop(0) if served else None

    bridge._conn = SimpleNamespace(wait_heartbeat=fake_wait)
    hb = bridge._wait_vehicle_heartbeat(10.0)

    assert hb is not None
    assert hb.type == mavutil.mavlink.MAV_TYPE_QUADROTOR
    assert served == [], "it must keep reading, not give up on the first miss"


def test_a_link_carrying_only_other_nodes_reports_no_heartbeat(bridge) -> None:
    """Better a clean 'No heartbeat received' and a retry than a green dot on
    a link whose aircraft never spoke."""
    bridge._conn = SimpleNamespace(
        wait_heartbeat=lambda blocking=True, timeout=None: _gcs_heartbeat())

    assert bridge._wait_vehicle_heartbeat(0.3) is None


def test_the_wait_is_bounded_even_while_heartbeats_keep_arriving(bridge) -> None:
    """The budget is wall-clock, not attempts: a chatty router must not hold
    _connect open past the timeout it was given."""
    import time
    bridge._conn = SimpleNamespace(
        wait_heartbeat=lambda blocking=True, timeout=None: _gcs_heartbeat())

    started = time.monotonic()
    assert bridge._wait_vehicle_heartbeat(0.2) is None
    assert time.monotonic() - started < 2.0


# ---------------------------------------------------------------------------
# Dialling out
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("conn", [
    "udpout:127.0.0.1:14550",     # mavlink-router UdpEndpoint, Mode = Server
    "tcpin:0.0.0.0:5760",
])
def test_the_dial_out_schemes_are_accepted_and_classified(bridge, conn) -> None:
    """Without these Corvus can only ever bind and wait, which leaves a router
    that binds and waits too — and any router behind NAT — unreachable."""
    bridge.validate_connection(conn)                  # must not raise
    bridge.set_connection(conn)
    assert bridge.transport() in ("udp", "tcp")
    assert bridge.transport() != "unknown"
    assert bridge._is_serial() is False


def test_the_forwarders_copy_of_the_gcs_system_id_matches_the_bridge() -> None:
    """mavlink_forwarder repeats the id rather than importing it, to stay
    stdlib-only. This is the thing that stops the two drifting apart."""
    from corvus.mavlink_forwarder import _CORVUS_SYSTEM_ID
    assert _CORVUS_SYSTEM_ID == GCS_SYSTEM_ID
