"""Tlog writer (F2) tests — raw MAVLink frame logging to disk.

Covers the ``TlogWriter`` metadata header (with the GCS version from the
single-source ``corvus.version``), raw-frame round-trip, the bounded
drop-oldest queue, idempotent ``stop()``, and the bridge integration: the
connect path starts a tlog, the receive loop writes raw frames, and ``stop()``
joins the writer thread and closes the file. All hermetic (``tmp_path``, no
real sockets, the log dir monkeypatched).
"""
from __future__ import annotations

import datetime
import pathlib
import threading
import time
from types import SimpleNamespace
from typing import Any

import pytest

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore
from corvus.tlog import TlogWriter


# ---------------------------------------------------------------------------
# TlogWriter — metadata header
# ---------------------------------------------------------------------------

def test_metadata_header_contains_product_version_and_format(
    tmp_path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The first line is the magic-prefixed JSON header with the GCS version."""
    monkeypatch.setattr("corvus.version.get_version", lambda: "9.9.9-test")
    path = tmp_path / "session.tlog"
    writer = TlogWriter(str(path))
    try:
        writer.set_conn("udp:0.0.0.0:14540")
        # Header is written lazily on the first frame.
        writer.write_frame(b"\xfe\x09\x00")
    finally:
        writer.stop()

    data = path.read_bytes()
    assert data.startswith(b"#CORVUS-TLOG ")
    header_line = data.split(b"\n", 1)[0]
    assert b'"product": "Corvus GCS"' in header_line
    assert b'"version": "9.9.9-test"' in header_line
    assert b'"conn": "udp:0.0.0.0:14540"' in header_line
    assert b'"format": "mavlink-raw"' in header_line
    assert b'"started_at":' in header_line


def test_empty_tlog_still_has_header(tmp_path) -> None:
    """stop() before any frame still writes the header for attributability."""
    path = tmp_path / "empty.tlog"
    writer = TlogWriter(str(path))
    writer.stop()

    data = path.read_bytes()
    assert data.startswith(b"#CORVUS-TLOG ")
    assert data.endswith(b"\n")
    # Header is the entire content: no frames were enqueued.
    assert data.count(b"\n") == 1


# ---------------------------------------------------------------------------
# TlogWriter — raw-frame round-trip
# ---------------------------------------------------------------------------

def test_write_frame_round_trips_raw_bytes(tmp_path) -> None:
    """Raw frame bytes follow the header line verbatim."""
    path = tmp_path / "roundtrip.tlog"
    writer = TlogWriter(str(path))
    frame = b"\xfe\x09\x00\x00\x00\x01\x02\x03\x04\x05\x06\x07"
    try:
        writer.write_frame(frame)
    finally:
        writer.stop()

    body = path.read_bytes().split(b"\n", 1)[1]
    assert body == frame


def test_write_frame_preserves_frame_order(tmp_path) -> None:
    """Multiple frames are written in arrival order."""
    path = tmp_path / "order.tlog"
    writer = TlogWriter(str(path))
    frames = [bytes([0xAB, i, i ^ 0xFF]) for i in range(50)]
    try:
        for f in frames:
            writer.write_frame(f)
    finally:
        writer.stop()

    body = path.read_bytes().split(b"\n", 1)[1]
    assert body == b"".join(frames)


def test_write_frame_after_stop_is_dropped(tmp_path) -> None:
    """A stop()d writer discards further frames without raising."""
    path = tmp_path / "late.tlog"
    writer = TlogWriter(str(path))
    writer.write_frame(b"FIRST")
    writer.stop()
    # Must not raise; the frame is silently dropped.
    writer.write_frame(b"LATE")
    body = path.read_bytes().split(b"\n", 1)[1]
    assert b"FIRST" in body
    assert b"LATE" not in body


# ---------------------------------------------------------------------------
# TlogWriter — writer thread drain + close
# ---------------------------------------------------------------------------

def test_stop_drains_and_closes(tmp_path) -> None:
    """stop() flushes pending frames, joins the writer, and closes the file."""
    path = tmp_path / "drain.tlog"
    writer = TlogWriter(str(path))
    thread = writer._thread
    frame = b"\xfe" * 64
    for _ in range(100):
        writer.write_frame(frame)

    writer.stop()

    assert not thread.is_alive()
    assert writer._file.closed
    body = path.read_bytes().split(b"\n", 1)[1]
    # All 100 frames were drained before close.
    assert body.count(frame) == 100


def test_stop_is_idempotent(tmp_path) -> None:
    """Calling stop() multiple times is safe and does not raise."""
    path = tmp_path / "idem.tlog"
    writer = TlogWriter(str(path))
    writer.write_frame(b"X")
    writer.stop()
    writer.stop()
    writer.stop()
    assert not writer._thread.is_alive()
    assert writer._file.closed


def test_context_manager_stops_on_exit(tmp_path) -> None:
    """__exit__ calls stop(); the writer thread is gone after the with-block."""
    path = tmp_path / "ctx.tlog"
    with TlogWriter(str(path)) as writer:
        writer.write_frame(b"Y")
    assert not writer._thread.is_alive()
    assert writer._file.closed
    assert b"Y" in path.read_bytes().split(b"\n", 1)[1]


# ---------------------------------------------------------------------------
# TlogWriter — drop-oldest bounded queue
# ---------------------------------------------------------------------------

def test_drop_oldest_when_queue_full(tmp_path) -> None:
    """A full buffer drops the oldest frames; recent frames survive.

    Uses a tiny deque cap and a gated writer so the fill is deterministic: the
    writer drains the seed frame then blocks, the flood overflows the deque
    (dropping oldest), and releasing the gate writes the survivors.
    """
    path = tmp_path / "drop.tlog"
    writer = TlogWriter(str(path), max_queue=100)
    gate = threading.Event()
    original = writer._write_batch  # bound method

    def gated(batch: list[bytes]) -> None:
        gate.wait(timeout=5)
        original(batch)

    writer._write_batch = gated  # type: ignore[assignment]
    try:
        writer.write_frame(b"SEED|")
        # Wait until the writer drained the seed, then blocked inside `gated`.
        deadline = time.monotonic() + 2.0
        while writer._buf and time.monotonic() < deadline:
            time.sleep(0.001)
        assert not writer._buf  # writer is now parked in gated()
        # Flood well past the 100-frame cap; oldest frames are dropped.
        for i in range(1000):
            writer.write_frame(f"F{i:04d}|".encode())
        gate.set()
    finally:
        writer.stop()

    body = path.read_bytes().split(b"\n", 1)[1]
    # Most recent frame survived (oldest-drop preserves recent data).
    assert b"F0999|" in body
    # The first flooded frame was dropped once the deque overflowed.
    assert b"F0000|" not in body
    # Far fewer than 1000 frames were written: SEED + at most 100 survivors.
    assert 0 < body.count(b"|") < 1000


def test_write_frame_never_blocks_on_full_queue(tmp_path) -> None:
    """Flooding a full deque returns promptly (non-blocking producer)."""
    path = tmp_path / "flood.tlog"
    writer = TlogWriter(str(path), max_queue=50)
    gate = threading.Event()
    writer._write_batch = lambda batch: gate.wait(timeout=5)  # type: ignore[assignment]
    try:
        start = time.monotonic()
        # 10x the cap; each append is O(1) and drops oldest when full.
        for i in range(500):
            writer.write_frame(f"F{i:04d}|".encode())
        elapsed = time.monotonic() - start
        gate.set()
        # No deadlock and no blocking put: a tight 500-iter loop is fast.
        assert elapsed < 2.0
    finally:
        writer.stop()


# ---------------------------------------------------------------------------
# Bridge integration — connect path, receive loop, stop()
# ---------------------------------------------------------------------------

class _FakeTlogMsg(SimpleNamespace):
    """A MAVLink message stand-in carrying raw wire bytes via get_msgbuf()."""

    def get_type(self) -> str:
        return self.message_type

    def get_msgbuf(self) -> bytes:
        return self._raw  # type: ignore[attr-defined]


class _FakeConn:
    """Minimal recv/close stand-in; the tlog path touches only these."""

    def __init__(self) -> None:
        self.closed = False

    def close(self) -> None:
        self.closed = True


class _FakeClock:
    """Controllable wall clock for the store's is_stale() (time.time)."""

    def __init__(self, start: float = 100.0) -> None:
        self.now = start

    def time(self) -> float:
        return self.now


def test_bridge_connect_starts_tlog_in_log_dir(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    """The connect-path tlog start writes into default_log_dir (monkeypatched)."""
    monkeypatch.setattr("corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path))
    store = VehicleStateStore()
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")
    bridge._running.set()
    bridge._target_system = 1
    bridge._target_component = 1

    bridge._start_tlog()

    assert bridge._tlog is not None
    files = list(tmp_path.glob("*.tlog"))
    assert len(files) == 1
    # Clean up the writer thread.
    bridge.stop()
    assert bridge._tlog is None
    # The header records the live connection string.
    header = files[0].read_bytes().split(b"\n", 1)[0]
    assert b'"conn": "udp:0.0.0.0:14540"' in header


def test_bridge_tlog_disabled_when_not_running(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    """_start_tlog is a no-op when _running is clear (hermetic direct-_connect)."""
    monkeypatch.setattr("corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path))
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    assert not bridge._running.is_set()

    bridge._start_tlog()

    assert bridge._tlog is None
    assert list(tmp_path.glob("*.tlog")) == []


def test_bridge_tlog_log_dir_failure_never_breaks_connection(
    tmp_path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A log-dir permission error disables the tlog, not the link."""
    monkeypatch.setattr(
        "corvus.mavlink_bridge.default_log_dir",
        lambda: str(tmp_path / "nope"),
    )
    # Make the dir creation fail (parent is a file, not a dir).
    blocker = tmp_path / "nope"
    blocker.write_text("not a dir")
    store = VehicleStateStore()
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")
    bridge._running.set()

    bridge._start_tlog()  # must not raise

    assert bridge._tlog is None  # tlog disabled, link unaffected
    bridge.stop()


def test_bridge_receive_loop_writes_raw_frame_to_tlog(
    tmp_path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A received frame is written raw to the tlog; stop() joins the writer."""
    monkeypatch.setattr("corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path))
    # Pin the store clock so is_stale() returns False inside the receive loop.
    clock = _FakeClock()
    monkeypatch.setattr("corvus.state_store.time.time", clock.time)

    store = VehicleStateStore()
    store.heartbeat()  # last_heartbeat == clock.now (100.0)
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._running.set()
    # Start the tlog the way _connect would (the connect-path action).
    bridge._start_tlog()
    assert bridge._tlog is not None
    tlog = bridge._tlog
    tlog_thread = tlog._thread

    raw = b"\xfe\x09\x00\x00\x00\x01\x02\x03\x04\x05\x06\x07"
    msg = _FakeTlogMsg(message_type="UNKNOWN", _raw=raw)
    conn = _FakeConn()
    calls = {"n": 0}

    def fake_recv(blocking: bool, timeout: float) -> Any:
        calls["n"] += 1
        if calls["n"] == 1:
            return msg
        bridge._running.clear()  # exit the loop after one message
        return None

    conn.recv_match = fake_recv  # type: ignore[assignment]
    bridge._conn = conn

    # Drives _write_tlog(msg) then _dispatch(msg) (unknown type = no-op).
    bridge._receive_loop()

    bridge.stop()

    # The tlog writer thread was joined and the file closed.
    assert bridge._tlog is None
    assert not tlog_thread.is_alive()
    assert tlog._file.closed

    files = list(tmp_path.glob("*.tlog"))
    assert len(files) == 1
    data = files[0].read_bytes()
    # Header first, then the raw frame verbatim.
    assert data.startswith(b"#CORVUS-TLOG ")
    body = data.split(b"\n", 1)[1]
    assert raw in body


def test_bridge_reconnect_cycle_closes_old_tlog_and_starts_new(
    tmp_path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Each connect cycle closes the prior tlog; a fresh file is opened next."""
    monkeypatch.setattr("corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path))
    store = VehicleStateStore()
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._running.set()

    # First session.
    bridge._start_tlog()
    first = bridge._tlog
    assert first is not None
    first.write_frame(b"AAA")
    # _run's per-cycle teardown closes it.
    bridge._stop_tlog()
    assert bridge._tlog is None
    assert not first._thread.is_alive()

    # Second session opens a brand-new file.
    bridge._start_tlog()
    second = bridge._tlog
    assert second is not None and second is not first
    second.write_frame(b"BBB")
    bridge.stop()

    files = sorted(tmp_path.glob("*.tlog"))
    assert len(files) == 2
    bodies = [f.read_bytes().split(b"\n", 1)[1] for f in files]
    assert b"AAA" in bodies[0]
    assert b"BBB" in bodies[1]


def test_bridge_tlog_write_failure_never_breaks_recv_loop(
    tmp_path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A tlog write exception is swallowed; the recv loop keeps going."""
    monkeypatch.setattr("corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path))
    clock = _FakeClock()
    monkeypatch.setattr("corvus.state_store.time.time", clock.time)

    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._running.set()
    bridge._start_tlog()
    tlog = bridge._tlog
    assert tlog is not None
    # Force every write_frame to blow up.
    def boom(_raw: bytes) -> None:
        raise OSError("disk full")
    tlog.write_frame = boom  # type: ignore[assignment]

    good = b"\xfe\x09\x01"
    msgs = [
        _FakeTlogMsg(message_type="UNKNOWN", _raw=good),
        _FakeTlogMsg(message_type="UNKNOWN", _raw=good),
    ]
    conn = _FakeConn()
    idx = {"i": 0}

    def fake_recv(blocking: bool, timeout: float) -> Any:
        idx["i"] += 1
        if idx["i"] <= len(msgs):
            return msgs[idx["i"] - 1]
        bridge._running.clear()
        return None

    conn.recv_match = fake_recv  # type: ignore[assignment]
    bridge._conn = conn

    # Must not raise despite the tlog write blowing up on every frame.
    bridge._receive_loop()
    bridge.stop()


def test_two_reconnects_in_one_clock_tick_still_get_their_own_file(
    tmp_path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A flight's telemetry must not be overwritten by the next reconnect.

    The tlog name is a microsecond timestamp, which was assumed to be
    collision-free. It is not: datetime.now() resolves to the platform clock,
    and Windows' is ~1 ms at best, so two reconnects inside one tick produced
    one name — the second session opened the first session's file. Freezing the
    clock reproduces on any host what Windows does on its own.
    """
    class _FrozenClock(datetime.datetime):
        """Real datetime in every respect except that now() does not move."""

        @classmethod
        def now(cls, tz=None):
            return cls(2026, 9, 9, 12, 0, 0, 500000)

    monkeypatch.setattr(datetime, "datetime", _FrozenClock)

    first = MavlinkBridge._next_tlog_path(str(tmp_path))
    pathlib.Path(first).write_bytes(b"session one")
    second = MavlinkBridge._next_tlog_path(str(tmp_path))
    pathlib.Path(second).write_bytes(b"session two")
    third = MavlinkBridge._next_tlog_path(str(tmp_path))

    assert first != second != third
    assert len({first, second, third}) == 3
    assert pathlib.Path(first).read_bytes() == b"session one", (
        "the second session must not have opened the first session's file"
    )
    # Still sorts by session order: the suffix only separates a shared stamp.
    assert sorted([first, second, third]) == [first, second, third]
