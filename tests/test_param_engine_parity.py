"""The Rust parameter engine against the Python protocol it replaced.

``tests/reference/`` keeps Corvus' parameter protocol and value codec as they
were in Python. Each test here runs the same stream of PARAM_VALUEs, writes,
reads and uploads through a bridge on the reference and a bridge on
``corvus_link.params.ParamEngine``, and compares everything an observer can
see: the cache, the progress, every listener notification, every message put
on the link, every result and every command error.
"""
from __future__ import annotations

import math
import random
import struct
import time
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import autopilot  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from tests.reference import mavlink_params_python as reference  # noqa: E402
from tests.reference import param_codec_python as reference_codec  # noqa: E402

_STATE_NAMES = (
    "_params", "_param_download_state", "_param_count", "_param_received",
    "_param_seen_indices", "_param_rx_seq", "_param_misses",
    "_param_download_started_at", "_param_retransmit_round", "_param_last_value_at",
    "_param_upload_total", "_param_upload_done", "_param_state_before_upload",
)


class ReferenceBridge(reference.ParamProtocolMixin, MavlinkBridge):
    """A bridge whose parameter protocol is the Python reference."""


# The reference keeps its state in plain attributes; the engine's properties
# further down the MRO would otherwise take the assignments.
for _name in _STATE_NAMES:
    setattr(ReferenceBridge, _name, None)


class Message:
    def __init__(self, kind: str, **fields: Any) -> None:
        self.kind = kind
        self.__dict__.update(fields)

    def get_type(self) -> str:
        return self.kind

    def get_srcSystem(self) -> int:
        return 1

    def get_srcComponent(self) -> int:
        return 1


class Vehicle:
    """A link whose far end answers parameter reads and writes at once."""

    def __init__(self, values: dict[str, tuple[float, int]], answer: bool = True,
                 drop: set[str] | None = None, keep: set[str] | None = None) -> None:
        self.values = dict(values)
        self.answer = answer
        self.drop = drop or set()
        self.keep = keep or set()
        self.sent: list[tuple] = []
        self.bridge: Any = None

    def _reply(self, name: str) -> None:
        if not self.answer or name in self.drop or name not in self.values:
            return
        value, ptype = self.values[name]
        names = sorted(self.values)
        self.bridge._handle_param_value(Message(
            "PARAM_VALUE", param_id=name.encode(), param_value=value,
            param_type=ptype, param_index=names.index(name), param_count=len(names)))

    def param_request_list_send(self, ts: int, tc: int) -> None:
        self.sent.append(("list", ts, tc))

    def param_request_read_send(self, ts: int, tc: int, name: bytes, index: int) -> None:
        self.sent.append(("read", ts, tc, bytes(name), index))
        if name:
            self._reply(name.decode())

    def param_set_send(self, ts: int, tc: int, name: bytes, value: float, ptype: int) -> None:
        self.sent.append(("set", ts, tc, bytes(name), value, ptype))
        key = name.decode()
        if key not in self.keep:
            self.values[key] = (value, ptype)
        self._reply(key)


class Connection:
    def __init__(self, vehicle: Vehicle) -> None:
        self.mav = vehicle


def make(kind: type, vehicle: Vehicle | None = None, armed: bool = False) -> tuple[Any, list, list]:
    store = VehicleStateStore()
    store.heartbeat()
    if armed:
        store.update(armed=True)
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    if vehicle is not None:
        vehicle.bridge = bridge
        bridge._conn = Connection(vehicle)
    notes: list[dict] = []
    console: list[tuple] = []
    bridge.add_param_listener(lambda status: notes.append(dict(status)))
    bridge._console_publish = lambda *args: console.append(args)
    return bridge, notes, console


def both(vehicle_factory=None, armed: bool = False):
    out = []
    for kind in (ReferenceBridge, MavlinkBridge):
        vehicle = vehicle_factory() if vehicle_factory else None
        bridge, notes, console = make(kind, vehicle, armed)
        out.append((bridge, notes, console, vehicle))
    return out


def observed(bridge: Any) -> dict:
    return {
        "params": bridge.get_params(),
        "status": bridge.param_status(),
        "state": bridge._param_download_state,
        "count": bridge._param_count,
        "received": bridge._param_received,
        "seen": set(bridge._param_seen_indices),
        "misses": dict(bridge._param_misses),
        "upload": bridge.get_param_upload_result(),
        "error": bridge.get_last_command_error(),
    }


# ---------------------------------------------------------------- codec ----

def _px4(value: int, code: str) -> float:
    return struct.unpack("<f", struct.pack(code, value).ljust(4, b"\0"))[0]


def test_the_codec_matches_on_every_type_and_edge() -> None:
    rng = random.Random(41)
    values: list[Any] = [0, 1, -1, 4, 255, 256, -129, 65535, 2**31 - 1, -2**31, 2**32 - 1,
                         0.5, -0.25, 1e10, float("inf"), float("nan"), 3, 7.0, 0x7F800001,
                         -5_000_000, 12.0000001, 1e39]
    values += [rng.randint(-2**33, 2**33) for _ in range(300)]
    values += [rng.uniform(-1e6, 1e6) for _ in range(200)]
    encodings = (reference_codec.PARAM_ENCODING_BYTEWISE, reference_codec.PARAM_ENCODING_C_CAST)
    for ptype in range(0, 12):
        assert autopilot.param_type_is_integer(ptype) == reference_codec.param_type_is_integer(ptype)
        for encoding in encodings:
            for value in values:
                try:
                    want = reference_codec.encode_param_value(value, ptype, encoding)
                except (ValueError, OverflowError) as exc:
                    with pytest.raises(type(exc), match=str(exc).split("(")[0]):
                        autopilot.encode_param_value(value, ptype, encoding)
                else:
                    got = autopilot.encode_param_value(value, ptype, encoding)
                    assert struct.pack("<d", got) == struct.pack("<d", want), (value, ptype, encoding)
                if isinstance(value, float) and not math.isfinite(value):
                    continue
                raw = struct.pack("<f", float(value)) if abs(value) < 3e38 else None
                for wire in (None, raw):
                    a = reference_codec.decode_param_value(float(value) if abs(value) < 3e38 else 0.0,
                                                           ptype, encoding, raw=wire)
                    b = autopilot.decode_param_value(float(value) if abs(value) < 3e38 else 0.0,
                                                     ptype, encoding, raw=wire)
                    assert struct.pack("<d", a) == struct.pack("<d", b), (value, ptype, encoding)
            for got in values[:40]:
                for wanted in values[:40]:
                    assert autopilot.param_values_match(ptype, got, wanted) == \
                        reference_codec.param_values_match(ptype, got, wanted), (ptype, got, wanted)
    for x in (2.5, 3.5, -2.5, 0.5, 1.4999999):
        assert autopilot.param_values_match(6, x, round(x)) == reference_codec.param_values_match(6, x, round(x))
    assert autopilot.param_type_is_integer("6") is reference_codec.param_type_is_integer("6")
    assert autopilot.param_type_is_integer(object()) is reference_codec.param_type_is_integer(object())


def test_names_encode_identically() -> None:
    for name in ["A", "A" * 16, "A" * 17, "", "ÄBC", 3, None, b"BYTES", "MPC_XY_VEL_MAX"]:
        assert MavlinkBridge._encode_param_name(name) == ReferenceBridge._encode_param_name(name)


# ---------------------------------------------------------- PARAM_VALUE ----

def _random_stream(rng: random.Random, n: int) -> list[Message]:
    names = [f"P{i:03d}" for i in range(40)] + ["_HASH_CHECK", "TOO_LONG_NAME_OVER_16", ""]
    out = []
    for _ in range(n):
        name = rng.choice(names)
        ptype = rng.choice([1, 2, 3, 4, 5, 6, 9, 9, 9, 0, 11])
        count = rng.choice([40, 40, 40, 0, 10_001, 39])
        index = rng.choice([rng.randrange(0, 45), 0xFFFF, -1, 40])
        value = rng.choice([_px4(rng.randint(-1000, 1000), "<i"), rng.uniform(-10, 10), float("nan"), 4.0])
        param_id: Any = name.encode() if rng.random() < 0.7 else name
        if rng.random() < 0.03:
            param_id = b"\xff\xfeBAD"
        out.append(Message("PARAM_VALUE", param_id=param_id, param_value=value,
                            param_type=ptype, param_index=index, param_count=count))
    return out


@pytest.mark.parametrize("encoding", ["bytewise", "c_cast"])
@pytest.mark.parametrize("seed", range(6))
def test_param_value_streams_cache_and_notify_identically(seed: int, encoding: str) -> None:
    stream = _random_stream(random.Random(seed), 400)
    runs = both()
    for bridge, _notes, _console, _vehicle in runs:
        bridge._param_encoding_declared = encoding
        if seed % 2:
            bridge._param_download_state = "downloading"
        for msg in stream:
            bridge._handle_param_value(msg)
    (ref, ref_notes, _, _), (rs, rs_notes, _, _) = runs
    assert observed(rs) == observed(ref)
    assert rs_notes == ref_notes
    assert list(rs._params) == list(ref._params)
    for name, entry in ref._params.items():
        mine = rs._params[name]
        assert (mine.name, mine.value, mine.type, mine.index, mine.count, mine.seq) == \
            (entry.name, entry.value, entry.type, entry.index, entry.count, entry.seq)


# ------------------------------------------------------------- download ----

def test_a_lossy_download_and_its_watchdog_match(monkeypatch: pytest.MonkeyPatch) -> None:
    clock = [1000.0]
    monkeypatch.setattr(time, "monotonic", lambda: clock[0])
    names = {f"P{i:02d}": (float(i), 9) for i in range(30)}
    lost = {f"P{i:02d}" for i in (3, 7, 8, 21)}
    results = []
    for bridge, notes, _console, vehicle in both(lambda: Vehicle(names, drop=set(lost))):
        bridge._running.set()
        bridge._interruptible_sleep = lambda s: clock.__setitem__(0, clock[0] + s)
        bridge._start_param_watchdog = lambda: None
        assert bridge.request_param_list() is True
        for name in sorted(names):
            if name not in lost:
                vehicle._reply(name)
        vehicle.drop = {"P21"}
        clock[0] += 5.0
        bridge._param_watchdog()
        clock[0] = 1000.0
        results.append((observed(bridge), notes, vehicle.sent))
    assert results[1] == results[0]
    assert results[0][0]["state"] == "incomplete"


def test_a_download_refused_while_another_runs_matches() -> None:
    runs = both(lambda: Vehicle({}))
    for bridge, *_ in runs:
        bridge._start_param_watchdog = lambda: None
        assert bridge.request_param_list() is True
        assert bridge.request_param_list() is False
    assert observed(runs[1][0]) == observed(runs[0][0])


# --------------------------------------------------------------- writes ----

@pytest.mark.parametrize("case", [
    ("MPC_XY_VEL_MAX", 9.5, 9, {}),
    ("BAT1_N_CELLS", 6, 6, {}),
    ("BAT1_N_CELLS", 6.5, 6, {}),
    ("BAT1_N_CELLS", 2**40, 6, {}),
    ("MPC_XY_VEL_MAX", float("nan"), 9, {}),
    ("MPC_XY_VEL_MAX", True, 9, {}),
    ("MPC_XY_VEL_MAX", "1", 9, {}),
    ("A" * 17, 1.0, 9, {}),
    ("MISSING_PARAM", 1.0, 9, {"absent": True}),
    ("MPC_XY_VEL_MAX", 7.0, 9, {"keep": True}),
    ("MPC_XY_VEL_MAX", 7.0, 9, {"armed": True}),
])
def test_writes_match(case: tuple) -> None:
    name, value, ptype, opts = case
    values = {} if opts.get("absent") else {name: (12.0, ptype)}

    def vehicle() -> Vehicle:
        return Vehicle(values, keep={name} if opts.get("keep") else set())

    results = []
    for bridge, notes, console, link in both(vehicle, armed=bool(opts.get("armed"))):
        bridge._param_encoding_declared = "bytewise"
        if not opts.get("absent"):
            link._reply(name)
        ok = bridge.set_param(name, value) if not opts.get("absent") else None
        if opts.get("absent"):
            started = time.monotonic()
            ok = bridge.set_param(name, value)
            assert time.monotonic() - started < 3.5
        results.append((ok, observed(bridge), notes, console, link.sent))
    assert results[1] == results[0]


def test_a_write_cut_off_by_a_reconnect_matches() -> None:
    import threading
    results = []
    for bridge, _notes, _console, link in both(lambda: Vehicle({"MPC_XY_P": (0.95, 9)})):
        link._reply("MPC_XY_P")
        link.answer = False
        timer = threading.Timer(0.2, bridge._abort_parameter_operations)
        timer.start()
        ok = bridge.set_param("MPC_XY_P", 0.5)
        timer.join()
        results.append((ok, observed(bridge), link.sent))
    assert results[1] == results[0]
    assert results[0][0] is False and results[0][1]["error"] == "not connected"


# ---------------------------------------------------------------- reads ----

@pytest.mark.parametrize("fresh", [False, True])
def test_named_reads_match(fresh: bool) -> None:
    vehicle_values = {f"MC_{i}": (float(i), 9) for i in range(12)}
    wanted = [f"MC_{i}" for i in range(16)] + ["MC_1", "", 5]
    results = []
    for bridge, notes, _console, link in both(lambda: Vehicle(vehicle_values)):
        link._reply("MC_0")
        first = bridge.fetch_params(wanted, timeout=1.0, fresh=fresh)
        second = bridge.fetch_params(wanted, timeout=1.0, fresh=fresh)
        third = bridge.fetch_params(wanted, timeout=1.0, fresh=fresh)
        results.append((first, second, third, observed(bridge), notes, link.sent))
    assert results[1] == results[0]
    # 12 answers of 12 make the set complete, so the rest are known absent
    # after the first read and never asked for again.
    assert results[0][3]["misses"] == {"MC_12": 1, "MC_13": 1, "MC_14": 1, "MC_15": 1}


def test_a_silent_vehicle_reads_match() -> None:
    results = []
    for bridge, _notes, _console, link in both(lambda: Vehicle({}, answer=False)):
        got = bridge.fetch_params(["A", "B"], timeout=0.5)
        results.append((got, observed(bridge), link.sent))
    assert results[1] == results[0]
    assert results[0][1]["misses"] == {}


# --------------------------------------------------------------- verify ----

@pytest.mark.parametrize("rewrite", [True, False])
def test_verify_matches(rewrite: bool) -> None:
    values = {"MPC_XY_P": (0.95, 9), "BAT1_N_CELLS": (_px4(4, "<i"), 6), "STUCK": (1.0, 9)}
    targets = [{"name": "MPC_XY_P", "value": 0.95}, {"name": "BAT1_N_CELLS", "value": 6},
               {"name": "STUCK", "value": 2.0}, {"name": "ABSENT", "value": 1}]
    results = []
    for bridge, notes, console, link in both(lambda: Vehicle(values, keep={"STUCK"})):
        bridge._param_encoding_declared = "bytewise"
        for name in values:
            link._reply(name)
        out = bridge.verify_params(targets, names=["MPC_XY_P", "OTHER"], rewrite=rewrite, timeout=0.6)
        results.append((out, observed(bridge), notes, console, link.sent))
    assert results[1] == results[0]


@pytest.mark.parametrize("bad", [
    "not a list", [{"name": "A"}], [{"name": "A", "value": True}], [{"name": "A" * 17, "value": 1}],
    [{"name": "A", "value": float("inf")}], [], [3],
])
def test_unusable_verify_requests_match(bad: Any) -> None:
    results = []
    for bridge, *_ in both(lambda: Vehicle({})):
        results.append((bridge.verify_params(bad), bridge.get_last_command_error()))
    assert results[1] == results[0]


# --------------------------------------------------------------- upload ----

@pytest.mark.parametrize("plan", [
    [{"name": "MPC_XY_P", "value": 0.5}, {"name": "BAT1_N_CELLS", "value": 6}, {"name": "NOPE", "value": 1}],
    [{"name": "MPC_XY_P", "value": 0.5}],
    [{"name": "MPC_XY_P", "value": True}],
    [{"name": "", "value": 1}],
    [],
    "x",
])
@pytest.mark.parametrize("complete_before", [False, True])
def test_uploads_match(plan: Any, complete_before: bool) -> None:
    values = {"MPC_XY_P": (0.95, 9), "BAT1_N_CELLS": (_px4(4, "<i"), 6)}
    results = []
    for bridge, notes, console, link in both(lambda: Vehicle(values)):
        bridge._param_encoding_declared = "bytewise"
        bridge._running.set()
        for name in values:
            link._reply(name)
        if complete_before:
            bridge._param_download_state = "complete"
        started = bridge.start_param_upload(plan)
        if bridge._param_upload_thread is not None:
            bridge._param_upload_thread.join(timeout=10)
        results.append((started, observed(bridge), notes, console, link.sent))
    assert results[1] == results[0]


def test_an_upload_while_armed_matches() -> None:
    results = []
    for bridge, *_ in both(lambda: Vehicle({}), armed=True):
        results.append((bridge.start_param_upload([{"name": "A", "value": 1}]), observed(bridge)))
    assert results[1] == results[0]
