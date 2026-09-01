from __future__ import annotations

import json
from typing import Any

import pytest

from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore


class FakeParamBridge:
    """Stand-in for ``MavlinkBridge`` exposing the param/calibrate/autotune API."""

    def __init__(
        self,
        *,
        param_status: dict | None = None,
        params: list[dict] | None = None,
        result: bool = True,
        error: str = "",
    ) -> None:
        self._param_status = param_status or {"state": "idle", "count": 0, "received": 0}
        self._params = params or []
        self.result = result
        self.error = error
        self.requested_lists: int = 0
        self.set_calls: list[tuple[str, float]] = []
        self.calibrate_calls: list[str] = []
        self.autotune_calls: list[str] = []
        self.param_listeners: list[Any] = []
        self.add_listener_calls: int = 0
        self.remove_listener_calls: int = 0

    def request_param_list(self) -> bool:
        self.requested_lists += 1
        return self.result

    def request_param(self, name: str) -> bool:
        return self.result

    def set_param(self, name: str, value: float) -> bool:
        self.set_calls.append((name, value))
        return self.result

    def get_params(self) -> list[dict]:
        return [dict(p) for p in self._params]

    def get_param(self, name: str) -> dict | None:
        for p in self._params:
            if p["name"] == name:
                return dict(p)
        return None

    def param_status(self) -> dict:
        return dict(self._param_status)

    def add_param_listener(self, fn: Any) -> None:
        self.add_listener_calls += 1
        self.param_listeners.append(fn)

    def remove_param_listener(self, fn: Any) -> None:
        self.remove_listener_calls += 1
        try:
            self.param_listeners.remove(fn)
        except ValueError:
            pass

    def calibrate(self, sensor: str) -> bool:
        self.calibrate_calls.append(sensor)
        return self.result

    def autotune(self, axis: str) -> bool:
        self.autotune_calls.append(axis)
        return self.result

    def get_last_command_error(self) -> str:
        return self.error


def _handler_with_bridge(
    bridge: Any,
) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _handler_without_bridge() -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = None
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


# ---- GET /api/params ----

def test_get_params_complete_returns_full_param_list() -> None:
    params = [
        {"name": "MC_ROLL_P", "value": 6.0, "type": "FLOAT"},
        {"name": "MC_PITCH_P", "value": 7.0, "type": "FLOAT"},
        {"name": "MC_YAW_P", "value": 2.5, "type": "FLOAT"},
    ]
    bridge = FakeParamBridge(
        param_status={"state": "complete", "count": 3, "received": 3},
        params=params,
    )
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params()

    assert responses == [({
        "state": "complete",
        "count": 3,
        "received": 3,
        "complete": True,
        "params": params,
    }, 200)]


def test_get_params_downloading_is_lean_no_partial_list() -> None:
    bridge = FakeParamBridge(
        param_status={"state": "downloading", "count": 100, "received": 42},
        params=[{"name": "X", "value": 1.0, "type": "FLOAT"}],
    )
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params()

    assert len(responses) == 1
    payload, status = responses[0]
    assert status == 200
    assert payload["state"] == "downloading"
    assert payload["count"] == 100
    assert payload["received"] == 42
    assert payload["complete"] is False
    # lean: partial param lists are not sent — the editor waits for a complete set
    assert payload["params"] == []


def test_get_params_without_bridge_returns_idle_empty() -> None:
    handler, responses = _handler_without_bridge()

    handler._api_params()

    assert responses == [({
        "state": "idle",
        "count": 0,
        "received": 0,
        "complete": False,
        "params": [],
    }, 200)]


def test_get_params_route_dispatches_to_helper() -> None:
    bridge = FakeParamBridge(
        param_status={"state": "complete", "count": 1, "received": 1},
        params=[{"name": "MC_ROLL_P", "value": 6.0, "type": "FLOAT"}],
    )
    handler, responses = _handler_with_bridge(bridge)

    handler._handle_api_get("/api/params")

    assert responses == [({
        "state": "complete",
        "count": 1,
        "received": 1,
        "complete": True,
        "params": [{"name": "MC_ROLL_P", "value": 6.0, "type": "FLOAT"}],
    }, 200)]


# ---- POST /api/params/download ----

def test_params_download_ok_returns_downloading() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_download({})

    assert bridge.requested_lists == 1
    assert responses == [({"ok": True, "state": "downloading"}, 200)]


def test_params_download_disconnected_returns_503() -> None:
    bridge = FakeParamBridge(result=False, error="not connected")
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_download({})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


def test_params_download_without_bridge_returns_503() -> None:
    handler, responses = _handler_without_bridge()

    handler._api_params_download({})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


# ---- POST /api/params/set ----

def test_params_set_ok_returns_ok() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_set({"name": "MC_ROLL_P", "value": 6.5})

    assert bridge.set_calls == [("MC_ROLL_P", 6.5)]
    assert responses == [({"ok": True}, 200)]


def test_params_set_accepts_int_value() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_set({"name": "COM_ARM_NUM", "value": 3})

    assert bridge.set_calls == [("COM_ARM_NUM", 3.0)]
    assert responses == [({"ok": True}, 200)]


def test_params_set_rejected_returns_409() -> None:
    bridge = FakeParamBridge(result=False, error="param rejected by vehicle")
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_set({"name": "MC_ROLL_P", "value": 6.5})

    assert responses == [({"ok": False, "error": "param rejected by vehicle"}, 409)]


def test_params_set_disconnected_returns_503() -> None:
    bridge = FakeParamBridge(result=False, error="not connected")
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_set({"name": "MC_ROLL_P", "value": 6.5})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


def test_params_set_rejects_bool_value() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_set({"name": "MC_ROLL_P", "value": True})

    assert bridge.set_calls == []
    assert responses == [({"ok": False, "error": "value must be a number"}, 400)]


def test_params_set_rejects_non_numeric_value() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_set({"name": "MC_ROLL_P", "value": "6.5"})

    assert bridge.set_calls == []
    assert responses == [({"ok": False, "error": "value must be a number"}, 400)]


def test_params_set_rejects_missing_name() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_params_set({"value": 6.5})

    assert bridge.set_calls == []
    payload, status = responses[0]
    assert status == 400
    assert payload["ok"] is False


def test_params_set_without_bridge_returns_503() -> None:
    handler, responses = _handler_without_bridge()

    handler._api_params_set({"name": "MC_ROLL_P", "value": 6.5})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


# ---- POST /api/calibrate ----

def test_calibrate_compass_ok_calls_bridge_lowercased() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_calibrate({"type": "compass"})

    assert bridge.calibrate_calls == ["compass"]
    assert responses == [({"ok": True}, 200)]


def test_calibrate_accepts_case_insensitive_type() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_calibrate({"type": "GYRO"})

    assert bridge.calibrate_calls == ["gyro"]
    assert responses == [({"ok": True}, 200)]


def test_calibrate_unknown_type_returns_400() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_calibrate({"type": "foo"})

    assert bridge.calibrate_calls == []
    payload, status = responses[0]
    assert status == 400
    assert payload["ok"] is False


def test_calibrate_armed_rejected_returns_409() -> None:
    bridge = FakeParamBridge(result=False, error="cannot calibrate while armed")
    handler, responses = _handler_with_bridge(bridge)

    handler._api_calibrate({"type": "gyro"})

    assert responses == [({"ok": False, "error": "cannot calibrate while armed"}, 409)]


def test_calibrate_without_bridge_returns_503() -> None:
    handler, responses = _handler_without_bridge()

    handler._api_calibrate({"type": "gyro"})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


# ---- POST /api/autotune ----

def test_autotune_roll_ok_calls_bridge_lowercased() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_autotune({"axis": "roll"})

    assert bridge.autotune_calls == ["roll"]
    assert responses == [({"ok": True}, 200)]


def test_autotune_accepts_case_insensitive_axis() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_autotune({"axis": "ALL"})

    assert bridge.autotune_calls == ["all"]
    assert responses == [({"ok": True}, 200)]


def test_autotune_unknown_axis_returns_400() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_autotune({"axis": "foobar"})

    assert bridge.autotune_calls == []
    payload, status = responses[0]
    assert status == 400
    assert payload["ok"] is False


def test_autotune_rejected_returns_409() -> None:
    bridge = FakeParamBridge(result=False, error="autotune denied")
    handler, responses = _handler_with_bridge(bridge)

    handler._api_autotune({"axis": "roll"})

    assert responses == [({"ok": False, "error": "autotune denied"}, 409)]


def test_autotune_without_bridge_returns_503() -> None:
    handler, responses = _handler_without_bridge()

    handler._api_autotune({"axis": "roll"})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


# ---- POST route dispatch ----

def test_post_params_download_route_dispatches_to_helper() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)
    handler.headers = _Headers(b'{"connection": "udp:x"}')  # type: ignore[assignment]
    handler.rfile = _ScriptedReader([b'{"connection": "udp:x"}'])  # type: ignore[assignment]

    handler._handle_api_post("/api/params/download")

    assert bridge.requested_lists == 1
    assert responses == [({"ok": True, "state": "downloading"}, 200)]


def test_post_calibrate_route_dispatches_to_helper() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)
    body = b'{"type": "gyro"}'
    handler.headers = _Headers(body)  # type: ignore[assignment]
    handler.rfile = _ScriptedReader([body])  # type: ignore[assignment]

    handler._handle_api_post("/api/calibrate")

    assert bridge.calibrate_calls == ["gyro"]
    assert responses == [({"ok": True}, 200)]


def test_post_autotune_route_dispatches_to_helper() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)
    body = b'{"axis": "all"}'
    handler.headers = _Headers(body)  # type: ignore[assignment]
    handler.rfile = _ScriptedReader([body])  # type: ignore[assignment]

    handler._handle_api_post("/api/autotune")

    assert bridge.autotune_calls == ["all"]
    assert responses == [({"ok": True}, 200)]


def test_post_params_set_route_dispatches_to_helper() -> None:
    bridge = FakeParamBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)
    body = b'{"name": "MC_ROLL_P", "value": 6.5}'
    handler.headers = _Headers(body)  # type: ignore[assignment]
    handler.rfile = _ScriptedReader([body])  # type: ignore[assignment]

    handler._handle_api_post("/api/params/set")

    assert bridge.set_calls == [("MC_ROLL_P", 6.5)]
    assert responses == [({"ok": True}, 200)]


class _Headers:
    """Minimal stand-in for http.client.HTTPMessage exposing ``get``."""

    def __init__(self, body: bytes) -> None:
        self._length = str(len(body))

    def get(self, name: str, default: Any = None) -> Any:
        if name == "Content-Length":
            return self._length
        return default


class _ScriptedReader:
    """``rfile`` stub returning scripted payloads one per ``read`` call."""

    def __init__(self, payloads: list[bytes]) -> None:
        self._payloads = payloads
        self._idx = 0

    def read(self, n: int) -> bytes:
        if self._idx >= len(self._payloads):
            return b"{}"
        payload = self._payloads[self._idx]
        self._idx += 1
        return payload


# ---- SSE /api/params/progress ----

def _make_sse_handler(bridge: Any) -> tuple[CorvusHandler, list[tuple[str, str]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    sent: list[tuple[str, str]] = []
    handler._send_sse = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    handler.send_response = lambda status: None  # type: ignore[method-assign]
    handler.send_header = lambda name, value: None  # type: ignore[method-assign]
    handler.end_headers = lambda: None  # type: ignore[method-assign]
    return handler, sent


def test_sse_params_cleans_up_listener_on_disconnect(monkeypatch: pytest.MonkeyPatch) -> None:
    import corvus.server as server_module

    def boom(self: Any, timeout: float | None = None) -> Any:
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(server_module._BoundedSseBuffer, "get", boom)

    bridge = FakeParamBridge(
        param_status={"state": "downloading", "count": 50, "received": 10},
    )
    handler, sent = _make_sse_handler(bridge)

    handler._sse_params()

    assert bridge.add_listener_calls == 1
    assert bridge.remove_listener_calls == 1
    assert bridge.param_listeners == []
    # initial progress event emitted before the loop broke
    assert len(sent) == 1
    assert sent[0][0] == "progress"
    assert json.loads(sent[0][1]) == {
        "state": "downloading",
        "count": 50,
        "received": 10,
    }


def test_sse_params_without_bridge_sends_idle_and_skips_listener(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import corvus.server as server_module

    def boom(self: Any, timeout: float | None = None) -> Any:
        raise BrokenPipeError("client gone")

    monkeypatch.setattr(server_module._BoundedSseBuffer, "get", boom)

    handler = object.__new__(CorvusHandler)
    handler.mavlink = None
    sent: list[tuple[str, str]] = []
    handler._send_sse = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    handler.send_response = lambda status: None  # type: ignore[method-assign]
    handler.send_header = lambda name, value: None  # type: ignore[method-assign]
    handler.end_headers = lambda: None  # type: ignore[method-assign]

    handler._sse_params()

    assert sent == [
        ("progress", json.dumps({"state": "idle", "count": 0, "received": 0})),
    ]


# ---- State store body-rate fields ----

def test_state_store_snapshot_has_body_rate_defaults() -> None:
    store = VehicleStateStore()
    snap = store.get_snapshot()

    assert snap["rollspeed"] == 0.0
    assert snap["pitchspeed"] == 0.0
    assert snap["yawspeed"] == 0.0


def test_state_store_update_body_rates_flows_into_snapshot() -> None:
    store = VehicleStateStore()
    store.update(rollspeed=12.5, pitchspeed=-3.0, yawspeed=0.5)

    snap = store.get_snapshot()
    assert snap["rollspeed"] == 12.5
    assert snap["pitchspeed"] == -3.0
    assert snap["yawspeed"] == 0.5
