"""MAVLink telemetry log (tlog) writer — raw frame capture to disk.

Records every raw MAVLink frame the bridge receives into a ``.tlog`` file so a
flight is always attributable to the build that produced it. The file starts
with a single metadata header line (magic prefix + JSON, terminated by a
newline) that a reader skips in one read; raw MAVLink frames follow verbatim.

The receive loop never blocks on disk: ``write_frame`` only appends to a
bounded, drop-oldest deque (recent data is preserved when the writer falls
behind). A writer thread drains the deque and flushes. ``stop()`` drains the
remainder, flushes, closes the file, and joins the writer with a bounded
timeout — never blocks forever.

The writer's logic and thread are ``corvus_link.bridge.TlogEngine``, in Rust
(a ``CoreThread``, under the same shutdown guard as the link). The file, the
queue and the locks stay here, on the writer, and the engine works on them.
"""
from __future__ import annotations

import collections
import logging
import pathlib
import sys
import threading
from typing import Any

from corvus_link import bridge as _bridge

logger = logging.getLogger("corvus.tlog")

# A reader identifies the metadata header by this leading byte sequence on the
# first line; everything after the newline is raw MAVLink frames.
_MAGIC = b"#CORVUS-TLOG "

# Bound the in-flight buffer so a slow disk can never make the recv loop drop
# frames or block. Drop-oldest keeps the most recent data when the writer falls
# behind (a stale tlog tail is more useful than a stale head).
_DEFAULT_MAX_QUEUE = 10000

# Writer wake cadence: how long it waits for new frames before re-checking the
# stop flag. Also bounds the flush interval on a quiet link.
_DRAIN_TIMEOUT_S = 0.5

_engine = _bridge.TlogEngine(sys.modules[__name__])


class TlogWriter:
    """Thread-safe raw-MAVLink tlog writer with a background drain thread.

    The metadata header is written lazily on the first frame so the caller can
    set the connection string (``set_conn``) before any bytes hit disk. All
    disk I/O happens on the writer thread; the producer (the MAVLink recv loop)
    only does a non-blocking deque append.
    """

    def __init__(self, path: str, max_queue: int = _DEFAULT_MAX_QUEUE) -> None:
        self._path = path
        # Parent dir must exist before opening; create lazily so a missing
        # ~/.corvus/logs does not block the first flight.
        pathlib.Path(path).parent.mkdir(parents=True, exist_ok=True)
        self._file: Any = open(path, "ab")
        self._file_lock = threading.Lock()
        self._buf: collections.deque[bytes] = collections.deque(maxlen=max_queue)
        self._cond = threading.Condition()
        self._stop = threading.Event()
        self._conn_str = ""
        self._header_written = False
        # Set once a write to disk has failed (a field laptop fills its disk
        # mid-flight): reported once, further frames dropped, and stop() still
        # closes the file.
        self._failed = False
        self._thread = _engine.start(self)

    def set_conn(self, conn_str: str) -> None:
        """Amend the ``conn`` field before the header is written; no-op after."""
        if self._header_written:
            return
        self._conn_str = conn_str

    def write_frame(self, raw_bytes: bytes) -> None:
        """Enqueue one raw MAVLink frame; never blocks on disk.

        Drops the oldest buffered frame when the deque is full (recent data is
        preserved). A stop()d writer discards further frames.
        """
        _engine.write_frame(self, raw_bytes)

    def stop(self) -> None:
        """Drain remaining frames, flush, close, and join the writer thread.

        Idempotent and bounded: the writer is joined with a 2 s timeout so a
        stuck disk cannot hang shutdown, and a failing disk never skips the
        close.
        """
        _engine.stop(self)

    def __enter__(self) -> TlogWriter:
        return self

    def __exit__(self, *exc: object) -> None:
        self.stop()

    # ------------------------------------------------------------------
    # Writer thread
    # ------------------------------------------------------------------

    def _writer_loop(self) -> None:
        _engine.writer_loop(self)

    def _drain_until_stopped(self) -> None:
        _engine.drain_until_stopped(self)

    def _note_failure(self, message: str, exc: BaseException | None = None) -> None:
        """Report the first write failure and stop accepting further frames."""
        _engine.note_failure(self, message, exc)

    def _ensure_header(self) -> None:
        """Write the metadata header once (double-checked under the file lock)."""
        _engine.ensure_header(self)

    def _build_header(self) -> bytes:
        return _engine.build_header(self)

    def _write_batch(self, batch: list[bytes]) -> None:
        _engine.write_batch(self, batch)
