from __future__ import annotations

import threading

from corvus.state_store import VehicleStateStore


def test_concurrent_warning_merges_do_not_lose_entries() -> None:
    store = VehicleStateStore()
    barrier = threading.Barrier(11)

    def add_warning(index: int) -> None:
        barrier.wait()
        store.merge_warning(f"warning-{index}", "warning", meta=str(index))

    workers = [threading.Thread(target=add_warning, args=(index,)) for index in range(10)]
    for worker in workers:
        worker.start()
    barrier.wait()
    for worker in workers:
        worker.join(timeout=1.0)

    assert all(not worker.is_alive() for worker in workers)
    assert {warning["msg"] for warning in store.get_snapshot()["warnings"]} == {
        f"warning-{index}" for index in range(10)
    }


def test_warning_merge_refreshes_and_escalates_atomically() -> None:
    store = VehicleStateStore()
    store.merge_warning("problem", "info", meta="first")
    store.merge_warning("problem", "warning", meta="second")
    store.merge_warning("problem", "info", meta="third")

    assert store.get_snapshot()["warnings"] == [
        {"level": "warning", "msg": "problem", "meta": "third"}
    ]


def test_resolving_marks_tagged_warnings_without_removing_them() -> None:
    store = VehicleStateStore()
    store.merge_warning("Preflight Fail: compass", "critical", meta="a", event="prearm")
    store.merge_warning("Battery low", "warning", meta="b")
    before = store.get_snapshot()["warnings"]

    assert store.resolve_warnings({"prearm"}) is True
    assert store.get_snapshot()["warnings"] == [
        {"level": "critical", "msg": "Preflight Fail: compass", "meta": "a",
         "event": "prearm", "resolved": True},
        {"level": "warning", "msg": "Battery low", "meta": "b"},
    ]
    assert "resolved" not in before[0], "an earlier snapshot is not rewritten under its reader"


def test_resolving_nothing_is_not_a_mutation() -> None:
    """The bridge resolves on every SYS_STATUS. A call with nothing to do
    must not wake every SSE stream."""
    store = VehicleStateStore()
    store.merge_warning("Preflight Fail: compass", "critical", event="prearm")
    store.resolve_warnings({"prearm"})
    version = store.version()
    assert store.resolve_warnings({"prearm"}) is False
    assert store.resolve_warnings({"kill"}) is False
    assert store.version() == version


def test_a_resolved_warning_said_again_is_live_again() -> None:
    store = VehicleStateStore()
    store.merge_warning("Kill switch engaged", "critical", meta="a", event="kill")
    store.resolve_warnings({"kill"})
    store.merge_warning("Kill switch engaged", "critical", meta="b", event="kill")
    assert store.get_snapshot()["warnings"] == [
        {"level": "critical", "msg": "Kill switch engaged", "meta": "b", "event": "kill"}
    ]
