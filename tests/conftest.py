"""Shared pytest fixtures and hooks for the Corvus GCS test suite.

Top-level imports stay stdlib-only so collection never fails in a minimal
environment that lacks pymavlink/paramiko. Anything that transitively pulls
those (the server and bridge modules) is imported lazily inside fixtures via
``pytest.importorskip``.
"""
from __future__ import annotations

import os
import threading
from typing import Any
from unittest.mock import MagicMock

import pytest

from corvus.state_store import VehicleStateStore


@pytest.fixture
def store() -> VehicleStateStore:
    """A fresh, isolated Vehicle State Store per test."""
    return VehicleStateStore()


@pytest.fixture
def fake_bridge() -> MagicMock:
    """Stand-in for ``MavlinkBridge`` that opens no sockets.

    A plain ``MagicMock`` configured with the read-side returns the API
    endpoints expect, so server/HTTP-level tests can run without a live
    autopilot or the pymavlink dependency.
    """
    bridge = MagicMock()
    bridge.get_available_modes.return_value = ["MANUAL", "MISSION"]
    bridge.get_last_command_error.return_value = ""
    bridge.param_status.return_value = {"state": "idle", "count": 0, "received": 0}
    bridge.get_params.return_value = []
    return bridge


@pytest.fixture
def server_with_store(store: VehicleStateStore, fake_bridge: MagicMock):
    """A live ``CorvusServer`` bound to loopback, wired with a fake bridge.

    Mirrors the wiring in ``tests/test_server_params.py`` and
    ``corvus.server.create_server``: it sets the ``CorvusHandler`` class
    attributes (``store``/``mavlink``/``ssh``) so request handlers see them, but
    uses the in-process fake bridge instead of opening a real MAVLink
    connection. The server is started on an ephemeral port and torn down
    cleanly (``shutdown`` + ``server_close`` + thread join) so no socket or
    thread leaks. Reach it via ``http.client``/``urllib`` against
    ``server.server_address``.
    """
    # corvus.server imports mavlink_bridge (pymavlink) and ssh_bridge
    # (paramiko) at module import time, so guard it for minimal CI envs.
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler, CorvusServer

    # Preserve and restore class-level state so the fixture never bleeds into
    # sibling tests that read CorvusHandler's class attributes directly.
    saved = (CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh)
    CorvusHandler.store = store
    CorvusHandler.mavlink = fake_bridge
    CorvusHandler.ssh = None

    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(target=server.serve_forever, name="corvus-test-http", daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh = saved


def pytest_collection_modifyitems(config: pytest.Config, items: list[Any]) -> None:
    """Auto-skip ``@pytest.mark.sitl`` items unless ``CORVUS_SITL=1`` is set.

    Keeps SITL-dependent tests out of the default offline run without
    requiring authors to remember a CLI flag.
    """
    if os.environ.get("CORVUS_SITL") == "1":
        return
    skip = pytest.mark.skip(reason="needs SITL (set CORVUS_SITL=1 to enable)")
    for item in items:
        if "sitl" in item.keywords:
            item.add_marker(skip)
