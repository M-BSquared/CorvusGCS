"""MAVLink bridge — real autopilot communication via corvus_link.

Connects to a PX4 autopilot (SITL or real) over UDP/serial/TCP, parses
incoming messages, updates the Vehicle State Store, and forwards raw
message names to the console subscribers. Runs on corvus_link's native
link threads, which stop() joins.

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

import datetime  # noqa: F401 - the engines read it from here (SYSTEM_TIME, tlog names)
import errno
import logging
import os
import random  # noqa: F401 - the reconnect backoff's jitter, read by the engine
import sys
import threading
import time
from typing import Any
from collections.abc import Callable

from corvus_link import bridge as _receive
from corvus_link import mavutil

from . import autopilot as autopilot_dialect
from . import rtk
from .autopilot import (  # noqa: F401 - re-exported, see the tables below
    PX4_AUTO_SUBMODE,
    PX4_AVAILABLE_MODES,
    PX4_MAIN_MODE,
    Dialect,
    ModeCommand,
    VehicleEvent,
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
from .fleet import MAIN_LINK, FleetTracker
from .mavlink_forwarder import frame_source_system
from .mavlink_flight import FlightCommandsMixin
from .mavlink_ftp import FtpClientMixin
from .mavlink_logs import LogTransferMixin
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
    split_serial_connection,
)
from .state_store import VehicleStateStore
from .mavlink_inspector import MessageScope
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
_LEVEL_RANK = {"info": 0, "warning": 1, "critical": 2}

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

# A frame whose exact bytes (sequence number included) arrive again inside this
# window is a copy, not news: a routing loop, or one frame reaching us over two
# paths. Short against the time a busy PX4 link takes to wrap its 8-bit
# sequence, long against a loop's round trip.
DUPLICATE_FRAME_WINDOW_S = 0.3

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
# When the vehicle's readiness flips from ready to not ready, the reason is
# asked for at once (something changed: a kill switch moved, a sensor
# dropped out), but not more often than this.
PREARM_CHANGE_REQUEST_MIN_S = 2.0
# A SYS_STATUS saying "ready" this soon after the vehicle reported its kill
# switch engaged is taken for one composed before the switch moved, not as
# proof the switch is off. Both stacks run their arming checks on a slower
# clock than they send the switch's own line, and taking the stale "ready"
# at its word cleared the kill switch moments after it engaged.
KILL_SETTLE_S = 3.0
# While the switch is engaged and the vehicle disarmed, on a stack whose
# report names every failing check, the report is asked for this often, so
# a release whose own line was lost on the radio is still noticed.
KILL_RECHECK_S = 5.0
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
# field alone lets through. Kept in step with corvus_link's own
# `probably_vehicle_heartbeat`, plus the peripherals PX4 and ArduPilot
# airframes actually carry.
_NON_VEHICLE_TYPES: frozenset[int] = _receive.NON_VEHICLE_TYPES

# Component ids that are never the autopilot even under the vehicle's own
# system id. The gimbal is the one corvus_link singles out; the SiK radio and a
# UDP bridge matter here because both sit on the wire between Corvus and the
# aircraft and both heartbeat.
_NON_AUTOPILOT_COMPONENTS: frozenset[int] = _receive.NON_AUTOPILOT_COMPONENTS


# Upper bounds a real HEARTBEAT stays under. Deliberately far above what the
# dialect defines today (MAV_TYPE ends in the 40s, MAV_AUTOPILOT in the 20s,
# MAV_STATE at 8), so an aircraft from a newer firmware with a type this
# corvus_link does not know yet still connects.
_HEARTBEAT_LIMITS: tuple[tuple[str, int], ...] = tuple(_receive.HEARTBEAT_LIMITS)


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
    return _receive.is_plausible_heartbeat(hb)


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
    return _receive.is_vehicle_heartbeat(hb)


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
    return _receive.sensor_health(msg, bit)


# MAV_SENSOR_ROTATION_PITCH_270: the sensor that looks straight down, the only
# one whose reading is a height. A forward or side sensor is an obstacle range.
_DOWNWARD_ORIENTATION: int = _receive.DOWNWARD_ORIENTATION
# A distance sensor that stops sending must not keep showing its last reading.
_RANGEFINDER_STALE_S = 3.0


def _rangefinder_distance_m(msg) -> float | None:
    """DISTANCE_SENSOR current_distance (cm) -> metres, None unless usable.

    Only the downward sensor counts, and only inside its own min/max: a
    reading at or beyond max_distance is "nothing in range", not a distance.
    """
    return _receive.rangefinder_distance_m(msg)


def _gps_altitude_m(msg) -> float | None:
    """GPS_RAW_INT alt (mm AMSL) -> metres, None without a 3D fix.

    The field is meaningless below a 3D fix (receivers send 0 or a stale
    value), and a 0 m reading that is not real would sit next to a true
    altitude in the top bar's card as if it were a measurement.
    """
    return _receive.gps_altitude_m(msg)


def _gps_accuracy_m(raw) -> float:
    """GPS_RAW_INT h_acc/v_acc (mm) -> metres, -1 when not reported.

    Both are MAVLink 2 extensions, so a MAVLink 1 frame or a receiver that
    does not fill them carries 0, which is "unknown" and not a perfect fix.
    """
    return _receive.gps_accuracy_m(raw)


def _prearm_ok(msg) -> bool | None:
    """SYS_STATUS -> True (ready to arm) / False (refused) / None (not reported)."""
    return _receive.prearm_ok(msg)


class MavlinkBridge(
    FlightCommandsMixin,
    LogTransferMixin,
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
        # Raw-frame tlog: one file per connect cycle and per flight, owned by
        # the bridge so a reconnect or a disarm closes the old file and starts
        # a fresh one. None = inactive.
        self._tlog: TlogWriter | None = None
        # Per-message statistics for the Analysis page's Live Messages. Plain
        # counters behind a lock: no thread, so nothing to tear down.
        self.message_scope = MessageScope()
        # The armed flag the last autopilot heartbeat carried, None until the
        # first one of a connect cycle. Only an armed to disarmed edge that
        # this cycle saw itself rolls the tlog over.
        self._tlog_armed: bool | None = None
        self._running = threading.Event()
        # Serialises start(), stop() and switch_connection(). Auto-connect's
        # watcher and an operator's connect both switch the link from their
        # own threads; interleaved, the second start() found the first one's
        # thread running and did nothing, leaving the bridge dialled to one
        # port while it reported the other. Re-entrant because
        # switch_connection() holds it across its own stop() and start().
        self._lifecycle_lock = threading.RLock()
        # Which start() the connect/receive thread belongs to. A thread that
        # stop() could not join in time (blocked opening a port, say) must not
        # carry on once start() has handed the link to a new thread: both would
        # open the same port and each read half the frames.
        self._run_generation = 0
        self._cycle = threading.local()
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
        # (major, minor, patch) from AUTOPILOT_VERSION, None until it arrives.
        # Some modes exist only from a given release on, and the dialect needs
        # this to know whether the connected one has them.
        self._firmware_version: tuple[int, int, int] | None = None
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
        # Raw frame -> when it last arrived, for _is_duplicate_frame.
        self._recent_frames: dict[bytes, float] = {}
        self._recent_frames_swept = 0.0
        self._duplicate_frames = 0
        # Message types whose handler raised, each logged once.
        self._dispatch_failed: set[str] = set()
        # Frames a second station asked to put on the uplink that carried our
        # own or the vehicle's system id; see inject_raw.
        self._inject_refused_warned = False
        self._send_lock = threading.Lock()
        self._statustext_chunks: dict[tuple[int, int, int], dict[str, Any]] = {}
        self._home_alt_amsl: float | None = None
        self._position_home_alt_amsl: float | None = None
        self._rangefinder_seen: float | None = None
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
        # The kill switch, as more than one line on a lossy radio tells it:
        # when the vehicle last said it was engaged, the readiness SYS_STATUS
        # last reported, and the preflight report in progress (when it began,
        # and whether it named the switch).
        self._kill_evidence_at = 0.0
        self._prearm_last: bool | None = None
        self._prearm_report_started_at = 0.0
        self._prearm_report_kill = False
        # The one vehicle this link flies, as the vehicle list and the map
        # see it (corvus/fleet.py). Read only: nothing in it decides how the
        # link is flown. One link is one vehicle; another autopilot heard on
        # it is ignored, and a second vehicle needs a radio of its own.
        self.fleet_tracker = FleetTracker(MAV_TYPE_MAP)
        # Which link this is to the fleet; "main" unless corvus.fleet opened it.
        self.fleet_link_id = MAIN_LINK
        # Other autopilots heard on this link, each told to the console once.
        self._ignored_systems: set[int] = set()
        # Each protocol owns its own state; see the mixin modules.
        self._init_param_state()
        self._init_param_metadata_state()
        self._init_ftp_state()
        self._init_mission_state()
        self._init_setup_state()
        self._init_shell_state()
        self._init_telemetry_state()
        self._init_remote_id_state()
        self._init_flight_state()
        # The receive path and the command transaction; see corvus_link.bridge.
        self._receive_engine = _receive.ReceiveEngine(sys.modules[__name__])
        # The connection cycle and the threads it runs on; see corvus_link.bridge.
        self._link_cycle = _receive.LinkCycle(sys.modules[__name__])
        # STATUSTEXT, vehicle events and the preflight report; see corvus_link.bridge.
        self._status_engine = _receive.StatusEngine(sys.modules[__name__])
        # The tlog: which file, when, and each frame to it; see corvus_link.bridge.
        self._tlog_session = _receive.TlogSession(sys.modules[__name__])
        # The bridge's own requests and signing; see corvus_link.bridge.
        self._request_engine = _receive.RequestEngine(sys.modules[__name__])

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
        if conn_str != self._conn_str:
            # Another link carries another vehicle.
            self.fleet_tracker.forget()
        self._conn_str = conn_str

    def controlled_system(self) -> int | None:
        """The system id this link is flying, or None while it flies none."""
        return self._target_system if self._connection_ready() else None

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

        corvus_link's ``mavlink_connection`` has no ``serial:`` handler — any
        string containing a colon is routed to mavudp, which rejects a serial
        device. We translate the user-facing ``serial:`` form to the bare
        device + ``baud`` kwarg that mavserial expects. Baud defaults to
        57600 (SiK Telemetry Radio V3 factory default). Never raises: on any
        parse problem the whole remainder is treated as the device path.
        """
        return split_serial_connection(conn_str)

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

    def switch_connection(self, conn_str: str) -> None:
        """Close the link and open it again on *conn_str*, as one step.

        Raises ValueError for an unusable spec before anything is torn down.
        """
        self.validate_connection(conn_str)
        with self._lifecycle_lock:
            self.stop()
            self.set_connection(conn_str)
            self.start()

    def start(self) -> None:
        with self._lifecycle_lock:
            self._start_locked()

    def _start_locked(self) -> None:
        """Start the connect/receive thread unless a healthy one runs; the
        lifecycle lock is held."""
        self._link_cycle.start_locked(self)

    def stop(self) -> None:
        with self._lifecycle_lock:
            self._stop_locked()

    def _stop_locked(self) -> None:
        self._link_cycle.stop_locked(self)

    def _superseded(self) -> bool:
        """Has a later start() replaced the thread this runs on?

        False on any thread that is not a connect/receive thread (the unit
        tests call _connect directly), so only an orphaned cycle stands down.
        """
        return self._link_cycle.superseded(self)

    def _run(self, generation: int | None = None) -> None:
        """The connect/receive cycle of one start(): connect, receive until the
        link ends, tear down and back off before the next attempt. A cycle a
        later start() replaced closes only what it opened and leaves."""
        self._link_cycle.run(self, generation)

    def _claim_serial_exclusive(self, device: str) -> None:
        """Take an exclusive advisory lock on the just-opened serial port.

        POSIX will let any number of processes open the same ``/dev/tty*`` and
        will report no error to any of them; it simply hands each read whatever
        bytes arrived first. Two ground stations on one USB link therefore each
        receive an arbitrary *half* of the MAVLink stream — attitude updating
        but position frozen, commands whose ACK lands in the other program —
        and neither shows a disconnect. It is the quietest way this application
        can be badly wrong, and it is exactly what a second launch used to do.

        ``flock`` is what pyserial's own ``exclusive=True`` uses; corvus_link
        does not pass that through, so it is applied here to the descriptor
        corvus_link opened. The lock lives on the file handle, so closing the
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
        return self._link_cycle.reconnect_delay(attempt)

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
        # The binding forms only. corvus_link's "udp:" defaults to input=True
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
        """Open the link and latch onto the aircraft on it: speak first, wait
        for an autopilot's heartbeat, pin the target, latch the dialect and
        the mode table, ask for version, home and (on serial) the streams,
        and start this session's tlog. Everything the previous connection
        taught the bridge is forgotten first."""
        self._link_cycle.connect(self)

    def _serial_open_error(self, exc: OSError) -> str:
        """What the operator can do about a serial port that would not open.

        "Unavailable" is true of every failure and helps with one of them. A
        port listed in the dropdown that will not open is, on a fresh Linux
        install, nearly always a user outside the dialout group, and on
        Windows a COM port another program holds: Windows opens COM ports
        exclusively and answers the second open with "access denied".
        """
        device = self.serial_device()
        text = str(exc)
        denied = (
            getattr(exc, "errno", None) == errno.EACCES
            or "PermissionError" in text
            or "Access is denied" in text
        )
        if denied and is_windows_com_port(device):
            return (
                f"{device} is in use by another program. Close the other "
                f"ground station and reconnect"
            )
        if denied:
            return (
                f"No permission to open {device}. On Linux, add your user to "
                f"the dialout group (sudo usermod -aG dialout $USER), then log "
                f"out and back in"
            )
        if getattr(exc, "errno", None) == errno.EBUSY:
            return (
                f"{device} is busy: another program has it open (on Linux "
                f"often ModemManager probing a new device)"
            )
        return f"serial device unavailable: {self._conn_str}"

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
        self._request_engine.apply_signing(self)

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
        return self._link_cycle.wait_vehicle_heartbeat(self, timeout)

    def _latch_dialect(self, autopilot_id: Any, mav_type: Any) -> bool:
        """Pick the flight-stack dialect this link will be flown with.

        Called from :meth:`_connect` and again from every HEARTBEAT, because a
        vehicle behind mavlink-router can be swapped for a different one
        without the socket ever closing. Rebuilding the mode table is left to
        :meth:`_build_mode_mapping`; this only records what we are talking to,
        which every decode below then reads, and returns whether it changed.
        """
        try:
            type_id = int(mav_type)
        except (TypeError, ValueError):
            type_id = 0
        dialect = autopilot_dialect.dialect_for(autopilot_id)
        changed = dialect is not self._dialect or type_id != self._mav_type_id
        if dialect is not self._dialect:
            # Another stack's release number says nothing about this one.
            self._firmware_version = None
        self._dialect = dialect
        self._mav_type_id = type_id
        if changed:
            logger.info(
                "Flight stack: %s (MAV_TYPE %s)", dialect.label, type_id,
            )
        return changed

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

        corvus_link answers ``mode_mapping()`` in two shapes, because the flight
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
        usable = self._dialect.adopt_live_mapping(
            mapping, self._mav_type_id, self._firmware_version,
        )
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
        builtin = self._dialect.mode_table(self._mav_type_id, self._firmware_version)
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
        self._link_cycle.start_gcs_heartbeat(self)

    def _send_gcs_heartbeat(self, conn: Any) -> bool:
        """Send one GCS heartbeat on *conn* now. Never raises.

        A binding UDP socket that has not heard from anybody yet has nowhere to
        send to; corvus_link drops the frame, which is the right outcome there.
        """
        return self._link_cycle.send_gcs_heartbeat(self, conn)

    def _gcs_hb_loop(self) -> None:
        # Runs while _running is set regardless of link_status: a degraded
        # link (A3) must NOT stop the GCS heartbeat, or the autopilot drops us
        # while we wait for its heartbeat to return (A7).
        self._link_cycle.gcs_hb_loop(self)

    # ------------------------------------------------------------------
    # Telemetry log (tlog) — one file per connect cycle and per flight (F2)
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
        return _receive.TlogSession(sys.modules[__name__]).next_path(log_dir)

    def _start_tlog(self) -> None:
        """Open a fresh tlog for this flight session.

        No-op when the bridge is not running (e.g. direct-_connect unit tests)
        so they stay hermetic. A log-dir permission error must never break the
        link: on failure the tlog is simply disabled for the session.
        """
        self._tlog_session.start(self)

    def _roll_tlog_on_disarm(self, armed: bool) -> None:
        """Start the next tlog when the vehicle disarms after a flight.

        Two sorties on one battery swap used to share a file, so a log could
        not be picked by flight. The disarm heartbeat itself is already in the
        closing file (frames are logged before they are dispatched), so that
        file holds the whole flight and the new one opens on the ground. The
        firmware version is asked for again so the new file carries its own
        AUTOPILOT_VERSION for the flight review.
        """
        self._tlog_session.roll_on_disarm(self, armed)

    def _stop_tlog(self) -> None:
        """Stop and close the current tlog if active; idempotent."""
        self._tlog_session.stop(self)

    def _write_tlog(self, msg: Any) -> None:
        """Log a raw MAVLink frame if a tlog is active; never breaks the link.

        Also hands the same frame to the forwarder when one is attached: both
        consumers want the bytes exactly as they came off the wire, and both
        are non-blocking appends, so taking the msgbuf once serves both.
        """
        self._tlog_session.write(self, msg)

    def local_udp_address(self) -> tuple[str, int] | None:
        """Where a UDP-listening vehicle link is bound, else None.

        The forwarder asks, so it never mirrors into this socket.
        """
        conn = self._conn
        if conn is None or not getattr(conn, "udp_server", False):
            return None
        try:
            host, port = conn.port.getsockname()[:2]
        except (AttributeError, OSError, TypeError, ValueError):
            return None
        return str(host), int(port)

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

        A frame under Corvus' own system id, or under the vehicle's, is
        refused: no second station speaks as either, and both are what a
        forwarding loop hands back, which would replay Corvus' commands and
        the aircraft's own telemetry onto the uplink.
        """
        conn = self._conn
        if conn is None or not frame:
            return False
        source = frame_source_system(frame)
        if source in (GCS_SYSTEM_ID, self._target_system):
            if not self._inject_refused_warned:
                self._inject_refused_warned = True
                logger.warning(
                    "not forwarding frames from system %d to the vehicle: that "
                    "is %s", source,
                    "Corvus' own id" if source == GCS_SYSTEM_ID else "the vehicle's id",
                )
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
        return self._request_engine.stream_rates(self)

    def _request_streams(self) -> None:
        """Send the deprecated REQUEST_DATA_STREAM set, at most once per link.

        The legacy path. On serial it runs from _connect() so telemetry starts
        without waiting on a command round trip; on UDP it runs only if
        _request_message_intervals() finds the modern mechanism missing. The
        _request_sent latch is what lets that second caller retry per message
        without sending the whole set again, and _connect() clears it so the
        next connection makes the decision afresh.
        """
        self._request_engine.request_streams(self)

    def _message_intervals(self) -> dict[int, int]:
        """Per-message MAV_CMD_SET_MESSAGE_INTERVAL targets in microseconds.

        Modern PX4 (v1.16-v1.18) rate-control, and the only mechanism PX4 has
        ever answered; REQUEST_DATA_STREAM (above) is the fallback for stacks
        that do not, which in practice means ArduPilot. Radio links are
        throttled to fit a 57 kbps SiK radio; a USB cable is not. VIBRATION
        is on-demand only (not listed here).
        """
        return self._request_engine.message_intervals(self)

    def _interruptible_sleep(self, seconds: float) -> None:
        """Sleep that returns early when _stop_event is set.

        Sleeps in small slices via time.sleep so tests that monkeypatch
        ``corvus.mavlink_bridge.time.sleep`` still short-circuit the whole
        delay, while stop() can wake the daemon worker within one slice
        (~0.1 s). Used by the deferred interval worker and the version retry
        (BUG 1, 12).
        """
        self._link_cycle.interruptible_sleep(self, seconds)

    def _schedule_message_intervals(self) -> None:
        """Defer _request_message_intervals until the receive loop is running.

        MAV_CMD_SET_MESSAGE_INTERVAL is ACK-confirmed and the ACK is
        dispatched by _receive_loop(). Requesting intervals from _connect()
        (before the loop exists) made every ACK time out. This spawns a
        one-shot daemon that fires once the loop is draining the socket, so
        COMMAND_ACKs are actually delivered (BUG 1).
        """
        self._link_cycle.schedule_message_intervals(self)

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
        self._request_engine.check_gcs_authority(self)

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

        Quietly, too: a refusal here is an answer the loop acts on, not a
        failure the operator did anything to cause. Reported the way an
        operator's own request is, every connect to a PX4 put a critical
        "Set message interval failed" on the status bar for MISSION_CURRENT,
        which PX4 sends from its mission manager rather than as a stream.
        """
        self._request_engine.request_message_intervals(self)

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
        self._request_engine.request_home(self)

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
        self._request_engine.request_version(self)

    def _schedule_version_retry(self) -> None:
        """Send one delayed AUTOPILOT_VERSION retry if the first got no reply.

        PX4 v1.18.0-alpha1 SITL ACKs the capabilities command with
        MAV_RESULT_UNSUPPORTED, so AUTOPILOT_VERSION never arrives; a single
        retry ~4s after connect covers slow/lossy responders without looping
        or spamming the link. Daemon thread; checks _running/_conn/_stop_event
        so it stays silent after shutdown, and the 4s wait uses
        _interruptible_sleep so stop() can wake it and join it promptly (BUG 12).
        """
        self._link_cycle.schedule_version_retry(self)

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
        return _receive.decode_git_hash(raw)

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
        self._receive_engine.receive_loop(self)

    def _process_frame(self, msg: Any) -> None:
        """Log, count and dispatch one received frame. Never raises.

        A handler that fails on a message is a bug in that handler, not a read
        error. Counted as one, it cost the loop a backoff sleep per frame: one
        handler failing on a 50 Hz message held the loop to 20 frames a
        second, the socket backed up, and the heartbeats queued behind it
        dropped the link. Logged once per message type instead.
        """
        self._receive_engine.process_frame(self, msg)

    def _is_duplicate_frame(self, msg: Any) -> bool:
        """Has this exact frame already arrived in the last few hundred ms?

        A copy is dropped before anything reads it. When a link loops, an old
        HEARTBEAT decoded again flips the mode and armed state back to what
        they were, and an old COMMAND_ACK confirms a command the vehicle never
        acknowledged. It also stops the copy being mirrored again, which is
        what keeps a loop going.
        """
        return self._receive_engine.is_duplicate_frame(self, msg)

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
        self._receive_engine.note_boot_time(self, boot_ms)

    def _mark_degraded_once(self) -> None:
        """Transition to degraded link quality once per connection cycle (A3).

        Guarded so the receive loop does not spam the store every iteration
        while heartbeats are late but not yet drop-timeout-lost.
        """
        self._receive_engine.mark_degraded_once(self)

    _CONSOLE_MSGS = frozenset({"COMMAND_ACK"})

    def _observe_fleet(self, msg: Any, name: str) -> None:
        """Hand a frame of the vehicle this link flies to the vehicle list.

        Frames of any other system are not the list's: one link is one
        vehicle. Never raises, never blocks.
        """
        self._receive_engine.observe_fleet(self, msg, name)

    def _note_ignored_system(self, hb: Any, system: int) -> None:
        """Tell the console, once per system, that another autopilot is ignored."""
        self._receive_engine.note_ignored_system(self, hb, system)

    def _dispatch(self, msg: Any) -> None:
        """Route one message to its handler.

        COMMAND_ACK, the mission handshake, the shell, the SiK radio and GNSS
        integrity check their own source; everything that writes the
        aircraft's own state must come from this vehicle, and its mode, arm
        state and versions from the autopilot component, so a ground station,
        a companion computer or a gimbal on the same link cannot overwrite it.
        """
        self._receive_engine.dispatch(self, msg)

    def _is_from_vehicle(self, msg: Any) -> bool:
        """Did *msg* come from the aircraft this bridge is flying?

        Source system 0 means the frame carries no sender id — the synthetic
        messages unit tests build, and corvus_link's own BAD_DATA — and is
        accepted: this rejects an identified *other* node, it does not demand
        provenance a frame never had.
        """
        return self._receive_engine.is_from_vehicle(self, msg)

    def _is_from_autopilot(self, msg: Any) -> bool:
        """Tighter than :meth:`_is_from_vehicle`: the flight controller itself.

        A gimbal, a camera and a companion computer all heartbeat under the
        vehicle's system id, each with its own MAV_TYPE and an empty base_mode.
        Only the autopilot's component speaks for the airframe's mode and arm
        state, so the HEARTBEAT branch asks for that one.
        """
        return self._receive_engine.is_from_autopilot(self, msg)

    def _ack_is_for_us(self, msg: Any) -> bool:
        # Snapshot: stop()/reconnect can clear _conn between the guard and the
        # source_system/source_component reads below.
        return self._receive_engine.ack_is_for_us(self, msg)

    def _cancel_pending_commands(self) -> None:
        self._receive_engine.cancel_pending_commands(self)

    def _link_send(self, conn: Any, method: str, *args: Any) -> bool:
        """Send one message on *conn* under the send lock, addressed to the vehicle.

        False when *conn* is no longer this bridge's link (a reconnect swapped
        it); a send that fails raises. The protocols in corvus_link send
        through this.
        """
        with self._send_lock:
            if conn is None or conn is not self._conn:
                return False
            getattr(conn.mav, method)(self._target_system, self._target_component, *args)
            return True

    def _connection_ready(self) -> bool:
        return self._receive_engine.connection_ready(self)

    def _handle_statustext(self, msg: Any) -> None:
        """Reassemble MAVLink2 STATUSTEXT chunks and publish complete messages."""
        self._status_engine.handle_statustext(self, msg)

    def _cleanup_statustext_chunks(self, now: float | None = None) -> None:
        self._status_engine.cleanup_statustext_chunks(self, now)

    def _publish_statustext(self, text: str, severity: int) -> None:
        # PX4 ends a STATUSTEXT that doubles one of its events with a tab (a
        # marker for stations that show the event instead). Corvus shows the
        # text, and without the strip the same message is two warnings.
        self._status_engine.publish_statustext(self, text, severity)

    def _note_vehicle_event(self, event: VehicleEvent, level: str) -> None:
        """Latch the kill switch a line reports and put the line on the board."""
        self._status_engine.note_vehicle_event(self, event, level)

    def _release_kill_switch(self) -> None:
        """The switch is off: clear it, and the lockdown blamed on it.

        PX4 reports the kill switch as flight termination in a heartbeat that
        comes once a second, so waiting for the next one had the bar read
        TERMINATED for up to a second after every release. A termination that
        really is something else is back with that heartbeat.
        """
        self._status_engine.release_kill_switch(self)

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
        self._status_engine.note_prearm_failure(self, reason)

    def _clear_prearm_reasons(self) -> None:
        self._status_engine.clear_prearm_reasons(self)

    def _note_prearm_state(self, prearm: bool | None) -> None:
        """React to SYS_STATUS's verdict: clear the reasons, or go and get them."""
        self._status_engine.note_prearm_state(self, prearm)

    def _check_kill_released_by_report(self, now: float, snapshot: dict[str, Any]) -> None:
        """Release the switch when a whole report asked for since says nothing of it.

        Only on a stack whose report names every failing check, only while
        disarmed (the checks do not run armed), and only for a report that
        began after the last request, which was itself made after the switch
        was last reported engaged. A report still arriving is not judged.
        """
        self._status_engine.check_kill_released_by_report(self, now, snapshot)

    def _request_prearm_report(self) -> None:
        """Ask for the preflight report now. Fire and forget, like _request_home.

        Runs on the receive thread, so it cannot wait for its own ACK; whether
        it worked is judged by the reasons arriving.
        """
        self._status_engine.request_prearm_report(self)

    def _store_update_warning(self, text: str, level: str, event: str | None = None) -> None:
        """Atomically merge a warning into the vehicle state."""
        self._store.merge_warning(text, level, event=event)

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
        ordered = self._dialect.available_modes(self._mav_type_id, self._firmware_version)
        if self._mode_mapping:
            known = [name for name in ordered if name in self._mode_mapping]
            return known + sorted(self._mode_mapping.difference(known))
        if self._modes_unsupported:
            return []
        return ordered

    def get_mode_labels(self) -> dict[str, str]:
        """What the operator reads for each mode get_available_modes() offers.

        The connected stack's words ("POSITION" for PX4's POSCTL). The keys stay
        the names a mode change sends.
        """
        return {name: self._dialect.mode_label(name) for name in self.get_available_modes()}

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
        payload.update(self.mission_capabilities())
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
        level = "warning" if result in (-3, -2, -1, 1) else "critical"
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
        # The engine registers the waiter; one slot per command id is safe
        # because every waiter registers under this lock.
        with self._operation_lock:
            return self._receive_engine.send_command_and_wait(
                self, command, params, timeout, retries, accept_in_progress, interim,
            )

    def _await_final_ack(self, pending: _PendingAck, timeout: float) -> int:
        """The vehicle said IN_PROGRESS: wait for its verdict, never resend.

        A resend of a command the vehicle is already carrying out can start it
        over (a second calibration, a second takeoff target), which is what the
        retry loop used to do whenever the verdict took longer than one ACK
        timeout. IN_PROGRESS is returned if the verdict does not come.
        """
        return self._receive_engine.await_final_ack(self, pending, timeout)

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
            return self._receive_engine.send_command_int_and_wait(
                self, command, frame, params, x, y, z, timeout, retries,
            )

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

    # On-board log download (MAVLink LOG_*): LogTransferMixin in
    # corvus/mavlink_logs.py.

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

    def _set_message_interval(
        self, msg_id: int, interval_us: int, quiet: bool = False,
    ) -> tuple[bool, int]:
        """The same request, with the raw COMMAND_ACK result beside the verdict.

        The connect-time batch needs the result code and not just the bool:
        it is what separates a firmware that has never heard of
        MAV_CMD_SET_MESSAGE_INTERVAL (MAV_RESULT_UNSUPPORTED) from one that
        refuses a single message it does not carry (MAV_RESULT_DENIED), and
        only the first is grounds for falling back to REQUEST_DATA_STREAM.
        -1 is no ACK at all, -2 no usable link. *quiet* keeps the outcome off
        the console and the status bar; the reason is still the last error.
        """
        with self._operation_lock:
            return tuple(self._request_engine.set_message_interval(self, msg_id, interval_us, quiet))

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
