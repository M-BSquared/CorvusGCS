"""Tests for POST /api/mavlink/sethome — the map context menu's "Set home here".

The endpoint's whole job is to refuse anything that is not a real coordinate
before it can reach the vehicle, and to translate the bridge's failure into the
right status (503 for a dead link, 409 for a refusal by the autopilot). Mirrors
the handler fakes in tests/test_server_takeoff.py.
"""
from __future__ import annotations

from typing import Any

import pytest

from corvus.server import CorvusHandler


class FakeBridge:
    def __init__(self, result: bool = True, error: str = "") -> None:
        self.result = result
        self.error = error
        self.calls: list[tuple[float, float]] = []

    def set_home(self, lat: float, lon: float) -> bool:
        self.calls.append((lat, lon))
        return self.result

    def get_last_command_error(self) -> str:
        return self.error


def handler_with_bridge(bridge: FakeBridge | None) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def test_sethome_forwards_the_coordinate_unchanged() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome({"lat": 48.0812345, "lon": 11.6405678})

    assert bridge.calls == [(48.0812345, 11.6405678)], (
        "the endpoint must not round the point the operator clicked"
    )
    assert responses == [({"ok": True}, 200)]


def test_sethome_accepts_integer_coordinates() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome({"lat": 48, "lon": 11})

    assert bridge.calls == [(48.0, 11.0)]
    assert responses[0][1] == 200


@pytest.mark.parametrize("payload", [
    {},
    {"lat": 48.1},
    {"lon": 11.6},
    {"lat": "48.1", "lon": 11.6},
    {"lat": None, "lon": 11.6},
    {"lat": float("nan"), "lon": 11.6},
    {"lat": 48.1, "lon": float("inf")},
    # bool is a subclass of int — True must never fly as latitude 1.0.
    {"lat": True, "lon": 11.6},
    {"lat": 48.1, "lon": False},
])
def test_sethome_rejects_non_numeric_coordinates(payload: dict[str, Any]) -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome(payload)

    assert responses[0][1] == 400
    assert bridge.calls == [], "nothing reached the vehicle"


@pytest.mark.parametrize("lat,lon", [
    (90.5, 11.6),
    (-91.0, 11.6),
    (48.1, 180.5),
    (48.1, -181.0),
])
def test_sethome_rejects_out_of_range_coordinates(lat: float, lon: float) -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome({"lat": lat, "lon": lon})

    assert responses[0][1] == 400
    assert bridge.calls == []


@pytest.mark.parametrize("lat,lon", [(90.0, 180.0), (-90.0, -180.0), (0.0, 0.0)])
def test_sethome_accepts_the_range_boundaries(lat: float, lon: float) -> None:
    bridge = FakeBridge()
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome({"lat": lat, "lon": lon})

    assert responses[0][1] == 200
    assert bridge.calls == [(lat, lon)]


def test_sethome_without_a_bridge_is_not_connected() -> None:
    handler, responses = handler_with_bridge(None)

    handler._api_mavlink_sethome({"lat": 48.1, "lon": 11.6})

    assert responses == [({"error": "not connected"}, 400)]


def test_sethome_reports_a_px4_rejection_as_409() -> None:
    bridge = FakeBridge(False, "Set home failed: DENIED")
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome({"lat": 48.1, "lon": 11.6})

    assert responses == [({"ok": False, "error": "Set home failed: DENIED"}, 409)]


def test_sethome_reports_a_dead_link_as_503() -> None:
    bridge = FakeBridge(False, "Set home failed: DISCONNECTED")
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome({"lat": 48.1, "lon": 11.6})

    assert responses == [({"ok": False, "error": "Set home failed: DISCONNECTED"}, 503)]


def test_sethome_falls_back_to_a_generic_error() -> None:
    """A bridge that failed without setting a message still gets a reason."""
    bridge = FakeBridge(False, "")
    handler, responses = handler_with_bridge(bridge)

    handler._api_mavlink_sethome({"lat": 48.1, "lon": 11.6})

    assert responses == [({"ok": False, "error": "set home failed"}, 409)]
