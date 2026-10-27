"""The Rust fleet tracker against the Python one it replaced.

``tests/reference/fleet_python.py`` keeps the fleet module as it was in
Python. The tests feed both trackers the same random frames (heartbeats from
the autopilot, a companion and a station, positions, attitude, VFR_HUD,
STATUSTEXT and SYS_STATUS, realistic and odd), binds and forgets, and compare
the vehicle each reports and its events at every step; and they start and
stop the fleet's publish thread both ways.
"""
from __future__ import annotations

import math
import random
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus.fleet import Fleet, FleetTracker  # noqa: E402
from corvus.mavlink_bridge import MAV_TYPE_MAP, MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from tests.reference import fleet_python as reference  # noqa: E402

TRACKERS = (reference.FleetTracker, FleetTracker)
ODD = [None, 0, -1, 65535, 2.5, float("nan"), float("inf"), "x", True, 10**12, b"\x00"]


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return (type(value).__name__, value)
    return value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


class Frame(SimpleNamespace):
    def get_type(self) -> Any:
        return self._type

    def get_srcSystem(self) -> Any:  # noqa: N802 - MAVLink's spelling
        return self._sys

    def get_srcComponent(self) -> Any:  # noqa: N802
        return self._comp


FIELDS: dict[str, dict[str, list[Any]]] = {
    "HEARTBEAT": {"type": [2, 1, 10, 13, 99], "autopilot": [12, 3, 0, 8], "base_mode": [0, 128, 209, 81],
                  "custom_mode": [0, 65536, 50593792, 4, 10], "system_status": [3, 4, 7]},
    "GLOBAL_POSITION_INT": {"lat": [480801234, -339000000, 0, 91 * 10**7], "lon": [116409690, 0, -181 * 10**7],
                            "alt": [512400, -100], "relative_alt": [30250, 0], "hdg": [9000, 65535, 0, 35999],
                            "vx": [120, 0, -55], "vy": [0, 75]},
    "VFR_HUD": {"heading": [90, 359, 0], "groundspeed": [0.0, 12.345, 7]},
    "ATTITUDE": {"yaw": [0.0, -1.5, 3.14159, -0.0, -1e-20]},
    "STATUSTEXT": {"severity": [0, 2, 4, 5, 6], "text": ["Kill engaged", "  Low   battery  ", b"Pre\x00Arm", "", "a\ud800b"]},
    "SYS_STATUS": {"battery_remaining": [-1, 0, 55, 100, 101], "voltage_battery": [0, 22200, 65535, 16800]},
}


def random_frame(rng: random.Random) -> Frame:
    kind = rng.choice(list(FIELDS))
    frame = Frame(_type=kind, _sys=rng.choice([1, 1, 1, 2, 0, "x"]), _comp=rng.choice([1, 1, 1, 191, 0, 154]))
    for name, choices in FIELDS[kind].items():
        if rng.random() < 0.05:
            continue
        setattr(frame, name, rng.choice(choices) if rng.random() < 0.9 else rng.choice(ODD))
    return frame


@pytest.mark.parametrize("seed", range(30))
def test_tracking_a_stream_matches(seed: int) -> None:
    results = []
    for kind in TRACKERS:
        rng = random.Random(seed)
        tracker = kind(MAV_TYPE_MAP if seed % 2 else None)
        log: list = []
        now = 100.0
        cursor = 0
        for step in range(300):
            now += rng.choice([0.1, 1.0, 3.0, 7.0, 700.0 if rng.random() < 0.01 else 0.5])
            roll = rng.random()
            if roll < 0.06:
                log.append(("bind", outcome(tracker.bind, rng.choice([1, 2, 0, 300, "x", None]),
                                            rng.choice([1, 0, 191, None, "y"]))))
            elif roll < 0.08:
                log.append(("forget", outcome(tracker.forget)))
            else:
                log.append(("observe", outcome(tracker.observe, random_frame(rng),
                                               vehicle_heartbeat=rng.random() < 0.8, now=now)))
            log.append(("systems", step, outcome(tracker.systems, now), outcome(tracker.bound_system)))
            if rng.random() < 0.2:
                seq, fresh = tracker.events_since(cursor)
                log.append(("events", seq, _plain(fresh)))
                cursor = seq if rng.random() < 0.7 else 0
        results.append(log)
    assert results[1] == results[0]


def test_the_streams_reach_every_field() -> None:
    seen: set[str] = set()
    for seed in range(30):
        rng = random.Random(seed)
        tracker = FleetTracker(MAV_TYPE_MAP)
        tracker.bind(1, 1)
        for _ in range(300):
            outcome(tracker.observe, random_frame(rng), vehicle_heartbeat=True, now=100.0)
            for item in tracker.systems(100.0):
                seen.update(k for k, v in item.items() if v not in (None, "", 0, False))
        seen.update(f"event:{sev}" for _, sev, _ in tracker.events_since(0)[1])
    for key in ("position", "altitude_rel", "heading", "groundspeed", "battery", "voltage", "mode", "armed",
                "event:2", "event:4"):
        assert key in seen, key


def test_the_publish_thread_starts_and_stops_alike() -> None:
    results = []
    for kind in (reference.Fleet, Fleet):
        store = VehicleStateStore()
        fleet = kind(MavlinkBridge(store), store)
        published: list = []
        fleet.add_listener(published.append)
        fleet.start()
        thread = fleet._thread
        first = (thread.name, thread.is_alive())
        fleet.start()
        same = fleet._thread is thread
        deadline = 2.0
        import time

        started = time.monotonic()
        while not published and time.monotonic() - started < deadline:
            time.sleep(0.02)
        fleet.stop()
        results.append((first, same, bool(published), fleet._thread, thread.is_alive()))
    assert results[1] == results[0]
    assert results[0][0] == ("fleet-publish", True) and results[0][2]
