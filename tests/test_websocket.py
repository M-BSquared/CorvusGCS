"""The terminal's WebSocket: the protocol, and ``/api/ssh/ws`` on a real server.

The route exists because every terminal on SSE held one of the six
connections Chromium allows per host, shared by every window of the app;
four terminals and nothing else loaded (see corvus/websocket.py). What is
checked here is what the browser relies on: the handshake it validates, the
frames it sends (masked, maybe fragmented, pings), and that input, resizes,
the end of a session and the end of the server all arrive as they do over
``/api/ssh/stream`` and its POSTs.
"""
from __future__ import annotations

import base64
import json
import os
import socket
import struct
import threading
import time
from typing import Any

import pytest

from corvus import websocket as ws
from corvus.websocket import (
    OP_CLOSE, OP_PING, OP_PONG, OP_TEXT, FrameReader, ProtocolError, encode_frame, mask_frame,
)

# ---------------------------------------------------------------------------
# The protocol
# ---------------------------------------------------------------------------


def test_the_accept_key_is_the_one_rfc_6455_gives():
    assert ws.accept_key("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="


def _headers(**overrides: str) -> dict[str, str]:
    base = {
        "Upgrade": "websocket", "Connection": "keep-alive, Upgrade",
        "Sec-WebSocket-Version": "13", "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
    }
    base.update(overrides)
    return base


def test_a_browsers_opening_handshake_is_accepted():
    assert ws.handshake_problem(_headers()) == ""
    assert ws.handshake_problem(_headers(Upgrade="WebSocket", Connection="Upgrade")) == ""


@pytest.mark.parametrize("override", [
    {"Upgrade": "h2c"}, {"Connection": "keep-alive"}, {"Sec-WebSocket-Version": "8"},
    {"Sec-WebSocket-Key": "short"}, {"Sec-WebSocket-Key": "not base64 at all!!"},
])
def test_anything_else_is_not(override):
    assert ws.handshake_problem(_headers(**override)) != ""


def test_a_server_frame_carries_its_length_in_the_form_the_size_needs():
    assert encode_frame(OP_TEXT, b"x" * 125)[:2] == bytes([0x81, 125])
    assert encode_frame(OP_TEXT, b"x" * 126)[:4] == bytes([0x81, 126]) + struct.pack("!H", 126)
    big = encode_frame(OP_TEXT, b"x" * 70000)
    assert big[:2] == bytes([0x81, 127]) and struct.unpack("!Q", big[2:10])[0] == 70000


def test_masked_and_fragmented_client_messages_are_joined():
    reader = FrameReader()
    data = (mask_frame(OP_TEXT, b"hel", fin=False)
            + mask_frame(OP_PING, b"p")
            + mask_frame(0, b"lo", fin=False)
            + mask_frame(0, "ä".encode(), fin=True))
    out: list[tuple[int, bytes]] = []
    for i in range(len(data)):              # one byte at a time: TCP may cut anywhere
        out += reader.feed(data[i:i + 1])
    assert out == [(OP_PING, b"p"), (OP_TEXT, "helloä".encode())]


def test_a_long_message_is_read_whole():
    reader = FrameReader()
    payload = os.urandom(70000)
    assert reader.feed(mask_frame(2, payload)) == [(2, payload)]


@pytest.mark.parametrize("frame,code", [
    (encode_frame(OP_TEXT, b"unmasked"), ws.CLOSE_PROTOCOL_ERROR),
    (bytes([0xC1]) + mask_frame(OP_TEXT, b"x")[1:], ws.CLOSE_PROTOCOL_ERROR),   # RSV1 set
    (mask_frame(0, b"x"), ws.CLOSE_PROTOCOL_ERROR),                              # stray continuation
    (mask_frame(OP_PING, b"x", fin=False), ws.CLOSE_PROTOCOL_ERROR),
    (mask_frame(0x3, b"x"), ws.CLOSE_PROTOCOL_ERROR),
])
def test_what_a_client_may_not_send_is_refused(frame, code):
    with pytest.raises(ProtocolError) as err:
        FrameReader().feed(frame)
    assert err.value.code == code


def test_a_message_past_the_limit_is_refused_before_it_is_buffered():
    reader = FrameReader(max_message=1000)
    header = bytes([0x81, 0x80 | 127]) + struct.pack("!Q", 10**12)
    with pytest.raises(ProtocolError) as err:
        reader.feed(header)
    assert err.value.code == ws.CLOSE_TOO_BIG


# ---------------------------------------------------------------------------
# /api/ssh/ws on a real server
# ---------------------------------------------------------------------------


class FakeSession:
    def __init__(self) -> None:
        self.connected = True
        self.subs: list[Any] = []
        self.lock = threading.Lock()

    def add_sub(self, fn, replay: bool = True) -> None:
        with self.lock:
            fn("banner\r\n$ ")
            self.subs.append(fn)

    def remove_sub(self, fn) -> None:
        with self.lock:
            if fn in self.subs:
                self.subs.remove(fn)

    def emit(self, text: str) -> None:
        with self.lock:
            for fn in list(self.subs):
                fn(text)


class FakeBridge:
    def __init__(self) -> None:
        self.sessions = {"schwalby/one": FakeSession()}
        self.sent: list[tuple[str, str]] = []
        self.resized: list[tuple[str, int, int]] = []

    def get_session(self, name: str):
        return self.sessions.get(name)

    def send(self, name: str, data: str) -> bool:
        self.sent.append((name, data))
        return True

    def resize(self, name: str, cols: int, rows: int) -> bool:
        self.resized.append((name, cols, rows))
        return True


@pytest.fixture
def live():
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler, CorvusServer
    saved = CorvusHandler.ssh
    bridge = FakeBridge()
    CorvusHandler.ssh = bridge
    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(target=server.serve_forever, name="corvus-test-ws", daemon=True)
    thread.start()
    try:
        yield server, bridge
    finally:
        server.stopping.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        CorvusHandler.ssh = saved


class Client:
    """Just enough of a browser's WebSocket to talk to the route."""

    def __init__(self, port: int, name: str = "schwalby/one", origin: str | None = None) -> None:
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=5)
        key = base64.b64encode(os.urandom(16)).decode()
        origin = f"http://127.0.0.1:{port}" if origin is None else origin
        request = (
            f"GET /api/ssh/ws?name={name.replace('/', '%2F')} HTTP/1.1\r\n"
            f"Host: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n"
            + (f"Origin: {origin}\r\n" if origin else "") + "\r\n"
        )
        self.sock.sendall(request.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(1)
            if not chunk:
                break
            head += chunk
        self.status_line = head.split(b"\r\n", 1)[0].decode()
        self.head = head.decode("latin-1")
        self.key = key
        self.buf = b""

    def _read(self, n: int) -> bytes:
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def frame(self) -> tuple[int, bytes]:
        b0, b1 = self._read(2)
        assert not b1 & 0x80, "a server frame is never masked"
        n = b1 & 0x7F
        if n == 126:
            n = struct.unpack("!H", self._read(2))[0]
        elif n == 127:
            n = struct.unpack("!Q", self._read(8))[0]
        return b0 & 0x0F, self._read(n)

    def message(self) -> dict[str, Any]:
        while True:
            op, payload = self.frame()
            if op == OP_TEXT:
                return json.loads(payload)
            if op == OP_CLOSE:
                raise ConnectionError("closed")

    def output_until(self, needle: str) -> str:
        seen = ""
        while needle not in seen:
            msg = self.message()
            assert msg["type"] == "output", msg
            seen += msg["text"]
        return seen

    def send(self, obj: Any, opcode: int = OP_TEXT) -> None:
        payload = obj if isinstance(obj, bytes) else json.dumps(obj).encode()
        self.sock.sendall(mask_frame(opcode, payload))

    def close(self) -> None:
        self.sock.close()


def _wait(predicate, timeout: float = 3.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return bool(predicate())


def test_a_terminal_gets_its_replay_its_output_and_sends_its_keys(live):
    server, bridge = live
    client = Client(server.server_address[1])
    try:
        assert client.status_line == "HTTP/1.1 101 Switching Protocols"
        assert f"Sec-WebSocket-Accept: {ws.accept_key(client.key)}" in client.head
        assert "banner" in client.output_until("$ ")
        bridge.sessions["schwalby/one"].emit("hello ä\r\n")
        assert "hello ä" in client.output_until("hello")
        client.send({"type": "input", "data": "ls -la\r"})
        client.send({"type": "resize", "cols": 120, "rows": 40})
        client.send({"type": "resize", "cols": True, "rows": 40})       # not a number
        client.send({"type": "resize", "cols": 5000, "rows": 40})       # out of range
        client.send(b"not json")
        assert _wait(lambda: bridge.sent == [("schwalby/one", "ls -la\r")])
        assert _wait(lambda: bridge.resized == [("schwalby/one", 120, 40)])
        client.send(b"are you there", opcode=OP_PING)
        op, payload = client.frame()
        while op == OP_TEXT:
            op, payload = client.frame()
        assert (op, payload) == (OP_PONG, b"are you there")
    finally:
        client.close()
    assert _wait(lambda: not bridge.sessions["schwalby/one"].subs), "the subscriber outlived the socket"


def test_a_page_on_another_site_is_refused_before_the_upgrade(live):
    server, bridge = live
    client = Client(server.server_address[1], origin="https://evil.example")
    try:
        assert " 403 " in client.status_line
        assert "Sec-WebSocket-Accept" not in client.head
    finally:
        client.close()
    assert not bridge.sessions["schwalby/one"].subs


def test_a_session_that_is_not_there_is_answered_with_closed(live):
    server, _ = live
    client = Client(server.server_address[1], name="nobody")
    try:
        assert client.status_line.startswith("HTTP/1.1 101")
        assert client.message() == {"type": "closed", "name": "nobody"}
        op, _ = client.frame()
        assert op == OP_CLOSE
    finally:
        client.close()


def test_the_end_of_a_session_reaches_the_terminal_after_its_last_words(live):
    server, bridge = live
    client = Client(server.server_address[1])
    session = bridge.sessions["schwalby/one"]
    try:
        client.output_until("$ ")
        session.emit("[Shell ended]\r\n")
        session.connected = False
        assert "[Shell ended]" in client.output_until("[Shell ended]")
        assert client.message() == {"type": "closed", "name": "schwalby/one"}
    finally:
        client.close()


def test_the_browser_closing_ends_the_handler(live):
    server, bridge = live
    client = Client(server.server_address[1])
    try:
        client.output_until("$ ")
        client.send(struct.pack("!H", 1000), opcode=OP_CLOSE)
        op, payload = client.frame()
        assert op == OP_CLOSE and struct.unpack("!H", payload[:2])[0] == 1000
    finally:
        client.close()
    assert _wait(lambda: not bridge.sessions["schwalby/one"].subs)
    assert _wait(lambda: not [t for t in threading.enumerate() if t.name.startswith("ssh-ws-")])


def test_an_unmasked_frame_is_answered_with_a_protocol_error(live):
    server, _ = live
    client = Client(server.server_address[1])
    try:
        client.output_until("$ ")
        client.sock.sendall(encode_frame(OP_TEXT, b'{"type":"input","data":"x"}'))
        op, payload = client.frame()
        while op == OP_TEXT:
            op, payload = client.frame()
        assert op == OP_CLOSE and struct.unpack("!H", payload[:2])[0] == ws.CLOSE_PROTOCOL_ERROR
    finally:
        client.close()


def test_a_request_that_is_not_an_upgrade_gets_an_error_not_a_hang(live):
    import http.client
    server, _ = live
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    try:
        conn.request("GET", "/api/ssh/ws?name=schwalby%2Fone")
        assert conn.getresponse().status == 400
    finally:
        conn.close()


def test_shutting_the_server_down_ends_an_open_terminal(live):
    server, bridge = live
    client = Client(server.server_address[1])
    try:
        client.output_until("$ ")
        server.stopping.set()
        client.sock.settimeout(5)
        op = None
        started = time.monotonic()
        while op != OP_CLOSE:
            op, _ = client.frame()
        assert time.monotonic() - started < 4
    finally:
        client.close()
    assert _wait(lambda: not bridge.sessions["schwalby/one"].subs)
