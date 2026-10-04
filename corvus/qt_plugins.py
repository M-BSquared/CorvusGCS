"""Qt plugins that macOS has flagged hidden, made loadable again.

Qt does not load a plugin it cannot see, and on macOS it does not see a file
that carries the hidden flag (``UF_HIDDEN``): its plugin loader lists each
plugin folder without hidden entries. A file sync agent that manages the folder
sets that flag on every file under a dot-directory in it, ``.venv`` included.
The agent does so in the background, a few files a second after they are
written, and sets it again after it is cleared. A checkout in such a place runs
fine right after ``pip install`` and stops starting minutes later:
Qt finds no platform plugin, and it does not raise but prints "Could not find
the Qt platform plugin" and aborts the process.

So each plugin is linked into a folder of links under the temporary directory,
which nothing flags, and Qt is pointed there. Qt reads a link's own flags, and
the plugin behind it still finds its Qt libraries, so nothing in the
installation is touched. A packaged build, whose plugins carry no flag, is
left alone, as is every platform without the flag.

stdlib only: the plugin folder is handed in, so this never imports Qt.
"""
from __future__ import annotations

import hashlib
import os
import shutil
import stat
import tempfile
from collections.abc import Callable
from pathlib import Path

_UF_HIDDEN = getattr(stat, "UF_HIDDEN", 0)


def flagged_hidden(path: str | os.PathLike) -> bool:
    """Whether *path* carries the hidden flag. Never true where files have no flags."""
    try:
        flags = getattr(os.stat(path), "st_flags", 0)
    except OSError:
        return False
    return bool(_UF_HIDDEN and flags & _UF_HIDDEN)


def links_dir(root: str | os.PathLike, base: str | os.PathLike | None = None) -> Path:
    """Where the links to the plugins under *root* live: one folder per plugin root."""
    digest = hashlib.sha256(str(Path(root).resolve()).encode("utf-8")).hexdigest()[:12]
    return Path(base if base is not None else tempfile.gettempdir()) / f"corvus-qt-plugins-{digest}"


def _link(link: Path, target: Path) -> None:
    """Make *link* point at *target*, replacing whatever was there in one step."""
    try:
        if os.readlink(link) == str(target):
            return
    except OSError:
        pass
    # A dot name, so a Qt scanning this folder meanwhile skips the half-made link.
    spare = link.with_name(f".{link.name}.{os.getpid()}")
    try:
        spare.unlink()
    except FileNotFoundError:
        pass
    os.symlink(target, spare)
    os.replace(spare, link)


def _remove(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink()


def visible_plugin_dir(root: str | os.PathLike, *, base: str | os.PathLike | None = None,
                       hidden: Callable[[Path], bool] = flagged_hidden) -> str | None:
    """A folder of links to the Qt plugins under *root*, when Qt cannot see them.

    ``None`` when there is nothing to do: no platform plugins under *root*, or
    none of them flagged hidden. Otherwise the folder, made or brought up to
    date: a link to a plugin that is gone is removed, one that points at
    another file is replaced. *base* and *hidden* stand in for the temporary
    directory and :func:`flagged_hidden`, for the tests. Raises ``OSError``
    when the links cannot be made.
    """
    root = Path(root)
    try:
        flagged = any(hidden(p) for p in (root / "platforms").iterdir() if p.is_file())
    except OSError:
        return None
    if not flagged:
        return None
    links = links_dir(root, base)
    links.mkdir(parents=True, exist_ok=True)
    folders = {p.name: p for p in root.iterdir() if p.is_dir()}
    for entry in links.iterdir():
        if entry.name not in folders:
            _remove(entry)
    for name, folder in sorted(folders.items()):
        mirror = links / name
        mirror.mkdir(exist_ok=True)
        plugins = {p.name: p for p in folder.iterdir() if p.is_file()}
        for entry in mirror.iterdir():
            if entry.name not in plugins:
                _remove(entry)
        for plugin_name, plugin in sorted(plugins.items()):
            _link(mirror / plugin_name, plugin)
    return str(links)
