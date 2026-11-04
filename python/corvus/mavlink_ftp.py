"""MAVLink FTP, the reading half: open a file on the autopilot and download it.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge` like the other protocol
mixins; it uses the bridge's link, send lock and stop event, and receives its
replies from the bridge's receive loop through :meth:`_handle_ftp_message`.

Only reading is implemented because reading is all Corvus needs it for: the
parameter metadata both flight stacks keep on board (see
:mod:`corvus.param_metadata`). The transfer is a burst read, the same one
QGroundControl and MAVProxy use, with the gaps a lossy link leaves re-requested
from where they start. One transfer runs at a time.

The protocol itself is ``corvus_link.ftp.FtpEngine``, in Rust. It reads the
tunables below, this module's ``time`` and :class:`FtpError` from here when it
needs them.

Wire format (the 251 byte FILE_TRANSFER_PROTOCOL payload)::

    seq u16 | session u8 | opcode u8 | size u8 | req_opcode u8 |
    burst_complete u8 | padding u8 | offset u32 | data[239]
"""
from __future__ import annotations

import sys
import threading
import time  # noqa: F401 - the engine reads this module's clock
from collections.abc import Callable
from typing import Any

from corvus_link import ftp as _engine
from corvus_link.ftp import (  # noqa: F401 - the protocol's numbers, re-exported
    FTP_DATA_MAX,
    FTP_ERR_EOF,
    FTP_ERR_FAIL,
    FTP_ERR_FAIL_ERRNO,
    FTP_ERR_FILE_EXISTS,
    FTP_ERR_FILE_NOT_FOUND,
    FTP_ERR_FILE_PROTECTED,
    FTP_ERR_INVALID_DATA_SIZE,
    FTP_ERR_INVALID_SESSION,
    FTP_ERR_NO_SESSIONS,
    FTP_ERR_UNKNOWN_COMMAND,
    FTP_OP_ACK,
    FTP_OP_BURST_READ_FILE,
    FTP_OP_CALC_FILE_CRC32,
    FTP_OP_NAK,
    FTP_OP_OPEN_FILE_RO,
    FTP_OP_READ_FILE,
    FTP_OP_RESET_SESSIONS,
    FTP_OP_TERMINATE_SESSION,
    FTP_PAYLOAD_LEN,
    FtpReply,
    decode_ftp_payload,
    encode_ftp_payload,
)

# Larger than any metadata file either stack ships (PX4's compressed parameter
# metadata is around 100 kB), small enough that a vehicle announcing a huge
# file cannot make Corvus hold it in memory.
FTP_MAX_FILE_BYTES = 8 * 1024 * 1024
# Bursts started for one file before the transfer gives up. Each one resumes
# at the first gap, so a lossy link costs a round per gap, not a restart.
FTP_BURST_ROUNDS = 25
FTP_REQUEST_RETRIES = 3
FTP_REPLY_TIMEOUT_S = 1.0
FTP_REPLY_TIMEOUT_S_SLOW = 3.0


class FtpError(Exception):
    """A transfer that did not complete. ``code`` is the NAK error, or 0."""

    def __init__(self, message: str, code: int = 0) -> None:
        super().__init__(message)
        self.code = code


class FtpClientMixin:
    """Read one file from the autopilot over MAVLink FTP."""

    # Kept addressable by the names the bridge used: the tests look at them.
    _ftp_listening = property(
        lambda self: self._ftp_engine.listening,
        lambda self, value: setattr(self._ftp_engine, "listening", value),
    )
    _ftp_inbox = property(lambda self: self._ftp_engine.inbox)
    _ftp_cancel = property(lambda self: self._ftp_engine.cancel)

    def _init_ftp_state(self) -> None:
        """Set up the transfer state; called from the bridge's __init__."""
        self._ftp_engine = _engine.FtpEngine(sys.modules[__name__])
        # The one transfer slot.
        self._ftp_lock = threading.Lock()

    def abort_ftp(self) -> None:
        """Stop the running transfer at its next wait; it raises FtpError."""
        self._ftp_engine.abort()

    def _handle_ftp_message(self, msg: Any) -> None:
        """Queue a reply for the running transfer. Runs on the receive thread.

        Behind a router the vehicle also answers another station's transfer,
        so a reply addressed elsewhere is dropped.
        """
        self._ftp_engine.handle_message(self, msg)

    def ftp_read_file(
        self, path: str,
        progress: Callable[[int, int], None] | None = None,
        cancel: threading.Event | None = None,
    ) -> bytes:
        """Download *path* from the autopilot and return its bytes.

        *progress* is called with (bytes received, file size or 0). Raises
        :class:`FtpError` when the vehicle refuses, the link drops, the bridge
        stops, *cancel* is set or :meth:`abort_ftp` is called.
        """
        return self._ftp_engine.read_file(self, path, progress, cancel)

    def ftp_file_crc32(self, path: str, cancel: threading.Event | None = None) -> int:
        """The CRC32 the vehicle computes of *path*, without transferring the file.

        Only ever compared with another answer from a vehicle, never with a
        checksum computed here, so the CRC32 variant the firmware uses does not
        matter.
        """
        return self._ftp_engine.file_crc32(self, path, cancel)
