"""The battery schemas and the estimator behind the Battery & Power page.

Three things are pinned here, and each of them is a bug this page could
otherwise ship without anybody noticing until a pack came down:

* the *curve*, because a linear reading of a LiPo is optimistic exactly where
  optimism costs an airframe;
* the *cell count*, because detecting it per frame rather than latching it lets
  a percentage move thirty points mid-flight; and
* the *candidate rule*, because a schema that assumes a parameter exists renders
  an empty page on the firmware that does not have it.
"""
from __future__ import annotations

import pytest

from corvus import ardupilot_battery, battery, battery_config


def _sections(doc: dict) -> dict[str, dict]:
    return {s["id"]: s for s in doc["sections"]}


def _fields(section: dict) -> dict[str, dict]:
    return {f["param"]: f for f in section["fields"]}


# ---------------------------------------------------------------------------
# The curve
# ---------------------------------------------------------------------------

def test_a_full_pack_reads_full_and_a_flat_one_reads_flat() -> None:
    six = {"cells": 6}
    assert battery.estimate(25.2, 0.0, six)["percent"] == 100.0
    assert battery.estimate(6 * 3.6, 0.0, six)["percent"] == 0.0


def test_each_default_empty_leaves_a_reserve_before_the_knee() -> None:
    """0% is where to land, so the default must sit above the cell's own floor.

    Placed at the curve's bottom it would read 0% on a pack already past the
    knee, with nothing left to fly a go-around on.
    """
    for name, chem in battery.CHEMISTRIES.items():
        at_empty = battery._interpolate(chem["curve"], chem["empty"])
        assert 3.0 <= at_empty <= 10.0, name
    assert battery.CHEMISTRIES["lipo"]["empty"] == 3.6


def test_the_curve_is_not_a_straight_line() -> None:
    """3.7 V a cell is the *nominal* voltage, not half a pack.

    A linear 3.3-4.2 V reading calls it 44%. It is closer to 12%, and the gap
    is most of a return leg.
    """
    nominal = battery.estimate(6 * 3.7, 0.0, {"cells": 6})["percent"]
    assert 5.0 < nominal < 20.0


def test_each_chemistry_is_read_against_its_own_curve() -> None:
    """3.3 V a cell is a flat LiPo and a LiFePO4 that is most of the way full.
    One table for both would have to be wrong about one of them."""
    lipo = battery.estimate(4 * 3.3, 0.0, {"chemistry": "lipo", "cells": 4})["percent"]
    lifepo4 = battery.estimate(4 * 3.3, 0.0,
                               {"chemistry": "lifepo4", "cells": 4})["percent"]
    assert lipo == 0.0
    assert lifepo4 > 70.0


def test_the_operators_own_endpoints_move_zero_and_full() -> None:
    """A pack configured to land at 3.5 V a cell must read 0 there, not 20%."""
    resolved = {"empty_cell": 3.5, "full_cell": 4.2, "cells": 6}
    assert battery.estimate(6 * 3.5, 0.0, resolved)["percent"] == 0.0
    assert battery.estimate(6 * 4.2, 0.0, resolved)["percent"] == 100.0


def test_a_chemistry_nobody_has_heard_of_falls_back_rather_than_raising() -> None:
    """The setting travels in a config file another build may have written."""
    assert battery.chemistry("unobtanium") is battery.CHEMISTRIES["lipo"]


# ---------------------------------------------------------------------------
# Sag
# ---------------------------------------------------------------------------

def test_current_draw_is_corrected_out_before_the_curve_is_consulted() -> None:
    """A 6S pack pulling 60 A through 5 mOhm a cell reads 1.8 V low. Uncorrected
    that is the difference between a climb and a landing decision."""
    loaded = battery.estimate(21.0, 60.0, {"resistance": 5.0, "cells": 6})["percent"]
    uncorrected = battery.estimate(21.0, 60.0, {"resistance": 0.0, "cells": 6})["percent"]
    assert loaded > uncorrected


def test_no_configured_resistance_means_no_correction_at_all() -> None:
    """Conservative is a defensible default; a correction by a number nobody
    measured is not."""
    est = battery.estimate(21.0, 60.0, {"cells": 6})
    assert est["rest_cell_voltage"] == est["cell_voltage"]


# ---------------------------------------------------------------------------
# Cell count
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(("volts", "cells"), [
    (25.2, 6), (22.2, 6), (16.8, 4), (15.2, 4), (12.6, 3), (50.4, 12),
])
def test_a_plugged_in_pack_is_counted_correctly(volts: float, cells: int) -> None:
    assert battery.estimate(volts, 0.0)["cells"] == cells


def test_a_configured_count_is_never_second_guessed() -> None:
    """An operator who says 6S means 6S, however flat the pack is."""
    assert battery.estimate(19.0, 0.0, {"cells": 6})["cells"] == 6


def test_a_flown_pack_is_genuinely_ambiguous_and_reads_as_the_smaller_one() -> None:
    """19.8 V is a fresh 5S and a tired 6S, and no arithmetic separates them.

    This is not a defect to be smoothed over, it is why the bridge latches the
    count from the first reading of a connection — when the pack has just been
    plugged in and the answer is unambiguous — and why the page lets the
    operator pin it outright.
    """
    assert battery.estimate(19.8, 0.0)["cells"] == 5
    assert battery.estimate(19.8, 0.0, {"cells": 6})["cells"] == 6


def test_a_voltage_no_pack_explains_is_declined_rather_than_guessed() -> None:
    """-1, not 0: an unwired monitor and a flat battery are different facts."""
    assert battery.estimate(0.0, 0.0)["percent"] == -1.0
    assert battery.estimate(1.2, 0.0)["percent"] == -1.0


# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

def test_the_estimate_is_off_until_somebody_turns_it_on() -> None:
    """Turning it on changes the number on the top bar. A ground station does
    not do that to an operator who never asked."""
    assert battery.settings(None)["estimate"] is False


def test_a_hand_edited_config_cannot_produce_a_percentage_at_all() -> None:
    coerced = battery.coerce_settings({
        "cells": -3, "resistance": 4000.0, "chemistry": "nope", "estimate": "yes",
    })
    assert coerced == {"cells": 0, "resistance": battery.MAX_RESISTANCE_MOHM}


def test_endpoints_that_cross_fall_back_to_the_chemistry() -> None:
    resolved = battery.settings({"full_cell": 3.0, "empty_cell": 4.0})
    assert resolved["full_cell"] == 4.2
    assert resolved["empty_cell"] == 3.6


# ---------------------------------------------------------------------------
# PX4 schema
# ---------------------------------------------------------------------------

def _px4() -> dict[str, float]:
    return {
        "BAT1_N_CELLS": 6.0, "BAT1_CAPACITY": 16000.0, "BAT1_V_CHARGED": 4.05,
        "BAT1_V_EMPTY": 3.6, "BAT1_V_LOAD_DROP": 0.3, "BAT1_R_INTERNAL": 0.005,
        "BAT1_SOURCE": 0.0, "BAT1_V_DIV": 18.1, "BAT1_A_PER_V": 36.4,
        "BAT_LOW_THR": 0.15, "BAT_CRIT_THR": 0.07, "BAT_EMERGEN_THR": 0.05,
    }


def test_a_current_px4_firmware_yields_the_pack_the_sensing_and_the_levels() -> None:
    doc = battery_config.build(_px4())
    assert set(_sections(doc)) == {"pack", "sensing", "levels"}
    assert _fields(_sections(doc)["pack"])["BAT1_N_CELLS"]["value"] == 6.0
    assert doc["received"] == len(_px4())


def test_a_disabled_battery_source_reads_as_disabled_not_unknown() -> None:
    """-1 is PX4's own value (v1.16 to v1.18) and the default of BAT2_SOURCE."""
    field = _fields(_sections(battery_config.build(_px4() | {"BAT1_SOURCE": -1.0}))["sensing"])
    options = {o["value"]: o["label"] for o in field["BAT1_SOURCE"]["options"]}
    assert options[-1] == "Disabled"
    assert not any("Unknown" in label for label in options.values())
    assert "MAVLink" in options[1], "External is BATTERY_STATUS, not an ADC"
    assert battery_config.build(_px4() | {"BAT1_SOURCE": -1.0})["pack"]["source"] == "Disabled"


def test_the_pre_multi_battery_names_serve_a_firmware_that_still_has_them() -> None:
    """BAT_N_CELLS and BAT1_N_CELLS never coexist, so exactly one set survives."""
    legacy = {"BAT_N_CELLS": 4.0, "BAT_V_CHARGED": 4.05, "BAT_V_EMPTY": 3.5}
    doc = battery_config.build(legacy)
    assert "BAT_N_CELLS" in _fields(_sections(doc)["pack"])
    assert doc["pack"]["cells"] == 4


def test_a_second_pack_appears_only_on_an_aircraft_that_reports_one() -> None:
    assert "pack2" not in _sections(battery_config.build(_px4()))
    doc = battery_config.build({**_px4(), "BAT2_N_CELLS": 6.0})
    assert "pack2" in _sections(doc)


def test_an_empty_readout_yields_no_sections_rather_than_a_crash() -> None:
    doc = battery_config.build({})
    assert doc["sections"] == []
    assert doc["pack"]["cells"] == 0
    assert doc["received"] == 0


def test_reading_every_parameter_at_once_never_raises() -> None:
    doc = battery_config.build({n: 1.0 for n in battery_config.param_names()})
    assert {s["id"] for s in doc["sections"]} == {"pack", "sensing", "levels", "pack2"}


def test_the_px4_thresholds_travel_as_the_fractions_they_are() -> None:
    """BAT_LOW_THR is 0.15 of a pack; the diagram wants 15%, and the write path
    still has to send 0.15."""
    pack = battery_config.build(_px4())["pack"]
    low = next(t for t in pack["thresholds"] if t["id"] == "low")
    assert low["percent"] == 15.0
    assert low["param"] == "BAT_LOW_THR"


# ---------------------------------------------------------------------------
# ArduPilot schema
# ---------------------------------------------------------------------------

def _apm() -> dict[str, float]:
    return {
        "BATT_MONITOR": 4.0, "BATT_CAPACITY": 16000.0, "BATT_LOW_VOLT": 21.0,
        "BATT_CRT_VOLT": 19.8, "BATT_FS_LOW_ACT": 2.0, "BATT_FS_CRT_ACT": 1.0,
        "BATT_VOLT_MULT": 12.02, "BATT_AMP_PERVLT": 60.0,
    }


def test_the_ardupilot_page_carries_its_own_failsafe_actions() -> None:
    """ArduPilot keeps them under BATT_FS_*, so by the prefix rule they belong
    to this page rather than to Safety & Sensors."""
    doc = ardupilot_battery.build(_apm(), vehicle="copter")
    assert "BATT_FS_LOW_ACT" in _fields(_sections(doc)["levels"])


def test_the_ardupilot_thresholds_travel_as_the_volts_they_are() -> None:
    """BATT_LOW_VOLT is 21.0 V where PX4's threshold is a fraction. Converting
    either one would write a value the parameter does not hold."""
    pack = ardupilot_battery.build(_apm())["pack"]
    low = next(t for t in pack["thresholds"] if t["id"] == "low")
    assert low["volts"] == 21.0
    assert "percent" not in low


def test_ardupilot_reports_no_cell_count_because_it_has_none() -> None:
    """Which is exactly why Corvus keeps one of its own."""
    assert ardupilot_battery.build(_apm())["pack"]["cells"] == 0


def test_the_failsafe_action_labels_differ_per_vehicle() -> None:
    copter = ardupilot_battery.build(_apm(), vehicle="copter")
    plane = ardupilot_battery.build(_apm(), vehicle="plane")
    labels = lambda doc: [  # noqa: E731
        o["label"]
        for o in _fields(_sections(doc)["levels"])["BATT_FS_LOW_ACT"]["options"]
    ]
    assert labels(copter) != labels(plane)
    assert "QLand" in labels(plane)


def test_an_unknown_monitor_value_is_preserved_rather_than_rewritten() -> None:
    doc = ardupilot_battery.build({**_apm(), "BATT_MONITOR": 99.0})
    options = _fields(_sections(doc)["pack"])["BATT_MONITOR"]["options"]
    assert {"value": 99, "label": "Unknown (99)"} in options


def test_the_two_stacks_share_no_parameter_name() -> None:
    """The check that proves these are two schemas and not one renamed copy."""
    assert not set(battery_config.param_names()) & set(ardupilot_battery.param_names())
