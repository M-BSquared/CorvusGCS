"""ExpressLRS telemetry: where the aircraft is, seen through the RC link (dormant).

**Not wired.** This is the CRSF reader, and only it. ExpressLRS in MAVLink mode
is a different path and is live: it is an ordinary serial MAVLink link with
``?link=elrs&dtr=off&rts=off`` on its connection string (see
:mod:`corvus.serial_open` and :mod:`corvus.serial_ports`), and a Betaflight
flight controller behind it is the ``betaflight`` dialect. Nothing in the app
imports this module: no route, no bridge
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
import math
import struct
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Protocol

from . import autopilot as autopilot_dialect

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
    crc = 0
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = ((crc << 1) ^ 0xD5) & 0xFF if crc & 0x80 else (crc << 1) & 0xFF
    return crc


@dataclass(frozen=True)
class CrsfFrame:
    """One checked frame: who it was addressed to, its type and its payload."""

    address: int
    type: int
    payload: bytes


def build_frame(frame_type: int, payload: bytes, address: int = ADDR_FLIGHT_CONTROLLER) -> bytes:
    """Encode one CRSF frame. Used by the tests, and by a future uplink."""
    body = bytes([frame_type]) + bytes(payload)
    if len(body) + 3 > MAX_FRAME_LEN:
        raise ValueError(f"CRSF frame too long ({len(body) + 3} > {MAX_FRAME_LEN} bytes)")
    return bytes([address, len(body) + 1]) + body + bytes([crc8_dvb_s2(body)])


class CrsfParser:
    """Turns a CRSF byte stream into frames, resynchronising on garbage.

    A telemetry mirror is a plain serial line: it starts mid-frame, drops
    bytes and carries noise. Anything that is not a well-formed frame with a
    matching CRC costs one byte of resync, never an exception.
    """

    def __init__(self) -> None:
        self._buf = bytearray()
        self.frames_ok = 0
        self.crc_errors = 0

    def feed(self, data: bytes) -> list[CrsfFrame]:
        """Add *data* and return every complete frame it finished."""
        self._buf.extend(data)
        frames: list[CrsfFrame] = []
        buf = self._buf
        while len(buf) >= 2:
            length = buf[1]
            if buf[0] not in SYNC_ADDRESSES or not MIN_LENGTH_FIELD <= length <= MAX_FRAME_LEN - 2:
                del buf[0]
                continue
            total = length + 2
            if len(buf) < total:
                break
            body = bytes(buf[2:total - 1])
            if crc8_dvb_s2(body) != buf[total - 1]:
                self.crc_errors += 1
                del buf[0]
                continue
            frames.append(CrsfFrame(buf[0], body[0], body[1:]))
            self.frames_ok += 1
            del buf[:total]
        return frames


def decode_gps(payload: bytes) -> dict[str, Any] | None:
    """GPS (0x02): position, ground speed, course, altitude and satellites.

    Altitude travels as metres plus 1000, so -1000 m to 64535 m fit an
    unsigned field. A position of exactly 0, 0 is what a flight controller
    sends before its first fix, and is reported as no fix.
    """
    if len(payload) < 15:
        return None
    lat, lon, speed, course, alt, sats = struct.unpack(">iiHHHB", payload[:15])
    fix = not (lat == 0 and lon == 0)
    return {
        "lat": lat / 1e7,
        "lon": lon / 1e7,
        "groundspeed": speed / 36.0,  # km/h * 10 -> m/s
        "heading": (course / 100.0) % 360.0,
        "altitude_amsl": float(alt - 1000),
        "satellites": sats,
        "fix": fix,
    }


def decode_vario(payload: bytes) -> dict[str, Any] | None:
    """Vario (0x07): climb rate in cm/s."""
    if len(payload) < 2:
        return None
    (climb,) = struct.unpack(">h", payload[:2])
    return {"vspeed": climb / 100.0}


def decode_battery(payload: bytes) -> dict[str, Any] | None:
    """Battery (0x08): voltage, current, mAh drawn and percent remaining."""
    if len(payload) < 8:
        return None
    voltage, current = struct.unpack(">HH", payload[:4])
    used = int.from_bytes(payload[4:7], "big")
    return {
        "battery_voltage": voltage / 10.0,
        "battery_current": current / 10.0,
        "battery_used_mah": used,
        "battery_percent": min(100, payload[7]),
    }


def decode_baro_altitude(payload: bytes) -> dict[str, Any] | None:
    """Barometric altitude (0x09), relative to where the flight controller armed.

    The 16-bit field packs two ranges: with the top bit clear it is decimetres
    offset by 10000 (-1000 m to 2276.7 m), with it set it is whole metres.
    The optional vertical speed that follows is not decoded; VARIO carries it.
    """
    if len(payload) < 2:
        return None
    (packed,) = struct.unpack(">H", payload[:2])
    if packed & 0x8000:
        alt = float(packed & 0x7FFF)
    else:
        alt = (packed - 10000) / 10.0
    return {"altitude_rel": alt}


def decode_link_statistics(payload: bytes) -> dict[str, Any] | None:
    """Link statistics (0x14): RSSI, link quality and SNR in both directions.

    RSSI travels as positive dBm and is negated here, so the snapshot holds
    the number an operator reads on the handset (-70, not 70).
    """
    if len(payload) < 10:
        return None
    (rssi1, rssi2, lq, snr, antenna, rf_mode, tx_power,
     down_rssi, down_lq, down_snr) = struct.unpack(">BBBbBBBBBb", payload[:10])
    return {
        "uplink_rssi_1": -rssi1,
        "uplink_rssi_2": -rssi2,
        "uplink_lq": lq,
        "uplink_snr": snr,
        "active_antenna": antenna,
        "rf_mode": rf_mode,
        "uplink_tx_power": tx_power,
        "downlink_rssi": -down_rssi,
        "downlink_lq": down_lq,
        "downlink_snr": down_snr,
    }


def decode_attitude(payload: bytes) -> dict[str, Any] | None:
    """Attitude (0x1E): pitch, roll and yaw in radians * 10000.

    Returned in degrees, like the store. Yaw is wrapped to 0 to 360 so it can
    stand in for the heading when the GPS frame has none yet.
    """
    if len(payload) < 6:
        return None
    pitch, roll, yaw = struct.unpack(">hhh", payload[:6])
    return {
        "pitch": math.degrees(pitch / 10000.0),
        "roll": math.degrees(roll / 10000.0),
        "yaw": math.degrees(yaw / 10000.0) % 360.0,
    }


def decode_flight_mode(payload: bytes) -> dict[str, Any] | None:
    """Flight mode (0x21): a NUL-terminated name, as the flight controller spells it.

    PX4 and ArduPilot fill it with their own short labels, and Betaflight
    appends ``*`` when disarmed. The name is shown, never sent back as a mode.
    """
    name = payload.split(b"\x00", 1)[0].decode("ascii", "replace").strip()
    return {"mode_label": name} if name else None


DECODERS: dict[int, Callable[[bytes], dict[str, Any] | None]] = {
    TYPE_GPS: decode_gps,
    TYPE_VARIO: decode_vario,
    TYPE_BATTERY: decode_battery,
    TYPE_BARO_ALTITUDE: decode_baro_altitude,
    TYPE_LINK_STATISTICS: decode_link_statistics,
    TYPE_ATTITUDE: decode_attitude,
    TYPE_FLIGHT_MODE: decode_flight_mode,
}


def decode_frame(frame: CrsfFrame) -> dict[str, Any] | None:
    """Decode one frame into snapshot fields, or None for a type not shown."""
    decoder = DECODERS.get(frame.type)
    if decoder is None:
        return None
    try:
        return decoder(frame.payload)
    except (struct.error, ValueError):
        return None


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
        with self._lock:
            frames = self._parser.feed(data)
            for frame in frames:
                self._apply_locked(frame)
            return len(frames)

    def apply(self, frame: CrsfFrame) -> None:
        """Apply one already parsed frame."""
        with self._lock:
            self._apply_locked(frame)

    def _apply_locked(self, frame: CrsfFrame) -> None:
        fields = decode_frame(frame)
        if not fields:
            return
        now = self._clock()
        self._last_frame = now
        if frame.type == TYPE_GPS:
            if fields.pop("fix"):
                self._data["position"] = [fields.pop("lat"), fields.pop("lon")]
                self._last_position = now
            else:
                fields.pop("lat")
                fields.pop("lon")
                fields.pop("altitude_amsl")
                fields.pop("heading")
            self._data.update(fields)
        elif frame.type == TYPE_LINK_STATISTICS:
            self._data["link"] = fields
        else:
            self._data.update(fields)

    def snapshot(self) -> dict[str, Any]:
        """A copy of the current state, with ``stale`` set from the clock."""
        with self._lock:
            out = dict(self._data)
            out["link"] = dict(self._data["link"])
            if out["position"] is not None:
                out["position"] = list(out["position"])
            last = self._last_position
            out["stale"] = last is None or self._clock() - last > STALE_S
            out["has_position"] = out["position"] is not None
            out["frames_ok"] = self._parser.frames_ok
            out["crc_errors"] = self._parser.crc_errors
            return out


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
        if not ENABLED:
            raise RuntimeError("ExpressLRS telemetry is not enabled in this build")
        if self.running:
            return
        self._stop.clear()
        source = self._open_source()
        with self._source_lock:
            self._source = source
        self._thread = threading.Thread(target=self._run, name="corvus-elrs", daemon=True)
        self._thread.start()

    def stop(self, timeout: float = JOIN_TIMEOUT_S) -> None:
        """Stop reading, close the source and join the thread. Safe to call twice."""
        self._stop.set()
        self._close_source()
        thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout)
        self._thread = None

    def _close_source(self) -> None:
        with self._source_lock:
            source, self._source = self._source, None
        if source is not None:
            try:
                source.close()
            except Exception:
                logger.debug("Closing the ExpressLRS source failed", exc_info=True)

    def _run(self) -> None:
        while not self._stop.is_set():
            with self._source_lock:
                source = self._source
            if source is None:
                break
            try:
                data = source.read(READ_CHUNK)
            except Exception:
                if not self._stop.is_set():
                    logger.warning("ExpressLRS telemetry source failed", exc_info=True)
                break
            if not data:
                continue
            if self.telemetry.feed(data) and self._on_update is not None:
                try:
                    self._on_update(self.telemetry.snapshot())
                except Exception:
                    logger.exception("ExpressLRS update callback failed")
        self._close_source()


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
    if link_mode not in LINK_MODES:
        return FlyToPlan(False, f"Unknown ExpressLRS link mode: {link_mode!r}")
    if link_mode == "crsf":
        return FlyToPlan(
            False,
            "Not available over CRSF telemetry. Switch the ExpressLRS link to "
            "MAVLink mode (ELRS 3.5 or later) to send a position.",
        )
    try:
        lat_f, lon_f, alt_f = float(lat), float(lon), float(alt_agl)
    except (TypeError, ValueError):
        return FlyToPlan(False, "Point is not a number")
    if isinstance(lat, bool) or isinstance(lon, bool) or isinstance(alt_agl, bool):
        return FlyToPlan(False, "Point is not a number")
    if not all(math.isfinite(v) for v in (lat_f, lon_f, alt_f)):
        return FlyToPlan(False, "Point is not a number")
    if not (-90.0 <= lat_f <= 90.0 and -180.0 <= lon_f <= 180.0):
        return FlyToPlan(False, "Point is outside valid coordinates")
    dialect = autopilot_dialect.dialect_for_stack(stack)
    nan = float("nan")
    params = (-1.0, float(DO_REPOSITION_CHANGE_MODE), 0.0, nan)
    if dialect.reposition_altitude_frame(mav_type) == "relative":
        frame, z = MAV_FRAME_GLOBAL_RELATIVE_ALT, alt_f
    else:
        if home_alt_amsl is None:
            return FlyToPlan(False, "No home altitude yet to convert the height above ground")
        frame, z = MAV_FRAME_GLOBAL, float(home_alt_amsl) + alt_f
    return FlyToPlan(
        True,
        command=MAV_CMD_DO_REPOSITION,
        frame=frame,
        params=params,
        x=int(round(lat_f * 1e7)),
        y=int(round(lon_f * 1e7)),
        z=z,
    )
