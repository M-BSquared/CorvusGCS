"""corvus.sik_config — the SiK radio schema, parsing and write validation.

This module is the one place that decides what a radio is allowed to be told,
and every one of those decisions has a way to be wrong that ends with an
operator holding a radio that no longer talks. So the tests are grouped by the
failure each rule prevents rather than by function.

No serial port and no radio is involved: everything here is text in, dict out.
"""
from __future__ import annotations

import pytest

from corvus import sik_config as sc

# A stock SiK v2 report, with the two registers a newer build adds on the end.
ATI5_REPORT = """ATI5
S0: FORMAT=25
S1: SERIAL_SPEED=57
S2: AIR_SPEED=64
S3: NETID=25
S4: TXPOWER=20
S5: ECC=0
S6: MAVLINK=1
S7: OPPRESEND=1
S8: MIN_FREQ=915000
S9: MAX_FREQ=928000
S10: NUM_CHANNELS=50
S11: DUTY_CYCLE=100
S12: LBT_RSSI=0
S13: MANCHESTER=0
S14: RTSCTS=0
S15: MAX_WINDOW=131
S16: ENCRYPTION_LEVEL=0
OK
"""


@pytest.fixture()
def registers() -> dict[str, dict[str, int]]:
    return sc.parse_registers(ATI5_REPORT)


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------

def test_every_register_line_is_parsed_with_its_number(registers) -> None:
    assert len(registers) == 17
    assert registers["AIR_SPEED"] == {"number": 2, "value": 64}
    assert registers["MAX_WINDOW"] == {"number": 15, "value": 131}


def test_the_command_echo_and_the_ok_are_not_registers(registers) -> None:
    assert "ATI5" not in registers
    assert "OK" not in registers


def test_firmware_that_prints_the_report_differently_still_parses() -> None:
    # No space after the colon, CR line endings, and a partner radio's traffic
    # interleaved — all three happen on real hardware.
    text = "S1:SERIAL_SPEED=115\r\n<garbage from the air>\r\nS2: AIR_SPEED=128\r"
    parsed = sc.parse_registers(text)
    assert parsed["SERIAL_SPEED"]["value"] == 115
    assert parsed["AIR_SPEED"]["value"] == 128


def test_a_radio_that_said_nothing_parses_to_nothing() -> None:
    assert sc.parse_registers("") == {}
    assert sc.parse_registers("ERROR\r\n") == {}


def test_the_firmware_banner_is_taken_and_the_echo_is_not() -> None:
    assert sc.parse_version("ATI\r\nSiK 2.0 on HM-TRP\r\nOK\r\n") == "SiK 2.0 on HM-TRP"
    assert sc.parse_version("ATI\r\nOK\r\n") == ""
    assert sc.parse_version("") == ""


def test_the_link_report_carries_both_ends_and_a_fade_margin() -> None:
    report = sc.parse_rssi(
        "L/R RSSI: 208/205  L/R noise: 41/38 pkts: 20 txe=0 rxe=0 stx=0")
    assert report is not None
    assert (report["local_rssi"], report["remote_rssi"]) == (208, 205)
    assert (report["local_noise"], report["remote_noise"]) == (41, 38)
    # (signal - noise) / 2, the SiK rule of thumb: range doubles per 6 dB.
    assert report["local_margin_db"] == pytest.approx(83.5)


def test_a_radio_with_no_link_report_is_reported_as_absent_not_as_zero() -> None:
    # Zeroes would read as a dead link on screen; None lets the page leave the
    # row out entirely, which is the truth.
    assert sc.parse_rssi("OK\r\n") is None


# ---------------------------------------------------------------------------
# The one-byte form
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(("register", "baud"), [
    (2, 2400), (9, 9600), (19, 19200), (38, 38400),
    (57, 57600), (115, 115200), (230, 230400),
])
def test_the_serial_speed_register_truncates_rather_than_divides(register, baud) -> None:
    # The regression this guards: 57 * 1000 is 57000, which is not a baud rate
    # any radio runs at, and reconnecting the link at it never comes back.
    assert sc.baud_from_register(register) == baud
    assert sc.register_from_baud(baud) == register


def test_a_register_above_two_thousand_is_already_a_baud_rate() -> None:
    assert sc.baud_from_register(115200) == 115200


# ---------------------------------------------------------------------------
# Describing a radio
# ---------------------------------------------------------------------------

def test_known_registers_get_their_labels_options_and_hints(registers) -> None:
    described = sc.describe_radio(registers, version="SiK 2.0 on HM-TRP")
    by_name = {f["name"]: f for f in described["fields"]}

    air = by_name["AIR_SPEED"]
    assert air["label"] == "Air data rate"
    assert air["kind"] == "enum"
    assert {o["value"] for o in air["options"]} == set(sc.AIR_SPEEDS)
    assert air["must_match"] is True
    assert air["hint"]


def test_the_eeprom_format_is_reported_but_not_editable(registers) -> None:
    by_name = {f["name"]: f for f in sc.describe_radio(registers)["fields"]}
    assert by_name["FORMAT"]["read_only"] is True


def test_a_register_this_build_has_never_heard_of_is_still_shown() -> None:
    # A newer SiK, or RFD900-class firmware with a longer table. Dropping it
    # would make a setting invisible until Corvus itself was updated.
    parsed = sc.parse_registers("S0: FORMAT=25\nS42: SOMETHING_NEW=7\n")
    fields = sc.describe_radio(parsed)["fields"]
    extra = [f for f in fields if f["name"] == "SOMETHING_NEW"]
    assert extra and extra[0]["register"] == 42 and extra[0]["value"] == 7


def test_an_enum_value_outside_its_option_list_is_kept_as_its_own_option() -> None:
    # Snapping it to the first option would rewrite a working radio the moment
    # the operator touched an unrelated field.
    parsed = sc.parse_registers("S2: AIR_SPEED=77\n")
    field = sc.describe_radio(parsed)["fields"][0]
    assert field["value"] == 77
    assert {"value": 77, "label": "Unknown (77)"} in field["options"]


def test_a_register_the_radio_did_not_report_is_not_invented(registers) -> None:
    trimmed = {k: v for k, v in registers.items() if k != "ENCRYPTION_LEVEL"}
    names = {f["name"] for f in sc.describe_radio(trimmed)["fields"]}
    assert "ENCRYPTION_LEVEL" not in names


# ---------------------------------------------------------------------------
# Comparing the pair
# ---------------------------------------------------------------------------

def test_only_the_registers_that_must_match_are_compared(registers) -> None:
    local = sc.describe_radio(registers)
    far = dict(registers)
    far["NETID"] = {"number": 3, "value": 30}       # must match  -> reported
    far["TXPOWER"] = {"number": 4, "value": 11}     # may differ  -> ignored
    remote = sc.describe_radio(far)

    names = {m["name"] for m in sc.compare(local, remote)}
    assert names == {"NETID"}


def test_an_unreachable_remote_is_not_eight_simultaneous_mismatches(registers) -> None:
    assert sc.compare(sc.describe_radio(registers), None) == []


def test_copying_to_remote_stages_only_the_differing_shared_registers(registers) -> None:
    local = sc.describe_radio(registers)
    far = dict(registers)
    far["NETID"] = {"number": 3, "value": 30}
    far["AIR_SPEED"] = {"number": 2, "value": 128}
    far["TXPOWER"] = {"number": 4, "value": 11}
    remote = sc.describe_radio(far)

    # Transmit power is a property of where each radio is installed, not of the
    # link, so an aircraft radio is allowed to keep its own.
    assert sc.copy_to_remote(local, remote) == {"AIR_SPEED": 64, "NETID": 25}


# ---------------------------------------------------------------------------
# Validating a write
# ---------------------------------------------------------------------------

def test_a_write_returns_only_what_actually_changed(registers) -> None:
    writes = sc.validate_writes({"NETID": 42, "TXPOWER": 20}, registers)
    # TXPOWER was already 20; rewriting it would cost a reboot for nothing.
    assert writes == [{"name": "NETID", "register": 3, "value": 42}]


def test_writes_come_back_in_register_order(registers) -> None:
    writes = sc.validate_writes({"MAX_WINDOW": 33, "NETID": 42, "AIR_SPEED": 128},
                                registers)
    assert [w["register"] for w in writes] == [2, 3, 15]


def test_an_air_rate_the_radio_cannot_produce_is_refused(registers) -> None:
    # The radio would silently round 100 up to 128 and the operator would have
    # no way to know the number they chose was not the number in effect.
    with pytest.raises(sc.SikConfigError, match="Air data rate"):
        sc.validate_writes({"AIR_SPEED": 100}, registers)


def test_the_eeprom_format_cannot_be_written(registers) -> None:
    with pytest.raises(sc.SikConfigError, match="FORMAT"):
        sc.validate_writes({"FORMAT": 1}, registers)


def test_a_register_this_radio_does_not_have_is_refused(registers) -> None:
    trimmed = {k: v for k, v in registers.items() if k != "ENCRYPTION_LEVEL"}
    with pytest.raises(sc.SikConfigError, match="no ENCRYPTION_LEVEL"):
        sc.validate_writes({"ENCRYPTION_LEVEL": 1}, trimmed)


@pytest.mark.parametrize("bad", [
    {"NETID": 900},          # above the range
    {"DUTY_CYCLE": 101},     # above 100%
    {"MAX_WINDOW": 5},       # below the floor
])
def test_a_value_outside_its_range_is_refused(registers, bad) -> None:
    with pytest.raises(sc.SikConfigError):
        sc.validate_writes(bad, registers)


@pytest.mark.parametrize("bad", ["64", 64.5, True, None])
def test_a_value_that_is_not_a_whole_number_is_refused(registers, bad) -> None:
    with pytest.raises(sc.SikConfigError):
        sc.validate_writes({"AIR_SPEED": bad}, registers)


def test_an_inverted_band_is_refused_even_when_only_one_end_was_sent(registers) -> None:
    # A radio given max <= min stops transmitting and has to be recovered over
    # a cable, so the check reads the half that was not sent from the radio.
    with pytest.raises(sc.SikConfigError, match="above the minimum"):
        sc.validate_writes({"MAX_FREQ": 900000}, registers)
    with pytest.raises(sc.SikConfigError, match="above the minimum"):
        sc.validate_writes({"MIN_FREQ": 930000}, registers)
    # And a band that is moved as a pair is fine.
    assert sc.validate_writes({"MIN_FREQ": 433050, "MAX_FREQ": 434790}, registers)


# ---------------------------------------------------------------------------
# The command sequence
# ---------------------------------------------------------------------------

def test_a_write_plan_sets_then_commits_then_reboots() -> None:
    plan = sc.write_plan([
        {"name": "NETID", "register": 3, "value": 42},
        {"name": "TXPOWER", "register": 4, "value": 14},
    ], remote=False)
    # Without &W the settings evaporate at the next power cycle; without Z most
    # of them are not in effect at all.
    assert plan == ["ATS3=42", "ATS4=14", "AT&W", "ATZ"]


def test_the_remote_plan_is_the_same_sequence_in_the_rt_dialect() -> None:
    plan = sc.write_plan([{"name": "NETID", "register": 3, "value": 42}], remote=True)
    assert plan == ["RTS3=42", "RT&W", "RTZ"]


def test_an_empty_plan_does_not_commit_or_reboot() -> None:
    # Nothing changed, so nothing is worth dropping the link for.
    assert sc.write_plan([], remote=False) == []


# ---------------------------------------------------------------------------
# The static schema
# ---------------------------------------------------------------------------

def test_the_schema_carries_every_register_with_its_help_text() -> None:
    schema = sc.schema()
    names = [r["name"] for r in schema["registers"]]
    assert names == [r["name"] for r in sc.REGISTERS]
    assert all(r.get("hint") for r in schema["registers"])
    assert set(schema["must_match"]) == set(sc.MUST_MATCH)
    assert schema["default_baud"] == sc.DEFAULT_BAUD
