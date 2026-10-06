"""Flight commands: flight mode, arming, takeoff, landing, return and home.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state, which the bridge's ``__init__``
sets up. What an operator asks of the aircraft in flight. Each command goes through
the bridge's command plumbing (``_send_command_and_wait``) and the dialect
layer for the stack differences (takeoff order, mission mode).
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import logging
import math
import time

from pymavlink import mavutil
from typing import Any

from .autopilot import ModeCommand
from .mavlink_common import MAV_RESULT_TEXT, TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M

logger = logging.getLogger("corvus.mavlink")

class FlightCommandsMixin:
    """Flight commands: flight mode, arming, takeoff, landing, return and home."""

    # How long to watch for the arm/disarm to show up in telemetry. Confirmation
    # arrives on HEARTBEAT, which is 1 Hz on every link Corvus supports, so the
    # old 3 s was three heartbeats — and on a lossy radio, sometimes one.
    ARMED_STATE_CONFIRM_S = 6.0

    # MAV_LANDED_STATE values that mean the aircraft is off the ground.
    _AIRBORNE_LANDED_STATES = frozenset({2, 3, 4})   # IN_AIR, TAKEOFF, LANDING
    # Height above home that counts as airborne when the firmware does not
    # publish EXTENDED_SYS_STATE. Clear of the noise on a stationary estimate.
    AIRBORNE_FALLBACK_M = 1.5

    # How long takeoff waits for an altitude reference to turn up before it
    # gives up on converting AGL to AMSL. HOME_POSITION streams at 0.5-1 Hz and
    # GLOBAL_POSITION_INT far faster, so this covers "the operator clicked
    # Takeoff a second after connecting" without putting a noticeable pause in
    # front of the command in the normal case, where the reference is already
    # there and this is never called.
    TAKEOFF_REFERENCE_WAIT_S = 2.0

    # How long a mode change is given to show up in telemetry before the
    # command that depends on it is sent anyway. HEARTBEAT is 1 Hz on every
    # link Corvus supports, so this is three of them on a lossy radio.
    MODE_CONFIRM_S = 3.0

    def set_mode(self, mode: str) -> bool:
        """Change flight mode, ACK-confirmed.

        Priority tier: a mode change is always a direct operator instruction
        and is always a single command, so it can neither be starved nor
        starve anything. Reaching for LOITER or RTL on the mode selector is
        how a flight gets taken back by hand, and it has to act at once.
        """
        with self._operation_lock.priority():
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure("Mode change", -2)
            value = self._mode_values.get(mode)
            if not isinstance(value, tuple) or len(value) < 3:
                # Three different failures used to share one message, and two
                # of them named PX4 at an aircraft that was not running it.
                if self._modes_unsupported:
                    text = (
                        "Mode changes are not supported on this autopilot: "
                        "Corvus does not know how it encodes a mode"
                    )
                else:
                    text = (
                        f"Unknown or unsupported {self._dialect.label} mode: "
                        f"{mode}"
                    )
                logger.error("%s", text)
                self._set_command_error(text)
                return False
            params = ModeCommand(*value[:3]).params()
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_DO_SET_MODE,
                params,
                timeout=3.0,
                retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Mode change", result)
            self._console_publish("MODE", f"Mode accepted: {mode}", "success")
            return True

    def _wait_for_armed_state(self, armed: bool, timeout: float = ARMED_STATE_CONFIRM_S) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            snapshot = self._store.get_snapshot()
            if snapshot.get("connected") and bool(snapshot.get("armed")) is armed:
                return True
            time.sleep(0.05)
        return False

    def arm(self, arm: bool = True) -> bool:
        """Arm/disarm with ACK confirmation. Returns True only if PX4 accepted.

        Disarm is an abort and takes the priority tier: it is the command an
        operator reaches for when something is wrong, and it must not queue
        behind a parameter upload. Arming is ordinary and queues normally.
        """
        if not isinstance(arm, bool):
            self._set_command_error("Arm state must be boolean")
            return False
        action = "Arm" if arm else "Disarm"
        # Disarm overtakes routine work; arm does not.
        gate = self._operation_lock if arm else self._operation_lock.priority()
        with gate:
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure(action, -2)
            if bool(self._store.get_snapshot().get("armed")) is arm:
                return True
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM,
                [1.0 if arm else 0.0, 0.0, nan, nan, nan, nan, nan],
                timeout=3.0, retries=2,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure(action, result)
        # Outside the lock deliberately. What follows sends nothing: it watches
        # the store for the vehicle's own report of the new state. Holding the
        # command lock through six seconds of reading telemetry blocked every
        # other command for the duration and protected nothing — and it was the
        # single longest hold in the class, because takeoff nests this call.
        if not self._wait_for_armed_state(arm):
            # The vehicle ACCEPTED the command; telemetry has not caught up.
            # That is a reporting gap, not a refusal, and calling it a
            # failure was the more dangerous of the two mistakes: the arm
            # confirmation only ever arrives on a HEARTBEAT from the exact
            # component this session latched onto at connect, so behind
            # mavlink-router or a companion computer it can simply never
            # come — and Corvus would then tell the operator the aircraft
            # was disarmed while it was armed and spinning.
            #
            # So the ACK is taken at its word and the *uncertainty* is what
            # gets reported. The armed indicator on the bar is driven by
            # telemetry either way, so it keeps showing the truth as soon
            # as any arrives.
            text = (
                f"{action} accepted by the vehicle, but no telemetry "
                f"confirmation within {self.ARMED_STATE_CONFIRM_S:.0f}s. "
                f"Check the armed indicator"
            )
            self._console_publish("ARM", text, "warning")
            self._store_update_warning(text, "warning")
            return True
        completed = "Armed" if arm else "Disarmed"
        self._console_publish("ARM", f"{completed} successfully", "success")
        return True

    def _is_airborne(self) -> bool:
        """Is the aircraft off the ground? The vehicle's own answer first.

        Mirrors the frontend's reading of the same two fields, so the mission
        planner and the status bar cannot disagree about whether a takeoff item
        needs prepending.
        """
        snapshot = self._store.get_snapshot()
        landed = snapshot.get("landed_state")
        if landed in self._AIRBORNE_LANDED_STATES:
            return True
        if landed == 1:            # ON_GROUND, stated outright
            return False
        try:
            return float(snapshot.get("altitude_agl", 0.0)) > self.AIRBORNE_FALLBACK_M
        except (TypeError, ValueError):
            return False

    def _takeoff_altitude_amsl(self, altitude_agl: float) -> float | None:
        reference = self._home_alt_amsl
        if reference is None:
            reference = self._position_home_alt_amsl
        return None if reference is None else reference + altitude_agl

    def _await_takeoff_reference(self, altitude_agl: float) -> float | None:
        """Nudge the vehicle for home and wait briefly for an altitude reference.

        Returns the AMSL takeoff altitude once one can be derived, or None if
        nothing arrives inside the window.
        """
        self._request_home()
        deadline = time.monotonic() + self.TAKEOFF_REFERENCE_WAIT_S
        while time.monotonic() < deadline:
            if self._stop_event.is_set() or not self._connection_ready():
                return None
            amsl = self._takeoff_altitude_amsl(altitude_agl)
            if amsl is not None:
                return amsl
            time.sleep(0.05)
        return self._takeoff_altitude_amsl(altitude_agl)

    def _wait_for_mode(self, mode: str, timeout: float = MODE_CONFIRM_S) -> bool:
        """Watch the store until the vehicle reports *mode*, or time out.

        Sends nothing. ArduPilot refuses a guided takeoff outside GUIDED, and
        it refuses it from the *old* mode if the command overtakes the mode
        change on the wire — so the takeoff waits for the aircraft's own word
        rather than for the DO_SET_MODE ack, which only says the command was
        understood.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._stop_event.is_set() or not self._connection_ready():
                return False
            if self._store.get_snapshot().get("mode") == mode:
                return True
            time.sleep(0.05)
        return self._store.get_snapshot().get("mode") == mode

    def _enter_guided(self, action: str) -> bool:
        """Put the vehicle in the mode a guided command needs, if it needs one.

        PX4 switches itself and answers "" here, so this is a no-op on a PX4
        link. ArduPilot has to be in GUIDED first, and a vehicle already in a
        mode that accepts guided commands (GUIDED itself, or the AUTO_RTL /
        TAKEOFF modes that are guided states in disguise) is left alone rather
        than kicked out of it.
        """
        wanted = self._dialect.guided_mode(self._mav_type_id)
        if not wanted:
            return True
        current = self._store.get_snapshot().get("mode") or ""
        if current == wanted:
            return True
        if not self.set_mode(wanted):
            existing = self.get_last_command_error()
            self._set_command_error(
                f"{action} needs {wanted} mode, which the vehicle refused"
                + (f": {existing}" if existing else "")
            )
            return False
        if not self._wait_for_mode(wanted):
            # The vehicle ACCEPTED the mode change; its heartbeat has not caught
            # up. Proceed rather than refuse — the command that follows is
            # ack-confirmed on its own, and a refusal here would strand a
            # takeoff on a slow link that was about to work.
            self._console_publish(
                "MODE",
                f"{wanted} accepted but not yet confirmed in telemetry, "
                f"continuing with {action.lower()}",
                "warning",
            )
        return True

    def _enter_mission_mode(self, action: str) -> bool:
        """Switch to the mode that runs a stored mission.

        PX4 calls it MISSION (its AUTO.MISSION sub-mode); ArduPilot calls it
        AUTO. Hardcoding either name is how a mission upload that the vehicle
        accepted was then followed by "Unknown or unsupported mode: MISSION".
        """
        mode = self._dialect.mission_mode
        if not mode:
            self._set_command_error(
                f"{action}: Corvus does not know this autopilot's mission mode"
            )
            return False
        if mode not in self._mode_values and self._mode_values:
            # The live mapping is the authority when there is one — a vehicle
            # that names its mission mode something else entirely says so here
            # rather than after the command is refused.
            self._set_command_error(
                f"{action}: this vehicle does not offer a {mode} mode"
            )
            return False
        return self.set_mode(mode)

    def takeoff(self, altitude_agl: float = 10.0) -> bool:
        """Command a guided takeoff to *altitude_agl* metres above the ground.

        The two supported stacks want this staged in opposite orders, and the
        altitude in different frames:

        **PX4** accepts ``MAV_CMD_NAV_TAKEOFF`` from any mode, reads param7 as
        an **AMSL** altitude, and switches itself into AUTO.TAKEOFF. Arming
        comes afterwards, so a refused takeoff never leaves an armed aircraft
        sitting on the ground.

        **ArduPilot** reads param7 as an altitude **relative to home**, refuses
        the command outside GUIDED, and refuses it while disarmed. So the order
        inverts — mode, then arm, then takeoff — and this method disarms again
        if the last step fails, because the inverted order is the one that *can*
        leave an aircraft armed for no reason.

        Sending PX4's payload to ArduPilot is not a cosmetic mismatch: a 10 m
        takeoff becomes a request to climb to 10 m above sea level, which on
        most of the world's land is a command to stay put.
        """
        self._set_command_error("")
        if isinstance(altitude_agl, bool):
            self._set_command_error("Takeoff altitude must be a number")
            return False
        try:
            altitude_agl = float(altitude_agl)
        except (TypeError, ValueError):
            self._set_command_error("Takeoff altitude must be a number")
            return False
        if not math.isfinite(altitude_agl):
            self._set_command_error("Takeoff altitude must be finite")
            return False
        if not TAKEOFF_ALTITUDE_MIN_M <= altitude_agl <= TAKEOFF_ALTITUDE_MAX_M:
            self._set_command_error(
                f"Takeoff altitude must be between {TAKEOFF_ALTITUDE_MIN_M:.0f} and "
                f"{TAKEOFF_ALTITUDE_MAX_M:.0f} m AGL"
            )
            return False
        if not self._connection_ready():
            return self._command_failure("Takeoff", -2)
        nan = float("nan")
        plan = self._dialect.takeoff_plan(self._mav_type_id)
        # Resolved BEFORE the command lock is taken. Validation reads nothing
        # shared, and the reference wait below sends a single HOME request and
        # then watches the store for up to two seconds — so holding the lock
        # across it blocked every other command, including the abort commands,
        # while this one did nothing but read telemetry.
        if plan.altitude_frame == "relative":
            # ArduPilot flies param7 as metres above home, which is the number
            # the operator typed. There is nothing to convert and therefore no
            # altitude reference to wait for — the whole HOME_POSITION dance
            # below exists only because PX4 wants AMSL.
            takeoff_alt: float = altitude_agl
            altitude_note = f"Requesting {altitude_agl:.0f} m above home"
        else:
            alt_amsl = self._takeoff_altitude_amsl(altitude_agl)
            if alt_amsl is None:
                # No altitude reference yet — usually because HOME_POSITION and
                # GLOBAL_POSITION_INT simply have not arrived in the second
                # since connect. Ask for home once and give the streams a
                # moment rather than refusing on the spot.
                alt_amsl = self._await_takeoff_reference(altitude_agl)
            if alt_amsl is None:
                # Still nothing. param7 is left unspecified rather than
                # invented: NaN is this protocol's "use your own default" (the
                # same convention every unused param in this file uses), so the
                # vehicle takes off to its configured takeoff altitude.
                #
                # Refusing here was the wrong call. Corvus was deciding, on the
                # ground, that a flight could not happen — using a reference it
                # only ever needed in order to *convert* the operator's number,
                # not to validate it. Whether this aircraft can take off right
                # now is the autopilot's judgement, and it is far better placed
                # to make it; if it cannot, its own refusal reaches the operator
                # with a reason attached.
                takeoff_alt = nan
                text = (
                    "No altitude reference yet (no HOME_POSITION or global "
                    "position), so asking the vehicle to take off to its own "
                    f"configured altitude instead of {altitude_agl:.0f} m"
                )
                logger.warning("%s", text)
                self._console_publish("TAKEOFF", text, "warning")
                self._store_update_warning(text, "warning")
                altitude_note = ""
            else:
                takeoff_alt = alt_amsl
                altitude_note = (
                    f"Requesting {altitude_agl:.0f} m AGL ({alt_amsl:.1f} m AMSL)"
                )

        # Also before the lock: the mode change ArduPilot needs first is itself
        # an ack-confirmed command followed by a wait on telemetry, and holding
        # the command lock across it would block the abort commands for as long
        # as the vehicle took to answer.
        if plan.order == "mode_arm_takeoff" and not self._enter_guided("Takeoff"):
            return False

        params = [plan.min_pitch_deg, nan, nan, nan, nan, nan, takeoff_alt]
        with self._operation_lock:
            if not self._connection_ready():
                # Re-checked: the waits above are not instantaneous, and the
                # link may have gone in the meantime.
                return self._command_failure("Takeoff", -2)
            if altitude_note:
                self._console_publish("TAKEOFF", altitude_note, "info")

            if plan.order == "takeoff_then_arm":
                result = self._send_command_and_wait(
                    mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
                    params, timeout=5.0, retries=1,
                )
                if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                    return self._command_failure("Takeoff", result)
                self._console_publish(
                    "TAKEOFF", "Takeoff target accepted; arming …", "info")
                if not self.arm(True):
                    return False
            else:
                # ArduPilot refuses NAV_TAKEOFF while disarmed, so arming comes
                # first — and that is the order that can strand an armed
                # aircraft on the ground, so a refused takeoff disarms again.
                self._console_publish(
                    "TAKEOFF", "Arming before takeoff …", "info")
                if not self.arm(True):
                    return False
                result = self._send_command_and_wait(
                    mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
                    params, timeout=5.0, retries=1,
                )
                if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                    failure = (
                        f"Takeoff failed: "
                        f"{MAV_RESULT_TEXT.get(result, f'RESULT_{result}')}"
                    )
                    self._console_publish(
                        "TAKEOFF",
                        "Takeoff refused after arming, disarming again",
                        "warning",
                    )
                    self.arm(False)
                    # After the disarm, which clears the last error itself.
                    self._set_command_error(failure)
                    self._console_publish("TAKEOFF", failure, "error")
                    self._store_update_warning(failure, "critical")
                    return False
            self._console_publish("TAKEOFF", "Takeoff accepted and vehicle armed", "success")
            return True

    def land(self) -> bool:
        """Command land at current position with NaN lat/lon and ACK.

        Priority tier: this is one of the three commands an operator uses to
        end a flight that is going wrong, and it must not wait behind a
        parameter upload or a stream-rate batch.
        """
        with self._operation_lock.priority():
            self._set_command_error("")
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_NAV_LAND,
                [0.0, 0.0, nan, nan, nan, nan, nan],
                timeout=5.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Land", result)
            self._console_publish("LAND", "Land accepted", "success")
            return True

    def rtl(self) -> bool:
        """Command return to launch with ACK.

        Priority tier, for the same reason :meth:`land` is.
        """
        with self._operation_lock.priority():
            self._set_command_error("")
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_NAV_RETURN_TO_LAUNCH,
                [nan, nan, nan, nan, nan, nan, nan],
                timeout=5.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("RTL", result)
            self._console_publish("RTL", "RTL accepted", "success")
            return True

    def _validate_lat_lon(
        self, lat: Any, lon: Any, action: str,
    ) -> tuple[float, float] | None:
        """Coerce and range-check one coordinate pair, or None with the error set.

        Shares :meth:`_fly_to_point_number`'s bool rejection — ``True`` is an
        ``int`` in Python and would otherwise fly as latitude 1.0.
        """
        lat_f = self._fly_to_point_number(lat)
        lon_f = self._fly_to_point_number(lon)
        if lat_f is None or not math.isfinite(lat_f) or not -90.0 <= lat_f <= 90.0:
            self._set_command_error(f"{action} failed: lat out of range")
            return None
        if lon_f is None or not math.isfinite(lon_f) or not -180.0 <= lon_f <= 180.0:
            self._set_command_error(f"{action} failed: lon out of range")
            return None
        return lat_f, lon_f

    def set_home(self, lat: float, lon: float) -> bool:
        """Move the home position to (*lat*, *lon*), keeping its altitude.

        MAV_CMD_DO_SET_HOME with param1=0 ("use the specified location"), sent
        as COMMAND_INT so the coordinates survive the wire intact. Works on
        PX4 v1.16, v1.17 and v1.18.

        The altitude is NOT taken from the click: a map gives no terrain, and
        PX4 treats the z of this command as AMSL, so guessing would move home
        vertically as a side effect of moving it laterally. The current home
        altitude is reused instead (falling back to the reference derived from
        AMSL minus AGL, exactly as takeoff does), which makes this a purely
        horizontal move. Without either reference the command is refused
        rather than sent with a made-up altitude.

        Deliberately allowed while armed, like takeoff/land/rtl/arm: relocating
        home is how an operator redirects RTL mid-flight, so refusing it in the
        air would remove the case it is most needed for.
        """
        with self._operation_lock:
            self._set_command_error("")
            coords = self._validate_lat_lon(lat, lon, "Set home")
            if coords is None:
                return False
            lat_f, lon_f = coords

            alt_amsl = self._takeoff_altitude_amsl(0.0)
            if alt_amsl is None:
                # Same brief wait the takeoff path takes: right after connect
                # the reference is usually just late, not absent.
                alt_amsl = self._await_takeoff_reference(0.0)
            if alt_amsl is None:
                # Unlike takeoff, this one keeps its refusal. The altitude here
                # is not a number being converted for the wire — it is the
                # altitude home will *have*, and home altitude is what RTL
                # descends to. A guessed value moves the landing point
                # vertically as a side effect of dragging it sideways, which is
                # the one outcome this command exists to avoid.
                text = "Set home failed: no home or global altitude reference"
                self._set_command_error("no home or global altitude reference")
                self._console_publish("SETHOME", text, "error")
                self._store_update_warning(text, "critical")
                return False

            nan = float("nan")
            result = self._send_command_int_and_wait(
                mavutil.mavlink.MAV_CMD_DO_SET_HOME,
                mavutil.mavlink.MAV_FRAME_GLOBAL,
                [0.0, nan, nan, nan],
                x=int(round(lat_f * 1e7)),
                y=int(round(lon_f * 1e7)),
                z=alt_amsl,
                timeout=5.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Set home", result)
            # PX4 publishes HOME_POSITION when home changes, but the operator
            # has just clicked a spot and is watching for the marker to move
            # there. Ask for it rather than leaving the confirmation to the
            # next scheduled one.
            self._request_home()
            self._console_publish(
                "SETHOME", f"Home set to {lat_f:.7f}, {lon_f:.7f}", "success",
            )
            return True
