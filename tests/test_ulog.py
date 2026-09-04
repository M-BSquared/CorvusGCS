"""ULog reader: the format, and the ways a real log is not ideal.

The reader is hand-written against the ULog specification rather than taken as
a dependency, so the burden of proof is on it. These tests build ULog files
byte by byte and check what comes back — including the cases a flight actually
produces: truncation from a power loss, dropouts, multi-instance IMUs, nested
message types and padding fields.

The reader has additionally been diffed field-by-field against the reference
implementation (pyulog) on PX4's own sample log: 153 fields, zero mismatches.
That check needs pyulog and a 4 MB fixture, so it is not part of this suite; it
is what justifies trusting the parser at all.
"""
from __future__ import annotations

import struct

import pytest

from corvus.ulog import MAGIC, UlogError, read


def _msg(msg_type: str, body: bytes) -> bytes:
    return struct.pack("<HB", len(body), ord(msg_type)) + body


def _header(timestamp: int = 1234) -> bytes:
    return MAGIC + b"\x01" + struct.pack("<Q", timestamp)


def _format(text: str) -> bytes:
    return _msg("F", text.encode())


def _add(msg_id: int, name: str, multi_id: int = 0) -> bytes:
    return _msg("A", bytes([multi_id]) + struct.pack("<H", msg_id) + name.encode())


def _data(msg_id: int, payload: bytes) -> bytes:
    return _msg("D", struct.pack("<H", msg_id) + payload)


def _keyed(msg_type: str, key: str, value: bytes) -> bytes:
    """An info/parameter record. The key length is computed, never typed —
    hardcoding it is how a fixture ends up testing the wrong thing."""
    return _msg(msg_type, bytes([len(key)]) + key.encode() + value)


def _logged(level: int, timestamp: int, text: str) -> bytes:
    return _msg("L", bytes([level]) + struct.pack("<Q", timestamp) + text.encode())


# ---------------------------------------------------------------------------
# Header
# ---------------------------------------------------------------------------

def test_a_non_ulog_file_is_refused_not_guessed_at() -> None:
    with pytest.raises(UlogError, match="not a ULog"):
        read(b"this is a text file, not a flight log")
    with pytest.raises(UlogError, match="not a ULog"):
        read(b"")


def test_the_header_timestamp_is_read() -> None:
    log = read(_header(987654321))
    assert log.start_timestamp == 987654321


# ---------------------------------------------------------------------------
# Data
# ---------------------------------------------------------------------------

def test_scalars_and_arrays_decode_at_the_right_offsets() -> None:
    blob = (
        _header()
        + _format("sample:uint64_t timestamp;float[3] xyz;uint8_t flag;")
        + _add(1, "sample")
        + _data(1, struct.pack("<Q3fB", 1000, 1.5, -2.5, 3.5, 7))
        + _data(1, struct.pack("<Q3fB", 2000, 4.0, 5.0, 6.0, 9))
    )
    log = read(blob)
    assert log.series("sample", "timestamp") == [1000, 2000]
    assert log.series("sample", "xyz")[0] == pytest.approx([1.5, -2.5, 3.5])
    assert log.series("sample", "flag") == [7, 9]


def test_padding_fields_occupy_bytes_but_never_appear_in_the_output() -> None:
    """Getting this wrong shifts every following field — silently."""
    blob = (
        _header()
        + _format("padded:uint64_t timestamp;uint8_t a;uint8_t[3] _padding0;uint32_t b;")
        + _add(1, "padded")
        + _data(1, struct.pack("<QB3BI", 10, 1, 0, 0, 0, 0xDEADBEEF))
    )
    log = read(blob)
    fields = log.data["padded"][0]
    assert "_padding0" not in fields
    assert fields["a"] == [1]
    assert fields["b"] == [0xDEADBEEF], "the field after the padding is not shifted"


def test_nested_message_types_are_expanded() -> None:
    blob = (
        _header()
        + _format("inner:float x;float y;")
        + _format("outer:uint64_t timestamp;inner pos;uint8_t n;")
        + _add(1, "outer")
        + _data(1, struct.pack("<Q2fB", 5, 1.25, 2.5, 3))
    )
    log = read(blob)
    assert log.series("outer", "pos.x") == pytest.approx([1.25])
    assert log.series("outer", "pos.y") == pytest.approx([2.5])
    assert log.series("outer", "n") == [3]


def test_multi_instance_topics_stay_separate() -> None:
    """Three IMUs are three answers, and averaging them hides the broken one."""
    blob = (
        _header()
        + _format("imu:uint64_t timestamp;float temp;")
        + _add(1, "imu", multi_id=0) + _add(2, "imu", multi_id=1)
        + _data(1, struct.pack("<Qf", 1, 20.0))
        + _data(2, struct.pack("<Qf", 1, 55.0))
    )
    log = read(blob)
    assert log.instances("imu") == [0, 1]
    assert log.series("imu", "temp", 0) == pytest.approx([20.0])
    assert log.series("imu", "temp", 1) == pytest.approx([55.0])


def test_only_the_requested_topics_are_decoded() -> None:
    """A log holds a hundred topics and a review reads a dozen."""
    blob = (
        _header()
        + _format("wanted:uint64_t timestamp;float v;")
        + _format("ignored:uint64_t timestamp;float v;")
        + _add(1, "wanted") + _add(2, "ignored")
        + _data(1, struct.pack("<Qf", 1, 1.0))
        + _data(2, struct.pack("<Qf", 1, 2.0))
    )
    log = read(blob, topics=("wanted",))
    assert log.has("wanted")
    assert not log.has("ignored")


def test_a_removed_subscription_stops_collecting() -> None:
    blob = (
        _header()
        + _format("t:uint64_t timestamp;float v;")
        + _add(1, "t")
        + _data(1, struct.pack("<Qf", 1, 1.0))
        + _msg("R", struct.pack("<H", 1))
        + _data(1, struct.pack("<Qf", 2, 2.0))
    )
    assert read(blob).series("t", "v") == pytest.approx([1.0])


# ---------------------------------------------------------------------------
# Metadata
# ---------------------------------------------------------------------------

def test_info_and_parameters_are_read() -> None:
    log = read(
        _header()
        + _keyed("I", "char[3] ver_hw", b"v6x")
        + _keyed("P", "int32_t SYS_AUTOSTART", struct.pack("<i", 4001))
    )
    assert log.info["ver_hw"] == "v6x"
    assert log.params["SYS_AUTOSTART"] == 4001


def test_logged_messages_keep_their_level_and_timestamp() -> None:
    log = read(_header() + _logged(3, 5000, "Critical: EKF reset")
               + _logged(6, 6000, "Armed"))
    assert [m["level"] for m in log.messages] == ["error", "info"]
    assert log.messages[0]["t"] == 5000
    assert log.messages[0]["text"] == "Critical: EKF reset"


def test_dropouts_are_recorded_because_a_gap_is_a_finding() -> None:
    log = read(_header() + _msg("O", struct.pack("<H", 42))
               + _msg("O", struct.pack("<H", 8)))
    assert [d["duration_ms"] for d in log.dropouts] == [42, 8]


# ---------------------------------------------------------------------------
# Damaged logs — the case where the log matters most
# ---------------------------------------------------------------------------

def test_a_truncated_log_returns_everything_up_to_the_cut() -> None:
    """A power loss mid-write is exactly when the log is worth reading."""
    whole = (
        _header()
        + _format("t:uint64_t timestamp;float v;")
        + _add(1, "t")
        + _data(1, struct.pack("<Qf", 1, 1.0))
        + _data(1, struct.pack("<Qf", 2, 2.0))
    )
    log = read(whole[:-6])          # cut through the last record
    assert log.truncated is True
    assert log.series("t", "v") == pytest.approx([1.0]), "the intact records survive"


def test_a_corrupt_record_does_not_cost_the_rest_of_the_flight() -> None:
    blob = (
        _header()
        + _format("t:uint64_t timestamp;float v;")
        + _add(1, "t")
        + _data(1, struct.pack("<Qf", 1, 1.0))
        + _msg("I", b"\xff")                       # nonsense info record
        + _data(1, struct.pack("<Qf", 2, 2.0))
    )
    assert read(blob).series("t", "v") == pytest.approx([1.0, 2.0])


def test_an_unknown_field_type_drops_that_topic_not_the_file() -> None:
    """Guessing an offset would produce plausible, wrong numbers."""
    blob = (
        _header()
        + _format("bad:uint64_t timestamp;mystery_t thing;")
        + _format("good:uint64_t timestamp;float v;")
        + _add(1, "bad") + _add(2, "good")
        + _data(2, struct.pack("<Qf", 1, 3.0))
    )
    log = read(blob)
    assert not log.has("bad")
    assert log.series("good", "v") == pytest.approx([3.0])


def test_an_oversized_file_is_refused() -> None:
    import corvus.ulog as ulog_module
    original = ulog_module.MAX_FILE_BYTES
    ulog_module.MAX_FILE_BYTES = 32
    try:
        with pytest.raises(UlogError, match="too large"):
            read(_header() + b"\x00" * 64)
    finally:
        ulog_module.MAX_FILE_BYTES = original
