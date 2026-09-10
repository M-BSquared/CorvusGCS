"""HTTP surface for the plugin folder and the one-shot SSH launcher.

Covers the four endpoints the plugin system adds — ``GET /api/plugins``,
``GET /api/plugins/asset/<id>/<path>``, ``POST /api/plugins/settings``,
``POST /api/plugins/folder`` — plus ``POST /api/ssh/run`` and the command
composition behind it.
"""
from __future__ import annotations

import json
from typing import Any

import pytest

from corvus.config import CorvusConfig
from corvus.server import CorvusHandler, _compose_remote_command


def _handler(tmp_path, **attrs: Any):
    """A handler with captured responses and plugin roots under tmp_path."""
    handler = object.__new__(CorvusHandler)
    handler.mavlink = None
    handler.store = None
    handler.ssh = None
    handler.config = CorvusConfig()
    handler.config_path = str(tmp_path / "config.json")
    handler.plugin_user_dir = str(tmp_path / "user")
    handler.plugin_bundled_dir = str(tmp_path / "bundled")
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    for key, value in attrs.items():
        setattr(handler, key, value)
    return handler, responses


def _make_plugin(root, plugin_id: str, **manifest: Any) -> None:
    folder = root / plugin_id
    folder.mkdir(parents=True, exist_ok=True)
    (folder / f"{plugin_id}.js").write_text("//", encoding="utf-8")
    body = {"id": plugin_id, "name": plugin_id, "scripts": [f"{plugin_id}.js"]}
    body.update(manifest)
    (folder / "plugin.json").write_text(json.dumps(body), encoding="utf-8")


class _RawCapture:
    """Captures the non-JSON response the asset route writes."""

    def __init__(self) -> None:
        self.status: int | None = None
        self.headers: dict[str, str] = {}
        self.body = b""

    def attach(self, handler: CorvusHandler) -> None:
        handler.send_response = lambda code: setattr(self, "status", code)  # type: ignore[method-assign]
        handler.send_header = lambda k, v: self.headers.__setitem__(k, v)  # type: ignore[method-assign]
        handler.end_headers = lambda: None  # type: ignore[method-assign]
        handler.wfile = self  # type: ignore[assignment]

    def write(self, data: bytes) -> None:
        self.body += data


# ---------------------------------------------------------------------------
# GET /api/plugins
# ---------------------------------------------------------------------------

def test_plugins_list_is_empty_without_any(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_plugins()
    body, status = responses[0]
    assert status == 200
    assert body["plugins"] == []
    assert body["user_dir"] == str(tmp_path / "user")
    assert body["settings"] == {}


def test_plugins_list_reports_installed_plugins(tmp_path):
    _make_plugin(tmp_path / "user", "demo", description="A demo", version="2.0")
    handler, responses = _handler(tmp_path)
    handler._api_plugins()
    body, _ = responses[0]
    assert [p["id"] for p in body["plugins"]] == ["demo"]
    assert body["plugins"][0]["description"] == "A demo"
    assert body["plugins"][0]["version"] == "2.0"
    # The browser has no use for a server-side path.
    assert "dir" not in body["plugins"][0]


def test_plugins_list_carries_saved_settings(tmp_path):
    _make_plugin(tmp_path / "user", "demo")
    handler, responses = _handler(tmp_path)
    handler.config.plugins = {"demo": {"directory": "/srv"}}
    handler._api_plugins()
    body, _ = responses[0]
    assert body["settings"] == {"demo": {"directory": "/srv"}}


# ---------------------------------------------------------------------------
# GET /api/plugins/asset/<id>/<path>
# ---------------------------------------------------------------------------

def test_asset_route_serves_a_plugin_file(tmp_path):
    _make_plugin(tmp_path / "user", "demo")
    (tmp_path / "user" / "demo" / "demo.js").write_text("console.log(1);", encoding="utf-8")
    handler, _ = _handler(tmp_path)
    raw = _RawCapture()
    raw.attach(handler)
    handler._api_plugin_asset("demo", "demo.js")
    assert raw.status == 200
    assert raw.body == b"console.log(1);"
    # mimetypes spells JavaScript differently across Python versions; both are
    # a script to the browser, and neither is application/octet-stream.
    assert "javascript" in raw.headers["Content-Type"]
    assert raw.headers["Cache-Control"] == "no-cache"


def test_asset_route_404s_for_an_unknown_plugin(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_plugin_asset("nope", "nope.js")
    assert responses[0][1] == 404


def test_asset_route_refuses_traversal(tmp_path):
    _make_plugin(tmp_path / "user", "demo")
    (tmp_path / "user" / "secret.js").write_text("secret", encoding="utf-8")
    handler, responses = _handler(tmp_path)
    handler._api_plugin_asset("demo", "../secret.js")
    assert responses[0][1] == 404


def test_asset_route_refuses_a_disallowed_suffix(tmp_path):
    _make_plugin(tmp_path / "user", "demo")
    (tmp_path / "user" / "demo" / "run.sh").write_text("#!/bin/sh", encoding="utf-8")
    handler, responses = _handler(tmp_path)
    handler._api_plugin_asset("demo", "run.sh")
    assert responses[0][1] == 404


def test_asset_route_cannot_reach_a_folder_that_is_not_a_plugin(tmp_path):
    (tmp_path / "user" / "bare").mkdir(parents=True)
    (tmp_path / "user" / "bare" / "x.js").write_text("//", encoding="utf-8")
    handler, responses = _handler(tmp_path)
    handler._api_plugin_asset("bare", "x.js")
    assert responses[0][1] == 404


# ---------------------------------------------------------------------------
# POST /api/plugins/settings
# ---------------------------------------------------------------------------

def test_settings_are_saved_and_merged(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_plugins_settings({"id": "demo", "settings": {"directory": "/srv"}})
    assert responses[-1][0]["settings"] == {"directory": "/srv"}
    handler._api_plugins_settings({"id": "demo", "settings": {"command": "./run"}})
    # Merged per key: writing one field must not clear the other.
    assert responses[-1][0]["settings"] == {"directory": "/srv", "command": "./run"}


def test_settings_are_scoped_per_plugin(tmp_path):
    handler, _ = _handler(tmp_path)
    handler._api_plugins_settings({"id": "a", "settings": {"x": 1}})
    handler._api_plugins_settings({"id": "b", "settings": {"y": 2}})
    assert handler.config.plugins == {"a": {"x": 1}, "b": {"y": 2}}


def test_settings_survive_a_save_and_reload(tmp_path):
    handler, _ = _handler(tmp_path)
    handler._api_plugins_settings({"id": "demo", "settings": {"directory": "/srv"}})
    from corvus.config import load_config
    assert load_config(handler.config_path).plugins == {"demo": {"directory": "/srv"}}


def test_settings_replace_drops_a_key_a_merge_cannot(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_plugins_settings({"id": "demo", "settings": {"old": 1, "keep": 2}})
    handler._api_plugins_settings({"id": "demo", "settings": {"keep": 3}, "replace": True})
    # A merge can only ever add; replace is how a plugin retires a key it no
    # longer writes.
    assert responses[-1][0]["settings"] == {"keep": 3}
    assert handler.config.plugins == {"demo": {"keep": 3}}


def test_settings_replace_leaves_other_plugins_alone(tmp_path):
    handler, _ = _handler(tmp_path)
    handler._api_plugins_settings({"id": "a", "settings": {"x": 1}})
    handler._api_plugins_settings({"id": "b", "settings": {"y": 2}, "replace": True})
    assert handler.config.plugins == {"a": {"x": 1}, "b": {"y": 2}}


@pytest.mark.parametrize("payload", [
    {}, {"id": ""}, {"id": 5, "settings": {}},
    {"id": "demo"}, {"id": "demo", "settings": "nope"}, {"id": "demo", "settings": []},
    {"id": "demo", "settings": {}, "replace": "yes"},
])
def test_settings_rejects_a_bad_payload(tmp_path, payload):
    handler, responses = _handler(tmp_path)
    handler._api_plugins_settings(payload)
    assert responses[0][1] == 400


# ---------------------------------------------------------------------------
# POST /api/plugins/folder
# ---------------------------------------------------------------------------

def test_folder_route_reports_the_path_even_when_it_cannot_open(tmp_path, monkeypatch):
    (tmp_path / "user").mkdir()
    handler, responses = _handler(tmp_path)
    monkeypatch.setattr("corvus.server.open_folder", lambda p: (False, "no desktop session"))
    handler._api_plugins_folder({})
    body, status = responses[0]
    assert status == 500
    assert body["path"] == str(tmp_path / "user")
    assert body["error"] == "no desktop session"


def test_folder_route_reports_success(tmp_path, monkeypatch):
    (tmp_path / "user").mkdir()
    handler, responses = _handler(tmp_path)
    monkeypatch.setattr("corvus.server.open_folder", lambda p: (True, ""))
    handler._api_plugins_folder({})
    body, status = responses[0]
    assert status == 200
    assert body["ok"] is True


# ---------------------------------------------------------------------------
# The command a launcher composes
# ---------------------------------------------------------------------------

def test_compose_quotes_the_directory_but_not_the_command():
    line = _compose_remote_command("/srv/my mission", "./run.sh --fast", False)
    assert line == "cd -- '/srv/my mission' && ./run.sh --fast"


def test_compose_quotes_a_hostile_directory():
    line = _compose_remote_command("/tmp; rm -rf ~", "./run.sh", False)
    # The injection lands inside the quotes, so it is a folder name, not a
    # second command.
    assert line == "cd -- '/tmp; rm -rf ~' && ./run.sh"


def test_compose_without_a_directory_is_just_the_command():
    assert _compose_remote_command("", "./run.sh", False) == "./run.sh"


def test_compose_detaches_with_nohup_and_echoes_the_pid():
    line = _compose_remote_command("/srv", "./run.sh", True)
    assert line == "cd -- /srv && { nohup ./run.sh >/dev/null 2>&1 & }; echo $!"


def test_compose_handles_a_leading_dash_folder():
    assert _compose_remote_command("-rf", "./run.sh", False).startswith("cd -- -rf &&")


# ---------------------------------------------------------------------------
# POST /api/ssh/run
# ---------------------------------------------------------------------------

class _RunSpy:
    """Records the ssh_run_command call and returns a canned result."""

    def __init__(self, result: dict | None = None) -> None:
        self.calls: list[dict] = []
        self.result = result or {
            "ok": True, "exit_status": 0, "stdout": "1234", "stderr": "", "error": "",
        }

    def __call__(self, **kwargs: Any) -> dict:
        self.calls.append(kwargs)
        return dict(self.result)


def _with_saved_connection(tmp_path, ssh: Any = None, **entry: Any):
    """A handler holding one saved SSH connection, and optionally a bridge."""
    handler, responses = _handler(tmp_path)
    if ssh is not None:
        handler.ssh = ssh
    handler.config.ssh_connections = [{
        "name": "companion", "host": "10.0.0.7", "port": 2222,
        "username": "pilot", "key_path": "", "password": "hunter2",
        **entry,
    }]
    return handler, responses


def test_run_uses_the_saved_connection_credentials(tmp_path, monkeypatch):
    spy = _RunSpy()
    monkeypatch.setattr("corvus.server.ssh_run_command", spy)
    handler, responses = _with_saved_connection(tmp_path)
    handler._api_ssh_run({
        "name": "companion", "directory": "/srv/mission",
        "command": "./start.sh", "detach": True,
    })
    call = spy.calls[0]
    assert call["host"] == "10.0.0.7"
    assert call["port"] == 2222
    assert call["username"] == "pilot"
    assert call["password"] == "hunter2"
    assert call["command"] == "cd -- /srv/mission && { nohup ./start.sh >/dev/null 2>&1 & }; echo $!"
    body, status = responses[0]
    assert status == 200
    assert body["ok"] is True
    assert body["stdout"] == "1234"


def test_run_never_echoes_the_password_back(tmp_path, monkeypatch):
    monkeypatch.setattr("corvus.server.ssh_run_command", _RunSpy())
    handler, responses = _with_saved_connection(tmp_path)
    handler._api_ssh_run({"name": "companion", "command": "./start.sh"})
    assert "hunter2" not in json.dumps(responses[0][0])


def test_run_reports_a_failing_command_with_200_and_stderr(tmp_path, monkeypatch):
    spy = _RunSpy({
        "ok": False, "exit_status": 127, "stdout": "",
        "stderr": "sh: ./start.sh: not found", "error": "command exited with status 127",
    })
    monkeypatch.setattr("corvus.server.ssh_run_command", spy)
    handler, responses = _with_saved_connection(tmp_path)
    handler._api_ssh_run({"name": "companion", "command": "./start.sh"})
    body, status = responses[0]
    # 200: the request worked, and the UI needs the stderr in the body to say
    # why the command did not.
    assert status == 200
    assert body["ok"] is False
    assert "not found" in body["stderr"]


def test_run_accepts_an_ad_hoc_host(tmp_path, monkeypatch):
    spy = _RunSpy()
    monkeypatch.setattr("corvus.server.ssh_run_command", spy)
    handler, responses = _handler(tmp_path)
    handler._api_ssh_run({
        "host": "192.168.1.9", "username": "root", "port": 22, "command": "uptime",
    })
    assert spy.calls[0]["host"] == "192.168.1.9"
    assert spy.calls[0]["password"] is None


@pytest.mark.parametrize("payload,expected", [
    ({}, "command"),
    ({"command": "   "}, "command"),
    ({"command": 5}, "command"),
    ({"command": "x", "directory": 5}, "directory"),
    ({"command": "x", "detach": "yes"}, "detach"),
    ({"command": "x", "host": "h", "username": "u", "port": "abc"}, "port"),
    ({"command": "x", "host": "h", "username": "u", "port": 99999}, "port"),
    ({"command": "x"}, "host"),
    ({"command": "x", "host": "h"}, "username"),
    ({"command": "x", "name": "missing"}, "saved connection"),
])
def test_run_rejects_a_bad_payload(tmp_path, monkeypatch, payload, expected):
    spy = _RunSpy()
    monkeypatch.setattr("corvus.server.ssh_run_command", spy)
    handler, responses = _handler(tmp_path)
    handler._api_ssh_run(payload)
    body, status = responses[0]
    assert status == 400
    assert expected in body["error"]
    assert spy.calls == []          # nothing reached the network


# ---------------------------------------------------------------------------
# POST /api/ssh/connect — the session a launcher's terminal button opens
# ---------------------------------------------------------------------------

class _ConnectSpy:
    """Records the SshBridge.connect call and returns a canned result."""

    def __init__(self, ok: bool = True) -> None:
        self.calls: list[tuple] = []
        self.ok = ok

    def connect(self, name, host, port, username, password, key_path):
        self.calls.append((name, host, port, username, password, key_path))
        return self.ok


def test_connect_can_borrow_a_saved_connections_credentials(tmp_path):
    """A plugin naming its own session still authenticates as the saved host.

    This is what lets one companion computer carry several live sessions — a
    launcher with a terminal per button — without a saved entry per button.
    """
    spy = _ConnectSpy()
    handler, responses = _with_saved_connection(tmp_path, ssh=spy)
    handler._api_ssh_connect({"name": "ssh-launcher/b1", "from": "companion"})
    assert spy.calls == [("ssh-launcher/b1", "10.0.0.7", 2222, "pilot", "hunter2", None)]
    body, _ = responses[0]
    assert body["ok"] is True
    assert body["name"] == "ssh-launcher/b1"
    assert "hunter2" not in json.dumps(body)


def test_connect_without_from_still_resolves_by_the_session_name(tmp_path):
    """The SSH tab's one-session-per-saved-host case is untouched."""
    spy = _ConnectSpy()
    handler, _ = _with_saved_connection(tmp_path, ssh=spy)
    handler._api_ssh_connect({"name": "companion"})
    assert spy.calls[0][0] == "companion"
    assert spy.calls[0][1] == "10.0.0.7"


def test_connect_names_the_saved_connection_it_could_not_find(tmp_path):
    spy = _ConnectSpy()
    handler, responses = _with_saved_connection(tmp_path, ssh=spy)
    handler._api_ssh_connect({"name": "ssh-launcher/b1", "from": "missing"})
    body, status = responses[0]
    assert status == 400
    assert "missing" in body["error"]
    assert spy.calls == []


def test_connect_rejects_a_non_string_from(tmp_path):
    spy = _ConnectSpy()
    handler, responses = _with_saved_connection(tmp_path, ssh=spy)
    handler._api_ssh_connect({"name": "s", "from": 5})
    assert responses[0][1] == 400
    assert spy.calls == []


def test_run_is_registered_on_the_post_route_table():
    assert CorvusHandler._POST_ROUTES["/api/ssh/run"] == "_api_ssh_run"
    assert CorvusHandler._POST_ROUTES["/api/plugins/settings"] == "_api_plugins_settings"
    assert CorvusHandler._POST_ROUTES["/api/plugins/folder"] == "_api_plugins_folder"
    assert CorvusHandler._GET_ROUTES["/api/plugins"] == "_api_plugins"
