"""Bounded per-subscriber buffers behind the HTTP layer's SSE streams.

Moved out of corvus/server.py. corvus.server re-exports both classes, which
is where the handlers and the tests reach them.
"""
from __future__ import annotations

import collections
import queue
import threading
import time
from typing import Any


class _BoundedSseBuffer:
    def __init__(self, capacity: int, condition: threading.Condition | None = None) -> None:
        self._capacity = max(1, int(capacity))
        self._items: collections.deque[Any] = collections.deque()
        # Normally its own condition. _MultiplexSseBuffer passes a shared one so
        # several topic buffers can wake the same reader thread; threading's
        # Condition is built on an RLock, so a drain that already holds it may
        # re-enter these methods.
        self._condition = condition if condition is not None else threading.Condition()

    def put_latest(self, item: Any) -> None:
        with self._condition:
            self._items.clear()
            self._items.append(item)
            self._condition.notify()

    def put_console(self, entry: dict[str, Any]) -> None:
        key = (entry.get("name"), entry.get("text"), entry.get("level"))
        urgent = entry.get("level") in {"error", "critical", "warning"}
        with self._condition:
            if entry.get("name") != "SHELL":
                self._items = collections.deque(
                    item for item in self._items
                    if (item.get("name"), item.get("text"), item.get("level")) != key
                )
            if len(self._items) >= self._capacity:
                drop_index = next(
                    (
                        index for index in range(len(self._items) - 1, -1, -1)
                        if self._items[index].get("level") not in {"error", "critical", "warning"}
                    ),
                    None,
                )
                if drop_index is None:
                    if not urgent:
                        return
                    self._items.pop()
                else:
                    del self._items[drop_index]
            # Always append, never appendleft. Urgency decides what survives
            # a full buffer (the eviction scan above), never what is delivered
            # first: the console is how someone reconstructs what the autopilot
            # did and in what order, and an error that overtakes the
            # informational line explaining it — "EKF2 switching to GPS"
            # arriving after the failure it caused — inverts cause and effect.
            self._items.append(entry)
            self._condition.notify()

    def put_fifo(self, item: Any) -> None:
        """Append one item, dropping the oldest when the buffer is full."""
        with self._condition:
            if len(self._items) >= self._capacity:
                self._items.popleft()
            self._items.append(item)
            self._condition.notify()

    def get(self, timeout: float | None = None) -> Any:
        deadline = None if timeout is None else time.monotonic() + timeout
        with self._condition:
            while not self._items:
                remaining = None if deadline is None else deadline - time.monotonic()
                if remaining is not None and remaining <= 0:
                    raise queue.Empty
                self._condition.wait(remaining)
            return self._items.popleft()

    def drain(self) -> list[Any]:
        """Take everything buffered, in order, without waiting."""
        with self._condition:
            items = list(self._items)
            self._items.clear()
            return items

    def qsize(self) -> int:
        with self._condition:
            return len(self._items)


class _MultiplexSseBuffer:
    """Several named topic buffers behind one condition, one reader thread.

    Exists so one HTTP connection can carry several event streams. Each topic
    keeps its OWN buffer, so each keeps its own drop policy — params and tiles
    coalesce to the latest, the console evicts by urgency, firmware keeps every
    step — which a single shared queue would have flattened into one.
    """

    def __init__(self) -> None:
        self._condition = threading.Condition()
        self._buffers: dict[str, _BoundedSseBuffer] = {}

    def topic(self, name: str, capacity: int) -> _BoundedSseBuffer:
        """Add a topic buffer and return it, for a listener to write into."""
        buf = _BoundedSseBuffer(capacity, condition=self._condition)
        with self._condition:
            self._buffers[name] = buf
        return buf

    def drain(self, timeout: float) -> list[tuple[str, Any]]:
        """Block for up to *timeout* and return ``[(topic, item), ...]``.

        Raises :class:`queue.Empty` if nothing arrived, which is the caller's
        cue to send a keep-alive ping and re-check whether the server is
        stopping — the same contract :meth:`_BoundedSseBuffer.get` has.
        """
        deadline = time.monotonic() + timeout
        with self._condition:
            while True:
                out: list[tuple[str, Any]] = []
                for name, buf in self._buffers.items():
                    out.extend((name, item) for item in buf.drain())
                if out:
                    return out
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise queue.Empty
                self._condition.wait(remaining)
