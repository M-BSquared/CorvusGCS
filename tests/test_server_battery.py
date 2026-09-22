"""GET /api/battery, and the two answers to "how much is left".

The endpoint has the same job every other schema read has — never break the
page — plus one of its own: it carries Corvus's *own* estimator settings
alongside the vehicle's parameters, because on ArduPilot there is no cell count
on the vehicle at all and on either stack the choice of whose estimate to fly by
belongs to the operator.

The second half of this file is the telemetry path. What is pinned there is
that the autopilot's figure and Corvus's are published side by side whichever
one the interface is showing, and that the cell count is latched rather than
recomputed per frame.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus import ardupilot_battery, battery_config  # noqa: E402
from corvus.config import CorvusConfig  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.server import CorvusHandler  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402


class FakeBridge:
    """Bridge stand-in exposing only what ``_api_battery`` touches."""

    stack = ""
    vehicle_type_id = 2

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


def _handler(bridge: Any, config: Any = None) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    handler.store = None  # type: ignore[assignment]
    handler.config = config if config is not None else CorvusConfig()  # type: ignore[assignment]
    handler.path = "/api/battery"  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _values() -> dict[str, float]:
    return {
        "BAT1_N_CELLS": 6.0, "BAT1_CAPACITY": 16000.0, "BAT1_V_CHARGED": 4.05,
        "BAT1_V_EMPTY": 3.6, "BAT1_SOURCE": 0.0, "BAT1_V_DIV": 18.1,
        "BAT_LOW_THR": 0.15, "BAT_CRIT_THR": 0.07,
    }


# ---------------------------------------------------------------------------
# The read
# ---------------------------------------------------------------------------

def test_a_connected_vehicle_returns_the_rendered_sections() -> None:
    handler, responses = _handler(FakeBridge(_values()))
    handler._api_battery()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is True
    assert [s["id"] for s in payload["sections"]] == ["pack", "sensing", "levels"]
    assert payload["received"] == len(_values())


def test_the_read_asks_for_the_whole_schema_in_one_batch() -> None:
    """One named batch read, not a probe per parameter — the page opens once."""
    bridge = FakeBridge(_values())
    handler, _responses = _handler(bridge)
    handler._api_battery()

    assert len(bridge.requested) == 1
    assert bridge.requested[0] == battery_config.param_names()


def test_the_diagram_gets_its_numbers_whether_or_not_a_vehicle_answered() -> None:
    """The page draws a battery either way; an empty summary is what makes it
    draw an unknown one rather than throwing."""
    handler, responses = _handler(FakeBridge(_values()))
    handler._api_battery()
    assert responses[0][0]["pack"]["cells"] == 6

    handler, responses = _handler(None)
    handler._api_battery()
    assert responses[0][0]["pack"]["cells"] == 0
    assert responses[0][0]["connected"] is False


def test_the_operators_own_estimator_settings_travel_with_the_page() -> None:
    """They are not on the vehicle: ArduPilot has no cell-count parameter, and
    whose estimate to believe was never the autopilot's call."""
    config = CorvusConfig(battery={"estimate": True, "cells": 6, "chemistry": "liion"})
    handler, responses = _handler(FakeBridge(_values()), config)
    handler._api_battery()

    settings = responses[0][0]["settings"]
    assert settings["estimate"] is True
    assert settings["cells"] == 6
    assert settings["chemistry"] == "liion"
    # Resolved against the chemistry, so the page never has to know the table.
    assert settings["full_cell"] == 4.2
    assert [c["value"] for c in responses[0][0]["chemistries"]] == [
        "lipo", "liion", "lifepo4",
    ]


def test_an_ardupilot_vehicle_is_read_with_the_ardupilot_schema() -> None:
    bridge = FakeBridge({"BATT_MONITOR": 4.0, "BATT_LOW_VOLT": 21.0})
    bridge.stack = "ardupilot"
    handler, responses = _handler(bridge)
    handler._api_battery()

    assert bridge.requested[0] == ardupilot_battery.param_names()
    assert responses[0][0]["stack"] == "ardupilot"


def test_a_bridge_that_raises_still_renders_the_page() -> None:
    handler, responses = _handler(FakeBridge(raises=RuntimeError("link down")))
    handler._api_battery()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert "link down" in payload["error"]
    assert payload["sections"] == []


def test_a_vehicle_that_answers_nothing_says_why() -> None:
    handler, responses = _handler(FakeBridge({}, error="no heartbeat"))
    handler._api_battery()

    payload, _status = responses[0]
    assert payload["connected"] is False
    assert payload["error"] == "no heartbeat"


# ---------------------------------------------------------------------------
# The telemetry path
# ---------------------------------------------------------------------------

def _bridge() -> tuple[MavlinkBridge, VehicleStateStore]:
    store = VehicleStateStore()
    return MavlinkBridge(store), store


def test_both_answers_are_published_whichever_one_is_shown() -> None:
    """An operator deciding which to trust has to be able to watch them
    disagree, so the estimate is computed even while it is switched off."""
    bridge, _store = _bridge()
    fields = bridge._battery_fields(22.2, 10.0, 87)

    assert fields["battery_percent"] == 87          # the autopilot's, by default
    assert fields["battery_percent_fc"] == 87
    assert 0 <= fields["battery_percent_est"] < 30  # 3.7 V a cell is nearly flat
    assert fields["battery_source"] == "autopilot"


def test_turning_the_estimate_on_changes_what_the_interface_shows() -> None:
    bridge, _store = _bridge()
    bridge.set_battery_settings({"estimate": True, "cells": 6})
    fields = bridge._battery_fields(22.2, 0.0, 87)

    assert fields["battery_source"] == "estimate"
    assert fields["battery_percent"] != 87
    assert fields["battery_percent_fc"] == 87       # still there, still honest


def test_an_autopilot_that_reports_nothing_is_not_a_flat_battery() -> None:
    bridge, _store = _bridge()
    fields = bridge._battery_fields(25.2, 0.0, -1)
    assert fields["battery_percent_fc"] == -1


def test_the_cell_count_is_latched_at_the_first_reading_of_a_connection() -> None:
    """Recomputing per frame lets a 6S fall to a 5S somewhere over the field,
    which moves the percentage thirty points while the aircraft is flying."""
    bridge, _store = _bridge()
    bridge.set_battery_settings({"estimate": True})

    assert bridge._battery_fields(25.2, 0.0, -1)["battery_cells"] == 6   # plugged in
    assert bridge._battery_fields(19.8, 0.0, -1)["battery_cells"] == 6   # flown down


def test_a_new_link_may_be_a_new_pack_so_the_latch_is_dropped() -> None:
    bridge, _store = _bridge()
    bridge.set_battery_settings({"estimate": True})
    bridge._battery_fields(25.2, 0.0, -1)
    bridge._forget_battery_cells()

    assert bridge._battery_fields(16.8, 0.0, -1)["battery_cells"] == 4


def test_changing_the_settings_takes_effect_on_the_next_frame() -> None:
    """Not on the next connection: an operator who has just corrected the count
    is watching the number they corrected."""
    bridge, _store = _bridge()
    bridge._battery_fields(19.8, 0.0, -1)
    bridge.set_battery_settings({"estimate": True, "cells": 6})

    assert bridge._battery_fields(19.8, 0.0, -1)["battery_cells"] == 6


class _BatteryStatus:
    """A BATTERY_STATUS as pymavlink hands one over."""

    def __init__(self, **kwargs: Any) -> None:
        self.id = 0
        self.voltages = [4200, 4180, 4190, 65535, 65535, 65535, 65535, 65535,
                         65535, 65535]
        self.voltages_ext = [0, 0, 0, 0]
        self.current_consumed = 1200
        self.temperature = 2500
        self.time_remaining = 480
        self.__dict__.update(kwargs)


def test_the_pack_detail_sys_status_has_no_room_for_is_published() -> None:
    bridge, store = _bridge()
    bridge._handle_battery_status(_BatteryStatus())
    snap = store.get_snapshot()

    assert snap["battery_cell_voltages"] == [4.2, 4.18, 4.19]
    assert snap["battery_consumed_mah"] == 1200.0
    assert snap["battery_temperature"] == 25.0
    assert snap["battery_time_remaining"] == 480


def test_a_pack_that_reports_no_temperature_says_so_rather_than_claiming_zero() -> None:
    """0 C is a real temperature on a winter morning."""
    bridge, store = _bridge()
    bridge._handle_battery_status(_BatteryStatus(temperature=32767))
    assert store.get_snapshot()["battery_temperature"] is None


def test_a_second_monitor_does_not_overwrite_the_first() -> None:
    """Otherwise the top bar reads whichever pack sent last."""
    bridge, store = _bridge()
    bridge._handle_battery_status(_BatteryStatus())
    bridge._handle_battery_status(_BatteryStatus(id=1, voltages=[3000] * 10))

    assert store.get_snapshot()["battery_cell_voltages"] == [4.2, 4.18, 4.19]
