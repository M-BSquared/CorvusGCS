"""Security hardening: who may reach the API, and who may read its answers.

Three separate doors, pinned here because each of them was open:

- **The bind.** ``bind_server`` defaulted to ``""`` — every interface. Nothing
  behind it authenticates anything, so arm, takeoff, a parameter write and a
  firmware upload were one unauthenticated POST from any machine on the
  flight-line network.
- **CORS.** Every JSON body and every SSE stream carried
  ``Access-Control-Allow-Origin: *``, so any website the operator visited could
  read live telemetry and pull the SSH host/user/key path out of
  ``GET /api/config``.
- **SSH host keys.** Both connect paths loaded no host keys at all, which makes
  ``AutoAddPolicy`` permanent: a companion computer whose key changed — the one
  signal that says somebody else is answering — looked like a first connect.
"""
from __future__ import annotations

import http.client
import json
import logging
import os
from typing import Any
from unittest.mock import MagicMock

import pytest

from corvus.server import (
    BIND_HOST_ENV,
    DEFAULT_BIND_HOST,
    REMOTE_ORIGIN_ENV,
    _cors_allowed_origin,
    bind_host,
)


# ---------------------------------------------------------------------------
# Which interface the HTTP server listens on
# ---------------------------------------------------------------------------

def test_the_http_server_listens_on_loopback_by_default(monkeypatch) -> None:
    """The API has no authentication, so the default must not be the network."""
    monkeypatch.delenv(BIND_HOST_ENV, raising=False)
    assert bind_host() == DEFAULT_BIND_HOST == "127.0.0.1"


def test_a_bound_server_is_actually_on_loopback(monkeypatch) -> None:
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler, bind_server

    monkeypatch.delenv(BIND_HOST_ENV, raising=False)
    server = bind_server(0, CorvusHandler)
    try:
        assert server.server_address[0] == "127.0.0.1"
    finally:
        server.server_close()


def test_corvus_bind_opts_into_the_network_and_says_so(monkeypatch, caplog) -> None:
    """The escape hatch stays, but an exposed ground station is never silent."""
    monkeypatch.setenv(BIND_HOST_ENV, "0.0.0.0")
    with caplog.at_level(logging.WARNING, logger="corvus.server"):
        host = bind_host()
    assert host == "0.0.0.0"
    assert any("no authentication" in r.getMessage() for r in caplog.records)


def test_an_empty_corvus_bind_is_not_an_opt_in(monkeypatch) -> None:
    monkeypatch.setenv(BIND_HOST_ENV, "   ")
    assert bind_host() == DEFAULT_BIND_HOST


def test_an_explicit_host_beats_the_environment(monkeypatch) -> None:
    """The tests bind where they say they bind, whatever the shell was set to."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler, bind_server

    monkeypatch.setenv(BIND_HOST_ENV, "0.0.0.0")
    server = bind_server(0, CorvusHandler, host="127.0.0.1")
    try:
        assert server.server_address[0] == "127.0.0.1"
    finally:
        server.server_close()


# ---------------------------------------------------------------------------
# CORS
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("origin", [
    "https://evil.example",
    "http://evil.example:8000",
    "http://127.0.0.1.evil.example",       # loopback as a prefix of a real host
    "http://localhost.evil.example",
    "file://",
    "null",
    "",
    "*",
])
def test_a_foreign_origin_is_never_allowed(origin: str) -> None:
    assert _cors_allowed_origin(origin) == ""


@pytest.mark.parametrize("origin", [
    "http://localhost:8000",
    "http://127.0.0.1:8000",
    "http://[::1]:8000",
    "https://localhost:8443",
])
def test_a_loopback_origin_is_echoed_by_name(origin: str) -> None:
    assert _cors_allowed_origin(origin) == origin


def test_an_exact_configured_remote_origin_is_allowed(monkeypatch) -> None:
    monkeypatch.setenv(REMOTE_ORIGIN_ENV, "https://control.example:8443")

    assert _cors_allowed_origin("https://control.example:8443") == (
        "https://control.example:8443"
    )
    assert _cors_allowed_origin("https://control.example") == ""
    assert _cors_allowed_origin("https://control.example:8443.evil.test") == ""


def test_a_non_string_origin_is_not_an_origin() -> None:
    """The header comes off the wire; it is never assumed to be a str."""
    assert _cors_allowed_origin(None) == ""
    assert _cors_allowed_origin(b"http://localhost") == ""
    assert _cors_allowed_origin("http://localhost:8000" + "a" * 300) == ""


def _get(server: Any, path: str, origin: str | None) -> http.client.HTTPResponse:
    host, port = server.server_address[0], server.server_address[1]
    conn = http.client.HTTPConnection(host, port, timeout=5)
    headers = {"Origin": origin} if origin is not None else {}
    conn.request("GET", path, headers=headers)
    return conn.getresponse()


def test_a_json_response_carries_no_wildcard_for_a_website(server_with_store) -> None:
    """The header that let any page the operator visited read the telemetry."""
    response = _get(server_with_store, "/api/state", "https://evil.example")
    response.read()
    assert response.getheader("Access-Control-Allow-Origin") is None
    # Vary rides along regardless, so no cache hands one origin's answer to
    # another.
    assert response.getheader("Vary") == "Origin"


def test_a_same_origin_request_needs_no_header_at_all(server_with_store) -> None:
    """The UI is served by this same server; its own fetches send no Origin."""
    response = _get(server_with_store, "/api/state", None)
    body = response.read()
    assert response.status == 200
    assert body  # the request still works
    assert response.getheader("Access-Control-Allow-Origin") is None


def test_another_tool_on_this_machine_is_still_allowed(server_with_store) -> None:
    response = _get(server_with_store, "/api/state", "http://localhost:5173")
    response.read()
    assert response.getheader("Access-Control-Allow-Origin") == "http://localhost:5173"


def test_the_telemetry_stream_is_not_subscribable_by_a_website(server_with_store) -> None:
    """``new EventSource`` worked cross-origin under the wildcard. It no longer does.

    Only the response headers are read — an SSE body never ends — and the
    connection is dropped straight after, which the handler treats as a client
    that went away.
    """
    host, port = server_with_store.server_address[0], server_with_store.server_address[1]
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        conn.request("GET", "/api/telemetry", headers={"Origin": "https://evil.example"})
        response = conn.getresponse()
        assert response.status == 200
        assert response.getheader("Content-Type") == "text/event-stream"
        assert response.getheader("Access-Control-Allow-Origin") is None
    finally:
        conn.close()


def test_the_wildcard_is_gone_from_the_source() -> None:
    """Every send site goes through _send_cors; none may reintroduce ``*``."""
    import inspect

    from corvus import server as server_module

    source = inspect.getsource(server_module)
    assert '"Access-Control-Allow-Origin", "*"' not in source


# ---------------------------------------------------------------------------
# Cross-site writes / DNS rebinding
# ---------------------------------------------------------------------------

def _post(
    server: Any,
    path: str,
    body: bytes,
    *,
    origin: str | None = None,
    fetch_site: str | None = None,
    content_type: str = "text/plain",
) -> tuple[int, dict[str, Any]]:
    """Issue a browser-shaped POST and return its status + JSON response."""
    host, port = server.server_address[0], server.server_address[1]
    headers = {
        "Content-Type": content_type,
        "Content-Length": str(len(body)),
    }
    if origin is not None:
        headers["Origin"] = origin
    if fetch_site is not None:
        headers["Sec-Fetch-Site"] = fetch_site
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        conn.request("POST", path, body=body, headers=headers)
        response = conn.getresponse()
        raw = response.read()
        return response.status, json.loads(raw)
    finally:
        conn.close()


@pytest.mark.parametrize(
    ("origin", "fetch_site"),
    [
        ("https://evil.example", "cross-site"),
        ("https://evil.example", None),
        (None, "cross-site"),
    ],
)
def test_a_cross_site_post_cannot_arm_the_vehicle(
    server_with_store: Any,
    fake_bridge: MagicMock,
    origin: str | None,
    fetch_site: str | None,
) -> None:
    """CORS protects reads, not writes; a simple text/plain POST needs no preflight."""
    status, payload = _post(
        server_with_store,
        "/api/mavlink/arm",
        b'{"arm":true}',
        origin=origin,
        fetch_site=fetch_site,
    )

    assert status == 403
    assert payload.get("ok") is not True
    fake_bridge.arm.assert_not_called()


def test_a_loopback_origin_can_still_post(
    server_with_store: Any,
    fake_bridge: MagicMock,
) -> None:
    """Local development tools remain usable when they identify a loopback origin."""
    fake_bridge.arm.return_value = True

    status, payload = _post(
        server_with_store,
        "/api/mavlink/arm",
        b'{"arm":true}',
        origin="http://localhost:5173",
        fetch_site="same-site",
    )

    assert status == 200
    assert payload == {"ok": True}
    fake_bridge.arm.assert_called_once_with(True)


def test_an_exact_configured_remote_origin_can_post(
    server_with_store: Any,
    fake_bridge: MagicMock,
    monkeypatch,
) -> None:
    monkeypatch.setenv(REMOTE_ORIGIN_ENV, "https://control.example:8443")
    fake_bridge.arm.return_value = True

    status, payload = _post(
        server_with_store,
        "/api/mavlink/arm",
        b'{"arm":true}',
        origin="https://control.example:8443",
        fetch_site="cross-site",
    )

    assert status == 200
    assert payload == {"ok": True}
    fake_bridge.arm.assert_called_once_with(True)


def test_a_mismatched_remote_origin_cannot_post(
    server_with_store: Any,
    fake_bridge: MagicMock,
    monkeypatch,
) -> None:
    monkeypatch.setenv(REMOTE_ORIGIN_ENV, "https://control.example:8443")

    status, payload = _post(
        server_with_store,
        "/api/mavlink/arm",
        b'{"arm":true}',
        origin="https://control.example",
        fetch_site="same-site",
    )

    assert status == 403
    assert payload.get("ok") is not True
    fake_bridge.arm.assert_not_called()


def test_a_cross_site_raw_upload_is_rejected_before_flashing(
    server_with_store: Any,
) -> None:
    """Raw POST branches bypass JSON parsing, but never the request-origin gate."""
    from corvus.server import CorvusHandler

    flash = MagicMock()
    flash.start.return_value = True
    previous = CorvusHandler.flash
    CorvusHandler.flash = flash
    try:
        status, payload = _post(
            server_with_store,
            "/api/firmware/upload",
            b"hostile firmware bytes",
            origin="https://evil.example",
            fetch_site="cross-site",
            content_type="application/octet-stream",
        )
    finally:
        CorvusHandler.flash = previous

    assert status == 403
    assert payload.get("ok") is not True
    flash.start.assert_not_called()


# ---------------------------------------------------------------------------
# SSH host keys
# ---------------------------------------------------------------------------

def test_the_default_policy_remembers_and_warns() -> None:
    paramiko = pytest.importorskip("paramiko")
    from corvus import ssh_bridge

    os.environ.pop(ssh_bridge.HOST_KEY_POLICY_ENV, None)
    client = ssh_bridge._new_client()
    try:
        policy = client._policy
        assert isinstance(policy, ssh_bridge._RememberAndWarnPolicy)
        assert isinstance(policy, paramiko.AutoAddPolicy)
        # Loaded, not merely defaulted: without a host-key file an auto-added
        # key is never written down, and every connect stays a first connect.
        assert client._host_keys_filename == ssh_bridge.known_hosts_path()
    finally:
        client.close()


def test_strict_refuses_an_unknown_host(monkeypatch) -> None:
    paramiko = pytest.importorskip("paramiko")
    from corvus import ssh_bridge

    monkeypatch.setenv(ssh_bridge.HOST_KEY_POLICY_ENV, "strict")
    client = ssh_bridge._new_client()
    try:
        assert isinstance(client._policy, paramiko.RejectPolicy)
    finally:
        client.close()


def test_an_auto_accepted_key_is_logged(monkeypatch, caplog, tmp_path) -> None:
    """A decision made on the operator's behalf is recorded where it can be read."""
    pytest.importorskip("paramiko")
    from corvus import ssh_bridge

    added: list[tuple[str, str]] = []

    class FakeKey:
        def get_name(self) -> str:
            return "ssh-ed25519"

        def get_fingerprint(self) -> bytes:
            return b"\xde\xad\xbe\xef"

    class FakeClient:
        _host_keys_filename = None

        class _Keys:
            def add(self, hostname: str, keytype: str, key: Any) -> None:
                added.append((hostname, keytype))

        _host_keys = _Keys()

        def _log(self, *args: Any, **kwargs: Any) -> None:
            pass

    with caplog.at_level(logging.WARNING, logger="corvus.ssh"):
        ssh_bridge._RememberAndWarnPolicy().missing_host_key(
            FakeClient(), "companion.local", FakeKey(),
        )

    assert added == [("companion.local", "ssh-ed25519")]
    message = " ".join(r.getMessage() for r in caplog.records)
    assert "companion.local" in message
    assert "without verification" in message
    assert ssh_bridge.HOST_KEY_POLICY_ENV in message


def test_a_changed_host_key_is_explained_not_just_raised() -> None:
    """paramiko's own message names the mismatch; it does not say what to do."""
    paramiko = pytest.importorskip("paramiko")
    from corvus import ssh_bridge

    class FakeKey:
        def get_base64(self) -> str:
            return "AAAA"

    exc = paramiko.BadHostKeyException("companion.local", FakeKey(), FakeKey())
    hint = ssh_bridge._host_key_hint(exc)
    assert "companion.local" in hint            # paramiko's half
    assert ssh_bridge.known_hosts_path() in hint  # where to go and fix it
    assert "answering on that address" in hint    # and why it might not be a typo


def test_a_failed_interactive_ssh_connect_closes_its_client(monkeypatch) -> None:
    """A refused/unreachable host must not leave Paramiko transports to GC."""
    pytest.importorskip("paramiko")
    from corvus import ssh_bridge

    class FailingClient:
        def __init__(self) -> None:
            self.closed = False

        def connect(self, **kwargs: Any) -> None:
            del kwargs
            raise OSError("host unreachable")

        def close(self) -> None:
            self.closed = True

    client = FailingClient()
    monkeypatch.setattr(ssh_bridge, "_new_client", lambda: client)
    session = ssh_bridge.SshSession("companion", "192.0.2.1")

    assert session.connect() is False
    assert client.closed is True
