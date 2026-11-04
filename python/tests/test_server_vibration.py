from __future__ import annotations

from typing import Any

from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore


class FakeVibrationBridge:
    """Stand-in for ``MavlinkBridge`` exposing the vibration-stream API."""

    def __init__(self, *, result: bool = True, error: str = "") -> None:
        self.result = result
        self.error = error
        self.stream_calls: list[tuple[bool, int]] = []

    def set_vibration_stream(self, enabled: bool, rate_hz: int) -> bool:
        self.stream_calls.append((enabled, rate_hz))
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


# ---- POST /api/vibration/stream: happy paths ----

def test_vibration_stream_enable_ok_calls_bridge_and_echoes_params() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 10})

    assert bridge.stream_calls == [(True, 10)]
    assert responses == [({"ok": True, "enabled": True, "rate_hz": 10}, 200)]


def test_vibration_stream_disable_still_echoes_rate_hz() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": False, "rate_hz": 10})

    assert bridge.stream_calls == [(False, 10)]
    # rate_hz is echoed even when disabling (the UI restores the default on close)
    assert responses == [({"ok": True, "enabled": False, "rate_hz": 10}, 200)]


def test_vibration_stream_rate_hz_missing_defaults_to_10() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True})

    assert bridge.stream_calls == [(True, 10)]
    assert responses == [({"ok": True, "enabled": True, "rate_hz": 10}, 200)]


def test_vibration_stream_integral_float_accepted() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 10.0})

    assert bridge.stream_calls == [(True, 10)]
    assert responses == [({"ok": True, "enabled": True, "rate_hz": 10}, 200)]


def test_vibration_stream_boundary_rates_accepted() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 1})
    handler._api_vibration_stream({"enabled": True, "rate_hz": 50})

    assert bridge.stream_calls == [(True, 1), (True, 50)]
    assert responses == [
        ({"ok": True, "enabled": True, "rate_hz": 1}, 200),
        ({"ok": True, "enabled": True, "rate_hz": 50}, 200),
    ]


# ---- POST /api/vibration/stream: validation (400) ----

def test_vibration_stream_enabled_not_bool_returns_400() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": "true", "rate_hz": 10})

    assert bridge.stream_calls == []
    assert responses == [({"ok": False, "error": "enabled must be boolean"}, 400)]


def test_vibration_stream_enabled_missing_returns_400() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"rate_hz": 10})

    assert bridge.stream_calls == []
    assert responses == [({"ok": False, "error": "enabled must be boolean"}, 400)]


def test_vibration_stream_non_integral_float_returns_400() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 10.5})

    assert bridge.stream_calls == []
    assert responses == [
        ({"ok": False, "error": "rate_hz must be between 1 and 50 Hz"}, 400)
    ]


def test_vibration_stream_rate_zero_returns_400() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 0})

    assert bridge.stream_calls == []
    assert responses == [
        ({"ok": False, "error": "rate_hz must be between 1 and 50 Hz"}, 400)
    ]


def test_vibration_stream_rate_over_max_returns_400() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 51})

    assert bridge.stream_calls == []
    assert responses == [
        ({"ok": False, "error": "rate_hz must be between 1 and 50 Hz"}, 400)
    ]


def test_vibration_stream_rate_bool_returns_400_number_message() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": True})

    # bool is a subclass of int — rejected before the range check, as a number-type error
    assert bridge.stream_calls == []
    assert responses == [({"ok": False, "error": "rate_hz must be a number"}, 400)]


def test_vibration_stream_rate_string_returns_400_number_message() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": "10"})

    assert bridge.stream_calls == []
    assert responses == [({"ok": False, "error": "rate_hz must be a number"}, 400)]


# ---- POST /api/vibration/stream: bridge outcomes (503 / 409) ----

def test_vibration_stream_without_bridge_returns_503() -> None:
    handler, responses = _handler_without_bridge()

    handler._api_vibration_stream({"enabled": True, "rate_hz": 10})

    assert responses == [({"ok": False, "error": "not connected"}, 503)]


def test_vibration_stream_bridge_disconnected_returns_503() -> None:
    bridge = FakeVibrationBridge(result=False, error="not connected")
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 10})

    assert bridge.stream_calls == [(True, 10)]
    assert responses == [({"ok": False, "error": "not connected"}, 503)]


def test_vibration_stream_bridge_denied_returns_409() -> None:
    bridge = FakeVibrationBridge(
        result=False, error="Set message interval failed: DENIED"
    )
    handler, responses = _handler_with_bridge(bridge)

    handler._api_vibration_stream({"enabled": True, "rate_hz": 10})

    assert bridge.stream_calls == [(True, 10)]
    assert responses == [
        ({"ok": False, "error": "Set message interval failed: DENIED"}, 409)
    ]


# ---- POST route dispatch ----

class _Headers:
    """Minimal stand-in for http.client.HTTPMessage exposing ``get``."""

    def __init__(self, body: bytes) -> None:
        self._length = str(len(body))

    def get(self, name: str, default: Any = None) -> Any:
        if name == "Content-Length":
            return self._length
        return default


class _ScriptedReader:
    """``rfile`` stub returning a scripted payload on the first ``read`` call."""

    def __init__(self, body: bytes) -> None:
        self._body = body
        self._read = False

    def read(self, n: int) -> bytes:
        if not self._read:
            self._read = True
            return self._body
        return b"{}"


def test_vibration_stream_route_dispatches_to_helper() -> None:
    bridge = FakeVibrationBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)
    body = b'{"enabled": true, "rate_hz": 25}'
    handler.headers = _Headers(body)  # type: ignore[assignment]
    handler.rfile = _ScriptedReader(body)  # type: ignore[assignment]

    handler._handle_api_post("/api/vibration/stream")

    assert bridge.stream_calls == [(True, 25)]
    assert responses == [({"ok": True, "enabled": True, "rate_hz": 25}, 200)]


# ---- State store VIBRATION fields ----

def test_state_store_snapshot_has_vibration_defaults() -> None:
    store = VehicleStateStore()
    snap = store.get_snapshot()

    assert snap["vibration_x"] == 0.0
    assert snap["vibration_y"] == 0.0
    assert snap["vibration_z"] == 0.0
    assert snap["clipping_0"] == 0
    assert snap["clipping_1"] == 0
    assert snap["clipping_2"] == 0


def test_state_store_update_vibration_flows_into_snapshot() -> None:
    store = VehicleStateStore()
    store.update(
        vibration_x=0.12,
        vibration_y=0.08,
        vibration_z=0.31,
        clipping_0=2,
        clipping_1=0,
        clipping_2=1,
    )

    snap = store.get_snapshot()
    assert snap["vibration_x"] == 0.12
    assert snap["vibration_y"] == 0.08
    assert snap["vibration_z"] == 0.31
    assert snap["clipping_0"] == 2
    assert snap["clipping_1"] == 0
    assert snap["clipping_2"] == 1
