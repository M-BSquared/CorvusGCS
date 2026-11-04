"""Opt-in MAVLink 2 signing: the key file and the messages exempt from it.

Both are ``corvus_link.bridge`` functions, in Rust; the key file is still read
through Python's ``os``, so its flags and checks are the same.
"""
from __future__ import annotations

import sys
from typing import Any

from corvus_link import bridge as _bridge

MAVLINK_SIGNING_KEY_FILE_ENV = "CORVUS_MAVLINK_SIGNING_KEY_FILE"
# RADIO_STATUS, ADSB_VEHICLE and COLLISION; see corvus_link.bridge.
_SIGNING_UNSIGNED_MESSAGE_IDS: frozenset[int] = _bridge.SIGNING_UNSIGNED_MESSAGE_IDS


def _load_signing_key() -> bytes | None:
    """Load an opt-in MAVLink 2 signing key from an owner-only file.

    O_BINARY is not optional on Windows, where a descriptor opened without it
    is a TEXT-mode descriptor: reads stop at the first 0x1A (DOS end-of-file)
    and CRLF pairs collapse to LF. A signing key is 32 bytes of entropy, so
    roughly one key in eight contains an 0x1A somewhere — and what the
    operator then sees is not a corrupted key, it is "MAVLink signing key must
    be 32 raw bytes or 64 hex characters" about a file that is exactly 32
    bytes long. The flag is guarded because only Windows defines it.
    """
    return _bridge.load_signing_key(sys.modules[__name__])


def _allow_unsigned_signed_link(_mav: Any, msg_id: int) -> bool:
    """Allow only PX4's non-command safety telemetry on a signed link."""
    return _bridge.allow_unsigned_signed_link(_mav, msg_id)
