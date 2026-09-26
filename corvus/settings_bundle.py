"""One file that carries a whole station setup: settings export and import.

The Settings page writes it and reads it back, so a station can be backed up,
restored after a reinstall, or copied to the next laptop. It holds:

* ``config``: the application config (``~/.corvus/config.json``), exactly as
  :func:`corvus.config._config_to_dict` writes it, minus the legacy
  ``plugins`` key;
* ``plugins``: every plugin's own config file (``corvus/plugin_config.py``);
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

Import is by section, so an operator can take the window layout from a file
and leave the serial port and folders of this machine alone. Every key of the
application config belongs to exactly one section; the test suite fails when a
new key is added without one.

stdlib only.
"""
from __future__ import annotations

import base64
import binascii
import copy
import time
from collections.abc import Iterable
from typing import Any

from .version import get_version

KIND = "corvus-settings"
FORMAT = 1
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
MAX_LOGO_BYTES = 4 * 1024 * 1024

# Section id -> the application config keys it carries. "plugins" and "layout"
# have none: the first is the plugin files, the second is browser state only.
SECTIONS: dict[str, tuple[str, ...]] = {
    "interface": ("theme", "ui", "controls", "branding", "updates"),
    "map": ("map", "map_tokens", "tile_sources"),
    "connections": ("mavlink_connection", "autoconnect", "forwarding",
                    "stream_rates", "ssh_connections"),
    "vehicle": ("battery", "remote_id", "rtk", "video", "parameters"),
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


def build(config: dict[str, Any], *, plugins: dict[str, Any] | None = None,
          logo: bytes | None = None, browser: dict[str, Any] | None = None,
          include_secrets: bool = False) -> dict[str, Any]:
    """Assemble the bundle written to disk by an export.

    *config* is the serialized application config. The result is plain JSON
    data; the caller writes it.
    """
    cfg = {k: v for k, v in config.items() if k not in EXCLUDED_KEYS}
    if not include_secrets:
        cfg = redact(cfg)
    bundle: dict[str, Any] = {
        "kind": KIND,
        "format": FORMAT,
        "product": "Corvus GCS",
        "version": get_version(),
        "exported_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "secrets": bool(include_secrets),
        "config": cfg,
        "plugins": {k: v for k, v in (plugins or {}).items() if isinstance(v, dict)},
        "browser": _clean_browser(browser),
    }
    if logo:
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
    added, this one would silently drop.
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
    plugins = data.get("plugins")
    logo = None
    if data.get("logo") is not None:
        logo = decode_logo(data.get("logo"))
    return {
        "version": data.get("version") if isinstance(data.get("version"), str) else "",
        "exported_at": data.get("exported_at") if isinstance(data.get("exported_at"), str) else "",
        "secrets": data.get("secrets") is True,
        "config": {k: v for k, v in config.items() if k not in EXCLUDED_KEYS},
        "plugins": ({k: v for k, v in plugins.items() if isinstance(k, str) and isinstance(v, dict)}
                    if isinstance(plugins, dict) else {}),
        "logo": logo,
        "browser": _clean_browser(data.get("browser")),
    }


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
