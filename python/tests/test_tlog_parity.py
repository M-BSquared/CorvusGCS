"""The Rust tlog against the Python one it replaced.

``tests/reference/tlog_python.py`` keeps the writer and
``tests/reference/mavlink_bridge_tlog_python.py`` the bridge's session handling
as they were in Python. The tests write the same frames through both writers
and compare the files byte for byte (the start time aside), run the same
failures (a full disk on write, on flush, on the header, a writer held
mid-batch while the queue overflows) and compare what each logged and left
behind, and drive both bridges' tlog sessions through the same connections,
disarms and frames.
"""
from __future__ import annotations

import json
import logging
import random
import threading
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

import corvus.tlog as rust_tlog  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from tests.reference import mavlink_bridge_tlog_python as reference_session  # noqa: E402
from tests.reference import tlog_python as reference_tlog  # noqa: E402

WRITERS = (reference_tlog.TlogWriter, rust_tlog.TlogWriter)


class ReferenceBridge(reference_session.TlogSessionMixin, MavlinkBridge):
    """A bridge whose tlog session handling is the Python reference."""


def body(path: Path) -> tuple[dict, bytes]:
    data = path.read_bytes()
    head, _, rest = data.partition(b"\n")
    meta = json.loads(head.removeprefix(b"#CORVUS-TLOG "))
    meta.pop("started_at")
    return meta, rest


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list[tuple[str, str, bool]] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage().split(": /")[0], record.exc_info is not None))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    targets = [logging.getLogger("corvus.tlog"), logging.getLogger("corvus.mavlink")]
    for log in targets:
        log.addHandler(handler)
    yield handler
    for log in targets:
        log.removeHandler(handler)


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize("conn", ["", "udpin:0.0.0.0:14550", "serial:/dev/ttyACM0:115200"])
def test_the_files_match(seed: int, conn: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("corvus.version.get_version", lambda: "2026.10.99")
    results = []
    for n, kind in enumerate(WRITERS):
        rng = random.Random(seed)
        path = tmp_path / f"{n}.tlog"
        writer = kind(str(path))
        writer.set_conn(conn)
        for _ in range(rng.randint(0, 400)):
            writer.write_frame(bytes(rng.randrange(256) for _ in range(rng.randint(1, 280))))
            if rng.random() < 0.02:
                time.sleep(0.002)
        writer.set_conn("too late")
        writer.stop()
        writer.stop()
        writer.write_frame(b"after stop")
        results.append((body(path), writer._file.closed, writer._thread.is_alive(), writer._failed))
    assert results[1] == results[0]


class FullDisk:
    def __init__(self, real: Any, on: set[str]) -> None:
        self.real = real
        self.on = on
        self.closed = False

    def write(self, data: bytes) -> None:
        if "write" in self.on:
            raise OSError(28, "No space left on device")
        self.real.write(data)

    def flush(self) -> None:
        if "flush" in self.on:
            raise OSError(28, "No space left on device")
        self.real.flush()

    def close(self) -> None:
        self.closed = True
        self.real.close()


@pytest.mark.parametrize("on", [{"write"}, {"flush"}, {"write", "flush"}])
@pytest.mark.parametrize("before_header", [True, False])
def test_a_full_disk_ends_alike(on: set[str], before_header: bool, tmp_path: Path, logs: Logs) -> None:
    results = []
    for n, kind in enumerate(WRITERS):
        logs.lines.clear()
        writer = kind(str(tmp_path / f"{n}.tlog"))
        if not before_header:
            writer.write_frame(b"first")
            deadline = time.monotonic() + 2
            while not writer._header_written and time.monotonic() < deadline:
                time.sleep(0.005)
        full = FullDisk(writer._file, on)
        with writer._file_lock:
            writer._file = full
        for i in range(20):
            writer.write_frame(f"F{i}".encode())
            time.sleep(0.001)
        time.sleep(0.05)
        writer.write_frame(b"late")
        writer.stop()
        results.append((full.closed, full.real.closed, writer._failed, sorted(set(logs.lines)),
                        writer._thread.is_alive(), len(writer._buf)))
    assert results[1] == results[0]


@pytest.mark.parametrize("cap", [5, 50])
def test_the_queue_drops_the_oldest_alike(cap: int, tmp_path: Path) -> None:
    results = []
    for n, kind in enumerate(WRITERS):
        path = tmp_path / f"{n}.tlog"
        writer = kind(str(path), max_queue=cap)
        gate = threading.Event()
        original = writer._write_batch

        def gated(batch: list[bytes], _original: Any = original, _gate: threading.Event = gate) -> None:
            _gate.wait(timeout=5)
            _original(batch)

        writer._write_batch = gated
        writer.write_frame(b"SEED|")
        deadline = time.monotonic() + 2
        while writer._buf and time.monotonic() < deadline:
            time.sleep(0.001)
        for i in range(300):
            writer.write_frame(f"F{i:04d}|".encode())
        gate.set()
        writer.stop()
        results.append(body(path)[1])
    assert results[1] == results[0]


# ------------------------------------------------------------ the session

class Frame(SimpleNamespace):
    def get_msgbuf(self) -> Any:
        if isinstance(self.raw, BaseException):
            raise self.raw
        return self.raw


@pytest.mark.parametrize("seed", range(8))
def test_tlog_sessions_match(seed: int, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, logs: Logs) -> None:
    monkeypatch.setattr("corvus.version.get_version", lambda: "2026.10.99")
    results = []
    for n, kind in enumerate((ReferenceBridge, MavlinkBridge)):
        logs.lines.clear()
        rng = random.Random(seed)
        log_dir = tmp_path / str(n)
        monkeypatch.setattr("corvus.mavlink_bridge.default_log_dir", lambda d=log_dir: str(d))
        bridge = kind(VehicleStateStore(), "udpin:0.0.0.0:14550")
        events: list = []
        bridge._request_version = lambda events=events: events.append("version")
        if rng.random() < 0.5:
            bridge.set_frame_sink(lambda raw, events=events: events.append(("sink", raw)))
        for step in range(60):
            roll = rng.random()
            if roll < 0.1:
                bridge._running.set()
                bridge._start_tlog()
            elif roll < 0.2:
                bridge._running.clear()
                bridge._start_tlog()
            elif roll < 0.4:
                bridge._roll_tlog_on_disarm(rng.random() < 0.5)
            elif roll < 0.45:
                bridge._stop_tlog()
            else:
                raw = rng.choice([b"\xfd\x01frame", b"\xfe\x02x", ValueError("no buffer"), None])
                frame = Frame(raw=raw) if rng.random() < 0.9 else SimpleNamespace()
                bridge._write_tlog(frame)
            events.append((step, bridge._tlog is not None, bridge._tlog_armed))
        bridge._stop_tlog()
        files = sorted(log_dir.glob("*.tlog")) if log_dir.exists() else []
        contents = [body(f)[1] for f in files]
        results.append((events, len(files), contents, logs.lines[:]))
    assert results[1] == results[0]


def test_file_names_match_when_the_clock_repeats(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    import datetime

    class Frozen(datetime.datetime):
        @classmethod
        def now(cls, tz: Any = None) -> Any:
            return datetime.datetime(2026, 10, 27, 12, 0, 0, 123456)

    monkeypatch.setattr(datetime, "datetime", Frozen)
    results = []
    for n, kind in enumerate((ReferenceBridge, MavlinkBridge)):
        folder = tmp_path / str(n)
        folder.mkdir()
        names = []
        for _ in range(12):
            path = kind._next_tlog_path(str(folder))
            Path(path).touch()
            names.append(Path(path).name)
        results.append(names)
    assert results[1] == results[0]
    assert results[0][1] == "20261027-120000-123456_01.tlog"


def test_a_log_dir_that_cannot_be_made_disables_the_tlog_alike(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                                logs: Logs) -> None:
    blocker = tmp_path / "file"
    blocker.write_text("not a directory")
    monkeypatch.setattr("corvus.mavlink_bridge.default_log_dir", lambda: str(blocker / "logs"))
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        logs.lines.clear()
        bridge = kind(VehicleStateStore())
        bridge._running.set()
        bridge._start_tlog()
        results.append((bridge._tlog, [(level, text.split(":")[0]) for level, text, _ in logs.lines]))
    assert results[1] == results[0]
    assert results[0][0] is None
