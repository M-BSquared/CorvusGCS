#!/usr/bin/env python3
"""Check that a packaged build holds no Python.

Corvus GCS ships as one native program with its backend (``corvus_core``)
inside it. A bundle (AppDir, ``.app``, Windows folder) must therefore carry
no interpreter, no package, no PyO3 extension and no backend script. This
walks a finished bundle and fails, naming every offender, when it finds:

* a Python interpreter or its library (``python``, ``python3``, ``python312.dll``,
  ``libpython*``, ``Python.framework``, ``Python.app``, ``pyvenv.cfg``),
* packages (``site-packages``, ``pip``, ``lib-dynload``, ``PySide*``,
  ``corvus_link*``, ``*.pyd``),
* Python sources and bytecode (``*.py``, ``*.pyc``),
* a native file that was built against Python (it exports ``PyInit_`` or
  ``Py_Initialize``, which is what ``corvus_link`` built with its ``python``
  feature does).

``--require`` names files that must be there (the executable, ``VERSION``,
``LICENSE.md``); a bundle that lost one fails too.

    python3 tools/bundle_trim.py check <bundle> [--require REL ...]

stdlib only. The build scripts make the same check inline, so a build host
without Python still refuses a bad bundle; this is the one the smoke test and CI
run on the finished artifact.
"""
from __future__ import annotations

import argparse
import fnmatch
import sys
from collections.abc import Iterator
from pathlib import Path

FORBIDDEN_NAMES: tuple[str, ...] = (
    "python", "python[0-9]*", "pythonw*", "libpython*", "Python", "Python.framework", "Python.app",
    "pyvenv.cfg", "site-packages", "dist-packages", "lib-dynload", "pip", "pip3*",
    "PySide*", "corvus_link*", "*.pyd", "*.py", "*.pyc", "*.pyo", "serve.py",
)

PYTHON_SYMBOLS: tuple[bytes, ...] = (b"PyInit_", b"Py_Initialize", b"Py_InitializeEx")

NATIVE_SUFFIXES = (".so", ".dylib", ".dll", ".exe", ".node")

_CHUNK = 1 << 20


def walk(root: Path) -> Iterator[Path]:
    """Every file and folder under ``root``, symlinks not followed."""
    yield from sorted(root.rglob("*"))


def names_python(path: Path) -> bool:
    """Whether the file or folder name is one a Python install would carry."""
    return any(fnmatch.fnmatchcase(path.name, pattern) for pattern in FORBIDDEN_NAMES)


def is_native(path: Path) -> bool:
    """Whether the file may be a compiled binary (by suffix, or the executable bit with no suffix)."""
    if not path.is_file() or path.is_symlink():
        return False
    if path.suffix.lower() in NATIVE_SUFFIXES:
        return True
    return path.suffix == "" and path.stat().st_mode & 0o111 != 0


def built_against_python(path: Path) -> bool:
    """Whether a compiled file carries the symbols of a Python extension or embedder."""
    keep = max(len(s) for s in PYTHON_SYMBOLS) - 1
    tail = b""
    with path.open("rb") as fh:
        while True:
            block = fh.read(_CHUNK)
            if not block:
                return False
            data = tail + block
            if any(symbol in data for symbol in PYTHON_SYMBOLS):
                return True
            tail = data[-keep:]


def check(root: Path, require: tuple[str, ...] = ()) -> list[str]:
    """The problems with the bundle at ``root``: empty when it holds no Python."""
    if not root.is_dir():
        return [f"{root} is not a folder"]
    problems: list[str] = []
    for path in walk(root):
        rel = path.relative_to(root)
        if names_python(path):
            problems.append(f"{rel}: a Python file or folder")
        elif is_native(path) and built_against_python(path):
            problems.append(f"{rel}: built against Python (PyO3 or an embedded interpreter)")
    for needed in require:
        if not (root / needed).exists():
            problems.append(f"{needed}: required and missing")
    return problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="bundle_trim.py", description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    check_parser = sub.add_parser("check", help="fail when the bundle holds Python")
    check_parser.add_argument("bundle", type=Path)
    check_parser.add_argument("--require", nargs="*", default=[], metavar="REL")
    args = parser.parse_args(argv)
    problems = check(args.bundle, tuple(args.require))
    if problems:
        print(f"ERROR: {args.bundle} is not a Python free bundle:", file=sys.stderr)
        for problem in problems:
            print(f"    {problem}", file=sys.stderr)
        return 1
    print(f"    {args.bundle.name}: no Python inside")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
