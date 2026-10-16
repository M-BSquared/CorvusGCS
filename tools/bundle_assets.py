#!/usr/bin/env python3
"""Copy the part of ``assets/`` that the running app reads into a packaged build.

``assets/`` in the repository also holds the screenshots and artwork made for
the README and the website. None of that is read at runtime, so a release
carries an allow-list instead of the folder:

* the Corvus GCS logo, both cuts (``corvus/app_icon.py`` builds the Dock and
  taskbar icon from them),
* the university logo,
* ``licenses/`` (the LGPL and GPL texts the Qt licence terms require to travel
  with the software).

A new file in ``assets/`` stays out of every release until it is added to
``SHIPPED`` here, which is the safe direction to be wrong in. A shipped file
that is missing fails the build.

``docs/`` (the website) and ``tools/`` (the scene scripts) are never copied by
any build; ``tests/test_bundle_contents.py`` pins that.

    python3 tools/bundle_assets.py <destination>
    python3 tools/bundle_assets.py --source <assets dir> <destination>

All three build scripts call this, so the rule lives in one place.

stdlib only.
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

SHIPPED = (
    "CorvusGCS_logo.png",
    "CorvusGCS_logo_inverted.png",
    "unibw_logo.png",
    "licenses",
)


def bundle(source: Path, destination: Path) -> list[str]:
    """Replace ``destination`` with the shipped assets from ``source``.

    Returns the names that were copied. Raises ``FileNotFoundError`` when a
    shipped entry is missing from ``source``.
    """
    missing = [name for name in SHIPPED if not (source / name).exists()]
    if missing:
        raise FileNotFoundError(f"assets missing from {source}: {', '.join(missing)}")
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    for name in SHIPPED:
        item = source / name
        if item.is_dir():
            shutil.copytree(item, destination / name, ignore=shutil.ignore_patterns(".DS_Store"))
        else:
            shutil.copy2(item, destination / name)
    return list(SHIPPED)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="bundle_assets.py", description=__doc__.split("\n\n")[0])
    parser.add_argument("destination", type=Path)
    parser.add_argument("--source", type=Path, default=ROOT / "assets")
    args = parser.parse_args(argv)
    try:
        copied = bundle(args.source, args.destination)
    except FileNotFoundError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    print(f"    assets: {', '.join(copied)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
