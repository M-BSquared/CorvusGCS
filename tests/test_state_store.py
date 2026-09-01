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
