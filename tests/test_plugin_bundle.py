"""Plugins in a release and plugin tests in the suite.

``tools/bundle_plugins.py`` puts the shipped plugins into every artifact:
the ones ``.gitignore`` lets back in, without their ``tests/``. A plugin that
is only on the build machine stays there, and a shipped one that is missing
is skipped rather than failing the build.

Each plugin keeps its own tests in ``plugins/<id>/tests/``. pytest and
``tools/frontend_tests.js`` both look there, and a plugin that is not
installed simply has no tests to run.
"""
from __future__ import annotations

import importlib.util
import json
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent

_spec = importlib.util.spec_from_file_location("corvus_bundle_plugins_tool", ROOT / "tools" / "bundle_plugins.py")
tool = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tool)

GITIGNORE = """# plugins besides the ones that ship with Corvus
/plugins/*/
!/plugins/alpha/
!/plugins/beta/
"""


def _plugin(root: Path, plugin_id: str) -> Path:
    folder = root / plugin_id
    (folder / "tests").mkdir(parents=True)
    (folder / "plugin.json").write_text(json.dumps({"id": plugin_id}), encoding="utf-8")
    (folder / f"{plugin_id}.js").write_text("//", encoding="utf-8")
    (folder / "tests" / f"test_{plugin_id}.js").write_text("//", encoding="utf-8")
    return folder


@pytest.fixture
def tree(tmp_path: Path) -> tuple[Path, Path]:
    source = tmp_path / "plugins"
    _plugin(source, "alpha")
    _plugin(source, "beta")
    _plugin(source, "local-only")
    gitignore = tmp_path / ".gitignore"
    gitignore.write_text(GITIGNORE, encoding="utf-8")
    return source, gitignore


# ---------------------------------------------------------------------------
# tools/bundle_plugins.py
# ---------------------------------------------------------------------------

def test_the_shipped_list_is_what_gitignore_lets_back_in(tree):
    _, gitignore = tree
    assert tool.shipped_plugin_ids(gitignore) == ["alpha", "beta"]


def test_without_a_gitignore_there_is_no_list(tmp_path):
    assert tool.shipped_plugin_ids(tmp_path / ".gitignore") is None


def test_only_shipped_plugins_are_bundled(tree, tmp_path):
    source, gitignore = tree
    out = tmp_path / "out"
    assert tool.bundle(source, out, gitignore) == ["alpha", "beta"]
    assert sorted(p.name for p in out.iterdir()) == ["alpha", "beta"]


def test_a_plugins_tests_stay_behind(tree, tmp_path):
    source, gitignore = tree
    (source / "alpha" / "__pycache__").mkdir()
    (source / "alpha" / "lib").mkdir()
    (source / "alpha" / "lib" / ".build.js").write_text("//", encoding="utf-8")
    out = tmp_path / "out"
    tool.bundle(source, out, gitignore)
    assert not (out / "alpha" / "tests").exists()
    assert not (out / "alpha" / "__pycache__").exists()
    assert (out / "alpha" / "alpha.js").is_file()
    assert (out / "alpha" / "plugin.json").is_file()
    assert (out / "alpha" / "lib" / ".build.js").is_file()


def test_a_missing_shipped_plugin_does_not_fail_the_build(tree, tmp_path, capsys):
    source, gitignore = tree
    shutil.rmtree(source / "beta")
    assert tool.bundle(source, tmp_path / "out", gitignore) == ["alpha"]
    assert "beta" in capsys.readouterr().err


def test_no_plugins_folder_at_all_bundles_nothing(tmp_path):
    (tmp_path / ".gitignore").write_text(GITIGNORE, encoding="utf-8")
    out = tmp_path / "out"
    assert tool.bundle(tmp_path / "plugins", out, tmp_path / ".gitignore") == []
    assert out.is_dir()


def test_without_a_gitignore_every_plugin_present_ships(tree, tmp_path):
    source, _ = tree
    assert tool.bundle(source, tmp_path / "out", tmp_path / "none") == ["alpha", "beta", "local-only"]


def test_the_destination_is_replaced_not_merged(tree, tmp_path):
    source, gitignore = tree
    out = tmp_path / "out"
    (out / "stale").mkdir(parents=True)
    tool.bundle(source, out, gitignore)
    assert not (out / "stale").exists()


@pytest.mark.parametrize("script", ["build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"])
def test_every_build_bundles_plugins_through_the_tool(script):
    """One rule for all three artifacts. Copying plugins/ wholesale would ship
    whatever happens to be installed on the build machine, tests included."""
    text = (ROOT / script).read_text(encoding="utf-8")
    assert "bundle_plugins.py" in text
    assert not re.search(r"""rsync[^\n]*REPO_DIR/plugins/""", text)
    assert "'plugins');plugins" not in text


# ---------------------------------------------------------------------------
# Plugin tests live with the plugin
# ---------------------------------------------------------------------------

def test_pytest_collects_the_plugin_folders():
    text = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    assert re.search(r'^testpaths = \[[^\]]*"plugins"', text, re.M)


def test_ci_runs_the_plugin_tests_too():
    ci = (ROOT / ".github" / "workflows" / "build.yml").read_text(encoding="utf-8")
    assert "node tools/frontend_tests.js" in ci
    assert "pytest -q tests" not in ci
    assert "ruff check corvus serve.py tests plugins" in ci


@pytest.mark.parametrize("plugin_id", tool.shipped_plugin_ids(ROOT / ".gitignore") or [])
def test_every_shipped_plugin_brings_its_own_tests(plugin_id):
    folder = ROOT / "plugins" / plugin_id
    if not folder.is_dir():
        pytest.skip(f"plugins/{plugin_id} is not in this checkout")
    assert any((folder / "tests").glob("test_*")), f"plugins/{plugin_id}/tests/ has no tests"


def test_no_core_test_needs_a_plugin():
    """A test in tests/ that loads a plugin's files fails the moment that
    plugin is not installed. Those checks belong in the plugin's own folder."""
    plugin_ids = [p.name for p in (ROOT / "plugins").iterdir() if p.is_dir()] if (ROOT / "plugins").is_dir() else []
    plugin_ids += tool.shipped_plugin_ids(ROOT / ".gitignore") or []
    offenders = []
    for test in sorted((ROOT / "tests").glob("*")):
        if test.suffix not in (".js", ".py") or test.name == Path(__file__).name:
            continue
        text = test.read_text(encoding="utf-8")
        for plugin_id in set(plugin_ids):
            if re.search(rf"""plugins["'/\\, ]+{re.escape(plugin_id)}["'/\\]""", text):
                offenders.append(f"{test.name}: {plugin_id}")
    assert offenders == []


def _node() -> str:
    node = shutil.which("node")
    if node is None:
        pytest.skip("node is not installed")
    return node


def test_the_frontend_runner_finds_core_and_plugin_tests():
    out = subprocess.run([_node(), str(ROOT / "tools" / "frontend_tests.js"), "--list"],
                         capture_output=True, text=True, check=True).stdout.split()
    listed = [Path(f).as_posix() for f in out]
    assert "tests/test_frontend_plugins.js" in listed
    for plugin_id in tool.shipped_plugin_ids(ROOT / ".gitignore") or []:
        if (ROOT / "plugins" / plugin_id / "tests").is_dir():
            assert any(f.startswith(f"plugins/{plugin_id}/tests/") for f in listed), plugin_id


def test_the_frontend_runner_is_fine_without_plugins(tmp_path):
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "test_a.js").write_text("//", encoding="utf-8")
    _plugin(tmp_path / "plugins", "alpha")
    (tmp_path / "plugins" / "bare").mkdir()
    script = (
        "const { testFiles } = require(process.argv[1]);"
        "const path = require('node:path');"
        "console.log(JSON.stringify(testFiles(process.argv[2]).map((f) => path.relative(process.argv[2], f))));"
    )
    run = subprocess.run([_node(), "-e", script, str(ROOT / "tools" / "frontend_tests.js"), str(tmp_path)],
                         capture_output=True, text=True, check=True)
    assert [Path(f).as_posix() for f in json.loads(run.stdout)] == ["tests/test_a.js", "plugins/alpha/tests/test_alpha.js"]
    shutil.rmtree(tmp_path / "plugins")
    run = subprocess.run([_node(), "-e", script, str(ROOT / "tools" / "frontend_tests.js"), str(tmp_path)],
                         capture_output=True, text=True, check=True)
    assert json.loads(run.stdout) == ["tests/test_a.js"]
