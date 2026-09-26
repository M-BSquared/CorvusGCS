"""Per-plugin config files: load, save, merge and the one-time migration."""
from __future__ import annotations

import json

from corvus import plugin_config
from corvus.config import CorvusConfig, load_config, save_config
from corvus.server import _migrate_plugin_settings


def test_path_is_the_plugin_folder_under_the_user_root(tmp_path):
    path = plugin_config.config_path("schwalby", str(tmp_path))
    assert path == str(tmp_path / "schwalby" / "config.json")


def test_path_refuses_an_id_that_is_not_a_folder_name(tmp_path):
    for bad in ("", "../x", "a/b", ".hidden", "a\\b", 5):
        assert plugin_config.config_path(bad, str(tmp_path)) is None  # type: ignore[arg-type]


def test_missing_file_is_none(tmp_path):
    assert plugin_config.load("demo", str(tmp_path)) is None


def test_a_broken_file_is_ignored_not_raised(tmp_path):
    (tmp_path / "demo").mkdir()
    (tmp_path / "demo" / "config.json").write_text("{not json", encoding="utf-8")
    assert plugin_config.load("demo", str(tmp_path)) is None
    (tmp_path / "demo" / "config.json").write_text("[1, 2]", encoding="utf-8")
    assert plugin_config.load("demo", str(tmp_path)) is None


def test_save_writes_readable_json_and_leaves_no_temp_file(tmp_path):
    plugin_config.save("demo", {"name": "Böck", "n": 1}, str(tmp_path))
    text = (tmp_path / "demo" / "config.json").read_text(encoding="utf-8")
    assert json.loads(text) == {"name": "Böck", "n": 1}
    assert "Böck" in text and text.endswith("\n")
    assert sorted(p.name for p in (tmp_path / "demo").iterdir()) == ["config.json"]


def test_update_merges_and_replace_drops(tmp_path):
    root = str(tmp_path)
    plugin_config.update("demo", {"a": 1}, user_dir=root)
    assert plugin_config.update("demo", {"b": 2}, user_dir=root) == {"a": 1, "b": 2}
    assert plugin_config.update("demo", {"b": 3}, replace=True, user_dir=root) == {"b": 3}
    assert plugin_config.load("demo", root) == {"b": 3}


def test_load_all_keys_by_folder_and_skips_the_rest(tmp_path):
    root = str(tmp_path)
    plugin_config.save("a", {"x": 1}, root)
    plugin_config.save("b", {"y": 2}, root)
    (tmp_path / "no-config").mkdir()
    (tmp_path / "README.md").write_text("hi", encoding="utf-8")
    assert plugin_config.load_all(root) == {"a": {"x": 1}, "b": {"y": 2}}


def test_load_all_of_a_missing_root_is_empty(tmp_path):
    assert plugin_config.load_all(str(tmp_path / "nope")) == {}


def test_migrate_writes_files_and_keeps_an_existing_one(tmp_path):
    root = str(tmp_path)
    plugin_config.save("kept", {"new": True}, root)
    ok = plugin_config.migrate({"kept": {"old": True}, "moved": {"v": 1}, "junk": 5}, root)
    assert ok
    assert plugin_config.load("kept", root) == {"new": True}
    assert plugin_config.load("moved", root) == {"v": 1}


def test_migrate_reports_a_failed_write(tmp_path):
    blocker = tmp_path / "file"
    blocker.write_text("", encoding="utf-8")
    assert plugin_config.migrate({"demo": {"v": 1}}, str(blocker)) is False


def test_startup_migration_empties_the_main_configs_plugins_key(tmp_path, monkeypatch):
    monkeypatch.setattr(plugin_config, "user_plugins_dir", lambda: str(tmp_path / "plugins"))
    cfg_path = str(tmp_path / "config.json")
    save_config(CorvusConfig(plugins={"schwalby": {"buttons": []}}), cfg_path)
    cfg = load_config(cfg_path)
    _migrate_plugin_settings(cfg, cfg_path)
    assert cfg.plugins is None
    assert "plugins" not in json.loads((tmp_path / "config.json").read_text(encoding="utf-8"))
    assert plugin_config.load("schwalby", str(tmp_path / "plugins")) == {"buttons": []}


def test_startup_migration_keeps_the_key_when_a_write_fails(tmp_path, monkeypatch):
    blocker = tmp_path / "plugins"
    blocker.write_text("", encoding="utf-8")
    monkeypatch.setattr(plugin_config, "user_plugins_dir", lambda: str(blocker))
    cfg_path = str(tmp_path / "config.json")
    save_config(CorvusConfig(plugins={"schwalby": {"buttons": []}}), cfg_path)
    cfg = load_config(cfg_path)
    _migrate_plugin_settings(cfg, cfg_path)
    assert cfg.plugins == {"schwalby": {"buttons": []}}
    assert load_config(cfg_path).plugins == {"schwalby": {"buttons": []}}
