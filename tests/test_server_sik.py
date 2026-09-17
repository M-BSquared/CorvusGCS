"""The /api/sik/* endpoints — the HTTP layer over SiK radio configuration.

The routes own exactly two things: turning a request body into arguments, and
turning what the service raised into a status code an operator's page can react
to. They own no radio knowledge, so the service is faked here and the
assertions are about the boundary — refusals landing before anything is touched,
a rejected value coming back as 400 rather than 500, and a radio that is simply
not powered coming back as 409 rather than as a server fault.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus import sik_config, sik_service  # noqa: E402
from corvus.server import CorvusHandler  # noqa: E402


class FakeSik:
    """Service stand-in recording the calls the routes make."""

    def __init__(self, result: dict[str, Any] | None = None,
                 raises: Exception | None = None) -> None:
        self.result = result if result is not None else {"local": {"fields": []}}
        self.raises = raises
        self.calls: list[tuple[str, tuple, dict]] = []

    def _record(self, name: str, args: tuple, kwargs: dict) -> dict[str, Any]:
        self.calls.append((name, args, kwargs))
        if self.raises is not None:
            raise self.raises
        return dict(self.result)

    def status(self) -> dict[str, Any]:
        return {
            "ports": [{"device": "/dev/ttyUSB0", "kind": "sik", "is_link": True}],
            "link_device": "/dev/ttyUSB0", "link_baud": 57600,
            "armed": False, "busy": False,
            "can_configure": True, "blocked_reason": "",
            "default_baud": 57600, "bauds": [57600],
        }

    def load(self, *args: Any, **kwargs: Any) -> dict[str, Any]:
        return self._record("load", args, kwargs)

    def save(self, *args: Any, **kwargs: Any) -> dict[str, Any]:
        return self._record("save", args, kwargs)

    def reset(self, *args: Any, **kwargs: Any) -> dict[str, Any]:
        return self._record("reset", args, kwargs)


def _handler(sik: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.sik = sik  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


# ---------------------------------------------------------------------------
# Status
# ---------------------------------------------------------------------------

def test_the_status_carries_the_register_table_so_the_page_can_render_first() -> None:
    handler, responses = _handler(FakeSik())
    handler._api_sik_status()

    payload, status = responses[0]
    assert status == 200
    assert payload["can_configure"] is True
    # The labels, option lists and help text live in one place, not twice.
    names = [r["name"] for r in payload["schema"]["registers"]]
    assert names == [r["name"] for r in sik_config.REGISTERS]


def test_an_unwired_service_still_answers_a_renderable_status() -> None:
    # A 500 here would leave the page unable to say why it cannot do anything.
    handler, responses = _handler(None)
    handler._api_sik_status()

    payload, status = responses[0]
    assert status == 200
    assert payload["can_configure"] is False
    assert payload["blocked_reason"]
    assert payload["schema"]["registers"]


# ---------------------------------------------------------------------------
# Request parsing
# ---------------------------------------------------------------------------

def test_a_load_passes_the_device_baud_and_scope_through() -> None:
    sik = FakeSik()
    handler, responses = _handler(sik)
    handler._api_sik_load({"device": " /dev/ttyUSB0 ", "baud": 57600, "remote": False})

    assert sik.calls == [("load", ("/dev/ttyUSB0", 57600), {"include_remote": False})]
    assert responses[0][1] == 200
    assert responses[0][0]["ok"] is True


def test_an_omitted_baud_is_left_for_the_service_to_choose() -> None:
    sik = FakeSik()
    handler, _ = _handler(sik)
    handler._api_sik_load({"device": "/dev/ttyUSB0"})

    assert sik.calls[0][1] == ("/dev/ttyUSB0", None)


@pytest.mark.parametrize("body", [
    {},                                        # no device at all
    {"device": "   "},                         # blank
    {"device": 7},                             # not a string
])
def test_a_request_without_a_usable_port_is_refused(body) -> None:
    sik = FakeSik()
    handler, responses = _handler(sik)
    handler._api_sik_load(body)

    assert responses[0][1] == 400
    assert sik.calls == []


@pytest.mark.parametrize("baud", ["57600", 0, -1, True, 57.6])
def test_a_baud_that_is_not_a_positive_integer_is_refused(baud) -> None:
    # Substituting a default for a typo would open the port, fail the escape,
    # and report the failure against a number the operator never chose.
    sik = FakeSik()
    handler, responses = _handler(sik)
    handler._api_sik_load({"device": "/dev/ttyUSB0", "baud": baud})

    assert responses[0][1] == 400
    assert sik.calls == []


def test_a_non_boolean_scope_is_refused() -> None:
    sik = FakeSik()
    handler, responses = _handler(sik)
    handler._api_sik_load({"device": "/dev/ttyUSB0", "remote": "yes"})

    assert responses[0][1] == 400
    assert sik.calls == []


# ---------------------------------------------------------------------------
# Saving
# ---------------------------------------------------------------------------

def test_a_save_passes_both_sides_through_untouched() -> None:
    sik = FakeSik({"applied": {}, "warnings": []})
    handler, responses = _handler(sik)
    handler._api_sik_save({
        "device": "/dev/ttyUSB0", "baud": 57600,
        "local": {"NETID": 42}, "remote": {"NETID": 42},
    })

    name, args, kwargs = sik.calls[0]
    assert name == "save" and args == ("/dev/ttyUSB0", 57600)
    assert kwargs == {"local": {"NETID": 42}, "remote": {"NETID": 42}}
    assert responses[0][1] == 200


def test_a_save_with_nothing_in_it_is_refused_rather_than_run() -> None:
    # An empty session still costs the telemetry link a few seconds.
    sik = FakeSik()
    handler, responses = _handler(sik)
    handler._api_sik_save({"device": "/dev/ttyUSB0"})

    assert responses[0][1] == 400
    assert sik.calls == []


@pytest.mark.parametrize("side", ["local", "remote"])
def test_a_side_that_is_not_an_object_is_refused(side) -> None:
    sik = FakeSik()
    handler, responses = _handler(sik)
    handler._api_sik_save({"device": "/dev/ttyUSB0", side: ["NETID", 42]})

    assert responses[0][1] == 400
    assert sik.calls == []


# ---------------------------------------------------------------------------
# Resetting
# ---------------------------------------------------------------------------

def test_a_reset_defaults_to_the_radio_on_the_cable() -> None:
    sik = FakeSik({"target": "local"})
    handler, _ = _handler(sik)
    handler._api_sik_reset({"device": "/dev/ttyUSB0"})

    assert sik.calls[0][2] == {"target": "local"}


def test_a_reset_of_an_unknown_target_is_refused() -> None:
    sik = FakeSik()
    handler, responses = _handler(sik)
    handler._api_sik_reset({"device": "/dev/ttyUSB0", "target": "both"})

    assert responses[0][1] == 400
    assert sik.calls == []


# ---------------------------------------------------------------------------
# How failures come back
# ---------------------------------------------------------------------------

def test_a_value_the_radio_would_not_accept_comes_back_as_a_bad_request() -> None:
    sik = FakeSik(raises=sik_config.SikConfigError("Air data rate does not accept 100"))
    handler, responses = _handler(sik)
    handler._api_sik_save({"device": "/dev/ttyUSB0", "local": {"AIR_SPEED": 100}})

    payload, status = responses[0]
    assert status == 400
    assert payload["ok"] is False
    assert "100" in payload["error"]


def test_a_radio_that_is_not_answering_is_a_conflict_not_a_server_fault() -> None:
    # Every SikError is something the operator can act on — power the radio,
    # close the other program, disarm — so none of them is a 500.
    sik = FakeSik(raises=sik_service.SikError("the remote radio is not answering"))
    handler, responses = _handler(sik)
    handler._api_sik_load({"device": "/dev/ttyUSB0"})

    payload, status = responses[0]
    assert status == 409
    assert payload["error"] == "the remote radio is not answering"


def test_an_armed_refusal_reaches_the_page_with_its_reason() -> None:
    sik = FakeSik(raises=sik_service.SikError(sik_service.ARMED_MESSAGE))
    handler, responses = _handler(sik)
    handler._api_sik_load({"device": "/dev/ttyUSB0"})

    payload, status = responses[0]
    assert status == 409
    assert "armed" in payload["error"]


def test_an_unexpected_failure_is_a_500_and_still_carries_a_message() -> None:
    sik = FakeSik(raises=OSError("device disappeared"))
    handler, responses = _handler(sik)
    handler._api_sik_load({"device": "/dev/ttyUSB0"})

    payload, status = responses[0]
    assert status == 500
    assert "device disappeared" in payload["error"]


@pytest.mark.parametrize("route", ["_api_sik_load", "_api_sik_save", "_api_sik_reset"])
def test_every_action_reports_an_unwired_service_rather_than_crashing(route) -> None:
    handler, responses = _handler(None)
    getattr(handler, route)({"device": "/dev/ttyUSB0", "local": {"NETID": 1}})

    assert responses[0][1] == 503


# ---------------------------------------------------------------------------
# Routing
# ---------------------------------------------------------------------------

def test_the_radio_endpoints_are_registered_and_the_actions_are_all_post() -> None:
    # A load takes the port from the bridge and puts a radio into command mode,
    # which has no business behind a method a browser may retry or prefetch.
    assert CorvusHandler._GET_ROUTES.get("/api/sik/status") == "_api_sik_status"
    for path in ("/api/sik/load", "/api/sik/save", "/api/sik/reset"):
        assert path in CorvusHandler._POST_ROUTES
        assert path not in CorvusHandler._GET_ROUTES
