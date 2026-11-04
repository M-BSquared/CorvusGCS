"""The ArduPilot rangefinder and optical-flow setup behind Setup -> Safety & Sensors.

Every number pinned here is ArduPilot's own: the RNGFND1_TYPE and FLOW_TYPE
values from AP_RangeFinder.h and AP_OpticalFlow.cpp, and the port, baud, CAN,
address and EKF source settings from the ArduPilot wiki. A table that is off
by one does not error; it writes a LidarLite driver for a TFmini, so the values
are checked one by one rather than trusted.

The other contract is the one the PX4 page follows: a driver is started with
everything it depends on, and the page reports what is still missing instead
of calling a half-configured sensor "on".
"""
from __future__ import annotations

from typing import Any

from corvus import ardupilot_safety as ap


def _base(**overrides: float) -> dict[str, float]:
    """A copter on a Pixhawk-standard board, ArduPilot 4.6, nothing fitted yet."""
    values: dict[str, float] = {name: 0.0 for name in ap.param_names()}
    for name in ("RNGFND1_MIN_CM", "RNGFND1_MAX_CM", "RNGFND1_GNDCLEAR", "GPS_TYPE"):
        del values[name]
    protocols = {1: 2, 2: 2, 3: 5, 4: 5, 5: -1, 6: -1, 7: -1, 8: -1}
    for n, protocol in protocols.items():
        values[f"SERIAL{n}_PROTOCOL"] = float(protocol)
        values[f"SERIAL{n}_BAUD"] = 57.0
    values.update({
        "GPS1_TYPE": 1.0, "RNGFND1_PIN": -1.0, "RNGFND1_ORIENT": 25.0,
        "CAN_D1_PROTOCOL": 1.0, "CAN_D2_PROTOCOL": 1.0,
        "EK3_SRC1_POSXY": 3.0, "EK3_SRC1_VELXY": 3.0, "EK3_SRC1_POSZ": 1.0,
        "EK3_SRC1_VELZ": 3.0, "EK3_SRC1_YAW": 1.0,
        "EK3_SRC2_POSZ": 1.0, "EK3_SRC3_POSZ": 1.0,
    })
    values.update(overrides)
    return values


def _section(values: dict[str, float], sid: str) -> dict[str, Any]:
    doc = ap.build(values, [], "copter")
    return next(s for s in doc["sections"] if s["id"] == sid)


def _toggle(values: dict[str, float], sid: str = "rangefinder") -> dict[str, Any]:
    return _section(values, sid)["toggle"]


def _driver(toggle: dict[str, Any], value: int) -> dict[str, Any]:
    return next(d for d in toggle["drivers"] if d["value"] == value)


def _writes(entries: list[dict[str, Any]]) -> list[tuple[str, float]]:
    return [(e["param"], e["value"]) for e in entries]


# ---- the enums are ArduPilot's ----

def test_the_rangefinder_types_are_ardupilots_own_numbers() -> None:
    """AP_RangeFinder.h. The table used to be shifted by one from 14 upwards."""
    labels = {t["value"]: t["label"] for t in ap.RANGEFINDER_TYPES}
    assert "LightWare" in labels[8] and "serial" in labels[8]
    assert "MAVLink" in labels[10]
    assert "TeraRanger" in labels[14]
    assert "Lidar-Lite v3" in labels[15]
    assert "VL53L0X" in labels[16]
    assert "TF02" in labels[19]
    assert "TFmini" in labels[20] and "serial" in labels[20]
    assert "v3HP" in labels[21]
    assert "Ping" in labels[23]
    assert labels[24] == "DroneCAN"
    assert "I2C" in labels[25]
    assert "TF03" in labels[27]
    assert "MSP" in labels[32]
    assert "TOFSense" in labels[37]


def test_the_flow_types_are_ardupilots_own_numbers() -> None:
    labels = {t["value"]: t["label"] for t in ap.FLOW_TYPES}
    assert "PX4Flow" in labels[1]
    assert "CX-OF" in labels[4]
    assert "MAVLink" in labels[5]
    assert "DroneCAN" in labels[6]
    assert "MSP" in labels[7]
    assert "UPFlow" in labels[8]


# ---- the rangefinder switch ----

def test_the_switch_never_makes_the_rangefinder_the_height_source() -> None:
    """ArduPilot's surface-tracking page: do not set EK3_SRC1_POSZ to Rangefinder."""
    toggle = _toggle(_base())
    written = [w["param"] for w in toggle["enable"]]
    for driver in toggle["drivers"]:
        written += [w["param"] for w in driver.get("extra", [])]
    assert not any(p.endswith("_POSZ") for p in written)


def test_switching_off_moves_a_rangefinder_height_source_back_to_the_barometer() -> None:
    """A set left pointing at a sensor that is gone has no height at all."""
    toggle = _toggle(_base(RNGFND1_TYPE=20.0, EK3_SRC1_POSZ=2.0, EK3_SRC2_POSZ=2.0))
    assert _writes(toggle["disable"]) == [("EK3_SRC1_POSZ", 1.0), ("EK3_SRC2_POSZ", 1.0)]
    assert _toggle(_base(RNGFND1_TYPE=20.0))["disable"] == []


def test_a_serial_rangefinder_carries_its_port_and_baud() -> None:
    toggle = _toggle(_base(SERIAL5_PROTOCOL=9.0))
    tfmini = _driver(toggle, 20)
    assert tfmini["serial"] is True
    assert _writes(tfmini["port_writes"]) == [
        ("SERIAL{port}_PROTOCOL", 9.0), ("SERIAL{port}_BAUD", 115.0)]
    # The first port on Rangefinder is the one the driver takes, so the
    # others on that protocol are freed when a new port is chosen.
    assert [c["port"] for c in tfmini["port_clear"]] == [5]
    assert _driver(toggle, 37)["port_writes"][1]["value"] == 230.0, "TOFSense talks at 230400"


def test_a_serial_rangefinder_without_its_port_is_incomplete() -> None:
    toggle = _toggle(_base(RNGFND1_TYPE=20.0))
    assert toggle["state"] == "partial"
    assert toggle["enabled"] is False
    assert "SERIALn_PROTOCOL = 9" in toggle["problems"][0]


def test_a_serial_rangefinder_at_the_wrong_baud_is_incomplete() -> None:
    toggle = _toggle(_base(RNGFND1_TYPE=20.0, SERIAL5_PROTOCOL=9.0, SERIAL5_BAUD=57.0))
    assert toggle["state"] == "partial"
    assert "115200" in toggle["problems"][0]

    toggle = _toggle(_base(RNGFND1_TYPE=20.0, SERIAL5_PROTOCOL=9.0, SERIAL5_BAUD=115.0))
    assert (toggle["state"], toggle["enabled"], toggle["problems"]) == ("on", True, [])
    assert toggle["port"] == 5, "the picker opens on the port the sensor is on"


def test_the_ports_are_labelled_with_what_they_do_now() -> None:
    ports = {p["value"]: p["label"] for p in _toggle(_base())["ports"]}
    assert ports[2] == "SERIAL2 (MAVLink2)"
    assert ports[3] == "SERIAL3 (GPS)"
    assert ports[5] == "SERIAL5 (not used)"
    assert _toggle(_base())["port"] == 5, "a free port is offered first"


def test_a_second_serial_rangefinder_blocks_the_port_setup() -> None:
    """Serial rangefinders take the Rangefinder ports in order; freeing one
    would move RNGFND2 onto RNGFND1's sensor."""
    toggle = _toggle(_base(RNGFND2_TYPE=20.0))
    assert "RNGFND2" in _driver(toggle, 20)["blocked"]
    assert "blocked" not in _driver(toggle, 7), "an I2C sensor is not affected"


def test_an_i2c_driver_that_needs_an_address_carries_it() -> None:
    toggle = _toggle(_base())
    assert ("RNGFND1_ADDR", 102.0) in _writes(_driver(toggle, 7)["extra"])
    assert ("RNGFND1_ADDR", 41.0) in _writes(_driver(toggle, 16)["extra"])
    assert ("RNGFND1_ADDR", 16.0) in _writes(_driver(toggle, 25)["extra"])

    # Already that type with an address set: the operator's address stays.
    toggle = _toggle(_base(RNGFND1_TYPE=7.0, RNGFND1_ADDR=103.0))
    assert "extra" not in _driver(toggle, 7)
    assert toggle["state"] == "on"

    toggle = _toggle(_base(RNGFND1_TYPE=7.0, RNGFND1_ADDR=0.0))
    assert toggle["state"] == "partial"
    assert "RNGFND1_ADDR is 0" in toggle["problems"][0]


def test_an_analog_rangefinder_without_a_pin_is_incomplete() -> None:
    toggle = _toggle(_base(RNGFND1_TYPE=1.0, RNGFND1_PIN=-1.0))
    assert toggle["state"] == "partial"
    assert "RNGFND1_PIN" in toggle["problems"][0]


def test_a_type_this_build_does_not_list_is_kept_and_never_replaced() -> None:
    toggle = _toggle(_base(RNGFND1_TYPE=45.0))
    assert toggle["selected"] == "RNGFND1_TYPE:45"
    assert _driver(toggle, 45)["label"].startswith("RNGFND1_TYPE = 45")
    assert toggle["state"] == "on"


# ---- DroneCAN ----

def test_dronecan_is_started_on_an_idle_first_port() -> None:
    toggle = _toggle(_base(CAN_P1_DRIVER=0.0, CAN_D1_PROTOCOL=0.0))
    assert _writes(_driver(toggle, 24)["extra"]) == [
        ("CAN_P1_DRIVER", 1.0), ("CAN_D1_PROTOCOL", 1.0)]


def test_a_running_dronecan_port_needs_nothing_more() -> None:
    toggle = _toggle(_base(CAN_P2_DRIVER=2.0, CAN_D2_PROTOCOL=1.0, RNGFND1_TYPE=24.0))
    assert "extra" not in _driver(toggle, 24)
    assert toggle["state"] == "on"
    assert "CAN2" in _driver(toggle, 24)["wiring"], "it says which connector runs DroneCAN"


def test_a_can_port_running_something_else_is_never_rewritten() -> None:
    toggle = _toggle(_base(CAN_P1_DRIVER=1.0, CAN_D1_PROTOCOL=11.0))
    assert "another protocol" in _driver(toggle, 24)["blocked"]


def test_a_dronecan_sensor_with_no_can_running_is_incomplete() -> None:
    toggle = _toggle(_base(RNGFND1_TYPE=24.0))
    assert toggle["state"] == "partial"
    assert "DroneCAN" in toggle["problems"][0]


# ---- optical flow and the EKF source sets ----

def test_without_gps_flow_becomes_the_primary_source() -> None:
    """The wiki's values for flight without GPS."""
    toggle = _toggle(_base(GPS1_TYPE=0.0), "flow")
    assert _writes(toggle["enable"]) == [
        ("EK3_SRC1_POSXY", 0.0), ("EK3_SRC1_VELXY", 5.0), ("EK3_SRC1_VELZ", 0.0)]
    assert "without GPS" in toggle["note"]


def test_with_gps_flow_goes_into_source_set_two() -> None:
    """GPS stays set 1; the wiki's GPS and flow setup switches with RC option 90."""
    toggle = _toggle(_base(), "flow")
    assert _writes(toggle["enable"]) == [("EK3_SRC2_VELXY", 5.0), ("EK3_SRC2_YAW", 1.0)]
    assert "RCx_OPTION = 90" in toggle["note"]


def test_a_source_set_in_use_is_not_overwritten() -> None:
    toggle = _toggle(_base(EK3_SRC2_POSXY=6.0, EK3_SRC2_VELXY=6.0), "flow")
    assert [p for p, _v in _writes(toggle["enable"])] == ["EK3_SRC3_VELXY", "EK3_SRC3_YAW"]

    toggle = _toggle(_base(EK3_SRC2_POSXY=6.0, EK3_SRC2_VELXY=6.0,
                           EK3_SRC3_POSXY=4.0, EK3_SRC3_VELXY=4.0), "flow")
    assert toggle["enable"] == []
    assert "both in use" in toggle["note"]


def test_fuse_all_velocities_is_cleared_as_the_wiki_sets_it() -> None:
    toggle = _toggle(_base(EK3_SRC_OPTIONS=3.0), "flow")
    assert ("EK3_SRC_OPTIONS", 2.0) in _writes(toggle["enable"])


def test_flow_the_estimator_does_not_use_is_incomplete() -> None:
    values = _base(FLOW_TYPE=6.0, CAN_P1_DRIVER=1.0, RNGFND1_TYPE=24.0)
    toggle = _toggle(values, "flow")
    assert toggle["state"] == "partial"
    assert "EK3_SRCn_VELXY = 5" in toggle["problems"][-1]

    values["EK3_SRC2_VELXY"] = 5.0
    toggle = _toggle(values, "flow")
    assert toggle["state"] == "on"
    assert "source set 2" in toggle["detail"]


def test_flow_without_a_distance_sensor_is_incomplete() -> None:
    values = _base(FLOW_TYPE=5.0, EK3_SRC2_VELXY=5.0)
    toggle = _toggle(values, "flow")
    assert toggle["state"] == "partial"
    assert any("no distance sensor" in p for p in toggle["problems"])


def test_switching_flow_off_puts_the_source_sets_back() -> None:
    values = _base(FLOW_TYPE=5.0, EK3_SRC1_POSXY=0.0, EK3_SRC1_VELXY=5.0,
                   EK3_SRC1_VELZ=0.0, EK3_SRC2_VELXY=5.0)
    toggle = _toggle(values, "flow")
    assert _writes(toggle["disable"]) == [
        ("EK3_SRC1_VELXY", 3.0), ("EK3_SRC1_POSXY", 3.0), ("EK3_SRC1_VELZ", 3.0),
        ("EK3_SRC2_VELXY", 0.0)]
    assert toggle["clear"] == ["FLOW_TYPE"]


# ---- presets ----

def _presets(values: dict[str, float], sid: str) -> dict[str, dict[str, Any]]:
    return {p["id"]: p for p in _section(values, sid)["presets"]}


def test_a_benewake_preset_writes_the_wiki_values_in_the_firmwares_unit() -> None:
    """4.6 stores the distances in metres; before that in centimetres."""
    preset = _presets(_base(), "rangefinder")["benewake-tfmini-s"]
    writes = dict(_writes(preset["writes"]))
    assert writes["SERIAL{port}_PROTOCOL"] == 9.0
    assert writes["SERIAL{port}_BAUD"] == 115.0
    assert writes["RNGFND1_TYPE"] == 20.0
    assert (writes["RNGFND1_MIN"], writes["RNGFND1_MAX"], writes["RNGFND1_GNDCLR"]) == (
        0.1, 6.0, 0.1)

    old = _base()
    for name in ("RNGFND1_MIN", "RNGFND1_MAX", "RNGFND1_GNDCLR"):
        del old[name]
    old.update(RNGFND1_MIN_CM=20.0, RNGFND1_MAX_CM=700.0, RNGFND1_GNDCLEAR=10.0)
    writes = dict(_writes(_presets(old, "rangefinder")["benewake-tfmini-s"]["writes"]))
    assert (writes["RNGFND1_MIN_CM"], writes["RNGFND1_MAX_CM"],
            writes["RNGFND1_GNDCLEAR"]) == (10.0, 600.0, 10.0)


def test_the_tf03_uses_its_own_driver() -> None:
    writes = dict(_writes(_presets(_base(), "rangefinder")["benewake-tf03"]["writes"]))
    assert writes["RNGFND1_TYPE"] == 27.0
    assert writes["RNGFND1_MAX"] == 50.0


def test_the_h_flow_sets_up_can_both_sensors_and_the_estimator() -> None:
    preset = _presets(_base(CAN_P1_DRIVER=0.0), "flow")["holybro-h-flow"]
    writes = _writes(preset["writes"])
    assert writes[0] == ("CAN_P1_DRIVER", 1.0)
    assert ("RNGFND1_TYPE", 24.0) in writes
    assert ("FLOW_TYPE", 6.0) in writes
    assert ("RNGFND1_MAX", 30.0) in writes
    assert ("EK3_SRC2_VELXY", 5.0) in writes
    assert not any(p.endswith("_POSZ") and v == 2.0 for p, v in writes), (
        "Holybro's page sets the rangefinder as height source; the wiki says not to")
    assert "CAN1" in preset["wiring"]


def test_the_matek_module_is_set_up_over_msp() -> None:
    """PX4 cannot read it; ArduPilot can."""
    preset = _presets(_base(), "flow")["matek-3901-l0x"]
    assert preset["supported"] is True
    writes = dict(_writes(preset["writes"]))
    assert writes["SERIAL{port}_PROTOCOL"] == 32.0
    assert writes["FLOW_TYPE"] == 7.0
    assert writes["RNGFND1_TYPE"] == 32.0
    assert writes["FLOW_FXSCALER"] == -800.0
    assert preset["clear"] == [], "MSP ports are never freed: one may drive an OSD"


def test_a_preset_whose_can_port_is_busy_is_offered_with_the_reason() -> None:
    preset = _presets(_base(CAN_P1_DRIVER=1.0, CAN_D1_PROTOCOL=11.0), "flow")["holybro-h-flow"]
    assert preset["supported"] is False
    assert "another protocol" in preset["unsupported"]
    assert preset["writes"] == []


def test_every_driver_says_where_it_is_plugged_in() -> None:
    for sid in ("rangefinder", "flow"):
        for driver in _toggle(_base(), sid)["drivers"]:
            assert driver["wiring"], driver["id"]
    serial = _driver(_toggle(_base()), 20)["wiring"]
    assert "SERIAL2 is TELEM2" in serial


def test_the_page_reports_how_many_parameters_it_read() -> None:
    values = _base()
    assert ap.build(values)["received"] == len(values)
