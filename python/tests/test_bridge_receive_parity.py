"""The Rust receive path against the Python one it replaced.

``tests/reference/mavlink_bridge_receive_python.py`` keeps the receive loop,
the duplicate frame filter, the dispatcher, the COMMAND_LONG / COMMAND_INT
waits and the module helpers as they were in Python. Each test runs a bridge
on the reference and a bridge on ``corvus_link.bridge.ReceiveEngine`` through
the same frames (realistic, random and malformed, from this vehicle, its
other components and other systems), on one shared fake clock, and compares
every store update, every handler the dispatcher called, the console, the
bridge attributes the frames set, the return values and the errors.
"""
from __future__ import annotations

import math
import random
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

import corvus.mavlink_bridge as rust_side  # noqa: E402
import corvus.state_store as state_store  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_bridge_receive_python as reference  # noqa: E402

M = mavutil.mavlink


class ReferenceBridge(reference.ReceivePathMixin, MavlinkBridge):
    """A bridge whose receive path is the Python reference."""


KINDS = (ReferenceBridge, MavlinkBridge)


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, (set, frozenset)):
        return sorted(_plain(v) for v in value)
    return (type(value).__name__, value) if isinstance(value, (int, float)) and not isinstance(value, bool) else value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


class Clock:
    def __init__(self) -> None:
        self.now = 5000.0

    def monotonic(self) -> float:
        return self.now

    def time(self) -> float:
        return 1_760_000_000.0 + self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds

    def strftime(self, fmt: str, *_: Any) -> str:
        return "12:00:00"


@pytest.fixture
def clock(monkeypatch: pytest.MonkeyPatch) -> Clock:
    fake = Clock()
    for module in (reference, rust_side, state_store):
        monkeypatch.setattr(module, "time", fake)
    return fake


# ---------------------------------------------------------------- frames

ODD = [None, 0, 1, -1, 3, 7, 65535, 65534, 2.5, float("nan"), float("inf"), "x", True, 10**12, b"\x00"]


class Frame(SimpleNamespace):
    def get_type(self) -> str:
        return self._type

    def get_srcSystem(self) -> Any:  # noqa: N802 - MAVLink's spelling
        return self._sys

    def get_srcComponent(self) -> Any:  # noqa: N802
        return self._comp


FIELDS: dict[str, dict[str, list[Any]]] = {
    "HEARTBEAT": {"type": [2, 1, 10, 6, 26, 18], "autopilot": [12, 3, 8, 0], "base_mode": [0, 128, 129, 81, 209],
                  "custom_mode": [0, 65536, 196608, 50593792, 4, 5], "system_status": [3, 4, 7, 0],
                  "mavlink_version": [3, 3, 3, 2]},
    "GLOBAL_POSITION_INT": {"lat": [480801234, -339000000], "lon": [116409690, 0], "alt": [512400, 0, -1500],
                            "relative_alt": [30250, 0, -20], "hdg": [9000, 65535, 0], "vx": [120, -55, 0, None],
                            "vy": [0, 75], "vz": [-12, 0]},
    "ATTITUDE": {"roll": [0.1, -0.25, 3.14159], "pitch": [0.0, 0.05], "yaw": [1.5707963, -3.0],
                 "rollspeed": [0.0, 0.01], "pitchspeed": [0.2], "yawspeed": [-0.1, 0.0]},
    "VFR_HUD": {"groundspeed": [0.0, 12.345, 7], "airspeed": [0.0, float("nan"), 15.55, 3], "heading": [90, 359],
                "climb": [0.0, -1.25, 2]},
    "GPS_RAW_INT": {"fix_type": [0, 2, 3, 6], "eph": [65535, 80, 0, 121], "epv": [65535, 140, 0],
                    "satellites_visible": [0, 14], "alt": [512400, 0], "h_acc": [0, 900, 2500], "v_acc": [0, 1500]},
    "SYS_STATUS": {"voltage_battery": [22200, 65535, 16800, 0], "current_battery": [-1, 1000, 250],
                   "battery_remaining": [-1, 70, 0, 100],
                   "onboard_control_sensors_present": [0, 32, 268435456, 268435488],
                   "onboard_control_sensors_enabled": [0, 32, 268435488],
                   "onboard_control_sensors_health": [0, 32, 268435456, 268435488]},
    "SYSTEM_TIME": {"time_unix_usec": [0, 1_760_000_000_000_000, 10**25, -5], "time_boot_ms": [1000, 400000, 50, None, 2.5]},
    "AUTOPILOT_VERSION": {"flight_sw_version": [0, 0x010F0200, 0x04050600, 0x01100000],
                          "flight_custom_version": [[0, 0, 0, 0xAB, 0xCD, 0xEF, 0x12, 0x34], b"4a7b3c9d", [0] * 8, "zz"],
                          "vendor_id": [0, 0x26AC, None], "product_id": [0, 0x11, "x"],
                          "capabilities": [0, 1 << 15, 1 << 16, None]},
    "VIBRATION": {"vibration_x": [0.0, 1.23456, 30], "vibration_y": [0.5], "vibration_z": [12.00005],
                  "clipping_0": [0, 3], "clipping_1": [0], "clipping_2": [7]},
    "EXTENDED_SYS_STATE": {"landed_state": [0, 1, 2, 4, 5, None, "x"], "vtol_state": [0]},
    "HOME_POSITION": {"latitude": [480801234, 0], "longitude": [116409690], "altitude": [488000, 512250]},
    "DISTANCE_SENSOR": {"orientation": [25, 0, 24, 25], "current_distance": [150, 20, 1200, 0],
                        "min_distance": [20, 0, None], "max_distance": [1000, 0]},
    "COMMAND_ACK": {"command": [400, 176, 212, 22, 511], "result": [0, 1, 2, 4, 5, 9], "progress": [0, 40, 255],
                    "target_system": [0, 254, 255], "target_component": [0, 190, 1]},
    "OPEN_DRONE_ID_ARM_STATUS": {"status": [0, 1], "error": [b"", b"no operator"]},
    "STATUSTEXT": {"severity": [2, 4, 6], "text": ["PreArm: Battery low", "Takeoff detected", b"ok\x00"], "id": [0]},
    "LOG_ENTRY": {"id": [1], "num_logs": [3], "last_log_num": [3], "time_utc": [0], "size": [1024]},
    "PARAM_VALUE": {"param_id": ["MPC_XY_VEL_MAX", "SYS_AUTOSTART"], "param_value": [12.0, 4001.0],
                    "param_type": [9, 6], "param_count": [1000], "param_index": [3, 65535]},
    "RADIO_STATUS": {"rssi": [200], "remrssi": [0, 120, 255], "txbuf": [100, 40], "noise": [30], "remnoise": [30],
                     "rxerrors": [0, 5], "fixed": [0, 2]},
    "RC_CHANNELS": {**{f"chan{i}_raw": [1500, 65535, 1000] for i in range(1, 19)}, "chancount": [8, 16],
                    "rssi": [255, 80]},
    "MISSION_CURRENT": {"seq": [0, 3], "total": [5, 65535], "mission_state": [0, 3]},
    "BATTERY_STATUS": {"id": [0, 1], "voltages": [[4100] * 4 + [65535] * 6], "current_consumed": [-1, 1200],
                       "temperature": [32767, 2500], "time_remaining": [0, 600]},
}


def random_frame(rng: random.Random, target: tuple[int, int]) -> Frame:
    kind = rng.choice(list(FIELDS))
    sys_id = rng.choice([target[0]] * 6 + [0, 255, 7, 254])
    comp = rng.choice([target[1]] * 5 + [0, 154, 191, 68, 240])
    frame = Frame(_type=kind, _sys=sys_id, _comp=comp)
    for name, choices in FIELDS[kind].items():
        if rng.random() < 0.04:
            continue
        setattr(frame, name, rng.choice(choices) if rng.random() < 0.92 else rng.choice(ODD))
    return frame


# ------------------------------------------------------------- recorders

HANDLERS = [
    "_handle_mission_request", "_handle_mission_ack", "_handle_mission_count", "_handle_mission_item",
    "_handle_serial_control", "_handle_radio_status", "_handle_gnss_integrity", "_handle_remote_id_arm_status",
    "_handle_battery_status", "_handle_statustext", "_handle_param_value", "_handle_ftp_message",
    "_handle_rc_channels", "_handle_attitude_target", "_handle_position_target", "_handle_mission_current",
    "_handle_mission_item_reached", "_handle_autotune_ack", "_note_prearm_state", "_clear_prearm_reasons",
    "_roll_tlog_on_disarm", "_build_mode_mapping", "_request_streams", "_request_version", "_request_home",
    "_schedule_message_intervals", "_reset_parameter_cache", "_reset_param_metadata", "_note_heartbeat",
]

STATE = ["_home_alt_amsl", "_position_home_alt_amsl", "_firmware_version", "_board_vendor_id", "_board_product_id",
         "_param_encoding_declared", "_degraded_warned", "_rangefinder_seen", "_last_boot_ms", "_kill_evidence_at",
         "_target_system", "_target_component", "_mav_type_id", "_request_sent", "_duplicate_frames",
         "_ignored_systems", "_dispatch_failed"]


def make(kind: type, clock: Clock, stack: int = M.MAV_AUTOPILOT_PX4) -> tuple[Any, list]:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._latch_dialect(stack, M.MAV_TYPE_QUADROTOR)
    bridge._conn = SimpleNamespace(source_system=254, source_component=190, mav=SimpleNamespace())
    log: list = []
    original = store.update

    def update(**fields: Any) -> None:
        log.append(("store", _plain(fields)))
        original(**fields)

    store.update = update  # type: ignore[method-assign]
    original_resolve = store.resolve_warnings
    store.resolve_warnings = lambda events: (log.append(("resolve", sorted(events))), original_resolve(events))[1]
    bridge._console_publish = lambda name, text, level: log.append(("console", name, text, level))
    for name in HANDLERS:
        real = getattr(bridge, name)

        def record(*args: Any, _name: str = name, _real: Any = real, **kwargs: Any) -> Any:
            log.append(("call", _name, _plain(kwargs), _plain([getattr(a, "_type", a) for a in args])))
            if _name in ("_build_mode_mapping", "_request_streams", "_request_version", "_request_home",
                         "_schedule_message_intervals", "_handle_statustext"):
                return None
            return _real(*args, **kwargs)

        setattr(bridge, name, record)
    bridge._log_sink = lambda msg: log.append(("sink", msg._type))
    return bridge, log


def state(bridge: Any) -> list:
    return [(name, _plain(getattr(bridge, name))) for name in STATE]


# ------------------------------------------------------------- helpers

HELPERS = [
    ("_is_plausible_heartbeat", lambda f: [f]), ("_is_vehicle_heartbeat", lambda f: [f]),
    ("_sensor_health", lambda f: [f, 32]), ("_prearm_ok", lambda f: [f]),
    ("_rangefinder_distance_m", lambda f: [f]), ("_gps_altitude_m", lambda f: [f]),
    ("_gps_accuracy_m", lambda f: [getattr(f, "h_acc", None)]),
]


@pytest.mark.parametrize("seed", range(6))
def test_the_helpers_match(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(3000):
        frame = random_frame(rng, (1, 1))
        for name, args in HELPERS:
            mine = outcome(getattr(rust_side, name), *args(frame))
            assert mine == outcome(getattr(reference, name), *args(frame)), (name, vars(frame))


@pytest.mark.parametrize("raw", [
    [0, 0, 0, 0xAB, 0xCD, 0xEF, 0x12, 0x34], b"4a7b3c9d\x00\x00", "4A7B3C9D", [0] * 8, b"", "", None, 7,
    [1, 2, "x"], [256 + 5, -1, 0], (0x12, 0x34, 0, 0, 0, 0, 0, 0), bytearray(b"\x00\x00abc"), "é€zz",
    b"\x00\x00\x00\x00\x00\x00\x00\x01", [0, 0, 0, 0x34, 0x12],
], ids=repr)
def test_git_hashes_match(raw: Any) -> None:
    assert outcome(MavlinkBridge._decode_git_hash, raw) == outcome(ReferenceBridge._decode_git_hash, raw)


# ------------------------------------------------------------ dispatch

@pytest.mark.parametrize("seed", range(16))
@pytest.mark.parametrize("stack", [M.MAV_AUTOPILOT_PX4, M.MAV_AUTOPILOT_ARDUPILOTMEGA])
def test_dispatching_a_stream_matches(seed: int, stack: int, clock: Clock) -> None:
    results = []
    for kind in KINDS:
        clock.now = 5000.0
        bridge, log = make(kind, clock, stack)
        rng = random.Random(seed)
        for step in range(400):
            clock.now += rng.choice([0.01, 0.2, 1.0, 4.0])
            if rng.random() < 0.02:
                bridge._rangefinder_seen = clock.now - 10
            if rng.random() < 0.02:
                bridge._degraded_warned = True
            frame = random_frame(rng, (1, 1))
            log.append(("dispatch", step, frame._type, outcome(bridge._dispatch, frame)))
            log.append(("state", state(bridge)))
        results.append(log)
    assert results[1] == results[0]


def test_the_streams_reach_every_branch(clock: Clock) -> None:
    """The comparison above is only as good as what it exercises."""
    bridge, log = make(MavlinkBridge, clock)
    rng = random.Random(0)
    for _ in range(3000):
        outcome(bridge._dispatch, random_frame(rng, (1, 1)))
    called = {entry[1] for entry in log if entry[0] == "call"}
    for handler in ("_handle_battery_status", "_handle_param_value", "_handle_rc_channels", "_note_prearm_state",
                    "_handle_autotune_ack", "_build_mode_mapping", "_request_home", "_roll_tlog_on_disarm"):
        assert handler in called, handler
    written = {key for entry in log if entry[0] == "store" for key in entry[1]}
    for key in ("position", "roll", "groundspeed", "gps_fix", "battery_voltage", "boot_ms", "px4_version",
                "vibration_x", "landed_state", "home", "rangefinder_distance", "armed", "time"):
        assert key in written, key


# ---------------------------------------------------- frames and the loop

class Raw(Frame):
    def get_msgbuf(self) -> bytes:
        return self.raw


@pytest.mark.parametrize("seed", range(4))
def test_duplicates_and_failing_handlers_match(seed: int, clock: Clock) -> None:
    results = []
    for kind in KINDS:
        clock.now = 5000.0
        bridge, log = make(kind, clock)
        bridge._write_tlog = lambda msg, log=log: log.append(("tlog", msg._type))
        bridge.message_scope = SimpleNamespace(record=lambda msg, log=log: log.append(("scope", msg._type)))
        rng = random.Random(seed)
        for step in range(300):
            clock.now += rng.choice([0.05, 0.1, 0.35])
            frame = Raw(_type=rng.choice(["ATTITUDE", "HEARTBEAT", "BOOM", "VFR_HUD"]), _sys=1, _comp=1,
                        raw=bytes([rng.randrange(4)]) * rng.choice([0, 3]),
                        roll=0.1, pitch=0.2, yaw=0.3, rollspeed=0, pitchspeed=0, yawspeed=0,
                        groundspeed=1.0, airspeed=1.0, heading=3, climb=0.0)
            if frame._type == "HEARTBEAT":
                frame.type, frame.autopilot, frame.base_mode, frame.custom_mode = 2, 12, 0, 0
            if frame._type == "BOOM":
                frame.get_type = lambda: (_ for _ in ()).throw(RuntimeError("bad frame"))
            if rng.random() < 0.05:
                frame.get_msgbuf = lambda: (_ for _ in ()).throw(ValueError("no buffer"))
            log.append(("frame", step, outcome(bridge._process_frame, frame), bridge._duplicate_frames,
                        len(bridge._recent_frames), sorted(bridge._dispatch_failed)))
        results.append(log)
    assert results[1] == results[0]


class Link:
    def __init__(self, bridge: Any, script: list, clock: Clock) -> None:
        self.bridge = bridge
        self.script = list(script)
        self.clock = clock
        self.source_system = 254
        self.source_component = 190

    def recv_match(self, blocking: bool = True, timeout: float = 1) -> Any:
        self.clock.now += 0.4
        if not self.script:
            self.bridge._running.clear()
            return None
        item = self.script.pop(0)
        if isinstance(item, BaseException):
            raise item
        if item == "swap":
            self.bridge._conn = SimpleNamespace()
            return Frame(_type="ATTITUDE", _sys=1, _comp=1)
        if item == "stale":
            self.clock.now += 9.0
            return None
        return item


@pytest.mark.parametrize("script", [
    "plain", "errors", "too-many-errors", "late", "lost", "swap", "connection-error",
])
def test_receive_loops_match(script: str, clock: Clock, monkeypatch: pytest.MonkeyPatch) -> None:
    hb = Frame(_type="HEARTBEAT", _sys=1, _comp=1, type=2, autopilot=12, base_mode=0, custom_mode=0)
    att = Frame(_type="ATTITUDE", _sys=1, _comp=1, roll=0.1, pitch=0, yaw=0, rollspeed=0, pitchspeed=0, yawspeed=0)
    scripts = {
        "plain": [hb, att, None, att, hb],
        "errors": [hb, OSError("read"), att, ValueError("bad"), hb],
        "too-many-errors": [hb] + [OSError("unplugged")] * 50,
        "late": [hb, None, None, None, None, None, None, None, None, att, hb],
        "lost": [hb, "stale", att],
        "swap": [hb, "swap", att],
        "connection-error": [hb, ConnectionResetError("reset"), att],
    }
    for module in (reference, rust_side):
        monkeypatch.setattr(module, "RECV_ERROR_LIMIT", 6)
    results = []
    for kind in KINDS:
        clock.now = 5000.0
        bridge, log = make(kind, clock)
        bridge._store.heartbeat()
        bridge._running.set()
        bridge._interruptible_sleep = lambda s, log=log: log.append(("sleep", s))
        bridge._cleanup_statustext_chunks = lambda log=log: log.append(("cleanup",))
        link = Link(bridge, scripts[script], clock)
        bridge._conn = link
        result = outcome(bridge._receive_loop)
        log.append(("end", result, state(bridge)))
        results.append(log)
    assert results[1] == results[0]


# ------------------------------------------------------------- ACK waits

class AckLink:
    """Answers a command with scripted COMMAND_ACKs, dispatched as it is sent."""

    def __init__(self, bridge: Any, answers: list, sent: list) -> None:
        self.bridge = bridge
        self.answers = list(answers)
        self.sent = sent
        self.source_system = 254
        self.source_component = 190
        self.mav = SimpleNamespace(command_long_send=self._send, command_int_send=self._send)

    def _send(self, *args: Any) -> None:
        self.sent.append(_plain(list(args)))
        if not self.answers:
            return
        answer = self.answers.pop(0)
        if answer == "raise":
            raise OSError("radio gone")
        if answer == "swap":
            self.bridge._conn = SimpleNamespace()
            return
        for result in answer:
            ack = Frame(_type="COMMAND_ACK", _sys=1, _comp=1, command=args[2] if len(args) == 11 else args[3],
                        result=result, target_system=254, target_component=190)
            self.bridge._dispatch(ack)


ANSWERS = {
    "accepted": [[0]], "denied": [[2]], "silent": [[], [], []], "late": [[], [0]],
    "in-progress-then-done": [[5, 0]], "in-progress-only": [[5]], "interim-then-final": [[1, 0]],
    "interim-only": [[1], []], "raise": ["raise"], "swap": ["swap"], "in-progress-accepted": [[5]],
}


@pytest.mark.parametrize("answers", list(ANSWERS))
@pytest.mark.parametrize("form", ["long", "int"])
@pytest.mark.parametrize("ready", [True, False])
def test_command_waits_match(answers: str, form: str, ready: bool, clock: Clock) -> None:
    results = []
    for kind in KINDS:
        bridge, log = make(kind, clock)
        sent: list = []
        bridge._conn = AckLink(bridge, ANSWERS[answers], sent)
        if not ready:
            bridge._store.set_disconnected()
        if form == "long":
            result = outcome(bridge._send_command_and_wait, 176, [1, 2.5, None][: 2 + (answers == "late")],
                             timeout=0.02, retries=2, accept_in_progress=answers == "in-progress-accepted",
                             interim=frozenset({1}) if answers.startswith("interim") else frozenset())
        else:
            result = outcome(bridge._send_command_int_and_wait, 192, 6, [1.0], x=481234567, y=116409690.4, z=505,
                             timeout=0.02, retries=1)
        results.append((result, sent, log, sorted(bridge._pending_acks)))
    assert results[1] == results[0]


def test_cancelling_waiters_matches(clock: Clock) -> None:
    results = []
    for kind in KINDS:
        bridge, _ = make(kind, clock)
        pending = [rust_side._PendingAck() for _ in range(3)]
        bridge._pending_acks.update({176: pending[0], 400: pending[1], 22: pending[2]})
        bridge._cancel_pending_commands()
        results.append([(p.result, p.event.is_set()) for p in pending])
    assert results[1] == results[0]
