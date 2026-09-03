"""Regression tests for the new input-validation paths in corvus/server.py.

These pin the fixed behaviour introduced by the input-hardening pass so a
future refactor cannot silently regress the 400/413/503 contract:

- ``_handle_api_post`` Content-Length guard (non-numeric / negative -> 400)
  and cap (``> MAX_JSON_BODY_BYTES`` -> 413), invalid JSON (400), and a
  non-object JSON payload (400). Previously a non-numeric Content-Length
  raised an uncaught ``ValueError`` (dropping the connection) and an
  unbounded length could exhaust memory before ``json.loads`` ran.
- ``_api_firmware_upload_raw`` Content-Length guard (400) and cap
  (``> MAX_FIRMWARE_BODY_BYTES`` -> 413); the 413 must fire BEFORE the body
  is drained and before the flash-service availability check.
- ``POST /api/mavlink/connect`` validates the connection string up front
  (non-string / empty -> 400), returns 503 when no bridge is wired, and
  surfaces a ``set_connection`` ``ValueError`` as 400 (not a 500 / false-200).
- ``POST /api/ssh/send`` rejects a non-string ``data`` with 400 before it
  can crash ``channel.send`` or silently mis-send.

Handler-level (no socket): each test builds a ``CorvusHandler`` via
``object.__new__`` and stubs ``_send_json``, mirroring the pattern in
tests/test_server_config.py and tests/test_server_firmware.py.
"""
from __future__ import annotations

import json
from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")
from corvus.server import (  # noqa: E402
    MAX_FIRMWARE_BODY_BYTES,
    MAX_JSON_BODY_BYTES,
    CorvusHandler,
)


# ---------------------------------------------------------------------------
# Stubs
# ---------------------------------------------------------------------------

class _Headers:
    """Stand-in for the request headers exposing ``get``.

    ``content_length`` is returned verbatim for the ``Content-Length`` name so
    a test can inject a non-numeric value (``"abc"``) or ``None`` to exercise
    the guard. Every other header name returns the supplied default.
    """

    def __init__(self, content_length: Any) -> None:
        self._cl = content_length

    def get(self, name: str, default: Any = None) -> Any:
        return self._cl if name == "Content-Length" else default


class _ScriptedReader:
    """``rfile`` stub returning a scripted payload on the first ``read``.

    Returns the whole scripted payload regardless of the requested length
    (the handler reads ``length`` bytes in one call), then ``b"{}"`` on any
    subsequent call — matching the read-once contract the dispatcher uses.
    """

    def __init__(self, payload: bytes) -> None:
        self._payload = payload
        self._read = False

    def read(self, n: int) -> bytes:
        if not self._read:
            self._read = True
            return self._payload
        return b"{}"


def _handler(
    *,
    mavlink: Any = None,
    ssh: Any = None,
    flash: Any = None,
) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    """Construct a CorvusHandler with ``_send_json`` captured to a list."""
    handler = object.__new__(CorvusHandler)
    handler.mavlink = mavlink
    handler.ssh = ssh
    handler.flash = flash
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


class _FakeMavlink:
    """Minimal bridge stand-in whose ``set_connection`` validates prefixes.

    Mirrors the real ``MavlinkBridge.set_connection`` contract: a non-string
    or empty value, or an unknown prefix, raises ``ValueError`` so the HTTP
    layer surfaces a 400. ``stop``/``start`` are no-ops so the connect handler
    can run its stop->set->start sequence without a live bridge.
    """

    _VALID = ("udp:", "udpin:", "udpbcast:", "tcp:", "serial:")

    def __init__(self) -> None:
        self.connection = "udp:127.0.0.1:14540"
        self.set_calls: list[str] = []

    def stop(self) -> None:
        pass

    def start(self) -> None:
        pass

    def set_connection(self, conn_str: str) -> None:
        self.set_calls.append(conn_str)
        if not isinstance(conn_str, str) or not conn_str:
            raise ValueError("connection must be a non-empty string")
        if not conn_str.startswith(self._VALID):
            raise ValueError(
                "connection must start with one of: " + ", ".join(self._VALID)
            )
        self.connection = conn_str


class _FakeSsh:
    """Bridge stand-in recording ``send`` calls; always reports a session."""

    def __init__(self) -> None:
        self.send_calls: list[tuple[str, str]] = []

    def send(self, name: str, data: str) -> bool:
        self.send_calls.append((name, data))
        return True


# ---------------------------------------------------------------------------
# _handle_api_post — Content-Length guard / cap / JSON shape
# ---------------------------------------------------------------------------

def test_post_non_numeric_content_length_returns_400() -> None:
    h, responses = _handler()
    h.headers = _Headers("abc")
    h.rfile = _ScriptedReader(b"")
    h._handle_api_post("/api/anything")
    assert responses == [({"error": "invalid content-length"}, 400)]


def test_post_negative_content_length_returns_400() -> None:
    h, responses = _handler()
    h.headers = _Headers("-1")
    h.rfile = _ScriptedReader(b"")
    h._handle_api_post("/api/anything")
    assert responses == [({"error": "invalid content-length"}, 400)]


def test_post_oversize_content_length_returns_413_without_reading_body() -> None:
    h, responses = _handler()
    h.headers = _Headers(str(MAX_JSON_BODY_BYTES + 1))
    # A reader that fails if the handler ever drains the body: the cap must
    # reject BEFORE the read so a huge Content-Length cannot exhaust memory.
    class _ExplodingReader:
        def read(self, n: int) -> bytes:
            raise AssertionError("413 path must not read the request body")
    h.rfile = _ExplodingReader()  # type: ignore[assignment]
    h._handle_api_post("/api/anything")
    assert responses == [({"error": "request too large"}, 413)]


def test_post_invalid_json_returns_400() -> None:
    h, responses = _handler()
    body = b"not-json{"
    h.headers = _Headers(str(len(body)))
    h.rfile = _ScriptedReader(body)
    h._handle_api_post("/api/anything")
    assert responses == [({"error": "invalid json"}, 400)]


def test_post_non_object_json_returns_400() -> None:
    h, responses = _handler()
    body = b"[1, 2, 3]"
    h.headers = _Headers(str(len(body)))
    h.rfile = _ScriptedReader(body)
    h._handle_api_post("/api/anything")
    assert responses == [({"error": "json payload must be an object"}, 400)]


def test_post_unknown_api_route_returns_404() -> None:
    h, responses = _handler()
    body = b'{"x": 1}'
    h.headers = _Headers(str(len(body)))
    h.rfile = _ScriptedReader(body)
    h._handle_api_post("/api/does-not-exist")
    assert responses == [({"error": "not found"}, 404)]


# ---------------------------------------------------------------------------
# _api_firmware_upload_raw — Content-Length guard / cap (raw binary endpoint)
# ---------------------------------------------------------------------------

def test_firmware_upload_non_numeric_content_length_returns_400() -> None:
    h, responses = _handler(flash=object(), mavlink=object())
    h.headers = _Headers("abc")
    h.rfile = _ScriptedReader(b"")
    h._api_firmware_upload_raw()
    assert responses == [({"ok": False, "error": "invalid content-length"}, 400)]


def test_firmware_upload_negative_content_length_returns_400() -> None:
    h, responses = _handler(flash=object(), mavlink=object())
    h.headers = _Headers("-5")
    h.rfile = _ScriptedReader(b"")
    h._api_firmware_upload_raw()
    assert responses == [({"ok": False, "error": "invalid content-length"}, 400)]


def test_firmware_upload_oversize_content_length_returns_413_without_reading() -> None:
    h, responses = _handler(flash=object(), mavlink=object())
    h.headers = _Headers(str(MAX_FIRMWARE_BODY_BYTES + 1))
    class _ExplodingReader:
        def read(self, n: int) -> bytes:
            raise AssertionError("firmware 413 path must not read the body")
    h.rfile = _ExplodingReader()  # type: ignore[assignment]
    h._api_firmware_upload_raw()
    assert responses == [({"ok": False, "error": "request too large"}, 413)]


def test_firmware_upload_no_service_returns_503_before_draining_body() -> None:
    h, responses = _handler(flash=None, mavlink=None)
    h.headers = _Headers("16")
    class _ExplodingReader:
        def read(self, n: int) -> bytes:
            raise AssertionError("503 path must not drain the body when no service")
    h.rfile = _ExplodingReader()  # type: ignore[assignment]
    h._api_firmware_upload_raw()
    assert responses == [({"ok": False, "error": "flash service unavailable"}, 503)]


# ---------------------------------------------------------------------------
# POST /api/mavlink/connect — connection-string validation
# ---------------------------------------------------------------------------

def test_mavlink_connect_non_string_connection_returns_400() -> None:
    h, responses = _handler(mavlink=_FakeMavlink())
    h._api_mavlink_connect({"connection": 12345})
    assert responses == [({"ok": False, "error": "connection must be a non-empty string"}, 400)]


def test_mavlink_connect_empty_connection_returns_400() -> None:
    h, responses = _handler(mavlink=_FakeMavlink())
    h._api_mavlink_connect({"connection": ""})
    assert responses == [({"ok": False, "error": "connection must be a non-empty string"}, 400)]


def test_mavlink_connect_no_bridge_returns_503() -> None:
    h, responses = _handler(mavlink=None)
    h._api_mavlink_connect({"connection": "udp:127.0.0.1:14540"})
    assert responses == [({"ok": False, "error": "mavlink not ready"}, 503)]


def test_mavlink_connect_bad_prefix_returns_400_not_500() -> None:
    h, responses = _handler(mavlink=_FakeMavlink())
    # A garbage prefix must surface as 400 (set_connection ValueError), not a
    # 500 / false-200, and must not reach mavlink_connection (which would hang
    # or reconnect-storm on a bad spec).
    h._api_mavlink_connect({"connection": "ftp://nowhere"})
    assert len(responses) == 1
    payload, status = responses[0]
    assert status == 400
    assert payload["ok"] is False
    assert "must start with" in payload["error"]


def test_mavlink_connect_valid_prefix_returns_200() -> None:
    h, responses = _handler(mavlink=_FakeMavlink())
    h._api_mavlink_connect({"connection": "udp:127.0.0.1:14550"})
    assert responses == [({"ok": True, "connection": "udp:127.0.0.1:14550"}, 200)]


# ---------------------------------------------------------------------------
# POST /api/ssh/send — data-string validation
# ---------------------------------------------------------------------------

def test_ssh_send_non_string_data_returns_400() -> None:
    h, responses = _handler(ssh=_FakeSsh())
    h._api_ssh_send({"name": "device", "data": 123})
    assert responses == [({"ok": False, "error": "data must be a string"}, 400)]


def test_ssh_send_list_data_returns_400() -> None:
    h, responses = _handler(ssh=_FakeSsh())
    h._api_ssh_send({"name": "device", "data": ["ls"]})
    assert responses == [({"ok": False, "error": "data must be a string"}, 400)]


def test_ssh_send_valid_string_data_passes_to_bridge() -> None:
    ssh = _FakeSsh()
    h, responses = _handler(ssh=ssh)
    h._api_ssh_send({"name": "device", "data": "ls -la\n"})
    assert responses == [({"ok": True}, 200)]
    assert ssh.send_calls == [("device", "ls -la\n")]


def test_ssh_send_no_session_returns_400() -> None:
    h, responses = _handler(ssh=_FakeSsh())
    h._api_ssh_send({"data": "ls"})
    assert responses == [({"error": "no session"}, 400)]
