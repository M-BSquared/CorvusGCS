"""Tests for the optional operator config (corvus/config.py).

Zero-config is the default: a missing file yields built-in defaults with no
error, a malformed file yields defaults with a warning, unknown keys are
ignored, and apply_overrides lets CLI args beat the file. All tests are
hermetic (tmp_path; none touch the user's real home).
"""
from __future__ import annotations

import json
import logging
import os

from corvus.config import (
    CorvusConfig,
    default_config_path,
    load_config,
    save_config,
    to_public_dict,
)


def _defaults() -> CorvusConfig:
    """Fresh built-in defaults for comparison."""
    return CorvusConfig()


def test_load_config_missing_file_returns_defaults(tmp_path) -> None:
    """No file at the given path -> built-in defaults, no raise."""
    missing = tmp_path / "does_not_exist.json"
    cfg = load_config(str(missing))
    expected = _defaults()
    assert cfg.mavlink_connection == expected.mavlink_connection
    assert cfg.http_port == expected.http_port
    assert cfg.tile_cache_dir == expected.tile_cache_dir
    assert cfg.tlog_dir == expected.tlog_dir
    assert cfg.tile_sources is None
    assert cfg.stream_rates is None


def test_load_config_no_arg_uses_default_path_when_absent(tmp_path, monkeypatch) -> None:
    """load_config() with no arg reads default_config_path(); absent -> defaults.

    HOME is pointed at a tmp dir so this never touches the real home.
    """
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setenv("HOME", str(fake_home))
    cfg = load_config()
    expected = _defaults()
    assert cfg.mavlink_connection == expected.mavlink_connection
    assert cfg.http_port == expected.http_port


def test_load_config_valid_json_overrides_fields(tmp_path) -> None:
    """A well-formed JSON file overrides every known field; port coerced to int."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({
        "mavlink_connection": "udpin:0.0.0.0:14550",
        "http_port": "9000",  # string -> coerced to int
        "tile_cache_dir": "/tmp/corvus-tiles",
        "tlog_dir": "/tmp/corvus-logs",
        "tile_sources": {
            "satellite": {
                "label": "Sat",
                "upstream": "https://x/{z}/{y}/{x}",
                "maxzoom": 18,
            },
        },
        "stream_rates": {"ATTITUDE": 10},
    }), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.mavlink_connection == "udpin:0.0.0.0:14550"
    assert cfg.http_port == 9000
    assert isinstance(cfg.http_port, int)
    assert cfg.tile_cache_dir == "/tmp/corvus-tiles"
    assert cfg.tlog_dir == "/tmp/corvus-logs"
    assert cfg.tile_sources == {
        "satellite": {
            "label": "Sat",
            "upstream": "https://x/{z}/{y}/{x}",
            "maxzoom": 18,
        },
    }
    assert cfg.stream_rates == {"ATTITUDE": 10}


def test_load_config_malformed_json_returns_defaults(tmp_path) -> None:
    """Malformed JSON must not raise; defaults returned, warning logged.

    A handler is attached directly to the ``corvus.config`` logger (rather than
    relying on root propagation via ``caplog``) so the assertion is
    deterministic regardless of pytest's logging-plugin wiring.
    """
    cfg_logger = logging.getLogger("corvus.config")
    captured: list[logging.LogRecord] = []

    class _Capture(logging.Handler):
        def emit(self, record: logging.LogRecord) -> None:
            captured.append(record)

    handler = _Capture(level=logging.WARNING)
    cfg_logger.addHandler(handler)
    prev_level = cfg_logger.level
    cfg_logger.setLevel(logging.WARNING)
    try:
        p = tmp_path / "bad.json"
        p.write_text("{ not valid json ", encoding="utf-8")
        cfg = load_config(str(p))
    finally:
        cfg_logger.removeHandler(handler)
        cfg_logger.setLevel(prev_level)

    expected = _defaults()
    assert cfg.mavlink_connection == expected.mavlink_connection
    assert cfg.http_port == expected.http_port
    assert any(r.name == "corvus.config" and r.levelno == logging.WARNING
               for r in captured)


def test_load_config_non_coercible_port_keeps_default(tmp_path) -> None:
    """A port value that cannot be int-coerced falls back to the default."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"http_port": ["not", "a", "port"]}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.http_port == _defaults().http_port


def test_load_config_unknown_keys_ignored(tmp_path) -> None:
    """Unknown keys do not raise and do not affect known fields."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({
        "mavlink_connection": "tcp:1.2.3.4:5760",
        "http_port": 8888,
        "unknown_key": 123,
        "another_unknown": {"a": 1},
    }), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.mavlink_connection == "tcp:1.2.3.4:5760"
    assert cfg.http_port == 8888
    # Unknown keys have no field to map onto.
    assert not hasattr(cfg, "unknown_key")
    assert not hasattr(cfg, "another_unknown")


def test_default_config_path_ends_with_corvus_config_json() -> None:
    """The conventional config location is ~/.corvus/config.json (expanded)."""
    path = default_config_path()
    assert path.endswith(os.path.join(".corvus", "config.json"))
    # ~ must be expanded to a real path (no literal tilde).
    assert "~" not in path


def test_apply_overrides_returns_new_config_non_none_only() -> None:
    """apply_overrides returns a copy with non-None kwargs replacing fields."""
    base = _defaults()
    overridden = base.apply_overrides(
        http_port=7777,
        mavlink_connection="tcp:1.2.3.4:5760",
        tile_cache_dir=None,        # None -> keep default ("")
        stream_rates={"ATTITUDE": 20},
        not_a_field="ignored",      # unknown -> dropped, no crash
    )
    # Overridden fields changed.
    assert overridden.http_port == 7777
    assert overridden.mavlink_connection == "tcp:1.2.3.4:5760"
    assert overridden.stream_rates == {"ATTITUDE": 20}
    # None kwargs keep the default.
    assert overridden.tile_cache_dir == base.tile_cache_dir
    # The original instance is not mutated.
    assert base.http_port == _defaults().http_port
    assert base.mavlink_connection == _defaults().mavlink_connection
    # Unknown kwargs are dropped (no attribute created).
    assert not hasattr(overridden, "not_a_field")


# ---------------------------------------------------------------------------
# New optional fields: ssh_connections, theme, map
# ---------------------------------------------------------------------------

def test_default_config_has_empty_new_fields() -> None:
    """A fresh CorvusConfig ships with empty ssh_connections and None theme/map."""
    cfg = CorvusConfig()
    assert cfg.ssh_connections == []
    assert cfg.theme is None
    assert cfg.map is None


def test_load_config_parses_ssh_connections(tmp_path) -> None:
    """ssh_connections parses to a clean list of dicts with int ports."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({
        "ssh_connections": [
            {"name": "CORVUS-01", "host": "192.168.2.10", "port": 22,
             "username": "corvus", "key_path": "", "password": "secret"},
            {"name": "PI", "host": "10.0.0.5", "port": "2222",  # str -> int
             "username": "pi"},
        ],
    }), encoding="utf-8")
    cfg = load_config(str(p))
    assert len(cfg.ssh_connections) == 2
    first = cfg.ssh_connections[0]
    assert first["name"] == "CORVUS-01"
    assert first["host"] == "192.168.2.10"
    assert first["port"] == 22
    assert isinstance(first["port"], int)
    assert first["password"] == "secret"
    second = cfg.ssh_connections[1]
    assert second["port"] == 2222
    # Missing keys fall back to sane empty defaults.
    assert second["key_path"] == ""
    assert second["password"] == ""


def test_load_config_ssh_connections_malformed_falls_back_to_empty(tmp_path) -> None:
    """A non-list ssh_connections or non-dict entries never crash."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({
        "ssh_connections": [
            "not-a-dict",
            {"host": "x", "port": 22},          # no name -> dropped
            {"name": 42, "host": "y"},           # name not a string -> dropped
            {"name": "OK", "host": "z", "port": "nope", "username": 5},
        ],
    }), encoding="utf-8")
    cfg = load_config(str(p))
    assert len(cfg.ssh_connections) == 1
    only = cfg.ssh_connections[0]
    assert only["name"] == "OK"
    assert only["host"] == "z"
    assert only["port"] == 22  # bad port coerced to default
    assert only["username"] == ""  # non-string username -> empty str


def test_load_config_ssh_connections_non_list_returns_empty(tmp_path) -> None:
    """A ssh_connections that is not a list at all yields an empty list."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ssh_connections": {"name": "x"}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.ssh_connections == []


def test_load_config_parses_theme(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"theme": {"accent": "#3DA876"}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.theme == {"accent": "#3DA876"}


def test_load_config_theme_non_string_accent_dropped(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"theme": {"accent": 42}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.theme is None  # bad accent -> None (use defaults)


def test_load_config_theme_non_dict_returns_none(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"theme": "dark"}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.theme is None


def test_load_config_parses_map(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"map": {"base_layer": "satellite"}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.map == {"base_layer": "satellite"}


def test_load_config_map_non_string_base_layer_dropped(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"map": {"base_layer": 3}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.map is None


def test_load_config_map_non_dict_returns_none(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"map": "satellite"}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.map is None


# ---------------------------------------------------------------------------
# ui — the interface scale written by Settings -> Appearance
# ---------------------------------------------------------------------------
def test_load_config_parses_ui_scale(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"scale": 1.25}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.ui == {"scale": 1.25}


def test_load_config_ui_scale_int_becomes_float(tmp_path) -> None:
    """An integer scale is a valid one; it is stored as the float it means."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"scale": 1}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.ui == {"scale": 1.0}


def test_load_config_ui_scale_clamped_not_rejected(tmp_path) -> None:
    """A hand-edited absurd scale is pulled into range, never honoured.

    Rejecting it would be worse than clamping: an interface painted at 4000%
    cannot reach the settings page that would fix it.
    """
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"scale": 40}}), encoding="utf-8")
    assert load_config(str(p)).ui == {"scale": 3.0}
    p.write_text(json.dumps({"ui": {"scale": 0.01}}), encoding="utf-8")
    assert load_config(str(p)).ui == {"scale": 0.5}


def test_load_config_ui_scale_bool_dropped(tmp_path) -> None:
    """True is a float in Python and would silently read as 100%."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"scale": True}}), encoding="utf-8")
    assert load_config(str(p)).ui is None


def test_load_config_ui_scale_string_dropped(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"scale": "1.25"}}), encoding="utf-8")
    assert load_config(str(p)).ui is None


def test_load_config_ui_non_dict_returns_none(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": 1.25}), encoding="utf-8")
    assert load_config(str(p)).ui is None


def test_ui_scale_round_trips_through_save(tmp_path) -> None:
    p = tmp_path / "config.json"
    save_config(CorvusConfig(ui={"scale": 1.1}), str(p))
    assert load_config(str(p)).ui == {"scale": 1.1}


def test_save_config_omits_none_ui(tmp_path) -> None:
    """No interface-size choice means no ``ui`` key; the file stays lean."""
    p = tmp_path / "config.json"
    save_config(CorvusConfig(), str(p))
    assert "ui" not in json.loads(p.read_text(encoding="utf-8"))


def test_load_config_parses_inverted_app_icon(tmp_path) -> None:
    """The desktop wrapper's Dock/taskbar icon choice, on its own."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"inverted_app_icon": True}}), encoding="utf-8")
    assert load_config(str(p)).ui == {"inverted_app_icon": True}


def test_load_config_ui_keys_are_independent(tmp_path) -> None:
    """A bad scale must not take the icon switch down with it, and vice versa."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"scale": 1.1, "inverted_app_icon": False}}),
                 encoding="utf-8")
    assert load_config(str(p)).ui == {"scale": 1.1, "inverted_app_icon": False}
    p.write_text(json.dumps({"ui": {"scale": "big", "inverted_app_icon": True}}),
                 encoding="utf-8")
    assert load_config(str(p)).ui == {"inverted_app_icon": True}


def test_load_config_inverted_app_icon_non_bool_dropped(tmp_path) -> None:
    """A string "true" is not a boolean; the icon falls back to the shipped cut."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"ui": {"inverted_app_icon": "true"}}), encoding="utf-8")
    assert load_config(str(p)).ui is None


def test_inverted_app_icon_round_trips_through_save(tmp_path) -> None:
    p = tmp_path / "config.json"
    save_config(CorvusConfig(ui={"scale": 1.1, "inverted_app_icon": True}), str(p))
    assert load_config(str(p)).ui == {"scale": 1.1, "inverted_app_icon": True}


def test_load_config_parses_branding(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"branding": {"logo": "unibw.png"}}), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.branding == {"logo": "unibw.png"}


def test_branding_defaults_to_none(tmp_path) -> None:
    """No company logo ships with the app: an untouched config carries none."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"http_port": 8000}), encoding="utf-8")
    assert load_config(str(p)).branding is None


def test_load_config_branding_non_string_logo_dropped(tmp_path) -> None:
    p = tmp_path / "config.json"
    p.write_text(json.dumps({"branding": {"logo": 7}}), encoding="utf-8")
    assert load_config(str(p)).branding is None


def test_branding_round_trips_through_save(tmp_path) -> None:
    p = tmp_path / "config.json"
    save_config(CorvusConfig(branding={"logo": "unibw.png"}), str(p))
    assert load_config(str(p)).branding == {"logo": "unibw.png"}


def test_load_config_old_file_without_new_fields_still_loads(tmp_path) -> None:
    """An old config file with none of the new keys loads with defaults."""
    p = tmp_path / "config.json"
    p.write_text(json.dumps({
        "mavlink_connection": "udp:0.0.0.0:14540",
        "http_port": 8000,
    }), encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg.ssh_connections == []
    assert cfg.theme is None
    assert cfg.map is None


# ---------------------------------------------------------------------------
# save_config: atomic write, chmod 600, round-trip
# ---------------------------------------------------------------------------

def test_save_config_creates_parent_dir_and_writes_file(tmp_path) -> None:
    cfg = CorvusConfig(
        mavlink_connection="udp:1.2.3.4:14550",
        http_port=9001,
        ssh_connections=[{"name": "X", "host": "h", "port": 22,
                          "username": "u", "key_path": "", "password": "p"}],
        theme={"accent": "#abcdef"},
        map={"base_layer": "satellite"},
    )
    path = tmp_path / "nested" / "config.json"
    save_config(cfg, str(path))
    assert path.is_file()
    data = json.loads(path.read_text(encoding="utf-8"))
    assert data["mavlink_connection"] == "udp:1.2.3.4:14550"
    assert data["http_port"] == 9001
    assert data["ssh_connections"][0]["name"] == "X"
    assert data["theme"] == {"accent": "#abcdef"}
    assert data["map"] == {"base_layer": "satellite"}


def test_save_config_file_mode_is_600(tmp_path) -> None:
    """The saved config is owner-read/write-only because it may hold passwords."""
    cfg = CorvusConfig(
        ssh_connections=[{"name": "X", "host": "h", "port": 22,
                          "username": "u", "key_path": "", "password": "hunter2"}],
    )
    path = tmp_path / "config.json"
    save_config(cfg, str(path))
    mode = path.stat().st_mode & 0o777
    assert mode == 0o600, f"expected 0o600, got {oct(mode)}"


def test_save_config_round_trip_load_returns_equivalent(tmp_path) -> None:
    cfg = CorvusConfig(
        mavlink_connection="tcp:1.2.3.4:5760",
        http_port=7777,
        tile_cache_dir="/tmp/x",
        tlog_dir="/tmp/y",
        tile_sources={"satellite": {"label": "S", "upstream": "u", "maxzoom": 18}},
        stream_rates={"ATTITUDE": 10},
        ssh_connections=[{"name": "A", "host": "h1", "port": 22,
                          "username": "u", "key_path": "k", "password": "p1"}],
        theme={"accent": "#3DA876"},
        map={"base_layer": "topo"},
    )
    path = tmp_path / "config.json"
    save_config(cfg, str(path))
    loaded = load_config(str(path))
    assert loaded.mavlink_connection == cfg.mavlink_connection
    assert loaded.http_port == cfg.http_port
    assert loaded.tile_cache_dir == cfg.tile_cache_dir
    assert loaded.tlog_dir == cfg.tlog_dir
    assert loaded.tile_sources == cfg.tile_sources
    assert loaded.stream_rates == cfg.stream_rates
    assert loaded.ssh_connections == cfg.ssh_connections
    assert loaded.theme == cfg.theme
    assert loaded.map == cfg.map


def test_save_config_omits_none_optional_fields(tmp_path) -> None:
    """None tile_sources/theme/map are not written; the file stays lean."""
    cfg = CorvusConfig()  # all None / empty defaults
    path = tmp_path / "config.json"
    save_config(cfg, str(path))
    data = json.loads(path.read_text(encoding="utf-8"))
    assert "tile_sources" not in data
    assert "stream_rates" not in data
    assert "theme" not in data
    assert "map" not in data
    # ssh_connections is real operator state; kept even when empty.
    assert data["ssh_connections"] == []


def test_save_config_atomic_no_partial_file_on_success(tmp_path) -> None:
    """After a successful save there are no leftover .tmp files in the dir."""
    cfg = CorvusConfig()
    path = tmp_path / "config.json"
    save_config(cfg, str(path))
    leftovers = [p for p in tmp_path.iterdir() if p.name.endswith(".tmp")]
    assert leftovers == []


def test_save_config_default_path_creates_corvus_dir(tmp_path, monkeypatch) -> None:
    """save_config(cfg) with no path writes to ~/.corvus/config.json (mode 600)."""
    fake_home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(fake_home))
    save_config(CorvusConfig(http_port=12345))
    p = fake_home / ".corvus" / "config.json"
    assert p.is_file()
    assert (p.stat().st_mode & 0o777) == 0o600
    assert json.loads(p.read_text(encoding="utf-8"))["http_port"] == 12345


# ---------------------------------------------------------------------------
# to_public_dict: the single redaction point
# ---------------------------------------------------------------------------

def test_to_public_dict_strips_password_from_ssh_connections() -> None:
    cfg = CorvusConfig(
        ssh_connections=[
            {"name": "A", "host": "h", "port": 22, "username": "u",
             "key_path": "/k", "password": "hunter2"},
            {"name": "B", "host": "h2", "port": 22, "username": "u",
             "key_path": "", "password": "s3cret"},
        ],
    )
    public = to_public_dict(cfg)
    for entry in public["ssh_connections"]:
        assert "password" not in entry
        # key_path is a path, not a secret — kept.
        assert "key_path" in entry
    # Non-ssh fields are untouched.
    assert "mavlink_connection" in public
    assert "http_port" in public


def test_to_public_dict_preserves_theme_and_map() -> None:
    cfg = CorvusConfig(theme={"accent": "#3DA876"}, map={"base_layer": "satellite"})
    public = to_public_dict(cfg)
    assert public["theme"] == {"accent": "#3DA876"}
    assert public["map"] == {"base_layer": "satellite"}


def test_to_public_dict_on_defaults_returns_redactable_shape() -> None:
    """A defaults config still serializes to a dict with ssh_connections key."""
    public = to_public_dict(CorvusConfig())
    assert public["ssh_connections"] == []
    assert public["mavlink_connection"] == CorvusConfig().mavlink_connection
