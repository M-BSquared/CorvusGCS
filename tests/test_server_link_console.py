"""The two capabilities the LINK and CONSOLE panels gained.

``POST /api/mavlink/disconnect``
    Until now the only way out of a connection was into another one, so an
    operator who wanted the radio free — to hand the aircraft to another GCS,
    swap a cable, or stop a reconnect loop hammering a port that had moved —
    had to quit the app.

``POST /api/console/save``
    Writes the console transcript to disk. Server-side for the same reason as
    parameter export: the desktop build runs in QtWebEngine, which drops an
    ``<a download>`` unless the host implements a handler, so a
    browser-download route would silently produce nothing there.

Hermetic: a fake bridge, temp directories, no network and no vehicle.
"""
from __future__ import annotations

import json
import os

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus.config import CorvusConfig  # noqa: E402
from corvus.server import CorvusHandler, _safe_filename  # noqa: E402

from conftest import posix_permissions


class _FakeBridge:
    """Records stop() calls; optionally raises to exercise the failure path."""

    def __init__(self, raises: bool = False) -> None:
        self.stops = 0
        self.raises = raises

    def stop(self) -> None:
        self.stops += 1
        if self.raises:
            raise RuntimeError("serial port vanished")


class _Captured:
    """Handler stand-in that records what _send_json was called with."""

    def __init__(self) -> None:
        self.payload = None
        self.status = 200

    def __call__(self, payload, status=200):
        self.payload = payload
        self.status = status


@pytest.fixture
def handler(tmp_path):
    h = object.__new__(CorvusHandler)
    h.mavlink = _FakeBridge()
    h.config = CorvusConfig(tlog_dir=str(tmp_path / "logs"))
    h.config_path = str(tmp_path / "config.json")
    h._send_json = _Captured()
    return h


# ---------------------------------------------------------------------------
# Disconnect
# ---------------------------------------------------------------------------

def test_disconnect_stops_the_bridge(handler) -> None:
    handler._api_mavlink_disconnect({})
    assert handler.mavlink.stops == 1
    assert handler._send_json.status == 200
    assert handler._send_json.payload == {"ok": True}


def test_disconnect_is_idempotent(handler) -> None:
    """Disconnecting an already-closed link is a success — the state the caller
    asked for is the state they get. stop() is itself idempotent."""
    for _ in range(3):
        handler._api_mavlink_disconnect({})
        assert handler._send_json.payload == {"ok": True}
    assert handler.mavlink.stops == 3


def test_disconnect_without_a_bridge_is_a_503(handler) -> None:
    handler.mavlink = None
    handler._api_mavlink_disconnect({})
    assert handler._send_json.status == 503
    assert handler._send_json.payload["ok"] is False


def test_disconnect_reports_a_teardown_failure_instead_of_crashing(handler) -> None:
    """A failing teardown must surface as an error, not take the server down —
    the operator still needs the rest of the UI."""
    handler.mavlink = _FakeBridge(raises=True)
    handler._api_mavlink_disconnect({})
    assert handler._send_json.status == 500
    assert handler._send_json.payload["ok"] is False
    assert "serial port vanished" in handler._send_json.payload["error"]


def test_disconnect_accepts_the_dispatched_payload(handler) -> None:
    """Every POST handler is called with the parsed body, even the ones with
    nothing to read from it — a signature that omits it is a 500 at runtime."""
    import inspect
    sig = inspect.signature(CorvusHandler._api_mavlink_disconnect)
    assert len(sig.parameters) == 2, "handler must accept (self, payload)"


# ---------------------------------------------------------------------------
# Console save
# ---------------------------------------------------------------------------

def test_console_save_writes_beside_the_tlogs(handler, tmp_path) -> None:
    handler._api_console_save({"filename": "session.log", "text": "12:00:00  hello"})
    res = handler._send_json.payload
    assert res["ok"] is True
    assert res["dir"] == str(tmp_path / "logs")
    assert os.path.isfile(res["path"])
    with open(res["path"], encoding="utf-8") as f:
        assert f.read() == "12:00:00  hello\n"


def test_console_save_creates_the_directory(handler, tmp_path) -> None:
    assert not (tmp_path / "logs").exists()
    handler._api_console_save({"text": "x"})
    assert (tmp_path / "logs").is_dir()


def test_console_save_generates_a_dated_default_name(handler) -> None:
    handler._api_console_save({"text": "x"})
    name = handler._send_json.payload["filename"]
    assert name.startswith("corvus-console_") and name.endswith(".log")


def test_console_save_does_not_double_the_trailing_newline(handler) -> None:
    handler._api_console_save({"filename": "a.log", "text": "line\n"})
    with open(handler._send_json.payload["path"], encoding="utf-8") as f:
        assert f.read() == "line\n"


@pytest.mark.parametrize("payload", [
    {"text": ""},
    {"text": "   "},
    {"text": None},
    {},
])
def test_console_save_rejects_an_empty_transcript(handler, payload) -> None:
    handler._api_console_save(payload)
    assert handler._send_json.status == 400
    assert handler._send_json.payload["ok"] is False


def test_console_save_cannot_escape_the_log_directory(handler, tmp_path) -> None:
    handler._api_console_save({"filename": "../../escaped.log", "text": "x"})
    res = handler._send_json.payload
    assert res["path"] == str(tmp_path / "logs" / "escaped.log")
    assert not (tmp_path.parent / "escaped.log").exists()


@posix_permissions
def test_console_save_reports_an_unwritable_directory(handler, tmp_path) -> None:
    blocked = tmp_path / "blocked"
    blocked.mkdir()
    blocked.chmod(0o500)
    handler.config = CorvusConfig(tlog_dir=str(blocked))
    try:
        handler._api_console_save({"text": "x"})
        assert handler._send_json.status == 400
        assert str(blocked) in handler._send_json.payload["error"]
    finally:
        blocked.chmod(0o700)


def test_console_save_leaves_no_temp_file_behind(handler, tmp_path) -> None:
    handler._api_console_save({"filename": "a.log", "text": "x"})
    leftovers = [p for p in os.listdir(tmp_path / "logs") if p.endswith(".tmp")]
    assert leftovers == []


# ---------------------------------------------------------------------------
# The shared filename sanitiser now carries a suffix
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("raw,suffix,expected", [
    ("run", ".log", "run.log"),
    ("run.log", ".log", "run.log"),
    ("run.LOG", ".log", "run.log"),
    ("../../etc/passwd", ".log", "passwd.log"),
    ("params", ".json", "params.json"),
    ("params.json", ".json", "params.json"),
])
def test_safe_filename_respects_the_requested_suffix(raw, suffix, expected) -> None:
    assert _safe_filename(raw, "FALLBACK" + suffix, suffix=suffix) == expected


def test_safe_filename_defaults_to_json_for_existing_callers() -> None:
    """Parameter export calls it without a suffix; that must keep working."""
    assert _safe_filename("params", "FB.json") == "params.json"


# ---------------------------------------------------------------------------
# Connect must validate BEFORE it tears anything down
# ---------------------------------------------------------------------------

class _TrackingBridge:
    """Records the order of stop/set_connection/start."""

    _VALID_PREFIXES = (
        "udp:", "udpin:", "udpout:", "udpbcast:", "tcp:", "tcpin:", "serial:",
    )

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.conn = "udp:0.0.0.0:14540"

    def validate_connection(self, conn_str):
        from corvus.mavlink_bridge import MavlinkBridge
        MavlinkBridge.validate_connection(self, conn_str)

    def stop(self):
        self.calls.append("stop")

    def set_connection(self, conn):
        self.validate_connection(conn)
        self.calls.append("set")
        self.conn = conn

    def start(self):
        self.calls.append("start")

    @staticmethod
    def _parse_serial(conn):
        rest = conn.split(":", 1)[1]
        device, _, baud = rest.rpartition(":")
        try:
            return device, int(baud)
        except ValueError:
            return device, 0


@pytest.fixture
def connect_handler(tmp_path):
    h = object.__new__(CorvusHandler)
    h.mavlink = _TrackingBridge()
    h.config = CorvusConfig()
    h.config_path = str(tmp_path / "config.json")
    h._send_json = _Captured()
    return h


def test_a_bad_connection_string_does_not_touch_the_live_link(connect_handler) -> None:
    """The bug this guards: connect() stopped the bridge and only THEN
    validated, so a typo in the connection field killed a working radio link
    and handed back a 400. The operator lost the aircraft to a spelling
    mistake, with no way back except retyping the old string from memory."""
    connect_handler._api_mavlink_connect({"connection": "rtsp://127.0.0.1:14550"})
    assert connect_handler._send_json.status == 400
    assert connect_handler.mavlink.calls == [], (
        "a rejected connection string must not stop, reconfigure or restart the bridge"
    )
    assert connect_handler.mavlink.conn == "udp:0.0.0.0:14540", "the live link is untouched"


@pytest.mark.parametrize("bad", [
    "",
    None,
    123,
    "ftp://nope",
    "serial:",
    "serial:/dev/ttyUSB0:notanumber",
])
def test_every_rejected_form_leaves_the_link_alone(connect_handler, bad) -> None:
    connect_handler._api_mavlink_connect({"connection": bad})
    assert connect_handler._send_json.status == 400
    assert connect_handler.mavlink.calls == []


def test_a_good_connection_string_still_restarts_the_bridge(connect_handler) -> None:
    connect_handler._api_mavlink_connect({"connection": "udp:0.0.0.0:14550"})
    assert connect_handler._send_json.status == 200
    assert connect_handler._send_json.payload["ok"] is True
    # Order matters: the old link goes down before the new one is configured.
    assert connect_handler.mavlink.calls == ["stop", "set", "start"]
    assert connect_handler.mavlink.conn == "udp:0.0.0.0:14550"


def test_validate_connection_accepts_what_set_connection_accepts() -> None:
    """The two must not drift: validate is the pre-check for set."""
    from corvus.mavlink_bridge import MavlinkBridge
    bridge = _TrackingBridge()
    for good in ("udp:0.0.0.0:14540", "udpin:0.0.0.0:14540", "tcp:127.0.0.1:5760",
                 # The dial-out halves: a mavlink-router endpoint in Server
                 # mode binds and waits, so the station must speak first.
                 "udpout:127.0.0.1:14550", "tcpin:0.0.0.0:5760",
                 "serial:/dev/ttyUSB0:57600"):
        MavlinkBridge.validate_connection(bridge, good)   # must not raise
    for bad in ("", "udpout", "rtsp://nope", "serial:/dev/ttyUSB0:0"):
        with pytest.raises(ValueError):
            MavlinkBridge.validate_connection(bridge, bad)
