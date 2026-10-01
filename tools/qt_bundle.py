#!/usr/bin/env python3
"""Trim the PySide6 wheels inside a packaged build to the Qt that Corvus loads.

PySide6 ships every Qt module in two wheels: Qt3D, Charts, Multimedia, Quick3D
and fifty more, the Designer, Assistant and Linguist tools, and the headers and
type data for building bindings. Corvus imports six modules. Left in, the rest
roughly doubles the Qt part of an AppImage or a .app for nothing.

What stays is read from the binaries themselves, not from a list of Qt
libraries. The starting points are the Python modules ``corvus/app.py``
imports, the Qt plugin folders a widget application with a web view loads
from, and QtWebEngine's helper process. Every library they link is kept, and
every library those link, to the end. A Qt release that adds a dependency is
followed without an edit here; a module nothing links is dropped. A kept Qt
library keeps its Python module too, because PySide6 modules import each other
at load time (``QtWebEngineWidgets`` imports ``QtPrintSupport``).

The last step reads every binary left in the bundle and fails when one of them
links a library that was taken out. A trimmed bundle that would not load is a
build error, never a surprise on the operator's machine.

    python3 tools/qt_bundle.py prune <site-packages>      # trim in place
    python3 tools/qt_bundle.py prune --dry-run <site-packages>

Runs on the build host's own python3 (macOS and Linux; the Windows build is
PyInstaller, which only collects what is imported). Reads Mach-O and ELF
itself, so it needs neither otool nor binutils.

stdlib only.
"""
from __future__ import annotations

import argparse
import re
import shutil
import struct
import sys
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from pathlib import Path

# What corvus/app.py imports from PySide6 (tools/scene_kit uses the same).
MODULES: tuple[str, ...] = (
    "QtCore", "QtGui", "QtWidgets", "QtNetwork",
    "QtWebChannel", "QtWebEngineCore", "QtWebEngineWidgets",
)

# Plugin folders a QApplication with a QWebEngineView may load from, on any
# platform. Everything in them is kept along with what it links.
PLUGIN_DIRS: frozenset[str] = frozenset({
    "platforms", "platformthemes", "platforminputcontexts", "styles",
    "imageformats", "iconengines", "generic", "tls", "networkinformation",
    "position", "printsupport", "xcbglintegrations", "egldeviceintegrations",
    "wayland-decoration-client", "wayland-graphics-integration-client",
    "wayland-shell-integration",
})

# Plugins in a kept folder that are left out anyway. The virtual keyboard is a
# QML keyboard for touch screens without one, only ever loaded on request
# (QT_IM_MODULE=qtvirtualkeyboard), and it would pull its own libraries in.
DROP_PLUGINS: tuple[str, ...] = ("virtualkeyboard",)

# The one program in Qt's libexec a running app starts.
LIBEXEC_KEEP: frozenset[str] = frozenset({"QtWebEngineProcess", "qt.conf"})

# For building against PySide6 and Qt, never read by a running app.
DEV_PATHS: tuple[str, ...] = (
    "PySide6/include", "PySide6/typesystems", "PySide6/glue", "PySide6/doc",
    "PySide6/scripts", "PySide6/lib", "PySide6/Qt/metatypes", "PySide6/Qt/qml",
    "shiboken6/include", "shiboken6/lib",
)

MODULE_SUFFIX = ".abi3.so"

_MACHO_THIN = {
    b"\xcf\xfa\xed\xfe": ("<", True), b"\xce\xfa\xed\xfe": ("<", False),
    b"\xfe\xed\xfa\xcf": (">", True), b"\xfe\xed\xfa\xce": (">", False),
}
_MACHO_FAT = {b"\xca\xfe\xba\xbe": False, b"\xca\xfe\xba\xbf": True}
_ELF = b"\x7fELF"
# LC_LOAD_DYLIB, LC_LOAD_WEAK_DYLIB, LC_REEXPORT_DYLIB, LC_LAZY_LOAD_DYLIB,
# LC_LOAD_UPWARD_DYLIB: every load command that names a library to load.
_DYLIB_COMMANDS = {0xC, 0x80000018, 0x8000001F, 0x20, 0x80000023}


class BundleError(RuntimeError):
    """The bundle cannot be trimmed, or would not load once it was."""


def _read(fh, offset: int, size: int) -> bytes:
    fh.seek(offset)
    data = fh.read(size)
    if len(data) != size:
        raise ValueError("truncated")
    return data


def _macho_slice_deps(fh, base: int) -> list[str]:
    head = _read(fh, base, 4)
    endian, wide = _MACHO_THIN[head]
    ncmds, sizeofcmds = struct.unpack(endian + "II", _read(fh, base + 16, 8))
    start = base + (32 if wide else 28)
    commands = _read(fh, start, sizeofcmds)
    out: list[str] = []
    pos = 0
    for _ in range(ncmds):
        cmd, size = struct.unpack_from(endian + "II", commands, pos)
        if size < 8:
            break
        if cmd in _DYLIB_COMMANDS:
            name_at = struct.unpack_from(endian + "I", commands, pos + 8)[0]
            raw = commands[pos + name_at:pos + size]
            out.append(raw.split(b"\0", 1)[0].decode("utf-8", "replace"))
        pos += size
    return out


def macho_deps(fh) -> list[str]:
    """The libraries a Mach-O file (thin or universal) loads, as it names them."""
    head = _read(fh, 0, 4)
    if head in _MACHO_THIN:
        return _macho_slice_deps(fh, 0)
    wide = _MACHO_FAT[head]
    count = struct.unpack(">I", _read(fh, 4, 4))[0]
    if count > 32:              # a Java class file shares the magic number
        return []
    out: list[str] = []
    for i in range(count):
        if wide:
            offset = struct.unpack(">Q", _read(fh, 8 + 32 * i + 8, 8))[0]
        else:
            offset = struct.unpack(">I", _read(fh, 8 + 20 * i + 8, 4))[0]
        for name in _macho_slice_deps(fh, offset):
            if name not in out:
                out.append(name)
    return out


def elf_deps(fh) -> list[str]:
    """The DT_NEEDED entries of a 64-bit ELF file. Others answer ``[]``."""
    ident = _read(fh, 0, 16)
    if ident[4] != 2:
        return []
    endian = "<" if ident[5] == 1 else ">"
    phoff = struct.unpack(endian + "Q", _read(fh, 32, 8))[0]
    phentsize, phnum = struct.unpack(endian + "HH", _read(fh, 54, 4))
    loads: list[tuple[int, int, int]] = []
    dynamic: tuple[int, int] | None = None
    for i in range(phnum):
        p_type, _flags, p_offset, p_vaddr, _paddr, p_filesz = struct.unpack(
            endian + "IIQQQQ", _read(fh, phoff + i * phentsize, 40))
        if p_type == 1:
            loads.append((p_vaddr, p_offset, p_filesz))
        elif p_type == 2:
            dynamic = (p_offset, p_filesz)
    if dynamic is None:
        return []
    table = _read(fh, dynamic[0], dynamic[1])
    needed: list[int] = []
    strtab = None
    for pos in range(0, len(table) - 15, 16):
        tag, value = struct.unpack_from(endian + "qQ", table, pos)
        if tag == 0:
            break
        if tag == 1:
            needed.append(value)
        elif tag == 5:
            strtab = value
    if strtab is None:
        return []
    base = next((off + strtab - vaddr for vaddr, off, size in loads
                 if vaddr <= strtab < vaddr + size), None)
    if base is None:
        return []
    out = []
    for at in needed:
        fh.seek(base + at)
        raw = fh.read(4096)
        out.append(raw.split(b"\0", 1)[0].decode("utf-8", "replace"))
    return out


def binary_kind(path: Path) -> str:
    """``"macho"``, ``"elf"`` or ``""`` for anything else."""
    try:
        with open(path, "rb") as fh:
            head = fh.read(4)
    except OSError:
        return ""
    if head in _MACHO_THIN or head in _MACHO_FAT:
        return "macho"
    if head == _ELF:
        return "elf"
    return ""


def binary_deps(path: Path) -> list[str]:
    """The libraries *path* links, or ``[]`` when it is not a binary."""
    kind = binary_kind(path)
    if not kind:
        return []
    try:
        with open(path, "rb") as fh:
            return macho_deps(fh) if kind == "macho" else elf_deps(fh)
    except (OSError, ValueError, struct.error, KeyError) as exc:
        raise BundleError(f"cannot read the load commands of {path}: {exc}") from exc


def library_key(name: str) -> str:
    """What a linked library is filed under: a framework's name, else its file name."""
    parts = name.replace("\\", "/").split("/")
    for part in parts:
        if part.endswith(".framework"):
            return part[:-len(".framework")]
    return parts[-1]


def module_for(key: str) -> str:
    """The PySide6 module named after a Qt library, or ``""``.

    ``QtPrintSupport`` (a macOS framework) and ``libQt6PrintSupport.so.6``
    (Linux) are both the library under ``PySide6.QtPrintSupport``.
    """
    match = re.match(r"^libQt6(.+?)\.so", key)
    if match:
        return "Qt" + match.group(1)
    return key if key.startswith("Qt") and "." not in key else ""


def _binaries(path: Path) -> list[Path]:
    if path.is_file():
        return [path] if binary_kind(path) else []
    return sorted(p for p in path.rglob("*")
                  if p.is_file() and not p.is_symlink() and binary_kind(p))


def library_index(site: Path) -> dict[str, Path]:
    """Every library the PySide6 wheels ship, by the key a dependency names it with."""
    out: dict[str, Path] = {}
    lib = site / "PySide6" / "Qt" / "lib"
    if lib.is_dir():
        for entry in sorted(lib.iterdir()):
            if entry.suffix == ".framework" and entry.is_dir():
                out[entry.stem] = entry
            elif entry.is_file() and binary_kind(entry):
                out[entry.name] = entry
    for package in ("PySide6", "shiboken6"):
        folder = site / package
        if not folder.is_dir():
            continue
        for entry in sorted(folder.iterdir()):
            if entry.name.startswith("lib") and entry.is_file() and binary_kind(entry):
                out[entry.name] = entry
    return out


@dataclass
class Plan:
    """What :func:`plan` decided: kept by name, and every path to remove."""

    libraries: set[str] = field(default_factory=set)
    modules: set[str] = field(default_factory=set)
    remove: list[Path] = field(default_factory=list)
    removed_keys: set[str] = field(default_factory=set)


def _tool_entry(entry: Path) -> bool:
    """A Qt tool at the top of the PySide6 package: designer, Linguist.app and so on."""
    if entry.is_dir():
        return entry.suffix == ".app"
    if entry.name.startswith("lib") or entry.name.endswith((".so", ".dylib", ".pyd")):
        return False
    return bool(binary_kind(entry))


def plan(site: Path, deps: Callable[[Path], list[str]] = binary_deps) -> Plan:
    """Decide what of PySide6 under *site* stays. Touches nothing."""
    pyside = site / "PySide6"
    qt = pyside / "Qt"
    missing = [m for m in MODULES if not (pyside / f"{m}{MODULE_SUFFIX}").is_file()]
    if missing:
        raise BundleError(f"{pyside} lacks {', '.join(missing)}; is PySide6 installed there?")

    libraries = library_index(site)
    result = Plan(modules=set(MODULES))
    seeds: list[Path] = [pyside / f"{m}{MODULE_SUFFIX}" for m in MODULES]
    seeds += _binaries(site / "shiboken6")
    plugins = qt / "plugins"
    dropped_plugins: list[Path] = []
    if plugins.is_dir():
        for folder in sorted(plugins.iterdir()):
            if folder.name not in PLUGIN_DIRS:
                continue
            for binary in _binaries(folder):
                if any(word in binary.name for word in DROP_PLUGINS):
                    dropped_plugins.append(binary)
                else:
                    seeds.append(binary)
    libexec = qt / "libexec"
    if libexec.is_dir():
        for entry in sorted(libexec.iterdir()):
            if entry.name in LIBEXEC_KEEP:
                seeds += _binaries(entry)

    todo = list(seeds)
    scanned: set[Path] = set()
    while todo:
        binary = todo.pop()
        if binary in scanned:
            continue
        scanned.add(binary)
        for name in deps(binary):
            key = library_key(name)
            if key not in libraries or key in result.libraries:
                continue
            result.libraries.add(key)
            todo += _binaries(libraries[key])
            module = module_for(key)
            module_file = pyside / f"{module}{MODULE_SUFFIX}"
            if module and module not in result.modules and module_file.is_file():
                result.modules.add(module)
                todo.append(module_file)

    remove: list[Path] = []
    for key, path in libraries.items():
        if key not in result.libraries and path.parent != site / "shiboken6":
            remove.append(path)
            result.removed_keys.add(key)
    for entry in sorted(pyside.iterdir()):
        name = entry.name
        stem = name.split(".", 1)[0]
        if stem.startswith("Qt") and name.endswith((MODULE_SUFFIX, ".pyi")) \
                and stem not in result.modules:
            remove.append(entry)
        elif _tool_entry(entry):
            remove.append(entry)
    if plugins.is_dir():
        remove += [f for f in sorted(plugins.iterdir()) if f.name not in PLUGIN_DIRS]
    remove += dropped_plugins
    if libexec.is_dir():
        remove += [e for e in sorted(libexec.iterdir()) if e.name not in LIBEXEC_KEEP]
    remove += [site / rel for rel in DEV_PATHS if (site / rel).exists()]
    result.remove = sorted(set(remove))
    return result


def check(site: Path, removed_keys: Iterable[str],
          deps: Callable[[Path], list[str]] = binary_deps) -> None:
    """Fail when a binary left under *site* links a library that was taken out."""
    removed = set(removed_keys)
    broken: list[str] = []
    for package in ("PySide6", "shiboken6"):
        for binary in _binaries(site / package):
            for name in deps(binary):
                if library_key(name) in removed:
                    broken.append(f"{binary.relative_to(site)} links {name}")
    for module in MODULES:
        if not (site / "PySide6" / f"{module}{MODULE_SUFFIX}").is_file():
            broken.append(f"PySide6.{module} is gone")
    if broken:
        raise BundleError("the trimmed Qt would not load:\n  " + "\n  ".join(broken))


def _size(path: Path) -> int:
    if path.is_symlink() or path.is_file():
        return path.lstat().st_size
    return sum(p.lstat().st_size for p in path.rglob("*") if p.is_file() or p.is_symlink())


def prune(site: Path, dry_run: bool = False, log: Callable[[str], None] = print) -> Plan:
    """Trim PySide6 under *site* in place and prove the rest still links."""
    site = site.resolve()
    decided = plan(site)
    before = _size(site / "PySide6") + _size(site / "shiboken6")
    for path in decided.remove:
        if site not in path.parents:
            raise BundleError(f"refusing to remove {path}, outside {site}")
    if dry_run:
        for path in decided.remove:
            log(f"    would remove {path.relative_to(site)}  ({_size(path) >> 20} MB)")
    else:
        for path in decided.remove:
            if path.is_dir() and not path.is_symlink():
                shutil.rmtree(path)
            elif path.exists() or path.is_symlink():
                path.unlink()
        check(site, decided.removed_keys)
    after = before - sum(_size(p) for p in decided.remove) if dry_run else (
        _size(site / "PySide6") + _size(site / "shiboken6"))
    log(f"    Qt modules : {', '.join(sorted(decided.modules))}")
    log(f"    Qt libs    : {len(decided.libraries)} kept, {len(decided.removed_keys)} removed")
    log(f"    PySide6    : {before >> 20} MB -> {after >> 20} MB")
    return decided


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="qt_bundle.py", description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    trim = sub.add_parser("prune", help="trim PySide6 under a site-packages folder")
    trim.add_argument("site", type=Path, help="the bundle's site-packages")
    trim.add_argument("--dry-run", action="store_true", help="list, remove nothing")
    args = parser.parse_args(argv)
    try:
        prune(args.site, dry_run=args.dry_run)
    except BundleError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
