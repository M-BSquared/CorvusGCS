"""What a release carries from the repository, and what it leaves behind.

The README and website pictures (``assets/screenshot_*``, the unused artwork),
``docs/`` (the website) and ``tools/`` (the scene scripts and build helpers)
are for the repository: no build script may copy ``docs/`` or ``tools/``.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
BUILDS = ["build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"]

@pytest.mark.parametrize("script", BUILDS)
def test_no_build_copies_the_website_or_the_scene_scripts(script):
    text = (ROOT.parent / script).read_text(encoding="utf-8")
    for line in text.splitlines():
        if re.search(r"\b(rsync|cp|copytree)\b|--add-data", line):
            assert not re.search(r"""[/\\]docs\b|[/\\]tools\b(?![/\\]\w+\.py)|'docs'|'tools'""", line), line
