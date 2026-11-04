"""Review regressions for link-readiness and real PX4 SITL lifecycle."""
from __future__ import annotations

import os
import time

import pytest

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore


def test_serial_command_gate_uses_the_serial_heartbeat_grace(monkeypatch) -> None:
    """A SiK dropout kept alive for 15 s must not reject commands after 5 s."""
    from corvus import state_store as state_store_module

    now = [100.0]
    monkeypatch.setattr(state_store_module.time, "monotonic", lambda: now[0])
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store, "serial:/dev/ttyUSB0:57600")
    bridge._conn = object()

    now[0] = 106.0

    assert bridge._heartbeat_timeout() == 10.0
    assert bridge._connection_ready() is True


def _wait_until(predicate, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return bool(predicate())


@pytest.mark.sitl
def test_live_px4_sitl_connects_reports_identity_and_stops_cleanly() -> None:
    """Opt-in smoke test against PX4 SITL.

    Deselected from the default offline run (see ``pytest_collection_modifyitems``).
    Once selected, a simulator that does not answer is a failure and not a
    skip: ``CORVUS_SITL=1`` is someone stating a simulator is there, and a
    silent skip would report a green run for a link that never came up.
    """
    connection = os.environ.get(
        "CORVUS_SITL_CONNECTION", "udp:0.0.0.0:14550",
    )
    store = VehicleStateStore()
    bridge = MavlinkBridge(store, connection)
    started = time.monotonic()
    bridge.start()
    try:
        assert _wait_until(lambda: bool(store.get_snapshot()["connected"]), 15.0), (
            f"CORVUS_SITL=1 is set but no PX4 SITL heartbeat arrived on {connection}; "
            "start a simulator or unset CORVUS_SITL"
        )

        def ready() -> bool:
            snapshot = store.get_snapshot()
            return bool(
                snapshot.get("autopilot") == "PX4"
                and snapshot.get("vehicle_type")
                and snapshot.get("px4_version")
                and snapshot.get("position") != [0.0, 0.0]
            )

        assert _wait_until(ready, 15.0), store.get_snapshot()
        snapshot = store.get_snapshot()
        assert snapshot["px4_version"].startswith("v1.")
        assert bridge._target_system > 0
        assert bridge._target_component > 0
    finally:
        bridge.stop()

    assert time.monotonic() - started < 35.0
    assert bridge._conn is None
    assert not bridge._running.is_set()
    assert not (bridge._thread and bridge._thread.is_alive())
    assert not (bridge._hb_thread and bridge._hb_thread.is_alive())
    assert store.get_snapshot()["connected"] is False
