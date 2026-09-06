"""GET /api/safety — the Setup -> Safety & Sensors read path.

The endpoint has one job beyond serialisation: it must never break the page. A
missing bridge, a bridge that answers nothing, and a bridge that raises all have
to come back as a renderable 200 with ``connected`` telling the truth, because a
page that 500s in the field tells the operator nothing about the limits their
aircraft is actually flying under.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus import safety_config  # noqa: E402
from corvus.server import CorvusHandler  # noqa: E402


class FakeSafetyBridge:
    """Bridge stand-in exposing only what ``_api_safety`` touches."""

    def __init__(self, values: dict[str, float] | None = None,
                 error: str = "", raises: Exception | None = None) -> None:
        self.values = values or {}
        self.error = error
        self.raises = raises
        self.requested: list[list[str]] = []

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.requested.append(list(names))
        if self.raises is not None:
            raise self.raises
        return {n: v for n, v in self.values.items() if n in set(names)}

    def get_last_command_error(self) -> str:
        return self.error


def _handler(bridge: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _safety_values() -> dict[str, float]:
    return {
        "GF_MAX_HOR_DIST": 500.0, "GF_MAX_VER_DIST": 120.0, "GF_ACTION": 2.0,
        "RTL_RETURN_ALT": 60.0, "RTL_DESCEND_ALT": 30.0,
        "NAV_RCL_ACT": 2.0, "COM_LOW_BAT_ACT": 3.0,
        "BAT_LOW_THR": 0.15,
        "SENS_EN_SF1XX": 6.0, "EKF2_RNG_CTRL": 1.0,
        "SENS_EN_PMW3901": 0.0, "EKF2_OF_CTRL": 0.0,
    }


def _sections(payload: dict) -> dict[str, dict]:
    return {s["id"]: s for s in payload["sections"]}


def test_a_connected_vehicle_returns_the_rendered_sections() -> None:
    handler, responses = _handler(FakeSafetyBridge(_safety_values()))
    handler._api_safety()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is True
    assert [s["id"] for s in payload["sections"]] == [
        "limits", "rtl", "failsafe", "battery", "rangefinder", "flow",
    ]
    assert payload["received"] == len(_safety_values())
    assert _sections(payload)["rangefinder"]["toggle"]["enabled"] is True


def test_the_read_asks_for_the_whole_schema_in_one_batch() -> None:
    """One named batch read, not a probe per parameter — the page opens once."""
    bridge = FakeSafetyBridge(_safety_values())
    handler, _responses = _handler(bridge)
    handler._api_safety()

    assert len(bridge.requested) == 1
    assert bridge.requested[0] == safety_config.param_names()


def test_without_a_bridge_the_page_still_renders() -> None:
    handler, responses = _handler(None)
    handler._api_safety()

    payload, status = responses[0]
    assert status == 200
    assert payload == {"connected": False, "sections": [], "received": 0}


def test_a_vehicle_that_answers_nothing_reports_why_at_200() -> None:
    handler, responses = _handler(FakeSafetyBridge({}, error="parameter read timed out"))
    handler._api_safety()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert payload["error"] == "parameter read timed out"
    assert payload["sections"] == []


def test_a_raising_bridge_is_reported_not_propagated() -> None:
    handler, responses = _handler(FakeSafetyBridge(raises=RuntimeError("link died")))
    handler._api_safety()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert "link died" in payload["error"]


def test_a_firmware_missing_parameters_yields_fewer_sections_not_an_error() -> None:
    """AGENTS.md: an absent parameter is one field fewer, never a broken page."""
    values = {"GF_MAX_HOR_DIST": 500.0, "NAV_RCL_ACT": 2.0}
    handler, responses = _handler(FakeSafetyBridge(values))
    handler._api_safety()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is True
    assert set(_sections(payload)) == {"limits", "failsafe"}
