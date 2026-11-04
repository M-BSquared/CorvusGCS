"""The /api/rtk/* endpoints — the HTTP layer over the RTK base station.

The routes own three things and no RTK knowledge: turning a request body into
settings, refusing the two configurations that cannot work *before* they are
stored, and keeping the NTRIP password out of every response. The service is
faked here, so the assertions are about that boundary.

The password is the one with teeth. It leaves the config file by exactly two
doors — this endpoint and ``GET /api/config`` — and the form that edits it is
drawn from a response that must never contain it. That makes "empty means keep
the stored one" a correctness requirement rather than a convenience: without
it, every save through the page would blank the password and break the caster
connection the page exists to configure.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus import rtk  # noqa: E402
from corvus.config import CorvusConfig, to_public_dict  # noqa: E402
from corvus.server import CorvusHandler  # noqa: E402


class FakeRtk:
    """Service stand-in recording what the routes asked it to do."""

    def __init__(self, raises: Exception | None = None) -> None:
        self.applied: list[dict[str, Any]] = []
        self.restarts = 0
        self.raises = raises
        self.settings_now = rtk.defaults()

    def status(self) -> dict[str, Any]:
        # Redacted through the same function the real service uses, so this
        # fake cannot pass a test the real one would fail.
        return {"state": "searching",
                "settings": rtk.public_settings(self.settings_now),
                "survey_target": {"accuracy": self.settings_now["survey_accuracy"],
                                  "duration": self.settings_now["survey_duration"]}}

    def apply_settings(self, resolved: dict[str, Any]) -> dict[str, Any]:
        if self.raises is not None:
            raise self.raises
        self.applied.append(resolved)
        self.settings_now = resolved
        return resolved

    def restart_survey(self) -> None:
        if self.raises is not None:
            raise self.raises
        self.restarts += 1


def _handler(service: Any, config: Any = None, tmp_path: Any = None):
    handler = object.__new__(CorvusHandler)
    handler.rtk = service  # type: ignore[assignment]
    handler.config = config if config is not None else CorvusConfig()
    handler.config_path = str(tmp_path / "config.json") if tmp_path else None
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


# ---------------------------------------------------------------------------
# Status
# ---------------------------------------------------------------------------

def test_the_status_is_always_renderable() -> None:
    handler, responses = _handler(FakeRtk())
    handler._api_rtk_status()
    payload, status = responses[0]
    assert status == 200
    assert payload["state"] == "searching"


def test_an_unwired_service_still_answers_a_renderable_status() -> None:
    """A 500 here would leave the page unable to say why it can do nothing."""
    handler, responses = _handler(None)
    handler._api_rtk_status()
    payload, status = responses[0]
    assert status == 200
    assert payload["enabled"] is False
    assert payload["message"]
    # The page draws its form from this, so the shape has to be complete even
    # when there is no service behind it.
    assert payload["settings"] == rtk.defaults()
    assert payload["survey_target"]["accuracy"] == rtk.DEFAULT_SURVEY_ACCURACY_M


# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

def test_settings_are_stored_bounded_and_applied(tmp_path) -> None:
    service = FakeRtk()
    handler, responses = _handler(service, tmp_path=tmp_path)
    handler._api_rtk_settings({
        "enabled": True, "source": "usb", "mode": "survey",
        "survey_accuracy": 0.5, "survey_duration": 300,
    })

    assert responses[0][1] == 200
    assert responses[0][0]["ok"] is True
    assert service.applied[0]["survey_accuracy"] == 0.5
    assert service.applied[0]["survey_duration"] == 300
    # And persisted, so a restart keeps them.
    assert handler.config.rtk["survey_duration"] == 300


def test_an_out_of_range_value_is_corrected_rather_than_refused(tmp_path) -> None:
    """A survey accuracy of zero never finishes; a negative one is a base that
    declares itself valid immediately and corrects towards a position it never
    established. Neither may reach the receiver."""
    service = FakeRtk()
    handler, responses = _handler(service, tmp_path=tmp_path)
    handler._api_rtk_settings({"survey_accuracy": 0, "survey_duration": 1})

    assert responses[0][1] == 200
    assert service.applied[0]["survey_accuracy"] >= rtk.SURVEY_ACCURACY_MIN_M
    assert service.applied[0]["survey_duration"] >= rtk.SURVEY_DURATION_MIN_S


def test_a_fixed_base_at_null_island_is_refused_before_it_is_stored(tmp_path) -> None:
    """0, 0 is what an unfilled form produces and is a valid coordinate.
    Storing it would report the same failure on every restart afterwards."""
    service = FakeRtk()
    handler, responses = _handler(service, tmp_path=tmp_path)
    handler._api_rtk_settings({"source": "usb", "mode": "fixed",
                               "fixed": {"latitude": 0, "longitude": 0}})

    assert responses[0][1] == 400
    assert "0, 0" in responses[0][0]["error"]
    assert service.applied == []
    assert handler.config.rtk is None, "a configuration that cannot work was stored"


def test_an_ntrip_stream_without_an_address_is_refused(tmp_path) -> None:
    service = FakeRtk()
    handler, responses = _handler(service, tmp_path=tmp_path)
    handler._api_rtk_settings({"source": "ntrip", "ntrip": {"host": "", "mountpoint": ""}})

    assert responses[0][1] == 400
    assert service.applied == []


def test_a_fixed_position_is_only_checked_when_it_would_be_used(tmp_path) -> None:
    """An operator on survey-in has no reason to have filled the mark in."""
    service = FakeRtk()
    handler, responses = _handler(service, tmp_path=tmp_path)
    handler._api_rtk_settings({"source": "usb", "mode": "survey",
                               "fixed": {"latitude": 0, "longitude": 0}})
    assert responses[0][1] == 200


# ---------------------------------------------------------------------------
# The NTRIP password
# ---------------------------------------------------------------------------

def _save(handler, body):
    handler._api_rtk_settings(body)


def test_an_empty_password_keeps_the_stored_one(tmp_path) -> None:
    """The page never receives the real password, so the empty box it posts
    back has to mean "unchanged" — or every save breaks the connection."""
    config = CorvusConfig(rtk=rtk.settings({
        "source": "ntrip",
        "ntrip": {"host": "caster.example", "mountpoint": "MSM4",
                  "username": "me", "password": "hunter2"},
    }))
    service = FakeRtk()
    handler, responses = _handler(service, config=config, tmp_path=tmp_path)
    _save(handler, {"source": "ntrip", "ntrip": {
        "host": "caster.example", "port": 2101, "mountpoint": "MSM4",
        "username": "me", "password": "",
    }})

    assert responses[0][1] == 200
    assert service.applied[0]["ntrip"]["password"] == "hunter2"


def test_a_new_password_replaces_the_stored_one(tmp_path) -> None:
    config = CorvusConfig(rtk=rtk.settings({
        "source": "ntrip",
        "ntrip": {"host": "c", "mountpoint": "M", "password": "old"},
    }))
    service = FakeRtk()
    handler, _responses = _handler(service, config=config, tmp_path=tmp_path)
    _save(handler, {"source": "ntrip", "ntrip": {
        "host": "c", "mountpoint": "M", "password": "new",
    }})

    assert service.applied[0]["ntrip"]["password"] == "new"


def test_no_response_from_this_endpoint_carries_the_password(tmp_path) -> None:
    config = CorvusConfig(rtk=rtk.settings({
        "source": "ntrip",
        "ntrip": {"host": "c", "mountpoint": "M", "password": "hunter2"},
    }))
    service = FakeRtk()
    handler, responses = _handler(service, config=config, tmp_path=tmp_path)
    _save(handler, {"source": "ntrip", "ntrip": {"host": "c", "mountpoint": "M"}})

    assert "hunter2" not in repr(responses)
    # Nor through the other door the config file has.
    assert "hunter2" not in repr(to_public_dict(handler.config))
    assert to_public_dict(handler.config)["rtk"]["ntrip"]["has_password"] is True


# ---------------------------------------------------------------------------
# Restart
# ---------------------------------------------------------------------------

def test_restart_takes_a_payload_like_every_other_post_route() -> None:
    """The dispatcher calls every POST handler with the parsed body.

    A handler that does not accept one is a 500 on a route the audit tests
    otherwise report as present and resolving — which is exactly how this one
    shipped broken the first time.
    """
    service = FakeRtk()
    handler, responses = _handler(service)
    handler._api_rtk_restart({})

    assert responses[0][1] == 200
    assert service.restarts == 1


def test_a_restart_without_a_service_is_a_503_not_a_crash() -> None:
    handler, responses = _handler(None)
    handler._api_rtk_restart({})
    assert responses[0][1] == 503


def test_a_service_that_throws_becomes_a_500_with_a_reason() -> None:
    service = FakeRtk(raises=RuntimeError("the port went away"))
    handler, responses = _handler(service)
    handler._api_rtk_restart({})

    assert responses[0][1] == 500
    assert "the port went away" in responses[0][0]["error"]
