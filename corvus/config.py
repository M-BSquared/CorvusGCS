"""Optional operator configuration for Corvus GCS.

Zero-config field use is the default: if no config file is present the app
behaves exactly as before. An operator who wants to pin a default MAVLink
connection, custom tile upstreams, the cache dir, or stream-rate overrides
drops a small JSON file at ``~/.corvus/config.json`` (or a path passed to
:func:`load_config`) instead of forking the code. The Settings page writes the
same file for the UI state it owns (color theme, map service, SSH list).

Design rules (mandated by AGENTS.md):
- Fully optional and importable without side effects. No file is read at
  import time; only :func:`load_config` touches the disk.
- A missing file = defaults, silently. A malformed file = defaults, with a
  warning logged to ``corvus.config``. The app NEVER crashes on a bad config.
- Unknown keys are ignored; known keys are type-coerced where reasonable.

``save_config`` writes the file **atomically** with mode 0o600 because the
file may carry SSH passwords. ``to_public_dict`` is the single redaction
point: it strips ``password`` from every ``ssh_connections`` entry so no
HTTP response ever echoes a secret back.

stdlib only.
"""
from __future__ import annotations

import dataclasses
import json
import logging
import os
import pathlib
import tempfile
from typing import Any

logger = logging.getLogger("corvus.config")

# Canonical key order for the serialized config file. Keeping it stable makes
# diffs of the on-disk JSON readable across edits (an operator can review a
# git diff of ~/.corvus/config.json in the field).
_CONFIG_FIELD_ORDER: tuple[str, ...] = (
    "mavlink_connection",
    "http_port",
    "tile_cache_dir",
    "tlog_dir",
    "params_dir",
    "firmware_dir",
    "log_download_dir",
    "tile_sources",
    "stream_rates",
    "ssh_connections",
    "theme",
    "map",
    "branding",
)

# Required keys on a saved ssh_connections entry; missing keys default to a
# sane empty value so a partial entry never crashes the parser.
_SSH_CONN_KEYS: tuple[str, ...] = ("name", "host", "port", "username", "key_path", "password")


@dataclasses.dataclass
class CorvusConfig:
    """Operator-tunable runtime defaults.

    Empty-string dir fields (``tile_cache_dir``/``tlog_dir``/``params_dir``/
    ``firmware_dir``/``log_download_dir``) mean "use the built-in default"
    (``~/.corvus/tiles`` / ``~/.corvus/logs`` / ``~/.corvus/params`` /
    ``~/.corvus/firmware`` / ``~/.corvus/flightlogs``); a non-empty value pins the location. ``None`` dict fields mean "use built-in
    defaults"; a dict overrides the whole registry.

    ``ssh_connections``/``theme``/``map``/``branding`` are persisted operator UI state:
    the SSH connection list, the selected color theme (``{"name": ...}``, one
    of the predefined themes in ``src/css/themes.css``; the legacy
    ``{"accent": "#RRGGBB"}`` from the old accent picker is still parsed), and
    the map service + base layer (``{"provider": ..., "base_layer": ...}``,
    see ``corvus/tile_sources.py``), and the optional operator-supplied
    company logo (``{"logo": "<original filename>"}``; the bytes live beside
    the config file, never in it). They default to empty/None so an old
    config file with none of these keys still loads cleanly.
    """

    mavlink_connection: str = "udp:0.0.0.0:14540"
    http_port: int = 8000
    tile_cache_dir: str = ""          # "" = ~/.corvus/tiles (default_cache_dir)
    tlog_dir: str = ""               # "" = ~/.corvus/logs
    params_dir: str = ""             # "" = ~/.corvus/params (exported param files)
    firmware_dir: str = ""           # "" = ~/.corvus/firmware (downloaded PX4 images)
    log_download_dir: str = ""       # "" = ~/.corvus/flightlogs (ULogs + exported tlogs)
    tile_sources: dict[str, dict] | None = None
    stream_rates: dict | None = None
    ssh_connections: list[dict[str, Any]] = dataclasses.field(default_factory=list)
    theme: dict[str, Any] | None = None
    map: dict[str, Any] | None = None
    branding: dict[str, Any] | None = None

    def apply_overrides(self, **kwargs: Any) -> "CorvusConfig":
        """Return a copy with non-None kwargs overriding matching fields.

        Used so CLI args beat the config file: only fields that are actually
        passed (and not None) replace the loaded values. Unknown kwargs are
        dropped so a caller passing extras can never crash this call.
        """
        field_names = {f.name for f in dataclasses.fields(self)}
        overrides = {
            k: v for k, v in kwargs.items()
            if v is not None and k in field_names
        }
        return dataclasses.replace(self, **overrides)


def default_config_path() -> str:
    """Return the conventional config file location (``~`` expanded)."""
    return os.path.expanduser("~/.corvus/config.json")


def _coerce_int(value: Any, default: int) -> int:
    """Coerce *value* to int; fall back to *default* on any failure."""
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _coerce_ssh_entry(raw: Any) -> dict[str, Any] | None:
    """Coerce one ssh_connections entry to a clean dict, or None to drop it.

    Tolerates missing and extra keys: a non-dict entry is dropped, a dict
    missing keys gets the sane empty defaults. ``port`` is int-coerced.
    """
    if not isinstance(raw, dict):
        return None
    name = raw.get("name")
    if not isinstance(name, str) or not name:
        # A nameless entry is useless to the SSH UI (sessions are keyed by
        # name); drop it rather than synthesize one.
        return None
    host = raw.get("host", "")
    if not isinstance(host, str):
        host = ""
    port = _coerce_int(raw.get("port", 22), 22)
    username = raw.get("username", "")
    if not isinstance(username, str):
        username = ""
    key_path = raw.get("key_path", "")
    if not isinstance(key_path, str):
        key_path = ""
    password = raw.get("password", "")
    if not isinstance(password, str):
        password = ""
    return {
        "name": name,
        "host": host,
        "port": port,
        "username": username,
        "key_path": key_path,
        "password": password,
    }


def _coerce_ssh_connections(raw: Any) -> list[dict[str, Any]]:
    """Coerce the ``ssh_connections`` field to a clean list; never raises."""
    if not isinstance(raw, list):
        return []
    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for entry in raw:
        coerced = _coerce_ssh_entry(entry)
        if coerced is None:
            continue
        # Later entries win on a duplicate name (matches POST upsert).
        if coerced["name"] in seen:
            for i, existing in enumerate(out):
                if existing["name"] == coerced["name"]:
                    out[i] = coerced
                    break
        else:
            out.append(coerced)
            seen.add(coerced["name"])
    return out


def _coerce_str_keys(raw: Any, keys: tuple[str, ...]) -> dict[str, Any] | None:
    """Keep the string-valued entries of *raw* named in *keys*; else None.

    The shared coercion behind ``theme`` and ``map``: both are small,
    open-ended string maps whose unknown/mistyped members must be dropped
    rather than crash the load. Only keys actually present AND string-valued
    survive, so an old config file round-trips byte-for-byte and a partially
    written one still loads. Returns None (= "use defaults") when nothing
    usable is left, matching the ``None`` semantics of the dataclass fields.
    """
    if not isinstance(raw, dict):
        return None
    out = {k: raw[k] for k in keys if isinstance(raw.get(k), str)}
    return out or None


def _coerce_theme(raw: Any) -> dict[str, Any] | None:
    """Keep the string-valued ``name``/``accent`` theme keys; else None.

    ``name`` selects one of the predefined CSS themes shipped in
    ``src/css/themes.css`` (green/blue/pink/orange/light) and is what the
    Appearance settings write today. ``accent`` is the legacy single-color
    override from the v1 accent picker: still parsed so an existing
    ``~/.corvus/config.json`` keeps loading, no longer written by the UI.
    """
    return _coerce_str_keys(raw, ("name", "accent"))


def _coerce_map(raw: Any) -> dict[str, Any] | None:
    """Keep the string-valued ``base_layer``/``provider`` map keys; else None.

    ``provider`` names the tile service (esri/osm/google/bing) and
    ``base_layer`` the concrete source id within it; see
    ``corvus/tile_sources.py``. Neither is validated against the registry
    here — an unknown value must not stop the config from loading, so the
    frontend falls back to its defaults when it cannot resolve one.
    """
    return _coerce_str_keys(raw, ("base_layer", "provider"))


def _coerce_branding(raw: Any) -> dict[str, Any] | None:
    """Keep the string-valued ``logo`` branding key; else None.

    ``logo`` is the *display* filename of the operator's company logo (e.g.
    ``unibw.png``); its presence is what tells the UI a logo is configured.
    The image itself is stored next to the config file as
    ``branding/logo.png``, so a config with a stale key simply renders no
    logo rather than failing to load. Absent by default — no company logo
    ships with the app.
    """
    return _coerce_str_keys(raw, ("logo",))


def _build_config(data: dict[str, Any]) -> CorvusConfig:
    """Build a CorvusConfig from a parsed JSON object, ignoring unknown keys.

    Each known field is taken from ``data`` only when present and of a sane
    type; otherwise the built-in default is kept. This never raises.
    """
    defaults = CorvusConfig()

    mavlink_connection = defaults.mavlink_connection
    if isinstance(data.get("mavlink_connection"), str):
        mavlink_connection = data["mavlink_connection"]

    http_port = defaults.http_port
    if "http_port" in data:
        coerced = _coerce_int(data["http_port"], defaults.http_port)
        # Reject out-of-range TCP ports (negative or >65535); fall back to the
        # default so a bad config never reaches the server bind later.
        http_port = coerced if 0 <= coerced <= 65535 else defaults.http_port

    tile_cache_dir = defaults.tile_cache_dir
    if isinstance(data.get("tile_cache_dir"), str):
        tile_cache_dir = data["tile_cache_dir"]

    tlog_dir = defaults.tlog_dir
    if isinstance(data.get("tlog_dir"), str):
        tlog_dir = data["tlog_dir"]

    params_dir = defaults.params_dir
    if isinstance(data.get("params_dir"), str):
        params_dir = data["params_dir"]

    firmware_dir = defaults.firmware_dir
    if isinstance(data.get("firmware_dir"), str):
        firmware_dir = data["firmware_dir"]

    log_download_dir = defaults.log_download_dir
    if isinstance(data.get("log_download_dir"), str):
        log_download_dir = data["log_download_dir"]

    tile_sources = defaults.tile_sources
    if isinstance(data.get("tile_sources"), dict):
        tile_sources = data["tile_sources"]

    stream_rates = defaults.stream_rates
    if isinstance(data.get("stream_rates"), dict):
        stream_rates = data["stream_rates"]

    ssh_connections = _coerce_ssh_connections(data.get("ssh_connections"))
    theme = _coerce_theme(data.get("theme"))
    map_cfg = _coerce_map(data.get("map"))
    branding = _coerce_branding(data.get("branding"))

    return CorvusConfig(
        mavlink_connection=mavlink_connection,
        http_port=http_port,
        tile_cache_dir=tile_cache_dir,
        tlog_dir=tlog_dir,
        params_dir=params_dir,
        firmware_dir=firmware_dir,
        log_download_dir=log_download_dir,
        tile_sources=tile_sources,
        stream_rates=stream_rates,
        ssh_connections=ssh_connections,
        theme=theme,
        map=map_cfg,
        branding=branding,
    )


def load_config(path: str | None = None) -> CorvusConfig:
    """Load a CorvusConfig from *path* (default: ``~/.corvus/config.json``).

    Missing file -> defaults (silent). Present but malformed/unreadable ->
    defaults with a warning logged. Unknown keys ignored. Never raises.
    """
    cfg_path = pathlib.Path(path) if path else pathlib.Path(default_config_path())
    if not cfg_path.is_file():
        return CorvusConfig()
    try:
        raw = cfg_path.read_text(encoding="utf-8")
        data = json.loads(raw)
    except (OSError, ValueError) as exc:
        logger.warning("config file %s unreadable/malformed; using defaults: %s",
                       cfg_path, exc)
        return CorvusConfig()
    if not isinstance(data, dict):
        logger.warning("config file %s top-level is not a JSON object; "
                       "using defaults", cfg_path)
        return CorvusConfig()
    return _build_config(data)


def _config_to_dict(cfg: CorvusConfig) -> dict[str, Any]:
    """Serialize a CorvusConfig to a plain dict in stable key order.

    Omits ``None`` optional dict fields (``tile_sources``/``stream_rates``/
    ``theme``/``map``/``branding``) so the on-disk file stays lean when nothing overrides
    them; an empty ``ssh_connections`` list is kept (it is real operator
    state, the absence of which still round-trips through ``[]``).
    """
    out: dict[str, Any] = {
        "mavlink_connection": cfg.mavlink_connection,
        "http_port": cfg.http_port,
        "tile_cache_dir": cfg.tile_cache_dir,
        "tlog_dir": cfg.tlog_dir,
        "params_dir": cfg.params_dir,
        "firmware_dir": cfg.firmware_dir,
        "log_download_dir": cfg.log_download_dir,
        "ssh_connections": [dict(entry) for entry in cfg.ssh_connections],
    }
    if cfg.tile_sources is not None:
        out["tile_sources"] = cfg.tile_sources
    if cfg.stream_rates is not None:
        out["stream_rates"] = cfg.stream_rates
    if cfg.theme is not None:
        out["theme"] = dict(cfg.theme)
    if cfg.map is not None:
        out["map"] = dict(cfg.map)
    if cfg.branding is not None:
        out["branding"] = dict(cfg.branding)
    # Stable key order for a readable on-disk diff.
    return {k: out[k] for k in _CONFIG_FIELD_ORDER if k in out}


def save_config(cfg: CorvusConfig, path: str | None = None) -> None:
    """Persist *cfg* to JSON at *path* (default: ``~/.corvus/config.json``).

    Atomic + 0o600: the file may carry SSH passwords, so it is written to a
    temp file in the same directory, chmod'd 0o600, then ``os.replace``'d
    onto the final path (so a reader never sees a half-written file). The
    parent directory is created with ``exist_ok=True``.

    Stdlib convention: genuine IO failures raise OSError; everything else
    (e.g. a non-serializable value) is logged and returns. The password-
    bearing file is never left in a partial state on disk.
    """
    cfg_path = pathlib.Path(path) if path else pathlib.Path(default_config_path())
    data = _config_to_dict(cfg)
    try:
        text = json.dumps(data, indent=2, ensure_ascii=False) + "\n"
    except (TypeError, ValueError) as exc:
        # Should not happen — every value is JSON-native — but never let a
        # serialization bug leave a password file in a bad state.
        logger.error("config serialization failed; not writing %s: %s", cfg_path, exc)
        return
    cfg_path.parent.mkdir(parents=True, exist_ok=True)
    # NamedTemporaryFile in the same dir guarantees the os.replace is on the
    # same filesystem (atomic). delete=False so we can chmod + replace it.
    fd, tmp_name = tempfile.mkstemp(
        prefix=cfg_path.name + ".", suffix=".tmp", dir=str(cfg_path.parent),
    )
    tmp_path = pathlib.Path(tmp_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        # Restrict to owner-only BEFORE the replace so the final file is
        # never briefly world-readable.
        os.chmod(tmp_path, 0o600)
        os.replace(tmp_path, cfg_path)
    except OSError:
        # Genuine IO failure — surface it (stdlib convention) but clean up.
        try:
            tmp_path.unlink()
        except OSError:
            pass
        raise


def to_public_dict(cfg: CorvusConfig) -> dict[str, Any]:
    """Return the config as a dict with every SSH ``password`` stripped.

    The single redaction point: ``GET /api/config`` and
    ``GET /api/ssh/connections`` both build their response from this so no
    HTTP response ever echoes a password back. ``key_path`` is kept (it is a
    path, not a secret).
    """
    public = _config_to_dict(cfg)
    redacted: list[dict[str, Any]] = []
    for entry in public.get("ssh_connections", []):
        if not isinstance(entry, dict):
            continue
        clean = dict(entry)
        clean.pop("password", None)
        redacted.append(clean)
    public["ssh_connections"] = redacted
    return public
