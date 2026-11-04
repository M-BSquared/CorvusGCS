"""The Rust mission engine against the Python protocol it replaced.

``tests/reference/mavlink_missions_python.py`` keeps Corvus' mission protocol
as it was in Python. Each test runs the same uploads, downloads, MISSION_CURRENT
streams, fence transfers and payload checks through a bridge on the reference
and a bridge on ``corvus_link.missions.MissionEngine``, and compares what an
observer sees: every frame sent, the store's mission fields, the console, the
results and the command errors.
"""
from __future__ import annotations

import random
import threading
import time
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import mission  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_missions_python as reference  # noqa: E402

M = mavutil.mavlink
_STATE_NAMES = (
    "_mission_items", "_mission_upload_type", "_vehicle_fence", "_pending_mission_ack",
    "_mission_last_request", "_vehicle_mission_index", "_vehicle_mission_id",
    "_vehicle_mission_id_pending", "_vehicle_mission_confirmed", "_vehicle_mission_noted_at",
    "_mission_revision", "_mission_verdict",
)
MISSION_FIELDS = ("mission_known", "mission_revision", "mission_total", "mission_item",
                  "mission_reached_item", "mission_seq", "mission_state", "mission_reached")


class ReferenceBridge(reference.MissionProtocolMixin, MavlinkBridge):
    """A bridge whose mission protocol is the Python reference."""

    def _cancel_pending_commands(self) -> None:
        with self._ack_lock:
            for pending in self._pending_acks.values():
                pending.result = -2
                pending.event.set()
        with self._mission_lock:
            self._mission_items = None
            pending = self._pending_mission_ack
            self._pending_mission_ack = None
        if pending is not None:
            pending.result = -2
            pending.event.set()


for _name in _STATE_NAMES:
    setattr(ReferenceBridge, _name, None)


class Message(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)

    def get_srcComponent(self) -> int:
        return getattr(self, "source_component", 1)


def _to_us(**fields: Any) -> Message:
    return Message(target_system=255, target_component=190, **fields)


class Vehicle:
    """The vehicle's half of uploads and downloads, recorded frame by frame."""

    def __init__(self, ack: int = 0, verdict: int | None = 2, reason: str = "",
                 serve: bool = True, stored: list[Message] | None = None,
                 drop: set[int] | None = None, lost: set[int] | None = None,
                 silent: bool = False) -> None:
        self.ack = ack
        self.verdict = verdict
        self.reason = reason
        self.serve = serve
        self.stored = stored or []
        self.drop = set(drop or ())
        self.lost = set(lost or ())
        self.silent = silent
        self.sent: list[tuple] = []
        self.bridge: Any = None
        self.done = threading.Event()

    def _respond(self, count: int, mission_type: int) -> None:
        bridge = self.bridge

        def run() -> None:
            try:
                if not self.serve:
                    return
                for seq in range(count):
                    bridge._dispatch(_to_us(message_type="MISSION_REQUEST_INT", seq=seq,
                                            mission_type=mission_type))
                bridge._dispatch(_to_us(message_type="MISSION_ACK", type=self.ack,
                                        mission_type=mission_type))
                if not count or self.ack != 0 or mission_type:
                    return
                if self.reason:
                    bridge._console_publish("STATUSTEXT", self.reason, "critical")
                if self.verdict is None:
                    return
                deadline = time.monotonic() + 3.0
                while time.monotonic() < deadline and bridge._mission_verdict is not None:
                    time.sleep(0.05)
                    bridge._handle_mission_current(Message(
                        message_type="MISSION_CURRENT", seq=0, total=count,
                        mission_state=self.verdict, mission_mode=0, mission_id=0))
            finally:
                self.done.set()
        threading.Thread(target=run, daemon=True).start()

    def mission_count_send(self, ts: int, tc: int, count: int, *typed: int) -> None:
        self.sent.append(("count", ts, tc, count, *typed))
        self.done.clear()
        self._respond(count, typed[0] if typed else 0)

    def mission_item_int_send(self, *args: Any) -> None:
        self.sent.append(("item_int", *args))

    def mission_item_send(self, *args: Any) -> None:
        self.sent.append(("item", *args))

    def mission_ack_send(self, *args: Any) -> None:
        self.sent.append(("ack", *args))

    def mission_request_list_send(self, ts: int, tc: int, mission_type: int = 0) -> None:
        self.sent.append(("list", ts, tc, mission_type))
        if not self.silent:
            self.bridge._dispatch(_to_us(message_type="MISSION_COUNT", count=len(self.stored),
                                         mission_type=mission_type))

    def mission_request_int_send(self, ts: int, tc: int, seq: int, mission_type: int = 0) -> None:
        self.sent.append(("request", ts, tc, seq, mission_type))
        if seq in self.lost:
            return
        if seq in self.drop:
            self.drop.discard(seq)
            return
        self.bridge._dispatch(self.stored[seq])

    def command_long_send(self, *args: Any) -> None:
        self.sent.append(("command", *args))


def _canonical(frame: tuple) -> tuple:
    """NaN != NaN; a frame is compared with its NaNs spelled out."""
    return tuple("nan" if isinstance(v, float) and v != v else v for v in frame)


def make(kind: type, vehicle: Vehicle, stack: str = "px4") -> tuple[Any, list]:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    if stack == "ardupilot":
        bridge._latch_dialect(M.MAV_AUTOPILOT_ARDUPILOTMEGA, M.MAV_TYPE_QUADROTOR)
    else:
        bridge._latch_dialect(M.MAV_AUTOPILOT_PX4, M.MAV_TYPE_QUADROTOR)
    vehicle.bridge = bridge
    bridge._conn = SimpleNamespace(mav=vehicle, source_system=255, source_component=190)
    console: list[tuple] = []
    original = bridge._console_publish

    def record(name: str, text: str, level: str) -> None:
        console.append((name, text, level))
        original(name, text, level)

    bridge._console_publish = record
    return bridge, console


def observed(bridge: Any, vehicle: Vehicle, console: list) -> dict:
    snap = bridge._store.get_snapshot()
    return {
        "sent": [_canonical(f) for f in vehicle.sent],
        "store": {k: snap.get(k) for k in MISSION_FIELDS},
        "console": sorted(console),
        "error": bridge.get_last_command_error(),
        "index": bridge._vehicle_mission_index,
        "fence": bridge._vehicle_fence,
        "revision": bridge._mission_revision,
        "verdict_armed": bridge._mission_verdict is not None,
    }


def both(run, vehicle_factory, stack: str = "px4") -> list:
    out = []
    for kind in (ReferenceBridge, MavlinkBridge):
        vehicle = vehicle_factory()
        bridge, console = make(kind, vehicle, stack)
        result = run(bridge)
        vehicle.done.wait(4.0)
        out.append((_canonical_result(result), observed(bridge, vehicle, console)))
    return out


def _canonical_result(result: Any) -> Any:
    if isinstance(result, dict):
        return {k: _canonical_result(v) for k, v in result.items()}
    if isinstance(result, (list, tuple)):
        return type(result)(_canonical_result(v) for v in result)
    if isinstance(result, float) and result != result:
        return "nan"
    return result


@pytest.fixture(autouse=True)
def quick(monkeypatch: pytest.MonkeyPatch) -> None:
    """The same short budgets on both sides."""
    from corvus import mavlink_missions as rust_side
    for module in (reference, rust_side):
        monkeypatch.setattr(module, "MISSION_UPLOAD_QUIET_S", 0.6)
        monkeypatch.setattr(module, "MISSION_COUNT_RETRY_S", 0.15)
        monkeypatch.setattr(module, "MISSION_DOWNLOAD_WAIT_S", 0.1)
        monkeypatch.setattr(module, "MISSION_DOWNLOAD_WAIT_S_SLOW", 0.1)
        monkeypatch.setattr(module, "MISSION_VERDICT_WAIT_S", 1.0)


def _plan(kinds: list[str]) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    for n, kind in enumerate(kinds):
        lat, lon = 48.08 + n * 0.001, 11.64 + n * 0.001
        if kind == "takeoff":
            items.append({"type": "takeoff", "lat": lat, "lon": lon, "alt": 25})
        elif kind == "waypoint":
            items.append({"type": "waypoint", "lat": lat, "lon": lon, "alt": 30, "hold": 2, "speed": 9})
        elif kind == "circle":
            items.append({"type": "loiter_turns", "lat": lat, "lon": lon, "alt": 40,
                          "turns": 2, "radius": 60, "direction": -1})
        elif kind == "loiter":
            items.append({"type": "loiter_time", "lat": lat, "lon": lon, "alt": 40,
                          "seconds": 20, "radius": 50})
        elif kind == "land":
            items.append({"type": "land", "lat": lat, "lon": lon})
        else:
            items.append({"type": "rtl"})
    plan, error = mission.validate_plan({"home": {"lat": 48.08, "lon": 11.64}, "speed": 7,
                                         "items": items})
    assert error == "" and plan is not None
    return mission.plan_to_items(plan)


# --------------------------------------------------------------- upload ----

@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("case", [
    {"ack": 0, "verdict": 2},
    {"ack": 0, "verdict": 1, "reason": "Mission rejected: too far from home"},
    {"ack": 0, "verdict": 1},
    {"ack": 0, "verdict": None},
    {"ack": 14},
    {"ack": 3},
    {"serve": False},
])
def test_plan_uploads_match(stack: str, case: dict) -> None:
    items = _plan(["takeoff", "waypoint", "circle", "loiter", "rtl"])
    results = both(lambda b: b.upload_mission_plan(items), lambda: Vehicle(**case), stack)
    assert results[1] == results[0]


@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
def test_home_slot_and_item_specs_match(stack: str) -> None:
    rng = random.Random(stack)
    calls = [((seq, rng.choice([16, 22, 21, 178]), rng.uniform(-90, 90), rng.uniform(-180, 180),
               rng.uniform(0, 120), rng.random(), float("nan"), 0, -1.5),
              {"frame": rng.choice([None, 2, 3, 0])}) for seq in range(6)]
    specs = [MavlinkBridge._build_mission_item_spec(*a, **k) for a, k in calls]
    ref_specs = [ReferenceBridge._build_mission_item_spec(*a, **k) for a, k in calls]
    assert _canonical_result(specs) == _canonical_result(ref_specs)
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, _ = make(kind, Vehicle(), stack)
        bridge._store.update(home=[11.5, 48.5])
        bridge._home_alt_amsl = 512.0
        results.append(_canonical_result(bridge._with_mission_home_slot(specs)))
    assert results[1] == results[0]


def test_clearing_matches() -> None:
    for run in (lambda b: b.clear_mission(), lambda b: b.clear_fence()):
        results = both(run, Vehicle)
        assert results[1] == results[0]


@pytest.mark.parametrize("polygon", [
    [[11.64, 48.08], [11.65, 48.08], [11.65, 48.09], [11.64, 48.09]],
    [[11.64, 48.08], [11.65, 48.08]],
    "not a polygon",
])
def test_fence_uploads_match(polygon: Any) -> None:
    results = both(lambda b: b.upload_fence(polygon), Vehicle)
    assert results[1] == results[0]


def test_an_upload_refused_for_its_items_matches() -> None:
    items = _plan(["takeoff", "circle", "rtl"])
    results = both(lambda b: b.upload_mission_plan(items), Vehicle, "ardupilot")
    assert results[1] == results[0]


# ------------------------------------------------------------- download ----

def _stored(items: list[dict[str, Any]], offset: int = 0) -> list[Message]:
    out = []
    if offset:
        out.append(_to_us(message_type="MISSION_ITEM_INT", seq=0, frame=0, command=16, current=0,
                          autocontinue=1, param1=0, param2=0, param3=0, param4=0,
                          x=480000000, y=116400000, z=500.0, mission_type=0))
    for n, entry in enumerate(items):
        params = list(entry.get("params") or [0.0] * 4)
        command = int(entry["command"])
        frame = M.MAV_FRAME_MISSION if command in mission.POSITIONLESS_COMMANDS else 3
        out.append(_to_us(
            message_type="MISSION_ITEM_INT", seq=n + offset, frame=frame, command=command,
            current=0, autocontinue=1, param1=params[0], param2=params[1], param3=params[2],
            param4=params[3], x=int(round(entry.get("lat", 0.0) * 1e7)),
            y=int(round(entry.get("lon", 0.0) * 1e7)), z=float(entry.get("alt", 0.0)),
            mission_type=0))
    return out


@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("trouble", [{}, {"drop": {1, 3}}, {"lost": {2}}, {"silent": True}])
def test_downloads_match(stack: str, trouble: dict) -> None:
    items = _plan(["takeoff", "waypoint", "loiter", "land"])
    offset = 1 if stack == "ardupilot" else 0
    results = both(lambda b: b.download_mission(),
                   lambda: Vehicle(stored=_stored(items, offset), **trouble), stack)
    assert results[1] == results[0]


def test_an_empty_mission_downloads_alike() -> None:
    results = both(lambda b: b.download_mission(), lambda: Vehicle(stored=[]))
    assert results[1] == results[0]


# ------------------------------------------------------ MISSION_CURRENT ----

@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("seed", range(4))
def test_progress_reports_match(stack: str, seed: int, monkeypatch: pytest.MonkeyPatch) -> None:
    from corvus import mavlink_missions as rust_side
    rng = random.Random(seed)
    stream = []
    for _ in range(60):
        stream.append(rng.choice([
            Message(message_type="MISSION_CURRENT", seq=rng.randrange(-1, 8),
                    total=rng.choice([0, 3, 4, 5, 9, 65535, None]),
                    mission_state=rng.choice([0, 1, 2, 3, 4, 5, None]),
                    mission_id=rng.choice([0, 0, 77, 78])),
            Message(message_type="MISSION_ITEM_REACHED", seq=rng.randrange(-1, 8)),
            Message(message_type="MISSION_CURRENT"),
        ]))
    clock = [500.0]
    results = []
    for kind, module in ((ReferenceBridge, reference), (MavlinkBridge, rust_side)):
        monkeypatch.setattr(module, "time", SimpleNamespace(monotonic=lambda: clock[0]))
        clock[0] = 500.0
        vehicle = Vehicle()
        bridge, console = make(kind, vehicle, stack)
        bridge._note_vehicle_mission([0, 1, None, 2] if seed % 2 else [0, 1, 2, 3])
        for msg in stream:
            clock[0] += 1.0
            if msg.message_type == "MISSION_CURRENT":
                bridge._handle_mission_current(msg)
            else:
                bridge._handle_mission_item_reached(msg)
        results.append(observed(bridge, vehicle, console))
    assert results[1] == results[0]


# ----------------------------------------------------- fly-to payloads ----

@pytest.mark.parametrize("points", [
    [], "x", [{"lat": 48.0, "lon": 11.0, "alt_agl": 30}],
    [{"lat": 48.0, "lon": 11.0}], [{"lat": True, "lon": 11.0, "alt_agl": 30}],
    [{"lat": 91, "lon": 11.0, "alt_agl": 30}], [{"lat": 48.0, "lon": 181, "alt_agl": 30}],
    [{"lat": 48.0, "lon": 11.0, "alt_agl": 0.1}], [{"lat": 48.0, "lon": 11.0, "alt_agl": 9999}],
    [{"lat": float("nan"), "lon": 11.0, "alt_agl": 30}], [3],
    [{"lat": 48.0, "lon": 11.0, "alt_agl": 30}] * 256,
])
@pytest.mark.parametrize("vehicle_type", ["QUADROTOR", "GROUND_ROVER"])
def test_fly_to_point_checks_match(points: Any, vehicle_type: str) -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, _ = make(kind, Vehicle())
        bridge._latch_dialect(M.MAV_AUTOPILOT_PX4, getattr(M, f"MAV_TYPE_{vehicle_type}"))
        results.append((bridge._validate_fly_to_points(points), bridge.get_last_command_error()))
    assert results[1] == results[0]


def test_takeoff_item_positions_match() -> None:
    for position, home in [([11.6, 48.1], [11.5, 48.0]), ([0.0, 0.0], [11.5, 48.0]),
                           ([], []), (["x"], [11.0, 95.0]), (None, None)]:
        results = []
        for kind in (ReferenceBridge, MavlinkBridge):
            bridge, _ = make(kind, Vehicle())
            bridge._store.update(position=position, home=home)
            results.append(bridge._takeoff_item_position((47.0, 10.0, 30.0)))
        assert results[1] == results[0], (position, home)


def test_ack_mapping_matches() -> None:
    for ack in range(-3, 20):
        assert MavlinkBridge._mission_ack_to_result(ack) == ReferenceBridge._mission_ack_to_result(ack)
