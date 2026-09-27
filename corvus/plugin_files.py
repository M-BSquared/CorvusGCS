"""A plugin's own files, carried inside a settings file.

A settings export can take an operator's plugin along with its settings, so the
next station gets the plugin itself and not only a config it has no code for.
Only plugins from the operator's folder (``~/.corvus/plugins``) travel this
way: a bundled plugin ships with Corvus GCS and is already on the other side.

What is collected is what the plugin asset route could serve
(``plugin_registry.ASSET_SUFFIXES``), from inside the folder only. Symlinks,
hidden files and folders, and the plugin's ``config.json`` stay behind: the
config travels as the plugin's settings, which the operator chooses on its own.

Installing writes the files into ``~/.corvus/plugins/<id>`` by staging them in
a hidden folder beside it and swapping that in, so a failure halfway leaves the
installed copy as it was. The plugin's saved settings, which live in the same
folder, are carried over into the new copy. Discovery skips hidden folders, so
a staging folder left behind by a crash is never loaded as a plugin.

Nothing here runs plugin code. The plugin loads in the browser on the next
reload, like one copied into the folder by hand.

stdlib only.
"""
from __future__ import annotations

import os
import pathlib
import shutil
import tempfile
import uuid
from typing import Any

from .plugin_config import CONFIG_NAME
from .plugin_registry import ASSET_SUFFIXES, MANIFEST_NAME, is_valid_id, user_plugins_dir

# A plugin is a few scripts and perhaps an icon. The limits keep a settings
# file small enough to import in one request, and stop a folder that holds a
# video or a dataset from being carried along by accident.
MAX_PLUGIN_BYTES = 4 * 1024 * 1024
MAX_FILES = 200
MAX_DEPTH = 8
MAX_PATH_CHARS = 240

_WINDOWS_RESERVED = frozenset(
    {"CON", "PRN", "AUX", "NUL"}
    | {f"COM{i}" for i in range(1, 10)}
    | {f"LPT{i}" for i in range(1, 10)}
)
_BAD_CHARS = frozenset('<>:"|?*')


def clean_path(rel: Any) -> str | None:
    """*rel* as a safe path inside a plugin folder, or None when it is not one.

    Forward slashes, relative, no ``.`` or ``..`` step, no hidden part, a
    suffix the asset route serves, and a name every supported platform can
    write. The plugin's own ``config.json`` at the top is refused: it is the
    plugin's settings, not one of its files.
    """
    if not isinstance(rel, str) or not rel or len(rel) > MAX_PATH_CHARS:
        return None
    rel = rel.replace("\\", "/")
    if rel.startswith("/") or (len(rel) > 1 and rel[1] == ":"):
        return None
    parts = rel.split("/")
    if len(parts) > MAX_DEPTH:
        return None
    for part in parts:
        if part in ("", ".", "..") or part.startswith(".") or part.endswith((" ", ".")):
            return None
        if any(c in _BAD_CHARS or ord(c) < 32 for c in part):
            return None
        if part.split(".")[0].upper() in _WINDOWS_RESERVED:
            return None
    if parts == [CONFIG_NAME]:
        return None
    if pathlib.PurePosixPath(rel).suffix.lower() not in ASSET_SUFFIXES:
        return None
    return rel


def collect(plugin_dir: str | os.PathLike[str]) -> dict[str, bytes]:
    """Every file of the plugin in *plugin_dir* that travels, by relative path.

    Raises ValueError when the plugin is larger than MAX_PLUGIN_BYTES or holds
    more than MAX_FILES files, and OSError when the folder cannot be read.
    """
    root = pathlib.Path(plugin_dir)
    out: dict[str, bytes] = {}
    total = 0
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames
                             if not d.startswith(".") and d != "__pycache__"
                             and not os.path.islink(os.path.join(dirpath, d)))
        for name in sorted(filenames):
            full = pathlib.Path(dirpath) / name
            rel = clean_path(full.relative_to(root).as_posix())
            if rel is None or full.is_symlink() or not full.is_file():
                continue
            data = full.read_bytes()
            total += len(data)
            if total > MAX_PLUGIN_BYTES:
                raise ValueError(f"larger than {MAX_PLUGIN_BYTES // (1024 * 1024)} MB")
            out[rel] = data
            if len(out) > MAX_FILES:
                raise ValueError(f"made of more than {MAX_FILES} files")
    return out


def check(files: Any) -> dict[str, bytes]:
    """*files* if it is a plugin that may be installed; ValueError otherwise.

    Every path clean, every value bytes, a ``plugin.json`` among them, and
    within the same limits :func:`collect` applies.
    """
    if not isinstance(files, dict) or not files:
        raise ValueError("no files")
    if len(files) > MAX_FILES:
        raise ValueError(f"more than {MAX_FILES} files")
    total = 0
    out: dict[str, bytes] = {}
    for rel, data in files.items():
        clean = clean_path(rel)
        if clean is None or clean != rel or not isinstance(data, bytes):
            raise ValueError(f"unusable file {rel!r}")
        total += len(data)
        out[clean] = data
    if total > MAX_PLUGIN_BYTES:
        raise ValueError(f"larger than {MAX_PLUGIN_BYTES // (1024 * 1024)} MB")
    if MANIFEST_NAME not in out:
        raise ValueError(f"no {MANIFEST_NAME}")
    return out


def install(plugin_id: str, files: dict[str, bytes], user_dir: str | None = None) -> str:
    """Install *files* as plugin *plugin_id* in the operator's plugin folder.

    Replaces an installed copy with the same id, keeping its saved settings.
    Returns the plugin's folder. Raises ValueError for an invalid id or file
    set, OSError when the disk refuses; either way the installed copy, if
    any, is left as it was.
    """
    if not is_valid_id(plugin_id):
        raise ValueError(f"invalid plugin id {plugin_id!r}")
    files = check(files)
    root = pathlib.Path(user_dir if user_dir is not None else user_plugins_dir())
    root.mkdir(parents=True, exist_ok=True)
    target = root / plugin_id
    staging = pathlib.Path(tempfile.mkdtemp(prefix=f".{plugin_id}.import-", dir=root))
    try:
        for rel, data in files.items():
            path = staging.joinpath(*rel.split("/"))
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        settings = target / CONFIG_NAME
        if settings.is_file():
            shutil.copyfile(settings, staging / CONFIG_NAME)
        _swap_in(staging, target)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return str(target)


def _swap_in(staging: pathlib.Path, target: pathlib.Path) -> None:
    """Put *staging* where *target* is, and remove what was there."""
    old: pathlib.Path | None = None
    if target.is_symlink() or target.exists():
        if not target.is_dir():
            raise OSError(f"{target} exists and is not a folder")
        old = target.with_name(f".{target.name}.old-{uuid.uuid4().hex[:8]}")
        os.replace(target, old)
    try:
        os.replace(staging, target)
    except OSError:
        if old is not None:
            os.replace(old, target)
        raise
    if old is None:
        return
    # A symlinked plugin (a developer's working copy) loses the link, never
    # the folder it points at.
    if old.is_symlink():
        old.unlink()
    else:
        shutil.rmtree(old, ignore_errors=True)
