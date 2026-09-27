"""Which start is a first start: create_server and the config file.

The first start setup (src/js/welcome.js) opens when GET /api/welcome says it
is pending, and create_server decides that once, from whether a config file
was there when the process started. Checked on a real backend here, because
the failure is an ordering one: anything that writes the config during
startup, before the decision is taken, would hide the setup for good.
"""
from __future__ import annotations

import json
import threading

import pytest

import corvus.server as srv


@pytest.fixture
def start(tmp_path, monkeypatch):
    """Build a backend on a config path in tmp; tear every one down after."""
    pytest.importorskip("pymavlink")
    from corvus.mavlink_bridge import MavlinkBridge

    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(cache_dir))
    monkeypatch.setattr(MavlinkBridge, "start", lambda self: None)
    built = []

    def _start(config_path):
        server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999",
                                   config_path=str(config_path))
        # Serving, because stop_backend shuts the HTTP loop down and waits for it.
        thread = threading.Thread(target=server.serve_forever, daemon=True,
                                  kwargs={"poll_interval": 0.05})
        thread.start()
        built.append((server, thread))
        return server

    yield _start
    for server, thread in built:
        srv.stop_backend(server)
        thread.join(timeout=5)


def test_a_station_without_a_config_file_gets_the_setup(tmp_path, start):
    start(tmp_path / "config.json")
    assert srv.CorvusHandler.first_start == {"pending": True}


def test_a_station_with_a_config_file_does_not(tmp_path, start):
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"theme": {"name": "green"}}), encoding="utf-8")
    start(path)
    assert srv.CorvusHandler.first_start == {"pending": False}


def test_a_config_written_during_startup_does_not_hide_the_setup(tmp_path, start, monkeypatch):
    """The decision is taken before anything in create_server can write."""
    path = tmp_path / "config.json"
    real_load = srv.load_config

    def _load_and_write(p):
        cfg = real_load(p)
        srv.save_config(cfg, p)
        return cfg

    monkeypatch.setattr(srv, "load_config", _load_and_write)
    start(path)
    assert path.exists()
    assert srv.CorvusHandler.first_start == {"pending": True}
