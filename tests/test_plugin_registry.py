"""Plugin folder discovery: what counts as a plugin and what is refused.

Every test points the scan at a ``tmp_path`` so it never reads the developer's
own ``~/.corvus/plugins``.
"""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path

import pytest

from corvus import plugin_registry as reg


def _make_plugin(root, plugin_id: str, manifest: dict | None = None,
                 script: str | None = "console.log('x');") -> str:
    """Create a plugin folder under *root* and return its path."""
    folder = root / plugin_id
    folder.mkdir(parents=True, exist_ok=True)
    if script is not None:
        (folder / f"{plugin_id}.js").write_text(script, encoding="utf-8")
    body = {"id": plugin_id, "name": plugin_id.title(), "scripts": [f"{plugin_id}.js"]}
    if manifest is not None:
        body = manifest
    (folder / "plugin.json").write_text(json.dumps(body), encoding="utf-8")
    return str(folder)


# ---------------------------------------------------------------------------
# Discovery
# ---------------------------------------------------------------------------

def test_discovers_a_plugin_folder(tmp_path):
    user = tmp_path / "user"
    _make_plugin(user, "demo")
    found = reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none"))
    assert [p["id"] for p in found] == ["demo"]
    assert found[0]["name"] == "Demo"
    assert found[0]["scripts"] == ["demo.js"]
    assert found[0]["source"] == "user"


def test_missing_roots_are_not_an_error(tmp_path):
    assert reg.discover(user_dir=str(tmp_path / "a"), bundled_dir=str(tmp_path / "b")) == []


def test_defaults_fill_in_for_a_minimal_manifest(tmp_path):
    user = tmp_path / "user"
    folder = user / "minimal"
    folder.mkdir(parents=True)
    (folder / "minimal.js").write_text("//", encoding="utf-8")
    (folder / "plugin.json").write_text("{}", encoding="utf-8")
    found = reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none"))
    assert len(found) == 1
    # id from the folder name, name from the id, the conventional <id>.js, and
    # the fallback icon — a manifest can be empty and still work.
    assert found[0]["id"] == "minimal"
    assert found[0]["name"] == "minimal"
    assert found[0]["icon"] == "puzzle"
    assert found[0]["scripts"] == ["minimal.js"]


def test_user_plugin_overrides_a_bundled_one(tmp_path):
    bundled, user = tmp_path / "bundled", tmp_path / "user"
    _make_plugin(bundled, "demo")
    _make_plugin(user, "demo", manifest={
        "id": "demo", "name": "Patched", "scripts": ["demo.js"],
    })
    found = reg.discover(user_dir=str(user), bundled_dir=str(bundled))
    assert len(found) == 1
    assert found[0]["name"] == "Patched"
    assert found[0]["source"] == "user"


def test_bundled_and_user_plugins_are_listed_together(tmp_path):
    bundled, user = tmp_path / "bundled", tmp_path / "user"
    _make_plugin(bundled, "alpha")
    _make_plugin(user, "beta")
    found = reg.discover(user_dir=str(user), bundled_dir=str(bundled))
    assert [p["id"] for p in found] == ["alpha", "beta"]
    assert [p["source"] for p in found] == ["bundled", "user"]


def test_order_decides_the_grid_not_the_root(tmp_path):
    bundled, user = tmp_path / "bundled", tmp_path / "user"
    _make_plugin(bundled, "last", manifest={
        "id": "last", "name": "Last", "order": 90, "scripts": ["last.js"]})
    _make_plugin(user, "first", manifest={
        "id": "first", "name": "First", "order": 10, "scripts": ["first.js"]})
    found = reg.discover(user_dir=str(user), bundled_dir=str(bundled))
    # A user plugin can sort ahead of a bundled one — the root is not the
    # ordering, the manifest is.
    assert [p["id"] for p in found] == ["first", "last"]


def test_plugins_without_an_order_sort_by_name(tmp_path):
    user = tmp_path / "user"
    _make_plugin(user, "zulu", manifest={"id": "zulu", "name": "Zulu", "scripts": ["zulu.js"]})
    _make_plugin(user, "alpha", manifest={"id": "alpha", "name": "Alpha", "scripts": ["alpha.js"]})
    found = reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none"))
    assert [p["id"] for p in found] == ["alpha", "zulu"]
    assert [p["order"] for p in found] == [reg.DEFAULT_ORDER] * 2


@pytest.mark.parametrize("raw", ["5", True, None, [], {}, float("nan")])
def test_a_bad_order_falls_back_to_the_default(tmp_path, raw):
    user = tmp_path / "user"
    folder = user / "demo"
    folder.mkdir(parents=True)
    (folder / "demo.js").write_text("//", encoding="utf-8")
    (folder / "plugin.json").write_text(
        json.dumps({"id": "demo", "order": raw, "scripts": ["demo.js"]}), encoding="utf-8")
    found = reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none"))
    assert found[0]["order"] == reg.DEFAULT_ORDER


# ---------------------------------------------------------------------------
# Folders that are refused — a bad plugin costs itself, never the scan
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("body", ["{ not json", '"a string"', "[]"])
def test_malformed_manifest_is_skipped(tmp_path, body):
    user = tmp_path / "user"
    folder = user / "broken"
    folder.mkdir(parents=True)
    (folder / "broken.js").write_text("//", encoding="utf-8")
    (folder / "plugin.json").write_text(body, encoding="utf-8")
    _make_plugin(user, "good")
    found = reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none"))
    assert [p["id"] for p in found] == ["good"]


def test_folder_without_a_manifest_is_not_a_plugin(tmp_path):
    user = tmp_path / "user"
    (user / "notaplugin").mkdir(parents=True)
    (user / "notaplugin" / "notaplugin.js").write_text("//", encoding="utf-8")
    assert reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none")) == []


def test_manifest_without_a_loadable_script_is_skipped(tmp_path):
    user = tmp_path / "user"
    folder = user / "empty"
    folder.mkdir(parents=True)
    (folder / "plugin.json").write_text(
        json.dumps({"id": "empty", "scripts": ["gone.js"]}), encoding="utf-8")
    assert reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none")) == []


@pytest.mark.parametrize("bad_id", ["../evil", "with/slash", ".hidden", "", "a" * 100])
def test_invalid_id_is_refused(tmp_path, bad_id):
    user = tmp_path / "user"
    folder = user / "folder"
    folder.mkdir(parents=True)
    (folder / "folder.js").write_text("//", encoding="utf-8")
    (folder / "plugin.json").write_text(
        json.dumps({"id": bad_id, "scripts": ["folder.js"]}), encoding="utf-8")
    found = reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none"))
    # An empty id falls back to the folder name, which is valid; every other
    # shape here must not produce a plugin.
    assert [p["id"] for p in found] == (["folder"] if bad_id == "" else [])


def test_a_file_is_not_scanned_as_a_plugin(tmp_path):
    user = tmp_path / "user"
    user.mkdir(parents=True)
    (user / "plugin.json").write_text("{}", encoding="utf-8")
    assert reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none")) == []


# ---------------------------------------------------------------------------
# Asset resolution — the containment check behind the HTTP route
# ---------------------------------------------------------------------------

def test_resolve_asset_returns_a_file_in_the_folder(tmp_path):
    folder = tmp_path / "demo"
    folder.mkdir()
    (folder / "demo.js").write_text("//", encoding="utf-8")
    assert reg.resolve_asset(str(folder), "demo.js") == (folder / "demo.js").resolve()


def test_resolve_asset_allows_a_subfolder(tmp_path):
    folder = tmp_path / "demo"
    (folder / "assets").mkdir(parents=True)
    (folder / "assets" / "icon.svg").write_text("<svg/>", encoding="utf-8")
    assert reg.resolve_asset(str(folder), "assets/icon.svg") is not None


@pytest.mark.parametrize("rel", [
    "../secret.js",
    "../../etc/passwd",
    "assets/../../secret.js",
    "/etc/passwd",
    "",
])
def test_resolve_asset_refuses_traversal(tmp_path, rel):
    folder = tmp_path / "demo"
    folder.mkdir()
    (tmp_path / "secret.js").write_text("//", encoding="utf-8")
    assert reg.resolve_asset(str(folder), rel) is None


def test_resolve_asset_refuses_a_disallowed_suffix(tmp_path):
    folder = tmp_path / "demo"
    folder.mkdir()
    (folder / "run.sh").write_text("#!/bin/sh", encoding="utf-8")
    (folder / "run").write_text("x", encoding="utf-8")
    assert reg.resolve_asset(str(folder), "run.sh") is None
    assert reg.resolve_asset(str(folder), "run") is None


def test_resolve_asset_refuses_a_directory_and_a_missing_file(tmp_path):
    folder = tmp_path / "demo"
    (folder / "assets.js").mkdir(parents=True)
    assert reg.resolve_asset(str(folder), "assets.js") is None
    assert reg.resolve_asset(str(folder), "gone.js") is None


@pytest.mark.skipif(os.name == "nt", reason="POSIX symlink semantics")
def test_resolve_asset_refuses_a_symlink_out_of_the_folder(tmp_path):
    folder = tmp_path / "demo"
    folder.mkdir()
    (tmp_path / "outside.js").write_text("//", encoding="utf-8")
    (folder / "link.js").symlink_to(tmp_path / "outside.js")
    assert reg.resolve_asset(str(folder), "link.js") is None


def test_manifest_drops_a_style_that_escapes_the_folder(tmp_path):
    user = tmp_path / "user"
    folder = user / "demo"
    folder.mkdir(parents=True)
    (folder / "demo.js").write_text("//", encoding="utf-8")
    (user / "outside.css").write_text("a{}", encoding="utf-8")
    (folder / "plugin.json").write_text(json.dumps({
        "id": "demo", "scripts": ["demo.js"], "styles": ["../outside.css"],
    }), encoding="utf-8")
    found = reg.discover(user_dir=str(user), bundled_dir=str(tmp_path / "none"))
    assert found[0]["styles"] == []


# ---------------------------------------------------------------------------
# Lookup + the public shape
# ---------------------------------------------------------------------------

def test_find_dir_only_resolves_a_discovered_plugin(tmp_path):
    user = tmp_path / "user"
    path = _make_plugin(user, "demo")
    (user / "bare").mkdir()          # a folder, but not a plugin
    assert reg.find_dir("demo", str(user), str(tmp_path / "none")) == path
    assert reg.find_dir("bare", str(user), str(tmp_path / "none")) is None
    assert reg.find_dir("../etc", str(user), str(tmp_path / "none")) is None


def test_public_list_drops_the_server_side_path(tmp_path):
    user = tmp_path / "user"
    _make_plugin(user, "demo")
    public = reg.public_list(str(user), str(tmp_path / "none"))
    assert "dir" not in public[0]
    assert public[0]["id"] == "demo"


# ---------------------------------------------------------------------------
# The user folder itself
# ---------------------------------------------------------------------------

def test_ensure_user_plugins_dir_creates_it_with_a_readme(tmp_path, monkeypatch):
    monkeypatch.setattr(reg, "user_plugins_dir", lambda: str(tmp_path / "plugins"))
    path = reg.ensure_user_plugins_dir()
    assert os.path.isdir(path)
    readme = os.path.join(path, "README.md")
    assert "plugin.json" in open(readme, encoding="utf-8").read()


def test_ensure_user_plugins_dir_is_idempotent(tmp_path, monkeypatch):
    monkeypatch.setattr(reg, "user_plugins_dir", lambda: str(tmp_path / "plugins"))
    path = reg.ensure_user_plugins_dir()
    readme = os.path.join(path, "README.md")
    with open(readme, "w", encoding="utf-8") as fh:
        fh.write("edited by the operator")
    reg.ensure_user_plugins_dir()
    assert open(readme, encoding="utf-8").read() == "edited by the operator"


# ---------------------------------------------------------------------------
# The plugins that ship with Corvus
# ---------------------------------------------------------------------------

_ROOT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("corvus_bundle_plugins_tool", _ROOT / "tools" / "bundle_plugins.py")
bundle_plugins = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bundle_plugins)

# What a release carries: the plugin folders .gitignore lets back in.
SHIPPED = bundle_plugins.shipped_plugin_ids(_ROOT / ".gitignore") or []


def _bundled() -> list[dict]:
    return [p for p in reg.discover(user_dir=os.path.join(str(os.devnull), "none"))
            if p["source"] == "bundled"]


@pytest.mark.parametrize("plugin_id", SHIPPED)
def test_bundled_plugin_is_discoverable(plugin_id):
    """Every shipped plugin parses through the same code path a dropped-in
    one does — that path being the same one is the whole point. A plugin
    that is not in this checkout has nothing to check."""
    if not (_ROOT / "plugins" / plugin_id / "plugin.json").is_file():
        pytest.skip(f"plugins/{plugin_id} is not in this checkout")
    found = {p["id"]: p for p in _bundled()}
    assert plugin_id in found
    plugin = found[plugin_id]
    assert plugin["scripts"], plugin_id
    for name in plugin["scripts"] + plugin["styles"]:
        assert (_ROOT / "plugins" / plugin_id / name).is_file(), (plugin_id, name)


def test_bundled_plugins_keep_the_grid_order_they_declare():
    """Vibration first, then the launcher — the order the TOOLS tab had before
    either of them lived in a folder — then Schwalby. Only the ones in this
    checkout are compared, and a plugin installed into plugins/ besides them
    takes the place its own order gives it."""
    found = [p["id"] for p in _bundled()]
    assert [i for i in found if i in ("vibration", "ssh-launcher", "schwalby")] == \
        [i for i in ("vibration", "ssh-launcher", "schwalby") if i in found]
    orders = [(p["order"], p["name"].lower()) for p in _bundled()]
    assert orders == sorted(orders)


def test_gitignore_keeps_every_plugin_out_but_the_shipped_ones():
    """`.gitignore` keeps operator plugins out of the repository by ignoring
    every folder under plugins/ and letting the shipped ones back in, and the
    build ships exactly that list (tools/bundle_plugins.py). The NuttX Console
    and the Trajectory Viewer are installed separately, from their own
    repositories, so they are not on it."""
    rules = (_ROOT / ".gitignore").read_text(encoding="utf-8").splitlines()
    assert "/plugins/*/" in [r.strip() for r in rules]
    assert SHIPPED, "no plugin ships with Corvus"
    assert "nuttx-console" not in SHIPPED
    assert "trajectory-viewer" not in SHIPPED


def test_a_file_name_that_starts_with_a_dot_keeps_it(tmp_path):
    """Only a leading "./" is dropped, never the dot of a name."""
    from corvus import plugin_registry
    folder = tmp_path / "dotty"
    (folder / "lib").mkdir(parents=True)
    (folder / ".build.js").write_text("//", encoding="utf-8")
    (folder / "lib" / "x.js").write_text("//", encoding="utf-8")
    (folder / "plugin.json").write_text(
        '{"id": "dotty", "scripts": ["./.build.js", "././lib/x.js", ".\\\\lib\\\\x.js"]}',
        encoding="utf-8")
    manifests = plugin_registry.discover(user_dir=str(tmp_path), bundled_dir=str(tmp_path / "none"))
    assert manifests[0]["scripts"] == [".build.js", "lib/x.js"]
