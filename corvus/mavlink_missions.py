"""The MAVLink mission protocol: fly to points, planned missions, clear.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`. The transfer
protocol itself (the upload and download handshakes for missions and
geofences, the item requests served during an upload, MISSION_CURRENT and
MISSION_ITEM_REACHED, the record of what is on the vehicle and the vehicle's
verdict on a new mission) is ``corvus_link.missions.MissionEngine``, in Rust.
It reads the tunables below and this module's ``time`` when it needs them.
The flight sequences on top (fly to points, mission start with its mode and
arming order, reposition) are still here, and move with the flight commands.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import logging
import re
import sys
import threading
import time  # noqa: F401 - the engine reads this module's clock, and tests replace it
from typing import Any

from corvus_link import mavutil
from corvus_link import missions as _engine

from . import geofence
from . import mission as mission_plan
from .autopilot import is_ground_vehicle
# Read by the engine from this module, like the tunables below.
from .mavlink_common import TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M, _PendingAck  # noqa: F401

logger = logging.getLogger("corvus.mavlink")

# Waypoints one "fly to points" request may carry.
#
# There was no bound at all, and the mission upload waits
# 2 s per item while holding the lock every other command takes — so a request
# carrying a few thousand points parked arm, land and RTL behind it for hours.
# A route an operator clicks onto a map is tens of points; 255 is far past
# anything hand-planned and still bounds the upload to something an operator
# can wait out.
FLY_TO_MAX_POINTS = 255

# corvus/mission.py states each planner item's MAV_CMD as a frozen literal so
# it stays importable without corvus_link. This is where that claim is checked:
# a mismatch is a wire-format bug, and it fails at import rather than uploading
# a mission whose "circle" turns out to be some other command entirely.
_MISSION_CMD_NAMES: dict[str, str] = {
    "takeoff": "MAV_CMD_NAV_TAKEOFF",
    "waypoint": "MAV_CMD_NAV_WAYPOINT",
    "loiter_turns": "MAV_CMD_NAV_LOITER_TURNS",
    "loiter_time": "MAV_CMD_NAV_LOITER_TIME",
    "land": "MAV_CMD_NAV_LAND",
    "rtl": "MAV_CMD_NAV_RETURN_TO_LAUNCH",
}
assert set(_MISSION_CMD_NAMES) == set(mission_plan.COMMAND_OF), \
    "mission.py item types and the MAV_CMD name table disagree"
for _kind, _name in _MISSION_CMD_NAMES.items():
    assert mission_plan.COMMAND_OF[_kind] == getattr(mavutil.mavlink, _name), \
        f"mission.py MAV_CMD for {_kind} disagrees with corvus_link"
assert mission_plan.MAV_CMD_DO_CHANGE_SPEED == mavutil.mavlink.MAV_CMD_DO_CHANGE_SPEED
assert geofence.MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION == \
    mavutil.mavlink.MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION
assert geofence.MAV_MISSION_TYPE_FENCE == mavutil.mavlink.MAV_MISSION_TYPE_FENCE

# Non-navigation mission items (DO_CHANGE_SPEED) travel in MAV_FRAME_MISSION:
# they name no place, and PX4's feasibility checker reads the frame of every
# item it walks.
MISSION_DO_FRAME = mavutil.mavlink.MAV_FRAME_MISSION

# MAV_DO_REPOSITION_FLAGS_CHANGE_MODE: switch into the mode that holds the new
# position (PX4: Hold, ArduPilot: GUIDED) rather than only moving a target the
# vehicle may not be following. Pinned by number: not every corvus_link build
# carries the enum.
DO_REPOSITION_CHANGE_MODE = 1
# How long the upload waits with no MISSION_REQUEST and no MISSION_ACK before
# calling it lost. PX4 requests items back to back, so a gap this long means
# the vehicle has stopped asking — a dropped MISSION_COUNT, or a mission
# rejected without an ACK. Generous enough for a 57 kbps radio mid-burst.
MISSION_UPLOAD_QUIET_S = 10.0
# MISSION_COUNT is the one frame of an upload the vehicle does not ask for
# again: lost on the radio, the vehicle never starts requesting items. It is
# sent again after this long without a request (longer on a radio), a few
# times, inside the quiet window above.
MISSION_COUNT_RETRY_S = 1.5
MISSION_COUNT_RETRY_S_SLOW = 3.0
MISSION_COUNT_TRIES = 3
# Absolute ceiling on one upload, whatever the item count.
MISSION_UPLOAD_MAX_S = 120.0

# Download: how long one request waits for its answer, and how often it is
# asked again before the item counts as lost. A radio gets the longer wait:
# at 57 kbps the answer shares the link with the telemetry stream.
MISSION_DOWNLOAD_WAIT_S = 1.5
MISSION_DOWNLOAD_WAIT_S_SLOW = 3.0
MISSION_DOWNLOAD_TRIES = 4
# An item that does not arrive is not asked for again as it stands: the vehicle
# may have dropped the transfer (PX4 answers a request outside one with "Req. WP
# was unexpected"), and a request it does not expect is never answered. The
# whole handshake starts again from MISSION_REQUEST_LIST, this many times.
MISSION_DOWNLOAD_RESTARTS = 3
_UNEXPECTED_REQUEST = "req. wp was unexpected"

# MAV_MISSION_RESULT a GCS sends to end a transfer it is abandoning, so the
# vehicle's mission manager is not left waiting for the next request.
MAV_MISSION_OPERATION_CANCELLED = 15

# MISSION_CURRENT's MISSION_STATE extension (PX4 v1.14+; 0 from a firmware
# that does not fill it in, which reads as "not reported").
MISSION_STATE_NAMES: dict[int, str] = {
    1: "no_mission", 2: "not_started", 3: "active", 4: "paused", 5: "complete",
}
# MISSION_CURRENT.total when the vehicle holds no mission at all.
MISSION_TOTAL_NONE = 65535
# How long after Corvus's own transfer a MISSION_CURRENT still counting the
# old mission is taken for one sent before the transfer, not for a change.
MISSION_CHANGE_GRACE_S = 5.0

# After an accepted upload, how long a stack that reports a verdict (see
# Dialect.mission_validity_reported) is given to say whether it will fly the
# mission. PX4 streams MISSION_CURRENT at 1 Hz, so two periods and a margin.
# A report sooner than the settle time after the ACK may have been put
# together before the navigator checked the new mission, and is not its
# verdict: taken as one, an earlier rejected mission would fail a good upload.
MISSION_VERDICT_WAIT_S = 2.5
MISSION_VERDICT_SETTLE_S = 0.3
_REJECTED_PREFIX = re.compile(r"^mission rejected:\s*", re.IGNORECASE)


def _engine_attribute(name: str) -> property:
    """A bridge attribute the mission state used to be, now the engine's."""
    def get(self: Any) -> Any:
        return getattr(self._mission_engine, name)

    def set_(self: Any, value: Any) -> None:
        setattr(self._mission_engine, name, value)

    return property(get, set_)


class MissionProtocolMixin:
    """MISSION_COUNT / MISSION_REQUEST(_INT) / MISSION_ITEM(_INT) / MISSION_ACK."""

    # The state the bridge used to hold itself, kept addressable by its old
    # names: the shutdown path, the vehicle list and the tests read them.
    _mission_items = _engine_attribute("items")
    _mission_upload_type = _engine_attribute("upload_type")
    _vehicle_fence = _engine_attribute("vehicle_fence")
    _pending_mission_ack = _engine_attribute("pending_ack")
    _mission_last_request = _engine_attribute("last_request")
    _vehicle_mission_index = _engine_attribute("vehicle_index")
    _vehicle_mission_id = _engine_attribute("vehicle_mission_id")
    _vehicle_mission_id_pending = _engine_attribute("vehicle_mission_id_pending")
    _vehicle_mission_confirmed = _engine_attribute("vehicle_mission_confirmed")
    _vehicle_mission_noted_at = _engine_attribute("vehicle_mission_noted_at")
    _mission_revision = _engine_attribute("revision")
    _mission_verdict = property(lambda self: self._mission_engine.verdict)

    def _init_mission_state(self) -> None:
        """Set up the mission protocol; called from the bridge's __init__."""
        self._mission_engine = _engine.MissionEngine(sys.modules[__name__])
        # Kept for code that pairs reads of the state above under one lock;
        # the engine guards its own state.
        self._mission_lock = threading.Lock()

    def mission_capabilities(self) -> dict[str, Any]:
        """The planner's part of GET /api/mavlink/capabilities.

        ``mission_unsupported`` maps each plan item type this stack will not
        fly to the reason, so the Mission page greys the tool with that reason
        on it rather than letting a plan be drawn that the upload refuses.
        """
        return {"mission_unsupported": mission_plan.refused_item_types(
            self._dialect.mission_refusals(self._mav_type_id))}

    def _handle_mission_request(self, msg: Any, as_int: bool) -> None:
        """Serve one MISSION_ITEM(_INT) during an upload we initiated."""
        self._mission_engine.handle_request(self, msg, as_int)

    # ------------------------------------------------------------------
    # Download (MISSION_REQUEST_LIST) and progress (MISSION_CURRENT)
    # ------------------------------------------------------------------

    def _handle_mission_count(self, msg: Any) -> None:
        """The vehicle's answer to MISSION_REQUEST_LIST: how many items."""
        self._mission_engine.handle_count(self, msg)

    def _handle_mission_item(self, msg: Any, as_int: bool) -> None:
        """One item of a download, kept only if it is the one asked for."""
        self._mission_engine.handle_item(self, msg, as_int)

    def _mission_offset(self) -> int:
        """Vehicle sequence numbers minus this is the plan's numbering."""
        return self._mission_engine.mission_offset(self)

    def _plan_item_of(self, seq: int) -> int:
        return self._mission_engine.plan_item_of(seq)

    def _handle_mission_current(self, msg: Any) -> None:
        """Which item the vehicle is flying to, and whether its mission changed.

        Somebody else may have put a mission on the vehicle (QGroundControl, a
        companion, a second Corvus); then no leg of Corvus' plan is claimed to
        be flown. The signals are the mission id PX4 v1.14+ sends and the item
        count every stack sends.
        """
        self._mission_engine.handle_current(self, msg)

    def _handle_mission_item_reached(self, msg: Any) -> None:
        self._mission_engine.handle_item_reached(self, msg)

    def _note_vehicle_mission(self, plan_index: list[int | None] | None) -> None:
        """Record what Corvus just put on (or read off) the vehicle."""
        self._mission_engine.note_vehicle_mission(self, plan_index)

    def _forget_vehicle_mission(self, reason: str = "") -> None:
        """Corvus no longer knows what is on the vehicle (a new link, another station)."""
        self._mission_engine.forget_vehicle_mission(self, reason)

    def download_mission(self) -> dict[str, Any] | None:
        """Read the mission stored on the vehicle, and read it back as a plan.

        MISSION_REQUEST_LIST, then one MISSION_REQUEST_INT per item, asked
        again until it arrives or :data:`MISSION_DOWNLOAD_TRIES` is spent, the
        whole handshake started over :data:`MISSION_DOWNLOAD_RESTARTS` times,
        then MISSION_ACK. ArduPilot's home slot is left out. A mission longer
        than the planner holds is refused after the count. An abort waiting
        for the command lock cancels the transfer on the vehicle.

        Returns ``{"items", "count"}`` plus :func:`corvus.mission.items_to_plan`
        output, or None with the reason as the last command error.
        """
        with self._operation_lock:
            return self._mission_engine.download_mission(self)

    def _send_mission_ack(self, result: int, mission_type: int = 0) -> None:
        self._mission_engine.send_ack(self, result, mission_type)

    def _cancel_mission_download(self) -> None:
        self._send_mission_ack(MAV_MISSION_OPERATION_CANCELLED)

    def _handle_mission_ack(self, msg: Any) -> None:
        """Resolve the pending upload waiter with the MISSION_ACK result."""
        self._mission_engine.handle_ack(self, msg)

    def _upload_mission(self, items: list[dict[str, Any]], mission_type: int = 0) -> int:
        """Upload mission items and wait for MISSION_ACK.

        *mission_type* is MAV_MISSION_TYPE: 0 for the mission, FENCE for the
        geofence. Returns MAV_MISSION_ACCEPTED on success, the MISSION_ACK type
        on rejection, -1 on timeout, -2 on disconnect/shutdown, -3 when an
        abort command (RTL, Land, Disarm, a mode change) is waiting: the
        transfer is cancelled on the vehicle, which keeps the mission it had,
        and the abort goes first. Bounded by progress: a vehicle that stops
        asking for items ends it after :data:`MISSION_UPLOAD_QUIET_S`.
        """
        return self._mission_engine.upload(self, items, mission_type)

    # ------------------------------------------------------------------
    # Fly to points (Punktabflug)
    # ------------------------------------------------------------------

    @staticmethod
    def _fly_to_point_number(value: Any) -> float | None:
        """Coerce a point field to float, rejecting bool (subclass of int)."""
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return None
        return float(value)

    def _validate_fly_to_points(
        self, points: list[dict[str, float]],
    ) -> list[tuple[float, float, float]] | None:
        """Validate the operator payload. Returns cleaned points or None."""
        return self._mission_engine.validate_points(self, points)

    @staticmethod
    def _build_mission_item_spec(
        seq: int, command: int, lat: float, lon: float, alt: float,
        p1: float, p2: float, p3: float, p4: float,
        frame: int | None = None,
    ) -> dict[str, Any]:
        """Build one MISSION_ITEM_INT / MISSION_ITEM send-spec.

        Frame defaults to MAV_FRAME_GLOBAL_RELATIVE_ALT (relative to home =
        AGL). A planner item that names no place passes MISSION_DO_FRAME.
        """
        return _engine.MissionEngine.build_item_spec(
            seq, command, lat, lon, alt, p1, p2, p3, p4, frame)

    def _with_mission_home_slot(
        self, specs: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
        """Prepend the home slot ArduPilot reserves at mission sequence 0.

        PX4 numbers mission items from 0; ArduPilot keeps *home* in slot 0,
        and an upload that ignores that stores the first item as home and
        flies the mission without its takeoff. A zero-item upload (a clear)
        is passed through.
        """
        return self._mission_engine.with_home_slot(self, specs)

    def _mission_start_range(self, count: int) -> tuple[float, float]:
        """The (first, last) item indices MAV_CMD_MISSION_START should carry.

        *count* is how many items the planner produced, before the home slot.
        The pair is the dialect's: PX4 is told both ends, ArduPilot is sent
        (0, 0) because 4.6 refuses any other pair.
        """
        return self._dialect.mission_start_params(count)

    def _start_uploaded_mission(self, action: str, count: int, mark: int | None = None) -> bool:
        """MISSION_START, the mission mode and arming, in this stack's order.

        PX4's MISSION_START switches to the mission and arms by itself; the
        mode change and arm that follow only confirm it. ArduPilot's switches
        to AUTO and nothing more, and a copter will not arm in AUTO, so a
        vehicle still on the ground is put in the dialect's arming mode and
        armed first. That order can leave an aircraft armed for nothing, so a
        MISSION_START refused after arming disarms again, as takeoff does.

        Called without the command lock: each step takes it for its own round
        trip, so Disarm, Land, RTL or a mode change never waits behind the
        mode and arm confirmations, and a sequence one of them interrupted
        sends nothing more (*mark*, see :meth:`_sequence_interrupted`).
        Returns False with the reason set.
        """
        if mark is None:
            mark = self._operation_lock.abort_mark()
        nan = float("nan")
        first, last = self._mission_start_range(count)
        arm_mode = self._dialect.mission_arm_mode(self._mav_type_id)
        armed_here = False
        if arm_mode and not self._store.get_snapshot().get("armed"):
            if arm_mode == self._dialect.mission_mode:
                entered = self._enter_mission_mode(action)
            else:
                entered = self._enter_guided(action)
            if not entered:
                return False
            if self._sequence_interrupted(mark, action):
                return False
            if not self.arm(True):
                return False
            armed_here = True
        if self._sequence_interrupted(mark, action):
            return False
        with self._operation_lock:
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_MISSION_START,
                [first, last, nan, nan, nan, nan, nan],
                timeout=5.0, retries=1,
            )
        if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
            if armed_here:
                self._console_publish(
                    "MISSION", "Mission start refused after arming, disarming again",
                    "warning",
                )
                self.arm(False)
            return self._command_failure(action, result)
        if arm_mode:
            # ArduPilot's MISSION_START is itself the switch to AUTO, and the
            # vehicle was armed above (or already was).
            return True
        # PX4's Commander accepts MISSION_START only after it has switched to
        # AUTO.MISSION and armed, so the ACK already says both. A DO_SET_MODE
        # and an arm sent after it used to follow anyway: two more mode events
        # for the operator, and on a short hop that had already finished, a
        # second MISSION that restarted the mission. So telemetry is watched
        # instead, and nothing more is sent.
        mode = self._dialect.mission_mode
        if mode and not self._wait_for_mode(mode):
            snapshot = self._store.get_snapshot()
            if snapshot.get("mode") != mode:
                self._console_publish(
                    "MISSION",
                    f"{action}: accepted, but the vehicle reports "
                    f"{snapshot.get('mode') or 'no mode'} rather than {mode}",
                    "warning",
                )
        return True

    @staticmethod
    def _mission_ack_to_result(ack_type: int) -> int:
        """Map MAV_MISSION_RESULT to MAV_RESULT for "Fly to points failed: ..."."""
        return _engine.MissionEngine.ack_to_result(ack_type)

    def _takeoff_item_position(
        self, first: tuple[float, float, float],
    ) -> tuple[float, float]:
        """Where a prepended takeoff item is placed: the vehicle, else home, else the first point.

        Never 0/0: PX4 counts the takeoff item as the first waypoint.
        """
        return self._mission_engine.takeoff_item_position(self, first)

    def _reposition_to(self, lat: float, lon: float, alt_agl: float) -> bool | None:
        """Send an airborne vehicle to one point with MAV_CMD_DO_REPOSITION.

        The one point "Fly to this point" asks for does not need a mission:
        uploading one replaced whatever mission was on board, and flew through
        MISSION into LOITER, two mode changes and two notifications for a
        single hop. A reposition moves the Hold target (GUIDED on ArduPilot)
        and the vehicle stays in the mode it is in when it is already holding.

        Returns None when the vehicle cannot take it (no altitude reference to
        convert AGL on PX4, or the command is unsupported), so the caller
        falls back to the mission, which needs neither.
        """
        nan = float("nan")
        if self._dialect.reposition_altitude_frame(self._mav_type_id) == "relative":
            frame = mavutil.mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT
            altitude = float(alt_agl)
        else:
            amsl = self._takeoff_altitude_amsl(alt_agl)
            if amsl is None:
                return None
            frame = mavutil.mavlink.MAV_FRAME_GLOBAL
            altitude = float(amsl)
        action = "Drive to point" if is_ground_vehicle(self._mav_type_id) else "Fly to point"
        if not self._enter_guided(action):
            return False
        result = self._send_command_int_and_wait(
            mavutil.mavlink.MAV_CMD_DO_REPOSITION, frame,
            [-1.0, float(DO_REPOSITION_CHANGE_MODE), 0.0, nan],
            x=int(round(lat * 1e7)), y=int(round(lon * 1e7)), z=altitude,
            timeout=5.0, retries=1,
        )
        if result == mavutil.mavlink.MAV_RESULT_UNSUPPORTED:
            return None
        if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
            return self._command_failure(action, result)
        if is_ground_vehicle(self._mav_type_id):
            self._console_publish("GOTOPOINTS", "Driving to point", "success")
        else:
            self._console_publish(
                "GOTOPOINTS", f"Flying to point at {alt_agl:.0f} m AGL", "success",
            )
        return True

    def _can_reposition(self, ground: bool, on_ground: bool) -> bool:
        """Is one point best sent as a DO_REPOSITION rather than a mission?

        An aircraft that is in the air, yes. A rover has no air to be in, so it
        is armed instead, and only on ArduPilot: PX4 reads a reposition's
        altitude as AMSL and a rover has none to give.
        """
        if not ground:
            return not on_ground
        relative = self._dialect.reposition_altitude_frame(self._mav_type_id) == "relative"
        return relative and bool(self._store.get_snapshot().get("armed"))

    def fly_to_points(self, points: list[dict[str, float]]) -> bool:
        """Fly to the given points in order (Punktabflug) by uploading a mission.

        Each point is ``{"lat","lon","alt_agl"}`` with ``alt_agl`` in metres
        above home (same reference as :meth:`takeoff`). Uploads a
        MISSION_ITEM_INT mission (MAV_FRAME_GLOBAL_RELATIVE_ALT), then starts
        and arms it in the order the connected stack needs (see
        :meth:`_start_uploaded_mission`). Returns True only when the mission is
        accepted, started, and the vehicle is armed. Works on PX4 v1.16, v1.17
        and v1.18, and on ArduPilot 4.3 to 4.6.

        One point for a vehicle already in the air is a reposition instead
        (see :meth:`_reposition_to`), which leaves the stored mission alone.
        """
        self._set_command_error("")
        mark = self._operation_lock.abort_mark()
        ground = is_ground_vehicle(self._mav_type_id)
        label = "Drive to points" if ground else "Fly to points"
        cleaned = self._validate_fly_to_points(points)
        if cleaned is None:
            return False
        if not self._connection_ready():
            return self._command_failure(label, -2)
        # No altitude-reference gate here, on purpose. There used to be one,
        # and it was pure ceremony: every item below is built in
        # MAV_FRAME_GLOBAL_RELATIVE_ALT, whose z *is* the AGL number the
        # operator typed, so the AMSL value the gate computed was thrown away
        # without ever being sent. It refused missions over a conversion the
        # mission does not need.
        on_ground = not ground and not self._is_airborne()
        if len(cleaned) == 1 and self._can_reposition(ground, on_ground):
            # A mode change and one command, each under the lock for itself.
            handled = self._reposition_to(*cleaned[0])
            if handled is not None:
                return handled
        nan = float("nan")
        items: list[dict[str, Any]] = []
        seq = 0
        if on_ground:
            # Climb to the first waypoint's AGL, then proceed to the points.
            takeoff_agl = cleaned[0][2]
            takeoff_lat, takeoff_lon = self._takeoff_item_position(cleaned[0])
            items.append(self._build_mission_item_spec(
                seq, mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
                takeoff_lat, takeoff_lon, takeoff_agl, nan, nan, nan, nan,
            ))
            seq += 1
        for lat, lon, alt_agl in cleaned:
            items.append(self._build_mission_item_spec(
                seq, mavutil.mavlink.MAV_CMD_NAV_WAYPOINT,
                lat, lon, alt_agl, 0.0, nan, nan, nan,
            ))
            seq += 1

        planned = len(items)
        items = self._with_mission_home_slot(items)
        # The upload is a handshake and holds the lock; it gives way to an
        # abort by cancelling itself. Starting is a sequence, and does not.
        with self._operation_lock:
            if not self._connection_ready():
                return self._command_failure(label, -2)
            self._console_publish(
                "GOTOPOINTS", f"Uploading {planned} mission item(s) …", "info",
            )
            ack = self._upload_mission(items)
            if ack != mavutil.mavlink.MAV_MISSION_ACCEPTED:
                result = ack if ack < 0 else self._mission_ack_to_result(ack)
                return self._command_failure(label, result)
            # Not a plan: the Mission page's plan is no longer what is on board.
            self._note_vehicle_mission(None)

        self._console_publish(
            "GOTOPOINTS", "Mission accepted; starting …", "info",
        )
        if not self._start_uploaded_mission(label, planned, mark):
            return False

        self._console_publish("GOTOPOINTS", f"{label} started", "success")
        return True

    # ------------------------------------------------------------------
    # Mission plan (the Mission page)
    # ------------------------------------------------------------------
    #
    # Separate from fly_to_points on purpose. "Fly to points" is one gesture —
    # click, click, FLY — and uploading, starting and arming are all part of
    # that one press. A planned mission is drawn, reviewed, saved and only then
    # flown, so upload and start are two decisions and two methods: an operator
    # must be able to put a route on the aircraft and walk out to it before
    # anything spins.

    def lower_mission_plan(self, items: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """*items* as this autopilot flies them: a Circle as the waypoints
        around its rim where the stack has no NAV_LOITER_TURNS in a mission.

        The HTTP layer calls this before it uploads, because the count it
        starts the mission with has to be the count on the vehicle.
        """
        if self._dialect.mission_circles_as_waypoints:
            return mission_plan.lower_circles(items)
        return items

    def upload_mission_plan(self, items: list[dict[str, Any]]) -> bool:
        """Upload a planned mission. Does not start it and does not arm.

        *items* is :func:`corvus.mission.plan_to_items` output, a flat list of
        ``{"command", "lat", "lon", "alt", "params": [p1..p4]}``. Returns True
        only on MAV_MISSION_ACCEPTED and when a stack that reports a verdict
        (see Dialect.mission_validity_reported) does not reject it.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not isinstance(items, list) or not items:
                self._set_command_error("mission must carry at least one item")
                return False
            drawn = len(items)
            items = self.lower_mission_plan(items)
            return self._mission_engine.upload_plan(self, items, drawn)

    def _mission_refusal(self, items: list[dict[str, Any]]) -> str:
        """Why this stack will not take *items*, or "" when it will."""
        return self._mission_engine.refusal(self, items)

    def _home_lat_lon(self) -> tuple[float, float] | None:
        """The home the vehicle reported, as (lat, lon), or None before it has one."""
        return self._mission_engine.home_lat_lon(self)

    def start_mission(self, count: int) -> bool:
        """Run the mission already on the vehicle: MISSION_START, AUTO, arm.

        *count* is how many items were uploaded. On PX4 MAV_CMD_MISSION_START
        carries an explicit first/last pair rather than the "0 = last item"
        convention, which is the one part of this that differs across v1.16 to
        v1.18; the order of the steps is the dialect's, see
        :meth:`_start_uploaded_mission`.
        """
        self._set_command_error("")
        mark = self._operation_lock.abort_mark()
        if isinstance(count, bool) or not isinstance(count, int) or count <= 0:
            self._set_command_error("mission item count must be positive")
            return False
        if not self._connection_ready():
            return self._command_failure("Mission start", -2)
        if not self._start_uploaded_mission("Mission start", count, mark):
            return False
        self._console_publish("MISSION", "Mission started", "success")
        return True

    def clear_mission(self) -> bool:
        """Wipe the mission stored on the vehicle: a zero-item upload, so a
        vehicle that refuses reports why, as a real upload does."""
        with self._operation_lock:
            return self._mission_engine.clear_mission(self)

    # ------------------------------------------------------------------
    # Geofence (mission_type FENCE)
    # ------------------------------------------------------------------

    def vehicle_fence(self) -> list[list[float]] | None:
        """The area last put on the vehicle over this link, [] after a clear,
        None when this link has not transferred one."""
        return self._vehicle_fence

    def upload_fence(self, polygon: list[list[float]]) -> bool:
        """Put *polygon* (``[[lon, lat], ...]``) on the vehicle as its inclusion fence.

        Replaces whatever fence the vehicle held. Does not decide whether the
        fence acts: that is a parameter, written by the caller from the safety
        schema. Returns True only on MAV_MISSION_ACCEPTED.
        """
        with self._operation_lock:
            return self._mission_engine.upload_fence(self, polygon)

    def clear_fence(self) -> bool:
        """Remove every fence the vehicle holds: a zero-item FENCE transfer."""
        with self._operation_lock:
            return self._mission_engine.clear_fence(self)
