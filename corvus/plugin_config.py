"""Per-plugin config files, kept apart from the application's own config.

Each plugin's saved settings live in a file of their own::

    ~/.corvus/plugins/<plugin id>/config.json

and nowhere else. Not in ``~/.corvus/config.json``, for two reasons:

* **Copyable.** A plugin set up on one laptop is deployed on the next by
  copying that one file (or the whole plugin folder) to the same place. The
  main config cannot be copied that way: it carries the machine's own serial
  port, folders and SSH passwords.
* **Separate.** A plugin writing its state cannot touch the application's, and
  resetting the application's config does not wipe a plugin's shelf.

The file holds exactly the object the plugin saved through
``api.saveSettings``, as indented JSON, so it can be read and edited by hand.
It sits in the operator's plugin folder even for a plugin that ships inside
the application: the bundled folder is read-only and replaced by an update.
A folder holding only a ``config.json`` has no ``plugin.json``, so discovery
skips it without a word; a user plugin that overrides a bundled one finds its
config already beside it.

Nothing secret belongs here. The file is written with ordinary permissions,
and a plugin references a saved SSH connection by name rather than storing a
password.

Earlier versions kept these objects under the main config's ``plugins`` key.
:func:`migrate` moves them out once, at startup.

stdlib only.
"""
from __future__ import annotations

import json
import logging
import os
import pathlib
import tempfile
import threading
from typing import Any

from .plugin_registry import is_valid_id, user_plugins_dir

logger = logging.getLogger("corvus.plugins")

CONFIG_NAME = "config.json"

# One writer at a time: a merge is read-modify-write, and two plugins (or two
# tabs of one) saving at once must not interleave on the same file.
_write_lock = threading.RLock()


def config_path(plugin_id: str, user_dir: str | None = None) -> str | None:
    """Where *plugin_id* keeps its config, or None for an id that is not one.

    The id is checked with the registry's own rule before it is joined onto
    a path, so a request cannot name a file outside the plugin folder.
    """
    if not is_valid_id(plugin_id):
        return None
    root = user_dir if user_dir is not None else user_plugins_dir()
    return os.path.join(root, plugin_id, CONFIG_NAME)


def load(plugin_id: str, user_dir: str | None = None) -> dict[str, Any] | None:
    """The saved config of *plugin_id*, or None when it has none.

    Never raises. A file that is unreadable or not a JSON object is logged and
    treated as absent: a plugin starting from empty settings is recoverable, a
    ground station that fails to start over a hand-edited typo is not.
    """
    path = config_path(plugin_id, user_dir)
    if path is None:
        return None
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as exc:
        logger.warning("plugin %s: unreadable %s (%s); ignored", plugin_id, path, exc)
        return None
    if not isinstance(data, dict):
        logger.warning("plugin %s: %s is not a JSON object; ignored", plugin_id, path)
        return None
    return data


def load_all(user_dir: str | None = None) -> dict[str, dict[str, Any]]:
    """Every plugin config under the plugin folder, keyed by plugin id.

    Keyed by folder name, which is the id :func:`config_path` wrote it under.
    A config whose plugin is not installed is still returned: it is harmless,
    and it is what makes a config copied ahead of its plugin work once the
    plugin arrives.
    """
    root = user_dir if user_dir is not None else user_plugins_dir()
    try:
        entries = sorted(os.scandir(root), key=lambda e: e.name)
    except OSError:
        return {}
    out: dict[str, dict[str, Any]] = {}
    for entry in entries:
        try:
            if not entry.is_dir():
                continue
        except OSError:
            continue
        data = load(entry.name, root)
        if data is not None:
            out[entry.name] = data
    return out


def save(plugin_id: str, settings: dict[str, Any], user_dir: str | None = None) -> None:
    """Write *settings* as the whole config of *plugin_id*.

    Atomic, like the main config: written to a temp file beside the target and
    ``os.replace``'d over it, so a crash mid-write leaves the old file rather
    than half of a new one. Raises ValueError for an invalid id or a value
    that is not JSON, OSError when the disk refuses.
    """
    path = config_path(plugin_id, user_dir)
    if path is None:
        raise ValueError(f"invalid plugin id {plugin_id!r}")
    text = json.dumps(settings, indent=2, ensure_ascii=False) + "\n"
    target = pathlib.Path(path)
    with _write_lock:
        target.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp_name = tempfile.mkstemp(
            prefix=target.name + ".", suffix=".tmp", dir=str(target.parent),
        )
        tmp_path = pathlib.Path(tmp_name)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                fh.write(text)
                fh.flush()
                os.fsync(fh.fileno())
            os.replace(tmp_path, target)
        except OSError:
            try:
                tmp_path.unlink()
            except OSError:
                pass
            raise


def update(plugin_id: str, patch: dict[str, Any], *, replace: bool = False,
           user_dir: str | None = None) -> dict[str, Any]:
    """Merge *patch* into the config of *plugin_id* (or replace it) and save.

    Returns the object now on disk. The read and the write happen under one
    lock, so two merges cannot each drop the other's key.
    """
    with _write_lock:
        existing = None if replace else load(plugin_id, user_dir)
        merged = {**existing, **patch} if existing else dict(patch)
        save(plugin_id, merged, user_dir)
        return merged


def migrate(legacy: dict[str, Any] | None, user_dir: str | None = None) -> bool:
    """Move settings out of the main config's old ``plugins`` key into files.

    A plugin that already has a file keeps it, even one that does not parse:
    the file is newer than anything left in the main config, and it may have
    been copied in on purpose. Returns True when every legacy entry is now accounted for, so the
    caller may drop the key; False when a write failed, so the caller keeps it
    and nothing is lost.
    """
    if not isinstance(legacy, dict) or not legacy:
        return True
    complete = True
    for plugin_id, settings in legacy.items():
        if not isinstance(settings, dict):
            continue
        path = config_path(plugin_id, user_dir)
        if path is None or os.path.exists(path):
            continue
        try:
            save(plugin_id, settings, user_dir)
            logger.info("plugin %s: settings moved to %s", plugin_id, path)
        except (OSError, ValueError) as exc:
            logger.warning("plugin %s: could not move settings out of the main config: %s",
                           plugin_id, exc)
            complete = False
    return complete
