"""Synthetic Corvus tlogs, frame by frame, for holding the Rust telemetry review
to ``tlog_review``: the bytes are what ``TlogWriter`` writes (a metadata line,
then raw MAVLink frames), plus the damage a real tail has (garbage, a bad
checksum, an unknown message, a cut last frame)."""
from __future__ import annotations

import json
import math
import random
import struct
from collections.abc import Callable

from pymavlink import mavutil

M = mavutil.mavlink
MAGIC = b"#CORVUS-TLOG "
PX4 = 12
ARDU = 3
ARMED = 128
QUAD = 2
MODE_MISSION = (4 << 16) | (4 << 24)
MODE_POSITION = 3 << 16
MODE_RTL = (4 << 16) | (5 << 24)
MODE_ORBIT = (3 << 16) | (3 << 24)
HEADER = {"product": "Corvus GCS", "version": "9.9.9-test", "conn": "udp:0.0.0.0:14540",
          "format": "mavlink-raw", "started_at": "2026-11-07T10:00:00+00:00"}


class Rec:
    def __init__(self, system: int = 1, v1: bool = False) -> None:
        self.link = M.MAVLink(None, srcSystem=system, srcComponent=1)
        if v1:
            self.link.protocol_marker = 0xFE
        self.frames: list[bytes] = []

    def send(self, msg) -> None:
        self.frames.append(msg.pack(self.link))

    def heartbeat(self, custom: int = MODE_MISSION, armed: bool = True, autopilot: int = PX4, kind: int = QUAD,
                  base_extra: int = 0) -> None:
        self.send(M.MAVLink_heartbeat_message(kind, autopilot, (ARMED if armed else 0) | base_extra, custom, 4, 3))

    def position(self, boot_ms: int, lat: float, lon: float, alt_m: float, vx: float = 5.0, vy: float = 1.0,
                 vz: float = 0.0) -> None:
        self.send(M.MAVLink_global_position_int_message(boot_ms, int(lat * 1e7), int(lon * 1e7), int(alt_m * 1000),
                                                        int(alt_m * 1000 - 30000), int(vx * 100), int(vy * 100),
                                                        int(vz * 100), 0))

    def attitude(self, boot_ms: int, roll: float = 0.0, pitch: float = 0.0, yaw: float = 0.0) -> None:
        self.send(M.MAVLink_attitude_message(boot_ms, roll, pitch, yaw, 0.01 * roll, 0.02, -0.01))

    def target(self, boot_ms: int, roll: float = 0.0, pitch: float = 0.0, yaw: float = 0.0, thrust: float = 0.5) -> None:
        cr, sr = math.cos(roll / 2), math.sin(roll / 2)
        cp, sp = math.cos(pitch / 2), math.sin(pitch / 2)
        cy, sy = math.cos(yaw / 2), math.sin(yaw / 2)
        q = [cr * cp * cy + sr * sp * sy, sr * cp * cy - cr * sp * sy, cr * sp * cy + sr * cp * sy,
             cr * cp * sy - sr * sp * cy]
        self.send(M.MAVLink_attitude_target_message(boot_ms, 0, q, 0.0, 0.0, 0.0, thrust))

    def sys_status(self, volts_mv: int = 16000, load: int = 250, drop: int = 0, enabled: int = 0, health: int = 0,
                   current: int = 1200, remaining: int = 80, errors: int = 0) -> None:
        self.send(M.MAVLink_sys_status_message(enabled, enabled, health, load, volts_mv, current, remaining, drop,
                                               errors, 0, 0, 0, 0))

    def hud(self, throttle: int = 50, airspeed: float = 12.0, climb: float = 0.5) -> None:
        self.send(M.MAVLink_vfr_hud_message(airspeed, 11.0, 90, throttle, 100.0, climb))

    def gps(self, fix: int = 3, sats: int = 14, eph: int = 90, epv: int = 120, lat: float = 48.1, lon: float = 11.5) -> None:
        self.send(M.MAVLink_gps_raw_int_message(0, fix, int(lat * 1e7), int(lon * 1e7), 100000, eph, epv, 0, 0, sats))

    def battery(self, cells_mv: list[int], consumed: int = 100) -> None:
        v = (cells_mv + [65535] * 10)[:10]
        self.send(M.MAVLink_battery_status_message(0, 0, 0, 2500, v, 800, consumed, -1, 70))

    def vibration(self, boot_us: int, vibe: float, clipping: int = 0) -> None:
        self.send(M.MAVLink_vibration_message(boot_us, vibe, vibe * 0.5, vibe * 2, clipping, clipping // 2, 0))

    def rc(self, boot_ms: int, rssi: int = 200, raw: int = 1500) -> None:
        self.send(M.MAVLink_rc_channels_message(boot_ms, 8, raw, raw + 10, 1100, raw - 10, 0, 65535, 1500, 1500,
                                                1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500, rssi))

    def radio(self, rssi: int, remrssi: int, noise: int = 60, rxerrors: int = 0) -> None:
        self.send(M.MAVLink_radio_status_message(rssi, remrssi, 100, noise, rxerrors, 0, 0))

    def estimator(self, flags: int = 0, vel: float = 0.3, pos: float = 0.2, mag: float = 0.1) -> None:
        self.send(M.MAVLink_estimator_status_message(0, flags, vel, pos, 0.0, mag, 0.0, 0.0, 0.0, 0.0))

    def nav(self, alt_error: float = 1.0, xtrack: float = 2.0) -> None:
        self.send(M.MAVLink_nav_controller_output_message(0.0, 0.0, 0, 0, 120, alt_error, 0.0, xtrack))

    def text(self, severity: int, text: str) -> None:
        self.send(M.MAVLink_statustext_message(severity, text.encode("utf-8")[:50]))

    def system_time(self, unix_us: int, boot_ms: int) -> None:
        self.send(M.MAVLink_system_time_message(unix_us, boot_ms))

    def version(self, sw: int = (1 << 24) | (16 << 16) | (2 << 8), board: int = 6000) -> None:
        self.send(M.MAVLink_autopilot_version_message(0, sw, 0, 0, board, b"\0" * 8, b"\0" * 8, b"\0" * 8, 0, 0, 0))

    def raw(self, blob: bytes) -> None:
        self.frames.append(blob)

    def blob(self, meta: dict | None = None, header: bool = True) -> bytes:
        head = MAGIC + json.dumps(HEADER if meta is None else meta).encode() + b"\n" if header else b""
        return head + b"".join(self.frames)


def flight(seconds: int = 40, hz: int = 5, system: int = 1, mode_at: Callable[[float], int] | None = None,
           armed_at: Callable[[float], bool] | None = None, autopilot: int = PX4, boot0: int = 5000) -> Rec:
    """A mission with everything a telemetry link carries; knobs go to the caller."""
    rec = Rec(system)
    rec.system_time(1_700_000_000_000_000, boot0)
    rec.version()
    mode_at = mode_at or (lambda t: MODE_POSITION if t < 10 else MODE_MISSION if t < seconds - 8 else MODE_RTL)
    armed_at = armed_at or (lambda t: 5 <= t < seconds - 3)
    for step in range(seconds * hz):
        t = step / hz
        boot = boot0 + int(step * 1000 / hz)
        rec.position(boot, 48.1 + 4e-4 * math.sin(t * 0.2), 11.5 + 5e-4 * t / seconds, 100 + 20 * math.sin(t * 0.3),
                     vx=8 * math.cos(t * 0.2), vy=2, vz=-0.5 * math.cos(t * 0.3))
        rec.attitude(boot, roll=0.2 * math.sin(t), pitch=0.1 * math.sin(t * 0.7), yaw=0.3 * t)
        rec.target(boot, roll=0.2 * math.sin(t - 0.1), pitch=0.1 * math.sin(t * 0.7 - 0.1), yaw=0.3 * t, thrust=0.5)
        if step % hz == 0:
            rec.heartbeat(mode_at(t), armed_at(t), autopilot)
            rec.sys_status(volts_mv=16200 - int(t * 10), load=250 + step % 7, remaining=90 - step // hz)
            rec.gps(sats=14 - (step // hz) % 3)
            rec.hud(throttle=int(50 + 30 * math.sin(t)))
            rec.battery([4050, 4040, 4045, 4048])
            rec.rc(boot, rssi=200 - step % 11)
            rec.nav(alt_error=0.5 * math.sin(t), xtrack=1.5)
            rec.estimator(flags=0, vel=0.3 + 0.01 * (step % 4))
    rec.heartbeat(MODE_POSITION, False, autopilot)
    return rec


def build_tlogs() -> dict[str, Callable[[], bytes]]:
    out: dict[str, Callable[[], bytes]] = {}

    def reg(name: str):
        def deco(fn: Callable[[], bytes]):
            out[name] = fn
            return fn
        return deco

    @reg("clean")
    def _():
        r = flight()
        r.text(6, "Armed by RC")
        r.text(4, "Low battery")
        return r.blob()

    @reg("ardupilot_modes")
    def _():
        r = flight(autopilot=ARDU, mode_at=lambda t: 5 if t < 12 else 9 if t < 30 else 6)
        return r.blob()

    @reg("px4_mode_variants")
    def _():
        order = [MODE_POSITION, MODE_ORBIT, MODE_MISSION, MODE_RTL, (1 << 16), (2 << 16), (7 << 16), 0xDEAD0000]
        r = flight(seconds=48, mode_at=lambda t: order[int(t // 6) % len(order)])
        return r.blob()

    @reg("two_systems")
    def _():
        r = flight()
        gcs = Rec(system=255)
        for _ in range(60):
            gcs.heartbeat(0, False, 8, 6)
        other = flight(seconds=20, system=2, boot0=900)
        return r.blob() + b"".join(gcs.frames) + b"".join(other.frames)

    @reg("no_timeline")
    def _():
        r = Rec()
        for _ in range(30):
            r.heartbeat()
            r.sys_status()
            r.text(6, "hello")
        return r.blob()

    @reg("reboot_unarmed")
    def _():
        r = Rec()
        for step in range(60):
            r.position(10_000 + step * 200, 48.1, 11.5, 100.0)
            r.heartbeat(MODE_POSITION, False)
        for step in range(60):
            r.position(300 + step * 200, 48.1, 11.5, 100.0)
            r.heartbeat(MODE_POSITION, False)
        return r.blob()

    @reg("reboot_armed")
    def _():
        r = Rec()
        for step in range(60):
            r.position(10_000 + step * 200, 48.1, 11.5, 100.0)
            r.heartbeat(MODE_MISSION, True)
        for step in range(60):
            r.position(300 + step * 200, 48.1, 11.5, 100.0)
            r.heartbeat(MODE_MISSION, True)
        return r.blob()

    @reg("silence_armed")
    def _():
        r = Rec()
        for step in range(200):
            boot = 1000 + step * 200 + (15000 if step >= 100 else 0)
            r.position(boot, 48.1 + step * 1e-5, 11.5, 100.0)
            if step % 5 == 0:
                r.heartbeat(MODE_MISSION, 20 <= step < 180)
        r.heartbeat(MODE_MISSION, False)
        return r.blob()

    @reg("slow_link")
    def _():
        r = Rec()
        for step in range(40):
            r.position(1000 + step * 3000, 48.1 + step * 1e-5, 11.5, 100.0)
            r.heartbeat(MODE_MISSION, True)
        r.heartbeat(MODE_POSITION, False)
        return r.blob()

    @reg("still_armed")
    def _():
        r = flight(armed_at=lambda t: t >= 5)
        return r.blob()

    @reg("unhealthy")
    def _():
        r = Rec()
        enabled = (1 << 0) | (1 << 1) | (1 << 5) | (1 << 28) | (1 << 30) | (1 << 3) | (1 << 9) | (1 << 15)
        for step in range(150):
            t = step * 0.2
            r.position(1000 + step * 200, 48.1, 11.5, 100.0)
            health = enabled if step < 60 else enabled & ~((1 << 5) | (1 << 15) | (1 << 30) | (1 << 1) | (1 << 3))
            r.sys_status(enabled=enabled, health=health & ~(1 << 28))
            r.heartbeat(MODE_MISSION, 10 <= step < 140)
        return r.blob()

    @reg("estimator_flags")
    def _():
        r = Rec()
        for step in range(100):
            r.position(1000 + step * 200, 48.1, 11.5, 100.0)
            r.estimator(flags=(1 << 10) if step in range(5, 8) or step in range(50, 53) else
                        ((1 << 11) if 60 <= step < 70 else 0))
            r.heartbeat(MODE_POSITION, 30 <= step < 90)
        return r.blob()

    @reg("attitude_error")
    def _():
        r = Rec()
        for step in range(200):
            boot = 1000 + step * 100
            r.attitude(boot, roll=math.radians(30), pitch=math.radians(12) * math.sin(step / 7))
            r.target(boot, roll=0.0, pitch=0.0)
            if step % 10 == 0:
                r.heartbeat(MODE_POSITION, 3 <= step // 10 < 18)
        return r.blob()

    @reg("vibration_throttle")
    def _():
        r = Rec()
        for step in range(300):
            boot = 1000 + step * 100
            r.position(boot, 48.1, 11.5, 100.0)
            thr = int(50 + 40 * math.sin(step / 11))
            r.hud(throttle=thr)
            r.vibration(boot * 1000, vibe=20 + 0.6 * thr, clipping=step * 4)
            if step % 10 == 0:
                r.heartbeat(MODE_POSITION, 2 <= step // 10 < 28)
        return r.blob()

    @reg("link_trouble")
    def _():
        r = Rec()
        for step in range(150):
            boot = 1000 + step * 200
            r.position(boot, 48.1, 11.5, 100.0)
            r.radio(rssi=150, remrssi=max(20, 120 - step), noise=60 + step % 5, rxerrors=step)
            r.sys_status(drop=800 if 40 < step < 80 else 150)
            r.gps(fix=3 if step < 60 or step > 100 else 2)
            if step % 5 == 0:
                r.heartbeat(MODE_MISSION, 5 <= step // 5 < 28)
        return r.blob()

    @reg("battery_low")
    def _():
        r = Rec()
        for step in range(150):
            r.position(1000 + step * 200, 48.1, 11.5, 100.0)
            r.sys_status(volts_mv=12400 if step > 40 else 16800)
            r.battery([3100, 3100, 3100, 3100])
            if step % 5 == 0:
                r.heartbeat(MODE_MISSION, 2 <= step // 5 < 28)
        return r.blob()

    @reg("many_messages")
    def _():
        r = Rec()
        for step in range(120):
            r.position(1000 + step * 200, 48.1, 11.5, 100.0)
            if step % 5 == 0:
                r.heartbeat(MODE_MISSION, 10 <= step < 100)
        for i in range(420):
            r.text(i % 8, f"msg {i} ä ok")
            if i % 7 == 0:
                r.position(1000 + 24000 + i * 50, 48.1, 11.5, 100.0)
        return r.blob()

    @reg("damaged_tail")
    def _():
        rng = random.Random(7)
        r = flight(seconds=12)
        frames = list(r.frames)
        mixed: list[bytes] = []
        for i, fr in enumerate(frames):
            mixed.append(fr)
            if i % 17 == 3:
                mixed.append(bytes(rng.randrange(256) for _ in range(rng.randrange(1, 12))))
            if i % 23 == 5:
                bad = bytearray(fr)
                bad[-1] ^= 0xFF
                mixed.append(bytes(bad))
            if i % 31 == 7:
                unknown = bytearray(b"\xfd\x02\x00\x00\x00\x01\x01") + bytes([0x77, 0x77, 0x00]) + b"\x01\x02" + b"\x12\x34"
                mixed.append(bytes(unknown))
        blob = HEADER_BYTES() + b"".join(mixed)
        return blob + frames[5][:9]

    @reg("v1_frames")
    def _():
        r = Rec(v1=True)
        for step in range(80):
            r.position(1000 + step * 250, 48.1, 11.5, 100.0)
            r.attitude(1000 + step * 250, roll=0.1)
            if step % 4 == 0:
                r.heartbeat(MODE_POSITION, 4 <= step < 70)
        return r.blob()

    @reg("no_header")
    def _():
        return flight(seconds=10).blob(header=False)

    @reg("bad_header")
    def _():
        r = flight(seconds=10)
        return MAGIC + b"{not json\n" + b"".join(r.frames)

    @reg("array_header")
    def _():
        r = flight(seconds=10)
        return MAGIC + b"[1, 2]\n" + b"".join(r.frames)

    @reg("header_only")
    def _():
        return MAGIC + json.dumps(HEADER).encode() + b"\n"

    @reg("header_no_newline")
    def _():
        return MAGIC + json.dumps(HEADER).encode()

    @reg("garbage_only")
    def _():
        return MAGIC + b"{}\n" + bytes(range(1, 200)) * 3

    @reg("gps_only_track")
    def _():
        r = Rec()
        for step in range(100):
            r.gps(lat=47.0 + step * 1e-4, lon=8.0 - step * 1e-4)
            r.hud()
            r.attitude(1000 + step * 200)
            if step % 5 == 0:
                r.heartbeat(MODE_POSITION, 3 <= step < 90, kind=1)
        return r.blob()

    return out


def HEADER_BYTES() -> bytes:
    return MAGIC + json.dumps(HEADER).encode() + b"\n"
