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
from typing import Any, Callable, Iterable

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
# Findings
# ---------------------------------------------------------------------------

def _findings(log: ULog, plots: list[dict]) -> list[dict[str, str]]:
    """The few sentences worth reading before the plots.

    Only things that are actually derivable from this log — no scoring, no
    grades. A review that invents a verdict is worse than one that points at
    the plot and lets the operator look.
    """
    out: list[dict[str, str]] = []
    by_id = {p["id"]: p for p in plots}

    clipping = by_id.get("clipping")
    if clipping:
        total = 0.0
        for s in clipping["series"]:
            finite = [v for v in s["y"] if v is not None]
            if finite:
                total = max(total, max(finite))
        if total > 0:
            out.append({"level": "critical", "text":
                        f"Accelerometer clipping occurred ({int(total)} samples). "
                        "The IMU saturated — treat the attitude and position "
                        "estimates in this log with suspicion."})

    ekf = by_id.get("ekf")
    if ekf:
        worst_name, worst = "", 0.0
        for s in ekf["series"]:
            finite = [v for v in s["y"] if v is not None]
            if finite and max(finite) > worst:
                worst, worst_name = max(finite), s["name"]
        if worst >= 1.0:
            out.append({"level": "warning", "text":
                        f"{worst_name} innovations reached {worst:.2f} — above "
                        "1.0 the estimator was rejecting that sensor."})

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
                out.append({"level": "warning", "text":
                            f"Motor outputs are uneven — {worst} ran "
                            f"{spread:.0f} µs above the lowest one on average. "
                            "That is an airframe imbalance, not a tuning one."})

    vibration = by_id.get("vibration")
    if vibration:
        worst = 0.0
        for s in vibration["series"]:
            finite = [v for v in s["y"] if v is not None]
            if finite:
                worst = max(worst, max(finite))
        if worst >= 60.0:
            out.append({"level": "critical", "text":
                        f"Vibration peaked at {worst:.0f} m/s². Above about 60 "
                        "the estimator starts to suffer — check props, motors "
                        "and the flight-controller mounting."})
        elif worst >= 30.0:
            out.append({"level": "warning", "text":
                        f"Vibration peaked at {worst:.0f} m/s² — usable, but "
                        "worth watching."})

    rc = by_id.get("rc")
    if rc:
        lost = next((s for s in rc["series"] if "lost" in s["name"].lower()), None)
        if lost:
            finite = [v for v in lost["y"] if v is not None]
            if finite and max(finite) >= 1:
                out.append({"level": "warning", "text":
                            "The RC link reported a signal loss during this "
                            "flight — the next one may hit the failsafe."})

    battery = by_id.get("battery")
    if battery:
        voltage = next((s for s in battery["series"] if s["name"] == "Voltage"), None)
        if voltage:
            finite = [v for v in voltage["y"] if v is not None and v > 1.0]
            if finite:
                sag = max(finite) - min(finite)
                if sag > 3.0:
                    out.append({"level": "warning", "text":
                                f"Pack voltage sagged {sag:.1f} V between rest "
                                "and load. A pack that dives like that is near "
                                "the end of its useful life."})

    if log.dropouts:
        total_ms = sum(d.get("duration_ms", 0) for d in log.dropouts)
        out.append({"level": "warning", "text":
                    f"{len(log.dropouts)} logging dropout(s), {total_ms} ms total. "
                    "The gaps are missing data, not quiet flight."})

    if log.truncated:
        out.append({"level": "warning", "text":
                    "The log ends mid-record — the aircraft lost power before "
                    "the file was closed. Everything up to the cut is shown."})

    errors = [m for m in log.messages if m["level"] in ("emergency", "alert", "critical", "error")]
    if errors:
        out.append({"level": "warning", "text":
                    f"{len(errors)} error-level message(s) in the flight log — "
                    "see the messages below."})

    if not out:
        out.append({"level": "ok", "text":
                    "Nothing flagged: no clipping, no EKF rejection, no "
                    "logging dropouts, no RC loss and no motor imbalance in "
                    "this log."})
    return out


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
    _plot_thrust,
    # Airframe
    _plot_actuators,
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
        "findings": _findings(log, plots),
        "plots": plots,
        # The tail of the flight's own commentary. Capped: an aircraft that
        # spent a flight complaining can produce thousands.
        "messages": log.messages[-300:],
    }


# --------------------------------------------------------------------------
# Reviewing a file
# --------------------------------------------------------------------------

# Reviews already produced, newest last. Reading a log is the expensive half of
# this module — a 40 MB flight is most of a second — and the way the page is
# used is to open one log, go back, and open it (or the one next to it) again.
# Two entries cover that without holding a folder's worth of flights in memory.
_CACHE_ENTRIES = 2
_cache: "OrderedDict[tuple, dict[str, Any]]" = OrderedDict()
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
