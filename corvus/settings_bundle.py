"""One file that carries a whole station setup: settings export and import.

The Settings page writes it and reads it back, so a station can be backed up,
restored after a reinstall, or copied to the next laptop. It holds:

* ``config``: the application config (``~/.corvus/config.json``), exactly as
  :func:`corvus.config._config_to_dict` writes it, minus the legacy
  ``plugins`` key;
* ``plugins``: the chosen plugins' own config files
  (``corvus/plugin_config.py``);
* ``plugin_files``: the chosen plugins themselves, as base64 per relative
  path, for plugins installed in the operator's folder
  (``corvus/plugin_files.py``);
* ``logo``: the company logo as base64, when one is set;
* ``browser``: the interface state the browser keeps for itself, such as where
  the flight HUD and the virtual joystick sit. This module never reads it. It
  is carried through so the backup is one file rather than two, and
  ``src/js/settings-transfer.js`` owns what goes into it.

Secrets (SSH passwords, the NTRIP password, camera passwords, map service keys)
stay out unless the operator asks for them, because the file is the kind of
thing that gets passed on. A file without them never erases the ones already
on the importing station: an entry that is still the same entry (same name,
host and user; same camera address; same caster) keeps its stored secret, and
nothing is ever handed to a host it was not stored for.

Export and import are both by section, so an operator can write a file with
only the interface in it, or take the window layout from a full file and leave
the serial port and folders of this machine alone. ``sections`` names what a
file carries; a section it does not carry is never imported from it, because
importing a section resets every key the file leaves out. Every key of the
application config belongs to exactly one section; the test suite fails when a
new key is added without one.

Format 2 added ``sections`` and ``plugin_files``. A format 1 file carries every
section and no plugin files, and is still read.

stdlib only.
"""
from __future__ import annotations

import base64
import binascii
import copy
import time
from collections.abc import Iterable
from typing import Any

from . import plugin_files as plugin_files_mod
from .plugin_registry import is_valid_id
from .version import get_version

KIND = "corvus-settings"
FORMAT = 2
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
MAX_LOGO_BYTES = 4 * 1024 * 1024

# Section id -> the application config keys it carries. "plugins" and "layout"
# have none: the first is the plugin files, the second is browser state only.
SECTIONS: dict[str, tuple[str, ...]] = {
    "interface": ("theme", "ui", "controls", "branding", "updates", "review"),
    "map": ("map", "map_tokens", "tile_sources"),
    "connections": ("mavlink_connection", "autoconnect", "forwarding",
                    "stream_rates", "ssh_connections"),
    "vehicle": ("battery", "remote_id", "rtk", "video", "parameters", "checklists"),
    "folders": ("http_port", "tile_cache_dir", "tlog_dir", "params_dir",
                "firmware_dir", "log_download_dir", "missions_dir"),
    "plugins": (),
    "layout": (),
}

# Config keys a bundle never carries. ``plugins`` is the legacy home of plugin
# settings; they travel as the bundle's own ``plugins`` block instead.
EXCLUDED_KEYS: frozenset[str] = frozenset({"plugins"})


def section_of(key: str) -> str | None:
    """The section a config key is imported with, or None for none."""
    for section, keys in SECTIONS.items():
        if key in keys:
            return section
    return None


def redact(config: dict[str, Any]) -> dict[str, Any]:
    """A deep copy of *config* with every secret taken out.

    The same four secrets :func:`corvus.config.to_public_dict` keeps out of
    HTTP responses, removed the same way: the SSH and camera passwords are
    dropped, the NTRIP password is blanked, and the map keys go entirely.
    """
    out = copy.deepcopy(config)
    out.pop("map_tokens", None)
    for entry in out.get("ssh_connections") or []:
        if isinstance(entry, dict):
            entry.pop("password", None)
    rtk_block = out.get("rtk")
    if isinstance(rtk_block, dict) and isinstance(rtk_block.get("ntrip"), dict):
        rtk_block["ntrip"]["password"] = ""
    video_block = out.get("video")
    if isinstance(video_block, dict):
        for stream in video_block.get("streams") or []:
            if isinstance(stream, dict):
                stream.pop("password", None)
    return out


def build(config: dict[str, Any], *, sections: Iterable[str] | None = None,
          plugins: dict[str, Any] | None = None,
          plugin_files: dict[str, dict[str, bytes]] | None = None,
          logo: bytes | None = None, browser: dict[str, Any] | None = None,
          include_secrets: bool = False) -> dict[str, Any]:
    """Assemble the bundle written to disk by an export.

    *config* is the serialized application config and *sections* the parts
    to write (every one when None). Only the config keys of those sections go
    in; the plugins only with ``plugins``, the logo only with ``interface``.
    *plugins* and *plugin_files* are already the chosen plugins. The result is
    plain JSON data; the caller writes it.
    """
    chosen = set(SECTIONS) if sections is None else set(sections) & set(SECTIONS)
    cfg = {k: v for k, v in config.items()
           if k not in EXCLUDED_KEYS and section_of(k) in chosen}
    if not include_secrets:
        cfg = redact(cfg)
    with_plugins = "plugins" in chosen
    bundle: dict[str, Any] = {
        "kind": KIND,
        "format": FORMAT,
        "product": "Corvus GCS",
        "version": get_version(),
        "exported_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "secrets": bool(include_secrets),
        "sections": [s for s in SECTIONS if s in chosen],
        "config": cfg,
        "plugins": ({k: v for k, v in (plugins or {}).items() if isinstance(v, dict)}
                    if with_plugins else {}),
        "plugin_files": ({pid: {rel: base64.b64encode(data).decode("ascii")
                                for rel, data in sorted(files.items())}
                          for pid, files in (plugin_files or {}).items()}
                         if with_plugins else {}),
        "browser": _clean_browser(browser),
    }
    if logo and "interface" in chosen:
        bundle["logo"] = base64.b64encode(logo).decode("ascii")
    return bundle


def _clean_browser(raw: Any) -> dict[str, str]:
    """Browser entries as ``{key: string}``, anything else dropped."""
    if not isinstance(raw, dict):
        return {}
    return {k: v for k, v in raw.items()
            if isinstance(k, str) and k.startswith("corvus.") and isinstance(v, str)}


def parse(data: Any) -> dict[str, Any]:
    """Check that *data* is a settings bundle and return it normalized.

    Raises ValueError with a sentence an operator can act on. A bundle from a
    newer format is refused rather than half applied: whatever that version
    added, this one would silently drop. So is a bundle with a plugin that
    could not be installed as it stands.
    """
    if not isinstance(data, dict) or data.get("kind") != KIND:
        raise ValueError("This is not a Corvus GCS settings file.")
    fmt = data.get("format")
    if isinstance(fmt, bool) or not isinstance(fmt, int) or fmt < 1:
        raise ValueError("The settings file has no valid format number.")
    if fmt > FORMAT:
        raise ValueError("The settings file was written by a newer Corvus GCS. "
                         "Update this station first.")
    config = data.get("config")
    if not isinstance(config, dict):
        raise ValueError("The settings file carries no configuration.")
    if fmt < 2:
        sections = set(SECTIONS)
    else:
        raw_sections = data.get("sections")
        if not isinstance(raw_sections, list):
            raise ValueError("The settings file does not say which settings it carries.")
        sections = {s for s in raw_sections if isinstance(s, str) and s in SECTIONS}
    plugins = data.get("plugins")
    logo = None
    if data.get("logo") is not None and "interface" in sections:
        logo = decode_logo(data.get("logo"))
    with_plugins = "plugins" in sections
    return {
        "version": data.get("version") if isinstance(data.get("version"), str) else "",
        "exported_at": data.get("exported_at") if isinstance(data.get("exported_at"), str) else "",
        "secrets": data.get("secrets") is True,
        "sections": [s for s in SECTIONS if s in sections],
        "config": {k: v for k, v in config.items()
                   if k not in EXCLUDED_KEYS and section_of(k) in sections},
        "plugins": ({k: v for k, v in plugins.items() if isinstance(k, str) and isinstance(v, dict)}
                    if with_plugins and isinstance(plugins, dict) else {}),
        "plugin_files": (decode_plugin_files(data.get("plugin_files"))
                         if with_plugins and fmt >= 2 else {}),
        "logo": logo,
        "browser": _clean_browser(data.get("browser")),
    }


def decode_plugin_files(raw: Any) -> dict[str, dict[str, bytes]]:
    """A bundle's ``plugin_files`` as ``{plugin id: {path: bytes}}``.

    Every plugin is checked the way an install checks it
    (:func:`corvus.plugin_files.check`), so a file that parses here installs.
    """
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise ValueError("The plugins in the settings file are unreadable.")
    out: dict[str, dict[str, bytes]] = {}
    for plugin_id, files in raw.items():
        if not is_valid_id(plugin_id) or not isinstance(files, dict):
            raise ValueError("The plugins in the settings file are unreadable.")
        decoded: dict[str, bytes] = {}
        try:
            for rel, blob in files.items():
                if not isinstance(blob, str):
                    raise ValueError(rel)
                decoded[rel] = base64.b64decode(blob, validate=True)
            out[plugin_id] = plugin_files_mod.check(decoded)
        except (binascii.Error, ValueError) as exc:
            raise ValueError(f"The plugin {plugin_id} in the settings file is unusable "
                             f"({exc}).") from exc
    return out


def decode_logo(raw: Any) -> bytes:
    """The PNG bytes of a bundle's ``logo``; ValueError when it is not one."""
    if not isinstance(raw, str):
        raise ValueError("The company logo in the settings file is unreadable.")
    try:
        blob = base64.b64decode(raw, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise ValueError("The company logo in the settings file is unreadable.") from exc
    if len(blob) > MAX_LOGO_BYTES:
        raise ValueError("The company logo in the settings file is larger than 4 MB.")
    if not blob.startswith(PNG_MAGIC):
        raise ValueError("The company logo in the settings file is not a PNG image.")
    return blob


def normalize_sections(raw: Any) -> set[str]:
    """The known section ids in *raw*; every section when *raw* is None."""
    if raw is None:
        return set(SECTIONS)
    if not isinstance(raw, list):
        raise ValueError("sections must be a list")
    return {s for s in raw if isinstance(s, str) and s in SECTIONS}


def normalize_ids(raw: Any, available: Iterable[str], name: str) -> set[str]:
    """The plugin ids in *raw* that are also in *available*; all of them when None."""
    pool = set(available)
    if raw is None:
        return pool
    if not isinstance(raw, list):
        raise ValueError(f"{name} must be a list")
    return {p for p in raw if isinstance(p, str) and p in pool}


def merge(current: dict[str, Any], imported: dict[str, Any],
          sections: Iterable[str], *, secrets: bool) -> dict[str, Any]:
    """The config that results from importing *imported* over *current*.

    Both are serialized configs. For every key of a chosen section the
    imported value replaces the current one, and a key the file leaves out
    goes back to its default, so the result matches the station that wrote
    the file. Keys of sections not chosen are left exactly as they are.

    With ``secrets`` false the file was written without them, and the stored
    ones are carried over where the entry is unchanged (see
    :func:`_keep_secrets`).
    """
    out = copy.deepcopy(current)
    chosen = set(sections)
    for section in chosen:
        for key in SECTIONS.get(section, ()):
            if key in imported:
                out[key] = copy.deepcopy(imported[key])
            else:
                out.pop(key, None)
    if not secrets:
        _keep_secrets(out, current, chosen)
    return out


def _keep_secrets(out: dict[str, Any], current: dict[str, Any], chosen: set[str]) -> None:
    """Put the stored secrets back into entries that are still the same entry.

    Matched on what the secret is sent to, never on a name alone: a password
    stored for one host must not follow a renamed entry to another.
    """
    if "map" in chosen:
        stored = current.get("map_tokens")
        if isinstance(stored, dict) and stored:
            out["map_tokens"] = {**stored, **(out.get("map_tokens") or {})}

    if "connections" in chosen:
        stored_ssh = {
            (e.get("name"), e.get("host"), e.get("username")): e.get("password")
            for e in current.get("ssh_connections") or [] if isinstance(e, dict)
        }
        for entry in out.get("ssh_connections") or []:
            if not isinstance(entry, dict) or entry.get("password"):
                continue
            password = stored_ssh.get((entry.get("name"), entry.get("host"), entry.get("username")))
            if password:
                entry["password"] = password

    if "vehicle" in chosen:
        new_ntrip = (out.get("rtk") or {}).get("ntrip")
        old_ntrip = (current.get("rtk") or {}).get("ntrip")
        if (isinstance(new_ntrip, dict) and isinstance(old_ntrip, dict)
                and not new_ntrip.get("password") and old_ntrip.get("password")
                and all(new_ntrip.get(k) == old_ntrip.get(k)
                        for k in ("host", "port", "username"))):
            new_ntrip["password"] = old_ntrip["password"]

        stored_cams = {
            (s.get("url"), s.get("username")): s.get("password")
            for s in (current.get("video") or {}).get("streams") or [] if isinstance(s, dict)
        }
        for stream in (out.get("video") or {}).get("streams") or []:
            if not isinstance(stream, dict) or stream.get("password"):
                continue
            password = stored_cams.get((stream.get("url"), stream.get("username")))
            if password:
                stream["password"] = password
