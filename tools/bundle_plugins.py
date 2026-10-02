#!/usr/bin/env python3
"""Copy the plugins that ship with Corvus into a packaged build.

``plugins/`` in a checkout can hold more than ships: a plugin installed from
its own repository for development, or one being written. What ships is what
``.gitignore`` lets back in after ignoring every plugin folder::

    /plugins/*/
    !/plugins/schwalby/

so the list a release carries is the list the repository tracks, and a
plugin that only sits on the build machine stays there. Each plugin's
``tests/`` folder stays behind too: the checks are for the repository, not
for the operator.

A shipped plugin whose folder is missing is reported and skipped, never a
failed build. Without a ``.gitignore`` (a source copy without one) every
plugin folder present ships, since there is nothing to tell them apart.

    python3 tools/bundle_plugins.py <destination>
    python3 tools/bundle_plugins.py --source <plugins dir> <destination>

All three build scripts call this, so the rule lives in one place.

stdlib only.
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

LEFT_BEHIND = {"tests", "__pycache__", ".git", ".github", ".gitignore", ".DS_Store"}


def shipped_plugin_ids(gitignore: Path) -> list[str] | None:
    """The plugin ids ``gitignore`` lets back in, or None without the file."""
    if not gitignore.is_file():
        return None
    ids = []
    for line in gitignore.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line.startswith("!/plugins/") and line.endswith("/"):
            plugin_id = line[len("!/plugins/"):-1]
            if plugin_id and "/" not in plugin_id:
                ids.append(plugin_id)
    return ids


def bundle(source: Path, destination: Path, gitignore: Path) -> list[str]:
    """Replace ``destination`` with the shipped plugins from ``source``.

    Returns the ids that were copied, in name order.
    """
    shipped = shipped_plugin_ids(gitignore)
    present = sorted(p.name for p in source.iterdir() if (p / "plugin.json").is_file()) if source.is_dir() else []
    wanted = present if shipped is None else sorted(shipped)

    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)

    copied = []
    for plugin_id in wanted:
        folder = source / plugin_id
        if not (folder / "plugin.json").is_file():
            print(f"    plugin {plugin_id}: not in {source}, not bundled", file=sys.stderr)
            continue

        def leave_out(directory: str, names: list[str], top: str = str(folder)) -> set[str]:
            return {n for n in names if n in LEFT_BEHIND} if directory == top else {"__pycache__"} & set(names)

        shutil.copytree(folder, destination / plugin_id, ignore=leave_out)
        copied.append(plugin_id)
    return copied


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="bundle_plugins.py", description=__doc__.split("\n\n")[0])
    parser.add_argument("destination", type=Path)
    parser.add_argument("--source", type=Path, default=ROOT / "plugins")
    parser.add_argument("--gitignore", type=Path, default=ROOT / ".gitignore")
    args = parser.parse_args(argv)
    copied = bundle(args.source, args.destination, args.gitignore)
    print(f"    plugins: {', '.join(copied) if copied else 'none'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
