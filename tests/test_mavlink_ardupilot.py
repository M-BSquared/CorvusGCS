"""The bridge talking to an ArduPilot vehicle.

:mod:`tests.test_autopilot_dialect` pins what the two stacks *want*. This pins
what the bridge actually puts on the wire once it knows which one is on the
other end — the sequencing, which is where the differences bite:

* a takeoff must reach GUIDED and be armed before the command goes out, and the
  altitude must be the number the operator typed, not one converted to AMSL;
* a refused takeoff must disarm again, because the ArduPilot order is the one
  that can leave an aircraft armed on the ground for nothing;
* a mission upload must reserve slot 0 for home, and the mode that runs it is
  AUTO rather than MISSION;
* the shell and the ESC calibration must be refused with a sentence, not
  attempted.
"""
from __future__ import annotations

from types import SimpleNamespace
from collections.abc import Callable

import pytest
from pymavlink import mavutil

from corvus import autopilot
from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore

CUSTOM = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)


class FakeMav:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.commands: list[tuple] = []
        self.mission_counts: list[int] = []
        self.items: list[tuple] = []
        self.on_send = on_send

    def command_long_send(self, *args: float) -> None:
        self.commands.append(args)
        if self.on_send:
            self.on_send(args)

    def mission_count_send(self, _sys: int, _comp: int, count: int) -> None:
        self.mission_counts.append(count)

    def mission_item_int_send(self, *args: float) -> None:
        self.items.append(args)


class FakeConnection:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.mav = FakeMav(on_send)
        self.source_system = 255
        self.source_component = 190


def ack(command: int, result: int) -> FakeMessage:
    return FakeMessage(
        message_type="COMMAND_ACK", command=command, result=result,
        target_system=255, target_component=190, source_system=1,
    )


def ardupilot_bridge(mav_type: int = mavutil.mavlink.MAV_TYPE_QUADROTOR) -> MavlinkBridge:
    """A bridge that has already heard an ArduPilot heartbeat."""
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._latch_dialect(mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA, mav_type)
    bridge._mode_values = dict(bridge._dialect.mode_table(mav_type))
    bridge._mode_mapping = set(bridge._mode_values)
    return bridge


def accept_everything(bridge: MavlinkBridge) -> FakeConnection:
    """Wire an ACCEPTED ack back for every command, and report the mode change."""
    def on_send(args: tuple) -> None:
        command = int(args[2])
        if command == mavutil.mavlink.MAV_CMD_DO_SET_MODE:
            number = int(args[5])
            name = bridge._dialect.decode_mode(number, CUSTOM, bridge._mav_type_id)
            bridge._store.update(mode=name)
        if command == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM:
            bridge._store.update(armed=bool(args[4]))
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    conn = FakeConnection(on_send)
    bridge._conn = conn
    return conn


def commands_of(conn: FakeConnection) -> list[int]:
    return [int(c[2]) for c in conn.mav.commands]


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------

def test_a_mode_change_goes_out_as_a_flat_custom_mode() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    assert bridge.set_mode("LOITER") is True
    sent = conn.mav.commands[0]
    assert int(sent[2]) == mavutil.mavlink.MAV_CMD_DO_SET_MODE
    assert sent[4] == float(CUSTOM)
    assert sent[5] == 5.0        # ArduPilot copter LOITER
    assert sent[6] == 0.0


def test_a_heartbeat_relatches_the_dialect_mid_session() -> None:
    """Behind mavlink-router the aircraft can be swapped without the socket
    closing, and the mode word means something else the moment it is."""
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._dispatch(FakeMessage(
        message_type="HEARTBEAT",
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA,
        base_mode=CUSTOM, custom_mode=6, source_system=1, source_component=1,
    ))
    snapshot = store.get_snapshot()
    assert snapshot["autopilot"] == "ARDUPILOTMEGA"
    assert snapshot["autopilot_stack"] == "ardupilot"
    assert snapshot["mode"] == "RTL"


# ---------------------------------------------------------------------------
# Takeoff
# ---------------------------------------------------------------------------

def test_takeoff_enters_guided_arms_and_then_commands_a_relative_altitude() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)

    assert bridge.takeoff(25.0) is True

    assert commands_of(conn) == [
        mavutil.mavlink.MAV_CMD_DO_SET_MODE,
        mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM,
        mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
    ]
    mode, arm, takeoff = conn.mav.commands
    assert mode[5] == 4.0                     # GUIDED
    assert arm[4] == 1.0
    # The number the operator typed, not one converted against a home altitude
    # that ArduPilot does not read param7 as.
    assert takeoff[10] == 25.0
    assert takeoff[4] == 0.0                  # minimum climb pitch, not NaN


def test_a_takeoff_altitude_is_not_converted_even_when_home_is_known() -> None:
    """The bug this guards: a 25 m takeoff behind a 300 m home altitude became
    a request to climb to 325 m above home."""
    bridge = ardupilot_bridge()
    bridge._home_alt_amsl = 300.0
    conn = accept_everything(bridge)
    assert bridge.takeoff(25.0) is True
    assert conn.mav.commands[-1][10] == 25.0


def test_a_takeoff_refused_after_arming_disarms_again() -> None:
    """The inverted order is the one that can strand an armed aircraft."""
    bridge = ardupilot_bridge()
    seen: list[int] = []

    def on_send(args: tuple) -> None:
        command = int(args[2])
        seen.append(command)
        if command == mavutil.mavlink.MAV_CMD_NAV_TAKEOFF:
            bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_DENIED))
            return
        if command == mavutil.mavlink.MAV_CMD_DO_SET_MODE:
            bridge._store.update(mode="GUIDED")
        if command == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM:
            bridge._store.update(armed=bool(args[4]))
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.takeoff(10.0) is False
    assert "Takeoff failed: DENIED" in bridge.get_last_command_error()
    assert seen[-1] == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM
    assert bridge._store.get_snapshot()["armed"] is False


def test_a_takeoff_that_cannot_reach_guided_never_arms() -> None:
    bridge = ardupilot_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    conn = FakeConnection(on_send)
    bridge._conn = conn
    assert bridge.takeoff(10.0) is False
    assert commands_of(conn) == [mavutil.mavlink.MAV_CMD_DO_SET_MODE]
    assert "GUIDED" in bridge.get_last_command_error()


def test_a_px4_takeoff_still_commands_the_target_before_it_arms() -> None:
    """The PX4 order is not a special case of the ArduPilot one; it is the
    opposite, and it stays that way."""
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._latch_dialect(mavutil.mavlink.MAV_AUTOPILOT_PX4,
                          mavutil.mavlink.MAV_TYPE_QUADROTOR)
    bridge._home_alt_amsl = 300.0
    conn = accept_everything(bridge)

    assert bridge.takeoff(25.0) is True
    assert commands_of(conn) == [
        mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
        mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM,
    ]
    assert conn.mav.commands[0][10] == 325.0    # AMSL, which is what PX4 reads


# ---------------------------------------------------------------------------
# Calibration
# ---------------------------------------------------------------------------

def test_a_compass_calibration_starts_ardupilots_own_mag_fit() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    assert bridge.calibrate("compass") is True
    assert int(conn.mav.commands[0][2]) == autopilot.MAV_CMD_DO_START_MAG_CAL


def test_cancelling_a_mag_fit_uses_the_command_that_cancels_one() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    assert bridge.calibrate("compass") is True
    assert bridge.cancel_calibration() is True
    assert int(conn.mav.commands[-1][2]) == autopilot.MAV_CMD_DO_CANCEL_MAG_CAL


def test_an_esc_calibration_is_refused_with_the_reason() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    assert bridge.calibrate("motor") is False
    assert conn.mav.commands == []
    assert "ESC_CALIBRATION" in bridge.get_last_command_error()
    assert "motor" not in bridge.available_calibrations()


def test_a_confirmed_position_reaches_the_vehicle_as_accelcal_vehicle_pos() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    assert bridge.accel_calibration_position("back") is True
    sent = conn.mav.commands[0]
    assert int(sent[2]) == autopilot.MAV_CMD_ACCELCAL_VEHICLE_POS
    assert sent[4] == 6.0


def test_px4_refuses_the_position_step_because_it_detects_them_itself() -> None:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = MavlinkBridge(store)
    bridge._latch_dialect(mavutil.mavlink.MAV_AUTOPILOT_PX4, 2)
    conn = accept_everything(bridge)
    assert bridge.accel_calibration_position("level") is False
    assert conn.mav.commands == []
    assert "detects" in bridge.get_last_command_error()


# ---------------------------------------------------------------------------
# Autotune, shell, mission
# ---------------------------------------------------------------------------

def test_an_ardupilot_autotune_is_a_mode_change_and_leaving_it_is_another() -> None:
    bridge = ardupilot_bridge()
    bridge._store.update(armed=True, landed_state=2, mode="LOITER")
    conn = accept_everything(bridge)

    assert bridge.autotune("all", enable=True) is True
    assert int(conn.mav.commands[0][5]) == 15          # copter AUTOTUNE
    assert bridge._store.get_snapshot()["autotune_state"] == "running"

    assert bridge.autotune("all", enable=False) is True
    assert int(conn.mav.commands[-1][5]) == 5          # back to LOITER
    assert bridge._store.get_snapshot()["autotune_state"] == ""


def test_the_autotune_still_refuses_to_start_on_the_ground() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    assert bridge.autotune("all", enable=True) is False
    assert conn.mav.commands == []
    assert "arm the vehicle" in bridge.get_last_command_error()


def test_the_shell_is_refused_on_a_stack_that_has_none() -> None:
    bridge = ardupilot_bridge()
    bridge._conn = FakeConnection()
    assert bridge.send_shell_command("ls") is False
    error = bridge.get_last_command_error()
    assert "ArduPilot" in error and "no MAVLink shell" in error


def test_a_mission_upload_reserves_slot_zero_for_home() -> None:
    """ArduPilot stores item 0 as home. Without the placeholder the first real
    item is eaten, and the aircraft flies a mission missing its takeoff."""
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    # [lon, lat], the order HOME_POSITION is stored in.
    bridge._store.update(home=[11.0, 48.0])

    def fake_upload(items: list[dict]) -> int:
        conn.mav.items = list(items)
        return mavutil.mavlink.MAV_MISSION_ACCEPTED

    bridge._upload_mission = fake_upload   # type: ignore[method-assign]
    ok = bridge.upload_mission_plan([
        {"command": mavutil.mavlink.MAV_CMD_NAV_WAYPOINT,
         "lat": 48.1, "lon": 11.1, "alt": 30.0, "params": [0, 0, 0, 0]},
    ])
    assert ok is True
    assert len(conn.mav.items) == 2
    home, waypoint = conn.mav.items
    assert home["frame"] == mavutil.mavlink.MAV_FRAME_GLOBAL
    assert home["x_int"] == int(round(48.0 * 1e7)), "x is the latitude"
    assert home["y_int"] == int(round(11.0 * 1e7)), "y is the longitude"
    assert waypoint["x_int"] == int(round(48.1 * 1e7))


def test_the_home_slot_is_read_from_a_real_home_position() -> None:
    """The store's home is [lon, lat]. Read the other way round, a vehicle at
    48.1 N 11.6 E got a home slot at 11.6 N 48.1 E, in Somalia."""
    bridge = ardupilot_bridge()
    accept_everything(bridge)
    bridge._dispatch(FakeMessage(
        message_type="HOME_POSITION", latitude=481234567, longitude=116543210,
        altitude=520000, source_system=1,
    ))
    home = bridge._with_mission_home_slot([
        bridge._build_mission_item_spec(
            0, mavutil.mavlink.MAV_CMD_NAV_WAYPOINT, 48.2, 11.7, 30.0, 0, 0, 0, 0),
    ])[0]
    assert home["x_int"] == 481234567
    assert home["y_int"] == 116543210
    assert home["z"] == 520.0


def test_a_px4_mission_upload_still_starts_at_item_zero() -> None:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = MavlinkBridge(store)
    bridge._latch_dialect(mavutil.mavlink.MAV_AUTOPILOT_PX4, 2)
    bridge._conn = FakeConnection()
    sent: list[dict] = []
    bridge._upload_mission = lambda items: (   # type: ignore[method-assign]
        sent.extend(items) or mavutil.mavlink.MAV_MISSION_ACCEPTED
    )
    assert bridge.upload_mission_plan([
        {"command": mavutil.mavlink.MAV_CMD_NAV_WAYPOINT,
         "lat": 48.1, "lon": 11.1, "alt": 30.0, "params": [0, 0, 0, 0]},
    ]) is True
    assert len(sent) == 1


def test_a_copter_mission_arms_in_guided_and_is_started_by_mission_start() -> None:
    """Copter refuses to arm in AUTO unless AUTO_OPTIONS allows it (off by
    default), so AUTO then arm could never start a mission from the ground.
    GUIDED, arm, then MISSION_START, which is itself the switch to AUTO."""
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    assert bridge.start_mission(3) is True
    assert commands_of(conn) == [
        mavutil.mavlink.MAV_CMD_DO_SET_MODE,
        mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM,
        mavutil.mavlink.MAV_CMD_MISSION_START,
    ]
    mode, arm, start = conn.mav.commands
    assert mode[5] == 4.0                        # copter GUIDED
    assert arm[4] == 1.0
    # ArduPilot 4.6 answers any other first/last pair with DENIED.
    assert (start[4], start[5]) == (0.0, 0.0)


def test_a_plane_mission_arms_in_auto() -> None:
    """A tiltrotor armed in GUIDED spins up in the forward flight position."""
    bridge = ardupilot_bridge(mavutil.mavlink.MAV_TYPE_FIXED_WING)
    conn = accept_everything(bridge)
    assert bridge.start_mission(3) is True
    mode = conn.mav.commands[0]
    assert int(mode[2]) == mavutil.mavlink.MAV_CMD_DO_SET_MODE
    assert mode[5] == 10.0                       # plane AUTO
    assert commands_of(conn)[-1] == mavutil.mavlink.MAV_CMD_MISSION_START


def test_a_refused_mission_start_disarms_what_it_armed() -> None:
    bridge = ardupilot_bridge()
    conn = accept_everything(bridge)
    accepted = conn.mav.on_send

    def refuse_start(args: tuple) -> None:
        if int(args[2]) == mavutil.mavlink.MAV_CMD_MISSION_START:
            bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_FAILED))
            return
        accepted(args)

    conn.mav.on_send = refuse_start
    assert bridge.start_mission(3) is False
    assert commands_of(conn)[-1] == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM
    assert conn.mav.commands[-1][4] == 0.0, "disarmed again"
    assert bridge._store.get_snapshot()["armed"] is False
    assert "Mission start failed: FAILED" in bridge.get_last_command_error()


def test_an_airborne_ardupilot_mission_start_only_sends_mission_start() -> None:
    bridge = ardupilot_bridge()
    bridge._store.update(armed=True, landed_state=2, mode="LOITER")
    conn = accept_everything(bridge)
    assert bridge.start_mission(3) is True
    assert commands_of(conn) == [mavutil.mavlink.MAV_CMD_MISSION_START]


@pytest.mark.parametrize(("mav_type", "mode_number"), [
    (mavutil.mavlink.MAV_TYPE_QUADROTOR, 4),
    (mavutil.mavlink.MAV_TYPE_FIXED_WING, 15),
    (mavutil.mavlink.MAV_TYPE_GROUND_ROVER, 15),
])
def test_guided_is_looked_up_per_airframe(mav_type: int, mode_number: int) -> None:
    """GUIDED is 4 on a copter and 15 on a plane. One hardcoded number would be
    the wrong mode on two of these three."""
    bridge = ardupilot_bridge(mav_type)
    conn = accept_everything(bridge)
    assert bridge._enter_guided("Takeoff") is True
    assert conn.mav.commands[0][5] == float(mode_number)


# ---------------------------------------------------------------------------
# Who the vehicle listens to
# ---------------------------------------------------------------------------

def _authority_bridge(answers: dict[str, float],
                      stack_id: int = mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA,
                      ) -> tuple[MavlinkBridge, list[dict]]:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = MavlinkBridge(store)
    bridge._latch_dialect(stack_id, mavutil.mavlink.MAV_TYPE_QUADROTOR)
    bridge._conn = FakeConnection()
    bridge.fetch_params = lambda names, timeout=4.0: {   # type: ignore[method-assign]
        n: answers[n] for n in names if n in answers
    }
    published: list[dict] = []
    bridge._console_publish = lambda name, text, level: published.append(   # type: ignore[method-assign]
        {"name": name, "text": text, "level": level})
    return bridge, published


def test_a_vehicle_that_will_ignore_our_sticks_says_so_at_connect() -> None:
    """ArduPilot drops MANUAL_CONTROL from any system id but SYSID_MYGCS, and
    it drops it silently — no NAK, no STATUSTEXT, no ACK to miss. Corvus is 254
    and the default is 255, so on a stock vehicle the joystick does nothing at
    all and gives no sign of it."""
    bridge, published = _authority_bridge({"SYSID_MYGCS": 255.0})
    bridge._check_gcs_authority()
    assert len(published) == 1
    text = published[0]["text"]
    assert "SYSID_MYGCS" in text
    assert "254" in text and "255" in text
    assert "joystick will be ignored" in text
    # The rest of the link is fine, and saying otherwise would be alarming.
    assert "Flight commands and mode changes are unaffected" in text


def test_a_vehicle_already_pointed_at_corvus_is_not_warned_about() -> None:
    bridge, published = _authority_bridge({"SYSID_MYGCS": 254.0})
    bridge._check_gcs_authority()
    assert published == []


def test_the_parameter_ardupilot_renamed_it_to_is_accepted_too() -> None:
    """4.5 renamed SYSID_MYGCS to MAV_GCS_SYSID."""
    bridge, published = _authority_bridge({"MAV_GCS_SYSID": 255.0})
    bridge._check_gcs_authority()
    assert len(published) == 1
    assert "MAV_GCS_SYSID" in published[0]["text"]


def test_corvus_never_writes_the_parameter_itself() -> None:
    """Which ground station may take an aircraft's sticks is the operator's
    decision, not something to change behind their back."""
    bridge, _published = _authority_bridge({"SYSID_MYGCS": 255.0})
    writes: list[tuple[str, float]] = []
    bridge.set_param = lambda name, value: (   # type: ignore[method-assign]
        writes.append((name, value)) or True
    )
    bridge._check_gcs_authority()
    assert writes == []


def test_a_vehicle_that_answers_for_neither_name_is_left_alone() -> None:
    """Guessing would be worse than saying nothing."""
    bridge, published = _authority_bridge({})
    bridge._check_gcs_authority()
    assert published == []


def test_px4_is_never_asked_because_it_has_no_such_gate() -> None:
    asked: list[list[str]] = []
    bridge, published = _authority_bridge(
        {"SYSID_MYGCS": 255.0}, mavutil.mavlink.MAV_AUTOPILOT_PX4)
    bridge.fetch_params = lambda names, timeout=4.0: (   # type: ignore[method-assign]
        asked.append(list(names)) or {}
    )
    bridge._check_gcs_authority()
    assert asked == []
    assert published == []


@pytest.mark.parametrize(("raw", "expected"), [
    # PX4: the first 5 bytes of the SHA in the high bytes of a little-endian
    # uint64, so the wire bytes are [0,0,0, g4,g3,g2,g1,g0].
    (bytes([0, 0, 0, 0x0d, 0x0c, 0x0b, 0x0a, 0x09]), "090a0b0c0d"),
    # ArduPilot writes the hash as TEXT. Hex-encoding it a second time is what
    # put "3461376233633964" on the status bar of every ArduPilot vehicle.
    (b"4a7b3c9d", "4a7b3c9d"),
    (b"4A7B3C9D", "4a7b3c9d"),
    # Neither: a packed integer that happens not to lead with a NUL.
    (bytes([0, 0, 0, 0xff, 0xee, 0xdd, 0xcc, 0xbb]), "bbccddeeff"),
    (bytes(8), ""),
])
def test_the_firmware_hash_is_read_in_the_format_it_arrives_in(
    raw: bytes, expected: str,
) -> None:
    """The two stacks fill the same eight bytes with two different things and
    neither says which, so the field is sniffed rather than assumed."""
    assert MavlinkBridge._decode_git_hash(list(raw)) == expected


def test_capabilities_reach_the_http_layer_with_the_live_mode_list() -> None:
    bridge = ardupilot_bridge()
    caps = bridge.capabilities()
    assert caps["stack"] == "ardupilot"
    assert "GUIDED" in caps["modes"]
    assert caps["vehicle_type"] == mavutil.mavlink.MAV_TYPE_QUADROTOR
