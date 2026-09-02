"""Optional operator configuration for Corvus GCS.

Zero-config field use is the default: if no config file is present the app
behaves exactly as before. An operator who wants to pin a default MAVLink
connection, custom tile upstreams, the cache dir, or stream-rate overrides
drops a small JSON file at ``~/.corvus/config.json`` (or a path passed to
:func:`load_config`) instead of forking the code.

Design rules (mandated by AGENTS.md):
- Fully optional and importable without side effects. No file is read at
  import time; only :func:`load_config` touches the disk.
- A missing file = defaults, silently. A malformed file = defaults, with a
  warning logged to ``corvus.config``. The app NEVER crashes on a bad config.
- Unknown keys are ignored; known keys are type-coerced where reasonable.

stdlib only.
"""
from __future__ import annotations

import dataclasses
import json
import logging
import os
import pathlib
from typing import Any

logger = logging.getLogger("corvus.config")


@dataclasses.dataclass
class CorvusConfig:
    """Operator-tunable runtime defaults.

    Empty-string dir fields (``tile_cache_dir``/``tlog_dir``) mean "use the
    built-in default" (``~/.corvus/tiles`` / ``~/.corvus/logs``); a non-empty
    value pins the location. ``None`` dict fields mean "use built-in
    defaults"; a dict overrides the whole registry.
    """

    mavlink_connection: str = "udp:0.0.0.0:14540"
    http_port: int = 8000
    tile_cache_dir: str = ""          # "" = ~/.corvus/tiles (default_cache_dir)
    tlog_dir: str = ""               # "" = ~/.corvus/logs
    tile_sources: dict[str, dict] | None = None
    stream_rates: dict | None = None

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

    tile_sources = defaults.tile_sources
    if isinstance(data.get("tile_sources"), dict):
        tile_sources = data["tile_sources"]

    stream_rates = defaults.stream_rates
    if isinstance(data.get("stream_rates"), dict):
        stream_rates = data["stream_rates"]

    return CorvusConfig(
        mavlink_connection=mavlink_connection,
        http_port=http_port,
        tile_cache_dir=tile_cache_dir,
        tlog_dir=tlog_dir,
        tile_sources=tile_sources,
        stream_rates=stream_rates,
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
