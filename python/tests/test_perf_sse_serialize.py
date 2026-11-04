"""SSE telemetry serialization reuse (B3).

With N browser tabs the telemetry SSE path used to call
``json.dumps(_sanitize(snap))`` once per client per state event. The
version-keyed memoization in ``corvus.server._serialize_telemetry_snapshot``
computes the bytes once per snapshot version and reuses them for every
client. These tests pin that dedup and the staleness guard, and verify the
wire output is byte-identical to the pre-optimization path (no behaviour
change visible to clients).
"""
from __future__ import annotations

import json
from typing import Any

import pytest

# corvus.server imports mavlink_bridge (pymavlink) and ssh_bridge (paramiko)
# at module import time; guard the whole module for minimal CI envs, mirroring
# the conftest server_with_store fixture.
pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

import corvus.server as server_module
from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore, _sanitize


def _count_dumps(monkeypatch: pytest.MonkeyPatch) -> dict[str, int]:
    """Wrap ``json.dumps`` in the server module with a call counter.

    Only the telemetry serialization path calls ``json.dumps`` during these
    tests, so the count is exactly the serialize count.
    """
    calls = {"n": 0}
    real = server_module.json.dumps

    def counting(obj: Any, *args: Any, **kwargs: Any) -> str:
        calls["n"] += 1
        return real(obj, *args, **kwargs)

    monkeypatch.setattr(server_module.json, "dumps", counting)
    return calls


def _make_telemetry_handler(
    store: VehicleStateStore, sent: list[tuple[str, bytes]]
) -> CorvusHandler:
    """A CorvusHandler stubbed down to just the SSE send path."""
    handler = object.__new__(CorvusHandler)
    handler.store = store  # type: ignore[assignment]
    handler._send_sse_bytes = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    handler.send_response = lambda status: None  # type: ignore[method-assign]
    handler.send_header = lambda name, value: None  # type: ignore[method-assign]
    handler.end_headers = lambda: None  # type: ignore[method-assign]
    return handler


# ---- _serialize_telemetry_snapshot: dedup ----

def test_serialize_dedups_same_version_across_clients(
    monkeypatch: pytest.MonkeyPatch, store: VehicleStateStore
) -> None:
    """Two 'clients' reading the same snapshot version reuse one serialization."""
    calls = _count_dumps(monkeypatch)

    snap = store.get_snapshot()
    b1 = server_module._serialize_telemetry_snapshot(store, snap)
    b2 = server_module._serialize_telemetry_snapshot(store, snap)

    # Byte-identical output, serialized exactly once for this version.
    assert b1 == b2
    assert calls["n"] == 1


def test_serialize_reserializes_on_new_version(
    monkeypatch: pytest.MonkeyPatch, store: VehicleStateStore
) -> None:
    """A new snapshot version triggers a fresh serialize; a third client at the
    same new version reuses the cache."""
    calls = _count_dumps(monkeypatch)

    snap1 = store.get_snapshot()
    b1 = server_module._serialize_telemetry_snapshot(store, snap1)
    assert calls["n"] == 1

    store.update(roll=5.0)  # new version
    snap2 = store.get_snapshot()
    b2 = server_module._serialize_telemetry_snapshot(store, snap2)
    assert calls["n"] == 2  # re-serialized for the new version
    assert b1 != b2  # content differs (roll changed)

    # A third 'client' at the same new version hits the cache.
    b3 = server_module._serialize_telemetry_snapshot(store, snap2)
    assert calls["n"] == 2
    assert b3 == b2


def test_serialize_does_not_cache_stale_snapshot(
    monkeypatch: pytest.MonkeyPatch, store: VehicleStateStore
) -> None:
    """A stale snapshot from the SSE queue is serialized and sent as-is but
    never cached for the current version, so a subsequent fresh snapshot
    serializes fresh (no stale-bytes leak)."""
    calls = _count_dumps(monkeypatch)

    stale = store.get_snapshot()  # version v
    store.update(roll=9.0)  # advance to v+1; stale is no longer current
    fresh = store.get_snapshot()

    b_stale = server_module._serialize_telemetry_snapshot(store, stale)
    assert calls["n"] == 1  # serialized (sent as-is)
    assert json.loads(b_stale.decode("utf-8"))["roll"] == 0.0  # the stale value

    b_fresh = server_module._serialize_telemetry_snapshot(store, fresh)
    assert calls["n"] == 2  # fresh serialize, did NOT reuse the stale bytes
    assert json.loads(b_fresh.decode("utf-8"))["roll"] == 9.0
    assert b_fresh != b_stale


def test_serialize_output_matches_uncached_path(
    store: VehicleStateStore,
) -> None:
    """The cached bytes are byte-identical to the pre-optimization
    ``json.dumps(_sanitize(snap))`` path (no wire-visible change)."""
    snap = store.get_snapshot()
    expected = json.dumps(_sanitize(snap)).encode("utf-8")
    got = server_module._serialize_telemetry_snapshot(store, snap)
    assert got == expected


# ---- _sse_telemetry handler path ----

def test_sse_telemetry_dedups_serialization_across_events(
    monkeypatch: pytest.MonkeyPatch, store: VehicleStateStore
) -> None:
    """The initial event and a loop event at the same version serialize once;
    both are byte-identical 'state' events sent via ``_send_sse_bytes``."""
    snap = store.get_snapshot()
    returned = {"done": False}

    def fake_get(self: Any, timeout: float | None = None) -> Any:
        if not returned["done"]:
            returned["done"] = True
            return snap
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(server_module._BoundedSseBuffer, "get", fake_get)

    sent: list[tuple[str, bytes]] = []
    handler = _make_telemetry_handler(store, sent)
    calls = _count_dumps(monkeypatch)

    handler._sse_telemetry()

    # Initial event + one loop event, both "state", byte-identical.
    assert [event for event, _ in sent] == ["state", "state"]
    assert sent[0][1] == sent[1][1]
    # Serialized once: the loop event reused the cached bytes.
    assert calls["n"] == 1
    # The listener is cleaned up on disconnect (no leak across clients).
    assert store._listeners == []


def test_sse_telemetry_reserializes_when_state_advances(
    monkeypatch: pytest.MonkeyPatch, store: VehicleStateStore
) -> None:
    """When state advances between the initial event and a loop event, the
    loop event re-serializes for the new version (cache invalidated)."""
    advanced = {"done": False}

    def fake_get(self: Any, timeout: float | None = None) -> Any:
        if not advanced["done"]:
            advanced["done"] = True
            # Simulate the mavlink thread advancing state and pushing the fresh
            # snapshot to the SSE buffer between the initial and loop events.
            store.update(roll=3.0)
            return store.get_snapshot()
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(server_module._BoundedSseBuffer, "get", fake_get)

    sent: list[tuple[str, bytes]] = []
    handler = _make_telemetry_handler(store, sent)
    calls = _count_dumps(monkeypatch)

    handler._sse_telemetry()

    assert [event for event, _ in sent] == ["state", "state"]
    # Two distinct versions -> two serializations.
    assert calls["n"] == 2
    assert sent[0][1] != sent[1][1]
    assert json.loads(sent[0][1].decode("utf-8"))["roll"] == 0.0
    assert json.loads(sent[1][1].decode("utf-8"))["roll"] == 3.0
    assert store._listeners == []


def test_sse_telemetry_without_store_emits_nothing_and_skips_listener(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """With no store wired, the handler emits nothing and never touches the
    listener API (defensive: the finally block checks ``self.store``)."""
    def boom(self: Any, timeout: float | None = None) -> Any:
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(server_module._BoundedSseBuffer, "get", boom)

    sent: list[tuple[str, bytes]] = []
    handler = object.__new__(CorvusHandler)
    handler.store = None  # type: ignore[assignment]
    handler._send_sse_bytes = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    handler.send_response = lambda status: None  # type: ignore[method-assign]
    handler.send_header = lambda name, value: None  # type: ignore[method-assign]
    handler.end_headers = lambda: None  # type: ignore[method-assign]

    handler._sse_telemetry()  # must not raise

    assert sent == []
