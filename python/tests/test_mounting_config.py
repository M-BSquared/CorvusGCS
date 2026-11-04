"""How the flight controller is mounted, and how far apart the motors are.

Three schemas meet here, because they answer one physical question between
them: where things sit on the airframe.

* :mod:`corvus.mounting_config` / :mod:`corvus.ardupilot_mounting`: the board
  rotation every calibration is measured through, and the lever arms of the
  flight controller and the GPS antenna. Both stacks renamed the GPS antenna
  inside the supported range (PX4 v1.18, ArduPilot 4.6), which is the version
  tolerance worth pinning.
* :func:`corvus.motor_config.frame`: the frame size the Motors page rescales
  the lift rotors by.
"""
from __future__ import annotations

from typing import Any

import pytest

from corvus import ardupilot_motors, ardupilot_mounting, motor_config, mounting_config


def _params(fields: list[dict[str, Any]]) -> list[str]:
    return [f["param"] for f in fields]


def _sensor(doc: dict[str, Any], sensor_id: str) -> dict[str, Any] | None:
    return next((s for s in doc["positions"] if s["id"] == sensor_id), None)


# ---------------------------------------------------------------------------
# PX4
# ---------------------------------------------------------------------------

def _px4(**extra: float) -> dict[str, float]:
    values = {
        "SENS_BOARD_ROT": 0.0, "SENS_BOARD_X_OFF": 0.0,
        "SENS_BOARD_Y_OFF": 0.0, "SENS_BOARD_Z_OFF": 0.0,
        "EKF2_IMU_POS_X": 0.02, "EKF2_IMU_POS_Y": 0.0, "EKF2_IMU_POS_Z": 0.0,
    }
    values.update(extra)
    return values


def test_px4_v116_places_the_gps_with_the_ekf2_parameters() -> None:
    doc = mounting_config.build(_px4(
        EKF2_GPS_POS_X=-0.1, EKF2_GPS_POS_Y=0.0, EKF2_GPS_POS_Z=-0.12))
    gps = _sensor(doc, "gps1")
    assert gps is not None
    assert _params(gps["fields"]) == ["EKF2_GPS_POS_X", "EKF2_GPS_POS_Y", "EKF2_GPS_POS_Z"]
    assert (gps["x"], gps["z"]) == (-0.1, -0.12)
    assert _sensor(doc, "gps2") is None


def test_px4_v118_places_the_gps_with_the_per_receiver_parameters() -> None:
    """v1.18 moved the antenna offset to SENS_GPS0_OFF*. A vehicle that still
    answers for the old name as well is read from the one v1.18 uses."""
    doc = mounting_config.build(_px4(
        EKF2_GPS_POS_X=0.5,
        SENS_GPS0_OFFX=-0.1, SENS_GPS0_OFFY=0.0, SENS_GPS0_OFFZ=-0.12,
        SENS_GPS1_OFFX=0.0, SENS_GPS1_OFFY=0.0, SENS_GPS1_OFFZ=0.0,
        SENS_GPS1_ID=0.0, GPS_2_CONFIG=0.0))
    gps = _sensor(doc, "gps1")
    assert gps is not None
    assert _params(gps["fields"]) == ["SENS_GPS0_OFFX", "SENS_GPS0_OFFY", "SENS_GPS0_OFFZ"]
    assert gps["x"] == -0.1
    assert _sensor(doc, "gps2") is None, "no second receiver is configured"


@pytest.mark.parametrize("extra", [
    {"SENS_GPS1_ID": 123456.0},
    {"GPS_2_CONFIG": 202.0},
    {"SENS_GPS1_OFFY": 0.05},
])
def test_px4_v118_shows_the_second_antenna_once_there_is_one(extra: dict[str, float]) -> None:
    values = _px4(SENS_GPS0_OFFX=0.0, SENS_GPS0_OFFY=0.0, SENS_GPS0_OFFZ=0.0,
                  SENS_GPS1_OFFX=0.0, SENS_GPS1_OFFY=0.0, SENS_GPS1_OFFZ=0.0)
    values.update(extra)
    gps2 = _sensor(mounting_config.build(values), "gps2")
    assert gps2 is not None
    assert _params(gps2["fields"]) == ["SENS_GPS1_OFFX", "SENS_GPS1_OFFY", "SENS_GPS1_OFFZ"]


def test_px4_flight_controller_position_is_the_imu_lever_arm() -> None:
    fc = _sensor(mounting_config.build(_px4()), "fc")
    assert fc is not None
    assert fc["kind"] == "fc" and fc["tag"] == "FC"
    assert _params(fc["fields"]) == ["EKF2_IMU_POS_X", "EKF2_IMU_POS_Y", "EKF2_IMU_POS_Z"]
    assert [f["label"] for f in fc["fields"]] == ["Forward", "Right", "Down"]
    assert all(f["unit"] == "m" for f in fc["fields"])


def test_px4_orientation_is_the_board_rotation_and_its_trims() -> None:
    orientation = mounting_config.build(_px4())["orientation"]
    assert orientation is not None
    assert _params(orientation["fields"]) == [
        "SENS_BOARD_ROT", "SENS_BOARD_X_OFF", "SENS_BOARD_Y_OFF", "SENS_BOARD_Z_OFF"]
    rotation = orientation["fields"][0]
    assert rotation["kind"] == "enum" and rotation["reboot"] is True
    assert "before calibrating" in orientation["hint"]


def test_an_unknown_rotation_is_kept_rather_than_snapped_to_none() -> None:
    rotation = mounting_config.build(_px4(SENS_BOARD_ROT=41.0))["orientation"]["fields"][0]
    assert rotation["value"] == 41.0
    assert {"value": 41, "label": "Unknown (41)"} in rotation["options"]


def test_nothing_answered_is_nothing_shown() -> None:
    doc = mounting_config.build({})
    assert doc["orientation"] is None
    assert doc["positions"] == []


def test_the_rotation_table_is_mavlinks_numbering() -> None:
    """Both stacks number the board rotation as MAV_SENSOR_ORIENTATION, which
    is why one table serves both. A label moved by one would turn an aircraft
    the wrong way before its calibration."""
    mavutil = pytest.importorskip("pymavlink.mavutil")
    names = mavutil.mavlink.enums["MAV_SENSOR_ORIENTATION"]
    for option in mounting_config.ROTATION_OPTIONS:
        entry = names[option["value"]].name.replace("MAV_SENSOR_ROTATION_", "")
        expected = "NONE" if option["value"] == 0 else entry
        spoken = "NONE" if option["value"] == 0 else "_".join(
            part.upper().replace("°", "")
            for part in option["label"].replace(",", "").split())
        assert spoken == expected, (option, entry)


# ---------------------------------------------------------------------------
# ArduPilot
# ---------------------------------------------------------------------------

def _ardupilot(**extra: float) -> dict[str, float]:
    values = {"AHRS_ORIENTATION": 0.0, "AHRS_TRIM_X": 0.0, "AHRS_TRIM_Y": 0.0}
    for imu in (1, 2, 3):
        for axis in ("X", "Y", "Z"):
            values[f"INS_POS{imu}_{axis}"] = 0.0
    values.update(extra)
    return values


def test_ardupilot_43_to_45_places_the_gps_with_gps_pos1() -> None:
    doc = ardupilot_mounting.build(_ardupilot(
        GPS_POS1_X=-0.1, GPS_POS1_Y=0.0, GPS_POS1_Z=-0.12, GPS_TYPE2=0.0))
    gps = _sensor(doc, "gps1")
    assert gps is not None
    assert _params(gps["fields"]) == ["GPS_POS1_X", "GPS_POS1_Y", "GPS_POS1_Z"]
    assert _sensor(doc, "gps2") is None


def test_ardupilot_46_places_the_gps_with_gps1_pos() -> None:
    doc = ardupilot_mounting.build(_ardupilot(
        GPS1_POS_X=-0.1, GPS1_POS_Y=0.0, GPS1_POS_Z=-0.12,
        GPS2_TYPE=1.0, GPS2_POS_X=0.2, GPS2_POS_Y=0.0, GPS2_POS_Z=0.0))
    gps = _sensor(doc, "gps1")
    assert gps is not None
    assert _params(gps["fields"]) == ["GPS1_POS_X", "GPS1_POS_Y", "GPS1_POS_Z"]
    gps2 = _sensor(doc, "gps2")
    assert gps2 is not None and gps2["x"] == 0.2 and gps2["tag"] == "GPS 2"


def test_ardupilot_writes_every_imu_that_shares_the_first_ones_position() -> None:
    """Three IMUs on one board sit within millimetres: one field moves them
    all. An IMU that was given its own position is left alone."""
    fc = _sensor(ardupilot_mounting.build(_ardupilot(INS_POS3_X=0.3)), "fc")
    assert fc is not None
    by_param = {f["param"]: f for f in fc["fields"]}
    assert list(by_param) == ["INS_POS1_X", "INS_POS1_Y", "INS_POS1_Z"]
    assert by_param["INS_POS1_X"]["also"] == ["INS_POS2_X"]
    assert by_param["INS_POS1_Y"]["also"] == ["INS_POS2_Y", "INS_POS3_Y"]


def test_ardupilot_with_one_imu_carries_nothing_along() -> None:
    values = {"INS_POS1_X": 0.0, "INS_POS1_Y": 0.0, "INS_POS1_Z": 0.0}
    fc = _sensor(ardupilot_mounting.build(values), "fc")
    assert fc is not None
    assert all("also" not in f for f in fc["fields"])


def test_ardupilot_orientation_is_ahrs_orientation_and_the_level_trims() -> None:
    orientation = ardupilot_mounting.build(_ardupilot())["orientation"]
    assert orientation is not None
    assert _params(orientation["fields"]) == ["AHRS_ORIENTATION", "AHRS_TRIM_X", "AHRS_TRIM_Y"]
    values = [o["value"] for o in orientation["fields"][0]["options"]]
    assert 42 in values and 101 in values and 41 not in values


def test_both_mounting_schemas_have_the_same_shape() -> None:
    px4 = mounting_config.build(_px4(EKF2_GPS_POS_X=0.0, EKF2_GPS_POS_Y=0.0,
                                     EKF2_GPS_POS_Z=0.0))
    apm = ardupilot_mounting.build(_ardupilot(GPS_POS1_X=0.0, GPS_POS1_Y=0.0,
                                              GPS_POS1_Z=0.0))
    assert set(px4) == set(apm)
    for a, b in zip(px4["positions"], apm["positions"]):
        assert set(a) - {"hint"} == set(b) - {"hint"}


def test_the_mounting_names_never_cross_stacks() -> None:
    assert not set(mounting_config.param_names()) & set(ardupilot_mounting.param_names())


# ---------------------------------------------------------------------------
# Frame size
# ---------------------------------------------------------------------------

def _rotors(positions: list[tuple[float, float]], **extra: float) -> dict[str, float]:
    values = {"CA_AIRFRAME": 0.0, "CA_ROTOR_COUNT": float(len(positions))}
    for i, (x, y) in enumerate(positions):
        values[f"CA_ROTOR{i}_PX"] = x
        values[f"CA_ROTOR{i}_PY"] = y
        values[f"CA_ROTOR{i}_PZ"] = 0.0
    values.update(extra)
    return values


def test_a_450_quad_x_measures_450_mm_on_the_diagonal() -> None:
    a = 0.45 / 2 / 2 ** 0.5
    frame = motor_config.build(_rotors([(a, a), (-a, -a), (a, -a), (-a, a)]))["frame"]
    assert frame["adjustable"] is True
    assert frame["diagonal"] == pytest.approx(0.45, abs=1e-4)
    assert frame["length"] == pytest.approx(2 * a, abs=1e-4)
    assert frame["width"] == pytest.approx(2 * a, abs=1e-4)
    assert frame["center"] == {"x": pytest.approx(0.0), "y": pytest.approx(0.0)}
    assert [m["x_param"] for m in frame["motors"]] == [
        f"CA_ROTOR{i}_PX" for i in range(4)]


def test_an_h_frame_keeps_its_own_length_and_width() -> None:
    frame = motor_config.build(_rotors(
        [(0.2, 0.15), (-0.2, -0.15), (0.2, -0.15), (-0.2, 0.15)]))["frame"]
    assert frame["length"] == pytest.approx(0.4)
    assert frame["width"] == pytest.approx(0.3)
    assert frame["diagonal"] == pytest.approx(0.5)


def test_a_centre_of_gravity_off_the_middle_is_the_frames_centre() -> None:
    frame = motor_config.build(_rotors(
        [(0.25, 0.2), (-0.15, -0.2), (0.25, -0.2), (-0.15, 0.2)]))["frame"]
    assert frame["center"]["x"] == pytest.approx(0.05)
    assert frame["length"] == pytest.approx(0.4)


def test_a_quadplanes_pusher_is_not_part_of_the_frame() -> None:
    values = _rotors([(0.3, 0.3), (-0.3, -0.3), (0.3, -0.3), (-0.3, 0.3), (-0.8, 0.0)],
                     CA_AIRFRAME=2.0)
    for i in range(4):
        values[f"CA_ROTOR{i}_AZ"] = -1.0
    values["CA_ROTOR4_AX"] = 1.0
    values["CA_ROTOR4_AZ"] = 0.0
    frame = motor_config.build(values)["frame"]
    assert [m["number"] for m in frame["motors"]] == [1, 2, 3, 4]
    assert frame["length"] == pytest.approx(0.6)


def test_a_frame_with_no_positions_says_so() -> None:
    frame = motor_config.build(_rotors([(0.0, 0.0)] * 4))["frame"]
    assert frame["adjustable"] is False
    assert "No motor positions" in frame["note"]


def test_rotors_in_one_line_have_no_width_to_scale() -> None:
    frame = motor_config.build(_rotors([(0.3, 0.0), (-0.3, 0.0)]))["frame"]
    assert frame["adjustable"] is True
    assert frame["length"] == pytest.approx(0.6)
    assert frame["width"] is None


def test_a_rotor_without_both_coordinates_is_not_scaled() -> None:
    values = _rotors([(0.2, 0.2), (-0.2, -0.2), (0.2, -0.2), (-0.2, 0.2)])
    del values["CA_ROTOR3_PY"]
    frame = motor_config.build(values)["frame"]
    assert [m["number"] for m in frame["motors"]] == [1, 2, 3]


def test_ardupilot_reports_the_frame_size_rather_than_hiding_it() -> None:
    frame = ardupilot_motors.build({"FRAME_CLASS": 1.0, "FRAME_TYPE": 1.0})["frame"]
    assert frame["adjustable"] is False
    assert "ArduPilot" in frame["note"]


# ---------------------------------------------------------------------------
# Operator-facing text
# ---------------------------------------------------------------------------

def _strings(node: Any) -> list[str]:
    if isinstance(node, str):
        return [node]
    if isinstance(node, dict):
        return [s for k, v in node.items() if k not in ("param", "also") for s in _strings(v)]
    if isinstance(node, list):
        return [s for v in node for s in _strings(v)]
    return []


def test_no_dashes_in_the_text_an_operator_reads() -> None:
    """AGENTS.md: no em dash, en dash or spaced hyphen in user-facing prose."""
    docs = [
        mounting_config.build(_px4(EKF2_GPS_POS_X=0.0, EKF2_GPS_POS_Y=0.0,
                                   EKF2_GPS_POS_Z=0.0)),
        ardupilot_mounting.build(_ardupilot(GPS1_POS_X=0.0, GPS2_TYPE=1.0,
                                            GPS2_POS_X=0.0)),
        motor_config.build(_rotors([(0.2, 0.2), (-0.2, -0.2)])),
        motor_config.build(_rotors([(0.0, 0.0), (0.0, 0.0)])),
        ardupilot_motors.build({"FRAME_CLASS": 1.0}),
    ]
    for text in _strings([d.get("orientation") for d in docs[:2]]
                         + [d.get("positions") for d in docs[:2]]
                         + [d.get("frame") for d in docs[2:]]):
        assert "—" not in text and "–" not in text and " - " not in text, text
