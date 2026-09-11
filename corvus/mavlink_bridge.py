"""MAVLink bridge — real autopilot communication via pymavlink.

Connects to a PX4 autopilot (SITL or real) over UDP/serial/TCP, parses
incoming messages, updates the Vehicle State Store, and forwards raw
message names to the console subscribers. Runs on a daemon thread.

Sends GCS heartbeats at 1 Hz so the autopilot recognizes us as a ground
control station and allows arming and mode changes.
"""
from __future__ import annotations

import codecs
import collections
import datetime
import errno
import logging
import math
import os
import random
import re
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Callable

from pymavlink import mavutil

from . import rc_config
from .paths import corvus_path
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
    7: "RTGS", 8: "FOLLOWME",
    9: "PRECLAND", 10: "VTOL_TAKEOFF",
    11: "EXTERNAL1", 12: "EXTERNAL2", 13: "EXTERNAL3",
    14: "EXTERNAL4", 15: "EXTERNAL5", 16: "EXTERNAL6",
    17: "EXTERNAL7", 18: "EXTERNAL8",
}

PX4_AVAILABLE_MODES: list[str] = [
    "MANUAL", "ALTCTL", "POSCTL", "STABILIZED", "ACRO", "RATTITUDE",
    "LOITER", "MISSION", "RTL", "LAND", "TAKEOFF",
    "OFFBOARD", "RTGS", "FOLLOWME",
]

# Built-in (base_mode, main_mode, sub_mode) tuples keyed by the fallback mode
# names, mirroring pymavlink's px4_map. Used when mode_mapping() returns empty
# so set_mode and get_available_modes stay consistent (BUG 9). PX4 sends these
# exact base_mode values (216 = MAV_MODE_FLAG_CUSTOM_MODE_ENABLED is the
# generic custom-mode flag; the per-mode base_mode below is what px4_map uses).
PX4_FALLBACK_MODE_VALUES: dict[str, tuple[int, int, int]] = {
    "MANUAL": (81, 1, 0), "ALTCTL": (81, 2, 0), "POSCTL": (81, 3, 0),
    "STABILIZED": (81, 7, 0), "ACRO": (65, 5, 0), "RATTITUDE": (65, 8, 0),
    "LOITER": (29, 4, 3), "MISSION": (29, 4, 4), "RTL": (29, 4, 5),
    "LAND": (29, 4, 6), "TAKEOFF": (29, 4, 2), "OFFBOARD": (29, 6, 0),
    "RTGS": (29, 4, 7), "FOLLOWME": (29, 4, 8),
}

TAKEOFF_ALTITUDE_MIN_M = 1.0
# 120 m, not 50. The old ceiling was an arbitrary round number that refused
# perfectly ordinary survey and inspection heights; 120 m AGL is the actual
# operational ceiling most crews fly to — it is the EU open-category limit and
# matches the equivalent rule in the UK, and is the same number PX4's own
# defaults are written around. A GCS should stop at the number the rules stop
# at, not at one somebody picked because it sounded cautious.
#
# The takeoff slider in src/index.html carries the same maximum; a test pins
# the two together so they cannot drift.
TAKEOFF_ALTITUDE_MAX_M = 120.0
HEARTBEAT_TIMEOUT_S = 5.0
STATUSTEXT_CHUNK_BYTES = 50
STATUSTEXT_CHUNK_TIMEOUT_S = 10.0
# Partial multi-chunk STATUSTEXTs held at once. They age out after
# STATUSTEXT_CHUNK_TIMEOUT_S, but nothing bounded how many could accumulate
# *inside* that window: one entry per (system, component, id), and a noisy
# router carries plenty of all three. A cap keeps a link that is misbehaving
# from growing the table without limit on the receive thread.
STATUSTEXT_MAX_PARTIALS = 64
# Linux 8250 platform serial ports (always phantom on field laptops).
_PHANTOM_TTY_RE = re.compile(r"^/dev/ttyS[0-9]+$")

# Firmware-Flash transport classification. A CDC ACM node is a DIRECT USB link
# to a Pixhawk-class flight controller (flashable); a /dev/ttyUSB* node is a
# USB-to-serial adapter (SiK radio, NOT flashable). _BYID_TOKENS are the
# Pixhawk/STM32 vendor identifiers that appear in /dev/serial/by-id/ names and
# pyserial hwid strings (26AC = Pixhawk vendor ID, 0483 = STMicro STM32).
_DIRECT_USB_ACM_RE = re.compile(r"^/dev/ttyACM[0-9]+$")
_SIK_RADIO_RE = re.compile(r"^/dev/ttyUSB[0-9]+$")
_BYID_TOKENS: tuple[str, ...] = (
    "px4", "pixhawk", "fmu",
    "3d_robotics", "3d robotics",
    "hex_proficnc", "arducy", "mro",
    "hex ",
    "usb vid:pid=26ac", "usb vid:pid=0483",
)
_PIXHAWK_BYID_RE = re.compile(
    "|".join(re.escape(t) for t in _BYID_TOKENS), re.IGNORECASE,
)

# Windows names every serial port the same way, so the port name carries no
# information at all: COM7 is a Pixhawk, a SiK radio or a Bluetooth pairing
# with equal probability. The identity is in the USB descriptor pyserial
# reports as ``hwid`` instead, which is why the classification below looks the
# port up rather than reading its name.
_WINDOWS_COM_RE = re.compile(r"^(?:\\\\\.\\)?COM[0-9]+$", re.IGNORECASE)
# The USB-to-serial bridges a SiK Telemetry Radio (and its clones) is built on.
# These are adapters: whatever is behind them, it is not a flashable FMU.
_USB_SERIAL_BRIDGE_TOKENS: tuple[str, ...] = (
    "vid:pid=0403",   # FTDI FT232 — Holybro/3DR SiK V2 and V3
    "vid:pid=10c4",   # Silicon Labs CP210x — the common SiK clone bridge
    "vid:pid=1a86",   # WCH CH340/CH341
    "vid:pid=067b",   # Prolific PL2303
)
_USB_SERIAL_BRIDGE_RE = re.compile(
    "|".join(re.escape(t) for t in _USB_SERIAL_BRIDGE_TOKENS), re.IGNORECASE,
)


def is_windows_com_port(device: str) -> bool:
    """Is *device* a Windows COM port name (``COM7``, ``\\\\.\\COM12``)?

    Matched by shape rather than by ``os.name`` so the classification can be
    exercised from a test on any host — and so a COM name typed into the
    connection field on Linux is still recognised for what it is.
    """
    return bool(_WINDOWS_COM_RE.match(str(device or "").strip()))

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

# SiK radio RSSI is a 0-255 relative scale; ~150 strong, ~40 weak. Used to
# map RADIO_STATUS.remrssi (the drone's signal as seen by the radio) to 0-100.
_SIK_RSSI_STRONG = 150.0
_SIK_RSSI_WEAK = 40.0

# The NSH shell reassembles 70-byte chunks into lines, so a "line" is only
# bounded by the vehicle sending a newline. `dd`-ing a binary to the console,
# or a firmware that streams without one, would otherwise grow the buffer for
# as long as the shell stays open — on the receive thread. At the cap the
# buffer is flushed as a line of its own, which keeps the output visible
# instead of silently discarding it.
SHELL_LINE_MAX_CHARS = 8192

# A distinct GCS system id keeps Corvus COMMAND_ACK and SERIAL_CONTROL replies
# separate from QGroundControl (which normally uses system 255) when both share
# a vehicle through mavlink-router or the built-in raw-frame forwarder.
GCS_SYSTEM_ID = 254
GCS_COMPONENT_ID = mavutil.mavlink.MAV_COMP_ID_MISSIONPLANNER

# How long _connect waits for the aircraft to introduce itself.
HEARTBEAT_WAIT_S = 10.0


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

# Lossy-link parameter-download recovery (BUG 5). After the PARAM_VALUE burst
# settles, re-request missing indices (bounded rounds), then give up to a
# terminal "incomplete" state so the editor is never stuck at complete:false.
PARAM_DOWNLOAD_INACTIVITY_S = 1.0
PARAM_DOWNLOAD_MAX_ROUNDS = 3
PARAM_DOWNLOAD_TIMEOUT_S = 30.0
PARAM_WATCHDOG_TICK_S = 0.5


# Flight readiness, straight from the autopilot. PX4 mirrors its preflight
# checks into the MAV_SYS_STATUS_PREARM_CHECK bit of SYS_STATUS: present/enabled
# say the firmware publishes the check at all, health says whether an arm
# command would be accepted right now. Firmware that never sets the bit (some
# ArduPilot builds, older PX4) yields None — "unknown", never a green light the
# vehicle did not give.
_PREARM_BIT = mavutil.mavlink.MAV_SYS_STATUS_PREARM_CHECK


def _prearm_ok(msg) -> bool | None:
    """SYS_STATUS -> True (ready to arm) / False (refused) / None (not reported)."""
    enabled = getattr(msg, "onboard_control_sensors_enabled", 0) or 0
    present = getattr(msg, "onboard_control_sensors_present", 0) or 0
    if not ((enabled | present) & _PREARM_BIT):
        return None
    health = getattr(msg, "onboard_control_sensors_health", 0) or 0
    return bool(health & _PREARM_BIT)


def _rc_rssi_percent(value: Any) -> int:
    """RC_CHANNELS.rssi -> 0-100, or -1 when the receiver does not report it.

    MAVLink reserves 255 for "unknown". Below that the field is ambiguous in
    practice: PX4 publishes a percentage, while several receivers publish the
    raw 0-254 scale the message was originally specified with. A value above
    100 can only be the second, so it is scaled; anything at or below 100 is
    already the percentage it claims to be. Guessing either way beats showing a
    254 % link.
    """
    try:
        rssi = int(value)
    except (TypeError, ValueError):
        return -1
    if rssi < 0 or rssi >= 255:
        return -1
    if rssi <= 100:
        return rssi
    return max(0, min(100, round(rssi * 100 / 254)))


def _quaternion_to_euler_deg(w: float, x: float, y: float, z: float) -> tuple[float, float, float]:
    """Convert a MAVLink attitude quaternion to roll/pitch/yaw in degrees.

    Same convention as the ATTITUDE message this is plotted against (NED body
    frame, roll about the nose axis) so a setpoint trace and a response trace
    share one scale. Pitch is clamped before ``asin`` because a quaternion that
    arrives fractionally un-normalised over a lossy link would otherwise raise
    on a value a hair outside [-1, 1].
    """
    sinr_cosp = 2.0 * (w * x + y * z)
    cosr_cosp = 1.0 - 2.0 * (x * x + y * y)
    roll = math.atan2(sinr_cosp, cosr_cosp)
    sinp = max(-1.0, min(1.0, 2.0 * (w * y - z * x)))
    pitch = math.asin(sinp)
    siny_cosp = 2.0 * (w * z + x * y)
    cosy_cosp = 1.0 - 2.0 * (y * y + z * z)
    yaw = math.atan2(siny_cosp, cosy_cosp)
    return math.degrees(roll), math.degrees(pitch), math.degrees(yaw)


@dataclass
class _PendingAck:
    event: threading.Event = field(default_factory=threading.Event)
    result: int | None = None
    accept_in_progress: bool = False


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
        self._console_subs: list[Callable[[dict[str, Any]], None]] = []
        # Guards _console_subs: appended/removed by HTTP handler threads,
        # iterated by the receive thread on every STATUSTEXT.
        self._console_lock = threading.Lock()
        self._target_system: int = 1
        self._target_component: int = 1
        self._request_sent = False
        self._mode_mapping: set[str] = set()
        self._mode_values: dict[str, Any] = {}
        self._pending_acks: dict[int, _PendingAck] = {}
        self._ack_lock = threading.Lock()
        self._operation_lock = threading.RLock()
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
        # Heartbeat inter-arrival timestamps for jitter (A1); rolling window.
        self._hb_times: collections.deque[float] = collections.deque(maxlen=30)
        # Per-connection-cycle flags (A3): warn-once guard and radio presence.
        self._degraded_warned: bool = False
        self._radio_status_seen: bool = False
        # Last computed uplink score; kept so heartbeat-only links can leave
        # it at 0 and link_quality can fall back to heartbeat freshness.
        self._uplink_score: int = 0
        self._params: dict[str, ParamEntry] = {}
        self._param_count: int = -1
        self._param_received: int = 0
        self._param_download_state: str = "idle"
        self._param_seen_indices: set[int] = set()
        # Lossy-link download recovery (BUG 5): the watchdog re-requests lost
        # PARAM_VALUEs once the burst settles, then flips to a terminal
        # "incomplete" so the editor never waits forever at complete:false.
        self._param_download_started_at: float = 0.0
        self._param_retransmit_round: int = 0
        self._param_last_value_at: float = 0.0
        self._param_watchdog_thread: threading.Thread | None = None
        self._param_lock = threading.Lock()
        self._param_listeners: list[Callable[[dict[str, Any]], None]] = []
        self._param_set_pending: dict[str, _PendingAck] = {}
        # Background batch-upload state. The worker thread owns the loop; the
        # fields are touched under _param_lock so the SSE/HTTP readers see a
        # consistent snapshot. _param_upload_failed is reset per start_param_upload
        # and copied out into _param_upload_result at completion.
        self._param_upload_thread: threading.Thread | None = None
        self._param_upload_failed: list[dict[str, Any]] = []
        self._param_upload_result: dict[str, Any] = {}
        # Mission-upload handshake state (set only during fly_to_points). The
        # receive thread reads _mission_items on MISSION_REQUEST_INT and wakes
        # _pending_mission_ack on MISSION_ACK; _mission_lock guards both.
        self._mission_lock = threading.Lock()
        self._mission_items: list[dict[str, Any]] | None = None
        self._pending_mission_ack: _PendingAck | None = None
        # NSH debug-shell state and UTF-8-safe line reassembly.
        self._shell_lock = threading.RLock()
        self._shell_buffer: str = ""
        self._shell_decoder: Any = codecs.getincrementaldecoder("utf-8")(
            errors="replace",
        )
        self._shell_active: bool = False

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

        On Windows the device name is COM<n> and says nothing about what is on
        the other end, so the answer comes from the port's USB descriptor
        instead — see :meth:`_classify_com_port`.
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
            if is_windows_com_port(device):
                return self._classify_com_port(device)
            return "unknown"
        return "unknown"

    def _classify_com_port(self, device: str) -> str:
        """'usb' / 'sik' / 'unknown' for a Windows COM port, from its descriptor.

        A COM name is just an index the OS handed out, so unlike a POSIX device
        path it cannot be classified by inspection. pyserial reports the USB
        vendor/product ids in ``hwid``, and those do separate the two cases
        that matter here: a Pixhawk-class FMU presenting its own CDC interface,
        versus an FTDI/CP210x/CH340 bridge with a radio behind it.

        Unknown wins ties. Getting this wrong in the permissive direction would
        offer to flash firmware down a telemetry radio, and the operator finds
        out at the point the autopilot stops answering.
        """
        hwid = ""
        for port in self.list_serial_ports():
            if str(port.get("device", "")).strip().lower() == device.strip().lower():
                hwid = f"{port.get('hwid', '')} {port.get('description', '')}"
                break
        if not hwid.strip():
            return "unknown"
        # Bridge first: an FTDI descriptor may also carry a product string with
        # a vendor name in it, and a bridge is never the flight controller.
        if _USB_SERIAL_BRIDGE_RE.search(hwid):
            return "sik"
        if _PIXHAWK_BYID_RE.search(hwid):
            return "usb"
        return "unknown"

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
        # Wake any pending set_param waiters and mark download idle.
        with self._param_lock:
            self._param_download_state = "idle"
            for pending in self._param_set_pending.values():
                pending.result = -2
                pending.event.set()
            self._param_set_pending.clear()
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
                f"{device} is in use by another program — close the other "
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

    def _connect(self) -> None:
        logger.info("Connecting to %s …", self._conn_str)
        self._reset_shell_state()
        self._request_sent = False
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
                    f"(QGroundControl, MAVROS/MAVSDK or a second Corvus) — "
                    f"close it or connect on a different port"
                )
                self._store.update(link_error=message)
                raise ConnectionError(message) from exc
            raise
        if self._is_serial():
            self._claim_serial_exclusive(self._parse_serial(self._conn_str)[0])
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
        # REQUEST_DATA_STREAM first as a fallback for v1.12-v1.15, then the
        # modern per-message SET_MESSAGE_INTERVAL (A5) for v1.16-v1.18. The
        # ACK-confirmed interval requests are deferred to _run() so the
        # receive loop exists to dispatch their COMMAND_ACKs (BUG 1).
        self._request_streams()
        self._request_version()
        self._request_home()
        self._start_gcs_heartbeat()
        # Start a fresh tlog for this flight session. Gated on _running so the
        # direct-_connect() unit tests (which never start() the bridge) do not
        # open files or spawn writer threads in ~/.corvus/logs; in production
        # _run only calls _connect while _running is set.
        self._start_tlog()

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
        # Fallback (BUG 9): no live mode_mapping — populate _mode_values from
        # the built-in px4_map so set_mode and get_available_modes stay
        # consistent (the fallback list and the value table share the same
        # names, including RTGS and FOLLOWME).
        self._mode_mapping = set()
        self._mode_values = dict(PX4_FALLBACK_MODE_VALUES)

    def _start_gcs_heartbeat(self) -> None:
        if self._hb_thread and self._hb_thread.is_alive():
            return
        self._hb_thread = threading.Thread(target=self._gcs_hb_loop, name="gcs-hb", daemon=True)
        self._hb_thread.start()

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
            try:
                with self._send_lock:
                    # Re-check under the lock: stop()/reconnect can clear
                    # _conn or _running while we waited for the lock.
                    if not self._running.is_set() or conn is not self._conn:
                        break
                    conn.mav.heartbeat_send(
                        mavutil.mavlink.MAV_TYPE_GCS,
                        mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                        0, 0, 0,
                    )
            except Exception:
                # Socket closed under us: exit cleanly so _start_gcs_heartbeat
                # can spawn a fresh loop on reconnect (A6).
                break
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

        Modern PX4 (v1.16-v1.18) rate-control; REQUEST_DATA_STREAM (above) is
        the fallback for v1.12-v1.15. Serial links are throttled to fit a
        57 kbps SiK radio. VIBRATION is on-demand only (not listed here).
        """
        m = mavutil.mavlink
        if self._is_serial():
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

        self._intervals_thread = threading.Thread(
            target=_runner, name="mavlink-intervals", daemon=True,
        )
        self._intervals_thread.start()

    def _request_message_intervals(self) -> None:
        """Ask PX4 for per-message stream intervals (A5).

        Best-effort: a firmware that DENIES one message must not block the
        rest. Each call goes through _send_command_and_wait (ACK-confirmed).
        """
        for msg_id, interval_us in self._message_intervals().items():
            try:
                self.set_message_interval(msg_id, interval_us)
            except Exception as exc:
                logger.debug("SET_MESSAGE_INTERVAL msg %d failed: %s", msg_id, exc)

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
                    if pending and (
                        msg.result != mavutil.mavlink.MAV_RESULT_IN_PROGRESS
                        or pending.accept_in_progress
                    ):
                        pending.result = msg.result
                        pending.event.set()

        if name in ("MISSION_REQUEST_INT", "MISSION_REQUEST"):
            self._handle_mission_request(msg, as_int=(name == "MISSION_REQUEST_INT"))
        elif name == "MISSION_ACK":
            self._handle_mission_ack(msg)

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
            armed = bool(msg.base_mode & mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
            mode = self._decode_mode(msg)
            self._store.update(vehicle_type=vtype, autopilot=autopilot, armed=armed, mode=mode)
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
            self._store.update(gps_fix=fix, gps_satellites=msg.satellites_visible, gps_hdop=round(hdop, 1))
        elif name == "SYS_STATUS":
            voltage = msg.voltage_battery / 1000.0 if msg.voltage_battery != 65535 else 0
            current = msg.current_battery / 100.0 if msg.current_battery != -1 else 0
            self._store.update(
                battery_voltage=round(voltage, 1),
                battery_current=round(current, 1),
                battery_percent=msg.battery_remaining if msg.battery_remaining != -1 else 0,
                prearm_ok=_prearm_ok(msg),
            )
        elif name == "SYSTEM_TIME":
            # utcfromtimestamp is deprecated (and gone in a coming release);
            # more to the point it raises on an out-of-range value, and an
            # autopilot that has no GPS lock yet sends exactly that — a huge
            # or nonsensical time_unix_usec — which used to abort the whole
            # dispatch for this frame, taking the boot_ms read below with it.
            if msg.time_unix_usec:
                try:
                    t = datetime.datetime.fromtimestamp(
                        msg.time_unix_usec / 1e6, datetime.timezone.utc,
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
                self._store.update(boot_ms=int(boot_ms))
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
            # Kept on the bridge rather than in the telemetry state: these two
            # identify the board for the firmware picker and are read once, not
            # pushed to the browser on every frame.
            try:
                self._board_vendor_id = int(getattr(msg, "vendor_id", 0) or 0)
                self._board_product_id = int(getattr(msg, "product_id", 0) or 0)
            except (TypeError, ValueError):
                pass
        elif name in ("LOG_ENTRY", "LOG_DATA"):
            # Routed to the log service rather than handled here: the download
            # is a stateful protocol, and the receive loop must not own it.
            sink = self._log_sink
            if sink is not None:
                try:
                    sink(msg)
                except Exception:  # noqa: BLE001 - a sink must never kill the loop
                    logger.debug("log sink raised", exc_info=True)
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
        elif name == "HOME_POSITION":
            lat = msg.latitude / 1e7
            lon = msg.longitude / 1e7
            self._home_alt_amsl = msg.altitude / 1000.0
            self._store.update(home=[lon, lat])

    # ------------------------------------------------------------------
    # Controller setpoints (PID tuning)
    # ------------------------------------------------------------------

    def _handle_attitude_target(self, msg: Any) -> None:
        """Publish the attitude and body-rate setpoints the controller is following.

        ATTITUDE_TARGET carries the attitude as a quaternion and the body rates
        as rad/s, and it is the counterpart of ATTITUDE: together they are the
        commanded-versus-achieved pair the PID tuning page plots. Streamed at
        PX4's default rate normally, and raised while the tuning page is open
        (see :meth:`set_tuning_stream`).

        A malformed or short quaternion is dropped rather than guessed at: an
        invented setpoint on a tuning graph is worse than a missing one.
        """
        try:
            quat = list(getattr(msg, "q", None) or [])
            rates = (
                math.degrees(float(msg.body_roll_rate)),
                math.degrees(float(msg.body_pitch_rate)),
                math.degrees(float(msg.body_yaw_rate)),
            )
        except (TypeError, ValueError, AttributeError):
            return
        update: dict[str, Any] = {
            "rollspeed_sp": round(rates[0], 1),
            "pitchspeed_sp": round(rates[1], 1),
            "yawspeed_sp": round(rates[2], 1),
            "setpoints_live": True,
        }
        if len(quat) >= 4:
            try:
                roll, pitch, yaw = _quaternion_to_euler_deg(
                    float(quat[0]), float(quat[1]), float(quat[2]), float(quat[3]))
            except (TypeError, ValueError):
                pass
            else:
                update["roll_sp"] = round(roll, 1)
                update["pitch_sp"] = round(pitch, 1)
                update["yaw_sp"] = round(yaw, 1)
        self._store.update(**update)

    def _handle_position_target(self, msg: Any) -> None:
        """Publish the velocity setpoint the position controller is following.

        POSITION_TARGET_LOCAL_NED carries the whole setpoint triplet; only the
        velocity part is kept, because that is the one the velocity-controller
        gains are read against. The type mask is not consulted: PX4 fills the
        velocity fields with the controller's own setpoint regardless of which
        of them the active mode considers authoritative, and a masked field
        reads as the zero the controller is in fact tracking.
        """
        try:
            self._store.update(
                vx_sp=round(float(msg.vx), 2),
                vy_sp=round(float(msg.vy), 2),
                vz_sp=round(float(msg.vz), 2),
                setpoints_live=True,
            )
        except (TypeError, ValueError, AttributeError):
            return

    # ------------------------------------------------------------------
    # Transmitter channels (Setup -> Radio Control)
    # ------------------------------------------------------------------

    def _handle_rc_channels(self, msg: Any, raw: bool = False) -> None:
        """Publish the raw transmitter channel values, in microseconds.

        The Radio Control page is built on these: the calibration wizard has no
        autopilot-side procedure to follow (PX4 has none for RC), so what the
        operator sweeps on the transmitter reaches the browser through this
        message and nothing else.

        ``RC_CHANNELS`` carries all 18 channels and is what PX4 sends.
        ``RC_CHANNELS_RAW`` carries eight at a time with ``port`` naming the
        bank, and is accepted as a fallback so a non-PX4 autopilot on the same
        link is not silently channel-less; its banks are merged into the
        published list rather than replacing it.

        A channel value of ``UINT16_MAX`` is MAVLink's "not delivered", which is
        published as 0 — the page draws that as an empty channel instead of a
        bar pinned to the top of its range.
        """
        try:
            offset = (int(getattr(msg, "port", 0) or 0) * 8) if raw else 0
            width = 8 if raw else rc_config.MAX_CHANNELS
        except (TypeError, ValueError):
            return
        if offset < 0 or offset >= rc_config.MAX_CHANNELS:
            return

        values: list[int] = []
        for index in range(1, width + 1):
            value = getattr(msg, f"chan{index}_raw", None)
            if value is None:
                break
            try:
                pulse = int(value)
            except (TypeError, ValueError):
                pulse = 0
            values.append(0 if pulse == 65535 else max(0, min(pulse, 65534)))

        if raw:
            # Merge the bank into whatever the other bank already published, so
            # a receiver split across two messages does not flip between eight
            # channels and the other eight on every frame.
            # Clamp the bank to the channels PX4 defines before merging: the
            # offset guard above admits port=2 (channels 17-24), and without
            # this the merged list grows past MAX_CHANNELS and the page draws
            # channels no firmware here has. The RC_CHANNELS path below clamps
            # for the same reason.
            room = rc_config.MAX_CHANNELS - offset
            if room <= 0:
                return
            values = values[:room]
            if not values:
                return
            existing = list(self._store.get_snapshot().get("rc_channels") or [])
            needed = offset + len(values)
            if len(existing) < needed:
                existing.extend([0] * (needed - len(existing)))
            existing[offset:offset + len(values)] = values
            values = existing
            count = len(values)
        else:
            try:
                count = int(getattr(msg, "chancount", 0) or 0)
            except (TypeError, ValueError):
                count = 0
            # chancount is what the receiver actually delivers; trust it over
            # the fixed 18 slots, but never let a bad value grow the list.
            if 0 < count <= len(values):
                values = values[:count]
            else:
                count = len(values)

        self._store.update(
            rc_channels=values,
            rc_channel_count=count,
            rc_rssi=_rc_rssi_percent(getattr(msg, "rssi", 255)),
            rc_live=True,
        )

    # ------------------------------------------------------------------
    # Link-quality tracking (A1)
    # ------------------------------------------------------------------

    def _handle_radio_status(self, msg: Any) -> None:
        """Parse RADIO_STATUS into uplink score and radio metrics (A1).

        SiK radios report RSSI on a 0-255 relative scale; remrssi is the
        drone's signal as seen by the ground radio (the uplink quality
        proxy). txbuf near 100 means the buffer is full (congested).
        rxerrors/fixed are cumulative counters from the radio — stored as-is.
        """
        # Full RADIO_STATUS fields; rssi/noise/remnoise are parsed for
        # completeness and future use (the uplink score uses remrssi + txbuf).
        _rssi = float(getattr(msg, "rssi", 0))
        remrssi = float(getattr(msg, "remrssi", 0))
        txbuf = float(getattr(msg, "txbuf", 0))
        _noise = float(getattr(msg, "noise", 0))
        _remnoise = float(getattr(msg, "remnoise", 0))
        rxerrors = int(getattr(msg, "rxerrors", 0))
        fixed = int(getattr(msg, "fixed", 0))
        # 0/255 remrssi on SiK means no remote signal reading → unknown
        # uplink: keep the last score (avoids a momentary 0 flapping the link
        # to "poor" while the radio resyncs). Real signal loss is caught by
        # the heartbeat two-tier path (A3).
        self._radio_status_seen = True
        if remrssi not in (0.0, 255.0):
            remrssi_pct = max(
                0.0, min(100.0, (remrssi - _SIK_RSSI_WEAK)
                         / (_SIK_RSSI_STRONG - _SIK_RSSI_WEAK) * 100.0)
            )
            txbuf_pct = max(0.0, min(100.0, txbuf))
            score = round(0.7 * remrssi_pct + 0.3 * txbuf_pct)
            self._uplink_score = max(0, min(100, score))
        self._store.update(
            uplink=self._uplink_score,
            uplink_rssi=remrssi,
            uplink_rxerrors=rxerrors,
            uplink_fixed=fixed,
        )
        self._publish_link_quality()

    def _publish_heartbeat_jitter(self) -> None:
        """Publish rolling stddev of heartbeat inter-arrival in ms (A1).

        Needs at least two timestamps to compute one interval; below that
        the link is too fresh to characterize and jitter stays 0.
        """
        times = list(self._hb_times)
        if len(times) < 2:
            self._store.update(heartbeat_jitter_ms=0.0)
            return
        intervals_ms = [
            (times[i] - times[i - 1]) * 1000.0 for i in range(1, len(times))
        ]
        mean = sum(intervals_ms) / len(intervals_ms)
        var = sum((v - mean) ** 2 for v in intervals_ms) / len(intervals_ms)
        self._store.update(heartbeat_jitter_ms=round(math.sqrt(var), 1))

    def _current_jitter_ms(self) -> float:
        """Latest published heartbeat jitter (0 before two heartbeats)."""
        times = list(self._hb_times)
        if len(times) < 2:
            return 0.0
        intervals_ms = [
            (times[i] - times[i - 1]) * 1000.0 for i in range(1, len(times))
        ]
        mean = sum(intervals_ms) / len(intervals_ms)
        var = sum((v - mean) ** 2 for v in intervals_ms) / len(intervals_ms)
        return round(math.sqrt(var), 1)

    def _publish_link_quality(self) -> None:
        """Derive the link_quality string from uplink + jitter (A1).

        With RADIO_STATUS: good/fair/poor from uplink score + jitter. Without
        RADIO_STATUS (UDP SITL): heartbeat-freshness-driven — good when fresh,
        left to A3 to mark poor/lost on staleness. 'lost' is owned by A3's drop
        path, not here.
        """
        jitter = self._current_jitter_ms()
        if not self._radio_status_seen:
            # Heartbeat-driven: a fresh heartbeat just arrived (we're in the
            # HEARTBEAT branch) → good. A3 flips to poor/lost on staleness.
            self._store.update(link_quality="good")
            return
        uplink = self._uplink_score
        if uplink >= 70 and jitter < 50.0:
            quality = "good"
        elif uplink >= 40:
            quality = "fair"
        else:
            quality = "poor"
        self._store.update(link_quality=quality)

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
        level = "critical" if severity <= 3 else ("warning" if severity <= 5 else "info")
        self._console_publish("STATUSTEXT", text, level)
        self._store_update_warning(text, level)

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
        accept_in_progress: bool = False,
    ) -> int:
        """Send COMMAND_LONG and wait for COMMAND_ACK. Returns result int (-1 = no ack)."""
        with self._operation_lock:
            if not self._connection_ready():
                return -2
            # Snapshot the connection (A6): if a concurrent stop()/reconnect
            # swaps self._conn, bail with -2 instead of sending on a closed
            # socket.
            conn = self._conn
            p = list(params or [])[:7]
            p.extend([float("nan")] * (7 - len(p)))
            pending = _PendingAck(accept_in_progress=accept_in_progress)
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
                    f"confirmation within {self.ARMED_STATE_CONFIRM_S:.0f}s — "
                    f"check the armed indicator"
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
            nan = float("nan")
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
                    "position) — asking the vehicle to take off to its own "
                    f"configured altitude instead of {altitude_agl:.0f} m"
                )
                logger.warning("%s", text)
                self._console_publish("TAKEOFF", text, "warning")
                self._store_update_warning(text, "warning")
            else:
                takeoff_alt = alt_amsl
                self._console_publish(
                    "TAKEOFF",
                    f"Requesting {altitude_agl:.0f} m AGL ({alt_amsl:.1f} m AMSL)",
                    "info",
                )
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
                [nan, nan, nan, nan, nan, nan, takeoff_alt],
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
        streams until requested. Returns False when disconnected or another
        parameter operation is in progress.
        """
        with self._operation_lock:
            if not self._connection_ready():
                # Surface a fresh error rather than a stale thread-local one
                # (BUG 11).
                self._set_command_error("not connected")
                return False
            with self._param_lock:
                # Guard against a concurrent upload/download (BUG 4): the upload
                # worker reads self._params for MAV_PARAM_TYPE, so clobbering it
                # here would corrupt an in-flight upload.
                if self._param_download_state in ("downloading", "uploading"):
                    self._set_command_error("another parameter operation in progress")
                    return False
                self._param_download_state = "downloading"
                self._params.clear()
                self._param_received = 0
                self._param_count = -1
                self._param_seen_indices = set()
                # Lossy-link recovery bookkeeping (BUG 5).
                now = time.monotonic()
                self._param_download_started_at = now
                self._param_last_value_at = now
                self._param_retransmit_round = 0
            conn = self._conn
            try:
                with self._send_lock:
                    if conn is None or conn is not self._conn:
                        raise ConnectionError("link closed")
                    conn.mav.param_request_list_send(
                        self._target_system, self._target_component,
                    )
            except Exception as exc:  # noqa: BLE001 - a torn-down link is not a 500
                # Roll the state machine back: leaving it at "downloading" with
                # nothing on the wire would block every later parameter
                # operation behind an "in progress" that never progresses.
                with self._param_lock:
                    if self._param_download_state == "downloading":
                        self._param_download_state = "idle"
                self._set_command_error(f"parameter list request failed: {exc}")
                return False
            self._start_param_watchdog()
            return True

    # PARAM_VALUE / PARAM_SET carry the id in a fixed char[16] with no
    # terminator when it is exactly full, so anything longer is not a
    # parameter this vehicle could ever have — and pymavlink packs it by
    # raising rather than truncating.
    PARAM_ID_MAX_BYTES = 16

    @classmethod
    def _encode_param_name(cls, name: Any) -> bytes | None:
        """Encode *name* for the wire, or None when it cannot be one.

        Rejects instead of truncating: a silently shortened name addresses a
        *different* parameter, which on a parameter write is the difference
        between setting a rate gain and setting something else entirely.
        """
        if not isinstance(name, str) or not name:
            return None
        try:
            encoded = name.encode("ascii")
        except UnicodeEncodeError:
            return None
        if len(encoded) > cls.PARAM_ID_MAX_BYTES:
            return None
        return encoded

    def request_param(self, name: str) -> bool:
        """Request a single parameter by name (param_index=-1)."""
        with self._operation_lock:
            if not self._connection_ready():
                return False
            encoded = self._encode_param_name(name)
            if encoded is None:
                self._set_command_error("invalid parameter name")
                return False
            conn = self._conn
            try:
                with self._send_lock:
                    if conn is None or conn is not self._conn:
                        return False
                    conn.mav.param_request_read_send(
                        self._target_system, self._target_component,
                        encoded, -1,
                    )
            except Exception as exc:  # noqa: BLE001 - a torn-down link is not a 500
                logger.debug("param read request failed (%s): %s", name, exc)
                return False
            return True

    def fetch_params(
        self, names: list[str], timeout: float = 4.0,
    ) -> dict[str, float]:
        """Read a named set of parameters without a full parameter download.

        The Motors page needs ~100 specific parameters, not the ~1300 a full
        download pulls. This asks for the missing ones in one burst of
        ``PARAM_REQUEST_READ`` (one retransmit round for the stragglers, since a
        lossy link drops individual replies) and returns ``{name: value}`` for
        whatever arrived before *timeout*.

        Names the vehicle never answers for are simply absent from the result —
        that is the version-tolerance contract: a parameter this firmware does
        not have is a missing key, never an error. Nothing here mutates the
        download state machine, so it is safe to call while the parameter
        editor's full download is idle *or* running.
        """
        wanted = [n for n in names if isinstance(n, str) and n]
        if not wanted:
            return {}
        if not self._connection_ready():
            self._set_command_error("not connected")
            return {}

        def snapshot() -> dict[str, float]:
            with self._param_lock:
                return {n: self._params[n].value for n in wanted if n in self._params}

        deadline = time.monotonic() + max(0.5, timeout)
        have = snapshot()
        missing = [n for n in wanted if n not in have]
        # Two rounds: the initial burst, then one retransmit of whatever is
        # still outstanding halfway through the budget.
        for round_index in range(2):
            if not missing:
                break
            for name in missing:
                # stop() marks the vehicle disconnected before it tears the
                # socket down, so this is also the shutdown bail-out.
                if self._stop_event.is_set() or not self._connection_ready():
                    return snapshot()
                self.request_param(name)
            round_deadline = deadline if round_index else (
                time.monotonic() + max(0.25, (deadline - time.monotonic()) / 2))
            while time.monotonic() < min(round_deadline, deadline):
                have = snapshot()
                missing = [n for n in wanted if n not in have]
                if not missing:
                    break
                if self._stop_event.is_set():
                    return have
                time.sleep(0.05)
            have = snapshot()
            missing = [n for n in wanted if n not in have]
        return have

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
            if self._encode_param_name(name) is None:
                self._set_command_error("invalid parameter name")
                return False
            # bool is an int in Python: True would be written as 1.0.
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                self._set_command_error("parameter value must be a number")
                return False
            value = float(value)
            if not math.isfinite(value):
                # NaN/Inf reaches PX4 as a valid float32 and is stored as one.
                # The echo check below could never confirm it either (NaN is
                # not equal to itself), so it would report an unconfirmed write
                # for a value that did land on the vehicle.
                self._set_command_error("parameter value must be finite")
                return False
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
                        self._set_command_error("not connected")
                        return False
                    conn = self._conn
                    try:
                        with self._send_lock:
                            if conn is None or conn is not self._conn:
                                self._set_command_error("not connected")
                                return False
                            conn.mav.param_set_send(
                                self._target_system, self._target_component,
                                name.encode("ascii"), value, entry.type,
                            )
                    except Exception as exc:  # noqa: BLE001 - report, never raise
                        self._set_command_error(f"parameter write failed: {exc}")
                        return False
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
            if abs(echoed.value - value) <= max(1e-4, 1e-3 * abs(value)):
                self._console_publish("PARAM", f"{name} set to {value}", "success")
                return True
            text = (f"parameter write not confirmed (got {echoed.value} expected {value})")
            self._set_command_error(text)
            return False

    def start_param_upload(self, params: list[dict]) -> bool:
        """Apply a saved parameter file to the vehicle as a background upload.

        Validates the list up front, flips the protocol state to ``"uploading"``,
        and spawns one daemon worker that walks the list calling :meth:`set_param`
        (the confirmed-write path). Progress is published over the existing
        param listeners so ``GET /api/params/progress`` emits per-param updates;
        the final tally is read via :meth:`get_param_upload_result`. Mirrors
        :meth:`request_param_list`: the validation+start run under
        ``_operation_lock`` so they cannot race with ``set_param``/``stop``.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not isinstance(params, list) or not params:
                self._set_command_error("invalid parameter list")
                return False
            for entry in params:
                if not isinstance(entry, dict):
                    self._set_command_error("invalid parameter list")
                    return False
                name = entry.get("name")
                value = entry.get("value")
                if not isinstance(name, str) or not name:
                    self._set_command_error("invalid parameter list")
                    return False
                # bool is a subclass of int — reject it so True is never coerced
                # to 1.0 and silently written to the autopilot.
                if isinstance(value, bool) or not isinstance(value, (int, float)):
                    self._set_command_error("invalid parameter list")
                    return False
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            # Defense-in-depth: PX4 also rejects param writes while armed, but
            # refuse up front so the operator gets a clear error instead of N
            # per-param failures from the worker.
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot upload parameters while armed")
                return False
            with self._param_lock:
                if self._param_download_state in ("downloading", "uploading"):
                    self._set_command_error("another parameter operation in progress")
                    return False
                self._param_download_state = "uploading"
                self._param_count = len(params)
                self._param_received = 0
                self._param_upload_failed = []
                self._param_upload_result = {}
            # set_param acquires _operation_lock internally; since this runs on
            # the worker thread (not this one) there is no recursive-lock
            # issue, and we deliberately do NOT hold _operation_lock across
            # the loop — that would block every other command for the whole
            # upload.
            self._param_upload_thread = threading.Thread(
                target=self.set_params_batch, args=(params,),
                name="param-upload", daemon=True,
            )
            self._param_upload_thread.start()
            return True

    def set_params_batch(self, params: list[dict]) -> None:
        """Worker target: write each parameter via the confirmed-write path.

        Runs on the ``param-upload`` daemon thread. Reuses :meth:`set_param`
        so each write inherits the armed-check, cache-lookup, and PARAM_VALUE
        echo confirmation. Per-param progress is pushed to the param listeners
        (the SSE handler reads only ``state``/``count``/``received``); the
        terminal ``"upload_complete"`` status is always emitted, even on error,
        so the UI never waits on a stuck ``"uploading"`` state.
        """
        failed: list[dict[str, Any]] = []
        try:
            for p in params:
                if not self._running.is_set():
                    break
                name = p["name"]
                value = float(p["value"])
                ok = self.set_param(name, value)
                with self._param_lock:
                    if ok:
                        self._param_received += 1
                    else:
                        # Read the error on this worker thread (set_param sets
                        # the thread-local _command_context here); copy it out
                        # under the lock so the result is self-consistent.
                        failed.append({
                            "name": name,
                            "error": self.get_last_command_error() or "write failed",
                        })
                    status = {
                        "state": self._param_download_state,
                        "count": self._param_count,
                        "received": self._param_received,
                        "name": name,
                        "value": value,
                    }
                self._notify_param_listeners(status)
        except Exception as exc:
            logger.error("param upload failed: %s", exc)
            with self._param_lock:
                self._param_download_state = "upload_complete"
                self._param_upload_result = {
                    "state": "upload_complete",
                    "written": self._param_received,
                    "failed": len(failed),
                    "errors": list(failed),
                }
                final = {
                    "state": "upload_complete",
                    "count": self._param_count,
                    "received": self._param_received,
                }
            self._notify_param_listeners(final)
            return
        with self._param_lock:
            self._param_download_state = "upload_complete"
            self._param_upload_result = {
                "state": "upload_complete",
                "written": self._param_received,
                "failed": len(failed),
                "errors": list(failed),
            }
            final = {
                "state": "upload_complete",
                "count": self._param_count,
                "received": self._param_received,
            }
        self._notify_param_listeners(final)

    def get_param_upload_result(self) -> dict[str, Any]:
        """Return the final tally of the last batch upload.

        ``{"state","written","failed","errors"}`` — idle default before any
        upload has run so the HTTP endpoint can render before a vehicle is
        connected. Copied under ``_param_lock`` so concurrent readers see a
        stable snapshot.
        """
        with self._param_lock:
            if not self._param_upload_result:
                return {"state": "idle", "written": 0, "failed": 0, "errors": []}
            return dict(self._param_upload_result)

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
            # Last-arrival timestamp feeds the watchdog's inactivity retransmit
            # (BUG 5).
            self._param_last_value_at = time.monotonic()
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
    # Lossy-link parameter-download recovery (BUG 5)
    # ------------------------------------------------------------------

    def _start_param_watchdog(self) -> None:
        """Spawn the download watchdog (only while the bridge is running)."""
        if not self._running.is_set():
            return
        if self._param_watchdog_thread and self._param_watchdog_thread.is_alive():
            return
        self._param_watchdog_thread = threading.Thread(
            target=self._param_watchdog, name="param-watchdog", daemon=True,
        )
        self._param_watchdog_thread.start()

    def _finish_param_download_incomplete(self) -> None:
        """Flip a stuck download to a terminal "incomplete" (partial params kept)."""
        with self._param_lock:
            if self._param_download_state != "downloading":
                return
            self._param_download_state = "incomplete"
            status = {
                "state": "incomplete",
                "count": self._param_count,
                "received": self._param_received,
            }
        self._notify_param_listeners(status)

    def _param_watchdog(self) -> None:
        """Re-request lost PARAM_VALUEs and time out a stuck download (BUG 5).

        PX4 bursts the full parameter list on PARAM_REQUEST_LIST; on a lossy
        link some PARAM_VALUEs are lost and _param_received never reaches
        _param_count, leaving the editor stuck at complete:false. This
        watchdog wakes periodically and, once the burst has settled (no new
        PARAM_VALUE for PARAM_DOWNLOAD_INACTIVITY_S), re-requests the missing
        indices via param_request_read_send. After PARAM_DOWNLOAD_MAX_ROUNDS
        or PARAM_DOWNLOAD_TIMEOUT_S it flips the state to a terminal
        "incomplete" (partial params kept) so the UI never waits forever.
        Bounded: never loops past the rounds/timeout caps.
        """
        while self._running.is_set() and not self._stop_event.is_set():
            self._interruptible_sleep(PARAM_WATCHDOG_TICK_S)
            with self._param_lock:
                state = self._param_download_state
                if state != "downloading":
                    return
                count = self._param_count
                received = self._param_received
                seen = set(self._param_seen_indices)
                started_at = self._param_download_started_at
                last_value = self._param_last_value_at
                round_ = self._param_retransmit_round
            if count <= 0:
                continue
            if received >= count:
                return
            now = time.monotonic()
            if now - started_at > PARAM_DOWNLOAD_TIMEOUT_S:
                self._finish_param_download_incomplete()
                return
            if now - last_value < PARAM_DOWNLOAD_INACTIVITY_S:
                continue
            if round_ >= PARAM_DOWNLOAD_MAX_ROUNDS:
                self._finish_param_download_incomplete()
                return
            missing = set(range(count)) - seen
            if not missing:
                return
            with self._param_lock:
                self._param_retransmit_round = round_ + 1
            conn = self._conn
            if conn is None or conn is not self._conn:
                return
            # Re-request each missing index by index (name=b"", index=idx).
            for idx in sorted(missing):
                try:
                    with self._send_lock:
                        if conn is not self._conn:
                            return
                        conn.mav.param_request_read_send(
                            self._target_system, self._target_component,
                            b"", idx,
                        )
                except Exception as exc:
                    logger.debug("param retransmit idx=%d failed: %s", idx, exc)

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
    # Sensor calibration
    # ------------------------------------------------------------------

    _CALIBRATION_MAP: dict[str, list[float]] = {
        # MAV_CMD_PREFLIGHT_CALIBRATION (241) — verified against PX4 v1.16,
        # v1.17, v1.18 (Commander.cpp ~line 1430). Unset params are NaN; the
        # selected one 1.0. Motor/ESC calibration (param7=1.0) verified across
        # all three target versions.
        "gyro":        [1.0, float("nan"), float("nan"), float("nan"), float("nan"), float("nan"), float("nan")],
        "compass":     [float("nan"), 1.0, float("nan"), float("nan"), float("nan"), float("nan"), float("nan")],
        "baro":        [float("nan"), float("nan"), 1.0, float("nan"), float("nan"), float("nan"), float("nan")],
        "accel":      [float("nan"), float("nan"), float("nan"), float("nan"), 1.0, float("nan"), float("nan")],
        "level":       [float("nan"), float("nan"), float("nan"), float("nan"), 2.0, float("nan"), float("nan")],
        "accel_quick": [float("nan"), float("nan"), float("nan"), float("nan"), 4.0, float("nan"), float("nan")],
        "airspeed":    [float("nan"), float("nan"), float("nan"), float("nan"), float("nan"), 1.0, float("nan")],
        # motor/ESC calibration — props MUST be removed; motors spin at max PWM
        "motor":       [float("nan"), float("nan"), float("nan"), float("nan"), float("nan"), float("nan"), 1.0],
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

    def cancel_calibration(self) -> bool:
        """Abort a running calibration via MAV_CMD_PREFLIGHT_CALIBRATION (all 0).

        PX4's Commander reads an all-zero PREFLIGHT_CALIBRATION as "cancel the
        calibration currently in progress" — verified against v1.16, v1.17 and
        v1.18. Without it, an operator who starts the wrong calibration, or one
        that stalls waiting for a side it will never see, has no way out except
        power-cycling the autopilot.

        Deliberately not gated on the armed state: this only ever *stops* work
        the vehicle is doing, so refusing it would be a safety regression rather
        than defense in depth.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure("Cancel calibration", -2)
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION,
                [0.0] * 7, timeout=5.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Cancel calibration", result)
            self._console_publish("CALIBRATE", "calibration cancelled", "warning")
            return True

    # ------------------------------------------------------------------
    # Motor test
    # ------------------------------------------------------------------

    # MAV_CMD_DO_MOTOR_TEST (209) — verified against PX4 v1.16, v1.17 and v1.18
    # (mavlink_receiver.cpp -> actuator_test). param1 is the 1-based motor,
    # param2 the throttle type (0 = percent), param3 the throttle value,
    # param4 the timeout in seconds, param5 the motor count (0 = this motor
    # only) and param6 the test order (0 = default).
    MOTOR_TEST_THROTTLE_PERCENT = 0.0

    # A spinning motor with no timeout is a hazard: if the link drops mid-test
    # nothing stops it. Every test therefore carries a bounded timeout that PX4
    # enforces on the vehicle itself, so the motor stops even if the GCS dies.
    MOTOR_TEST_MAX_DURATION_S = 10.0

    def motor_test(self, motor: int, throttle_pct: float, duration_s: float) -> bool:
        """Spin one motor on the bench so the operator can identify it.

        PROPELLERS MUST BE OFF. This is the identification tool behind Setup ->
        Motors: it answers "which physical motor is Motor 3?" without arming.
        The UI gates it behind an explicit propellers-removed acknowledgement;
        this layer enforces what it can — disarmed only, a valid motor number, a
        throttle inside the protocol range, and a bounded duration that the
        *vehicle* counts down, so a dropped link cannot leave a motor running.
        """
        with self._operation_lock:
            self._set_command_error("")
            # bool is a subclass of int — reject it so True never becomes motor 1.
            if isinstance(motor, bool) or not isinstance(motor, int) or not 1 <= motor <= 16:
                self._set_command_error("motor must be between 1 and 16")
                return False
            if not 0.0 <= float(throttle_pct) <= 100.0:
                self._set_command_error("throttle must be between 0 and 100 percent")
                return False
            duration = max(0.0, min(self.MOTOR_TEST_MAX_DURATION_S, float(duration_s)))
            # Defense-in-depth: refuse while armed (PX4 also rejects an actuator
            # test on an armed vehicle).
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot test motors while armed")
                return False
            if not self._connection_ready():
                return self._command_failure(f"Motor test {motor}", -2)
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_DO_MOTOR_TEST,
                [float(motor), self.MOTOR_TEST_THROTTLE_PERCENT, float(throttle_pct),
                 duration, 0.0, 0.0, 0.0],
                timeout=5.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure(f"Motor test {motor}", result)
            self._console_publish(
                "MOTOR", f"motor {motor} test at {throttle_pct:g}% for {duration:g}s",
                "warning")
            return True

    def stop_motor_test(self) -> bool:
        """Stop a running motor test (throttle 0, timeout 0).

        Deliberately not gated on the armed state, and not gated on a valid
        preceding test: this only ever *stops* a motor, so refusing it would be
        a safety regression rather than defense in depth. Mirrors
        :meth:`cancel_calibration`.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure("Stop motor test", -2)
            ok = True
            # Every motor, not just the last one tested: the operator pressing
            # Stop wants silence, and a test that was started by something else
            # (or before a reload) is exactly when that matters most.
            for motor in range(1, 9):
                result = self._send_command_and_wait(
                    mavutil.mavlink.MAV_CMD_DO_MOTOR_TEST,
                    [float(motor), self.MOTOR_TEST_THROTTLE_PERCENT, 0.0,
                     0.0, 0.0, 0.0, 0.0],
                    timeout=2.0, retries=0,
                )
                if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                    ok = False
            if not ok:
                self._set_command_error("motor stop not confirmed for every motor")
            self._console_publish("MOTOR", "motor test stopped", "warning")
            return ok

    # ------------------------------------------------------------------
    # Autotune
    # ------------------------------------------------------------------
    #
    # The autotune runs IN FLIGHT. PX4's mc_autotune_attitude_control injects
    # steps into the rate controller and identifies the airframe from the
    # response, which it can only do on an armed, airborne vehicle; the command
    # handler answers TEMPORARILY_REJECTED while the vehicle is disarmed.
    #
    # Corvus used to refuse to send this command *unless* the vehicle was
    # disarmed, copying the armed-refusal that is correct for calibration and
    # parameter writes. Applied here it inverted the precondition, so the only
    # state in which the command was sent was the only state PX4 rejects it in,
    # and the autotune could never run. The gate below is the real one: the
    # vehicle must be armed, and must not be sitting on the ground.
    #
    # Progress is not STATUSTEXT. PX4 answers this command with a repeated
    # COMMAND_ACK carrying MAV_RESULT_IN_PROGRESS and a 0-100 progress field
    # for as long as the tune runs, then a final ACK with the outcome; that
    # stream is latched into the store by _handle_autotune_ack.

    _AUTOTUNE_AXIS_MAP: dict[str, float] = {
        # PX4's multicopter autotune always tunes all three axes: it ignores
        # param2 on v1.16/v1.17 and wants zero on v1.18. The fixed-wing tune
        # does take an axis selection, but through FW_AT_AXES rather than
        # through this command, so "all" stays the only value on the wire.
        "all": 0.0,
    }

    def autotune(self, axis: str = "all", enable: bool = True) -> bool:
        """Start or stop the PX4 autotune via MAV_CMD_DO_AUTOTUNE_ENABLE (212).

        param1 is 1 to start and 0 to stop; param2 is the axis selection, which
        PX4 v1.16-v1.18 accept only as 0 ("all"). A successful start is
        acknowledged with ACCEPTED or IN_PROGRESS.

        Starting is gated on the vehicle being armed and airborne, because that
        is what PX4 requires. Stopping is gated on nothing at all: an operator
        who wants the injection to end is never told to satisfy a precondition
        first, the same reasoning as :meth:`cancel_calibration`.
        """
        with self._operation_lock:
            self._set_command_error("")
            axis_val = self._AUTOTUNE_AXIS_MAP.get(axis)
            if axis_val is None:
                self._set_command_error(f"unknown autotune axis: {axis}")
                return False
            if enable:
                problem = self._autotune_precondition()
                if problem:
                    self._set_command_error(problem)
                    return False
            if not self._connection_ready():
                return self._command_failure(f"Autotune {axis}", -2)
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE,
                [1.0 if enable else 0.0, axis_val, nan, nan, nan, nan, nan],
                timeout=5.0, retries=0,
                accept_in_progress=True,
            )
            if result not in (
                mavutil.mavlink.MAV_RESULT_ACCEPTED,
                mavutil.mavlink.MAV_RESULT_IN_PROGRESS,
            ):
                if enable:
                    self._store.update(autotune_state="failed", autotune_progress=0)
                return self._command_failure(f"Autotune {axis}", result)
            if enable:
                self._store.update(autotune_state="running", autotune_progress=0)
                self._console_publish("AUTOTUNE", "Autotune started", "success")
            else:
                self._store.update(autotune_state="", autotune_progress=0)
                self._console_publish("AUTOTUNE", "Autotune stopped", "warning")
            return True

    def _autotune_precondition(self) -> str:
        """Why the autotune cannot start right now, or "" when it can.

        The message is the whole point of this method: "cannot autotune" tells
        an operator standing in a field nothing, whereas naming the missing
        precondition tells them what to do next.

        An unknown landed state (0, the firmware does not publish
        EXTENDED_SYS_STATE, or nothing has arrived yet) is not treated as being
        on the ground. Blocking on the absence of a message would refuse a
        command PX4 would have accepted, and PX4 remains the authority — it
        rejects the tune itself if the vehicle really is grounded.
        """
        snapshot = self._store.get_snapshot()
        if not snapshot.get("armed"):
            return ("the autotune runs in flight: arm the vehicle and take off "
                    "before starting it")
        on_ground = mavutil.mavlink.MAV_LANDED_STATE_ON_GROUND
        if int(snapshot.get("landed_state") or 0) == on_ground:
            return ("the vehicle is still on the ground: hold a stable hover "
                    "before starting the autotune")
        return ""

    def _handle_autotune_ack(self, msg: Any) -> None:
        """Latch the autotune's progress and outcome out of its COMMAND_ACK.

        PX4 re-acknowledges MAV_CMD_DO_AUTOTUNE_ENABLE for the whole run: one
        IN_PROGRESS ACK per step carrying `progress` 0-100, then one final ACK
        whose result is the outcome. This is the only progress a ground station
        receives — the module's uORB status topic never leaves the autopilot —
        so it is stored rather than left to scroll past in the console.

        Called from the receive loop for every autotune ACK, including ones
        answering a command another station sent: a tune running on this
        aircraft is this station's business regardless of who started it.
        """
        try:
            result = int(msg.result)
        except (TypeError, ValueError):
            return
        if result == mavutil.mavlink.MAV_RESULT_IN_PROGRESS:
            try:
                progress = int(getattr(msg, "progress", 0) or 0)
            except (TypeError, ValueError):
                progress = 0
            self._store.update(
                autotune_state="running",
                autotune_progress=max(0, min(100, progress)),
            )
            return
        if result == mavutil.mavlink.MAV_RESULT_ACCEPTED:
            # ACCEPTED closes a run that was already reporting progress; on the
            # very first ACK it only means "command taken", which the start
            # path has already recorded as running.
            if self._store.get_snapshot().get("autotune_state") == "running":
                self._store.update(autotune_state="done", autotune_progress=100)
            return
        self._store.update(autotune_state="failed")
        self._store.merge_warning(
            "Autotune refused: " + MAV_RESULT_TEXT.get(result, f"result={result}"),
            "warning",
        )

    # ------------------------------------------------------------------
    # Reboot to bootloader (Firmware Flash)
    # ------------------------------------------------------------------

    def reboot_to_bootloader(self) -> bool:
        """Reboot the autopilot into its USB bootloader (for firmware flashing).

        Sends MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN with the param that PX4
        v1.16-v1.18 interpret as 'reboot to bootloader'. Refused while armed
        (defense-in-depth, even though PX4 would reject too). Returns True only
        if PX4 ACCEPTED.

        Verified param1 = 3.0 (REBOOT_TO_BOOTLOADER) against PX4 source
        (src/modules/commander/Commander.cpp — the uORB vehicle_command handler,
        NOT commander_helper.cpp which only deals with LEDs/tunes):
          - v1.16.0  Commander.cpp:1254-1256
            `else if ((param1 == 3) && !isArmed() &&
                       (px4_reboot_request(REBOOT_TO_BOOTLOADER, 400_ms) == 0))`
          - v1.17.0  Commander.cpp:1274-1276  (identical)
          - v1.18    Commander.cpp:1415-1417  (v1.18.0-beta2; v1.18.0 final is
            not yet tagged, beta2 is the latest pre-release — identical)
        All three: param1==3 → px4_reboot_request(REBOOT_TO_BOOTLOADER), ACK =
        VEHICLE_CMD_RESULT_ACCEPTED, then the commander parks in a busy loop
        until the board resets. Armed or boards without CONFIG_BOARDCTL_RESET
        fall through to VEHICLE_CMD_RESULT_DENIED. The three target versions
        agree on param1=3, so we send one shot (retries=0) with a short 3 s
        timeout — the ACK arrives before the FC actually resets. We do NOT stop
        the bridge or close the connection here: the caller (FlashService) owns
        teardown ordering so the bootloader stays reachable on the same device.
        """
        with self._operation_lock:
            self._set_command_error("")
            # Defense-in-depth: PX4 gates every reboot branch on !isArmed()
            # (Commander.cpp), so an armed FC would DENY us regardless.
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot reboot to bootloader while armed")
                return False
            if not self._connection_ready():
                return self._command_failure("Reboot to bootloader", -2)
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN,
                [3.0, nan, nan, nan, nan, nan, nan],
                timeout=3.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Reboot to bootloader", result)
            self._console_publish("REBOOT", "Reboot to bootloader accepted", "success")
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
    # vehicle's decision via ``COM_RC_IN_MODE`` — 1 (joystick only) or 3 (both
    # stick sources) — which the GCS deliberately does not set on the
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
        stick. Values outside those ranges are clamped rather than rejected —
        a stick that overshoots by a rounding error must still fly.

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

    def set_rc_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        """Raise (or restore) the RC_CHANNELS rate the Radio Control page needs.

        Lean, exactly like :meth:`set_vibration_stream`: PX4 streams
        RC_CHANNELS at a few hertz by default, which is enough for a channel
        bar and not enough for a calibration — a stick swept through its travel
        in half a second is three samples at 5 Hz, and the endpoint the wizard
        writes is then whatever those three happened to catch.

        Safe while armed, and deliberately so — this is read-only telemetry,
        and the page reads channels in flight to check a switch does what the
        operator thinks it does. Disabling sends interval 0, handing the rate
        back to the firmware's own default rather than to a number this build
        picked.
        """
        with self._operation_lock:
            self._set_command_error("")
            if enabled:
                if isinstance(rate_hz, bool) or not isinstance(rate_hz, int):
                    self._set_command_error("RC rate must be an integer")
                    return False
                if not 1 <= rate_hz <= 50:
                    self._set_command_error("RC rate must be between 1 and 50 Hz")
                    return False
                interval_us = max(1, int(1_000_000 / rate_hz))
            else:
                interval_us = 0
            if not self._connection_ready():
                # set_message_interval returns False here without a reason, and
                # the page opened over a dead link is the common case for this
                # route — an unexplained refusal would reach the browser as a
                # conflict rather than as "there is no vehicle".
                self._set_command_error("not connected")
                self._store.update(rc_live=False)
                return False
            ok = self.set_message_interval(
                mavutil.mavlink.MAVLINK_MSG_ID_RC_CHANNELS, interval_us,
            )
            if not enabled:
                # The channel bars stop being live the moment the rate is handed
                # back, so the page stops claiming a reading it is no longer
                # being sent.
                self._store.update(rc_live=False)
            return ok

    # The messages the PID tuning page plots, as commanded/achieved pairs.
    # ATTITUDE is already streamed for the HUD; it is raised here too because a
    # 50 Hz response sampled against a 5 Hz setpoint makes a clean tune look
    # like a lagging one.
    _TUNING_MSG_IDS: tuple[int, ...] = (
        mavutil.mavlink.MAVLINK_MSG_ID_ATTITUDE,
        mavutil.mavlink.MAVLINK_MSG_ID_ATTITUDE_TARGET,
        mavutil.mavlink.MAVLINK_MSG_ID_POSITION_TARGET_LOCAL_NED,
    )

    def set_tuning_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        """Raise (or restore) the setpoint stream the PID tuning page plots.

        Lean, exactly like :meth:`set_vibration_stream`: PX4 streams
        ATTITUDE_TARGET and POSITION_TARGET_LOCAL_NED slowly by default, the
        GCS asks for a tuning rate only while the page is open, and disabling
        sends interval 0 to hand the rate back to the firmware's own default
        rather than to a number this build picked.

        Safe while armed, and deliberately so — this is read-only telemetry,
        and the page that wants it is used in flight.

        Returns True only if every message was accepted; a firmware that
        refuses one still gets the others, because a missing setpoint trace is
        a degraded graph and not a failure worth aborting the page for.
        """
        with self._operation_lock:
            self._set_command_error("")
            if enabled:
                if isinstance(rate_hz, bool) or not isinstance(rate_hz, int):
                    self._set_command_error("tuning rate must be an integer")
                    return False
                if not 1 <= rate_hz <= 50:
                    self._set_command_error("tuning rate must be between 1 and 50 Hz")
                    return False
                interval_us = max(1, int(1_000_000 / rate_hz))
            else:
                interval_us = 0
            if not self._connection_ready():
                # Same reason set_rc_stream carries this guard: without it
                # set_message_interval clears the command error and returns
                # False without setting one, so the route answers 409 with a
                # generic "request failed" for what is simply no vehicle.
                self._set_command_error("not connected")
                self._store.update(setpoints_live=False)
                return False
            ok = True
            for msg_id in self._TUNING_MSG_IDS:
                if not self.set_message_interval(msg_id, interval_us):
                    ok = False
            if not enabled:
                # The traces stop being live the moment the rate is handed
                # back, so the page stops claiming a setpoint it is no longer
                # being sent.
                self._store.update(setpoints_live=False)
            return ok

    # ------------------------------------------------------------------
    # PX4 NuttShell (NSH) debug shell — SERIAL_CONTROL device=SHELL
    # ------------------------------------------------------------------
    #
    # The NSH shell is the only MAVLink path to NSH builtins with no MAV_CMD
    # equivalent — ``listener <topic>``, ``top``, ``free``, ``dmesg``. PX4
    # exposes it over SERIAL_CONTROL with device=SERIAL_CONTROL_DEV_SHELL;
    # this is the standard QGC MAVLink Console mechanism, stable across PX4
    # v1.16, v1.17, and v1.18. The debug shell must be enabled on the
    # autopilot: it is on by default for SITL; on hardware the MAVLink
    # instance must permit shell access (MAV_ADVANCED_PARAMS / instance
    # config). RESPOND|EXCLUSIVE|MULTI is the QGroundControl framing: PX4
    # keeps the shell open and streams every available 70-byte response chunk.

    def _reset_shell_state(self) -> None:
        """Clear all state associated with the current PX4 shell session."""
        with self._shell_lock:
            self._shell_active = False
            self._shell_buffer = ""
            self._shell_decoder.reset()

    def _send_serial_control(
        self, conn: Any, flags: int, count: int, data: bytes,
    ) -> None:
        """Send SERIAL_CONTROL, using target extensions when available."""
        args: tuple[Any, ...] = (
            mavutil.mavlink.SERIAL_CONTROL_DEV_SHELL,
            flags,
            0,
            0,
            count,
            data,
        )
        fields = getattr(
            mavutil.mavlink.MAVLink_serial_control_message,
            "fieldnames",
            (),
        )
        if "target_system" in fields and "target_component" in fields:
            args += (self._target_system, self._target_component)
        conn.mav.serial_control_send(*args)

    def send_shell_command(self, text: str) -> bool:
        """Send ``text`` (plus a newline) to the PX4 NSH debug shell.

        Sends one or more ``SERIAL_CONTROL`` packets with ``device=SHELL`` and
        ``flags=RESPOND|EXCLUSIVE|MULTI`` so PX4 streams the response back.
        Returns
        ``True`` if the SERIAL_CONTROL was sent, ``False`` on disconnect
        or send failure (mirrors the ``-2``/``False`` convention of the
        other command methods).
        """
        # Clear any stale thread-local error so the caller sees a fresh result
        # (BUG 10). The success path leaves it empty; the failure paths set it.
        self._set_command_error("")
        if not isinstance(text, str):
            self._set_command_error("shell command must be text")
            return False
        with self._send_lock:
            conn = self._conn
            # A6: bail if stop()/reconnect swapped the connection, or the
            # link isn't healthy enough to dispatch an operator command.
            if conn is not self._conn or not self._connection_ready():
                return self._command_failure("Shell", -2)
            payload = (text if text.endswith("\n") else text + "\n").encode(
                "utf-8", errors="replace",
            )
            flags = (
                mavutil.mavlink.SERIAL_CONTROL_FLAG_RESPOND
                | mavutil.mavlink.SERIAL_CONTROL_FLAG_EXCLUSIVE
                | mavutil.mavlink.SERIAL_CONTROL_FLAG_MULTI
            )
            with self._shell_lock:
                self._reset_shell_state()
                self._shell_active = True
                try:
                    for offset in range(0, len(payload), 70):
                        chunk = payload[offset:offset + 70]
                        self._send_serial_control(
                            conn, flags, len(chunk), chunk.ljust(70, b"\x00"),
                        )
                except Exception as exc:
                    self._reset_shell_state()
                    logger.error("shell command send failed: %s", exc)
                    # Surface the real cause instead of a stale/generic error (BUG 10).
                    self._set_command_error(f"shell send failed: {exc}")
                    return False
            return True

    def stop_shell(self) -> None:
        """Release exclusive NSH shell mode and clear the line buffer.

        Sends a ``SERIAL_CONTROL`` with ``flags=0`` and ``count=0`` (the
        documented release) so the autopilot stops streaming shell output
        and frees the shell for other clients. Best-effort and idempotent:
        swallows exceptions and is a safe no-op when not connected. Called
        from :meth:`stop` so exclusive mode is released on shutdown.
        """
        with self._send_lock:
            conn = self._conn
            self._reset_shell_state()
            # A6: only release on the connection we snapshotted; no-op if
            # the link was already torn down (best-effort release).
            if conn is None or conn is not self._conn:
                return
            try:
                self._send_serial_control(conn, 0, 0, b"\x00" * 70)
            except Exception:
                pass

    def _handle_serial_control(self, msg: Any) -> None:
        """Reassemble an NSH-shell SERIAL_CONTROL reply into console lines.

        PX4 streams the shell response in 70-byte SERIAL_CONTROL chunks.
        Each chunk is decoded incrementally, sanitized (CR stripped,
        backspace applied),
        appended to the line buffer, and complete lines are published to the
        console subscribers. Runs in the receive loop and never blocks.
        """
        try:
            if int(getattr(msg, "device", -1)) != mavutil.mavlink.SERIAL_CONTROL_DEV_SHELL:
                return
            if not self._shell_active or not self._ack_is_for_us(msg):
                return
            lines: list[str] = []
            with self._shell_lock:
                if not self._shell_active:
                    return
                count = int(getattr(msg, "count", 0))
                if count <= 0 or count > 70:
                    return
                raw_data = bytes(msg.data)
                if count > len(raw_data):
                    return
                text = self._shell_decoder.decode(raw_data[:count], final=False)
                # Sanitize: drop CR (NSH sends \r\n); apply BS by removing the
                # previous char — in this chunk, or the buffer tail if a
                # backspace crosses a chunk boundary (NSH line-edit echo).
                chunk: list[str] = []
                for ch in text:
                    if ch == "\r":
                        continue
                    if ch == "\b":
                        if chunk:
                            chunk.pop()
                        elif self._shell_buffer:
                            self._shell_buffer = self._shell_buffer[:-1]
                        continue
                    chunk.append(ch)
                self._shell_buffer += "".join(chunk)
                # Publish each complete line; keep the trailing partial line.
                while "\n" in self._shell_buffer:
                    line, self._shell_buffer = self._shell_buffer.split("\n", 1)
                    if line:
                        lines.append(line)
                # No newline in sight and the buffer has run long: flush what
                # is there rather than keep growing it.
                if len(self._shell_buffer) >= SHELL_LINE_MAX_CHARS:
                    lines.append(self._shell_buffer)
                    self._shell_buffer = ""
            for line in lines:
                self._console_publish("SHELL", line, "info")
        except Exception as exc:
            logger.debug("SERIAL_CONTROL handling failed: %s", exc)
