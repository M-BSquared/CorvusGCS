"""What a release carries from the repository, and what it leaves behind.

The README and website pictures (``assets/screenshot_*``, the unused artwork),
``docs/`` (the website) and ``tools/`` (the scene scripts and build helpers)
are for the repository. ``tools/bundle_assets.py`` ships only what the app
reads, and no build script may copy ``docs/`` or ``tools/``.
"""
from __future__ import annotations

import importlib.util
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
BUILDS = ["build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"]

_spec = importlib.util.spec_from_file_location("corvus_bundle_assets_tool", ROOT / "tools" / "bundle_assets.py")
tool = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tool)


def test_a_release_carries_only_the_logos_and_the_licences(tmp_path):
    out = tmp_path / "assets"
    tool.bundle(ROOT / "assets", out)
    assert sorted(p.name for p in out.iterdir()) == sorted(
        ["CorvusGCS_logo.png", "CorvusGCS_logo_inverted.png", "unibw_logo.png", "licenses"]
    )
    assert (out / "licenses" / "LGPL-3.0.txt").is_file()


def test_no_screenshot_reaches_the_bundle(tmp_path):
    out = tmp_path / "assets"
    tool.bundle(ROOT / "assets", out)
    assert not [p for p in out.rglob("*") if p.name.startswith("screenshot_")]


def test_an_unlisted_file_stays_out(tmp_path):
    source = tmp_path / "src"
    (source / "licenses").mkdir(parents=True)
    for name in ("CorvusGCS_logo.png", "CorvusGCS_logo_inverted.png", "unibw_logo.png", "extra.png"):
        (source / name).write_bytes(b"x")
    out = tmp_path / "out"
    tool.bundle(source, out)
    assert not (out / "extra.png").exists()


def test_a_missing_shipped_file_fails_the_build(tmp_path):
    (tmp_path / "src").mkdir()
    with pytest.raises(FileNotFoundError):
        tool.bundle(tmp_path / "src", tmp_path / "out")


def test_the_icon_code_finds_its_logos_in_the_bundle():
    text = (ROOT / "corvus" / "app_icon.py").read_text(encoding="utf-8")
    for name in re.findall(r'"(CorvusGCS_logo[\w]*\.png)"', text):
        assert name in tool.SHIPPED, name


@pytest.mark.parametrize("script", BUILDS)
def test_every_build_ships_assets_through_the_tool(script):
    text = (ROOT / script).read_text(encoding="utf-8")
    assert "bundle_assets.py" in text
    assert not re.search(r"""rsync[^\n]*REPO_DIR/assets/""", text)
    assert "RepoDir 'assets');assets" not in text


@pytest.mark.parametrize("script", BUILDS)
def test_no_build_copies_the_website_or_the_scene_scripts(script):
    text = (ROOT / script).read_text(encoding="utf-8")
    for line in text.splitlines():
        if re.search(r"\b(rsync|cp|copytree)\b|--add-data", line):
            assert not re.search(r"""[/\\]docs\b|[/\\]tools\b(?![/\\]\w+\.py)|'docs'|'tools'""", line), line
