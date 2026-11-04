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
    _host_header_allowed,
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


# ---------------------------------------------------------------------------
# Host header: the half of DNS rebinding the CSRF guard cannot see
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("host", [
    "localhost", "localhost:8000",
    "127.0.0.1", "127.0.0.1:8000",
    "[::1]", "[::1]:8000",
])
def test_a_loopback_host_is_answered(host: str) -> None:
    assert _host_header_allowed(host) is True


@pytest.mark.parametrize("host", [
    "evil.example", "evil.example:8000",
    "127.0.0.1.evil.example", "localhost.evil.example:8000",
    "10.0.0.5:8000",
])
def test_a_host_this_server_is_not_is_refused(host: str) -> None:
    """The name in the URL bar is the one thing the rebinder cannot forge."""
    assert _host_header_allowed(host) is False


def test_a_missing_host_is_not_a_rebinding_attack() -> None:
    """HTTP/1.0 clients omit it; a browser never does, and rebinding is a browser."""
    assert _host_header_allowed("") is True
    assert _host_header_allowed(None) is True
    assert _host_header_allowed("   ") is True


def test_a_malformed_host_is_refused() -> None:
    assert _host_header_allowed("localhost:not-a-port") is False
    assert _host_header_allowed(":::::") is False
    assert _host_header_allowed("a" * 300) is False


def test_the_configured_remote_origin_is_also_a_host_this_server_answers_to(monkeypatch) -> None:
    monkeypatch.setenv(REMOTE_ORIGIN_ENV, "https://control.example:8443")
    assert _host_header_allowed("control.example:8443") is True
    assert _host_header_allowed("control.example") is True
    assert _host_header_allowed("other.example") is False


def test_a_wider_bind_accepts_any_host(monkeypatch) -> None:
    """``CORVUS_BIND`` is the operator opting into a LAN name we cannot enumerate.

    That mode already warns it has no authentication; refusing the LAN name
    would break a supported setup to harden a case already accepted.
    """
    monkeypatch.setenv(BIND_HOST_ENV, "0.0.0.0")
    assert _host_header_allowed("192.168.1.50:8000") is True
    assert _host_header_allowed("gcs.lan") is True


def _request_with_host(
    server: Any, method: str, path: str, host_header: str, body: bytes | None = None,
) -> tuple[int, bytes]:
    """One request whose Host says something other than what we connected to."""
    host, port = server.server_address[0], server.server_address[1]
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        headers = {"Host": host_header}
        if body is not None:
            headers["Content-Type"] = "text/plain"
            headers["Content-Length"] = str(len(body))
        conn.request(method, path, body=body, headers=headers)
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


def test_a_rebound_page_cannot_arm_the_vehicle(
    server_with_store: Any, fake_bridge: MagicMock,
) -> None:
    """The attack the CSRF guard passes: no Origin, same-origin per the browser.

    Every check in ``_mutating_request_allowed`` is satisfied — this is the
    request that used to reach ``/api/mavlink/arm``.
    """
    status, raw = _request_with_host(
        server_with_store, "POST", "/api/mavlink/arm", "evil.example:8000", b'{"arm":true}',
    )
    assert status == 403
    assert json.loads(raw).get("ok") is not True
    fake_bridge.arm.assert_not_called()


def test_a_rebound_page_cannot_read_the_ssh_credentials(server_with_store: Any) -> None:
    """The check covers reads too: /api/config lists hosts, users and key paths."""
    status, _raw = _request_with_host(
        server_with_store, "GET", "/api/config", "evil.example:8000",
    )
    assert status == 403


def test_the_ui_itself_is_unaffected(server_with_store: Any, fake_bridge: MagicMock) -> None:
    """The window is opened at loopback, so it keeps working."""
    fake_bridge.arm.return_value = True
    status, raw = _request_with_host(
        server_with_store, "POST", "/api/mavlink/arm", "127.0.0.1:8000", b'{"arm":true}',
    )
    assert status == 200
    assert json.loads(raw) == {"ok": True}


# ---------------------------------------------------------------------------
# Static assets: percent-decoding, and the containment that makes it safe
# ---------------------------------------------------------------------------

def _get_raw(server: Any, path: str) -> tuple[int, bytes]:
    """A GET with the path sent exactly as written, encoding and all."""
    host, port = server.server_address[0], server.server_address[1]
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        conn.request("GET", path)
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


def test_a_percent_encoded_asset_name_is_decoded(server_with_store: Any, tmp_path, monkeypatch) -> None:
    """A URL path is an encoded form; "%20" in it means a space in the name."""
    from corvus import server as server_module

    asset = tmp_path / "my file.js"
    asset.write_bytes(b"// hello\n")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)

    status, body = _get_raw(server_with_store, "/my%20file.js")
    assert status == 200
    assert body == b"// hello\n"


@pytest.mark.parametrize("path", [
    "/../../../etc/passwd",
    "/%2e%2e%2f%2e%2e%2f%2e%2e%2fetc/passwd",
    "/..%2f..%2f..%2fetc/passwd",
    "/%2e%2e/%2e%2e/etc/passwd",
])
def test_decoding_does_not_open_a_traversal(server_with_store: Any, tmp_path, monkeypatch, path: str) -> None:
    """Decoding is what makes "%2e%2e%2f" mean "../" — containment is what stops it.

    The check is resolve() + relative_to(WEB_DIR), which collapses the
    traversal before deciding, so an encoded climb is refused exactly like a
    plain one.
    """
    from corvus import server as server_module

    (tmp_path / "index.html").write_bytes(b"<!doctype html>")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)

    status, body = _get_raw(server_with_store, path)
    assert status in (403, 404), body
    assert b"root:" not in body


def test_a_decoded_nul_byte_is_refused_not_a_traceback(server_with_store: Any, tmp_path, monkeypatch) -> None:
    """pathlib refuses an embedded NUL; that must be a 403, not a 500."""
    from corvus import server as server_module

    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)
    status, _body = _get_raw(server_with_store, "/index%00.html")
    assert status in (403, 404)


# ---------------------------------------------------------------------------
# Static assets: conditional requests
# ---------------------------------------------------------------------------

def _conditional_get(
    server: Any, path: str, headers: dict[str, str],
) -> tuple[int, bytes, dict[str, str]]:
    host, port = server.server_address[0], server.server_address[1]
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        conn.request("GET", path, headers=headers)
        response = conn.getresponse()
        body = response.read()
        return response.status, body, {k.lower(): v for k, v in response.getheaders()}
    finally:
        conn.close()


def test_an_asset_carries_validators(server_with_store: Any, tmp_path, monkeypatch) -> None:
    """Without these, "no-cache" can only ever mean a full re-send."""
    from corvus import server as server_module

    (tmp_path / "app.js").write_bytes(b"console.log(1);\n")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)

    status, body, headers = _conditional_get(server_with_store, "/app.js", {})
    assert status == 200
    assert body == b"console.log(1);\n"
    assert headers.get("etag")
    assert headers.get("last-modified")
    # Still revalidated every time: a plugin asset edited on disk has to show
    # up on the next reload, which is what this header is here for.
    assert headers.get("cache-control") == "no-cache"


def test_an_unchanged_asset_comes_back_304_with_no_body(
    server_with_store: Any, tmp_path, monkeypatch,
) -> None:
    from corvus import server as server_module

    (tmp_path / "big.js").write_bytes(b"x" * 100_000)
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)

    status, body, headers = _conditional_get(server_with_store, "/big.js", {})
    assert status == 200 and len(body) == 100_000

    status, body, _ = _conditional_get(
        server_with_store, "/big.js", {"If-None-Match": headers["etag"]})
    assert status == 304
    assert body == b""


def test_an_edited_asset_is_sent_again(server_with_store: Any, tmp_path, monkeypatch) -> None:
    """The point of keeping "no-cache": an edit on disk must reach the browser."""
    from corvus import server as server_module

    asset = tmp_path / "plugin.js"
    asset.write_bytes(b"old")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)
    _status, _body, headers = _conditional_get(server_with_store, "/plugin.js", {})
    stale_etag = headers["etag"]

    asset.write_bytes(b"new content, same second")
    status, body, fresh = _conditional_get(
        server_with_store, "/plugin.js", {"If-None-Match": stale_etag})
    assert status == 200
    assert body == b"new content, same second"
    assert fresh["etag"] != stale_etag, (
        "two writes in the same second produced the same tag; the size and "
        "nanosecond mtime in the ETag are what stop that"
    )


def test_if_modified_since_is_honoured_when_there_is_no_etag(
    server_with_store: Any, tmp_path, monkeypatch,
) -> None:
    from corvus import server as server_module

    (tmp_path / "a.css").write_bytes(b"body{}")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)
    _status, _body, headers = _conditional_get(server_with_store, "/a.css", {})

    status, _body, _ = _conditional_get(
        server_with_store, "/a.css", {"If-Modified-Since": headers["last-modified"]})
    assert status == 304


def test_a_malformed_conditional_header_just_sends_the_file(
    server_with_store: Any, tmp_path, monkeypatch,
) -> None:
    """A header off the wire is never trusted to parse."""
    from corvus import server as server_module

    (tmp_path / "a.css").write_bytes(b"body{}")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)

    for bad in ("not-a-date", "", "Tue, 99 Zzz 9999 99:99:99 GMT"):
        status, body, _ = _conditional_get(
            server_with_store, "/a.css", {"If-Modified-Since": bad})
        assert status == 200, bad
        assert body == b"body{}"


# ---------------------------------------------------------------------------
# Response hardening headers
#
# The fourth open door: the UI is a local page that can arm an aircraft, and
# the browser was told nothing about what that page is allowed to do. One
# injected script could read GET /api/config and POST /api/mavlink/arm, and —
# the part a CSP actually stops — send what it found to a host of its choosing.
# ---------------------------------------------------------------------------

def test_every_response_carries_the_hardening_headers(server_with_store: Any) -> None:
    """Not just the HTML: a JSON body and a static asset alike."""
    from corvus.server import CONTENT_SECURITY_POLICY

    for path in ("/api/version", "/api/state"):
        response = _get(server_with_store, path, None)
        response.read()
        assert response.getheader("Content-Security-Policy") == CONTENT_SECURITY_POLICY, path
        assert response.getheader("X-Content-Type-Options") == "nosniff", path
        assert response.getheader("X-Frame-Options") == "DENY", path
        assert response.getheader("Referrer-Policy") == "no-referrer", path


def test_the_policy_pins_the_page_to_this_server(server_with_store: Any) -> None:
    """``connect-src 'self'`` is what keeps an injection from phoning home.

    Each of these is load-bearing rather than boilerplate: without
    ``connect-src`` a script in the page can POST the telemetry, the SSH
    credentials and the map service keys anywhere; without ``frame-ancestors``
    another page can embed the UI and clickjack ARM; ``object-src`` and
    ``base-uri`` close the two classic ways a script gets re-introduced.
    """
    from corvus.server import CONTENT_SECURITY_POLICY

    directives = {
        part.strip().split(" ")[0]: part.strip()
        for part in CONTENT_SECURITY_POLICY.split(";")
    }
    assert directives["default-src"] == "default-src 'self'"
    assert directives["connect-src"] == "connect-src 'self'"
    assert directives["frame-ancestors"] == "frame-ancestors 'none'"
    assert directives["object-src"] == "object-src 'none'"
    assert directives["base-uri"] == "base-uri 'none'"
    assert directives["form-action"] == "form-action 'none'"
    # MapLibre builds its tile worker from a blob URL; without this there is
    # no map at all, which is why the exception is written down rather than
    # discovered by an operator in the field.
    assert "blob:" in directives["worker-src"]
    # 'unsafe-eval' is NOT here, and stays out: nothing the app ships needs it
    # (checked against MapLibre, Plotly and xterm), and it is the single
    # directive that would give a string-to-code path back.
    assert "'unsafe-eval'" not in CONTENT_SECURITY_POLICY


def test_a_route_that_sets_its_own_header_is_not_overridden(
    server_with_store: Any, tmp_path, monkeypatch,
) -> None:
    """The headers are defaults from one choke point, not a second opinion."""
    from corvus import server as server_module

    (tmp_path / "a.css").write_bytes(b"body{}")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)
    response = _get(server_with_store, "/a.css", None)
    response.read()
    # The route's own Content-Type survived alongside the added headers, and
    # nothing was emitted twice.
    assert response.getheader("Content-Type") == "text/css"
    assert len(response.headers.get_all("Content-Security-Policy") or []) == 1
    assert len(response.headers.get_all("X-Content-Type-Options") or []) == 1


def test_a_304_is_hardened_too(server_with_store: Any, tmp_path, monkeypatch) -> None:
    """The revalidation path writes its own header block and must not skip them."""
    from corvus import server as server_module

    (tmp_path / "a.css").write_bytes(b"body{}")
    monkeypatch.setattr(server_module, "WEB_DIR", tmp_path)
    _status, _body, headers = _conditional_get(server_with_store, "/a.css", {})
    status, _body, fresh = _conditional_get(
        server_with_store, "/a.css", {"If-None-Match": headers["etag"]})
    assert status == 304
    assert fresh["x-content-type-options"] == "nosniff"


def test_the_app_loads_nothing_from_off_this_machine() -> None:
    """The policy is only honest while the page stays self-contained.

    An offline field station has no business fetching a CDN script or a remote
    font, and the moment one is added ``default-src 'self'`` breaks the app
    rather than the app quietly breaking the promise. This pins the contract
    so the failure is caught here instead of on a laptop with no internet.
    """
    import pathlib
    import re

    root = pathlib.Path(__file__).resolve().parent.parent / "src"
    pattern = re.compile(r"""(?:src|href)\s*=\s*["']https?://""", re.I)
    offenders = [
        str(path.relative_to(root))
        for path in list(root.glob("*.html")) + list(root.glob("css/*.css"))
        if pattern.search(path.read_text(encoding="utf-8", errors="replace"))
    ]
    assert offenders == [], f"remote asset references: {offenders}"


def test_a_second_request_on_one_connection_is_hardened_too(server_with_store: Any) -> None:
    """A keep-alive connection serves many responses through one handler.

    The per-response bookkeeping is reset in ``send_response`` rather than
    after the write for exactly this: state left behind by the first response
    would make the second skip its headers, and nothing about the page would
    look different.
    """
    from corvus.server import CONTENT_SECURITY_POLICY

    host, port = server_with_store.server_address[0], server_with_store.server_address[1]
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        for attempt in range(3):
            conn.request("GET", "/api/version")
            response = conn.getresponse()
            response.read()
            assert response.getheader("Content-Security-Policy") == \
                CONTENT_SECURITY_POLICY, f"request {attempt}"
            assert response.getheader("X-Content-Type-Options") == "nosniff", attempt
    finally:
        conn.close()
