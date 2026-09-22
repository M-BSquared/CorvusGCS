"""Auto-connect against a real vehicle on a real socket.

``tests/test_autoconnect.py`` proves the policy: given these ports, this is the
link, and these are the cases where nothing may be touched. It proves it over
injected data, which is the only way to test a decision about hardware that is
not plugged in — and it is therefore the one thing a policy test cannot prove
that the plumbing underneath it works.

So this file removes the fakes. A real ``MavlinkBridge`` binds a real UDP
socket; a MAVLink speaker built out of pymavlink sends real heartbeat frames at
it; the real Vehicle State Store is what gets read. The questions are the
acceptance criteria that say "without any click":

  * a station launched with a vehicle already publishing connects by itself,
  * one launched with nothing there scans without spinning hot or dying,
  * one that loses the vehicle reconnects when it comes back, and
  * a disconnect the operator asked for stays disconnected.

**What this is not.** The vehicle here is a heartbeat source, not PX4: it does
not answer ``AUTOPILOT_VERSION``, does not carry a parameter set, and does not
implement a mode table. The PX4 v1.16/v1.17/v1.18 behaviour on top of the link
is covered by the version, parameter and stream-fallback suites against real
firmware, and the plan's L1-L6 live smoke against Gazebo is still the only
thing that exercises all of it at once.

The serial half of auto-connect — the USB and SiK priority, the bootloader
skip — cannot be reached from a test at all: it needs a flight controller on a
cable. It is covered by the policy tests against the real classifier, which is
the same one the firmware flasher trusts.
"""
from __future__ import annotations

import socket
import threading
import time
from typing import Any

import pytest

pytest.importorskip("pymavlink")

from pymavlink import mavutil as mv  # noqa: E402

from corvus import autoconnect as ac  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402


# A live link has to be waited for, and the waits are the slow part of this
# file. They are generous rather than tight: a loaded CI box is not a bug, and a
# test that fails there teaches everyone to re-run it instead of reading it.
CONNECT_TIMEOUT_S = 12.0
POLL_S = 0.05


def free_port() -> int:
    """A port nothing is listening on, for the bridge to bind."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


class FakeVehicle:
    """A MAVLink heartbeat source: a quadrotor with a PX4 autopilot, at 5 Hz.

    Sends rather than binds, because ``udp:`` makes the ground station the
    binder — which is the shape a simulator and a telemetry radio both have,
    and the shape the ``EADDRINUSE`` trap in the audit is about.

    5 Hz, not 1: the wait below is a test's patience, and a heartbeat every
    200 ms crosses the bridge's 10 s acceptance window early enough that a slow
    machine does not turn a passing property into a flake.
    """

    def __init__(self, target_port: int, system_id: int = 1) -> None:
        self._addr = ("127.0.0.1", target_port)
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._mav = mv.mavlink.MAVLink(None, srcSystem=system_id, srcComponent=1)
        self._stop = threading.Event()
        self._thread = threading.Thread(
            target=self._loop, name="fake-vehicle", daemon=True,
        )
        self.sent = 0

    def _frame(self) -> bytes:
        msg = mv.mavlink.MAVLink_heartbeat_message(
            type=mv.mavlink.MAV_TYPE_QUADROTOR,
            autopilot=mv.mavlink.MAV_AUTOPILOT_PX4,
            base_mode=mv.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
            custom_mode=0, system_status=mv.mavlink.MAV_STATE_STANDBY,
            mavlink_version=3,
        )
        msg.pack(self._mav)
        return msg.get_msgbuf()

    def _loop(self) -> None:
        frame = self._frame()
        while not self._stop.wait(0.2):
            try:
                self._sock.sendto(frame, self._addr)
                self.sent += 1
            except OSError:
                return

    def start(self) -> FakeVehicle:
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(timeout=2.0)
        self._sock.close()


def wait_for(predicate, timeout: float = CONNECT_TIMEOUT_S) -> bool:
    """Poll *predicate* until it holds or the budget runs out."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(POLL_S)
    return False


@pytest.fixture
def live():
    """A real store + bridge + watcher, torn down in the launchers' order.

    Teardown is the watcher first and the bridge second, because that is the
    order ``serve.py`` and ``app.py`` use and the reason they use it: a watcher
    tick that lands after ``stop()`` would start the link the shutdown just
    closed.
    """
    made: list[tuple[Any, Any]] = []

    def build(port: int, *, cli: str | None = None, configured: str | None = None):
        store = VehicleStateStore()
        bridge = MavlinkBridge(store, f"udp:127.0.0.1:{port}")
        session = ac.SessionState()
        from corvus.server import apply_startup_connection
        apply_startup_connection(
            bridge, cli, configured=configured, session=session, store=store,
        )
        bridge.start()
        watcher = ac.AutoConnectWatcher(
            bridge=bridge, store=store, session=session,
            list_ports_fn=list, poll_s=0.2,
        )
        watcher.start()
        made.append((watcher, bridge))
        return store, bridge, session, watcher

    try:
        yield build
    finally:
        for watcher, bridge in made:
            watcher.stop()
            bridge.stop()


# ---------------------------------------------------------------------------
# AC2 / L1 — a vehicle that is already talking is connected to without a click.
# ---------------------------------------------------------------------------

def test_a_station_launched_beside_a_talking_vehicle_connects_by_itself(live):
    """The acceptance criterion in one test: nothing is clicked, nothing is
    typed, and the aircraft is on the HUD."""
    port = free_port()
    vehicle = FakeVehicle(port).start()
    try:
        store, bridge, session, _ = live(port, configured=f"udp:127.0.0.1:{port}")

        assert wait_for(lambda: store.get_snapshot()["link_status"] == "connected"), (
            f"never connected; last status "
            f"{store.get_snapshot()['link_status']!r} "
            f"error {store.get_snapshot()['link_error']!r}"
        )
        snapshot = store.get_snapshot()
        assert snapshot["connected"] is True
        assert snapshot["vehicle_type"], "the heartbeat's vehicle type reached the store"
        assert bridge.is_connected()
        # And it says which rule picked the link, rather than leaving the
        # operator to wonder what it is talking to.
        assert snapshot["link_auto"]["reason"] == ac.REASON_CONFIGURED
        assert snapshot["link_auto"]["winner"] == f"udp:127.0.0.1:{port}"
    finally:
        vehicle.stop()


def test_with_nothing_configured_the_udp_fallback_is_what_gets_dialled(live):
    """No argument, no config, no serial: the resolver reaches for the
    ground-station UDP port, which is where a simulator publishes."""
    store, bridge, session, _ = live(free_port(), cli=None, configured=None)
    assert bridge.connection_string() == ac.UDP_FALLBACK_CONNECTION
    assert store.get_snapshot()["link_auto"]["reason"] == ac.REASON_UDP_FALLBACK


# ---------------------------------------------------------------------------
# AC3 — no vehicle at all: honest, quiet, and still alive.
# ---------------------------------------------------------------------------

def test_a_station_with_no_vehicle_scans_without_spinning_or_dying(live):
    """The failure this rules out is a watcher that polls flat out, or one that
    takes the app down with it when there is nothing to find."""
    store, bridge, session, watcher = live(free_port(), configured=None)

    # Give it several poll intervals with nothing to connect to.
    time.sleep(2.0)

    assert watcher.is_alive(), "the watcher survived finding nothing"
    assert bridge.is_running(), "and so did the bridge"
    assert store.get_snapshot()["connected"] is False
    assert store.get_snapshot()["link_status"] in {
        "connecting", "reconnecting", "disconnected",
    }
    # Nothing was invented to connect to, and nothing was offered.
    assert store.get_snapshot()["link_suggestion"] is None
    # One watcher thread, not one per tick.
    assert [t.name for t in threading.enumerate()].count("autoconnect-watcher") == 1


# ---------------------------------------------------------------------------
# L6 — the vehicle goes away and comes back.
# ---------------------------------------------------------------------------

def test_a_vehicle_that_goes_quiet_and_returns_is_picked_up_again(live):
    """A simulator restarted, or a radio that walked behind a building. The
    bridge's own backoff owns this; what is checked here is that auto-connect
    does not interfere with it."""
    port = free_port()
    vehicle = FakeVehicle(port).start()
    store, bridge, session, watcher = live(port, configured=f"udp:127.0.0.1:{port}")
    try:
        assert wait_for(lambda: store.get_snapshot()["connected"] is True), "first link"

        vehicle.stop()
        assert wait_for(
            lambda: store.get_snapshot()["link_status"] != "connected", timeout=20.0,
        ), "the dead link was noticed"

        vehicle = FakeVehicle(port).start()
        assert wait_for(
            lambda: store.get_snapshot()["connected"] is True, timeout=20.0,
        ), "and the returning vehicle was picked up again"

        # The watcher never took the link away from the bridge mid-recovery:
        # the string is still the one the resolver chose.
        assert bridge.connection_string() == f"udp:127.0.0.1:{port}"
        assert watcher.is_alive()
    finally:
        vehicle.stop()


# ---------------------------------------------------------------------------
# AC7 — a disconnect the operator asked for is a disconnect.
# ---------------------------------------------------------------------------

def test_a_disconnect_stays_disconnected_while_the_vehicle_keeps_calling(live):
    """The vehicle is still transmitting the whole time. Nothing may dial it
    back: the operator freed the radio on purpose, and taking that decision
    back is the thing auto-connect is least allowed to do."""
    port = free_port()
    vehicle = FakeVehicle(port).start()
    try:
        store, bridge, session, watcher = live(port, configured=f"udp:127.0.0.1:{port}")
        assert wait_for(lambda: store.get_snapshot()["connected"] is True)

        # What POST /api/mavlink/disconnect does, in the order it does it.
        bridge.stop()
        session.note_manual_connect()

        time.sleep(2.0)
        assert not bridge.is_running(), "the bridge stayed stopped"
        assert store.get_snapshot()["link_status"] == "disconnected"
        assert watcher.is_alive(), "the watcher is still there — it is just not acting"
        assert watcher.tick() == "noop: manual-override"
    finally:
        vehicle.stop()


# ---------------------------------------------------------------------------
# L5 — the forwarder keeps its sink across an auto-connect dial.
# ---------------------------------------------------------------------------

def test_a_second_station_keeps_receiving_across_a_re_dial(live):
    """QGroundControl attached through the forwarder must not be dropped
    because auto-connect changed the link underneath it. The sink is attached
    outside the bridge's connect cycle, and this is the test that says so."""
    port = free_port()
    vehicle = FakeVehicle(port).start()
    try:
        store, bridge, session, _ = live(port, configured=f"udp:127.0.0.1:{port}")
        frames: list[bytes] = []
        bridge.set_frame_sink(frames.append)

        assert wait_for(lambda: store.get_snapshot()["connected"] is True)
        assert wait_for(lambda: len(frames) > 0), "the second station is being fed"

        # The stop/set/start an auto-dial performs.
        before = len(frames)
        bridge.stop()
        bridge.set_connection(f"udp:127.0.0.1:{port}")
        bridge.start()

        assert wait_for(lambda: len(frames) > before, timeout=20.0), (
            "the sink survived the re-dial rather than being detached by it"
        )
    finally:
        bridge.set_frame_sink(None)
        vehicle.stop()


# ---------------------------------------------------------------------------
# Lifecycle — nothing is left running.
# ---------------------------------------------------------------------------

def test_the_whole_thing_leaves_no_thread_or_socket_behind(live):
    """The field laptop is rebooted between flights and the app has to shut
    down perfectly every time. A watcher is a new thread on that path."""
    before = {t.name for t in threading.enumerate()}
    port = free_port()
    vehicle = FakeVehicle(port).start()
    try:
        store, bridge, session, watcher = live(port, configured=f"udp:127.0.0.1:{port}")
        assert wait_for(lambda: store.get_snapshot()["connected"] is True)

        # Launcher order: watcher, then bridge.
        watcher.stop()
        bridge.stop()

        assert wait_for(
            lambda: not {t.name for t in threading.enumerate()}
            - before - {"fake-vehicle"},
            timeout=6.0,
        ), (
            "threads left running: "
            f"{sorted({t.name for t in threading.enumerate()} - before)}"
        )
    finally:
        vehicle.stop()
