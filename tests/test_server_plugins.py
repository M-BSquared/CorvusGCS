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
    (tmp_path / "user" / "demo" / "config.json").write_text(
        json.dumps({"directory": "/srv"}), encoding="utf-8")
    handler, responses = _handler(tmp_path)
    handler._api_plugins()
    body, _ = responses[0]
    assert body["settings"] == {"demo": {"directory": "/srv"}}


def test_plugins_list_reads_a_bundled_plugins_config_from_the_user_folder(tmp_path):
    # The bundled folder is read-only inside an artifact, so a shipped plugin's
    # config lives in a folder of the same name under the user root.
    _make_plugin(tmp_path / "bundled", "schwalby")
    (tmp_path / "user" / "schwalby").mkdir(parents=True)
    (tmp_path / "user" / "schwalby" / "config.json").write_text(
        json.dumps({"buttons": [{"id": "b1"}]}), encoding="utf-8")
    handler, responses = _handler(tmp_path)
    handler._api_plugins()
    body, _ = responses[0]
    assert [p["id"] for p in body["plugins"]] == ["schwalby"]
    assert body["plugins"][0]["source"] == "bundled"
    assert body["settings"] == {"schwalby": {"buttons": [{"id": "b1"}]}}


def test_plugins_list_falls_back_to_unmigrated_settings(tmp_path):
    handler, responses = _handler(tmp_path)
    handler.config.plugins = {"old": {"a": 1}, "demo": {"stale": True}}
    (tmp_path / "user" / "demo").mkdir(parents=True)
    (tmp_path / "user" / "demo" / "config.json").write_text('{"fresh": true}', encoding="utf-8")
    handler._api_plugins()
    # The file wins over what is left in the main config.
    assert responses[0][0]["settings"] == {"old": {"a": 1}, "demo": {"fresh": True}}


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


def _plugin_file(tmp_path, plugin_id: str) -> dict:
    return json.loads((tmp_path / "user" / plugin_id / "config.json").read_text(encoding="utf-8"))


def test_settings_are_scoped_per_plugin(tmp_path):
    handler, _ = _handler(tmp_path)
    handler._api_plugins_settings({"id": "a", "settings": {"x": 1}})
    handler._api_plugins_settings({"id": "b", "settings": {"y": 2}})
    assert _plugin_file(tmp_path, "a") == {"x": 1}
    assert _plugin_file(tmp_path, "b") == {"y": 2}


def test_settings_are_kept_apart_from_the_main_config(tmp_path):
    handler, _ = _handler(tmp_path)
    handler._api_plugins_settings({"id": "demo", "settings": {"directory": "/srv"}})
    assert _plugin_file(tmp_path, "demo") == {"directory": "/srv"}
    assert handler.config.plugins is None
    main = tmp_path / "config.json"
    assert not main.exists() or "plugins" not in json.loads(main.read_text(encoding="utf-8"))


def test_a_copied_config_file_deploys_the_same_settings(tmp_path):
    source, _ = _handler(tmp_path / "a")
    source._api_plugins_settings({"id": "schwalby", "settings": {"buttons": [{"id": "b1"}]}})
    target_dir = tmp_path / "b" / "user" / "schwalby"
    target_dir.mkdir(parents=True)
    (target_dir / "config.json").write_bytes(
        (tmp_path / "a" / "user" / "schwalby" / "config.json").read_bytes())
    target, responses = _handler(tmp_path / "b")
    target._api_plugins()
    assert responses[0][0]["settings"] == {"schwalby": {"buttons": [{"id": "b1"}]}}


def test_saving_moves_an_unmigrated_plugin_out_of_the_main_config(tmp_path):
    handler, responses = _handler(tmp_path)
    handler.config.plugins = {"demo": {"directory": "/srv"}, "other": {"z": 1}}
    handler._api_plugins_settings({"id": "demo", "settings": {"command": "./run"}})
    # The merge starts from the legacy object, not from nothing.
    assert responses[-1][0]["settings"] == {"directory": "/srv", "command": "./run"}
    assert _plugin_file(tmp_path, "demo") == {"directory": "/srv", "command": "./run"}
    assert handler.config.plugins == {"other": {"z": 1}}


def test_settings_rejects_an_id_that_could_leave_the_folder(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_plugins_settings({"id": "../escape", "settings": {"x": 1}})
    assert responses[0][1] == 400
    assert not (tmp_path / "escape").exists()


def test_settings_replace_drops_a_key_a_merge_cannot(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_plugins_settings({"id": "demo", "settings": {"old": 1, "keep": 2}})
    handler._api_plugins_settings({"id": "demo", "settings": {"keep": 3}, "replace": True})
    # A merge can only ever add; replace is how a plugin retires a key it no
    # longer writes.
    assert responses[-1][0]["settings"] == {"keep": 3}
    assert _plugin_file(tmp_path, "demo") == {"keep": 3}


def test_settings_replace_leaves_other_plugins_alone(tmp_path):
    handler, _ = _handler(tmp_path)
    handler._api_plugins_settings({"id": "a", "settings": {"x": 1}})
    handler._api_plugins_settings({"id": "b", "settings": {"y": 2}, "replace": True})
    assert _plugin_file(tmp_path, "a") == {"x": 1}
    assert _plugin_file(tmp_path, "b") == {"y": 2}


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

    def __init__(self, ok: bool = True, reason: str = "") -> None:
        self.calls: list[tuple] = []
        self.ok = ok
        self.reason = reason

    def connect(self, name, host, port, username, password, key_path):
        self.calls.append((name, host, port, username, password, key_path))
        return self.ok

    def connect_error(self, name):
        return self.reason


def test_a_failed_connect_says_why(tmp_path):
    """The launcher's warning shows `error`. Without it every failure, a host
    that is off or a wrong password alike, read as the same generic line."""
    spy = _ConnectSpy(ok=False, reason="No answer from 10.0.0.7:2222 within 8 s.")
    handler, responses = _with_saved_connection(tmp_path, ssh=spy)
    handler._api_ssh_connect({"name": "ssh-launcher/b1", "from": "companion"})
    body, status = responses[0]
    assert status == 200
    assert body["ok"] is False and body["connected"] is False
    assert body["error"] == "No answer from 10.0.0.7:2222 within 8 s."


def test_a_failed_connect_without_a_reason_still_says_something(tmp_path):
    spy = _ConnectSpy(ok=False)
    handler, responses = _with_saved_connection(tmp_path, ssh=spy)
    handler._api_ssh_connect({"name": "ssh-launcher/b1", "from": "companion"})
    body, _ = responses[0]
    assert body["error"] == "Connection failed."


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


def test_a_registry_that_calls_javascript_plain_text_changes_nothing(tmp_path, monkeypatch):
    """Windows: mimetypes reads the registry, where an editor can have set .js
    to text/plain. With nosniff, Chromium would then run no script at all."""
    import mimetypes
    monkeypatch.setattr(mimetypes, "guess_type", lambda *_a, **_k: ("text/plain", None))
    _make_plugin(tmp_path / "bundled", "demo")
    handler, _ = _handler(tmp_path)
    raw = _RawCapture()
    raw.attach(handler)
    handler._api_plugin_asset("demo", "demo.js")
    assert raw.status == 200
    assert raw.headers["Content-Type"] == "text/javascript"


@pytest.mark.parametrize("name,expected", [
    ("app.js", "text/javascript"), ("m.mjs", "text/javascript"), ("main.css", "text/css"),
    ("index.html", "text/html"), ("f.woff2", "font/woff2"), ("x.svg", "image/svg+xml"),
    ("x.map", "application/json"), ("README.md", "text/markdown"), ("blob", "application/octet-stream"),
    ("UPPER.JS", "text/javascript"),
])
def test_content_types_are_the_same_on_every_host(name, expected):
    from corvus.server import content_type
    assert content_type(name) == expected


# ---------------------------------------------------------------------------
# `needs`: a failure more typing can fix names the connection to set up
# ---------------------------------------------------------------------------

class _AuthSpy(_ConnectSpy):
    """A bridge whose connect was refused at the login."""

    def __init__(self, refused: bool = True) -> None:
        super().__init__(ok=False, reason="Authentication failed.")
        self.refused = refused

    def connect_auth_failed(self, name):
        return self.refused


def test_connect_to_a_missing_connection_says_to_set_it_up(tmp_path):
    handler, responses = _handler(tmp_path, ssh=_ConnectSpy())
    handler._api_ssh_connect({"name": "schwalby/s", "from": "companion"})
    body, status = responses[0]
    assert status == 400
    assert body["needs"] == "connection"
    assert body["connection"] == "companion"


def test_a_refused_login_says_to_ask_for_it(tmp_path):
    handler, responses = _with_saved_connection(tmp_path, ssh=_AuthSpy(), password="")
    handler._api_ssh_connect({"name": "schwalby/s", "from": "companion"})
    body, _ = responses[0]
    assert body["ok"] is False
    assert body["needs"] == "credentials"
    assert body["connection"] == "companion"


def test_an_unreachable_host_asks_for_nothing(tmp_path):
    handler, responses = _with_saved_connection(tmp_path, ssh=_AuthSpy(refused=False))
    handler._api_ssh_connect({"name": "companion"})
    assert "needs" not in responses[0][0]


def test_an_ad_hoc_connect_names_no_connection(tmp_path):
    handler, responses = _handler(tmp_path, ssh=_AuthSpy())
    handler._api_ssh_connect({"name": "x", "host": "10.0.0.9", "username": "pi"})
    assert "needs" not in responses[0][0]


def test_run_on_a_missing_connection_says_to_set_it_up(tmp_path, monkeypatch):
    monkeypatch.setattr("corvus.server.ssh_run_command", _RunSpy())
    handler, responses = _handler(tmp_path)
    handler._api_ssh_run({"name": "ground", "command": "./record.sh"})
    body, status = responses[0]
    assert status == 400
    assert (body["needs"], body["connection"]) == ("connection", "ground")


def test_run_with_a_refused_login_says_to_ask_for_it(tmp_path, monkeypatch):
    spy = _RunSpy({"ok": False, "exit_status": None, "stdout": "", "stderr": "",
                   "error": "Authentication failed.", "auth_failed": True})
    monkeypatch.setattr("corvus.server.ssh_run_command", spy)
    handler, responses = _with_saved_connection(tmp_path)
    handler._api_ssh_run({"name": "companion", "command": "./start.sh"})
    body, status = responses[0]
    assert status == 200
    assert (body["needs"], body["connection"]) == ("credentials", "companion")
    assert "auth_failed" not in body


def test_run_that_failed_otherwise_asks_for_nothing(tmp_path, monkeypatch):
    spy = _RunSpy({"ok": False, "exit_status": 127, "stdout": "", "stderr": "nope",
                   "error": "command exited with status 127", "auth_failed": False})
    monkeypatch.setattr("corvus.server.ssh_run_command", spy)
    handler, responses = _with_saved_connection(tmp_path)
    handler._api_ssh_run({"name": "companion", "command": "./start.sh"})
    assert "needs" not in responses[0][0]
    assert "auth_failed" not in responses[0][0]


def test_the_connection_list_says_whether_a_password_is_stored(tmp_path):
    handler, responses = _with_saved_connection(tmp_path)
    handler.config.ssh_connections.append(
        {"name": "copied", "host": "10.0.0.8", "port": 22, "username": "pi",
         "key_path": "", "password": ""})
    handler._api_ssh_connections()
    listed = {c["name"]: c for c in responses[0][0]["connections"]}
    assert listed["companion"]["has_password"] is True
    assert listed["copied"]["has_password"] is False
    assert "hunter2" not in json.dumps(responses[0][0])
