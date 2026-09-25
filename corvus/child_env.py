"""The environment for a program Corvus starts that is not part of Corvus.

A packaged Corvus runs with variables its launcher set for Corvus' own
interpreter and Qt, all pointing inside the bundle:

* ``PYTHONHOME`` and ``PYTHONPATH`` (the AppImage's ``AppRun`` and the macOS
  launcher), so the bundled interpreter finds its stdlib;
* ``LD_LIBRARY_PATH``, ``QT_PLUGIN_PATH`` and the QtWebEngine paths (the
  AppImage), so the bundled Qt is the one loaded;
* ``QTWEBENGINE_CHROMIUM_FLAGS`` (every launcher, and :mod:`corvus.app`), for
  the embedded Chromium only;
* ``APPDIR``, ``APPIMAGE``, ``ARGV0`` and ``OWD`` (the AppImage runtime), and
  ``_PYI_*`` (the Windows build's PyInstaller bootloader).

A child inherits every one of them unless told otherwise, and for the programs
Corvus starts on the operator's behalf that is wrong: ``python3 tool.py`` from
a launcher button loads the bundle's stdlib into a different interpreter and
dies before its first line, a Qt program opened from the local terminal loads
Corvus' Qt libraries and plugins, and ``xdg-open`` on a KDE desktop is itself
a Qt program. So everything that starts one (the local shell, a background
program, ffmpeg, the file manager, the browser) asks :func:`child_env` for its
environment instead of passing ``os.environ`` on.

What is kept is exactly what the operator's own environment had: only entries
pointing into this Corvus are taken out, and a list such as
``LD_LIBRARY_PATH`` keeps whatever the operator had in it besides.

stdlib only.
"""
from __future__ import annotations

import os
import sys
from collections.abc import Mapping

# The value corvus/app.py gave QT_QPA_PLATFORM, when it chose one. Recorded so
# a child gets the variable taken away only when Corvus put it there.
QPA_DEFAULT_ENV = "CORVUS_QPA_DEFAULT"

# Lists of directories. An entry inside this Corvus is removed, the rest kept.
_PATH_LISTS: tuple[str, ...] = (
    "PYTHONPATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "QT_PLUGIN_PATH",
    "QT_QPA_PLATFORM_PLUGIN_PATH",
    "QML_IMPORT_PATH",
    "QML2_IMPORT_PATH",
    "QTWEBENGINE_RESOURCES_PATH",
    "QTWEBENGINE_DICTIONARIES_PATH",
    "QTWEBENGINEPROCESS_PATH",
)

# Only ever meaningful to this interpreter or to the embedded Chromium.
_ALWAYS_DROP: tuple[str, ...] = (
    "PYTHONEXECUTABLE",
    "__PYVENV_LAUNCHER__",
    "QTWEBENGINE_CHROMIUM_FLAGS",
    "_MEIPASS2",
)

# Set by the AppImage runtime for the AppImage itself.
_APPIMAGE_VARS: tuple[str, ...] = ("APPDIR", "APPIMAGE", "ARGV0", "OWD")


def app_root() -> str:
    """The folder holding ``corvus/``: the repository, or the bundle's app folder."""
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _norm(path: str) -> str:
    return os.path.normcase(os.path.normpath(os.path.abspath(path)))


def _inside(path: str, root: str) -> bool:
    """Whether *path* is *root* or somewhere under it."""
    if not path or not root:
        return False
    p, r = _norm(path), _norm(root)
    return p == r or p.startswith(r.rstrip(os.sep) + os.sep)


def bundle_dirs(env: Mapping[str, str] | None = None, root: str | None = None,
                meipass: str | None = None) -> list[str]:
    """The folders that belong to this Corvus and to nothing else.

    The app folder itself; the AppImage mount it is inside, if it is; the
    ``.app`` bundle it is inside, if it is; and PyInstaller's unpack folder in
    the Windows build. Never a system prefix such as ``/usr``: a development
    run's interpreter lives there too, and the operator's own entries under it
    are theirs.
    """
    env = os.environ if env is None else env
    root = app_root() if root is None else root
    dirs = [root]
    appdir = env.get("APPDIR", "")
    if appdir and _inside(root, appdir):
        dirs.append(appdir)
    head = _norm(root)
    while True:
        if head.lower().endswith(".app"):
            dirs.append(head)
            break
        parent = os.path.dirname(head)
        if parent == head:
            break
        head = parent
    meipass = getattr(sys, "_MEIPASS", None) if meipass is None else meipass
    if meipass:
        dirs.append(meipass)
    return dirs


def _ours(path: str, dirs: list[str]) -> bool:
    return any(_inside(path, d) for d in dirs)


def child_env(env: Mapping[str, str] | None = None, *, root: str | None = None,
              prefixes: tuple[str, ...] | None = None,
              meipass: str | None = None) -> dict[str, str]:
    """A copy of *env* (``os.environ`` by default) fit for a program that is not Corvus.

    *root*, *prefixes* and *meipass* stand in for :func:`app_root`, this
    interpreter's prefixes and ``sys._MEIPASS``, for the tests.
    """
    src = dict(os.environ if env is None else env)
    dirs = bundle_dirs(src, root, meipass)
    out = dict(src)
    for key in _ALWAYS_DROP:
        out.pop(key, None)
    for key in [k for k in out if k.startswith("_PYI_")]:
        out.pop(key, None)

    home = out.get("PYTHONHOME", "")
    if home:
        if prefixes is None:
            prefixes = (sys.prefix, sys.base_prefix, sys.exec_prefix)
        if _ours(home, dirs) or any(p and _norm(home) == _norm(p) for p in prefixes):
            out.pop("PYTHONHOME")

    for key in _PATH_LISTS:
        value = out.get(key)
        if value is None:
            continue
        kept = [p for p in value.split(os.pathsep) if p and not _ours(p, dirs)]
        if kept:
            out[key] = os.pathsep.join(kept)
        else:
            out.pop(key)

    defaulted = out.pop(QPA_DEFAULT_ENV, None)
    if defaulted is not None and out.get("QT_QPA_PLATFORM") == defaulted:
        out.pop("QT_QPA_PLATFORM")

    appdir = src.get("APPDIR", "")
    if appdir and appdir in dirs:
        for key in _APPIMAGE_VARS:
            out.pop(key, None)
    return out
