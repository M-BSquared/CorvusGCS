"""ExpressLRS telemetry: where the aircraft is, seen through the RC link (dormant).

**Not wired.** Nothing in the app imports this module: no route, no bridge
hook, no frontend control. :data:`ENABLED` is ``False`` and
:meth:`ElrsTelemetryReader.start` refuses to run while it is. It exists as
code to build on, with tests, so that switching it on later is a wiring job
rather than a protocol job. ``tests/test_elrs.py`` fails if something starts
importing it without that decision being made on purpose.

What it does, once it is wired:

* **See the aircraft.** An ExpressLRS receiver sends CRSF telemetry down the
  RC link. The flight controller fills it (PX4 ``crsf_rc``, ArduPilot
  ``RCx_PROTOCOL`` CRSF with ``RC_OPTIONS`` telemetry), and the handset or TX
  module passes it on: EdgeTX's telemetry mirror on an AUX serial port, a TX
  module's USB serial, or the ELRS Backpack. :class:`CrsfParser` turns that
  byte stream into frames, :class:`ElrsTelemetry` folds the frames into a
  snapshot whose keys match the Vehicle State Store (``position``,
  ``altitude_amsl``, ``heading``, ``battery_voltage``, ...), so a map marker
  or a fleet entry can take it as it is.
* **Fly to this point.** CRSF has no command for it. Telemetry flows down,
  sticks flow up, and nothing else. The one way to send a position over an
  ExpressLRS link is its MAVLink mode (ELRS 3.5 and later), where the link
  carries MAVLink itself and the ordinary bridge connects to it like to any
  radio. :func:`plan_fly_to` builds the ``MAV_CMD_DO_REPOSITION`` such a link
  would carry, asking the dialect layer how its altitude is sent, and answers
  "not available" with the reason on a plain CRSF link.

Wire format (CRSF, as ExpressLRS 3.x speaks it): ``[address][length][type]
[payload][crc]``, where ``length`` counts type, payload and CRC, the whole
frame is at most 64 bytes, multi-byte fields are big-endian, and the CRC is
CRC-8/DVB-S2 over type and payload.
"""
from __future__ import annotations

import logging
import sys
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Protocol

from corvus_link import elrs as _elrs

from . import autopilot as autopilot_dialect  # noqa: F401 - read by the Rust engine

logger = logging.getLogger("corvus.elrs")

# The switch. Wiring this module in means setting it to True in the same
# change that adds the route, the lifecycle hook and the frontend control.
ENABLED = False

# Frame addresses a CRSF stream may start a frame with. Broadcast (0x00) is
# left out on purpose: telemetry never uses it, and a zero byte inside a
# payload would otherwise pass for a frame start and stall the parser.
ADDR_FLIGHT_CONTROLLER = 0xC8
ADDR_RADIO_TRANSMITTER = 0xEA
ADDR_CRSF_RECEIVER = 0xEC
ADDR_CRSF_TRANSMITTER = 0xEE
ADDR_ELRS_LUA = 0xEF
SYNC_ADDRESSES = frozenset({
    ADDR_FLIGHT_CONTROLLER, ADDR_RADIO_TRANSMITTER,
    ADDR_CRSF_RECEIVER, ADDR_CRSF_TRANSMITTER, ADDR_ELRS_LUA,
})

MAX_FRAME_LEN = 64
# type + crc: the shortest a frame's length byte can be.
MIN_LENGTH_FIELD = 2

TYPE_GPS = 0x02
TYPE_VARIO = 0x07
TYPE_BATTERY = 0x08
TYPE_BARO_ALTITUDE = 0x09
TYPE_LINK_STATISTICS = 0x14
TYPE_RC_CHANNELS = 0x16
TYPE_ATTITUDE = 0x1E
TYPE_FLIGHT_MODE = 0x21

# A position older than this is drawn as lost, like a vehicle whose heartbeat
# stopped (corvus.fleet.STALE_S).
STALE_S = 5.0

# ExpressLRS telemetry is slow: at the common 1:64 ratio a GPS frame arrives a
# few times a second. The reader still wakes this often to notice a stop.
READ_TIMEOUT_S = 0.2
READ_CHUNK = 256
JOIN_TIMEOUT_S = 2.0


def crc8_dvb_s2(data: bytes) -> int:
    """CRC-8/DVB-S2 (polynomial 0xD5), the checksum every CRSF frame carries."""
    return _elrs.crc8_dvb_s2(data)


@dataclass(frozen=True)
class CrsfFrame:
    """One checked frame: who it was addressed to, its type and its payload."""

    address: int
    type: int
    payload: bytes


def build_frame(frame_type: int, payload: bytes, address: int = ADDR_FLIGHT_CONTROLLER) -> bytes:
    """Encode one CRSF frame. Used by the tests, and by a future uplink."""
    return _elrs.build_frame(frame_type, payload, address)


CrsfParser = _elrs.CrsfParser



decode_gps = _elrs.decode_gps


decode_vario = _elrs.decode_vario


decode_battery = _elrs.decode_battery


decode_baro_altitude = _elrs.decode_baro_altitude


decode_link_statistics = _elrs.decode_link_statistics


decode_attitude = _elrs.decode_attitude


decode_flight_mode = _elrs.decode_flight_mode


DECODERS: dict[int, Callable[[bytes], dict[str, Any] | None]] = {
    TYPE_GPS: decode_gps,
    TYPE_VARIO: decode_vario,
    TYPE_BATTERY: decode_battery,
    TYPE_BARO_ALTITUDE: decode_baro_altitude,
    TYPE_LINK_STATISTICS: decode_link_statistics,
    TYPE_ATTITUDE: decode_attitude,
    TYPE_FLIGHT_MODE: decode_flight_mode,
}


decode_frame = _elrs.decode_frame


class ElrsTelemetry:
    """What the RC link has told us about the aircraft, thread-safe.

    :meth:`snapshot` uses the Vehicle State Store's keys where one exists, so
    a consumer that draws a vehicle from the store draws this one unchanged.
    ``source`` says where it came from: a position heard over the RC link is a
    fallback with no command path behind it, and must never be mistaken for a
    MAVLink vehicle that can be flown.
    """

    def __init__(self, clock: Callable[[], float] = time.monotonic) -> None:
        self._clock = clock
        self._lock = threading.Lock()
        self._parser = CrsfParser()
        self._data: dict[str, Any] = {
            "source": "elrs",
            "position": None,
            "altitude_amsl": 0.0,
            "altitude_rel": None,
            "heading": 0.0,
            "groundspeed": 0.0,
            "vspeed": 0.0,
            "pitch": 0.0,
            "roll": 0.0,
            "yaw": 0.0,
            "satellites": 0,
            "battery_voltage": 0.0,
            "battery_current": 0.0,
            "battery_used_mah": 0,
            "battery_percent": -1,
            "mode_label": "",
            "link": {},
        }
        self._last_position: float | None = None
        self._last_frame: float | None = None

    def feed(self, data: bytes) -> int:
        """Parse raw bytes from the link and apply every frame. Returns the frame count."""
        return _engine.feed(self, data)

    def apply(self, frame: CrsfFrame) -> None:
        """Apply one already parsed frame."""
        with self._lock:
            self._apply_locked(frame)

    def _apply_locked(self, frame: CrsfFrame) -> None:
        _engine.apply_locked(self, frame)

    def snapshot(self) -> dict[str, Any]:
        """A copy of the current state, with ``stale`` set from the clock."""
        return _engine.snapshot(self)


class ByteSource(Protocol):
    """The part of a ``serial.Serial`` the reader uses."""

    def read(self, size: int = 1) -> bytes: ...

    def close(self) -> None: ...


class ElrsTelemetryReader:
    """Reads a CRSF stream on a background thread into an :class:`ElrsTelemetry`.

    *open_source* returns something with ``read`` and ``close``, typically
    ``serial.Serial(port, 115200, timeout=READ_TIMEOUT_S)`` for an EdgeTX
    telemetry mirror. The reader owns that handle: :meth:`stop` closes it and
    joins the thread, so it fits the shutdown path the same way every other
    link does once it is wired into it.
    """

    def __init__(
        self,
        open_source: Callable[[], ByteSource],
        telemetry: ElrsTelemetry | None = None,
        on_update: Callable[[dict[str, Any]], None] | None = None,
    ) -> None:
        self._open_source = open_source
        self.telemetry = telemetry or ElrsTelemetry()
        self._on_update = on_update
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._source: ByteSource | None = None
        self._source_lock = threading.Lock()

    @property
    def running(self) -> bool:
        """True while the reader thread is alive."""
        return self._thread is not None and self._thread.is_alive()

    def start(self) -> None:
        """Open the source and start reading. Refused while :data:`ENABLED` is False."""
        _engine.start(self)

    def stop(self, timeout: float = JOIN_TIMEOUT_S) -> None:
        """Stop reading, close the source and join the thread. Safe to call twice."""
        _engine.stop(self, timeout)

    def _close_source(self) -> None:
        _engine.close_source(self)

    def _run(self) -> None:
        _engine.run(self)


# How an ExpressLRS link carries data. "crsf" is the default every ELRS
# receiver ships with; "mavlink" is ELRS 3.5's MAVLink mode.
LINK_MODES = ("crsf", "mavlink")


@dataclass(frozen=True)
class FlyToPlan:
    """What "Fly to this point" would send over an ExpressLRS link, or why it cannot.

    ``available`` False carries the reason the control shows, greyed, in
    place of a missing button (see the capability rule in AGENTS.md).
    """

    available: bool
    reason: str = ""
    command: int = 0
    frame: int = 0
    params: tuple[float, float, float, float] = (0.0, 0.0, 0.0, 0.0)
    x: int = 0
    y: int = 0
    z: float = 0.0
    extra: dict[str, Any] = field(default_factory=dict)


MAV_CMD_DO_REPOSITION = 192
MAV_FRAME_GLOBAL = 0
MAV_FRAME_GLOBAL_RELATIVE_ALT = 3
DO_REPOSITION_CHANGE_MODE = 1


def plan_fly_to(
    link_mode: str,
    stack: str,
    mav_type: int,
    lat: Any,
    lon: Any,
    alt_agl: Any,
    home_alt_amsl: float | None = None,
) -> FlyToPlan:
    """Plan a single "Fly to this point" for an aircraft reached over ExpressLRS.

    Over plain CRSF the answer is always no: the protocol has no frame that
    carries a position to the aircraft. Over ELRS's MAVLink mode the plan is
    the same ``MAV_CMD_DO_REPOSITION`` the mission client sends over any other
    radio, with its altitude frame taken from the dialect layer rather than
    decided here: PX4 reads AMSL and needs *home_alt_amsl*, ArduPilot takes
    the altitude relative to home.

    Nothing is sent. Once wired, a MAVLink-mode link is an ordinary
    ``MavlinkBridge`` connection and its own fly-to path applies; this
    function is what a CRSF-only link would ask to grey the control.
    """
    return _engine.plan_fly_to(link_mode, stack, mav_type, lat, lon, alt_agl, home_alt_amsl)


_engine = _elrs.ElrsEngine(sys.modules[__name__])
