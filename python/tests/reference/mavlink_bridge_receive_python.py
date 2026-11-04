"""The bridge's receive path and command transaction, as Corvus had them in
Python: the receive loop, the duplicate frame filter, the dispatcher and the
COMMAND_LONG / COMMAND_INT waits, with the module helpers they use.

Kept unchanged as the reference tests/test_bridge_receive_parity.py holds the
Rust engine to; not imported by the app.
"""
from __future__ import annotations

import datetime
import logging
import math
import time
from typing import Any

from corvus_link import mavutil

from corvus import autopilot as autopilot_dialect
from corvus.mavlink_bridge import (
    DUPLICATE_FRAME_WINDOW_S,
    GCS_SYSTEM_ID,
    GPS_FIX_MAP,
    MAV_AUTOPILOT_MAP,
    MAV_TYPE_MAP,
    RECV_ERROR_BACKOFF_S,
    RECV_ERROR_LIMIT,
    VEHICLE_REBOOT_BACKSTEP_MS,
    _CAP_PARAM_ENCODE_BYTEWISE,
    _CAP_PARAM_ENCODE_C_CAST,
    _DOWNWARD_ORIENTATION,
    _GPS_SENSOR_BIT,
    _HEARTBEAT_LIMITS,
    _NON_AUTOPILOT_COMPONENTS,
    _NON_VEHICLE_TYPES,
    _PREARM_BIT,
    _RANGEFINDER_STALE_S,
)
from corvus.mavlink_common import MAV_RESULT_TEXT, _PendingAck

logger = logging.getLogger("corvus.mavlink")



def _is_plausible_heartbeat(hb: Any) -> bool:
    """Could this HEARTBEAT have been sent by a real MAVLink node?

    A noisy radio link, a serial port at the wrong baud rate, or two radios on
    one channel produce bytes that now and then pass the 16 bit checksum as a
    HEARTBEAT. Such a frame carries a random system id (241, 231, ...) and
    used to show up as a vehicle of its own. Every real node sends
    ``mavlink_version`` 3 and small enum values; a forged frame passes all
    four checks about once in 16000 times, on top of the checksum. A field
    that is not an int (a synthetic frame) is not judged.
    """
    version = getattr(hb, "mavlink_version", None)
    if isinstance(version, int) and not isinstance(version, bool) and version != 3:
        return False
    for field, limit in _HEARTBEAT_LIMITS:
        value = getattr(hb, field, None)
        if isinstance(value, bool) or not isinstance(value, int):
            continue
        if not 0 <= value < limit:
            return False
    return True


def _is_vehicle_heartbeat(hb: Any) -> bool:
    """Is this HEARTBEAT the flight controller's, or some other node's?

    On a direct link the question never comes up — the only thing heartbeating
    is the autopilot. Behind mavlink-router it decides whether this session
    works at all: the link then also carries QGroundControl's own 1 Hz
    heartbeat, a companion computer's, a gimbal's. Whichever lands first is
    what ``mavutil.wait_heartbeat`` returns, and Corvus latches its command
    target onto it.

    Three questions, because one is not enough. The spec's own answer is the
    ``autopilot`` field: a component that is not an autopilot must announce
    ``MAV_AUTOPILOT_INVALID``. Plenty of them do not — a companion computer
    running MAVSDK announces ``MAV_AUTOPILOT_GENERIC`` — so the MAV_TYPE is
    checked against the peripherals that are never an airframe, and the
    component id against the ones that are never a flight controller even on
    the airframe's own system id. Latching onto any of them gives a session
    that looks connected while every command is addressed to a gimbal.

    A frame that no real node could have sent (see
    :func:`_is_plausible_heartbeat`) is not a vehicle either.
    """
    if not _is_plausible_heartbeat(hb):
        return False
    try:
        component = getattr(hb, "get_srcComponent", lambda: 0)()
        if component and int(component) in _NON_AUTOPILOT_COMPONENTS:
            return False
    except (TypeError, ValueError):
        pass
    try:
        vehicle_type = getattr(hb, "type", None)
        if vehicle_type is not None and int(vehicle_type) in _NON_VEHICLE_TYPES:
            return False
    except (TypeError, ValueError):
        pass
    autopilot = getattr(hb, "autopilot", None)
    if autopilot is None:
        # Nothing to judge by (a synthetic heartbeat, or a dialect without the
        # field). Accept: this filter rejects an identified non-autopilot, it
        # does not demand provenance a frame never carried.
        return True
    try:
        return int(autopilot) != mavutil.mavlink.MAV_AUTOPILOT_INVALID
    except (TypeError, ValueError):
        return True


def _sensor_health(msg, bit: int) -> str:
    """SYS_STATUS -> "ok" / "fault" for one sensor, "" when it is not reported."""
    enabled = getattr(msg, "onboard_control_sensors_enabled", 0) or 0
    present = getattr(msg, "onboard_control_sensors_present", 0) or 0
    if not ((enabled | present) & bit):
        return ""
    health = getattr(msg, "onboard_control_sensors_health", 0) or 0
    return "ok" if health & bit else "fault"


def _rangefinder_distance_m(msg) -> float | None:
    """DISTANCE_SENSOR current_distance (cm) -> metres, None unless usable.

    Only the downward sensor counts, and only inside its own min/max: a
    reading at or beyond max_distance is "nothing in range", not a distance.
    """
    try:
        if int(msg.orientation) != _DOWNWARD_ORIENTATION:
            return None
        current = int(msg.current_distance)
        low = int(getattr(msg, "min_distance", 0) or 0)
        high = int(getattr(msg, "max_distance", 0) or 0)
    except (AttributeError, TypeError, ValueError):
        return None
    if current < low or (high > 0 and current >= high):
        return None
    return round(current / 100.0, 2)


def _gps_altitude_m(msg) -> float | None:
    """GPS_RAW_INT alt (mm AMSL) -> metres, None without a 3D fix.

    The field is meaningless below a 3D fix (receivers send 0 or a stale
    value), and a 0 m reading that is not real would sit next to a true
    altitude in the top bar's card as if it were a measurement.
    """
    try:
        if int(getattr(msg, "fix_type", 0) or 0) < 3:
            return None
        return round(int(msg.alt) / 1000.0, 1)
    except (AttributeError, TypeError, ValueError):
        return None


def _gps_accuracy_m(raw) -> float:
    """GPS_RAW_INT h_acc/v_acc (mm) -> metres, -1 when not reported.

    Both are MAVLink 2 extensions, so a MAVLink 1 frame or a receiver that
    does not fill them carries 0, which is "unknown" and not a perfect fix.
    """
    try:
        value = int(raw or 0)
    except (TypeError, ValueError):
        return -1.0
    return round(value / 1000.0, 2) if value > 0 else -1.0


def _prearm_ok(msg) -> bool | None:
    """SYS_STATUS -> True (ready to arm) / False (refused) / None (not reported)."""
    enabled = getattr(msg, "onboard_control_sensors_enabled", 0) or 0
    present = getattr(msg, "onboard_control_sensors_present", 0) or 0
    if not ((enabled | present) & _PREARM_BIT):
        return None
    health = getattr(msg, "onboard_control_sensors_health", 0) or 0
    return bool(health & _PREARM_BIT)


class ReceivePathMixin:
    """The receive path and command transaction, as the bridge had them."""

    @staticmethod
    def _decode_git_hash(raw: Any) -> str:
        """Decode AUTOPILOT_VERSION.flight_custom_version into a git hash.

        The two stacks fill the same eight bytes with two different things, and
        neither says which:

        **PX4** packs the first 5 bytes of the git SHA-1 into the high bytes of
        a little-endian uint64 (px4_update_git_header.py + mavlink_main.cpp
        send_autopilot_capabilities), so the wire bytes for a stock build are
        ``[0, 0, 0, g4, g3, g2, g1, g0]`` — raw bytes to be hex-encoded.

        **ArduPilot** writes the hash as *text*: the ASCII characters of the
        abbreviated SHA, straight into the field. Hex-encoding those a second
        time turns "4a7b3c9d" into "3461376233633964", which is what the build
        line on the status bar used to show for every ArduPilot vehicle.

        So the format is sniffed rather than assumed: a field that is already
        printable hex digits is the text, everything else is the packed
        integer. Sniffing beats keying off the dialect here because the field is
        self-describing and the stack is not always latched when this runs.

        Returns "" when the field is empty or all-zero. Handles
        list/bytes/bytearray/str inputs (corvus_link yields a list of ints for
        uint8_t[] arrays).
        """
        if isinstance(raw, str):
            raw = raw.encode("latin-1", "replace")
        elif isinstance(raw, (list, tuple)):
            try:
                raw = bytes(int(x) & 0xFF for x in raw)
            except (TypeError, ValueError):
                return ""
        elif isinstance(raw, (bytes, bytearray)):
            raw = bytes(raw)
        else:
            return ""
        trimmed = raw.strip(b"\x00")
        if not trimmed:
            return ""
        # Already text: every byte is a hex digit, and there are enough of them
        # to be a hash rather than a coincidence.
        if len(trimmed) >= 6 and all(c in b"0123456789abcdefABCDEF" for c in trimmed):
            return trimmed.decode("ascii").lower()
        if raw[:1] == b"\x00":
            raw = raw[::-1]
        return raw.rstrip(b"\x00").hex()

    def _receive_loop(self) -> None:
        # A read that fails does not necessarily fail slowly. Unplug the USB
        # cable and pyserial raises on every call the instant it is made, so
        # the loop below used to spin at full CPU until the heartbeat drop
        # timeout finally ended the cycle — fifteen seconds of a pegged core on
        # a battery-powered field laptop, at the exact moment the operator has
        # just lost the aircraft. Consecutive failures are counted: each one
        # costs a short sleep, and enough of them in a row end the cycle so the
        # reconnect path (with its proper backoff) takes over instead.
        consecutive_errors = 0
        while self._running.is_set() and self._conn:
            # Snapshot the connection (A6): a concurrent stop()/reconnect can
            # close self._conn under us; use the local ref for recv_match.
            conn = self._conn
            try:
                if self._store.is_stale(timeout=self._warn_timeout()):
                    if self._store.is_stale(timeout=self._drop_timeout()):
                        raise ConnectionError("heartbeat timeout")
                    self._mark_degraded_once()
                msg = conn.recv_match(blocking=True, timeout=1)
                consecutive_errors = 0
                self._cleanup_statustext_chunks()
                if msg is None:
                    continue
                # Connection swapped under us during recv: drop this message.
                if conn is not self._conn:
                    continue
                self._process_frame(msg)
                if self._store.is_stale(timeout=self._warn_timeout()):
                    if self._store.is_stale(timeout=self._drop_timeout()):
                        raise ConnectionError("heartbeat timeout")
                    self._mark_degraded_once()
            except ConnectionError:
                raise
            except Exception as exc:
                consecutive_errors += 1
                logger.debug("recv error (%d in a row): %s", consecutive_errors, exc)
                if consecutive_errors >= RECV_ERROR_LIMIT:
                    raise ConnectionError(f"link read failed: {exc}") from exc
                self._interruptible_sleep(RECV_ERROR_BACKOFF_S)

    def _process_frame(self, msg: Any) -> None:
        """Log, count and dispatch one received frame. Never raises.

        A handler that fails on a message is a bug in that handler, not a read
        error. Counted as one, it cost the loop a backoff sleep per frame: one
        handler failing on a 50 Hz message held the loop to 20 frames a
        second, the socket backed up, and the heartbeats queued behind it
        dropped the link. Logged once per message type instead.
        """
        try:
            if self._is_duplicate_frame(msg):
                return
            # Capture the raw frame before dispatch; a log write never
            # blocks the recv loop (TlogWriter.enqueue is non-blocking) and
            # never breaks the link (guarded).
            self._write_tlog(msg)
            self.message_scope.record(msg)
            self._dispatch(msg)
        except Exception:  # noqa: BLE001 - one bad frame never costs the link
            try:
                name = str(msg.get_type())
            except Exception:  # noqa: BLE001
                name = "?"
            if name not in self._dispatch_failed:
                self._dispatch_failed.add(name)
                logger.exception("handling %s failed; further failures are not logged", name)

    def _is_duplicate_frame(self, msg: Any) -> bool:
        """Has this exact frame already arrived in the last few hundred ms?

        A copy is dropped before anything reads it. When a link loops, an old
        HEARTBEAT decoded again flips the mode and armed state back to what
        they were, and an old COMMAND_ACK confirms a command the vehicle never
        acknowledged. It also stops the copy being mirrored again, which is
        what keeps a loop going.
        """
        get_buf = getattr(msg, "get_msgbuf", None)
        if get_buf is None:
            return False
        try:
            raw = bytes(get_buf())
        except Exception:  # noqa: BLE001 - an odd message is processed, not dropped
            return False
        if not raw:
            return False
        now = time.monotonic()
        recent = self._recent_frames
        if now - self._recent_frames_swept > DUPLICATE_FRAME_WINDOW_S:
            self._recent_frames = recent = {
                frame: seen for frame, seen in recent.items()
                if now - seen <= DUPLICATE_FRAME_WINDOW_S
            }
            self._recent_frames_swept = now
        seen = recent.get(raw)
        recent[raw] = now
        if seen is not None and now - seen <= DUPLICATE_FRAME_WINDOW_S:
            self._duplicate_frames += 1
            return True
        return False

    def _note_boot_time(self, boot_ms: int) -> None:
        """Notice a vehicle that rebooted while the link stayed up.

        A reboot that is quicker than the heartbeat drop timeout (SITL always,
        a board after a calibration or a reboot command often) never ends the
        connection, so nothing used to forget what the old boot said: the
        parameter cache, a download marked complete, the declared parameter
        encoding. A parameter that only exists after the reboot (a driver just
        enabled) then read as absent until the operator reconnected by hand.
        The stream rates go with a reboot too, since the autopilot starts from
        its defaults, so they are asked for again.
        """
        last = self._last_boot_ms
        self._last_boot_ms = boot_ms
        if last is None or boot_ms + VEHICLE_REBOOT_BACKSTEP_MS >= last:
            return
        text = "The vehicle rebooted. Parameters are read again when next needed"
        logger.info("%s (uptime %d ms after %d ms)", text, boot_ms, last)
        self._console_publish("LINK", text, "info")
        self._reset_parameter_cache()
        self._reset_param_metadata()
        self._param_encoding_declared = ""
        self._request_sent = False
        if self._is_serial():
            self._request_streams()
        self._request_version()
        self._request_home()
        self._schedule_message_intervals()

    def _mark_degraded_once(self) -> None:
        """Transition to degraded link quality once per connection cycle (A3).

        Guarded so the receive loop does not spam the store every iteration
        while heartbeats are late but not yet drop-timeout-lost.
        """
        if self._degraded_warned:
            return
        self._degraded_warned = True
        self._store.update(link_status="degraded", link_quality="poor")
        logger.warning("Link degraded: heartbeat late (>%.0fs)", self._warn_timeout())

    def _observe_fleet(self, msg: Any, name: str) -> None:
        """Hand a frame of the vehicle this link flies to the vehicle list.

        Frames of any other system are not the list's: one link is one
        vehicle. Never raises, never blocks.
        """
        try:
            source = int(getattr(msg, "get_srcSystem", lambda: 0)() or 0)
            heartbeat = name == "HEARTBEAT"
            if source != self._target_system:
                if heartbeat and source:
                    self._note_ignored_system(msg, source)
                return
            self.fleet_tracker.observe(
                msg, vehicle_heartbeat=heartbeat and _is_vehicle_heartbeat(msg),
            )
        except Exception:  # noqa: BLE001 - the vehicle list never costs the link
            logger.debug("fleet tracker failed", exc_info=True)

    def _note_ignored_system(self, hb: Any, system: int) -> None:
        """Tell the console, once per system, that another autopilot is ignored."""
        if system in self._ignored_systems or len(self._ignored_systems) >= 16:
            return
        if system == GCS_SYSTEM_ID or not _is_vehicle_heartbeat(hb):
            return
        self._ignored_systems.add(system)
        logger.info("ignoring autopilot sys=%d: this link flies sys=%d",
                    system, self._target_system)
        self._console_publish(
            "LINK",
            f"Another autopilot (system {system}) is on this link and is ignored. "
            f"This link flies system {self._target_system}; a second vehicle "
            "needs a radio and a link of its own.",
            "warning",
        )

    def _dispatch(self, msg: Any) -> None:
        name = msg.get_type()
        self._observe_fleet(msg, name)

        if name == "COMMAND_ACK" and self._is_from_vehicle(msg):
            # The console is this aircraft's console: through a router it would
            # otherwise fill with acknowledgements for commands another station
            # sent to another vehicle.
            result_text = MAV_RESULT_TEXT.get(msg.result, f"result={msg.result}")
            formatted = f"COMMAND_ACK: cmd={msg.command} {result_text}"
            level = "success" if msg.result == 0 else ("info" if msg.result == 5 else "error")
            self._console_publish(name, formatted, level)
            if msg.command == mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE:
                self._handle_autotune_ack(msg)
            if self._ack_is_for_us(msg):
                with self._ack_lock:
                    pending = self._pending_acks.get(msg.command)
                    if (pending is not None
                            and msg.result == mavutil.mavlink.MAV_RESULT_IN_PROGRESS
                            and not pending.accept_in_progress):
                        # Heard and being worked on: the final word follows.
                        pending.in_progress = True
                    elif pending is not None and msg.result in pending.interim:
                        pending.interim_result = msg.result
                    elif pending and (
                        msg.result != mavutil.mavlink.MAV_RESULT_IN_PROGRESS
                        or pending.accept_in_progress
                    ):
                        pending.result = msg.result
                        pending.event.set()

        if name in ("MISSION_REQUEST_INT", "MISSION_REQUEST"):
            self._handle_mission_request(msg, as_int=(name == "MISSION_REQUEST_INT"))
        elif name == "MISSION_ACK":
            self._handle_mission_ack(msg)
        elif name == "MISSION_COUNT":
            self._handle_mission_count(msg)
        elif name in ("MISSION_ITEM_INT", "MISSION_ITEM"):
            self._handle_mission_item(msg, as_int=(name == "MISSION_ITEM_INT"))

        # NSH debug-shell reply (device=SHELL). Routed as a standalone branch
        # so a chatty shell never starves the telemetry elif chain; the
        # handler never blocks the receive loop.
        if name == "SERIAL_CONTROL":
            self._handle_serial_control(msg)

        # The SiK radio speaks for itself, not for the aircraft: RADIO_STATUS
        # carries the radio's own system id (ord('3')/ord('D')), so it is
        # served before the from-the-vehicle guard below would reject it.
        if name == "RADIO_STATUS":
            self._handle_radio_status(msg)
            return

        # Not in the dialect corvus_link loads, so it arrives without a source id
        # and checks the aircraft's own itself. See _handle_gnss_integrity.
        if name in ("UNKNOWN_441", "GNSS_INTEGRITY"):
            self._handle_gnss_integrity(msg)
            return

        # Everything past here writes the aircraft's own state — position,
        # mode, battery, parameters, the armed flag. On a link that carries
        # more than one node (mavlink-router, or a second station sharing the
        # vehicle) the receive loop also sees a ground station's heartbeats, a
        # companion computer's STATUSTEXT and a second aircraft's telemetry.
        # Taking those as this vehicle's means an ARMED indicator that blinks
        # off once a second because QGroundControl said so, and — worse — a
        # lost aircraft that still looks connected, because somebody else's
        # heartbeat kept feeding the staleness timer that drives the reconnect.
        if not self._is_from_vehicle(msg):
            return

        # The Remote ID verdict. Guarded by the vehicle check above but not by
        # the autopilot one: a serial or DroneCAN Remote ID module is a
        # component of its own (MAV_COMP_ID_ODID_TXRX_*) on the same airframe,
        # and it is the part of the system that actually knows whether the
        # identity it is being sent is good enough to arm on.
        if name == "OPEN_DRONE_ID_ARM_STATUS":
            self._handle_remote_id_arm_status(msg)
            return

        if name == "HEARTBEAT":
            # Tighter than the guard above: every component on the airframe
            # heartbeats under the vehicle's system id with its own type,
            # autopilot and base_mode, so a gimbal would otherwise overwrite
            # the vehicle type, the flight mode and the armed flag.
            if not self._is_from_autopilot(msg):
                return
            # One clock read per heartbeat, shared by the staleness timer and
            # the jitter window, so the two cannot disagree about when this
            # heartbeat landed (A1).
            self._note_heartbeat(self._store.heartbeat())
            self._publish_heartbeat_jitter()
            if (self._rangefinder_seen is not None
                    and time.monotonic() - self._rangefinder_seen > _RANGEFINDER_STALE_S):
                self._rangefinder_seen = None
                self._store.update(rangefinder_distance=None)
            if self._degraded_warned:
                # The heartbeat is back before the drop timeout. Without this
                # the link read "degraded" until the next reconnect, and a
                # second dropout in the same session was never reported.
                self._degraded_warned = False
                self._store.update(link_status="connected", link_error="")
                logger.info("Link recovered: heartbeat back")
            vtype = MAV_TYPE_MAP.get(msg.type, f"TYPE_{msg.type}")
            autopilot = MAV_AUTOPILOT_MAP.get(msg.autopilot, f"AP_{msg.autopilot}")
            # Behind a router the aircraft on the far end can be swapped
            # without the socket closing, so the stack is re-read from every
            # heartbeat rather than trusted from connect time. The mode table
            # goes with it: mode 4 is GUIDED on a copter and ACRO on a plane.
            if self._latch_dialect(msg.autopilot, msg.type) and self._conn is not None:
                self._build_mode_mapping()
            armed = bool(msg.base_mode & mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
            mode = self._decode_mode(msg)
            terminated = (
                int(getattr(msg, "system_status", 0))
                == mavutil.mavlink.MAV_STATE_FLIGHT_TERMINATION
            )
            extra: dict[str, Any] = {}
            before = self._store.get_snapshot()
            if terminated and before.get("kill_switch"):
                self._kill_evidence_at = time.monotonic()
            lifted = not terminated and bool(before.get("flight_termination"))
            if lifted:
                # The lockdown the heartbeat reported has lifted. On PX4 that
                # is the kill switch coming back, even if its "Kill disengaged"
                # line was lost on the way.
                extra["kill_switch"] = False
            self._store.update(
                vehicle_type=vtype, autopilot=autopilot, armed=armed, mode=mode,
                mode_label=self._dialect.mode_label(mode),
                autopilot_stack=self._dialect.stack, flight_termination=terminated,
                **extra,
            )
            if lifted:
                self._store.resolve_warnings({"kill"})
            if armed:
                # It armed, so whatever held it back no longer does.
                if self._prearm_reasons:
                    self._clear_prearm_reasons()
                self._store.resolve_warnings({"prearm"})
            self._roll_tlog_on_disarm(armed)
            # A heartbeat recovers a degraded link; recompute quality. When no
            # RADIO_STATUS arrives (UDP SITL), quality is heartbeat-driven.
            self._publish_link_quality()
        elif name == "GLOBAL_POSITION_INT":
            altitude_amsl = msg.alt / 1000.0
            altitude_agl = msg.relative_alt / 1000.0
            self._position_home_alt_amsl = altitude_amsl - altitude_agl
            self._store.update(
                position=[msg.lon / 1e7, msg.lat / 1e7],
                altitude_amsl=round(altitude_amsl, 1),
                altitude_agl=round(altitude_agl, 1),
                heading=msg.hdg / 100.0 if msg.hdg != 65535 else 0,
                # cm/s in the local NED frame. Kept as components, not as the
                # groundspeed magnitude VFR_HUD already carries, because the
                # velocity controller commands components and the tuning page
                # plots each one against what was asked for. Read through
                # getattr for the same reason landed_state is: a truncated or
                # non-standard message must cost a trace, not the position
                # update the HUD and the map are built on.
                vx=round(float(getattr(msg, "vx", 0) or 0) / 100.0, 2),
                vy=round(float(getattr(msg, "vy", 0) or 0) / 100.0, 2),
                vz=round(float(getattr(msg, "vz", 0) or 0) / 100.0, 2),
            )
        elif name == "DISTANCE_SENSOR":
            if int(getattr(msg, "orientation", -1)) == _DOWNWARD_ORIENTATION:
                self._rangefinder_seen = time.monotonic()
                self._store.update(rangefinder_distance=_rangefinder_distance_m(msg))
        elif name == "ATTITUDE":
            self._store.update(
                roll=round(math.degrees(msg.roll), 1),
                pitch=round(math.degrees(msg.pitch), 1),
                yaw=round(math.degrees(msg.yaw), 1),
                rollspeed=round(math.degrees(msg.rollspeed), 1),
                pitchspeed=round(math.degrees(msg.pitchspeed), 1),
                yawspeed=round(math.degrees(msg.yawspeed), 1),
            )
        elif name == "VFR_HUD":
            self._store.update(
                groundspeed=round(msg.groundspeed, 1),
                airspeed=round(msg.airspeed, 1) if msg.airspeed == msg.airspeed else 0,
                heading=msg.heading,
                vspeed=round(msg.climb, 1),
            )
        elif name == "GPS_RAW_INT":
            fix = GPS_FIX_MAP.get(msg.fix_type, f"FIX_{msg.fix_type}")
            hdop = msg.eph / 100.0 if msg.eph != 65535 and msg.eph > 0 else 99.0
            epv = getattr(msg, "epv", 65535)
            vdop = epv / 100.0 if epv != 65535 and epv > 0 else 99.0
            self._store.update(
                gps_fix=fix, gps_satellites=msg.satellites_visible,
                gps_hdop=round(hdop, 1), gps_vdop=round(vdop, 1),
                gps_h_acc=_gps_accuracy_m(getattr(msg, "h_acc", 0)),
                gps_v_acc=_gps_accuracy_m(getattr(msg, "v_acc", 0)),
                gps_altitude=_gps_altitude_m(msg),
            )
        elif name == "SYS_STATUS":
            voltage = msg.voltage_battery / 1000.0 if msg.voltage_battery != 65535 else 0
            current = msg.current_battery / 100.0 if msg.current_battery != -1 else 0
            reported = int(msg.battery_remaining) if msg.battery_remaining != -1 else -1
            prearm = _prearm_ok(msg)
            battery = self._battery_fields(voltage, current, reported)
            known = battery["battery_source"] == "estimate" or reported >= 0
            battery.update(self._battery_endurance_fields(
                battery["battery_percent"] if known else None,
                battery["battery_source"],
                bool(self._store.get_snapshot().get("armed")),
            ))
            self._store.update(
                prearm_ok=prearm,
                gps_health=_sensor_health(msg, _GPS_SENSOR_BIT),
                **battery,
            )
            self._note_prearm_state(prearm)
        elif name == "BATTERY_STATUS":
            self._handle_battery_status(msg)
        elif name == "SYSTEM_TIME":
            # The autopilot's own clock only. A companion computer sends this
            # message too, under the vehicle's system id (MAVROS does it at
            # 1 Hz), and its uptime interleaved with the autopilot's reads as
            # the vehicle rebooting every second, each time dropping the
            # parameter cache and asking for every stream again.
            if not self._is_from_autopilot(msg):
                return
            # utcfromtimestamp is deprecated (and gone in a coming release);
            # more to the point it raises on an out-of-range value, and an
            # autopilot that has no GPS lock yet sends exactly that — a huge
            # or nonsensical time_unix_usec — which used to abort the whole
            # dispatch for this frame, taking the boot_ms read below with it.
            if msg.time_unix_usec:
                try:
                    t = datetime.datetime.fromtimestamp(
                        msg.time_unix_usec / 1e6, datetime.UTC,
                    )
                except (OverflowError, OSError, ValueError, TypeError):
                    logger.debug("SYSTEM_TIME out of range: %s", msg.time_unix_usec)
                else:
                    self._store.update(time=t.strftime("%H:%M:%S UTC"))
            # Autopilot uptime. Published because it is the only honest signal
            # that the vehicle REBOOTED as opposed to the link having dropped:
            # time_boot_ms restarts near zero on boot, so a value that moves
            # backwards means a new flight session. The map uses it to decide
            # when the flown track belongs to a previous flight.
            boot_ms = getattr(msg, "time_boot_ms", None)
            if isinstance(boot_ms, (int, float)) and boot_ms >= 0:
                self._note_boot_time(int(boot_ms))
                self._store.update(boot_ms=int(boot_ms))
        elif name == "STATUSTEXT":
            self._handle_statustext(msg)
        elif name == "AUTOPILOT_VERSION":
            if not self._is_from_autopilot(msg):
                return
            sw = msg.flight_sw_version
            if sw:
                major = (sw >> 24) & 0xFF
                minor = (sw >> 16) & 0xFF
                patch = (sw >> 8) & 0xFF
                detail = self._decode_git_hash(getattr(msg, "flight_custom_version", b""))
                self._store.update(
                    px4_version=f"v{major}.{minor}.{patch}",
                    px4_version_detail=detail,
                )
                version = (major, minor, patch)
                if version != self._firmware_version:
                    # The mode table was built from the heartbeat, before this
                    # arrived, so it holds only what every release accepts.
                    self._firmware_version = version
                    self._build_mode_mapping()
            # Kept on the bridge rather than in the telemetry state: these two
            # identify the board for the firmware picker and are read once, not
            # pushed to the browser on every frame.
            try:
                self._board_vendor_id = int(getattr(msg, "vendor_id", 0) or 0)
                self._board_product_id = int(getattr(msg, "product_id", 0) or 0)
            except (TypeError, ValueError):
                pass
            try:
                capabilities = int(getattr(msg, "capabilities", 0) or 0)
            except (TypeError, ValueError):
                capabilities = 0
            if capabilities & _CAP_PARAM_ENCODE_BYTEWISE:
                self._param_encoding_declared = autopilot_dialect.PARAM_ENCODING_BYTEWISE
            elif capabilities & _CAP_PARAM_ENCODE_C_CAST:
                self._param_encoding_declared = autopilot_dialect.PARAM_ENCODING_C_CAST
        elif name in ("LOG_ENTRY", "LOG_DATA"):
            if not self._is_from_autopilot(msg):
                return
            # Routed to the log service rather than handled here: the download
            # is a stateful protocol, and the receive loop must not own it.
            sink = self._log_sink
            if sink is not None:
                try:
                    sink(msg)
                except Exception:  # noqa: BLE001 - a sink must never kill the loop
                    logger.debug("log sink raised", exc_info=True)
        elif name == "PARAM_VALUE":
            if not self._is_from_autopilot(msg):
                return
            self._handle_param_value(msg)
        elif name == "FILE_TRANSFER_PROTOCOL":
            if self._is_from_autopilot(msg):
                self._handle_ftp_message(msg)
        elif name == "VIBRATION":
            self._store.update(
                vibration_x=round(float(msg.vibration_x), 4),
                vibration_y=round(float(msg.vibration_y), 4),
                vibration_z=round(float(msg.vibration_z), 4),
                clipping_0=int(msg.clipping_0),
                clipping_1=int(msg.clipping_1),
                clipping_2=int(msg.clipping_2),
            )
        elif name == "EXTENDED_SYS_STATE":
            # The autopilot's own on-ground/in-air verdict. Worth having rather
            # than inferring: armed does not mean flying (a vehicle sits armed
            # on the ground before every takeoff), and altitude cannot tell a
            # low hover from a bad home reference.
            try:
                landed = int(getattr(msg, "landed_state", 0) or 0)
            except (TypeError, ValueError):
                landed = 0
            if 0 <= landed <= 4:
                self._store.update(landed_state=landed)
        elif name in ("RC_CHANNELS", "RC_CHANNELS_RAW"):
            self._handle_rc_channels(msg, raw=(name == "RC_CHANNELS_RAW"))
        elif name == "ATTITUDE_TARGET":
            self._handle_attitude_target(msg)
        elif name == "POSITION_TARGET_LOCAL_NED":
            self._handle_position_target(msg)
        elif name in ("MISSION_CURRENT", "MISSION_ITEM_REACHED"):
            if not self._is_from_autopilot(msg):
                return
            if name == "MISSION_CURRENT":
                self._handle_mission_current(msg)
            else:
                self._handle_mission_item_reached(msg)
        elif name == "HOME_POSITION":
            if not self._is_from_autopilot(msg):
                return
            lat = msg.latitude / 1e7
            lon = msg.longitude / 1e7
            self._home_alt_amsl = msg.altitude / 1000.0
            self._store.update(home=[lon, lat])

    def _is_from_vehicle(self, msg: Any) -> bool:
        """Did *msg* come from the aircraft this bridge is flying?

        Source system 0 means the frame carries no sender id — the synthetic
        messages unit tests build, and corvus_link's own BAD_DATA — and is
        accepted: this rejects an identified *other* node, it does not demand
        provenance a frame never had.
        """
        source_system = getattr(msg, "get_srcSystem", lambda: 0)()
        return not source_system or source_system == self._target_system

    def _is_from_autopilot(self, msg: Any) -> bool:
        """Tighter than :meth:`_is_from_vehicle`: the flight controller itself.

        A gimbal, a camera and a companion computer all heartbeat under the
        vehicle's system id, each with its own MAV_TYPE and an empty base_mode.
        Only the autopilot's component speaks for the airframe's mode and arm
        state, so the HEARTBEAT branch asks for that one.
        """
        if not self._is_from_vehicle(msg):
            return False
        source_component = getattr(msg, "get_srcComponent", lambda: 0)()
        return not source_component or source_component == self._target_component

    def _ack_is_for_us(self, msg: Any) -> bool:
        # Snapshot: stop()/reconnect can clear _conn between the guard and the
        # source_system/source_component reads below.
        conn = self._conn
        if conn is None:
            return False
        source_system = getattr(msg, "get_srcSystem", lambda: 0)()
        if source_system and source_system != self._target_system:
            return False
        source_component = getattr(msg, "get_srcComponent", lambda: 0)()
        if source_component and source_component != self._target_component:
            return False
        target_system = getattr(msg, "target_system", 0)
        expected_system = getattr(conn, "source_system", 0)
        if target_system and expected_system and target_system != expected_system:
            return False
        target_component = getattr(msg, "target_component", 0)
        expected_component = getattr(conn, "source_component", 0)
        return not (target_component and expected_component and target_component != expected_component)

    def _cancel_pending_commands(self) -> None:
        with self._ack_lock:
            for pending in self._pending_acks.values():
                pending.result = -2
                pending.event.set()
        # Wake a blocked mission uploader so shutdown cannot hang.
        self._mission_engine.cancel_upload()

    def _connection_ready(self) -> bool:
        return bool(
            self._conn
            and self._store.get_snapshot().get("connected")
            and not self._store.is_stale(timeout=self._heartbeat_timeout())
        )

    def _send_command_and_wait(
        self, command: int, params: list[float] | None = None,
        timeout: float = 3.0, retries: int = 2,
        accept_in_progress: bool = False,
        interim: frozenset[int] = frozenset(),
    ) -> int:
        """Send COMMAND_LONG and wait for COMMAND_ACK. Returns result int (-1 = no ack).

        *interim* names results that may be followed by the real answer (see
        ``cancel_calibration``). They do not end the wait; one is returned only
        when nothing else arrives, and it stops the retries, since the vehicle
        evidently heard the command.
        """
        with self._operation_lock:
            if not self._connection_ready():
                return -2
            # Snapshot the connection (A6): if a concurrent stop()/reconnect
            # swaps self._conn, bail with -2 instead of sending on a closed
            # socket.
            conn = self._conn
            p = list(params or [])[:7]
            p.extend([float("nan")] * (7 - len(p)))
            pending = _PendingAck(accept_in_progress=accept_in_progress, interim=interim)
            with self._ack_lock:
                self._pending_acks[command] = pending

            try:
                for attempt in range(retries + 1):
                    if conn is not self._conn or not self._connection_ready():
                        return -2
                    try:
                        with self._send_lock:
                            if conn is not self._conn:
                                return -2
                            conn.mav.command_long_send(
                                self._target_system, self._target_component, command,
                                0 if attempt == 0 else 1,
                                p[0], p[1], p[2], p[3], p[4], p[5], p[6],
                            )
                    except Exception as exc:
                        logger.error("send command %d failed: %s", command, exc)
                        return -2
                    # Connection swapped during/after the send: don't wait for
                    # an ACK on a torn-down link (A6).
                    if conn is not self._conn:
                        return -2
                    if pending.event.wait(timeout=timeout):
                        return pending.result if pending.result is not None else -1
                    if pending.interim_result is not None:
                        return pending.interim_result
                    if pending.in_progress:
                        return self._await_final_ack(pending, timeout)
                    logger.warning("Command %d ACK timeout (attempt %d)", command, attempt + 1)
                return -1
            finally:
                with self._ack_lock:
                    if self._pending_acks.get(command) is pending:
                        self._pending_acks.pop(command, None)

    def _await_final_ack(self, pending: _PendingAck, timeout: float) -> int:
        """The vehicle said IN_PROGRESS: wait for its verdict, never resend.

        A resend of a command the vehicle is already carrying out can start it
        over (a second calibration, a second takeoff target), which is what the
        retry loop used to do whenever the verdict took longer than one ACK
        timeout. IN_PROGRESS is returned if the verdict does not come.
        """
        if pending.event.wait(timeout=timeout):
            return pending.result if pending.result is not None else -1
        return mavutil.mavlink.MAV_RESULT_IN_PROGRESS

    def _send_command_int_and_wait(
        self, command: int, frame: int,
        params: list[float] | None = None,
        x: int = 0, y: int = 0, z: float = 0.0,
        timeout: float = 3.0, retries: int = 2,
    ) -> int:
        """Send COMMAND_INT and wait for COMMAND_ACK. Returns result int (-1 = no ack).

        The COMMAND_LONG twin above cannot carry a position: its params are
        float32, which holds about seven significant digits — enough for
        48.081 but not for 48.0812345, so a latitude sent that way lands tens
        of metres from where the operator pointed. COMMAND_INT carries x/y as
        int32 degrees x 1e7, which is exact. Everything else — the ACK
        plumbing, the connection-swap guards, the retry loop — is identical,
        because COMMAND_ACK is keyed by command id and does not care which
        wrapper carried it.
        """
        with self._operation_lock:
            if not self._connection_ready():
                return -2
            conn = self._conn
            p = list(params or [])[:4]
            p.extend([float("nan")] * (4 - len(p)))
            pending = _PendingAck()
            with self._ack_lock:
                self._pending_acks[command] = pending

            try:
                for attempt in range(retries + 1):
                    if conn is not self._conn or not self._connection_ready():
                        return -2
                    try:
                        with self._send_lock:
                            if conn is not self._conn:
                                return -2
                            conn.mav.command_int_send(
                                self._target_system, self._target_component,
                                frame, command, 0, 0,
                                p[0], p[1], p[2], p[3],
                                int(x), int(y), float(z),
                            )
                    except Exception as exc:
                        logger.error("send command_int %d failed: %s", command, exc)
                        return -2
                    if conn is not self._conn:
                        return -2
                    if pending.event.wait(timeout=timeout):
                        return pending.result if pending.result is not None else -1
                    if pending.in_progress:
                        return self._await_final_ack(pending, timeout)
                    logger.warning("Command %d ACK timeout (attempt %d)", command, attempt + 1)
                return -1
            finally:
                with self._ack_lock:
                    if self._pending_acks.get(command) is pending:
                        self._pending_acks.pop(command, None)
