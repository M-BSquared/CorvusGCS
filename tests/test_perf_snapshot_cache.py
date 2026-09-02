"""Snapshot-cache and version-counter behaviour for ``VehicleStateStore`` (B2).

The store caches its last published snapshot and rebuilds it only when a
monotonic ``_data_version`` advances on a mutation. These tests pin the
contract the SSE layer (B3) relies on: consecutive reads with no intervening
mutation return the cached object; a mutation invalidates the cache so the
next read reflects the latest state even when the listener push was
coalesced; the ``warnings`` list is a defensive copy so a caller cannot
mutate the store's canonical warnings; and ``version()`` is a monotonic
mutation counter.
"""
from __future__ import annotations

import time

import pytest

from corvus.state_store import VehicleStateStore


def test_get_snapshot_returns_cached_object_across_reads(
    store: VehicleStateStore,
) -> None:
    """With no intervening mutation, ``get_snapshot`` returns equal data and
    the SAME cached object (read-only contract) so the SSE layer can share it."""
    first = store.get_snapshot()
    second = store.get_snapshot()

    assert first == second
    # Same object pins the caching optimization: a cache hit must not rebuild.
    assert first is second


def test_get_snapshot_invalidates_after_update(
    store: VehicleStateStore,
) -> None:
    """A mutation bumps the version, so the next ``get_snapshot`` rebuilds the
    cache and reflects the new value (cache invalidation is on mutation)."""
    before = store.get_snapshot()
    assert before["roll"] == 0.0

    store.update(roll=42.0)

    after = store.get_snapshot()
    assert after["roll"] == 42.0
    # A rebuilt cache is a new object distinct from the pre-mutation one.
    assert after is not before


def test_snapshot_warnings_are_defensively_copied(
    store: VehicleStateStore,
) -> None:
    """Mutating a returned snapshot's ``warnings`` list (or a warning dict in
    it) must NOT mutate the store's canonical warnings: the cached snapshot
    owns a defensive copy. Force a cache rebuild to read the store's true
    warnings rather than the polluted cache."""
    store.merge_warning("original", "warning", meta="m1")
    snap = store.get_snapshot()
    assert [w["msg"] for w in snap["warnings"]] == ["original"]

    # Corrupt both the list and a warning dict in the returned snapshot.
    snap["warnings"].append({"level": "info", "msg": "INJECTED", "meta": "x"})
    snap["warnings"][0]["level"] = "critical"

    # An unrelated mutation bumps the version, forcing the cache to rebuild
    # from the store's (unmutated) canonical warnings.
    store.update(roll=1.0)
    fresh = store.get_snapshot()

    assert [w["msg"] for w in fresh["warnings"]] == ["original"]
    assert fresh["warnings"][0]["level"] == "warning"


def test_version_increments_on_each_mutation(
    store: VehicleStateStore,
) -> None:
    """``version()`` advances on every recognized mutation and stays flat when
    nothing changes (unknown keys, non-transition heartbeats)."""
    start = store.version()

    store.update(roll=1.0)
    assert store.version() == start + 1

    store.update(roll=2.0)
    assert store.version() == start + 2

    store.merge_warning("w", "warning")
    assert store.version() == start + 3

    store.set_disconnected()
    assert store.version() == start + 4

    # False->True connected transition bumps the version.
    store.heartbeat()
    assert store.version() == start + 5

    # A heartbeat on an already-connected store mutates nothing -> no bump.
    store.heartbeat()
    assert store.version() == start + 5

    # An update of only unknown keys is a no-op -> no bump, cache stays fresh.
    store.update(not_a_real_key=123)
    assert store.version() == start + 5


def test_get_snapshot_reflects_coalesced_update_via_version(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Cache invalidation is on mutation, NOT on notification: a coalesced
    update (no listener push) still bumps the version, so ``get_snapshot``
    returns the latest state. Mirrors the coalesce test but asserts the
    version-based invalidation mechanism B3 relies on."""
    clock = {"t": 1000.0}
    monkeypatch.setattr(time, "monotonic", lambda: clock["t"])

    store = VehicleStateStore()
    received: list = []
    store.add_listener(received.append)

    store.update(roll=1.0)  # notify (start - 0 >= interval)
    v_after_notify = store.version()
    store.update(roll=2.0)  # same instant -> coalesced, but version bumps
    store.update(roll=3.0)  # coalesced, version bumps again

    assert len(received) == 1  # only the first push fired
    assert received[-1]["roll"] == 1.0  # the flushed snapshot
    # The version advanced past the notify despite coalescing...
    assert store.version() == v_after_notify + 2
    # ...so get_snapshot rebuilds the cache and returns the latest value.
    assert store.get_snapshot()["roll"] == 3.0


def test_to_json_uses_cached_snapshot(store: VehicleStateStore) -> None:
    """``to_json`` reads the cached snapshot; two calls with no mutation
    produce identical JSON (the cache is not rebuilt)."""
    a = store.to_json()
    b = store.to_json()
    assert a == b
    store.update(roll=7.0)
    c = store.to_json()
    assert c != a  # rebuilt after the mutation
