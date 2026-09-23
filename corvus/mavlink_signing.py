"""Opt-in MAVLink 2 signing: the key file and the messages exempt from it."""
from __future__ import annotations

import os
import stat
from typing import Any

from pymavlink import mavutil

MAVLINK_SIGNING_KEY_FILE_ENV = "CORVUS_MAVLINK_SIGNING_KEY_FILE"
_SIGNING_UNSIGNED_MESSAGE_IDS = frozenset({
    mavutil.mavlink.MAVLINK_MSG_ID_RADIO_STATUS,
    mavutil.mavlink.MAVLINK_MSG_ID_ADSB_VEHICLE,
    mavutil.mavlink.MAVLINK_MSG_ID_COLLISION,
})


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
    path = (os.environ.get(MAVLINK_SIGNING_KEY_FILE_ENV) or "").strip()
    if not path:
        return None
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    fd = os.open(path, flags)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise ValueError("MAVLink signing key path is not a regular file")
        if os.name != "nt":
            if info.st_mode & 0o077:
                raise PermissionError("MAVLink signing key file must have mode 0600")
            if hasattr(os, "getuid") and info.st_uid != os.getuid():
                raise PermissionError("MAVLink signing key file must be owned by this user")
        data = os.read(fd, 129)
    finally:
        os.close(fd)
    stripped = data.strip()
    if len(data) == 32:
        key = data
    elif len(stripped) == 64:
        try:
            key = bytes.fromhex(stripped.decode("ascii"))
        except (UnicodeError, ValueError) as exc:
            raise ValueError("MAVLink signing key must be 32 raw bytes or 64 hex characters") from exc
    else:
        raise ValueError("MAVLink signing key must be 32 raw bytes or 64 hex characters")
    if not any(key):
        raise ValueError("MAVLink signing key must not be all zero")
    return key


def _allow_unsigned_signed_link(_mav: Any, msg_id: int) -> bool:
    """Allow only PX4's non-command safety telemetry on a signed link."""
    return int(msg_id) in _SIGNING_UNSIGNED_MESSAGE_IDS
