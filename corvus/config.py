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

from .paths import corvus_path

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
    "controls",
    "ui",
    "forwarding",
    "updates",
    "plugins",
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

    ``ssh_connections``/``theme``/``map``/``branding``/``controls``/``ui``/``updates`` are persisted operator UI state:
    the SSH connection list, the selected color theme (``{"name": ...}``, one
    of the predefined themes in ``src/css/themes.css``; the legacy
    ``{"accent": "#RRGGBB"}`` from the old accent picker is still parsed), and
    the map service + base layer (``{"provider": ..., "base_layer": ...}``,
    see ``corvus/tile_sources.py``), and the optional operator-supplied
    company logo (``{"logo": "<original filename>"}``; the bytes live beside
    the config file, never in it), and the optional input controls
    (``{"virtual_joystick": true}``, the on-screen stick over the map, plus
    ``arrow_keys``/``wasd_keys`` for the two key clusters beside it and
    ``key_gain`` for how much stick one held key is worth), and
    the interface size, desktop app icon and top bar (``{"scale": 1.25,
    "inverted_app_icon": false, "app_icon_backplate": false,
    "topbar_status_dots": false}`` — the multiplier
    the frontend puts on every length in the UI, which cut of the mark the
    Dock / taskbar gets, whether that mark sits on a filled backplate, and
    whether the top bar shows its per-block state dots),
    and the update check
    (``{"check": true, "skipped": "2026.09.27"}`` — whether to look at the
    GitHub releases at all, and the one release the operator dismissed).
    ``plugins`` is the state each TOOLS-tab plugin saves for itself
    (``{"<plugin id>": {...}}``; see ``corvus/plugin_registry.py``).

    They default to empty/None so an old config file with none of these keys
    still loads cleanly.
    """

    # 14550 is the port a ground station is expected on. 14540 — which this
    # defaulted to — is PX4's *onboard* link, the one MAVSDK and MAVROS use,
    # and "udp:" means bind, so Corvus was taking a socket a companion process
    # on the same machine needs. Only one of them can hold it: whoever loses
    # gets no telemetry at all, and on a simulator that looks exactly like a
    # broken vehicle rather than a port clash.
    #
    # An existing config file keeps whatever it already has; this moves the
    # default for a fresh install only.
    mavlink_connection: str = "udp:0.0.0.0:14550"
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
    controls: dict[str, Any] | None = None
    ui: dict[str, Any] | None = None
    forwarding: dict[str, Any] | None = None
    updates: dict[str, Any] | None = None
    plugins: dict[str, Any] | None = None

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
    return corvus_path("config.json")


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


# The on-screen manual-control surfaces, each its own switch: the stick pair,
# the four arrow keys (pitch and roll) and the four WASD keys (thrust and
# yaw). All three feed the same MANUAL_CONTROL stream.
_CONTROL_KEYS: tuple[str, ...] = ("virtual_joystick", "arrow_keys", "wasd_keys")

# How much stick one held key is worth, as a fraction of full travel
# (``controls.key_gain``; the frontend's GAIN_STEPS in src/js/joystick.js).
# Mirrored here only as bounds, not as the step list, for the same reason
# ``ui.scale`` is: the backend stores what the operator picked and has no
# opinion about which steps a given build offers, but it must never persist a
# gain of zero — a key that moves nothing — or one past the stop.
_KEY_GAIN_MIN = 0.05
_KEY_GAIN_MAX = 1.0


def _coerce_controls(raw: Any) -> dict[str, Any] | None:
    """Keep the three boolean surface switches and the numeric ``key_gain``; else None.

    The switches are real safety-relevant ones, so only genuine booleans count:
    a config carrying a string ``"true"`` reads as "not set", i.e. off, rather
    than as an accidental enable. Keys that are absent stay absent, so turning
    one surface on never writes a decision about the other.

    ``key_gain`` is not a decision to fly or not — the surfaces are already
    off unless switched on — so it is coerced like ``ui.scale`` instead: a
    number is clamped into the offered range, and anything else (a string, a
    bool, NaN) is dropped so the frontend falls back to its own default rather
    than to a value nobody chose.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {k: raw[k] for k in _CONTROL_KEYS if isinstance(raw.get(k), bool)}
    gain = raw.get("key_gain")
    # bool is an int in Python; True must not read as a gain of 1.0.
    if isinstance(gain, (int, float)) and not isinstance(gain, bool):
        value = float(gain)
        if value == value and value not in (float("inf"), float("-inf")):  # not NaN / inf
            out["key_gain"] = min(max(value, _KEY_GAIN_MIN), _KEY_GAIN_MAX)
    return out or None


# The interface-scale range the frontend offers (Corvus.scale in
# src/js/sidenav.js). Mirrored here only as bounds, not as the step list: the
# backend stores whatever the operator picked and has no opinion about which
# steps a given build offers, but it must never persist a value that would
# paint an unusable interface on the next launch.
_UI_SCALE_MIN = 0.5
_UI_SCALE_MAX = 3.0


_FORWARD_PORT_MIN = 1
_FORWARD_PORT_MAX = 65535


def _coerce_forwarding(raw: Any) -> dict[str, Any] | None:
    """Second-station MAVLink forwarding (see ``corvus/mavlink_forwarder.py``).

    ``enabled`` and ``allow_commands`` are genuine booleans only, for the same
    reason the controls keys are: ``allow_commands`` decides whether another
    ground station can arm this aircraft, and a config carrying the string
    ``"true"`` must read as "not set", never as an accidental yes.

    ``host``/``port`` say where the OTHER station listens; ``listen_host``/
    ``listen_port`` are Corvus' own socket, and they are deliberately not the
    same port (see ``corvus/mavlink_forwarder.py``). ``listen_port`` accepts 0
    — "any free port" — which is why it is range-checked separately.

    ``endpoints`` are kept as typed; the forwarder discards the ones it cannot
    parse, so a typo costs that endpoint and not the feature.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {}
    for key in ("enabled", "allow_commands"):
        if isinstance(raw.get(key), bool):
            out[key] = raw[key]
    for key in ("host", "listen_host"):
        value = raw.get(key)
        if isinstance(value, str) and value.strip():
            out[key] = value.strip()
    port = raw.get("port")
    if isinstance(port, int) and not isinstance(port, bool):
        if _FORWARD_PORT_MIN <= port <= _FORWARD_PORT_MAX:
            out["port"] = port
    listen_port = raw.get("listen_port")
    if isinstance(listen_port, int) and not isinstance(listen_port, bool):
        if 0 <= listen_port <= _FORWARD_PORT_MAX:
            out["listen_port"] = listen_port
    endpoints = raw.get("endpoints")
    if isinstance(endpoints, list):
        kept = [e.strip() for e in endpoints if isinstance(e, str) and e.strip()]
        if kept:
            out["endpoints"] = kept
    return out or None


def _coerce_ui(raw: Any) -> dict[str, Any] | None:
    """Keep the known UI keys (``scale``, the app-icon and top-bar keys); else None.

    ``scale`` multiplies every length in the frontend (see ``--ui-scale`` in
    ``src/css/themes.css``). Clamped rather than rejected: a hand-edited 40
    would otherwise leave the operator with an interface too large to reach
    the settings page that set it. Booleans are excluded explicitly — ``True``
    is a valid ``float`` in Python and would silently mean 100%.

    ``inverted_app_icon`` picks which cut of the mark the desktop wrapper
    hands the Dock / taskbar (see ``corvus/app.py``), and ``app_icon_backplate``
    asks for that mark to be drawn on a filled rounded square instead of bare
    transparency. They are independent: the backplate is the contrast the
    silhouette lacks on *any* dock, the inversion picks which way that contrast
    runs. Genuine booleans only, like the controls keys: a config carrying
    ``"false"`` must not read as on. Each key is kept independently — a file
    that names only one of them is not a reason to drop the others.

    ``topbar_status_dots`` asks the top bar to show the small state dot beside
    the VEHICLE / STATUS / GPS / BATTERY captions. Absent means off: those
    blocks already carry their state in the colour of the value itself, so the
    dots are opt-in rather than something an old config file turns on by
    saying nothing.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {}
    value = raw.get("scale")
    if not isinstance(value, bool) and isinstance(value, (int, float)):
        scale = float(value)
        if scale == scale and scale not in (float("inf"), float("-inf")):  # not NaN / inf
            out["scale"] = min(max(scale, _UI_SCALE_MIN), _UI_SCALE_MAX)
    for key in ("inverted_app_icon", "app_icon_backplate", "topbar_status_dots"):
        if isinstance(raw.get(key), bool):
            out[key] = raw[key]
    return out or None


def _coerce_updates(raw: Any) -> dict[str, Any] | None:
    """Keep the boolean ``check`` and the version string ``skipped``; else None.

    ``check`` is a genuine boolean only, like the controls keys: a config
    carrying ``"false"`` must not read as "on". ``skipped`` is the version the
    operator dismissed, stored as the plain version string; anything that is
    not a version is dropped rather than silencing an update forever.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {}
    if isinstance(raw.get("check"), bool):
        out["check"] = raw["check"]
    skipped = raw.get("skipped")
    # Imported here, not at module scope: config.py is pulled in by almost
    # everything, and update_check drags urllib.request behind it.
    from .update_check import parse_version
    if isinstance(skipped, str) and parse_version(skipped) is not None:
        out["skipped"] = skipped.strip().removeprefix("v")
    return out or None


def _coerce_plugins(raw: Any) -> dict[str, Any] | None:
    """Keep the per-plugin settings objects; drop everything else.

    ``{"<plugin id>": {...}}`` — the state a plugin saves for itself through
    ``POST /api/plugins/settings`` (see ``corvus/plugin_registry.py`` for what
    a plugin is). Corvus has no opinion about what is inside one object beyond
    it *being* an object, so the values pass through as written; an entry that
    is not a dict is dropped rather than handed back to a plugin that expects
    one. A plugin id is a folder name, so a non-string key cannot address
    anything and goes too.

    Not a place for secrets: the file also carries SSH passwords and is 0o600
    for that reason, but a plugin references a saved SSH connection by name so
    it never has to store one here.
    """
    if not isinstance(raw, dict):
        return None
    out = {k: v for k, v in raw.items() if isinstance(k, str) and k and isinstance(v, dict)}
    return out or None


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
    controls = _coerce_controls(data.get("controls"))
    forwarding = _coerce_forwarding(data.get("forwarding"))
    ui = _coerce_ui(data.get("ui"))
    updates = _coerce_updates(data.get("updates"))
    plugins = _coerce_plugins(data.get("plugins"))

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
        controls=controls,
        forwarding=forwarding,
        ui=ui,
        updates=updates,
        plugins=plugins,
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
    ``theme``/``map``/``branding``/``controls``/``ui``/``updates``/``plugins``) so the on-disk file stays
    lean when nothing overrides
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
    if cfg.controls is not None:
        out["controls"] = dict(cfg.controls)
    if cfg.forwarding is not None:
        out["forwarding"] = dict(cfg.forwarding)
    if cfg.ui is not None:
        out["ui"] = dict(cfg.ui)
    if cfg.updates is not None:
        out["updates"] = dict(cfg.updates)
    if cfg.plugins is not None:
        out["plugins"] = {k: dict(v) for k, v in cfg.plugins.items()}
    # Stable key order for a readable on-disk diff.
    return {k: out[k] for k in _CONFIG_FIELD_ORDER if k in out}


def save_config(cfg: CorvusConfig, path: str | None = None) -> None:
    """Persist *cfg* to JSON at *path* (default: ``~/.corvus/config.json``).

    Atomic + 0o600: the file may carry SSH passwords, so it is written to a
    temp file in the same directory, chmod'd 0o600, then ``os.replace``'d
    onto the final path (so a reader never sees a half-written file). The
    parent directory is created with ``exist_ok=True``.

    **On Windows the 0o600 is not protection.** ``os.chmod`` there sets only
    the read-only attribute; the permission bits are ignored and the file
    inherits the ACL of its parent directory. ``%USERPROFILE%\\.corvus`` is not
    readable by other standard users by default, so this is not open to the
    world — but it is not the explicit owner-only restriction POSIX gets, and
    an SSH password stored in the config is protected by directory inheritance
    alone. Storing a key path rather than a password avoids the question.

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
