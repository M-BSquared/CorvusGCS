"""The Remote ID identity: what is stored, what is checked, and what is sent.

Three things are pinned here, and they are pinned separately because they fail
separately.

*Coercion* is the boundary. Everything in this model arrives from a browser or
from a hand-edited config file and leaves on a radio a regulator can listen to,
so a 40-character serial number, a UTF-8 operator name or a latitude of 500 has
to be cut before it reaches an encoder that would take the first 20 bytes and
shift every field after them.

*Findings* are the filing. A serial number that is 20 valid characters and not
in ANSI/CTA-2063-A form encodes perfectly and fails a ramp check, so the only
place that mistake can be caught is here — and it must be caught as a report
rather than as a refusal, because an operator flying under an exemption is not
misconfigured.

*Messages* are the wire. degE7 latitudes, the 2019 epoch, the reserved
``id_or_mac``, and the -1000 that means "not known" are all encodings nobody
sees until a receiver on the ground reads them wrong, which is why they are
asserted here rather than trusted to a live link.
"""
from __future__ import annotations

import datetime

from corvus import remote_id


# ---------------------------------------------------------------------------
# Coercion
# ---------------------------------------------------------------------------

def test_an_absent_identity_resolves_to_a_broadcast_that_is_off() -> None:
    """Off is the only safe default: a blank filing is a false one, not a small one."""
    resolved = remote_id.defaults()

    assert resolved["enabled"] is False
    assert resolved["basic_id"]["uas_id"] == ""
    assert resolved["system"]["operator_altitude_geo"] == remote_id.ALTITUDE_UNKNOWN


def test_every_text_field_is_cut_to_the_width_the_message_carries() -> None:
    resolved = remote_id.settings({
        "basic_id": {"uas_id": "X" * 40},
        "operator_id": {"operator_id": "Y" * 40},
        "self_id": {"description": "Z" * 40},
    })

    assert len(resolved["basic_id"]["uas_id"]) == remote_id.UAS_ID_MAX
    assert len(resolved["operator_id"]["operator_id"]) == remote_id.OPERATOR_ID_MAX
    assert len(resolved["self_id"]["description"]) == remote_id.DESCRIPTION_MAX


def test_non_ascii_is_dropped_rather_than_transliterated() -> None:
    """An 'ö' does not arrive as 'o'; it arrives as two bytes of noise."""
    resolved = remote_id.settings({"self_id": {"description": "Böck survey"}})

    assert resolved["self_id"]["description"] == "Bck survey"


def test_an_out_of_range_enum_falls_back_instead_of_being_preserved() -> None:
    """Unlike a parameter page: this number came from a browser, not an aircraft."""
    resolved = remote_id.settings({"basic_id": {"ua_type": 99, "id_type": 77}})

    assert resolved["basic_id"]["ua_type"] == 0
    assert resolved["basic_id"]["id_type"] == remote_id.ID_TYPE_SERIAL


def test_coordinates_are_clamped_to_the_globe() -> None:
    resolved = remote_id.settings({"system": {
        "operator_latitude": 500.0, "operator_longitude": -900.0,
    }})

    assert resolved["system"]["operator_latitude"] == 90.0
    assert resolved["system"]["operator_longitude"] == -180.0


def test_a_non_finite_number_becomes_the_default_rather_than_nan() -> None:
    resolved = remote_id.settings({"system": {"operator_latitude": float("nan")}})

    assert resolved["system"]["operator_latitude"] == 0.0


def test_settings_is_idempotent() -> None:
    """The load path and the save path run it, so twice must equal once."""
    once = remote_id.settings({"enabled": True, "basic_id": {"uas_id": "ABCD3XYZ"}})

    assert remote_id.settings(once) == once


# ---------------------------------------------------------------------------
# The CTA-2063-A serial number
# ---------------------------------------------------------------------------

def test_a_well_formed_serial_number_has_no_problem() -> None:
    # 4 characters of manufacturer code, 'F' = 15 characters follow, 15 follow.
    assert remote_id.serial_number_problem("1596F483658SK8U6PNJ1") == ""
    assert remote_id.serial_number_problem("ABCD3XYZ") == ""


def test_the_length_character_has_to_agree_with_the_length() -> None:
    problem = remote_id.serial_number_problem("ABCD4XYZ")

    assert "4 characters follow it" in problem
    assert "3 do" in problem


def test_the_letters_that_look_like_digits_are_refused() -> None:
    """CTA-2063-A drops I and O because a printed label cannot distinguish them."""
    assert "I" in remote_id.serial_number_problem("ABCI3XYZ")
    assert "O" in remote_id.serial_number_problem("ABCO3XYZ")


def test_lowercase_is_refused_rather_than_quietly_upcased() -> None:
    assert remote_id.serial_number_problem("abcd3xyz") != ""


# ---------------------------------------------------------------------------
# The EU operator registration
# ---------------------------------------------------------------------------

def test_the_secret_half_of_an_eu_registration_is_called_out() -> None:
    """The three characters after the 16 are the holder's and are never broadcast."""
    problem = remote_id.eu_operator_id_problem("FIN87astrdge12k8xyz")

    assert "secret" in problem


def test_a_sixteen_character_registration_passes() -> None:
    assert remote_id.eu_operator_id_problem("FIN87astrdge12k8") == ""


# ---------------------------------------------------------------------------
# Findings
# ---------------------------------------------------------------------------

def _identity(**over: object) -> dict:
    base = {
        "enabled": True,
        "basic_id": {"id_type": 1, "ua_type": 2, "uas_id": "ABCD3XYZ"},
        "operator_id": {"operator_id_type": 0, "operator_id": "FIN87astrdge12k8"},
        "self_id": {"description_type": 0, "description": "Survey"},
        "system": {"operator_location_type": 2, "operator_latitude": 48.07,
                   "operator_longitude": 11.64, "classification_type": 1,
                   "category_eu": 1, "class_eu": 2},
    }
    for key, value in over.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            base[key] = {**base[key], **value}  # type: ignore[dict-item]
        else:
            base[key] = value  # type: ignore[assignment]
    return base


def _fields(found: list[dict[str, str]], level: str | None = None) -> set[str]:
    return {f["field"] for f in found if level is None or f["level"] == level}


def test_a_complete_eu_filing_has_nothing_to_report() -> None:
    assert remote_id.findings(_identity(), "eu") == []


def test_a_missing_aircraft_id_is_an_error_in_every_region() -> None:
    for region in remote_id.REGIONS:
        found = remote_id.findings(_identity(basic_id={"uas_id": ""}), region)
        assert "uas_id" in _fields(found, "error")


def test_the_eu_asks_for_an_operator_registration_and_the_faa_does_not() -> None:
    """The two rules genuinely differ here, which is the reason the region exists."""
    blank = _identity(operator_id={"operator_id": ""})

    assert "operator_id" in _fields(remote_id.findings(blank, "eu"), "error")
    assert "operator_id" not in _fields(remote_id.findings(blank, "faa"))


def test_the_faa_does_not_accept_the_take_off_point_as_the_operator_position() -> None:
    """Part 89 broadcasts the control station's position, which is not the take-off point."""
    takeoff = _identity(system={"operator_location_type": remote_id.LOCATION_TAKEOFF})
    found = remote_id.findings(takeoff, "faa")

    assert "operator_location_type" in _fields(found, "error")
    # And the EU permits it, so the same identity is clean there.
    assert "operator_location_type" not in _fields(remote_id.findings(takeoff, "eu"))


def test_a_caa_registration_is_not_a_part_89_broadcast_id() -> None:
    caa = _identity(basic_id={"id_type": remote_id.ID_TYPE_CAA, "uas_id": "N12345"})

    assert "id_type" in _fields(remote_id.findings(caa, "faa"), "error")


def test_a_declared_eu_classification_without_a_category_is_an_error() -> None:
    found = remote_id.findings(_identity(system={"category_eu": 0}), "eu")

    assert "category_eu" in _fields(found, "error")


def test_an_undeclared_eu_classification_is_only_a_warning() -> None:
    """Outside the EU, undeclared is the correct setting rather than an omission."""
    found = remote_id.findings(_identity(system={"classification_type": 0}), "eu")

    assert "classification_type" in _fields(found, "warning")
    assert "classification_type" not in _fields(found, "error")


def test_live_gnss_is_reported_as_unavailable_rather_than_hidden() -> None:
    found = remote_id.findings(
        _identity(system={"operator_location_type": remote_id.LOCATION_LIVE_GNSS}), "eu")

    assert "operator_location_type" in _fields(found, "error")
    options = {o["value"]: o for o in remote_id.location_options()}
    assert options[remote_id.LOCATION_LIVE_GNSS]["disabled"] is True
    assert options[remote_id.LOCATION_LIVE_GNSS]["reason"]


def test_a_fixed_operator_position_at_null_island_is_an_error() -> None:
    found = remote_id.findings(
        _identity(system={"operator_latitude": 0.0, "operator_longitude": 0.0}), "eu")

    assert "operator_latitude" in _fields(found, "error")


def test_an_unknown_region_falls_back_to_the_eu_rather_than_reporting_nothing() -> None:
    blank = _identity(operator_id={"operator_id": ""})

    assert "operator_id" in _fields(remote_id.findings(blank, "atlantis"), "error")


# ---------------------------------------------------------------------------
# Messages
# ---------------------------------------------------------------------------

def _sent(identity: dict) -> dict[str, dict]:
    return dict(remote_id.messages(identity, target_system=1, target_component=1))


def test_all_four_messages_go_out_together() -> None:
    """Both stacks time the last one out, so 'unchanged' is not a reason to skip."""
    names = [name for name, _ in
             remote_id.messages(_identity(), target_system=1, target_component=1)]

    assert names == [
        "open_drone_id_basic_id", "open_drone_id_operator_id",
        "open_drone_id_self_id", "open_drone_id_system",
    ]


def test_every_message_is_addressed_and_carries_the_reserved_id_or_mac() -> None:
    for _name, kwargs in remote_id.messages(_identity(), target_system=7,
                                            target_component=3):
        assert kwargs["target_system"] == 7
        assert kwargs["target_component"] == 3
        assert kwargs["id_or_mac"] == b"\x00" * 20


def test_text_fields_are_zero_padded_to_their_exact_width() -> None:
    sent = _sent(_identity())

    assert sent["open_drone_id_basic_id"]["uas_id"] == b"ABCD3XYZ" + b"\x00" * 12
    assert len(sent["open_drone_id_self_id"]["description"]) == remote_id.DESCRIPTION_MAX


def test_a_fixed_operator_position_is_sent_in_deg_e7() -> None:
    sent = _sent(_identity())["open_drone_id_system"]

    assert sent["operator_latitude"] == 480700000
    assert sent["operator_longitude"] == 116400000


def test_the_take_off_setting_sends_no_coordinates_at_all() -> None:
    """The vehicle supplies them; a stale pair beside that flag claims nothing."""
    sent = _sent(_identity(system={
        "operator_location_type": remote_id.LOCATION_TAKEOFF,
        "operator_latitude": 48.07, "operator_longitude": 11.64,
    }))["open_drone_id_system"]

    assert sent["operator_latitude"] == 0
    assert sent["operator_longitude"] == 0
    assert sent["operator_altitude_geo"] == remote_id.ALTITUDE_UNKNOWN


def test_a_class_mark_is_not_broadcast_without_the_classification_it_belongs_to() -> None:
    """A class number under an undeclared scheme is a number nobody can read."""
    sent = _sent(_identity(system={
        "classification_type": remote_id.CLASSIFICATION_UNDECLARED,
        "category_eu": 1, "class_eu": 2,
    }))["open_drone_id_system"]

    assert sent["category_eu"] == 0
    assert sent["class_eu"] == 0


def test_the_system_timestamp_counts_from_2019_not_from_1970() -> None:
    moment = datetime.datetime(2019, 1, 2, tzinfo=datetime.timezone.utc)

    assert remote_id.system_timestamp(moment) == 86400


def test_a_naive_timestamp_is_read_as_utc_rather_than_local_time() -> None:
    naive = datetime.datetime(2019, 1, 2)

    assert remote_id.system_timestamp(naive) == 86400


# ---------------------------------------------------------------------------
# The vehicle's answer, and the airframe suggestion
# ---------------------------------------------------------------------------

def test_the_arm_status_is_normalised_into_something_renderable() -> None:
    good = remote_id.arm_status(remote_id.ARM_STATUS_GOOD, b"")
    bad = remote_id.arm_status(remote_id.ARM_STATUS_FAIL, b"no transmitter")

    assert good["ok"] is True
    assert bad["ok"] is False
    assert bad["error"] == "no transmitter"


def test_an_unreadable_arm_status_reads_as_a_failure_not_as_clearance() -> None:
    assert remote_id.arm_status(None, None)["ok"] is False


def test_the_airframe_suggestion_covers_the_shapes_corvus_flies() -> None:
    assert remote_id.ua_type_for_vehicle(2) == 2    # QUADROTOR -> multirotor
    assert remote_id.ua_type_for_vehicle(1) == 1    # FIXED_WING -> aeroplane
    assert remote_id.ua_type_for_vehicle(21) == 4   # VTOL_TILTROTOR -> hybrid lift
    assert remote_id.ua_type_for_vehicle(999) == 0  # nothing known -> undeclared
