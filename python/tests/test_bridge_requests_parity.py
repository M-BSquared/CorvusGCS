"""The Rust request engine and signing against the Python they replaced.

``tests/reference/mavlink_bridge_requests_python.py`` keeps the bridge's own
requests (stream rates, the interval batch with its fallback, home, version,
the GCS authority check, the interval command) and applying signing, and
``tests/reference/mavlink_signing_python.py`` the key loader, as they were in
Python. Each test runs both through the same key files, links and scripted
answers and compares what was sent, logged, stored, shown and raised.
"""
from __future__ import annotations

import logging
import random
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

import corvus.mavlink_signing as rust_signing  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_bridge_requests_python as reference  # noqa: E402
from tests.reference import mavlink_signing_python as reference_signing  # noqa: E402

M = mavutil.mavlink


class ReferenceBridge(reference.RequestMixin, MavlinkBridge):
    """A bridge whose own requests and signing are the Python reference."""


KINDS = (ReferenceBridge, MavlinkBridge)


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", call(*args, **kwargs))
    except Exception as exc:  # noqa: BLE001 - compared below
        cause = exc.__cause__
        return ("error", type(exc).__name__, str(exc), type(cause).__name__ if cause else None)


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list[tuple[str, str]] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage()))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    log = logging.getLogger("corvus.mavlink")
    previous = log.level
    log.addHandler(handler)
    log.setLevel(logging.DEBUG)
    yield handler
    log.removeHandler(handler)
    log.setLevel(previous)


# ----------------------------------------------------------------- signing

KEYS: dict[str, Any] = {
    "raw": bytes(range(1, 33)),
    "raw-with-1a": bytes([0x1A] * 32),
    "hex": bytes(range(1, 33)).hex().encode(),
    "hex-newline": b"  " + bytes(range(1, 33)).hex().encode() + b"\n",
    "hex-upper": bytes(range(1, 33)).hex().upper().encode(),
    "bad-hex": b"zz" * 32,
    "non-ascii-hex": "é".encode() * 32,
    "short": b"abc",
    "long": b"x" * 129,
    "zero": bytes(32),
    "zero-hex": b"00" * 32,
}


@pytest.mark.parametrize("case", [*KEYS, "unset", "blank", "missing", "directory", "symlink", "group-readable"])
def test_signing_keys_load_alike(case: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = tmp_path / "key"
    if case in KEYS:
        path.write_bytes(KEYS[case])
        path.chmod(0o600)
    elif case == "directory":
        path.mkdir()
    elif case == "symlink":
        target = tmp_path / "real"
        target.write_bytes(KEYS["raw"])
        target.chmod(0o600)
        path.symlink_to(target)
    elif case == "group-readable":
        path.write_bytes(KEYS["raw"])
        path.chmod(0o640)
    if case == "unset":
        monkeypatch.delenv(rust_signing.MAVLINK_SIGNING_KEY_FILE_ENV, raising=False)
    else:
        monkeypatch.setenv(rust_signing.MAVLINK_SIGNING_KEY_FILE_ENV, "   " if case == "blank" else f" {path} ")
    mine = outcome(rust_signing._load_signing_key)
    theirs = outcome(reference_signing._load_signing_key)
    if mine[0] == "error" and mine[1] in ("FileNotFoundError", "OSError", "IsADirectoryError"):
        mine, theirs = mine[:2], theirs[:2]
    assert mine == theirs


@pytest.mark.parametrize("msg_id", [109, 246, 247, 0, 76, "109", 109.0, True, 2**70, None, "x"], ids=repr)
def test_the_unsigned_exemptions_match(msg_id: Any) -> None:
    mine = outcome(rust_signing._allow_unsigned_signed_link, None, msg_id)
    assert mine == outcome(reference_signing._allow_unsigned_signed_link, None, msg_id)


class SignLink:
    def __init__(self, version: str, fails: bool, log: list) -> None:
        self.WIRE_PROTOCOL_VERSION = version
        self.fails = fails
        self.log = log

    def auto_mavlink_version(self, buf: bytes) -> None:
        self.log.append(("auto", buf))
        self.WIRE_PROTOCOL_VERSION = "2.0"

    def setup_signing(self, key: bytes, sign_outgoing: bool = False, allow_unsigned_callback: Any = None) -> None:
        self.log.append(("setup", key, sign_outgoing, allow_unsigned_callback(None, 109),
                         allow_unsigned_callback(None, 11)))
        if self.fails:
            raise RuntimeError("dialect has no signing")


@pytest.mark.parametrize("key", ["none", "good", "bad", "unreadable"])
@pytest.mark.parametrize("version", ["1.0", "2.0"])
@pytest.mark.parametrize("fails", [False, True])
def test_applying_signing_matches(key: str, version: str, fails: bool, tmp_path: Path, logs: Logs,
                                  monkeypatch: pytest.MonkeyPatch) -> None:
    path = tmp_path / "key"
    if key in ("good", "bad"):
        path.write_bytes(KEYS["raw"] if key == "good" else b"short")
        path.chmod(0o600)
    if key == "none":
        monkeypatch.delenv(rust_signing.MAVLINK_SIGNING_KEY_FILE_ENV, raising=False)
    else:
        monkeypatch.setenv(rust_signing.MAVLINK_SIGNING_KEY_FILE_ENV, str(path))
    results = []
    for kind in KINDS:
        logs.lines.clear()
        bridge = kind(VehicleStateStore(), "udp:0.0.0.0:14550")
        log: list = []
        bridge._conn = SignLink(version, fails, log)
        bridge._console_publish = lambda name, text, level, log=log: log.append(("console", name, text, level))
        result = outcome(bridge._apply_signing)
        if result[0] == "error" and "No such file" in result[2]:
            result = result[:2]
        results.append((result, log, bridge._store.get_snapshot().get("link_error"),
                        [line for line in logs.lines if "No such file" not in line[1]]))
    assert results[1] == results[0]


# ------------------------------------------------------------ the rates

@pytest.mark.parametrize("conn", ["udp:0.0.0.0:14550", "serial:/dev/ttyUSB0:57600", "serial:/dev/ttyACM0:115200",
                                  "tcp:127.0.0.1:5760"])
def test_the_rate_tables_match(conn: str) -> None:
    tables = [(kind(VehicleStateStore(), conn)._stream_rates(), kind(VehicleStateStore(), conn)._message_intervals())
              for kind in KINDS]
    assert tables[1] == tables[0]
    assert [list(t) for t in tables[1]] == [list(t) for t in tables[0]], "order"


class Link:
    def __init__(self, log: list, fail_on: set[int] = frozenset(), swap: Any = None) -> None:
        self.log = log
        self.fail_on = set(fail_on)
        self.swap = swap
        self.calls = 0
        self.mav = SimpleNamespace(request_data_stream_send=self.stream, command_long_send=self.command,
                                   autopilot_version_request_send=self.version)

    def _maybe_fail(self) -> None:
        self.calls += 1
        if self.swap is not None and self.calls == self.swap[0]:
            self.swap[1]()
        if self.calls in self.fail_on:
            raise OSError("radio gone")

    def stream(self, *args: Any) -> None:
        self.log.append(("stream", args))
        self._maybe_fail()

    def command(self, *args: Any) -> None:
        self.log.append(("command", [str(a) for a in args]))
        self._maybe_fail()

    def version(self, *args: Any) -> None:
        self.log.append(("version", args))
        self._maybe_fail()


@pytest.mark.parametrize("fail_on", [set(), {2}, {1, 3, 5}])
@pytest.mark.parametrize("swap_at", [None, 3])
@pytest.mark.parametrize("already", [False, True])
def test_legacy_stream_requests_match(fail_on: set[int], swap_at: Any, already: bool, logs: Logs) -> None:
    results = []
    for kind in KINDS:
        logs.lines.clear()
        bridge = kind(VehicleStateStore(), "udp:0.0.0.0:14550")
        log: list = []
        swap = None if swap_at is None else (swap_at, lambda b=bridge: setattr(b, "_conn", SimpleNamespace()))
        bridge._conn = Link(log, fail_on, swap)
        bridge._request_sent = already
        bridge._request_streams()
        bridge._request_streams()
        results.append((log, bridge._request_sent, logs.lines[:]))
    assert results[1] == results[0]


ANSWERS = [0, 2, 4, 3, -1, -2, "raise"]


@pytest.mark.parametrize("seed", range(12))
@pytest.mark.parametrize("conn", ["udp:0.0.0.0:14550", "serial:/dev/ttyUSB0:57600"])
def test_the_interval_batch_matches(seed: int, conn: str, logs: Logs) -> None:
    results = []
    for kind in KINDS:
        logs.lines.clear()
        rng = random.Random(seed)
        bridge = kind(VehicleStateStore(), conn)
        log: list = []

        def answer(msg_id: int, interval: int, quiet: bool = False, _log: list = log, _rng: Any = rng) -> Any:
            choice = _rng.choice(ANSWERS[:5] * 3 + ANSWERS[5:])
            _log.append(("interval", msg_id, interval, quiet, choice))
            if choice == "raise":
                raise RuntimeError("bad")
            return (choice == 0, choice)

        bridge._set_message_interval = answer
        bridge._request_streams = lambda _log=log: _log.append(("fallback",))
        bridge._request_message_intervals()
        results.append((log, logs.lines[:]))
    assert results[1] == results[0]


@pytest.mark.parametrize("result", [0, 2, 3, 4, -1, -2, 99])
@pytest.mark.parametrize("quiet", [False, True])
@pytest.mark.parametrize("ready", [True, False])
def test_the_interval_command_matches(result: int, quiet: bool, ready: bool, logs: Logs) -> None:
    results = []
    for kind in KINDS:
        logs.lines.clear()
        store = VehicleStateStore()
        store.heartbeat()
        store.update(connected=True)
        bridge = kind(store)
        log: list = []
        bridge._conn = SimpleNamespace()
        if not ready:
            bridge._connection_ready = lambda: False
        bridge._send_command_and_wait = lambda cmd, params, _log=log, **kw: (_log.append((cmd, [str(p) for p in params],
                                                                                          kw)), result)[1]
        bridge._console_publish = lambda name, text, level, _log=log: _log.append(("console", name, text, level))
        verdict = outcome(bridge._set_message_interval, 33, 100_000, quiet=quiet)
        public = outcome(bridge.set_message_interval, 33, 100_000)
        results.append((verdict, public, log, bridge.get_last_command_error(), logs.lines[:]))
    assert results[1] == results[0]


@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("values", [{}, {"SYSID_MYGCS": 255.0}, {"SYSID_MYGCS": 254.0}, {"MAV_GCS_SYSID": 253.6},
                                    {"MAV_GCS_SYSID": 1.0, "SYSID_MYGCS": 254.0}], ids=repr)
@pytest.mark.parametrize("ready", [True, False])
def test_the_gcs_authority_check_matches(stack: str, values: dict, ready: bool, logs: Logs) -> None:
    results = []
    for kind in KINDS:
        logs.lines.clear()
        bridge = kind(VehicleStateStore())
        bridge._latch_dialect(M.MAV_AUTOPILOT_PX4 if stack == "px4" else M.MAV_AUTOPILOT_ARDUPILOTMEGA, 2)
        bridge._connection_ready = lambda ready=ready: ready
        log: list = []
        bridge.fetch_params = lambda names, timeout=0, _log=log: (_log.append((names, timeout)), dict(values))[1]
        bridge._console_publish = lambda name, text, level, _log=log: _log.append(("console", name, text, level))
        bridge._store_update_warning = lambda text, level, event=None, _log=log: _log.append(("warn", text, level))
        bridge._check_gcs_authority()
        results.append((log, logs.lines[:]))
    assert results[1] == results[0]


@pytest.mark.parametrize("fail_on", [set(), {1}, {2}, {1, 2, 3}])
@pytest.mark.parametrize("connected", [True, False])
def test_home_and_version_requests_match(fail_on: set[int], connected: bool, logs: Logs) -> None:
    results = []
    for kind in KINDS:
        logs.lines.clear()
        bridge = kind(VehicleStateStore())
        log: list = []
        bridge._conn = Link(log, fail_on) if connected else None
        bridge._schedule_version_retry = lambda _log=log: _log.append(("retry",))
        bridge._request_home()
        bridge._request_version()
        results.append((log, logs.lines[:]))
    assert results[1] == results[0]
