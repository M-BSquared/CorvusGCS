"""Shared pytest fixtures and hooks for the Corvus GCS test suite.

Top-level imports stay stdlib-only so collection never fails in a minimal
environment that lacks pymavlink/paramiko. Anything that transitively pulls
those (the server and bridge modules) is imported lazily inside fixtures via
``pytest.importorskip``.
"""
from __future__ import annotations

import logging
import os
import threading
from typing import Any
from unittest.mock import MagicMock

import pytest

from corvus.state_store import VehicleStateStore


@pytest.fixture(autouse=True)
def corvus_log_propagation() -> Any:
    """Keep ``corvus.*`` records reaching pytest's ``caplog`` handler.

    A handful of tests assert on what the code logged — a throttled listener
    failure, a dropped config key, a traceback that must survive. ``caplog``
    installs its handler on the ROOT logger, so a record only ever arrives
    there if the logger that emitted it propagates.

    Propagation is not this suite's to assume. An environment that also has the
    ROS 2 pytest plugins installed (``launch_testing_ros``, ``ament_lint``) has
    them sweep every logger at collection time and set ``propagate = False``;
    the ``corvus.*`` loggers own no handlers, so their records were then dropped
    on the floor. The tests failed, the code was correct, and the only signal
    was the log line appearing on stderr and not in ``caplog``.

    So the suite states the property it depends on instead of inheriting it,
    and puts back whatever it found afterwards.
    """
    names = ["corvus"] + [
        name for name in list(logging.Logger.manager.loggerDict)
        if name.startswith("corvus.")
    ]
    saved = {}
    for name in names:
        log = logging.getLogger(name)
        saved[name] = log.propagate
        log.propagate = True
    try:
        yield
    finally:
        for name, value in saved.items():
            logging.getLogger(name).propagate = value


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
    """Deselect ``@pytest.mark.sitl`` items unless ``CORVUS_SITL=1`` is set.

    Keeps SITL-dependent tests out of the default offline run without
    requiring authors to remember a CLI flag.

    Deselected, not skipped. A skip line says "this test was passed over
    because a condition it needs was not met", and a clean run that prints
    two of them trains everyone reading it to ignore skip lines — which is
    how a real skip goes unnoticed. These tests need a simulator that the
    default offline suite never claimed to have, so they are simply not part
    of it. ``CORVUS_SITL=1 pytest`` selects them, and then they must pass:
    the test itself fails rather than skips when nothing answers.
    """
    if os.environ.get("CORVUS_SITL") == "1":
        return
    selected, deselected = [], []
    for item in items:
        (deselected if "sitl" in item.keywords else selected).append(item)
    if deselected:
        config.hook.pytest_deselected(items=deselected)
        items[:] = selected


# ---------------------------------------------------------------------------
# Platform gates
# ---------------------------------------------------------------------------
# A handful of tests assert POSIX file-permission behaviour: that the saved
# config is mode 0o600, and that an unwritable directory is reported rather
# than crashed on. Windows has neither mechanism — os.chmod sets only the
# read-only attribute, and a directory cannot be made un-creatable-in that way
# — so those tests are skipped there rather than rewritten into something that
# no longer checks the thing they exist for. The gap they leave on Windows is
# named in save_config()'s docstring.
posix_permissions = pytest.mark.skipif(
    os.name != "posix",
    reason="POSIX file permissions; os.chmod on Windows sets only read-only",
)
