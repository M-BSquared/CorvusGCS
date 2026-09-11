"""Telemetry Review — a recorded tlog reduced to the same plots a ULog gets.

The ULog is the better log and, when it can be had, it is the one to read: it
is written on the flight controller at loop rate and it carries signals that
never go near a radio. This module exists because it cannot always be had.

* The SD card was not fitted, was full, or the log was never started.
* The download failed, or the aircraft is on the far side of a 57600 link and
  a 40 MB log is an hour that nobody has.
* The aircraft did not come back.

A tlog is recorded by Corvus for **every** session, on this laptop, from the
first frame — including the flights that ended badly, which are exactly the
ones somebody needs to read afterwards. So it is the log that always exists,
and leaving it as a list of filenames threw that away.

What it can and cannot show, stated plainly because the difference matters:

* **Rate.** Telemetry arrives at 1–50 Hz, not the 200–1000 Hz of a ULog. A
  control-loop oscillation is visible here; the shape of one cycle is not.
* **Coverage.** There are no motor outputs, no per-IMU data, no estimator
  innovations — those are never streamed. Attitude, position, battery, GPS
  quality, vibration and the flight's own STATUSTEXT chatter all are.
* **The link itself.** This is the one thing a tlog has that a ULog cannot:
  RADIO_STATUS and the comm drop counters describe the radio between the two
  ends. A ULog written on the aircraft has no idea the ground station stopped
  hearing it.

The payload is deliberately the same shape :func:`corvus.flight_review.review`
returns, so the Analysis page draws a tlog with the renderer it already has
rather than a second one that drifts away from it.
"""
from __future__ import annotations

import json
import logging
import math
import os
import threading
from collections import Counter, OrderedDict
from typing import Any, Iterable

from .flight_review import GROUP_ORDER, MAX_POINTS, MIN_MODE_S, _decimate

logger = logging.getLogger("corvus.tlogreview")


class TlogError(Exception):
    """A tlog that cannot be read, with a reason worth showing an operator."""


# The header line TlogWriter writes; everything after the newline is frames.
_MAGIC = b"#CORVUS-TLOG "
# A tlog is raw frames with no index, so reading one means reading all of it.
# A season of unattended SITL can leave a very large file in the folder, and
# refusing it with a reason beats an out-of-memory kill of the whole GCS.
MAX_TLOG_BYTES = 512 * 1024 * 1024
# pymavlink hands back one Python object per frame. Chunked so peak memory is
# a chunk of parsed messages rather than the whole file's worth at once.
_CHUNK_BYTES = 1 << 20

# MAV_MODE_FLAG_SAFETY_ARMED.
_ARMED_FLAG = 128
# MAV_AUTOPILOT_PX4. Every other autopilot packs custom_mode differently, and
# guessing at one produces confidently mislabelled bands.
_AUTOPILOT_PX4 = 12

# PX4 packs px4_custom_mode into HEARTBEAT.custom_mode as
# (main_mode << 16) | (sub_mode << 24). The names match the ones the ULog
# review uses, so a mode band is the same colour and the same word whichever
# log it came from.
_PX4_MAIN = {
    1: "Manual", 2: "Altitude", 3: "Position", 4: "Mission", 5: "Acro",
    6: "Offboard", 7: "Stabilized", 8: "Rattitude", 9: "Simple",
    10: "Position slow",
}
_PX4_AUTO_SUB = {
    1: "Loiter", 2: "Takeoff", 3: "Loiter", 4: "Mission", 5: "Return",
    6: "Land", 7: "Return", 8: "Follow target", 9: "Precision land",
    10: "VTOL takeoff",
}
_MAV_TYPE = {
    1: "Fixed wing", 2: "Quadrotor", 3: "Coaxial helicopter",
    4: "Helicopter", 6: "Ground control station", 7: "Airship",
    10: "Ground rover", 11: "Surface boat", 12: "Submarine",
    13: "Hexarotor", 14: "Octorotor", 15: "Tricopter",
    19: "VTOL (tailsitter duo)", 20: "VTOL (tailsitter quad)",
    21: "VTOL (tiltrotor)", 22: "VTOL", 23: "VTOL", 24: "VTOL", 25: "VTOL",
}
_SEVERITY = {
    0: "emergency", 1: "alert", 2: "critical", 3: "error",
    4: "warning", 5: "notice", 6: "info", 7: "debug",
}

# A clock that jumps backwards by more than this is the vehicle rebooting
# mid-recording, not jitter. Below it, it is an out-of-order frame.
_REBOOT_GAP_S = 5.0
_EARTH_RADIUS_M = 6371000.0


# ---------------------------------------------------------------------------
# Reading the file
# ---------------------------------------------------------------------------

def _parse_header(blob: bytes) -> tuple[dict[str, Any], int]:
    """Split Corvus's metadata line off the front. Both halves are optional.

    Returns the metadata and the OFFSET at which the frames begin, rather than
    a second copy of the body: at the accepted file size that slice was another
    half-gigabyte held for the length of the review, for nothing.

    A file without the header is still read: an operator may have dropped a
    tlog from another station into the folder, and the frames are the same
    frames. Only the recording metadata is lost.
    """
    if not blob.startswith(_MAGIC):
        return {}, 0
    end = blob.find(b"\n")
    if end < 0:
        return {}, len(blob)
    try:
        meta = json.loads(blob[len(_MAGIC):end].decode("utf-8", "replace"))
    except (ValueError, UnicodeDecodeError):
        meta = {}
    return (meta if isinstance(meta, dict) else {}), end + 1


def _messages(blob: bytes, start: int = 0) -> Iterable[Any]:
    """Every MAVLink frame in *blob* from *start*, in the order it was received.

    A generator, and it matters that callers keep it one: the frames of a
    recording at :data:`MAX_TLOG_BYTES` are tens of millions of pymavlink
    objects, which is gigabytes of Python that no cap on the file size bounds.
    Nothing here may be wrapped in ``list()``.

    ``robust_parsing`` because a tlog's tail is whatever was on the wire when
    the link or the process died: the last frame is routinely a fragment, and
    a strict parser throws away the whole flight over it.
    """
    try:
        from pymavlink import mavutil
    except ImportError as exc:  # pragma: no cover - pymavlink is a hard dep
        raise TlogError("pymavlink is not installed on this machine") from exc
    link = mavutil.mavlink.MAVLink(None)
    link.robust_parsing = True
    for offset in range(start, len(blob), _CHUNK_BYTES):
        try:
            batch = link.parse_buffer(blob[offset:offset + _CHUNK_BYTES])
        except Exception:  # noqa: BLE001 - one bad frame is not a bad file
            continue
        for msg in (batch or []):
            if msg.get_type() != "BAD_DATA":
                yield msg


def _dominant_system(msgs: Iterable[Any]) -> tuple[int | None, int]:
    """The vehicle this recording is about, and how many frames it holds.

    A station mirroring its link, or a mavlink-router with two aircraft on it,
    puts more than one system in one file. Plotting both as one flight would
    interleave two aircraft's attitude into a single trace, so the recording is
    read as being about the autopilot that sent the most heartbeats.

    Takes an iterator and keeps only the two counters, so the caller can stream
    the file past it instead of holding it. The frame total comes back with the
    answer because the caller needs it to tell an empty recording from one it
    simply could not name a system for, and a second pass to count would be a
    second parse of the whole file.
    """
    beats: Counter = Counter()
    ids: Counter = Counter()
    frames = 0
    for msg in msgs:
        frames += 1
        ids[int(msg.get_srcSystem())] += 1
        if msg.get_type() != "HEARTBEAT":
            continue
        # MAV_TYPE_GCS: another ground station on the same link is not the
        # subject of the recording however loudly it beats.
        if int(getattr(msg, "type", 0) or 0) == 6:
            continue
        beats[int(msg.get_srcSystem())] += 1
    if beats:
        return beats.most_common(1)[0][0], frames
    return (ids.most_common(1)[0][0] if ids else None), frames


# ---------------------------------------------------------------------------
# Reducing it
# ---------------------------------------------------------------------------

class _Recording:
    """One tlog, walked once, with every series it carried collected by name."""

    def __init__(self) -> None:
        self.xs: dict[str, list[float]] = {}
        self.ys: dict[str, list[float]] = {}
        self.modes: list[dict[str, Any]] = []
        self.armed: list[dict[str, float]] = []
        self.messages: list[dict[str, Any]] = []
        self.fixes: list[tuple[float, float]] = []      # (lat, lon) degrees
        self.gps_fixes: list[tuple[float, float]] = []
        self.meta: dict[str, Any] = {}
        self.duration = 0.0
        self.frames = 0
        self.types: Counter = Counter()

    def add(self, name: str, when: float, value: Any) -> None:
        try:
            number = float(value)
        except (TypeError, ValueError):
            return
        if number != number or number in (float("inf"), float("-inf")):
            return
        self.xs.setdefault(name, []).append(when)
        self.ys.setdefault(name, []).append(number)

    def has(self, *names: str) -> bool:
        return any(self.ys.get(n) for n in names)

    def series(self, name: str, label: str = "") -> dict[str, Any] | None:
        """One collected signal, decimated, in the shape the page draws."""
        ys = self.ys.get(name)
        if not ys:
            return None
        xs, ys = _decimate(self.xs[name], ys)
        return {"name": label or name,
                "x": [round(t, 3) for t in xs],
                "y": [round(v, 5) for v in ys]}


def _clock_of(msg: Any) -> float | None:
    """Seconds since boot for a frame that carries them, else None.

    Only ``time_boot_ms`` is trusted. ``time_usec`` is documented as "UNIX
    epoch **or** time since boot" and different autopilots and different
    messages pick differently; mixing the two on one axis would put half a
    flight in 1970 and the other half at second 400.
    """
    raw = getattr(msg, "time_boot_ms", None)
    if raw is None:
        return None
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return None
    return value / 1000.0 if value > 0 else None


def _walk(msgs: Iterable[Any], system: int | None) -> _Recording:
    """One pass over the frames, building every series the plots need.

    The timeline is the interesting part. Corvus writes raw frames with no
    receive timestamp of its own, so time has to come out of the frames: the
    ones that carry ``time_boot_ms`` set the clock, and everything else is
    placed at the last time the clock was set. That is accurate to one
    inter-message interval — tens of milliseconds on a live link — which is far
    finer than anything a telemetry-rate plot can resolve anyway.
    """
    rec = _Recording()
    epoch: float | None = None      # boot-time of the first sample
    clock: float | None = None      # seconds into the recording, last known
    last_boot: float | None = None
    mode_name: str | None = None
    mode_start = 0.0
    armed_since: float | None = None
    unix_at: tuple[float, float] | None = None   # (clock, unix seconds)

    for msg in msgs:
        if system is not None and int(msg.get_srcSystem()) != system:
            continue
        kind = msg.get_type()
        rec.frames += 1
        rec.types[kind] += 1

        boot = _clock_of(msg)
        if boot is not None:
            if epoch is None:
                epoch = boot
            elif last_boot is not None and boot < last_boot - _REBOOT_GAP_S:
                # The autopilot rebooted mid-recording and its boot clock
                # started again. Re-anchor so the recording's own timeline
                # keeps running forwards; a reset axis would fold the second
                # half of the session on top of the first.
                epoch = boot - (clock or 0.0)
            last_boot = boot
            clock = max(0.0, boot - epoch)
            rec.duration = max(rec.duration, clock)
        if clock is None:
            # Nothing has established a timeline yet. HEARTBEATs and STATUSTEXT
            # before the first timestamped frame have no place to go.
            if kind == "STATUSTEXT":
                rec.messages.append(_statustext(msg, 0.0))
            continue
        now = clock

        if kind == "GLOBAL_POSITION_INT":
            lat = int(getattr(msg, "lat", 0)) / 1e7
            lon = int(getattr(msg, "lon", 0)) / 1e7
            if lat or lon:
                rec.fixes.append((lat, lon))
            rec.add("alt_amsl", now, int(getattr(msg, "alt", 0)) / 1000.0)
            rec.add("alt_rel", now, int(getattr(msg, "relative_alt", 0)) / 1000.0)
            vx = int(getattr(msg, "vx", 0)) / 100.0
            vy = int(getattr(msg, "vy", 0)) / 100.0
            rec.add("vel_n", now, vx)
            rec.add("vel_e", now, vy)
            rec.add("vel_d", now, int(getattr(msg, "vz", 0)) / 100.0)
            rec.add("groundspeed", now, math.hypot(vx, vy))
        elif kind == "ATTITUDE":
            rec.add("roll", now, math.degrees(float(msg.roll)))
            rec.add("pitch", now, math.degrees(float(msg.pitch)))
            rec.add("yaw", now, math.degrees(float(msg.yaw)))
            rec.add("rate_roll", now, math.degrees(float(msg.rollspeed)))
            rec.add("rate_pitch", now, math.degrees(float(msg.pitchspeed)))
            rec.add("rate_yaw", now, math.degrees(float(msg.yawspeed)))
        elif kind == "VFR_HUD":
            rec.add("hud_airspeed", now, msg.airspeed)
            rec.add("hud_groundspeed", now, msg.groundspeed)
            rec.add("climb", now, msg.climb)
            rec.add("throttle", now, msg.throttle)
        elif kind == "GPS_RAW_INT":
            rec.add("gps_sats", now, getattr(msg, "satellites_visible", 0))
            rec.add("gps_fix", now, getattr(msg, "fix_type", 0))
            eph = int(getattr(msg, "eph", 65535) or 65535)
            epv = int(getattr(msg, "epv", 65535) or 65535)
            # 65535 is the "unknown" sentinel, not a 65 km error estimate.
            if eph != 65535:
                rec.add("gps_eph", now, eph / 100.0)
            if epv != 65535:
                rec.add("gps_epv", now, epv / 100.0)
            lat = int(getattr(msg, "lat", 0)) / 1e7
            lon = int(getattr(msg, "lon", 0)) / 1e7
            if lat or lon:
                rec.gps_fixes.append((lat, lon))
        elif kind == "SYS_STATUS":
            voltage = int(getattr(msg, "voltage_battery", 0) or 0)
            if 0 < voltage < 65535:
                rec.add("batt_v", now, voltage / 1000.0)
            current = int(getattr(msg, "current_battery", -1))
            if current >= 0:
                rec.add("batt_a", now, current / 100.0)
            remaining = int(getattr(msg, "battery_remaining", -1))
            if remaining >= 0:
                rec.add("batt_pct", now, remaining)
            rec.add("cpu", now, int(getattr(msg, "load", 0) or 0) / 10.0)
            rec.add("drop_rate", now, int(getattr(msg, "drop_rate_comm", 0) or 0) / 100.0)
            rec.add("comm_errors", now, getattr(msg, "errors_comm", 0))
        elif kind == "BATTERY_STATUS":
            cells = [int(v) for v in (getattr(msg, "voltages", None) or [])
                     if 0 < int(v) < 65535]
            if cells:
                rec.add("batt_pack_v", now, sum(cells) / 1000.0)
            consumed = int(getattr(msg, "current_consumed", -1))
            if consumed >= 0:
                rec.add("batt_mah", now, consumed)
        elif kind == "VIBRATION":
            rec.add("vibe_x", now, msg.vibration_x)
            rec.add("vibe_y", now, msg.vibration_y)
            rec.add("vibe_z", now, msg.vibration_z)
            rec.add("clip_0", now, msg.clipping_0)
            rec.add("clip_1", now, msg.clipping_1)
            rec.add("clip_2", now, msg.clipping_2)
        elif kind == "RC_CHANNELS":
            rssi = int(getattr(msg, "rssi", 255) or 255)
            if rssi != 255:
                rec.add("rc_rssi", now, rssi)
            for channel in (1, 2, 3, 4):
                value = int(getattr(msg, f"chan{channel}_raw", 0) or 0)
                if 0 < value < 65535:
                    rec.add(f"rc_{channel}", now, value)
        elif kind == "RADIO_STATUS":
            rec.add("radio_rssi", now, getattr(msg, "rssi", 0))
            rec.add("radio_remrssi", now, getattr(msg, "remrssi", 0))
            rec.add("radio_noise", now, getattr(msg, "noise", 0))
            rec.add("radio_rxerrors", now, getattr(msg, "rxerrors", 0))
        elif kind == "ESTIMATOR_STATUS":
            rec.add("ekf_vel", now, getattr(msg, "vel_ratio", 0))
            rec.add("ekf_pos", now, getattr(msg, "pos_horiz_ratio", 0))
            rec.add("ekf_hgt", now, getattr(msg, "hgt_ratio", 0))
            rec.add("ekf_mag", now, getattr(msg, "mag_ratio", 0))
        elif kind == "SYSTEM_TIME":
            unix = int(getattr(msg, "time_unix_usec", 0) or 0)
            if unix > 0 and unix_at is None:
                unix_at = (now, unix / 1e6)
        elif kind == "STATUSTEXT":
            rec.messages.append(_statustext(msg, now))
        elif kind == "AUTOPILOT_VERSION":
            rec.meta.setdefault("sw", _version_text(getattr(msg, "flight_sw_version", 0)))
            board = int(getattr(msg, "board_version", 0) or 0)
            if board:
                rec.meta.setdefault("hw", f"board {board}")
        elif kind == "HEARTBEAT":
            rec.meta.setdefault("frame", _MAV_TYPE.get(
                int(getattr(msg, "type", 0) or 0), ""))
            name = _mode_name(msg)
            if name != mode_name:
                if mode_name is not None:
                    rec.modes.append({"mode": mode_name, "state": 0,
                                      "start": round(mode_start, 2),
                                      "end": round(now, 2)})
                mode_name = name
                mode_start = now
            is_armed = bool(int(getattr(msg, "base_mode", 0) or 0) & _ARMED_FLAG)
            if is_armed and armed_since is None:
                armed_since = now
            elif not is_armed and armed_since is not None:
                rec.armed.append({"start": round(armed_since, 2), "end": round(now, 2)})
                armed_since = None

    if mode_name is not None and rec.duration > mode_start:
        rec.modes.append({"mode": mode_name, "state": 0,
                          "start": round(mode_start, 2),
                          "end": round(rec.duration, 2)})
    if armed_since is not None and rec.duration > armed_since:
        rec.armed.append({"start": round(armed_since, 2), "end": round(rec.duration, 2)})
    # A mode held across a single heartbeat is a transition, not a phase of the
    # flight; drawn, it is a sliver of colour that says nothing.
    rec.modes = [m for m in rec.modes if m["end"] - m["start"] >= MIN_MODE_S]
    rec.armed = [a for a in rec.armed if a["end"] - a["start"] >= MIN_MODE_S]
    if unix_at is not None:
        rec.meta["start_unix"] = unix_at[1] - unix_at[0]
    return rec


def _statustext(msg: Any, when: float) -> dict[str, Any]:
    text = getattr(msg, "text", "")
    if isinstance(text, (bytes, bytearray)):
        text = text.decode("utf-8", "replace")
    return {
        "t": round(when, 2),
        "level": _SEVERITY.get(int(getattr(msg, "severity", 6) or 6), "info"),
        "text": str(text).rstrip("\x00").strip(),
    }


def _mode_name(msg: Any) -> str:
    """The flight mode a HEARTBEAT is reporting.

    Only PX4's packing is decoded. Every autopilot uses ``custom_mode``
    differently, and a confidently mislabelled band is worse than a number:
    the whole point of the mode strip is that the reader trusts it.
    """
    custom = int(getattr(msg, "custom_mode", 0) or 0)
    if int(getattr(msg, "autopilot", 0) or 0) != _AUTOPILOT_PX4:
        return f"Mode {custom}"
    main = (custom >> 16) & 0xFF
    sub = (custom >> 24) & 0xFF
    if main == 4:
        return _PX4_AUTO_SUB.get(sub, "Mission")
    return _PX4_MAIN.get(main, f"Mode {custom}")


def _version_text(raw: Any) -> str:
    """MAVLink packs a firmware version as one uint32: major.minor.patch.type."""
    try:
        value = int(raw or 0)
    except (TypeError, ValueError):
        return ""
    if not value:
        return ""
    return f"{(value >> 24) & 0xFF}.{(value >> 16) & 0xFF}.{(value >> 8) & 0xFF}"


# ---------------------------------------------------------------------------
# Plots
# ---------------------------------------------------------------------------

def _plot(plot_id: str, title: str, unit: str, series: list,
          group: str = "Flight", **extra: Any) -> dict | None:
    """A plot, or None when the recording carried nothing to draw.

    Every plot is optional on purpose. Which messages a link carries is a
    stream-rate setting, not a property of the aircraft, so two recordings of
    the same airframe legitimately contain different things. A page of empty
    axes would suggest the flight was missing them.
    """
    kept = [s for s in series if s]
    if not kept:
        return None
    plot = {"id": plot_id, "title": title, "unit": unit,
            "group": group, "series": kept}
    plot.update(extra)
    return plot


def _track(rec: _Recording) -> dict | None:
    """The flight from above, in metres about the first fix.

    Projected the same way the ULog review projects its track — azimuthal
    equidistant about a reference point — so the two are read the same way and
    a tlog track laid beside a ULog track is the same shape.
    """
    if not rec.fixes and not rec.gps_fixes:
        return None
    origin = (rec.fixes or rec.gps_fixes)[0]
    sin_lat0 = math.sin(math.radians(origin[0]))
    cos_lat0 = math.cos(math.radians(origin[0]))
    lon0 = math.radians(origin[1])

    def project(points: list[tuple[float, float]], name: str) -> dict | None:
        if not points:
            return None
        east: list[float] = []
        north: list[float] = []
        for lat_deg, lon_deg in points:
            if not (-90.0 <= lat_deg <= 90.0 and -180.0 <= lon_deg <= 180.0):
                continue
            lat = math.radians(lat_deg)
            d_lon = math.radians(lon_deg) - lon0
            sin_lat = math.sin(lat)
            cos_lat = math.cos(lat)
            arg = max(-1.0, min(1.0, sin_lat0 * sin_lat
                                + cos_lat0 * cos_lat * math.cos(d_lon)))
            angle = math.acos(arg)
            scale = angle / math.sin(angle) if abs(angle) > 1e-12 else 1.0
            north.append(scale * (cos_lat0 * sin_lat - sin_lat0 * cos_lat
                                  * math.cos(d_lon)) * _EARTH_RADIUS_M)
            east.append(scale * cos_lat * math.sin(d_lon) * _EARTH_RADIUS_M)
        if not east:
            return None
        # Decimated on easting so the extreme of each bucket survives: a track
        # thinned by stride loses the corners, which is the whole shape.
        if len(east) > MAX_POINTS:
            step = len(east) / MAX_POINTS
            picks = [min(len(east) - 1, int(i * step)) for i in range(MAX_POINTS)]
            east = [east[i] for i in picks]
            north = [north[i] for i in picks]
        return {"name": name, "x": [round(v, 2) for v in east],
                "y": [round(v, 2) for v in north]}

    series = [project(rec.fixes, "Estimated position"),
              project(rec.gps_fixes, "GPS")]
    return _plot(
        "track", "Ground track", "m", series, "Flight",
        equal=True, xlabel="East (m)", ylabel="North (m)",
        note="Metres east and north of the first fix. Telemetry rate, so the "
             "line is the path flown sampled a few times a second — not every "
             "metre of it.",
    )


def _build_plots(rec: _Recording) -> list[dict]:
    """Every plot this recording can honestly support, in reading order."""
    out: list[dict | None] = [
        _track(rec),
        _plot("altitude", "Altitude", "m", [
            rec.series("alt_rel", "Above home"),
            rec.series("alt_amsl", "AMSL"),
        ], "Flight"),
        _plot("speed", "Speed", "m/s", [
            rec.series("groundspeed", "Groundspeed"),
            rec.series("hud_airspeed", "Airspeed"),
        ], "Flight"),
        _plot("climb", "Vertical speed", "m/s", [
            rec.series("climb", "Climb rate"),
        ], "Flight"),
        _plot("velocity", "Velocity NED", "m/s", [
            rec.series("vel_n", "North"),
            rec.series("vel_e", "East"),
            rec.series("vel_d", "Down"),
        ], "Flight"),

        _plot("attitude", "Attitude", "deg", [
            rec.series("roll", "Roll"),
            rec.series("pitch", "Pitch"),
        ], "Control"),
        _plot("heading", "Heading", "deg", [
            rec.series("yaw", "Yaw"),
        ], "Control"),
        _plot("rates", "Angular rates", "deg/s", [
            rec.series("rate_roll", "Roll rate"),
            rec.series("rate_pitch", "Pitch rate"),
            rec.series("rate_yaw", "Yaw rate"),
        ], "Control", note="Telemetry rate. An oscillation shows here; the "
                           "shape of a single cycle needs the ULog."),
        _plot("throttle", "Throttle", "%", [
            rec.series("throttle", "Throttle"),
        ], "Control"),
        _plot("rc", "RC input", "µs", [
            rec.series("rc_1", "Channel 1"),
            rec.series("rc_2", "Channel 2"),
            rec.series("rc_3", "Channel 3"),
            rec.series("rc_4", "Channel 4"),
        ], "Control"),

        _plot("vibration", "Vibration", "m/s²", [
            rec.series("vibe_x", "X"),
            rec.series("vibe_y", "Y"),
            rec.series("vibe_z", "Z"),
        ], "Airframe"),
        _plot("clipping", "Accelerometer clipping", "count", [
            rec.series("clip_0", "IMU 0"),
            rec.series("clip_1", "IMU 1"),
            rec.series("clip_2", "IMU 2"),
        ], "Airframe", note="A running total. Any climb at all means the "
                            "accelerometer saturated during the flight."),

        _plot("ekf", "EKF innovation test ratios", "ratio", [
            rec.series("ekf_vel", "Velocity"),
            rec.series("ekf_pos", "Position"),
            rec.series("ekf_hgt", "Height"),
            rec.series("ekf_mag", "Magnetometer"),
        ], "Estimator", threshold=1.0),

        _plot("gps_sats", "GPS satellites and fix", "count", [
            rec.series("gps_sats", "Satellites"),
            rec.series("gps_fix", "Fix type"),
        ], "Sensors"),
        _plot("gps_accuracy", "GPS accuracy estimate", "m", [
            rec.series("gps_eph", "Horizontal"),
            rec.series("gps_epv", "Vertical"),
        ], "Sensors"),

        _plot("battery", "Battery", "V", [
            rec.series("batt_v", "Voltage"),
            rec.series("batt_pack_v", "Pack voltage"),
        ], "System"),
        _plot("battery_draw", "Battery draw", "A", [
            rec.series("batt_a", "Current"),
        ], "System"),
        _plot("battery_remaining", "Battery remaining", "%", [
            rec.series("batt_pct", "Remaining"),
            rec.series("batt_mah", "Consumed (mAh)"),
        ], "System"),
        _plot("cpu", "CPU load", "%", [
            rec.series("cpu", "Load"),
        ], "System"),

        # The one group a ULog can never have. A log written on the aircraft
        # cannot know the ground station stopped hearing it.
        _plot("radio", "Radio signal", "dB", [
            rec.series("radio_rssi", "Local RSSI"),
            rec.series("radio_remrssi", "Remote RSSI"),
            rec.series("radio_noise", "Noise floor"),
        ], "Link", note="Reported by the radio modem, not the autopilot — "
                        "this is the link between the two ends of the flight."),
        _plot("link_loss", "Dropped telemetry", "%", [
            rec.series("drop_rate", "Drop rate"),
        ], "Link"),
        _plot("link_errors", "Link error counters", "count", [
            rec.series("comm_errors", "Comm errors"),
            rec.series("radio_rxerrors", "Radio RX errors"),
        ], "Link"),
        _plot("rc_rssi", "RC link strength", "", [
            rec.series("rc_rssi", "RC RSSI"),
        ], "Link"),
    ]
    return [p for p in out if p]


# The ULog review's groups plus the one only a tlog has.
TLOG_GROUP_ORDER = tuple(GROUP_ORDER[:-1]) + ("Link", GROUP_ORDER[-1])


# ---------------------------------------------------------------------------
# Findings
# ---------------------------------------------------------------------------

def _peak(rec: _Recording, name: str) -> float | None:
    values = rec.ys.get(name)
    return max(values) if values else None


def _trough(rec: _Recording, name: str) -> float | None:
    values = rec.ys.get(name)
    return min(values) if values else None


def _findings(rec: _Recording) -> list[dict[str, str]]:
    """The few sentences worth reading before the plots.

    Only what this recording actually shows. No score, no grade: a review that
    invents a verdict from telemetry-rate data is worse than one that points at
    a plot and lets the operator look.
    """
    out: list[dict[str, str]] = []

    # No frame in the whole recording carried time_boot_ms, so there is no axis
    # to draw anything against. Said plainly and first: a page of no plots with
    # "nothing stands out" under it reads as a clean flight, and this is the
    # opposite — it is a recording nothing could be read from.
    if not rec.ys:
        kinds = ", ".join(name for name, _ in rec.types.most_common(4))
        out.append({"level": "warning", "text":
                    "This recording carries no timeline — nothing in it is "
                    "timestamped, so none of it can be plotted. It holds "
                    f"{rec.frames:,} frames"
                    + (f" ({kinds})" if kinds else "")
                    + ". A session that only ever exchanged heartbeats looks "
                    "like this."})
        return out

    clipped = max((_peak(rec, n) or 0.0) for n in ("clip_0", "clip_1", "clip_2"))
    if clipped > 0:
        out.append({"level": "critical", "text":
                    f"Accelerometer clipping reached {int(clipped)} samples — "
                    "the IMU saturated, so the estimator was working from "
                    "measurements it could not trust."})

    vibe = max((_peak(rec, n) or 0.0) for n in ("vibe_x", "vibe_y", "vibe_z"))
    if vibe >= 30:
        out.append({"level": "critical", "text":
                    f"Vibration peaked at {vibe:.0f} m/s² — well past the 30 "
                    "PX4 treats as unflyable."})
    elif vibe >= 15:
        out.append({"level": "warning", "text":
                    f"Vibration peaked at {vibe:.0f} m/s². Under 15 is healthy."})

    drop = _peak(rec, "drop_rate")
    if drop and drop >= 1.0:
        out.append({"level": "warning", "text":
                    f"The vehicle reported dropping up to {drop:.1f}% of "
                    "telemetry. Gaps in these plots are the link, not the "
                    "flight."})

    rssi = _trough(rec, "radio_remrssi")
    if rssi is not None and rssi < 60:
        out.append({"level": "warning", "text":
                    f"Remote radio RSSI fell to {rssi:.0f}. Below about 60 the "
                    "modem is close to losing the link."})

    fix = _trough(rec, "gps_fix")
    if fix is not None and fix < 3 and rec.ys.get("gps_fix"):
        out.append({"level": "warning", "text":
                    "GPS dropped below a 3D fix during the recording — any "
                    "position held on the plots there was dead reckoning."})

    volts = _trough(rec, "batt_v")
    peak = _peak(rec, "batt_v")
    if volts is not None and volts > 0 and peak:
        # Cell count from the HIGHEST voltage seen, never the lowest: a lithium
        # cell cannot exceed about 4.25 V, so the peak divided by that is a
        # firm lower bound on the count. Dividing the sagged voltage by a
        # nominal 3.8 instead reads a sagging 4S as a healthy 3S — exactly
        # backwards, and it hides the one battery finding that matters.
        cells = max(1, math.ceil(peak / 4.25))
        per_cell = volts / cells
        if per_cell < 3.3:
            out.append({"level": "critical", "text":
                        f"Battery reached {volts:.1f} V ({per_cell:.2f} V per "
                        f"cell across {cells}) — into the range that damages "
                        "a lithium pack."})
        elif per_cell < 3.5:
            out.append({"level": "warning", "text":
                        f"Battery reached {volts:.1f} V ({per_cell:.2f} V per "
                        "cell) — a sag worth reading against the current plot."})

    errors = [m for m in rec.messages
              if m["level"] in ("emergency", "alert", "critical", "error")]
    if errors:
        out.append({"level": "warning", "text":
                    f"The aircraft printed {len(errors)} error-level "
                    f"message(s); the first was “{errors[0]['text']}”."})

    if not out:
        out.append({"level": "ok", "text":
                    "Nothing in this recording stands out. It is telemetry, "
                    "though — for motor outputs, per-IMU data and estimator "
                    "innovations, read the ULog."})
    return out


# ---------------------------------------------------------------------------
# The review
# ---------------------------------------------------------------------------

def review_bytes(blob: bytes, name: str = "") -> dict[str, Any]:
    """Reduce one tlog's bytes to the payload the Analysis page renders."""
    if len(blob) > MAX_TLOG_BYTES:
        raise TlogError("that recording is too large to review")
    meta, start = _parse_header(blob)
    if start >= len(blob):
        raise TlogError("that recording is empty")
    # Two streaming passes, never one list. _walk needs to know which system
    # the recording is about before it can place a frame, and the alternative
    # to reading the file twice is holding every parsed frame at once — which
    # at the accepted file size is the out-of-memory kill MAX_TLOG_BYTES exists
    # to prevent. Parsing is CPU; holding is the whole process.
    system, frames = _dominant_system(_messages(blob, start))
    if not frames:
        raise TlogError("no MAVLink frames in that recording")
    rec = _walk(_messages(blob, start), system)

    plots = _build_plots(rec)
    duration = rec.duration
    for span in rec.modes:
        duration = max(duration, span["end"])

    summary: dict[str, Any] = {
        "name": name,
        "duration_s": round(duration, 1),
        "airframe": rec.meta.get("frame") or "",
        "sw": rec.meta.get("sw", ""),
        "hw": rec.meta.get("hw", ""),
        "sys_name": f"System {system}" if system is not None else "",
        "dropouts": 0,
        "dropout_ms": 0,
        "truncated": False,
        "armed_s": round(sum(a["end"] - a["start"] for a in rec.armed), 1),
        "modes_flown": sorted({m["mode"] for m in rec.modes}),
        "frames": rec.frames,
        "start_unix": rec.meta.get("start_unix", 0.0),
        "recorded_by": meta.get("product", ""),
        "recorder_version": meta.get("version", ""),
        "conn": meta.get("conn", ""),
        "started_at": meta.get("started_at", ""),
        "topics": sorted(rec.types),
        # The five facts the page prints, chosen for a tlog rather than
        # inherited from the ULog review: a recording has no ULog dropouts to
        # report, and it does have a link and a frame count that matter.
        "facts": [
            ["Duration", f"{duration:.0f} s" if duration else "—"],
            ["Armed", f"{sum(a['end'] - a['start'] for a in rec.armed):.0f} s"
                      if rec.armed else "not armed"],
            ["Airframe", rec.meta.get("frame") or "—"],
            ["Firmware", rec.meta.get("sw") or "—"],
            ["Frames", f"{rec.frames:,}"],
        ],
    }
    return {
        "summary": summary,
        "groups": [g for g in TLOG_GROUP_ORDER
                   if any(p["group"] == g for p in plots)],
        "modes": rec.modes,
        "armed": rec.armed,
        "findings": _findings(rec),
        "plots": plots,
        "messages": rec.messages[-300:],
        "kind": "tlog",
    }


# Reviews already produced, newest last. Parsing is the expensive half of this
# module and the way the page is used is to open one recording, go back, and
# open it or its neighbour again.
_CACHE_ENTRIES = 2
_cache: "OrderedDict[tuple, dict[str, Any]]" = OrderedDict()
_cache_lock = threading.Lock()


def review_file(path: str, name: str = "") -> dict[str, Any]:
    """Review the tlog at *path*, reusing a recent result for the same file.

    The key carries size and mtime, so the recording of a session still being
    written is re-read as it grows rather than answered from a stale review —
    the one mistake a cache here could make that matters.
    """
    label = name or os.path.basename(path)
    try:
        stat = os.stat(path)
        key = (os.path.realpath(path), stat.st_size, stat.st_mtime_ns)
    except OSError as exc:
        raise TlogError(f"could not read that recording ({exc.strerror or exc})") from exc
    if stat.st_size > MAX_TLOG_BYTES:
        raise TlogError("that recording is too large to review")
    with _cache_lock:
        hit = _cache.get(key)
        if hit is not None:
            _cache.move_to_end(key)
            return hit
    with open(path, "rb") as handle:
        blob = handle.read()
    data = review_bytes(blob, label)
    with _cache_lock:
        _cache[key] = data
        _cache.move_to_end(key)
        while len(_cache) > _CACHE_ENTRIES:
            _cache.popitem(last=False)
    return data


def clear_cache() -> None:
    """Drop every cached review. For tests, and for a shutdown that wants the
    memory back."""
    with _cache_lock:
        _cache.clear()
