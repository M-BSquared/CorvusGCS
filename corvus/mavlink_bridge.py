"""MAVLink bridge — real autopilot communication via pymavlink.

Connects to a PX4 autopilot (SITL or real) over UDP/serial/TCP, parses
incoming messages, updates the Vehicle State Store, and forwards raw
message names to the console subscribers. Runs on a daemon thread.

Sends GCS heartbeats at 1 Hz so the autopilot recognizes us as a ground
control station and allows arming and mode changes.
"""
from __future__ import annotations

import logging
import math
import re
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Callable

from pymavlink import mavutil

from .state_store import VehicleStateStore

logger = logging.getLogger("corvus.mavlink")

MAV_TYPE_MAP: dict[int, str] = {
    0: "GENERIC", 1: "FIXED_WING", 2: "QUADROTOR", 3: "COAXIAL",
    4: "HELICOPTER", 7: "AIRSHIP", 8: "FREE_BALLOON", 9: "ROCKET",
    10: "GROUNDED_ROVER", 11: "SURFACE_BOAT", 12: "SUBMARINE",
    13: "HEXAROTOR", 14: "OCTOROTOR", 15: "TRIROTOR",
    19: "VTOL_DUOROTOR", 20: "VTOL_QUADROTOR", 21: "VTOL_TILTROTOR",
}

MAV_AUTOPILOT_MAP: dict[int, str] = {
    0: "GENERIC", 2: "APM", 3: "PPZ", 4: "ARDUPILOTMEGA",
    12: "PX4", 13: "SMACCMPilot",
}

GPS_FIX_MAP: dict[int, str] = {
    0: "NO_GPS", 1: "NO_FIX", 2: "2D_FIX", 3: "3D_FIX",
    4: "DGPS", 5: "RTK_FLOAT", 6: "RTK_FIXED", 7: "STATIC", 8: "PPP",
}

MAV_RESULT_TEXT: dict[int, str] = {
    -2: "DISCONNECTED", -1: "ACK_TIMEOUT",
    0: "ACCEPTED", 1: "TEMPORARILY_REJECTED", 2: "DENIED",
    3: "UNSUPPORTED", 4: "FAILED", 5: "IN_PROGRESS",
    6: "CANCELLED", 7: "COMMAND_LONG_ONLY", 8: "COMMAND_INT_ONLY",
    9: "UNSUPPORTED_FRAME", 10: "NOT_IN_CONTROL",
}

# PX4 main_mode values (bits 16-23 of custom_mode)
PX4_MAIN_MODE: dict[int, str] = {
    1: "MANUAL", 2: "ALTCTL", 3: "POSCTL", 4: "AUTO",
    5: "ACRO", 6: "OFFBOARD", 7: "STABILIZED", 8: "RATTITUDE",
}

# PX4 AUTO sub_mode values (bits 24-31 of custom_mode when main_mode=4).
# Prefix-less so state.mode matches PX4_AVAILABLE_MODES and the mode selector.
PX4_AUTO_SUBMODE: dict[int, str] = {
    1: "READY", 2: "TAKEOFF", 3: "LOITER",
    4: "MISSION", 5: "RTL", 6: "LAND",
    7: "RTGS", 8: "FOLLOW_TARGET",
    9: "PRECLAND", 10: "VTOL_TAKEOFF",
    11: "EXTERNAL1", 12: "EXTERNAL2", 13: "EXTERNAL3",
    14: "EXTERNAL4", 15: "EXTERNAL5", 16: "EXTERNAL6",
    17: "EXTERNAL7", 18: "EXTERNAL8",
}

PX4_AVAILABLE_MODES: list[str] = [
    "MANUAL", "ALTCTL", "POSCTL", "STABILIZED", "ACRO", "RATTITUDE",
    "LOITER", "MISSION", "RTL", "LAND", "TAKEOFF",
    "OFFBOARD", "FOLLOWME",
]

TAKEOFF_ALTITUDE_MIN_M = 1.0
TAKEOFF_ALTITUDE_MAX_M = 50.0
HEARTBEAT_TIMEOUT_S = 5.0
STATUSTEXT_CHUNK_BYTES = 50
STATUSTEXT_CHUNK_TIMEOUT_S = 10.0
# Linux 8250 platform serial ports (always phantom on field laptops).
_PHANTOM_TTY_RE = re.compile(r"^/dev/ttyS[0-9]+$")


@dataclass
class _PendingAck:
    event: threading.Event = field(default_factory=threading.Event)
    result: int | None = None


@dataclass
class ParamEntry:
    name: str
    value: float
    type: int        # MAV_PARAM_TYPE_* from the PARAM_VALUE message
    index: int
    count: int


class MavlinkBridge:
    """MAVLink connection manager running on a background thread."""

    def __init__(
        self,
        store: VehicleStateStore,
        connection: str = "udp:0.0.0.0:14540",
    ) -> None:
        self._store = store
        self._conn_str = connection
        self._conn: Any = None
        self._thread: threading.Thread | None = None
        self._hb_thread: threading.Thread | None = None
        self._version_retry_thread: threading.Thread | None = None
        self._running = threading.Event()
        self._console_subs: list[Callable[[dict[str, Any]], None]] = []
        self._target_system: int = 1
        self._target_component: int = 1
        self._request_sent = False
        self._mode_mapping: set[str] = set()
        self._mode_values: dict[str, Any] = {}
        self._pending_acks: dict[int, _PendingAck] = {}
        self._ack_lock = threading.Lock()
        self._operation_lock = threading.RLock()
        self._send_lock = threading.Lock()
        self._statustext_chunks: dict[tuple[int, int, int], dict[str, Any]] = {}
        self._home_alt_amsl: float | None = None
        self._position_home_alt_amsl: float | None = None
        self._command_context = threading.local()
        self._reconnect_attempt: int = 1
        self._params: dict[str, ParamEntry] = {}
        self._param_count: int = -1
        self._param_received: int = 0
        self._param_download_state: str = "idle"
        self._param_seen_indices: set[int] = set()
        self._param_lock = threading.Lock()
        self._param_listeners: list[Callable[[dict[str, Any]], None]] = []
        self._param_set_pending: dict[str, _PendingAck] = {}
        # Mission-upload handshake state (set only during fly_to_points). The
        # receive thread reads _mission_items on MISSION_REQUEST_INT and wakes
        # _pending_mission_ack on MISSION_ACK; _mission_lock guards both.
        self._mission_lock = threading.Lock()
        self._mission_items: list[dict[str, Any]] | None = None
        self._pending_mission_ack: _PendingAck | None = None

    def set_connection(self, conn_str: str) -> None:
        self._conn_str = conn_str

    @staticmethod
    def list_serial_ports() -> list[dict[str, str]]:
        """Enumerate available serial ports without opening them.

        Returns ``{"device","description","hwid"}`` dicts sorted by device.
        Falls back to ``/dev`` globbing when pyserial is missing or finds
        nothing. Read-only: never opens a port and never raises.
        """
        ports: list[dict[str, str]] = []
        try:
            import serial.tools.list_ports  # type: ignore[import-not-found]
            for info in serial.tools.list_ports.comports():
                ports.append({
                    "device": str(info.device),
                    "description": str(getattr(info, "description", "") or ""),
                    "hwid": str(getattr(info, "hwid", "") or ""),
                })
        except Exception:
            ports = []
        if not ports:
            try:
                import glob
                devices: list[str] = []
                for pattern in (
                    "/dev/ttyUSB*", "/dev/ttyACM*", "/dev/serial/by-id/*",
                ):
                    devices.extend(glob.glob(pattern))
                ports = [
                    {"device": d, "description": "", "hwid": ""}
                    for d in sorted(set(devices))
                ]
            except Exception:
                ports = []
        # Drop phantom 8250 platform ports so the real radio isn't buried.
        ports = [p for p in ports if not _PHANTOM_TTY_RE.match(p["device"])]
        ports.sort(key=lambda p: p["device"])
        return ports

    def _is_serial(self) -> bool:
        """True when the configured connection is a serial (UART/USB) link."""
        return self._conn_str.startswith("serial:")

    def _parse_serial(self, conn_str: str) -> tuple[str, int]:
        """Strip a ``serial:`` prefix and return ``(device, baud)``.

        pymavlink's ``mavlink_connection`` has no ``serial:`` handler — any
        string containing a colon is routed to mavudp, which rejects a serial
        device. We translate the user-facing ``serial:`` form to the bare
        device + ``baud`` kwarg that mavserial expects. Baud defaults to
        57600 (SiK Telemetry Radio V3 factory default). Never raises: on any
        parse problem the whole remainder is treated as the device path.
        """
        rest = conn_str[len("serial:"):]
        if ":" in rest:
            device, _, baud_str = rest.rpartition(":")
            if baud_str.isdigit():
                return device, int(baud_str)
        # No colon, or the suffix wasn't numeric → whole remainder is device.
        return rest, 57600

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._running.set()
        self._thread = threading.Thread(target=self._run, name="mavlink", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._running.clear()
        self._store.update(link_status="disconnected")
        self._cancel_pending_commands()
        # Wake any pending set_param waiters and mark download idle.
        with self._param_lock:
            self._param_download_state = "idle"
            for pending in self._param_set_pending.values():
                pending.result = -2
                pending.event.set()
            self._param_set_pending.clear()
        if self._conn:
            try:
                self._conn.close()
            except Exception:
                pass
        if self._thread:
            self._thread.join(timeout=3)
        if self._hb_thread:
            self._hb_thread.join(timeout=2)

    def _run(self) -> None:
        while self._running.is_set():
            cycle_exc: Exception | None = None
            try:
                self._connect()
                self._reconnect_attempt = 1
                self._receive_loop()
            except Exception as exc:
                cycle_exc = exc
                logger.error("MAVLink error: %s", exc)
            self._cancel_pending_commands()
            conn = self._conn
            self._conn = None
            if conn:
                try:
                    conn.close()
                except Exception:
                    pass
            self._store.set_disconnected()
            if self._running.is_set():
                delay = self._reconnect_delay(self._reconnect_attempt)
                logger.info(
                    "Reconnecting in %.1fs (attempt %d) …",
                    delay, self._reconnect_attempt,
                )
                if cycle_exc is not None:
                    self._store.update(link_status="reconnecting", link_error=str(cycle_exc))
                else:
                    self._store.update(link_status="reconnecting")
                time.sleep(delay)
                self._reconnect_attempt += 1

    def _reconnect_delay(self, attempt: int) -> float:
        """Backoff between reconnect cycles; serial escalates and caps at 5s."""
        if self._is_serial():
            return min(2.0 * attempt, 5.0)
        return 2.0

    def _connect(self) -> None:
        logger.info("Connecting to %s …", self._conn_str)
        self._request_sent = False
        self._home_alt_amsl = None
        self._position_home_alt_amsl = None
        self._store.update(
            link_status="connecting",
            link_connection=self._conn_str,
            link_error="",
        )
        try:
            if self._is_serial():
                # mavserial takes the bare device path + a baud kwarg; the
                # ``serial:`` prefix is only a user-facing convention here.
                device, baud = self._parse_serial(self._conn_str)
                self._conn = mavutil.mavlink_connection(device, baud=baud)
            else:
                self._conn = mavutil.mavlink_connection(self._conn_str, timeout=2)
        except (OSError, FileNotFoundError) as exc:
            if self._is_serial():
                message = f"serial device unavailable: {self._conn_str}"
                self._store.update(link_error=message)
                raise ConnectionError(message) from exc
            raise
        logger.info("Waiting for heartbeat …")
        hb = self._conn.wait_heartbeat(blocking=True, timeout=10)
        if hb is None:
            raise ConnectionError("No heartbeat received")
        # PX4 autopilot is comp 1; comp 0 = broadcast (not addressable).
        src_sys = getattr(hb, "get_srcSystem", lambda: 0)() or self._conn.target_system
        src_comp = getattr(hb, "get_srcComponent", lambda: 0)() or self._conn.target_component
        self._target_system = src_sys or 1
        self._target_component = src_comp or 1
        vtype = MAV_TYPE_MAP.get(hb.type, f"TYPE_{hb.type}")
        autopilot = MAV_AUTOPILOT_MAP.get(hb.autopilot, f"AP_{hb.autopilot}")
        armed = bool(hb.base_mode & mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
        mode = self._decode_mode(hb)
        logger.info("Heartbeat: %s / %s mode=%s (sys=%d comp=%d)", vtype, autopilot,
                    mode, self._target_system, self._target_component)
        self._store.update(
            connected=True, vehicle_type=vtype, autopilot=autopilot,
            armed=armed, mode=mode, link_status="connected", link_error="",
        )
        self._store.heartbeat()
        self._build_mode_mapping()
        self._request_streams()
        self._request_version()
        self._start_gcs_heartbeat()

    def _decode_mode(self, hb: Any) -> str:
        """Decode the PX4 custom mode layout used by PX4 v1.16-v1.18."""
        try:
            custom = hb.custom_mode
            main_mode = (custom >> 16) & 0xFF
            sub_mode = (custom >> 24) & 0xFF
            if main_mode == 4:
                return PX4_AUTO_SUBMODE.get(sub_mode, f"SUBMODE_{sub_mode}")
            return PX4_MAIN_MODE.get(main_mode, f"MODE_{custom}")
        except Exception:
            return ""

    def _build_mode_mapping(self) -> None:
        try:
            mapping = self._conn.mode_mapping()
            if mapping:
                self._mode_mapping = set(mapping.keys())
                self._mode_values = dict(mapping)
                logger.info("Mode mapping: %s", sorted(mapping.keys()))
                return
        except Exception as exc:
            logger.debug("mode_mapping error: %s", exc)
        self._mode_mapping = set()
        self._mode_values = {}

    def _start_gcs_heartbeat(self) -> None:
        if self._hb_thread and self._hb_thread.is_alive():
            return
        self._hb_thread = threading.Thread(target=self._gcs_hb_loop, name="gcs-hb", daemon=True)
        self._hb_thread.start()

    def _gcs_hb_loop(self) -> None:
        while self._running.is_set():
            if not self._conn:
                time.sleep(1)
                continue
            try:
                with self._send_lock:
                    self._conn.mav.heartbeat_send(
                        mavutil.mavlink.MAV_TYPE_GCS,
                        mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                        0, 0, 0,
                    )
            except Exception:
                pass
            time.sleep(1)

    def _stream_rates(self) -> dict[int, int]:
        """Per-stream REQUEST_DATA_STREAM rates, throttled on serial links.

        A SiK Telemetry Radio V3 tunnels MAVLink over a ~57 kbps serial link,
        so the default 50 Hz attitude/RC streams would saturate it. Keep
        MAV_DATA_STREAM_* constants (honored by PX4 v1.16-v1.18).
        """
        if self._is_serial():
            return {
                mavutil.mavlink.MAV_DATA_STREAM_POSITION: 5,
                mavutil.mavlink.MAV_DATA_STREAM_EXTRA1: 10,
                mavutil.mavlink.MAV_DATA_STREAM_EXTRA2: 5,
                mavutil.mavlink.MAV_DATA_STREAM_EXTRA3: 2,
                mavutil.mavlink.MAV_DATA_STREAM_RAW_SENSORS: 0,
                mavutil.mavlink.MAV_DATA_STREAM_RC_CHANNELS: 0,
            }
        return {
            mavutil.mavlink.MAV_DATA_STREAM_POSITION: 10,
            mavutil.mavlink.MAV_DATA_STREAM_EXTRA1: 50,
            mavutil.mavlink.MAV_DATA_STREAM_EXTRA2: 10,
            mavutil.mavlink.MAV_DATA_STREAM_EXTRA3: 5,
            mavutil.mavlink.MAV_DATA_STREAM_RAW_SENSORS: 5,
            mavutil.mavlink.MAV_DATA_STREAM_RC_CHANNELS: 5,
        }

    def _request_streams(self) -> None:
        if self._request_sent or not self._conn:
            return
        for stream, rate in self._stream_rates().items():
            if rate == 0:
                continue
            with self._send_lock:
                self._conn.mav.request_data_stream_send(
                    self._target_system, self._target_component, stream, rate, 1,
                )
        self._request_sent = True

    def _request_version(self) -> None:
        if self._conn:
            try:
                with self._send_lock:
                    self._conn.mav.autopilot_version_request_send(
                        self._target_system, self._target_component,
                    )
            except Exception:
                pass
        self._schedule_version_retry()

    def _schedule_version_retry(self) -> None:
        """Send one delayed AUTOPILOT_VERSION retry if the first got no reply.

        PX4 v1.18.0-alpha1 SITL ACKs the capabilities command with
        MAV_RESULT_UNSUPPORTED, so AUTOPILOT_VERSION never arrives; a single
        retry ~4s after connect covers slow/lossy responders without looping
        or spamming the link. Daemon thread; checks _running/_conn so it stays
        silent after shutdown and is never joined by stop().
        """
        if not self._running.is_set():
            return
        prior = self._version_retry_thread
        if prior is not None and prior.is_alive():
            return

        def _retry() -> None:
            time.sleep(4.0)
            if not self._running.is_set() or not self._conn:
                return
            if self._store.get_snapshot().get("px4_version"):
                return
            try:
                with self._send_lock:
                    if not self._running.is_set() or not self._conn:
                        return
                    self._conn.mav.autopilot_version_request_send(
                        self._target_system, self._target_component,
                    )
            except Exception:
                pass

        self._version_retry_thread = threading.Thread(
            target=_retry, name="px4-ver-retry", daemon=True,
        )
        self._version_retry_thread.start()

    @staticmethod
    def _decode_git_hash(raw: Any) -> str:
        """Decode AUTOPILOT_VERSION.flight_custom_version into a lowercase hex git hash.

        PX4 packs the first 5 bytes of the git SHA-1 into the high bytes of a
        little-endian uint64 (px4_update_git_header.py + mavlink_main.cpp
        send_autopilot_capabilities), so the wire bytes for a stock build are
        ``[0,0,0, g4, g3, g2, g1, g0]``. Reverse to big-endian, strip the NUL
        padding, and hex-encode. Returns "" when the field is empty or all-zero.
        Handles list/bytes/bytearray/str inputs (pymavlink yields a list of
        ints for uint8_t[] arrays).
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
        if raw[:1] == b"\x00":
            raw = raw[::-1]
        return raw.rstrip(b"\x00").hex()

    def _heartbeat_timeout(self) -> float:
        """Stale-heartbeat threshold; serial tolerates longer radio dropouts."""
        return 10.0 if self._is_serial() else HEARTBEAT_TIMEOUT_S

    def _receive_loop(self) -> None:
        while self._running.is_set() and self._conn:
            try:
                if self._store.is_stale(timeout=self._heartbeat_timeout()):
                    raise ConnectionError("heartbeat timeout")
                msg = self._conn.recv_match(blocking=True, timeout=1)
                self._cleanup_statustext_chunks()
                if msg is None:
                    continue
                self._dispatch(msg)
                if self._store.is_stale(timeout=self._heartbeat_timeout()):
                    raise ConnectionError("heartbeat timeout")
            except ConnectionError:
                raise
            except Exception as exc:
                logger.debug("recv error: %s", exc)

    _CONSOLE_MSGS = frozenset({"COMMAND_ACK"})

    def _dispatch(self, msg: Any) -> None:
        name = msg.get_type()

        if name == "COMMAND_ACK":
            result_text = MAV_RESULT_TEXT.get(msg.result, f"result={msg.result}")
            formatted = f"COMMAND_ACK: cmd={msg.command} {result_text}"
            level = "success" if msg.result == 0 else ("info" if msg.result == 5 else "error")
            self._console_publish(name, formatted, level)
            if self._ack_is_for_us(msg):
                with self._ack_lock:
                    pending = self._pending_acks.get(msg.command)
                    if pending and msg.result != mavutil.mavlink.MAV_RESULT_IN_PROGRESS:
                        pending.result = msg.result
                        pending.event.set()

        if name in ("MISSION_REQUEST_INT", "MISSION_REQUEST"):
            self._handle_mission_request(msg, as_int=(name == "MISSION_REQUEST_INT"))
        elif name == "MISSION_ACK":
            self._handle_mission_ack(msg)

        if name == "HEARTBEAT":
            self._store.heartbeat()
            vtype = MAV_TYPE_MAP.get(msg.type, f"TYPE_{msg.type}")
            autopilot = MAV_AUTOPILOT_MAP.get(msg.autopilot, f"AP_{msg.autopilot}")
            armed = bool(msg.base_mode & mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
            mode = self._decode_mode(msg)
            self._store.update(vehicle_type=vtype, autopilot=autopilot, armed=armed, mode=mode)
        elif name == "GLOBAL_POSITION_INT":
            altitude_amsl = msg.alt / 1000.0
            altitude_agl = msg.relative_alt / 1000.0
            self._position_home_alt_amsl = altitude_amsl - altitude_agl
            self._store.update(
                position=[msg.lon / 1e7, msg.lat / 1e7],
                altitude_amsl=round(altitude_amsl, 1),
                altitude_agl=round(altitude_agl, 1),
                heading=msg.hdg / 100.0 if msg.hdg != 65535 else 0,
            )
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
            self._store.update(gps_fix=fix, gps_satellites=msg.satellites_visible, gps_hdop=round(hdop, 1))
        elif name == "SYS_STATUS":
            voltage = msg.voltage_battery / 1000.0 if msg.voltage_battery != 65535 else 0
            current = msg.current_battery / 100.0 if msg.current_battery != -1 else 0
            self._store.update(
                battery_voltage=round(voltage, 1),
                battery_current=round(current, 1),
                battery_percent=msg.battery_remaining if msg.battery_remaining != -1 else 0,
            )
        elif name == "SYSTEM_TIME":
            if msg.time_unix_usec:
                import datetime
                t = datetime.datetime.utcfromtimestamp(msg.time_unix_usec / 1e6)
                self._store.update(time=t.strftime("%H:%M:%S UTC"))
        elif name == "STATUSTEXT":
            self._handle_statustext(msg)
        elif name == "AUTOPILOT_VERSION":
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
        elif name == "PARAM_VALUE":
            self._handle_param_value(msg)
        elif name == "VIBRATION":
            self._store.update(
                vibration_x=round(float(msg.vibration_x), 4),
                vibration_y=round(float(msg.vibration_y), 4),
                vibration_z=round(float(msg.vibration_z), 4),
                clipping_0=int(msg.clipping_0),
                clipping_1=int(msg.clipping_1),
                clipping_2=int(msg.clipping_2),
            )
        elif name == "HOME_POSITION":
            lat = msg.latitude / 1e7
            lon = msg.longitude / 1e7
            self._home_alt_amsl = msg.altitude / 1000.0
            self._store.update(home=[lon, lat])

    def _ack_is_for_us(self, msg: Any) -> bool:
        if not self._conn:
            return False
        source_system = getattr(msg, "get_srcSystem", lambda: 0)()
        if source_system and source_system != self._target_system:
            return False
        target_system = getattr(msg, "target_system", 0)
        expected_system = getattr(self._conn, "source_system", 0)
        if target_system and expected_system and target_system != expected_system:
            return False
        target_component = getattr(msg, "target_component", 0)
        expected_component = getattr(self._conn, "source_component", 0)
        return not (target_component and expected_component and target_component != expected_component)

    def _handle_mission_request(self, msg: Any, as_int: bool) -> None:
        """Serve one MISSION_ITEM(_INT) during an upload we initiated."""
        if not self._conn or not self._ack_is_for_us(msg):
            return
        seq = int(getattr(msg, "seq", -1))
        with self._mission_lock:
            items = self._mission_items
        if items is None or not (0 <= seq < len(items)):
            return
        item = items[seq]
        try:
            with self._send_lock:
                if as_int:
                    self._conn.mav.mission_item_int_send(
                        self._target_system, self._target_component, seq,
                        item["frame"], item["command"], item["current"],
                        item["autocontinue"], item["param1"], item["param2"],
                        item["param3"], item["param4"], item["x_int"],
                        item["y_int"], item["z"],
                    )
                else:
                    self._conn.mav.mission_item_send(
                        self._target_system, self._target_component, seq,
                        item["frame"], item["command"], item["current"],
                        item["autocontinue"], item["param1"], item["param2"],
                        item["param3"], item["param4"], item["x_f"],
                        item["y_f"], item["z"],
                    )
        except Exception as exc:
            logger.debug("mission item send failed (seq=%d): %s", seq, exc)

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
        try:
            try:
                with self._send_lock:
                    self._conn.mav.mission_count_send(
                        self._target_system, self._target_component, len(items),
                    )
            except Exception as exc:
                logger.error("mission_count_send failed: %s", exc)
                return -2
            # Upload time scales with item count; PX4 requests items one at a time.
            timeout = 5.0 + 2.0 * len(items)
            if not pending.event.wait(timeout=timeout):
                return -1
            return pending.result if pending.result is not None else -1
        finally:
            with self._mission_lock:
                if self._pending_mission_ack is pending:
                    self._pending_mission_ack = None
                self._mission_items = None

    def _cancel_pending_commands(self) -> None:
        with self._ack_lock:
            for pending in self._pending_acks.values():
                pending.result = -2
                pending.event.set()
        # Wake a blocked fly_to_points uploader so shutdown cannot hang.
        with self._mission_lock:
            items = self._mission_items
            self._mission_items = None
            pending = self._pending_mission_ack
            self._pending_mission_ack = None
        if items is not None and pending is not None:
            pending.result = -2
            pending.event.set()

    def _connection_ready(self) -> bool:
        return bool(
            self._conn
            and self._store.get_snapshot().get("connected")
            and not self._store.is_stale(timeout=HEARTBEAT_TIMEOUT_S)
        )
    def _handle_statustext(self, msg: Any) -> None:
        """Reassemble MAVLink2 STATUSTEXT chunks and publish complete messages."""
        now = time.monotonic()
        self._cleanup_statustext_chunks(now)
        raw_value = msg.text
        if isinstance(raw_value, bytes):
            raw_bytes = raw_value
            text = raw_bytes.split(b"\x00", 1)[0].decode("utf-8", errors="replace")
        else:
            raw_text = str(raw_value)
            raw_bytes = raw_text.encode("utf-8", errors="replace")
            text = raw_text.split("\x00", 1)[0]
        severity = int(msg.severity)
        chunk_id = int(getattr(msg, "id", 0))
        chunk_seq = int(getattr(msg, "chunk_seq", 0))
        has_terminator = b"\x00" in raw_bytes
        is_final = has_terminator or len(raw_bytes) < STATUSTEXT_CHUNK_BYTES

        if chunk_id == 0:
            self._publish_statustext(text, severity)
            return

        source_system = getattr(msg, "get_srcSystem", lambda: 0)()
        source_component = getattr(msg, "get_srcComponent", lambda: 0)()
        key = (int(source_system), int(source_component), chunk_id)
        partial = self._statustext_chunks.setdefault(
            key,
            {"parts": {}, "final_seq": None, "severity": severity, "updated": now},
        )
        partial["parts"][chunk_seq] = text
        partial["severity"] = min(int(partial["severity"]), severity)
        partial["updated"] = now
        if is_final:
            partial["final_seq"] = chunk_seq
        final_seq = partial["final_seq"]
        if final_seq is None or not all(seq in partial["parts"] for seq in range(final_seq + 1)):
            return
        complete = "".join(partial["parts"][seq] for seq in range(final_seq + 1))
        complete_severity = int(partial["severity"])
        self._statustext_chunks.pop(key, None)
        self._publish_statustext(complete, complete_severity)

    def _cleanup_statustext_chunks(self, now: float | None = None) -> None:
        current = time.monotonic() if now is None else now
        stale = [
            key for key, partial in self._statustext_chunks.items()
            if current - float(partial["updated"]) > STATUSTEXT_CHUNK_TIMEOUT_S
        ]
        for key in stale:
            self._statustext_chunks.pop(key, None)

    def _publish_statustext(self, text: str, severity: int) -> None:
        level = "critical" if severity <= 3 else ("warning" if severity <= 5 else "info")
        self._console_publish("STATUSTEXT", text, level)
        self._store_update_warning(text, level)

    def _store_update_warning(self, text: str, level: str) -> None:
        """Atomically merge a warning into the vehicle state."""
        self._store.merge_warning(text, level)

    def _console_publish(self, name: str, text: str, level: str) -> None:
        entry = {"ts": time.time(), "name": name, "text": text, "level": level}
        for sub in self._console_subs:
            try:
                sub(entry)
            except Exception:
                pass

    def add_console_sub(self, fn: Callable[[dict[str, Any]], None]) -> None:
        self._console_subs.append(fn)

    def remove_console_sub(self, fn: Callable[[dict[str, Any]], None]) -> None:
        try:
            self._console_subs.remove(fn)
        except ValueError:
            pass

    def get_available_modes(self) -> list[str]:
        if self._mode_mapping:
            return sorted(self._mode_mapping)
        return PX4_AVAILABLE_MODES

    def get_last_command_error(self) -> str:
        """Return the most recent operator-facing command failure."""
        return getattr(self._command_context, "last_error", "")

    def _set_command_error(self, text: str) -> None:
        self._command_context.last_error = text

    def _command_failure(self, action: str, result: int) -> bool:
        text = f"{action} failed: {MAV_RESULT_TEXT.get(result, f'RESULT_{result}')}"
        self._set_command_error(text)
        self._console_publish(action.upper(), text, "error")
        level = "warning" if result in (-2, -1, 1) else "critical"
        self._store_update_warning(text, level)
        return False

    def _send_command_and_wait(
        self, command: int, params: list[float] | None = None,
        timeout: float = 3.0, retries: int = 2,
    ) -> int:
        """Send COMMAND_LONG and wait for COMMAND_ACK. Returns result int (-1 = no ack)."""
        with self._operation_lock:
            if not self._connection_ready():
                return -2
            p = list(params or [])[:7]
            p.extend([float("nan")] * (7 - len(p)))
            pending = _PendingAck()
            with self._ack_lock:
                self._pending_acks[command] = pending

            try:
                for attempt in range(retries + 1):
                    if not self._connection_ready():
                        return -2
                    try:
                        with self._send_lock:
                            self._conn.mav.command_long_send(
                                self._target_system, self._target_component, command,
                                0 if attempt == 0 else 1,
                                p[0], p[1], p[2], p[3], p[4], p[5], p[6],
                            )
                    except Exception as exc:
                        logger.error("send command %d failed: %s", command, exc)
                        return -2

                    if pending.event.wait(timeout=timeout):
                        return pending.result if pending.result is not None else -1
                    logger.warning("Command %d ACK timeout (attempt %d)", command, attempt + 1)
                return -1
            finally:
                with self._ack_lock:
                    if self._pending_acks.get(command) is pending:
                        self._pending_acks.pop(command, None)

    def send_command_long(self, command: int, params: list[float] | None = None, confirmation: int = 0) -> bool:
        """Send command without waiting for ACK (legacy compatibility)."""
        with self._operation_lock:
            if not self._connection_ready():
                return False
            p = list(params or [])[:7]
            p.extend([float("nan")] * (7 - len(p)))
            try:
                with self._send_lock:
                    self._conn.mav.command_long_send(
                        self._target_system, self._target_component, command, confirmation,
                        p[0], p[1], p[2], p[3], p[4], p[5], p[6],
                    )
                return True
            except Exception as exc:
                logger.error("send command failed: %s", exc)
                return False

    def set_mode(self, mode: str) -> bool:
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure("Mode change", -2)
            value = self._mode_values.get(mode)
            if not isinstance(value, tuple) or len(value) < 3:
                logger.error("Unknown or unsupported PX4 mode: %s", mode)
                self._set_command_error(f"Unknown or unsupported PX4 mode: {mode}")
                return False
            base_mode, main_mode, sub_mode = value[:3]
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_DO_SET_MODE,
                [float(base_mode), float(main_mode), float(sub_mode), nan, nan, nan, nan],
                timeout=3.0,
                retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Mode change", result)
            self._console_publish("MODE", f"Mode accepted: {mode}", "success")
            return True

    def _wait_for_armed_state(self, armed: bool, timeout: float = 3.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            snapshot = self._store.get_snapshot()
            if snapshot.get("connected") and bool(snapshot.get("armed")) is armed:
                return True
            time.sleep(0.05)
        return False

    def arm(self, arm: bool = True) -> bool:
        """Arm/disarm with ACK confirmation. Returns True only if PX4 accepted."""
        with self._operation_lock:
            self._set_command_error("")
            if not isinstance(arm, bool):
                self._set_command_error("Arm state must be boolean")
                return False
            if not self._connection_ready():
                return self._command_failure("Arm" if arm else "Disarm", -2)
            if bool(self._store.get_snapshot().get("armed")) is arm:
                return True
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM,
                [1.0 if arm else 0.0, 0.0, nan, nan, nan, nan, nan],
                timeout=3.0, retries=2,
            )
            action = "Arm" if arm else "Disarm"
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure(action, result)
            if not self._wait_for_armed_state(arm):
                text = f"{action} accepted but vehicle state did not change"
                self._set_command_error(text)
                self._console_publish("ARM", text, "error")
                self._store_update_warning(text, "critical")
                return False
            completed = "Armed" if arm else "Disarmed"
            self._console_publish("ARM", f"{completed} successfully", "success")
            return True

    def _takeoff_altitude_amsl(self, altitude_agl: float) -> float | None:
        reference = self._home_alt_amsl
        if reference is None:
            reference = self._position_home_alt_amsl
        return None if reference is None else reference + altitude_agl

    def takeoff(self, altitude_agl: float = 10.0) -> bool:
        """Set a PX4 takeoff target in AGL metres, then arm after acceptance."""
        with self._operation_lock:
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
            alt_amsl = self._takeoff_altitude_amsl(altitude_agl)
            if alt_amsl is None:
                text = "Takeoff failed: no home or global altitude reference"
                self._set_command_error(text)
                self._console_publish("TAKEOFF", text, "error")
                self._store_update_warning(text, "critical")
                return False

            nan = float("nan")
            self._console_publish(
                "TAKEOFF",
                f"Requesting {altitude_agl:.0f} m AGL ({alt_amsl:.1f} m AMSL)",
                "info",
            )
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
                [nan, nan, nan, nan, nan, nan, alt_amsl],
                timeout=5.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Takeoff", result)

            self._console_publish("TAKEOFF", "Takeoff target accepted; arming …", "info")
            if not self.arm(True):
                return False
            self._console_publish("TAKEOFF", "Takeoff accepted and vehicle armed", "success")
            return True

    def land(self) -> bool:
        """Command land at current position with NaN lat/lon and ACK."""
        with self._operation_lock:
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
        """Command return to launch with ACK."""
        with self._operation_lock:
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
    ) -> dict[str, Any]:
        """Build one MISSION_ITEM_INT / MISSION_ITEM send-spec.

        Frame is MAV_FRAME_GLOBAL_RELATIVE_ALT (relative to home = AGL),
        supported by PX4 v1.16-v1.18 for MISSION_ITEM_INT.
        """
        return {
            "frame": mavutil.mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT,
            "command": command,
            "current": 1 if seq == 0 else 0,
            "autocontinue": 1,
            "param1": float(p1), "param2": float(p2),
            "param3": float(p3), "param4": float(p4),
            "x_int": int(round(lat * 1e7)),
            "y_int": int(round(lon * 1e7)),
            "x_f": float(lat), "y_f": float(lon), "z": float(alt),
        }

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

    def fly_to_points(self, points: list[dict[str, float]]) -> bool:
        """Fly to the given points in order (Punktabflug) by uploading a mission.

        Each point is ``{"lat","lon","alt_agl"}`` with ``alt_agl`` in metres
        above home (same reference as :meth:`takeoff`). Uploads a
        MISSION_ITEM_INT mission (MAV_FRAME_GLOBAL_RELATIVE_ALT), starts it
        with MAV_CMD_MISSION_START, switches to AUTO.MISSION, and arms if
        needed. Returns True only when the mission is accepted, started, and
        the vehicle is armed. Works on PX4 v1.16, v1.17, and v1.18.
        """
        with self._operation_lock:
            self._set_command_error("")
            cleaned = self._validate_fly_to_points(points)
            if cleaned is None:
                return False
            if not self._connection_ready():
                return self._command_failure("Fly to points", -2)
            # Reuse the takeoff altitude reference. The relative-alt frame
            # below carries AGL directly, so the AMSL value only validates
            # that a home/global altitude reference exists.
            for _, _, alt_agl in cleaned:
                if self._takeoff_altitude_amsl(alt_agl) is None:
                    text = "Fly to points failed: no home or global altitude reference"
                    self._set_command_error(
                        "no home or global altitude reference"
                    )
                    self._console_publish("GOTOPOINTS", text, "error")
                    self._store_update_warning(text, "critical")
                    return False

            snapshot = self._store.get_snapshot()
            on_ground = float(snapshot.get("altitude_agl", 0.0)) < 0.5
            nan = float("nan")
            items: list[dict[str, Any]] = []
            seq = 0
            if on_ground:
                # Climb to the first waypoint's AGL, then proceed to the points.
                takeoff_agl = cleaned[0][2]
                items.append(self._build_mission_item_spec(
                    seq, mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
                    0.0, 0.0, takeoff_agl, nan, nan, nan, nan,
                ))
                seq += 1
            for lat, lon, alt_agl in cleaned:
                items.append(self._build_mission_item_spec(
                    seq, mavutil.mavlink.MAV_CMD_NAV_WAYPOINT,
                    lat, lon, alt_agl, 0.0, nan, nan, nan,
                ))
                seq += 1

            self._console_publish(
                "GOTOPOINTS", f"Uploading {len(items)} mission item(s) …", "info",
            )
            ack = self._upload_mission(items)
            if ack != mavutil.mavlink.MAV_MISSION_ACCEPTED:
                result = ack if ack < 0 else self._mission_ack_to_result(ack)
                return self._command_failure("Fly to points", result)

            self._console_publish(
                "GOTOPOINTS", "Mission accepted; starting …", "info",
            )
            # param1 = first item index, param2 = last item index (unambiguous
            # across PX4 v1.16-v1.18; avoids the "0 = last item" convention).
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_MISSION_START,
                [0.0, float(len(items) - 1), nan, nan, nan, nan, nan],
                timeout=5.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Fly to points", result)

            # AUTO.MISSION mode + arming starts the uploaded mission from item 0.
            if not self.set_mode("MISSION"):
                return False
            if not self.arm(True):
                return False

            self._console_publish("GOTOPOINTS", "Fly to points started", "success")
            return True

    # ------------------------------------------------------------------
    # Parameter protocol
    # ------------------------------------------------------------------

    def request_param_list(self) -> bool:
        """Start a full parameter download from the vehicle.

        Lazy, operator-triggered — keeps the GCS lean; only GPS/telemetry
        streams until requested. Returns False when disconnected.
        """
        with self._operation_lock:
            if not self._connection_ready():
                return False
            with self._param_lock:
                self._param_download_state = "downloading"
                self._params.clear()
                self._param_received = 0
                self._param_count = -1
                self._param_seen_indices = set()
            with self._send_lock:
                self._conn.mav.param_request_list_send(
                    self._target_system, self._target_component,
                )
            return True

    def request_param(self, name: str) -> bool:
        """Request a single parameter by name (param_index=-1)."""
        with self._operation_lock:
            if not self._connection_ready():
                return False
            with self._send_lock:
                self._conn.mav.param_request_read_send(
                    self._target_system, self._target_component,
                    name.encode(), -1,
                )
            return True

    def _wait_for_param(self, name: str, timeout: float = 2.0) -> ParamEntry | None:
        """Poll the param cache until *name* appears or *timeout* expires."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._param_lock:
                entry = self._params.get(name)
            if entry is not None:
                return entry
            time.sleep(0.02)
        return None

    def set_param(self, name: str, value: float) -> bool:
        """Write a parameter and confirm the echoed PARAM_VALUE matches."""
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            # Defense-in-depth: refuse while armed (PX4 also rejects).
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot set parameter while armed")
                return False
            with self._param_lock:
                entry = self._params.get(name)
            if entry is None:
                # Ad-hoc fetch: request then wait for the value to arrive.
                self.request_param(name)
                entry = self._wait_for_param(name, timeout=2.0)
                if entry is None:
                    self._set_command_error("parameter not found on vehicle")
                    return False
            pending = _PendingAck()
            with self._param_lock:
                self._param_set_pending[name] = pending
            try:
                for attempt in range(3):
                    if not self._connection_ready():
                        return False
                    with self._send_lock:
                        self._conn.mav.param_set_send(
                            self._target_system, self._target_component,
                            name.encode(), float(value), entry.type,
                        )
                    if pending.event.wait(timeout=1.0):
                        break
                else:
                    self._set_command_error("parameter write not confirmed (timeout)")
                    return False
            finally:
                with self._param_lock:
                    if self._param_set_pending.get(name) is pending:
                        self._param_set_pending.pop(name, None)
            with self._param_lock:
                echoed = self._params.get(name)
            if echoed is None:
                self._set_command_error("parameter write not confirmed (no echo)")
                return False
            if abs(echoed.value - float(value)) <= max(1e-4, 1e-3 * abs(float(value))):
                self._console_publish("PARAM", f"{name} set to {value}", "success")
                return True
            text = (f"parameter write not confirmed (got {echoed.value} expected {value})")
            self._set_command_error(text)
            return False

    def get_params(self) -> list[dict[str, Any]]:
        """Return cached parameters as sorted ``{"name","value","type"}`` dicts."""
        with self._param_lock:
            return [
                {"name": e.name, "value": e.value, "type": e.type}
                for _, e in sorted(self._params.items())
            ]

    def get_param(self, name: str) -> dict[str, Any] | None:
        """Return a single cached parameter dict or None."""
        with self._param_lock:
            entry = self._params.get(name)
            if entry is None:
                return None
            return {"name": entry.name, "value": entry.value, "type": entry.type}

    def param_status(self) -> dict[str, Any]:
        """Return ``{"state","count","received"}`` for the download progress."""
        with self._param_lock:
            return {
                "state": self._param_download_state,
                "count": self._param_count,
                "received": self._param_received,
            }

    def add_param_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._param_lock:
            self._param_listeners.append(fn)

    def remove_param_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._param_lock:
            try:
                self._param_listeners.remove(fn)
            except ValueError:
                pass

    def _notify_param_listeners(self, status: dict[str, Any]) -> None:
        with self._param_lock:
            listeners = list(self._param_listeners)
        for fn in listeners:
            try:
                fn(status)
            except Exception:
                pass

    def _handle_param_value(self, msg: Any) -> None:
        """Cache a received PARAM_VALUE and notify listeners."""
        raw_id = msg.param_id
        if isinstance(raw_id, bytes):
            pname = raw_id.split(b"\x00", 1)[0].decode("utf-8", errors="replace")
        else:
            pname = str(raw_id).split("\x00", 1)[0]
        pval = float(msg.param_value)
        ptype = int(msg.param_type)
        pindex = int(msg.param_index)
        pcount = int(msg.param_count)
        with self._param_lock:
            is_new = pname not in self._params
            if is_new:
                self._param_received += 1
            self._param_seen_indices.add(pindex)
            self._params[pname] = ParamEntry(pname, pval, ptype, pindex, pcount)
            if pcount > 0:
                self._param_count = max(self._param_count, pcount)
            if self._param_count > 0 and self._param_received >= self._param_count:
                self._param_download_state = "complete"
            status = {
                "state": self._param_download_state,
                "count": self._param_count,
                "received": self._param_received,
                "name": pname,
                "value": pval,
                "type": ptype,
            }
            # Wake a pending set_param waiter if the echoed name matches.
            pending = self._param_set_pending.get(pname)
            if pending is not None:
                pending.result = 0
                pending.event.set()
        self._notify_param_listeners(status)

    # ------------------------------------------------------------------
    # Sensor calibration
    # ------------------------------------------------------------------

    _CALIBRATION_MAP: dict[str, list[float]] = {
        # MAV_CMD_PREFLIGHT_CALIBRATION (241) — verified against PX4 v1.18
        # Commander.cpp ~line 1430. Unset params are NaN; the selected one 1.0.
        "gyro":        [1.0, float("nan"), float("nan"), float("nan"), float("nan"), float("nan"), float("nan")],
        "compass":     [float("nan"), 1.0, float("nan"), float("nan"), float("nan"), float("nan"), float("nan")],
        "baro":        [float("nan"), float("nan"), 1.0, float("nan"), float("nan"), float("nan"), float("nan")],
        "accel":      [float("nan"), float("nan"), float("nan"), float("nan"), 1.0, float("nan"), float("nan")],
        "level":       [float("nan"), float("nan"), float("nan"), float("nan"), 2.0, float("nan"), float("nan")],
        "accel_quick": [float("nan"), float("nan"), float("nan"), float("nan"), 4.0, float("nan"), float("nan")],
        "airspeed":    [float("nan"), float("nan"), float("nan"), float("nan"), float("nan"), 1.0, float("nan")],
    }

    def calibrate(self, sensor: str) -> bool:
        """Start a PX4 sensor calibration via MAV_CMD_PREFLIGHT_CALIBRATION.

        Calibration is interactive: the ACCEPTED ACK arrives quickly (PX4
        starts a worker task); the operator then follows STATUSTEXT guidance
        (compass rotation, accel positions, etc.) which flows through
        ``_handle_statustext``.
        """
        with self._operation_lock:
            self._set_command_error("")
            params = self._CALIBRATION_MAP.get(sensor)
            if params is None:
                self._set_command_error(f"unknown calibration: {sensor}")
                return False
            # Defense-in-depth: refuse while armed (PX4 also rejects).
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot calibrate while armed")
                return False
            if not self._connection_ready():
                return self._command_failure(f"Calibrate {sensor}", -2)
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION,
                params, timeout=5.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure(f"Calibrate {sensor}", result)
            self._console_publish("CALIBRATE", f"{sensor} calibration started", "success")
            return True

    # ------------------------------------------------------------------
    # Autotune
    # ------------------------------------------------------------------

    _AUTOTUNE_AXIS_MAP: dict[str, float] = {
        # AUTOTUNE_AXIS bitmask — verified against PX4 v1.18 mavlink_receiver.cpp
        # and the AUTOTUNE_AXIS enum (roll=1, pitch=2, yaw=4; 0 = tune all).
        "roll": 1.0,
        "pitch": 2.0,
        "yaw": 4.0,
        "all": 0.0,
    }

    def autotune(self, axis: str) -> bool:
        """Start PX4 autotune via MAV_CMD_DO_AUTOTUNE_ENABLE (212).

        param1=1 (enable), param2=axis bitmask. The ACK (ACCEPTED) arrives
        when PX4 starts the autotune task; tuning progress streams as
        STATUSTEXT. PX4 v1.18 only runs the full tune (param2=0); specific
        axes are DENIED — the operator should use ``"all"``.
        """
        with self._operation_lock:
            self._set_command_error("")
            axis_val = self._AUTOTUNE_AXIS_MAP.get(axis)
            if axis_val is None:
                self._set_command_error(f"unknown autotune axis: {axis}")
                return False
            # Defense-in-depth: refuse while armed.
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot autotune while armed")
                return False
            if not self._connection_ready():
                return self._command_failure(f"Autotune {axis}", -2)
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE,
                [1.0, axis_val, nan, nan, nan, nan, nan],
                timeout=5.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure(f"Autotune {axis}", result)
            self._console_publish("AUTOTUNE", f"Autotune ({axis}) started", "success")
            return True

    # ------------------------------------------------------------------
    # Per-message rate control
    # ------------------------------------------------------------------

    def set_message_interval(self, msg_id: int, interval_us: int) -> bool:
        """Request PX4 to stream a specific MAVLink message at a given interval.

        Standard PX4 per-message rate control (replaces REQUEST_DATA_STREAM);
        works v1.16-v1.18. ``interval_us`` < 0 disables the stream, 0 restores
        the default rate, a positive value sets the interval in microseconds.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                return False
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL,
                [float(msg_id), float(interval_us), nan, nan, nan, nan, nan],
                timeout=3.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Set message interval", result)
            self._console_publish(
                "STREAM",
                f"Message {msg_id} interval set to {interval_us} us", "success",
            )
            return True

    def set_vibration_stream(self, enabled: bool, rate_hz: int = 10) -> bool:
        """Enable/disable high-rate VIBRATION streaming from the vehicle.

        Lean — VIBRATION defaults to 0.1 Hz on PX4; the GCS requests ~10 Hz
        only while the vibration plugin is open, then restores the default.
        Safe to call while armed (vibration data is read-only telemetry).
        """
        with self._operation_lock:
            self._set_command_error("")
            if enabled:
                if not isinstance(rate_hz, int) or not (1 <= rate_hz <= 50):
                    self._set_command_error("vibration rate must be between 1 and 50 Hz")
                    return False
                interval_us = max(1, int(1_000_000 / rate_hz))
            else:
                # 0 = restore PX4 default rate (lean: high-rate only on demand).
                interval_us = 0
            return self.set_message_interval(
                mavutil.mavlink.MAVLINK_MSG_ID_VIBRATION, interval_us,
            )
