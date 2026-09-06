"""The PX4 safety/sensor schema behind Setup -> Safety & Sensors.

Two contracts are pinned here. The first is the version tolerance every schema
module in this project owes (AGENTS.md): PX4 v1.16, v1.17 and v1.18 do not carry
an identical parameter set, so what is absent disappears, what is unknown is
preserved, and nothing is ever silently rewritten.

The second is specific to the sensor toggles. Bringing a rangefinder or an
optical-flow sensor up in PX4 is two acts — start the driver, then tell the
estimator to fuse it — and the whole point of the toggle is that one press does
both. These tests hold the write lists to that: the right parameters, in an
order that never leaves two drivers claiming one bus.
"""
from __future__ import annotations

from typing import Any

from corvus import safety_config


def _sections(doc: dict) -> dict[str, dict]:
    return {s["id"]: s for s in doc["sections"]}


def _fields(section: dict) -> dict[str, dict]:
    return {f["param"]: f for f in section["fields"]}


def _writes(entries: list[dict[str, Any]]) -> list[tuple[str, float]]:
    return [(e["param"], e["value"]) for e in entries]


def _safe() -> dict[str, float]:
    """A realistic safety readout on a current firmware."""
    return {
        "GF_ACTION": 2.0, "GF_MAX_HOR_DIST": 500.0, "GF_MAX_VER_DIST": 120.0,
        "GF_SOURCE": 0.0, "GF_PREDICT": 1.0, "LNDMC_ALT_MAX": 122.0,
        "RTL_TYPE": 0.0, "RTL_RETURN_ALT": 60.0, "RTL_DESCEND_ALT": 30.0,
        "RTL_CONE_ANG": 45.0, "RTL_MIN_DIST": 10.0, "RTL_LAND_DELAY": 0.0,
        "NAV_RCL_ACT": 2.0, "COM_RC_LOSS_T": 0.5,
        "NAV_DLL_ACT": 0.0, "COM_DL_LOSS_T": 10.0,
        "COM_LOW_BAT_ACT": 3.0, "COM_POSCTL_NAVL": 0.0,
        "COM_DISARM_LAND": 2.0, "COM_DISARM_PRFLT": 10.0,
        "BAT_LOW_THR": 0.15, "BAT_CRIT_THR": 0.07, "BAT_EMERGEN_THR": 0.05,
    }


def _with_rangefinder(**overrides: float) -> dict[str, float]:
    values = _safe()
    values.update({
        "SENS_EN_SF1XX": 0.0,
        "SENS_TFMINI_CFG": 0.0,
        "EKF2_RNG_CTRL": 0.0,
        "EKF2_HGT_REF": 0.0,
        "EKF2_RNG_A_HMAX": 7.0,
        "EKF2_RNG_POS_Z": 0.0,
        "MPC_ALT_MODE": 0.0,
    })
    values.update(overrides)
    return values


def _with_flow(**overrides: float) -> dict[str, float]:
    values = _safe()
    values.update({
        "SENS_EN_PMW3901": 0.0,
        "EKF2_OF_CTRL": 0.0,
        "SENS_FLOW_ROT": 0.0,
        "EKF2_OF_QMIN": 1.0,
    })
    values.update(overrides)
    return values


# ---- the happy path ----

def test_a_current_firmware_yields_every_safety_section() -> None:
    doc = safety_config.build(_safe())
    assert set(_sections(doc)) == {"limits", "rtl", "failsafe", "battery"}
    assert doc["received"] == len(_safe())


def test_the_limits_the_operator_came_for_are_present_with_units() -> None:
    limits = _fields(_sections(safety_config.build(_safe()))["limits"])
    assert limits["GF_MAX_HOR_DIST"]["value"] == 500.0
    assert limits["GF_MAX_HOR_DIST"]["unit"] == "m"
    assert limits["GF_MAX_VER_DIST"]["value"] == 120.0
    assert limits["GF_ACTION"]["kind"] == "enum"


def test_the_return_height_is_a_field_not_a_guess() -> None:
    rtl = _fields(_sections(safety_config.build(_safe()))["rtl"])
    assert rtl["RTL_RETURN_ALT"]["value"] == 60.0
    assert rtl["RTL_RETURN_ALT"]["unit"] == "m"


def test_every_field_names_the_parameter_it_writes() -> None:
    """The frontend applies edits by parameter name, so every field must carry one."""
    doc = safety_config.build(_with_flow(**_with_rangefinder()))
    for section in doc["sections"]:
        for field in section["fields"]:
            assert field["param"], section["id"]
            assert field["kind"] in {"enum", "number"}
            assert "value" in field
            if field["kind"] == "enum":
                assert field["options"]


def test_rc_and_data_link_loss_share_one_action_set() -> None:
    failsafe = _fields(_sections(safety_config.build(_safe()))["failsafe"])
    rc = [int(o["value"]) for o in failsafe["NAV_RCL_ACT"]["options"]]
    dll = [int(o["value"]) for o in failsafe["NAV_DLL_ACT"]["options"]]
    assert rc == dll == [0, 1, 2, 3, 5, 6]


# ---- version tolerance ----

def test_a_parameter_the_firmware_lacks_is_simply_absent() -> None:
    values = _safe()
    del values["COM_POSCTL_NAVL"]
    del values["RTL_CONE_ANG"]
    doc = safety_config.build(values)
    assert "COM_POSCTL_NAVL" not in _fields(_sections(doc)["failsafe"])
    assert "RTL_CONE_ANG" not in _fields(_sections(doc)["rtl"])
    assert "NAV_RCL_ACT" in _fields(_sections(doc)["failsafe"])


def test_a_section_with_nothing_left_disappears_entirely() -> None:
    values = {k: v for k, v in _safe().items() if not k.startswith("BAT_")}
    assert "battery" not in _sections(safety_config.build(values))


def test_an_empty_readout_yields_no_sections_rather_than_a_crash() -> None:
    doc = safety_config.build({})
    assert doc["sections"] == []
    assert doc["received"] == 0


def test_an_unknown_enum_value_is_preserved_as_its_own_option() -> None:
    """A failsafe action this build has never heard of must survive untouched."""
    doc = safety_config.build(_safe() | {"NAV_RCL_ACT": 9.0})
    field = _fields(_sections(doc)["failsafe"])["NAV_RCL_ACT"]
    assert field["value"] == 9.0
    assert {"value": 9, "label": "Unknown (9)"} in field["options"]


# ---- the rangefinder toggle ----

def test_a_rangefinder_off_offers_its_drivers_and_reports_it_is_off() -> None:
    doc = safety_config.build(_with_rangefinder())
    toggle = _sections(doc)["rangefinder"]["toggle"]
    assert toggle["enabled"] is False
    assert toggle["clear"] == []
    labels = [d["label"] for d in toggle["drivers"]]
    assert "Lightware SF/LW20/c" in labels
    assert "Benewake TFmini / TF02 (serial)" in labels
    assert labels[-1] == "External / MAVLink"


def test_enabling_a_rangefinder_writes_the_driver_and_the_estimator() -> None:
    """The whole point of the toggle: one press does both halves of the chain."""
    doc = safety_config.build(_with_rangefinder())
    toggle = _sections(doc)["rangefinder"]["toggle"]
    assert _writes(toggle["enable"]) == [("EKF2_RNG_CTRL", 1.0)]
    assert _writes(toggle["disable"]) == [("EKF2_RNG_CTRL", 0.0)]


def test_an_enabled_bus_rangefinder_is_recognised_by_its_model_value() -> None:
    doc = safety_config.build(_with_rangefinder(SENS_EN_SF1XX=6.0, EKF2_RNG_CTRL=1.0))
    toggle = _sections(doc)["rangefinder"]["toggle"]
    assert toggle["enabled"] is True
    assert toggle["selected"] == "SENS_EN_SF1XX:6"
    assert "Lightware SF/LW20/c" in toggle["detail"]
    assert "Conditional (range aid)" in toggle["detail"]
    # Only the driver that is actually on has to be zeroed to switch sensors.
    assert toggle["clear"] == ["SENS_EN_SF1XX"]


def test_a_serial_rangefinder_is_enabled_by_naming_its_port() -> None:
    doc = safety_config.build(_with_rangefinder(SENS_TFMINI_CFG=102.0))
    toggle = _sections(doc)["rangefinder"]["toggle"]
    assert toggle["selected"] == "SENS_TFMINI_CFG"
    assert toggle["port"] == 102
    assert toggle["clear"] == ["SENS_TFMINI_CFG"]
    assert {"value": 102, "label": "TELEM 2"} in toggle["ports"]
    serial = next(d for d in toggle["drivers"] if d["id"] == "SENS_TFMINI_CFG")
    assert serial["serial"] is True and serial["value"] is None


def test_fusion_on_without_a_local_driver_reads_as_an_external_sensor() -> None:
    """A vehicle streaming DISTANCE_SENSOR over MAVLink is configured, not broken."""
    doc = safety_config.build(_with_rangefinder(EKF2_RNG_CTRL=2.0))
    toggle = _sections(doc)["rangefinder"]["toggle"]
    assert toggle["enabled"] is True
    assert toggle["selected"] == "external"
    assert "MAVLink" in toggle["detail"]


def test_a_driver_running_with_fusion_off_is_shown_as_the_half_state_it_is() -> None:
    """The classic PX4 trap: perfect readings the estimator ignores."""
    doc = safety_config.build(_with_rangefinder(SENS_EN_SF1XX=4.0, EKF2_RNG_CTRL=0.0))
    toggle = _sections(doc)["rangefinder"]["toggle"]
    assert "Lightware SF11/c" in toggle["detail"]
    assert "fusion: Disabled" in toggle["detail"]


def test_a_pre_v114_firmware_falls_back_to_the_older_range_aid_parameter() -> None:
    values = _with_rangefinder()
    del values["EKF2_RNG_CTRL"]
    values["EKF2_RNG_AID"] = 0.0
    toggle = _sections(safety_config.build(values))["rangefinder"]["toggle"]
    assert _writes(toggle["enable"]) == [("EKF2_RNG_AID", 1.0)]
    assert _writes(toggle["disable"]) == [("EKF2_RNG_AID", 0.0)]


def test_a_board_without_any_rangefinder_support_offers_no_section() -> None:
    assert "rangefinder" not in _sections(safety_config.build(_safe()))


def test_the_rangefinder_settings_stay_editable_beside_the_toggle() -> None:
    fields = _fields(_sections(safety_config.build(_with_rangefinder()))["rangefinder"])
    assert fields["EKF2_RNG_A_HMAX"]["unit"] == "m"
    assert fields["MPC_ALT_MODE"]["kind"] == "enum"
    assert fields["EKF2_HGT_REF"]["kind"] == "enum"


# ---- the optical-flow toggle ----

def test_enabling_optical_flow_writes_the_driver_and_the_estimator() -> None:
    toggle = _sections(safety_config.build(_with_flow()))["flow"]["toggle"]
    assert toggle["enabled"] is False
    assert _writes(toggle["enable"]) == [("EKF2_OF_CTRL", 1.0)]
    assert [d["id"] for d in toggle["drivers"]] == ["SENS_EN_PMW3901:1", "external"]


def test_optical_flow_falls_back_to_the_aiding_mask_bit_on_older_firmware() -> None:
    """Pre-v1.14 has no EKF2_OF_CTRL — flow is bit 1 of the aiding mask."""
    values = _with_flow()
    del values["EKF2_OF_CTRL"]
    values["EKF2_AID_MASK"] = 1.0
    toggle = _sections(safety_config.build(values))["flow"]["toggle"]
    assert _writes(toggle["enable"]) == [("EKF2_AID_MASK", 3.0)]
    # Switching flow off must not clear the GPS bit that was already set.
    assert _writes(toggle["disable"]) == [("EKF2_AID_MASK", 1.0)]


def test_the_aiding_mask_fallback_reports_the_bit_it_reads() -> None:
    values = _with_flow()
    del values["EKF2_OF_CTRL"]
    values["EKF2_AID_MASK"] = 3.0
    toggle = _sections(safety_config.build(values))["flow"]["toggle"]
    assert toggle["enabled"] is True
    assert _writes(toggle["disable"]) == [("EKF2_AID_MASK", 1.0)]


def test_the_mask_fallback_works_even_with_no_local_flow_driver() -> None:
    """An external flow sensor on old firmware is still switchable."""
    values = _safe() | {"EKF2_AID_MASK": 1.0, "SENS_FLOW_ROT": 0.0}
    toggle = _sections(safety_config.build(values))["flow"]["toggle"]
    assert [d["id"] for d in toggle["drivers"]] == ["external"]
    assert _writes(toggle["enable"]) == [("EKF2_AID_MASK", 3.0)]
    assert toggle["reboot"] is False, "nothing local to restart"


def test_a_board_without_flow_support_offers_no_flow_section() -> None:
    assert "flow" not in _sections(safety_config.build(_safe()))


def test_the_flow_section_says_it_needs_a_distance_sensor() -> None:
    """Flow without a height above ground cannot become a velocity, and the
    operator has to be told that before they trust position hold to it."""
    section = _sections(safety_config.build(_with_flow()))["flow"]
    assert "distance sensor" in section["hint"]


# ---- the batched read ----

def test_param_names_covers_every_parameter_the_schema_can_emit() -> None:
    """The page does one batched read; a name missing from it can never render."""
    names = set(safety_config.param_names())
    assert len(names) == len(safety_config.param_names()), "no duplicates in the batch"

    emitted: set[str] = set()
    values = {n: 1.0 for n in names}
    doc = safety_config.build(values)
    for section in doc["sections"]:
        for field in section["fields"]:
            emitted.add(field["param"])
        toggle = section.get("toggle")
        if not toggle:
            continue
        for driver in toggle["drivers"]:
            if driver["param"]:
                emitted.add(driver["param"])
        for write in list(toggle["enable"]) + list(toggle["disable"]):
            emitted.add(write["param"])
        emitted.update(toggle["clear"])
    assert emitted <= names, sorted(emitted - names)


def test_reading_every_parameter_at_once_never_raises() -> None:
    """A vehicle that answers for the whole superset must still build cleanly."""
    doc = safety_config.build({n: 1.0 for n in safety_config.param_names()})
    assert {s["id"] for s in doc["sections"]} == {
        "limits", "rtl", "failsafe", "battery", "rangefinder", "flow",
    }
