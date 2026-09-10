"""The icons a Linux desktop keeps on disk for an AppImage.

The Settings switch flips the icon of the *running* process: corvus/app.py
hands Qt a QIcon and the taskbar entry of the live window follows. Everywhere
else the desktop draws the app from a file it wrote earlier, and there are two
such files:

* the **launcher icon**. When a desktop integrator (appimaged,
  AppImageLauncher) first sees the AppImage it copies the bundled ``.desktop``
  entry into the user's applications dir and the bundled PNG into the user's
  icon theme. That is what the applications grid, the dash, the search results
  and the launcher pin read.
* the **file thumbnail** the file manager paints on the ``.AppImage`` file.
  Contrary to how it looks, this is *not* served from the read-only
  ``.DirIcon`` inside the SquashFS — it is a freedesktop thumbnail under
  ``$XDG_CACHE_HOME``, which an integrator merely seeds from the ``.DirIcon``.

Both keep whatever the build shipped, and both are ordinary files under
``$HOME`` — which is what lets the switch reach them without repacking
anything. This module locates the ones belonging to *this* AppImage — matched
through the ``Exec=`` line of the integrated entry and the hash of the file's
own URI, never by name alone — and rewrites them with the requested cut of
the mark.

Deliberately narrow:

* Linux only, and only inside a packaged AppImage. Without ``$APPIMAGE``
  there is no integrated entry to correct; a source checkout has none, and
  macOS and Windows keep the icon their build baked in (rewriting a signed
  ``.app`` bundle breaks its seal, and the Windows ``.ico`` lives inside the
  running executable).
* Only ``.png`` targets are overwritten. An integrator that installed an SVG
  is left alone rather than handed PNG bytes under an ``.svg`` name.
* No backup is kept, because none is needed: both cuts of the mark ship in
  ``assets/`` inside the bundle, so flipping the switch back restores the
  other one byte for byte, and a thumbnail the desktop rejects is simply
  regenerated from the ``.DirIcon`` — i.e. the behaviour before this module.

Nothing in here raises. It runs from the desktop wrapper's Qt timer, where
a missing directory or a read-only theme must cost the operator a log line,
not the app.
"""
from __future__ import annotations

import hashlib
import logging
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import urllib.parse
from typing import Callable, Iterable

logger = logging.getLogger("corvus.desktop_icon")

# .../hicolor/256x256/apps/name.png — the theme spec names the directory
# after the size it holds, which is the only size hint we get.
_SIZE_DIR = re.compile(r"^(\d+)x(\d+)$")

_ICON_SUFFIX = ".png"

#: ``render(source, dest, size, text)`` writes *source* into *dest* as a PNG,
#: scaled to *size* px when *size* is not None, with *text* embedded as PNG
#: ``tEXt`` keys (empty for a plain icon; the thumbnail spec's ``Thumb::*``
#: keys for a thumbnail). Injected by the caller so this module stays free of
#: an image library — corvus/app.py already has Qt loaded and passes a
#: QImage-backed renderer.
Renderer = Callable[[str, str, "int | None", "dict[str, str]"], None]


def appimage_path(env=None) -> str | None:
    """The AppImage this process runs from, or ``None`` when there is none.

    The AppImage type-2 runtime exports ``$APPIMAGE`` as the absolute path of
    the ``.AppImage`` file itself (not the ``/tmp/.mount_*`` mount point), so
    it doubles as the "are we packaged?" test. Non-Linux platforms never get
    a path back: their launcher icons are not editable files.
    """
    if not sys.platform.startswith("linux"):
        return None
    value = (env if env is not None else os.environ).get("APPIMAGE")
    if not value:
        return None
    path = os.path.realpath(value)
    return path if os.path.isfile(path) else None


def _data_home(env) -> str:
    home = env.get("XDG_DATA_HOME") or ""
    if home:
        return home
    return os.path.join(os.path.expanduser("~"), ".local", "share")


def applications_dirs(env=None) -> list[str]:
    """User-writable dirs a desktop integrator drops ``.desktop`` files in.

    System dirs (``/usr/share/applications``) are deliberately absent: an
    AppImage never integrates itself there, and the switch must not touch
    entries that belong to a package manager.
    """
    env = env if env is not None else os.environ
    return [os.path.join(_data_home(env), "applications")]


def icon_dirs(env=None) -> list[str]:
    """User-writable icon roots, in the order the icon spec searches them."""
    env = env if env is not None else os.environ
    return [
        os.path.join(_data_home(env), "icons"),
        os.path.join(os.path.expanduser("~"), ".icons"),
    ]


def _exec_launches(value: str, appimage: str) -> bool:
    """Whether a ``Exec=``/``TryExec=`` value starts *appimage*.

    Every token is checked, not just the first: integrators quote the path
    (AppImageLauncher) and some wrap it in ``env VAR=x …``, so the binary is
    not reliably argv[0]. Field codes like ``%U`` simply never match a path.
    """
    try:
        tokens = shlex.split(value)
    except ValueError:                      # unbalanced quotes in a hand-edit
        tokens = value.split()
    return any(os.path.realpath(t) == appimage for t in tokens if t)


def desktop_entry_icon(text: str, appimage: str) -> str | None:
    """The ``Icon=`` of a ``[Desktop Entry]`` that launches *appimage*.

    Returns ``None`` for every other entry, which is what keeps the rewrite
    from wandering into a neighbouring app's icon. Only the main group is
    read — parsing stops at the first ``[Desktop Action …]`` header, whose
    ``Icon=`` belongs to that action, not to the app.
    """
    in_entry = False
    icon: str | None = None
    launches = False
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            if in_entry:
                break
            in_entry = line == "[Desktop Entry]"
            continue
        if not in_entry or "=" not in line:
            continue
        key, _, value = line.partition("=")
        key, value = key.strip(), value.strip()
        if key == "Icon":                   # plain key only; Icon[de] is a label
            icon = value
        elif key in ("Exec", "TryExec") and _exec_launches(value, appimage):
            launches = True
    return icon if (launches and icon) else None


def integrated_icon_names(appimage: str, env=None) -> list[str]:
    """``Icon=`` names of every integrated entry that launches *appimage*.

    Usually one. A machine that has been through both integrators, or that
    kept an entry from an older copy of the AppImage, can hold several — all
    of them are worth correcting, so all of them come back.
    """
    names: list[str] = []
    for directory in applications_dirs(env):
        try:
            entries = sorted(os.listdir(directory))
        except OSError:
            continue
        for name in entries:
            if not name.endswith(".desktop"):
                continue
            path = os.path.join(directory, name)
            try:
                with open(path, "r", encoding="utf-8", errors="replace") as fh:
                    text = fh.read()
            except OSError:
                continue
            icon = desktop_entry_icon(text, appimage)
            if icon and icon not in names:
                names.append(icon)
    return names


def nominal_size(path: str) -> int | None:
    """Pixel size the icon theme expects at *path*, from its directory name.

    ``.../hicolor/256x256/apps/x.png`` is 256. A flat drop (``~/.icons/x.png``)
    or a ``scalable/`` directory has no size to honour, so the source is
    written through unscaled.
    """
    head = os.path.dirname(os.path.abspath(path))
    while True:
        head, tail = os.path.split(head)
        if not tail:
            return None
        match = _SIZE_DIR.match(tail)
        if match and match.group(1) == match.group(2):
            return int(match.group(1))
        if not head or head == os.sep:
            return None


def icon_files(names: Iterable[str], env=None) -> list[str]:
    """Every writable ``.png`` an integrator installed under *names*.

    An absolute ``Icon=`` (legal, and what some integrators write) is taken
    as the file itself. A bare name is searched for across the user's icon
    roots, which is where the theme spec says it must live — one file per
    size the integrator extracted.
    """
    found: list[str] = []

    def keep(path: str) -> None:
        if path.endswith(_ICON_SUFFIX) and os.path.isfile(path) and path not in found:
            found.append(path)

    roots = icon_dirs(env)
    for name in names:
        if os.path.isabs(name):
            keep(name)
            continue
        target = os.path.basename(name) + _ICON_SUFFIX
        for root in roots:
            for dirpath, _dirnames, filenames in os.walk(root):
                if target in filenames:
                    keep(os.path.join(dirpath, target))
    return found


def _theme_root(path: str) -> str | None:
    """The icon theme directory (the one holding ``index.theme``) above *path*."""
    head = os.path.dirname(os.path.abspath(path))
    while head and head != os.sep:
        if os.path.isfile(os.path.join(head, "index.theme")):
            return head
        head = os.path.dirname(head)
    return None


def refresh_icon_cache(paths: Iterable[str]) -> None:
    """Re-run ``gtk-update-icon-cache`` for themes that already have a cache.

    A cached theme serves the stale icon until the cache is rebuilt; an
    uncached one (the usual case for ``~/.local/share/icons``) reads the file
    and needs nothing. So this only fires where a cache file actually exists,
    which keeps it from *creating* a cache the desktop never asked for.
    """
    tool = shutil.which("gtk-update-icon-cache")
    if not tool:
        return
    roots = {r for r in (_theme_root(p) for p in paths) if r}
    for root in roots:
        if not os.path.isfile(os.path.join(root, "icon-theme.cache")):
            continue
        try:
            subprocess.run(
                [tool, "--force", "--quiet", root],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                timeout=10, check=False,
            )
        except (OSError, subprocess.SubprocessError):
            logger.debug("icon cache refresh failed for %s", root, exc_info=True)


def sync_integrated_icon(source: str, render: Renderer, *,
                         appimage: str | None = None, env=None) -> list[str]:
    """Rewrite this AppImage's integrated launcher icons from *source*.

    Returns the paths actually rewritten — empty whenever there is nothing to
    do, which covers every non-AppImage run and every machine whose desktop
    never integrated the app. Each file is written to a temporary sibling and
    then :func:`os.replace`-d, so a failure mid-write cannot leave the
    launcher pointing at a truncated PNG.
    """
    appimage = appimage if appimage is not None else appimage_path(env)
    if not appimage:
        return []
    if not os.path.isfile(source):
        logger.warning("app icon %s missing; leaving the launcher icon alone", source)
        return []

    targets = icon_files(integrated_icon_names(appimage, env), env)
    written: list[str] = []
    for target in targets:
        tmp = ""
        try:
            fd, tmp = tempfile.mkstemp(
                prefix=".corvus-icon-", suffix=_ICON_SUFFIX,
                dir=os.path.dirname(target),
            )
            os.close(fd)
            render(source, tmp, nominal_size(target), {})
            os.replace(tmp, target)
            tmp = ""
            written.append(target)
        except Exception:
            logger.warning("could not rewrite launcher icon %s", target, exc_info=True)
        finally:
            if tmp:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass

    if written:
        refresh_icon_cache(written)
        logger.info("launcher icon updated (%d file(s)) from %s",
                    len(written), os.path.basename(source))
    return written


# ---- the icon a file manager paints on the .AppImage file itself ------------
#
# That one does NOT come from the read-only .DirIcon inside the SquashFS, even
# though the .DirIcon is where it originates. File managers render a file's
# preview out of the freedesktop thumbnail cache under $XDG_CACHE_HOME, and a
# desktop integrator merely *seeds* that cache from the .DirIcon. The cache is
# an ordinary directory of PNGs under $HOME, so the switches reach it — which
# is what makes the file icon changeable without repacking the AppImage.
#
# The spec (Thumbnail Managing Standard) is strict about two things, and both
# are what keeps this from being a hack:
#
#   * the file name is the MD5 of the file's canonical URI, so an entry can
#     only ever describe the one file it is named after;
#   * the PNG must carry Thumb::URI and Thumb::MTime, and a file manager
#     discards any thumbnail whose MTime disagrees with the file on disk.
#
# Honouring both means the desktop treats what we write exactly as it treats
# its own thumbnails. A downloaded AppImage never changes mtime, so the entry
# stays valid indefinitely; if it ever is replaced, the stale thumbnail is
# rejected rather than shown, and the next launch writes a fresh one.

#: Directory name -> the largest edge a thumbnail in it may have.
_THUMB_SIZES = {"normal": 128, "large": 256, "x-large": 512, "xx-large": 1024}
#: Written unconditionally; the two larger ones only where the desktop already
#: keeps that size, so we never create a cache tier nobody asked for.
_THUMB_ALWAYS = ("normal", "large")


def _cache_home(env) -> str:
    home = env.get("XDG_CACHE_HOME") or ""
    if home:
        return home
    return os.path.join(os.path.expanduser("~"), ".cache")


def file_uri(path: str) -> str:
    """The canonical ``file://`` URI a file manager keys its thumbnail on.

    Percent-encoded with GLib's path rules rather than Python's defaults —
    the hash has to match the one GIO computes, byte for byte, or the desktop
    simply never finds the entry. In practice only a space in the path makes
    the two disagree, which is exactly the case worth getting right.
    """
    return "file://" + urllib.parse.quote(os.path.abspath(path),
                                          safe="/!$&'()*+,;=:@")


def thumbnail_name(path: str) -> str:
    """``<md5 of the file URI>.png`` — the spec's name for *path*'s thumbnail."""
    digest = hashlib.md5(file_uri(path).encode("utf-8")).hexdigest()
    return digest + _ICON_SUFFIX


def thumbnail_targets(appimage: str, env=None) -> list[tuple[str, int]]:
    """``(path, size)`` of every thumbnail to write for *appimage*.

    Always the two standard tiers, plus any larger tier the desktop already
    maintains — a HiDPI GNOME reads ``x-large`` first, and leaving a stale
    entry there would show the old icon while the ones we wrote sit unused.
    """
    env = env if env is not None else os.environ
    root = os.path.join(_cache_home(env), "thumbnails")
    name = thumbnail_name(appimage)
    targets = []
    for tier, size in _THUMB_SIZES.items():
        directory = os.path.join(root, tier)
        if tier in _THUMB_ALWAYS or os.path.isdir(directory):
            targets.append((os.path.join(directory, name), size))
    return targets


def _clear_failed_thumbnails(appimage: str, env=None) -> None:
    """Drop any ``fail/`` marker for *appimage*.

    A thumbnailer that once gave up leaves a marker there, and a file manager
    that finds one will not look at the real thumbnail at all — our PNG would
    be written and then ignored forever.
    """
    env = env if env is not None else os.environ
    fail_root = os.path.join(_cache_home(env), "thumbnails", "fail")
    name = thumbnail_name(appimage)
    try:
        producers = os.listdir(fail_root)
    except OSError:
        return
    for producer in producers:
        marker = os.path.join(fail_root, producer, name)
        try:
            os.unlink(marker)
        except OSError:
            continue
        logger.debug("cleared stale failed-thumbnail marker %s", marker)


def sync_appimage_thumbnail(source: str, render: Renderer, *,
                            appimage: str | None = None, env=None) -> list[str]:
    """Write this AppImage's file-manager thumbnail from *source*.

    Returns the thumbnails actually written. Same guarantees as
    :func:`sync_integrated_icon`: nothing raises, every file is written to a
    temporary sibling and then :func:`os.replace`-d, and a run outside an
    AppImage does nothing at all.
    """
    appimage = appimage if appimage is not None else appimage_path(env)
    if not appimage:
        return []
    if not os.path.isfile(source):
        logger.warning("app icon %s missing; leaving the file thumbnail alone", source)
        return []

    try:
        mtime = int(os.path.getmtime(appimage))
    except OSError:
        return []
    # The two keys a file manager validates against the file on disk. Getting
    # MTime wrong does not corrupt anything — the thumbnail is simply treated
    # as stale and regenerated from the .DirIcon, i.e. the old behaviour.
    text = {
        "Thumb::URI": file_uri(appimage),
        "Thumb::MTime": str(mtime),
        "Software": "Corvus GCS",
    }

    written: list[str] = []
    for target, size in thumbnail_targets(appimage, env):
        tmp = ""
        try:
            # 0700 is what the spec asks of the cache dirs, and it is also
            # simply correct: a thumbnail can reveal the contents of a file
            # its owner never shared.
            os.makedirs(os.path.dirname(target), mode=0o700, exist_ok=True)
            fd, tmp = tempfile.mkstemp(prefix=".corvus-thumb-", suffix=_ICON_SUFFIX,
                                       dir=os.path.dirname(target))
            os.close(fd)
            render(source, tmp, size, text)
            os.chmod(tmp, 0o600)
            os.replace(tmp, target)
            tmp = ""
            written.append(target)
        except Exception:
            logger.warning("could not write thumbnail %s", target, exc_info=True)
        finally:
            if tmp:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass

    if written:
        _clear_failed_thumbnails(appimage, env)
        logger.info("file thumbnail updated (%d tier(s)) from %s",
                    len(written), os.path.basename(source))
    return written
