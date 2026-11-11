"""Synthetic PX4 flights as ULog bytes, for holding the Rust review to the Python one.

Every flight is built from blocks (status, local position, GPS, motors, ...)
with a few knobs that inject one fault each, so each detector and each plot of
``flight_review`` runs on something. Deterministic: a flight is a name and a
seed, nothing else.
"""
from __future__ import annotations

import math
import random
import struct
from collections.abc import Callable

from corvus.ulog import MAGIC

CT = {"uint64_t": "Q", "int64_t": "q", "int32_t": "i", "uint32_t": "I", "uint8_t": "B",
      "int8_t": "b", "uint16_t": "H", "int16_t": "h", "float": "f", "double": "d", "bool": "?"}


def _msg(kind: str, body: bytes) -> bytes:
    return struct.pack("<HB", len(body), ord(kind)) + body


class Log:
    def __init__(self, start_us: int = 5_000_000, nested: dict[str, list[str]] | None = None) -> None:
        self.start_us = start_us
        self.defs: list[bytes] = []
        self.adds: list[bytes] = []
        self.records: list[tuple[int, int, bytes]] = []
        self.next_id = 0
        self.nested = nested or {}
        self.seq = 0
        for name, fields in self.nested.items():
            self.defs.append(_msg("F", f"{name}:{';'.join(fields)};".encode()))

    def info(self, key: str, value: bytes) -> None:
        self.defs.append(_msg("I", bytes([len(key)]) + key.encode() + value))

    def param(self, key: str, value: bytes) -> None:
        self.defs.append(_msg("P", bytes([len(key)]) + key.encode() + value))

    def topic(self, name: str, fields: list[str], multi: int = 0) -> "Topic":
        return Topic(self, name, fields, multi)

    def message(self, t_us: int, level: int, text: str) -> None:
        self.records.append((t_us, self.seq, _msg("L", bytes([level]) + struct.pack("<Q", t_us) + text.encode())))
        self.seq += 1

    def dropout(self, t_us: int, ms: int) -> None:
        self.records.append((t_us, self.seq, _msg("O", struct.pack("<H", ms))))
        self.seq += 1

    def blob(self, truncate: int = 0) -> bytes:
        out = MAGIC + b"\x01" + struct.pack("<Q", self.start_us)
        out += b"".join(self.defs) + b"".join(self.adds)
        for _, _, rec in sorted(self.records, key=lambda r: (r[0], r[1])):
            out += rec
        return out[:-truncate] if truncate else out


class Topic:
    def __init__(self, log: Log, name: str, fields: list[str], multi: int) -> None:
        self.log = log
        self.msg_id = log.next_id
        log.next_id += 1
        self.names: list[tuple[str, str, int]] = []
        for spec in fields:
            ctype, _, fname = spec.partition(" ")
            count = 1
            if "[" in ctype:
                ctype, _, tail = ctype.partition("[")
                count = int(tail.rstrip("]"))
            if ctype in log.nested:
                for k in range(count):
                    prefix = fname if count == 1 else f"{fname}[{k}]"
                    for sub in log.nested[ctype]:
                        st, _, sn = sub.partition(" ")
                        sc = 1
                        if "[" in st:
                            st, _, tail = st.partition("[")
                            sc = int(tail.rstrip("]"))
                        self.names.append((st, f"{prefix}.{sn}", sc))
            else:
                self.names.append((ctype, fname, count))
        self.fmt = "<" + "".join(f"{c}{CT[t]}" if c != 1 else CT[t] for t, _, c in self.names)
        text = f"{name}:{';'.join(fields)};"
        if not any(text.encode() in d for d in log.defs):
            log.defs.append(_msg("F", text.encode()))
        log.adds.append(_msg("A", bytes([multi]) + struct.pack("<H", self.msg_id) + name.encode()))

    def add(self, t_us: int, **values) -> None:
        args: list = []
        for ctype, fname, count in self.names:
            v = t_us if fname == "timestamp" else values.get(fname.replace(".", "__"), 0)
            if count == 1:
                args.append(v)
            else:
                v = list(v) if isinstance(v, (list, tuple)) else [v] * count
                args.extend(v + [0] * (count - len(v)))
        body = struct.pack("<H", self.msg_id) + struct.pack(self.fmt, *args)
        self.log.records.append((t_us, self.log.seq, _msg("D", body)))
        self.log.seq += 1


NESTED = {
    "position_setpoint": ["bool valid", "double lat", "double lon", "float x", "float y"],
    "esc_report": ["uint32_t esc_rpm"],
}


def fl(x: float) -> float:
    return struct.unpack("<f", struct.pack("<f", x))[0]


class Flight:
    """One synthetic flight: a clock, noise and the blocks that write topics."""

    def __init__(self, seed: int, seconds: float = 14.0, hz: int = 50, legacy: bool = False) -> None:
        self.rng = random.Random(seed)
        self.log = Log(nested=NESTED)
        self.seconds = seconds
        self.hz = hz
        self.legacy = legacy
        self.t0 = 7_000_000
        self.arm = (2.0, seconds - 2.0)
        self.thr = lambda t: 0.5 + 0.2 * math.sin(t * 0.9)

    def ticks(self, hz: int | None = None):
        hz = hz or self.hz
        n = int(self.seconds * hz)
        for i in range(n):
            t = i / hz
            yield self.t0 + int(t * 1e6), t

    def noise(self, s: float = 1.0) -> float:
        return self.rng.gauss(0, s)

    # -------------------------------------------------------------- blocks

    def status(self, nav: Callable[[float], int] = lambda t: 2, hz: int = 10, failsafe: Callable[[float], bool] | None = None,
               failure: Callable[[float], int] | None = None, armed: Callable[[float], int] | None = None,
               frozen: bool = False, legacy: bool | None = None) -> None:
        legacy = self.legacy if legacy is None else legacy
        a, b = self.arm
        armed = armed or (lambda t: 2 if a <= t <= b else 1)
        if legacy:
            tp = self.log.topic("commander_state", ["uint64_t timestamp", "uint8_t main_state"])
            for ts, t in self.ticks(hz):
                tp.add(self.t0 if frozen else ts, main_state=nav(t))
            return
        tp = self.log.topic("vehicle_status", ["uint64_t timestamp", "uint8_t nav_state", "uint8_t arming_state",
                                               "uint16_t failure_detector_status", "bool failsafe"])
        for ts, t in self.ticks(hz):
            tp.add(self.t0 if frozen else ts, nav_state=nav(t), arming_state=armed(t),
                   failure_detector_status=failure(t) if failure else 0,
                   failsafe=bool(failsafe(t)) if failsafe else False)

    def local(self, moving: bool = True, global_ref: bool = True, resets: list[float] | None = None, hz: int = 50) -> None:
        tp = self.log.topic("vehicle_local_position", [
            "uint64_t timestamp", "float x", "float y", "float z", "float vx", "float vy", "float vz",
            "double ref_lat", "double ref_lon", "float ref_alt", "bool xy_global",
            "uint8_t xy_reset_counter", "uint8_t z_reset_counter", "uint8_t vxy_reset_counter",
            "uint8_t heading_reset_counter"])
        resets = resets or []
        for ts, t in self.ticks(hz):
            n = sum(1 for r in resets if t >= r)
            x = 40 * math.sin(t * 0.5) if moving else 0.01
            y = 30 * math.sin(t * 0.3) * (1 if moving else 0)
            tp.add(ts, x=x, y=y, z=-(5 + 3 * math.sin(t * 0.4)), vx=20 * math.cos(t * 0.5) * (1 if moving else 0),
                   vy=9 * math.cos(t * 0.3) * (1 if moving else 0), vz=-1.2 * math.cos(t * 0.4),
                   ref_lat=47.3977419 if global_ref else 0.0, ref_lon=8.5455938 if global_ref else 0.0,
                   ref_alt=488.0, xy_global=global_ref, xy_reset_counter=n, z_reset_counter=n,
                   vxy_reset_counter=n, heading_reset_counter=n % 256)

    def local_setpoint(self, hz: int = 20) -> None:
        tp = self.log.topic("vehicle_local_position_setpoint", ["uint64_t timestamp", "float x", "float y", "float z",
                                                                "float vx", "float vy", "float vz"])
        for ts, t in self.ticks(hz):
            tp.add(ts, x=40 * math.sin(t * 0.5 - 0.1), y=30 * math.sin(t * 0.3), z=-(5 + 3 * math.sin(t * 0.4)),
                   vx=20 * math.cos(t * 0.5), vy=9 * math.cos(t * 0.3), vz=-1.2 * math.cos(t * 0.4))

    def global_pos(self, hz: int = 20) -> None:
        tp = self.log.topic("vehicle_global_position", ["uint64_t timestamp", "double lat", "double lon", "float alt"])
        for ts, t in self.ticks(hz):
            tp.add(ts, lat=47.3977419 + 1e-5 * math.sin(t * 0.5), lon=8.5455938, alt=488.0 + 5 + 3 * math.sin(t * 0.4))

    def gps(self, fix: Callable[[float], int] = lambda t: 3, legacy: bool | None = None, hz: int = 10) -> None:
        legacy = self.legacy if legacy is None else legacy
        if legacy:
            tp = self.log.topic("sensor_gps", ["uint64_t timestamp", "double lat", "double lon", "float alt",
                                               "uint8_t fix_type", "uint8_t satellites_used", "float eph", "float epv",
                                               "float s_variance_m_s", "float noise_per_ms", "float jamming_indicator",
                                               "float vel_n_m_s", "float vel_e_m_s"])
            for ts, t in self.ticks(hz):
                f = fix(t)
                tp.add(ts, lat=47.3977419 + 4.5e-5 * math.sin(t * 0.5) if f else 0.0,
                       lon=8.5455938 + 3e-5 * math.sin(t * 0.3) if f else 0.0, alt=493.0 + 3 * math.sin(t * 0.4),
                       fix_type=f, satellites_used=12 - (t > 8), eph=0.8 + 0.1 * math.sin(t), epv=1.2,
                       s_variance_m_s=0.3, noise_per_ms=90 + 5 * math.sin(t), jamming_indicator=10,
                       vel_n_m_s=20 * math.cos(t * 0.5), vel_e_m_s=9 * math.cos(t * 0.3))
            return
        tp = self.log.topic("vehicle_gps_position", ["uint64_t timestamp", "int32_t lat", "int32_t lon", "int32_t alt",
                                                     "uint8_t fix_type", "uint8_t satellites_used", "float eph", "float epv",
                                                     "float s_variance_m_s", "uint16_t noise_per_ms", "uint8_t jamming_indicator",
                                                     "float vel_n_m_s", "float vel_e_m_s"])
        for ts, t in self.ticks(hz):
            f = fix(t)
            tp.add(ts, lat=int((47.3977419 + 4.5e-5 * math.sin(t * 0.5)) * 1e7) if f else 0,
                   lon=int((8.5455938 + 3e-5 * math.sin(t * 0.3)) * 1e7) if f else 0,
                   alt=int((493.0 + 3 * math.sin(t * 0.4)) * 1000), fix_type=f, satellites_used=12, eph=0.8, epv=1.2,
                   s_variance_m_s=0.3, noise_per_ms=90, jamming_indicator=10,
                   vel_n_m_s=20 * math.cos(t * 0.5), vel_e_m_s=9 * math.cos(t * 0.3))

    def waypoints(self, local_frame: bool = False) -> None:
        tp = self.log.topic("position_setpoint_triplet", ["uint64_t timestamp", "position_setpoint current"])
        pts = [(47.3977419 + 4e-5 * k, 8.5455938 + 2e-5 * k) for k in range(4)]
        for ts, t in self.ticks(10):
            k = min(3, int(t / (self.seconds / 4)))
            kw = {"current__valid": t > 0.5, "current__lat": pts[k][0], "current__lon": pts[k][1]}
            if local_frame:
                kw = {"current__valid": True, "current__x": 10.0 * k, "current__y": 5.0 * k}
            tp.add(ts, **kw)

    def attitude(self, legacy: bool | None = None, hz: int = 50, offset: float = 0.0, setpoint: bool = True) -> None:
        legacy = self.legacy if legacy is None else legacy
        name = "control_state" if legacy else "vehicle_attitude"
        tp = self.log.topic(name, ["uint64_t timestamp", "float[4] q"])
        for ts, t in self.ticks(hz):
            roll = 0.25 * math.sin(t * 1.1) + offset
            pitch = 0.18 * math.sin(t * 0.7)
            yaw = 0.6 * t
            cr, sr, cp, sp, cy, sy = (math.cos(roll / 2), math.sin(roll / 2), math.cos(pitch / 2), math.sin(pitch / 2),
                                      math.cos(yaw / 2), math.sin(yaw / 2))
            tp.add(ts, q=[cr * cp * cy + sr * sp * sy, sr * cp * cy - cr * sp * sy,
                          cr * sp * cy + sr * cp * sy, cr * cp * sy - sr * sp * cy])
        if setpoint:
            sp_ = self.log.topic("vehicle_attitude_setpoint", ["uint64_t timestamp", "float roll_body", "float pitch_body",
                                                                "float yaw_body"])
            for ts, t in self.ticks(20):
                sp_.add(ts, roll_body=0.25 * math.sin(t * 1.1 - 0.05), pitch_body=0.18 * math.sin(t * 0.7 - 0.04),
                        yaw_body=math.atan2(math.sin(0.6 * t), math.cos(0.6 * t)))

    def rates(self, legacy: bool | None = None, hz: int = 200, ring: float = 0.0, ring_hz: float = 18.0) -> None:
        legacy = self.legacy if legacy is None else legacy
        if legacy:
            tp = self.log.topic("sensor_combined", ["uint64_t timestamp", "float[3] gyro_rad"])
        else:
            tp = self.log.topic("vehicle_angular_velocity", ["uint64_t timestamp", "float[3] xyz"])
        for ts, t in self.ticks(hz):
            r = 0.3 * math.cos(t * 1.1) + ring * math.sin(2 * math.pi * ring_hz * t) + self.noise(0.01)
            vals = [r, 0.2 * math.cos(t * 0.7) + self.noise(0.01), 0.1 + self.noise(0.01)]
            tp.add(ts, **({"gyro_rad": vals} if legacy else {"xyz": vals}))
        sp = self.log.topic("vehicle_rates_setpoint", ["uint64_t timestamp", "float roll", "float pitch", "float yaw"])
        for ts, t in self.ticks(50):
            sp.add(ts, roll=0.3 * math.cos(t * 1.1 + 0.03), pitch=0.2 * math.cos(t * 0.7), yaw=0.1)

    def thrust(self, legacy: bool | None = None, hz: int = 50) -> None:
        legacy = self.legacy if legacy is None else legacy
        if legacy:
            tp = self.log.topic("actuator_controls_0", ["uint64_t timestamp", "float[8] control"])
            for ts, t in self.ticks(hz):
                tp.add(ts, control=[0, 0, 0, self.thr(t)])
            return
        tp = self.log.topic("vehicle_thrust_setpoint", ["uint64_t timestamp", "float[3] xyz"])
        for ts, t in self.ticks(hz):
            tp.add(ts, xyz=[0, 0, -self.thr(t)])

    def motors(self, pinned: tuple[int, float] | None = None, all_high: bool = False, hz: int = 50,
               legacy: bool | None = None, spread: float = 0.0) -> None:
        legacy = self.legacy if legacy is None else legacy
        if legacy:
            tp = self.log.topic("actuator_outputs", ["uint64_t timestamp", "uint32_t noutputs", "float[16] output"])
            for ts, t in self.ticks(hz):
                base = 1100 + 600 * self.thr(t)
                tp.add(ts, noutputs=4, output=[base + 10 * i + spread * (i == 2) for i in range(4)] + [1500] * 4)
            return
        tp = self.log.topic("actuator_motors", ["uint64_t timestamp", "float[12] control"])
        for ts, t in self.ticks(hz):
            base = self.thr(t) + 0.05 * math.sin(t * 3)
            row = [base + 0.02 * i for i in range(4)]
            if all_high and 3 < t < 7:
                row = [0.98] * 4
            if pinned and pinned[1] <= t <= pinned[1] + 4:
                row[pinned[0]] = 1.0
            tp.add(ts, control=row)

    def esc(self, count: int = 4) -> None:
        tp = self.log.topic("esc_status", ["uint64_t timestamp", "uint8_t esc_count", "esc_report[8] esc"])
        for ts, t in self.ticks(20):
            kw = {"esc_count": count}
            for i in range(count):
                kw[f"esc[{i}]__esc_rpm"] = int(4000 + 3000 * self.thr(t) + 40 * i) if i != 1 or t < 9 else 0
            tp.add(ts, **kw)

    def estimator(self, rejected: tuple[float, float] | None = None, vibe: bool = False, hz: int = 20) -> None:
        tp = self.log.topic("estimator_status", ["uint64_t timestamp", "float mag_test_ratio", "float vel_test_ratio",
                                                 "float pos_test_ratio", "float hgt_test_ratio", "float tas_test_ratio",
                                                 "float[3] vibe"])
        for ts, t in self.ticks(hz):
            hi = rejected is not None and rejected[0] <= t <= rejected[1]
            tp.add(ts, mag_test_ratio=1.8 if hi else 0.3 + 0.1 * math.sin(t), vel_test_ratio=0.2, pos_test_ratio=0.25,
                   hgt_test_ratio=0.3, tas_test_ratio=0.1, vibe=[0.01, 0.02 + (25 if vibe else 0) * self.thr(t) * 0.0 + 0.0, 0.5 + 20 * vibe * self.thr(t)])

    def imu(self, vib: Callable[[float], float] = lambda t: 8.0, clip_at: float | None = None, n: int = 2,
            counter: bool = False, legacy: bool | None = None, hz: int = 20) -> None:
        legacy = self.legacy if legacy is None else legacy
        for m in range(n):
            if legacy or counter:
                tp = self.log.topic("sensor_accel_status", ["uint64_t timestamp", "uint32_t accel_clipping_count",
                                                            "float temperature_accel"], multi=m)
            else:
                tp = self.log.topic("vehicle_imu_status", ["uint64_t timestamp", "float accel_vibration_metric",
                                                           "float gyro_vibration_metric", "float temperature_accel",
                                                           "uint32_t[3] accel_clipping"], multi=m)
            for ts, t in self.ticks(hz):
                clip = 0 if clip_at is None else int(max(0.0, t - clip_at) * 40) if m == 0 else 0
                if legacy or counter:
                    tp.add(ts, accel_clipping_count=clip, temperature_accel=30 + t * 0.3 + m)
                else:
                    tp.add(ts, accel_vibration_metric=vib(t) + m, gyro_vibration_metric=0.01, temperature_accel=30 + t * 0.3 + m,
                           accel_clipping=[clip, clip // 2, 0])

    def mag(self, coupled: float = 0.0, legacy: bool | None = None, hz: int = 50) -> None:
        legacy = self.legacy if legacy is None else legacy
        if legacy:
            tp = self.log.topic("sensor_mag", ["uint64_t timestamp", "float x", "float y", "float z"])
            for ts, t in self.ticks(hz):
                f = 0.45 + coupled * self.thr(t) + self.noise(0.002)
                tp.add(ts, x=f * 0.8, y=f * 0.5, z=f * 0.33)
            return
        tp = self.log.topic("vehicle_magnetometer", ["uint64_t timestamp", "float[3] magnetometer_ga"])
        for ts, t in self.ticks(hz):
            f = 0.45 + coupled * self.thr(t) + self.noise(0.002)
            tp.add(ts, magnetometer_ga=[f * 0.8, f * 0.5, f * 0.33])

    def battery(self, cells: int = 4, sag: float = 0.0, floor: float | None = None, logged_cells: bool = True) -> None:
        tp = self.log.topic("battery_status", ["uint64_t timestamp", "float voltage_filtered_v", "float current_filtered_a",
                                               "float remaining", "float discharged_mah", "int8_t cell_count"])
        for ts, t in self.ticks(10):
            v = cells * (4.15 - 0.012 * t) - sag * max(0.0, math.sin(t * 1.3)) ** 6
            if floor is not None and t > 5:
                v = cells * floor
            tp.add(ts, voltage_filtered_v=v, current_filtered_a=20 * self.thr(t), remaining=1 - t / 60,
                   discharged_mah=t * 20, cell_count=cells if logged_cells else 0)

    def cpu(self, high: tuple[float, float] | None = None) -> None:
        tp = self.log.topic("cpuload", ["uint64_t timestamp", "float load", "float ram_usage"])
        for ts, t in self.ticks(10):
            hi = high is not None and high[0] <= t <= high[1]
            tp.add(ts, load=0.97 if hi else 0.4 + 0.05 * math.sin(t), ram_usage=0.5)

    def rc(self, lost: Callable[[float], bool] = lambda t: False, name: str = "input_rc") -> None:
        field = "rc_lost" if name == "input_rc" else "signal_lost"
        tp = self.log.topic(name, ["uint64_t timestamp", "int32_t rssi", f"bool {field}"])
        for ts, t in self.ticks(10):
            tp.add(ts, rssi=80, **{field: lost(t)})

    def misc(self) -> None:
        a = self.log.topic("airspeed", ["uint64_t timestamp", "float indicated_airspeed_m_s", "float true_airspeed_m_s"])
        for ts, t in self.ticks(10):
            a.add(ts, indicated_airspeed_m_s=14 + math.sin(t), true_airspeed_m_s=15 + math.sin(t))
        w = self.log.topic("wind", ["uint64_t timestamp", "float windspeed_north", "float windspeed_east"])
        for ts, t in self.ticks(10):
            w.add(ts, windspeed_north=3 + math.sin(t), windspeed_east=2)
        for m in range(2):
            d = self.log.topic("distance_sensor", ["uint64_t timestamp", "float current_distance"], multi=m)
            for ts, t in self.ticks(10):
                d.add(ts, current_distance=5 + m + math.sin(t))
        b = self.log.topic("vehicle_air_data", ["uint64_t timestamp", "float baro_alt_meter", "float baro_temp_celcius"])
        for ts, t in self.ticks(20):
            b.add(ts, baro_alt_meter=488 + 5 + 3 * math.sin(t * 0.4) + self.noise(0.05), baro_temp_celcius=21 + t * 0.01)
        bias = self.log.topic("estimator_sensor_bias", ["uint64_t timestamp", "float[3] gyro_bias"])
        for ts, t in self.ticks(10):
            bias.add(ts, gyro_bias=[0.001 * t, -0.002, 0.0005])
        mc = self.log.topic("manual_control_setpoint", ["uint64_t timestamp", "float roll", "float pitch", "float yaw",
                                                        "float throttle"])
        for ts, t in self.ticks(20):
            mc.add(ts, roll=0.3 * math.sin(t), pitch=0.1, yaw=0.0, throttle=self.thr(t))

    def meta(self, sw: str = "v1.16.0-beta1", hw: str = "PX4_FMU_V6X", autostart: int = 4001) -> None:
        key = "char[%d] ver_sw" % len(sw)
        self.log.info(key, sw.encode())
        key = "char[%d] ver_hw" % len(hw)
        self.log.info(key, hw.encode())
        self.log.info("char[3] sys_name", b"PX4")
        self.log.param("int32_t SYS_AUTOSTART", struct.pack("<i", autostart))

    def messages(self, texts: list[tuple[float, int, str]]) -> None:
        for t, level, text in texts:
            self.log.message(self.t0 + int(t * 1e6), level, text)

    def build(self, truncate: int = 0) -> bytes:
        return self.log.blob(truncate)


def _full(f: Flight) -> Flight:
    f.meta()
    f.status(nav=lambda t: 2 if t < 6 else 3 if t < 10 else 5)
    f.local()
    f.local_setpoint()
    f.global_pos()
    f.gps()
    f.waypoints()
    f.attitude()
    f.rates(hz=100)
    f.thrust()
    f.motors()
    f.esc()
    f.estimator()
    f.imu()
    f.mag()
    f.battery()
    f.cpu()
    f.rc()
    f.misc()
    return f


def _quad(seed: int, **kw) -> Flight:
    return _full(Flight(seed, **kw))


def build_flights() -> dict[str, Callable[[], bytes]]:
    out: dict[str, Callable[[], bytes]] = {}

    def reg(name: str):
        def deco(fn: Callable[[], Flight | bytes]):
            def make() -> bytes:
                r = fn()
                return r if isinstance(r, bytes) else r.build()
            out[name] = make
            return fn
        return deco

    @reg("clean_quad")
    def _():
        return _quad(1)

    @reg("legacy_names")
    def _():
        f = Flight(2, legacy=True)
        f.meta(autostart=4011)
        f.status(nav=lambda t: 0 if t < 4 else 2 if t < 8 else 6)
        f.local()
        f.local_setpoint()
        f.gps()
        f.attitude()
        f.rates(hz=100)
        f.thrust()
        f.motors(spread=150.0)
        f.imu(vib=lambda t: 12.0)
        f.mag()
        f.rc(name="rc_channels")
        return f

    @reg("saturated_motor")
    def _():
        f = _quad(3)
        f.motors(pinned=(1, 4.0))
        f.messages([(3.0, ord("3"), "Preflight Fail: ekf2 missing data"), (8.0, ord("4"), "Low battery")])
        return f

    @reg("climb_all_pinned")
    def _():
        f = Flight(4)
        f.status()
        f.local()
        f.motors(all_high=True)
        return f

    @reg("vibration_throttle")
    def _():
        f = Flight(5, seconds=16.0, hz=60)
        f.status()
        f.local()
        f.thrust(hz=60)
        f.imu(vib=lambda t: 18 + 50 * f.thr(t), hz=60)
        f.motors(hz=60)
        return f

    @reg("vibration_mount")
    def _():
        f = Flight(6)
        f.status()
        f.local()
        f.thrust()
        f.imu(vib=lambda t: 50 + 6 * math.sin(t * 7) + f.noise(2))
        return f

    @reg("clipping")
    def _():
        f = Flight(7)
        f.status()
        f.imu(clip_at=5.0, n=3)
        return f

    @reg("clipping_legacy_counter")
    def _():
        f = Flight(8, legacy=True)
        f.status()
        f.imu(clip_at=6.0, n=2)
        return f

    @reg("ekf_rejection")
    def _():
        f = Flight(9)
        f.status()
        f.local()
        f.estimator(rejected=(5.0, 9.0), vibe=True)
        f.imu(vib=lambda t: 12)
        return f

    @reg("estimator_resets")
    def _():
        f = Flight(10)
        f.status()
        f.local(resets=[1.0, 5.0, 5.2, 8.0, 11.0, 11.4])
        return f

    @reg("failure_failsafe")
    def _():
        f = Flight(11)
        f.status(nav=lambda t: 2 if t < 7 else 5, failsafe=lambda t: 7 <= t < 9.5,
                 failure=lambda t: 0 if t < 6 else (1 | 32 | 128))
        f.local()
        return f

    @reg("gps_lost_rc_lost")
    def _():
        f = Flight(12)
        f.status()
        f.local()
        f.gps(fix=lambda t: 3 if t < 6 or t > 10 else 1)
        f.rc(lost=lambda t: 3 < t < 4.5 or t > 5 and t < 5.2 or t < 1)
        return f

    @reg("battery_sag")
    def _():
        f = Flight(13)
        f.status()
        f.local()
        f.battery(cells=6, sag=2.6)
        return f

    @reg("battery_floor_unlogged")
    def _():
        f = Flight(14)
        f.status()
        f.battery(cells=4, floor=3.05, logged_cells=False)
        return f

    @reg("cpu_dropouts_truncated")
    def _():
        f = Flight(15)
        f.status()
        f.cpu(high=(4.0, 8.0))
        f.log.dropout(f.t0 + 3_000_000, 150)
        f.log.dropout(f.t0 + 4_000_000, 450)
        return f.build(truncate=5)

    @reg("ringing_rates")
    def _():
        f = Flight(16, seconds=12.0)
        f.status()
        f.rates(ring=0.5, hz=200)
        return f

    @reg("attitude_error")
    def _():
        f = Flight(17)
        f.status()
        f.attitude(offset=0.0)
        return f

    @reg("attitude_offset_setpoint")
    def _():
        f = Flight(18)
        f.status()
        tp = f.log.topic("vehicle_attitude", ["uint64_t timestamp", "float[4] q"])
        for ts, t in f.ticks(50):
            roll = 0.4 * math.sin(t * 1.1) + 0.5
            tp.add(ts, q=[math.cos(roll / 2), math.sin(roll / 2), 0.0, 0.0])
        sp = f.log.topic("vehicle_attitude_setpoint", ["uint64_t timestamp", "float roll_body", "float pitch_body", "float yaw_body"])
        for ts, t in f.ticks(20):
            sp.add(ts, roll_body=0.0, pitch_body=0.0, yaw_body=0.0)
        return f

    @reg("mag_interference")
    def _():
        f = Flight(19, seconds=16.0)
        f.status()
        f.thrust()
        f.mag(coupled=0.12)
        return f

    @reg("frozen_clock")
    def _():
        f = Flight(20)
        f.status(frozen=True)
        f.local()
        return f

    @reg("no_global_reference")
    def _():
        f = Flight(21)
        f.status()
        f.local(global_ref=False)
        f.waypoints(local_frame=True)
        return f

    @reg("stationary")
    def _():
        f = Flight(22)
        f.status()
        f.local(moving=False)
        f.motors()
        return f

    @reg("many_messages")
    def _():
        f = Flight(23)
        f.status()
        f.messages([(1.0 + i * 0.05, ord(str(i % 8)), f"message {i} äö") for i in range(420)])
        return f

    @reg("wing_air_data")
    def _():
        f = Flight(24, legacy=False)
        f.status(nav=lambda t: 10 if t < 5 else 18)
        f.misc()
        f.local()
        f.gps(legacy=True)
        return f

    @reg("header_only")
    def _():
        f = Flight(25)
        f.meta()
        return f

    @reg("long_decimated")
    def _():
        f = Flight(26, seconds=40.0, hz=50)
        f.status()
        f.local()
        f.attitude()
        f.rates(hz=100, ring=0.4)
        f.imu(vib=lambda t: 10 + (45 if 20 < t < 20.05 else 0))
        f.battery()
        return f

    return out
