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
    assert set(_sections(doc)) == {"limits", "rtl", "failsafe"}
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
    values = {k: v for k, v in _safe().items() if not k.startswith("RTL_")}
    assert "rtl" not in _sections(safety_config.build(values))


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
        "limits", "rtl", "failsafe", "rangefinder", "flow",
    }


# ---- hardware presets ----
#
# A preset is a product, not a driver: the operator owns a TFmini-S, not a
# SENS_TFMINI_CFG. What is pinned here is that the catalogue keeps that promise
# — the whole chain in the right order, the datasheet numbers that go with the
# module, and the same version tolerance everything else on this page owes.

def _presets(section: dict) -> dict[str, dict]:
    return {p["id"]: p for p in section["presets"]}


def _full() -> dict[str, float]:
    """A board that answers for the entire superset."""
    return {n: 0.0 for n in safety_config.param_names()}


def test_every_preset_reaches_the_page_it_claims_to_cover() -> None:
    sections = _sections(safety_config.build(_full()))
    for preset in safety_config.SENSOR_PRESETS:
        for kind, sid in (("range", "rangefinder"), ("flow", "flow")):
            offered = kind in preset["provides"]
            assert (preset["id"] in _presets(sections[sid])) is offered, (
                f"{preset['id']} on the {sid} page"
            )


def test_a_module_carrying_both_sensors_is_offered_on_both_pages() -> None:
    """The H-Flow is one board with a flow camera and a rangefinder on it, so it
    configures the whole module from either page rather than half of itself."""
    sections = _sections(safety_config.build(_full()))
    flow = _presets(sections["flow"])["holybro-h-flow"]
    rng = _presets(sections["rangefinder"])["holybro-h-flow"]
    assert _writes(flow["writes"]) == _writes(rng["writes"])
    assert ("EKF2_OF_CTRL", 1.0) in _writes(flow["writes"])
    assert ("EKF2_RNG_CTRL", 1.0) in _writes(flow["writes"])


def test_a_dronecan_preset_starts_the_can_stack_before_it_subscribes() -> None:
    """A subscription is silent while the CAN stack is off, and that is the
    usual reason a CAN sensor never appears."""
    preset = _presets(_sections(safety_config.build(_full()))["rangefinder"])["holybro-h-flow"]
    written = [w["param"] for w in preset["writes"]]
    assert written.index("UAVCAN_ENABLE") < written.index("UAVCAN_SUB_RNG")
    assert written.index("UAVCAN_SUB_RNG") < written.index("EKF2_RNG_CTRL"), (
        "the estimator is told last, as everywhere else on this page"
    )


def test_a_serial_preset_leaves_the_port_for_the_operator_to_name() -> None:
    """Only the operator knows which UART the lidar is soldered to, so the
    driver write is left open rather than guessed."""
    preset = _presets(_sections(safety_config.build(_full()))["rangefinder"])["benewake-tfmini-s"]
    assert preset["serial"] is True
    first = preset["writes"][0]
    assert first["param"] == "SENS_TFMINI_CFG"
    assert first["value"] is None and first["port"] is True
    assert [w["param"] for w in preset["writes"][1:]] == [
        "EKF2_RNG_CTRL", "EKF2_RNG_A_HMAX", "EKF2_RNG_NOISE", "EKF2_RNG_SFE",
    ]


def test_the_benewake_presets_differ_in_their_datasheet_numbers() -> None:
    """They share one driver — the point of separate presets is the numbers: how
    far the unit still returns, and the noise its accuracy implies."""
    presets = _presets(_sections(safety_config.build(_full()))["rangefinder"])
    noise = {
        pid: dict(_writes(presets[pid]["writes"]))["EKF2_RNG_NOISE"]
        for pid in ("benewake-tfmini-s", "benewake-tfmini-plus", "benewake-tf03")
    }
    assert noise["benewake-tfmini-plus"] < noise["benewake-tfmini-s"] < noise["benewake-tf03"]
    hmax = dict(_writes(presets["benewake-tf03"]["writes"]))["EKF2_RNG_A_HMAX"]
    assert hmax == 10.0, "the range aid stays near the ground whatever the 180 m reach"


def test_a_module_px4_cannot_read_is_listed_but_writes_nothing() -> None:
    """Hiding it would leave an operator who owns one concluding their wiring is
    wrong. It is offered with the reason, and with nothing to apply."""
    preset = _presets(_sections(safety_config.build(_full()))["flow"])["matek-3901-l0x"]
    assert preset["supported"] is False
    assert preset["writes"] == []
    assert "MSP" in preset["unsupported"]
    assert preset["reboot"] is False


def test_a_parameter_the_firmware_lacks_is_skipped_and_reported() -> None:
    """Same tolerance as every field on this page — except a preset that quietly
    wrote eleven of its thirteen parameters would be the half-configured sensor
    this page exists to prevent, so what fell out is named."""
    values = _full()
    del values["EKF2_RNG_QLTY_T"]
    del values["UAVCAN_RNG_MAX"]
    preset = _presets(_sections(safety_config.build(values))["rangefinder"])["holybro-h-flow"]
    assert preset["missing"] == ["UAVCAN_RNG_MAX", "EKF2_RNG_QLTY_T"]
    assert "EKF2_RNG_QLTY_T" not in [w["param"] for w in preset["writes"]]
    assert preset["supported"] is True, "the rest of the module still comes up"


def test_a_firmware_without_the_driver_parameter_cannot_offer_the_preset() -> None:
    values = _full()
    del values["UAVCAN_SUB_RNG"]
    preset = _presets(_sections(safety_config.build(values))["rangefinder"])["holybro-h-flow"]
    assert preset["supported"] is False
    assert "UAVCAN_SUB_RNG" in preset["unsupported"]
    assert preset["writes"] == []


def test_a_preset_reports_its_driver_running_without_claiming_the_model() -> None:
    """SENS_TFMINI_CFG says a Benewake lidar is configured and cannot say which
    one, so all three report it and none of them claims to be the fitted part."""
    values = _full()
    values["SENS_TFMINI_CFG"] = 102.0
    presets = _presets(_sections(safety_config.build(values))["rangefinder"])
    assert all(presets[pid]["active"] for pid in
               ("benewake-tfmini-s", "benewake-tfmini-plus", "benewake-tf03"))
    assert presets["holybro-h-flow"]["active"] is False

    values["SENS_TFMINI_CFG"] = 0.0
    values["UAVCAN_SUB_RNG"] = 1.0
    presets = _presets(_sections(safety_config.build(values))["rangefinder"])
    assert presets["holybro-h-flow"]["active"] is True
    assert not any(presets[pid]["active"] for pid in
                   ("benewake-tfmini-s", "benewake-tf03"))


def test_every_preset_parameter_is_in_the_batched_read() -> None:
    """A preset parameter missing from the batch would look absent on every
    firmware, so the preset would silently shrink on all of them."""
    names = set(safety_config.param_names())
    for preset in safety_config.SENSOR_PRESETS:
        for param, _value, _why in preset.get("params", []):
            assert param in names, f"{preset['id']} writes {param}, which is never read"


def test_a_can_driver_carries_the_stack_write_only_when_it_is_needed() -> None:
    """Dropping a board running DroneCAN ESCs from 3 to 2 to bring a lidar up
    would stop the motors answering."""
    values = _full()
    toggle = _sections(safety_config.build(values))["rangefinder"]["toggle"]
    can = [d for d in toggle["drivers"] if d["id"] == "UAVCAN_SUB_RNG:1"][0]
    assert _writes(can["extra"]) == [("UAVCAN_ENABLE", 2.0)]

    values["UAVCAN_ENABLE"] = 3.0
    toggle = _sections(safety_config.build(values))["rangefinder"]["toggle"]
    can = [d for d in toggle["drivers"] if d["id"] == "UAVCAN_SUB_RNG:1"][0]
    assert "extra" not in can, "a board already past the threshold is left alone"


def test_each_sensor_section_is_tagged_for_its_own_page() -> None:
    """The frontend collects these into the Sensors card and gives each one a
    page; a section that lost the tag would fall back into the envelope form."""
    sections = _sections(safety_config.build(_full()))
    for sid in ("rangefinder", "flow"):
        assert sections[sid]["group"] == "sensors"
        assert sections[sid]["icon"]
        assert sections[sid]["short"]
    for sid in ("limits", "rtl", "failsafe"):
        assert "group" not in sections[sid]


# ---- parameters the operator adds by name ----
#
# These names come from the browser, which makes them untrusted input reaching
# the parameter bridge. What is pinned here is the bound on that: shape, count,
# and that nothing the page already draws can be duplicated by one.

def test_an_added_name_is_read_and_comes_back_as_a_field() -> None:
    doc = safety_config.build({"MPC_XY_P": 0.95}, ["MPC_XY_P"])
    assert doc["extra"] == [{
        "param": "MPC_XY_P", "label": "MPC_XY_P", "kind": "number",
        "present": True, "value": 0.95,
    }]


def test_a_name_the_vehicle_never_answered_for_still_comes_back_flagged() -> None:
    """Dropping it would leave the operator unable to tell a typo from a
    parameter their PX4 version does not have."""
    doc = safety_config.build({}, ["NOT_A_PARAM"])
    assert [f["param"] for f in doc["extra"]] == ["NOT_A_PARAM"]
    assert doc["extra"][0]["present"] is False


def test_a_name_that_is_not_shaped_like_a_parameter_never_reaches_the_bridge() -> None:
    assert safety_config.normalise_extra([
        "lower_case_ok", "has space", "has-dash", "1LEADING", "", "TOO_LONG_A_NAME_X",
        None, 7,
    ]) == ["LOWER_CASE_OK"], "case is normalised; everything else is dropped"


def test_the_added_list_is_deduplicated_and_capped() -> None:
    """An unbounded list here is an unbounded burst of PARAM_REQUEST_READ at an
    aircraft that is very likely flying."""
    assert safety_config.normalise_extra(["A_B", "a_b", " A_B "]) == ["A_B"]
    many = [f"P{i:03d}" for i in range(safety_config.EXTRA_PARAM_LIMIT * 3)]
    assert len(safety_config.normalise_extra(many)) == safety_config.EXTRA_PARAM_LIMIT


def test_a_parameter_the_page_already_draws_is_not_offered_twice() -> None:
    """Two controls over one number can disagree until the next read, and the
    one the operator did not touch is then lying about the aircraft."""
    values = _full()
    doc = safety_config.build(values, ["EKF2_RNG_A_HMAX", "MPC_XY_P"])
    assert [f["param"] for f in doc["extra"]] == ["MPC_XY_P"]


def test_a_parameter_read_but_never_drawn_can_still_be_added() -> None:
    """param_names() is the batch this page reads, which is far wider than the
    set it draws — EKF2_RNG_QLTY_T is read for a preset and shown nowhere.
    Refusing it would block the exact case this feature exists for."""
    values = _full()
    assert "EKF2_RNG_QLTY_T" in safety_config.param_names()
    doc = safety_config.build(values, ["EKF2_RNG_QLTY_T"])
    assert [f["param"] for f in doc["extra"]] == ["EKF2_RNG_QLTY_T"]


def test_no_added_names_is_an_empty_list_not_a_missing_key() -> None:
    for extra in (None, []):
        assert safety_config.build(_safe(), extra)["extra"] == []
