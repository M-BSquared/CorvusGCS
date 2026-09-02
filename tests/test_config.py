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
