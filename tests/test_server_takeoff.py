from __future__ import annotations

from typing import Any

import pytest

from corvus.mavlink_bridge import TAKEOFF_ALTITUDE_MAX_M
from corvus.server import CorvusHandler, _BoundedSseBuffer, _parse_takeoff_altitude


class FakeBridge:
    def __init__(self, result: bool = True, error: str = "") -> None:
        self.result = result
        self.error = error
        self.altitudes: list[float] = []

    def takeoff(self, altitude: float) -> bool:
        self.altitudes.append(altitude)
        return self.result

    def get_last_command_error(self) -> str:
        return self.error

    def arm(self, arm: bool) -> bool:
        return self.result


def handler_with_bridge(bridge: FakeBridge) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


# Derived, not typed — see the sibling case in tests/test_mavlink_takeoff.py.
@pytest.mark.parametrize(
    "value",
    [True, None, "bad", "nan", float("inf"), 0, TAKEOFF_ALTITUDE_MAX_M + 1],
)
def test_parse_takeoff_altitude_rejects_invalid_values(value: Any) -> None:
    with pytest.raises(ValueError):
        _parse_takeoff_altitude(value)


def test_takeoff_api_returns_validated_agl_altitude() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_takeoff({"altitude": "12.5"})

    assert bridge.altitudes == [12.5]
    assert responses == [({"ok": True, "altitude_agl": 12.5}, 200)]


def test_takeoff_api_reports_px4_rejection() -> None:
    bridge = FakeBridge(False, "Takeoff failed: DENIED")
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_takeoff({"altitude": 10})

    assert responses == [({"ok": False, "error": "Takeoff failed: DENIED"}, 409)]


def test_takeoff_api_reports_disconnected_bridge() -> None:
    bridge = FakeBridge(False, "Takeoff failed: DISCONNECTED")
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_takeoff({"altitude": 10})

    assert responses == [({"ok": False, "error": "Takeoff failed: DISCONNECTED"}, 503)]


def test_console_takeoff_uses_bridge_workflow_and_validates_arguments() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_console_command({"command": "takeoff 15"})
    handler._api_console_command({"command": "takeoff 15 extra"})

    assert bridge.altitudes == [15.0]
    assert responses == [
        ({"ok": True}, 200),
        ({"error": "usage: takeoff [altitude_agl]"}, 400),
    ]


def test_arm_api_rejects_non_boolean_state() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_arm({"arm": "false"})

    assert responses == [({"ok": False, "error": "arm must be boolean"}, 400)]


def test_telemetry_sse_buffer_coalesces_to_latest_state() -> None:
    buffer = _BoundedSseBuffer(1)
    buffer.put_latest({"sequence": 1})
    buffer.put_latest({"sequence": 2})
    buffer.put_latest({"sequence": 3})

    assert buffer.qsize() == 1
    assert buffer.get(timeout=0.01) == {"sequence": 3}


def test_console_sse_buffer_is_bounded_coalesced_and_prioritizes_errors() -> None:
    buffer = _BoundedSseBuffer(3)
    repeated = {"name": "INFO", "text": "same", "level": "info"}
    buffer.put_console(repeated)
    buffer.put_console(repeated)
    buffer.put_console({"name": "INFO", "text": "two", "level": "info"})
    buffer.put_console({"name": "INFO", "text": "three", "level": "info"})
    buffer.put_console({"name": "ACK", "text": "denied", "level": "error"})

    assert buffer.qsize() == 3
    assert buffer.get(timeout=0.01) == {"name": "ACK", "text": "denied", "level": "error"}


def test_console_sse_buffer_does_not_evict_errors_for_low_priority_traffic() -> None:
    buffer = _BoundedSseBuffer(2)
    first = {"name": "ACK", "text": "denied", "level": "error"}
    second = {"name": "STATUS", "text": "critical", "level": "critical"}
    buffer.put_console(first)
    buffer.put_console(second)
    buffer.put_console({"name": "INFO", "text": "noise", "level": "info"})

    assert buffer.qsize() == 2
    assert {buffer.get(timeout=0.01)["text"], buffer.get(timeout=0.01)["text"]} == {
        "denied", "critical"
    }
