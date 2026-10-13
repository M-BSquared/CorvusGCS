"""The fleet on a real backend: wiring, endpoints, persistence and teardown."""
from __future__ import annotations

import http.client
import json
import threading

import pytest

import corvus.server as srv
from corvus.fleet import ActiveBridge, ActiveStore
from corvus.state_store import VehicleStateStore


@pytest.fixture
def backend(tmp_path, monkeypatch):
    pytest.importorskip("pymavlink")
    from corvus.mavlink_bridge import MavlinkBridge

    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(cache_dir))
    monkeypatch.setattr(MavlinkBridge, "start", lambda self: None)
    config_path = tmp_path / "config.json"
    config_path.write_text("{}", encoding="utf-8")
    server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999",
                               config_path=str(config_path))
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    yield server, config_path
    srv.stop_backend(server)
    thread.join(timeout=5)


def _hear(bridge, system: int, armed: bool = False) -> None:
    """The bridge latched *system*, as its connect path does."""
    from pymavlink import mavutil
    base_mode = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
    if armed:
        base_mode |= mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED
    hb = mavutil.mavlink.MAVLink_heartbeat_message(
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR, autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=base_mode, custom_mode=3 << 16, system_status=4, mavlink_version=3,
    )
    hb.pack(mavutil.mavlink.MAVLink(None, srcSystem=system, srcComponent=1))
    bridge.fleet_tracker.bind(system, 1)
    bridge.fleet_tracker.observe(hb, vehicle_heartbeat=True)


def _request(server, method: str, path: str, payload: dict | None = None):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    try:
        body = json.dumps(payload or {}) if method == "POST" else None
        headers = {"Content-Type": "application/json"} if body is not None else {}
        conn.request(method, path, body=body, headers=headers)
        response = conn.getresponse()
        return response.status, json.loads(response.read() or b"{}")
    finally:
        conn.close()


def test_the_http_layer_talks_to_the_main_link_until_told_otherwise(backend) -> None:
    server, _ = backend
    assert isinstance(srv.CorvusHandler.mavlink, ActiveBridge)
    assert isinstance(srv.CorvusHandler.store, ActiveStore)
    assert srv.CorvusHandler.mavlink.target is server.mavlink
    assert srv.CorvusHandler.store.target is server.store
    status, body = _request(server, "GET", "/api/fleet")
    assert status == 200
    assert [link["id"] for link in body["links"]] == ["main"]
    assert body["active_link"] == "main"


def test_a_startup_writes_no_fleet_block(backend) -> None:
    _server, config_path = backend
    assert "fleet" not in json.loads(config_path.read_text(encoding="utf-8"))


def test_an_extra_link_is_opened_saved_selected_and_closed(backend) -> None:
    server, config_path = backend
    status, body = _request(server, "POST", "/api/fleet/links/add",
                            {"connection": "udp:127.0.0.1:9998"})
    assert status == 200 and body["id"] == "link-2"
    saved = json.loads(config_path.read_text(encoding="utf-8"))
    assert saved["fleet"]["links"] == [{"id": "link-2", "connection": "udp:127.0.0.1:9998"}]

    status, body = _request(server, "POST", "/api/fleet/links/add",
                            {"connection": "udp:127.0.0.1:9999"})
    assert status == 400, "the main link's own port cannot be opened twice"

    status, body = _request(server, "POST", "/api/fleet/select", {"id": "link-2:1"})
    assert status == 400, "a link with no vehicle heard on it has nothing to fly"
    extra = server.fleet.link("link-2")
    _hear(extra.bridge, 1)
    status, body = _request(server, "POST", "/api/fleet/select", {"id": "link-2:1"})
    assert status == 200 and body["fleet"]["active_link"] == "link-2"
    assert srv.CorvusHandler.mavlink.target is extra.bridge
    assert srv.CorvusHandler.store.target is extra.store

    # Services bound to the main link say so instead of acting on it.
    status, body = _request(server, "POST", "/api/logs/refresh", {})
    assert status == 409 and "main link" in body["error"]

    # The LINK tab follows the selected vehicle, and its change is saved.
    status, body = _request(server, "POST", "/api/mavlink/connect",
                            {"connection": "udp:127.0.0.1:9997"})
    assert status == 200
    saved = json.loads(config_path.read_text(encoding="utf-8"))
    assert saved["fleet"]["links"][0]["connection"] == "udp:127.0.0.1:9997"
    assert server.mavlink.connection_string() == "udp:127.0.0.1:9999"

    status, body = _request(server, "POST", "/api/fleet/links/remove", {"id": "link-2"})
    assert status == 200
    assert srv.CorvusHandler.mavlink.target is server.mavlink
    saved = json.loads(config_path.read_text(encoding="utf-8"))
    assert "links" not in (saved.get("fleet") or {})


def test_vehicle_names_and_colours_are_saved(backend) -> None:
    server, config_path = backend
    status, _ = _request(server, "POST", "/api/fleet/vehicle",
                         {"id": "main:2", "name": "Scout", "color": "#1fa971"})
    assert status == 200
    saved = json.loads(config_path.read_text(encoding="utf-8"))
    assert saved["fleet"]["vehicles"]["main:2"] == {"name": "Scout", "color": "#1FA971"}
    status, body = _request(server, "POST", "/api/fleet/vehicle",
                            {"id": "main:2", "color": "not a colour"})
    assert status == 400


def test_configured_links_are_opened_at_start_and_closed_with_the_backend(tmp_path, monkeypatch) -> None:
    pytest.importorskip("pymavlink")
    from corvus.mavlink_bridge import MavlinkBridge

    started: list[str] = []
    stopped: list[str] = []
    real_stop = MavlinkBridge.stop
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(tmp_path))
    monkeypatch.setattr(MavlinkBridge, "start",
                        lambda self: started.append(self.connection_string()))

    def tracking_stop(self):
        stopped.append(self.connection_string())
        real_stop(self)

    monkeypatch.setattr(MavlinkBridge, "stop", tracking_stop)
    config_path = tmp_path / "config.json"
    config_path.write_text(json.dumps({"fleet": {"links": [
        {"id": "link-2", "connection": "udp:127.0.0.1:9996"},
    ]}}), encoding="utf-8")
    server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999",
                               config_path=str(config_path))
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    assert "udp:127.0.0.1:9996" in started
    srv.stop_backend(server)
    thread.join(timeout=5)
    assert stopped.index("udp:127.0.0.1:9996") < stopped.index("udp:127.0.0.1:9999"), (
        "the extra links go before the main one"
    )
    assert server.fleet._thread is None


def test_serialised_telemetry_follows_a_switch_even_at_equal_versions() -> None:
    a, b = VehicleStateStore(), VehicleStateStore()
    a.update(battery_voltage=11.1)
    b.update(battery_voltage=22.2)
    assert a.version() == b.version()
    proxy = ActiveStore(a)
    first = srv._serialize_telemetry_snapshot(proxy, proxy.get_snapshot())
    proxy.retarget(b)
    second = srv._serialize_telemetry_snapshot(proxy, proxy.get_snapshot())
    assert json.loads(first)["battery_voltage"] == 11.1
    assert json.loads(second)["battery_voltage"] == 22.2


def test_the_shared_link_switch_is_gone(backend) -> None:
    server, _ = backend
    status, _ = _request(server, "POST", "/api/fleet/settings", {"shared_links": True})
    assert status == 404
    _status, body = _request(server, "GET", "/api/fleet")
    assert "shared_links" not in body


def test_removing_a_vehicle_on_an_extra_link_closes_it(backend) -> None:
    server, config_path = backend
    _request(server, "POST", "/api/fleet/links/add", {"connection": "udp:127.0.0.1:9998"})
    extra = server.fleet.link("link-2")
    _hear(extra.bridge, 1, armed=True)
    _request(server, "POST", "/api/fleet/vehicle", {"id": "link-2:1", "name": "Scout"})
    status, body = _request(server, "POST", "/api/fleet/vehicle/remove", {"id": "link-2:1"})
    assert status == 409 and "armed" in body["error"]
    assert server.fleet.link("link-2") is not None
    status, body = _request(server, "POST", "/api/fleet/vehicle/remove",
                            {"id": "link-2:1", "confirm_armed": True})
    assert status == 200
    assert [v["id"] for v in body["fleet"]["vehicles"]] == []
    assert [link["id"] for link in body["fleet"]["links"]] == ["main"]
    saved = json.loads(config_path.read_text(encoding="utf-8"))
    assert "link-2:1" not in (saved.get("fleet") or {}).get("vehicles", {}), (
        "the next link-2 may carry another aircraft"
    )
    status, _ = _request(server, "POST", "/api/fleet/vehicle/remove", {"id": "link-2:1"})
    assert status == 404


def test_removing_the_main_vehicle_disconnects_and_leaves_it_closed(backend) -> None:
    server, _ = backend
    _hear(server.mavlink, 1)
    server.store.update(vehicle_id="main:1")
    status, body = _request(server, "POST", "/api/fleet/vehicle/remove", {"id": "main:1"})
    assert status == 200
    assert body["fleet"]["vehicles"] == []
    assert server.store.get_snapshot()["vehicle_id"] == ""
    assert server.mavlink.fleet_tracker.bound_system() is None


def test_auto_connect_is_told_which_ports_the_extra_links_hold(backend) -> None:
    server, _ = backend
    _request(server, "POST", "/api/fleet/links/add", {"connection": "serial:/dev/ttyUSB7:57600"})
    assert server.fleet.extra_serial_devices() == ["/dev/ttyUSB7"]
    watcher = getattr(server, "autoconnect", None)
    if watcher is not None:
        assert watcher._busy() == ["/dev/ttyUSB7"]
