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


# ---------------------------------------------------------------------------
# Snapshot isolation is by value shape, not by a list of known field names
# ---------------------------------------------------------------------------

def test_every_mutable_field_is_isolated_from_the_store(
    store: VehicleStateStore,
) -> None:
    """A caller mutating what it was handed must not reach canonical state.

    ``warnings`` was the only field this was ever checked for, because it was
    the only one copied. Position, home and the RC channel list are just as
    mutable and were just as reachable.
    """
    store.update(
        position=[11.5, 48.1], home=[11.4, 48.0], rc_channels=[1500] * 8,
    )
    store.merge_warning("battery low", "warning")

    snapshot = store.get_snapshot()
    snapshot["position"][0] = 999.0
    snapshot["home"][0] = 999.0
    snapshot["rc_channels"][0] = 9999
    snapshot["warnings"][0]["msg"] = "TAMPERED"

    store.update(boot_ms=1)          # any mutation forces a rebuild
    fresh = store.get_snapshot()
    assert fresh["position"] == [11.5, 48.1]
    assert fresh["home"] == [11.4, 48.0]
    assert fresh["rc_channels"][0] == 1500
    assert fresh["warnings"][0]["msg"] == "battery low"


def test_a_nested_container_is_isolated_too(store: VehicleStateStore) -> None:
    """Isolation follows the shape down, so a list of dicts of lists is safe.

    ``mission`` carries nothing yet, which is exactly why this is worth
    pinning: it is the field most likely to gain nested structure, and the
    copy has to keep working when it does without anyone remembering to
    register it anywhere.
    """
    store.update(mission=[{"seq": 0, "waypoint": [11.5, 48.1]}])

    snapshot = store.get_snapshot()
    snapshot["mission"][0]["waypoint"][0] = 999.0
    snapshot["mission"][0]["seq"] = 42

    store.update(boot_ms=1)
    fresh = store.get_snapshot()
    assert fresh["mission"] == [{"seq": 0, "waypoint": [11.5, 48.1]}]


def test_the_write_path_copies_what_the_caller_kept_a_reference_to(
    store: VehicleStateStore,
) -> None:
    """update() must not store the caller's own list.

    The bridge builds an RC channel list per frame, but a caller that reuses
    one buffer would otherwise be editing stored state from outside the lock.
    """
    channels = [1500] * 4
    store.update(rc_channels=channels)
    channels[0] = 9999

    assert store.get_snapshot()["rc_channels"] == [1500] * 4

    nested = [{"waypoint": [1.0]}]
    store.update(mission=nested)
    nested[0]["waypoint"].append(2.0)

    assert store.get_snapshot()["mission"] == [{"waypoint": [1.0]}]


def test_scalars_are_shared_rather_than_copied(store: VehicleStateStore) -> None:
    """What makes the rebuild cheap: only containers are rebuilt.

    Sharing an immutable value is not a route back into the store, so the
    scalars — which are almost the whole dict — are handed out as they are.
    """
    store.update(link_connection="serial:/dev/ttyUSB0:57600")

    snapshot = store.get_snapshot()

    assert snapshot["link_connection"] is store._data["link_connection"]
    assert snapshot["rc_channels"] is not store._data["rc_channels"]
