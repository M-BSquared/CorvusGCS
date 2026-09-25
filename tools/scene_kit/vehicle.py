"""A PX4 autopilot, simulated well enough to photograph.

Every screenshot in the README is the real interface driven by real MAVLink.
This is the other end of that wire: a process that speaks the protocol PX4
speaks — heartbeats and telemetry outbound, the parameter protocol, the command
protocol, the calibration transcript and the log protocol inbound — so Corvus
parses it with the same code a flight uses. Nothing here reaches into the
application: if a page renders, it renders because the bytes said so.

What it is not: a flight dynamics model. Attitude comes from the path's
geometry (see :mod:`flight`), not from forces, and the vehicle will fly through
a hill. The point is a *picture* that is true to the protocol and plausible to
someone who flies, not a simulator — PX4 SITL exists and is better at that.

Scope, deliberately:

* **Answers** the parameter protocol (full list burst and single reads), the
  command protocol, the mission upload and download handshakes, and the log
  list/download.
* **Streams** position, attitude, HUD, GPS, battery, RC, vibration and the
  status text a flight produces.
* **Refuses nothing** it should refuse: an arm command while a calibration runs
  is rejected exactly as PX4 rejects it, because a screenshot of a GCS that
  shows an impossible state is a worse artefact than no screenshot.

The calibration transcripts follow PX4's own wording (v1.16-v1.18
``calibration_routines.cpp``) as far as the side names, the ordering and the
progress reports go — that vocabulary is what ``src/js/calib-protocol.js``
parses, and it is checked by ``tests/test_calib_protocol.js`` against real
transcripts.
"""
from __future__ import annotations

import json
import lzma
import math
import os
import random
import struct
import threading
import time
import zlib
from dataclasses import dataclass, field
from typing import Any, Callable

# PX4 speaks MAVLink 2; so must we, or signing, the long message ids and the
# 18-channel RC message are all off the table. Set before pymavlink is imported,
# because that is when it decides.
os.environ.setdefault("MAVLINK20", "1")

from pymavlink import mavutil  # noqa: E402  (must follow the MAVLINK20 line)
from pymavlink.dialects.v20 import common as mavlink2  # noqa: E402

from . import flight as flight_mod  # noqa: E402
from . import params as params_mod  # noqa: E402

# PX4's custom_mode layout: main mode in bits 16-23, AUTO sub-mode in 24-31.
# Mirrors corvus/mavlink_bridge.py's decoder, which is what reads it back.
MAIN_MODES = {
    "MANUAL": 1, "ALTCTL": 2, "POSCTL": 3, "AUTO": 4,
    "ACRO": 5, "OFFBOARD": 6, "STABILIZED": 7, "RATTITUDE": 8,
}
AUTO_SUBMODES = {
    "READY": 1, "TAKEOFF": 2, "LOITER": 3, "MISSION": 4,
    "RTL": 5, "LAND": 6, "RTGS": 7, "FOLLOWME": 8, "PRECLAND": 9,
}

# The six accelerometer positions, in the order PX4 asks for them, spelled the
# way PX4 spells them: it names the side facing DOWN.
ACCEL_SIDES = ("down", "front", "left", "right", "up", "back")

# MAV_SYS_STATUS_PREARM_CHECK — the bit Corvus reads for "ready to arm".
PREARM_BIT = 1 << 27

_SENSOR_BITS = (
    mavlink2.MAV_SYS_STATUS_SENSOR_3D_GYRO
    | mavlink2.MAV_SYS_STATUS_SENSOR_3D_ACCEL
    | mavlink2.MAV_SYS_STATUS_SENSOR_3D_MAG
    | mavlink2.MAV_SYS_STATUS_SENSOR_ABSOLUTE_PRESSURE
    | mavlink2.MAV_SYS_STATUS_SENSOR_GPS
    | mavlink2.MAV_SYS_STATUS_SENSOR_ANGULAR_RATE_CONTROL
    | mavlink2.MAV_SYS_STATUS_SENSOR_ATTITUDE_STABILIZATION
    | mavlink2.MAV_SYS_STATUS_SENSOR_YAW_POSITION
    | mavlink2.MAV_SYS_STATUS_SENSOR_Z_ALTITUDE_CONTROL
    | mavlink2.MAV_SYS_STATUS_SENSOR_XY_POSITION_CONTROL
    | mavlink2.MAV_SYS_STATUS_SENSOR_MOTOR_OUTPUTS
    | mavlink2.MAV_SYS_STATUS_SENSOR_RC_RECEIVER
    | PREARM_BIT
)


@dataclass
class RcState:
    """What the transmitter is sending.

    Sticks are fractions of full travel (-1..1, throttle 0..1); switches are a
    position index. Held as intent rather than as microseconds so a scene can
    say "mode switch in the middle" without doing PWM arithmetic.
    """

    roll: float = 0.0
    pitch: float = 0.0
    throttle: float = 0.55
    yaw: float = 0.0
    # channel -> (position, of_positions) for the six-switch layout the drawn
    # transmitter models, plus the two knobs.
    switches: dict[int, tuple[int, int]] = field(default_factory=lambda: {
        5: (1, 3),    # mode switch, middle position
        6: (0, 2),    # return switch, off
        7: (0, 2),    # arm switch, off
        8: (0, 2),    # kill switch, off
        9: (1, 3),
        10: (0, 2),
    })
    knobs: dict[int, float] = field(default_factory=lambda: {11: 0.5, 12: 0.25})
    rssi: int = 96
    present: bool = True
    # A live transmitter is never perfectly still; this is the jitter, in
    # microseconds, that makes the channel bars on the Radio Control page look
    # measured rather than drawn.
    jitter_us: float = 2.5

    def pulses(self, channels: int = 16, rng: random.Random | None = None,
               wobble: float = 0.0) -> list[int]:
        """The channel values, in microseconds."""
        rng = rng or random.Random()

        def pwm(fraction: float) -> int:
            value = 1500 + fraction * 500 + rng.gauss(0.0, self.jitter_us)
            return int(max(1000, min(2000, round(value))))

        out = [1500] * channels
        if channels >= 4:
            out[0] = pwm(self.roll + wobble * 0.15)
            out[1] = pwm(self.pitch + wobble * 0.1)
            out[2] = pwm(self.throttle * 2 - 1)
            out[3] = pwm(self.yaw)
        for chan, (index, count) in self.switches.items():
            if chan <= channels:
                span = max(1, count - 1)
                out[chan - 1] = pwm(-1 + 2 * index / span)
        for chan, value in self.knobs.items():
            if chan <= channels:
                out[chan - 1] = pwm(value * 2 - 1)
        return out


@dataclass
class LogEntry:
    """One log on the vehicle's card, as the Analysis page lists them."""

    id: int
    utc: int          # unix seconds
    size: int         # bytes
    # The file itself, when the scene has one (see :mod:`flight_log`). Without
    # it a download still runs and completes, over deterministic filler.
    data: bytes | None = None


@dataclass
class VehicleOptions:
    """Everything about the simulated aircraft that a scene chooses."""

    params: dict[str, Any] = field(default_factory=dict)
    model: flight_mod.FlightModel | None = None
    armed: bool = True
    mode: str = "MISSION"
    vehicle_type: int = mavlink2.MAV_TYPE_QUADROTOR
    px4_version: tuple[int, int, int] = (1, 18, 0)
    git_hash: str = "c9b8a7f6e5d4c3b2a1908f7e6d5c4b3a29180706"
    board_vendor: int = 0x3162          # Holybro
    board_product: int = 0x0047         # Pixhawk 6X
    satellites: int = 18
    rc: RcState = field(default_factory=RcState)
    rc_channels: int = 16
    logs: tuple[LogEntry, ...] = ()
    chatter: tuple[tuple[float, int, str], ...] = ()
    follow_missions: bool = True
    seed: int = 20260916

    # Calibration staging. ``calibration_pause_after`` is the number of sides
    # to finish before the transcript stops and waits — the frame the
    # calibration screenshot is taken on. None runs it to completion.
    calibration_pause_after: int | None = None
    calibration_step_seconds: float = 2.2

    # Which streams to send, and how often (Hz). A scene that wants a quiet
    # console or a slow link turns rates down here rather than in the code.
    rates: dict[str, float] = field(default_factory=lambda: {
        "HEARTBEAT": 1.0,
        "GLOBAL_POSITION_INT": 10.0,
        "ATTITUDE": 20.0,
        "VFR_HUD": 10.0,
        "GPS_RAW_INT": 5.0,
        "SYS_STATUS": 2.0,
        "BATTERY_STATUS": 2.0,
        "EXTENDED_SYS_STATE": 2.0,
        "RC_CHANNELS": 10.0,
        "VIBRATION": 2.0,
        "SYSTEM_TIME": 1.0,
        "HOME_POSITION": 0.2,
        "MISSION_CURRENT": 1.0,
        "ALTITUDE": 0.0,
    })


class SimVehicle:
    """A PX4 autopilot on a UDP socket.

    Two threads: one sends the streams on their schedule, one answers whatever
    the ground station asks. Both are daemons and both stop on :meth:`stop`,
    which is what makes this safe to drive from a screenshot script that may be
    interrupted at any moment.
    """

    def __init__(self, connection: str = "udpout:127.0.0.1:14550",
                 options: VehicleOptions | None = None,
                 on_event: Callable[[str], None] | None = None) -> None:
        self.opts = options or VehicleOptions()
        self.connection_string = connection
        self._on_event = on_event or (lambda _text: None)
        self._rng = random.Random(self.opts.seed)

        self.model = self.opts.model or flight_mod.FlightModel(flight_mod.orbit())
        self.params: dict[str, Any] = dict(self.opts.params)
        self.armed = self.opts.armed
        self.mode = self.opts.mode
        self.rc = self.opts.rc

        self._conn: Any = None
        self._t0 = 0.0
        self._stop = threading.Event()
        self._threads: list[threading.Thread] = []
        self._send_lock = threading.Lock()
        self._calibration: _Calibration | None = None
        self.calibration_pause_after = self.opts.calibration_pause_after
        self.calibration_step_seconds = self.opts.calibration_step_seconds
        self._mission: list[tuple[float, float, float]] = []
        self._mission_expect = 0
        # Every uploaded item as it arrived, so a download gives it back
        # unchanged, and the item number of each point the flown path visits.
        self._mission_items: list[tuple[Any, ...]] = []
        self._mission_point_seq: list[int] = []
        # Extra one-off messages the RX thread hands to the TX thread, so
        # everything on the wire is written by one thread and cannot interleave.
        self._pending: list[Callable[[], None]] = []
        # MAVLink FTP: the one file open, and the parameter metadata built on
        # first request from the table the vehicle booted with.
        self._ftp_open: bytes | None = None
        self._param_metadata: bytes | None = None

    # -- lifecycle --------------------------------------------------------

    def start(self) -> None:
        self._conn = mavutil.mavlink_connection(
            self.connection_string, source_system=1, source_component=1,
            dialect="common", autoreconnect=True,
        )
        self._t0 = time.monotonic()
        self._stop.clear()
        for target in (self._tx_loop, self._rx_loop):
            thread = threading.Thread(target=target, name=target.__name__, daemon=True)
            thread.start()
            self._threads.append(thread)
        self._on_event(f"vehicle up on {self.connection_string}")

    def stop(self) -> None:
        self._stop.set()
        for thread in self._threads:
            thread.join(timeout=2.0)
        self._threads.clear()
        conn = self._conn
        self._conn = None
        if conn is not None:
            try:
                conn.close()
            except Exception:  # noqa: BLE001 - teardown must not raise
                pass

    def __enter__(self) -> "SimVehicle":
        self.start()
        return self

    def __exit__(self, *_exc: Any) -> None:
        self.stop()

    # -- time -------------------------------------------------------------

    @property
    def elapsed(self) -> float:
        """Seconds since the vehicle came up — the flight clock."""
        return time.monotonic() - self._t0

    def _boot_ms(self) -> int:
        return int(self.elapsed * 1000) & 0xFFFFFFFF

    # -- sending ----------------------------------------------------------

    def _send(self, fn: Callable[..., None], *args: Any, **kwargs: Any) -> None:
        """Send one message, swallowing a link that has gone away."""
        conn = self._conn
        if conn is None:
            return
        try:
            with self._send_lock:
                fn(*args, **kwargs)
        except Exception as exc:  # noqa: BLE001 - a closed socket is not an error here
            self._on_event(f"send failed: {exc}")

    def statustext(self, text: str, severity: int = mavlink2.MAV_SEVERITY_INFO) -> None:
        """Emit a STATUSTEXT — the console's content, and the calibration's."""
        conn = self._conn
        if conn is None:
            return
        payload = text.encode("utf-8")[:50]
        self._send(conn.mav.statustext_send, severity, payload)

    # -- the stream -------------------------------------------------------

    def _tx_loop(self) -> None:
        next_due: dict[str, float] = {}
        senders = {
            "HEARTBEAT": self._send_heartbeat,
            "GLOBAL_POSITION_INT": self._send_position,
            "ATTITUDE": self._send_attitude,
            "VFR_HUD": self._send_hud,
            "GPS_RAW_INT": self._send_gps,
            "SYS_STATUS": self._send_sys_status,
            "BATTERY_STATUS": self._send_battery,
            "EXTENDED_SYS_STATE": self._send_extended_state,
            "RC_CHANNELS": self._send_rc,
            "VIBRATION": self._send_vibration,
            "SYSTEM_TIME": self._send_system_time,
            "HOME_POSITION": self._send_home,
            "MISSION_CURRENT": self._send_mission_current,
        }
        chatter = sorted(self.opts.chatter, key=lambda c: c[0])
        chatter_at = 0

        while not self._stop.is_set():
            now = self.elapsed
            # The schedule runs on its own clock, not on the flight's: adopting
            # an uploaded mission restarts the flight clock, and a schedule
            # keyed to it held every stream (the heartbeat too) until the new
            # flight caught up with the old one, so the link dropped.
            tick = time.monotonic()
            for name, sender in senders.items():
                hz = float(self.opts.rates.get(name, 0.0) or 0.0)
                if hz <= 0:
                    continue
                if tick >= next_due.get(name, 0.0):
                    next_due[name] = tick + 1.0 / hz
                    sender()
            # Scripted console traffic, in flight order.
            while chatter_at < len(chatter) and chatter[chatter_at][0] <= now:
                _at, severity, text = chatter[chatter_at]
                self.statustext(text, severity)
                chatter_at += 1
            # Work the RX thread handed over.
            while self._pending:
                try:
                    self._pending.pop(0)()
                except Exception as exc:  # noqa: BLE001
                    self._on_event(f"deferred send failed: {exc}")
            self._stop.wait(0.01)

    def _fix(self) -> flight_mod.Fix:
        return self.model.sample(self.elapsed)

    def _custom_mode(self) -> int:
        mode = (self.mode or "").upper()
        if mode in MAIN_MODES:
            return MAIN_MODES[mode] << 16
        if mode in AUTO_SUBMODES:
            return (MAIN_MODES["AUTO"] << 16) | (AUTO_SUBMODES[mode] << 24)
        if mode in ("HOLD", "LOITER"):
            return (MAIN_MODES["AUTO"] << 16) | (AUTO_SUBMODES["LOITER"] << 24)
        return MAIN_MODES["POSCTL"] << 16

    def _send_heartbeat(self) -> None:
        base = mavlink2.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
        if self.armed:
            base |= mavlink2.MAV_MODE_FLAG_SAFETY_ARMED
        base |= (mavlink2.MAV_MODE_FLAG_STABILIZE_ENABLED
                 | mavlink2.MAV_MODE_FLAG_GUIDED_ENABLED
                 | mavlink2.MAV_MODE_FLAG_AUTO_ENABLED)
        self._send(
            self._conn.mav.heartbeat_send,
            self.opts.vehicle_type, mavlink2.MAV_AUTOPILOT_PX4,
            base, self._custom_mode(),
            mavlink2.MAV_STATE_ACTIVE if self.armed else mavlink2.MAV_STATE_STANDBY,
        )

    def _send_position(self) -> None:
        fix = self._fix()
        amsl = fix.alt_agl + 570.0      # Neubiberg sits about 570 m up
        self._send(
            self._conn.mav.global_position_int_send,
            self._boot_ms(),
            int(fix.lat * 1e7), int(fix.lon * 1e7),
            int(amsl * 1000), int(fix.alt_agl * 1000),
            int(fix.vx * 100), int(fix.vy * 100), int(fix.vz * 100),
            int(fix.heading * 100) % 36000,
        )

    def _send_attitude(self) -> None:
        fix = self._fix()
        # Rates from the difference across a tenth of a second: a HUD whose
        # attitude moves but whose rates read zero is visibly wrong on the
        # tuning plots, which draw both.
        ahead = self.model.sample(self.elapsed + 0.1)
        # A gyro is never exactly still; without its noise a steady axis draws
        # as a ruled line on the tuning charts.
        noise = self._rng.gauss
        self._send(
            self._conn.mav.attitude_send, self._boot_ms(),
            fix.roll, fix.pitch, fix.yaw,
            (ahead.roll - fix.roll) * 10.0 + noise(0.0, 0.004),
            (ahead.pitch - fix.pitch) * 10.0 + noise(0.0, 0.004),
            _wrap_pi(ahead.yaw - fix.yaw) * 10.0 + noise(0.0, 0.004),
        )

    def _send_hud(self) -> None:
        fix = self._fix()
        throttle = int(max(0, min(100, 42 + fix.ground_speed * 2 + fix.climb * 6)))
        self._send(
            self._conn.mav.vfr_hud_send,
            fix.ground_speed * 1.04, fix.ground_speed, int(fix.heading) % 360,
            throttle, fix.alt_agl + 570.0, fix.climb,
        )

    def _send_gps(self) -> None:
        fix = self._fix()
        self._send(
            self._conn.mav.gps_raw_int_send,
            int(time.time() * 1e6), mavlink2.GPS_FIX_TYPE_RTK_FIXED,
            int(fix.lat * 1e7), int(fix.lon * 1e7), int((fix.alt_agl + 570.0) * 1000),
            int(60 + self._rng.random() * 20),      # eph, cm
            int(80 + self._rng.random() * 30),      # epv, cm
            int(fix.ground_speed * 100), int(fix.heading * 100) % 36000,
            self.opts.satellites,
        )

    def _send_sys_status(self) -> None:
        volts, amps, remaining = self.model.battery(self.elapsed)
        health = _SENSOR_BITS
        if self._calibration is not None:
            health &= ~PREARM_BIT          # a calibration in progress refuses arming
        self._send(
            self._conn.mav.sys_status_send,
            _SENSOR_BITS, _SENSOR_BITS, health,
            int(180 + self._rng.random() * 60),     # load, 0.1 %
            int(volts * 1000), int(amps * 100), remaining,
            0, 0, 0, 0, 0, 0,
        )

    def _send_battery(self) -> None:
        volts, amps, remaining = self.model.battery(self.elapsed)
        cells = max(1, int(self.params.get("BAT1_N_CELLS", 6)))
        per_cell = int(volts / cells * 1000)
        voltages = [per_cell] * cells + [65535] * (10 - cells)
        consumed = int(amps * self.elapsed / 3.6)   # mAh
        self._send(
            self._conn.mav.battery_status_send,
            0, mavlink2.MAV_BATTERY_FUNCTION_ALL, mavlink2.MAV_BATTERY_TYPE_LIPO,
            int(2850 + self._rng.random() * 100),   # centi-degrees C
            voltages, int(amps * 100), consumed, -1, remaining,
        )

    def _send_extended_state(self) -> None:
        fix = self._fix()
        landed = (mavlink2.MAV_LANDED_STATE_ON_GROUND if fix.phase == "landed"
                  else mavlink2.MAV_LANDED_STATE_TAKEOFF if fix.phase == "takeoff"
                  else mavlink2.MAV_LANDED_STATE_IN_AIR)
        self._send(self._conn.mav.extended_sys_state_send,
                   mavlink2.MAV_VTOL_STATE_UNDEFINED, landed)

    def _send_rc(self) -> None:
        if not self.rc.present:
            return
        channels = max(4, min(18, self.opts.rc_channels))
        # The sticks breathe with the flight: the pilot is holding an aircraft
        # in the air, not a trim tab.
        wobble = self._fix().roll
        pulses = self.rc.pulses(channels, self._rng, wobble)
        pulses += [65535] * (18 - len(pulses))
        self._send(self._conn.mav.rc_channels_send,
                   self._boot_ms(), channels, *pulses[:18], self.rc.rssi)

    def _send_vibration(self) -> None:
        base = 2.4 + abs(self._fix().ground_speed) * 0.08
        self._send(
            self._conn.mav.vibration_send, int(time.time() * 1e6),
            base + self._rng.random() * 0.4,
            base + self._rng.random() * 0.4,
            base * 1.3 + self._rng.random() * 0.5,
            0, 0, 0,
        )

    def _send_system_time(self) -> None:
        self._send(self._conn.mav.system_time_send,
                   int(time.time() * 1e6), self._boot_ms())

    def _send_home(self) -> None:
        home = self.model.home
        self._send(
            self._conn.mav.home_position_send,
            int(home[0] * 1e7), int(home[1] * 1e7), int(570.0 * 1000),
            0.0, 0.0, 0.0, [1.0, 0.0, 0.0, 0.0], 0.0, 0.0, 0.0,
        )

    def _send_autopilot_version(self) -> None:
        major, minor, patch = self.opts.px4_version
        version = (major << 24) | (minor << 16) | (patch << 8) | 255
        custom = bytes.fromhex(self.opts.git_hash[:16])
        self._send(
            self._conn.mav.autopilot_version_send,
            (mavlink2.MAV_PROTOCOL_CAPABILITY_MISSION_INT
             | mavlink2.MAV_PROTOCOL_CAPABILITY_PARAM_FLOAT
             | mavlink2.MAV_PROTOCOL_CAPABILITY_COMMAND_INT
             | mavlink2.MAV_PROTOCOL_CAPABILITY_MAVLINK2),
            version, version, 0, 0,
            custom, custom, custom,
            self.opts.board_vendor, self.opts.board_product,
            0x0123456789ABCDEF,
        )

    # -- answering --------------------------------------------------------

    def _rx_loop(self) -> None:
        while not self._stop.is_set():
            conn = self._conn
            if conn is None:
                break
            try:
                msg = conn.recv_match(blocking=True, timeout=0.2)
            except Exception:  # noqa: BLE001 - a closed socket during teardown
                continue
            if msg is None:
                continue
            try:
                self._handle(msg)
            except Exception as exc:  # noqa: BLE001 - one bad frame is not fatal
                self._on_event(f"handler error on {msg.get_type()}: {exc}")

    def _handle(self, msg: Any) -> None:
        kind = msg.get_type()
        if kind == "PARAM_REQUEST_LIST":
            self._burst_params()
        elif kind == "PARAM_REQUEST_READ":
            self._answer_param_read(msg)
        elif kind == "PARAM_SET":
            self._apply_param_set(msg)
        elif kind in ("COMMAND_LONG", "COMMAND_INT"):
            self._handle_command(msg)
        elif kind == "MISSION_COUNT":
            self._begin_mission_upload(msg)
        elif kind == "MISSION_ITEM_INT":
            self._collect_mission_item(msg)
        elif kind == "MISSION_REQUEST_LIST":
            self._send(self._conn.mav.mission_count_send,
                       msg.get_srcSystem(), msg.get_srcComponent(), len(self._mission_items))
        elif kind in ("MISSION_REQUEST_INT", "MISSION_REQUEST"):
            self._answer_mission_request(msg)
        elif kind == "LOG_REQUEST_LIST":
            self._answer_log_list(msg)
        elif kind == "LOG_REQUEST_DATA":
            self._answer_log_data(msg)
        elif kind == "LOG_ERASE":
            self._on_event("log erase requested")
        elif kind == "FILE_TRANSFER_PROTOCOL":
            self._answer_ftp(msg)
        elif kind == "SET_MODE":
            self.mode = _mode_name(getattr(msg, "custom_mode", 0))
            self._on_event(f"mode -> {self.mode}")

    # -- parameters -------------------------------------------------------

    def _param_wire(self, name: str) -> tuple[bytes, float, int]:
        """(id, value, MAV_PARAM_TYPE) for *name*, typed the way Python has it.

        Integers go out the way PX4 sends them: the int32's four bytes copied
        into the float field, not the number cast to a float. Casting is what
        ArduPilot does, and a simulator that cast hid for a long time that
        Corvus read every PX4 integer parameter wrong.
        """
        value = self.params[name]
        if isinstance(value, bool):
            value = int(value)
        if isinstance(value, int):
            bytewise = struct.unpack("<f", struct.pack("<i", value))[0]
            return name.encode("ascii")[:16], bytewise, mavlink2.MAV_PARAM_TYPE_INT32
        return name.encode("ascii")[:16], float(value), mavlink2.MAV_PARAM_TYPE_REAL32

    def _send_param(self, name: str, index: int) -> None:
        ident, value, ptype = self._param_wire(name)
        self._send(self._conn.mav.param_value_send,
                   ident, value, ptype, len(self.params), index)

    def _burst_params(self) -> None:
        """Answer PARAM_REQUEST_LIST the way PX4 does: all of them, in order.

        Sent from a worker rather than inline so a 1200-parameter table cannot
        stall the receive loop — which is exactly the shape of the real thing,
        and the reason the download has a progress bar at all.
        """
        names = list(self.params)
        self._on_event(f"parameter list requested ({len(names)})")

        def worker() -> None:
            for index, name in enumerate(names):
                if self._stop.is_set():
                    return
                self._send_param(name, index)
                # ~1000 params/s: fast enough not to bore a screenshot, slow
                # enough that the progress bar exists to be photographed.
                time.sleep(0.001)
            self._on_event("parameter list sent")

        threading.Thread(target=worker, name="param-burst", daemon=True).start()

    def _answer_param_read(self, msg: Any) -> None:
        name = _param_id(msg)
        index = int(getattr(msg, "param_index", -1))
        if name and name in self.params:
            self._send_param(name, list(self.params).index(name))
            return
        if 0 <= index < len(self.params):
            self._send_param(list(self.params)[index], index)
            return
        # A name this firmware does not have goes unanswered, which is what a
        # real board does and what the pages' version tolerance is built on.

    def _apply_param_set(self, msg: Any) -> None:
        name = _param_id(msg)
        if not name:
            return
        if self.armed and name.startswith(("CA_", "SYS_", "PWM_")):
            # PX4 refuses a geometry change in the air. Corvus shows a refused
            # write by snapping the control back, and that is worth being able
            # to photograph.
            self._on_event(f"refused {name} while armed")
            return
        value = float(msg.param_value)
        if isinstance(self.params.get(name), int):
            self.params[name] = struct.unpack("<i", struct.pack("<f", value))[0]
        else:
            self.params[name] = value
        self._send_param(name, list(self.params).index(name))
        self._on_event(f"{name} = {self.params[name]}")

    # -- MAVLink FTP ------------------------------------------------------

    _FTP_HEADER = struct.Struct("<HBBBBBBI")

    def _ftp_file(self, path: str) -> bytes | None:
        """The files this autopilot serves: PX4's parameter metadata, compressed."""
        if path.lstrip("/") != "etc/extras/parameters.json.xz":
            return None
        if self._param_metadata is None:
            doc = params_mod.px4_metadata(dict(self.opts.params))
            self._param_metadata = lzma.compress(
                json.dumps(doc).encode("utf-8"), format=lzma.FORMAT_XZ)
        return self._param_metadata

    def _ftp_reply(self, msg: Any, seq: int, opcode: int, req_opcode: int, *,
                   data: bytes = b"", offset: int = 0, burst_complete: bool = False) -> None:
        header = self._FTP_HEADER.pack(seq & 0xFFFF, 0, opcode, len(data), req_opcode,
                                       int(burst_complete), 0, offset)
        payload = list((header + data).ljust(251, b"\x00"))
        self._send(self._conn.mav.file_transfer_protocol_send,
                   0, msg.get_srcSystem(), msg.get_srcComponent(), payload)

    def _answer_ftp(self, msg: Any) -> None:
        """Open, burst-read, checksum and close: what a ground station reads metadata with."""
        raw = bytes(msg.payload)
        seq, _session, opcode, size, _req, _burst, _pad, offset = self._FTP_HEADER.unpack_from(raw)
        data = raw[12:12 + size]
        ack, nak = 128, 129
        if opcode == 4:        # OpenFileRO
            content = self._ftp_file(data.split(b"\x00")[0].decode("utf-8", "replace"))
            if content is None:
                self._ftp_reply(msg, seq + 1, nak, opcode, data=bytes([10]))   # FileNotFound
                return
            self._ftp_open = content
            self._ftp_reply(msg, seq + 1, ack, opcode, data=struct.pack("<I", len(content)))
        elif opcode == 15:     # BurstReadFile
            content = self._ftp_open
            if content is None:
                self._ftp_reply(msg, seq + 1, nak, opcode, data=bytes([4]))    # InvalidSession
                return

            def burst() -> None:
                pos, reply_seq = offset, seq + 1
                while pos < len(content) and not self._stop.is_set():
                    chunk = content[pos:pos + 239]
                    self._ftp_reply(msg, reply_seq, ack, opcode, data=chunk, offset=pos,
                                    burst_complete=pos + len(chunk) >= len(content))
                    pos += len(chunk)
                    reply_seq += 1
                    time.sleep(0.0005)
                self._ftp_reply(msg, reply_seq, nak, opcode, data=bytes([6]), offset=pos)  # EOF

            threading.Thread(target=burst, name="ftp-burst", daemon=True).start()
        elif opcode == 14:     # CalcFileCRC32
            content = self._ftp_file(data.split(b"\x00")[0].decode("utf-8", "replace"))
            if content is None:
                self._ftp_reply(msg, seq + 1, nak, opcode, data=bytes([10]))   # FileNotFound
                return
            self._ftp_reply(msg, seq + 1, ack, opcode,
                            data=struct.pack("<I", zlib.crc32(content)))
        elif opcode in (1, 2):  # TerminateSession, ResetSessions
            self._ftp_open = None
            self._ftp_reply(msg, seq + 1, ack, opcode)
        else:
            self._ftp_reply(msg, seq + 1, nak, opcode, data=bytes([7]))       # UnknownCommand

    # -- commands ---------------------------------------------------------

    def _ack(self, command: int, result: int) -> None:
        self._send(self._conn.mav.command_ack_send, command, result)

    def _handle_command(self, msg: Any) -> None:
        command = int(msg.command)
        params = [float(getattr(msg, f"param{i}", 0.0) or 0.0) for i in range(1, 8)]
        m = mavlink2

        if command == m.MAV_CMD_PREFLIGHT_CALIBRATION:
            self._ack(command, m.MAV_RESULT_ACCEPTED)
            self._start_calibration(params)
        elif command == m.MAV_CMD_COMPONENT_ARM_DISARM:
            want = params[0] >= 0.5
            if want and self._calibration is not None:
                self._ack(command, m.MAV_RESULT_TEMPORARILY_REJECTED)
                self.statustext("Arming denied: calibration in progress",
                                m.MAV_SEVERITY_CRITICAL)
                return
            self.armed = want
            self._ack(command, m.MAV_RESULT_ACCEPTED)
            self.statustext("Armed by external command" if want else "Disarmed",
                            m.MAV_SEVERITY_INFO)
        elif command == m.MAV_CMD_DO_SET_MODE:
            self.mode = _mode_from_triple(params[1], params[2])
            self._ack(command, m.MAV_RESULT_ACCEPTED)
            self.statustext(f"Mode: {self.mode}")
        elif command == m.MAV_CMD_NAV_TAKEOFF:
            self.mode = "TAKEOFF"
            self.armed = True
            self._ack(command, m.MAV_RESULT_ACCEPTED)
        elif command == m.MAV_CMD_NAV_LAND:
            self.mode = "LAND"
            self._ack(command, m.MAV_RESULT_ACCEPTED)
        elif command == m.MAV_CMD_NAV_RETURN_TO_LAUNCH:
            self.mode = "RTL"
            self._ack(command, m.MAV_RESULT_ACCEPTED)
        elif command == m.MAV_CMD_DO_SET_HOME:
            if params[4] or params[5]:
                self.model.home = (params[4], params[5])
            self._ack(command, m.MAV_RESULT_ACCEPTED)
        elif command == m.MAV_CMD_REQUEST_MESSAGE:
            self._ack(command, m.MAV_RESULT_ACCEPTED)
            self._request_message(int(params[0]))
        elif command == m.MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES:
            self._ack(command, m.MAV_RESULT_ACCEPTED)
            self._pending.append(self._send_autopilot_version)
        elif command == m.MAV_CMD_GET_HOME_POSITION:
            self._ack(command, m.MAV_RESULT_ACCEPTED)
            self._pending.append(self._send_home)
        elif command == m.MAV_CMD_SET_MESSAGE_INTERVAL:
            self._ack(command, m.MAV_RESULT_ACCEPTED)
        elif command == m.MAV_CMD_DO_MOTOR_TEST:
            if self.armed:
                self._ack(command, m.MAV_RESULT_TEMPORARILY_REJECTED)
                return
            self._ack(command, m.MAV_RESULT_ACCEPTED)
            self.statustext(f"Motor test: output {int(params[0]) + 1} "
                            f"at {int(params[2])} %")
        else:
            self._ack(command, m.MAV_RESULT_UNSUPPORTED)

    def _request_message(self, message_id: int) -> None:
        if message_id == mavlink2.MAVLINK_MSG_ID_AUTOPILOT_VERSION:
            self._pending.append(self._send_autopilot_version)
        elif message_id == mavlink2.MAVLINK_MSG_ID_HOME_POSITION:
            self._pending.append(self._send_home)

    # -- missions ---------------------------------------------------------

    def _begin_mission_upload(self, msg: Any) -> None:
        self._mission = []
        self._mission_items = []
        self._mission_point_seq = []
        self._mission_expect = int(msg.count)
        if self._mission_expect <= 0:
            self._send(self._conn.mav.mission_ack_send,
                       msg.get_srcSystem(), msg.get_srcComponent(),
                       mavlink2.MAV_MISSION_ACCEPTED)
            return
        self._send(self._conn.mav.mission_request_int_send,
                   msg.get_srcSystem(), msg.get_srcComponent(), 0)

    def _collect_mission_item(self, msg: Any) -> None:
        seq = int(msg.seq)
        self._mission.append((msg.x / 1e7, msg.y / 1e7, float(msg.z)))
        self._mission_items.append((
            int(msg.frame), int(msg.command), float(msg.param1), float(msg.param2),
            float(msg.param3), float(msg.param4), int(msg.x), int(msg.y), float(msg.z),
        ))
        if seq + 1 < self._mission_expect:
            self._send(self._conn.mav.mission_request_int_send,
                       msg.get_srcSystem(), msg.get_srcComponent(), seq + 1)
            return
        self._send(self._conn.mav.mission_ack_send,
                   msg.get_srcSystem(), msg.get_srcComponent(),
                   mavlink2.MAV_MISSION_ACCEPTED)
        self._on_event(f"mission uploaded ({len(self._mission)} items)")
        self.statustext(f"Mission accepted: {len(self._mission)} waypoints")
        if self.opts.follow_missions:
            self._adopt_mission()

    def _adopt_mission(self) -> None:
        """Fly what was just uploaded.

        The map's "fly to these points" is the one command whose result is the
        picture: the aircraft has to actually go there, or the screenshot shows
        a plan line and a vehicle ignoring it.
        """
        placed = [(seq, p) for seq, p in enumerate(self._mission)
                  if abs(p[0]) > 0.0001 or abs(p[1]) > 0.0001]
        points = [p for _seq, p in placed]
        if len(points) < 2:
            return
        self._mission_point_seq = [seq for seq, _p in placed]
        path = flight_mod.waypoints([(lat, lon, alt or 50.0) for lat, lon, alt in points],
                                    speed=self.model.path.speed, loop=True)
        self.model = flight_mod.FlightModel(path, home=self.model.home, takeoff_time=0.0)
        self._t0 = time.monotonic()
        self.mode = "MISSION"

    def _answer_mission_request(self, msg: Any) -> None:
        """One item of a download, exactly as it was uploaded."""
        seq = int(msg.seq)
        if not 0 <= seq < len(self._mission_items):
            return
        frame, command, p1, p2, p3, p4, x, y, z = self._mission_items[seq]
        self._send(self._conn.mav.mission_item_int_send,
                   msg.get_srcSystem(), msg.get_srcComponent(), seq, frame, command,
                   0, 1, p1, p2, p3, p4, x, y, z)

    def _send_mission_current(self) -> None:
        """Which item is being flown to, the way PX4 reports it about once a second.

        The flown path visits the mission's points in order, so the leg being
        flown names the point ahead; the item number is that point's.
        """
        if not self._mission_items:
            self._send(self._conn.mav.mission_current_send,
                       0, 65535, 1, 0)                     # MISSION_STATE_NO_MISSION
            return
        seq = 0
        if self._mission_point_seq:
            leg = self.model.leg_at(self.elapsed)
            ahead = (leg + 1) % len(self._mission_point_seq)
            seq = self._mission_point_seq[ahead]
        flying = self.armed and self.mode == "MISSION"
        state = 3 if flying else (4 if self.armed else 2)   # ACTIVE / PAUSED / NOT_STARTED
        self._send(self._conn.mav.mission_current_send,
                   seq, len(self._mission_items), state, 1 if flying else 0)

    # -- logs -------------------------------------------------------------

    def _answer_log_list(self, msg: Any) -> None:
        logs = self.opts.logs
        if not logs:
            # "No logs" is a real answer, and the Analysis page has a state for
            # it; PX4 says so with a single entry whose count is zero.
            self._send(self._conn.mav.log_entry_send, 0, 0, 0, 0, 0)
            return
        last = max(entry.id for entry in logs)
        for entry in logs:
            self._send(self._conn.mav.log_entry_send,
                       entry.id, len(logs), last, entry.utc, entry.size)

    def _answer_log_data(self, msg: Any) -> None:
        log_id = int(msg.id)
        entry = next((e for e in self.opts.logs if e.id == log_id), None)
        if entry is None:
            return
        offset = int(msg.ofs)
        wanted = int(msg.count)
        # A ground station asks for a large window and expects LOG_DATA
        # packets back to back until it is filled, the way PX4 streams them.
        # Answering one packet per request would make a download of a real
        # log take minutes.
        end = min(entry.size, offset + max(0, wanted))
        if end <= offset:
            self._send(self._conn.mav.log_data_send, log_id, offset, 0, [0] * 90)
            return
        pos = offset
        while pos < end and not self._stop.is_set():
            count = min(90, end - pos)
            if entry.data is not None:
                chunk = entry.data[pos:pos + count]
            else:
                chunk = bytes((pos + i) & 0xFF for i in range(count))
            self._send(self._conn.mav.log_data_send, log_id, pos, count,
                       list(chunk) + [0] * (90 - count))
            pos += count

    # -- calibration ------------------------------------------------------

    def _start_calibration(self, params: list[float]) -> None:
        """Run PX4's calibration transcript for whichever sensor was selected."""
        if all(_is_zero(p) for p in params):
            session = self._calibration
            if session is not None:
                session.cancel()
            return
        if self._calibration is not None:
            self.statustext("[cal] calibration already in progress",
                            mavlink2.MAV_SEVERITY_WARNING)
            return

        sensor = _selected_sensor(params)
        if sensor is None:
            return
        session = _Calibration(self, sensor)
        self._calibration = session
        session.start()

    def _calibration_finished(self) -> None:
        self._calibration = None


class _Calibration:
    """One calibration run, as the STATUSTEXT sequence PX4 would send.

    It can be told to stop partway — ``pause_after`` — and stay there. That is
    what makes the calibration screenshot reproducible: the wizard is caught
    with three sides ticked off and the fourth being asked for, and it will sit
    in that state for as long as it takes to frame the picture, because nothing
    is driving it but this script.
    """

    def __init__(self, vehicle: SimVehicle, sensor: str) -> None:
        self.vehicle = vehicle
        self.sensor = sensor
        self._cancel = threading.Event()
        self._thread: threading.Thread | None = None
        # Set by the scene through the vehicle's options, not by PX4.
        self.pause_after: int | None = getattr(vehicle, "calibration_pause_after", None)
        self.step_seconds: float = getattr(vehicle, "calibration_step_seconds", 2.2)

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name="calibration", daemon=True)
        self._thread.start()

    def cancel(self) -> None:
        self._cancel.set()
        self.vehicle.statustext("[cal] calibration cancelled",
                                mavlink2.MAV_SEVERITY_WARNING)
        self.vehicle._calibration_finished()

    # -- the transcripts --------------------------------------------------

    def _run(self) -> None:
        try:
            if self.sensor in ("accel", "compass"):
                self._sided(rotating=self.sensor == "compass")
            elif self.sensor == "level":
                self._simple("level", ("[cal] Hold still, measuring level",))
            elif self.sensor == "gyro":
                self._simple("gyro", ("[cal] Hold still, measuring gyro",))
            elif self.sensor == "baro":
                self._simple("baro", ("[cal] Hold still, measuring baro",))
            elif self.sensor == "airspeed":
                self._simple("airspeed", (
                    "[cal] Keep wind away from the sensor",
                    "[cal] Hold still, measuring airspeed",
                    "[cal] Blow into the tube now",
                ))
            elif self.sensor == "motor":
                self._simple("esc", (
                    "[cal] Disconnect the battery now",
                    "[cal] Connect the battery now",
                ), done_text="[cal] ESC calibration finished")
        finally:
            if not self._cancel.is_set():
                self.vehicle._calibration_finished()

    def _wait(self, seconds: float) -> bool:
        """Sleep, unless the calibration was cancelled. True = carry on."""
        return not self._cancel.wait(seconds)

    def _simple(self, name: str, lines: tuple[str, ...],
                done_text: str | None = None) -> None:
        say = self.vehicle.statustext
        say(f"[cal] calibration started: 2 {name}")
        if not self._wait(self.step_seconds):
            return
        for line in lines:
            say(line)
            if not self._wait(self.step_seconds):
                return
        for percent in (30, 70, 100):
            say(f"[cal] progress <{percent}>")
            if not self._wait(self.step_seconds / 3):
                return
        say(done_text or f"[cal] calibration done: {name}")

    def _sided(self, rotating: bool) -> None:
        """The six-position transcript, the heart of the calibration picture."""
        say = self.vehicle.statustext
        name = self.sensor
        pending = list(ACCEL_SIDES)
        say(f"[cal] calibration started: 2 {name}")
        if not self._wait(self.step_seconds):
            return

        done = 0
        while pending:
            say("[cal] Rotate to a pending side: " + ", ".join(pending))
            if self.pause_after is not None and done >= self.pause_after:
                # Hold here: the wizard is showing the side it wants next, with
                # everything already accepted ticked off. This is the frame.
                self.vehicle._on_event(
                    f"calibration paused with {done}/{len(ACCEL_SIDES)} sides done "
                    f"— next: {pending[0]}")
                self._cancel.wait()
                return
            if not self._wait(self.step_seconds):
                return

            side = pending.pop(0)
            say(f"[cal] {side} orientation detected")
            if not self._wait(self.step_seconds * 0.6):
                return
            say(f"[cal] Hold still, measuring {side} side")
            if not self._wait(self.step_seconds):
                return
            if rotating:
                say("[cal] Rotate vehicle around the detected orientation")
                if not self._wait(self.step_seconds * 1.4):
                    return
            done += 1
            say(f"[cal] progress <{int(done / len(ACCEL_SIDES) * 100)}>")
            if not self._wait(self.step_seconds * 0.3):
                return
            say(f"[cal] {side} side done")
            if not self._wait(self.step_seconds * 0.5):
                return

        say(f"[cal] calibration done: {name}")


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------


def _is_zero(value: float) -> bool:
    return value == 0.0


def _selected_sensor(params: list[float]) -> str | None:
    """Which calibration MAV_CMD_PREFLIGHT_CALIBRATION's parameters select.

    The same table corvus/mavlink_bridge.py sends, read the other way round.
    """
    def on(value: float, expect: float = 1.0) -> bool:
        return value == value and abs(value - expect) < 0.01   # NaN-safe

    if on(params[0]):
        return "gyro"
    if on(params[1]):
        return "compass"
    if on(params[2]):
        return "baro"
    if on(params[4]):
        return "accel"
    if on(params[4], 2.0):
        return "level"
    if on(params[4], 4.0):
        return "accel"          # quick accel: same transcript, fewer sides
    if on(params[5]):
        return "airspeed"
    if on(params[6]):
        return "motor"
    return None


def _param_id(msg: Any) -> str:
    raw = getattr(msg, "param_id", "")
    if isinstance(raw, bytes):
        raw = raw.decode("ascii", "ignore")
    return str(raw).rstrip("\x00").strip()


def _mode_name(custom_mode: int) -> str:
    main = (int(custom_mode) >> 16) & 0xFF
    sub = (int(custom_mode) >> 24) & 0xFF
    if main == MAIN_MODES["AUTO"]:
        for name, value in AUTO_SUBMODES.items():
            if value == sub:
                return name
    for name, value in MAIN_MODES.items():
        if value == main:
            return name
    return "POSCTL"


def _mode_from_triple(main: float, sub: float) -> str:
    return _mode_name((int(main) << 16) | (int(sub) << 24))


def _wrap_pi(angle: float) -> float:
    return (angle + math.pi) % (2 * math.pi) - math.pi
