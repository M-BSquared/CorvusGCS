"""MAVLink bridge — real autopilot communication via pymavlink.

Connects to a PX4 autopilot (SITL or real) over UDP/serial/TCP, parses
incoming messages, updates the Vehicle State Store, and forwards raw
message names to the console subscribers. Runs on a daemon thread.

Sends GCS heartbeats at 1 Hz so the autopilot recognizes us as a ground
control station and allows arming and mode changes.

This module is the link itself: connect, receive, dispatch, the command
transaction, and the flight commands. The protocols that ride on it live in
their own modules and are mixed into :class:`MavlinkBridge`:

======================================  ====================================
:mod:`corvus.mavlink_params`            parameter download, read, write, verify
:mod:`corvus.mavlink_missions`          fly to points, planned missions
:mod:`corvus.mavlink_setup`             calibration, motor test, autotune
:mod:`corvus.mavlink_shell`             the PX4 NSH shell
:mod:`corvus.mavlink_telemetry`         battery, setpoints, RC, link quality
:mod:`corvus.mavlink_remote_id`         the Remote ID broadcast
======================================  ====================================

Helpers with no bridge state are plain modules: :mod:`corvus.serial_ports`,
:mod:`corvus.mavlink_signing`, :mod:`corvus.priority_lock` and
:mod:`corvus.mavlink_common`. Every name that used to be defined here is still
importable from here.
"""
from __future__ import annotations

import datetime
import errno
import logging
import math
import os
import random
import threading
import time
from typing import Any
from collections.abc import Callable

from pymavlink import mavutil

from . import autopilot as autopilot_dialect
from . import rtk
from .autopilot import (  # noqa: F401 - re-exported, see the tables below
    PX4_AUTO_SUBMODE,
    PX4_AVAILABLE_MODES,
    PX4_MAIN_MODE,
    Dialect,
    ModeCommand,
)
from .mavlink_common import (  # noqa: F401 - re-exported
    MAV_RESULT_TEXT,
    TAKEOFF_ALTITUDE_MAX_M,
    TAKEOFF_ALTITUDE_MIN_M,
    _PendingAck,
)
from .mavlink_missions import (  # noqa: F401 - re-exported
    FLY_TO_MAX_POINTS,
    MISSION_DO_FRAME,
    MISSION_UPLOAD_MAX_S,
    MISSION_UPLOAD_QUIET_S,
    MissionProtocolMixin,
)
from .mavlink_ftp import FtpClientMixin
from .mavlink_params import (  # noqa: F401 - re-exported
    PARAM_CACHE_MAX_ENTRIES,
    PARAM_DOWNLOAD_INACTIVITY_S,
    PARAM_DOWNLOAD_MAX_ROUNDS,
    PARAM_DOWNLOAD_TIMEOUT_S,
    PARAM_DOWNLOAD_TIMEOUT_S_SERIAL,
    PARAM_RETRANSMIT_GAP_S_SERIAL,
    PARAM_RETRANSMIT_GAP_S_UDP,
    PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL,
    PARAM_RETRANSMIT_MAX_PER_ROUND_UDP,
    PARAM_VERIFY_MAX_NAMES,
    PARAM_VERIFY_REWRITES,
    PARAM_WATCHDOG_TICK_S,
    _CAP_PARAM_ENCODE_BYTEWISE,
    _CAP_PARAM_ENCODE_C_CAST,
    ParamEntry,
    ParamProtocolMixin,
)
from .mavlink_remote_id import RemoteIdMixin
from .mavlink_setup import VehicleSetupMixin
from .mavlink_shell import SHELL_LINE_MAX_CHARS, ShellMixin  # noqa: F401 - re-exported
from .mavlink_signing import (  # noqa: F401 - re-exported
    MAVLINK_SIGNING_KEY_FILE_ENV,
    _allow_unsigned_signed_link,
    _load_signing_key,
)
from .mavlink_telemetry import (  # noqa: F401 - re-exported
    TelemetryMixin,
    _quaternion_to_euler_deg,
    _rc_rssi_percent,
)
from .param_metadata import ParamMetadataMixin
from .paths import corvus_path
from .priority_lock import _PriorityLock, _PriorityLockAbort  # noqa: F401 - re-exported
from .serial_ports import (  # noqa: F401 - re-exported
    _DIRECT_USB_ACM_RE,
    _MACOS_SERIAL_RE,
    _PHANTOM_TTY_RE,
    _PIXHAWK_BYID_RE,
    _SIK_RADIO_RE,
    classify_hwid,
    classify_serial_device,
    is_bootloader_port,
    is_phantom_device,
    is_rtk_device,
    is_windows_com_port,
)
from .state_store import VehicleStateStore
from .tlog import TlogWriter

logger = logging.getLogger("corvus.mavlink")


def default_log_dir() -> str:
    """Return the conventional on-disk location for tlog files.

    Mirrors ``tile_cache.default_cache_dir``: the directory is created lazily
    by whoever first writes here (the bridge on connect). Tests monkeypatch
    this to stay hermetic.
    """
    return corvus_path("logs")

MAV_TYPE_MAP: dict[int, str] = {
    0: "GENERIC", 1: "FIXED_WING", 2: "QUADROTOR", 3: "COAXIAL",
    4: "HELICOPTER", 7: "AIRSHIP", 8: "FREE_BALLOON", 9: "ROCKET",
    10: "GROUNDED_ROVER", 11: "SURFACE_BOAT", 12: "SUBMARINE",
    13: "HEXAROTOR", 14: "OCTOROTOR", 15: "TRIROTOR",
    19: "VTOL_DUOROTOR", 20: "VTOL_QUADROTOR", 21: "VTOL_TILTROTOR",
}

# MAV_AUTOPILOT, verbatim from common.xml. Written out rather than read from
# mavutil because this table is also what decides which flight stack Corvus is
# talking to, and a name that drifts from the number would misroute that.
#
# The three middle entries used to be shifted by one — MAV_AUTOPILOT_RESERVED
# (1) was skipped when the table was written, so an ArduPilot vehicle (3) was
# labelled "PPZ" and OpenPilot (4) was labelled "ARDUPILOTMEGA". The value is
# the one field that says which stack is on the other end; getting it wrong
# names the aircraft after somebody else's firmware.
MAV_AUTOPILOT_MAP: dict[int, str] = {
    0: "GENERIC", 1: "RESERVED", 2: "SLUGS", 3: "ARDUPILOTMEGA",
    4: "OPENPILOT", 8: "INVALID", 9: "PPZ", 10: "UDB", 11: "FP",
    12: "PX4", 13: "SMACCMPILOT", 14: "AUTOQUAD", 15: "ARMAZILA",
    16: "AEROB", 17: "ASLUAV", 18: "SMARTAP", 19: "AIRRAILS", 20: "REFLEX",
}

GPS_FIX_MAP: dict[int, str] = {
    0: "NO_GPS", 1: "NO_FIX", 2: "2D_FIX", 3: "3D_FIX",
    4: "DGPS", 5: "RTK_FLOAT", 6: "RTK_FIXED", 7: "STATIC", 8: "PPP",
}

# The flight-stack tables moved to corvus.autopilot when ArduPilot stopped
# being a stack Corvus merely *named* and became one it flies. They are
# re-exported here because this module was their home for a long time and the
# names are load-bearing in tests and in the simulated vehicle.
#
# PX4_MAIN_MODE / PX4_AUTO_SUBMODE decode PX4's packed custom_mode;
# PX4_AVAILABLE_MODES is the selector fallback; PX4_FALLBACK_MODE_VALUES is the
# (base_mode, main_mode, sub_mode) table DO_SET_MODE wants.
PX4_FALLBACK_MODE_VALUES: dict[str, tuple[int, int, int]] = dict(
    autopilot_dialect.PX4_MODE_VALUES
)

HEARTBEAT_TIMEOUT_S = 5.0
STATUSTEXT_CHUNK_BYTES = 50
STATUSTEXT_CHUNK_TIMEOUT_S = 10.0
# Partial multi-chunk STATUSTEXTs held at once. They age out after
# STATUSTEXT_CHUNK_TIMEOUT_S, but nothing bounded how many could accumulate
# *inside* that window: one entry per (system, component, id), and a noisy
# router carries plenty of all three. A cap keeps a link that is misbehaving
# from growing the table without limit on the receive thread.
STATUSTEXT_MAX_PARTIALS = 64

# Two-tier heartbeat staleness (A3). WARN marks the link degraded (socket
# stays open, parsing continues); DROP tears it down and reconnects. Serial
# tolerates longer SiK-radio dropouts than UDP/SITL.
HEARTBEAT_WARN_TIMEOUT_SERIAL = 6.0
HEARTBEAT_WARN_TIMEOUT_UDP = 3.0
HEARTBEAT_DROP_TIMEOUT_SERIAL = 15.0
HEARTBEAT_DROP_TIMEOUT_UDP = 8.0

# Consecutive-read-failure handling in the receive loop. A failing read can
# return instantly (an unplugged USB serial port does), so each failure costs a
# short pause, and this many in a row ends the connection cycle and hands over
# to the reconnect backoff rather than spinning until the heartbeat times out.
RECV_ERROR_BACKOFF_S = 0.05
RECV_ERROR_LIMIT = 40

# Exponential reconnect backoff (A4): base*2^(n-1), capped, +/- jitter.
RECONNECT_BASE_S = 0.5
RECONNECT_CAP_S = 8.0
RECONNECT_JITTER = 0.25
RECONNECT_MIN_S = 0.1

# A distinct GCS system id keeps Corvus COMMAND_ACK and SERIAL_CONTROL replies
# separate from QGroundControl (which normally uses system 255) when both share
# a vehicle through mavlink-router or the built-in raw-frame forwarder.
GCS_SYSTEM_ID = 254
GCS_COMPONENT_ID = mavutil.mavlink.MAV_COMP_ID_MISSIONPLANNER

# How long _connect waits for the aircraft to introduce itself.
HEARTBEAT_WAIT_S = 10.0

# How far SYSTEM_TIME.time_boot_ms has to step backwards to mean the vehicle
# rebooted rather than that two UDP frames arrived out of order.
VEHICLE_REBOOT_BACKSTEP_MS = 5000

# The autopilot sends one preflight report's lines back to back; a line after
# a gap this long starts the next report rather than adding to the last one.
PREARM_REPORT_GAP_S = 2.0
# How often a vehicle that says "not ready" without saying why is asked to
# report again (MAV_CMD_RUN_PREARM_CHECKS). PX4 reports on its own only when
# a result changes, so a station that connects afterwards hears nothing.
PREARM_REPORT_REQUEST_EVERY_S = 30.0
# Reasons kept for one report. PX4 has ~70 checks; a report this long is a
# bench aircraft with nothing connected, and the first lines are the point.
PREARM_REASONS_MAX = 20

# Consecutive unanswered SET_MESSAGE_INTERVAL requests that end the connect-time
# batch. Each one costs two ACK timeouts and holds _operation_lock for both, so
# a link that answers none of them used to park every operator command behind
# the whole list.
INTERVAL_REQUEST_SILENT_LIMIT = 3


# MAV_TYPEs that are peripherals rather than aircraft. A node announcing one
# of these is never the flight controller however it fills in `autopilot` —
# and some do fill it in: a companion computer running MAVSDK reports
# MAV_AUTOPILOT_GENERIC, not INVALID, which is exactly the case the autopilot
# field alone lets through. Kept in step with pymavlink's own
# `probably_vehicle_heartbeat`, plus the peripherals PX4 and ArduPilot
# airframes actually carry.
_NON_VEHICLE_TYPES = frozenset({
    mavutil.mavlink.MAV_TYPE_GCS,
    mavutil.mavlink.MAV_TYPE_GIMBAL,
    mavutil.mavlink.MAV_TYPE_ADSB,
    mavutil.mavlink.MAV_TYPE_ONBOARD_CONTROLLER,
    mavutil.mavlink.MAV_TYPE_CAMERA,
    mavutil.mavlink.MAV_TYPE_SERVO,
    mavutil.mavlink.MAV_TYPE_BATTERY,
    mavutil.mavlink.MAV_TYPE_PARACHUTE,
    mavutil.mavlink.MAV_TYPE_LOG,
    mavutil.mavlink.MAV_TYPE_OSD,
    mavutil.mavlink.MAV_TYPE_IMU,
    mavutil.mavlink.MAV_TYPE_GPS,
    mavutil.mavlink.MAV_TYPE_WINCH,
})

# Component ids that are never the autopilot even under the vehicle's own
# system id. The gimbal is the one pymavlink singles out; the SiK radio and a
# UDP bridge matter here because both sit on the wire between Corvus and the
# aircraft and both heartbeat.
_NON_AUTOPILOT_COMPONENTS = frozenset({
    mavutil.mavlink.MAV_COMP_ID_GIMBAL,
    mavutil.mavlink.MAV_COMP_ID_ONBOARD_COMPUTER,
    mavutil.mavlink.MAV_COMP_ID_TELEMETRY_RADIO,
    mavutil.mavlink.MAV_COMP_ID_UDP_BRIDGE,
})


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
    """
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


# Flight readiness, straight from the autopilot. PX4 mirrors its preflight
# checks into the MAV_SYS_STATUS_PREARM_CHECK bit of SYS_STATUS: present/enabled
# say the firmware publishes the check at all, health says whether an arm
# command would be accepted right now. Firmware that never sets the bit (some
# ArduPilot builds, older PX4) yields None — "unknown", never a green light the
# vehicle did not give.
_PREARM_BIT = mavutil.mavlink.MAV_SYS_STATUS_PREARM_CHECK


_GPS_SENSOR_BIT = mavutil.mavlink.MAV_SYS_STATUS_SENSOR_GPS


def _sensor_health(msg, bit: int) -> str:
    """SYS_STATUS -> "ok" / "fault" for one sensor, "" when it is not reported."""
    enabled = getattr(msg, "onboard_control_sensors_enabled", 0) or 0
    present = getattr(msg, "onboard_control_sensors_present", 0) or 0
    if not ((enabled | present) & bit):
        return ""
    health = getattr(msg, "onboard_control_sensors_health", 0) or 0
    return "ok" if health & bit else "fault"


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


class MavlinkBridge(
    ParamProtocolMixin,
    ParamMetadataMixin,
    FtpClientMixin,
    MissionProtocolMixin,
    VehicleSetupMixin,
    ShellMixin,
    TelemetryMixin,
    RemoteIdMixin,
):
    """MAVLink connection manager running on a background thread."""

    def __init__(
        self,
        store: VehicleStateStore,
        connection: str = "udp:0.0.0.0:14550",
    ) -> None:
        self._store = store
        self._conn_str = connection
        self._conn: Any = None
        self._thread: threading.Thread | None = None
        self._hb_thread: threading.Thread | None = None
        self._version_retry_thread: threading.Thread | None = None
        # Deferred MAV_CMD_SET_MESSAGE_INTERVAL worker (BUG 1): the ACK-confirmed
        # interval requests must run AFTER the receive loop exists, so they are
        # scheduled here and joined in stop().
        self._intervals_thread: threading.Thread | None = None
        # Raw-frame tlog: one file per connect cycle, owned by the bridge so a
        # reconnect closes the old file and starts a fresh one. None = inactive.
        self._tlog: TlogWriter | None = None
        self._running = threading.Event()
        # Complement of _running: set by stop() so daemon workers can sleep via
        # _interruptible_sleep and wake immediately on shutdown (BUG 1, 12).
        self._stop_event = threading.Event()
        # RTCM injection (corvus/rtk_service.py). The sequence number is
        # per-link rather than per-source, because it is what a receiver uses
        # to tell one correction's fragments from the next one's; two sources
        # numbering independently down the same link would interleave into
        # nonsense. Guarded by its own lock so a 1 Hz correction stream never
        # waits behind a parameter download.
        self._rtcm_lock = threading.Lock()
        self._rtcm_sequence = 0
        self._rtcm_bytes = 0
        self._rtcm_messages = 0
        self._rtcm_dropped = 0
        self._rtcm_last_send = 0.0
        self._console_subs: list[Callable[[dict[str, Any]], None]] = []
        # Guards _console_subs: appended/removed by HTTP handler threads,
        # iterated by the receive thread on every STATUSTEXT.
        self._console_lock = threading.Lock()
        self._target_system: int = 1
        self._target_component: int = 1
        self._request_sent = False
        self._mode_mapping: set[str] = set()
        self._mode_values: dict[str, ModeCommand] = {}
        # Set when the autopilot reports modes in an encoding this session
        # cannot send. Since the dialect layer landed that is only ever true
        # for a stack Corvus does not recognise at all — PX4's packed words and
        # ArduPilot's flat custom_mode are both sendable now.
        self._modes_unsupported = False
        # Which flight stack is on the other end, and the MAV_TYPE its mode
        # table is keyed by. Both are latched from the heartbeat at connect and
        # refreshed from every heartbeat after it — nothing is sent before that
        # happens. PX4 is the default only because it is the primary target and
        # a bridge that has never seen a vehicle has to behave like *something*;
        # the moment a real heartbeat lands, the vehicle's own answer wins.
        self._dialect: Dialect = autopilot_dialect.dialect_for_stack(
            autopilot_dialect.STACK_PX4
        )
        self._mav_type_id = 0
        self._pending_acks: dict[int, _PendingAck] = {}
        self._ack_lock = threading.Lock()
        # Two-tier: routine commands queue, abort commands overtake the queue.
        # See _PriorityLock for why one lock could not stay one tier.
        self._operation_lock = _PriorityLock()
        # USB vendor/product ids as reported in AUTOPILOT_VERSION (0 = unknown).
        self._board_vendor_id = 0
        self._board_product_id = 0
        # Set by LogService while a listing or download is running.
        self._log_sink: Callable[[Any], None] | None = None
        # Set by MavlinkForwarder: every raw frame off the link, so a second
        # station (QGroundControl) can share the one physical connection.
        self._frame_sink: Callable[[bytes], None] | None = None
        self._send_lock = threading.Lock()
        self._statustext_chunks: dict[tuple[int, int, int], dict[str, Any]] = {}
        self._home_alt_amsl: float | None = None
        self._position_home_alt_amsl: float | None = None
        self._command_context = threading.local()
        self._reconnect_attempt: int = 1
        # Per-connection-cycle warn-once guard for a late heartbeat (A3).
        self._degraded_warned: bool = False
        # The vehicle's uptime at the last SYSTEM_TIME; see _note_boot_time.
        self._last_boot_ms: int | None = None
        # Why the vehicle will not arm: the last preflight report's reasons.
        self._prearm_reasons: list[str] = []
        self._prearm_reason_at = 0.0
        self._prearm_requested_at = 0.0
        # Each protocol owns its own state; see the mixin modules.
        self._init_param_state()
        self._init_param_metadata_state()
        self._init_ftp_state()
        self._init_mission_state()
        self._init_setup_state()
        self._init_shell_state()
        self._init_telemetry_state()
        self._init_remote_id_state()

    # Accepted MAVLink connection-string prefixes (BUG 2). set_connection()
    # rejects anything else so a non-str/empty/garbage JSON value cannot reach
    # _is_serial() (AttributeError on .startswith) or mavlink_connection("")
    # (which hangs/reconnect-storms).
    # ``udpout:``/``tcpin:`` are the dial-out halves: without them Corvus can
    # only ever bind and wait, which leaves a mavlink-router UdpEndpoint in
    # Server mode (the router binds, the station speaks first) unreachable,
    # along with any router behind NAT.
    _VALID_PREFIXES: tuple[str, ...] = (
        "udp:", "udpin:", "udpout:", "udpbcast:", "tcp:", "tcpin:", "serial:",
    )

    def validate_connection(self, conn_str: str) -> None:
        """Raise ValueError if *conn_str* is not a usable connection spec.

        Split out from :meth:`set_connection` so a caller can check a candidate
        BEFORE tearing down the link it already has. ``/api/mavlink/connect``
        used to stop the bridge and only then validate, so a typo in the
        connection field killed a working radio link and returned a 400 —
        the operator lost the aircraft to a spelling mistake.
        """
        if not isinstance(conn_str, str) or not conn_str:
            raise ValueError("connection must be a non-empty string")
        if not conn_str.startswith(self._VALID_PREFIXES):
            raise ValueError(
                "connection must start with one of: "
                + ", ".join(self._VALID_PREFIXES)
            )
        if conn_str.startswith("serial:"):
            device, baud = self._parse_serial(conn_str)
            if not device:
                raise ValueError("serial connection requires a device path")
            if not isinstance(baud, int) or baud <= 0:
                raise ValueError("serial connection requires a numeric baud rate")

    def set_connection(self, conn_str: str) -> None:
        # The server passes raw JSON here; validate before storing so an
        # invalid value cannot crash _is_serial() or hang mavlink_connection.
        self.validate_connection(conn_str)
        self._conn_str = conn_str

    def connection_string(self) -> str:
        """Return the configured MAVLink connection string (read-only accessor)."""
        return self._conn_str

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
                    # Linux, then macOS. The "cu" nodes are the call-out ones
                    # (the half of each macOS pair that is meant to be opened);
                    # globbing "tty.*" as well would list every port twice.
                    "/dev/ttyUSB*", "/dev/ttyACM*", "/dev/serial/by-id/*",
                    "/dev/cu.usbmodem*", "/dev/cu.usbserial*", "/dev/cu.SLAB_*",
                ):
                    devices.extend(glob.glob(pattern))
                ports = [
                    {"device": d, "description": "", "hwid": ""}
                    for d in sorted(set(devices))
                ]
            except Exception:
                ports = []
        # Drop phantom 8250 platform ports so the real radio isn't buried.
        ports = [p for p in ports if not is_phantom_device(p["device"])]
        ports.sort(key=lambda p: p["device"])
        return ports

    def is_running(self) -> bool:
        """Is the bridge's connect/receive cycle live (``start()`` without ``stop()``)?

        Distinct from :meth:`is_connected`: a bridge that is reconnecting is
        running and not connected. Auto-connect needs that distinction — it may
        act on a link that is retrying, and must not act on one an operator
        deliberately closed.
        """
        return self._running.is_set()

    def is_connected(self) -> bool:
        """Public read of the link gate the command paths use internally.

        Exists so services (log download, flash) can ask without reaching into
        a private, and so "connected" means the same thing to all of them: a
        live link whose heartbeat is not stale.
        """
        return self._connection_ready()

    def board_identity(self) -> dict[str, Any]:
        """Whatever identifies the connected board, for the firmware picker.

        The USB descriptor of the port we are actually connected on (PX4 builds
        its product string from the board, so this is the most direct answer),
        plus the vendor/product ids from AUTOPILOT_VERSION for the boards whose
        descriptor says nothing useful. Read-only and never raises: an unknown
        board just means the operator picks from the list themselves.
        """
        identity: dict[str, Any] = {
            "device": "", "description": "", "hwid": "",
            "vendor_id": self._board_vendor_id,
            "product_id": self._board_product_id,
        }
        device = self.serial_device()
        if not device:
            return identity
        identity["device"] = device
        try:
            for port in self.list_serial_ports():
                if port.get("device") == device:
                    identity["description"] = port.get("description", "")
                    identity["hwid"] = port.get("hwid", "")
                    break
        except Exception:  # noqa: BLE001 - identification is best-effort
            logger.debug("board identification failed", exc_info=True)
        return identity

    def _is_serial(self) -> bool:
        """True when the configured connection is a serial (UART/USB) link."""
        return self._conn_str.startswith("serial:")

    def _is_slow_link(self) -> bool:
        """True for a serial link that is not a USB cable into the autopilot.

        What the stream rates and the parameter budget key off. A USB CDC link
        to the flight controller carries megabits and is throttled like UDP;
        a radio, and any serial port Corvus cannot identify, is treated as a
        57 kbps SiK link, because guessing fast on a radio saturates it.
        Cached per connection string: on macOS and Windows the answer comes
        from enumerating the serial ports, which the parameter watchdog would
        otherwise repeat on every pass.
        """
        if not self._is_serial():
            return False
        cached = getattr(self, "_slow_link_for", None)
        if cached is not None and cached[0] == self._conn_str:
            return cached[1]
        slow = self.transport() != "usb"
        self._slow_link_for = (self._conn_str, slow)
        return slow

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

    def transport(self) -> str:
        """Classify the configured connection: 'usb' | 'sik' | 'udp' | 'tcp' | 'unknown'.

        'usb' = a DIRECT USB link to the flight controller (flashable). This is a
        serial: connection whose device is a CDC ACM node (/dev/ttyACM*, /dev/serial/by-id/*
        pointing at a known Pixhawk-class FC), NOT a USB-to-serial radio adapter.
        'sik' = a SiK Telemetry Radio or other USB-to-serial adapter (/dev/ttyUSB*).
        'udp' = any udp:/udpin:/udpout:/udpbcast: connection.
        'tcp' = any tcp:/tcpin: connection.
        'unknown' = anything else (including unrecognised serial devices).

        On Windows (COM<n>) and macOS (/dev/cu.*) the device name says nothing
        about what is on the other end, so the answer comes from the port's USB
        descriptor instead — see :meth:`_classify_by_descriptor`.
        """
        conn = self._conn_str
        if conn.startswith(("udp:", "udpin:", "udpout:", "udpbcast:")):
            return "udp"
        if conn.startswith(("tcp:", "tcpin:")):
            return "tcp"
        if conn.startswith("serial:"):
            # Reuse the existing parser so the device/baud split stays in one
            # place (the trailing ':baud' is stripped here, not duplicated).
            device, _ = self._parse_serial(conn)
            if _DIRECT_USB_ACM_RE.match(device):
                return "usb"
            if device.startswith("/dev/serial/by-id/") and _PIXHAWK_BYID_RE.search(device):
                return "usb"
            if _SIK_RADIO_RE.match(device):
                return "sik"
            if is_windows_com_port(device) or _MACOS_SERIAL_RE.match(device):
                return self._classify_by_descriptor(device)
            return "unknown"
        return "unknown"

    def _classify_by_descriptor(self, device: str) -> str:
        """'usb' / 'sik' / 'unknown' from the port's USB descriptor, not its name.

        For the two platforms whose device names carry no information about
        what is behind them. A Windows COM number is just an index the OS
        handed out; a macOS /dev/cu.* node is named after the driver that
        claimed it. Neither can be classified by inspection the way a Linux
        /dev/ttyACM0 can.

        pyserial reports the USB vendor/product ids in ``hwid``, and those do
        separate the two cases that matter here: a Pixhawk-class FMU presenting
        its own CDC interface, versus an FTDI/CP210x/CH340 bridge with a radio
        behind it.

        Unknown wins ties. Getting this wrong in the permissive direction would
        offer to flash firmware down a telemetry radio, and the operator finds
        out at the point the autopilot stops answering.
        """
        hwid = ""
        for candidate in self._descriptor_aliases(device):
            for port in self.list_serial_ports():
                if str(port.get("device", "")).strip().lower() == candidate:
                    hwid = f"{port.get('hwid', '')} {port.get('description', '')}"
                    break
            if hwid.strip():
                break
        return classify_hwid(hwid)

    @staticmethod
    def _descriptor_aliases(device: str) -> list[str]:
        """The names one physical port can be spelled as, lowercased.

        macOS gives every serial device two nodes — ``/dev/cu.X`` (call-out)
        and ``/dev/tty.X`` (dial-in) — and pyserial enumerates only the first.
        An operator who types the ``tty`` spelling is naming the same hardware,
        so the descriptor lookup tries its sibling rather than answering
        "unknown" about a port it can see perfectly well.
        """
        name = device.strip().lower()
        aliases = [name]
        for this, other in (("/dev/tty.", "/dev/cu."), ("/dev/cu.", "/dev/tty.")):
            if name.startswith(this):
                aliases.append(other + name[len(this):])
        return aliases

    def is_direct_usb(self) -> bool:
        """True when the link is a direct FC USB connection (transport() == 'usb')."""
        return self.transport() == "usb"

    def serial_device(self) -> str:
        """Return the serial device path for a serial: connection (stripped of the
        'serial:' prefix and the trailing ':baud'), or '' for non-serial links.
        """
        if self._conn_str.startswith("serial:"):
            device, _ = self._parse_serial(self._conn_str)
            return device
        return ""

    def start(self) -> None:
        self._stop_event.clear()
        if self._thread and self._thread.is_alive():
            if self._running.is_set():
                # A healthy MAVLink thread is already running — don't spawn a
                # second one (preserves the original idempotent no-op).
                return
            # stop() was just called but the old thread hasn't fully died yet
            # (its join timed out while _connect() was blocked). Join briefly,
            # then proceed so a fresh start() can reconnect (BUG 3).
            self._thread.join(timeout=1.0)
        self._running.set()
        self._thread = threading.Thread(target=self._run, name="mavlink", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._running.clear()
        self._stop_event.set()
        self._stop_tlog()
        self._store.update(link_status="disconnected")
        # Mark the vehicle disconnected so _connection_ready() returns False and
        # any in-flight command/param writer bails (BUG 3, 7).
        self._store.set_disconnected()
        self._cancel_pending_commands()
        self._abort_parameter_operations()
        # Release exclusive NSH shell mode before the link closes so PX4 frees
        # the shell for other clients. Best-effort; never masks teardown.
        try:
            self.stop_shell()
        except Exception:
            pass
        # Join the batch-upload worker. Its set_param calls were already woken
        # with result=-2 by the _param_set_pending block above, and it re-checks
        # _running each iteration, so it exits promptly; the daemon flag is a
        # backstop. Done before the socket close so a mid-flight send isn't
        # writing on a torn-down link.
        if self._param_upload_thread:
            self._param_upload_thread.join(timeout=3)
            self._param_upload_thread = None
        # Cancels a metadata download mid-transfer: its waits check the
        # cancel event and the stop event every 0.1 s.
        self._stop_param_metadata()
        if self._conn:
            try:
                self._conn.close()
            except Exception:
                pass
            # Clear immediately so _send_command_and_wait's `conn is not
            # self._conn` guard bails on remaining loop iterations (BUG 3).
            self._conn = None
        # Join the deferred daemon workers so stop() never leaves them
        # spinning. _stop_event wakes their interruptible sleeps promptly.
        if self._param_watchdog_thread:
            self._param_watchdog_thread.join(timeout=2)
            self._param_watchdog_thread = None
        if self._version_retry_thread:
            self._version_retry_thread.join(timeout=2)
            self._version_retry_thread = None
        if self._intervals_thread:
            self._intervals_thread.join(timeout=2)
            self._intervals_thread = None
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
                # Defer the ACK-confirmed interval requests until the receive
                # loop is draining the socket (BUG 1): _connect() used to run
                # them before _receive_loop() existed, so every COMMAND_ACK
                # timed out and stalled all operator commands issued then.
                self._schedule_message_intervals()
                self._receive_loop()
            except Exception as exc:
                cycle_exc = exc
                logger.error("MAVLink error: %s", exc)
            self._cancel_pending_commands()
            self._abort_parameter_operations()
            self._reset_shell_state()
            # Close this cycle's tlog so a reconnect opens a fresh file.
            self._stop_tlog()
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
                # Interruptible: the backoff reaches 8 s, stop() joins this
                # thread for 3, so a plain sleep meant shutdown returned while
                # the reconnect loop was still parked — and then woke up to
                # reconnect against a store the app had already torn down.
                self._interruptible_sleep(delay)
                self._reconnect_attempt += 1

    def _claim_serial_exclusive(self, device: str) -> None:
        """Take an exclusive advisory lock on the just-opened serial port.

        POSIX will let any number of processes open the same ``/dev/tty*`` and
        will report no error to any of them; it simply hands each read whatever
        bytes arrived first. Two ground stations on one USB link therefore each
        receive an arbitrary *half* of the MAVLink stream — attitude updating
        but position frozen, commands whose ACK lands in the other program —
        and neither shows a disconnect. It is the quietest way this application
        can be badly wrong, and it is exactly what a second launch used to do.

        ``flock`` is what pyserial's own ``exclusive=True`` uses; pymavlink
        does not pass that through, so it is applied here to the descriptor
        pymavlink opened. The lock lives on the file handle, so closing the
        port or losing the process releases it — a crash leaves nothing stale.

        Windows needs none of this: it opens a COM port exclusively already,
        and the second open fails with a plain "access denied".

        Best-effort by design. A device that cannot be locked (a pty in the
        tests, an odd driver, a platform without ``fcntl``) is used anyway:
        refusing to fly over a missing lock would be the worse failure.
        """
        if os.name == "nt":
            return
        try:
            import fcntl
        except ImportError:  # pragma: no cover - POSIX only path
            return
        handle = getattr(self._conn, "port", None)
        fileno = getattr(handle, "fileno", None)
        if not callable(fileno):
            return
        try:
            fd = fileno()
        except Exception as exc:  # noqa: BLE001 - a port with no fd is not lockable
            logger.debug("serial port has no descriptor to lock: %s", exc)
            return
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            message = (
                f"{device} is in use by another program. Close the other "
                f"ground station and reconnect"
            )
            logger.error("%s", message)
            self._store.update(link_error=message)
            try:
                self._conn.close()
            except Exception:  # noqa: BLE001 - we are already failing this cycle
                pass
            self._conn = None
            raise ConnectionError(message) from exc
        except OSError as exc:
            logger.debug("serial port %s could not be locked: %s", device, exc)

    def _reconnect_delay(self, attempt: int) -> float:
        """Exponential backoff with jitter between reconnect cycles (A4).

        Unified for serial and UDP — the old linear serial backoff (2s/attempt,
        cap 5s) was too aggressive on lossy 57 kbps radios, hammering the link
        while it was still recovering. base*2^(n-1), capped, +/- 25% jitter.
        """
        raw = min(RECONNECT_BASE_S * (2 ** max(0, attempt - 1)), RECONNECT_CAP_S)
        delayed = raw * (1.0 + random.uniform(-RECONNECT_JITTER, RECONNECT_JITTER))
        return max(RECONNECT_MIN_S, delayed)

    # PX4's onboard/offboard links: 14540+instance is where MAVROS and MAVSDK
    # bind, 14550 is the ground station's. The default moved off 14540 in
    # config.py, but only for a fresh install — a config file written before
    # that still names it, and "udp:" means bind.
    _ONBOARD_PORTS = range(14540, 14550)

    def _warn_if_onboard_port(self) -> None:
        """Say so when Corvus is holding the socket a companion process needs.

        Two programs cannot both read one UDP port, and which one loses is
        decided by the operating system rather than by anything the operator
        did: Linux refuses the second bind outright (the EADDRINUSE message
        above covers that), while macOS lets it through and quietly hands the
        datagrams to one socket. When Corvus wins that toss, MAVROS goes
        silent — no HOME_POSITION, no odometry — and every node downstream of
        it waits on topics that will never be published, which looks like a
        broken autopilot and not like a port clash. Nothing here is fatal and
        nothing is changed on the operator's behalf: this is a link they
        configured, and a single line naming the real problem is worth more
        than a connection refused on their behalf.
        """
        # The binding forms only. pymavlink's "udp:" defaults to input=True
        # and "udpin:" is the explicit spelling of the same thing, so both take
        # the port away from whoever else wants it; "udpout:"/"udpbcast:"/
        # "tcp:" dial out from an ephemeral local port and name somebody else's
        # listening socket, so they take nothing from anyone. Checking only
        # "udp:" meant an operator who wrote the port out as "udpin:0.0.0.0:
        # 14540" held the companion link with no warning at all.
        if not self._conn_str.startswith(("udp:", "udpin:")):
            return
        port = self._conn_str.rsplit(":", 1)[-1]
        if not port.isdigit() or int(port) not in self._ONBOARD_PORTS:
            return
        text = (
            f"Connected on port {port}, PX4's onboard link, which MAVROS and "
            f"MAVSDK bind. If a companion process needs it, move Corvus to "
            f"udp:0.0.0.0:14550 (the ground station port) instead."
        )
        logger.warning(text)
        self._console_publish("LINK", text, "warning")

    def _connect(self) -> None:
        logger.info("Connecting to %s …", self._conn_str)
        self._reset_shell_state()
        self._reset_parameter_cache()
        self._reset_param_metadata()
        self._request_sent = False
        self._param_encoding_declared = ""
        self._last_boot_ms = None
        self._clear_prearm_reasons()
        self._prearm_requested_at = 0.0
        self._home_alt_amsl = None
        self._position_home_alt_amsl = None
        # And forget the DISPLAYED home with it. The bridge already dropped
        # its own altitude reference here; leaving the map's marker behind
        # meant a session that reconnected — to the same aircraft moved between
        # flights, or to a different one — drew last session's launch point as
        # this one's, and the "centre on vehicle" fallback flew there. A home
        # marker has to be something the aircraft said in this session or
        # nothing at all; _request_home() below asks for it immediately so the
        # gap is one round trip rather than the stream's own 0.5 Hz.
        self._store.update(home=[0.0, 0.0])
        # Per-connection-cycle state (A1/A3): fresh jitter window + flags.
        self._hb_times.clear()
        self._degraded_warned = False
        self._radio_status_seen = False
        self._uplink_score = 0
        # A new link may be a new aircraft with a different pack, so the
        # latched cell count goes with the old connection.
        self._forget_battery_cells()
        self._forget_gnss_integrity()
        # Same reasoning for Remote ID: the arm status and the "we are sending"
        # timestamp both describe the link that just went away.
        self._forget_remote_id_session()
        # A new link may be a new aircraft, and even the same one may have had
        # its mission replaced while nobody was listening.
        self._forget_vehicle_mission()
        self._vehicle_mission_id = None
        self._store.update(mission_state="", mission_seq=-1, mission_reached=-1,
                           mission_total=-1)
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
                self._conn = mavutil.mavlink_connection(
                    device,
                    baud=baud,
                    source_system=GCS_SYSTEM_ID,
                    source_component=GCS_COMPONENT_ID,
                )
            else:
                self._conn = mavutil.mavlink_connection(
                    self._conn_str,
                    timeout=2,
                    source_system=GCS_SYSTEM_ID,
                    source_component=GCS_COMPONENT_ID,
                )
        except (OSError, FileNotFoundError) as exc:
            if self._is_serial():
                message = f"serial device unavailable: {self._conn_str}"
                self._store.update(link_error=message)
                raise ConnectionError(message) from exc
            if getattr(exc, "errno", None) == errno.EADDRINUSE:
                # "[Errno 48] Address already in use" is true and useless. The
                # cause is nearly always another program already listening on
                # the port — QGroundControl, MAVROS, a second Corvus — and the
                # fix is to pick a different one, which the message should say.
                message = (
                    f"{self._conn_str}: port already in use by another program "
                    f"(QGroundControl, MAVROS/MAVSDK or a second Corvus). "
                    f"Close it or connect on a different port"
                )
                self._store.update(link_error=message)
                raise ConnectionError(message) from exc
            raise
        if self._is_serial():
            self._claim_serial_exclusive(self._parse_serial(self._conn_str)[0])
        else:
            self._warn_if_onboard_port()
        self._apply_signing()
        # Speak first, then listen. A far end that waits to hear from a station
        # before it sends anything (a mavlink-router server endpoint, SITL in a
        # VM reached with udpout:, a USB board with SYS_USB_AUTO = 1) and a
        # station that waits for the vehicle first would each wait for the
        # other until the heartbeat wait below gave up, on every reconnect.
        self._send_gcs_heartbeat(self._conn)
        self._start_gcs_heartbeat()
        logger.info("Waiting for heartbeat …")
        hb = self._wait_vehicle_heartbeat(HEARTBEAT_WAIT_S)
        if hb is None:
            raise ConnectionError("No heartbeat received")
        # PX4 autopilot is comp 1; comp 0 = broadcast (not addressable).
        src_sys = getattr(hb, "get_srcSystem", lambda: 0)() or self._conn.target_system
        src_comp = getattr(hb, "get_srcComponent", lambda: 0)() or self._conn.target_component
        self._target_system = src_sys or 1
        self._target_component = src_comp or 1
        # Tell pymavlink which node we picked. It does its own latching in
        # post_message — first heartbeat its `probably_vehicle_heartbeat`
        # accepts wins — and that is not always the one _wait_vehicle_heartbeat
        # chose: behind a router the two can land on different nodes, and
        # mode_mapping() then reads the mode table of whatever pymavlink
        # latched onto instead of the aircraft Corvus is flying. Everything
        # this module sends already addresses _target_system explicitly; this
        # aligns the parts of mavutil that do not.
        try:
            self._conn.target_system = self._target_system
            self._conn.target_component = self._target_component
        except Exception as exc:  # noqa: BLE001 - a mavutil that will not be told is not fatal
            logger.debug("could not pin mavutil target: %s", exc)
        vtype = MAV_TYPE_MAP.get(hb.type, f"TYPE_{hb.type}")
        autopilot = MAV_AUTOPILOT_MAP.get(hb.autopilot, f"AP_{hb.autopilot}")
        # Before anything decodes a mode. The mode word means different things
        # on different stacks, so reading it with the previous session's
        # dialect is how a reconnect from a PX4 bench to an ArduPilot aircraft
        # used to show PX4 mode names for the first second of the flight.
        self._latch_dialect(hb.autopilot, hb.type)
        armed = bool(hb.base_mode & mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
        mode = self._decode_mode(hb)
        logger.info("Heartbeat: %s / %s (%s) mode=%s (sys=%d comp=%d)",
                    vtype, autopilot, self._dialect.label, mode,
                    self._target_system, self._target_component)
        self._store.update(
            connected=True, vehicle_type=vtype, autopilot=autopilot,
            armed=armed, mode=mode, link_status="connected", link_error="",
            autopilot_stack=self._dialect.stack,
        )
        self._store.heartbeat()
        self._build_mode_mapping()
        # Per-message SET_MESSAGE_INTERVAL (A5) is what every supported target
        # (v1.16-v1.18) wants; the ACK-confirmed requests are deferred to
        # _run() so the receive loop exists to dispatch their COMMAND_ACKs
        # (BUG 1). The deprecated REQUEST_DATA_STREAM is only the fallback for
        # stacks that do not answer it — ArduPilot above all, where it is the
        # classic mechanism behind the SRx_ parameters. It is NOT a fallback
        # for old PX4: PX4 carries no handler for the message in any release
        # from v1.11 to today, so on a PX4 target it is simply ignored. On UDP
        # it no longer goes out blind here: it waits
        # until those per-message requests prove it is needed — see
        # _request_message_intervals(). A UDP link is rarely the autopilot's
        # only client (MAVROS, a companion computer, mavlink-router), and a
        # blanket legacy rate request is the loudest thing a GCS can say to a
        # stack it did not set up. A serial link still asks up front: it is
        # point to point, there is nobody else to disturb, and a 57 kbps radio
        # must not hold the HUD empty for a command round trip first.
        if self._is_serial():
            self._request_streams()
        self._request_version()
        self._request_home()
        # Start a fresh tlog for this flight session. Gated on _running so the
        # direct-_connect() unit tests (which never start() the bridge) do not
        # open files or spawn writer threads in ~/.corvus/logs; in production
        # _run only calls _connect while _running is set.
        self._start_tlog()

    def _apply_signing(self) -> None:
        """Turn on MAVLink 2 signing when the operator has configured a key.

        Opt-in, and off unless ``CORVUS_MAVLINK_SIGNING_KEY_FILE`` names a
        32-byte key the autopilot has been given as well. With it on, every
        frame Corvus sends is signed and unsigned frames are rejected — which
        is the only thing that stops somebody within radio range of a SiK link
        from injecting a disarm, because the radio itself authenticates
        nothing and will relay whatever it hears on its frequency.

        Three messages stay exempt, and deliberately: RADIO_STATUS comes from
        the SiK modem, which has no key and cannot sign; ADSB_VEHICLE and
        COLLISION are traffic warnings from transponders outside the vehicle.
        None of the three can command anything, and dropping them would cost
        the link-quality readout and collision alerts for no gain.

        A key that cannot be loaded fails the connection rather than falling
        back to an unsigned link. Anything else would mean a typo in a path
        silently downgrades the security the operator asked for, and they would
        have no way to tell the difference from the outside.
        """
        try:
            key = _load_signing_key()
        except (OSError, ValueError, PermissionError) as exc:
            message = f"MAVLink signing key unusable: {exc}"
            logger.error("%s", message)
            self._store.update(link_error=message)
            raise ConnectionError(message) from exc
        if key is None:
            return
        try:
            self._conn.setup_signing(
                key,
                sign_outgoing=True,
                allow_unsigned_callback=_allow_unsigned_signed_link,
            )
        except Exception as exc:  # noqa: BLE001 - a dialect without signing support
            message = f"MAVLink signing could not be enabled: {exc}"
            logger.error("%s", message)
            self._store.update(link_error=message)
            raise ConnectionError(message) from exc
        logger.info("MAVLink 2 signing enabled for %s", self._conn_str)
        self._console_publish("LINK", "MAVLink signing enabled", "success")

    def _wait_vehicle_heartbeat(self, timeout: float) -> Any:
        """The first HEARTBEAT from an actual autopilot, within *timeout*.

        ``mavutil.wait_heartbeat`` matches on message type alone, so on a link
        that carries more than one node — anything behind mavlink-router — it
        hands back whichever heartbeat happens to arrive first. If that is
        QGroundControl's, ``_target_system`` becomes 255 and every command this
        session sends is addressed to a ground station: arming, mode changes,
        parameter reads and mission uploads all time out, with a connected
        green dot the whole time.

        So keep reading until the aircraft identifies itself, inside the same
        wall-clock budget a single wait would have had.
        """
        deadline = time.monotonic() + timeout
        while True:
            # stop() during connect: bail rather than sit out the rest of the
            # budget. Closing the socket would end the wait too, but only by
            # raising through _run's error path, which then logs a shutdown as
            # a link failure.
            if self._stop_event.is_set():
                return None
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            conn = self._conn
            if conn is None:
                return None
            # Each wait is capped so the stop check above is reached within a
            # second even on a link where nothing arrives at all; the deadline
            # above still bounds the whole thing to the caller's budget.
            wait_for = min(1.0, remaining)
            waited_from = time.monotonic()
            hb = conn.wait_heartbeat(blocking=True, timeout=wait_for)
            if hb is None:
                # Retry only if that wait really blocked for its timeout. A
                # transport that answers "nothing" the instant it is asked is
                # not going to start answering differently, and retrying it
                # would burn the whole budget at full tilt — so an immediate
                # None is taken at face value, exactly as a single
                # full-length wait used to take it.
                if time.monotonic() - waited_from < wait_for / 2:
                    return None
                continue
            if _is_vehicle_heartbeat(hb):
                return hb
            logger.info(
                "ignoring heartbeat from sys=%s comp=%s: not an autopilot",
                getattr(hb, "get_srcSystem", lambda: "?")(),
                getattr(hb, "get_srcComponent", lambda: "?")(),
            )

    def _latch_dialect(self, autopilot_id: Any, mav_type: Any) -> None:
        """Pick the flight-stack dialect this link will be flown with.

        Called from :meth:`_connect` and again from every HEARTBEAT, because a
        vehicle behind mavlink-router can be swapped for a different one
        without the socket ever closing. Rebuilding the mode table is deferred
        to :meth:`_build_mode_mapping`; this only records what we are talking
        to, which every decode below then reads.
        """
        try:
            type_id = int(mav_type)
        except (TypeError, ValueError):
            type_id = 0
        dialect = autopilot_dialect.dialect_for(autopilot_id)
        changed = dialect is not self._dialect or type_id != self._mav_type_id
        self._dialect = dialect
        self._mav_type_id = type_id
        if changed:
            logger.info(
                "Flight stack: %s (MAV_TYPE %s)", dialect.label, type_id,
            )

    def _decode_mode(self, hb: Any) -> str:
        """HEARTBEAT -> the mode name, in the connected stack's own vocabulary.

        PX4 packs a main mode and a sub mode into ``custom_mode``; ArduPilot
        puts one flat number there and keeps a different table per vehicle.
        Neither decoding is a superset of the other, so this delegates rather
        than guessing — see :mod:`corvus.autopilot`.
        """
        try:
            return self._dialect.decode_mode(
                getattr(hb, "custom_mode", 0),
                getattr(hb, "base_mode", 0),
                getattr(hb, "type", self._mav_type_id),
            )
        except Exception:
            return ""

    def _build_mode_mapping(self) -> None:
        """Latch the mode names this link can actually command.

        pymavlink answers ``mode_mapping()`` in two shapes, because the flight
        stacks encode a mode differently: PX4 gets ``px4_map``, whose values are
        the ``(base_mode, main_mode, sub_mode)`` triples DO_SET_MODE wants;
        everything else gets a per-MAV_TYPE table whose values are a single flat
        ``custom_mode`` integer.

        Corvus used to publish the first shape and throw the second away, which
        meant an ArduPilot pilot got an empty mode selector — the honest answer
        at the time, because ``set_mode`` could only send the triple. It can now
        send either, so the flat table is exactly what an ArduPilot vehicle
        needs: ``MAV_MODE_FLAG_CUSTOM_MODE_ENABLED`` in param1 and the number in
        param2. The dialect decides which reading applies, and a stack neither
        dialect recognises still publishes nothing rather than somebody else's
        modes.
        """
        self._modes_unsupported = False
        mapping: dict[str, Any] = {}
        try:
            mapping = self._conn.mode_mapping() or {}
        except Exception as exc:
            logger.debug("mode_mapping error: %s", exc)
        usable = self._dialect.adopt_live_mapping(mapping, self._mav_type_id)
        if usable:
            self._mode_mapping = set(usable)
            self._mode_values = dict(usable)
            logger.info("Mode mapping (%s): %s", self._dialect.label, sorted(usable))
            return
        # No live mapping this dialect can use. The built-in table is the
        # fallback: for PX4 that is px4_map's own values (BUG 9, so set_mode and
        # get_available_modes cannot disagree), for ArduPilot the per-vehicle
        # table in corvus.autopilot, and for an unrecognised stack nothing at
        # all — which is what sets _modes_unsupported.
        builtin = self._dialect.mode_table(self._mav_type_id)
        self._mode_values = dict(builtin)
        self._mode_mapping = set()
        if not builtin:
            self._modes_unsupported = True
            logger.info(
                "no mode table for this autopilot (%s, MAV_TYPE %s), so mode "
                "selection is unavailable on this link",
                self._dialect.label, self._mav_type_id,
            )

    def _start_gcs_heartbeat(self) -> None:
        if self._hb_thread and self._hb_thread.is_alive():
            return
        self._hb_thread = threading.Thread(target=self._gcs_hb_loop, name="gcs-hb", daemon=True)
        self._hb_thread.start()

    def _send_gcs_heartbeat(self, conn: Any) -> bool:
        """Send one GCS heartbeat on *conn* now. Never raises.

        A binding UDP socket that has not heard from anybody yet has nowhere to
        send to; pymavlink drops the frame, which is the right outcome there.
        """
        if conn is None:
            return False
        try:
            with self._send_lock:
                if conn is not self._conn:
                    return False
                conn.mav.heartbeat_send(
                    mavutil.mavlink.MAV_TYPE_GCS,
                    mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                    0, 0, 0,
                )
            return True
        except Exception as exc:  # noqa: BLE001 - a first frame that fails is retried at 1 Hz
            logger.debug("GCS heartbeat send failed: %s", exc)
            return False

    def _gcs_hb_loop(self) -> None:
        # Runs while _running is set regardless of link_status: a degraded
        # link (A3) must NOT stop the GCS heartbeat, or the autopilot drops us
        # while we wait for its heartbeat to return (A7).
        me = threading.current_thread()
        while self._running.is_set() and not self._stop_event.is_set():
            conn = self._conn
            if not conn:
                self._interruptible_sleep(1)
                continue
            # Read outside the send lock. The heartbeat starts before the
            # vehicle is found; the Remote ID identity is addressed to it.
            vehicle_known = bool(self._store.get_snapshot().get("connected"))
            try:
                with self._send_lock:
                    # Re-check under the lock: stop()/reconnect can clear
                    # _conn or _running while we waited for the lock.
                    if not self._running.is_set():
                        break
                    if conn is not self._conn:
                        continue
                    conn.mav.heartbeat_send(
                        mavutil.mavlink.MAV_TYPE_GCS,
                        mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                        0, 0, 0,
                    )
                    # Remote ID rides the heartbeat rather than getting a thread
                    # of its own. Both stacks want the identity at about 1 Hz and
                    # fail the pre-arm check when it stops, which is exactly this
                    # loop's cadence and exactly its lifetime — and a second
                    # 1 Hz worker would be a second thing to join on shutdown
                    # (AGENTS.md: every thread the change introduces is torn down).
                    if vehicle_known:
                        self._send_remote_id(conn)
            except Exception:
                # A reconnect swaps/closes the socket under this persistent
                # worker. Stay alive so the next connection gets heartbeats
                # even when it became ready before this thread observed the
                # old socket failure.
                self._interruptible_sleep(0.1)
                continue
            self._interruptible_sleep(1)
        # Allow _start_gcs_heartbeat to restart us on the next connect — but
        # only if nobody already has. A reconnect can call
        # _start_gcs_heartbeat() while this loop is on its way out; clearing
        # the field unconditionally then orphaned the *new* thread, so stop()
        # never joined it and the next connect started a second heartbeat
        # alongside it — two GCS heartbeats at 1 Hz on the same link.
        if self._hb_thread is me:
            self._hb_thread = None

    # ------------------------------------------------------------------
    # Telemetry log (tlog) — one file per connect cycle (F2)
    # ------------------------------------------------------------------

    @staticmethod
    def _next_tlog_path(log_dir: str) -> str:
        """A tlog path in *log_dir* that is not already taken.

        The name is a timestamp because that sorts, and it carried microseconds
        because two reconnects can land in the same second. That was assumed to
        make it collision-free; it does not. ``datetime.now()`` resolves to the
        platform clock, and Windows' is ~1 ms at best — two reconnects inside
        one tick produced one name, so the second session opened the first
        session's file and overwrote a flight's recorded telemetry.

        The clock is therefore no longer trusted to be unique on its own: if
        the path is taken, a counter is appended. The separator is "_" and not
        "-" so the names still sort into session order — "-" (0x2D) sorts
        BEFORE "." (0x2E), which would have put the second session's file ahead
        of the first's in the Analysis listing.
        """
        stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
        path = os.path.join(log_dir, stamp + ".tlog")
        n = 1
        while os.path.exists(path):
            # Zero-padded so _02 sorts after _01 rather than lexicographically
            # scattering once a tick collects ten sessions.
            path = os.path.join(log_dir, f"{stamp}_{n:02d}.tlog")
            n += 1
        return path

    def _start_tlog(self) -> None:
        """Open a fresh tlog for this flight session.

        No-op when the bridge is not running (e.g. direct-_connect unit tests)
        so they stay hermetic. A log-dir permission error must never break the
        link: on failure the tlog is simply disabled for the session.
        """
        if not self._running.is_set():
            return
        # Close any leftover writer from a prior cycle (defensive: _run tears
        # down between reconnects, but guard against a double connect).
        self._stop_tlog()
        try:
            log_dir = default_log_dir()
            os.makedirs(log_dir, exist_ok=True)
            path = self._next_tlog_path(log_dir)
            self._tlog = TlogWriter(path)
            self._tlog.set_conn(self._conn_str)
        except Exception as exc:
            logger.warning("tlog disabled: %s", exc)
            self._tlog = None

    def _stop_tlog(self) -> None:
        """Stop and close the current tlog if active; idempotent."""
        tlog = self._tlog
        if tlog is None:
            return
        self._tlog = None
        try:
            tlog.stop()
        except Exception as exc:
            logger.debug("tlog stop failed: %s", exc)

    def _write_tlog(self, msg: Any) -> None:
        """Log a raw MAVLink frame if a tlog is active; never breaks the link.

        Also hands the same frame to the forwarder when one is attached: both
        consumers want the bytes exactly as they came off the wire, and both
        are non-blocking appends, so taking the msgbuf once serves both.
        """
        tlog = self._tlog
        sink = self._frame_sink
        if tlog is None and sink is None:
            return
        try:
            get_buf = getattr(msg, "get_msgbuf", None)
            if get_buf is None:
                return
            raw = get_buf()
        except Exception as exc:
            logger.debug("frame capture failed: %s", exc)
            return
        if tlog is not None:
            try:
                tlog.write_frame(raw)
            except Exception as exc:
                logger.debug("tlog write failed: %s", exc)
        if sink is not None:
            try:
                sink(raw)
            except Exception as exc:  # noqa: BLE001 - a sink never breaks the link
                logger.debug("frame forward failed: %s", exc)

    def set_frame_sink(self, sink: Callable[[bytes], None] | None) -> None:
        """Route every raw received frame to *sink* (None detaches)."""
        self._frame_sink = sink

    def inject_raw(self, frame: bytes) -> bool:
        """Write one already-framed MAVLink message straight to the aircraft.

        The entry point for a second ground station: the bytes are passed
        through untouched, so QGroundControl's own sequence numbers, system id
        and (if it uses them) signature reach PX4 exactly as it wrote them —
        re-encoding here would break signing and confuse PX4's per-sender
        sequence tracking.

        Guarded by the same send lock as every other transmit path, so an
        injected frame can never interleave with one Corvus is writing.
        """
        conn = self._conn
        if conn is None or not frame:
            return False
        try:
            with self._send_lock:
                if conn is not self._conn:
                    return False
                conn.write(frame)
        except Exception as exc:  # noqa: BLE001 - a second station never kills the link
            logger.debug("inject failed: %s", exc)
            return False
        return True

    def _stream_rates(self) -> dict[int, int]:
        """Per-stream REQUEST_DATA_STREAM rates, throttled on radio links.

        A SiK Telemetry Radio V3 tunnels MAVLink over a ~57 kbps serial link,
        so the default 50 Hz attitude/RC streams would saturate it. Keep
        MAV_DATA_STREAM_* constants: they are what ArduPilot honours, and the
        reason this path exists at all. PX4 ignores the message entirely, so
        these rates only ever reach a non-PX4 stack.
        """
        if self._is_slow_link():
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
        """Send the deprecated REQUEST_DATA_STREAM set, at most once per link.

        The legacy path. On serial it runs from _connect() so telemetry starts
        without waiting on a command round trip; on UDP it runs only if
        _request_message_intervals() finds the modern mechanism missing. The
        _request_sent latch is what lets that second caller retry per message
        without sending the whole set again, and _connect() clears it so the
        next connection makes the decision afresh.
        """
        conn = self._conn
        if self._request_sent or conn is None:
            return
        for stream, rate in self._stream_rates().items():
            if rate == 0:
                continue
            try:
                with self._send_lock:
                    if conn is not self._conn:
                        return
                    conn.mav.request_data_stream_send(
                        self._target_system, self._target_component, stream, rate, 1,
                    )
            except Exception as exc:  # noqa: BLE001 - a fallback request is best-effort
                logger.debug("REQUEST_DATA_STREAM %s failed: %s", stream, exc)
        self._request_sent = True

    def _message_intervals(self) -> dict[int, int]:
        """Per-message MAV_CMD_SET_MESSAGE_INTERVAL targets in microseconds.

        Modern PX4 (v1.16-v1.18) rate-control, and the only mechanism PX4 has
        ever answered; REQUEST_DATA_STREAM (above) is the fallback for stacks
        that do not, which in practice means ArduPilot. Radio links are
        throttled to fit a 57 kbps SiK radio; a USB cable is not. VIBRATION
        is on-demand only (not listed here).
        """
        m = mavutil.mavlink
        if self._is_slow_link():
            return {
                m.MAVLINK_MSG_ID_ATTITUDE: 100000,            # 10 Hz
                m.MAVLINK_MSG_ID_GLOBAL_POSITION_INT: 200000,  # 5 Hz
                m.MAVLINK_MSG_ID_VFR_HUD: 200000,             # 5 Hz
                m.MAVLINK_MSG_ID_SYS_STATUS: 1_000_000,       # 1 Hz
                m.MAVLINK_MSG_ID_GPS_RAW_INT: 1_000_000,      # 1 Hz
                # 0.2 Hz, because home is where RTL ends and a stale one on
                # the map is a lie about where the aircraft will come back to.
                # Cheap at this rate — HOME_POSITION is 60 bytes — and it is
                # the one field the serial set used to leave entirely to PX4's
                # own defaults, which not every firmware publishes.
                m.MAVLINK_MSG_ID_HOME_POSITION: 5_000_000,    # 0.2 Hz
                # 1 Hz even on a 57 kbps radio: the message is two bytes of
                # payload, and it is what tells the bar the aircraft is flying.
                m.MAVLINK_MSG_ID_EXTENDED_SYS_STATE: 1_000_000,  # 1 Hz
                # 0.5 Hz: the pack's own detail — per-cell volts, consumed mAh,
                # temperature. SYS_STATUS carries none of it, and on a smart or
                # DroneCAN battery it is the difference between a pack voltage
                # and knowing which cell is dragging the pack down. 41 bytes
                # twice a minute of a 57 kbps budget.
                m.MAVLINK_MSG_ID_BATTERY_STATUS: 2_000_000,   # 0.5 Hz
            }
        return {
            m.MAVLINK_MSG_ID_HEARTBEAT: 1_000_000,            # 1 Hz
            m.MAVLINK_MSG_ID_ATTITUDE: 20_000,                # 50 Hz
            m.MAVLINK_MSG_ID_GLOBAL_POSITION_INT: 100_000,    # 10 Hz
            m.MAVLINK_MSG_ID_VFR_HUD: 100_000,               # 10 Hz
            m.MAVLINK_MSG_ID_SYS_STATUS: 1_000_000,          # 1 Hz
            m.MAVLINK_MSG_ID_GPS_RAW_INT: 1_000_000,         # 1 Hz
            m.MAVLINK_MSG_ID_HOME_POSITION: 1_000_000,       # 1 Hz
            m.MAVLINK_MSG_ID_EXTENDED_SYS_STATE: 1_000_000,  # 1 Hz
            # 5 Hz, matching what MAV_DATA_STREAM_RC_CHANNELS used to ask for.
            # That request is no longer sent on UDP, and RC_CHANNELS is the one
            # message Corvus displays that the legacy set covered and this one
            # did not — leaving it out would trade a stream nobody asked to
            # lose for the fix. Off the serial list still, where the radio's
            # 57 kbps buys attitude and position instead.
            m.MAVLINK_MSG_ID_RC_CHANNELS: 200_000,           # 5 Hz
            # See the serial list: the pack detail SYS_STATUS has no room for.
            # Asked for explicitly rather than left to the firmware's defaults,
            # which publish it on some builds and not others.
            m.MAVLINK_MSG_ID_BATTERY_STATUS: 1_000_000,      # 1 Hz
        }

    def _interruptible_sleep(self, seconds: float) -> None:
        """Sleep that returns early when _stop_event is set.

        Sleeps in small slices via time.sleep so tests that monkeypatch
        ``corvus.mavlink_bridge.time.sleep`` still short-circuit the whole
        delay, while stop() can wake the daemon worker within one slice
        (~0.1 s). Used by the deferred interval worker and the version retry
        (BUG 1, 12).
        """
        slept = 0.0
        while slept < seconds:
            if self._stop_event.is_set():
                return
            chunk = min(0.1, seconds - slept)
            time.sleep(chunk)
            slept += chunk

    def _schedule_message_intervals(self) -> None:
        """Defer _request_message_intervals until the receive loop is running.

        MAV_CMD_SET_MESSAGE_INTERVAL is ACK-confirmed and the ACK is
        dispatched by _receive_loop(). Requesting intervals from _connect()
        (before the loop exists) made every ACK time out. This spawns a
        one-shot daemon that fires once the loop is draining the socket, so
        COMMAND_ACKs are actually delivered (BUG 1).
        """
        prior = self._intervals_thread
        if prior is not None and prior.is_alive():
            return

        def _runner() -> None:
            # Let the receive loop start draining the socket first.
            self._interruptible_sleep(0.05)
            if not self._running.is_set() or not self._conn:
                return
            if self._stop_event.is_set():
                return
            try:
                self._request_message_intervals()
            except Exception as exc:
                logger.debug("deferred message intervals failed: %s", exc)
            try:
                self._check_gcs_authority()
            except Exception as exc:
                logger.debug("GCS authority check failed: %s", exc)

        self._intervals_thread = threading.Thread(
            target=_runner, name="mavlink-intervals", daemon=True,
        )
        self._intervals_thread.start()

    # The parameter that decides whose sticks an ArduPilot vehicle listens to.
    # ArduPilot renamed it in 4.5; both are asked for and the first answer wins.
    _GCS_AUTHORITY_PARAMS: tuple[str, ...] = ("SYSID_MYGCS", "MAV_GCS_SYSID")

    def _check_gcs_authority(self) -> None:
        """Warn when this ArduPilot vehicle will ignore our stick input.

        ArduPilot accepts ``MANUAL_CONTROL`` and ``RC_CHANNELS_OVERRIDE`` only
        from the system id in ``SYSID_MYGCS`` (``MAV_GCS_SYSID`` from 4.5), and
        it *silently drops* everything else — there is no NAK, no STATUSTEXT and
        no ACK to miss. Corvus announces itself as system 254 rather than the
        conventional 255, deliberately, so that it can share a link with
        QGroundControl without the two fighting over one id. On a stock
        ArduPilot vehicle that means the joystick does nothing at all, and does
        it without a single sign that anything is wrong.

        So it is checked once per connection and reported with the fix in it.
        Corvus does not write the parameter itself: which ground station is
        allowed to take an aircraft's sticks is the operator's decision, not a
        thing to change behind their back.

        PX4 has no equivalent gate — it accepts MANUAL_CONTROL from any GCS —
        so this runs on an ArduPilot link only.
        """
        if self._dialect.stack != autopilot_dialect.STACK_ARDUPILOT:
            return
        if not self._connection_ready():
            return
        values = self.fetch_params(list(self._GCS_AUTHORITY_PARAMS), timeout=3.0)
        name = next((n for n in self._GCS_AUTHORITY_PARAMS if n in values), "")
        if not name:
            # An ArduPilot that answers for neither is old enough or odd enough
            # that guessing would be worse than saying nothing.
            return
        current = int(round(values[name]))
        if current == GCS_SYSTEM_ID:
            return
        text = (
            f"This vehicle only accepts stick input from ground station "
            f"{current} ({name}), and Corvus is {GCS_SYSTEM_ID}, so the joystick "
            f"will be ignored until {name} is set to {GCS_SYSTEM_ID} in "
            f"Parameters. Flight commands and mode changes are unaffected."
        )
        logger.info("%s", text)
        self._console_publish("JOYSTICK", text, "warning")
        self._store_update_warning(text, "warning")

    def _request_message_intervals(self) -> None:
        """Ask PX4 for per-message stream intervals (A5).

        Best-effort: a firmware that DENIES one message must not block the
        rest. Each call goes through _send_command_and_wait (ACK-confirmed).

        This is also where the legacy REQUEST_DATA_STREAM fallback is decided
        on UDP, because the ACKs are the only honest evidence of which
        mechanism the autopilot speaks — the firmware version cannot be used:
        AUTOPILOT_VERSION arrives later than this batch, and on the builds
        that answer the capabilities command with MAV_RESULT_UNSUPPORTED it
        may not arrive at all. So the three answers are read for what they
        each mean. MAV_RESULT_UNSUPPORTED is the firmware saying it has never
        heard of the command: that is an older ArduPilot, or something that is
        not PX4 at all, the fallback is the whole point, and the rest of the
        list would only spend another round trip each proving the same thing.
        (It is never an old PX4 — every PX4 release answers this command, and
        none of them answer the fallback.) A missing ACK is the link saying nothing
        at all — send the fallback once as insurance so the operator is not
        left with a connected link and an empty HUD, but keep going, since the
        next message may well be answered. Anything else — an accept, or a
        DENIED for one message this build does not carry — is proof the modern
        path works, and REQUEST_DATA_STREAM is then never sent.
        """
        silent = 0
        for msg_id, interval_us in self._message_intervals().items():
            try:
                _ok, result = self._set_message_interval(msg_id, interval_us)
            except Exception as exc:
                logger.debug("SET_MESSAGE_INTERVAL msg %d failed: %s", msg_id, exc)
                continue
            if result == -2:
                # Link torn down under us; the next connect starts over.
                return
            if result == mavutil.mavlink.MAV_RESULT_UNSUPPORTED:
                logger.info(
                    "SET_MESSAGE_INTERVAL unsupported (msg %d), falling back "
                    "to REQUEST_DATA_STREAM for this connection", msg_id,
                )
                self._request_streams()
                return
            if result == -1:
                self._request_streams()
                silent += 1
                # Each unanswered request costs two full ACK timeouts, and it
                # takes _operation_lock for both — so on a link that is not
                # answering at all, walking the whole list spent the better
                # part of a minute with every operator command queued behind a
                # batch that was already known to be going nowhere. A few in a
                # row is that answer; the fallback has been sent, and the
                # remaining messages would only prove the same thing again.
                if silent >= INTERVAL_REQUEST_SILENT_LIMIT:
                    logger.info(
                        "no COMMAND_ACK for %d interval requests, leaving the "
                        "rest to REQUEST_DATA_STREAM for this connection",
                        silent,
                    )
                    return
            else:
                silent = 0

    # MAVLINK_MSG_ID_AUTOPILOT_VERSION. Spelled out rather than taken from
    # mavutil so the request cannot silently follow a dialect rename.
    _MSG_ID_AUTOPILOT_VERSION = 148
    _MSG_ID_HOME_POSITION = 242

    def _request_home(self) -> None:
        """Ask the autopilot to send HOME_POSITION now.

        Home arrives on its own — PX4 publishes it at 0.5 Hz and again whenever
        it changes — so this is not what makes the marker appear. It is what
        makes it appear *promptly*, and the two moments where the wait is worst
        are the two that call it: straight after connect, where the map would
        otherwise hold an empty home for up to two seconds while the operator
        looks for the launch point, and straight after ``set_home``, where the
        operator has just clicked a spot and is waiting to see the marker move
        there.

        Fire-and-forget, both times. Whether it worked is judged by
        HOME_POSITION arriving, and the periodic stream is the backstop if it
        did not; waiting for an ACK here would stall the connect path (the
        receive loop is not dispatching yet) and would put a round trip in
        front of set_home's own reply.
        """
        conn = self._conn
        if conn is None:
            return
        nan = float("nan")
        try:
            with self._send_lock:
                if conn is not self._conn:
                    return
                conn.mav.command_long_send(
                    self._target_system, self._target_component,
                    mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE, 0,
                    float(self._MSG_ID_HOME_POSITION),
                    nan, nan, nan, nan, nan, nan,
                )
        except Exception as exc:  # noqa: BLE001 - a nudge that fails costs nothing
            logger.debug("HOME_POSITION request failed: %s", exc)

    def _request_version(self) -> None:
        """Ask the autopilot for AUTOPILOT_VERSION — the firmware version.

        Three requests, because no single one covers the supported firmware
        range. PX4 answers MAV_CMD_REQUEST_MESSAGE (v1.16-v1.18, and the only
        one v1.18 accepts); older builds answer the now-deprecated
        MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES; the bare
        AUTOPILOT_VERSION_REQUEST message is an ArduPilot-era legacy that PX4
        does not handle at all — which is why sending only that one left the
        firmware version blank on every supported target.

        Fire-and-forget: this runs inside _connect(), before the receive loop
        is dispatching COMMAND_ACKs, so waiting for an ACK here would stall the
        connect path. Whether it worked is judged by the answer arriving, and
        _schedule_version_retry() covers the case where it did not.
        """
        if self._conn:
            nan = float("nan")
            requests = (
                (mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE,
                 [float(self._MSG_ID_AUTOPILOT_VERSION), nan, nan, nan, nan, nan, nan]),
                (mavutil.mavlink.MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES,
                 [1.0, nan, nan, nan, nan, nan, nan]),
            )
            for command, params in requests:
                try:
                    with self._send_lock:
                        self._conn.mav.command_long_send(
                            self._target_system, self._target_component,
                            command, 0, *params,
                        )
                except Exception:
                    pass
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
        or spamming the link. Daemon thread; checks _running/_conn/_stop_event
        so it stays silent after shutdown, and the 4s wait uses
        _interruptible_sleep so stop() can wake it and join it promptly (BUG 12).
        """
        if not self._running.is_set():
            return
        prior = self._version_retry_thread
        if prior is not None and prior.is_alive():
            return

        def _retry() -> None:
            self._interruptible_sleep(4.0)
            if not self._running.is_set() or not self._conn:
                return
            if self._store.get_snapshot().get("px4_version"):
                return
            # Separate try blocks on purpose: one request failing must not
            # skip the others, which is exactly what a shared try would do.
            nan = float("nan")
            try:
                with self._send_lock:
                    if not self._running.is_set() or not self._conn:
                        return
                    self._conn.mav.command_long_send(
                        self._target_system, self._target_component,
                        mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE, 0,
                        float(self._MSG_ID_AUTOPILOT_VERSION),
                        nan, nan, nan, nan, nan, nan,
                    )
            except Exception:
                pass
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
        list/bytes/bytearray/str inputs (pymavlink yields a list of ints for
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

    def _heartbeat_timeout(self) -> float:
        """Stale threshold for _connection_ready() (command dispatch gate).

        Kept at the legacy 10s serial / 5s udp so a briefly-degraded link
        still rejects new operator commands. The receive loop uses the
        two-tier _warn_timeout() / _drop_timeout() instead (A3).
        """
        return 10.0 if self._is_serial() else HEARTBEAT_TIMEOUT_S

    def _warn_timeout(self) -> float:
        """Heartbeat-late threshold: link becomes 'degraded' (A3)."""
        return (
            HEARTBEAT_WARN_TIMEOUT_SERIAL if self._is_serial()
            else HEARTBEAT_WARN_TIMEOUT_UDP
        )

    def _drop_timeout(self) -> float:
        """Heartbeat-lost threshold: tear down and reconnect (A3)."""
        return (
            HEARTBEAT_DROP_TIMEOUT_SERIAL if self._is_serial()
            else HEARTBEAT_DROP_TIMEOUT_UDP
        )

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
                # Capture the raw frame before dispatch; a log write never
                # blocks the recv loop (TlogWriter.enqueue is non-blocking) and
                # never breaks the link (guarded).
                self._write_tlog(msg)
                self._dispatch(msg)
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

    _CONSOLE_MSGS = frozenset({"COMMAND_ACK"})

    def _dispatch(self, msg: Any) -> None:
        name = msg.get_type()

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
                    if pending is not None and msg.result in pending.interim:
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

        # Not in the dialect pymavlink loads, so it arrives without a source id
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
            self._hb_times.append(self._store.heartbeat())
            self._publish_heartbeat_jitter()
            vtype = MAV_TYPE_MAP.get(msg.type, f"TYPE_{msg.type}")
            autopilot = MAV_AUTOPILOT_MAP.get(msg.autopilot, f"AP_{msg.autopilot}")
            # Behind a router the aircraft on the far end can be swapped
            # without the socket closing, so the stack is re-read from every
            # heartbeat rather than trusted from connect time.
            self._latch_dialect(msg.autopilot, msg.type)
            armed = bool(msg.base_mode & mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
            mode = self._decode_mode(msg)
            self._store.update(
                vehicle_type=vtype, autopilot=autopilot, armed=armed, mode=mode,
                autopilot_stack=self._dialect.stack,
            )
            if armed and self._prearm_reasons:
                # It armed, so whatever held it back no longer does.
                self._clear_prearm_reasons()
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
        messages unit tests build, and pymavlink's own BAD_DATA — and is
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
        # Wake a blocked fly_to_points uploader so shutdown cannot hang.
        with self._mission_lock:
            self._mission_items = None
            pending = self._pending_mission_ack
            self._pending_mission_ack = None
        if pending is not None:
            pending.result = -2
            pending.event.set()

    def _connection_ready(self) -> bool:
        return bool(
            self._conn
            and self._store.get_snapshot().get("connected")
            and not self._store.is_stale(timeout=self._heartbeat_timeout())
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
        if (
            key not in self._statustext_chunks
            and len(self._statustext_chunks) >= STATUSTEXT_MAX_PARTIALS
        ):
            # Evict the least recently updated rather than refusing the new
            # one: the oldest partial is the one least likely to still be
            # completed, and dropping the newest would stall the live message.
            oldest = min(
                self._statustext_chunks,
                key=lambda k: float(self._statustext_chunks[k]["updated"]),
            )
            self._statustext_chunks.pop(oldest, None)
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
        # PX4 ends a STATUSTEXT that doubles one of its events with a tab (a
        # marker for stations that show the event instead). Corvus shows the
        # text, and without the strip the same message is two warnings.
        text = text.rstrip()
        level = "critical" if severity <= 3 else ("warning" if severity <= 5 else "info")
        self._console_publish("STATUSTEXT", text, level)
        self._store_update_warning(text, level)
        reason = self._dialect.prearm_failure(text)
        if reason:
            self._note_prearm_failure(reason)

    # ------------------------------------------------------------------
    # Why it will not arm
    # ------------------------------------------------------------------
    #
    # SYS_STATUS's prearm bit says whether the vehicle would arm, not why it
    # would not. The why is text: PX4 v1.16 to v1.18 send a "Preflight Fail:"
    # STATUSTEXT beside each arming-check event, ArduPilot a "PreArm:" one.
    # Decoding the events themselves needs the component metadata fetched over
    # MAVLink FTP; the text carries the same reasons without it. But PX4 sends
    # the report only when a result changes, on an arm attempt, or when asked,
    # so the station asks (MAV_CMD_RUN_PREARM_CHECKS) whenever the vehicle
    # says "not ready" and no reason has arrived.

    def _note_prearm_failure(self, reason: str) -> None:
        now = time.monotonic()
        if now - self._prearm_reason_at > PREARM_REPORT_GAP_S:
            self._prearm_reasons = []
        self._prearm_reason_at = now
        if reason in self._prearm_reasons or len(self._prearm_reasons) >= PREARM_REASONS_MAX:
            return
        self._prearm_reasons.append(reason)
        self._store.update(prearm_reasons=list(self._prearm_reasons))

    def _clear_prearm_reasons(self) -> None:
        self._prearm_reasons = []
        self._prearm_reason_at = 0.0
        self._store.update(prearm_reasons=[])

    def _note_prearm_state(self, prearm: bool | None) -> None:
        """React to SYS_STATUS's verdict: clear the reasons, or go and get them."""
        if prearm is True:
            if self._prearm_reasons:
                self._clear_prearm_reasons()
            return
        if prearm is not False or self._prearm_reasons:
            return
        now = time.monotonic()
        if now - self._prearm_requested_at < PREARM_REPORT_REQUEST_EVERY_S:
            return
        self._prearm_requested_at = now
        self._request_prearm_report()

    def _request_prearm_report(self) -> None:
        """Ask for the preflight report now. Fire and forget, like _request_home.

        Runs on the receive thread, so it cannot wait for its own ACK; whether
        it worked is judged by the reasons arriving.
        """
        conn = self._conn
        if conn is None:
            return
        nan = float("nan")
        try:
            with self._send_lock:
                if conn is not self._conn:
                    return
                conn.mav.command_long_send(
                    self._target_system, self._target_component,
                    mavutil.mavlink.MAV_CMD_RUN_PREARM_CHECKS, 0,
                    nan, nan, nan, nan, nan, nan, nan,
                )
        except Exception as exc:  # noqa: BLE001 - the next SYS_STATUS asks again
            logger.debug("prearm report request failed: %s", exc)

    def _store_update_warning(self, text: str, level: str) -> None:
        """Atomically merge a warning into the vehicle state."""
        self._store.merge_warning(text, level)

    def _console_publish(self, name: str, text: str, level: str) -> None:
        """Fan one console line out to every open console stream.

        Called on the MAVLink receive thread; the subscriber list is mutated
        by HTTP handler threads as tabs open and close. It was the only
        subscriber list in the backend without a lock — the state store, the
        param listeners and the SSH bridge all take one — and iterating it
        while another thread removed an entry silently skipped the subscriber
        that shifted into the vacated slot, so a closing tab could cost a
        *different* tab a STATUSTEXT. A copy taken under the lock, then
        published outside it, keeps the publish off the lock (subscribers are
        SSE buffers, and a slow one must not stall the receive thread).
        """
        entry = {"ts": time.time(), "name": name, "text": text, "level": level}
        with self._console_lock:
            subs = list(self._console_subs)
        for sub in subs:
            try:
                sub(entry)
            except Exception:
                pass

    def add_console_sub(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._console_lock:
            self._console_subs.append(fn)

    def remove_console_sub(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._console_lock:
            try:
                self._console_subs.remove(fn)
            except ValueError:
                pass

    def get_available_modes(self) -> list[str]:
        """The modes this vehicle can actually be commanded into.

        The live mapping's names when there is one, the dialect's own table
        when there is not, and an empty list for a stack neither dialect
        recognises — never another stack's mode list, which is what used to
        fill an ArduPilot selector with PX4 names.

        Either way in the dialect's order, not alphabetical: PX4's list runs
        from the most manual mode to the most automatic, which is how an
        operator reads a mode selector, and alphabetising it opened the
        selector with ACRO. A live name the dialect has no place for goes
        after the ones it knows, alphabetically.
        """
        ordered = self._dialect.available_modes(self._mav_type_id)
        if self._mode_mapping:
            known = [name for name in ordered if name in self._mode_mapping]
            return known + sorted(self._mode_mapping.difference(known))
        if self._modes_unsupported:
            return []
        return ordered

    @property
    def vehicle_type_id(self) -> int:
        """The MAV_TYPE from the vehicle's heartbeat, 0 before one arrives.

        The setup-page schemas need the number rather than the name: ArduPilot's
        flight-mode numbering and half its parameter set depend on which
        firmware family the airframe runs, and "QUADROTOR" does not sort into
        that on its own.
        """
        return self._mav_type_id

    @property
    def stack(self) -> str:
        """Which flight stack this link is talking to: see corvus.autopilot."""
        return self._dialect.stack

    def capabilities(self) -> dict[str, Any]:
        """What the connected stack can do, for the UI to adapt to.

        A button that is missing because the firmware has no such feature is a
        far better answer than one that is offered and refused, and only this
        layer knows which is which.
        """
        payload = self._dialect.capabilities(self._mav_type_id)
        payload["modes"] = self.get_available_modes()
        payload["vehicle_type"] = self._mav_type_id
        return payload

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
                    logger.warning("Command %d ACK timeout (attempt %d)", command, attempt + 1)
                return -1
            finally:
                with self._ack_lock:
                    if self._pending_acks.get(command) is pending:
                        self._pending_acks.pop(command, None)

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

    # ---- RTCM corrections ------------------------------------------------

    def inject_rtcm(self, frame: bytes) -> bool:
        """Put one RTCM 3 correction on the link as GPS_RTCM_DATA.

        Called from the RTK service's reader thread at a few hertz for as long
        as a base station is streaming. Three things about it are deliberate:

        **It does not take the operation lock.** Every other send path here
        serialises behind ``_operation_lock`` because it is a *transaction* —
        a command awaiting its ACK, a parameter awaiting its echo. A
        correction is neither: it is fire-and-forget, it arrives whether or
        not anything else is in progress, and queueing it behind a mission
        upload would stall the corrections for the length of the upload. Only
        ``_send_lock`` is held, which is the same guarantee the heartbeat loop
        works under.

        **It never raises.** The caller is a reader thread whose other job is
        the serial port; a throw here would end the RTK session over a link
        that had merely gone away for a moment. A refusal is counted and
        reported through :meth:`rtcm_stats` instead, which is what the page
        shows.

        **It drops rather than truncates.** A correction too long to express
        in four fragments (see :func:`corvus.rtk.fragments`) is discarded
        whole and counted, because a receiver that reassembles three quarters
        of a message does not get three quarters of a fix — it gets a CRC
        failure, having waited for the rest first.

        Returns True when every fragment was written.
        """
        if not frame:
            return False
        if not self._connection_ready():
            with self._rtcm_lock:
                self._rtcm_dropped += 1
            return False
        with self._rtcm_lock:
            sequence = self._rtcm_sequence
            self._rtcm_sequence = (sequence + 1) & 0x1F
        parts = rtk.fragments(frame, sequence)
        if not parts:
            with self._rtcm_lock:
                self._rtcm_dropped += 1
            logger.warning(
                "RTCM message of %d bytes is too long for GPS_RTCM_DATA; dropped",
                len(frame),
            )
            return False
        try:
            with self._send_lock:
                conn = self._conn
                if conn is None:
                    raise RuntimeError("link closed")
                for flags, chunk in parts:
                    # The field is a fixed 180 bytes; the length is carried
                    # separately, so the tail is padding and not data.
                    conn.mav.gps_rtcm_data_send(
                        flags, len(chunk),
                        list(chunk) + [0] * (rtk.FRAGMENT_BYTES - len(chunk)),
                    )
        except Exception as exc:  # noqa: BLE001 - see the docstring
            with self._rtcm_lock:
                self._rtcm_dropped += 1
            logger.debug("RTCM injection failed: %s", exc)
            return False
        with self._rtcm_lock:
            self._rtcm_bytes += len(frame)
            self._rtcm_messages += 1
            self._rtcm_last_send = time.monotonic()
        return True

    def rtcm_stats(self) -> dict[str, Any]:
        """What has actually reached the aircraft, for the RTK page.

        Separate from what the base *produced*, which the service counts: a
        base streaming happily into a link that is down is the failure this
        pair of numbers exists to make visible.
        """
        with self._rtcm_lock:
            last = self._rtcm_last_send
            return {
                "bytes": self._rtcm_bytes,
                "messages": self._rtcm_messages,
                "dropped": self._rtcm_dropped,
                "age": (time.monotonic() - last) if last else None,
            }

    def forget_rtcm_session(self) -> None:
        """Reset the injection counters when the correction source changes."""
        with self._rtcm_lock:
            self._rtcm_bytes = 0
            self._rtcm_messages = 0
            self._rtcm_dropped = 0
            self._rtcm_last_send = 0.0

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

    # How long to watch for the arm/disarm to show up in telemetry. Confirmation
    # arrives on HEARTBEAT, which is 1 Hz on every link Corvus supports, so the
    # old 3 s was three heartbeats — and on a lossy radio, sometimes one.
    ARMED_STATE_CONFIRM_S = 6.0

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

    # MAV_LANDED_STATE values that mean the aircraft is off the ground.
    _AIRBORNE_LANDED_STATES = frozenset({2, 3, 4})   # IN_AIR, TAKEOFF, LANDING
    # Height above home that counts as airborne when the firmware does not
    # publish EXTENDED_SYS_STATE. Clear of the noise on a stationary estimate.
    AIRBORNE_FALLBACK_M = 1.5

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

    # How long takeoff waits for an altitude reference to turn up before it
    # gives up on converting AGL to AMSL. HOME_POSITION streams at 0.5-1 Hz and
    # GLOBAL_POSITION_INT far faster, so this covers "the operator clicked
    # Takeoff a second after connecting" without putting a noticeable pause in
    # front of the command in the normal case, where the reference is already
    # there and this is never called.
    TAKEOFF_REFERENCE_WAIT_S = 2.0

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

    # How long a mode change is given to show up in telemetry before the
    # command that depends on it is sent anyway. HEARTBEAT is 1 Hz on every
    # link Corvus supports, so this is three of them on a lossy radio.
    MODE_CONFIRM_S = 3.0

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

    # ------------------------------------------------------------------
    # On-board log download (MAVLink LOG_* protocol)
    # ------------------------------------------------------------------

    def set_log_sink(self, sink: Callable[[Any], None] | None) -> None:
        """Route LOG_ENTRY / LOG_DATA to *sink* (None detaches)."""
        self._log_sink = sink

    def request_log_list(self, start: int = 0, end: int = 0xFFFF) -> bool:
        """Ask the vehicle to enumerate its on-board logs.

        Answered with a LOG_ENTRY per log, which the receive loop hands to the
        registered sink. Fire-and-forget: the protocol has no ACK, so
        completeness is judged from the entries themselves.
        """
        with self._operation_lock:
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_request_list_send(
                        self._target_system, self._target_component,
                        int(start), int(end),
                    )
            except Exception as exc:  # noqa: BLE001
                self._set_command_error(f"log list request failed: {exc}")
                return False
            return True

    def request_log_data(self, log_id: int, offset: int, count: int) -> bool:
        """Ask for one chunk of a log. Answered with LOG_DATA messages."""
        with self._operation_lock:
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_request_data_send(
                        self._target_system, self._target_component,
                        int(log_id), int(offset), int(count),
                    )
            except Exception as exc:  # noqa: BLE001
                self._set_command_error(f"log data request failed: {exc}")
                return False
            return True

    def erase_logs(self) -> bool:
        """Erase EVERY on-board log via MAVLink LOG_ERASE (121).

        There is no per-log delete in the MAVLink log protocol — LOG_ERASE
        clears the whole log directory — so this method is named for what it
        actually does rather than for what a caller might wish it did.

        Refused while armed: it is destructive, irreversible, and has no
        business happening with a vehicle that is live.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot erase logs while armed")
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_erase_send(
                        self._target_system, self._target_component,
                    )
            except Exception as exc:  # noqa: BLE001
                self._set_command_error(f"log erase failed: {exc}")
                return False
            self._console_publish("LOGS", "erasing all on-board logs", "warning")
            return True

    def log_request_end(self) -> bool:
        """Tell the vehicle the GCS is done reading logs.

        PX4 keeps the log session open until it hears this, which blocks
        logging of the next flight — so it is sent on every exit path, success
        or not.
        """
        with self._operation_lock:
            if not self._connection_ready():
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_request_end_send(
                        self._target_system, self._target_component,
                    )
            except Exception:  # noqa: BLE001 - best-effort teardown
                return False
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
        return self._set_message_interval(msg_id, interval_us)[0]

    def _set_message_interval(self, msg_id: int, interval_us: int) -> tuple[bool, int]:
        """The same request, with the raw COMMAND_ACK result beside the verdict.

        The connect-time batch needs the result code and not just the bool:
        it is what separates a firmware that has never heard of
        MAV_CMD_SET_MESSAGE_INTERVAL (MAV_RESULT_UNSUPPORTED) from one that
        refuses a single message it does not carry (MAV_RESULT_DENIED), and
        only the first is grounds for falling back to REQUEST_DATA_STREAM.
        -1 is no ACK at all, -2 no usable link.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                return False, -2
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL,
                [float(msg_id), float(interval_us), nan, nan, nan, nan, nan],
                timeout=3.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Set message interval", result), result
            self._console_publish(
                "STREAM",
                f"Message {msg_id} interval set to {interval_us} us", "success",
            )
            return True, result

    # ------------------------------------------------------------------
    # MANUAL_CONTROL — the on-screen virtual joystick
    # ------------------------------------------------------------------
    #
    # PX4 treats MANUAL_CONTROL exactly like a physical transmitter's stick
    # frame, so nothing here is a special GCS path: the axes are the RC axes
    # and the flight mode decides what they mean (Position holds a velocity
    # setpoint, Altitude an attitude one, Manual raw actuator). The joystick
    # is only ever an input source; it never changes mode, never arms, and
    # never overrides a failsafe. Whether the autopilot listens at all is the
    # vehicle's decision via ``COM_RC_IN_MODE`` — 1 (joystick only), 2 or 3
    # (either stick source) — which the GCS deliberately does not set on the
    # operator's behalf.
    #
    # No ACK exists for MANUAL_CONTROL and none is waited for: it is a
    # continuous stream whose next packet supersedes the last, so the sender
    # takes ``_send_lock`` alone rather than ``_operation_lock``. A 20 Hz
    # stick stream must never sit behind a parameter download.

    # Stick extents on the wire: MANUAL_CONTROL carries int16 axes scaled to
    # +/-1000, and z runs 0..1000 on PX4 (no negative thrust on a multicopter).
    MANUAL_AXIS_RANGE = 1000

    def manual_control(
        self,
        x: float = 0.0,
        y: float = 0.0,
        z: float = 0.0,
        r: float = 0.0,
        buttons: int = 0,
    ) -> bool:
        """Send one MANUAL_CONTROL frame; axes are normalized, not raw ticks.

        *x* is pitch (forward positive), *y* roll (right positive) and *r* yaw
        (clockwise positive), each in ``[-1, 1]``. *z* is thrust in ``[0, 1]``
        where 0.5 is the neutral hover detent PX4 expects from a spring-return
        stick; ArduPilot maps the same 0-1000 range onto the throttle channel's
        1000-2000 us travel, so 0.5 is mid-stick there too. Values outside
        those ranges are clamped rather than rejected — a stick that overshoots
        by a rounding error must still fly.

        Whether the vehicle *listens* is a separate question on ArduPilot,
        which drops stick input from any system id but ``SYSID_MYGCS``. That is
        checked once per connection and reported — see
        :meth:`_check_gcs_authority`.

        Returns ``True`` when the frame reached the wire. ``False`` means
        disconnected or a send failure; the caller decides whether one lost
        frame in a stream is worth surfacing.
        """
        self._set_command_error("")
        if not self._connection_ready():
            # Same wording as every other command failure so the HTTP layer's
            # "DISCONNECTED" -> 503 mapping holds for this path too.
            self._set_command_error(f"Manual control failed: {MAV_RESULT_TEXT[-2]}")
            return False

        def axis(value: Any, lo: float, hi: float) -> int:
            try:
                v = float(value)
            except (TypeError, ValueError):
                v = 0.0
            if v != v:      # NaN would become an arbitrary int16 on the wire
                v = 0.0
            return int(round(min(hi, max(lo, v)) * self.MANUAL_AXIS_RANGE))

        try:
            mask = int(buttons) & 0xFFFF if isinstance(buttons, int) and not isinstance(buttons, bool) else 0
            conn = self._conn
            with self._send_lock:
                if conn is None or conn is not self._conn:
                    self._set_command_error(
                        f"Manual control failed: {MAV_RESULT_TEXT[-2]}"
                    )
                    return False
                conn.mav.manual_control_send(
                    self._target_system,
                    axis(x, -1.0, 1.0),
                    axis(y, -1.0, 1.0),
                    axis(z, 0.0, 1.0),
                    axis(r, -1.0, 1.0),
                    mask,
                )
            return True
        except Exception as exc:
            logger.error("manual control send failed: %s", exc)
            self._set_command_error(f"Manual control send failed: {exc}")
            return False
