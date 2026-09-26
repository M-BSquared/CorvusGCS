"""The MAVLink mission protocol: fly to points, planned missions, clear.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import logging
import math
import threading
import time
from typing import Any

from pymavlink import mavutil

from . import mission as mission_plan
from .mavlink_common import TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M, _PendingAck

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
# it stays importable without pymavlink. This is where that claim is checked:
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
        f"mission.py MAV_CMD for {_kind} disagrees with pymavlink"
assert mission_plan.MAV_CMD_DO_CHANGE_SPEED == mavutil.mavlink.MAV_CMD_DO_CHANGE_SPEED

# Non-navigation mission items (DO_CHANGE_SPEED) travel in MAV_FRAME_MISSION:
# they name no place, and PX4's feasibility checker reads the frame of every
# item it walks.
MISSION_DO_FRAME = mavutil.mavlink.MAV_FRAME_MISSION
# How long the upload waits with no MISSION_REQUEST and no MISSION_ACK before
# calling it lost. PX4 requests items back to back, so a gap this long means
# the vehicle has stopped asking — a dropped MISSION_COUNT, or a mission
# rejected without an ACK. Generous enough for a 57 kbps radio mid-burst.
MISSION_UPLOAD_QUIET_S = 10.0
# Absolute ceiling on one upload, whatever the item count.
MISSION_UPLOAD_MAX_S = 120.0

# Download: how long one request waits for its answer, and how often it is
# asked again before the item counts as lost. A radio gets the longer wait:
# at 57 kbps the answer shares the link with the telemetry stream.
MISSION_DOWNLOAD_WAIT_S = 1.5
MISSION_DOWNLOAD_WAIT_S_SLOW = 3.0
MISSION_DOWNLOAD_TRIES = 4

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


class _MissionDownload:
    """One download in flight: what the receive thread fills in, and the wake-up."""

    def __init__(self) -> None:
        self.event = threading.Event()
        self.count: int | None = None
        self.waiting_for: int = -1
        self.items: dict[int, dict[str, Any]] = {}


def _finite_or_none(value: Any) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None



class MissionProtocolMixin:
    """MISSION_COUNT / MISSION_REQUEST(_INT) / MISSION_ITEM(_INT) / MISSION_ACK."""

    def _init_mission_state(self) -> None:
        """Set up the upload handshake state; called from the bridge's __init__."""
        # Mission-upload handshake state (set only during fly_to_points). The
        # receive thread reads _mission_items on MISSION_REQUEST_INT and wakes
        # _pending_mission_ack on MISSION_ACK; _mission_lock guards both.
        self._mission_lock = threading.Lock()
        self._mission_items: list[dict[str, Any]] | None = None
        self._pending_mission_ack: _PendingAck | None = None
        # When the vehicle last asked for an item. The upload watches this so a
        # stalled handshake ends in seconds rather than sitting out a budget
        # scaled to the item count.
        self._mission_last_request: float = 0.0
        # A download in flight, filled in by the receive thread.
        self._mission_download: _MissionDownload | None = None
        # What Corvus knows is on the vehicle: the plan item each vehicle item
        # belongs to, from the last upload or download this link made. None is
        # "not known", which is the honest answer after a reconnect, or once
        # the vehicle reports a mission id that is not the one Corvus left.
        self._vehicle_mission_index: list[int | None] | None = None
        # The id the vehicle gave its mission (MISSION_CURRENT.mission_id,
        # PX4 only). Adopted from the first report after Corvus's own transfer,
        # and compared against every report after that.
        self._vehicle_mission_id: int | None = None
        self._vehicle_mission_id_pending = False
        # The count check: once the vehicle has reported the count Corvus
        # expects, any other count is a mission somebody else put there.
        self._vehicle_mission_confirmed = False
        self._vehicle_mission_noted_at = 0.0
        self._mission_revision = 0

    def _handle_mission_request(self, msg: Any, as_int: bool) -> None:
        """Serve one MISSION_ITEM(_INT) during an upload we initiated."""
        conn = self._conn
        if not conn or not self._ack_is_for_us(msg):
            return
        seq = int(getattr(msg, "seq", -1))
        with self._mission_lock:
            items = self._mission_items
        if items is None or not (0 <= seq < len(items)):
            return
        item = items[seq]
        with self._mission_lock:
            self._mission_last_request = time.monotonic()
        try:
            with self._send_lock:
                # Connection swapped under us (A6): drop the request.
                if conn is not self._conn:
                    return
                if as_int:
                    conn.mav.mission_item_int_send(
                        self._target_system, self._target_component, seq,
                        item["frame"], item["command"], item["current"],
                        item["autocontinue"], item["param1"], item["param2"],
                        item["param3"], item["param4"], item["x_int"],
                        item["y_int"], item["z"],
                    )
                else:
                    conn.mav.mission_item_send(
                        self._target_system, self._target_component, seq,
                        item["frame"], item["command"], item["current"],
                        item["autocontinue"], item["param1"], item["param2"],
                        item["param3"], item["param4"], item["x_f"],
                        item["y_f"], item["z"],
                    )
        except Exception as exc:
            logger.debug("mission item send failed (seq=%d): %s", seq, exc)

    # ------------------------------------------------------------------
    # Download (MISSION_REQUEST_LIST) and progress (MISSION_CURRENT)
    # ------------------------------------------------------------------

    def _handle_mission_count(self, msg: Any) -> None:
        """The vehicle's answer to MISSION_REQUEST_LIST: how many items."""
        if not self._ack_is_for_us(msg) or int(getattr(msg, "mission_type", 0) or 0):
            return
        with self._mission_lock:
            download = self._mission_download
            if download is None or download.count is not None:
                return
            try:
                download.count = max(0, int(msg.count))
            except (TypeError, ValueError):
                return
            download.event.set()

    def _handle_mission_item(self, msg: Any, as_int: bool) -> None:
        """One item of a download, kept only if it is the one asked for."""
        if not self._ack_is_for_us(msg) or int(getattr(msg, "mission_type", 0) or 0):
            return
        try:
            seq = int(msg.seq)
            x, y = msg.x, msg.y
            item = {
                "command": int(msg.command),
                "frame": int(msg.frame),
                "lat": (int(x) / 1e7) if as_int else float(x),
                "lon": (int(y) / 1e7) if as_int else float(y),
                "alt": _finite_or_none(msg.z) or 0.0,
                # NaN is how a yaw that was never asked for travels; kept as
                # None so the item survives JSON and means the same thing.
                "params": [_finite_or_none(getattr(msg, f"param{n}", None))
                           for n in range(1, 5)],
                "autocontinue": int(getattr(msg, "autocontinue", 1) or 0),
            }
        except (AttributeError, TypeError, ValueError):
            logger.debug("discarding malformed mission item")
            return
        with self._mission_lock:
            download = self._mission_download
            if download is None or seq != download.waiting_for:
                return
            download.items[seq] = item
            download.event.set()

    def _mission_offset(self) -> int:
        """Vehicle sequence numbers minus this is the plan's numbering."""
        return 1 if self._dialect.mission_seq0_is_home else 0

    def _plan_item_of(self, seq: int) -> int:
        index = self._vehicle_mission_index
        if index is None or not 0 <= seq < len(index) or index[seq] is None:
            return -1
        return int(index[seq])

    def _handle_mission_current(self, msg: Any) -> None:
        """Which item the vehicle is flying to, and whether its mission changed."""
        try:
            seq = int(msg.seq) - self._mission_offset()
        except (AttributeError, TypeError, ValueError):
            return
        fields: dict[str, Any] = {
            "mission_seq": seq if seq >= 0 else -1,
            "mission_item": self._plan_item_of(seq),
        }
        total = getattr(msg, "total", None)
        if isinstance(total, int) and total:
            fields["mission_total"] = (
                0 if total == MISSION_TOTAL_NONE
                else max(0, total - self._mission_offset()))
        state = MISSION_STATE_NAMES.get(int(getattr(msg, "mission_state", 0) or 0))
        if state:
            fields["mission_state"] = state
        # Somebody else may have put a mission on the vehicle: QGroundControl,
        # a companion, a second Corvus. Then what Corvus thought was there is
        # not, and no leg of its plan is claimed to be flown. Two signals: the
        # mission id PX4 v1.14+ sends (when this pymavlink parses it), and the
        # item count, which every stack sends.
        changed = False
        mission_id = int(getattr(msg, "mission_id", 0) or 0)
        if mission_id:
            if self._vehicle_mission_id_pending or self._vehicle_mission_id is None:
                self._vehicle_mission_id = mission_id
                self._vehicle_mission_id_pending = False
            elif mission_id != self._vehicle_mission_id:
                self._vehicle_mission_id = mission_id
                changed = True
        index = self._vehicle_mission_index
        if index is not None and "mission_total" in fields:
            if fields["mission_total"] == len(index):
                self._vehicle_mission_confirmed = True
            elif (self._vehicle_mission_confirmed
                  or time.monotonic() - self._vehicle_mission_noted_at >= MISSION_CHANGE_GRACE_S):
                changed = True
        if changed and index is not None:
            self._forget_vehicle_mission("the vehicle holds a different mission now")
            fields["mission_item"] = -1
        self._store.update(**fields)

    def _handle_mission_item_reached(self, msg: Any) -> None:
        try:
            seq = int(msg.seq) - self._mission_offset()
        except (AttributeError, TypeError, ValueError):
            return
        if seq >= 0:
            self._store.update(mission_reached=seq,
                               mission_reached_item=self._plan_item_of(seq))

    def _note_vehicle_mission(self, plan_index: list[int | None] | None) -> None:
        """Record what Corvus just put on (or read off) the vehicle."""
        self._vehicle_mission_index = list(plan_index) if plan_index is not None else None
        self._vehicle_mission_id_pending = True
        self._vehicle_mission_confirmed = False
        self._vehicle_mission_noted_at = time.monotonic()
        self._mission_revision += 1
        self._store.update(
            mission_known=plan_index is not None,
            mission_revision=self._mission_revision,
            mission_total=len(plan_index) if plan_index is not None else -1,
            mission_item=-1, mission_reached_item=-1,
        )

    def _forget_vehicle_mission(self, reason: str = "") -> None:
        """Corvus no longer knows what is on the vehicle (a new link, another station)."""
        had = self._vehicle_mission_index is not None
        self._vehicle_mission_index = None
        self._mission_revision += 1
        self._store.update(mission_known=False, mission_revision=self._mission_revision,
                           mission_item=-1, mission_reached_item=-1)
        if had and reason:
            self._console_publish("MISSION", reason.capitalize(), "info")

    def download_mission(self) -> dict[str, Any] | None:
        """Read the mission stored on the vehicle, and read it back as a plan.

        MISSION_REQUEST_LIST, then one MISSION_REQUEST_INT per item, each
        asked again until it arrives or :data:`MISSION_DOWNLOAD_TRIES` is
        spent, then MISSION_ACK. ArduPilot's home slot is left out, so the
        items are numbered as the plan numbers them. A mission longer than the
        planner holds is refused after the count, before any item is asked
        for.

        Holds the command lock like an upload does, but gives it up between
        items if an abort is waiting: the transfer is cancelled on the
        vehicle and the RTL goes first.

        Returns ``{"items", "count"}`` plus :func:`corvus.mission.items_to_plan`
        output, or None with the reason as the last command error.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                self._command_failure("Mission download", -2)
                return None
            download = _MissionDownload()
            with self._mission_lock:
                self._mission_download = download
            try:
                items = self._run_mission_download(download)
            finally:
                with self._mission_lock:
                    if self._mission_download is download:
                        self._mission_download = None
            if items is None:
                return None
            snapshot = self._store.get_snapshot()
            home = None
            value = snapshot.get("home") or []
            try:
                lon, lat = float(value[0]), float(value[1])
                if (lat, lon) != (0.0, 0.0):
                    home = {"lat": lat, "lon": lon}
            except (TypeError, ValueError, IndexError):
                home = None
            converted = mission_plan.items_to_plan(
                items, home=home, home_alt_amsl=self._home_alt_amsl)
            self._note_vehicle_mission(converted["plan_index"])
            self._console_publish(
                "MISSION", f"Read {len(items)} mission item(s) from the vehicle", "success")
            return dict(converted, items=items, count=len(items),
                        revision=self._mission_revision)

    def _mission_wait_s(self) -> float:
        slow = getattr(self, "_is_slow_link", lambda: False)()
        return MISSION_DOWNLOAD_WAIT_S_SLOW if slow else MISSION_DOWNLOAD_WAIT_S

    def _run_mission_download(self, download: _MissionDownload) -> list[dict[str, Any]] | None:
        """The handshake itself. None on failure, with the reason set."""
        target = (self._target_system, self._target_component)

        def ask(send: Any, answered: Any) -> bool | None:
            """Send, wait, repeat. True answered, False lost, None interrupted."""
            for _attempt in range(MISSION_DOWNLOAD_TRIES):
                if self._stop_event.is_set() or not self._connection_ready():
                    return None
                download.event.clear()
                conn = self._conn
                try:
                    with self._send_lock:
                        if conn is None or conn is not self._conn:
                            return None
                        send(conn)
                except Exception as exc:  # noqa: BLE001 - a torn-down link is a failed download
                    logger.debug("mission download send failed: %s", exc)
                    return None
                deadline = time.monotonic() + self._mission_wait_s()
                while time.monotonic() < deadline:
                    if download.event.wait(timeout=0.1) and answered():
                        return True
                    if self._operation_lock.abort_waiting():
                        return None
                if answered():
                    return True
            return False

        got = ask(lambda conn: conn.mav.mission_request_list_send(*target, 0),
                  lambda: download.count is not None)
        if got is None:
            self._cancel_mission_download()
            self._set_command_error("mission download interrupted")
            return None
        if not got:
            self._set_command_error("the vehicle did not answer the mission request")
            return None
        total = int(download.count or 0)
        offset = self._mission_offset() if total else 0
        if total - offset > mission_plan.MISSION_MAX_ITEMS:
            self._cancel_mission_download()
            self._set_command_error(
                f"the vehicle's mission has {total - offset} items, more than the "
                f"{mission_plan.MISSION_MAX_ITEMS} the planner holds")
            return None
        for seq in range(total):
            with self._mission_lock:
                download.waiting_for = seq
            got = ask(lambda conn, seq=seq: conn.mav.mission_request_int_send(*target, seq, 0),
                      lambda seq=seq: seq in download.items)
            if not got:
                self._cancel_mission_download()
                self._set_command_error(
                    "mission download interrupted" if got is None
                    else f"mission item {seq + 1} of {total} never arrived")
                return None
        if total:
            self._send_mission_ack(mavutil.mavlink.MAV_MISSION_ACCEPTED)
        return [download.items[seq] for seq in range(offset, total)]

    def _send_mission_ack(self, result: int) -> None:
        conn = self._conn
        try:
            with self._send_lock:
                if conn is not None and conn is self._conn:
                    conn.mav.mission_ack_send(
                        self._target_system, self._target_component, result, 0)
        except Exception as exc:  # noqa: BLE001 - the vehicle times the transfer out anyway
            logger.debug("mission ack send failed: %s", exc)

    def _cancel_mission_download(self) -> None:
        self._send_mission_ack(MAV_MISSION_OPERATION_CANCELLED)

    def _handle_mission_ack(self, msg: Any) -> None:
        """Resolve the pending upload waiter with the MISSION_ACK result."""
        if not self._ack_is_for_us(msg):
            return
        ack_type = int(getattr(msg, "type", -1))
        text = f"MISSION_ACK: type={ack_type}"
        level = "success" if ack_type == mavutil.mavlink.MAV_MISSION_ACCEPTED else "error"
        self._console_publish("GOTOPOINTS", text, level)
        with self._mission_lock:
            pending = self._pending_mission_ack
            if pending is not None and pending.result is None:
                pending.result = ack_type
                pending.event.set()

    def _upload_mission(self, items: list[dict[str, Any]]) -> int:
        """Upload mission items and wait for MISSION_ACK.

        Returns MAV_MISSION_ACCEPTED on success, the MISSION_ACK type on
        rejection, -1 on timeout, -2 on disconnect/shutdown.
        """
        pending = _PendingAck()
        with self._mission_lock:
            self._mission_items = list(items)
            self._pending_mission_ack = pending
            self._mission_last_request = time.monotonic()
        try:
            conn = self._conn
            try:
                with self._send_lock:
                    if conn is not self._conn:
                        return -2
                    conn.mav.mission_count_send(
                        self._target_system, self._target_component, len(items),
                    )
            except Exception as exc:
                logger.error("mission_count_send failed: %s", exc)
                return -2
            # Bounded by progress, not by item count. PX4 requests the items
            # one at a time and back to back, so "the vehicle has stopped
            # asking" is known within MISSION_UPLOAD_QUIET_S — where a budget
            # of 2 s per item held _operation_lock (and with it arm, land and
            # RTL) for minutes after a handshake had already died.
            deadline = time.monotonic() + min(
                MISSION_UPLOAD_MAX_S, 5.0 + 2.0 * len(items),
            )
            while True:
                if pending.event.wait(timeout=0.2):
                    return pending.result if pending.result is not None else -1
                now = time.monotonic()
                if now > deadline:
                    return -1
                with self._mission_lock:
                    last = self._mission_last_request
                # No item has been asked for in a while and no ACK has landed:
                # the handshake is not in progress, it is over.
                if now - last > MISSION_UPLOAD_QUIET_S:
                    return -1
        finally:
            with self._mission_lock:
                if self._pending_mission_ack is pending:
                    self._pending_mission_ack = None
                self._mission_items = None

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
        if not isinstance(points, list) or not points:
            self._set_command_error("points must be a non-empty list")
            return None
        if len(points) > FLY_TO_MAX_POINTS:
            self._set_command_error(
                f"too many points ({len(points)}); the maximum is {FLY_TO_MAX_POINTS}"
            )
            return None
        cleaned: list[tuple[float, float, float]] = []
        for idx, entry in enumerate(points):
            if not isinstance(entry, dict):
                self._set_command_error(f"point {idx} must be an object")
                return None
            lat = self._fly_to_point_number(entry.get("lat"))
            lon = self._fly_to_point_number(entry.get("lon"))
            alt = self._fly_to_point_number(entry.get("alt_agl"))
            if lat is None:
                self._set_command_error(f"point {idx} lat must be a number")
                return None
            if not math.isfinite(lat) or not -90.0 <= lat <= 90.0:
                self._set_command_error(f"point {idx} lat out of range")
                return None
            if lon is None:
                self._set_command_error(f"point {idx} lon must be a number")
                return None
            if not math.isfinite(lon) or not -180.0 <= lon <= 180.0:
                self._set_command_error(f"point {idx} lon out of range")
                return None
            if alt is None:
                self._set_command_error(f"point {idx} alt_agl must be a number")
                return None
            if not math.isfinite(alt) or not (
                TAKEOFF_ALTITUDE_MIN_M <= alt <= TAKEOFF_ALTITUDE_MAX_M
            ):
                self._set_command_error(
                    f"point {idx} alt_agl must be between "
                    f"{TAKEOFF_ALTITUDE_MIN_M:.0f} and {TAKEOFF_ALTITUDE_MAX_M:.0f} m"
                )
                return None
            cleaned.append((lat, lon, alt))
        return cleaned

    @staticmethod
    def _build_mission_item_spec(
        seq: int, command: int, lat: float, lon: float, alt: float,
        p1: float, p2: float, p3: float, p4: float,
        frame: int | None = None,
    ) -> dict[str, Any]:
        """Build one MISSION_ITEM_INT / MISSION_ITEM send-spec.

        Frame defaults to MAV_FRAME_GLOBAL_RELATIVE_ALT (relative to home =
        AGL), supported by PX4 v1.16-v1.18 for MISSION_ITEM_INT. A planner item
        that names no place passes MISSION_DO_FRAME instead.
        """
        return {
            "frame": (mavutil.mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT
                      if frame is None else int(frame)),
            "command": command,
            "current": 1 if seq == 0 else 0,
            "autocontinue": 1,
            "param1": float(p1), "param2": float(p2),
            "param3": float(p3), "param4": float(p4),
            "x_int": int(round(lat * 1e7)),
            "y_int": int(round(lon * 1e7)),
            "x_f": float(lat), "y_f": float(lon), "z": float(alt),
        }

    def _with_mission_home_slot(
        self, specs: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
        """Prepend the home slot ArduPilot reserves at mission sequence 0.

        PX4 numbers mission items from 0. ArduPilot's AP_Mission keeps *home*
        in slot 0 and the first real item in slot 1, and an upload that does
        not account for that does not fail — the vehicle simply stores the
        first item as a home position and flies a mission with its takeoff
        missing. Which is exactly the sort of difference that only shows up on
        the airfield.

        The placeholder carries the home the vehicle has already told us about
        when it has, and zeroes when it has not; ArduPilot overwrites slot 0
        with its own home either way, so the values are a courtesy rather than
        a command. A zero-item upload (the protocol's "clear") is passed
        through untouched — a clear has no home slot to reserve.
        """
        if not specs or not self._dialect.mission_seq0_is_home:
            return specs
        snapshot = self._store.get_snapshot()
        home = snapshot.get("home") or [0.0, 0.0]
        # The store keeps home as [lon, lat], like every position it holds.
        try:
            home_lon, home_lat = float(home[0]), float(home[1])
        except (TypeError, ValueError, IndexError):
            home_lat = home_lon = 0.0
        home_alt = self._home_alt_amsl or 0.0
        placeholder = self._build_mission_item_spec(
            0, mavutil.mavlink.MAV_CMD_NAV_WAYPOINT,
            home_lat, home_lon, home_alt, 0.0, 0.0, 0.0, 0.0,
            frame=mavutil.mavlink.MAV_FRAME_GLOBAL,
        )
        out = [placeholder]
        for item in specs:
            shifted = dict(item)
            shifted["current"] = 0
            out.append(shifted)
        return out

    def _mission_start_range(self, count: int) -> tuple[float, float]:
        """The (first, last) item indices MAV_CMD_MISSION_START should carry.

        *count* is how many items the planner produced, before the home slot.
        The pair is the dialect's: PX4 is told both ends, ArduPilot is sent
        (0, 0) because 4.6 refuses any other pair.
        """
        return self._dialect.mission_start_params(count)

    def _start_uploaded_mission(self, action: str, count: int) -> bool:
        """MISSION_START, the mission mode and arming, in this stack's order.

        PX4's MISSION_START switches to the mission and arms by itself; the
        mode change and arm that follow only confirm it. ArduPilot's switches
        to AUTO and nothing more, and a copter will not arm in AUTO, so a
        vehicle still on the ground is put in the dialect's arming mode and
        armed first. That order can leave an aircraft armed for nothing, so a
        MISSION_START refused after arming disarms again, as takeoff does.

        Called with the command lock held; returns False with the reason set.
        """
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
            if not self.arm(True):
                return False
            armed_here = True
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
        if not self._enter_mission_mode(action):
            return False
        return self.arm(True)

    @staticmethod
    def _mission_ack_to_result(ack_type: int) -> int:
        """Map MAV_MISSION_RESULT to MAV_RESULT for "Fly to points failed: ..."."""
        if ack_type == mavutil.mavlink.MAV_MISSION_ACCEPTED:
            return mavutil.mavlink.MAV_RESULT_ACCEPTED
        if ack_type == mavutil.mavlink.MAV_MISSION_DENIED:
            return mavutil.mavlink.MAV_RESULT_DENIED
        # 2 = MAV_MISSION_UNSUPPORTED_FRAME, 3 = MAV_MISSION_UNSUPPORTED
        if ack_type in (2, 3):
            return mavutil.mavlink.MAV_RESULT_UNSUPPORTED
        return mavutil.mavlink.MAV_RESULT_FAILED

    def _takeoff_item_position(
        self, first: tuple[float, float, float],
    ) -> tuple[float, float]:
        """Where a prepended takeoff item is placed: the vehicle, else home, else the first point.

        Never 0/0, which is what it used to carry. PX4's feasibility check
        counts the takeoff item as the first waypoint (``MIS_DIST_1WP``), so a
        takeoff "at" 0/0 is thousands of kilometres from the aircraft, and once
        airborne PX4 treats the item's position as somewhere to go.
        """
        snapshot = self._store.get_snapshot()
        for key in ("position", "home"):
            value = snapshot.get(key) or []
            try:
                lon, lat = float(value[0]), float(value[1])
            except (TypeError, ValueError, IndexError):
                continue
            if (lat, lon) == (0.0, 0.0):
                continue
            if math.isfinite(lat) and math.isfinite(lon) \
                    and -90.0 <= lat <= 90.0 and -180.0 <= lon <= 180.0:
                return lat, lon
        return first[0], first[1]

    def fly_to_points(self, points: list[dict[str, float]]) -> bool:
        """Fly to the given points in order (Punktabflug) by uploading a mission.

        Each point is ``{"lat","lon","alt_agl"}`` with ``alt_agl`` in metres
        above home (same reference as :meth:`takeoff`). Uploads a
        MISSION_ITEM_INT mission (MAV_FRAME_GLOBAL_RELATIVE_ALT), then starts
        and arms it in the order the connected stack needs (see
        :meth:`_start_uploaded_mission`). Returns True only when the mission is
        accepted, started, and the vehicle is armed. Works on PX4 v1.16, v1.17
        and v1.18, and on ArduPilot 4.3 to 4.6.
        """
        with self._operation_lock:
            self._set_command_error("")
            cleaned = self._validate_fly_to_points(points)
            if cleaned is None:
                return False
            if not self._connection_ready():
                return self._command_failure("Fly to points", -2)
            # No altitude-reference gate here, on purpose. There used to be
            # one, and it was pure ceremony: every item below is built in
            # MAV_FRAME_GLOBAL_RELATIVE_ALT, whose z *is* the AGL number the
            # operator typed, so the AMSL value the gate computed was thrown
            # away without ever being sent. It refused missions over a
            # conversion the mission does not need.
            on_ground = not self._is_airborne()
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
            self._console_publish(
                "GOTOPOINTS", f"Uploading {planned} mission item(s) …", "info",
            )
            ack = self._upload_mission(items)
            if ack != mavutil.mavlink.MAV_MISSION_ACCEPTED:
                result = ack if ack < 0 else self._mission_ack_to_result(ack)
                return self._command_failure("Fly to points", result)
            # Not a plan: the Mission page's plan is no longer what is on board.
            self._note_vehicle_mission(None)

            self._console_publish(
                "GOTOPOINTS", "Mission accepted; starting …", "info",
            )
            if not self._start_uploaded_mission("Fly to points", planned):
                return False

            self._console_publish("GOTOPOINTS", "Fly to points started", "success")
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

    def upload_mission_plan(self, items: list[dict[str, Any]]) -> bool:
        """Upload a planned mission. Does not start it and does not arm.

        *items* is :func:`corvus.mission.plan_to_items` output — a flat list of
        ``{"command", "lat", "lon", "alt", "params": [p1..p4]}``. Returns True
        only on MAV_MISSION_ACCEPTED.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not isinstance(items, list) or not items:
                self._set_command_error("mission must carry at least one item")
                return False
            if len(items) > FLY_TO_MAX_POINTS:
                self._set_command_error(
                    f"too many mission items ({len(items)}); "
                    f"the maximum is {FLY_TO_MAX_POINTS}"
                )
                return False
            if not self._connection_ready():
                return self._command_failure("Mission upload", -2)

            specs: list[dict[str, Any]] = []
            for seq, entry in enumerate(items):
                params = list(entry.get("params") or [0.0, 0.0, 0.0, 0.0])
                params += [0.0] * (4 - len(params))
                command = int(entry["command"])
                # A DO_ command carries no position, so it carries no altitude
                # frame either; everything that navigates stays in the
                # relative-alt frame the plan's altitudes are written in.
                frame = (MISSION_DO_FRAME
                         if command == mavutil.mavlink.MAV_CMD_DO_CHANGE_SPEED
                         else None)
                specs.append(self._build_mission_item_spec(
                    seq, command,
                    float(entry.get("lat", 0.0)), float(entry.get("lon", 0.0)),
                    float(entry.get("alt", 0.0)),
                    params[0], params[1], params[2], params[3],
                    frame=frame,
                ))

            self._console_publish(
                "MISSION", f"Uploading {len(specs)} mission item(s) …", "info",
            )
            ack = self._upload_mission(self._with_mission_home_slot(specs))
            if ack != mavutil.mavlink.MAV_MISSION_ACCEPTED:
                result = ack if ack < 0 else self._mission_ack_to_result(ack)
                return self._command_failure("Mission upload", result)
            # Which plan item each uploaded item came from, when the caller
            # lowered a plan (corvus.mission.plan_to_items marks them).
            index = [entry.get("item") for entry in items]
            self._note_vehicle_mission(
                index if all(isinstance(i, int) for i in index) else None)
            self._console_publish("MISSION", "Mission accepted by the vehicle", "success")
            return True

    def start_mission(self, count: int) -> bool:
        """Run the mission already on the vehicle: MISSION_START, AUTO, arm.

        *count* is how many items were uploaded. On PX4 MAV_CMD_MISSION_START
        carries an explicit first/last pair rather than the "0 = last item"
        convention, which is the one part of this that differs across v1.16 to
        v1.18; the order of the steps is the dialect's, see
        :meth:`_start_uploaded_mission`.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not isinstance(count, int) or count <= 0:
                self._set_command_error("mission item count must be positive")
                return False
            if not self._connection_ready():
                return self._command_failure("Mission start", -2)
            if not self._start_uploaded_mission("Mission start", count):
                return False
            self._console_publish("MISSION", "Mission started", "success")
            return True

    def clear_mission(self) -> bool:
        """Wipe the mission stored on the vehicle.

        Uploading a zero-item mission is the protocol's own way of saying this,
        and it goes through the same MISSION_COUNT / MISSION_ACK handshake —
        so a vehicle that refuses reports why, exactly as a real upload does.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure("Mission clear", -2)
            ack = self._upload_mission([])
            if ack != mavutil.mavlink.MAV_MISSION_ACCEPTED:
                result = ack if ack < 0 else self._mission_ack_to_result(ack)
                return self._command_failure("Mission clear", result)
            self._note_vehicle_mission([])
            self._console_publish("MISSION", "Mission cleared", "success")
            return True
