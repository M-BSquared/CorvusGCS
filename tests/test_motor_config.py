"""The PX4 motor/actuator schema behind Setup -> Motors.

Two contracts are pinned here.

*Version tolerance.* PX4 v1.16, v1.17 and v1.18 do not carry an identical
parameter set, and a page that insisted on a fixed list would break on the
firmware that happens to lack one. What is absent disappears, what is unknown is
preserved, and nothing is ever silently rewritten.

*The motor-centric inversion.* PX4 stores the output mapping pin-first; the page
presents it motor-first, because that is the question an operator holding an
aircraft actually has. The inversion has to survive gaps, duplicates and
non-motor functions on the pins.
"""
from __future__ import annotations

import pytest

from corvus import motor_config


def _quad(**overrides: float) -> dict[str, float]:
    """A realistic quad-X readout: geometry, rotors, MAIN outputs, protocol."""
    values: dict[str, float] = {
        "SYS_AUTOSTART": 4001.0,
        "CA_AIRFRAME": 0.0,
        "CA_ROTOR_COUNT": 4.0,
        "PWM_MAIN_TIM0": -4.0,
        "PWM_MAIN_MIN": 1000.0,
        "PWM_MAIN_MAX": 2000.0,
    }
    for i, (x, y) in enumerate([(0.15, 0.15), (-0.15, -0.15), (0.15, -0.15), (-0.15, 0.15)]):
        values[f"CA_ROTOR{i}_PX"] = x
        values[f"CA_ROTOR{i}_PY"] = y
        values[f"CA_ROTOR{i}_PZ"] = 0.0
        values[f"CA_ROTOR{i}_KM"] = 0.05 if i < 2 else -0.05
        values[f"CA_ROTOR{i}_CT"] = 6.5
    for pin in range(1, 9):
        values[f"PWM_MAIN_FUNC{pin}"] = float(100 + pin) if pin <= 4 else 0.0
    values.update(overrides)
    return values


def _sections(doc: dict) -> dict[str, dict]:
    return {s["id"]: s for s in doc["sections"]}


def _fields(section: dict) -> dict[str, dict]:
    return {f["param"]: f for f in section["fields"]}


# ---- the happy path ----

def test_a_quad_yields_geometry_motors_outputs_and_protocol() -> None:
    doc = motor_config.build(_quad())

    assert set(_sections(doc)) == {"protocol_main"}
    assert [f["param"] for f in doc["geometry"]] == ["CA_AIRFRAME", "CA_ROTOR_COUNT"]
    assert doc["rotor_count"] == 4
    assert doc["airframe_preset"] == 4001
    assert [m["label"] for m in doc["motors"]] == [
        "Motor 1", "Motor 2", "Motor 3", "Motor 4",
    ]
    assert [b["id"] for b in doc["banks"]] == ["MAIN"]
    assert len(doc["outputs"]) == 8


def test_every_motor_carries_the_position_the_diagram_is_drawn_from() -> None:
    """The airframe picture is only honest if the distances come from the vehicle."""
    motors = motor_config.build(_quad())["motors"]

    assert [(m["x"], m["y"]) for m in motors] == [
        (0.15, 0.15), (-0.15, -0.15), (0.15, -0.15), (-0.15, 0.15),
    ]
    assert all(m["z"] == 0.0 for m in motors)
    assert [m["spin"] for m in motors] == ["CCW", "CCW", "CW", "CW"]


def test_a_motor_with_no_position_parameters_still_has_coordinates() -> None:
    """A diagram cannot draw None; an unpositioned motor sits at the centre."""
    doc = motor_config.build({"CA_ROTOR_COUNT": 1.0, "PWM_MAIN_FUNC1": 101.0})
    motor = doc["motors"][0]
    assert (motor["x"], motor["y"], motor["z"]) == (0.0, 0.0, 0.0)
    assert motor["spin"] is None
    assert motor["fields"] == []


def test_every_field_names_the_parameter_it_writes() -> None:
    """The frontend applies edits by parameter name, so every field must carry one."""
    doc = motor_config.build(_quad())
    entries = [f for s in doc["sections"] for f in s["fields"]] + list(doc["geometry"])
    entries += [f for m in doc["motors"] for f in m["fields"]]
    assert entries
    for field in entries:
        assert field["param"]
        assert field["kind"] in {"enum", "number", "sign"}
        assert "value" in field


def test_the_airframe_class_and_motor_count_ask_for_a_reload() -> None:
    """Both change which fields exist, so a write to either must re-read the page."""
    fields = {f["param"]: f for f in motor_config.build(_quad())["geometry"]}
    assert fields["CA_AIRFRAME"]["reload"] is True
    assert fields["CA_ROTOR_COUNT"]["reload"] is True


def test_the_airframe_and_motor_count_are_returned_beside_the_drawing() -> None:
    """They change the picture, so they belong on the airframe card, not in a
    generic form section further down the page."""
    doc = motor_config.build(_quad())
    assert [f["param"] for f in doc["geometry"]] == ["CA_AIRFRAME", "CA_ROTOR_COUNT"]
    assert all(s["id"] != "geometry" for s in doc["sections"])


def test_protocol_offers_dshot_oneshot_and_pwm_rates() -> None:
    field = _fields(_sections(motor_config.build(_quad()))["protocol_main"])["PWM_MAIN_TIM0"]
    labels = [o["label"] for o in field["options"]]
    assert "DShot600" in labels and "OneShot" in labels and "PWM 400 Hz" in labels


# ---- the pin-first -> motor-first inversion ----

def test_each_motor_reports_the_output_pin_that_drives_it() -> None:
    motors = motor_config.build(_quad())["motors"]
    assert [m["output"]["label"] for m in motors] == [
        "MAIN 1", "MAIN 2", "MAIN 3", "MAIN 4",
    ]
    assert motors[2]["output"] == {
        "bank": "MAIN", "pin": 3, "param": "PWM_MAIN_FUNC3", "label": "MAIN 3",
    }


def test_a_motor_on_no_pin_reports_no_output_rather_than_guessing() -> None:
    doc = motor_config.build(_quad(PWM_MAIN_FUNC2=0.0))
    assert doc["motors"][1]["output"] is None
    assert doc["motors"][0]["output"]["label"] == "MAIN 1"


def test_a_scrambled_assignment_is_reported_as_wired_not_as_ordered() -> None:
    """Motor 1 on pin 4 must read as MAIN 4 — that is the whole point of the page."""
    doc = motor_config.build(_quad(
        PWM_MAIN_FUNC1=104.0, PWM_MAIN_FUNC2=103.0,
        PWM_MAIN_FUNC3=102.0, PWM_MAIN_FUNC4=101.0,
    ))
    assert [m["output"]["label"] for m in doc["motors"]] == [
        "MAIN 4", "MAIN 3", "MAIN 2", "MAIN 1",
    ]


def test_the_output_catalogue_names_what_each_pin_currently_drives() -> None:
    doc = motor_config.build(_quad(PWM_MAIN_FUNC5=201.0))
    by_label = {o["label"]: o for o in doc["outputs"]}
    assert by_label["MAIN 1"]["function"] == "Motor 1"
    assert by_label["MAIN 1"]["motor"] == 1
    assert by_label["MAIN 5"]["function"] == "Servo 1"
    assert by_label["MAIN 5"]["motor"] is None
    assert by_label["MAIN 6"]["function"] == "Disabled"


def test_aux_and_dronecan_banks_appear_only_when_the_board_has_them() -> None:
    assert [b["id"] for b in motor_config.build(_quad())["banks"]] == ["MAIN"]

    doc = motor_config.build(_quad(PWM_AUX_FUNC1=0.0, UAVCAN_EC_FUNC1=0.0))
    assert [b["id"] for b in doc["banks"]] == ["MAIN", "AUX", "CAN"]
    labels = {o["label"] for o in doc["outputs"]}
    assert "AUX 1" in labels and "DroneCAN 1" in labels


def test_a_motor_can_be_wired_to_aux_or_dronecan() -> None:
    doc = motor_config.build(_quad(PWM_MAIN_FUNC2=0.0, PWM_AUX_FUNC3=102.0))
    assert doc["motors"][1]["output"]["label"] == "AUX 3"


def test_function_param_names_the_parameter_for_a_bank_and_pin() -> None:
    assert motor_config.function_param("MAIN", 3) == "PWM_MAIN_FUNC3"
    assert motor_config.function_param("AUX", 1) == "PWM_AUX_FUNC1"
    assert motor_config.function_param("CAN", 8) == "UAVCAN_EC_FUNC8"
    assert motor_config.function_param("AUX", 9) is None, "AUX has 8 pins"
    assert motor_config.function_param("NOPE", 1) is None


# ---- version tolerance: PX4 v1.16 / v1.17 / v1.18 ----

def test_a_parameter_the_firmware_lacks_is_simply_absent() -> None:
    """Never an error: an older firmware just shows one field fewer."""
    values = _quad()
    del values["PWM_MAIN_TIM0"]
    del values["CA_AIRFRAME"]
    doc = motor_config.build(values)

    assert [f["param"] for f in doc["geometry"]] == ["CA_ROTOR_COUNT"]
    assert "PWM_MAIN_TIM0" not in _fields(_sections(doc)["protocol_main"])


def test_an_empty_readout_yields_nothing_to_render_rather_than_a_crash() -> None:
    doc = motor_config.build({})
    assert doc["sections"] == []
    assert doc["geometry"] == []
    assert doc["airframe_family"] == "multirotor", "the drawing that assumes least"
    assert doc["motors"] == []
    assert doc["outputs"] == []
    assert doc["banks"] == []
    assert doc["rotor_count"] == 0
    assert doc["airframe_preset"] is None


def test_legacy_dshot_config_is_offered_when_the_firmware_has_it() -> None:
    doc = motor_config.build(_quad(DSHOT_CONFIG=600.0))
    field = _fields(_sections(doc)["protocol_main"])["DSHOT_CONFIG"]
    assert [o["value"] for o in field["options"]] == [0, 150, 300, 600, 1200]


def test_motor_count_falls_back_to_the_rotors_that_actually_arrived() -> None:
    values = _quad()
    del values["CA_ROTOR_COUNT"]
    doc = motor_config.build(values)
    assert doc["rotor_count"] == 4
    assert len(doc["motors"]) == 4


def test_motor_count_is_capped_at_what_control_allocation_supports() -> None:
    doc = motor_config.build({"CA_ROTOR_COUNT": 99.0})
    assert doc["rotor_count"] == motor_config.MAX_ROTORS
    assert doc["max_motors"] == motor_config.MAX_ROTORS


def test_a_hexa_shows_six_motors_not_a_fixed_four() -> None:
    values = _quad(CA_ROTOR_COUNT=6.0)
    for i in (4, 5):
        values[f"CA_ROTOR{i}_PX"] = 0.0
        values[f"CA_ROTOR{i}_PY"] = 0.2 if i == 4 else -0.2
        values[f"CA_ROTOR{i}_KM"] = 0.05
    values["PWM_MAIN_FUNC5"] = 105.0
    values["PWM_MAIN_FUNC6"] = 106.0

    doc = motor_config.build(values)
    assert len(doc["motors"]) == 6
    assert doc["motors"][5]["output"]["label"] == "MAIN 6"


# ---- never rewrite what we do not understand ----

def test_an_unknown_enum_value_is_preserved_as_its_own_option() -> None:
    """A value this build has never heard of must survive being displayed.

    Snapping it to the first option would silently rewrite a real aircraft's
    airframe the moment the operator touched an unrelated field.
    """
    doc = motor_config.build(_quad(CA_AIRFRAME=77.0))
    field = {f["param"]: f for f in doc["geometry"]}["CA_AIRFRAME"]
    assert {"value": 77, "label": "Unknown (77)"} in field["options"]
    assert field["value"] == 77.0


def test_an_unknown_output_function_is_named_not_mistaken_for_a_motor() -> None:
    doc = motor_config.build(_quad(PWM_MAIN_FUNC5=421.0))
    entry = next(o for o in doc["outputs"] if o["label"] == "MAIN 5")
    assert entry["function"] == "Function 421"
    assert entry["motor"] is None
    assert motor_config.motor_of(421.0) is None


def test_spin_direction_is_matched_on_the_sign_not_the_magnitude() -> None:
    """CA_ROTORn_KM carries direction in its sign and tuning in its magnitude.

    The field is a plain CW/CCW choice with no "Unknown (0)" leaking in from the
    generic enum path, so the magnitude stays the frontend's to preserve.
    """
    motors = motor_config.build(_quad())["motors"]
    ccw = next(f for f in motors[0]["fields"] if f["param"] == "CA_ROTOR0_KM")
    cw = next(f for f in motors[2]["fields"] if f["param"] == "CA_ROTOR2_KM")

    assert ccw["kind"] == "sign" and ccw["value"] == 0.05
    assert cw["kind"] == "sign" and cw["value"] == -0.05
    assert [o["label"] for o in ccw["options"]] == ["CCW", "CW"]


# ---- the read list ----

def test_param_names_covers_every_field_the_schema_can_emit() -> None:
    """A field whose parameter is never requested could never be rendered."""
    names = motor_config.param_names()
    assert len(names) == len(set(names)), "duplicate names in the read list"
    unique = set(names)

    doc = motor_config.build(
        {n: 0.0 for n in unique} | {"CA_ROTOR_COUNT": float(motor_config.MAX_ROTORS)})
    entries = [f for s in doc["sections"] for f in s["fields"]] + list(doc["geometry"])
    entries += [f for m in doc["motors"] for f in m["fields"]]
    for field in entries:
        assert field["param"] in unique, f"{field['param']} is never requested"
    for entry in doc["outputs"]:
        assert entry["param"] in unique


# ---- the drawing: which airframe, and which way each rotor pushes ----

@pytest.mark.parametrize("ca_airframe, family", [
    (0, "multirotor"),
    (1, "wing"),
    (2, "vtol"),
    (3, "vtol"),
    (4, "vtol"),
    (5, "rover"),
    (6, "rover"),
    (7, "multirotor"),
    (8, "multirotor"),
    (10, "helicopter"),
    (12, "helicopter"),
])
def test_the_airframe_class_picks_the_silhouette(ca_airframe: int, family: str) -> None:
    doc = motor_config.build(_quad(CA_AIRFRAME=float(ca_airframe)))
    assert doc["airframe_family"] == family
    assert doc["airframe_label"]


def test_an_unknown_airframe_class_falls_back_to_the_drawing_that_assumes_least() -> None:
    """Better a plain arms-and-ring picture than a wing that is not there."""
    assert motor_config.build(_quad(CA_AIRFRAME=77.0))["airframe_family"] == "multirotor"
    values = _quad()
    del values["CA_AIRFRAME"]
    assert motor_config.build(values)["airframe_family"] == "multirotor"


def test_a_quadplane_reports_its_pusher_apart_from_its_lift_rotors() -> None:
    """The distinction the VTOL drawing needs, and it comes from the vehicle."""
    values = _quad(CA_AIRFRAME=2.0, CA_ROTOR_COUNT=5.0)
    for i in range(4):
        values[f"CA_ROTOR{i}_AX"] = 0.0
        values[f"CA_ROTOR{i}_AY"] = 0.0
        values[f"CA_ROTOR{i}_AZ"] = -1.0
    values["CA_ROTOR4_PX"] = -0.3
    values["CA_ROTOR4_PY"] = 0.0
    values["CA_ROTOR4_AX"] = 1.0
    values["CA_ROTOR4_AY"] = 0.0
    values["CA_ROTOR4_AZ"] = 0.0
    values["PWM_MAIN_FUNC5"] = 105.0

    motors = motor_config.build(values)["motors"]
    assert [m["thrust"] for m in motors] == ["lift"] * 4 + ["horizontal"]
    assert motors[4]["axis"] == {"x": 1.0, "y": 0.0, "z": 0.0, "kind": "horizontal"}


def test_a_rotor_axis_is_normalised_so_the_drawing_can_use_it_directly() -> None:
    values = _quad()
    values["CA_ROTOR0_AX"] = 0.0
    values["CA_ROTOR0_AY"] = 0.0
    values["CA_ROTOR0_AZ"] = -4.0
    axis = motor_config.build(values)["motors"][0]["axis"]
    assert axis == {"x": 0.0, "y": 0.0, "z": -1.0, "kind": "lift"}


def test_a_firmware_without_rotor_axes_reports_none_rather_than_guessing() -> None:
    """No axis means the drawing shows a plain disc — not an invented direction."""
    motors = motor_config.build(_quad())["motors"]
    assert all(m["axis"] is None and m["thrust"] is None for m in motors)


def test_a_zero_length_axis_is_treated_as_no_axis_at_all() -> None:
    values = _quad()
    for a in ("AX", "AY", "AZ"):
        values[f"CA_ROTOR0_{a}"] = 0.0
    assert motor_config.build(values)["motors"][0]["axis"] is None
