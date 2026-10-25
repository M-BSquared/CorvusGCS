"""The parameter value codec as Corvus had it in Python, before corvus_link.

Kept unchanged as the reference tests/test_param_engine_parity.py holds the
Rust codec to. Not imported by the app.
"""
from __future__ import annotations

import math
import struct
from typing import Any

PARAM_ENCODING_BYTEWISE = "bytewise"
PARAM_ENCODING_C_CAST = "c_cast"

# MAV_PARAM_TYPE -> (struct code, low, high) for the integer types. REAL32 and
# the 64-bit types are not here: the first needs no conversion and the others
# do not fit the field at all.
_PARAM_INT_TYPES: dict[int, tuple[str, int, int]] = {
    1: ("<B", 0, 0xFF),
    2: ("<b", -0x80, 0x7F),
    3: ("<H", 0, 0xFFFF),
    4: ("<h", -0x8000, 0x7FFF),
    5: ("<I", 0, 0xFFFFFFFF),
    6: ("<i", -0x80000000, 0x7FFFFFFF),
}


def param_type_is_integer(param_type: Any) -> bool:
    """Is *param_type* one of the MAV_PARAM_TYPE integer types?"""
    try:
        return int(param_type) in _PARAM_INT_TYPES
    except (TypeError, ValueError):
        return False


def decode_param_value(
    value: float, param_type: int, encoding: str, raw: bytes | None = None,
) -> float:
    """The parameter's real value from the float32 field it travelled in.

    *raw* is the field's four bytes off the wire when the caller has them. They
    are preferred over *value* because a float that is a NaN bit pattern does
    not survive the trip through a Python float intact, and on PX4 every
    integer from -8388607 to -4194305 is one of those.
    """
    spec = _PARAM_INT_TYPES.get(int(param_type))
    if spec is None or encoding != PARAM_ENCODING_BYTEWISE:
        return float(value)
    if raw is None or len(raw) < 4:
        raw = struct.pack("<f", float(value))
    code = spec[0]
    return float(struct.unpack_from(code, raw[:struct.calcsize(code)])[0])


def encode_param_value(value: float, param_type: int, encoding: str) -> float:
    """The float32 to put in PARAM_SET so the vehicle stores *value*.

    Raises ValueError for a value the parameter cannot hold (a fraction or an
    out of range number for an integer), and for the few integers whose byte
    pattern is a signalling NaN: corvus_link packs the field through a Python
    float, which quietens that NaN and would change the number on the way out.
    """
    spec = _PARAM_INT_TYPES.get(int(param_type))
    if spec is None:
        return float(value)
    number = float(value)
    if not math.isfinite(number) or number != int(number):
        raise ValueError("this parameter takes whole numbers only")
    code, low, high = spec
    whole = int(number)
    if not low <= whole <= high:
        raise ValueError(f"this parameter takes values from {low} to {high}")
    if encoding != PARAM_ENCODING_BYTEWISE:
        return number
    packed = struct.pack(code, whole).ljust(4, b"\x00")
    bits = struct.unpack("<I", packed)[0]
    if (bits >> 23) & 0xFF == 0xFF and bits & 0x7FFFFF and not bits & 0x400000:
        raise ValueError("this value cannot be sent exactly over MAVLink")
    return struct.unpack("<f", packed)[0]


def param_values_match(param_type: int, got: float, wanted: float) -> bool:
    """Does the vehicle hold *wanted*? Exact for integers, float32 exact otherwise.

    Tighter than the echo check in ``set_param`` on purpose: this answers "was
    it applied", and a float parameter holds exactly the float32 it was sent,
    so anything beyond float32 rounding is a different value.
    """
    if not (math.isfinite(got) and math.isfinite(wanted)):
        return False
    if param_type_is_integer(param_type):
        return int(round(got)) == int(round(wanted))
    try:
        as_sent = struct.unpack("<f", struct.pack("<f", wanted))[0]
    except OverflowError:
        return False
    return abs(got - as_sent) <= 1e-6 * max(1.0, abs(as_sent))

