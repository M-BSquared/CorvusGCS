"""A PX4 ULog of a scene's sortie, so Flight Review has a real file to read.

The log protocol the simulated aircraft answers was filler for a long time:
the download ran, the bytes were a counter, and a Flight Review screenshot
needed somebody's real flight dropped in by hand. That real flight then showed
a different field, a different aircraft and a different day from every other
picture in the README.

This module writes the log the scene's own aircraft would have written. It is
the same sortie the map pictures show, flown start to finish: the aircraft
arms at the start point, climbs, flies the scene's path, holds at its end,
returns and lands. The file is a ULog by the specification (header, flag bits,
formats, info, parameters, subscriptions, data and logged messages), so
``corvus/ulog.py`` reads it with the code a real log goes through, and so does
pyulog.

What it is not: a flight dynamics model. Attitude is derived from the
acceleration the trajectory needs, the estimate trails the setpoint by a fixed
lag plus a little gusting, and every sensor is that state plus noise. The
plots read like a calm day with a well tuned quadcopter, because that is what
the model is. The README says the aircraft in its pictures is simulated, and
this log is part of that simulation.
"""
from __future__ import annotations

import math
import random
import struct
from dataclasses import dataclass
from typing import Any
from collections.abc import Iterable

from . import flight as flight_mod

MAGIC = b"ULog\x01\x12\x35"
ULOG_VERSION = 1
G = flight_mod.G
HOME_AMSL = 570.0      # the vehicle module reports Neubiberg at about 570 m

# PX4 vehicle_status.nav_state and arming_state values.
NAV_POSCTL, NAV_MISSION, NAV_LOITER, NAV_RTL, NAV_TAKEOFF = 2, 3, 4, 5, 17
ARMING_STANDBY, ARMING_ARMED = 1, 2

_PRIMITIVES = {
    "int8_t": "b", "uint8_t": "B", "int16_t": "h", "uint16_t": "H",
    "int32_t": "i", "uint32_t": "I", "int64_t": "q", "uint64_t": "Q",
    "float": "f", "double": "d", "bool": "?", "char": "c",
}

# The topics, in PX4's own names and with the fields Flight Review reads. Not
# every field PX4 logs: a review reads these, and a field nobody reads is bytes
# a screenshot does not need.
_FORMATS: dict[str, list[tuple[str, str]]] = {
    "position_setpoint": [
        ("uint64_t", "timestamp"), ("bool", "valid"), ("uint8_t", "type"),
        ("double", "lat"), ("double", "lon"), ("float", "alt"),
    ],
    "position_setpoint_triplet": [
        ("uint64_t", "timestamp"), ("position_setpoint", "previous"),
        ("position_setpoint", "current"), ("position_setpoint", "next"),
    ],
    "vehicle_status": [
        ("uint64_t", "timestamp"), ("uint8_t", "nav_state"),
        ("uint8_t", "arming_state"), ("bool", "failsafe"),
        ("uint16_t", "failure_detector_status"),
    ],
    "vehicle_attitude": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"), ("float[4]", "q"),
    ],
    "vehicle_attitude_setpoint": [
        ("uint64_t", "timestamp"), ("float", "roll_body"), ("float", "pitch_body"),
        ("float", "yaw_body"), ("float[4]", "q_d"), ("float[3]", "thrust_body"),
    ],
    "vehicle_angular_velocity": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"), ("float[3]", "xyz"),
    ],
    "vehicle_rates_setpoint": [
        ("uint64_t", "timestamp"), ("float", "roll"), ("float", "pitch"),
        ("float", "yaw"), ("float[3]", "thrust_body"),
    ],
    "vehicle_thrust_setpoint": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"), ("float[3]", "xyz"),
    ],
    "actuator_motors": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"),
        ("uint16_t", "reversible_flags"), ("float[12]", "control"),
    ],
    "vehicle_local_position": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"),
        ("bool", "xy_valid"), ("bool", "z_valid"), ("bool", "v_xy_valid"),
        ("bool", "v_z_valid"), ("float", "x"), ("float", "y"), ("float", "z"),
        ("float", "vx"), ("float", "vy"), ("float", "vz"), ("float", "ax"),
        ("float", "ay"), ("float", "az"), ("float", "heading"),
        ("uint8_t", "xy_reset_counter"), ("uint8_t", "z_reset_counter"),
        ("uint8_t", "vxy_reset_counter"), ("uint8_t", "vz_reset_counter"),
        ("uint8_t", "heading_reset_counter"), ("bool", "xy_global"),
        ("bool", "z_global"), ("uint64_t", "ref_timestamp"), ("double", "ref_lat"),
        ("double", "ref_lon"), ("float", "ref_alt"), ("float", "dist_bottom"),
        ("float", "eph"), ("float", "epv"),
    ],
    "vehicle_local_position_setpoint": [
        ("uint64_t", "timestamp"), ("float", "x"), ("float", "y"), ("float", "z"),
        ("float", "vx"), ("float", "vy"), ("float", "vz"), ("float", "yaw"),
    ],
    "vehicle_global_position": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"), ("double", "lat"),
        ("double", "lon"), ("float", "alt"), ("float", "eph"), ("float", "epv"),
    ],
    "vehicle_gps_position": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"),
        ("uint32_t", "device_id"), ("int32_t", "lat"), ("int32_t", "lon"),
        ("int32_t", "alt"), ("float", "s_variance_m_s"), ("float", "eph"),
        ("float", "epv"), ("int32_t", "noise_per_ms"),
        ("int32_t", "jamming_indicator"), ("float", "vel_m_s"),
        ("float", "vel_n_m_s"), ("float", "vel_e_m_s"), ("float", "vel_d_m_s"),
        ("float", "cog_rad"), ("uint8_t", "fix_type"), ("bool", "vel_ned_valid"),
        ("uint8_t", "satellites_used"),
    ],
    "vehicle_air_data": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"),
        ("uint32_t", "baro_device_id"), ("float", "baro_alt_meter"),
        ("float", "baro_temp_celcius"), ("float", "baro_pressure_pa"),
        ("float", "rho"),
    ],
    "vehicle_magnetometer": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_sample"),
        ("uint32_t", "device_id"), ("float[3]", "magnetometer_ga"),
    ],
    "vehicle_imu_status": [
        ("uint64_t", "timestamp"), ("uint32_t", "accel_device_id"),
        ("uint32_t", "gyro_device_id"), ("uint32_t[3]", "accel_clipping"),
        ("float", "accel_vibration_metric"), ("float", "gyro_vibration_metric"),
        ("float", "temperature_accel"), ("float", "temperature_gyro"),
    ],
    "estimator_status": [
        ("uint64_t", "timestamp"), ("float", "vel_test_ratio"),
        ("float", "pos_test_ratio"), ("float", "hgt_test_ratio"),
        ("float", "mag_test_ratio"), ("float", "tas_test_ratio"),
    ],
    "estimator_sensor_bias": [
        ("uint64_t", "timestamp"), ("float[3]", "gyro_bias"), ("float[3]", "accel_bias"),
    ],
    "battery_status": [
        ("uint64_t", "timestamp"), ("float", "voltage_v"), ("float", "voltage_filtered_v"),
        ("float", "current_a"), ("float", "current_filtered_a"),
        ("float", "discharged_mah"), ("float", "remaining"), ("float", "temperature"),
        ("uint8_t", "cell_count"), ("bool", "connected"),
    ],
    "cpuload": [
        ("uint64_t", "timestamp"), ("float", "load"), ("float", "ram_usage"),
    ],
    "input_rc": [
        ("uint64_t", "timestamp"), ("uint64_t", "timestamp_last_signal"),
        ("uint32_t", "channel_count"), ("int32_t", "rssi"), ("bool", "rc_lost"),
        ("uint16_t[18]", "values"),
    ],
    "manual_control_setpoint": [
        ("uint64_t", "timestamp"), ("float", "roll"), ("float", "pitch"),
        ("float", "yaw"), ("float", "throttle"), ("bool", "valid"),
    ],
    "wind": [
        ("uint64_t", "timestamp"), ("float", "windspeed_north"),
        ("float", "windspeed_east"),
    ],
}


class ULogWriter:
    """The ULog container: definitions first, then time-ordered data."""

    def __init__(self, start_us: int) -> None:
        self._out = bytearray(MAGIC + bytes([ULOG_VERSION]) + struct.pack("<Q", start_us))
        # Flag bits must be the first record: no compatibility flags, no
        # incompatible ones, nothing appended.
        self._record("B", bytes(16) + bytes(24))
        self._layouts: dict[str, list[tuple[str, int, str]]] = {}
        self._ids: dict[tuple[str, int], int] = {}
        self._pending: list[tuple[int, int, str, bytes]] = []

    def _record(self, kind: str, body: bytes) -> None:
        self._out += struct.pack("<HB", len(body), ord(kind)) + body

    # -- definitions -------------------------------------------------------

    def info(self, key: str, value: str | int) -> None:
        if isinstance(value, str):
            raw = value.encode("utf-8")
            type_ = f"char[{len(raw)}]"
        else:
            raw = struct.pack("<I", int(value) & 0xFFFFFFFF)
            type_ = "uint32_t"
        name = f"{type_} {key}".encode("ascii")
        self._record("I", bytes([len(name)]) + name + raw)

    def parameter(self, name: str, value: Any) -> None:
        if isinstance(value, bool):
            value = int(value)
        if isinstance(value, int):
            type_, raw = "int32_t", struct.pack("<i", value)
        else:
            type_, raw = "float", struct.pack("<f", float(value))
        key = f"{type_} {name}".encode("ascii")
        self._record("P", bytes([len(key)]) + key + raw)

    def define(self, name: str, fields: list[tuple[str, str]]) -> None:
        parsed = []
        for type_, field in fields:
            count = 1
            if "[" in type_:
                type_, _, tail = type_.partition("[")
                count = int(tail.rstrip("]"))
            parsed.append((type_, count, field))
        self._layouts[name] = parsed
        text = name + ":" + "".join(
            f"{t}[{c}] {f};" if c > 1 else f"{t} {f};" for t, c, f in parsed)
        self._record("F", text.encode("ascii"))

    def subscribe(self, topic: str, multi_id: int = 0) -> None:
        msg_id = len(self._ids)
        self._ids[(topic, multi_id)] = msg_id
        self._record("A", bytes([multi_id]) + struct.pack("<H", msg_id)
                     + topic.encode("ascii"))

    # -- data --------------------------------------------------------------

    def _pack(self, name: str, values: dict[str, Any]) -> bytes:
        out = bytearray()
        for type_, count, field in self._layouts[name]:
            value = values.get(field)
            code = _PRIMITIVES.get(type_)
            if code is None:
                nested = value if isinstance(value, dict) else {}
                out += self._pack(type_, nested)
                continue
            if count == 1:
                out += struct.pack("<" + code, _default(code, value))
            else:
                items = list(value) if value is not None else []
                items = (items + [0] * count)[:count]
                out += struct.pack(f"<{count}{code}", *(_default(code, v) for v in items))
        return bytes(out)

    def data(self, topic: str, values: dict[str, Any], multi_id: int = 0) -> None:
        msg_id = self._ids[(topic, multi_id)]
        body = struct.pack("<H", msg_id) + self._pack(topic, values)
        self._pending.append((int(values["timestamp"]), len(self._pending), "D", body))

    def log(self, timestamp: int, level: int, text: str) -> None:
        # PX4 writes the syslog level as an ASCII digit, '0' to '7'.
        body = bytes([ord("0") + level]) + struct.pack("<Q", timestamp) + text.encode("utf-8")
        self._pending.append((timestamp, len(self._pending), "L", body))

    def finish(self) -> bytes:
        for _stamp, _seq, kind, body in sorted(self._pending):
            self._record(kind, body)
        self._pending.clear()
        return bytes(self._out)


def _default(code: str, value: Any) -> Any:
    if value is None:
        return False if code == "?" else 0
    if code in "fd":
        return float(value)
    if code == "?":
        return bool(value)
    return int(value)


# ---------------------------------------------------------------------------
# The sortie
# ---------------------------------------------------------------------------


@dataclass
class _Leg:
    """A straight or polyline stretch flown with a smooth speed profile."""

    points: list[tuple[float, float]]      # (north, east) metres from home
    speed: float
    ramp: float = 5.0                      # seconds to reach cruise speed

    def __post_init__(self) -> None:
        self._lengths = [math.hypot(b[0] - a[0], b[1] - a[1])
                         for a, b in zip(self.points, self.points[1:])]
        self.length = sum(self._lengths)
        if self.length < self.speed * self.ramp:
            self.speed = max(0.5, self.length / self.ramp)
        self.duration = self.length / self.speed + self.ramp

    def distance(self, t: float) -> float:
        t = max(0.0, min(self.duration, t))
        v, ramp = self.speed, self.ramp

        def ease(x: float) -> float:          # integral of smoothstep, 0..0.5
            return x ** 3 - x ** 4 / 2

        if t < ramp:
            return v * ramp * ease(t / ramp)
        if t > self.duration - ramp:
            return self.length - v * ramp * ease((self.duration - t) / ramp)
        return v * ramp / 2 + v * (t - ramp)

    def at(self, t: float) -> tuple[float, float]:
        remaining = self.distance(t)
        for (a, b), length in zip(zip(self.points, self.points[1:]), self._lengths):
            if remaining <= length or length <= 0:
                f = remaining / length if length > 0 else 0.0
                return a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f
            remaining -= length
        return self.points[-1]


def _smooth(series: list[float], rate: float, window_s: float = 0.6) -> list[float]:
    """A centred moving average over *window_s* seconds."""
    half = max(1, int(window_s * rate / 2))
    count = len(series)
    prefix = [0.0]
    for value in series:
        prefix.append(prefix[-1] + value)
    out = []
    for i in range(count):
        lo, hi = max(0, i - half), min(count, i + half + 1)
        out.append((prefix[hi] - prefix[lo]) / (hi - lo))
    return out


def _smootherstep(x: float) -> float:
    x = max(0.0, min(1.0, x))
    return x * x * x * (x * (6 * x - 15) + 10)


def _wrap(angle: float) -> float:
    return (angle + math.pi) % (2 * math.pi) - math.pi


def _quat(roll: float, pitch: float, yaw: float) -> list[float]:
    cr, sr = math.cos(roll / 2), math.sin(roll / 2)
    cp, sp = math.cos(pitch / 2), math.sin(pitch / 2)
    cy, sy = math.cos(yaw / 2), math.sin(yaw / 2)
    return [cr * cp * cy + sr * sp * sy, sr * cp * cy - cr * sp * sy,
            cr * sp * cy + sr * cp * sy, cr * cp * sy - sr * sp * cy]


def _tilt(acc: tuple[float, float, float], vel: tuple[float, float, float],
          yaw: float) -> tuple[float, float, float]:
    """(roll, pitch, thrust in g) a multirotor needs for this acceleration.

    Drag is folded in as a tilt that grows with airspeed, which is what makes
    a multirotor fly nose down at a steady cruise and not only while speeding
    up.
    """
    drag = 0.06
    fn = acc[0] + drag * vel[0]
    fe = acc[1] + drag * vel[1]
    fd = acc[2] - G
    fx = math.cos(yaw) * fn + math.sin(yaw) * fe
    fy = -math.sin(yaw) * fn + math.cos(yaw) * fe
    norm = math.sqrt(fx * fx + fy * fy + fd * fd) or G
    roll = math.asin(max(-1.0, min(1.0, fy / norm)))
    pitch = math.atan2(-fx, -fd)
    return roll, pitch, norm / G


@dataclass
class Sortie:
    """The whole flight a log records, sampled at a fixed rate."""

    home: tuple[float, float]
    path: flight_mod.Path
    alt: float
    rate: float = 50.0
    seed: int = 20260924

    def samples(self) -> list[dict[str, Any]]:
        mn, me = flight_mod._metres_per_degree(self.home[0])
        pts = [((p.lat - self.home[0]) * mn, (p.lon - self.home[1]) * me)
               for p in self.path.points]
        if math.hypot(*pts[0]) > 1.0:
            pts.insert(0, (0.0, 0.0))
        out_leg = _Leg(pts, speed=max(3.0, self.path.speed))
        back_leg = _Leg([pts[-1], (0.0, 0.0)], speed=8.0)

        # Phase boundaries, seconds from the start of the log.
        t_arm = 4.0
        t_climb = t_arm + 2.0
        climb = max(8.0, self.alt / 3.0)
        t_out = t_climb + climb
        t_hold = t_out + out_leg.duration
        t_back = t_hold + 8.0
        t_land = t_back + back_leg.duration
        descend = max(8.0, self.alt / 2.0)
        t_down = t_land + descend
        t_disarm = t_down + 2.0
        end = t_disarm + 3.0

        # Mission items as the triplet reports them: a few points along the
        # outbound path, then its end, then home for the return.
        stride = max(1, (len(pts) - 1) // 4)
        items = [pts[i] for i in range(stride, len(pts) - 1, stride)] + [pts[-1]]
        item_at = [out_leg.length * (i + 1) / len(items) for i in range(len(items))]

        def setpoint(t: float) -> tuple[float, float, float, int, bool, tuple[float, float]]:
            if t < t_climb:
                nav = NAV_POSCTL if t < t_arm else NAV_TAKEOFF
                return 0.0, 0.0, 0.0, nav, t >= t_arm, pts[0]
            if t < t_out:
                h = self.alt * _smootherstep((t - t_climb) / climb)
                return 0.0, 0.0, -h, NAV_TAKEOFF, True, items[0]
            if t < t_hold:
                n, e = out_leg.at(t - t_out)
                flown = out_leg.distance(t - t_out)
                target = next((items[i] for i, d in enumerate(item_at) if flown < d - 0.5),
                              items[-1])
                return n, e, -self.alt, NAV_MISSION, True, target
            if t < t_back:
                return pts[-1][0], pts[-1][1], -self.alt, NAV_LOITER, True, pts[-1]
            if t < t_land:
                n, e = back_leg.at(t - t_back)
                return n, e, -self.alt, NAV_RTL, True, (0.0, 0.0)
            if t < t_down:
                h = self.alt * (1 - _smootherstep((t - t_land) / descend))
                return 0.0, 0.0, -h, NAV_RTL, True, (0.0, 0.0)
            return 0.0, 0.0, 0.0, NAV_RTL, t < t_disarm, (0.0, 0.0)

        dt = 1.0 / self.rate
        count = int(end * self.rate)
        rng = random.Random(self.seed)
        lag = 0.25
        # Gusting: a few slow sinusoids per axis, so the estimate wanders off
        # the setpoint the way a real one does on a light breeze.
        # Each component is sized by the acceleration it causes, not by its
        # displacement: that is what the attitude plots draw, and a few slow
        # metre-sized swings read there as a sine wave nobody would fly.
        waves = []
        for _axis in range(3):
            parts = []
            for _ in range(6):
                freq = rng.uniform(0.08, 0.6)
                accel = rng.uniform(0.03, 0.08)
                parts.append((accel / (2 * math.pi * freq) ** 2, freq, rng.uniform(0, 6.3)))
            waves.append(parts)

        def wobble(t: float, axis: int, order: int) -> float:
            total = 0.0
            for amp, freq, phase in waves[axis]:
                w = 2 * math.pi * freq
                scale = amp * (0.35 if axis == 2 else 1.0)
                if order == 0:
                    total += scale * math.sin(w * t + phase)
                elif order == 1:
                    total += scale * w * math.cos(w * t + phase)
                else:
                    total -= scale * w * w * math.sin(w * t + phase)
            return total

        raw = [setpoint(i * dt) for i in range(count + 2)]
        rows: list[dict[str, Any]] = []
        yaw_sp = math.atan2(pts[1][1] - pts[0][1], pts[1][0] - pts[0][0])
        prev_euler: tuple[float, float, float] | None = None
        prev_sp_euler: tuple[float, float, float] | None = None
        filt_sp = (0.0, 0.0)
        filt_act = (0.0, 0.0)

        def diff(series: list[float], i: int) -> float:
            lo, hi = max(0, i - 1), min(len(series) - 1, i + 1)
            return (series[hi] - series[lo]) / ((hi - lo) * dt) if hi > lo else 0.0

        # The setpoint as a controller would shape it: the path's corners
        # rounded off before it is differentiated. A polyline differentiated
        # twice is an acceleration spike at every vertex, and every plot of
        # attitude, rate, thrust and motor output then shows it.
        sp_n = _smooth(_smooth([r[0] for r in raw], self.rate), self.rate)
        sp_e = _smooth(_smooth([r[1] for r in raw], self.rate), self.rate)
        sp_d = _smooth(_smooth([r[2] for r in raw], self.rate), self.rate)
        sp_vn = [diff(sp_n, i) for i in range(len(raw))]
        sp_ve = [diff(sp_e, i) for i in range(len(raw))]
        sp_vd = [diff(sp_d, i) for i in range(len(raw))]
        sp_an = [diff(sp_vn, i) for i in range(len(raw))]
        sp_ae = [diff(sp_ve, i) for i in range(len(raw))]
        sp_ad = [diff(sp_vd, i) for i in range(len(raw))]
        shift = int(lag * self.rate)

        for i in range(count):
            t = i * dt
            n_sp, e_sp, d_sp, nav, armed, target = raw[i]
            airborne = d_sp < -0.3 or (t_climb <= t < t_down)

            # Yaw: along the track while moving, towards home while holding
            # before the return, rate-limited as PX4 limits it.
            speed_h = math.hypot(sp_vn[i], sp_ve[i])
            want = yaw_sp
            if speed_h > 0.8:
                want = math.atan2(sp_ve[i], sp_vn[i])
            elif nav == NAV_LOITER and airborne:
                want = math.atan2(-pts[-1][1], -pts[-1][0])
            step = _wrap(want - yaw_sp)
            limit = math.radians(40.0) * dt
            yaw_sp = _wrap(yaw_sp + max(-limit, min(limit, step)))

            j = max(0, i - shift)
            act = (sp_n[j] + (wobble(t, 0, 0) if airborne else 0.0),
                   sp_e[j] + (wobble(t, 1, 0) if airborne else 0.0),
                   sp_d[j] + (wobble(t, 2, 0) if airborne else 0.0))
            act_v = (sp_vn[j] + (wobble(t, 0, 1) if airborne else 0.0),
                     sp_ve[j] + (wobble(t, 1, 1) if airborne else 0.0),
                     sp_vd[j] + (wobble(t, 2, 1) if airborne else 0.0))
            act_a = (sp_an[j] + (wobble(t, 0, 2) if airborne else 0.0),
                     sp_ae[j] + (wobble(t, 1, 2) if airborne else 0.0),
                     sp_ad[j] + (wobble(t, 2, 2) if airborne else 0.0))
            yaw = rows[j]["yaw_sp"] if j < len(rows) else yaw_sp

            roll_sp, pitch_sp, thrust_g = _tilt((sp_an[i], sp_ae[i], sp_ad[i]),
                                                (sp_vn[i], sp_ve[i], sp_vd[i]), yaw_sp)
            roll, pitch, _ = _tilt(act_a, act_v, yaw)
            if not airborne:
                roll_sp = pitch_sp = 0.0
                roll, pitch = 0.004, -0.006
            # A light first-order filter on the tilt, the attitude controller's
            # own bandwidth; without it a step in acceleration is a step in
            # angle, which no airframe flies.
            alpha = dt / (0.12 + dt)
            filt_sp = (filt_sp[0] + alpha * (roll_sp - filt_sp[0]),
                       filt_sp[1] + alpha * (pitch_sp - filt_sp[1]))
            filt_act = (filt_act[0] + alpha * (roll - filt_act[0]),
                        filt_act[1] + alpha * (pitch - filt_act[1]))
            clean = (filt_act[0], filt_act[1], yaw)
            sp_euler = (filt_sp[0], filt_sp[1], yaw_sp)

            def rates(now: tuple[float, float, float],
                      before: tuple[float, float, float] | None) -> tuple[float, float, float]:
                if before is None:
                    return 0.0, 0.0, 0.0
                dr = (now[0] - before[0]) / dt
                dp = (now[1] - before[1]) / dt
                dy = _wrap(now[2] - before[2]) / dt
                r_, p_ = now[0], now[1]
                return (dr - dy * math.sin(p_),
                        dp * math.cos(r_) + dy * math.cos(p_) * math.sin(r_),
                        -dp * math.sin(r_) + dy * math.cos(p_) * math.cos(r_))

            # Rates come from the clean attitude and the sensor noise goes on
            # afterwards: differentiating a noisy angle at 50 Hz turns a
            # hundredth of a degree into a rate loop that looks like it rings.
            body = rates(clean, prev_euler)
            body_sp = rates(sp_euler, prev_sp_euler)
            prev_euler, prev_sp_euler = clean, sp_euler
            euler = (clean[0] + rng.gauss(0, 0.0015), clean[1] + rng.gauss(0, 0.0015),
                     _wrap(clean[2] + rng.gauss(0, 0.0015)))

            hover = 0.42
            thrust = hover * thrust_g if airborne else (0.12 if armed else 0.0)
            rows.append({
                "t": t, "nav": nav, "armed": armed, "airborne": airborne,
                "target": target,
                "sp": (n_sp, e_sp, d_sp), "sp_v": (sp_vn[i], sp_ve[i], sp_vd[i]),
                "pos": act, "vel": act_v, "acc": act_a,
                "euler": euler, "sp_euler": sp_euler, "yaw_sp": yaw_sp,
                "rates": tuple(r + rng.gauss(0, 0.0025) for r in body),
                "rates_sp": body_sp, "thrust": thrust,
            })
        return rows


def build(scene_home: tuple[float, float], path: flight_mod.Path, params: dict[str, Any],
          *, px4_version: tuple[int, int, int] = (1, 18, 0),
          git_hash: str = "c9b8a7f6e5d4c3b2a1908f7e6d5c4b3a29180706",
          boot_offset_s: float = 12.0,
          chatter: Iterable[tuple[float, int, str]] = ()) -> bytes:
    """The ULog of one sortie over *path*, flown from *scene_home*."""
    alt = max((p.alt for p in path.points), default=40.0) or 40.0
    sortie = Sortie(scene_home, path, alt)
    rows = sortie.samples()
    rng = random.Random(sortie.seed + 1)
    mn, me = flight_mod._metres_per_degree(scene_home[0])

    def stamp(t: float) -> int:
        return int((boot_offset_s + t) * 1e6)

    writer = ULogWriter(stamp(0.0))
    major, minor, patch = px4_version
    writer.info("sys_name", "PX4")
    writer.info("ver_hw", "PX4_FMU_V6X")
    writer.info("ver_sw", git_hash)
    writer.info("ver_sw_release", (major << 24) | (minor << 16) | (patch << 8) | 0xFF)
    writer.info("ver_sw_branch", f"release/{major}.{minor}")
    writer.info("sys_os_name", "NuttX")
    writer.info("sys_mcu", "STM32H7[4|5]xxx, rev. V")
    for name, fields in _FORMATS.items():
        writer.define(name, fields)
    for name, value in params.items():
        writer.parameter(name, value)
    for name in _FORMATS:
        if name == "position_setpoint":
            continue
        writer.subscribe(name)
    writer.subscribe("vehicle_imu_status", 1)

    def latlon(n: float, e: float) -> tuple[float, float]:
        return scene_home[0] + n / mn, scene_home[1] + e / me

    capacity = float(params.get("BAT1_CAPACITY", 16000.0) or 16000.0)
    cells = int(params.get("BAT1_N_CELLS", 6) or 6)
    used_mah = 0.0
    v_filt = None
    i_filt = 0.0
    earth = (0.205, 0.012, 0.437)          # gauss, NED, southern Bavaria

    every = {name: max(1, int(round(sortie.rate / hz))) for name, hz in (
        ("fast", 50), ("pos", 10), ("gps", 10), ("slow", 5), ("sys", 2), ("bat", 5))}

    for index, row in enumerate(rows):
        t = row["t"]
        ts = stamp(t)
        roll, pitch, yaw = row["euler"]
        sroll, spitch, syaw = row["sp_euler"]
        thrust = row["thrust"]

        # Current and pack voltage: idle on the ground, hover current in the
        # air, more for climbing and for the tilt that fast flight needs.
        current = 0.8 if not row["armed"] else (3.5 if not row["airborne"]
                                                else 4.0 + 72.0 * thrust ** 1.5)
        used_mah += current * (1.0 / sortie.rate) / 3.6
        used = min(1.0, used_mah / capacity)
        ocv = cells * (4.16 - 0.45 * used - 0.25 * used ** 6)
        volts = ocv - current * 0.021 + rng.gauss(0, 0.012)
        v_filt = volts if v_filt is None else v_filt + 0.05 * (volts - v_filt)
        i_filt += 0.05 * (current - i_filt)

        if index % every["fast"] == 0:
            writer.data("vehicle_attitude", {"timestamp": ts, "timestamp_sample": ts - 800,
                                             "q": _quat(roll, pitch, yaw)})
            writer.data("vehicle_attitude_setpoint", {
                "timestamp": ts, "roll_body": sroll, "pitch_body": spitch, "yaw_body": syaw,
                "q_d": _quat(sroll, spitch, syaw), "thrust_body": [0.0, 0.0, -thrust]})
            writer.data("vehicle_angular_velocity", {
                "timestamp": ts, "timestamp_sample": ts - 400, "xyz": list(row["rates"])})
            writer.data("vehicle_rates_setpoint", {
                "timestamp": ts, "roll": row["rates_sp"][0], "pitch": row["rates_sp"][1],
                "yaw": row["rates_sp"][2], "thrust_body": [0.0, 0.0, -thrust]})
            writer.data("vehicle_thrust_setpoint", {
                "timestamp": ts, "timestamp_sample": ts, "xyz": [0.0, 0.0, -thrust]})
            p, q, r = row["rates"]
            if row["armed"]:
                # Quad X, PX4 order: 1 front right CCW, 2 rear left CCW,
                # 3 front left CW, 4 rear right CW.
                mix = (-p + q + r, p - q + r, p + q - r, -p - q - r)
                trim = (0.004, -0.003, 0.009, -0.006)
                motors = [max(0.0, min(1.0, thrust + 0.035 * m + trim[k] + rng.gauss(0, 0.006)))
                          for k, m in enumerate(mix)]
            else:
                motors = [0.0] * 4
            writer.data("actuator_motors", {"timestamp": ts, "timestamp_sample": ts,
                                            "control": motors + [0.0] * 8})

        if index % every["pos"] == 0:
            n, e, d = row["pos"]
            vn, ve, vd = row["vel"]
            an, ae, ad = row["acc"]
            writer.data("vehicle_local_position", {
                "timestamp": ts, "timestamp_sample": ts - 2000,
                "xy_valid": True, "z_valid": True, "v_xy_valid": True, "v_z_valid": True,
                "x": n, "y": e, "z": d, "vx": vn, "vy": ve, "vz": vd,
                "ax": an, "ay": ae, "az": ad, "heading": yaw,
                "xy_global": True, "z_global": True, "ref_timestamp": stamp(0.0),
                "ref_lat": scene_home[0], "ref_lon": scene_home[1], "ref_alt": HOME_AMSL,
                "dist_bottom": -d, "eph": 0.12, "epv": 0.18})
            sn, se, sd = row["sp"]
            svn, sve, svd = row["sp_v"]
            writer.data("vehicle_local_position_setpoint", {
                "timestamp": ts, "x": sn, "y": se, "z": sd, "vx": svn, "vy": sve, "vz": svd,
                "yaw": syaw})
            lat, lon = latlon(n, e)
            writer.data("vehicle_global_position", {
                "timestamp": ts, "timestamp_sample": ts - 2000, "lat": lat, "lon": lon,
                "alt": HOME_AMSL - d, "eph": 0.12, "epv": 0.18})

        if index % every["gps"] == 0:
            n, e, d = row["pos"]
            vn, ve, vd = row["vel"]
            lat, lon = latlon(n + rng.gauss(0, 0.012), e + rng.gauss(0, 0.012))
            vel_n, vel_e = vn + rng.gauss(0, 0.02), ve + rng.gauss(0, 0.02)
            writer.data("vehicle_gps_position", {
                "timestamp": ts, "timestamp_sample": ts - 60000, "device_id": 11469065,
                "lat": int(lat * 1e7), "lon": int(lon * 1e7),
                "alt": int((HOME_AMSL - d + rng.gauss(0, 0.02)) * 1000),
                "s_variance_m_s": 0.04 + rng.random() * 0.02,
                "eph": 0.014 + rng.random() * 0.006, "epv": 0.021 + rng.random() * 0.008,
                "noise_per_ms": 88 + rng.randint(0, 9), "jamming_indicator": 18 + rng.randint(0, 5),
                "vel_m_s": math.hypot(vel_n, vel_e), "vel_n_m_s": vel_n, "vel_e_m_s": vel_e,
                "vel_d_m_s": vd + rng.gauss(0, 0.03), "cog_rad": math.atan2(vel_e, vel_n),
                "fix_type": 6, "vel_ned_valid": True,
                "satellites_used": 19 + (1 if (index // 700) % 3 == 1 else 0)})

        if index % every["slow"] == 0:
            n, e, d = row["pos"]
            writer.data("vehicle_air_data", {
                "timestamp": ts, "timestamp_sample": ts, "baro_device_id": 12018473,
                "baro_alt_meter": HOME_AMSL - d + 0.3 * math.sin(t / 23.0) + rng.gauss(0, 0.08),
                "baro_temp_celcius": 31.5 + t * 0.012, "baro_pressure_pa": 94430.0 + 11.4 * d,
                "rho": 1.12})
            cy, sy = math.cos(yaw), math.sin(yaw)
            cp, sp = math.cos(pitch), math.sin(pitch)
            cr, sr = math.cos(roll), math.sin(roll)
            bn, be, bd = earth
            # World to body, ZYX: the field the magnetometer sees as the
            # airframe turns under it.
            x1 = cy * bn + sy * be
            y1 = -sy * bn + cy * be
            x2 = cp * x1 - sp * bd
            z2 = sp * x1 + cp * bd
            mag = [x2, cr * y1 + sr * z2, -sr * y1 + cr * z2]
            writer.data("vehicle_magnetometer", {
                "timestamp": ts, "timestamp_sample": ts, "device_id": 396825,
                "magnetometer_ga": [m + rng.gauss(0, 0.0015) for m in mag]})
            for imu in (0, 1):
                writer.data("vehicle_imu_status", {
                    "timestamp": ts, "accel_device_id": 2818058 + imu, "gyro_device_id": 2818060 + imu,
                    "accel_clipping": [0, 0, 0],
                    "accel_vibration_metric": (1.6 + 4.2 * thrust + rng.random() * 0.5) * (1.0 + 0.15 * imu),
                    "gyro_vibration_metric": (0.012 + 0.03 * thrust + rng.random() * 0.006) * (1.0 + 0.2 * imu),
                    "temperature_accel": 44.5 + 1.2 * imu + t * 0.018,
                    "temperature_gyro": 44.5 + 1.2 * imu + t * 0.018}, multi_id=imu)
            writer.data("estimator_status", {
                "timestamp": ts, "vel_test_ratio": 0.08 + rng.random() * 0.16,
                "pos_test_ratio": 0.05 + rng.random() * 0.12,
                "hgt_test_ratio": 0.10 + rng.random() * 0.20,
                "mag_test_ratio": 0.04 + rng.random() * 0.08, "tas_test_ratio": 0.0})
            writer.data("estimator_sensor_bias", {
                "timestamp": ts, "gyro_bias": [0.0011 + t * 2e-6, -0.0007, 0.0004 - t * 1e-6],
                "accel_bias": [0.021, -0.013, 0.034]})
            writer.data("vehicle_status", {
                "timestamp": ts, "nav_state": row["nav"],
                "arming_state": ARMING_ARMED if row["armed"] else ARMING_STANDBY,
                "failsafe": False, "failure_detector_status": 0})
            writer.data("input_rc", {
                "timestamp": ts, "timestamp_last_signal": ts, "channel_count": 16,
                "rssi": 94 + rng.randint(-1, 1), "rc_lost": False,
                "values": [1500, 1500, 1500 if row["airborne"] else 1000, 1500,
                           1500, 1000, 2000 if row["armed"] else 1000, 1000,
                           1500, 1000, 1700, 1300, 1500, 1500, 1500, 1500, 0, 0]})
            writer.data("manual_control_setpoint", {
                "timestamp": ts, "roll": rng.gauss(0, 0.004), "pitch": rng.gauss(0, 0.004),
                "yaw": rng.gauss(0, 0.003), "throttle": 0.0 if row["airborne"] else -1.0,
                "valid": True})
            tn, te = row["target"]
            lat, lon = latlon(tn, te)
            writer.data("position_setpoint_triplet", {
                "timestamp": ts,
                "current": {"timestamp": ts, "valid": row["armed"], "type": 0,
                            "lat": lat, "lon": lon, "alt": HOME_AMSL + sortie.alt}})

        if index % every["bat"] == 0:
            writer.data("battery_status", {
                "timestamp": ts, "voltage_v": volts, "voltage_filtered_v": v_filt,
                "current_a": current + rng.gauss(0, 0.3), "current_filtered_a": i_filt,
                "discharged_mah": used_mah, "remaining": 1.0 - used,
                "temperature": 29.0 + t * 0.03, "cell_count": cells, "connected": True})

        if index % every["sys"] == 0:
            writer.data("cpuload", {"timestamp": ts, "load": 0.46 + rng.random() * 0.06,
                                    "ram_usage": 0.583})
            writer.data("wind", {"timestamp": ts,
                                 "windspeed_north": 1.4 + 0.3 * math.sin(t / 17.0),
                                 "windspeed_east": -0.9 + 0.2 * math.cos(t / 13.0)})

    # What PX4 says while it flies, placed at the transitions it is about.
    previous = None
    said: list[tuple[float, int, str]] = [(0.2, 6, "[logger] file: /fs/microsd/log/sess042/log001.ulg"),
                                          (1.0, 6, "Using GPS 1 (UBX, RTK fixed)")]
    for row in rows:
        key = (row["nav"], row["armed"], row["airborne"])
        if previous is not None and key != previous:
            t = row["t"]
            if row["armed"] and not previous[1]:
                said.append((t, 6, "Armed by external command"))
            if row["nav"] != previous[0]:
                if row["nav"] == NAV_TAKEOFF:
                    said.append((t + 0.1, 6, f"Takeoff to {sortie.alt:.0f} m above home"))
                elif row["nav"] == NAV_MISSION:
                    said.append((t, 6, "Executing mission item 1"))
                elif row["nav"] == NAV_LOITER:
                    said.append((t, 6, "Mission finished, loitering"))
                elif row["nav"] == NAV_RTL:
                    said.append((t, 6, f"RTL: start return at {sortie.alt:.0f} m "
                                       f"({sortie.alt:.0f} m above destination)"))
            if previous[2] and not row["airborne"]:
                said.append((t, 6, "Landing detected"))
            if not row["armed"] and previous[1]:
                said.append((t, 6, "Disarmed by landing"))
        previous = key
    for at, level, text in list(said) + list(chatter):
        writer.log(stamp(at), level, text)
    return writer.finish()
