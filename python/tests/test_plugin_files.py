"""A plugin carried in a settings file: collecting it and installing it.

Covers ``corvus/plugin_files.py``: which paths may travel, what a collect
leaves behind, and that an install replaces a plugin whole, keeps its saved
settings, and leaves the installed copy alone when it fails. Plus the one
registry rule it relies on: a hidden folder is never loaded as a plugin.
"""
from __future__ import annotations

import json
import os
import pathlib
import sys

import pytest

from corvus import plugin_files
from corvus.plugin_registry import discover


def _manifest(plugin_id: str = "demo", **extra: object) -> bytes:
    return json.dumps({"id": plugin_id, "name": "Demo", "scripts": ["demo.js"], **extra}).encode()


def _write_plugin(root, plugin_id: str = "demo") -> str:
    folder = root / plugin_id
    (folder / "img").mkdir(parents=True)
    (folder / "plugin.json").write_bytes(_manifest(plugin_id))
    (folder / "demo.js").write_text("Corvus.plugins.register('demo', {});\n")
    (folder / "img" / "icon.png").write_bytes(b"\x89PNG\r\n\x1a\n")
    (folder / "config.json").write_text('{"folder": "/data"}\n')
    (folder / "build.py").write_text("print('not served')\n")
    (folder / ".secret.js").write_text("hidden\n")
    (folder / ".git").mkdir()
    (folder / ".git" / "HEAD.txt").write_text("ref\n")
    return str(folder)


@pytest.mark.parametrize("rel", ["demo.js", "img/icon.png", "a/b/c.css", "README.md"])
def test_clean_path_accepts_plugin_files(rel):
    assert plugin_files.clean_path(rel) == rel


@pytest.mark.parametrize("rel", [
    "", "../demo.js", "img/../../x.js", "/etc/x.js", "C:/x.js", "img\\..\\..\\x.js",
    ".hidden.js", "a/.git/x.js", "config.json", "build.py", "run.sh", "noext",
    "con.js", "img/nul.png", "a:b.js", "trailing./x.js", None, 7,
])
def test_clean_path_refuses_what_could_escape_or_is_not_served(rel):
    assert plugin_files.clean_path(rel) is None


def test_clean_path_keeps_a_nested_config_json():
    """Only the top level config.json is the plugin's settings."""
    assert plugin_files.clean_path("data/config.json") == "data/config.json"


def test_collect_takes_the_plugin_and_leaves_the_rest(tmp_path):
    folder = _write_plugin(tmp_path)
    files = plugin_files.collect(folder)
    assert sorted(files) == ["demo.js", "img/icon.png", "plugin.json"]
    assert files["plugin.json"] == _manifest()


@pytest.mark.skipif(sys.platform == "win32", reason="symlinks need privileges on Windows")
def test_collect_never_follows_a_symlink_out_of_the_folder(tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "secret.txt").write_text("private\n")
    folder = pathlib.Path(_write_plugin(tmp_path))
    os.symlink(outside / "secret.txt", folder / "leak.txt")
    os.symlink(outside, folder / "linked")
    assert "leak.txt" not in plugin_files.collect(folder)
    assert not any(k.startswith("linked/") for k in plugin_files.collect(folder))


def test_collect_refuses_a_plugin_that_is_too_large(tmp_path, monkeypatch):
    folder = _write_plugin(tmp_path)
    monkeypatch.setattr(plugin_files, "MAX_PLUGIN_BYTES", 16)
    with pytest.raises(ValueError, match="larger than"):
        plugin_files.collect(folder)


def test_install_writes_a_new_plugin_that_discovery_finds(tmp_path):
    files = plugin_files.collect(_write_plugin(tmp_path / "a"))
    user = tmp_path / "b"
    path = plugin_files.install("demo", files, str(user))
    assert path == str(user / "demo")
    assert (user / "demo" / "img" / "icon.png").read_bytes() == b"\x89PNG\r\n\x1a\n"
    found = discover(str(user), str(tmp_path / "none"))
    assert [m["id"] for m in found] == ["demo"]
    assert [p.name for p in user.iterdir()] == ["demo"]


def test_install_replaces_the_old_copy_and_keeps_its_settings(tmp_path):
    user = tmp_path / "user"
    old = user / "demo"
    old.mkdir(parents=True)
    (old / "plugin.json").write_bytes(_manifest(version="1"))
    (old / "demo.js").write_text("old\n")
    (old / "gone.js").write_text("only in the old copy\n")
    (old / "config.json").write_text('{"kept": true}\n')

    plugin_files.install("demo", {"plugin.json": _manifest(version="2"), "demo.js": b"new\n"},
                         str(user))
    assert (old / "demo.js").read_text() == "new\n"
    assert not (old / "gone.js").exists()
    assert json.loads((old / "config.json").read_text()) == {"kept": True}
    assert sorted(p.name for p in user.iterdir()) == ["demo"]


@pytest.mark.parametrize("files", [
    {"demo.js": b"x"},                                  # no plugin.json
    {"plugin.json": _manifest(), "../evil.js": b"x"},   # escapes the folder
    {"plugin.json": _manifest(), "demo.js": "text"},    # not bytes
])
def test_a_refused_install_leaves_the_installed_copy_alone(tmp_path, files):
    user = tmp_path / "user"
    (user / "demo").mkdir(parents=True)
    (user / "demo" / "demo.js").write_text("installed\n")
    with pytest.raises(ValueError):
        plugin_files.install("demo", files, str(user))
    assert (user / "demo" / "demo.js").read_text() == "installed\n"
    assert sorted(p.name for p in user.iterdir()) == ["demo"]


def test_install_refuses_an_id_that_is_not_a_folder_name(tmp_path):
    with pytest.raises(ValueError):
        plugin_files.install("../demo", {"plugin.json": _manifest()}, str(tmp_path))


@pytest.mark.skipif(sys.platform == "win32", reason="symlinks need privileges on Windows")
def test_install_over_a_symlinked_plugin_keeps_the_linked_folder(tmp_path):
    work = tmp_path / "work"
    work.mkdir()
    (work / "plugin.json").write_bytes(_manifest())
    (work / "demo.js").write_text("working copy\n")
    user = tmp_path / "user"
    user.mkdir()
    os.symlink(work, user / "demo")
    plugin_files.install("demo", {"plugin.json": _manifest(), "demo.js": b"imported\n"}, str(user))
    assert (work / "demo.js").read_text() == "working copy\n"
    assert not (user / "demo").is_symlink()
    assert (user / "demo" / "demo.js").read_text() == "imported\n"
    assert sorted(p.name for p in user.iterdir()) == ["demo"]


def test_discovery_skips_a_hidden_staging_folder(tmp_path):
    user = tmp_path / "user"
    staging = user / ".demo.import-abc123"
    staging.mkdir(parents=True)
    (staging / "plugin.json").write_bytes(_manifest())
    (staging / "demo.js").write_text("half written\n")
    assert discover(str(user), str(tmp_path / "none")) == []
