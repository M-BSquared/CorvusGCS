"""Flight commands: flight mode, arming, takeoff, landing, return and home.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state, which the bridge's ``__init__``
sets up. What an operator asks of the aircraft in flight. Each command goes through
the bridge's command plumbing (``_send_command_and_wait``) and the dialect
layer for the stack differences (takeoff order, mission mode).
Nothing here imports the bridge, so the bridge can import this.

The commands are ``corvus_link.flight.FlightEngine``, in Rust, which also runs
the mission sequences of :mod:`corvus.mavlink_missions` (fly to points,
mission start, reposition). It reads its clock (``time``) and the names
imported below from this module, and calls every step through the bridge, so
a bridge that overrides one step changes the sequence.
"""
from __future__ import annotations

import sys
import time  # noqa: F401 - the engine's clock
from typing import Any

from corvus_link import flight as _engine
from corvus_link import mavutil  # noqa: F401 - the engine names unknown commands

# Read by the engine from this module.
from .autopilot import PLUGIN_POSITION_COMMANDS, ModeCommand, is_ground_vehicle  # noqa: F401
from .mavlink_common import (  # noqa: F401
    MAV_RESULT_TEXT, TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M,
    VELOCITY_MAX_MS, YAW_RATE_MAX_DEG_S,
)
from .mavlink_missions import DO_REPOSITION_CHANGE_MODE  # noqa: F401


class FlightCommandsMixin:
    """Flight commands: flight mode, arming, takeoff, landing, return and home."""

    # How long to watch for the arm/disarm to show up in telemetry. Confirmation
    # arrives on HEARTBEAT, 1 Hz on every link Corvus supports, so this is six
    # of them: on a lossy radio three were sometimes one.
    ARMED_STATE_CONFIRM_S = 6.0

    # MAV_LANDED_STATE values that mean the aircraft is off the ground.
    _AIRBORNE_LANDED_STATES = frozenset({2, 3, 4})   # IN_AIR, TAKEOFF, LANDING
    # Height above home that counts as airborne when the firmware does not
    # publish EXTENDED_SYS_STATE. Clear of the noise on a stationary estimate.
    AIRBORNE_FALLBACK_M = 1.5

    # How long takeoff waits for an altitude reference to turn up before it
    # gives up on converting AGL to AMSL: "the operator clicked Takeoff a
    # second after connecting", without a pause in the normal case.
    TAKEOFF_REFERENCE_WAIT_S = 2.0

    # How long a mode change is given to show up in telemetry before the
    # command that depends on it is sent anyway.
    MODE_CONFIRM_S = 3.0

    def _init_flight_state(self) -> None:
        """Set up the flight command engine; called from the bridge's __init__."""
        self._flight_engine = _engine.FlightEngine(sys.modules[__name__])

    def set_mode(self, mode: str) -> bool:
        """Change flight mode, ACK-confirmed.

        Priority tier: a mode change is always a direct operator instruction
        and a single command. Reaching for LOITER or RTL on the mode selector
        is how a flight gets taken back by hand, and it has to act at once.
        """
        with self._operation_lock.priority():
            return self._flight_engine.set_mode(self, mode)

    def _sequence_interrupted(self, mark: int, action: str) -> bool:
        """Did the operator take over while a multi-step command was under way?

        Takeoff and mission start do not hold the command lock across their
        telemetry waits, so Disarm, Land, RTL or a mode change never queue
        behind them. Each later step asks this first and stops with the reason
        rather than send its next step on top of the operator's.
        """
        return self._flight_engine.sequence_interrupted(self, mark, action)

    def _reports_armed_state(self, armed: bool) -> bool:
        """Does fresh telemetry say the vehicle is already *armed* (or not)?"""
        return self._flight_engine.reports_armed_state(self, armed)

    def _wait_for_armed_state(self, armed: bool, timeout: float = ARMED_STATE_CONFIRM_S) -> bool:
        """Watch the store until it reports *armed*, for up to *timeout* seconds."""
        return self._flight_engine.wait_for_armed_state(self, armed, timeout)

    def arm(self, arm: bool = True) -> bool:
        """Arm/disarm with ACK confirmation.

        Disarm is an abort and takes the priority tier; arming queues. The
        command is always sent, since telemetry lags the vehicle. An accepted
        command whose telemetry confirmation never arrives (behind
        mavlink-router, say) is reported as unconfirmed, not as failed: calling
        it a failure would tell the operator an armed aircraft was disarmed.
        """
        if not isinstance(arm, bool):
            self._set_command_error("Arm state must be boolean")
            return False
        # Disarm overtakes routine work; arm does not.
        gate = self._operation_lock if arm else self._operation_lock.priority()
        with gate:
            verdict = self._flight_engine.arm_command(self, arm)
        if verdict is not None:
            return verdict
        # Outside the lock: the confirmation sends nothing, it watches the
        # store for up to ARMED_STATE_CONFIRM_S, and holding the lock across
        # it blocked every other command for the duration.
        return self._flight_engine.arm_confirm(self, arm)

    def _is_airborne(self) -> bool:
        """Is the aircraft off the ground? The vehicle's own answer first.

        Mirrors the frontend's reading of the same two fields, so the mission
        planner and the status bar cannot disagree about whether a takeoff item
        needs prepending.
        """
        return self._flight_engine.is_airborne(self)

    def _takeoff_altitude_amsl(self, altitude_agl: float) -> float | None:
        return self._flight_engine.takeoff_altitude_amsl(self, altitude_agl)

    def _await_takeoff_reference(self, altitude_agl: float) -> float | None:
        """Nudge the vehicle for home and wait briefly for an altitude reference.

        Returns the AMSL takeoff altitude once one can be derived, or None if
        nothing arrives inside the window.
        """
        return self._flight_engine.await_takeoff_reference(self, altitude_agl)

    def _wait_for_mode(self, mode: str, timeout: float = MODE_CONFIRM_S) -> bool:
        """Watch the store until the vehicle reports *mode*, or time out.

        Sends nothing. ArduPilot refuses a guided takeoff outside GUIDED, and
        from the old mode if the command overtakes the mode change on the
        wire, so the takeoff waits for the aircraft's own word.
        """
        return self._flight_engine.wait_for_mode(self, mode, timeout)

    def _enter_guided(self, action: str) -> bool:
        """Put the vehicle in the mode a guided command needs, if it needs one.

        PX4 needs none. ArduPilot has to be in GUIDED first, and a vehicle
        already in a mode that accepts guided commands is left alone.
        """
        return self._flight_engine.enter_guided(self, action)

    def _enter_mission_mode(self, action: str) -> bool:
        """Switch to the mode that runs a stored mission: MISSION on PX4, AUTO
        on ArduPilot, from the dialect rather than hardcoded."""
        return self._flight_engine.enter_mission_mode(self, action)

    def takeoff(self, altitude_agl: float = 10.0) -> bool:
        """Command a guided takeoff to *altitude_agl* metres above the ground.

        **PX4** accepts ``MAV_CMD_NAV_TAKEOFF`` from any mode with param7 as an
        **AMSL** altitude and is armed afterwards, so a refused takeoff never
        leaves an armed aircraft on the ground. Without an altitude reference
        param7 is NaN, the vehicle's own configured altitude.

        **ArduPilot** reads param7 **relative to home**, and refuses the
        command outside GUIDED or while disarmed: mode, arm, takeoff, and a
        disarm again if the takeoff is refused.
        """
        return self._flight_engine.takeoff(self, altitude_agl)

    def land(self) -> bool:
        """Command land at current position with NaN lat/lon and ACK.

        Priority tier: one of the three commands an operator uses to end a
        flight that is going wrong.
        """
        with self._operation_lock.priority():
            return self._flight_engine.land(self)

    def rtl(self) -> bool:
        """Command return to launch with ACK; priority tier, as :meth:`land`."""
        with self._operation_lock.priority():
            return self._flight_engine.rtl(self)

    def _validate_lat_lon(
        self, lat: Any, lon: Any, action: str,
    ) -> tuple[float, float] | None:
        """Coerce and range-check one coordinate pair, or None with the error set.

        Shares :meth:`_fly_to_point_number`'s bool rejection: ``True`` is an
        ``int`` in Python and would otherwise fly as latitude 1.0.
        """
        return self._flight_engine.validate_lat_lon(self, lat, lon, action)

    def set_home(self, lat: float, lon: float) -> bool:
        """Move the home position to (*lat*, *lon*), keeping its altitude.

        MAV_CMD_DO_SET_HOME with param1=0 as COMMAND_INT; works on PX4 v1.16,
        v1.17 and v1.18. The altitude is the current home's (or the AMSL minus
        AGL reference), never the click's, so this is a purely horizontal move,
        and without one the command is refused rather than sent with a made up
        altitude. Allowed while armed: it is how RTL is redirected in flight.
        """
        return self._flight_engine.set_home(self, lat, lon)

    # ---- control from plugins -------------------------------------------

    def hold(self) -> bool:
        """Stop where the vehicle is and stay there, in the stack's hold mode:
        Hold on PX4, LOITER on an ArduPilot copter or plane, HOLD on a rover,
        POSHOLD on a sub."""
        return self._flight_engine.hold(self)

    def plugin_command(
        self, command: int, params: list[float] | None = None,
        position: tuple[float, float, float, str] | None = None,
    ) -> bool:
        """Send one MAV_CMD from the dialect's plugin table and wait for its ACK.

        *params* are p1 to p7 (p1 to p4 for a command that names a place), NaN
        where unset. *position* is ``(lat, lon, alt, frame)``, required for and
        only for :data:`PLUGIN_POSITION_COMMANDS`, which travel as COMMAND_INT;
        a "relative" altitude is converted to AMSL where the stack reads it so.
        True on ACCEPTED, or IN_PROGRESS. A command outside the table is
        refused without sending.
        """
        return self._flight_engine.plugin_command(self, command, params, position)

    # SET_POSITION_TARGET_LOCAL_NED type_mask: fly the velocity, ignore the
    # position, the acceleration and the yaw angle.
    _VELOCITY_TYPE_MASK = 1 | 2 | 4 | 64 | 128 | 256 | 1024
    _YAW_RATE_IGNORE = 2048

    def velocity_setpoint(
        self, vx: float, vy: float, vz: float,
        yaw_rate_deg: float | None = None, body: bool = False,
    ) -> bool:
        """Send one velocity setpoint as SET_POSITION_TARGET_LOCAL_NED; no ACK.

        In metres per second: north, east, down, or with *body* forward,
        right, down. *yaw_rate_deg* turns clockwise in degrees per second;
        None leaves the heading to the vehicle. A stream at 2 Hz or more whose
        end the vehicle's own timeout catches; never changes the mode itself.
        """
        return self._flight_engine.velocity_setpoint(self, vx, vy, vz, yaw_rate_deg, body)
