"""Settings export and import: the bundle format and its two endpoints.

Covers ``corvus/settings_bundle.py`` (sections, redaction, parsing, the merge
that keeps stored secrets) and ``POST /api/settings/export`` /
``POST /api/settings/import`` on a handler with its responses captured.
"""
from __future__ import annotations

import base64
import json
import os
import sys
from typing import Any

import pytest

from corvus import plugin_config, settings_bundle
from corvus.config import _CONFIG_FIELD_ORDER, CorvusConfig, _build_config, _config_to_dict
from corvus.server import CorvusHandler

PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 16


def _secret_config() -> dict[str, Any]:
    """A serialized config holding one of each of the four secrets."""
    return _config_to_dict(_build_config({
        "mavlink_connection": "udpin:0.0.0.0:14550",
        "ssh_connections": [{"name": "companion", "host": "10.0.0.2", "port": 22,
                             "username": "pi", "password": "ssh-secret"}],
        "map_tokens": {"mapbox": "pk.token"},
        "rtk": {"source": "ntrip", "ntrip": {"host": "caster.example", "port": 2101,
                                             "mountpoint": "M1", "username": "u",
                                             "password": "ntrip-secret"}},
        "video": {"streams": [{"id": "cam1", "name": "Nose", "kind": "rtsp",
                               "url": "rtsp://10.0.0.5/live", "username": "admin",
                               "password": "cam-secret"}]},
        "theme": {"name": "dusk"},
    }))


def _secrets_in(text: str) -> list[str]:
    return [s for s in ("ssh-secret", "pk.token", "ntrip-secret", "cam-secret") if s in text]


# ---------------------------------------------------------------------------
# settings_bundle
# ---------------------------------------------------------------------------

def test_every_config_key_belongs_to_exactly_one_section():
    """A new config key without a section would silently never be imported."""
    for key in _CONFIG_FIELD_ORDER:
        owners = [s for s, keys in settings_bundle.SECTIONS.items() if key in keys]
        if key in settings_bundle.EXCLUDED_KEYS:
            assert owners == [], key
        else:
            assert len(owners) == 1, f"{key} is in {owners or 'no section'}"


def test_build_leaves_every_secret_out_by_default():
    bundle = settings_bundle.build(_secret_config())
    assert _secrets_in(json.dumps(bundle)) == []
    assert bundle["secrets"] is False
    assert bundle["kind"] == settings_bundle.KIND
    # The entries themselves travel, only the secrets are gone.
    assert bundle["config"]["ssh_connections"][0]["host"] == "10.0.0.2"
    assert bundle["config"]["video"]["streams"][0]["url"] == "rtsp://10.0.0.5/live"


def test_build_carries_the_secrets_when_asked():
    bundle = settings_bundle.build(_secret_config(), include_secrets=True)
    assert sorted(_secrets_in(json.dumps(bundle))) == sorted(
        ["ssh-secret", "pk.token", "ntrip-secret", "cam-secret"])
    assert bundle["secrets"] is True


def test_build_does_not_mutate_the_config_it_redacts():
    config = _secret_config()
    settings_bundle.build(config)
    assert config["ssh_connections"][0]["password"] == "ssh-secret"
    assert config["map_tokens"] == {"mapbox": "pk.token"}


def test_build_keeps_only_corvus_string_entries_from_the_browser():
    bundle = settings_bundle.build({}, browser={
        "corvus.hud": '{"x":10}', "other.key": "x", "corvus.bad": 3,
    })
    assert bundle["browser"] == {"corvus.hud": '{"x":10}'}


@pytest.mark.parametrize("data", [None, [], {"kind": "other"}, {"kind": "corvus-settings"}])
def test_parse_rejects_what_is_not_a_bundle(data):
    with pytest.raises(ValueError):
        settings_bundle.parse(data)


def test_parse_refuses_a_newer_format():
    bundle = settings_bundle.build({})
    bundle["format"] = settings_bundle.FORMAT + 1
    with pytest.raises(ValueError, match="newer"):
        settings_bundle.parse(bundle)


def test_parse_refuses_a_logo_that_is_not_a_png():
    bundle = settings_bundle.build({}, logo=b"GIF89a....")
    with pytest.raises(ValueError, match="PNG"):
        settings_bundle.parse(bundle)
    bundle["logo"] = "not base64!"
    with pytest.raises(ValueError):
        settings_bundle.parse(bundle)


def test_parse_round_trips_a_built_bundle():
    bundle = settings_bundle.build(_secret_config(), plugins={"demo": {"a": 1}},
                                   logo=PNG, browser={"corvus.hud": "{}"})
    parsed = settings_bundle.parse(json.loads(json.dumps(bundle)))
    assert parsed["logo"] == PNG
    assert parsed["plugins"] == {"demo": {"a": 1}}
    assert parsed["browser"] == {"corvus.hud": "{}"}
    assert parsed["config"]["theme"] == {"name": "dusk"}


def test_merge_leaves_sections_not_chosen_alone():
    current = _secret_config()
    imported = dict(current, mavlink_connection="COM3", theme={"name": "light"})
    out = settings_bundle.merge(current, imported, {"interface"}, secrets=True)
    assert out["theme"] == {"name": "light"}
    assert out["mavlink_connection"] == "udpin:0.0.0.0:14550"


def test_merge_resets_a_key_the_file_leaves_out():
    current = _secret_config()
    imported = {k: v for k, v in current.items() if k != "theme"}
    out = settings_bundle.merge(current, imported, {"interface"}, secrets=True)
    assert "theme" not in out


def test_merge_keeps_stored_secrets_for_unchanged_entries():
    current = _secret_config()
    imported = settings_bundle.redact(current)
    out = settings_bundle.merge(current, imported, set(settings_bundle.SECTIONS), secrets=False)
    assert out["ssh_connections"][0]["password"] == "ssh-secret"
    assert out["map_tokens"] == {"mapbox": "pk.token"}
    assert out["rtk"]["ntrip"]["password"] == "ntrip-secret"
    assert out["video"]["streams"][0]["password"] == "cam-secret"


def test_merge_never_hands_a_stored_secret_to_another_host():
    current = _secret_config()
    imported = settings_bundle.redact(current)
    imported["ssh_connections"][0]["host"] = "10.9.9.9"
    imported["rtk"]["ntrip"]["host"] = "other-caster.example"
    imported["video"]["streams"][0]["url"] = "rtsp://10.9.9.9/live"
    out = settings_bundle.merge(current, imported, set(settings_bundle.SECTIONS), secrets=False)
    assert not out["ssh_connections"][0].get("password")
    assert not out["rtk"]["ntrip"].get("password")
    assert not out["video"]["streams"][0].get("password")


def test_merge_with_secrets_takes_the_file_as_it_is():
    current = _secret_config()
    imported = settings_bundle.redact(current)
    out = settings_bundle.merge(current, imported, set(settings_bundle.SECTIONS), secrets=True)
    assert "map_tokens" not in out
    assert not out["ssh_connections"][0].get("password")


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------

class _Store:
    def __init__(self, armed: bool) -> None:
        self.armed = armed

    def get_snapshot(self) -> dict[str, Any]:
        return {"armed": self.armed}

    def update(self, **_kw: Any) -> None:
        pass


def _handler(tmp_path, config: CorvusConfig | None = None, **attrs: Any):
    handler = object.__new__(CorvusHandler)
    handler.mavlink = None
    handler.store = None
    handler.ssh = None
    handler.video = None
    handler.rtk = None
    handler.autoconnect_session = None
    handler.config = config if config is not None else CorvusConfig()
    handler.config_path = str(tmp_path / "config.json")
    handler.plugin_user_dir = str(tmp_path / "plugins")
    handler.plugin_bundled_dir = str(tmp_path / "bundled")
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    for key, value in attrs.items():
        setattr(handler, key, value)
    return handler, responses


def _export(handler, responses, tmp_path, **payload: Any) -> dict[str, Any]:
    body = {"dir": str(tmp_path / "out"), "filename": "station.json"}
    body.update(payload)
    handler._api_settings_export(body)
    data, status = responses[-1]
    assert status == 200, data
    return data


def test_export_writes_the_file_and_never_echoes_a_secret(tmp_path):
    handler, responses = _handler(tmp_path, _build_config(_secret_config()))
    plugin_config.save("demo", {"folder": "/data"}, str(tmp_path / "plugins"))
    data = _export(handler, responses, tmp_path, include_secrets=True,
                   browser={"corvus.hud": '{"x":40,"y":60}'})
    assert _secrets_in(json.dumps(data)) == []
    assert data["path"] == str(tmp_path / "out" / "station.json")
    written = json.loads((tmp_path / "out" / "station.json").read_text(encoding="utf-8"))
    assert written["kind"] == "corvus-settings"
    assert written["browser"] == {"corvus.hud": '{"x":40,"y":60}'}
    assert written["plugins"] == {"demo": {"folder": "/data"}}
    assert len(_secrets_in(json.dumps(written))) == 4


def test_export_without_secrets_writes_none(tmp_path):
    handler, responses = _handler(tmp_path, _build_config(_secret_config()))
    _export(handler, responses, tmp_path)
    assert _secrets_in((tmp_path / "out" / "station.json").read_text(encoding="utf-8")) == []


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX permission bits")
def test_export_file_is_owner_only(tmp_path):
    handler, responses = _handler(tmp_path)
    _export(handler, responses, tmp_path, include_secrets=True)
    assert os.stat(tmp_path / "out" / "station.json").st_mode & 0o077 == 0


def test_export_carries_the_company_logo(tmp_path):
    handler, responses = _handler(tmp_path, _build_config({"branding": {"logo": "mark.png"}}))
    handler._store_logo(PNG)
    _export(handler, responses, tmp_path)
    written = json.loads((tmp_path / "out" / "station.json").read_text(encoding="utf-8"))
    assert base64.b64decode(written["logo"]) == PNG


def test_export_refuses_a_non_boolean_secret_switch(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_settings_export({"include_secrets": "yes"})
    assert responses[-1][1] == 400


def test_import_round_trips_an_export_onto_a_fresh_station(tmp_path):
    source_dir = tmp_path / "a"
    source_dir.mkdir()
    source, responses = _handler(source_dir, _build_config(_secret_config()))
    source.config.branding = {"logo": "mark.png"}
    source._store_logo(PNG)
    plugin_config.save("demo", {"folder": "/data"}, str(source_dir / "plugins"))
    _export(source, responses, tmp_path, include_secrets=True)
    bundle = json.loads((tmp_path / "out" / "station.json").read_text(encoding="utf-8"))

    target_dir = tmp_path / "b"
    target_dir.mkdir()
    target, responses = _handler(target_dir)
    target._api_settings_import({"bundle": bundle})
    data, status = responses[-1]
    assert status == 200, data
    assert data["ok"] is True
    assert data["plugins"] == 1
    assert _secrets_in(json.dumps(data)) == []

    assert _config_to_dict(target.config) == _config_to_dict(source.config)
    on_disk = json.loads((target_dir / "config.json").read_text(encoding="utf-8"))
    assert on_disk["theme"] == {"name": "dusk"}
    assert (target_dir / "branding" / "logo.png").read_bytes() == PNG
    assert plugin_config.load("demo", str(target_dir / "plugins")) == {"folder": "/data"}


def test_import_only_touches_the_chosen_sections(tmp_path):
    handler, responses = _handler(tmp_path, _build_config({"mavlink_connection": "COM7"}))
    bundle = settings_bundle.build(_secret_config())
    handler._api_settings_import({"bundle": bundle, "sections": ["interface"]})
    assert responses[-1][1] == 200
    assert handler.config.theme == {"name": "dusk"}
    assert handler.config.mavlink_connection == "COM7"
    assert handler.config.ssh_connections == []


def test_import_is_refused_while_armed(tmp_path):
    handler, responses = _handler(tmp_path, store=_Store(armed=True))
    handler._api_settings_import({"bundle": settings_bundle.build(_secret_config())})
    data, status = responses[-1]
    assert status == 409
    assert handler.config.theme is None
    assert not (tmp_path / "config.json").exists()


@pytest.mark.parametrize("payload", [
    {"bundle": {"kind": "something-else"}},
    {"bundle": settings_bundle.build({}), "sections": "interface"},
    {"bundle": settings_bundle.build({}), "sections": []},
])
def test_import_rejects_bad_input_without_writing(tmp_path, payload):
    handler, responses = _handler(tmp_path)
    handler._api_settings_import(payload)
    assert responses[-1][1] == 400
    assert not (tmp_path / "config.json").exists()


def test_import_without_a_logo_removes_the_stored_one(tmp_path):
    handler, responses = _handler(tmp_path, _build_config({"branding": {"logo": "mark.png"}}))
    handler._store_logo(PNG)
    bundle = settings_bundle.build(_config_to_dict(CorvusConfig()))
    handler._api_settings_import({"bundle": bundle, "sections": ["interface"]})
    assert responses[-1][1] == 200
    assert handler.config.branding is None
    assert not (tmp_path / "branding" / "logo.png").exists()


def test_import_names_the_settings_that_need_a_restart(tmp_path):
    handler, responses = _handler(tmp_path)
    imported = _config_to_dict(CorvusConfig())
    imported["http_port"] = 9123
    imported["params_dir"] = "/somewhere"
    handler._api_settings_import({"bundle": settings_bundle.build(imported),
                                  "sections": ["folders"]})
    data, status = responses[-1]
    assert status == 200
    assert data["restart"] == ["http_port"]
    assert handler.config.params_dir == "/somewhere"


def test_import_restarts_the_forwarder_only_when_forwarding_changed(tmp_path):
    handler, responses = _handler(tmp_path)
    calls: list[int] = []
    handler._restart_forwarder = lambda: calls.append(1)  # type: ignore[method-assign]
    unchanged = settings_bundle.build(_config_to_dict(CorvusConfig()))
    handler._api_settings_import({"bundle": unchanged})
    assert calls == []
    changed = _config_to_dict(CorvusConfig())
    changed["forwarding"] = {"enabled": False, "port": 14560}
    handler._api_settings_import({"bundle": settings_bundle.build(changed)})
    assert calls == [1]


# ---------------------------------------------------------------------------
# Choosing what goes in: sections and plugins
# ---------------------------------------------------------------------------

def _user_plugin(root, plugin_id: str = "mine") -> None:
    folder = root / plugin_id
    folder.mkdir(parents=True)
    (folder / "plugin.json").write_text(json.dumps({"id": plugin_id, "name": "Mine"}))
    (folder / f"{plugin_id}.js").write_text("Corvus.plugins.register('mine', {});\n")


def _bundled_plugin(root, plugin_id: str = "shipped") -> None:
    _user_plugin(root, plugin_id)


def test_build_carries_only_the_chosen_sections():
    config = _secret_config()
    bundle = settings_bundle.build(config, sections=["interface"],
                                   plugins={"demo": {"a": 1}}, logo=PNG)
    assert bundle["format"] == 2
    assert bundle["sections"] == ["interface"]
    assert set(bundle["config"]) == {"theme"}
    assert bundle["plugins"] == {}
    assert bundle["plugin_files"] == {}
    assert base64.b64decode(bundle["logo"]) == PNG


def test_build_leaves_the_logo_out_without_the_interface():
    bundle = settings_bundle.build(_secret_config(), sections=["connections"], logo=PNG)
    assert "logo" not in bundle
    assert "mavlink_connection" in bundle["config"]
    assert "theme" not in bundle["config"]


def test_build_encodes_plugin_files():
    bundle = settings_bundle.build({}, sections=["plugins"],
                                   plugin_files={"mine": {"plugin.json": b"{}", "mine.js": b"x"}})
    assert bundle["sections"] == ["plugins"]
    assert base64.b64decode(bundle["plugin_files"]["mine"]["mine.js"]) == b"x"


def test_parse_reads_a_format_1_file_as_carrying_everything():
    old = settings_bundle.build(_secret_config())
    old["format"] = 1
    del old["sections"]
    parsed = settings_bundle.parse(old)
    assert parsed["sections"] == list(settings_bundle.SECTIONS)
    assert parsed["plugin_files"] == {}


def test_parse_refuses_a_format_2_file_that_does_not_say_what_it_carries():
    bundle = settings_bundle.build({})
    del bundle["sections"]
    with pytest.raises(ValueError, match="which settings"):
        settings_bundle.parse(bundle)


def test_parse_ignores_config_keys_of_sections_the_file_does_not_carry():
    bundle = settings_bundle.build(_secret_config(), sections=["interface"])
    bundle["config"]["mavlink_connection"] = "COM9"
    assert "mavlink_connection" not in settings_bundle.parse(bundle)["config"]


@pytest.mark.parametrize("files", [
    {"mine.js": base64.b64encode(b"x").decode()},                       # no plugin.json
    {"plugin.json": "e30=", "../escape.js": "eA=="},                    # escapes the folder
    {"plugin.json": "e30=", "run.py": "eA=="},                          # not a plugin file
    {"plugin.json": "not base64!"},
])
def test_parse_refuses_a_plugin_that_could_not_be_installed(files):
    bundle = settings_bundle.build({}, sections=["plugins"])
    bundle["plugin_files"] = {"mine": files}
    with pytest.raises(ValueError, match="plugin mine"):
        settings_bundle.parse(bundle)


def test_export_writes_only_the_chosen_sections(tmp_path):
    handler, responses = _handler(tmp_path, _build_config(_secret_config()))
    plugin_config.save("demo", {"folder": "/data"}, str(tmp_path / "plugins"))
    data = _export(handler, responses, tmp_path, sections=["interface", "map"],
                   browser={"corvus.theme": "green"})
    assert data["sections"] == ["interface", "map"]
    written = json.loads((tmp_path / "out" / "station.json").read_text(encoding="utf-8"))
    assert written["sections"] == ["interface", "map"]
    assert "mavlink_connection" not in written["config"]
    assert written["plugins"] == {}


def test_export_refuses_an_empty_choice(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_settings_export({"sections": [], "dir": str(tmp_path)})
    assert responses[-1][1] == 400
    assert list(tmp_path.iterdir()) == []


def test_export_takes_the_settings_of_the_chosen_plugins_only(tmp_path):
    handler, responses = _handler(tmp_path)
    plugin_config.save("one", {"a": 1}, str(tmp_path / "plugins"))
    plugin_config.save("two", {"b": 2}, str(tmp_path / "plugins"))
    _export(handler, responses, tmp_path, plugin_settings=["two", "unknown"])
    written = json.loads((tmp_path / "out" / "station.json").read_text(encoding="utf-8"))
    assert written["plugins"] == {"two": {"b": 2}}


def test_export_carries_a_user_plugin_with_its_files(tmp_path):
    handler, responses = _handler(tmp_path)
    _user_plugin(tmp_path / "plugins")
    plugin_config.save("mine", {"a": 1}, str(tmp_path / "plugins"))
    _export(handler, responses, tmp_path, plugin_files=["mine"])
    written = json.loads((tmp_path / "out" / "station.json").read_text(encoding="utf-8"))
    files = written["plugin_files"]["mine"]
    assert sorted(files) == ["mine.js", "plugin.json"]
    assert written["plugins"] == {"mine": {"a": 1}}


def test_export_refuses_the_files_of_a_bundled_plugin(tmp_path):
    handler, responses = _handler(tmp_path)
    _bundled_plugin(tmp_path / "bundled")
    handler._api_settings_export({"dir": str(tmp_path / "out"), "plugin_files": ["shipped"]})
    data, status = responses[-1]
    assert status == 400
    assert "shipped" in data["error"]


def test_a_partial_file_never_resets_the_sections_it_does_not_carry(tmp_path):
    """Importing a file with only the interface keeps this station's link."""
    handler, responses = _handler(tmp_path, _build_config({
        "mavlink_connection": "COM7",
        "ssh_connections": [{"name": "companion", "host": "10.0.0.2"}],
    }))
    bundle = settings_bundle.build(_secret_config(), sections=["interface"])
    handler._api_settings_import({"bundle": bundle})
    data, status = responses[-1]
    assert status == 200, data
    assert data["sections"] == ["interface"]
    assert handler.config.theme == {"name": "dusk"}
    assert handler.config.mavlink_connection == "COM7"
    assert handler.config.ssh_connections[0]["host"] == "10.0.0.2"


def test_import_of_only_sections_the_file_lacks_is_refused(tmp_path):
    handler, responses = _handler(tmp_path)
    bundle = settings_bundle.build(_secret_config(), sections=["interface"])
    handler._api_settings_import({"bundle": bundle, "sections": ["connections"]})
    assert responses[-1][1] == 400
    assert not (tmp_path / "config.json").exists()


def test_import_installs_the_chosen_plugins(tmp_path):
    source_dir = tmp_path / "a"
    source, responses = _handler(source_dir)
    _user_plugin(source_dir / "plugins")
    _user_plugin(source_dir / "plugins", "other")
    plugin_config.save("mine", {"a": 1}, str(source_dir / "plugins"))
    _export(source, responses, tmp_path, plugin_files=["mine", "other"])
    bundle = json.loads((tmp_path / "out" / "station.json").read_text(encoding="utf-8"))

    target_dir = tmp_path / "b"
    target, responses = _handler(target_dir)
    target._api_settings_import({"bundle": bundle, "sections": ["plugins"],
                                 "plugin_files": ["mine"], "plugin_settings": []})
    data, status = responses[-1]
    assert status == 200, data
    assert data["plugins_installed"] == 1
    assert data["plugins"] == 0
    assert (target_dir / "plugins" / "mine" / "mine.js").is_file()
    assert not (target_dir / "plugins" / "other").exists()
    assert plugin_config.load("mine", str(target_dir / "plugins")) is None


def test_import_of_plugin_files_keeps_the_settings_stored_here(tmp_path):
    handler, responses = _handler(tmp_path)
    plugin_config.save("mine", {"here": True}, str(tmp_path / "plugins"))
    files = {"plugin.json": json.dumps({"id": "mine"}).encode(), "mine.js": b"x"}
    bundle = settings_bundle.build({}, sections=["plugins"], plugins={"mine": {"file": True}},
                                   plugin_files={"mine": files})
    handler._api_settings_import({"bundle": bundle, "plugin_settings": []})
    assert responses[-1][1] == 200
    assert plugin_config.load("mine", str(tmp_path / "plugins")) == {"here": True}
    handler._api_settings_import({"bundle": bundle})
    assert plugin_config.load("mine", str(tmp_path / "plugins")) == {"file": True}


# ---------------------------------------------------------------------------
# First start setup
# ---------------------------------------------------------------------------

def test_first_start_is_not_pending_without_create_server(tmp_path):
    handler, responses = _handler(tmp_path)
    handler._api_welcome()
    assert responses[-1] == ({"pending": False}, 200)


def test_finishing_the_first_start_writes_the_config(tmp_path):
    state = {"pending": True}
    handler, responses = _handler(tmp_path, first_start=state)
    handler._api_welcome()
    assert responses[-1][0] == {"pending": True}
    handler._api_welcome_done({})
    assert responses[-1] == ({"ok": True, "saved": True}, 200)
    assert state == {"pending": False}
    assert (tmp_path / "config.json").is_file()
    handler._api_welcome()
    assert responses[-1][0] == {"pending": False}


def test_an_import_counts_as_the_first_start_setup(tmp_path):
    state = {"pending": True}
    handler, responses = _handler(tmp_path, first_start=state)
    handler._api_settings_import({"bundle": settings_bundle.build(_secret_config())})
    assert responses[-1][1] == 200
    assert state == {"pending": False}


def test_a_refused_import_leaves_the_first_start_pending(tmp_path):
    state = {"pending": True}
    handler, responses = _handler(tmp_path, first_start=state, store=_Store(armed=True))
    handler._api_settings_import({"bundle": settings_bundle.build(_secret_config())})
    assert responses[-1][1] == 409
    assert state == {"pending": True}
