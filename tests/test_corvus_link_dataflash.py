"""corvus_link's DataFlash readers against pymavlink's.

The logs are made here, byte by byte, so every clock and every corner can be
asked for: binary logs with all the format characters, instance fields,
units and multipliers, parameters, flight modes, and the four ways a log
keeps time (microsecond, millisecond, PX4 and interpolated GPS stamps, or
none); damaged binary logs (garbage between messages, a cut last message,
an unknown type, a format with the wrong length); and text logs in both
delimiters. Each is read through pymavlink's ``DFReader`` (in its pure Python
indexing mode, the one corvus_link follows) and through corvus_link, and the
two are compared message by message and on everything the reader exposes
afterwards: parameters, flight modes, formats, units, the index, the last
timestamp, ``recv_match`` in every form, and what each one printed.
"""
from __future__ import annotations

import contextlib
import io
import math
import random
import struct
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("corvus_link")
pytest.importorskip("pymavlink")
from pymavlink import DFReader as df  # noqa: E402
from pymavlink import mavutil as py_mavutil  # noqa: E402

from corvus_link import mavutil as rust_mavutil  # noqa: E402

CODES = df.FORMAT_TO_STRUCT


# ---------------------------------------------------------------- generators ----

class BinLog:
    """Builds a binary DataFlash log."""

    def __init__(self) -> None:
        self.out = bytearray()
        self.defs: dict[str, tuple[int, str, str]] = {}
        self.struct = {0x80: "<BB4s16s64s"}
        self.define(0x80, "FMT", "BBnNZ", "Type,Length,Name,Format,Columns")

    def define(self, tid: int, name: str, fmt: str, cols: str) -> None:
        st = "<" + "".join(CODES[c][0] for c in fmt)
        length = 3 + struct.calcsize(st)
        self.struct[tid] = st
        self.defs[name] = (tid, fmt, st)
        self.raw(0x80, struct.pack(self.struct[0x80], tid, length, name.encode(), fmt.encode(), cols.encode()))

    def raw(self, tid: int, payload: bytes) -> None:
        self.out += bytes([0xA3, 0x95, tid]) + payload

    def add(self, name: str, *values: Any) -> None:
        tid, fmt, st = self.defs[name]
        packed = []
        for _c, v in zip(fmt, values):
            packed.append(v)
        self.raw(tid, struct.pack(st, *packed))

    def bytes(self) -> bytes:
        return bytes(self.out)


def standard_defs(log: BinLog, gps: str = "usec") -> None:
    if gps == "usec":
        log.define(0x81, "GPS", "QBIHLLe", "TimeUS,Status,GMS,GWk,Lat,Lng,Alt")
    elif gps == "msec":
        log.define(0x81, "GPS", "BIHILLe", "Status,TimeMS,Week,T,Lat,Lng,Alt")
    elif gps == "px4":
        log.define(0x81, "GPS", "QQLLe", "GPSTime,TimeUS,Lat,Lng,Alt")
    elif gps == "interp":
        log.define(0x81, "GPS", "BIHLLe", "Status,TimeMS,Week,Lat,Lng,Alt")
    log.define(0x82, "PARM", "QNff", "TimeUS,Name,Value,Default")
    log.define(0x83, "MODE", "QMBB", "TimeUS,Mode,ModeNum,Rsn")
    log.define(0x84, "MSG", "QZ", "TimeUS,Message")
    log.define(0x85, "VER", "QB", "TimeUS,BU")
    log.define(0x86, "UNIT", "QbZ", "TimeUS,Id,Label")
    log.define(0x87, "MULT", "Qbd", "TimeUS,Id,Mult")
    log.define(0x88, "FMTU", "QBNN", "TimeUS,FmtType,UnitIds,MultIds")
    log.define(0x89, "IMU", "QBfff", "TimeUS,I,GyrX,GyrY,GyrZ")
    log.define(0x8A, "ALL", "QbBhHiIfdnNZcCeE", "TimeUS,b,B,h,H,i,I,f,d,n,N,Z,c,C,e,E")
    log.define(0x8D, "ALL2", "QELMqQ", "TimeUS,E2,L,M,q,Q")
    log.define(0x8B, "ARR", "Qa", "TimeUS,Samples")
    log.define(0x8C, "STAT", "QB", "TimeUS,MainState")


def add_units(log: BinLog) -> None:
    log.add("UNIT", 1, ord("s"), b"s")
    log.add("UNIT", 1, ord("r"), b"rad")
    log.add("UNIT", 1, ord("#"), b"instance")
    log.add("UNIT", 1, ord("m"), b"m/s")
    log.add("MULT", 1, ord("-"), 1.0)
    log.add("MULT", 1, ord("c"), 0.01)
    log.add("MULT", 1, ord("m"), 0.001)
    log.add("MULT", 1, ord("x"), 3.5)
    log.add("FMTU", 1, 0x89, b"s#rrr", b"-----")
    log.add("FMTU", 1, 0x8A, b"sss", b"cmx")
    log.add("FMTU", 1, 0x8D, b"s", b"-")
    log.add("FMTU", 1, 0xEE, b"s", b"-")


def body_messages(log: BinLog, rng: random.Random, t0: int, count: int = 30, with_gps: bool = True) -> int:
    t = t0
    log.add("PARM", t, b"RC1_MIN", 1000.0, 1100.0)
    log.add("PARM", t, b"SYSID_THISMAV", 3.0, float("nan"))
    log.add("MSG", t, b"ArduCopter V4.5")
    log.add("VER", t, 2)
    modes = [(0, 0), (5, 5), (4, 4), (99, 99)]
    for _i in range(count):
        t += rng.randint(1000, 90000)
        roll = rng.random()
        if roll < 0.3:
            log.add("IMU", t, rng.randint(0, 2), rng.uniform(-1, 1), rng.uniform(-1, 1), rng.uniform(-1, 1))
        elif roll < 0.4:
            m = rng.choice(modes)
            log.add("MODE", t, m[0], m[1], 2)
        elif roll < 0.5:
            log.add("STAT", t, rng.choice([0, 1, 2, 5, 9]))
        elif roll < 0.6:
            log.add("PARM", t, rng.choice([b"A", b"B", b"C"]), rng.uniform(0, 5), rng.choice([1.5, float("nan")]))
        elif roll < 0.7:
            log.add("ARR", t, struct.pack("<32h", *[rng.randint(-100, 100) for _ in range(32)]))
        elif roll < 0.85:
            log.add("ALL", t, -5, 200, -300, 60000, -70000, 4000000000, 1.5, 2.25, b"abcd", b"sixteen-chars-ok", b"text\x00junk",
                    rng.randint(-500, 500), rng.randint(0, 5000), rng.randint(-100000, 100000), rng.randint(0, 100000))
            log.add("ALL2", t, rng.randint(0, 100000), rng.randint(-900000000, 900000000), 7, -(2**40), 2**41)
        elif with_gps:
            gps_row(log, t, rng)
    return t


def gps_row(log: BinLog, t: int, rng: random.Random) -> None:
    _tid, fmt, _st = log.defs["GPS"]
    if fmt == "QBIHLLe":
        log.add("GPS", t, 3, 100000 + t // 1000, 2300, 480000000 + rng.randint(0, 1000), 110000000, 52000)
    elif fmt == "BIHILLe":
        log.add("GPS", 3, 50000 + t // 1000, 2300, t // 1000, 480000000, 110000000, 52000)
    elif fmt == "QQLLe":
        log.add("GPS", 1_700_000_000_000_000 + t, t, 480000000, 110000000, 52000)
    else:
        log.add("GPS", 3, 50000 + t // 1000 * 7, 2300, 480000000, 110000000, 52000)


def make_binary(kind: str, seed: int) -> bytes:
    rng = random.Random(seed)
    log = BinLog()
    if kind in ("usec", "usec-late-gps", "usec-no-gps", "usec-bad-week"):
        standard_defs(log, "usec")
        add_units(log)
        t = 2_000_000
        if kind == "usec-no-gps":
            body_messages(log, rng, t, with_gps=False)
        elif kind == "usec-bad-week":
            log.add("GPS", t, 0, 0, 0, 0, 0, 0)
            body_messages(log, rng, t + 1000)
        elif kind == "usec-late-gps":
            t = body_messages(log, rng, t, 12, with_gps=False)
            gps_row(log, t + 5000, rng)
            body_messages(log, rng, t + 10000)
        else:
            gps_row(log, t, rng)
            body_messages(log, rng, t + 1000)
    elif kind in ("msec", "msec-no-gps"):
        standard_defs(log, "msec")
        log.define(0x8E, "ATT", "IBH", "TimeMS,Pitch,Yaw")
        add_units(log)
        for i in range(25):
            log.add("ATT", 1000 + i * 40, i % 7, i)
            if kind == "msec" and i == 3:
                gps_row(log, 40_000_000, rng)
        body_messages(log, rng, 5000, 10, with_gps=False)
    elif kind in ("px4", "px4-no-time"):
        standard_defs(log, "px4")
        log.define(0x8F, "TIME", "Q", "StartTime")
        if kind == "px4":
            log.add("TIME", 1_000_000)
        gps_row(log, 5000, rng)
        body_messages(log, rng, 10000, 10)
    elif kind in ("interp", "interp-single"):
        standard_defs(log, "interp")
        log.define(0x90, "IMU2", "BfH", "Cnt,Val,Num")
        for i in range(30):
            log.add("IMU2", i, i * 0.5, i)
            if i % 7 == 3 and (kind == "interp" or i == 3):
                gps_row(log, i * 90000, rng)
        body_messages(log, rng, 100000, 6, with_gps=False)
    else:
        raise ValueError(kind)
    return log.bytes()


BINARY_KINDS = ["usec", "usec-late-gps", "usec-no-gps", "usec-bad-week", "msec", "msec-no-gps", "px4", "px4-no-time",
                "interp", "interp-single"]


def damage(data: bytes, how: str, rng: random.Random) -> bytes:
    data = bytearray(data)
    if how == "garbage-between":
        for _ in range(4):
            at = rng.randrange(200, len(data))
            data[at:at] = bytes(rng.randrange(256) for _ in range(rng.randint(1, 40)))
    elif how == "cut":
        del data[len(data) - rng.randint(1, 30):]
    elif how == "trailing-garbage":
        data += bytes(rng.randrange(256) for _ in range(700))
    elif how == "zero-block":
        at = rng.randrange(300, max(301, len(data) - 100))
        data[at:at + 64] = bytes(64)
    elif how == "unknown-type":
        at = len(data) // 2
        data[at:at] = bytes([0xA3, 0x95, 0xF0]) + bytes(20)
    elif how == "bad-fmt":
        data += bytes([0xA3, 0x95, 0x80]) + struct.pack("<BB4s16s64s", 0x70, 9, b"BAD", b"Q?Q", b"A,B,C")
    elif how == "no-header":
        data = bytearray(rng.randrange(256) for _ in range(300))
    elif how == "short-fmt-len":
        data += bytes([0xA3, 0x95, 0x80]) + struct.pack("<BB4s16s64s", 0x71, 6, b"SHRT", b"QQ", b"A,B")
        data += bytes([0xA3, 0x95, 0x71]) + struct.pack("<QQ", 1, 2)
    return bytes(data)


DAMAGE = ["garbage-between", "cut", "trailing-garbage", "zero-block", "unknown-type", "bad-fmt", "short-fmt-len"]


def make_text(kind: str, seed: int) -> str:
    rng = random.Random(seed)
    sep = ", " if "comma-only" not in kind else ","
    lines = [sep.join(["FMT", "128", "89", "FMT", "BBnNZ", "Type,Length,Name,Format,Columns"])]

    def fmt(tid: int, name: str, f: str, cols: str) -> None:
        lines.append(sep.join(["FMT", str(tid), "30", name, f, cols]))

    fmt(129, "GPS", "QBIHLLe", "TimeUS,Status,GMS,GWk,Lat,Lng,Alt")
    fmt(130, "PARM", "QNff", "TimeUS,Name,Value,Default")
    fmt(131, "MODE", "QMBB", "TimeUS,Mode,ModeNum,Rsn")
    fmt(132, "MSG", "QZ", "TimeUS,Message")
    fmt(133, "UNIT", "QbZ", "TimeUS,Id,Label")
    fmt(134, "MULT", "Qbd", "TimeUS,Id,Mult")
    fmt(135, "FMTU", "QBNN", "TimeUS,FmtType,UnitIds,MultIds")
    fmt(136, "IMU", "QBfc", "TimeUS,I,GyrX,Gyr")
    fmt(137, "EMPTY", "", "")
    if kind != "text-no-units":
        lines.append(sep.join(["UNIT", "10", str(ord("s")), "s"]))
        lines.append(sep.join(["MULT", "10", str(ord("c")), "0.01"]))
        lines.append(sep.join(["FMTU", "10", "136", "s#-c", "-c--"]))
    lines.append(sep.join(["PARM", "5", "RC1_MIN", "1000", "1100"]))
    lines.append(sep.join(["MSG", "6", "ArduPlane V4.5"]))
    t = 1_000_000
    if kind != "text-no-gps":
        lines.append(sep.join(["GPS", str(t), "3", "345600000", "2300", "480000000", "110000000", "52000"]))
    for i in range(20):
        t += rng.randint(1000, 90000)
        roll = rng.random()
        if roll < 0.4:
            lines.append(sep.join(["IMU", str(t), str(rng.randint(0, 2)), f"{rng.uniform(-1, 1):.4f}", str(rng.randint(-500, 500))]))
        elif roll < 0.55:
            lines.append(sep.join(["MODE", str(t), rng.choice(["Stabilize", "auto", "Loiter"]), "5", "2"]))
        elif roll < 0.65:
            lines.append(sep.join(["PARM", str(t), "P" + str(i), f"{rng.uniform(0, 9):.3f}", "nan"]))
        elif roll < 0.75:
            lines.append("EMPTY, ,")
        elif roll < 0.85:
            lines.append("")
        elif roll < 0.9:
            lines.append(sep.join(["IMU", str(t)]))
        elif roll < 0.95:
            lines.append(sep.join(["UNKNOWN", "1", "2"]))
        else:
            lines.append(sep.join(["GPS", str(t), "3", str(345600000 + i), "2300", "480000000", "110000000", "52000"]))
    text = "\n".join(lines)
    if kind != "text-no-newline":
        text += "\n"
    return text


TEXT_KINDS = ["text", "text-comma-only", "text-no-gps", "text-no-units", "text-no-newline"]


# --------------------------------------------------------------------- compare ----

def _plain(value: Any) -> Any:
    if isinstance(value, float):
        return "nan" if math.isnan(value) else ("float", value)
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        return ("int", value)
    if isinstance(value, bytes):
        return ("bytes", value)
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, dict):
        return {str(k): _plain(v) for k, v in value.items()}
    if hasattr(value, "tolist") and type(value).__name__ == "array":
        return ("array", value.tolist())
    if isinstance(value, str) or value is None:
        return value
    return "<" + type(value).__name__ + ">"


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def open_reader(module: Any, path: Path, zero: bool, monkeypatch: pytest.MonkeyPatch) -> tuple[Any, str]:
    """Open *path*; the text printed while doing so comes back with the reader."""
    monkeypatch.setenv("PYMAVLINK_FAST_INDEX", "0")
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        reader = module.mavlink_connection(str(path), zero_time_base=zero)
    return reader, out.getvalue() + "|" + err.getvalue()


def run_capturing(call: Any) -> tuple[Any, str]:
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        result = outcome(call)
    return result, out.getvalue() + "|" + err.getvalue()


def message_view(m: Any) -> list:
    view: list = [m.get_type(), str(m), _plain(m._timestamp), _plain(list(m.get_fieldnames()))]
    view.append(outcome(m.to_dict))
    view.append(outcome(m.get_msgbuf))
    dump = io.StringIO()
    view.append(outcome(lambda: (m.dump_verbose(dump), dump.getvalue().split(": ", 1)[1] if ": " in dump.getvalue() else dump.getvalue())[1]))
    view.append(outcome(lambda: m[0].get_type()))
    view.append(outcome(getattr, m, "NoSuchField"))
    return view


def read_all(reader: Any, limit: int = 400) -> list:
    out: list = []
    for _ in range(limit):
        captured = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(captured[0]), contextlib.redirect_stderr(captured[1]):
            try:
                m = reader.recv_msg()
            except Exception as exc:  # noqa: BLE001
                out.append(("raised", type(exc).__name__, str(exc)))
                break
        printed = captured[0].getvalue() + "|" + captured[1].getvalue()
        if m is None:
            out.append(("end", printed))
            break
        out.append((message_view(m), printed))
    return out


def state_view(reader: Any) -> dict:
    return {
        "params": _plain(reader.params),
        "param_defaults": outcome(lambda: reader.param_defaults),
        "mav_type": reader.mav_type,
        "flightmode": reader.flightmode,
        "message_keys": sorted(reader.messages.keys()),
        "formats": {str(k): [v.name, _plain(v.type), _plain(v.len), v.format, list(v.columns), v.instance_field, _plain(v.units),
                              _plain(getattr(v, "instance_ofs", None)), _plain(getattr(v, "instance_len", None)), v.msg_struct]
                    for k, v in reader.formats.items()},
        "unit_lookup": _plain(reader.unit_lookup),
        "mult_lookup": _plain(reader.mult_lookup),
        "name_to_id": _plain(reader.name_to_id),
        "id_to_name": _plain(reader.id_to_name),
        "count": reader._count,
        "counts": _plain(reader.counts),
        "offsets": _plain(reader.offsets),
        "data_len": reader.data_len,
    }


def compare_readers(py: Any, rust: Any, with_modes: bool = True) -> None:
    assert state_view(rust) == state_view(py)
    py_read, rust_read = read_all(py), read_all(rust)
    assert rust_read == py_read
    assert state_view(rust) == state_view(py)
    assert _plain(rust.percent) == _plain(py.percent)
    py.rewind()
    rust.rewind()
    assert outcome(rust.param, "RC1_MIN") == outcome(py.param, "RC1_MIN")
    assert outcome(rust.param, "missing", "dflt") == outcome(py.param, "missing", "dflt")
    for kind in ("GPS", ["GPS", "MODE"], {"PARM", "MSG"}, "NOPE", ["IMU"], "ALL"):
        for strict in (False, True):
            got = []
            for reader in (py, rust):
                reader.rewind()
                seen = []
                while True:
                    m, printed = run_capturing(lambda r=reader, k=kind, s=strict: r.recv_match(type=k, strict=s))
                    if m[0] != "ok" or m[1] is None:
                        seen.append(m)
                        break
                    seen.append(printed)
                    break
                reader.rewind()
                matches = []
                while True:
                    try:
                        m = reader.recv_match(type=kind, strict=strict)
                    except Exception as exc:  # noqa: BLE001
                        matches.append(("raised", type(exc).__name__))
                        break
                    if m is None:
                        break
                    matches.append((m.get_type(), str(m), _plain(m._timestamp)))
                    if len(matches) > 80:
                        break
                got.append((seen, matches, reader.flightmode, _plain(reader.params)))
            assert got[1] == got[0], (kind, strict)
    if with_modes:
        assert outcome(rust.flightmode_list) == outcome(py.flightmode_list)
        assert outcome(rust.last_timestamp) == outcome(py.last_timestamp)
        assert _plain(rust._flightmodes) == _plain(py._flightmodes)


# ------------------------------------------------------------------------ tests ----

def write(tmp_path: Path, name: str, data: bytes | str) -> Path:
    path = tmp_path / name
    path.write_bytes(data if isinstance(data, bytes) else data.encode())
    return path


@pytest.mark.parametrize("zero", [False, True])
@pytest.mark.parametrize("kind", BINARY_KINDS)
@pytest.mark.parametrize("seed", range(3))
def test_binary_logs_read_alike(kind: str, seed: int, zero: bool, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = write(tmp_path, "log.bin", make_binary(kind, seed))
    (py, py_print), (rust, rust_print) = (open_reader(m, path, zero, monkeypatch) for m in (py_mavutil, rust_mavutil))
    assert rust_print == py_print
    assert type(rust).__name__ == "DFReader_binary"
    compare_readers(py, rust)


@pytest.mark.parametrize("how", DAMAGE)
@pytest.mark.parametrize("kind", ["usec", "msec", "interp"])
@pytest.mark.parametrize("seed", range(3))
def test_damaged_binary_logs_read_alike(how: str, kind: str, seed: int, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    data = damage(make_binary(kind, seed), how, random.Random(seed * 31 + 7))
    path = write(tmp_path, "bad.bin", data)
    results = []
    for module in (py_mavutil, rust_mavutil):
        try:
            reader, printed = open_reader(module, path, False, monkeypatch)
        except Exception as exc:  # noqa: BLE001
            results.append(("open-error", type(exc).__name__, str(exc)))
            continue
        results.append((printed, read_all(reader), _plain(reader.params), reader.flightmode))
    assert results[1] == results[0]


@pytest.mark.parametrize("zero", [False, True])
@pytest.mark.parametrize("kind", TEXT_KINDS)
@pytest.mark.parametrize("seed", range(3))
def test_text_logs_read_alike(kind: str, seed: int, zero: bool, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = write(tmp_path, "log.log", make_text(kind, seed))
    results = []
    for module in (py_mavutil, rust_mavutil):
        try:
            reader, printed = open_reader(module, path, zero, monkeypatch)
        except Exception as exc:  # noqa: BLE001
            results.append(("open-error", type(exc).__name__, str(exc)))
            continue
        results.append((type(reader).__name__, printed, read_all(reader), state_view(reader)))
    assert results[1] == results[0]


def test_text_logs_reach_a_reader(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = write(tmp_path, "log.log", make_text("text", 1))
    reader, _ = open_reader(rust_mavutil, path, False, monkeypatch)
    assert type(reader).__name__ == "DFReader_text"
    messages = [m for m in iter(reader.recv_msg, None)]
    assert {m.get_type() for m in messages} >= {"GPS", "IMU", "PARM"}


@pytest.mark.parametrize("kind", ["usec", "msec", "px4", "interp"])
def test_the_fast_indexer_agrees_on_clean_logs(kind: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = write(tmp_path, "clean.bin", make_binary(kind, 4))
    monkeypatch.setenv("PYMAVLINK_FAST_INDEX", "1")
    py = py_mavutil.mavlink_connection(str(path))
    rust, _ = open_reader(rust_mavutil, path, False, monkeypatch)
    assert read_all(rust) == read_all(py)


def test_the_logs_reach_every_clock_and_corner(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    seen: set[str] = set()
    for kind in BINARY_KINDS:
        path = write(tmp_path, f"{kind}.bin", make_binary(kind, 1))
        reader, _ = open_reader(rust_mavutil, path, False, monkeypatch)
        times = [m._timestamp for m in iter(reader.recv_msg, None) if m.get_fieldnames()]
        seen.add("float stamps" if any(isinstance(t, float) and t > 1e9 for t in times) else "relative stamps")
        if any(t == 0 for t in times):
            seen.add("int zero stamps")
        seen.add("modes" if reader.flightmode != "UNKNOWN" else "no modes")
        seen.add(f"instances:{any('IMU[' in k for k in reader.messages)}")
        seen.add(f"units:{any(f.units for f in reader.formats.values())}")
    assert {"float stamps", "relative stamps", "modes", "instances:True", "units:True"} <= seen, seen


def test_a_reader_is_a_context_manager_and_closes(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = write(tmp_path, "ctx.bin", make_binary("usec", 2))
    for module in (py_mavutil, rust_mavutil):
        reader, _ = open_reader(module, path, False, monkeypatch)
        with reader as r:
            assert r.recv_msg() is not None
        assert outcome(reader.recv_msg)[0] == "error"


def test_empty_and_missing_files_fail_alike(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    for name, data in (("empty.bin", b""), ("empty.log", b"")):
        path = write(tmp_path, name, data)
        results = []
        for module in (py_mavutil, rust_mavutil):
            results.append(outcome(lambda m=module, p=path: m.mavlink_connection(str(p))))
        assert results[1][0] == results[0][0], name
    missing = tmp_path / "missing.bin"
    errs = [outcome(lambda m=m: m.mavlink_connection(str(missing))) for m in (py_mavutil, rust_mavutil)]
    assert errs[1] == errs[0]


def test_is_text_log_agrees(tmp_path: Path) -> None:
    cases = {
        "a.log": b"FMT, 128, 89, FMT, BBnNZ, Type,Length,Name,Format,Columns\n",
        "b.log": b"x" * 5000 + b"FMT,",
        "c.log": b"x" * 9000 + b"FMT,",
        "d.log": b"not a dataflash log\n",
    }
    for name, data in cases.items():
        path = write(tmp_path, name, data)
        assert rust_mavutil.DFReader_is_text_log(str(path)) == df.DFReader_is_text_log(str(path)), name


def test_expressions_are_the_one_thing_not_built(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = write(tmp_path, "e.bin", make_binary("usec", 2))
    reader, _ = open_reader(rust_mavutil, path, False, monkeypatch)
    with pytest.raises(NotImplementedError):
        reader.recv_match(condition="GPS.Lat > 1")
    assert reader.check_condition(None) is True


@pytest.mark.parametrize("seed", range(40))
def test_random_logs_read_alike(seed: int, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    rng = random.Random(9000 + seed)
    kind = rng.choice(BINARY_KINDS)
    data = make_binary(kind, rng.randrange(1000))
    if rng.random() < 0.6:
        for _ in range(rng.randint(1, 3)):
            data = damage(data, rng.choice(DAMAGE[:5]), rng)
    path = write(tmp_path, "random.bin", data)
    zero = rng.random() < 0.3
    results = []
    for module in (py_mavutil, rust_mavutil):
        try:
            reader, printed = open_reader(module, path, zero, monkeypatch)
        except Exception as exc:  # noqa: BLE001
            results.append(("open-error", type(exc).__name__, str(exc)))
            continue
        results.append((printed, read_all(reader), state_view(reader)))
    assert results[1] == results[0]


def test_a_format_of_no_length_ends_the_index_instead_of_hanging(tmp_path: Path) -> None:
    """pymavlink indexes such a log forever; corvus_link stops at the format."""
    import subprocess
    import sys

    log = BinLog()
    log.define(0x81, "ZERO", "QQ", "TimeUS,Value")
    log.raw(0x80, struct.pack("<BB4s16s64s", 0x82, 0, b"EMPTY", b"Q", b"TimeUS"))
    log.raw(0x82, b"")
    log.raw(0x82, b"")
    path = write(tmp_path, "zero.bin", log.bytes())
    code = (
        "import sys; from corvus_link import mavutil; r = mavutil.mavlink_connection(sys.argv[1]); "
        "print('opened', r.data_len > 0)"
    )
    done = subprocess.run([sys.executable, "-c", code, str(path)], capture_output=True, text=True, timeout=30)
    assert done.returncode == 0 and "opened True" in done.stdout, done.stderr
