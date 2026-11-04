"""Serial links against a real bridge, with the cable pulled and plugged back in.

``tests/test_autoconnect.py`` proves the policy over injected port lists, and
``tests/test_autoconnect_live.py`` proves the plumbing over UDP. Neither opens a
serial port, so neither can show what happens to the link when a USB cable is
pulled: whether the read errors end the cycle quickly, whether the bridge spins
while the device is gone, whether the port is reopened when it comes back, and
whether anything is left open after a dozen of those.

A pseudo-terminal stands in for the USB serial device. The bridge opens the
slave side through a path in ``tmp_path``, exactly as it would open
``/dev/ttyACM0``; the master side heartbeats like a flight controller. Pulling
the cable is closing the master and removing the path, which is what the bridge
sees when a CDC ACM node vanishes: reads that fail at once, then a device that
cannot be opened. Plugging it back in is a fresh pty at the same path, or at a
new one.

POSIX only: Windows has no pseudo-terminals, and its COM ports are covered by
``tests/test_windows_serial.py``.
"""
from __future__ import annotations

import os
import select
from collections import Counter
import threading
import time
from pathlib import Path
from typing import Any

import pytest

if not hasattr(os, "openpty"):
    pytest.skip("pseudo-terminals are POSIX only", allow_module_level=True)

pytest.importorskip("pymavlink")
pytest.importorskip("serial")

from pymavlink import mavutil as mv  # noqa: E402

from corvus import autoconnect as ac  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402

# Generous on purpose: the bridge's reconnect backoff reaches 2 s by the third
# attempt, and a loaded CI box is not a bug.
RECONNECT_TIMEOUT_S = 20.0


class FakeSerialBoard:
    """A flight controller on a "USB cable": a pty heartbeating at 5 Hz.

    It also drains everything the station sends, as a real board does. A pty
    whose master is never read fills up, and the station's writes would then
    block, which is a different test.
    """

    def __init__(self, path: Path, serial_number: str = "3A0029") -> None:
        self.path = path
        self.serial_number = serial_number
        self.master, slave = os.openpty()
        target = os.ttyname(slave)
        os.close(slave)
        path.symlink_to(target)
        self._mav = mv.mavlink.MAVLink(None, srcSystem=1, srcComponent=1)
        self._stop = threading.Event()
        self._thread = threading.Thread(
            target=self._loop, name="fake-serial-board", daemon=True,
        )
        self._thread.start()
        self.plugged = True

    def _heartbeat(self) -> bytes:
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
        next_beat = 0.0
        while not self._stop.is_set():
            now = time.monotonic()
            try:
                if now >= next_beat:
                    os.write(self.master, self._heartbeat())
                    next_beat = now + 0.2
                ready, _, _ = select.select([self.master], [], [], 0.05)
                if ready:
                    os.read(self.master, 4096)
            except OSError:
                time.sleep(0.05)

    def row(self) -> dict[str, str]:
        """This board as ``list_serial_ports()`` would report it."""
        return {
            "device": str(self.path),
            "description": "Pixhawk FMU v6X",
            "hwid": f"USB VID:PID=3185:0035 SER={self.serial_number} LOCATION=1-2",
        }

    def unplug(self) -> None:
        if not self.plugged:
            return
        self.plugged = False
        self._stop.set()
        self._thread.join(timeout=2.0)
        self.path.unlink(missing_ok=True)
        os.close(self.master)


def wait_for(predicate, timeout: float = RECONNECT_TIMEOUT_S) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return False


def open_fds() -> int:
    return len(os.listdir("/dev/fd"))


def connected(store: VehicleStateStore) -> bool:
    snap = store.get_snapshot()
    return snap["connected"] is True and not store.is_stale(1.0)


def heard_since(store: VehicleStateStore, instant: float) -> bool:
    """Has a vehicle heartbeat landed after the monotonic *instant*?"""
    return store.get_snapshot()["connected"] is True and not store.is_stale(
        time.monotonic() - instant,
    )


@pytest.fixture
def rig(tmp_path, monkeypatch):
    """Boards, bridges and watchers, all torn down whatever the test did."""
    monkeypatch.setattr(
        "corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path / "logs"),
    )
    boards: list[FakeSerialBoard] = []
    bridges: list[MavlinkBridge] = []
    watchers: list[ac.AutoConnectWatcher] = []

    class Rig:
        dev = tmp_path

        @staticmethod
        def board(name: str, **kwargs: Any) -> FakeSerialBoard:
            made = FakeSerialBoard(tmp_path / name, **kwargs)
            boards.append(made)
            return made

        @staticmethod
        def bridge(board: FakeSerialBoard, baud: int = 57600):
            store = VehicleStateStore()
            made = MavlinkBridge(store, f"serial:{board.path}:{baud}")
            bridges.append(made)
            made.start()
            return store, made

        @staticmethod
        def watcher(bridge, store, session) -> ac.AutoConnectWatcher:
            def enumerate_boards() -> list[dict]:
                return [b.row() for b in boards if b.plugged]
            made = ac.AutoConnectWatcher(
                bridge=bridge, store=store, session=session,
                list_ports_fn=enumerate_boards, poll_s=0.2,
            )
            watchers.append(made)
            made.start()
            return made

    try:
        yield Rig
    finally:
        for watcher in watchers:
            watcher.stop()
        for bridge in bridges:
            bridge.stop()
        for board in boards:
            board.unplug()


def test_a_pulled_cable_is_noticed_quickly_and_costs_no_cpu(rig):
    board = rig.board("ttyACM0")
    store, _ = rig.bridge(board)
    assert wait_for(lambda: connected(store)), "the board was never heard"

    pulled = time.monotonic()
    board.unplug()
    assert wait_for(
        lambda: store.get_snapshot()["link_status"] == "reconnecting", timeout=8.0,
    ), "the dead port was not noticed before the heartbeat timeout"
    assert time.monotonic() - pulled < 6.0
    assert store.get_snapshot()["connected"] is False

    # Reconnect attempts against a missing device fail at once; the backoff
    # between them is what keeps a field laptop's core from being pegged.
    cpu = time.process_time()
    time.sleep(2.0)
    assert time.process_time() - cpu < 1.0
    assert "unavailable" in store.get_snapshot()["link_error"] or (
        "read failed" in store.get_snapshot()["link_error"]
    )


def test_the_same_port_plugged_back_in_reconnects_every_time(rig):
    board = rig.board("ttyACM0")
    store, bridge = rig.bridge(board)
    assert wait_for(lambda: connected(store))
    time.sleep(0.5)
    fds = open_fds()
    threads_before = Counter(t.name for t in threading.enumerate())

    for cycle in range(3):
        board.unplug()
        assert wait_for(lambda: not store.get_snapshot()["connected"]), cycle
        time.sleep(0.5)
        board = rig.board("ttyACM0")
        assert wait_for(lambda: connected(store)), f"no reconnect on cycle {cycle}"

    time.sleep(0.5)
    assert bridge.connection_string() == f"serial:{rig.dev / 'ttyACM0'}:57600"
    # Worker threads come and go (the version request gives up on a board
    # that never answers it), so the check is that none piles up.
    names = Counter(t.name for t in threading.enumerate())
    piled = {
        name: n for name, n in names.items()
        if n > max(threads_before[name], 1)
    }
    assert not piled, f"threads accumulated across replugs: {piled}"
    assert open_fds() == fds, "a replug cycle leaked a file descriptor"


def test_a_cable_that_drops_out_for_a_moment_comes_back(rig):
    """A loose connector: gone and back within a fraction of a second."""
    board = rig.board("ttyACM0")
    store, _ = rig.bridge(board)
    assert wait_for(lambda: connected(store))
    board.unplug()
    time.sleep(0.3)
    rig.board("ttyACM0")
    replugged = time.monotonic()
    assert wait_for(lambda: heard_since(store, replugged)), (
        "the station never heard the board on the replugged port"
    )


def test_a_hand_chosen_link_follows_its_board_to_a_new_port(rig):
    """The board is plugged into a different socket and gets a new node name.
    Before, the bridge retried the old name for the rest of the session,
    because a manual connect switches every automatic decision off."""
    board = rig.board("ttyACM0")
    store, bridge = rig.bridge(board, baud=115200)
    session = ac.SessionState()
    session.note_manual_connect()
    rig.watcher(bridge, store, session)
    assert wait_for(lambda: connected(store))
    time.sleep(1.0)  # several watcher ticks while the board is there

    board.unplug()
    time.sleep(0.3)
    moved = rig.board("ttyACM1")

    assert wait_for(
        lambda: bridge.connection_string() == f"serial:{moved.path}:115200"
        and connected(store),
    ), f"still on {bridge.connection_string()}"
    assert session.manual_override is True


def test_a_different_board_on_a_new_port_is_left_to_the_operator(rig):
    board = rig.board("ttyACM0", serial_number="AAAA")
    store, bridge = rig.bridge(board)
    session = ac.SessionState()
    session.note_manual_connect()
    rig.watcher(bridge, store, session)
    assert wait_for(lambda: connected(store))
    time.sleep(1.0)

    board.unplug()
    rig.board("ttyACM1", serial_number="BBBB")
    assert wait_for(
        lambda: store.get_snapshot()["link_status"] == "reconnecting", timeout=8.0,
    )
    time.sleep(1.5)
    assert bridge.connection_string() == f"serial:{board.path}:57600"
    assert not store.get_snapshot()["connected"]


def test_a_port_held_by_another_station_is_refused_and_freed_by_a_switch(rig):
    """Two stations on one serial port each receive half the stream and
    neither shows a fault. The second is refused; once the first moves to
    another port, the second gets it."""
    shared = rig.board("ttyACM0")
    other = rig.board("ttyACM1")
    store_a, bridge_a = rig.bridge(shared)
    assert wait_for(lambda: connected(store_a))

    store_b, bridge_b = rig.bridge(shared)
    assert wait_for(
        lambda: "in use by another program" in store_b.get_snapshot()["link_error"],
        timeout=8.0,
    )
    assert not store_b.get_snapshot()["connected"]

    # What POST /api/mavlink/connect does to switch station A's port.
    bridge_a.stop()
    bridge_a.set_connection(f"serial:{other.path}:57600")
    bridge_a.start()
    assert wait_for(lambda: connected(store_a)), "station A on the other port"
    assert wait_for(lambda: connected(store_b)), "station B got the freed port"


def test_shutdown_after_a_replug_leaves_nothing_behind(rig):
    before = {t.name for t in threading.enumerate()}
    fds = open_fds()
    board = rig.board("ttyACM0")
    store, bridge = rig.bridge(board)
    assert wait_for(lambda: connected(store))
    board.unplug()
    assert wait_for(lambda: not store.get_snapshot()["connected"])
    board = rig.board("ttyACM0")
    assert wait_for(lambda: connected(store))

    bridge.stop()
    board.unplug()
    assert wait_for(
        lambda: not {t.name for t in threading.enumerate()} - before, timeout=6.0,
    ), sorted({t.name for t in threading.enumerate()} - before)
    assert open_fds() <= fds
