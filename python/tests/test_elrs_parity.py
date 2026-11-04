"""The Rust ExpressLRS code against the Python it replaced.

``tests/reference/elrs_python.py`` keeps the CRSF module as it was in Python.
The tests compare the checksum, the frame builder (odd arguments too), the
parser on random byte streams cut at random, every decoder on random
payloads of every length, the telemetry snapshot over random frame streams on
a fake clock, the reader thread against a scripted source, and the fly-to
planner for every link mode, stack and point.
"""
from __future__ import annotations

import itertools
import logging
import math
import random
import threading
import time
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import elrs as rust_side  # noqa: E402
from tests.reference import elrs_python as reference  # noqa: E402

MODULES = (reference, rust_side)


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, (bytes, bytearray)):
        return bytes(value)
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return (type(value).__name__, value)
    return value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def frame_tuple(frame: Any) -> tuple:
    return (frame.address, frame.type, frame.payload)


def random_payload(rng: random.Random, length: int | None = None) -> bytes:
    n = rng.choice([0, 1, 2, 5, 6, 8, 10, 14, 15, 16, 40, 58]) if length is None else length
    return bytes(rng.randrange(256) for _ in range(n))


def random_frames(rng: random.Random) -> bytes:
    out = b""
    for _ in range(rng.randint(0, 10)):
        roll = rng.random()
        if roll < 0.7:
            kind = rng.choice([0x02, 0x07, 0x08, 0x09, 0x14, 0x16, 0x1E, 0x21, 0x2B, 0xFF])
            out += reference.build_frame(kind, random_payload(rng), rng.choice(sorted(reference.SYNC_ADDRESSES)))
        elif roll < 0.8:
            frame = bytearray(reference.build_frame(0x02, random_payload(rng, 15)))
            frame[rng.randrange(2, len(frame))] ^= 0xFF
            out += bytes(frame)
        else:
            out += bytes(rng.choice([0xC8, 0xEA, 0x00, 0x40, rng.randrange(256)]) for _ in range(rng.randint(1, 20)))
    return out


# ----------------------------------------------------------- bytes and frames

@pytest.mark.parametrize("seed", range(20))
def test_streams_parse_alike(seed: int) -> None:
    rng = random.Random(seed)
    parsers = [m.CrsfParser() for m in MODULES]
    for _ in range(60):
        stream = random_frames(rng)
        cut = 0
        while cut < len(stream):
            step = rng.randint(1, 80)
            chunk = stream[cut:cut + step]
            cut += step
            if rng.random() < 0.2:
                chunk = bytearray(chunk)
            got = [([frame_tuple(f) for f in p.feed(chunk)], p.frames_ok, p.crc_errors, bytes(p._buf)) for p in parsers]
            assert got[1] == got[0]
    assert parsers[1].frames_ok > 0 and parsers[1].crc_errors > 0


def test_checksums_and_builders_agree() -> None:
    rng = random.Random(1)
    for _ in range(300):
        data = random_payload(rng)
        assert outcome(reference.crc8_dvb_s2, data) == outcome(rust_side.crc8_dvb_s2, data)
    assert rust_side.crc8_dvb_s2(b"123456789") == reference.crc8_dvb_s2(b"123456789")
    odd = [0, 2, 0x1E, 255, 256, -1, 2.0, True, "x", None]
    payloads = [b"", b"\x01" * 3, b"\x01" * 61, b"\x01" * 62, bytearray(b"ab"), [1, 2, 3], [300], "x", None, 5]
    for kind, payload, address in itertools.product(odd[:6], payloads, [None, 0xC8, 0xEE, 256, "x", -1]):
        args = (kind, payload) if address is None else (kind, payload, address)
        results = [outcome(m.build_frame, *args) for m in MODULES]
        assert results[1] == results[0], args


# ----------------------------------------------------------------- decoders

DECODER_NAMES = ["decode_gps", "decode_vario", "decode_battery", "decode_baro_altitude", "decode_link_statistics",
                 "decode_attitude", "decode_flight_mode"]


@pytest.mark.parametrize("name", DECODER_NAMES)
def test_decoders_agree_on_every_length(name: str) -> None:
    rng = random.Random(name)
    for length in list(range(0, 24)) + [30, 58]:
        for _ in range(40):
            payload = random_payload(rng, length)
            results = [outcome(getattr(m, name), payload) for m in MODULES]
            assert results[1] == results[0], (name, payload)
    for payload in [b"\x00" * 15, b"\xff" * 15, b"\x7f\xff\xff\xff\x7f\xff\xff\xff" + b"\x00" * 7, bytearray(b"\x00" * 15)]:
        results = [outcome(getattr(m, name), payload) for m in MODULES]
        assert results[1] == results[0]


def test_flight_mode_names_agree() -> None:
    for payload in [b"ACRO\x00junk", b"  Mission \x00", b"\x00", b"", b"\xff\xfeAB", b"A\x1fB\x1f", b"\x1f\x1c ", b"HOLD*", b"\x0b\x0cx\x0b"]:
        results = [outcome(m.decode_flight_mode, payload) for m in MODULES]
        assert results[1] == results[0], payload


def test_decode_frame_agrees_on_every_type() -> None:
    rng = random.Random(7)
    for _ in range(1500):
        kind = rng.choice([0x02, 0x07, 0x08, 0x09, 0x14, 0x16, 0x1E, 0x21, rng.randrange(256)])
        frames = [m.CrsfFrame(rng.choice([0xC8, 0xEA]), kind, b"") for m in MODULES]
        payload = random_payload(rng)
        results = [outcome(m.decode_frame, m.CrsfFrame(0xC8, kind, payload)) for m in MODULES]
        assert results[1] == results[0], (kind, payload)
        assert frames[0].type == frames[1].type


# ----------------------------------------------------------------- telemetry

class Clock:
    def __init__(self) -> None:
        self.t = 100.0

    def __call__(self) -> float:
        return self.t


@pytest.mark.parametrize("seed", range(20))
def test_telemetry_snapshots_agree(seed: int) -> None:
    results = []
    for module in MODULES:
        rng = random.Random(seed)
        clock = Clock()
        telemetry = module.ElrsTelemetry(clock)
        log: list = []
        for step in range(120):
            clock.t += rng.choice([0.0, 0.2, 1.0, 4.0, 6.0])
            roll = rng.random()
            if roll < 0.5:
                log.append(telemetry.feed(random_frames(rng)))
            elif roll < 0.8:
                kind = rng.choice([0x02, 0x08, 0x14, 0x1E, 0x21, 0x09, 0x07])
                log.append(outcome(telemetry.apply, module.CrsfFrame(0xC8, kind, random_payload(rng))))
            else:
                log.append(telemetry.feed(b""))
            if step % 3 == 0:
                log.append(_plain(telemetry.snapshot()))
        results.append(log)
    assert results[1] == results[0]


def test_the_streams_reach_a_fix_and_a_stale_position() -> None:
    seen = set()
    for seed in range(20):
        rng = random.Random(seed)
        clock = Clock()
        telemetry = rust_side.ElrsTelemetry(clock)
        for _ in range(120):
            clock.t += rng.choice([0.2, 1.0, 6.0])
            telemetry.feed(random_frames(rng))
            snap = telemetry.snapshot()
            seen.add(("fix" if snap["has_position"] else "nofix", snap["stale"]))
            seen.add(("link", bool(snap["link"])))
            seen.add(("mode", bool(snap["mode_label"])))
    assert {("fix", True), ("fix", False), ("nofix", True), ("link", True), ("mode", True)} <= seen


# -------------------------------------------------------------------- reader

class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage(), bool(record.exc_info)))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    log = logging.getLogger("corvus.elrs")
    previous = log.level
    log.addHandler(handler)
    log.setLevel(logging.DEBUG)
    yield handler
    log.removeHandler(handler)
    log.setLevel(previous)


class Source:
    def __init__(self, script: list[Any]) -> None:
        self.script = list(script)
        self.closed = 0
        self.reads = 0

    def read(self, size: int = 1) -> bytes:
        self.reads += 1
        if not self.script:
            time.sleep(0.01)
            return b""
        item = self.script.pop(0)
        if isinstance(item, BaseException):
            raise item
        return item

    def close(self) -> None:
        self.closed += 1
        if self.closed == 2:
            raise OSError("already closed")


def gps_frame(lat: int, lon: int) -> bytes:
    import struct

    return reference.build_frame(0x02, struct.pack(">iiHHHB", lat, lon, 360, 9000, 1100, 9))


SCRIPTS = {
    "frames": [gps_frame(480000000, 110000000), b"", reference.build_frame(0x21, b"HOLD\x00"), gps_frame(0, 0)],
    "fails": [gps_frame(1, 2), OSError("device gone")],
    "empty": [],
}


@pytest.mark.parametrize("script", sorted(SCRIPTS))
@pytest.mark.parametrize("callback", ["none", "ok", "raises"])
@pytest.mark.parametrize("stop_first", [False, True])
def test_the_reader_agrees(script: str, callback: str, stop_first: bool, monkeypatch: pytest.MonkeyPatch, logs: Logs) -> None:
    results = []
    for module in MODULES:
        logs.lines.clear()
        monkeypatch.setattr(module, "ENABLED", True)
        source = Source(SCRIPTS[script])
        updates: list = []

        def on_update(snapshot: dict, _u: list = updates, _c: str = callback) -> None:
            _u.append((snapshot["position"], snapshot["mode_label"], snapshot["satellites"]))
            if _c == "raises":
                raise RuntimeError("callback broke")

        reader = module.ElrsTelemetryReader(lambda _s=source: _s, on_update=None if callback == "none" else on_update)
        reader.start()
        thread = reader._thread
        names = [t.name for t in threading.enumerate()].count("corvus-elrs")
        first = (thread.name, thread.daemon, *((reader.running, names) if script == "empty" else (None, None)))
        if script == "empty":
            reader.start()
        same = reader._thread is thread if script == "empty" else None
        if not stop_first:
            deadline = time.monotonic() + 3
            while source.script and time.monotonic() < deadline:
                time.sleep(0.01)
            time.sleep(0.15)
        reader.stop()
        reader.stop()
        results.append((first, same, source.closed, reader.running, reader._thread, thread.is_alive(), None if stop_first else updates,
                        "corvus-elrs" in {t.name for t in threading.enumerate()},
                        None if stop_first else [line for line in logs.lines if "failed" in line[1]]))
    assert results[1] == results[0]
    assert results[0][0][:2] == ("corvus-elrs", True)


def test_a_disabled_reader_refuses_alike() -> None:
    results = []
    for module in MODULES:
        reader = module.ElrsTelemetryReader(lambda: Source([]))
        results.append((outcome(reader.start), reader.running))
    assert results[1] == results[0] and results[0][0][0] == "error"


# -------------------------------------------------------------------- fly to

class FlyArgs:
    modes = ["crsf", "mavlink", "elrs", "", None, 3]
    stacks = ["px4", "ardupilot", "PX4", "unknown", None]
    types = [2, 1, 10, 13, 99]
    points = [(48.0, 11.0, 50.0), (0, 0, 0), (91, 0, 5), (0, 181, 5), (-90, -180, -10), ("48.5", "11.5", "20"),
              (True, 1, 1), (1, False, 1), (1, 1, True), (None, 1, 1), ("x", 1, 1), (float("nan"), 1, 1),
              (1, float("inf"), 1), (1, 1, float("-inf")), (10**400, 1, 1), ([], 1, 1), (48.123456789, 11.987654321, 100.5)]
    homes = [None, 0, 545.2, "300", float("nan")]


def test_fly_to_agrees_for_every_combination() -> None:
    for mode, stack, mav_type, point, home in itertools.product(FlyArgs.modes, FlyArgs.stacks, FlyArgs.types, FlyArgs.points, FlyArgs.homes):
        args = (mode, stack, mav_type, *point)
        kwargs = {} if home is None else {"home_alt_amsl": home}
        results = []
        for module in MODULES:
            result = outcome(module.plan_fly_to, *args, **kwargs)
            if result[0] == "ok":
                plan = module.plan_fly_to(*args, **kwargs)
                result = ("ok", _plain([plan.available, plan.reason, plan.command, plan.frame, plan.params, plan.x, plan.y, plan.z, plan.extra]))
            results.append(result)
        assert results[1] == results[0], (args, home)


def test_fly_to_reaches_every_answer() -> None:
    reasons = set()
    for mode, stack, point, home in itertools.product(FlyArgs.modes, FlyArgs.stacks, FlyArgs.points, FlyArgs.homes):
        kwargs = {} if home is None else {"home_alt_amsl": home}
        try:
            plan = rust_side.plan_fly_to(mode, stack, 2, *point, **kwargs)
        except Exception:  # noqa: BLE001
            reasons.add("raised")
            continue
        reasons.add("ok" if plan.available else plan.reason.split(".")[0])
    for expected in ("ok", "raised", "Point is not a number", "Point is outside valid coordinates",
                     "No home altitude yet to convert the height above ground", "Not available over CRSF telemetry"):
        assert any(r.startswith(expected) for r in reasons), (expected, reasons)
