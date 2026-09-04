"""Tests for the config + SSH-connection endpoints in corvus/server.py.

Covers the read/write config API and the persisted SSH-connection list:

- ``GET /api/config`` redacts every SSH ``password``.
- ``POST /api/config`` persists a partial update (write to a tmp config path),
  drops unknown keys, and rejects bad input with 400.
- ``GET /api/ssh/connections`` returns the redacted list with live
  ``connected`` status merged from the SshBridge sessions.
- ``POST /api/ssh/connections`` upserts by name (replace-or-append), persists
  atomically, and returns the redacted list.
- ``POST /api/ssh/connections/remove`` disconnects a live session FIRST
  (stops the subprocess) then removes the entry; idempotent for an unknown
  name.
- ``POST /api/ssh/connect`` connect-by-name extension: a payload with only a
  ``name`` (no host) loads creds from the saved connection and connects.

Hermetic: every test uses a tmp config path and a fake SshBridge; no real
SSH connections are opened, no file under the user's home is touched.
"""
from __future__ import annotations

import http.client
import json
import os
import stat
import threading
from typing import Any

import pytest

from corvus.config import CorvusConfig, load_config, to_public_dict
from corvus.server import CorvusHandler, CorvusServer


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------

class FakeSshBridge:
    """Stand-in for ``SshBridge`` exposing the connect/disconnect/list API."""

    def __init__(self) -> None:
        self._sessions: dict[str, dict[str, Any]] = {}
        self.connect_calls: list[dict[str, Any]] = []
        self.disconnect_calls: list[str] = []

    def connect(
        self,
        name: str,
        host: str,
        port: int = 22,
        username: str = "corvus",
        password: str | None = None,
        key_path: str | None = None,
    ) -> bool:
        self.connect_calls.append({
            "name": name, "host": host, "port": port,
            "username": username, "password": password, "key_path": key_path,
        })
        self._sessions[name] = {
            "name": name, "host": host, "port": port,
            "username": username, "connected": True,
        }
        return True

    def disconnect(self, name: str) -> bool:
        self.disconnect_calls.append(name)
        return self._sessions.pop(name, None) is not None

    def send(self, name: str, data: str) -> bool:
        return name in self._sessions

    def get_session(self, name: str) -> Any:
        return None

    def list_sessions(self) -> list[dict[str, Any]]:
        return [dict(s) for s in self._sessions.values()]

    def shutdown(self) -> None:
        self._sessions.clear()


# ---------------------------------------------------------------------------
# Handler-level helpers (fast, no socket): construct a CorvusHandler via
# object.__new__ and stub _send_json. Mirrors tests/test_server_serial_ports.py
# and tests/test_server_params.py.
# ---------------------------------------------------------------------------

def _handler(
    *,
    config: CorvusConfig | None = None,
    config_path: str | None = None,
    ssh: Any = None,
) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.config = config
    handler.config_path = config_path
    handler.ssh = ssh
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


# ---------------------------------------------------------------------------
# GET /api/config
# ---------------------------------------------------------------------------

def test_get_config_returns_redacted_config() -> None:
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "/k", "password": "hunter2"},
        ],
        theme={"accent": "#3DA876"},
        map={"base_layer": "satellite"},
    )
    handler, responses = _handler(config=cfg)
    handler._api_config()
    assert len(responses) == 1
    payload, status = responses[0]
    assert status == 200
    pub = payload["config"]
    assert "password" not in pub["ssh_connections"][0]
    assert pub["ssh_connections"][0]["key_path"] == "/k"
    assert pub["theme"] == {"accent": "#3DA876"}
    assert pub["map"] == {"base_layer": "satellite"}


def test_get_config_never_echoes_password_in_response() -> None:
    """Acceptance check 4: NO password string in the GET /api/config response."""
    secret = "super-secret-pw-12345"
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "", "password": secret},
        ],
    )
    handler, responses = _handler(config=cfg)
    handler._api_config()
    payload, status = responses[0]
    assert status == 200
    # The literal secret string must not appear anywhere in the JSON response.
    assert secret not in json.dumps(payload)


def test_get_config_falls_back_to_defaults_when_no_config(tmp_path) -> None:
    """With no config attribute and no file, GET /api/config returns defaults."""
    handler, responses = _handler(config=None, config_path=str(tmp_path / "none.json"))
    handler._api_config()
    payload, status = responses[0]
    assert status == 200
    # Defaults: empty ssh_connections, default mavlink_connection present.
    assert payload["config"]["ssh_connections"] == []
    assert "mavlink_connection" in payload["config"]


# ---------------------------------------------------------------------------
# POST /api/config
# ---------------------------------------------------------------------------

def test_post_config_persists_partial_update(tmp_path) -> None:
    cfg_path = tmp_path / "config.json"
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(cfg_path),
    )
    handler._api_config_update({
        "theme": {"accent": "#3DA876"},
        "map": {"base_layer": "topo"},
    })
    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    assert payload["config"]["theme"] == {"accent": "#3DA876"}
    assert payload["config"]["map"] == {"base_layer": "topo"}
    # Persisted to disk.
    assert cfg_path.is_file()
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert on_disk["theme"] == {"accent": "#3DA876"}
    assert on_disk["map"] == {"base_layer": "topo"}


def test_post_config_unknown_key_dropped_with_warning(tmp_path, caplog) -> None:
    cfg_path = tmp_path / "config.json"
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(cfg_path),
    )
    with caplog.at_level("WARNING", logger="corvus.server"):
        handler._api_config_update({"bogus_key": 1, "theme": {"accent": "#abc"}})
    payload, status = responses[0]
    assert status == 200
    assert payload["config"]["theme"] == {"accent": "#abc"}
    # The unknown key is not persisted.
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert "bogus_key" not in on_disk
    # A warning was logged for the dropped key.
    assert any("bogus_key" in r.getMessage() for r in caplog.records)


def test_post_config_bad_http_port_returns_400(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_config_update({"http_port": 70000})
    payload, status = responses[0]
    assert status == 400
    assert "http_port" in payload["error"]


def test_post_config_non_dict_ssh_connections_returns_400(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_config_update({"ssh_connections": {"name": "x"}})
    payload, status = responses[0]
    assert status == 400
    assert "ssh_connections" in payload["error"]


def test_post_config_non_dict_theme_returns_400(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_config_update({"theme": "dark"})
    payload, status = responses[0]
    assert status == 400
    assert "theme" in payload["error"]


def test_post_config_merges_with_existing_keeps_other_fields(tmp_path) -> None:
    """A partial update does not wipe fields the caller did not send."""
    existing = CorvusConfig(
        mavlink_connection="tcp:1.2.3.4:5760",
        ssh_connections=[{"name": "A", "host": "h", "port": 22,
                          "username": "u", "key_path": "", "password": "p"}],
    )
    cfg_path = tmp_path / "config.json"
    handler, responses = _handler(config=existing, config_path=str(cfg_path))
    handler._api_config_update({"theme": {"accent": "#fff"}})
    payload, status = responses[0]
    assert status == 200
    assert payload["config"]["mavlink_connection"] == "tcp:1.2.3.4:5760"
    # ssh_connections preserved (and redacted).
    assert len(payload["config"]["ssh_connections"]) == 1
    assert "password" not in payload["config"]["ssh_connections"][0]


def test_post_config_rejects_empty_mavlink_connection(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_config_update({"mavlink_connection": ""})
    payload, status = responses[0]
    assert status == 400


# ---------------------------------------------------------------------------
# GET /api/ssh/connections
# ---------------------------------------------------------------------------

def test_get_ssh_connections_returns_redacted_with_connected_status() -> None:
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "CORVUS-01", "host": "192.168.2.10", "port": 22,
             "username": "corvus", "key_path": "", "password": "secret"},
            {"name": "PI", "host": "10.0.0.5", "port": 22,
             "username": "pi", "key_path": "/k", "password": ""},
        ],
    )
    ssh = FakeSshBridge()
    ssh.connect("CORVUS-01", "192.168.2.10")  # mark CORVUS-01 as live+connected
    handler, responses = _handler(config=cfg, ssh=ssh)
    handler._api_ssh_connections()
    payload, status = responses[0]
    assert status == 200
    conns = payload["connections"]
    assert len(conns) == 2
    by_name = {c["name"]: c for c in conns}
    assert by_name["CORVUS-01"]["connected"] is True
    assert by_name["PI"]["connected"] is False
    for c in conns:
        assert "password" not in c
    # key_path is a path, not a secret — kept.
    assert by_name["PI"]["key_path"] == "/k"


def test_get_ssh_connections_no_ssh_bridge_marks_all_disconnected() -> None:
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "", "password": "p"},
        ],
    )
    handler, responses = _handler(config=cfg, ssh=None)
    handler._api_ssh_connections()
    payload, status = responses[0]
    assert status == 200
    assert payload["connections"][0]["connected"] is False
    assert "password" not in payload["connections"][0]


def test_get_ssh_connections_never_echoes_password() -> None:
    secret = "hunter2-pw"
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "", "password": secret},
        ],
    )
    handler, responses = _handler(config=cfg, ssh=None)
    handler._api_ssh_connections()
    payload, status = responses[0]
    assert status == 200
    assert secret not in json.dumps(payload)


# ---------------------------------------------------------------------------
# POST /api/ssh/connections (upsert)
# ---------------------------------------------------------------------------

def test_post_ssh_connections_upsert_appends_new(tmp_path) -> None:
    cfg = CorvusConfig()
    cfg_path = tmp_path / "config.json"
    handler, responses = _handler(config=cfg, config_path=str(cfg_path))
    handler._api_ssh_connections_upsert({
        "name": "CORVUS-01", "host": "192.168.2.10", "port": 22,
        "username": "corvus", "password": "secret",
    })
    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    assert len(payload["connections"]) == 1
    assert payload["connections"][0]["name"] == "CORVUS-01"
    # Redacted in response.
    assert "password" not in payload["connections"][0]
    # Persisted to disk WITH the password (the file is the secret store).
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert on_disk["ssh_connections"][0]["password"] == "secret"


def test_post_ssh_connections_upsert_replaces_existing_by_name(tmp_path) -> None:
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "CORVUS-01", "host": "old-host", "port": 22,
             "username": "corvus", "key_path": "", "password": "old"},
        ],
    )
    cfg_path = tmp_path / "config.json"
    handler, responses = _handler(config=cfg, config_path=str(cfg_path))
    handler._api_ssh_connections_upsert({
        "name": "CORVUS-01", "host": "new-host", "port": 2222,
        "username": "corvus", "password": "new",
    })
    payload, status = responses[0]
    assert status == 200
    conns = payload["connections"]
    assert len(conns) == 1  # replaced, not appended
    assert conns[0]["host"] == "new-host"
    assert conns[0]["port"] == 2222


def test_post_ssh_connections_coerces_port_string_to_int(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_upsert({
        "name": "X", "host": "h", "port": "2222", "username": "u",
    })
    payload, status = responses[0]
    assert status == 200
    assert payload["connections"][0]["port"] == 2222


def test_post_ssh_connections_defaults_port_to_22(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_upsert({
        "name": "X", "host": "h", "username": "u",
    })
    payload, status = responses[0]
    assert status == 200
    assert payload["connections"][0]["port"] == 22


def test_post_ssh_connections_rejects_empty_name(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_upsert({"name": "", "host": "h"})
    payload, status = responses[0]
    assert status == 400
    assert "name" in payload["error"]


def test_post_ssh_connections_rejects_empty_host(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_upsert({"name": "X", "host": ""})
    payload, status = responses[0]
    assert status == 400
    assert "host" in payload["error"]


def test_post_ssh_connections_rejects_bad_port(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_upsert({
        "name": "X", "host": "h", "port": "not-a-port",
    })
    payload, status = responses[0]
    assert status == 400
    assert "port" in payload["error"]


def test_post_ssh_connections_rejects_out_of_range_port(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_upsert({
        "name": "X", "host": "h", "port": 70000,
    })
    payload, status = responses[0]
    assert status == 400


def test_post_ssh_connections_rejects_bool_port(tmp_path) -> None:
    """bool is a subclass of int — reject True so it never becomes 1."""
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_upsert({
        "name": "X", "host": "h", "port": True,
    })
    payload, status = responses[0]
    assert status == 400


# ---------------------------------------------------------------------------
# POST /api/ssh/connections/remove
# ---------------------------------------------------------------------------

def test_post_ssh_connections_remove_disconnects_live_then_removes(tmp_path) -> None:
    """Order matters: ssh.disconnect(name) is called BEFORE the entry is dropped."""
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "CORVUS-01", "host": "h", "port": 22,
             "username": "u", "key_path": "", "password": "p"},
        ],
    )
    cfg_path = tmp_path / "config.json"
    ssh = FakeSshBridge()
    ssh.connect("CORVUS-01", "h")  # mark live
    handler, responses = _handler(config=cfg, config_path=str(cfg_path), ssh=ssh)

    handler._api_ssh_connections_remove({"name": "CORVUS-01"})

    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    assert payload["connections"] == []
    # The live session was disconnected (subprocess stopped) first.
    assert ssh.disconnect_calls == ["CORVUS-01"]
    # The entry is gone from the persisted file.
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert on_disk["ssh_connections"] == []


def test_post_ssh_connections_remove_idempotent_for_unknown_name(tmp_path) -> None:
    """Removing a name that is neither live nor saved still returns ok."""
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "", "password": "p"},
        ],
    )
    cfg_path = tmp_path / "config.json"
    ssh = FakeSshBridge()
    handler, responses = _handler(config=cfg, config_path=str(cfg_path), ssh=ssh)

    handler._api_ssh_connections_remove({"name": "DOES-NOT-EXIST"})

    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    # The other entry is untouched.
    assert len(payload["connections"]) == 1
    assert payload["connections"][0]["name"] == "A"
    # No live disconnect was attempted (none was live, but the call is safe).
    assert ssh.disconnect_calls == ["DOES-NOT-EXIST"]


def test_post_ssh_connections_remove_missing_name_returns_400(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
    )
    handler._api_ssh_connections_remove({})
    payload, status = responses[0]
    assert status == 400
    assert "name" in payload["error"]


def test_post_ssh_connections_remove_with_no_live_ssh_still_removes_entry(tmp_path) -> None:
    """No SshBridge at all (ssh=None) — removal still drops the saved entry."""
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "", "password": "p"},
        ],
    )
    cfg_path = tmp_path / "config.json"
    handler, responses = _handler(config=cfg, config_path=str(cfg_path), ssh=None)

    handler._api_ssh_connections_remove({"name": "A"})

    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    assert payload["connections"] == []


# ---------------------------------------------------------------------------
# POST /api/ssh/connect — connect-by-name extension
# ---------------------------------------------------------------------------

def test_post_ssh_connect_by_name_loads_saved_creds(tmp_path) -> None:
    """No host in payload + name matches saved connection -> connect by name."""
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "CORVUS-01", "host": "192.168.2.10", "port": 2222,
             "username": "corvus", "key_path": "/k", "password": "secret"},
        ],
    )
    ssh = FakeSshBridge()
    handler, responses = _handler(config=cfg, config_path=str(tmp_path / "c.json"), ssh=ssh)

    handler._api_ssh_connect({"name": "CORVUS-01"})  # no host!

    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    # The bridge was called with the SAVED creds (including the password the
    # UI never held).
    assert len(ssh.connect_calls) == 1
    call = ssh.connect_calls[0]
    assert call["name"] == "CORVUS-01"
    assert call["host"] == "192.168.2.10"
    assert call["port"] == 2222
    assert call["username"] == "corvus"
    assert call["password"] == "secret"
    assert call["key_path"] == "/k"


def test_post_ssh_connect_by_name_unknown_name_returns_no_host(tmp_path) -> None:
    """No host + name not saved -> the existing 'no host' 400 path."""
    cfg = CorvusConfig()  # no saved connections
    ssh = FakeSshBridge()
    handler, responses = _handler(config=cfg, config_path=str(tmp_path / "c.json"), ssh=ssh)

    handler._api_ssh_connect({"name": "UNKNOWN"})
    payload, status = responses[0]
    assert status == 400
    assert payload["error"] == "no host"
    assert ssh.connect_calls == []


def test_post_ssh_connect_with_explicit_host_still_uses_provided_values(tmp_path) -> None:
    """Backward compat: when host IS provided, the saved entry is NOT used."""
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "CORVUS-01", "host": "saved-host", "port": 22,
             "username": "saved", "key_path": "", "password": "saved-pw"},
        ],
    )
    ssh = FakeSshBridge()
    handler, responses = _handler(config=cfg, config_path=str(tmp_path / "c.json"), ssh=ssh)

    handler._api_ssh_connect({
        "name": "CORVUS-01", "host": "explicit-host", "port": 2222,
        "username": "explicit", "password": "explicit-pw",
    })

    payload, status = responses[0]
    assert status == 200
    call = ssh.connect_calls[0]
    assert call["host"] == "explicit-host"
    assert call["username"] == "explicit"
    assert call["password"] == "explicit-pw"


def test_post_ssh_connect_no_ssh_bridge_returns_500(tmp_path) -> None:
    handler, responses = _handler(
        config=CorvusConfig(),
        config_path=str(tmp_path / "c.json"),
        ssh=None,
    )
    handler._api_ssh_connect({"name": "X", "host": "h"})
    payload, status = responses[0]
    assert status == 500
    assert payload["error"] == "ssh not ready"


# ---------------------------------------------------------------------------
# Live-HTTP end-to-end: real CorvusServer on an ephemeral port with tmp config
# ---------------------------------------------------------------------------

@pytest.fixture
def http_server(tmp_path):
    """A live CorvusServer wired with a tmp config path + fake ssh bridge."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")

    cfg_path = tmp_path / "config.json"
    ssh = FakeSshBridge()
    # Start from a clean on-disk config so reads hit the same path writes do.
    from corvus.config import save_config
    save_config(CorvusConfig(), str(cfg_path))

    saved = (
        CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
        CorvusHandler.config, CorvusHandler.config_path,
    )
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = ssh
    CorvusHandler.config = load_config(str(cfg_path))
    CorvusHandler.config_path = str(cfg_path)

    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    server.ssh = ssh
    server.config = CorvusHandler.config
    server.config_path = str(cfg_path)
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-test-config", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, ssh, cfg_path
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        (
            CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
            CorvusHandler.config, CorvusHandler.config_path,
        ) = saved


def _get(server: CorvusServer, path: str) -> tuple[int, bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, body


def _post(server: CorvusServer, path: str, payload: dict) -> tuple[int, bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("POST", path, json.dumps(payload), {"Content-Type": "application/json"})
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, body


def test_http_get_config_redacts_passwords(http_server) -> None:
    server, ssh, cfg_path = http_server
    # Seed the file with a connection carrying a real password.
    from corvus.config import save_config
    save_config(CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "", "password": "live-secret-pw"},
        ],
    ), str(cfg_path))
    # The server holds the old in-memory config; force a reload by clearing
    # the cached attribute so _live_config re-reads the file.
    CorvusHandler.config = None

    status, body = _get(server, "/api/config")
    assert status == 200
    data = json.loads(body)
    assert "live-secret-pw" not in body.decode("utf-8")
    assert "password" not in data["config"]["ssh_connections"][0]


def test_http_post_then_get_config_round_trip(http_server) -> None:
    server, ssh, cfg_path = http_server
    status, body = _post(server, "/api/config", {"theme": {"accent": "#abc"}})
    assert status == 200
    assert json.loads(body)["config"]["theme"] == {"accent": "#abc"}
    # A fresh GET reflects the persisted value.
    status, body = _get(server, "/api/config")
    assert status == 200
    assert json.loads(body)["config"]["theme"] == {"accent": "#abc"}
    # And the file on disk matches.
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert on_disk["theme"] == {"accent": "#abc"}


def test_http_post_ssh_connections_upsert_then_get(http_server) -> None:
    server, ssh, cfg_path = http_server
    status, body = _post(server, "/api/ssh/connections", {
        "name": "CORVUS-01", "host": "192.168.2.10", "port": 22,
        "username": "corvus", "password": "secret",
    })
    assert status == 200
    data = json.loads(body)
    assert data["ok"] is True
    assert len(data["connections"]) == 1
    assert "password" not in data["connections"][0]

    # GET reflects the new entry, redacted, with connected=False (not live).
    status, body = _get(server, "/api/ssh/connections")
    assert status == 200
    conns = json.loads(body)["connections"]
    assert len(conns) == 1
    assert conns[0]["name"] == "CORVUS-01"
    assert conns[0]["connected"] is False
    assert "password" not in conns[0]
    # The file on disk kept the password (it is the secret store).
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert on_disk["ssh_connections"][0]["password"] == "secret"


def test_http_post_ssh_connect_by_name(http_server) -> None:
    """End-to-end: save a connection, then connect by name only."""
    server, ssh, cfg_path = http_server
    _post(server, "/api/ssh/connections", {
        "name": "CORVUS-01", "host": "192.168.2.10", "port": 2222,
        "username": "corvus", "password": "secret",
    })
    # Connect by name only — no host/port/password in the payload.
    status, body = _post(server, "/api/ssh/connect", {"name": "CORVUS-01"})
    assert status == 200
    data = json.loads(body)
    assert data["ok"] is True
    # The fake bridge received the SAVED creds.
    assert len(ssh.connect_calls) == 1
    call = ssh.connect_calls[0]
    assert call["host"] == "192.168.2.10"
    assert call["port"] == 2222
    assert call["password"] == "secret"


def test_http_remove_disconnects_live_session(http_server) -> None:
    server, ssh, cfg_path = http_server
    # Save + connect a session so it is live.
    _post(server, "/api/ssh/connections", {
        "name": "CORVUS-01", "host": "192.168.2.10", "username": "corvus",
        "password": "p",
    })
    _post(server, "/api/ssh/connect", {"name": "CORVUS-01"})
    assert ssh.list_sessions() != []

    # Remove -> disconnect FIRST, then drop the entry.
    status, body = _post(server, "/api/ssh/connections/remove", {"name": "CORVUS-01"})
    assert status == 200
    assert json.loads(body)["ok"] is True
    # The live session is gone.
    assert ssh.list_sessions() == []
    # And the entry is gone from the persisted file.
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert on_disk["ssh_connections"] == []


def test_http_saved_config_file_is_chmod_600(http_server) -> None:
    """Acceptance check 5: the persisted config file is owner-read/write only."""
    server, ssh, cfg_path = http_server
    _post(server, "/api/ssh/connections", {
        "name": "X", "host": "h", "username": "u", "password": "p",
    })
    mode = stat.S_IMODE(cfg_path.stat().st_mode)
    assert mode == 0o600, f"expected 0o600, got {oct(mode)}"


# ---------------------------------------------------------------------------
# Company logo (GET/POST /api/branding/logo, POST /api/branding/logo/remove)
#
# The logo is optional operator branding: nothing ships with the app, the
# bytes live beside the config file (never inside it), and the config only
# records the display filename.
# ---------------------------------------------------------------------------

# Smallest valid PNG (1x1, transparent) — enough for the magic-byte gate.
_PNG_1PX = bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010806000000"
    "1f15c4890000000a49444154789c6360000002000100ffff0300000600"
    "05574bd0f70000000049454e44ae426082"
)


class _Capture:
    """Collects the raw (non-JSON) response an image handler writes."""

    def __init__(self) -> None:
        self.status: int | None = None
        self.headers: dict[str, str] = {}
        self.body = b""


def _logo_handler(tmp_path, body: bytes = b"", name: str = "") -> tuple[Any, list, _Capture]:
    """Handler wired for the branding routes, with a raw-response capture."""
    import io

    handler, responses = _handler(
        config=CorvusConfig(), config_path=str(tmp_path / "config.json"),
    )
    handler.path = "/api/branding/logo" + (f"?name={name}" if name else "")
    handler.headers = {"Content-Length": str(len(body))}
    handler.rfile = io.BytesIO(body)
    cap = _Capture()
    handler.send_response = lambda status: setattr(cap, "status", status)
    handler.send_header = lambda k, v: cap.headers.__setitem__(k, v)
    handler.end_headers = lambda: None
    handler.wfile = io.BytesIO()
    return handler, responses, cap


def test_no_company_logo_by_default(tmp_path) -> None:
    """Nothing is configured out of the box: GET 404s and the config is bare."""
    handler, responses, _cap = _logo_handler(tmp_path)
    handler._api_branding_logo()
    payload, status = responses[0]
    assert status == 404
    assert "branding" not in handler._public_config()


def test_post_company_logo_stores_file_and_records_name(tmp_path) -> None:
    cfg_path = tmp_path / "config.json"
    handler, responses, _cap = _logo_handler(tmp_path, _PNG_1PX, "unibw.png")
    handler._api_branding_logo_upload_raw()
    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    assert payload["logo"] == "unibw.png"
    # Bytes on disk beside the config, name in the config — not the other way.
    stored = tmp_path / "branding" / "logo.png"
    assert stored.read_bytes() == _PNG_1PX
    on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert on_disk["branding"] == {"logo": "unibw.png"}
    assert "89504e47" not in cfg_path.read_text(encoding="utf-8")


def test_get_company_logo_serves_the_stored_png(tmp_path) -> None:
    handler, _responses, _cap = _logo_handler(tmp_path, _PNG_1PX, "unibw.png")
    handler._api_branding_logo_upload_raw()

    handler2, _responses2, cap = _logo_handler(tmp_path)
    handler2._api_branding_logo()
    assert cap.status == 200
    assert cap.headers["Content-Type"] == "image/png"
    assert handler2.wfile.getvalue() == _PNG_1PX


def test_post_company_logo_rejects_non_png(tmp_path) -> None:
    """A renamed JPEG is refused here, not rendered as a broken top-bar image."""
    jpeg = b"\xff\xd8\xff\xe0" + b"0" * 64
    handler, responses, _cap = _logo_handler(tmp_path, jpeg, "logo.png")
    handler._api_branding_logo_upload_raw()
    payload, status = responses[0]
    assert status == 400
    assert payload["ok"] is False
    assert not (tmp_path / "branding" / "logo.png").exists()


def test_post_company_logo_rejects_empty_and_oversize(tmp_path) -> None:
    handler, responses, _cap = _logo_handler(tmp_path, b"", "logo.png")
    handler._api_branding_logo_upload_raw()
    assert responses[0][1] == 400

    from corvus.server import MAX_LOGO_BODY_BYTES
    handler2, responses2, _cap2 = _logo_handler(tmp_path, b"", "logo.png")
    handler2.headers = {"Content-Length": str(MAX_LOGO_BODY_BYTES + 1)}
    handler2._api_branding_logo_upload_raw()
    assert responses2[0][1] == 413
    assert not (tmp_path / "branding" / "logo.png").exists()


def test_post_company_logo_name_is_basename_only(tmp_path) -> None:
    """The stored name is display metadata; a path in it never escapes."""
    handler, responses, _cap = _logo_handler(
        tmp_path, _PNG_1PX, "..%2F..%2Fevil.png",
    )
    handler._api_branding_logo_upload_raw()
    payload, status = responses[0]
    assert status == 200
    assert payload["logo"] == "evil.png"
    assert (tmp_path / "branding" / "logo.png").is_file()
    assert not (tmp_path.parent / "evil.png").exists()


def test_remove_company_logo_clears_file_and_config(tmp_path) -> None:
    handler, _responses, _cap = _logo_handler(tmp_path, _PNG_1PX, "unibw.png")
    handler._api_branding_logo_upload_raw()

    handler2, responses2, _cap2 = _logo_handler(tmp_path)
    handler2._api_branding_logo_remove({})
    payload, status = responses2[0]
    assert status == 200
    assert payload["ok"] is True
    assert not (tmp_path / "branding" / "logo.png").exists()
    assert "branding" not in payload["config"]


def test_remove_company_logo_is_idempotent(tmp_path) -> None:
    """Removing when none is set succeeds — a stale UI never sees an error."""
    handler, responses, _cap = _logo_handler(tmp_path)
    handler._api_branding_logo_remove({})
    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
