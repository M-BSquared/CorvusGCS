"""The parameter workers on Rust threads against the Python threads they replaced.

``tests/reference/param_threads_python.py`` keeps how the batch upload, the
download watchdog and the metadata download were started, on
``threading.Thread``. Each test starts the same work both ways and compares
the refusals, the states, the work the thread did and that it ended; the
handles differ by design (a ``CoreThread`` against a ``threading.Thread``).
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import param_threads_python as reference  # noqa: E402

M = mavutil.mavlink


class ReferenceBridge(reference.ParamThreadsMixin, MavlinkBridge):
    """A bridge whose parameter workers start on Python threads."""


KINDS = (ReferenceBridge, MavlinkBridge)


def ready(kind: type, armed: bool = False) -> Any:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True, armed=armed)
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = SimpleNamespace(mav=SimpleNamespace())
    # Running, as a connected bridge is: an upload on a stopped bridge ends
    # at once as interrupted, and whether a second call then still finds its
    # thread alive is a race, not behaviour.
    bridge._running.set()
    return bridge


@pytest.mark.parametrize("params", [
    [{"name": "MPC_XY_VEL_MAX", "value": 12.0}, {"name": "SYS_AUTOSTART", "value": 4001}],
    [], "junk", [{"name": "", "value": 1}], [{"value": 1}],
], ids=["two", "empty", "junk", "blank-name", "no-name"])
@pytest.mark.parametrize("armed", [False, True])
def test_uploads_start_alike(params: Any, armed: bool) -> None:
    results = []
    for kind in KINDS:
        bridge = ready(kind, armed)
        written: list = []
        bridge.set_param = lambda name, value, _w=written: (_w.append((name, value)), True)[1]
        first = bridge.start_param_upload(params)
        error = bridge.get_last_command_error()
        worker = bridge._param_upload_thread
        if worker is not None:
            worker.join(timeout=5)
            assert not worker.is_alive()
        results.append((first, error, written, bridge.get_param_upload_result(),
                        None if worker is None else worker.name))
    assert results[1] == results[0]


def test_a_second_upload_is_refused_while_one_runs() -> None:
    import threading

    results = []
    for kind in KINDS:
        bridge = ready(kind)
        gate = threading.Event()
        bridge.set_param = lambda name, value, _g=gate: (_g.wait(timeout=5), True)[1]
        first = bridge.start_param_upload([{"name": "A_B", "value": 1.0}])
        second = bridge.start_param_upload([{"name": "C_D", "value": 2.0}])
        error = bridge.get_last_command_error()
        gate.set()
        bridge._param_upload_thread.join(timeout=5)
        results.append((first, second, error, bridge._param_upload_thread.is_alive()))
    assert results[1] == results[0]


@pytest.mark.parametrize("running", [False, True])
def test_the_watchdog_starts_alike(running: bool) -> None:
    results = []
    for kind in KINDS:
        bridge = ready(kind)
        ran: list = []
        bridge._param_watchdog = lambda _r=ran: _r.append("watchdog")
        if not running:
            bridge._running.clear()
        bridge._start_param_watchdog()
        worker = bridge._param_watchdog_thread
        if worker is not None:
            worker.join(timeout=5)
        bridge._start_param_watchdog()
        again = bridge._param_watchdog_thread
        if again is not None:
            again.join(timeout=5)
        results.append((ran, None if worker is None else (worker.name, worker.is_alive())))
    assert results[1] == results[0]


@pytest.mark.parametrize("stack", [M.MAV_AUTOPILOT_PX4, M.MAV_AUTOPILOT_ARDUPILOTMEGA])
@pytest.mark.parametrize("connected", [True, False])
def test_the_metadata_download_starts_alike(stack: int, connected: bool) -> None:
    results = []
    for kind in KINDS:
        bridge = ready(kind)
        bridge._latch_dialect(stack, M.MAV_TYPE_QUADROTOR)
        if not connected:
            bridge._store.set_disconnected()
        ran: list = []

        def worker(*args: Any, _bridge: Any = bridge, _ran: list = ran) -> None:
            _ran.append(args[1:3])
            with _bridge._param_meta_lock:
                _bridge._param_meta_state = "ready"

        bridge._param_metadata_worker = worker
        first = bridge.start_param_metadata()
        thread = bridge._param_meta_thread
        if thread is not None:
            thread.join(timeout=5)
            assert not thread.is_alive()
        second = bridge.start_param_metadata()
        results.append((first, second, ran, None if thread is None else thread.name))
    assert results[1] == results[0]
