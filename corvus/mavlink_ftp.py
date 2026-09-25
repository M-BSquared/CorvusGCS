"""MAVLink FTP, the reading half: open a file on the autopilot and download it.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge` like the other protocol
mixins; it uses the bridge's link, send lock and stop event, and receives its
replies from the bridge's receive loop through :meth:`_handle_ftp_message`.

Only reading is implemented because reading is all Corvus needs it for: the
parameter metadata both flight stacks keep on board (see
:mod:`corvus.param_metadata`). The transfer is a burst read, the same one
QGroundControl and MAVProxy use, with the gaps a lossy link leaves re-requested
from where they start. One transfer runs at a time.

Wire format (the 251 byte FILE_TRANSFER_PROTOCOL payload)::

    seq u16 | session u8 | opcode u8 | size u8 | req_opcode u8 |
    burst_complete u8 | padding u8 | offset u32 | data[239]
"""
from __future__ import annotations

import contextlib
import logging
import struct
import threading
import time
from collections import deque
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger("corvus.mavlink")

FTP_OP_TERMINATE_SESSION = 1
FTP_OP_RESET_SESSIONS = 2
FTP_OP_OPEN_FILE_RO = 4
FTP_OP_READ_FILE = 5
FTP_OP_CALC_FILE_CRC32 = 14
FTP_OP_BURST_READ_FILE = 15
FTP_OP_ACK = 128
FTP_OP_NAK = 129

FTP_ERR_FAIL = 1
FTP_ERR_FAIL_ERRNO = 2
FTP_ERR_INVALID_DATA_SIZE = 3
FTP_ERR_INVALID_SESSION = 4
FTP_ERR_NO_SESSIONS = 5
FTP_ERR_EOF = 6
FTP_ERR_UNKNOWN_COMMAND = 7
FTP_ERR_FILE_EXISTS = 8
FTP_ERR_FILE_PROTECTED = 9
FTP_ERR_FILE_NOT_FOUND = 10

FTP_PAYLOAD_LEN = 251
_FTP_HEADER = struct.Struct("<HBBBBBBI")
FTP_DATA_MAX = FTP_PAYLOAD_LEN - _FTP_HEADER.size

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

_NAK_TEXT = {
    FTP_ERR_FAIL: "the vehicle reported a failure",
    FTP_ERR_FAIL_ERRNO: "the vehicle could not read the file",
    FTP_ERR_INVALID_DATA_SIZE: "the vehicle rejected the request size",
    FTP_ERR_INVALID_SESSION: "the file session was lost",
    FTP_ERR_NO_SESSIONS: "the vehicle has no free file session",
    FTP_ERR_EOF: "unexpected end of file",
    FTP_ERR_UNKNOWN_COMMAND: "the vehicle does not support MAVLink FTP",
    FTP_ERR_FILE_EXISTS: "the file already exists",
    FTP_ERR_FILE_PROTECTED: "the file is protected",
    FTP_ERR_FILE_NOT_FOUND: "the file does not exist on the vehicle",
}


class FtpError(Exception):
    """A transfer that did not complete. ``code`` is the NAK error, or 0."""

    def __init__(self, message: str, code: int = 0) -> None:
        super().__init__(message)
        self.code = code


@dataclass
class FtpReply:
    seq: int
    session: int
    opcode: int
    size: int
    req_opcode: int
    burst_complete: bool
    offset: int
    data: bytes

    @property
    def error(self) -> int:
        """The NAK error code, or 0 for an ACK."""
        if self.opcode != FTP_OP_NAK:
            return 0
        return self.data[0] if self.size and self.data else FTP_ERR_FAIL


def encode_ftp_payload(
    seq: int, session: int, opcode: int, *,
    offset: int = 0, data: bytes = b"", size: int | None = None,
) -> list[int]:
    """Build the 251 byte payload list pymavlink's sender expects."""
    body = bytes(data[:FTP_DATA_MAX])
    length = len(body) if size is None else size
    header = _FTP_HEADER.pack(
        seq & 0xFFFF, session & 0xFF, opcode & 0xFF, length & 0xFF, 0, 0, 0,
        offset & 0xFFFFFFFF,
    )
    return list((header + body).ljust(FTP_PAYLOAD_LEN, b"\x00"))


def decode_ftp_payload(payload: Any) -> FtpReply | None:
    """Decode a FILE_TRANSFER_PROTOCOL payload, or None if it is malformed."""
    try:
        raw = bytes(payload)
    except (TypeError, ValueError):
        return None
    if len(raw) < _FTP_HEADER.size:
        return None
    seq, session, opcode, size, req_opcode, burst_complete, _pad, offset = \
        _FTP_HEADER.unpack_from(raw)
    size = min(size, FTP_DATA_MAX, len(raw) - _FTP_HEADER.size)
    data = raw[_FTP_HEADER.size:_FTP_HEADER.size + size]
    return FtpReply(seq, session, opcode, size, req_opcode, bool(burst_complete), offset, data)


def _nak_message(reply: FtpReply) -> str:
    return _NAK_TEXT.get(reply.error, f"the vehicle refused the request (error {reply.error})")


def _first_gap(chunks: dict[int, bytes]) -> int:
    """The first byte offset no received chunk covers."""
    pos = 0
    for offset in sorted(chunks):
        if offset > pos:
            return pos
        pos = max(pos, offset + len(chunks[offset]))
    return pos


class FtpClientMixin:
    """Read one file from the autopilot over MAVLink FTP."""

    def _init_ftp_state(self) -> None:
        """Set up the transfer state; called from the bridge's __init__."""
        self._ftp_lock = threading.Lock()
        self._ftp_cond = threading.Condition()
        self._ftp_inbox: deque[FtpReply] = deque(maxlen=1024)
        self._ftp_listening = False
        self._ftp_seq = 0
        # The running transfer's cancel event, or None between transfers.
        self._ftp_cancel: threading.Event | None = None

    def abort_ftp(self) -> None:
        """Stop the running transfer at its next wait; it raises FtpError."""
        cancel = self._ftp_cancel
        if cancel is not None:
            cancel.set()
        with self._ftp_cond:
            self._ftp_cond.notify_all()

    def _handle_ftp_message(self, msg: Any) -> None:
        """Queue a reply for the running transfer. Runs on the receive thread."""
        if not self._ftp_listening:
            return
        conn = self._conn
        if conn is None:
            return
        # Behind a router the vehicle also answers another station's transfer.
        for field, ours in (("target_system", "source_system"),
                            ("target_component", "source_component")):
            target = getattr(msg, field, 0)
            expected = getattr(conn, ours, 0)
            if target and expected and target != expected:
                return
        reply = decode_ftp_payload(getattr(msg, "payload", b""))
        if reply is None or reply.opcode not in (FTP_OP_ACK, FTP_OP_NAK):
            return
        with self._ftp_cond:
            if not self._ftp_listening:
                return
            self._ftp_inbox.append(reply)
            self._ftp_cond.notify_all()

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
        with self._ftp_exchange(cancel):
            session, size = self._ftp_open(path)
            try:
                return self._ftp_burst_read(session, size, progress)
            finally:
                self._ftp_terminate(session)

    def ftp_file_crc32(self, path: str, cancel: threading.Event | None = None) -> int:
        """The CRC32 the vehicle computes of *path*, without transferring the file.

        One request and one answer, so it costs a round trip where the file
        itself costs its whole size. The value is only ever compared with
        another answer from a vehicle, never with a checksum computed here,
        so the variant of CRC32 the firmware uses does not matter.
        """
        encoded = path.encode("utf-8")
        if len(encoded) >= FTP_DATA_MAX:
            raise FtpError("path too long")
        with self._ftp_exchange(cancel):
            reply = self._ftp_request(FTP_OP_CALC_FILE_CRC32, data=encoded)
        if reply.opcode == FTP_OP_NAK:
            raise FtpError(_nak_message(reply), reply.error)
        if reply.size < 4:
            raise FtpError("the vehicle sent no checksum")
        return struct.unpack_from("<I", reply.data)[0]

    @contextlib.contextmanager
    def _ftp_exchange(self, cancel: threading.Event | None) -> Iterator[None]:
        """Hold the one transfer slot and listen for replies while inside."""
        if not self._ftp_lock.acquire(blocking=False):
            raise FtpError("another file transfer is in progress")
        try:
            self._ftp_cancel = cancel or threading.Event()
            with self._ftp_cond:
                self._ftp_inbox.clear()
                self._ftp_listening = True
            yield
        finally:
            with self._ftp_cond:
                self._ftp_listening = False
                self._ftp_inbox.clear()
            self._ftp_cancel = None
            self._ftp_lock.release()

    # ------------------------------------------------------------------

    def _ftp_timeout(self) -> float:
        return FTP_REPLY_TIMEOUT_S_SLOW if self._is_slow_link() else FTP_REPLY_TIMEOUT_S

    def _ftp_check_alive(self) -> None:
        cancel = self._ftp_cancel
        if (cancel is not None and cancel.is_set()) or self._stop_event.is_set():
            raise FtpError("cancelled")
        if self._conn is None:
            raise FtpError("not connected")

    def _ftp_send(
        self, opcode: int, *, seq: int | None = None, session: int = 0,
        offset: int = 0, data: bytes = b"", size: int | None = None,
    ) -> int:
        """Send one request and return its sequence number."""
        self._ftp_check_alive()
        if seq is None:
            self._ftp_seq = (self._ftp_seq + 1) & 0xFFFF
            seq = self._ftp_seq
        payload = encode_ftp_payload(seq, session, opcode, offset=offset, data=data, size=size)
        conn = self._conn
        try:
            with self._send_lock:
                if conn is None or conn is not self._conn:
                    raise FtpError("not connected")
                conn.mav.file_transfer_protocol_send(
                    0, self._target_system, self._target_component, payload,
                )
        except FtpError:
            raise
        except Exception as exc:  # noqa: BLE001 - a send failure ends the transfer
            raise FtpError(f"could not send to the vehicle: {exc}") from exc
        return seq

    def _ftp_next_reply(self, timeout: float) -> FtpReply | None:
        """The next queued reply, or None once *timeout* passes without one."""
        deadline = time.monotonic() + timeout
        with self._ftp_cond:
            while True:
                self._ftp_check_alive()
                if self._ftp_inbox:
                    return self._ftp_inbox.popleft()
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return None
                self._ftp_cond.wait(min(remaining, 0.1))

    def _ftp_request(
        self, opcode: int, *, session: int = 0, offset: int = 0,
        data: bytes = b"", size: int | None = None,
        retries: int = FTP_REQUEST_RETRIES,
    ) -> FtpReply:
        """Send a request and wait for its answer, resending on a timeout.

        A resend keeps the sequence number, which is how both stacks tell a
        lost reply from a new request: they answer it with the reply they
        already sent rather than running the operation twice.
        """
        seq: int | None = None
        for _attempt in range(max(1, retries)):
            seq = self._ftp_send(opcode, seq=seq, session=session, offset=offset,
                                 data=data, size=size)
            deadline = time.monotonic() + self._ftp_timeout()
            while True:
                remaining = deadline - time.monotonic()
                reply = self._ftp_next_reply(max(0.0, remaining))
                if reply is None:
                    break
                if reply.req_opcode == opcode and reply.seq == (seq + 1) & 0xFFFF:
                    return reply
        raise FtpError("no answer from the vehicle")

    def _ftp_open(self, path: str) -> tuple[int, int]:
        """Open *path* read-only; returns (session, size or 0 if unknown)."""
        encoded = path.encode("utf-8")
        if len(encoded) >= FTP_DATA_MAX:
            raise FtpError("path too long")
        reply = self._ftp_request(FTP_OP_OPEN_FILE_RO, data=encoded)
        if reply.error == FTP_ERR_NO_SESSIONS:
            # A session left open by a transfer that never finished, most
            # likely a previous run of this station killed mid-download. PX4
            # keeps one session and does not time it out, so without the reset
            # the vehicle refuses every transfer until it reboots.
            logger.info("FTP: no free session on the vehicle, resetting sessions")
            try:
                self._ftp_request(FTP_OP_RESET_SESSIONS)
            except FtpError:
                pass
            reply = self._ftp_request(FTP_OP_OPEN_FILE_RO, data=encoded)
        if reply.opcode == FTP_OP_NAK:
            raise FtpError(_nak_message(reply), reply.error)
        size = struct.unpack_from("<I", reply.data)[0] if reply.size >= 4 else 0
        if size > FTP_MAX_FILE_BYTES:
            self._ftp_terminate(reply.session)
            raise FtpError("the file is too large")
        return reply.session, size

    def _ftp_terminate(self, session: int) -> None:
        """Close *session*; best effort, a vehicle that misses it frees it on reset."""
        try:
            self._ftp_request(FTP_OP_TERMINATE_SESSION, session=session, retries=1)
        except FtpError:
            pass

    def _ftp_burst_read(
        self, session: int, size: int,
        progress: Callable[[int, int], None] | None,
    ) -> bytes:
        """Read the open file in bursts, resuming each one at the first gap."""
        chunks: dict[int, bytes] = {}
        received = 0
        end: int | None = size if size > 0 else None
        idle = self._ftp_timeout()
        for burst in range(FTP_BURST_ROUNDS + 1):
            start = _first_gap(chunks)
            if end is not None and start >= end:
                break
            if burst == FTP_BURST_ROUNDS:
                raise FtpError("the transfer did not complete")
            self._ftp_send(FTP_OP_BURST_READ_FILE, session=session, offset=start,
                           size=FTP_DATA_MAX)
            while True:
                reply = self._ftp_next_reply(idle)
                if reply is None:
                    break   # the rest of this burst was lost; the next round resumes it
                if reply.req_opcode != FTP_OP_BURST_READ_FILE:
                    continue
                if reply.opcode == FTP_OP_NAK:
                    if reply.error != FTP_ERR_EOF:
                        raise FtpError(_nak_message(reply), reply.error)
                    if end is None:
                        top = max((o + len(d) for o, d in chunks.items()), default=0)
                        end = max(reply.offset, top)
                    break
                if reply.size:
                    if reply.offset + reply.size > FTP_MAX_FILE_BYTES:
                        raise FtpError("the file is too large")
                    if reply.offset not in chunks:
                        received += reply.size
                    chunks[reply.offset] = reply.data
                    if progress is not None:
                        try:
                            progress(received, end or 0)
                        except Exception:  # noqa: BLE001 - a progress hook must not end the transfer
                            logger.debug("FTP progress callback raised", exc_info=True)
                if end is not None and received >= end and _first_gap(chunks) >= end:
                    break
                if reply.burst_complete:
                    break
        total = end or 0
        out = bytearray(total)
        for offset, data in chunks.items():
            if offset < total:
                piece = data[:total - offset]
                out[offset:offset + len(piece)] = piece
        return bytes(out)
