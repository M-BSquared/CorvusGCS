from __future__ import annotations

from typing import Any

from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore


class FakeSerialBridge:
    """Stand-in for ``MavlinkBridge`` exposing only the serial-port API."""

    def __init__(
        self,
        ports: list[dict] | None = None,
        error: Exception | None = None,
    ) -> None:
        self._ports = ports if ports is not None else []
        self._error = error

    def list_serial_ports(self) -> list[dict]:
        if self._error is not None:
            raise self._error
        return self._ports


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


def test_serial_ports_endpoint_returns_bridge_ports_with_exact_shape() -> None:
    ports = [
        {
            "device": "/dev/ttyUSB0",
            "description": "Holybro SiK Telemetry Radio V3",
            "hwid": "USB VID:PID=26AC:0011",
        },
        {
            "device": "/dev/ttyUSB1",
            "description": "FTDI FT232R USB UART",
            "hwid": "USB VID:PID=0403:6001",
        },
    ]
    handler, responses = _handler_with_bridge(FakeSerialBridge(ports=ports))

    handler._api_mavlink_serial_ports()

    assert responses == [({"ports": ports}, 200)]


def test_serial_ports_endpoint_returns_empty_list_when_no_bridge() -> None:
    handler, responses = _handler_without_bridge()

    handler._api_mavlink_serial_ports()

    assert responses == [({"ports": []}, 200)]


def test_serial_ports_endpoint_returns_200_with_error_when_bridge_raises() -> None:
    handler, responses = _handler_with_bridge(
        FakeSerialBridge(error=RuntimeError("permission denied: /dev/ttyUSB0"))
    )

    handler._api_mavlink_serial_ports()

    assert len(responses) == 1
    payload, status = responses[0]
    assert status == 200
    assert payload == {"ports": [], "error": "permission denied: /dev/ttyUSB0"}


def test_serial_ports_route_dispatches_to_helper() -> None:
    ports = [
        {
            "device": "/dev/ttyUSB0",
            "description": "Holybro SiK Telemetry Radio V3",
            "hwid": "USB VID:PID=26AC:0011",
        },
    ]
    handler, responses = _handler_with_bridge(FakeSerialBridge(ports=ports))

    handler._handle_api_get("/api/mavlink/serial-ports")

    assert responses == [({"ports": ports}, 200)]


def test_serial_ports_route_without_bridge_dispatches_to_empty_list() -> None:
    handler, responses = _handler_without_bridge()

    handler._handle_api_get("/api/mavlink/serial-ports")

    assert responses == [({"ports": []}, 200)]


def test_state_store_snapshot_has_link_status_defaults() -> None:
    store = VehicleStateStore()
    snap = store.get_snapshot()

    assert snap["link_status"] == "disconnected"
    assert snap["link_connection"] == ""
    assert snap["link_error"] == ""


def test_state_store_set_disconnected_resets_link_status_but_keeps_last_connection_and_error() -> None:
    store = VehicleStateStore()
    store.update(
        link_status="connected",
        link_connection="serial:/dev/ttyUSB0@57600",
        link_error="",
    )

    store.set_disconnected()

    snap = store.get_snapshot()
    assert snap["link_status"] == "disconnected"
    # Preserved so the UI can still show the last connection + last error.
    assert snap["link_connection"] == "serial:/dev/ttyUSB0@57600"
    assert snap["link_error"] == ""


def test_state_store_set_disconnected_preserves_non_empty_link_error() -> None:
    store = VehicleStateStore()
    store.update(
        link_status="connected",
        link_connection="serial:/dev/ttyUSB0@57600",
        link_error="link timeout",
    )

    store.set_disconnected()

    snap = store.get_snapshot()
    assert snap["link_status"] == "disconnected"
    assert snap["link_connection"] == "serial:/dev/ttyUSB0@57600"
    assert snap["link_error"] == "link timeout"
