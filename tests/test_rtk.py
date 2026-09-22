"""The RTK wire formats, proven without hardware.

Every assertion here is about a byte that goes out of the building. The three
that matter most, and the failure each one prevents:

* **CRC-24Q.** A wrong table produces frames a receiver silently discards, and
  an RTK session that looks perfect from the ground station and delivers no fix.
* **Fragmentation.** ``GPS_RTCM_DATA`` numbers its fragments in two bits, so a
  correction that needs five is unsendable. Getting the boundary wrong by one
  sends four fifths of a message and waits.
* **Scaling.** Survey accuracy is metres to the operator and 0.1 mm on the
  wire; a base position is degrees and a 1e-9 remainder. A factor of ten in
  either is a base that converges on the wrong thing without ever erroring.
"""
from __future__ import annotations

import struct

import pytest

from corvus import rtk


# ---------------------------------------------------------------------------
# RTCM 3
# ---------------------------------------------------------------------------


def _frame(payload: bytes) -> bytes:
    head = b"\xd3" + bytes([(len(payload) >> 8) & 0x03, len(payload) & 0xFF])
    body = head + payload
    return body + rtk.crc24q(body).to_bytes(3, "big")


def test_crc24q_matches_the_published_check_value():
    """The standard's own check string, which pins the parameters.

    RTCM's CRC-24Q is the polynomial 0x1864CFB with an initial value of
    **zero** — not the 0xB704CE the OpenPGP variant of the same polynomial
    uses. Both are "CRC-24"; only one produces frames a receiver accepts.
    """
    assert rtk.crc24q(b"123456789") == 0xCDE703
    assert rtk.crc24q(b"") == 0


def test_a_frame_split_across_reads_is_reassembled():
    framer = rtk.RtcmFramer()
    frame = _frame(bytes([0x3E, 0xD0]) + b"payload")
    assert framer.feed(frame[:4]) == []
    assert framer.feed(frame[4:]) == [frame]
    assert framer.frames == 1
    assert framer.crc_errors == 0


def test_leading_rubbish_is_skipped_and_counted():
    framer = rtk.RtcmFramer()
    frame = _frame(b"\x3e\xd0hello")
    assert framer.feed(b"\x01\x02\x03" + frame) == [frame]
    assert framer.discarded == 3


def test_a_corrupt_frame_costs_only_itself():
    """One bad byte on a cable must not take the next message with it."""
    framer = rtk.RtcmFramer()
    good = _frame(b"\x3e\xd0" + b"a" * 30)
    bad = bytearray(_frame(b"\x3e\xd0" + b"b" * 30))
    bad[-1] ^= 0xFF
    out = framer.feed(bytes(bad) + good)
    assert out == [good]
    assert framer.crc_errors >= 1


def test_the_buffer_cannot_grow_without_limit():
    """A device streaming NMEA at 38400 baud is not a reason to run out of memory."""
    framer = rtk.RtcmFramer()
    for _ in range(50):
        framer.feed(b"$GPGGA,noise,and,more,noise\r\n" * 40)
    assert len(framer._buf) <= framer._MAX_BUFFER


def test_the_message_number_is_read_off_the_payload():
    assert rtk.message_number(_frame(bytes([0x3E, 0xD0]) + b"x" * 10)) == 1005
    assert rtk.message_number(_frame(bytes([0x43, 0x20]) + b"x" * 10)) == 1074
    assert rtk.message_number(b"\xd3\x00") == 0


# ---------------------------------------------------------------------------
# GPS_RTCM_DATA
# ---------------------------------------------------------------------------


def test_a_short_correction_is_sent_whole_with_the_fragment_bit_clear():
    parts = rtk.fragments(b"x" * 40, sequence=3)
    assert len(parts) == 1
    flags, data = parts[0]
    assert flags & 0x01 == 0, "an unfragmented message must not claim to be fragmented"
    assert (flags >> 3) & 0x1F == 3
    assert data == b"x" * 40


def test_a_long_correction_is_fragmented_in_order():
    parts = rtk.fragments(b"y" * 400, sequence=1)
    assert [len(d) for _f, d in parts] == [180, 180, 40]
    for index, (flags, _data) in enumerate(parts):
        assert flags & 0x01 == 1
        assert (flags >> 1) & 0x03 == index
        assert (flags >> 3) & 0x1F == 1


def test_an_exact_multiple_of_the_fragment_size_gets_a_terminator():
    """A receiver knows a sequence ended by seeing a fragment shorter than 180.

    Without the empty fragment, a 360-byte correction is two full fragments
    and the far end waits for a third that never comes — then discards the
    whole sequence when the next one starts.
    """
    parts = rtk.fragments(b"z" * 360, sequence=0)
    assert [len(d) for _f, d in parts] == [180, 180, 0]
    assert (parts[-1][0] >> 1) & 0x03 == 2


@pytest.mark.parametrize("size,count", [(179, 1), (180, 2), (540, 4), (719, 4)])
def test_everything_that_fits_in_four_fragments_is_sent(size, count):
    assert len(rtk.fragments(b"q" * size, 0)) == count


def test_a_correction_that_cannot_be_expressed_is_dropped_whole():
    """Three quarters of a correction is not three quarters of a fix."""
    assert rtk.fragments(b"q" * 720, 0) == []
    assert rtk.fragments(b"", 0) == []


def test_the_sequence_number_wraps_at_five_bits():
    flags, _data = rtk.fragments(b"x" * 10, sequence=32)[0]
    assert (flags >> 3) & 0x1F == 0


# ---------------------------------------------------------------------------
# UBX
# ---------------------------------------------------------------------------


def test_a_ubx_frame_round_trips_through_the_parser():
    frame = rtk.ubx_frame(0x06, 0x8A, b"\x01\x02\x03")
    parser = rtk.UbxParser()
    assert parser.feed(frame[:3]) == []
    assert parser.feed(frame[3:]) == [(0x06, 0x8A, b"\x01\x02\x03")]


def test_a_bad_checksum_does_not_swallow_the_next_message():
    good = rtk.ubx_frame(0x01, 0x3B, b"\x00" * 8)
    bad = bytearray(rtk.ubx_frame(0x01, 0x3B, b"\xff" * 8))
    bad[-1] ^= 0xFF
    assert rtk.UbxParser().feed(bytes(bad) + good) == [(0x01, 0x3B, b"\x00" * 8)]


def test_valset_encodes_each_value_at_the_width_its_key_declares():
    """The key carries its own storage size; a value written at the wrong
    width shifts every pair after it and the receiver reads nonsense."""
    frame = rtk.valset([
        (rtk.KEY_TMODE_MODE, 1),              # one byte
        (rtk.KEY_RATE_MEAS, 1000),            # two bytes
        (rtk.KEY_TMODE_SVIN_MIN_DUR, 180),    # four bytes
    ])
    payload = rtk.UbxParser().feed(frame)[0][2]
    assert payload[:4] == b"\x00\x01\x00\x00"   # version, RAM layer, reserved
    body = payload[4:]
    assert len(body) == (4 + 1) + (4 + 2) + (4 + 4)
    assert struct.unpack_from("<I", body, 0)[0] == rtk.KEY_TMODE_MODE
    assert body[4] == 1
    assert struct.unpack_from("<H", body, 9)[0] == 1000
    assert struct.unpack_from("<I", body, 15)[0] == 180


def test_survey_in_asks_for_the_accuracy_in_the_units_the_receiver_uses():
    """Metres to the operator, 0.1 mm on the wire."""
    frames = rtk.survey_in_frames(180, 2.0)
    items = _valset_items(frames)
    assert items[rtk.KEY_TMODE_SVIN_ACC_LIMIT] == 20000
    assert items[rtk.KEY_TMODE_SVIN_MIN_DUR] == 180
    assert items[rtk.KEY_TMODE_MODE] == 1


def test_survey_in_stops_the_old_survey_before_starting_the_new_one():
    """Reapplying survey-in mode does not restart a running survey, so the
    mode has to pass through 0 or the new settings are silently ignored."""
    modes = _mode_sequence(rtk.survey_in_frames(180, 2.0))
    assert modes == [0, 1]


def test_output_is_silenced_before_the_time_mode_changes():
    """Corrections emitted mid-reconfiguration are computed against a position
    the receiver is in the middle of abandoning."""
    frames = rtk.survey_in_frames(180, 2.0)
    first = _valset_items([frames[0]])
    assert all(value == 0 for value in first.values())
    assert rtk.KEY_MSGOUT_RTCM_1005_I2C + 3 in first


def test_a_fixed_base_position_splits_into_its_coarse_and_fine_halves():
    """One 32-bit field cannot hold a millimetre-accurate position on a planet
    this size, so u-blox stores two numbers per axis."""
    items = _valset_items(rtk.fixed_position_frames(48.0766789123, 11.6474, 560.1234))
    assert items[rtk.KEY_TMODE_MODE] == 2
    assert items[rtk.KEY_TMODE_POS_TYPE] == 1
    assert items[rtk.KEY_TMODE_LAT] == 480766789
    assert items[rtk.KEY_TMODE_LAT_HP] == 12
    assert items[rtk.KEY_TMODE_HEIGHT] == 56012        # cm
    assert items[rtk.KEY_TMODE_HEIGHT_HP] == 34        # 0.1 mm


def test_a_negative_coordinate_keeps_its_sign_in_both_halves():
    items = _valset_items(rtk.fixed_position_frames(-33.8688, -151.2093, 58.0))
    assert items[rtk.KEY_TMODE_LAT] < 0 or items[rtk.KEY_TMODE_LAT] > 2 ** 31
    # Read back as a signed 32-bit value, which is what the receiver does.
    raw = items[rtk.KEY_TMODE_LAT]
    signed = raw - 2 ** 32 if raw >= 2 ** 31 else raw
    assert signed == -338688000


def test_both_output_ports_are_configured():
    """Which port a receiver answers on is not knowable from this side of the
    cable — a bare F9P is on USB, the same chip behind an FTDI is on UART1."""
    items = _valset_items(rtk.activate_output_frames())
    assert items[rtk.KEY_MSGOUT_RTCM_1005_I2C + 1] == rtk.RTCM_1005_RATE
    assert items[rtk.KEY_MSGOUT_RTCM_1005_I2C + 3] == rtk.RTCM_1005_RATE


def test_activating_output_turns_the_survey_reports_off():
    items = _valset_items(rtk.activate_output_frames())
    assert items[rtk.KEY_MSGOUT_NAV_SVIN_I2C + 3] == 0
    assert items[rtk.KEY_RATE_MEAS] == 1000


def test_disabling_takes_the_receiver_out_of_base_mode():
    items = _valset_items(rtk.disable_base_frames())
    assert items[rtk.KEY_TMODE_MODE] == 0
    assert items[rtk.KEY_MSGOUT_RTCM_1005_I2C + 3] == 0


def test_an_older_receiver_gets_the_struct_messages_instead():
    """An M8P speaks CFG-TMODE3, not CFG-VALSET, and refusing it would tell an
    operator with working hardware that their base is unsupported."""
    frames = rtk.legacy_survey_in_frames(180, 2.0)
    parsed = [rtk.UbxParser().feed(f)[0] for f in frames]
    tmode = [p for p in parsed if p[:2] == (rtk.UBX_CLASS_CFG, rtk.UBX_ID_CFG_TMODE3)]
    assert len(tmode) == 2, "the old survey has to be stopped here too"
    assert len(tmode[0][2]) == 40
    flags, = struct.unpack_from("<H", tmode[1][2], 2)
    assert flags == 1
    # fixedPosAcc, svinMinDur, svinAccLimit sit at offsets 20, 24 and 28.
    _acc, dur, limit = struct.unpack_from("<III", tmode[1][2], 20)
    assert (dur, limit) == (180, 20000)


def test_protocol_version_decides_which_path_is_used():
    assert rtk.supports_valset({"protocol": 27.0}) is True
    assert rtk.supports_valset({"protocol": 20.01}) is False
    # A receiver that did not say is assumed old: the legacy messages are
    # still accepted by the new parts, and the reverse is not true.
    assert rtk.supports_valset({}) is False
    assert rtk.supports_valset(None) is False


def test_mon_ver_is_read_out_of_its_extension_lines():
    payload = b"EXT CORE 1.00 (a1b2)".ljust(30, b"\x00")
    payload += b"00190000".ljust(10, b"\x00")
    payload += b"PROTVER=27.11".ljust(30, b"\x00")
    payload += b"MOD=ZED-F9P".ljust(30, b"\x00")
    version = rtk.parse_mon_ver(payload)
    assert version["protocol"] == 27.11
    assert version["model"] == "ZED-F9P"
    assert "ZED-F9P" in rtk.receiver_label(version)


def test_nav_svin_is_reported_in_metres():
    payload = struct.pack(
        "<B3sIIiiibbbbIIBB",
        0, b"\x00" * 3, 0, 42, 1, 2, 3, 0, 0, 0, 0, 18300, 421, 1, 0,
    ) + b"\x00\x00"
    survey = rtk.parse_nav_svin(payload)
    assert survey["duration"] == 42
    assert survey["accuracy"] == pytest.approx(1.83)
    assert survey["valid"] is True and survey["active"] is False
    assert rtk.parse_nav_svin(b"too short") is None


# ---------------------------------------------------------------------------
# Progress and settings
# ---------------------------------------------------------------------------


def test_progress_reports_whichever_half_is_still_holding_the_survey_up():
    """Time and accuracy converge at different speeds, and a bar that tracks
    only one of them sits at 100% for minutes."""
    early = {"duration": 5, "accuracy": 20.0, "valid": False, "active": True}
    assert rtk.survey_progress(early, 180, 2.0) < 15
    # Long past the minimum duration but still far from the accuracy limit.
    slow = {"duration": 900, "accuracy": 8.0, "valid": False, "active": True}
    assert rtk.survey_progress(slow, 180, 2.0) == 25
    done = {"duration": 200, "accuracy": 1.2, "valid": True, "active": False}
    assert rtk.survey_progress(done, 180, 2.0) == 100
    assert rtk.survey_progress(None, 180, 2.0) == 0


def test_progress_never_reads_complete_before_the_receiver_says_so():
    nearly = {"duration": 10_000, "accuracy": 0.1, "valid": False, "active": True}
    assert rtk.survey_progress(nearly, 180, 2.0) == 99


def test_the_defaults_are_plug_and_play():
    """The feature is the default. Changing either of these first two lines
    turns RTK back into an option nobody switches on."""
    defaults = rtk.defaults()
    assert defaults["enabled"] is True
    assert defaults["source"] == "usb"
    assert defaults["mode"] == "survey"
    # QGroundControl's numbers, kept so the same hardware behaves the same way.
    assert defaults["survey_accuracy"] == 2.0
    assert defaults["survey_duration"] == 180


def test_settings_survive_a_hand_edited_config_file():
    """This runs against a file an operator may have edited, so nothing in it
    may raise — and a zero accuracy is a survey that never finishes."""
    resolved = rtk.settings({
        "enabled": "yes", "source": "carrier-pigeon", "mode": 7,
        "survey_accuracy": 0, "survey_duration": -1,
        "fixed": {"latitude": 900.0}, "ntrip": {"port": 99999},
    })
    assert resolved["enabled"] is True          # not a bool -> default
    assert resolved["source"] == "usb"
    assert resolved["mode"] == "survey"
    assert resolved["survey_accuracy"] >= rtk.SURVEY_ACCURACY_MIN_M
    assert resolved["survey_duration"] >= rtk.SURVEY_DURATION_MIN_S
    assert resolved["fixed"]["latitude"] == 90.0
    assert resolved["ntrip"]["port"] == 65535
    assert rtk.settings(None) == rtk.defaults()
    assert rtk.settings("nonsense") == rtk.defaults()


def test_null_island_is_refused_as_a_base_position():
    """0, 0 is what an unfilled form produces and a valid coordinate, and a
    base configured there tells the aircraft the errors of a receiver in the
    Atlantic — which the autopilot believes."""
    assert rtk.fixed_position_problem({"latitude": 0.0, "longitude": 0.0})
    assert rtk.fixed_position_problem({"latitude": 48.07, "longitude": 11.64}) == ""


def test_an_ntrip_stream_needs_both_halves_of_its_address():
    assert rtk.ntrip_problem({"host": "", "mountpoint": "M"})
    assert rtk.ntrip_problem({"host": "caster.example", "mountpoint": ""})
    assert rtk.ntrip_problem({"host": "caster.example", "mountpoint": "MSM4"}) == ""


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _valset_items(frames: list[bytes]) -> dict[int, int]:
    """Every key/value pair in a list of CFG-VALSET frames, flattened."""
    items: dict[int, int] = {}
    for frame in frames:
        for msg_class, msg_id, payload in rtk.UbxParser().feed(frame):
            if (msg_class, msg_id) != (rtk.UBX_CLASS_CFG, rtk.UBX_ID_CFG_VALSET):
                continue
            offset = 4
            while offset + 4 <= len(payload):
                key = struct.unpack_from("<I", payload, offset)[0]
                offset += 4
                size_field = (key >> 28) & 0x07
                width = 1 if size_field <= 2 else 2 if size_field == 3 else 4
                items[key] = int.from_bytes(payload[offset:offset + width], "little")
                offset += width
    return items


def _mode_sequence(frames: list[bytes]) -> list[int]:
    """Every value TMODE-MODE is set to, in order."""
    modes: list[int] = []
    for frame in frames:
        for msg_class, msg_id, payload in rtk.UbxParser().feed(frame):
            if (msg_class, msg_id) != (rtk.UBX_CLASS_CFG, rtk.UBX_ID_CFG_VALSET):
                continue
            offset = 4
            while offset + 4 <= len(payload):
                key = struct.unpack_from("<I", payload, offset)[0]
                offset += 4
                size_field = (key >> 28) & 0x07
                width = 1 if size_field <= 2 else 2 if size_field == 3 else 4
                if key == rtk.KEY_TMODE_MODE:
                    modes.append(int.from_bytes(payload[offset:offset + width], "little"))
                offset += width
    return modes
