"""A WebSocket (RFC 6455) on one of the HTTP server's own connections.

Why the terminals need one
--------------------------
Chromium holds at most six HTTP/1.1 connections per host, and the pool is
shared by every page of the profile: the app's window and every window taken
out of it. Two are taken for good by the telemetry and event streams. A
terminal used to take a third for its output stream (SSE), and each keystroke
a request of its own on top. With four terminals open the six were gone, and
from then on every request queued behind streams that never end: typing,
map tiles and camera frames all stopped, and nothing said why.

A WebSocket is not in that pool (Chromium counts them apart, with a limit in
the hundreds), and carries the input as well. So a terminal is one WebSocket
now, output one way and keystrokes and sizes the other, and the SSE stream
with its POSTs stays as what a client without WebSockets uses.

What is here
------------
The handshake (:func:`handshake_problem`, :func:`accept_key`), the framing
(:func:`encode_frame`, :class:`FrameReader`) and :class:`WebSocket`, which
runs them over a socket: text messages both ways, ping and pong, and the
closing handshake. Only what a browser sends is accepted: masked frames,
text, and messages no larger than :data:`MAX_MESSAGE`. No extensions and no
subprotocols are offered, so none can be in use.

stdlib only.
"""
from __future__ import annotations

import base64
import binascii
import hashlib
import os
import select
import socket
import struct
import threading
from collections.abc import Mapping
from typing import Any

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

OP_CONTINUATION = 0x0
OP_TEXT = 0x1
OP_BINARY = 0x2
OP_CLOSE = 0x8
OP_PING = 0x9
OP_PONG = 0xA

CLOSE_NORMAL = 1000
CLOSE_GOING_AWAY = 1001
CLOSE_PROTOCOL_ERROR = 1002
CLOSE_UNSUPPORTED = 1003
CLOSE_INVALID_DATA = 1007
CLOSE_TOO_BIG = 1009

# A terminal's messages are keystrokes and a paste now and then. A megabyte is
# far past any paste, and small enough that a client cannot make this process
# hold an unbounded message.
MAX_MESSAGE = 1024 * 1024


class ProtocolError(Exception):
    """The peer broke the protocol; *code* is the close code that says how."""

    def __init__(self, code: int, reason: str) -> None:
        super().__init__(reason)
        self.code = code
        self.reason = reason


class Closed(Exception):
    """The connection is over: closed by either side, or gone."""


def _tokens(value: Any) -> set[str]:
    return {t.strip().lower() for t in str(value or "").split(",") if t.strip()}


def handshake_problem(headers: Mapping[str, Any] | Any) -> str:
    """Why *headers* are not a WebSocket opening handshake, or ``""``."""
    get = headers.get
    if "websocket" not in _tokens(get("Upgrade", "")):
        return "not a WebSocket upgrade"
    if "upgrade" not in _tokens(get("Connection", "")):
        return "not a WebSocket upgrade"
    if str(get("Sec-WebSocket-Version", "")).strip() != "13":
        return "unsupported WebSocket version"
    key = str(get("Sec-WebSocket-Key", "")).strip()
    try:
        if len(base64.b64decode(key, validate=True)) != 16:
            return "bad Sec-WebSocket-Key"
    except (binascii.Error, ValueError):
        return "bad Sec-WebSocket-Key"
    return ""


def accept_key(key: str) -> str:
    """The ``Sec-WebSocket-Accept`` that answers ``Sec-WebSocket-Key`` *key*."""
    digest = hashlib.sha1((key.strip() + GUID).encode("ascii")).digest()
    return base64.b64encode(digest).decode("ascii")


def handshake_response(key: str) -> bytes:
    """The whole ``101`` answer, status line included.

    Written by hand rather than through ``send_response``: the handler answers
    as HTTP/1.0, and an upgrade is HTTP/1.1 or nothing.
    """
    return (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept_key(key)}\r\n"
        "\r\n"
    ).encode("ascii")


def encode_frame(opcode: int, payload: bytes = b"", fin: bool = True) -> bytes:
    """One unmasked frame, as a server sends it."""
    head = (0x80 if fin else 0) | (opcode & 0x0F)
    n = len(payload)
    if n < 126:
        header = struct.pack("!BB", head, n)
    elif n < 1 << 16:
        header = struct.pack("!BBH", head, 126, n)
    else:
        header = struct.pack("!BBQ", head, 127, n)
    return header + payload


def mask_frame(opcode: int, payload: bytes, mask: bytes | None = None, fin: bool = True) -> bytes:
    """One masked frame, as a client sends it. For the tests."""
    mask = os.urandom(4) if mask is None else mask
    head = (0x80 if fin else 0) | (opcode & 0x0F)
    n = len(payload)
    if n < 126:
        header = struct.pack("!BB", head, 0x80 | n)
    elif n < 1 << 16:
        header = struct.pack("!BBH", head, 0x80 | 126, n)
    else:
        header = struct.pack("!BBQ", head, 0x80 | 127, n)
    return header + mask + _unmask(payload, mask)


def _unmask(data: bytes, mask: bytes) -> bytes:
    if not data:
        return b""
    whole = (mask * (len(data) // 4 + 1))[:len(data)]
    return (int.from_bytes(data, "big") ^ int.from_bytes(whole, "big")).to_bytes(len(data), "big")


class FrameReader:
    """Turns the bytes a client sends into whole messages.

    :meth:`feed` returns ``(opcode, payload)`` for every message it
    completed: a text or binary message with its fragments joined, or a
    control frame. Raises :class:`ProtocolError` for anything RFC 6455 does
    not allow a client to send.
    """

    def __init__(self, max_message: int = MAX_MESSAGE) -> None:
        self._buf = bytearray()
        self._max = max_message
        self._parts: list[bytes] = []
        self._size = 0
        self._opcode: int | None = None

    def feed(self, data: bytes) -> list[tuple[int, bytes]]:
        self._buf += data
        out: list[tuple[int, bytes]] = []
        while True:
            frame = self._frame()
            if frame is None:
                return out
            fin, opcode, payload = frame
            if opcode >= 0x8:
                if not fin or len(payload) > 125:
                    raise ProtocolError(CLOSE_PROTOCOL_ERROR, "bad control frame")
                out.append((opcode, payload))
                continue
            if opcode == OP_CONTINUATION:
                if self._opcode is None:
                    raise ProtocolError(CLOSE_PROTOCOL_ERROR, "continuation without a message")
            elif opcode in (OP_TEXT, OP_BINARY):
                if self._opcode is not None:
                    raise ProtocolError(CLOSE_PROTOCOL_ERROR, "new message inside a fragmented one")
                self._opcode = opcode
            else:
                raise ProtocolError(CLOSE_PROTOCOL_ERROR, f"unknown opcode {opcode}")
            self._size += len(payload)
            if self._size > self._max:
                raise ProtocolError(CLOSE_TOO_BIG, "message too big")
            self._parts.append(payload)
            if fin:
                out.append((self._opcode, b"".join(self._parts)))
                self._parts, self._size, self._opcode = [], 0, None

    def _frame(self) -> tuple[bool, int, bytes] | None:
        buf = self._buf
        if len(buf) < 2:
            return None
        b0, b1 = buf[0], buf[1]
        if b0 & 0x70:
            raise ProtocolError(CLOSE_PROTOCOL_ERROR, "no extension was agreed on")
        if not b1 & 0x80:
            raise ProtocolError(CLOSE_PROTOCOL_ERROR, "a client frame must be masked")
        n = b1 & 0x7F
        pos = 2
        if n == 126:
            if len(buf) < 4:
                return None
            n = struct.unpack_from("!H", buf, 2)[0]
            pos = 4
        elif n == 127:
            if len(buf) < 10:
                return None
            n = struct.unpack_from("!Q", buf, 2)[0]
            pos = 10
        if n > self._max:
            raise ProtocolError(CLOSE_TOO_BIG, "message too big")
        if len(buf) < pos + 4 + n:
            return None
        mask = bytes(buf[pos:pos + 4])
        payload = _unmask(bytes(buf[pos + 4:pos + 4 + n]), mask)
        del buf[:pos + 4 + n]
        return bool(b0 & 0x80), b0 & 0x0F, payload


class WebSocket:
    """A server-side WebSocket over a connected socket, after the handshake.

    :meth:`send_text` may be called from any thread; :meth:`receive` from one
    thread only. :meth:`close` is safe to call more than once and from either.
    """

    def __init__(self, sock: socket.socket, max_message: int = MAX_MESSAGE) -> None:
        self._sock = sock
        self._reader = FrameReader(max_message)
        self._pending: list[tuple[int, bytes]] = []
        self._send_lock = threading.Lock()
        self._closed = threading.Event()

    @property
    def closed(self) -> bool:
        return self._closed.is_set()

    def _send(self, frame: bytes) -> None:
        if self._closed.is_set():
            raise Closed("closed")
        try:
            with self._send_lock:
                self._sock.sendall(frame)
        except OSError as exc:
            self._closed.set()
            raise Closed(str(exc)) from exc

    def send_text(self, text: str) -> None:
        self._send(encode_frame(OP_TEXT, text.encode("utf-8")))

    def ping(self, payload: bytes = b"") -> None:
        self._send(encode_frame(OP_PING, payload[:125]))

    def close(self, code: int = CLOSE_NORMAL, reason: str = "") -> None:
        """Say goodbye, if the connection is still there to say it on."""
        if self._closed.is_set():
            return
        body = struct.pack("!H", code) + reason.encode("utf-8")[:123]
        try:
            with self._send_lock:
                self._sock.sendall(encode_frame(OP_CLOSE, body))
        except OSError:
            pass
        self._closed.set()

    def receive(self, timeout: float) -> str | None:
        """The next text message, or None when none came within *timeout*.

        Answers pings on its own. Raises :class:`Closed` once the peer closed
        or went away, after answering its close; a protocol error is answered
        with the close code it deserves and raised as :class:`Closed` too.
        """
        while True:
            if self._pending:
                opcode, payload = self._pending.pop(0)
                text = self._handle(opcode, payload)
                if text is not None:
                    return text
                continue
            if self._closed.is_set():
                raise Closed("closed")
            try:
                ready, _, _ = select.select([self._sock], [], [], max(0.0, timeout))
            except (OSError, ValueError) as exc:
                self._closed.set()
                raise Closed(str(exc)) from exc
            if not ready:
                return None
            try:
                data = self._sock.recv(65536)
            except OSError as exc:
                self._closed.set()
                raise Closed(str(exc)) from exc
            if not data:
                self._closed.set()
                raise Closed("peer went away")
            try:
                self._pending.extend(self._reader.feed(data))
            except ProtocolError as exc:
                self.close(exc.code, exc.reason)
                raise Closed(exc.reason) from exc

    def _handle(self, opcode: int, payload: bytes) -> str | None:
        if opcode == OP_TEXT:
            try:
                return payload.decode("utf-8")
            except UnicodeDecodeError as exc:
                self.close(CLOSE_INVALID_DATA, "text is not UTF-8")
                raise Closed("text is not UTF-8") from exc
        if opcode == OP_PING:
            self._send(encode_frame(OP_PONG, payload))
            return None
        if opcode == OP_PONG:
            return None
        if opcode == OP_CLOSE:
            self.close(CLOSE_NORMAL)
            raise Closed("closed by the peer")
        self.close(CLOSE_UNSUPPORTED, "text only")
        raise Closed("binary message")
