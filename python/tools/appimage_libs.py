#!/usr/bin/env python3
"""Bundle the system libraries an AppImage needs and cannot count on finding.

The PySide6 wheels carry Qt, but Qt's X11 platform plugin links a ring of
small X libraries (``libxcb-cursor``, ``libxkbcommon-x11``, ``libxcb-icccm``
and more) that the wheels leave to the system. A desktop with a Qt or GTK
application installed tends to have them; a clean one does not, and there Qt
cannot load its platform plugin and aborts before a window exists. AppImageHub
tests on exactly such a machine.

So the AppDir is closed here. Every ELF file in it is read for the libraries
it links (``DT_NEEDED``, via :mod:`qt_bundle`), and every one the AppDir does
not carry itself is copied in from the build host, then read in turn, to the
end. Left out are the libraries that belong to the host and must come from it:
the C library, the graphics driver stack, the X and font core that every
desktop has, and NSS, which loads its own modules from beside itself. That
list is :data:`HOST_PROVIDED`; it follows the AppImage project's exclude list.

The last step fails the build when a binary the app cannot start without
still links a library that is neither in the AppDir nor on that list. A
plugin Qt only loads when the host has what it needs (the GTK theme, printing
through CUPS, framebuffer backends) is reported and left to the host, because
Qt skips a plugin it cannot load.

    python3 tools/appimage_libs.py bundle <AppDir>
    python3 tools/appimage_libs.py check <AppDir>

Linux build hosts only (it asks ``ldconfig`` where libraries are). stdlib only.
"""
from __future__ import annotations

import argparse
import fnmatch
import os
import shutil
import subprocess
import sys
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from qt_bundle import binary_deps, binary_kind  # noqa: E402

# Libraries every Linux desktop provides, and that must come from it: the C
# runtime (bound to the host's loader), the GL/EGL/Vulkan driver stack
# (bound to the host's GPU driver), the X and font core, sound, D-Bus, GLib,
# Wayland (bound to the compositor) and NSS. Globs, matched on the file name.
HOST_PROVIDED: tuple[str, ...] = (
    "ld-linux*.so*", "libc.so.*", "libm.so.*", "libmvec.so.*", "libdl.so.*",
    "libpthread.so.*", "librt.so.*", "libresolv.so.*", "libutil.so.*",
    "libanl.so.*", "libnsl.so.*", "libnss_*.so.*", "libthread_db.so.*",
    "libBrokenLocale.so.*", "libcidn.so.*",
    "libstdc++.so.*", "libgcc_s.so.*",
    "libGL.so.*", "libEGL.so.*", "libGLX.so.*", "libGLdispatch.so.*",
    "libOpenGL.so.*", "libGLESv2.so.*", "libglapi.so.*", "libgbm.so.*",
    "libdrm.so.*", "libvulkan.so.*",
    "libX11.so.*", "libX11-xcb.so.*", "libxcb.so.*",
    "libfontconfig.so.*", "libfreetype.so.*", "libharfbuzz.so.*",
    "libexpat.so.*", "libz.so.*", "libuuid.so.*",
    "libasound.so.*", "libdbus-1.so.*",
    "libglib-2.0.so.*", "libgobject-2.0.so.*", "libgio-2.0.so.*",
    "libgmodule-2.0.so.*", "libgthread-2.0.so.*",
    "libwayland-client.so.*", "libwayland-cursor.so.*", "libwayland-egl.so.*",
    "libwayland-server.so.*",
    "libnss3.so", "libnssutil3.so", "libsmime3.so", "libnspr4.so",
    "libplc4.so", "libplds4.so",
    "libcups.so.*", "libcom_err.so.*", "libgpg-error.so.*",
)

# Binaries whose missing library is reported, not fatal, and never copied in:
# Qt skips a plugin it cannot load, and Python a module nothing imports. The
# GTK theme plugin is the reason they are not bundled: it links GTK, which a
# GTK desktop has and which must match that desktop's own. Globs on the path
# inside the AppDir. The X11 platform plugin and its GL integrations are not
# here: without them there is no window.
OPTIONAL: tuple[str, ...] = (
    "*/PySide6/Qt/plugins/platformthemes/*",
    "*/PySide6/Qt/plugins/printsupport/*",
    "*/PySide6/Qt/plugins/egldeviceintegrations/*",
    "*/PySide6/Qt/plugins/platforms/libqeglfs.so",
    "*/PySide6/Qt/plugins/platforms/libqlinuxfb.so",
    "*/PySide6/Qt/plugins/platforms/libqvkkhrdisplay.so",
    "*/PySide6/Qt/plugins/platforms/libqvnc.so",
    "*/lib-dynload/_tkinter*",
)

LIB_DIRS: tuple[str, ...] = (
    "/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu",
    "/lib64", "/usr/lib64", "/lib", "/usr/lib", "/usr/local/lib",
)


class BundleError(RuntimeError):
    """The AppDir links a library nothing provides."""


def host_provided(name: str) -> bool:
    """Whether *name* is a library the target machine supplies itself."""
    return any(fnmatch.fnmatchcase(name, pattern) for pattern in HOST_PROVIDED)


def optional(rel: str) -> bool:
    """Whether the binary at *rel* (inside the AppDir) may go without a library."""
    rel = "/" + rel.lstrip("/")
    return any(fnmatch.fnmatchcase(rel, pattern) for pattern in OPTIONAL)


def ldconfig_index(run: Callable[..., subprocess.CompletedProcess] = subprocess.run) -> dict[str, Path]:
    """The host's 64-bit libraries by file name, as ``ldconfig -p`` lists them."""
    for exe in ("ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"):
        try:
            out = run([exe, "-p"], capture_output=True, text=True, check=False).stdout
        except OSError:
            continue
        index: dict[str, Path] = {}
        for line in out.splitlines():
            left, sep, target = line.partition(" => ")
            if not sep or "x86-64" not in left:
                continue
            name = left.strip().split(" ", 1)[0]
            index.setdefault(name, Path(target.strip()))
        return index
    return {}


def find_host_library(name: str, index: dict[str, Path],
                      dirs: Iterable[str] = LIB_DIRS) -> Path | None:
    """Where the build host keeps *name*, or ``None``."""
    hit = index.get(name)
    if hit is not None and hit.is_file():
        return hit
    for folder in dirs:
        candidate = Path(folder) / name
        if candidate.is_file() and binary_kind(candidate) == "elf":
            return candidate
    return None


def _elf_files(appdir: Path) -> list[Path]:
    return sorted(p for p in appdir.rglob("*")
                  if p.is_file() and not p.is_symlink() and binary_kind(p) == "elf")


def _provided(appdir: Path) -> set[str]:
    """Every library name the AppDir answers itself, symlinks included."""
    return {p.name for p in appdir.rglob("*")
            if (p.is_file() or p.is_symlink()) and ".so" in p.name}


@dataclass
class Report:
    """What :func:`bundle` copied and what :func:`unresolved` could not settle."""

    copied: dict[str, Path] = field(default_factory=dict)
    missing: dict[str, list[str]] = field(default_factory=dict)
    optional_missing: dict[str, list[str]] = field(default_factory=dict)


def unresolved(appdir: Path, deps: Callable[[Path], list[str]] = binary_deps) -> Report:
    """Libraries the AppDir links that neither it nor the host supplies."""
    provided = _provided(appdir)
    report = Report()
    for binary in _elf_files(appdir):
        rel = binary.relative_to(appdir).as_posix()
        for name in deps(binary):
            if name in provided or host_provided(name):
                continue
            target = report.optional_missing if optional(rel) else report.missing
            target.setdefault(name, []).append(rel)
    return report


def bundle(appdir: Path, libdir: Path | None = None,
           find: Callable[[str], Path | None] | None = None,
           deps: Callable[[Path], list[str]] = binary_deps,
           log: Callable[[str], None] = print) -> Report:
    """Copy into *libdir* every library the AppDir links and the host may lack."""
    appdir = appdir.resolve()
    libdir = libdir or appdir / "usr" / "lib"
    libdir.mkdir(parents=True, exist_ok=True)
    if find is None:
        index = ldconfig_index()
        find = lambda name: find_host_library(name, index)  # noqa: E731
    copied: dict[str, Path] = {}
    while True:
        report = unresolved(appdir, deps)
        wanted = sorted(report.missing)
        added = False
        for name in wanted:
            if name in copied:
                continue
            source = find(name)
            if source is None:
                copied[name] = Path()
                continue
            dest = libdir / name
            shutil.copy2(source.resolve(), dest)
            os.chmod(dest, 0o755)
            copied[name] = source
            added = True
            log(f"    bundled {name}  ({source})")
        if not added:
            break
    report = unresolved(appdir, deps)
    report.copied = {k: v for k, v in copied.items() if v != Path()}
    return report


def require(report: Report, log: Callable[[str], None] = print) -> None:
    """Raise when a binary the app needs still misses a library."""
    for name, users in sorted(report.optional_missing.items()):
        log(f"    note: {name} not bundled; optional {users[0]} will not load without it")
    if report.missing:
        lines = [f"{name}  (linked by {', '.join(users[:3])})"
                 for name, users in sorted(report.missing.items())]
        raise BundleError(
            "the AppImage links libraries that are neither bundled nor provided "
            "by every Linux desktop, and the build host has none to copy:\n  "
            + "\n  ".join(lines)
            + "\nInstall them on the build host (see the apt-get line in "
              ".github/workflows/build.yml) and build again.")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="appimage_libs.py", description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for command, text in (("bundle", "copy the libraries in, then check"),
                          ("check", "only check, copy nothing")):
        p = sub.add_parser(command, help=text)
        p.add_argument("appdir", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "bundle":
            report = bundle(args.appdir)
            print(f"    {len(report.copied)} system libraries bundled")
        else:
            report = unresolved(args.appdir.resolve())
        require(report)
    except BundleError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
