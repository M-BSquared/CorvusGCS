"""The multiplexed /api/events stream.

One connection carrying console, params, firmware and tile progress, because a
browser allows six per origin and this app could hold five: telemetry, console,
params, tiles and firmware, leaving ONE for every map tile and every fetch. A
viewport is dozens of tiles, so they queued one at a time at exactly the moment
— pre-flight setup with a region downloading — the operator was busiest.

What these tests hold in place:

* each topic arrives under its OWN event name (three of the four single-topic
  endpoints call theirs "progress"; three "progress" streams down one socket
  would be indistinguishable);
* each topic keeps its own buffer and drop policy, which is why the multiplex
  holds a buffer per topic rather than one shared queue;
* a topic whose service is absent is quiet rather than fatal;
* every topic unbinds on the way out, whatever ended the stream.
"""
from __future__ import annotations

import json
import queue
from typing import Any

import pytest

from corvus.server import (
    CONSOLE_SSE_CAPACITY,
    CorvusHandler,
    _BoundedSseBuffer,
    _MultiplexSseBuffer,
)


class _FakeBridge:
    """Records console/param subscriptions the way the real bridge is used."""

    def __init__(self) -> None:
        self.console_subs: list = []
        self.param_listeners: list = []

    def add_console_sub(self, fn) -> None: self.console_subs.append(fn)
    def remove_console_sub(self, fn) -> None: self.console_subs.remove(fn)
    def add_param_listener(self, fn) -> None: self.param_listeners.append(fn)
    def remove_param_listener(self, fn) -> None: self.param_listeners.remove(fn)
    def param_status(self) -> dict:
        return {"state": "downloading", "count": 40, "received": 7}


def _handler(bridge: Any = None, flash: Any = None, path: str = "/api/events") -> tuple:
    h = object.__new__(CorvusHandler)
    h.mavlink = bridge
    h.flash = flash
    h.tile_downloader = None
    h.tile_progress_bus = None
    h.path = path
    sent: list[tuple[str, str]] = []
    h._send_sse = lambda event, data: sent.append((event, data))
    h._send_json = lambda payload, status=200: sent.append((f"json:{status}", json.dumps(payload)))
    h.send_response = lambda status: None
    h.send_header = lambda name, value: None
    h.end_headers = lambda: None
    h._send_cors = lambda: None
    return h, sent


# ---------------------------------------------------------------------------
# The buffer primitive
# ---------------------------------------------------------------------------

def test_each_topic_keeps_its_own_drop_policy() -> None:
    """Coalescing and FIFO topics share a reader, never a buffer.

    A single shared queue would have flattened four different drop policies
    into one — and it is the coalescing that keeps a 50-tile-per-second
    progress feed from filling the socket.
    """
    mux = _MultiplexSseBuffer()
    params = mux.topic("params", 16)
    firmware = mux.topic("firmware", 16)

    params.put_latest({"received": 1})
    params.put_latest({"received": 2})
    params.put_latest({"received": 3})
    firmware.put_fifo({"percent": 10})
    firmware.put_fifo({"percent": 20})

    batch = mux.drain(timeout=0.1)
    by_topic: dict[str, list] = {}
    for topic, item in batch:
        by_topic.setdefault(topic, []).append(item)

    assert by_topic["params"] == [{"received": 3}], "params coalesces to the latest"
    assert by_topic["firmware"] == [{"percent": 10}, {"percent": 20}], "firmware keeps every step"


def test_drain_raises_empty_when_nothing_arrives() -> None:
    """The keep-alive contract: the caller pings and re-checks for shutdown."""
    mux = _MultiplexSseBuffer()
    mux.topic("params", 4)
    with pytest.raises(queue.Empty):
        mux.drain(timeout=0.01)


def test_a_write_to_any_topic_wakes_the_one_reader() -> None:
    mux = _MultiplexSseBuffer()
    console = mux.topic("console", CONSOLE_SSE_CAPACITY)
    mux.topic("params", 4)
    console.put_console({"name": "EKF2", "text": "using GPS", "level": "info"})
    assert mux.drain(timeout=0.1) == [
        ("console", {"name": "EKF2", "text": "using GPS", "level": "info"}),
    ]


def test_a_topic_buffer_is_still_an_ordinary_bounded_buffer() -> None:
    """Sharing a condition must not change what a buffer is."""
    mux = _MultiplexSseBuffer()
    buf = mux.topic("params", 16)
    assert isinstance(buf, _BoundedSseBuffer)
    buf.put_latest({"received": 9})
    assert buf.qsize() == 1


# ---------------------------------------------------------------------------
# The endpoint
# ---------------------------------------------------------------------------

def _stop_after(n: int, monkeypatch: pytest.MonkeyPatch) -> None:
    """Let the stream loop run *n* waits, then break it as a disconnect would."""
    calls = {"n": 0}

    def drain(self: Any, timeout: float | None = None) -> Any:
        calls["n"] += 1
        if calls["n"] > n:
            raise BrokenPipeError("client gone")
        raise queue.Empty

    monkeypatch.setattr(_MultiplexSseBuffer, "drain", drain)


def test_each_topic_arrives_under_its_own_event_name(monkeypatch: pytest.MonkeyPatch) -> None:
    """The reason the multiplexed names differ from the single-topic ones.

    /api/params/progress and /api/firmware/progress both call their event
    "progress". On one socket that is ambiguous, so here they are "params" and
    "firmware".
    """
    _stop_after(0, monkeypatch)
    flash = type("F", (), {
        "status": lambda self: {"state": "idle", "progress": 0, "message": ""},
        "add_listener": lambda self, fn: None,
        "remove_listener": lambda self, fn: None,
    })()
    h, sent = _handler(_FakeBridge(), flash, "/api/events?topics=params,firmware")
    h._sse_events()

    names = [event for event, _ in sent]
    assert names == ["params", "firmware"], f"expected one initial event each, got {names}"
    assert json.loads(sent[0][1]) == {"state": "downloading", "count": 40, "received": 7}
    assert json.loads(sent[1][1]) == {"state": "idle", "percent": 0, "message": ""}


def test_every_topic_unbinds_when_the_stream_ends(monkeypatch: pytest.MonkeyPatch) -> None:
    """A stream that ends must leave no listener on any of its topics."""
    _stop_after(0, monkeypatch)
    bridge = _FakeBridge()
    h, _sent = _handler(bridge, None, "/api/events?topics=console,params")
    h._sse_events()
    assert bridge.console_subs == [], "console listener left behind"
    assert bridge.param_listeners == [], "param listener left behind"


def test_an_unknown_topic_is_ignored_not_rejected(monkeypatch: pytest.MonkeyPatch) -> None:
    """A newer frontend asking for a topic this backend has never heard of
    gets the rest of what it asked for, not a 400 and no stream at all."""
    _stop_after(0, monkeypatch)
    bridge = _FakeBridge()
    h, sent = _handler(bridge, None, "/api/events?topics=console,warpdrive")
    h._sse_events()
    assert not any(e.startswith("json:") for e, _ in sent), "should not have refused"
    assert bridge.console_subs == [], "console was bound and then released"


def test_no_known_topic_is_a_400(monkeypatch: pytest.MonkeyPatch) -> None:
    h, sent = _handler(_FakeBridge(), None, "/api/events?topics=warpdrive")
    h._sse_events()
    assert sent and sent[0][0] == "json:400"

    h, sent = _handler(_FakeBridge(), None, "/api/events")
    h._sse_events()
    assert sent and sent[0][0] == "json:400"


def test_a_missing_service_is_quiet_not_fatal(monkeypatch: pytest.MonkeyPatch) -> None:
    """No flash service: the firmware topic simply never fires, and the rest of
    the stream is unaffected. An unavailable service must not fail a request
    that four other topics are riding on."""
    _stop_after(0, monkeypatch)
    h, sent = _handler(_FakeBridge(), None, "/api/events?topics=firmware,params")
    h._sse_events()
    assert [event for event, _ in sent] == ["params"]


def test_a_quiet_stream_pings_before_waiting_again(monkeypatch: pytest.MonkeyPatch) -> None:
    _stop_after(2, monkeypatch)
    h, sent = _handler(_FakeBridge(), None, "/api/events?topics=params")
    h._sse_events()
    assert [event for event, _ in sent] == ["params", "ping", "ping"]
