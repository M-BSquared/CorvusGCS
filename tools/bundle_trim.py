#!/usr/bin/env python3
"""Take out of a packaged build what the wheels ship and Corvus never reads.

``tools/qt_bundle.py`` decides which Qt modules stay. This takes the rest:

* **pymavlink dialects.** pymavlink ships about twenty generated dialects
  twice, once for MAVLink 1 and once for MAVLink 2, with their XML beside
  them: about 85 MB. ``mavutil`` loads ``$MAVLINK_DIALECT``, which it sets to ``all``
  itself, and Corvus never names another (corvus/app.py pins it), so
  ``v10/all.py`` and ``v20/all.py`` are the only two ever imported.
* **lxml.** A pymavlink dependency used only to validate XML while generating
  a dialect and to read a DataFlash help file downloaded by MAVExplorer.
  Neither happens in Corvus, and mavgen degrades to a warning without it.
* **pip.** Installs the bundle and is never run from it.
* **Qt translations.** ``.qm`` files load only through a ``QTranslator``,
  which Corvus does not install.
* **Chromium locales.** QtWebEngine ships 53 locale packs for its own context
  menus and form messages. Chromium falls back to ``en-US`` when the system
  locale has no pack, so that one is required and stays, with ``de``.
* **Chromium DevTools.** ``qtwebengine_devtools_resources.pak`` (12 MB) is
  the web inspector, which Corvus never opens: it sets no remote debugging
  port and shows no native context menu to inspect from.
* **The other architecture's V8 snapshot (macOS).** The framework carries
  ``v8_context_snapshot.arm64.bin`` and ``.x86_64.bin``; a bundle built for
  one architecture reads only its own.
* **Type stubs.** PySide6's ``.pyi`` files are for editors.
* **Duplicated frameworks (macOS).** A wheel cannot hold a symlink, so pip
  writes ``X.framework/Resources`` and ``Helpers`` out as full copies of
  ``Versions/A/...``. For QtWebEngineCore that is 70 MB twice. Each copy that
  is byte-identical to its twin becomes the symlink the framework layout
  expects.

Every step finds its targets by name under the folder it is given, so the
same call works on a venv's ``site-packages`` (AppImage, ``.app``) and on a
PyInstaller ``_internal`` folder (Windows). The last step checks that what
the app needs at runtime is still there, and fails the build if not.

    python3 tools/bundle_trim.py <site-packages>
    python3 tools/bundle_trim.py --dry-run <site-packages>

stdlib only.
"""
from __future__ import annotations

import argparse
import filecmp
import os
import platform
import shutil
import sys
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path

# The dialect mavutil loads; corvus/app.py pins MAVLINK_DIALECT to it.
MAVLINK_DIALECTS: frozenset[str] = frozenset({"all"})

# Chromium needs en-US.pak as its fallback; de is the operators' own.
KEEP_LOCALES: frozenset[str] = frozenset({"en-US", "de"})

DROP_PACKAGES: tuple[str, ...] = ("lxml", "pip")

DEVTOOLS_PAK = "qtwebengine_devtools_resources.pak"

# The architecture the bundle is built for; every build script builds for
# the host it runs on.
HOST_ARCH = platform.machine()


class TrimError(RuntimeError):
    """The trimmed bundle would miss something the app loads."""


@dataclass
class Plan:
    """Paths to delete and framework entries to turn into symlinks."""

    remove: list[Path] = field(default_factory=list)
    link: list[tuple[Path, str]] = field(default_factory=list)


def _dirs_named(root: Path, name: str) -> Iterator[Path]:
    for dirpath, dirnames, _files in os.walk(root):
        if name in dirnames:
            yield Path(dirpath) / name


def _pycache_of(source: Path) -> list[Path]:
    cache = source.parent / "__pycache__"
    if not cache.is_dir():
        return []
    return sorted(cache.glob(f"{source.stem}.*.pyc"))


def plan_dialects(root: Path) -> list[Path]:
    """Every pymavlink dialect module and XML file except the ones mavutil loads."""
    out: list[Path] = []
    for dialects in _dirs_named(root, "dialects"):
        if dialects.parent.name != "pymavlink":
            continue
        for wire in ("v10", "v20"):
            folder = dialects / wire
            if not folder.is_dir():
                continue
            for entry in sorted(folder.iterdir()):
                if not entry.is_file() or entry.suffix not in (".py", ".xml"):
                    continue
                if entry.stem == "__init__" or entry.stem in MAVLINK_DIALECTS:
                    continue
                out.append(entry)
                if entry.suffix == ".py":
                    out += _pycache_of(entry)
    return out


def plan_packages(root: Path) -> list[Path]:
    """The packages in DROP_PACKAGES, with their dist-info."""
    out: list[Path] = []
    for name in DROP_PACKAGES:
        package = root / name
        if package.is_dir():
            out.append(package)
        out += sorted(p for p in root.glob(f"{name}-*.dist-info") if p.is_dir())
    return out


def _foreign_snapshots(folder: Path, files: list[str], arch: str) -> list[Path]:
    """V8 snapshots for another architecture, when this one's is beside them."""
    if f"v8_context_snapshot.{arch}.bin" not in files:
        return []
    return [folder / f for f in sorted(files)
            if f.startswith("v8_context_snapshot.") and f.endswith(".bin")
            and f != f"v8_context_snapshot.{arch}.bin" and f.count(".") == 2]


def plan_qt_data(root: Path, arch: str = HOST_ARCH) -> list[Path]:
    """Qt translations, Chromium locales and DevTools, foreign V8 snapshots, type stubs."""
    out: list[Path] = []
    pyside = root / "PySide6"
    if not pyside.is_dir():
        return out
    out += sorted(pyside.glob("*.pyi"))
    for dirpath, _dirs, files in os.walk(pyside):
        here = Path(dirpath)
        if here.name == "qtwebengine_locales":
            out += [here / f for f in sorted(files)
                    if f.endswith(".pak") and f[:-4] not in KEEP_LOCALES]
        elif here.name == "translations":
            out += [here / f for f in sorted(files) if f.endswith(".qm")]
        if DEVTOOLS_PAK in files and not (here / DEVTOOLS_PAK).is_symlink():
            out.append(here / DEVTOOLS_PAK)
        out += [f for f in _foreign_snapshots(here, files, arch) if not f.is_symlink()]
    return out


def _same_tree(a: Path, b: Path) -> bool:
    if a.is_symlink() or b.is_symlink():
        return False
    if a.is_file() and b.is_file():
        return filecmp.cmp(a, b, shallow=False)
    if not (a.is_dir() and b.is_dir()):
        return False
    names_a = sorted(p.name for p in a.iterdir())
    if names_a != sorted(p.name for p in b.iterdir()):
        return False
    return all(_same_tree(a / n, b / n) for n in names_a)


def plan_frameworks(root: Path) -> list[tuple[Path, str]]:
    """Top-level framework entries that duplicate ``Versions/<one>/`` byte for byte."""
    out: list[tuple[Path, str]] = []
    for dirpath, dirnames, _files in os.walk(root):
        for name in sorted(dirnames):
            if not name.endswith(".framework"):
                continue
            framework = Path(dirpath) / name
            versions = framework / "Versions"
            real = [v for v in versions.iterdir()
                    if v.is_dir() and not v.is_symlink()] if versions.is_dir() else []
            if len(real) != 1:
                continue
            current = real[0]
            for entry in sorted(framework.iterdir()):
                if entry.name == "Versions" or entry.is_symlink():
                    continue
                twin = current / entry.name
                if twin.exists() and _same_tree(entry, twin):
                    out.append((entry, f"Versions/Current/{entry.name}"))
        dirnames[:] = [d for d in dirnames if not d.endswith(".framework")]
    return out


def plan(root: Path, arch: str = HOST_ARCH) -> Plan:
    """Decide what goes under *root*. Touches nothing."""
    result = Plan()
    result.remove = sorted(set(plan_dialects(root) + plan_packages(root) + plan_qt_data(root, arch)))
    result.link = plan_frameworks(root)
    return result


def check(root: Path) -> None:
    """Fail when something the app loads at runtime is gone."""
    missing: list[str] = []
    for dialects in _dirs_named(root, "dialects"):
        if dialects.parent.name != "pymavlink":
            continue
        for wire in ("v10", "v20"):
            folder = dialects / wire
            # PyInstaller keeps the modules in its archive and only the XML
            # here; that layout has nothing to check.
            if not folder.is_dir() or not any(folder.glob("*.py")):
                continue
            for dialect in MAVLINK_DIALECTS:
                if not (folder / f"{dialect}.py").is_file():
                    missing.append((folder / f"{dialect}.py").relative_to(root).as_posix())
    for locales in _dirs_named(root, "qtwebengine_locales"):
        if not (locales / "en-US.pak").is_file():
            missing.append(str((locales / "en-US.pak").relative_to(root)))
    for dirpath, dirnames, _files in os.walk(root):
        for name in dirnames:
            if not name.endswith(".framework"):
                continue
            framework = Path(dirpath) / name
            for entry in framework.iterdir():
                if entry.is_symlink() and not entry.exists():
                    missing.append(f"{entry.relative_to(root)} (dangling link)")
    if missing:
        raise TrimError("the trimmed bundle lacks:\n  " + "\n  ".join(sorted(missing)))


def _size(path: Path) -> int:
    if path.is_symlink() or path.is_file():
        return path.lstat().st_size
    return sum(p.lstat().st_size for p in path.rglob("*") if p.is_file() or p.is_symlink())


def _link(entry: Path, target: str) -> None:
    current = entry.parent / "Versions" / "Current"
    if not current.is_symlink():
        real = next(v for v in (entry.parent / "Versions").iterdir() if v.is_dir())
        current.symlink_to(real.name)
    if entry.is_dir():
        shutil.rmtree(entry)
    else:
        entry.unlink()
    entry.symlink_to(target)


def trim(root: Path, dry_run: bool = False, log: Callable[[str], None] = print) -> Plan:
    """Trim *root* in place and check that the runtime still has what it loads."""
    root = root.resolve()
    if not root.is_dir():
        raise TrimError(f"{root} is not a folder")
    decided = plan(root)
    for path in decided.remove + [entry for entry, _ in decided.link]:
        if root not in path.parents:
            raise TrimError(f"refusing to touch {path}, outside {root}")
    freed = sum(_size(p) for p in decided.remove) + sum(_size(e) for e, _ in decided.link)
    if dry_run:
        for path in decided.remove:
            log(f"    would remove {path.relative_to(root).as_posix()}")
        for entry, target in decided.link:
            log(f"    would link   {entry.relative_to(root).as_posix()} -> {target}")
    else:
        for path in decided.remove:
            if path.is_dir() and not path.is_symlink():
                shutil.rmtree(path)
            elif path.exists() or path.is_symlink():
                path.unlink()
        for entry, target in decided.link:
            _link(entry, target)
        check(root)
    log(f"    trimmed    : {len(decided.remove)} removed, {len(decided.link)} linked,"
        f" {freed >> 20} MB {'to free' if dry_run else 'freed'}")
    return decided


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="bundle_trim.py", description=__doc__.split("\n\n")[0])
    parser.add_argument("root", type=Path, help="the bundle's site-packages (or _internal)")
    parser.add_argument("--dry-run", action="store_true", help="list, change nothing")
    args = parser.parse_args(argv)
    try:
        trim(args.root, dry_run=args.dry_run)
    except TrimError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
