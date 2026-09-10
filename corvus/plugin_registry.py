"""Discovery of drop-in plugin folders for the TOOLS tab.

A plugin is a folder with a ``plugin.json`` manifest and one or more frontend
files beside it. Two roots are scanned, in this order:

1. **bundled** — ``<repo>/plugins``, the plugins that ship with Corvus. Inside
   a packaged artifact this lives next to ``corvus/`` and ``src/``, so it is
   read-only and replaced wholesale by an update.
2. **user** — ``~/.corvus/plugins``, the folder the operator drops plugins
   into. It survives updates, which is why the Settings button opens *this*
   one.

A user plugin whose id collides with a bundled one wins: that is how an
operator patches a shipped plugin without editing inside the app bundle.

The scan happens on request, not at import, and never raises: an unreadable
folder, a malformed manifest or a plugin with no scripts is skipped with a
warning. A field laptop must start with a broken plugin on disk, minus that
plugin — never not at all.

Nothing here executes plugin code. The frontend fetches the manifests over
``GET /api/plugins`` and loads the scripts itself, so the plugin runs in the
browser with exactly the reach the ``Corvus.plugins`` api gives it.

stdlib only.
"""
from __future__ import annotations

import json
import logging
import os
import pathlib
import re
from typing import Any

from .paths import corvus_path

logger = logging.getLogger("corvus.plugins")

# Plugin ids address a folder and end up in a URL, so they are deliberately
# narrow: no separators, no leading dot, nothing that could walk out of the
# plugin's own directory once it is pasted into a path.
_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")

# What a plugin folder is allowed to serve. Everything a frontend plugin needs
# and nothing that would turn the asset route into a general file server: no
# ``.py``, no ``.sh``, no extensionless files.
ASSET_SUFFIXES: frozenset[str] = frozenset({
    ".js", ".mjs", ".css", ".json", ".map",
    ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".ico",
    ".woff", ".woff2", ".ttf", ".otf",
    ".html", ".md", ".txt", ".csv",
})

MANIFEST_NAME = "plugin.json"

# Written into a freshly created user plugin folder so an operator who opens it
# from Settings finds instructions rather than an empty window.
_README = """# Corvus GCS plugins

Drop one folder per plugin into this directory. Corvus picks them up on the
next start.

    plugins/
      my-plugin/
        plugin.json
        my-plugin.js
        my-plugin.css      (optional)

`plugin.json` describes the plugin:

    {
      "id": "my-plugin",
      "name": "My Plugin",
      "icon": "puzzle",
      "description": "What it does, one line.",
      "version": "1.0.0",
      "author": "Your name",
      "order": 100,
      "scripts": ["my-plugin.js"],
      "styles": ["my-plugin.css"]
    }

`icon` is a Lucide icon name (https://lucide.dev). `id` defaults to the folder
name; `scripts` defaults to `<id>.js`. `order` decides where the card sits in
the grid (lower is earlier, default 100); ties are broken by name.

Each script registers the plugin when it loads:

    Corvus.plugins.register("my-plugin", {
      name: "My Plugin",
      icon: "puzzle",
      description: "What it does, one line.",
      init: function (containerEl, api) { /* build your UI */ },
      destroy: function (containerEl) { /* tear it down again */ },
    });

`api` is documented at the top of `src/js/plugins.js`. The plugins shipped in
the application's own `plugins/` folder are complete worked examples —
`ssh-launcher` for a form, saved settings, a backend call and a terminal,
`vibration` for a live chart on the telemetry stream. Copy one and start from
there.
"""


def user_plugins_dir() -> str:
    """The operator's plugin folder: ``~/.corvus/plugins``."""
    return corvus_path("plugins")


def bundled_plugins_dir() -> str:
    """The plugin folder that ships with Corvus: ``<repo>/plugins``.

    Resolved the same way ``corvus/server.py`` resolves ``src/`` — as a sibling
    of the ``corvus`` package — so it points inside the artifact when Corvus
    runs from one, and at the repository when it runs from a checkout.
    """
    return str(pathlib.Path(__file__).resolve().parent.parent / "plugins")


def ensure_user_plugins_dir() -> str:
    """Create ``~/.corvus/plugins`` (with its README) and return the path.

    Idempotent, and never raises: a read-only home directory costs the folder,
    not the application. The README is only written when absent, so an operator
    who deleted it does not get it back on every launch.
    """
    path = user_plugins_dir()
    try:
        os.makedirs(path, exist_ok=True)
        readme = os.path.join(path, "README.md")
        if not os.path.exists(readme):
            with open(readme, "w", encoding="utf-8") as fh:
                fh.write(_README)
    except OSError as exc:
        logger.warning("could not create plugin folder %s: %s", path, exc)
    return path


def _clean_str(raw: Any, default: str = "") -> str:
    """A stripped string, or *default* for anything that is not one."""
    return raw.strip() if isinstance(raw, str) and raw.strip() else default


# Where a plugin lands in the grid when its manifest does not say. Sitting in
# the middle leaves room on both sides, so a plugin can be pulled to the front
# or pushed to the back without renumbering the others.
DEFAULT_ORDER = 100


def _clean_order(raw: Any) -> int:
    """The manifest's ``order``, or DEFAULT_ORDER for anything that is not one.

    Without this the grid could only be arranged by renaming folders, which is
    a poor thing to ask of someone whose plugin is already installed. Booleans
    are excluded explicitly — ``True`` is a valid ``int`` in Python and would
    silently mean "first".
    """
    if isinstance(raw, bool) or not isinstance(raw, (int, float)):
        return DEFAULT_ORDER
    try:
        return int(raw)
    except (TypeError, ValueError, OverflowError):
        return DEFAULT_ORDER


def _clean_file_list(raw: Any, plugin_dir: pathlib.Path) -> list[str]:
    """Keep the entries of *raw* that name a real, servable file in the folder.

    Each entry is resolved against *plugin_dir* and dropped unless it stays
    inside it, carries an allowed suffix, and exists. A manifest listing a file
    that is not there is a typo, and a plugin missing a script it needs is
    better skipped than half-loaded.
    """
    if not isinstance(raw, list):
        return []
    out: list[str] = []
    for entry in raw:
        rel = _clean_str(entry)
        if not rel:
            continue
        resolved = resolve_asset(plugin_dir, rel)
        if resolved is None:
            logger.warning("plugin %s: dropping unusable file %r", plugin_dir.name, rel)
            continue
        # Store the path as the manifest spelled it (minus a leading "./"),
        # because that is what the frontend turns into a URL.
        normalized = rel.replace("\\", "/").lstrip("./")
        if normalized not in out:
            out.append(normalized)
    return out


def resolve_asset(plugin_dir: str | os.PathLike[str], rel: str) -> pathlib.Path | None:
    """Resolve *rel* inside *plugin_dir*, or None when it is not servable.

    The single containment check behind both the manifest parser and the HTTP
    asset route. Returns the resolved path only when it stays inside the plugin
    folder (after symlink resolution), has an allowed suffix, and is a regular
    file. Every failure — traversal, bad suffix, missing file, OS error — is a
    plain None, so callers have exactly one case to handle.
    """
    if not isinstance(rel, str) or not rel:
        return None
    # Reject absolute paths and drive letters before they can defeat the join.
    if rel.startswith(("/", "\\")) or (len(rel) > 1 and rel[1] == ":"):
        return None
    try:
        root = pathlib.Path(plugin_dir).resolve()
        target = (root / rel).resolve()
        target.relative_to(root)
    except (OSError, ValueError, RuntimeError):
        return None
    if target.suffix.lower() not in ASSET_SUFFIXES:
        return None
    if not target.is_file():
        return None
    return target


def _read_manifest(plugin_dir: pathlib.Path, source: str) -> dict[str, Any] | None:
    """Parse one plugin folder into a manifest dict, or None to skip it."""
    manifest_path = plugin_dir / MANIFEST_NAME
    try:
        data = json.loads(manifest_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None                      # not a plugin folder; not worth a log line
    except (OSError, ValueError) as exc:
        logger.warning("plugin %s: unreadable %s (%s); skipped",
                       plugin_dir.name, MANIFEST_NAME, exc)
        return None
    if not isinstance(data, dict):
        logger.warning("plugin %s: %s is not a JSON object; skipped",
                       plugin_dir.name, MANIFEST_NAME)
        return None

    plugin_id = _clean_str(data.get("id"), plugin_dir.name)
    if not _ID_RE.match(plugin_id):
        logger.warning("plugin %s: invalid id %r; skipped", plugin_dir.name, plugin_id)
        return None

    scripts = _clean_file_list(data.get("scripts"), plugin_dir)
    if not scripts:
        # A manifest with no usable "scripts" still has the conventional
        # single-file layout to fall back on before the folder is given up on.
        fallback = _clean_file_list([f"{plugin_id}.js"], plugin_dir)
        scripts = fallback
    if not scripts:
        logger.warning("plugin %s: no loadable script; skipped", plugin_dir.name)
        return None

    return {
        "id": plugin_id,
        "name": _clean_str(data.get("name"), plugin_id),
        "order": _clean_order(data.get("order")),
        "icon": _clean_str(data.get("icon"), "puzzle"),
        "description": _clean_str(data.get("description")),
        "version": _clean_str(data.get("version")),
        "author": _clean_str(data.get("author")),
        "scripts": scripts,
        "styles": _clean_file_list(data.get("styles"), plugin_dir),
        "source": source,
        "dir": str(plugin_dir),
    }


def _scan_root(root: str, source: str) -> list[dict[str, Any]]:
    """Read every plugin folder directly under *root*; never raises."""
    try:
        entries = sorted(os.scandir(root), key=lambda e: e.name.lower())
    except OSError:
        # Missing root is the normal case before the operator has dropped
        # anything in; anything else is equally not worth failing a start over.
        return []
    out: list[dict[str, Any]] = []
    for entry in entries:
        try:
            if not entry.is_dir():
                continue
        except OSError:
            continue
        manifest = _read_manifest(pathlib.Path(entry.path), source)
        if manifest is not None:
            out.append(manifest)
    return out


def discover(user_dir: str | None = None, bundled_dir: str | None = None) -> list[dict[str, Any]]:
    """Return the manifests of every installed plugin, bundled ones first.

    A user plugin replaces a bundled one with the same id, wherever the bundled
    one sat. The result is sorted by the manifest's ``order`` and then by name,
    so which root a plugin came from never decides where it appears. The
    directories are parameters so the tests can point the scan at a tmp_path
    instead of the operator's real home.
    """
    bundled = _scan_root(bundled_dir if bundled_dir is not None else bundled_plugins_dir(), "bundled")
    user = _scan_root(user_dir if user_dir is not None else user_plugins_dir(), "user")

    merged: list[dict[str, Any]] = list(bundled)
    index = {m["id"]: i for i, m in enumerate(merged)}
    for manifest in user:
        existing = index.get(manifest["id"])
        if existing is None:
            index[manifest["id"]] = len(merged)
            merged.append(manifest)
        else:
            logger.info("plugin %s: user copy overrides the bundled one", manifest["id"])
            merged[existing] = manifest
    # `order` first, then the display name, so the grid is arrangeable from the
    # manifest and everything that does not care still lands in a stable,
    # readable order rather than in whatever order the filesystem answered in.
    merged.sort(key=lambda m: (m["order"], m["name"].lower()))
    return merged


def find_dir(plugin_id: str,
             user_dir: str | None = None,
             bundled_dir: str | None = None) -> str | None:
    """Return the folder a discovered plugin was loaded from, else None.

    Goes through :func:`discover` rather than joining the id onto a root, so
    the asset route can only ever reach a folder that actually parsed as a
    plugin, and so it honours the same user-overrides-bundled rule.
    """
    if not isinstance(plugin_id, str) or not _ID_RE.match(plugin_id):
        return None
    for manifest in discover(user_dir, bundled_dir):
        if manifest["id"] == plugin_id:
            return manifest["dir"]
    return None


def public_list(user_dir: str | None = None,
                bundled_dir: str | None = None) -> list[dict[str, Any]]:
    """The discovered plugins as the HTTP layer returns them.

    Drops the absolute ``dir`` — the browser has no use for a server-side path,
    and the Settings page shows the two roots once instead of per plugin.
    """
    out = []
    for manifest in discover(user_dir, bundled_dir):
        public = {k: v for k, v in manifest.items() if k != "dir"}
        out.append(public)
    return out
