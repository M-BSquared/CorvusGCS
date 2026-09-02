"""MAVLink telemetry log (tlog) writer — raw frame capture to disk.

Records every raw MAVLink frame the bridge receives into a ``.tlog`` file so a
flight is always attributable to the build that produced it. The file starts
with a single metadata header line (magic prefix + JSON, terminated by a
newline) that a reader skips in one read; raw MAVLink frames follow verbatim.

Stdlib only. The receive loop never blocks on disk: ``write_frame`` only
appends to a bounded, drop-oldest deque (recent data is preserved when the
writer falls behind). A dedicated daemon thread drains the deque and flushes
periodically. ``stop()`` drains the remainder, flushes, closes the file, and
joins the writer with a bounded timeout — never blocks forever.
"""
from __future__ import annotations

import collections
import json
import pathlib
import threading
from typing import Any

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
        self._thread = threading.Thread(
            target=self._writer_loop, name="tlog-writer", daemon=True,
        )
        self._thread.start()

    def set_conn(self, conn_str: str) -> None:
        """Amend the ``conn`` field before the header is written; no-op after.

        Called by the bridge right after construction, before any frame
        arrives, so the metadata header records the live connection string.
        """
        if self._header_written:
            return
        self._conn_str = conn_str

    def write_frame(self, raw_bytes: bytes) -> None:
        """Enqueue one raw MAVLink frame; never blocks on disk.

        Drops the oldest buffered frame when the deque is full (recent data is
        preserved). A stop()d writer discards further frames.
        """
        if self._stop.is_set():
            return
        with self._cond:
            self._buf.append(raw_bytes)
            self._cond.notify()

    def stop(self) -> None:
        """Drain remaining frames, flush, close, and join the writer thread.

        Idempotent and bounded: the writer is joined with a 2 s timeout so a
        stuck disk cannot hang shutdown.
        """
        self._stop.set()
        with self._cond:
            self._cond.notify_all()
        if self._thread.is_alive():
            self._thread.join(timeout=2.0)
        # Best-effort final header + flush + close; safe if the writer did it.
        self._ensure_header()
        with self._file_lock:
            if self._file is not None and not self._file.closed:
                try:
                    self._file.flush()
                    self._file.close()
                except Exception:
                    pass

    def __enter__(self) -> "TlogWriter":
        return self

    def __exit__(self, *exc: object) -> None:
        self.stop()

    # ------------------------------------------------------------------
    # Writer thread
    # ------------------------------------------------------------------

    def _writer_loop(self) -> None:
        while not self._stop.is_set():
            with self._cond:
                while not self._buf and not self._stop.is_set():
                    self._cond.wait(timeout=_DRAIN_TIMEOUT_S)
                if not self._buf and self._stop.is_set():
                    break
                batch: list[bytes] = []
                while self._buf:
                    batch.append(self._buf.popleft())
            if batch:
                self._ensure_header()
                self._write_batch(batch)
        # Final drain on stop so no enqueued frame is lost.
        with self._cond:
            batch = list(self._buf)
            self._buf.clear()
        if batch:
            self._ensure_header()
            self._write_batch(batch)
        # An empty tlog still carries its metadata header for attributability.
        self._ensure_header()
        with self._file_lock:
            try:
                self._file.flush()
            except Exception:
                pass

    def _ensure_header(self) -> None:
        """Write the metadata header once (double-checked under the file lock)."""
        if self._header_written:
            return
        header = self._build_header()
        with self._file_lock:
            if self._header_written:
                return
            self._file.write(header)
            self._file.flush()
            self._header_written = True

    def _build_header(self) -> bytes:
        # Lazy import: tlog must not introduce an import cycle with the bridge
        # or any other module that reads the version at import time.
        from .version import get_version

        import datetime

        meta = {
            "product": "Corvus GCS",
            "version": get_version(),
            "conn": self._conn_str,
            "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "format": "mavlink-raw",
        }
        return _MAGIC + json.dumps(meta).encode("utf-8") + b"\n"

    def _write_batch(self, batch: list[bytes]) -> None:
        with self._file_lock:
            if self._file is None or self._file.closed:
                return
            for raw in batch:
                self._file.write(raw)
            self._file.flush()
