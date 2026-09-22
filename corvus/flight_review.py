"""Flight Review — a ULog reduced to the handful of plots that answer
"was that flight healthy?".

Modelled on PX4's own flight_review, deliberately not a port of it: this is the
quick pass an operator makes between flights, not a full analysis suite. It
covers the things that actually ground an aircraft — motors saturating, accel
clipping, EKF rejecting its own measurements, a battery sagging — plus whatever
PX4 printed to the log while it flew.

Two properties matter more than breadth here:

* **Version tolerance.** PX4 renames topics and fields between releases. Every
  plot names several candidates and takes the first the log actually contains,
  so a v1.14 log and a v1.18 log both produce something rather than one of them
  producing an empty page.
* **Honest decimation.** A ten-minute log has hundreds of thousands of samples
  and the browser gets a few thousand. Stride decimation would drop exactly the
  spikes a review is looking for, so each bucket contributes its most extreme
  sample instead.
"""
from __future__ import annotations

import hashlib
import math
import os
import threading
from collections import OrderedDict
from typing import Any
from collections.abc import Callable, Iterable

from .ulog import ULog, read

# Points per series handed to the browser. Plotly draws this smoothly and it is
# far more resolution than a between-flights check needs.
MAX_POINTS = 1200


def _decimate(times: list[float], values: list[float]) -> tuple[list[float], list[float]]:
    """Reduce to MAX_POINTS, keeping the extreme sample of each bucket.

    Stride decimation is the wrong filter for this job: a review is hunting for
    the one frame where a motor saturated or an accel clipped, and taking every
    n-th sample is precisely how that frame gets dropped.
    """
    count = len(values)
    if count <= MAX_POINTS:
        return times, values
    step = count / MAX_POINTS
    out_t: list[float] = []
    out_v: list[float] = []
    for bucket in range(MAX_POINTS):
        start = int(bucket * step)
        end = max(start + 1, int((bucket + 1) * step))
        best = start
        best_abs = -1.0
        for index in range(start, min(end, count)):
            value = values[index]
            magnitude = abs(value) if value == value else -1.0
            if magnitude > best_abs:
                best_abs = magnitude
                best = index
        out_t.append(times[best])
        out_v.append(values[best])
    return out_t, out_v


def _first_topic(log: ULog, names: Iterable[str]) -> str | None:
    for name in names:
        if log.has(name):
            return name
    return None


def _first_field(log: ULog, topic: str, names: Iterable[str], multi_id: int = 0) -> str | None:
    fields = log.data.get(topic, {}).get(multi_id, {})
    for name in names:
        if name in fields and fields[name]:
            return name
    return None


def _time_axis(log: ULog, topic: str, multi_id: int = 0) -> list[float]:
    """Seconds since the first sample of the log."""
    stamps = log.series(topic, "timestamp", multi_id)
    if not stamps:
        return []
    base = _log_start(log)
    return [(t - base) / 1e6 for t in stamps]


_START_CACHE = "_flight_review_start"


def _advancing(stamps: list) -> bool:
    """True when a topic's timestamps actually move.

    Some PX4 versions publish a topic without updating its timestamp —
    `commander_state` in the v1.4-era logs is one — leaving 678 records that all
    claim the same microsecond. Such a topic is not a clock, and letting one
    become the time base shifts every plot in the review by however far its
    frozen value sits from the real start.
    """
    return len(stamps) > 1 and stamps[-1] > stamps[0]


def _log_start(log: ULog) -> int:
    """Earliest timestamp across the topics whose clocks run — the flight's t=0."""
    cached = getattr(log, _START_CACHE, None)
    if cached is not None:
        return cached
    best: int | None = None
    fallback: int | None = None
    for instances in log.data.values():
        for fields in instances.values():
            stamps = fields.get("timestamp")
            if not stamps:
                continue
            if fallback is None or stamps[0] < fallback:
                fallback = stamps[0]
            if _advancing(stamps) and (best is None or stamps[0] < best):
                best = stamps[0]
    resolved = best if best is not None else (fallback or 0)
    setattr(log, _START_CACHE, resolved)
    return resolved


def _series(
    log: ULog, topic: str, field: str, name: str, multi_id: int = 0,
    scale: float = 1.0, transform: Callable[[float], float] | None = None,
) -> dict[str, Any] | None:
    values = log.series(topic, field, multi_id)
    if not values:
        return None
    times = _time_axis(log, topic, multi_id)
    if len(times) != len(values):
        span = min(len(times), len(values))
        times, values = times[:span], values[:span]
    numeric: list[float] = []
    for value in values:
        try:
            number = float(value)
        except (TypeError, ValueError):
            number = float("nan")
        if transform is not None and number == number:
            number = transform(number)
        numeric.append(number * scale)
    times, numeric = _decimate(times, numeric)
    return {"name": name, "x": [round(t, 3) for t in times],
            "y": [None if v != v else round(v, 5) for v in numeric]}


def _plot(plot_id: str, title: str, unit: str, series: list,
          group: str = "Flight", **extra: Any) -> dict | None:
    """A plot, or None when the log carries nothing to draw.

    `group` sections the page: a review with two dozen plots is a scroll unless
    the reader can tell "estimator" from "airframe" without reading titles.
    """
    kept = [s for s in series if s]
    if not kept:
        return None
    plot = {"id": plot_id, "title": title, "unit": unit,
            "group": group, "series": kept}
    plot.update(extra)
    return plot


def _vector(log: ULog, topic: str, field: str, labels: tuple[str, ...],
            multi_id: int = 0, scale: float = 1.0) -> list:
    """Split an array field into one series per component."""
    values = log.series(topic, field, multi_id)
    if not values or not isinstance(values[0], list):
        return []
    out = []
    for index, label in enumerate(labels):
        if index >= len(values[0]):
            break
        column = [
            float(row[index]) * scale if index < len(row) else float("nan")
            for row in values
        ]
        out.append(_series_from_column(log, topic, column, label, multi_id))
    return out


# ---------------------------------------------------------------------------
# Individual plots
# ---------------------------------------------------------------------------

def _plot_actuators(log: ULog) -> dict | None:
    """Per-motor output. A motor pinned at its limit is the classic finding."""
    topic = _first_topic(log, ("actuator_motors", "actuator_outputs"))
    if topic is None:
        return None
    series = []
    if topic == "actuator_motors":
        values = log.series(topic, "control", 0)
        if not values:
            return None
        width = min(8, len(values[0]) if values else 0)
        for index in range(width):
            column = [row[index] if index < len(row) else float("nan") for row in values]
            if all(v != v or v <= 0 for v in column):
                continue
            series.append(_series_from_column(log, topic, column, f"Motor {index + 1}"))
        unit = "normalised"
    else:
        values = log.series(topic, "output", 0)
        if not values:
            return None
        # `noutputs` says how many channels the mixer actually drives; the rest
        # of the fixed-width array is padding and plotting it buries the ones
        # that matter under flat lines.
        counts = log.series(topic, "noutputs", 0)
        width = int(counts[0]) if counts else len(values[0])
        width = max(0, min(8, width, len(values[0]) if values else 0))
        for index in range(width):
            column = [row[index] if index < len(row) else float("nan") for row in values]
            finite = [v for v in column if v == v]
            if not finite or all(v == 0 for v in finite):
                continue
            series.append(_series_from_column(log, topic, column, f"Output {index + 1}"))
        unit = "µs"
    note = ("A motor sitting at its limit while the others do not is an "
            "imbalance — check the airframe, not the tuning.")
    # A flat plot is still an answer, and a missing one is not: "the motors
    # never ran" is exactly what you need to know about a log that has no
    # flight in it.
    flat = all(len({y for y in s["y"] if y is not None}) <= 1 for s in series if s)
    if series and flat:
        note = "The outputs never changed — the motors did not run in this log."
    return _plot("actuators", "Motor outputs", unit, series, note=note,
    group="Airframe")


def _series_from_column(log: ULog, topic: str, column: list[float], name: str,
                        multi_id: int = 0) -> dict:
    times = _time_axis(log, topic, multi_id)
    span = min(len(times), len(column))
    t, v = _decimate(times[:span], [float(x) for x in column[:span]])
    return {"name": name, "x": [round(x, 3) for x in t],
            "y": [None if y != y else round(y, 5) for y in v]}


def _plot_clipping(log: ULog) -> dict | None:
    """Accelerometer clipping counters — a saturated IMU lies to the EKF."""
    topic = _first_topic(log, ("vehicle_imu_status", "sensor_accel_status"))
    if topic is None:
        return None
    series = []
    for multi_id in log.instances(topic)[:3]:
        values = log.series(topic, "accel_clipping", multi_id)
        if values and isinstance(values[0], list):
            total = [float(sum(row)) for row in values]
        else:
            counter = _first_field(log, topic, ("accel_clipping_count",), multi_id)
            if counter is None:
                continue
            total = [float(v) for v in log.series(topic, counter, multi_id)]
        if not total or max(total) <= 0:
            continue
        times = _time_axis(log, topic, multi_id)
        span = min(len(times), len(total))
        t, v = _decimate(times[:span], total[:span])
        series.append({"name": f"IMU {multi_id}", "x": [round(x, 3) for x in t],
                       "y": [round(y, 1) for y in v]})
    return _plot("clipping", "Accelerometer clipping", "clipped samples (cumulative)",
                 series,
                 note="Any rise at all means the accelerometer saturated — "
                      "soften the flight-controller mounting before trusting "
                      "the rest of this log.",
                      group="Sensors")


def _plot_vibration(log: ULog) -> dict | None:
    """Vibration metrics per IMU — accel and gyro, which fail differently."""
    topic = _first_topic(log, ("vehicle_imu_status", "estimator_status"))
    if topic is None:
        return None
    series = []
    if topic == "vehicle_imu_status":
        for multi_id in log.instances(topic)[:3]:
            for field, label in (("accel_vibration_metric", "Accel"),
                                 ("gyro_vibration_metric", "Gyro")):
                found = _first_field(log, topic, (field,), multi_id)
                if found:
                    series.append(_series(log, topic, found,
                                          f"{label} IMU {multi_id}", multi_id))
    else:
        # estimator_status.vibe: [delta-angle coning, delta-angle, delta-velocity]
        series = _vector(log, topic, "vibe",
                         ("Delta angle coning", "Delta angle", "Delta velocity"))
    return _plot("vibration", "Vibration metrics", "m/s² · rad/s", series,
                 group="Sensors",
                 note="Read against the thrust plot: a level that climbs with "
                      "throttle is a propeller or motor problem, not a tuning "
                      "one. PX4 treats roughly 30 as the point to act on and "
                      "60 as the point the estimator suffers.")


def _plot_ekf(log: ULog) -> dict | None:
    """EKF innovation test ratios. Above 1.0 the filter is rejecting a sensor."""
    if not log.has("estimator_status"):
        return None
    names = (("mag_test_ratio", "Magnetometer"), ("vel_test_ratio", "Velocity"),
             ("pos_test_ratio", "Position"), ("hgt_test_ratio", "Height"),
             ("tas_test_ratio", "Airspeed"))
    series = []
    for field, label in names:
        found = _first_field(log, "estimator_status", (field,))
        if found:
            series.append(_series(log, "estimator_status", found, label))
    return _plot("ekf", "EKF innovation test ratios", "ratio", series,
                 threshold=1.0,
                 note="1.0 is the rejection threshold: above it the estimator "
                      "is discarding that sensor. Brief spikes are normal, a "
                      "sustained excursion is not.",
                      group="Estimator")


def _euler(log: ULog) -> tuple[str, list[float], list[float], list[float]] | None:
    """Roll, pitch and yaw in degrees, from the logged attitude quaternion."""
    topic = _first_topic(log, ("vehicle_attitude", "control_state"))
    if topic is None:
        return None
    quats = log.series(topic, "q", 0)
    if not quats or not isinstance(quats[0], list):
        return None
    roll: list[float] = []
    pitch: list[float] = []
    yaw: list[float] = []
    for q in quats:
        if len(q) < 4:
            roll.append(float("nan"))
            pitch.append(float("nan"))
            yaw.append(float("nan"))
            continue
        w, x, y, z = float(q[0]), float(q[1]), float(q[2]), float(q[3])
        roll.append(math.degrees(math.atan2(2 * (w * x + y * z), 1 - 2 * (x * x + y * y))))
        pitch.append(math.degrees(math.asin(max(-1.0, min(1.0, 2 * (w * y - z * x))))))
        yaw.append(math.degrees(math.atan2(2 * (w * z + x * y), 1 - 2 * (y * y + z * z))))
    return topic, roll, pitch, yaw


def _attitude_plot(log: ULog, axis: str) -> dict | None:
    """One axis of attitude: estimate against setpoint.

    One plot per axis rather than all three on one, because that is how the
    question is actually asked — "is roll tracking?" — and three estimates plus
    three setpoints on a single axis is six lines nobody can read.
    """
    euler = _euler(log)
    if euler is None:
        return None
    topic, roll, pitch, yaw = euler
    values = {"roll": roll, "pitch": pitch, "yaw": yaw}[axis]
    label = axis.capitalize()
    series = [_series_from_column(log, topic, values, f"{label} estimate")]

    setpoint = _first_topic(log, ("vehicle_attitude_setpoint",))
    if setpoint:
        found = _first_field(log, setpoint, (f"{axis}_body", axis))
        if found:
            series.append(_series(log, setpoint, found, f"{label} setpoint",
                                  transform=math.degrees))
    note = ("The estimate should sit on the setpoint. A steady offset is trim "
            "or tuning; a sudden divergence is not.")
    if axis == "yaw":
        note = ("Yaw wraps at ±180°, so a jump between the two is the wrap, not "
                "the aircraft. Watch the gap between the pair instead.")
    return _plot(f"att_{axis}", f"{label} angle", "degrees", series,
                 group="Control", note=note)


def _rate_plot(log: ULog, axis: str) -> dict | None:
    """One axis of angular rate: measured against demanded — the tuning plot."""
    index = {"roll": 0, "pitch": 1, "yaw": 2}[axis]
    label = axis.capitalize()
    topic = _first_topic(log, ("vehicle_angular_velocity", "sensor_combined"))
    if topic is None:
        return None
    field = "xyz" if topic == "vehicle_angular_velocity" else "gyro_rad"
    values = log.series(topic, field, 0)
    if not values or not isinstance(values[0], list) or len(values[0]) <= index:
        return None
    column = [
        float(row[index]) * 180.0 / math.pi if index < len(row) else float("nan")
        for row in values
    ]
    series = [_series_from_column(log, topic, column, f"{label} rate")]

    setpoint = _first_topic(log, ("vehicle_rates_setpoint",))
    if setpoint:
        found = _first_field(log, setpoint, (axis,))
        if found:
            series.append(_series(log, setpoint, found, f"{label} rate setpoint",
                                  scale=180.0 / math.pi))
    return _plot(f"rate_{axis}", f"{label} angular rate", "deg/s", series,
                 group="Control",
                 note="Lag behind the setpoint means the gains are low; "
                      "overshoot and ringing mean they are high.")


def _plot_att_roll(log: ULog) -> dict | None:
    return _attitude_plot(log, "roll")


def _plot_rate_roll(log: ULog) -> dict | None:
    return _rate_plot(log, "roll")


def _plot_att_pitch(log: ULog) -> dict | None:
    return _attitude_plot(log, "pitch")


def _plot_rate_pitch(log: ULog) -> dict | None:
    return _rate_plot(log, "pitch")


def _plot_att_yaw(log: ULog) -> dict | None:
    return _attitude_plot(log, "yaw")


def _plot_rate_yaw(log: ULog) -> dict | None:
    return _rate_plot(log, "yaw")


def _local_pair(log: ULog, plot_id: str, title: str, field: str,
                unit: str, scale: float = 1.0, note: str = "") -> dict | None:
    """One local-frame channel: estimate and, where logged, its setpoint."""
    if not log.has("vehicle_local_position"):
        return None
    found = _first_field(log, "vehicle_local_position", (field,))
    if found is None:
        return None
    series = [_series(log, "vehicle_local_position", found, "Estimate",
                      scale=scale)]
    setpoint = _first_topic(log, ("vehicle_local_position_setpoint",))
    if setpoint:
        target = _first_field(log, setpoint, (field,))
        if target:
            series.append(_series(log, setpoint, target, "Setpoint", scale=scale))
    return _plot(plot_id, title, unit, series, group="Flight", note=note)


def _plot_pos_x(log: ULog) -> dict | None:
    return _local_pair(log, "pos_x", "Local position X (north)", "x", "m")


def _plot_pos_y(log: ULog) -> dict | None:
    return _local_pair(log, "pos_y", "Local position Y (east)", "y", "m")


def _plot_pos_z(log: ULog) -> dict | None:
    # NED: down is positive, and nobody reads height that way.
    return _local_pair(log, "pos_z", "Local position Z (height)", "z", "m",
                       scale=-1.0,
                       note="Flipped out of NED so up is up. Zero is where the "
                            "estimator started, not the ground.")


def _plot_vel_x(log: ULog) -> dict | None:
    return _local_pair(log, "vel_x", "Velocity X (north)", "vx", "m/s")


def _plot_vel_y(log: ULog) -> dict | None:
    return _local_pair(log, "vel_y", "Velocity Y (east)", "vy", "m/s")


def _plot_vel_z(log: ULog) -> dict | None:
    return _local_pair(log, "vel_z", "Velocity Z (climb)", "vz", "m/s",
                       scale=-1.0,
                       note="Flipped out of NED, so a climb reads as a climb.")


def _plot_manual_control(log: ULog) -> dict | None:
    """What the pilot asked for. Without it, every control plot is missing its
    input and an aggressive stick looks like an unstable aircraft."""
    topic = _first_topic(log, ("manual_control_setpoint", "manual_control_input"))
    if topic is None:
        return None
    series = []
    # PX4 renamed these from x/y/z/r to pitch/roll/throttle/yaw; both spellings
    # appear in the supported firmware range.
    for names, label in (
        (("roll", "y"), "Roll stick"),
        (("pitch", "x"), "Pitch stick"),
        (("yaw", "r"), "Yaw stick"),
        (("throttle", "z"), "Throttle"),
    ):
        found = _first_field(log, topic, names)
        if found:
            series.append(_series(log, topic, found, label))
    return _plot("manual", "Manual control input", "normalised", series,
                 group="Control",
                 note="The pilot's side of every control plot above. A rate "
                      "that only misbehaves while a stick is moving is a "
                      "different problem from one that misbehaves on its own.")


def _plot_altitude(log: ULog) -> dict | None:

    topic = _first_topic(log, ("vehicle_local_position", "vehicle_global_position"))
    if topic is None:
        return None
    series = []
    if topic == "vehicle_local_position":
        field = _first_field(log, topic, ("z",))
        if field:
            # NED: down is positive, and nobody reads altitude that way.
            series.append(_series(log, topic, field, "Altitude (local)", scale=-1.0))
    else:
        field = _first_field(log, topic, ("alt",))
        if field:
            series.append(_series(log, topic, field, "Altitude (AMSL)"))
    return _plot("altitude", "Altitude", "m", series,
    group="Flight")


def _plot_battery(log: ULog) -> dict | None:
    if not log.has("battery_status"):
        return None
    voltage = _first_field(log, "battery_status", ("voltage_filtered_v", "voltage_v"))
    current = _first_field(log, "battery_status", ("current_filtered_a", "current_a"))
    series = []
    if voltage:
        series.append(_series(log, "battery_status", voltage, "Voltage"))
    if current:
        series.append(_series(log, "battery_status", current, "Current"))
    return _plot("battery", "Battery", "V / A", series,
                 note="A voltage that dives under throttle and recovers is a "
                      "tired pack, and it is the reading that decides whether "
                      "the next flight is a good idea.",
                      group="System")


def _plot_cpu(log: ULog) -> dict | None:
    if not log.has("cpuload"):
        return None
    series = [
        _series(log, "cpuload", "load", "CPU load", scale=100.0),
        _series(log, "cpuload", "ram_usage", "RAM used", scale=100.0),
    ]
    return _plot("cpu", "Processor", "%", series,
    group="System")


def _plot_gps(log: ULog) -> dict | None:
    topic = _first_topic(log, ("vehicle_gps_position", "sensor_gps"))
    if topic is None:
        return None
    series = []
    sats = _first_field(log, topic, ("satellites_used",))
    if sats:
        series.append(_series(log, topic, sats, "Satellites"))
    eph = _first_field(log, topic, ("eph",))
    if eph:
        series.append(_series(log, topic, eph, "Horizontal accuracy (m)"))
    return _plot("gps", "GPS", "count / m", series,
    group="Sensors")


def _plot_rates(log: ULog) -> dict | None:
    """Angular rates against their setpoints — the tuning plot.

    Everything else says whether the aircraft was healthy; this one says
    whether it was *tuned*.
    """
    topic = _first_topic(log, ("vehicle_angular_velocity", "sensor_combined"))
    if topic is None:
        return None
    if topic == "vehicle_angular_velocity":
        series = _vector(log, topic, "xyz", ("Roll rate", "Pitch rate", "Yaw rate"),
                         scale=180.0 / math.pi)
    else:
        series = _vector(log, topic, "gyro_rad", ("Roll rate", "Pitch rate", "Yaw rate"),
                         scale=180.0 / math.pi)
    setpoint = _first_topic(log, ("vehicle_rates_setpoint",))
    if setpoint:
        for field, label in (("roll", "Roll setpoint"), ("pitch", "Pitch setpoint"),
                             ("yaw", "Yaw setpoint")):
            found = _first_field(log, setpoint, (field,))
            if found:
                series.append(_series(log, setpoint, found, label,
                                      scale=180.0 / math.pi))
    return _plot("rates", "Angular rates", "deg/s", series, group="Control",
                 note="The rate should sit on its setpoint. Lag means the gains "
                      "are low; overshoot and ringing mean they are high.")


# WGS-84 mean radius, the value PX4's own map projection uses.
_EARTH_RADIUS_M = 6371000.0


class _Projection:
    """PX4's azimuthal equidistant projection about the estimator's origin.

    Not an approximation of it: this is the same transform the estimator ran to
    turn GPS into ``vehicle_local_position``, so a projected fix and the
    estimate it is being compared against are genuinely in one frame. A
    flat-earth shortcut would put the two tracks a few metres apart at range and
    that gap would read as estimator error, which is precisely the thing this
    plot exists to show.
    """

    __slots__ = ("_sin_lat", "_cos_lat", "_lon")

    def __init__(self, ref_lat_deg: float, ref_lon_deg: float) -> None:
        lat = math.radians(ref_lat_deg)
        self._sin_lat = math.sin(lat)
        self._cos_lat = math.cos(lat)
        self._lon = math.radians(ref_lon_deg)

    def project(self, lat_deg: float, lon_deg: float) -> tuple[float, float]:
        """(north, east) in metres from the origin."""
        lat = math.radians(lat_deg)
        d_lon = math.radians(lon_deg) - self._lon
        sin_lat = math.sin(lat)
        cos_lat = math.cos(lat)
        cos_d_lon = math.cos(d_lon)
        arg = max(-1.0, min(1.0, self._sin_lat * sin_lat
                            + self._cos_lat * cos_lat * cos_d_lon))
        angle = math.acos(arg)
        # The limit of c/sin(c) at the origin, where the fix is the reference.
        scale = angle / math.sin(angle) if abs(angle) > 1e-12 else 1.0
        north = scale * (self._cos_lat * sin_lat
                         - self._sin_lat * cos_lat * cos_d_lon) * _EARTH_RADIUS_M
        east = scale * cos_lat * math.sin(d_lon) * _EARTH_RADIUS_M
        return north, east


def _local_origin(log: ULog) -> _Projection | None:
    """The estimator's own local-frame origin, or None when it never had one.

    A log flown without a global reference (indoors, no GPS) reports zeros here.
    Projecting against those would draw a GPS track off the coast of Africa and
    overlay it on the estimate as if the two disagreed.
    """
    if not log.has("vehicle_local_position"):
        return None
    fields = log.data.get("vehicle_local_position", {}).get(0, {})
    lats = fields.get("ref_lat") or []
    lons = fields.get("ref_lon") or []
    valid = fields.get("xy_global") or []
    for index in range(min(len(lats), len(lons))):
        if valid and index < len(valid) and not valid[index]:
            continue
        try:
            lat = float(lats[index])
            lon = float(lons[index])
        except (TypeError, ValueError):
            continue
        if lat == 0.0 and lon == 0.0:
            continue
        if -90.0 <= lat <= 90.0 and -180.0 <= lon <= 180.0:
            return _Projection(lat, lon)
    return None


def _degrees(value: Any) -> float | None:
    """Latitude or longitude as degrees.

    PX4 logs these as int32 in 1e-7 degrees on some topics and as doubles on
    others, and the two are told apart by magnitude: no coordinate in degrees
    exceeds 180, and no coordinate in 1e-7 degrees that is not zero is that
    small.
    """
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    if number != number:
        return None
    if abs(number) > 180.0:
        number /= 1e7
    return number if abs(number) <= 180.0 else None


def _xy_path(log: ULog, topic: str, north_field: str, east_field: str,
             name: str, **extra: Any) -> dict | None:
    """A local-frame track, decimated as a pair so the shape survives."""
    north = log.series(topic, north_field, 0)
    east = log.series(topic, east_field, 0)
    span = min(len(north), len(east))
    if span < 2:
        return None
    step = max(1, span // MAX_POINTS)
    xs: list[float] = []
    ys: list[float] = []
    for index in range(0, span, step):
        try:
            x = float(east[index])
            y = float(north[index])
        except (TypeError, ValueError):
            continue
        if x != x or y != y:
            continue
        xs.append(round(x, 2))
        ys.append(round(y, 2))
    if len(xs) < 2:
        return None
    series = {"name": name, "x": xs, "y": ys}
    series.update(extra)
    return series


def _gps_path(log: ULog, projection: _Projection) -> dict | None:
    """Raw GPS fixes projected into the estimator's frame."""
    topic = _first_topic(log, ("vehicle_gps_position", "sensor_gps"))
    if topic is None:
        return None
    lats = log.series(topic, "lat", 0)
    lons = log.series(topic, "lon", 0)
    fixes = log.series(topic, "fix_type", 0)
    span = min(len(lats), len(lons))
    if span < 2:
        return None
    step = max(1, span // MAX_POINTS)
    xs: list[float] = []
    ys: list[float] = []
    for index in range(0, span, step):
        # A 2D fix has no business anchoring a track, and a "no fix" row is
        # usually 0/0 — drawn, it is a line to the Gulf of Guinea.
        if fixes and index < len(fixes) and float(fixes[index] or 0) < 3:
            continue
        lat = _degrees(lats[index])
        lon = _degrees(lons[index])
        if lat is None or lon is None or (lat == 0.0 and lon == 0.0):
            continue
        north, east = projection.project(lat, lon)
        xs.append(round(east, 2))
        ys.append(round(north, 2))
    if len(xs) < 2:
        return None
    return {"name": "GPS (projected)", "x": xs, "y": ys}


def _waypoints(log: ULog, projection: _Projection | None) -> dict | None:
    """The commanded positions, as points rather than a path.

    Joining them would draw legs the aircraft was never asked to fly: the
    triplet is republished continuously, and consecutive entries are the same
    waypoint until the mission advances.
    """
    if not log.has("position_setpoint_triplet"):
        return None
    fields = log.data.get("position_setpoint_triplet", {}).get(0, {})
    valid = fields.get("current.valid") or []
    xs: list[float] = []
    ys: list[float] = []
    last: tuple[float, float] | None = None

    def add(east: float, north: float) -> None:
        nonlocal last
        point = (round(east, 2), round(north, 2))
        if point == last:
            return
        last = point
        xs.append(point[0])
        ys.append(point[1])

    lats = fields.get("current.lat") or []
    lons = fields.get("current.lon") or []
    if projection is not None and lats and lons:
        for index in range(min(len(lats), len(lons))):
            if valid and index < len(valid) and not valid[index]:
                continue
            lat = _degrees(lats[index])
            lon = _degrees(lons[index])
            if lat is None or lon is None or (lat == 0.0 and lon == 0.0):
                continue
            north, east = projection.project(lat, lon)
            add(east, north)
    else:
        # Firmware that publishes the triplet in the local frame only.
        norths = fields.get("current.x") or []
        easts = fields.get("current.y") or []
        for index in range(min(len(norths), len(easts))):
            if valid and index < len(valid) and not valid[index]:
                continue
            try:
                add(float(easts[index]), float(norths[index]))
            except (TypeError, ValueError):
                continue
    if not xs:
        return None
    return {"name": "Commanded position", "x": xs, "y": ys, "draw": "markers"}


def _plot_track(log: ULog) -> dict | None:
    """The flight path from above — the one plot that is not against time.

    Four things share the axes because the question is what disagrees: where the
    estimator thought it was, where it was told to go, where GPS said it was,
    and which points were actually commanded. Read apart, none of them answers
    it; the gap between the estimate and the projected GPS *is* the finding.
    """
    if not log.has("vehicle_local_position"):
        return None
    estimate = _xy_path(log, "vehicle_local_position", "x", "y", "Estimated")
    if estimate is None:
        return None
    if (max(estimate["x"]) - min(estimate["x"]) < 0.5
            and max(estimate["y"]) - min(estimate["y"]) < 0.5):
        return None            # never moved; a dot is not a track
    projection = _local_origin(log)
    # Bottom to top, deliberately. Plotly draws later traces over earlier ones,
    # and a noisy GPS trace laid over the estimate hides the very line the plot
    # is about. References first, the estimate on top of them, the commanded
    # points on top of everything because they are the fewest.
    series = [
        _gps_path(log, projection) if projection is not None else None,
        _xy_path(log, "vehicle_local_position_setpoint", "x", "y", "Setpoint"),
        estimate,
        _waypoints(log, projection),
    ]
    note = ("Local position, north up, equal axes. Origin is where the "
            "estimator started, not a survey point.")
    if projection is not None:
        note += (" GPS is projected through the estimator's own origin, so a "
                 "gap between the two tracks is estimator error and not a "
                 "difference of frames.")
    else:
        note += (" This flight had no global reference, so there is no GPS "
                 "track to compare against.")
    return _plot("track", "Ground track", "m", series,
                 group="Flight", xlabel="East (m)", ylabel="North (m)",
                 equal=True, note=note)


def _plot_velocity(log: ULog) -> dict | None:
    if not log.has("vehicle_local_position"):
        return None
    series = []
    vx = log.series("vehicle_local_position", "vx", 0)
    vy = log.series("vehicle_local_position", "vy", 0)
    if vx and vy:
        span = min(len(vx), len(vy))
        ground = [math.hypot(float(vx[i]), float(vy[i])) for i in range(span)]
        series.append(_series_from_column(log, "vehicle_local_position",
                                          ground, "Ground speed"))
    vz = _first_field(log, "vehicle_local_position", ("vz",))
    if vz:
        # NED again: down is positive, and a climb should read as a climb.
        series.append(_series(log, "vehicle_local_position", vz, "Climb rate",
                              scale=-1.0))
    return _plot("velocity", "Speed", "m/s", series, group="Flight")


def _plot_thrust(log: ULog) -> dict | None:
    """Collective thrust demand — the context every other plot is read against."""
    topic = _first_topic(log, ("vehicle_thrust_setpoint", "actuator_controls_0"))
    if topic is None:
        return None
    series = []
    if topic == "vehicle_thrust_setpoint":
        values = log.series(topic, "xyz", 0)
        if values and isinstance(values[0], list) and len(values[0]) >= 3:
            column = [abs(float(row[2])) for row in values]
            series.append(_series_from_column(log, topic, column, "Thrust"))
    else:
        values = log.series(topic, "control", 0)
        if values and isinstance(values[0], list) and len(values[0]) >= 4:
            column = [float(row[3]) for row in values]
            series.append(_series_from_column(log, topic, column, "Throttle"))
    return _plot("thrust", "Thrust demand", "normalised", series, group="Control",
                 note="Read the vibration and current plots against this: both "
                      "are expected to rise with thrust.")


def _plot_mag(log: ULog) -> dict | None:
    """Magnetic field strength. A norm that moves with throttle is current
    from the power wiring reaching the magnetometer."""
    topic = _first_topic(log, ("vehicle_magnetometer", "sensor_mag"))
    if topic is None:
        return None
    series = []
    for multi_id in log.instances(topic)[:3]:
        values = log.series(topic, "magnetometer_ga", multi_id)
        if values and isinstance(values[0], list) and len(values[0]) >= 3:
            norm = [math.sqrt(sum(float(c) ** 2 for c in row[:3])) for row in values]
        else:
            x = log.series(topic, "x", multi_id)
            y = log.series(topic, "y", multi_id)
            z = log.series(topic, "z", multi_id)
            if not (x and y and z):
                continue
            span = min(len(x), len(y), len(z))
            norm = [math.sqrt(float(x[i]) ** 2 + float(y[i]) ** 2 + float(z[i]) ** 2)
                    for i in range(span)]
        series.append(_series_from_column(log, topic, norm, f"Mag {multi_id}", multi_id))
    return _plot("mag", "Magnetic field strength", "gauss", series, group="Sensors",
                 note="A flat line is what you want. Movement that tracks "
                      "throttle is interference from the power wiring, and it "
                      "is what makes a compass calibration not stick.")


def _plot_imu_temp(log: ULog) -> dict | None:
    if not log.has("vehicle_imu_status"):
        return None
    series = []
    for multi_id in log.instances("vehicle_imu_status")[:3]:
        field = _first_field(log, "vehicle_imu_status",
                             ("temperature_accel", "temperature_gyro"), multi_id)
        if field:
            series.append(_series(log, "vehicle_imu_status", field,
                                  f"IMU {multi_id}", multi_id))
    return _plot("imu_temp", "IMU temperature", "°C", series, group="Sensors",
                 note="A sensor still warming up drifts. A cold first flight of "
                      "the day is where that shows.")


def _plot_bias(log: ULog) -> dict | None:
    """Estimator sensor bias. Large or moving bias is a sensor, not a filter."""
    topic = _first_topic(log, ("estimator_sensor_bias", "estimator_states"))
    if topic != "estimator_sensor_bias":
        return None
    series = _vector(log, topic, "gyro_bias", ("Gyro X", "Gyro Y", "Gyro Z"),
                     scale=180.0 / math.pi)
    return _plot("bias", "Estimated gyro bias", "deg/s", series, group="Estimator",
                 note="Bias should settle and stay put. One that keeps walking "
                      "is a gyro worth replacing.")


def _plot_rc(log: ULog) -> dict | None:
    topic = _first_topic(log, ("input_rc", "rc_channels"))
    if topic is None:
        return None
    series = []
    rssi = _first_field(log, topic, ("rssi", "rssi_dbm"))
    if rssi:
        series.append(_series(log, topic, rssi, "RC signal"))
    lost = _first_field(log, topic, ("rc_lost", "signal_lost"))
    if lost:
        series.append(_series(log, topic, lost, "Signal lost (1 = yes)"))
    return _plot("rc", "RC link", "rssi / flag", series, group="System",
                 note="A signal-lost flag that goes high in flight is a "
                      "failsafe waiting to happen.")


def _plot_airspeed(log: ULog) -> dict | None:
    """Fixed-wing only; absent on a multirotor and correctly skipped there."""
    if not log.has("airspeed"):
        return None
    series = []
    for field, label in (("indicated_airspeed_m_s", "Indicated"),
                         ("true_airspeed_m_s", "True")):
        found = _first_field(log, "airspeed", (field,))
        if found:
            series.append(_series(log, "airspeed", found, label))
    return _plot("airspeed", "Airspeed", "m/s", series, group="Flight")


def _plot_distance(log: ULog) -> dict | None:
    if not log.has("distance_sensor"):
        return None
    series = []
    for multi_id in log.instances("distance_sensor")[:2]:
        field = _first_field(log, "distance_sensor", ("current_distance",), multi_id)
        if field:
            series.append(_series(log, "distance_sensor", field,
                                  f"Rangefinder {multi_id}", multi_id))
    return _plot("distance", "Rangefinder", "m", series, group="Sensors")


def _plot_wind(log: ULog) -> dict | None:
    topic = _first_topic(log, ("wind", "wind_estimate"))
    if topic is None:
        return None
    north = log.series(topic, "windspeed_north", 0)
    east = log.series(topic, "windspeed_east", 0)
    if not north or not east:
        return None
    span = min(len(north), len(east))
    speed = [math.hypot(float(north[i]), float(east[i])) for i in range(span)]
    return _plot("wind", "Estimated wind", "m/s",
                 [_series_from_column(log, topic, speed, "Wind speed")],
                 group="Flight",
                 note="Estimated, not measured — but a climbing estimate "
                      "explains a lot of otherwise puzzling tracking error.")


def _plot_power(log: ULog) -> dict | None:
    """What the pack had left. The number that decides the next flight."""
    if not log.has("battery_status"):
        return None
    series = []
    remaining = _first_field(log, "battery_status", ("remaining",))
    if remaining:
        series.append(_series(log, "battery_status", remaining, "Remaining",
                              scale=100.0))
    used = _first_field(log, "battery_status", ("discharged_mah",))
    if used:
        series.append(_series(log, "battery_status", used, "Discharged (mAh)"))
    return _plot("power", "Battery state", "% / mAh", series, group="System")


def _plot_gps_quality(log: ULog) -> dict | None:
    topic = _first_topic(log, ("vehicle_gps_position", "sensor_gps"))
    if topic is None:
        return None
    series = []
    found = _first_field(log, topic, ("fix_type",))
    if found:
        series.append(_series(log, topic, found, "Fix type"))
    return _plot("gps_quality", "GPS fix type", "fix", series, group="Sensors",
                 note="Below 3 there is no 3-D fix, and every position plot "
                      "above is running on dead reckoning.")


def _plot_altitude_sources(log: ULog) -> dict | None:
    """The three altitudes on one axis: estimator, GPS and barometer.

    The single most informative estimation plot there is. Each source fails in
    its own way — GPS jumps, the barometer drifts with weather and prop wash,
    the estimator blends them — and the only way to see which one is lying is
    to put them side by side.
    """
    series = []
    reference = _reference_altitude(log)

    gps_topic = _first_topic(log, ("vehicle_gps_position", "sensor_gps"))
    if gps_topic:
        field = _first_field(log, gps_topic, ("alt",))
        if field:
            values = log.series(gps_topic, field, 0)
            # PX4 logs GPS altitude as int32 millimetres; a float field is
            # already metres. Guessing wrong puts one trace 1000x off.
            scale = 1e-3 if values and isinstance(values[0], int) else 1.0
            series.append(_series(log, gps_topic, field, "GPS altitude (MSL)",
                                  scale=scale))

    baro_topic = _first_topic(log, ("vehicle_air_data", "sensor_baro"))
    if baro_topic:
        field = _first_field(log, baro_topic, ("baro_alt_meter", "altitude"))
        if field:
            series.append(_series(log, baro_topic, field, "Barometer altitude"))

    if log.has("vehicle_global_position"):
        field = _first_field(log, "vehicle_global_position", ("alt",))
        if field:
            series.append(_series(log, "vehicle_global_position", field,
                                  "Fused estimate"))
    elif log.has("vehicle_local_position"):
        z = log.series("vehicle_local_position", "z", 0)
        ref = log.series("vehicle_local_position", "ref_alt", 0)
        if z and ref:
            span = min(len(z), len(ref))
            amsl = [float(ref[i]) - float(z[i]) for i in range(span)]
            series.append(_series_from_column(log, "vehicle_local_position",
                                              amsl, "Fused estimate"))

    # What the controller was AIMING at, drawn as markers because it is sparse
    # and stepped. A fused estimate that sits away from its setpoint is the
    # difference between a sensor problem and a control problem.
    setpoint_topic = _first_topic(log, ("vehicle_local_position_setpoint",))
    if setpoint_topic and reference is not None:
        z = log.series(setpoint_topic, "z", 0)
        if z:
            amsl = [
                reference - float(v) if -1e5 < float(v) < 1e5 else float("nan")
                for v in z
            ]
            point = _series_from_column(log, setpoint_topic, amsl,
                                        "Altitude setpoint")
            point["draw"] = "markers"
            series.append(point)

    if len([s for s in series if s and s.get("draw") != "markers"]) < 2:
        return None            # one line is the altitude plot, not a comparison
    return _plot("alt_sources", "Altitude estimate", "m AMSL", series,
                 group="Estimator",
                 note="The three sources should track each other. GPS stepping "
                      "away is a fix problem; the barometer drifting away is "
                      "weather or prop wash reaching the sensor. The fused "
                      "estimate sitting away from its setpoint is neither — "
                      "that is the controller.")


def _reference_altitude(log: ULog) -> float | None:
    """The local frame's AMSL origin, so a NED setpoint can be plotted with
    the absolute altitudes rather than on its own axis."""
    for value in log.series("vehicle_local_position", "ref_alt", 0):
        try:
            number = float(value)
        except (TypeError, ValueError):
            continue
        if -2000.0 < number < 20000.0:
            return number
    return None


def _plot_gps_accuracy(log: ULog) -> dict | None:
    """Reported position uncertainty — the number the EKF weights GPS by."""
    topic = _first_topic(log, ("vehicle_gps_position", "sensor_gps"))
    if topic is None:
        return None
    series = []
    for field, label in (("eph", "Horizontal (eph)"), ("epv", "Vertical (epv)"),
                         ("s_variance_m_s", "Speed variance")):
        found = _first_field(log, topic, (field,))
        if found:
            series.append(_series(log, topic, found, label))
    return _plot("gps_accuracy", "GPS uncertainty", "m", series,
                 group="Sensors",
                 note="This is what the receiver claims, not what it achieved. "
                      "A value that climbs while the satellite count holds is "
                      "usually multipath — read it with the noise plot below.")


def _plot_gps_noise(log: ULog) -> dict | None:
    """Receiver noise and jamming. Usually the aircraft's own electronics."""
    topic = _first_topic(log, ("vehicle_gps_position", "sensor_gps"))
    if topic is None:
        return None
    series = []
    for field, label in (("noise_per_ms", "Noise per ms"),
                         ("jamming_indicator", "Jamming indicator")):
        found = _first_field(log, topic, (field,))
        if found:
            series.append(_series(log, topic, found, label))
    return _plot("gps_noise", "GPS noise and jamming", "indicator", series,
                 group="Sensors",
                 note="A jamming indicator that rises with throttle is almost "
                      "always the aircraft's own power wiring or video "
                      "transmitter, not somebody else's jammer.")


def _plot_baro(log: ULog) -> dict | None:
    """Barometer altitude and its temperature — drift has a cause."""
    topic = _first_topic(log, ("vehicle_air_data", "sensor_baro"))
    if topic is None:
        return None
    series = []
    altitude = _first_field(log, topic, ("baro_alt_meter", "altitude"))
    if altitude:
        series.append(_series(log, topic, altitude, "Baro altitude"))
    temperature = _first_field(log, topic, ("baro_temp_celcius", "temperature"))
    if temperature:
        series.append(_series(log, topic, temperature, "Sensor temperature"))
    return _plot("baro", "Barometer", "m / °C", series, group="Sensors",
                 note="Barometric altitude drifts with temperature and with air "
                      "moving over the sensor — a reading that walks while the "
                      "aircraft sits still is the sensor, not the aircraft.")


def _plot_gps_velocity(log: ULog) -> dict | None:
    """GPS velocity against the estimate — the horizontal-velocity check."""
    gps_topic = _first_topic(log, ("vehicle_gps_position", "sensor_gps"))
    if gps_topic is None or not log.has("vehicle_local_position"):
        return None
    series = []
    north = log.series(gps_topic, "vel_n_m_s", 0)
    east = log.series(gps_topic, "vel_e_m_s", 0)
    if north and east:
        span = min(len(north), len(east))
        speed = [math.hypot(float(north[i]), float(east[i])) for i in range(span)]
        series.append(_series_from_column(log, gps_topic, speed, "GPS"))
    vx = log.series("vehicle_local_position", "vx", 0)
    vy = log.series("vehicle_local_position", "vy", 0)
    if vx and vy:
        span = min(len(vx), len(vy))
        speed = [math.hypot(float(vx[i]), float(vy[i])) for i in range(span)]
        series.append(_series_from_column(log, "vehicle_local_position",
                                          speed, "Estimator"))
    if len(series) < 2:
        return None
    return _plot("gps_velocity", "Horizontal velocity: GPS vs estimate", "m/s",
                 series, group="Estimator",
                 note="A persistent gap between the two is the estimator "
                      "distrusting GPS — cross-read it against the innovation "
                      "test ratios.")


def _plot_rate_error(log: ULog) -> dict | None:
    """Demanded rate minus measured rate, all three axes together.

    The per-axis plots draw the pair, and a pair that very nearly overlaps
    hides the only thing being asked about: the gap between them. Subtracted,
    the gap *is* the trace. Flat is tuned; a standing offset is saturation or
    trim; fast ringing about zero is a rate loop arguing with itself.

    The setpoint is held, not interpolated — the controller ran at 250 Hz
    against a setpoint published at 50, and the last value it was given is what
    it was actually trying to achieve.
    """
    topic = _first_topic(log, ("vehicle_angular_velocity", "sensor_combined"))
    setpoint = _first_topic(log, ("vehicle_rates_setpoint",))
    if topic is None or setpoint is None:
        return None
    field = "xyz" if topic == "vehicle_angular_velocity" else "gyro_rad"
    series = []
    for index, axis in enumerate(("roll", "pitch", "yaw")):
        times, measured = _raw_component(log, topic, field, index,
                                         scale=180.0 / math.pi)
        found = _first_field(log, setpoint, (axis,))
        if not times or found is None:
            continue
        sp_t, sp_v = _raw(log, setpoint, found, scale=180.0 / math.pi)
        if len(sp_v) < 2:
            continue
        held = _align(times, sp_t, sp_v)
        column = [held[i] - measured[i] if held[i] == held[i] else float("nan")
                  for i in range(len(measured))]
        out_t, out_v = _decimate(times, column)
        series.append({"name": f"{axis.capitalize()} error",
                       "x": [round(t, 3) for t in out_t],
                       "y": [None if v != v else round(v, 5) for v in out_v]})
    return _plot("rate_error", "Rate tracking error", "deg/s", series,
                 group="Control",
                 note="Setpoint minus measured, so zero is perfect. A trace "
                      "that sits off zero means the aircraft could not deliver "
                      "what was asked; one that buzzes about zero means the "
                      "gains are chasing noise.")


def _plot_esc(log: ULog) -> dict | None:
    """RPM the ESCs report back, on the airframes that have telemetry.

    Motor *commands* only say what was asked for. This says what happened, and
    the difference is where a failing motor, a damaged propeller or a bent arm
    shows up first: four motors given the same command in a level hover should
    report very nearly the same RPM.
    """
    if not log.has("esc_status"):
        return None
    counts = log.series("esc_status", "esc_count", 0)
    width = max(0, min(8, int(counts[0]) if counts else 8))
    series = []
    for index in range(width):
        times, column = _raw(log, "esc_status", f"esc[{index}].esc_rpm")
        if not column or max(column) <= 0:
            continue
        out_t, out_v = _decimate(times, column)
        series.append({"name": f"ESC {index + 1}",
                       "x": [round(t, 3) for t in out_t],
                       "y": [round(v, 1) for v in out_v]})
    return _plot("esc", "ESC RPM", "rpm", series, group="Airframe",
                 note="Reported by the ESCs, not commanded. One motor "
                      "consistently faster than its neighbours is carrying "
                      "more of the airframe; one that drops out and returns is "
                      "a motor or an ESC on its way out.")


# ---------------------------------------------------------------------------
# Flight modes
# ---------------------------------------------------------------------------
# Two different enums, and they are NOT interchangeable. `vehicle_status`
# carries NAVIGATION_STATE_*; the older `commander_state` carries MAIN_STATE_*,
# where 6 is Acro rather than Position Slow. Reading one table for the other
# would mislabel most of a flight, so each source gets its own.
_NAV_STATE = {
    0: "Manual", 1: "Altitude", 2: "Position", 3: "Mission", 4: "Loiter",
    5: "Return", 6: "Position slow", 10: "Acro", 12: "Descend",
    13: "Termination", 14: "Offboard", 15: "Stabilized", 17: "Takeoff",
    18: "Land", 19: "Follow target", 20: "Precision land", 21: "Orbit",
    22: "VTOL takeoff",
}
_MAIN_STATE = {
    0: "Manual", 1: "Altitude", 2: "Position", 3: "Mission", 4: "Loiter",
    5: "Return", 6: "Acro", 7: "Offboard", 8: "Stabilized", 9: "Rattitude",
    10: "Takeoff", 11: "Land", 12: "Follow target", 13: "Precision land",
    14: "Orbit",
}
# Shortest band worth drawing. A mode held for a few milliseconds during a
# transition is a sliver of colour that says nothing.
MIN_MODE_S = 0.4


def _flight_modes(log: ULog) -> list[dict[str, Any]]:
    """The flight as a list of mode intervals.

    This is what turns every other plot from a graph into a story: an
    oscillation in Position and the same oscillation in Manual are different
    findings, and without the mode behind the trace you cannot tell them apart.
    """
    topic = _first_topic(log, ("vehicle_status", "commander_state"))
    if topic is None:
        return []
    if topic == "vehicle_status":
        field = _first_field(log, topic, ("nav_state",))
        table = _NAV_STATE
    else:
        field = _first_field(log, topic, ("main_state",))
        table = _MAIN_STATE
    if field is None:
        return []

    states = log.series(topic, field, 0)
    stamps = log.series(topic, "timestamp", 0)
    if not _advancing(stamps):
        # A frozen clock cannot place the bands. Wrong shading is worse than
        # none: it would relabel the whole flight.
        return []
    times = _time_axis(log, topic, 0)
    span = min(len(states), len(times))
    if span == 0:
        return []

    spans: list[dict[str, Any]] = []
    current = None
    start = times[0]
    for index in range(span):
        state = int(states[index])
        if state != current:
            if current is not None:
                spans.append({"mode": table.get(current, f"Mode {current}"),
                              "state": current, "start": start, "end": times[index]})
            current = state
            start = times[index]
    spans.append({"mode": table.get(current, f"Mode {current}"), "state": current,
                  "start": start, "end": times[span - 1]})

    out = []
    for entry in spans:
        entry["start"] = round(entry["start"], 2)
        entry["end"] = round(entry["end"], 2)
        if entry["end"] - entry["start"] >= MIN_MODE_S:
            out.append(entry)
    return out


def _armed_spans(log: ULog) -> list[dict[str, float]]:
    """When the vehicle was armed — the part of the log that flew."""
    topic = _first_topic(log, ("vehicle_status",))
    if topic is None:
        return []
    field = _first_field(log, topic, ("arming_state",))
    if field is None:
        return []
    states = log.series(topic, field, 0)
    if not _advancing(log.series(topic, "timestamp", 0)):
        return []
    times = _time_axis(log, topic, 0)
    span = min(len(states), len(times))
    out: list[dict[str, float]] = []
    start: float | None = None
    for index in range(span):
        # PX4 arming_state: 2 is ARMED in every version that logs this field.
        armed = int(states[index]) == 2
        if armed and start is None:
            start = times[index]
        elif not armed and start is not None:
            out.append({"start": round(start, 2), "end": round(times[index], 2)})
            start = None
    if start is not None and span:
        out.append({"start": round(start, 2), "end": round(times[span - 1], 2)})
    return [s for s in out if s["end"] - s["start"] >= MIN_MODE_S]


# ---------------------------------------------------------------------------
# Measuring, rather than eyeballing
# ---------------------------------------------------------------------------
# Everything below reads the RAW columns, never the plotted ones. Decimation
# here keeps the extreme sample of each bucket, which is honest about how high
# a signal went and a lie about how long it stayed there: one spike becomes a
# whole bucket. A finding that says "for 8 seconds" has to have counted real
# samples.


def _raw(log: ULog, topic: str, field: str, multi_id: int = 0,
         scale: float = 1.0) -> tuple[list[float], list[float]]:
    """Undecimated ``(times, values)`` for one field, in seconds and floats."""
    values = log.series(topic, field, multi_id)
    if not values:
        return [], []
    times = _time_axis(log, topic, multi_id)
    span = min(len(times), len(values))
    out_t: list[float] = []
    out_v: list[float] = []
    for index in range(span):
        try:
            number = float(values[index])
        except (TypeError, ValueError):
            continue
        if number != number:
            continue
        out_t.append(times[index])
        out_v.append(number * scale)
    return out_t, out_v


def _raw_component(log: ULog, topic: str, field: str, index: int,
                   multi_id: int = 0, scale: float = 1.0) -> tuple[list[float], list[float]]:
    """One component of an array field, undecimated."""
    values = log.series(topic, field, multi_id)
    if not values or not isinstance(values[0], list):
        return [], []
    times = _time_axis(log, topic, multi_id)
    span = min(len(times), len(values))
    out_t: list[float] = []
    out_v: list[float] = []
    for row_index in range(span):
        row = values[row_index]
        if index >= len(row):
            continue
        try:
            number = float(row[index])
        except (TypeError, ValueError):
            continue
        if number != number:
            continue
        out_t.append(times[row_index])
        out_v.append(number * scale)
    return out_t, out_v


def _sample_dt(times: list[float]) -> float:
    """Mean spacing of a time column, or 0 when there is not enough of one."""
    if len(times) < 2:
        return 0.0
    span = times[-1] - times[0]
    return span / (len(times) - 1) if span > 0 else 0.0


def _time_above(times: list[float], values: list[float], threshold: float,
                below: bool = False) -> tuple[float, float | None, float]:
    """``(seconds past the threshold, when it first was, the worst value)``.

    Seconds are accumulated per sample interval rather than counted as samples,
    so a signal logged at 5 Hz and one logged at 500 Hz report the same number
    for the same event. That is the whole reason this is worth stating: a peak
    says how bad it got, and only a duration says whether it mattered.
    """
    total = 0.0
    first: float | None = None
    worst = float("-inf") if not below else float("inf")
    dt = _sample_dt(times)
    for index, value in enumerate(values):
        past = value < threshold if below else value > threshold
        if not past:
            continue
        if first is None:
            first = times[index]
        worst = min(worst, value) if below else max(worst, value)
        step = (times[index + 1] - times[index]) if index + 1 < len(times) else dt
        total += max(0.0, min(step, 1.0))
    if first is None:
        return 0.0, None, 0.0
    return total, first, worst


def _within(times: list[float], values: list[float],
            spans: list[dict[str, float]]) -> tuple[list[float], list[float]]:
    """The part of a signal that falls inside *spans* — normally armed flight.

    A review that measures tracking error or oscillation over the whole file
    measures the bench test at the start of it too, and a motor twitching on a
    workbench is not a finding about the flight.
    """
    if not spans:
        return times, values
    out_t: list[float] = []
    out_v: list[float] = []
    index = 0
    for span in spans:
        while index < len(times) and times[index] < span["start"]:
            index += 1
        while index < len(times) and times[index] <= span["end"]:
            out_t.append(times[index])
            out_v.append(values[index])
            index += 1
    return out_t, out_v


def _align(times: list[float], other_t: list[float],
           other_v: list[float]) -> list[float]:
    """Sample ``(other_t, other_v)`` at *times*, holding the last known value.

    Two PX4 topics are never on the same clock: attitude runs at 250 Hz and its
    setpoint at 50, so subtracting them index by index would compare a sample
    with whatever setpoint happened to share its position in a shorter list.
    Zero-order hold is what the controller itself saw — the last setpoint it
    was given — so the difference is the real tracking error and not an
    artefact of two sample rates.
    """
    out: list[float] = []
    cursor = 0
    last = float("nan")
    for moment in times:
        while cursor < len(other_t) and other_t[cursor] <= moment:
            last = other_v[cursor]
            cursor += 1
        out.append(last)
    return out


def _percentile(values: list[float], fraction: float) -> float:
    """A percentile of *values*, used instead of the maximum.

    One sample at the wrong microsecond is not a finding. The 95th percentile
    of a tracking error is the level the aircraft actually lived at, which is
    what decides whether anything needs doing.
    """
    if not values:
        return 0.0
    ordered = sorted(values)
    position = max(0, min(len(ordered) - 1,
                          int(round(fraction * (len(ordered) - 1)))))
    return ordered[position]


def _correlate(a: list[float], b: list[float]) -> float | None:
    """Pearson correlation of two aligned columns, or None when meaningless."""
    pairs = [(x, y) for x, y in zip(a, b)
             if x == x and y == y]
    if len(pairs) < 50:
        return None
    n = float(len(pairs))
    mean_a = sum(p[0] for p in pairs) / n
    mean_b = sum(p[1] for p in pairs) / n
    var_a = sum((p[0] - mean_a) ** 2 for p in pairs)
    var_b = sum((p[1] - mean_b) ** 2 for p in pairs)
    if var_a <= 0 or var_b <= 0:
        return None
    covariance = sum((p[0] - mean_a) * (p[1] - mean_b) for p in pairs)
    return covariance / math.sqrt(var_a * var_b)


def _oscillation(times: list[float], values: list[float]) -> tuple[float, float] | None:
    """``(amplitude in the signal's own unit, frequency in Hz)``, or None.

    The fast component of a signal is what a badly tuned rate loop adds to it:
    the aircraft's actual motion is slow — a second or more per cycle — and a
    ringing D term is tens of hertz. Subtracting a short moving average leaves
    exactly that fast part, its RMS is how large the ringing is, and its zero
    crossings are how quickly it rings.

    Refuses to answer below 50 Hz of data. Telemetry-rate attitude aliases a
    20 Hz oscillation into something that looks like a slow wobble, and a
    confident frequency read off aliased samples is worse than no answer.

    The amplitude is a floor rather than an exact figure: subtracting a moving
    average leaves part of the slower components behind, so a ring near the
    bottom of the detectable band reads smaller than it is. Erring that way is
    the right direction — it understates rather than invents.
    """
    if len(values) < 200:
        return None
    dt = _sample_dt(times)
    if dt <= 0:
        return None
    rate = 1.0 / dt
    if rate < 50.0 or times[-1] - times[0] < 2.0:
        return None
    window = int(round(0.15 * rate)) | 1          # odd, so it has a centre
    if window < 3 or window >= len(values):
        return None
    half = window // 2
    running = sum(values[:window])
    ac: list[float] = []
    for centre in range(half, len(values) - half):
        ac.append(values[centre] - running / window)
        leaving = centre - half
        entering = centre + half + 1
        if entering < len(values):
            running += values[entering] - values[leaving]
    if len(ac) < 100:
        return None
    rms = math.sqrt(sum(v * v for v in ac) / len(ac))
    crossings = sum(1 for i in range(1, len(ac))
                    if (ac[i - 1] < 0) != (ac[i] < 0))
    seconds = (len(ac) - 1) * dt
    if seconds <= 0:
        return None
    return rms, crossings / (2.0 * seconds)


def _mode_at(modes: list[dict[str, Any]], when: float | None) -> str:
    """Which flight mode was active at *when*.

    Half-open on purpose. One span's end is the next one's start, and a finding
    about the moment a mode was entered — the switch into Return — reads as
    happening in the mode being left if the boundary belongs to both.
    """
    if when is None:
        return ""
    for span in modes:
        if span["start"] <= when < span["end"]:
            return str(span["mode"])
    return str(modes[-1]["mode"]) if modes and when >= modes[-1]["end"] else ""


def _finding(level: str, text: str, when: float | None = None,
             modes: list[dict[str, Any]] | None = None,
             detail: str = "") -> dict[str, Any]:
    """One finding: a headline, and the explanation behind it.

    The split is what keeps a page of findings readable. Every one of these
    needs a paragraph to be useful and none of them needs it *immediately* —
    a dozen paragraphs stacked up is a wall nobody reads past the second of,
    and the reader loses the one line that mattered. So the claim is one line
    and the reasoning is a click away.

    The time is the difference between a review and a verdict: "vibration
    peaked at 48" is a number to argue with, and "48 at 4:12, in Position" is
    a place to look.
    """
    out: dict[str, Any] = {"level": level, "text": text}
    if detail:
        out["detail"] = detail
    if when is not None:
        out["t"] = round(when, 1)
        mode = _mode_at(modes or [], when)
        if mode:
            out["mode"] = mode
    return out


def _peak_of(plot: dict | None) -> tuple[str, float, float | None]:
    """``(series name, peak, when)`` across every series of a plot."""
    if not plot:
        return "", 0.0, None
    name, peak, when = "", float("-inf"), None
    for series in plot["series"]:
        for index, value in enumerate(series["y"]):
            if value is None or value <= peak:
                continue
            peak, name = value, series["name"]
            when = series["x"][index] if index < len(series["x"]) else None
    return (name, peak, when) if peak > float("-inf") else ("", 0.0, None)


# ---------------------------------------------------------------------------
# Detectors — the specific problems this review knows how to name
# ---------------------------------------------------------------------------

# A normalised motor command this close to 1.0 is the mixer asking a motor for
# everything it has and being told no.
_SATURATED = 0.95
# Below this it is a gust, not a finding.
_SATURATION_S = 0.5
# A motor under this still has room to give, so a neighbour at its limit
# is an imbalance rather than an aircraft asked for everything it had.
_HEADROOM = 0.8


def _longest(spans: list[dict[str, float]]) -> list[dict[str, float]]:
    """The single longest span, or nothing. Used where a measurement needs an
    evenly sampled stretch and stitching two flights together would fake one."""
    if not spans:
        return []
    return [max(spans, key=lambda s: s["end"] - s["start"])]


def _saturated_motor(log: ULog, armed: list[dict[str, float]]) -> tuple[str, float, float] | None:
    """The motor that spent longest at its limit *while the others had not*.

    Differential saturation only, and that qualification is the whole value of
    the finding. Every motor at its limit together is a pilot asking for full
    throttle — a climb, not a fault — and reporting a hard climb as a critical
    problem is precisely how a review teaches its reader to stop reading
    findings. One motor pinned while its neighbours still have headroom is the
    opposite: the mixer wanted to correct and could not.

    Only ``actuator_motors`` is read, never the older ``actuator_outputs``:
    there, channel 5 at 2000 µs is as likely to be a flap servo at full
    deflection as a motor in trouble, and a review that cries motor failure at
    a fixed wing's aileron is a review nobody trusts twice.
    """
    if not log.has("actuator_motors"):
        return None
    values = log.series("actuator_motors", "control", 0)
    if not values or not isinstance(values[0], list):
        return None
    width = min(8, len(values[0]))
    if width < 3:
        return None            # nothing to compare a pinned motor against
    times = _time_axis(log, "actuator_motors", 0)
    span = min(len(times), len(values))
    # One 0/1 column per motor: 1 where that motor is at its limit and at least
    # one other is not. Counted in seconds afterwards, like every other duration.
    flags = [[0.0] * span for _ in range(width)]
    for index in range(span):
        row = values[index]
        if len(row) < width:
            continue
        try:
            outputs = [float(row[i]) for i in range(width)]
        except (TypeError, ValueError):
            continue
        if min(outputs) > _HEADROOM:
            continue           # everything is high: the aircraft was climbing
        for motor, value in enumerate(outputs):
            if value >= _SATURATED:
                flags[motor][index] = 1.0
    worst: tuple[str, float, float] | None = None
    for motor in range(width):
        when, column = _within(times[:span], flags[motor], armed)
        seconds, first, _ = _time_above(when, column, 0.5)
        if seconds >= _SATURATION_S and (worst is None or seconds > worst[1]):
            worst = (f"Motor {motor + 1}", seconds, first or 0.0)
    return worst


# PX4's estimator bumps a counter every time it discards its own answer and
# starts again from a measurement. Each bump is a step in the estimate — and
# therefore in what the controller was flying towards.
_RESET_COUNTERS = (
    ("xy_reset_counter", "horizontal position"),
    ("z_reset_counter", "height"),
    ("vxy_reset_counter", "horizontal velocity"),
    ("heading_reset_counter", "heading"),
)


def _estimator_resets(log: ULog,
                      armed: list[dict[str, float]]) -> list[tuple[str, int, float]]:
    """``(what was reset, how many times, when it first happened)``, armed only.

    An estimator that resets on the ground is an estimator starting up: the
    first GPS fix arrives, the filter throws away its dead-reckoned guess and
    takes the measurement, and every counter in the topic ticks. That is the
    system working. Counting it produced four separate alarms about a perfectly
    ordinary boot, which is worse than counting nothing — after the ground is
    excluded, what is left is the estimator changing its mind in flight.
    """
    out: list[tuple[str, int, float]] = []
    for field, label in _RESET_COUNTERS:
        times, values = _within(*_raw(log, "vehicle_local_position", field), armed)
        if len(values) < 2:
            continue
        count = 0
        first: float | None = None
        for index in range(1, len(values)):
            # The counters are uint8 and wrap; the STEP is the event, and a
            # step of 200 is a wrap read backwards rather than 200 resets.
            step = (int(values[index]) - int(values[index - 1])) % 256
            if 0 < step < 32:
                count += step
                if first is None:
                    first = times[index]
        if count:
            out.append((label, count, first or 0.0))
    return out


# vehicle_status.failure_detector_status, as PX4 packs it. These are the
# autopilot's own conclusions about its airframe, which is a different and
# stronger thing than a number crossing a threshold in this file.
_FAILURE_BITS = (
    (1, "roll exceeded its limit"),
    (2, "pitch exceeded its limit"),
    (4, "altitude ran away"),
    (8, "an external failure trigger fired"),
    (16, "an ESC failed to arm"),
    (32, "the battery failed"),
    (64, "a propeller is imbalanced"),
    (128, "a motor stopped responding"),
)


def _failure_detector(log: ULog) -> tuple[list[str], float | None]:
    """What PX4's own failure detector flagged, and when it first did."""
    times, values = _raw(log, "vehicle_status", "failure_detector_status")
    flagged: list[str] = []
    first: float | None = None
    for index, value in enumerate(values):
        bits = int(value)
        if not bits:
            continue
        if first is None:
            first = times[index]
        for mask, label in _FAILURE_BITS:
            if bits & mask and label not in flagged:
                flagged.append(label)
    return flagged, first


def _failsafe(log: ULog) -> tuple[float, float | None]:
    """``(seconds in failsafe, when it started)``."""
    times, values = _raw(log, "vehicle_status", "failsafe")
    return _time_above(times, values, 0.5)[:2]


def _attitude_error(log: ULog, axis: str,
                    armed: list[dict[str, float]]) -> tuple[float, float, float | None] | None:
    """``(typical error, peak error, when the peak was)`` in degrees.

    "Typical" is the 95th percentile rather than the mean or the maximum: a
    mean is dragged to nothing by the hover, and a maximum is one sample at the
    moment a stick moved. The 95th is the level the aircraft actually lived at.
    """
    euler = _euler(log)
    setpoint_topic = _first_topic(log, ("vehicle_attitude_setpoint",))
    if euler is None or setpoint_topic is None:
        return None
    topic, roll, pitch, yaw = euler
    column = {"roll": roll, "pitch": pitch, "yaw": yaw}[axis]
    times = _time_axis(log, topic, 0)
    span = min(len(times), len(column))
    field = _first_field(log, setpoint_topic, (f"{axis}_body", axis))
    if field is None or span < 50:
        return None
    sp_t, sp_v = _raw(log, setpoint_topic, field, scale=180.0 / math.pi)
    if len(sp_v) < 10:
        return None
    times, column = _within(times[:span], column[:span], armed)
    if len(times) < 50:
        return None
    held = _align(times, sp_t, sp_v)
    errors: list[float] = []
    peak, peak_at = 0.0, None
    for index, value in enumerate(column):
        target = held[index]
        if target != target or value != value:
            continue
        error = abs(value - target)
        errors.append(error)
        if error > peak:
            peak, peak_at = error, times[index]
    if len(errors) < 50:
        return None
    return _percentile(errors, 0.95), peak, peak_at


def _rate_oscillation(log: ULog, axis: str,
                      armed: list[dict[str, float]]) -> tuple[float, float] | None:
    """Amplitude (deg/s) and frequency (Hz) of ringing on one rate axis."""
    index = {"roll": 0, "pitch": 1, "yaw": 2}[axis]
    topic = _first_topic(log, ("vehicle_angular_velocity", "sensor_combined"))
    if topic is None:
        return None
    field = "xyz" if topic == "vehicle_angular_velocity" else "gyro_rad"
    times, column = _raw_component(log, topic, field, index, scale=180.0 / math.pi)
    times, column = _within(times, column, _longest(armed))
    return _oscillation(times, column)


def _thrust_column(log: ULog) -> tuple[list[float], list[float]]:
    """Collective thrust demand, undecimated — the thing other signals are
    tested against for a common cause."""
    topic = _first_topic(log, ("vehicle_thrust_setpoint", "actuator_controls_0"))
    if topic is None:
        return [], []
    if topic == "vehicle_thrust_setpoint":
        times, values = _raw_component(log, topic, "xyz", 2)
        return times, [abs(v) for v in values]
    return _raw_component(log, topic, "control", 3)


def _mag_norm(log: ULog) -> tuple[list[float], list[float]]:
    """Magnetic field strength, undecimated."""
    topic = _first_topic(log, ("vehicle_magnetometer", "sensor_mag"))
    if topic is None:
        return [], []
    values = log.series(topic, "magnetometer_ga", 0)
    times = _time_axis(log, topic, 0)
    if values and isinstance(values[0], list) and len(values[0]) >= 3:
        span = min(len(times), len(values))
        return times[:span], [
            math.sqrt(sum(float(c) ** 2 for c in values[i][:3])) for i in range(span)
        ]
    xs = log.series(topic, "x", 0)
    ys = log.series(topic, "y", 0)
    zs = log.series(topic, "z", 0)
    if not (xs and ys and zs):
        return [], []
    span = min(len(times), len(xs), len(ys), len(zs))
    return times[:span], [
        math.sqrt(float(xs[i]) ** 2 + float(ys[i]) ** 2 + float(zs[i]) ** 2)
        for i in range(span)
    ]


def _tracks_thrust(log: ULog, times: list[float], values: list[float],
                   armed: list[dict[str, float]]) -> float | None:
    """How closely a signal follows the throttle, or None when it cannot be asked.

    Correlation is what separates two findings that look identical on a plot:
    vibration that rises with throttle is the rotating parts, and vibration
    that does not is the mounting. Guessing at that from two graphs side by
    side is exactly the guess a reviewer gets wrong.
    """
    thrust_t, thrust_v = _thrust_column(log)
    if len(thrust_v) < 50:
        return None
    times, values = _within(times, values, armed)
    if len(times) < 50:
        return None
    held = _align(times, thrust_t, thrust_v)
    finite = [v for v in held if v == v]
    # A throttle that never moved cannot explain anything moving with it.
    if len(finite) < 50 or max(finite) - min(finite) < 0.2:
        return None
    return _correlate(values, held)


# Modes PX4 selects for itself when something has gone wrong. Reaching one is
# not proof of a failsafe — a pilot can ask for Return — but it is always worth
# saying, because the reason is then one line away in the messages.
_RECOVERY_MODES = ("Return", "Descend", "Termination", "Precision land")


# ---------------------------------------------------------------------------
# Findings
# ---------------------------------------------------------------------------

def _findings(log: ULog, plots: list[dict], modes: list[dict[str, Any]],
              armed: list[dict[str, float]]) -> list[dict[str, Any]]:
    """The few lines worth reading before the plots.

    Only things that are actually derivable from this log — no scoring, no
    grades. A review that invents a verdict is worse than one that points at
    the plot and lets the operator look.

    Two rules keep this list short enough to be read at all. Each finding is a
    headline with its reasoning behind it rather than a paragraph on the page,
    and nothing appears that the log does not clearly support: a threshold on
    its own is not evidence, so a motor at its limit has to be at its limit
    *alone*, an estimator reset has to happen in the air, and a compass has to
    swing by a believable amount before the wiring is blamed for it.

    Each finding carries the second of the flight it was measured at and the
    mode flown then, because "vibration peaked at 48" is a number to argue with
    and "48 at 4:12, in Position" is a place to look.
    """
    out: list[dict[str, Any]] = []
    by_id = {p["id"]: p for p in plots}

    def add(level: str, text: str, detail: str = "",
            when: float | None = None) -> None:
        out.append(_finding(level, text, when, modes, detail))

    # PX4's own conclusion about its airframe comes first. It is a stronger
    # statement than anything derived here: the autopilot decided, in flight,
    # that something had broken.
    flagged, flagged_at = _failure_detector(log)
    if flagged:
        add("critical",
            "PX4's failure detector fired: " + ", ".join(flagged),
            "That is the autopilot's own verdict on the airframe, reached in "
            "flight from more than this log holds — not a threshold crossed in "
            "this file. Whatever else is on this page, start here.",
            flagged_at)

    saturated = _saturated_motor(log, armed)
    if saturated:
        name, seconds, when = saturated
        add("critical",
            f"{name} was pinned at full output for {seconds:.1f} s while the "
            "others still had headroom",
            "One motor at its limit alone is the mixer wanting to correct and "
            "having nothing left to correct with — the aircraft was flying "
            "without control authority on that corner. Look for a heavy or "
            "bent airframe, a failing motor, or too little thrust margin for "
            "the weight. (Every motor at its limit together would just be a "
            "hard climb, and is not reported.)",
            when)

    clipping = by_id.get("clipping")
    if clipping:
        _, total, when = _peak_of(clipping)
        if total > 0:
            add("critical",
                f"Accelerometer clipping — {int(total)} samples",
                "The IMU saturated, so for those samples it reported a limit "
                "rather than a measurement. Treat the attitude and position "
                "estimates in this log with suspicion, and soften the "
                "flight-controller mounting before trusting the next one.",
                when)

    vibration = by_id.get("vibration")
    if vibration:
        _, worst, when = _peak_of(vibration)
        # Whether it rides on the throttle decides where to look, and that is a
        # different answer from how bad it got.
        cause = ""
        vibe_t, vibe_v = _raw(log, "vehicle_imu_status", "accel_vibration_metric")
        follows = _tracks_thrust(log, vibe_t, vibe_v, armed)
        if follows is not None and follows >= 0.5:
            cause = (f" It rises and falls with the throttle (correlation "
                     f"{follows:.2f}), which points at the propellers and "
                     "motors rather than at how the controller is mounted.")
        elif follows is not None and follows < 0.2:
            cause = (" It does not follow the throttle, so the mounting or a "
                     "loose airframe is a likelier cause than the rotating "
                     "parts.")
        if worst >= 60.0:
            add("critical", f"Vibration peaked at {worst:.0f} m/s²",
                "Above about 60 the estimator itself starts to suffer. Check "
                "propellers, motor bearings and the flight-controller "
                "mounting." + cause, when)
        elif worst >= 30.0:
            add("warning", f"Vibration peaked at {worst:.0f} m/s²",
                "Usable, but past the 30 PX4 treats as the point to act on. "
                "Worth watching rather than grounding the aircraft for." + cause,
                when)

    failsafe_s, failsafe_at = _failsafe(log)
    if failsafe_s >= 0.5:
        add("warning", f"In failsafe for {failsafe_s:.0f} s",
            "What the aircraft did about it is the mode strip above; why it "
            "happened is in the messages below.", failsafe_at)

    ekf = by_id.get("ekf")
    if ekf:
        worst_name, worst, when = _peak_of(ekf)
        if worst >= 1.0:
            # How LONG the estimator disbelieved a sensor is the finding; the
            # peak alone cannot tell a bump in the road from a dead compass.
            field = {"Magnetometer": "mag_test_ratio", "Velocity": "vel_test_ratio",
                     "Position": "pos_test_ratio", "Height": "hgt_test_ratio",
                     "Airspeed": "tas_test_ratio"}.get(worst_name)
            seconds = 0.0
            if field:
                times, values = _raw(log, "estimator_status", field)
                seconds, first, _ = _time_above(times, values, 1.0)
                when = first if first is not None else when
            if seconds >= 1.0:
                add("warning",
                    f"{worst_name} innovations reached {worst:.2f} and stayed "
                    f"above 1.0 for {seconds:.1f} s",
                    "Above 1.0 the estimator is rejecting that sensor "
                    "outright. Held that long, it was flying without it — "
                    "read the altitude and velocity comparisons in the "
                    "Estimator section to see what it used instead.", when)
            else:
                add("note",
                    f"{worst_name} innovations touched {worst:.2f} briefly",
                    "Above 1.0 the estimator momentarily rejected that sensor. "
                    "A brief excursion is normal — noted rather than flagged "
                    "because it is only a problem when it lasts.", when)

    resets = _estimator_resets(log, armed)
    if resets:
        # One line, not one per counter. The four counters move together on the
        # same event, and four near-identical paragraphs about a single
        # estimator hiccup was the clearest way this page became unreadable.
        total = sum(count for _, count, _ in resets)
        first = min(when for _, _, when in resets)
        parts = ", ".join(f"{label} {count}×" for label, count, _ in resets)
        add("warning",
            f"The estimator reset its state {total} time(s) in flight",
            f"Which: {parts}. A reset is a step in the estimate and therefore "
            "in what the controller was flying towards — the track and "
            "position plots jump there because the aircraft's idea of where it "
            "was jumped, not because it moved. GPS being re-accepted after a "
            "glitch is the usual cause.", first)

    gps_t, gps_v = _raw(log, _first_topic(log, ("vehicle_gps_position", "sensor_gps")) or "",
                        "fix_type")
    if gps_v and armed:
        in_air_t, in_air_v = _within(gps_t, gps_v, armed)
        lost, lost_at, _ = _time_above(in_air_t, in_air_v, 3.0, below=True)
        if lost >= 1.0:
            add("warning", f"GPS below a 3-D fix for {lost:.0f} s while armed",
                "Position and ground speed across that stretch are dead "
                "reckoning, however smooth the line on the plot looks.",
                lost_at)

    for axis in ("roll", "pitch"):
        measured = _attitude_error(log, axis, armed)
        if measured is None:
            continue
        typical, peak, when = measured
        if typical >= 20.0:
            add("critical",
                f"{axis.capitalize()} missed its setpoint by {typical:.0f}° or "
                f"more for most of the flight",
                f"Peak {peak:.0f}°. An aircraft that cannot hold the angle it "
                "is given is short of control authority, not short of tuning — "
                "read the motor outputs before touching a gain.", when)
        elif typical >= 10.0:
            add("warning",
                f"{axis.capitalize()} tracked its setpoint to about {typical:.0f}°",
                f"Peak {peak:.0f}°. Not alarming on its own; worth a look at "
                "the attitude plot before the next flight.", when)

    for axis in ("roll", "pitch", "yaw"):
        ringing = _rate_oscillation(log, axis, armed)
        if ringing is None:
            continue
        amplitude, frequency = ringing
        if amplitude >= 12.0 and frequency >= 6.0:
            add("warning",
                f"The {axis} rate rings at about {frequency:.0f} Hz "
                f"({amplitude:.0f} deg/s)",
                "Oscillation that fast is the rate loop arguing with itself "
                "rather than the aircraft moving. The D gain for that axis is "
                "the usual cause; vibration reaching the gyro is the other.")

    actuators = by_id.get("actuators")
    if actuators and actuators.get("unit") != "normalised":
        # A motor pinned near the top while its neighbours are not is an
        # airframe problem — a bent arm, a heavy corner, a mistrimmed prop.
        peaks = []
        for s in actuators["series"]:
            finite = [v for v in s["y"] if v is not None]
            if finite:
                peaks.append((s["name"], max(finite), sum(finite) / len(finite)))
        if len(peaks) >= 3:
            averages = [p[2] for p in peaks]
            spread = max(averages) - min(averages)
            if spread > 80.0:      # µs, on a ~1000 µs band
                worst = max(peaks, key=lambda p: p[2])[0]
                add("warning",
                    f"Motor outputs are uneven — {worst} ran {spread:.0f} µs "
                    "above the lowest on average",
                    "One corner working harder than the others across a whole "
                    "flight is an airframe imbalance, not a tuning one: a bent "
                    "arm, a heavy corner, a mistrimmed propeller.")

    _magnetic_finding(log, armed, add)

    rc = by_id.get("rc")
    if rc:
        lost = next((s for s in rc["series"] if "lost" in s["name"].lower()), None)
        if lost:
            when = next((lost["x"][i] for i, v in enumerate(lost["y"])
                         if v is not None and v >= 1), None)
            if when is not None:
                add("warning", "The RC link reported a signal loss in flight",
                    "It recovered — the flight continued — but the next one "
                    "may reach the failsafe instead.", when)

    _battery_findings(log, by_id, add)

    cpu_t, cpu_v = _raw(log, "cpuload", "load", scale=100.0)
    if cpu_v:
        heavy, heavy_at, worst = _time_above(cpu_t, cpu_v, 90.0)
        if heavy >= 1.0:
            add("warning",
                f"CPU held above 90% for {heavy:.0f} s (peak {worst:.0f}%)",
                "At that load PX4 starts missing control-loop deadlines. Turn "
                "down a logging or telemetry stream rate before the next "
                "flight.", heavy_at)

    if log.dropouts:
        total_ms = sum(d.get("duration_ms", 0) for d in log.dropouts)
        # A few milliseconds lost to a slow SD card is housekeeping; half a
        # second of it is a hole in the evidence.
        add("warning" if total_ms >= 500 else "note",
            f"{len(log.dropouts)} logging dropout(s), {total_ms} ms total",
            "The logger could not keep up with the card. Those gaps are "
            "missing data rather than quiet flight, so a flat stretch there "
            "says nothing about the aircraft.")

    if log.truncated:
        add("warning", "The log ends mid-record",
            "The aircraft lost power before the file was closed — a crash, a "
            "battery pulled, or a brownout. Everything up to the cut is shown.")

    errors = [m for m in log.messages if m["level"] in ("emergency", "alert", "critical", "error")]
    if errors:
        add("warning",
            f"{len(errors)} error-level message(s) in the flight log",
            "The aircraft's own commentary, in full at the bottom of this "
            "page. It is usually the shortest route to why a flight went the "
            "way it did.")

    recovery = next((m for m in modes if m["mode"] in _RECOVERY_MODES), None)
    if recovery and failsafe_s < 0.5:
        # A note, not a warning: a pilot pressing Return is an ordinary end to
        # an ordinary flight, and only the messages can say which this was.
        add("note", f"The aircraft flew {recovery['mode']}",
            "PX4 also selects that for itself on a failsafe. Nothing else in "
            "this log says a failsafe fired, so if the pilot did not ask for "
            "it, the reason is in the messages below.", recovery["start"])

    if not any(f["level"] in ("critical", "warning") for f in out):
        out.insert(0, _finding("ok",
                               "Nothing flagged in this log",
                               None, modes,
                               "No clipping, no sustained EKF rejection, no "
                               "motor imbalance, no RC loss and no in-flight "
                               "estimator reset. Anything listed below this is "
                               "context rather than a problem."))
    return out


def _magnetic_finding(log: ULog, armed: list[dict[str, float]],
                      add: Callable[..., None]) -> None:
    """Compass interference, but only where the numbers are believable.

    The swing is measured between the 5th and 95th percentile rather than
    between the extremes. One dropped sample near zero turns a min/max spread
    into a claim that the field swung by twice its own value, which is not
    physically possible for a magnetometer on an aircraft — and a finding that
    prints an impossible number is a finding the reader stops believing, along
    with every other finding on the page.
    """
    times, values = _mag_norm(log)
    finite = sorted(v for v in values if v > 0)
    if len(finite) < 50:
        return
    middle = _percentile(finite, 0.5)
    if middle <= 0:
        return
    swing = (_percentile(finite, 0.95) - _percentile(finite, 0.05)) / middle
    # Under 5% there is nothing to explain. Over 60% this is not a compass in
    # flight — a sensor swap, a unit mix-up, a dropout — and blaming the
    # aircraft's wiring for it would be wrong.
    if not 0.05 <= swing <= 0.6:
        return
    follows = _tracks_thrust(log, times, values, armed)
    if follows is None or follows < 0.5:
        return
    add("warning",
        f"Compass field strength moves with the throttle (correlation "
        f"{follows:.2f})",
        f"It swings {swing * 100:.0f}% of its own value across the flight. "
        "That is current from the power wiring reaching the magnetometer: it "
        "biases heading exactly when the aircraft is working hardest, and it "
        "is why a compass calibration does not hold. The fix is routing and "
        "distance, not another calibration.")


def _battery_findings(log: ULog, by_id: dict[str, dict],
                      add: Callable[..., None]) -> None:
    """What the pack did, in the two ways it can go wrong.

    Sag is about the pack's health; the per-cell floor is about whether this
    flight damaged it. They are separate findings because they have separate
    answers — one retires a battery, the other shortens the next flight.
    """
    battery = by_id.get("battery")
    if not battery:
        return
    voltage = next((s for s in battery["series"] if s["name"] == "Voltage"), None)
    if not voltage:
        return
    finite = [(v, voltage["x"][i]) for i, v in enumerate(voltage["y"])
              if v is not None and v > 1.0]
    if not finite:
        return
    top = max(finite)[0]
    floor, floor_at = min(finite)
    sag = top - floor
    if sag > 3.0:
        add("warning", f"Pack voltage sagged {sag:.1f} V under load",
            "The gap between resting and loaded voltage is internal "
            "resistance, and a pack that dives this far is near the end of "
            "its useful life.", floor_at)

    # Cell count from the logged value where PX4 has one, and otherwise from
    # the HIGHEST voltage seen: a lithium cell cannot exceed about 4.25 V, so
    # the peak over that is a firm lower bound. Dividing the sagged voltage by
    # a nominal 3.8 instead reads a sagging 4S as a healthy 3S — backwards,
    # and it hides the one battery finding that matters.
    logged = [int(v) for v in log.series("battery_status", "cell_count", 0) if v]
    cells = logged[0] if logged else max(1, math.ceil(top / 4.25))
    per_cell = floor / cells
    if per_cell < 3.3:
        add("critical",
            f"The pack reached {floor:.1f} V — {per_cell:.2f} V per cell "
            f"across {cells}",
            "That is into the range that permanently costs a lithium pack "
            "capacity. If it only touched it under load and recovered after "
            "landing, the damage is smaller — but the next flight should be "
            "shorter either way.", floor_at)
    elif per_cell < 3.5:
        add("warning",
            f"The pack reached {floor:.1f} V ({per_cell:.2f} V per cell)",
            "Read it against the current plot: under load a pack recovers, "
            "and the number that decides the next flight is where it settles "
            "after landing.", floor_at)


# ---------------------------------------------------------------------------

# Order is the reading order of the page, grouped by section.
_PLOTTERS = (
    # Flight — where it went
    _plot_track, _plot_altitude, _plot_pos_x, _plot_pos_y, _plot_pos_z,
    _plot_velocity, _plot_vel_x, _plot_vel_y, _plot_vel_z,
    _plot_airspeed, _plot_wind,
    # Control — what was asked of it, per axis
    _plot_manual_control, _plot_att_roll, _plot_rate_roll,
    _plot_att_pitch, _plot_rate_pitch, _plot_att_yaw, _plot_rate_yaw,
    _plot_rate_error, _plot_thrust,
    # Airframe
    _plot_actuators, _plot_esc,
    # Estimator
    _plot_ekf, _plot_altitude_sources, _plot_gps_velocity, _plot_bias,
    # Sensors
    _plot_clipping, _plot_vibration, _plot_mag, _plot_imu_temp, _plot_baro,
    _plot_gps, _plot_gps_accuracy, _plot_gps_noise, _plot_gps_quality,
    _plot_distance,
    # System
    _plot_battery, _plot_power, _plot_cpu, _plot_rc,
)
# The order sections appear in; anything else falls to the end.
GROUP_ORDER = ("Flight", "Control", "Airframe", "Estimator", "Sensors", "System")

# Only these topics are decoded. A log holds a hundred; a review reads a dozen.
REVIEW_TOPICS = (
    "actuator_motors", "actuator_outputs", "actuator_controls_0",
    "esc_status",
    "vehicle_imu_status", "sensor_accel_status", "sensor_combined",
    "estimator_status", "estimator_sensor_bias",
    "vehicle_attitude", "control_state", "vehicle_attitude_setpoint",
    "vehicle_angular_velocity", "vehicle_rates_setpoint",
    "vehicle_thrust_setpoint", "manual_control_setpoint",
    "manual_control_input",
    "vehicle_local_position", "vehicle_local_position_setpoint",
    "vehicle_global_position", "position_setpoint_triplet",
    "vehicle_magnetometer", "sensor_mag",
    "battery_status", "cpuload", "vehicle_gps_position", "sensor_gps",
    "vehicle_status", "commander_state",
    "input_rc", "rc_channels", "airspeed", "distance_sensor",
    "wind", "wind_estimate", "vehicle_air_data", "sensor_baro",
)


def _timed_messages(log: ULog) -> list[dict[str, Any]]:
    """The flight's own commentary, placed on the same clock as the plots.

    The parser hands these back with the raw log timestamp PX4 wrote, which is
    microseconds on an arbitrary epoch. Left that way, a message can be read
    but never *located*: the one line that explains the flight sits next to a
    number that has nothing to do with the second of the flight it happened in.
    """
    base = _log_start(log)
    out = []
    for message in log.messages[-300:]:
        try:
            when = max(0.0, (float(message["t"]) - base) / 1e6)
        except (TypeError, ValueError, KeyError):
            when = 0.0
        out.append({"t": round(when, 2), "level": message["level"],
                    "text": message["text"]})
    return out


def review(log: ULog, name: str = "") -> dict[str, Any]:
    """Reduce a parsed ULog to the Flight Review payload the UI renders."""
    modes = _flight_modes(log)
    armed = _armed_spans(log)
    plots = [plot for plot in (make(log) for make in _PLOTTERS) if plot]

    # The modes count towards the duration in their own right: they are read
    # from topics no plot draws, so a flight whose last mode outlasts every
    # plotted signal would otherwise be reported as shorter than it was.
    duration = max((span["end"] for span in modes), default=0.0)
    for plot in plots:
        for s in plot["series"]:
            if s["x"]:
                duration = max(duration, s["x"][-1])

    summary = {
        "name": name,
        "duration_s": round(duration, 1),
        "start_utc": log.start_timestamp,
        "sw": str(log.info.get("ver_sw", "") or ""),
        "hw": str(log.info.get("ver_hw", "") or ""),
        "sys_name": str(log.info.get("sys_name", "") or ""),
        "airframe": log.params.get("SYS_AUTOSTART", ""),
        "dropouts": len(log.dropouts),
        "dropout_ms": sum(d.get("duration_ms", 0) for d in log.dropouts),
        "truncated": log.truncated,
        "topics": sorted(t for t in log.data if log.has(t)),
        "armed_s": round(sum(s["end"] - s["start"] for s in armed), 1),
        "modes_flown": sorted({m["mode"] for m in modes}),
    }
    return {
        "summary": summary,
        "groups": [g for g in GROUP_ORDER if any(p["group"] == g for p in plots)],
        # Drawn as bands behind every time plot: the same oscillation means
        # different things in Position and in Manual.
        "modes": modes,
        "armed": armed,
        "findings": _findings(log, plots, modes, armed),
        "plots": plots,
        # The tail of the flight's own commentary. Capped: an aircraft that
        # spent a flight complaining can produce thousands.
        "messages": _timed_messages(log),
    }


# --------------------------------------------------------------------------
# Reviewing a file
# --------------------------------------------------------------------------

# Reviews already produced, newest last. Reading a log is the expensive half of
# this module — a 40 MB flight is most of a second — and the way the page is
# used is to open one log, go back, and open it (or the one next to it) again.
# Two entries cover that without holding a folder's worth of flights in memory.
_CACHE_ENTRIES = 2
_cache: OrderedDict[tuple, dict[str, Any]] = OrderedDict()
_cache_lock = threading.Lock()


def review_bytes(blob: bytes, name: str) -> dict[str, Any]:
    """Review a ULog handed over as its own bytes.

    For a file the operator picked from outside the download folder: it arrives
    as an upload rather than as a path, so there is no stat to key a cache on.
    A digest of the content is the honest substitute — the same file picked
    twice is the same review, and an edited one is not.
    """
    key = ("sha256", hashlib.sha256(blob).hexdigest(), len(blob))
    with _cache_lock:
        hit = _cache.get(key)
        if hit is not None:
            _cache.move_to_end(key)
            return hit
    data = review(read(blob, topics=REVIEW_TOPICS), name)
    _remember(key, data)
    return data


def _remember(key: tuple, data: dict[str, Any]) -> None:
    with _cache_lock:
        _cache[key] = data
        _cache.move_to_end(key)
        while len(_cache) > _CACHE_ENTRIES:
            _cache.popitem(last=False)


def review_file(path: str, name: str = "") -> dict[str, Any]:
    """Review the ULog at *path*, reusing a recent result for the same file.

    The cache key carries the file's size and modification time, so a log that
    was re-downloaded over its own name is re-read rather than answered from a
    stale review — the one mistake a cache here could make that matters.
    """
    label = name or os.path.basename(path)
    try:
        stat = os.stat(path)
        key = (os.path.realpath(path), stat.st_size, stat.st_mtime_ns)
    except OSError:
        key = None
    if key is not None:
        with _cache_lock:
            hit = _cache.get(key)
            if hit is not None:
                _cache.move_to_end(key)
                return hit
    with open(path, "rb") as handle:
        parsed = read(handle, topics=REVIEW_TOPICS)
    data = review(parsed, label)
    if key is not None:
        _remember(key, data)
    return data


def clear_cache() -> None:
    """Drop every cached review. For tests, and for a shutdown that wants the
    memory back."""
    with _cache_lock:
        _cache.clear()
