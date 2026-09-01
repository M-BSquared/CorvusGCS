"""Integration tests authored by the review/safety agent.

These cover the cross-component contracts that no single unit test exercises.
They are deterministic, pure-stdlib + the repo's own modules, and run in well
under two seconds. The fakes mirror the shared patterns from
``tests/test_mavlink_takeoff.py`` (``FakeMessage``, ``heartbeat``, ``ack`` are
imported from there; a local recording connection records the param-set and
command sends so the refuse-when-armed path can prove "no command was sent").

Cases:
  (a) State store + MAVLink bridge dispatch pipeline: dispatch PARAM_VALUE →
      VIBRATION → HEARTBEAT and assert the param reaches a param listener +
      bridge cache, the vibration fields reach the store snapshot, and the
      armed/mode state reaches the store snapshot.
  (b) Refuse-when-armed end-to-end: with an armed HEARTBEAT dispatched,
      set_param / calibrate / autotune are refused client-side (no command
      sent, "armed" in the error) while set_vibration_stream is NOT refused
      (a command is actually sent and accepted).
  (c) SSE params listener cleanup: the real ``_BoundedSseBuffer`` used by
      ``_sse_params`` plus the bridge's ``add_param_listener`` /
      ``remove_param_listener`` leave no listener behind.
  (d) Version single-source: ``get_version()`` equals the VERSION file, no
      .py/.js/.html/.css file (outside the canonical source) hardcodes the
      literal, and README.md does not hardcode it either.
  (e) Shutdown race mechanism: ``tests/test_shutdown.py`` exists and its
      ``Event.wait()`` race-fix tests pass (the mechanism is covered there;
      this only confirms it, it does not duplicate it).
"""
from __future__ import annotations

import importlib
import pathlib
import sys
import time
from typing import Any, Callable

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge
from corvus.server import PARAMS_SSE_CAPACITY, _BoundedSseBuffer
from corvus.state_store import VehicleStateStore
from corvus.version import get_version

# Make both the repo root (for `corvus`) and the tests dir (for the sibling
# test fakes) importable regardless of the pytest import mode in use.
_TESTS_DIR = pathlib.Path(__file__).resolve().parent
_REPO_ROOT = _TESTS_DIR.parent
for _p in (str(_REPO_ROOT), str(_TESTS_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

# Reuse the shared message-fake patterns from the takeoff suite.
from test_mavlink_takeoff import FakeMessage, ack, heartbeat  # noqa: E402


# ---------------------------------------------------------------------------
# Local recording connection (mirrors the FakeConnection pattern, records both
# command_long_send and param_set_send so the armed-refusal path can prove no
# command and no param write left the client).
# ---------------------------------------------------------------------------

class _RecordingMav:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.commands: list[tuple] = []
        self.param_sets: list[tuple] = []
        self.param_requests: list[tuple] = []
        self.param_reads: list[tuple] = []
        self.on_send = on_send

    def command_long_send(self, *args: float) -> None:
        self.commands.append(args)
        if self.on_send:
            self.on_send(args)

    def param_request_list_send(self, target_system: int, target_component: int) -> None:
        self.param_requests.append((target_system, target_component))

    def param_request_read_send(
        self, target_system: int, target_component: int, name: bytes, index: int
    ) -> None:
        self.param_reads.append((target_system, target_component, name, index))

    def param_set_send(
        self, target_system: int, target_component: int,
        name: bytes, value: float, ptype: int,
    ) -> None:
        self.param_sets.append((target_system, target_component, name, value, ptype))


class _RecordingConnection:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.mav = _RecordingMav(on_send)
        self.source_system = 255
        self.source_component = 190


def _ready_bridge(on_send: Callable[[tuple], None] | None = None) -> MavlinkBridge:
    """A connected, non-stale bridge backed by a recording connection."""
    store = VehicleStateStore()
    store.heartbeat()  # connected=True + fresh heartbeat → not stale
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = _RecordingConnection(on_send)
    return bridge


def _param_value(
    name: str, value: float,
    ptype: int = mavutil.mavlink.MAV_PARAM_TYPE_REAL32,
    index: int = 0, count: int = 0,
) -> FakeMessage:
    return FakeMessage(
        message_type="PARAM_VALUE",
        param_id=name.encode("utf-8"),
        param_value=value,
        param_type=ptype,
        param_index=index,
        param_count=count,
    )


def _vibration(
    vx: float, vy: float, vz: float,
    c0: int = 0, c1: int = 0, c2: int = 0,
) -> FakeMessage:
    return FakeMessage(
        message_type="VIBRATION",
        time_usec=0,
        vibration_x=vx,
        vibration_y=vy,
        vibration_z=vz,
        clipping_0=c0,
        clipping_1=c1,
        clipping_2=c2,
    )


# ---------------------------------------------------------------------------
# (a) Full dispatch → store (+ param listener) pipeline
# ---------------------------------------------------------------------------

def test_dispatch_pipeline_param_vibration_heartbeat_reach_store() -> None:
    bridge = _ready_bridge()
    statuses: list[dict[str, Any]] = []
    bridge.add_param_listener(statuses.append)

    bridge._dispatch(_param_value("MC_ROLL_P", 6.0, index=0, count=1))
    bridge._dispatch(_vibration(0.12, 0.34, 0.56, 1, 2, 3))
    bridge._dispatch(heartbeat(armed=True))

    # PARAM_VALUE → param listener (the /api/params/progress SSE path) + cache.
    assert len(statuses) == 1
    assert statuses[0]["name"] == "MC_ROLL_P"
    assert statuses[0]["value"] == 6.0
    cached = bridge.get_param("MC_ROLL_P")
    assert cached is not None
    assert cached["value"] == 6.0

    snap = bridge._store.get_snapshot()
    # VIBRATION → store snapshot.
    assert snap["vibration_x"] == 0.12
    assert snap["vibration_y"] == 0.34
    assert snap["vibration_z"] == 0.56
    assert snap["clipping_0"] == 1
    assert snap["clipping_1"] == 2
    assert snap["clipping_2"] == 3
    # HEARTBEAT → store armed/mode + connected.
    assert snap["armed"] is True
    assert snap["mode"] == "POSCTL"
    assert snap["connected"] is True


# ---------------------------------------------------------------------------
# (b) Refuse-when-armed end-to-end (defense-in-depth, backend-enforced)
# ---------------------------------------------------------------------------

def test_refuse_when_armed_set_param_calibrate_autotune_but_not_vibration(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # No real sleeps: the armed-refusal paths return before any wait loop, and
    # the vibration path is auto-acked synchronously.
    monkeypatch.setattr(time, "sleep", lambda s: None)

    def on_send(args: tuple) -> None:
        command = int(args[2])
        if command == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL:
            bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge = _ready_bridge(on_send)
    bridge._dispatch(heartbeat(armed=True))  # store: armed, connected, fresh hb
    assert bridge._store.get_snapshot()["armed"] is True

    # set_param: refused client-side → no param write, no command.
    assert bridge.set_param("MC_ROLL_P", 8.0) is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.param_sets == []
    assert bridge._conn.mav.param_reads == []
    assert bridge._conn.mav.commands == []

    # calibrate: refused client-side → no command.
    assert bridge.calibrate("gyro") is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []

    # autotune: refused client-side → no command.
    assert bridge.autotune("roll") is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []

    # set_vibration_stream: NOT refused while armed — read-only stream-rate
    # control is safe. A SET_MESSAGE_INTERVAL command is actually sent and
    # accepted here.
    vib_ok = bridge.set_vibration_stream(True, 10)
    err = bridge.get_last_command_error()
    assert "armed" not in err
    assert vib_ok is True
    assert bridge._conn.mav.commands, "vibration stream-rate command was expected while armed"
    assert bridge._conn.mav.commands[-1][2] == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL


# ---------------------------------------------------------------------------
# (c) SSE params listener cleanup with the real _BoundedSseBuffer
# ---------------------------------------------------------------------------

def test_sse_params_buffer_and_bridge_listener_leave_no_leftover() -> None:
    bridge = _ready_bridge()
    buf = _BoundedSseBuffer(PARAMS_SSE_CAPACITY)
    listener = buf.put_latest
    bridge.add_param_listener(listener)

    # A param dispatch flows bridge → listener → buffer (the /api/params/progress
    # wiring), coalesced to the single latest item.
    bridge._dispatch(_param_value("MC_ROLL_P", 6.0, index=0, count=1))
    assert buf.qsize() == 1
    item = buf.get(timeout=0.1)
    assert item["name"] == "MC_ROLL_P"
    assert item["value"] == 6.0
    assert buf.qsize() == 0

    # Removing the listener empties the bridge's listener list (no leak).
    bridge.remove_param_listener(listener)
    assert bridge._param_listeners == []

    # A subsequent dispatch must NOT push into the buffer — the listener is gone.
    bridge._dispatch(_param_value("MC_PITCH_P", 7.0, index=1, count=2))
    assert buf.qsize() == 0


# ---------------------------------------------------------------------------
# (d) Version single-source enforcement
# ---------------------------------------------------------------------------

def _version_from_file() -> str:
    return (_REPO_ROOT / "VERSION").read_text(encoding="utf-8").strip()


def test_get_version_matches_version_file() -> None:
    assert get_version() == _version_from_file()


def test_no_hardcoded_version_literal_outside_canonical_source() -> None:
    version = _version_from_file()
    assert version, "VERSION file is empty"
    exclusions = {"corvus/version.py"}        # the only Python source allowed to name it
    skip_dir_parts = {".opencode", ".git", "__pycache__", ".pytest_cache", "node_modules"}
    extensions = {".py", ".js", ".html", ".css"}
    offenders: list[str] = []
    for path in _REPO_ROOT.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(_REPO_ROOT)
        if any(part in skip_dir_parts for part in rel.parts):
            continue
        if rel.as_posix() in exclusions:
            continue
        if path.suffix.lower() not in extensions:
            continue
        try:
            data = path.read_bytes()
        except OSError:
            continue
        if b"\x00" in data[:8192]:             # binary heuristic — skip binaries
            continue
        if version in data.decode("utf-8", errors="ignore"):
            offenders.append(rel.as_posix())
    assert not offenders, f"hardcoded version literal {version!r} found in: {offenders}"


def test_readme_does_not_hardcode_version_literal() -> None:
    version = _version_from_file()
    readme = _REPO_ROOT / "README.md"
    text = readme.read_text(encoding="utf-8")
    assert version not in text, f"README.md hardcodes the version literal {version!r}"


# ---------------------------------------------------------------------------
# (e) Shutdown race mechanism — confirm tests/test_shutdown.py exists & passes
# ---------------------------------------------------------------------------

def test_shutdown_race_mechanism_is_covered_and_passes() -> None:
    mod_path = _TESTS_DIR / "test_shutdown.py"
    assert mod_path.is_file(), "tests/test_shutdown.py must exist (Event.wait race-fix coverage)"
    mod = importlib.import_module("test_shutdown")
    for name in (
        "test_wait_returns_when_event_set_before_wait",
        "test_wait_blocks_when_event_not_set",
        "test_wait_returns_immediately_after_set",
    ):
        assert hasattr(mod, name), f"test_shutdown.py is missing {name}"
        getattr(mod, name)()  # raises on any assertion failure → confirms it passes
