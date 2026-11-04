"""GET /api/remoteid, and the config path the identity is saved through.

The endpoint has the usual job — never break the page — and two of its own.

It has to render *without a vehicle*. Remote ID is filled in on the ground,
often with the aircraft still in its case, and every other setup page is
allowed to come up empty when nothing is connected. This one is not: the
identity is the operator's and is the whole point of the page.

And it has to keep the identity and the vehicle's parameters apart. The serial
number is not a parameter and never becomes one; the ``DID_``/``COM_ARM_ODID``
family is, and is read in one batch like everywhere else. A payload that mixed
them would invite a page that writes a registration number to an autopilot.
"""
from __future__ import annotations

import json
from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus import ardupilot_remote_id, remote_id, remote_id_config  # noqa: E402
from corvus.config import CorvusConfig  # noqa: E402
from corvus.server import CorvusHandler  # noqa: E402


class FakeBridge:
    """Bridge stand-in exposing only what ``_api_remote_id`` touches."""

    stack = ""
    vehicle_type_id = 2

    def __init__(self, values: dict[str, float] | None = None,
                 status: dict[str, Any] | None = None,
                 error: str = "", raises: Exception | None = None) -> None:
        self.values = values or {}
        self.status = status
        self.error = error
        self.raises = raises
        self.requested: list[list[str]] = []
        self.installed: list[Any] = []

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.requested.append(list(names))
        if self.raises is not None:
            raise self.raises
        return {n: v for n, v in self.values.items() if n in set(names)}

    def remote_id_status(self) -> dict[str, Any]:
        if self.status is None:
            raise RuntimeError("no status")
        return self.status

    def set_remote_id(self, raw: Any) -> dict[str, Any]:
        self.installed.append(raw)
        return remote_id.settings(raw)

    def get_last_command_error(self) -> str:
        return self.error


def _handler(bridge: Any, config: Any = None) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    handler.store = None  # type: ignore[assignment]
    handler.config = config if config is not None else CorvusConfig()  # type: ignore[assignment]
    handler.path = "/api/remoteid"  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _identity() -> dict[str, Any]:
    return {
        "enabled": True,
        "basic_id": {"id_type": 1, "ua_type": 2, "uas_id": "ABCD3XYZ"},
        "operator_id": {"operator_id_type": 0, "operator_id": "FIN87astrdge12k8"},
        "self_id": {"description_type": 0, "description": "Survey"},
        "system": {"operator_location_type": 2, "operator_latitude": 48.07,
                   "operator_longitude": 11.64, "classification_type": 1,
                   "category_eu": 1, "class_eu": 2},
    }


# ---------------------------------------------------------------------------
# The read
# ---------------------------------------------------------------------------

def test_the_page_renders_with_no_vehicle_at_all() -> None:
    """The identity is the operator's; it does not need an aircraft to be edited."""
    config = CorvusConfig(remote_id=remote_id.settings(_identity()))
    handler, responses = _handler(None, config)
    handler._api_remote_id()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert payload["identity"]["basic_id"]["uas_id"] == "ABCD3XYZ"
    assert payload["schema"]["id_types"]
    assert payload["findings"] == []


def test_a_connected_px4_vehicle_returns_its_one_parameter() -> None:
    bridge = FakeBridge({"COM_ARM_ODID": 2.0})
    handler, responses = _handler(bridge)
    handler._api_remote_id()

    payload, _status = responses[0]
    assert payload["connected"] is True
    assert [s["id"] for s in payload["sections"]] == ["vehicle"]
    assert payload["sections"][0]["fields"][0]["param"] == "COM_ARM_ODID"
    assert bridge.requested == [remote_id_config.param_names()]


def test_an_ardupilot_vehicle_gets_the_did_family_instead() -> None:
    bridge = FakeBridge({"DID_ENABLE": 1.0, "DID_MAVPORT": -1.0, "DID_OPTIONS": 1.0})
    bridge.stack = "ardupilot"
    handler, responses = _handler(bridge)
    handler._api_remote_id()

    payload, _status = responses[0]
    params = [f["param"] for f in payload["sections"][0]["fields"]]
    assert params == ["DID_ENABLE", "DID_MAVPORT", "DID_OPTIONS"]
    assert bridge.requested == [ardupilot_remote_id.param_names()]


def test_a_parameter_the_firmware_lacks_is_one_field_fewer_not_an_error() -> None:
    bridge = FakeBridge({"DID_ENABLE": 1.0})
    bridge.stack = "ardupilot"
    handler, responses = _handler(bridge)
    handler._api_remote_id()

    payload, status = responses[0]
    assert status == 200
    assert [f["param"] for f in payload["sections"][0]["fields"]] == ["DID_ENABLE"]


def test_a_firmware_without_remote_id_support_drops_the_section_entirely() -> None:
    bridge = FakeBridge({})
    handler, responses = _handler(bridge)
    handler._api_remote_id()

    payload, status = responses[0]
    assert status == 200
    assert payload["sections"] == []
    assert payload["connected"] is False
    # But the identity half is still there, which is the point.
    assert "identity" in payload


def test_a_failed_parameter_read_is_reported_not_raised() -> None:
    handler, responses = _handler(FakeBridge(raises=RuntimeError("link gone")))
    handler._api_remote_id()

    payload, status = responses[0]
    assert status == 200
    assert payload["error"] == "link gone"
    assert payload["identity"]["enabled"] is False


def test_the_live_broadcast_status_rides_along() -> None:
    status = {"enabled": True, "supported": True, "broadcasting": True,
              "last_sent_age": 0.4, "error": "",
              "arm_status": {"ok": False, "status": 1, "error": "no transmitter"}}
    handler, responses = _handler(FakeBridge({"COM_ARM_ODID": 1.0}, status=status))
    handler._api_remote_id()

    payload, _status = responses[0]
    assert payload["status"]["broadcasting"] is True
    assert payload["status"]["arm_status"]["error"] == "no transmitter"


def test_a_bridge_that_cannot_report_its_status_does_not_break_the_page() -> None:
    handler, responses = _handler(FakeBridge({"COM_ARM_ODID": 1.0}, status=None))
    handler._api_remote_id()

    payload, status = responses[0]
    assert status == 200
    assert payload["status"]["supported"] is False


def test_the_airframe_suggestion_is_offered_but_nothing_is_written() -> None:
    """What an aircraft is and what it was registered as are two different facts."""
    config = CorvusConfig(remote_id=remote_id.settings(
        {"basic_id": {"ua_type": 0, "uas_id": "ABCD3XYZ"}}))
    handler, responses = _handler(FakeBridge({"COM_ARM_ODID": 1.0}), config)
    handler._api_remote_id()

    payload, _status = responses[0]
    assert payload["suggested_ua_type"] == 2     # a quadrotor
    assert payload["identity"]["basic_id"]["ua_type"] == 0


def test_the_findings_follow_the_region_the_operator_picked() -> None:
    faa = dict(_identity(), region="faa")
    faa["system"] = {**faa["system"], "operator_location_type": 0}
    config = CorvusConfig(remote_id=remote_id.settings(faa))
    handler, responses = _handler(None, config)
    handler._api_remote_id()

    payload, _status = responses[0]
    fields = {f["field"] for f in payload["findings"]}
    assert "operator_location_type" in fields


def test_no_identity_field_ever_appears_as_a_parameter() -> None:
    """The serial number is not a parameter, and a page that thought it was
    would offer to write a registration number to an autopilot."""
    bridge = FakeBridge({"DID_ENABLE": 1.0, "DID_MAVPORT": 2.0,
                         "DID_CANDRIVER": 0.0, "DID_OPTIONS": 1.0,
                         "DID_BARO_ACC": 0.5})
    bridge.stack = "ardupilot"
    handler, responses = _handler(bridge)
    handler._api_remote_id()

    payload, _status = responses[0]
    params = {f["param"] for s in payload["sections"] for f in s["fields"]}
    assert all(p.startswith("DID_") for p in params)


# ---------------------------------------------------------------------------
# The poll
# ---------------------------------------------------------------------------

def test_the_poll_endpoint_reads_no_parameters_at_all() -> None:
    """This is the whole reason it exists. A parameter read on a reconnecting
    link can sit for tens of seconds behind the bridge's operation lock; polled
    every few seconds, those stack up against the browser's six-connection
    limit and starve the map and every other fetch on the page."""
    status = {"enabled": True, "supported": True, "broadcasting": True,
              "last_sent_age": 0.4, "error": "", "arm_status": None}
    bridge = FakeBridge({"COM_ARM_ODID": 1.0}, status=status)
    handler, responses = _handler(bridge)
    handler.path = "/api/remoteid/status"
    handler._api_remote_id_status()

    payload, http_status = responses[0]
    assert http_status == 200
    assert bridge.requested == []
    assert payload["status"]["broadcasting"] is True


def test_the_poll_carries_the_findings_so_a_save_updates_the_checklist() -> None:
    config = CorvusConfig(remote_id=remote_id.settings(
        {"enabled": True, "basic_id": {"uas_id": ""}}))
    handler, responses = _handler(None, config)
    handler.path = "/api/remoteid/status"
    handler._api_remote_id_status()

    payload, _status = responses[0]
    assert {f["field"] for f in payload["findings"]} >= {"uas_id", "operator_id"}
    # And not the schema: the page already has it, and re-sending every option
    # table three times a minute is the cost this split exists to avoid.
    assert "schema" not in payload
    assert "sections" not in payload


def test_a_bridge_that_throws_on_status_still_answers_the_poll() -> None:
    handler, responses = _handler(FakeBridge(status=None))
    handler.path = "/api/remoteid/status"
    handler._api_remote_id_status()

    payload, http_status = responses[0]
    assert http_status == 200
    assert payload["status"]["broadcasting"] is False


# ---------------------------------------------------------------------------
# The save path
# ---------------------------------------------------------------------------

def _merge(config: CorvusConfig, partial: dict, bridge: Any = None) -> tuple[Any, str | None]:
    handler = object.__new__(CorvusHandler)
    handler.config = config  # type: ignore[assignment]
    handler.mavlink = bridge  # type: ignore[assignment]
    handler._save_live_config = lambda: None  # type: ignore[method-assign]
    handler._refresh_autoconnect_session = lambda _cfg: None  # type: ignore[method-assign]
    return handler._apply_config_partial(partial)


def test_a_patch_naming_one_card_leaves_the_others_alone() -> None:
    """The page saves one field at a time; a per-key replace would erase the rest."""
    config = CorvusConfig(remote_id=remote_id.settings(_identity()))
    _public, error = _merge(config, {"remote_id": {"basic_id": {"uas_id": "WXYZ2AB"}}})

    assert error is None
    assert config.remote_id["basic_id"]["uas_id"] == "WXYZ2AB"
    assert config.remote_id["operator_id"]["operator_id"] == "FIN87astrdge12k8"
    assert config.remote_id["self_id"]["description"] == "Survey"


def test_a_patch_naming_one_field_of_a_card_keeps_the_rest_of_that_card() -> None:
    """One level deeper than every other config key, because the cards are nested."""
    config = CorvusConfig(remote_id=remote_id.settings(_identity()))
    _public, error = _merge(config, {"remote_id": {"basic_id": {"ua_type": 1}}})

    assert error is None
    assert config.remote_id["basic_id"]["ua_type"] == 1
    assert config.remote_id["basic_id"]["uas_id"] == "ABCD3XYZ"


def test_a_saved_identity_reaches_the_bridge_immediately() -> None:
    """An operator correcting a mistyped serial is standing next to an aircraft
    that is still broadcasting the old one."""
    bridge = FakeBridge()
    config = CorvusConfig(remote_id=remote_id.settings(_identity()))
    _public, error = _merge(config, {"remote_id": {"enabled": False}}, bridge)

    assert error is None
    assert bridge.installed and bridge.installed[-1]["enabled"] is False


def test_a_bridge_that_refuses_the_new_identity_does_not_fail_the_save() -> None:
    class Angry(FakeBridge):
        def set_remote_id(self, raw: Any) -> dict[str, Any]:
            raise RuntimeError("nope")

    config = CorvusConfig(remote_id=remote_id.settings(_identity()))
    _public, error = _merge(config, {"remote_id": {"enabled": False}}, Angry())

    assert error is None
    assert config.remote_id["enabled"] is False


def test_a_remote_id_that_is_not_an_object_is_refused_with_a_reason() -> None:
    config = CorvusConfig()
    _public, error = _merge(config, {"remote_id": "ABCD3XYZ"})

    assert error == "remote_id must be an object"


def test_an_over_long_field_is_cut_on_the_way_in_rather_than_on_the_wire() -> None:
    config = CorvusConfig()
    _public, error = _merge(config, {"remote_id": {"self_id": {"description": "Z" * 60}}})

    assert error is None
    assert len(config.remote_id["self_id"]["description"]) == remote_id.DESCRIPTION_MAX


def test_the_identity_survives_a_round_trip_through_the_config_file() -> None:
    from corvus.config import _build_config, _config_to_dict

    config = CorvusConfig(remote_id=remote_id.settings(_identity()))
    restored = _build_config(json.loads(json.dumps(_config_to_dict(config))))

    assert restored.remote_id == config.remote_id
