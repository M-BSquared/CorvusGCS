"""corvus_link's ``csv:`` links against pymavlink's ``CSVReader``.

Random CSV files (quoted and unquoted fields, doubled quotes, fields that
span lines, blank lines, CRLF and lone CR line ends, a byte order mark, no
final newline, a quote left open at the end of the file, several separators,
numbers in every spelling Python's ``float`` takes) are read through both and
compared message by message: type, text, timestamp, every field, the count,
a rewind and every ``recv_match`` form. The device string options, the
``CSV_SEPARATOR`` override and the ways opening can fail are compared too.
"""
from __future__ import annotations

import math
import random
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("corvus_link")
pytest.importorskip("pymavlink")
from pymavlink import mavutil as py_mavutil  # noqa: E402

from corvus_link import mavutil as rust_mavutil  # noqa: E402

LINKS = (("pymavlink", py_mavutil), ("corvus_link", rust_mavutil))


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return (type(value).__name__, value)
    if isinstance(value, str) or value is None:
        return value
    return "<" + type(value).__name__ + ">"


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def describe(reader: Any, limit: int = 400) -> list:
    """Everything a consumer can see of a reader: its messages, one by one."""
    out: list = [("headings", _plain(reader.fmt.headings)), ("offsets", reader.fmt.field_offset)]
    for _ in range(limit):
        message = reader.recv_msg()
        if message is None:
            out.append("end")
            break
        entry = [message.get_type(), str(message), _plain(message.line), outcome(lambda m=message: m._timestamp)]
        for name in message.get_fieldnames():
            entry.append(outcome(getattr, message, name))
        entry.append(outcome(getattr, message, "no_such_field"))
        out.append(entry)
    return out


NUMBERS = ["0", "1", "-1", "2.5", "-0.0", "1e3", "1E-3", "nan", "inf", "-inf", "1_0", " 3 ", "0x10", "+7", ".5", "5.",
           "1e999", "١٢", "1,5"]
WORDS = ["abc", "x y", "", " ", "Straße", "é", "true", "None", "a;b", "a,b", 'say "hi"', "line1\nline2", "tab\there", ":"]


def cell(rng: random.Random, sep: str) -> str:
    text = rng.choice(NUMBERS) if rng.random() < 0.6 else rng.choice(WORDS)
    needs_quote = any(c in text for c in (sep, '"', "\n")) or text == ""
    if needs_quote or rng.random() < 0.1:
        return '"' + text.replace('"', '""') + '"'
    return text


def random_csv(rng: random.Random, sep: str) -> str:
    columns = rng.randint(1, 5)
    heads = [rng.choice(["TIME", "A", "B", "x", "line", "fmt", "dup", "dup", "", "N 1", "Ünï"]) for _ in range(columns)]
    heads[0] = rng.choice(["TimeUS", "time", "T"])
    eol = rng.choice(["\n", "\r\n", "\r"])
    rows = [sep.join(heads)]
    for _ in range(rng.randint(0, 8)):
        roll = rng.random()
        if roll < 0.08:
            rows.append("")
            continue
        count = columns + rng.choice([0, 0, 0, -1, 1])
        rows.append(sep.join(cell(rng, sep) for _ in range(max(count, 0))))
    text = eol.join(rows)
    if rng.random() < 0.7:
        text += eol
    if rng.random() < 0.1:
        text += '"open ' + sep + "never closed"
    if rng.random() < 0.08:
        text = "﻿" + text
    return text


def write(tmp_path: Path, text: str, name: str = "log.csv") -> Path:
    path = tmp_path / name
    path.write_bytes(text.encode("utf-8"))
    return path


def open_both(device: str, **kwargs: Any) -> list:
    out = []
    for _name, module in LINKS:
        try:
            out.append(module.mavlink_connection(device, **kwargs))
        except Exception as exc:  # noqa: BLE001 - compared by the caller
            out.append(exc)
    return out


@pytest.mark.parametrize("seed", range(30))
def test_random_files_read_alike(seed: int, tmp_path: Path) -> None:
    rng = random.Random(seed)
    for n in range(12):
        sep = rng.choice([";", ",", "\t", "|", ":", " "])
        text = random_csv(rng, sep)
        path = write(tmp_path, text, f"f{n}.csv")
        device = f"csv:{path}:separator={sep}"
        readers = open_both(device)
        if any(isinstance(r, Exception) for r in readers):
            results = [type(r).__name__ if isinstance(r, Exception) else "opened" for r in readers]
            assert results[1] == results[0], (text, results)
            continue
        py, rust = readers
        assert describe(rust) == describe(py), repr(text)
        assert outcome(lambda r=py: r._count) == outcome(lambda r=rust: r._count), repr(text)
        py.rewind()
        rust.rewind()
        assert describe(rust) == describe(py), repr(text)
        for reader in readers:
            reader.rewind()
        for kind in ("CSV", ["CSV"], {"CSV"}, "OTHER", ("CSV",), None):
            got = []
            for reader in readers:
                reader.rewind()
                seen = []
                while True:
                    m = reader.recv_match(type=kind)
                    if m is None:
                        break
                    seen.append(_plain(m.line))
                got.append(seen)
            assert got[1] == got[0], (kind, text)
        assert py.messages.keys() == rust.messages.keys()


def test_the_fuzz_reaches_the_hard_cases(tmp_path: Path) -> None:
    seen: set[str] = set()
    for seed in range(30):
        rng = random.Random(seed)
        for n in range(12):
            sep = rng.choice([";", ",", "\t", "|", ":", " "])
            text = random_csv(rng, sep)
            path = write(tmp_path, text, f"g{n}.csv")
            try:
                reader = rust_mavutil.mavlink_connection(f"csv:{path}:separator={sep}")
            except Exception as exc:  # noqa: BLE001
                seen.add("error:" + type(exc).__name__)
                continue
            for m in iter(reader.recv_msg, None):
                if any("\n" in x for x in m.line if isinstance(x, str)):
                    seen.add("multiline")
                if any(isinstance(x, str) for x in m.line):
                    seen.add("text")
                if any(isinstance(x, float) for x in m.line):
                    seen.add("number")
                if not m.line:
                    seen.add("empty row")
            if text.endswith('"open ' + sep + "never closed"):
                seen.add("open quote")
    assert {"multiline", "text", "number", "empty row", "open quote"} <= seen, seen


# --------------------------------------------------------- the device string ----

def simple_file(tmp_path: Path) -> Path:
    return write(tmp_path, "TimeUS;A;B\n100;1.5;x\n200;2.5;y\n300;nan;z\n")


@pytest.mark.parametrize("suffix", [
    "", ":separator=;", ":message_type=ROW", ":separator=;:message_type=ROW", ":message_type=Q:separator=;",
    ":separator=ab", ":separator=", ":unknown=1", ":novalue", ":a=b=c", ":separator=,",
])
def test_device_options_agree(suffix: str, tmp_path: Path) -> None:
    path = simple_file(tmp_path)
    readers = open_both(f"csv:{path}{suffix}")
    failed = [isinstance(r, Exception) for r in readers]
    assert failed[0] == failed[1], readers
    if failed[0]:
        assert (type(readers[0]).__name__, str(readers[0])) == (type(readers[1]).__name__, str(readers[1]))
        return
    assert describe(readers[1]) == describe(readers[0])
    assert readers[1].message_type == readers[0].message_type
    assert sorted(readers[1].messages) == sorted(readers[0].messages)


def test_the_colon_separator_syntax_agrees(tmp_path: Path) -> None:
    path = write(tmp_path, "T:A\n1:2\n3:4\n")
    readers = open_both(f"csv:{path}:separator=:")
    assert describe(readers[1]) == describe(readers[0])


def test_the_env_separator_overrides_alike(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = write(tmp_path, "T,A\n1,2\n3,4\n")
    monkeypatch.setenv("CSV_SEPARATOR", ",")
    readers = open_both(f"csv:{path}")
    assert describe(readers[1]) == describe(readers[0])


@pytest.mark.parametrize("content", [None, "", "\n", "TimeUS;A\n", "only;a;header", "﻿TimeUS;A\n1;2\n", "a\n\n\n"])
def test_opening_edge_files_agrees(content: str | None, tmp_path: Path) -> None:
    path = tmp_path / "edge.csv" if content is None else write(tmp_path, content, "edge.csv")
    readers = open_both(f"csv:{path}")
    failed = [isinstance(r, Exception) for r in readers]
    assert failed[0] == failed[1], readers
    if failed[0]:
        assert type(readers[0]).__name__ == type(readers[1]).__name__
    else:
        assert describe(readers[1]) == describe(readers[0])


def test_a_directory_and_no_path_fail_alike(tmp_path: Path) -> None:
    for device in (f"csv:{tmp_path}", "csv:", "CSV:/nonexistent/file.csv"):
        readers = open_both(device)
        names = [type(r).__name__ for r in readers]
        assert names[0] == names[1], (device, readers)


def test_a_huge_field_is_refused_alike(tmp_path: Path) -> None:
    path = write(tmp_path, "T;A\n1;" + "x" * 140000 + "\n")
    readers = open_both(f"csv:{path}")
    names = [type(r).__name__ for r in readers]
    assert names[0] == names[1]
    if not isinstance(readers[0], Exception):
        assert outcome(readers[1].recv_msg) == outcome(readers[0].recv_msg)
    else:
        assert str(readers[0]) == str(readers[1])


def test_expressions_are_the_one_thing_not_built(tmp_path: Path) -> None:
    path = simple_file(tmp_path)
    reader = rust_mavutil.mavlink_connection(f"csv:{path}")
    with pytest.raises(NotImplementedError):
        reader.recv_match(condition="CSV.A > 1")
    with pytest.raises(NotImplementedError):
        reader.check_condition("CSV.A > 1")
    assert reader.check_condition(None) is True
    with_expr = rust_mavutil.mavlink_connection(f"csv:{path}:timestamp_expression=CSV.TimeUS")
    message = with_expr.recv_msg()
    with pytest.raises(NotImplementedError):
        message._timestamp  # noqa: B018 - reading it is the test


def rows_of(module: Any, path: Path) -> Any:
    try:
        reader = module.mavlink_connection(f"csv:{path}:separator=;")
    except Exception as exc:  # noqa: BLE001
        return ("error", type(exc).__name__, str(exc))
    rows = [_plain(reader.fmt.headings)]
    for _ in range(200):
        message = reader.recv_msg()
        if message is None:
            break
        rows.append(_plain(message.line))
    return ("ok", rows)


@pytest.mark.parametrize("seed", range(10))
def test_the_row_state_machine_agrees_on_raw_text(seed: int, tmp_path: Path) -> None:
    rng = random.Random(1000 + seed)
    path = tmp_path / "raw.csv"
    for _ in range(300):
        text = "h1;h2\n" + "".join(rng.choice(['"', ";", "\n", "\r", "\r\n", "a", "b", " ", '""', "1", ".", "e"]) for _ in range(rng.randint(0, 30)))
        path.write_bytes(text.encode())
        assert rows_of(rust_mavutil, path) == rows_of(py_mavutil, path), repr(text)
