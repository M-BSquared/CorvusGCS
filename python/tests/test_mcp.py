"""The MCP server (corvus/mcp.py, corvus/routes/mcp.py).

What is pinned here:

* it is off until the operator turns it on, answers only this computer, and
  only with the token, which lives in its own file and never in the config;
* a tool whose permission is off is neither listed nor run, and a vehicle of
  the fleet is reachable only when the operator allowed that;
* "ask first" waits for the operator, refuses without a window to ask in,
  and Stop refuses whatever waits and takes every changing permission away;
* direct control is repeated while it lasts and ends in hold, which is the
  watchdog the operator was promised;
* a plugin tool is offered only while a window announces it, and its call
  goes through that window and back;
* every tool reaches the vehicle through the station's own endpoint.
"""
from __future__ import annotations

import http.client
import json
import os
import stat
import threading
import time
from typing import Any

import pytest

from corvus import mcp
from corvus.config import CorvusConfig, _build_config, _config_to_dict
from corvus import settings_bundle


class Station:
    """A stand-in for the endpoints: records calls, answers from a table."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, Any]] = []
        self.replies: dict[str, tuple[int, dict[str, Any]]] = {}
        self.lock = threading.Lock()

    def __call__(self, method: str, path: str, body: Any) -> tuple[int, dict[str, Any]]:
        with self.lock:
            self.calls.append((method, path, body))
        return self.replies.get(path.split("?")[0], (200, {"ok": True}))

    def paths(self) -> list[str]:
        with self.lock:
            return [p for _m, p, _b in self.calls]


@pytest.fixture
def svc(tmp_path):
    station = Station()
    saved: dict[str, Any] = {"block": {"enabled": True}}

    def save(block):
        saved["block"] = block

    service = mcp.McpService(dispatch=station, load_settings=lambda: saved["block"],
                             save_settings=save, token_path=tmp_path / "mcp-token")
    service.station = station  # type: ignore[attr-defined]
    service.saved = saved  # type: ignore[attr-defined]
    yield service
    service.shutdown()


def names(service: mcp.McpService) -> set[str]:
    return {t["name"] for t in service.tools()}


def text(result: dict[str, Any]) -> str:
    return result["content"][0]["text"]


# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

def test_defaults_are_off_and_read_only() -> None:
    s = mcp.effective_settings(None)
    assert s["enabled"] is False and s["fleet"] is False
    on = {k for k, v in s["permissions"].items() if v}
    assert on == {"telemetry", "params_read"}
    asking = {k for k, v in s["confirm"].items() if v}
    assert {"arm", "flight", "control", "params_write"} <= asking
    assert not s["confirm"]["telemetry"], "reading never asks"


def test_only_real_booleans_survive_the_config_file() -> None:
    raw = {"enabled": "true", "fleet": True,
           "permissions": {"arm": "yes", "flight": True, "bogus": True},
           "plugins": {"schwalby": False, "../x": True}}
    assert mcp.coerce_settings(raw) == {
        "fleet": True, "permissions": {"flight": True}, "plugins": {"schwalby": False}}


def test_the_block_round_trips_through_the_config_and_travels_in_connections() -> None:
    cfg = _build_config({"mcp": {"enabled": True, "permissions": {"arm": True}}})
    assert _config_to_dict(cfg)["mcp"] == {"enabled": True, "permissions": {"arm": True}}
    assert _config_to_dict(CorvusConfig()).get("mcp") is None
    assert settings_bundle.section_of("mcp") == "connections"


def test_update_rejects_what_is_not_a_boolean(svc) -> None:
    with pytest.raises(ValueError):
        svc.update_settings({"permissions": {"arm": 1}})
    with pytest.raises(ValueError):
        svc.update_settings({"permissions": {"launch_missiles": True}})
    svc.update_settings({"permissions": {"arm": True}, "confirm": {"arm": False}})
    assert mcp.level(svc.settings(), "arm") == "allow"


# ---------------------------------------------------------------------------
# Token
# ---------------------------------------------------------------------------

def test_the_token_is_its_own_private_file_and_can_be_replaced(svc, tmp_path) -> None:
    token = svc.token()
    path = tmp_path / "mcp-token"
    assert path.read_text().strip() == token and len(token) >= 32
    if os.name == "posix":
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
    assert svc.authorized(f"Bearer {token}")
    assert not svc.authorized(f"Bearer {token}x")
    assert not svc.authorized(token)
    assert not svc.authorized(None)
    fresh = svc.regenerate_token()
    assert fresh != token and not svc.authorized(f"Bearer {token}")


# ---------------------------------------------------------------------------
# The protocol
# ---------------------------------------------------------------------------

def rpc(svc, method, params=None, session=None, mid=1):
    msg = {"jsonrpc": "2.0", "id": mid, "method": method}
    if params is not None:
        msg["params"] = params
    return svc.handle_post(msg, session)


def test_initialize_opens_a_session_and_names_the_version(svc) -> None:
    status, reply, headers = rpc(svc, "initialize", {
        "protocolVersion": "2025-06-18", "clientInfo": {"name": "opencode", "version": "1"}})
    assert status == 200
    assert reply["result"]["protocolVersion"] == "2025-06-18"
    assert reply["result"]["capabilities"]["tools"]["listChanged"] is True
    assert reply["result"]["serverInfo"]["name"] == "corvus-gcs"
    sid = headers["Mcp-Session-Id"]
    assert svc.status()["clients"][0]["name"] == "opencode"

    status, reply, _ = svc.handle_post(
        {"jsonrpc": "2.0", "method": "notifications/initialized"}, sid)
    assert status == 202 and reply is None
    status, reply, _ = rpc(svc, "ping", session=sid)
    assert reply["result"] == {}
    status, reply, _ = rpc(svc, "ping", session="not-a-session")
    assert status == 404, "a stale session is told to initialize again"
    _status, reply, _ = rpc(svc, "initialize", {"protocolVersion": "1999-01-01"})
    assert reply["result"]["protocolVersion"] == mcp.DEFAULT_PROTOCOL
    _status, reply, _ = rpc(svc, "nope")
    assert reply["error"]["code"] == mcp.JSONRPC_METHOD_NOT_FOUND
    _status, reply, _ = rpc(svc, "tools/call", {"name": "does_not_exist", "arguments": {}})
    assert reply["error"]["code"] == mcp.JSONRPC_INVALID_PARAMS


def test_only_permitted_tools_are_listed(svc) -> None:
    listed = names(svc)
    assert {"get_vehicle_state", "list_parameters"} <= listed
    assert not {"arm", "takeoff", "set_parameter", "fly_to", "set_velocity"} & listed
    svc.update_settings({"permissions": {"flight": True}})
    assert {"takeoff", "land", "hold"} <= names(svc)
    schema = next(t for t in svc.tools() if t["name"] == "takeoff")["inputSchema"]
    assert "vehicle" not in schema["properties"], "one vehicle unless the operator allows more"
    svc.update_settings({"fleet": True})
    schema = next(t for t in svc.tools() if t["name"] == "takeoff")["inputSchema"]
    assert "vehicle" in schema["properties"]


def test_a_tool_that_is_off_is_refused_and_never_dispatched(svc) -> None:
    result = svc.call_tool("arm", {})
    assert result["isError"] and "not allowed" in text(result)
    assert "/api/mavlink/arm" not in svc.station.paths()


def test_another_vehicle_needs_the_fleet_switch(svc) -> None:
    result = svc.call_tool("get_vehicle_state", {"vehicle": "main:2"})
    assert result["isError"] and "only the vehicle they fly" in text(result)
    svc.update_settings({"fleet": True})
    result = svc.call_tool("get_vehicle_state", {"vehicle": "main:2"})
    assert not result["isError"]
    assert svc.station.paths()[-1] == "/api/state?vehicle=main%3A2"


def test_a_read_reaches_the_endpoint_and_returns_its_answer(svc) -> None:
    svc.station.replies["/api/state"] = (200, {"armed": False, "mode": "HOLD"})
    result = svc.call_tool("get_vehicle_state", {})
    assert result["structuredContent"] == {"armed": False, "mode": "HOLD"}
    assert not result["isError"]


def test_a_refusal_by_the_station_is_the_agents_answer(svc) -> None:
    svc.update_settings({"permissions": {"flight": True}, "confirm": {"flight": False}})
    svc.station.replies["/api/mavlink/takeoff"] = (409, {"ok": False, "error": "not armable"})
    result = svc.call_tool("takeoff", {"altitude": 10})
    assert result["isError"] and text(result) == "not armable"
    assert ("POST", "/api/mavlink/takeoff", {"altitude": 10}) in svc.station.calls


def test_a_parameter_the_firmware_did_not_report_is_not_written(svc) -> None:
    svc.update_settings({"permissions": {"params_write": True},
                         "confirm": {"params_write": False}})
    svc.station.replies["/api/params"] = (200, {
        "complete": True, "params": [{"name": "MPC_XY_VEL_MAX", "value": 12.0, "type": 9}]})
    result = svc.call_tool("set_parameter", {"name": "MPC_NOT_A_THING", "value": 1})
    assert result["isError"] and "not a parameter" in text(result)
    assert "/api/params/set" not in svc.station.paths()
    result = svc.call_tool("set_parameter", {"name": "MPC_XY_VEL_MAX", "value": 8})
    assert not result["isError"]
    assert ("POST", "/api/params/set", {"name": "MPC_XY_VEL_MAX", "value": 8.0}) in svc.station.calls


def test_parameters_wait_for_the_full_list(svc) -> None:
    svc.station.replies["/api/params"] = (200, {"complete": False, "state": "downloading",
                                                "received": 10, "count": 900})
    result = svc.call_tool("list_parameters", {"prefix": "MPC_"})
    assert result["isError"] and "download_parameters" in text(result)


# ---------------------------------------------------------------------------
# Asking the operator
# ---------------------------------------------------------------------------

def answer_soon(svc, allow, events):
    def run():
        for _ in range(200):
            pending = svc.status()["pending"]
            if pending:
                svc.answer(pending[0]["id"], allow)
                return
            time.sleep(0.01)
    events.append(threading.Thread(target=run, daemon=True))
    events[-1].start()


def test_ask_first_needs_an_open_window(svc) -> None:
    svc.update_settings({"permissions": {"arm": True}})
    result = svc.call_tool("arm", {})
    assert result["isError"] and "no Corvus window" in text(result)
    assert "/api/mavlink/arm" not in svc.station.paths()


def test_ask_first_waits_for_the_operator(svc) -> None:
    svc.update_settings({"permissions": {"arm": True}})
    svc.window_heartbeat("w1", [])
    seen: list[dict] = []
    svc.add_listener(seen.append)
    threads: list[threading.Thread] = []

    answer_soon(svc, False, threads)
    result = svc.call_tool("arm", {})
    assert result["isError"] and "refused" in text(result)
    assert "/api/mavlink/arm" not in svc.station.paths()
    assert any(e.get("type") == "confirm" and e["summary"] == "arm" for e in seen)

    answer_soon(svc, True, threads)
    result = svc.call_tool("arm", {})
    assert not result["isError"]
    assert ("POST", "/api/mavlink/arm", {"arm": True}) in svc.station.calls


def test_stop_refuses_what_waits_and_takes_every_write_away(svc) -> None:
    svc.update_settings({"permissions": {"arm": True, "flight": True, "mission": True}})
    svc.window_heartbeat("w1", [])
    out: list[dict] = []
    worker = threading.Thread(target=lambda: out.append(svc.call_tool("arm", {})), daemon=True)
    worker.start()
    for _ in range(200):
        if svc.status()["pending"]:
            break
        time.sleep(0.01)
    svc.stop()
    worker.join(timeout=2)
    assert out and out[0]["isError"] and "stopped" in text(out[0])
    perms = svc.settings()["permissions"]
    assert not any(perms[p.id] for p in mcp.PERMISSIONS if p.writes)
    assert perms["telemetry"], "the agent can still see what happened"


# ---------------------------------------------------------------------------
# Direct control and its watchdog
# ---------------------------------------------------------------------------

def test_direct_control_repeats_then_holds(svc) -> None:
    svc.update_settings({"permissions": {"control": True}, "confirm": {"control": False}})
    result = svc.call_tool("set_velocity", {"vx": 1.0, "duration_s": 0.5})
    assert not result["isError"]
    assert svc.status()["control"]["kind"] == "velocity"
    time.sleep(1.2)
    paths = svc.station.paths()
    assert paths.count("/api/mavlink/velocity") >= 3, "fed several times a second"
    assert paths[-1] == "/api/mavlink/hold", "holds once nothing new arrives"
    assert svc.status()["control"] is None


def test_control_asks_once_per_stretch(svc) -> None:
    svc.update_settings({"permissions": {"control": True}})
    svc.window_heartbeat("w1", [])
    threads: list[threading.Thread] = []
    answer_soon(svc, True, threads)
    assert not svc.call_tool("manual_control", {"x": 0.2, "duration_s": 3})["isError"]
    # Still flying: the next input goes without another question.
    started = time.monotonic()
    assert not svc.call_tool("manual_control", {"x": 0.1, "duration_s": 1})["isError"]
    assert time.monotonic() - started < 1.0
    assert not svc.call_tool("stop_control", {})["isError"]
    assert svc.station.paths()[-1] == "/api/mavlink/hold"


def test_a_refused_first_frame_starts_nothing(svc) -> None:
    svc.update_settings({"permissions": {"control": True}, "confirm": {"control": False}})
    svc.station.replies["/api/mavlink/velocity"] = (409, {"ok": False, "error": "not on a plane"})
    result = svc.call_tool("set_velocity", {"vx": 1.0})
    assert result["isError"] and "plane" in text(result)
    assert svc.status()["control"] is None


def test_turning_control_off_holds_at_once(svc) -> None:
    svc.update_settings({"permissions": {"control": True}, "confirm": {"control": False}})
    svc.call_tool("set_velocity", {"vx": 1.0, "duration_s": 5})
    svc.update_settings({"permissions": {"control": False}})
    assert svc.status()["control"] is None
    assert svc.station.paths()[-1] == "/api/mavlink/hold"


def test_shutdown_holds_a_vehicle_under_control(svc) -> None:
    svc.update_settings({"permissions": {"control": True}, "confirm": {"control": False}})
    svc.call_tool("manual_control", {"x": 0.5, "duration_s": 5})
    svc.shutdown()
    assert svc.station.paths()[-1] == "/api/mavlink/hold"
    assert svc._control_thread is None or not svc._control_thread.is_alive()


# ---------------------------------------------------------------------------
# Plugin tools
# ---------------------------------------------------------------------------

SHELF_TOOL = {"plugin": "schwalby", "plugin_name": "Schwalby", "name": "add_button",
              "description": "Add a button", "input_schema": {
                  "type": "object", "properties": {"label": {"type": "string"}}}}


def test_plugin_tools_need_a_window_the_permission_and_the_plugin_switch(svc) -> None:
    svc.window_heartbeat("w1", [SHELF_TOOL, {"plugin": "x", "name": "bad name!"}])
    assert "schwalby_add_button" not in names(svc), "plugin tools start off"
    svc.update_settings({"permissions": {"plugins": True}})
    assert "schwalby_add_button" in names(svc)
    assert svc.plugin_summary() == [{"id": "schwalby", "name": "Schwalby",
                                     "tools": ["schwalby_add_button"]}]
    svc.update_settings({"plugins": {"schwalby": False}})
    assert "schwalby_add_button" not in names(svc)
    svc.update_settings({"plugins": {"schwalby": True}})
    svc._windows["w1"]["seen"] -= mcp.WINDOW_TTL_S + 1
    assert "schwalby_add_button" not in names(svc), "gone with its window"


def test_a_plugin_call_goes_through_its_window_and_back(svc) -> None:
    svc.update_settings({"permissions": {"plugins": True}})
    svc.window_heartbeat("w1", [SHELF_TOOL])
    calls: list[dict] = []

    def window(event):
        if event.get("type") == "call":
            calls.append(event)
            threading.Thread(target=svc.call_result, daemon=True, args=(
                event["id"], True, {"id": "b1", "pending": True}, None)).start()

    svc.add_listener(window)
    result = svc.call_tool("schwalby_add_button", {"label": "Logger"})
    assert result["structuredContent"] == {"id": "b1", "pending": True}
    assert calls[0]["window"] == "w1" and calls[0]["tool"] == "add_button"
    assert calls[0]["args"] == {"label": "Logger"}


def test_the_tool_list_change_reaches_an_open_stream(svc) -> None:
    _s, _r, headers = rpc(svc, "initialize", {"protocolVersion": "2025-06-18"})
    stream = svc.open_stream(headers["Mcp-Session-Id"])
    svc.update_settings({"permissions": {"arm": True}})
    note = stream.get(timeout=1)
    assert note["method"] == "notifications/tools/list_changed"


# ---------------------------------------------------------------------------
# Over HTTP, on a real backend
# ---------------------------------------------------------------------------

@pytest.fixture
def backend(tmp_path, monkeypatch):
    pytest.importorskip("pymavlink")
    import corvus.server as srv
    from corvus.mavlink_bridge import MavlinkBridge

    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(cache_dir))
    monkeypatch.setattr(MavlinkBridge, "start", lambda self: None)
    server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999",
                               config_path=str(tmp_path / "config.json"))
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    yield server
    srv.stop_backend(server)
    thread.join(timeout=5)


def request(server, method, path, body=None, headers=None):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=10)
    data = json.dumps(body).encode() if body is not None else None
    hdrs = {"Content-Type": "application/json", **(headers or {})}
    conn.request(method, path, body=data, headers=hdrs)
    resp = conn.getresponse()
    raw = resp.read()
    conn.close()
    return resp.status, (json.loads(raw) if raw else None), dict(resp.getheaders())


def test_over_http_off_then_token_then_tools(backend, tmp_path) -> None:
    init = {"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "test"}}}
    status, body, _ = request(backend, "POST", "/mcp", init)
    assert status == 403 and "switched off" in body["error"]["message"]

    status, card, _ = request(backend, "GET", "/api/mcp")
    assert status == 200 and card["settings"]["enabled"] is False
    assert card["url"] == f"http://127.0.0.1:{backend.server_address[1]}/mcp"
    token = card["token"]
    assert len(token) >= 32
    assert (tmp_path / "mcp-token").is_file()

    status, body, _ = request(backend, "POST", "/api/mcp/settings", {"enabled": True})
    assert status == 200 and body["settings"]["enabled"] is True
    config = json.loads((tmp_path / "config.json").read_text())
    assert config["mcp"] == {"enabled": True}
    assert token not in json.dumps(config), "the token never goes in the config"

    status, body, headers = request(backend, "POST", "/mcp", init)
    assert status == 401 and "Bearer" in headers.get("WWW-Authenticate", "")

    auth = {"Authorization": f"Bearer {token}"}
    status, body, headers = request(backend, "POST", "/mcp", init, auth)
    assert status == 200 and body["result"]["serverInfo"]["name"] == "corvus-gcs"
    session = {"Mcp-Session-Id": headers["Mcp-Session-Id"], **auth}

    status, body, _ = request(backend, "POST", "/mcp",
                              {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, session)
    assert "get_vehicle_state" in {t["name"] for t in body["result"]["tools"]}

    status, body, _ = request(backend, "POST", "/mcp", {
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "get_vehicle_state", "arguments": {}}}, session)
    state = body["result"]["structuredContent"]
    assert "armed" in state and "position" in state, "the real /api/state answered"

    status, body, _ = request(backend, "GET", "/mcp", None, auth)
    assert status == 405, "no stream without asking for one"
    status, _body, _ = request(backend, "DELETE", "/mcp", None, session)
    assert status == 200


def test_the_ui_topic_is_on_the_event_stream(backend) -> None:
    conn = http.client.HTTPConnection("127.0.0.1", backend.server_address[1], timeout=5)
    conn.request("GET", "/api/events?topics=mcp")
    resp = conn.getresponse()
    assert resp.status == 200
    first = b""
    while b"\n\n" not in first:
        first += resp.read1(256) if hasattr(resp, "read1") else resp.read(1)
    conn.close()
    assert b"event: mcp" in first and b'"type": "status"' in first
