"""The Rust telemetry engine against the Python telemetry it replaced.

``tests/reference/mavlink_telemetry_python.py`` keeps the handlers as they
were in Python. The tests feed a bridge on the reference and a bridge on
``corvus_link.telemetry.TelemetryEngine`` the same messages (realistic ones,
random ones and malformed ones), heartbeats, battery readings and stream
requests, record every store update both make, and compare them along with
return values, raised errors, command errors and the remembered state.
"""
from __future__ import annotations

import collections
import math
import random
import struct
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import mavlink_telemetry as rust_side  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import telemetry as engine  # noqa: E402
from corvus_link import x25crc  # noqa: E402
from tests.reference import mavlink_telemetry_python as reference  # noqa: E402


class ReferenceBridge(reference.TelemetryMixin, MavlinkBridge):
    """A bridge whose telemetry is the Python reference."""

    def _note_heartbeat(self, stamp: float) -> None:
        self._hb_times.append(stamp)

    def _forget_link_quality(self) -> None:
        self._hb_times.clear()
        self._radio_status_seen = False
        self._uplink_score = 0


for _name in ("_hb_times", "_radio_status_seen", "_uplink_score", "_battery_cells",
              "_drain_samples", "_drain_source"):
    setattr(ReferenceBridge, _name, None)

KINDS = (ReferenceBridge, MavlinkBridge)


def _plain(value: Any) -> Any:
    if isinstance(value, float) and value != value:
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple, collections.deque)):
        return [_plain(v) for v in value]
    return value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def make(kind: type, **extra: Any) -> tuple[Any, list]:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = kind(store)
    bridge._target_system = extra.get("target", 1)
    bridge._target_component = 1
    updates: list[dict] = []
    original = store.update

    def update(**fields: Any) -> None:
        updates.append(_plain(fields))
        original(**fields)

    store.update = update  # type: ignore[method-assign]
    return bridge, updates


# ------------------------------------------------------------ Python math

def test_rounding_matches_python() -> None:
    rng = random.Random(7)
    values = [0.25, 0.35, 2.675, -0.04, 0.05, 1.45, 1e16 + 0.5, 5e-324, 123456.78949999]
    values += [rng.uniform(-1e4, 1e4) for _ in range(20000)]
    values += [rng.randint(-10**6, 10**6) / 1000 for _ in range(20000)]
    values += [rng.randint(-10**4, 10**4) / 8 for _ in range(5000)]
    for value in values:
        for digits in (1, 2, 3):
            assert engine._round(value, digits) == round(value, digits), (value, digits)


def test_sums_match_python() -> None:
    rng = random.Random(3)
    for _ in range(2000):
        values = [rng.choice([rng.uniform(-1, 1), rng.uniform(-1e12, 1e12), 1e100, -1e100, 0.1, -0.0])
                  for _ in range(rng.randint(1, 40))]
        assert engine._sum(values) == sum(values), values


# -------------------------------------------------------------- functions

@pytest.mark.parametrize("value", [None, 0, 1, 50, 100, 101, 180, 254, 255, 256, -1, 10**30, 77.9,
                                   "88", "x", True, float("nan"), b"7"], ids=repr)
def test_rc_rssi_matches(value: Any) -> None:
    assert outcome(engine.rc_rssi_percent, value) == outcome(reference._rc_rssi_percent, value)


def test_quaternions_match() -> None:
    rng = random.Random(11)
    quats = [(1.0, 0.0, 0.0, 0.0), (0.7072, 0.0, 0.7072, 0.0), (0.0, 1.0, 0.0, 0.0),
             (float("nan"), 0.0, 0.0, 0.0), (2.0, 2.0, 2.0, 2.0)]
    for _ in range(20000):
        q = [rng.gauss(0, 1) for _ in range(4)]
        norm = math.sqrt(sum(v * v for v in q)) * rng.choice([1.0, 1.0 + 1e-7, 1.0 - 1e-7])
        quats.append(tuple(v / norm for v in q))
    for q in quats:
        assert _plain(engine.quaternion_to_euler_deg(*q)) == _plain(reference._quaternion_to_euler_deg(*q))


def _frame(system: int = 1, receiver: int = 0, jamming: int = 3, spoofing: int = 1, signal: int = 7,
           trim: bool = True) -> bytearray:
    payload = struct.pack("<IHHBBBBBBBBB", 0, 0, 0, receiver, 0, jamming, spoofing, 0, 255, 255, signal, 255)
    if trim:
        payload = payload.rstrip(b"\0") or b"\0"
    head = bytes([0xFD, len(payload), 0, 0, 5, system, 1]) + (441).to_bytes(3, "little")
    crc = x25crc(head[1:] + payload)
    crc.accumulate(bytes([169]))
    return bytearray(head + payload + crc.crc.to_bytes(2, "little"))


def _frames() -> list[Any]:
    rng = random.Random(5)
    out: list[Any] = [_frame(), _frame(trim=False), _frame(system=7, receiver=1), _frame(signal=0),
                      b"", b"\xfe" + bytes(20), "text", 12, None, [0xFD, 1, 2], -1, 3.5]
    for _ in range(3000):
        frame = _frame(system=rng.randint(0, 255), receiver=rng.randint(0, 3), jamming=rng.randint(0, 4),
                       spoofing=rng.randint(0, 4), signal=rng.randint(0, 255), trim=rng.random() < 0.7)
        roll = rng.random()
        if roll < 0.2:
            frame[rng.randrange(len(frame))] ^= 1 << rng.randrange(8)
        elif roll < 0.3:
            frame = frame[:rng.randrange(len(frame))]
        elif roll < 0.35:
            frame[1] = rng.randint(0, 40)
        out.append(bytes(frame))
    return out


def test_gnss_integrity_frames_decode_alike() -> None:
    for frame in _frames():
        assert outcome(engine.decode_gnss_integrity, frame) == outcome(reference._decode_gnss_integrity, frame)


# --------------------------------------------------------------- handlers

ODD = [None, 0, 1, -1, 3, 7, 65534, 65535, 2.5, "4", "x", float("nan"), float("inf"), True, [], 10**12]


def _random_msg(rng: random.Random, fields: dict[str, list[Any]], drop: float = 0.1) -> SimpleNamespace:
    msg = SimpleNamespace()
    for name, choices in fields.items():
        if rng.random() < drop:
            continue
        setattr(msg, name, rng.choice(choices) if rng.random() < 0.85 else rng.choice(ODD))
    return msg


def _run(kind: type, feed: Any, **extra: Any) -> tuple:
    bridge, updates = make(kind, **extra)
    results = feed(bridge)
    return results, updates


def both(feed: Any, **extra: Any) -> None:
    ref = _run(ReferenceBridge, feed, **extra)
    mine = _run(MavlinkBridge, feed, **extra)
    assert mine == ref


BATTERY = {
    "id": [0, 0, 0, 1],
    "voltages": [[4100, 4090, 4080, 4070] + [65535] * 6, [16800] + [65535] * 9, [65534, 1000] + [65535] * 8,
                 [0] * 10, [], None, [65535] * 10, [3000, 0, 65535, 3700]],
    "voltages_ext": [[0, 0, 0, 0], [65535] * 4, [4000, 0, 0, 0], None],
    "current_consumed": [-1, 0, 1234, None, 2**31 - 1],
    "temperature": [32767, 2512, -1050, 0, None],
    "time_remaining": [0, 600, -5, None],
}


@pytest.mark.parametrize("seed", range(10))
def test_battery_status_matches(seed: int) -> None:
    def feed(bridge: Any) -> list:
        rng = random.Random(seed)
        return [outcome(bridge._handle_battery_status, _random_msg(rng, BATTERY)) for _ in range(150)]
    both(feed)


INTEGRITY = {
    "id": [0, 0, 1], "jamming_state": [0, 1, 2, 3, 4, None], "spoofing_state": [0, 1, 2, 3, 9],
    "gnss_signal_quality": [0, 5, 10, 11, 255, -1],
}


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize("target", [1, 7, 0])
def test_gnss_integrity_messages_match(seed: int, target: int) -> None:
    frames = _frames()

    def feed(bridge: Any) -> list:
        rng = random.Random(seed)
        out = []
        for _ in range(200):
            if rng.random() < 0.5:
                msg = _random_msg(rng, INTEGRITY)
                msg.get_type = lambda: "GNSS_INTEGRITY"
                if rng.random() < 0.8:
                    system = rng.choice([0, 1, 7, 9])
                    msg.get_srcSystem = lambda system=system: system
            else:
                frame = rng.choice(frames)
                msg = SimpleNamespace(get_type=lambda: "UNKNOWN_441", data=frame)
            out.append(outcome(bridge._handle_gnss_integrity, msg))
        out.append(outcome(bridge._forget_gnss_integrity))
        return out
    both(feed, target=target)


SETPOINTS = {
    "q": [[1.0, 0.0, 0.0, 0.0], [0.7072, 0.0, 0.7072, 0.0], [0.5, 0.5, 0.5, 0.5], [1.0, 0.0], None,
          [1.0, "x", 0.0, 0.0], (0.9, 0.1, -0.1, 0.4, 7.0), [float("nan")] * 4],
    "body_roll_rate": [0.0, 0.123, -1.5707, 3.0, "0.2"], "body_pitch_rate": [0.0, 0.05, -0.75],
    "body_yaw_rate": [0.0, 1.0, -0.333],
    "vx": [0.0, 1.234, -5.005, "2.5"], "vy": [0.0, 0.125, 9.995], "vz": [0.0, -0.015, 0.025],
}


@pytest.mark.parametrize("seed", range(10))
def test_setpoints_match(seed: int) -> None:
    def feed(bridge: Any) -> list:
        rng = random.Random(seed)
        out = []
        for _ in range(200):
            msg = _random_msg(rng, SETPOINTS)
            for name in ("body_roll_rate", "body_pitch_rate", "body_yaw_rate", "vx", "vy", "vz"):
                if rng.random() < 0.6 and isinstance(getattr(msg, name, None), float):
                    setattr(msg, name, rng.uniform(-6, 6))
            out.append(outcome(bridge._handle_attitude_target, msg))
            out.append(outcome(bridge._handle_position_target, msg))
        return out
    both(feed)


def _rc_fields(count: int) -> dict[str, list[Any]]:
    fields: dict[str, list[Any]] = {f"chan{i}_raw": [1000, 1500, 2000, 0, 65535, 65534, 70000, -3]
                                    for i in range(1, count + 1)}
    fields.update(chancount=[0, 8, 12, 18, 30, -1], rssi=[255, 0, 80, 200, 254], port=[0, 1, 2, 3, -1])
    return fields


@pytest.mark.parametrize("seed", range(10))
def test_rc_channels_match(seed: int) -> None:
    def feed(bridge: Any) -> list:
        rng = random.Random(seed)
        out = []
        for _ in range(150):
            raw = rng.random() < 0.5
            msg = _random_msg(rng, _rc_fields(8 if raw else 18), drop=0.03)
            out.append(outcome(bridge._handle_rc_channels, msg, raw=raw))
        return out
    both(feed)


RADIO = {"remrssi": [0, 255, 40, 95, 150, 200, 120.5], "txbuf": [0, 50, 100, 120, -4],
         "rxerrors": [0, 3, 70000], "fixed": [0, 1, 9]}


@pytest.mark.parametrize("seed", range(10))
def test_link_quality_and_jitter_match(seed: int) -> None:
    def feed(bridge: Any) -> list:
        rng = random.Random(seed)
        stamp = 1000.0
        out = []
        for step in range(300):
            roll = rng.random()
            if roll < 0.5:
                stamp += rng.choice([1.0, 1.0, rng.uniform(0.5, 1.5), rng.uniform(0.0, 4.0)])
                bridge._note_heartbeat(stamp)
                out.append(outcome(bridge._publish_heartbeat_jitter))
            elif roll < 0.85:
                out.append(outcome(bridge._handle_radio_status, _random_msg(rng, RADIO, drop=0.02)))
            elif roll < 0.97:
                out.append(outcome(bridge._publish_link_quality))
            else:
                bridge._forget_link_quality()
            out.append((bridge._current_jitter_ms(), bridge._radio_status_seen, bridge._uplink_score,
                        list(bridge._hb_times), step))
        return out
    both(feed)


# ---------------------------------------------------------------- battery

@pytest.mark.parametrize("settings", [None, {"estimate": True}, {"estimate": True, "cells": 6},
                                      {"chemistry": "liion", "estimate": True}, {"cells": 4}, "junk"],
                         ids=repr)
def test_battery_fields_match(settings: Any) -> None:
    def feed(bridge: Any) -> list:
        rng = random.Random(1)
        out = [outcome(bridge.set_battery_settings, settings), outcome(bridge.battery_settings)]
        for step in range(200):
            voltage = rng.choice([25.2, 22.2, 19.8, 16.8, 12.6, 0.0, rng.uniform(5, 26)])
            current = rng.choice([0.0, 10.0, rng.uniform(0, 60)])
            reported = rng.choice([-1, 0, 55, 87, 100])
            out.append(outcome(bridge._battery_fields, voltage, current, reported))
            if step % 50 == 49:
                bridge._forget_battery_cells()
            out.append(bridge._battery_cells)
        return out
    both(feed)


def test_battery_endurance_matches() -> None:
    def feed(bridge: Any) -> list:
        rng = random.Random(2)
        out = []
        for t in range(900):
            percent = rng.choice([80 - t / 10, 80 - t / 10, None, 60.0, float("nan"), 150.0, "x"])
            source = rng.choice(["autopilot"] * 9 + ["estimate"])
            armed = rng.random() < 0.95
            out.append(outcome(bridge._battery_endurance_fields, percent, source, armed, now=float(t)))
            out.append((_plain(list(bridge._drain_samples)[-3:]), bridge._drain_source))
        # A clean flight: a steady drain with sensor wobble, one source switch
        # and one disarm, so the drain rate itself is compared too.
        for t in range(1000, 1900):
            percent = 90 - (t - 1000) / 12 + rng.uniform(-0.3, 0.3)
            source = "estimate" if 1400 <= t < 1405 else "autopilot"
            armed = not 1600 <= t < 1610
            out.append(outcome(bridge._battery_endurance_fields, percent, source, armed, now=t + rng.random()))
        return out

    both(feed)
    bridge, _ = make(MavlinkBridge)
    estimates = [o[1]["battery_endurance_est"] for o in feed(bridge)
                 if isinstance(o, tuple) and o[0] == "ok"]
    assert sum(1 for e in estimates if e > 0) > 300


def test_battery_endurance_reads_the_clock(monkeypatch: pytest.MonkeyPatch) -> None:
    clock = iter(range(0, 10_000, 2))
    fake = SimpleNamespace(monotonic=lambda: float(next(clock)))
    results = []
    for kind, module in zip(KINDS, (reference, rust_side), strict=True):
        monkeypatch.setattr(module, "time", fake)
        bridge, _ = make(kind)
        results.append([bridge._battery_endurance_fields(90 - n / 4, "autopilot", True) for n in range(200)])
    assert results[1] == results[0]
    assert any(r["battery_endurance_est"] > 0 for r in results[0])


# ---------------------------------------------------------------- streams

def _wire_streams(bridge: Any, *, ready: bool, slow: bool, accept: set[int] | None,
                  baselines: dict[int, int]) -> list:
    sent: list = []

    def set_interval(msg_id: int, interval: int) -> bool:
        sent.append((msg_id, interval))
        return accept is None or msg_id in accept

    bridge.set_message_interval = set_interval
    bridge._connection_ready = lambda: ready
    bridge._is_slow_link = lambda: slow
    bridge._message_intervals = lambda: dict(baselines)
    return sent


@pytest.mark.parametrize("ready", [True, False])
@pytest.mark.parametrize("slow", [True, False])
@pytest.mark.parametrize("accept", [None, {30}], ids=["all", "attitude-only"])
@pytest.mark.parametrize("baselines", [{}, {30: 20000, 65: 200000}], ids=["none", "hud"])
def test_stream_requests_match(ready: bool, slow: bool, accept: Any, baselines: dict) -> None:
    calls = [
        ("set_vibration_stream", (True, 10)), ("set_vibration_stream", (True, True)),
        ("set_vibration_stream", (True, 0)), ("set_vibration_stream", (True, 51)),
        ("set_vibration_stream", (True, 2.0)), ("set_vibration_stream", (False, 0)),
        ("set_rangefinder_stream", (True,)), ("set_rangefinder_stream", (False,)),
        ("set_rc_stream", (True, 20)), ("set_rc_stream", (True, 3)), ("set_rc_stream", (True, True)),
        ("set_rc_stream", (True, 7.0)), ("set_rc_stream", (True, 99)), ("set_rc_stream", (False, 20)),
        ("set_tuning_stream", (True, 20)), ("set_tuning_stream", (True, 50)), ("set_tuning_stream", (True, "x")),
        ("set_tuning_stream", (True, 0)), ("set_tuning_stream", (False, 20)), ("set_tuning_stream", (1, 13)),
    ]
    results = []
    for kind in KINDS:
        bridge, updates = make(kind)
        sent = _wire_streams(bridge, ready=ready, slow=slow, accept=accept, baselines=baselines)
        out = []
        for name, args in calls:
            bridge._rangefinder_seen = 5.0
            out.append((name, outcome(getattr(bridge, name), *args), bridge.get_last_command_error(),
                        bridge._rangefinder_seen))
        out.append([outcome(bridge._stream_interval_us, m, r) for m in (30, 65, 132) for r in (None, 5000, 10**6)])
        results.append((out, sent, updates))
    assert results[1] == results[0]
