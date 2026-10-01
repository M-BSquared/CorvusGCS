"""The Credits dialog names every Python package the backend ships.

pyproject.toml's ``headless`` group names three packages, and those pull in
eight more. A dependency added there, or a new transitive one, would ship in
every bundle without a credit, so the whole installed tree is checked against
src/js/credits.js rather than a hand-kept list.
"""
from __future__ import annotations

import importlib.metadata as metadata
import re
import tomllib
from pathlib import Path

from packaging.requirements import Requirement

ROOT = Path(__file__).resolve().parent.parent
CREDITS = ROOT / "src" / "js" / "credits.js"


def _norm(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def _headless_roots() -> list[str]:
    groups = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))["dependency-groups"]
    return [Requirement(r).name for r in groups["headless"] if isinstance(r, str)]


def _installed_tree(roots: list[str]) -> set[str]:
    seen: set[str] = set()
    todo = list(roots)
    while todo:
        name = todo.pop()
        try:
            dist = metadata.distribution(name)
        except metadata.PackageNotFoundError:
            continue
        key = _norm(dist.metadata["Name"])
        if key in seen:
            continue
        seen.add(key)
        for spec in dist.requires or []:
            req = Requirement(spec)
            if req.marker is None or req.marker.evaluate({"extra": ""}):
                todo.append(req.name)
    return seen


def test_every_shipped_python_package_is_credited() -> None:
    text = _norm(CREDITS.read_text(encoding="utf-8"))
    tree = _installed_tree(_headless_roots())
    assert {"pymavlink", "paramiko", "pyserial"} <= tree, tree
    missing = sorted(name for name in tree if not re.search(rf"\b{re.escape(name)}\b", text))
    assert not missing, f"shipped but not in src/js/credits.js: {missing}"


def test_the_lgpl_2_1_text_ships_for_paramiko() -> None:
    text = (ROOT / "assets" / "licenses" / "LGPL-2.1.txt").read_text(encoding="utf-8")
    assert text.lstrip().startswith("GNU LESSER GENERAL PUBLIC LICENSE")
    assert "Version 2.1, February 1999" in text
    licence = metadata.distribution("paramiko").metadata
    assert "2.1" in (licence.get("License-Expression") or licence.get("License") or "")
