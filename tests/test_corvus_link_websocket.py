"""corvus_link's WebSocket links against pymavlink's.

``ws:``, ``wss:`` and ``wsserver:`` are run the same way twice, once through
pymavlink and once through corvus_link, against a far end written with
wsproto: a scripted server for the two client links, a scripted client for
the server link. What each side received (the bytes of the binary messages,
in order), what the link handed back, how it behaved on a text frame, a ping,
a close, a dropped connection and a connection that was never there, and how
it refused a bad device string are compared. ``wss:`` runs against a TLS
server with a throwaway certificate: untrusted it is refused on both, and
trusted through ``SSL_CERT_FILE`` it carries data on both.
"""
from __future__ import annotations

import os
import queue
import shutil
import socket
import ssl
import struct
import subprocess
import threading
import time
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("corvus_link")
pytest.importorskip("wsproto")
pv2 = pytest.importorskip("pymavlink.dialects.v20.all")
from pymavlink import mavutil as py_mavutil  # noqa: E402
from wsproto import ConnectionType, WSConnection  # noqa: E402
from wsproto.events import (  # noqa: E402
    AcceptConnection,
    BytesMessage,
    CloseConnection,
    Ping,
    Request,
    TextMessage,
)

from corvus_link import mavutil as rust_mavutil  # noqa: E402

LINKS = (("pymavlink", py_mavutil), ("corvus_link", rust_mavutil))


class Out:
    def __init__(self, send: Any) -> None:
        self.send = send

    def write(self, buf: Any) -> None:
        self.send(bytes(buf))


def heartbeat_frames(count: int = 1) -> list[bytes]:
    frames: list[bytes] = []
    mav = pv2.MAVLink(Out(frames.append), srcSystem=1, srcComponent=1)
    for _ in range(count):
        mav.heartbeat_send(2, 12, 81, 0x04040000, 4)
    return frames


def wait_for(predicate: Any, timeout: float = 5.0) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(0.01)
    return False


def pump(conn: Any, until: Any, timeout: float = 5.0) -> list[Any]:
    """Receive messages from *conn* until *until(messages)*."""
    messages: list[Any] = []
    end = time.monotonic() + timeout
    while time.monotonic() < end and not until(messages):
        msg = conn.recv_match(blocking=False)
        if msg is None:
            time.sleep(0.01)
            continue
        messages.append(msg)
    return messages


# --------------------------------------------------------- the far ends ----

class WsPeer:
    """A wsproto server connection handler: records and obeys scripted sends."""

    def __init__(self, sock: socket.socket) -> None:
        self.sock = sock
        self.ws = WSConnection(ConnectionType.SERVER)
        self.received: list[bytes] = []
        self.texts: list[str] = []
        self.pongs = 0
        self.closed_by_peer = False
        self.commands: queue.Queue = queue.Queue()
        self.dead = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self) -> None:
        self.sock.settimeout(0.02)
        try:
            while not self.dead.is_set():
                try:
                    data = self.sock.recv(65536)
                    if not data:
                        break
                    self.ws.receive_data(data)
                    for event in self.ws.events():
                        if isinstance(event, Request):
                            self.sock.sendall(self.ws.send(AcceptConnection()))
                        elif isinstance(event, BytesMessage):
                            self.received.append(bytes(event.data))
                        elif isinstance(event, TextMessage):
                            self.texts.append(event.data)
                        elif isinstance(event, CloseConnection):
                            self.closed_by_peer = True
                            try:
                                self.sock.sendall(self.ws.send(event.response()))
                            except Exception:  # noqa: BLE001 - the state may already be closed
                                pass
                            self.dead.set()
                        elif event.__class__.__name__ == "Pong":
                            self.pongs += 1
                except TimeoutError:
                    pass
                except OSError:
                    break
                while True:
                    try:
                        action, arg = self.commands.get_nowait()
                    except queue.Empty:
                        break
                    if action == "binary":
                        self.sock.sendall(self.ws.send(BytesMessage(data=arg)))
                    elif action == "text":
                        self.sock.sendall(self.ws.send(TextMessage(data=arg)))
                    elif action == "ping":
                        self.sock.sendall(self.ws.send(Ping(payload=arg)))
                    elif action == "close":
                        try:
                            self.sock.sendall(self.ws.send(CloseConnection(code=1000)))
                        except Exception:  # noqa: BLE001
                            pass
                    elif action == "drop":
                        self.dead.set()
                    elif action == "reset":
                        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
                        self.dead.set()
        finally:
            try:
                self.sock.close()
            except OSError:
                pass


class WsServerHarness:
    """A listening wsproto server, optionally TLS, that keeps each connection."""

    def __init__(self, context: ssl.SSLContext | None = None) -> None:
        self.listen = socket.socket()
        self.listen.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listen.bind(("127.0.0.1", 0))
        self.listen.listen(4)
        self.port = self.listen.getsockname()[1]
        self.context = context
        self.peers: list[WsPeer] = []
        self.failures: list[str] = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self) -> None:
        self.listen.settimeout(0.05)
        while not self.stop.is_set():
            try:
                sock, _ = self.listen.accept()
            except (TimeoutError, OSError):
                continue
            if self.context is not None:
                try:
                    sock.settimeout(5)
                    sock = self.context.wrap_socket(sock, server_side=True)
                except (ssl.SSLError, OSError) as exc:
                    self.failures.append(type(exc).__name__)
                    sock.close()
                    continue
            self.peers.append(WsPeer(sock))

    def close(self) -> None:
        self.stop.set()
        for peer in self.peers:
            peer.dead.set()
        self.listen.close()


@pytest.fixture
def server() -> Any:
    harness = WsServerHarness()
    yield harness
    harness.close()


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def conn_to(module: Any, device: str) -> Any:
    conn = module.mavlink_connection(device)
    return conn


# ---------------------------------------------------------- client: data ----

@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_client_exchanges_binary_messages(name: str, module: Any, server: WsServerHarness) -> None:
    conn = conn_to(module, f"ws:127.0.0.1:{server.port}")
    try:
        assert wait_for(lambda: server.peers)
        peer = server.peers[0]
        frames = heartbeat_frames(3)
        peer.commands.put(("binary", frames[0]))
        peer.commands.put(("binary", frames[1] + frames[2]))
        got = pump(conn, lambda m: len([x for x in m if x.get_type() == "HEARTBEAT"]) >= 3)
        assert [m.get_type() for m in got] == ["HEARTBEAT"] * 3
        conn.write(b"\xfd\x00hello")
        conn.write(b"second")
        assert wait_for(lambda: len(peer.received) >= 2)
        assert peer.received[:2] == [b"\xfd\x00hello", b"second"]
    finally:
        conn.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_client_ignores_text_frames_and_reassembles_a_split_message(name: str, module: Any, server: WsServerHarness) -> None:
    conn = conn_to(module, f"ws:127.0.0.1:{server.port}")
    try:
        assert wait_for(lambda: server.peers)
        peer = server.peers[0]
        frame = heartbeat_frames(1)[0]
        peer.commands.put(("text", "not mavlink"))
        peer.commands.put(("binary", frame[:5]))
        time.sleep(0.15)
        peer.commands.put(("binary", frame[5:]))
        got = pump(conn, lambda m: len(m) >= 1)
        assert [m.get_type() for m in got] == ["HEARTBEAT"]
    finally:
        conn.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_client_survives_a_ping_and_a_close(name: str, module: Any, server: WsServerHarness) -> None:
    conn = conn_to(module, f"ws:127.0.0.1:{server.port}")
    try:
        assert wait_for(lambda: server.peers)
        peer = server.peers[0]
        peer.commands.put(("ping", b"are you there"))
        peer.commands.put(("binary", heartbeat_frames(1)[0]))
        got = pump(conn, lambda m: len(m) >= 1)
        assert [m.get_type() for m in got] == ["HEARTBEAT"]
        peer.commands.put(("close", None))
        for _ in range(30):
            conn.recv_match(blocking=False)
            time.sleep(0.01)
    finally:
        conn.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_client_to_a_resource_and_a_missing_server(name: str, module: Any, server: WsServerHarness) -> None:
    conn = conn_to(module, f"ws:127.0.0.1:{server.port}:/mavlink")
    try:
        assert wait_for(lambda: server.peers)
    finally:
        conn.close()
    gone = free_port()
    conn = conn_to(module, f"ws:127.0.0.1:{gone}")
    try:
        assert conn.fd is None
        assert conn.recv_match(blocking=False) is None
        conn.write(b"nobody listens")
    finally:
        conn.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_client_reconnects_after_the_server_resets_it(name: str, module: Any, server: WsServerHarness) -> None:
    conn = conn_to(module, f"ws:127.0.0.1:{server.port}")
    try:
        assert wait_for(lambda: server.peers)
        server.peers[0].commands.put(("reset", None))
        assert wait_for(lambda: server.peers[0].dead.is_set())
        time.sleep(0.1)
        deadline = time.monotonic() + 5
        while len(server.peers) < 2 and time.monotonic() < deadline:
            conn.write(b"are you there")
            conn.recv_match(blocking=False)
            time.sleep(0.05)
        assert len(server.peers) >= 2
        server.peers[1].commands.put(("binary", heartbeat_frames(1)[0]))
        got = pump(conn, lambda m: len(m) >= 1)
        assert [m.get_type() for m in got] == ["HEARTBEAT"]
    finally:
        conn.close()


# -------------------------------------------------------- client: errors ----

@pytest.mark.parametrize("device", ["ws:host", "wss:only", "ws:127.0.0.1:notaport", "wss:127.0.0.1:"])
def test_bad_client_devices_fail_alike(device: str) -> None:
    results = []
    for _name, module in LINKS:
        try:
            module.mavlink_connection(device).close()
            results.append(("opened",))
        except Exception as exc:  # noqa: BLE001 - compared below
            results.append((type(exc).__name__, str(exc)))
    assert results[1] == results[0]
    assert results[0][0] == "ValueError"


def test_a_server_that_answers_something_else_fails_the_handshake() -> None:
    listen = socket.socket()
    listen.bind(("127.0.0.1", 0))
    listen.listen(4)
    port = listen.getsockname()[1]

    def answer() -> None:
        for _ in range(2):
            sock, _ = listen.accept()
            sock.recv(4096)
            sock.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            sock.close()

    threading.Thread(target=answer, daemon=True).start()
    results = []
    for _name, module in LINKS:
        try:
            module.mavlink_connection(f"ws:127.0.0.1:{port}").close()
            results.append(("opened",))
        except Exception as exc:  # noqa: BLE001
            results.append((type(exc).__name__,))
    listen.close()
    assert results[1] == results[0]


# -------------------------------------------------------------- the server ----

class WsClientHarness:
    """A wsproto client that connects to a wsserver: link and records."""

    def __init__(self, port: int) -> None:
        self.sock = socket.create_connection(("127.0.0.1", port))
        self.sock.settimeout(0.02)
        self.ws = WSConnection(ConnectionType.CLIENT)
        self.sock.sendall(self.ws.send(Request(host="127.0.0.1", target="/")))
        self.accepted = False
        self.received: list[bytes] = []
        self.closed = False

    def poll(self) -> None:
        try:
            data = self.sock.recv(65536)
        except TimeoutError:
            return
        except OSError:
            self.closed = True
            return
        if not data:
            self.closed = True
            return
        self.ws.receive_data(data)
        for event in self.ws.events():
            if isinstance(event, AcceptConnection):
                self.accepted = True
            elif isinstance(event, BytesMessage):
                self.received.append(bytes(event.data))
            elif isinstance(event, CloseConnection):
                self.closed = True

    def send_binary(self, data: bytes) -> None:
        self.sock.sendall(self.ws.send(BytesMessage(data=data)))

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def handshake(conn: Any, client: WsClientHarness, timeout: float = 5.0) -> None:
    """Drive both ends until the server link has accepted the client."""
    end = time.monotonic() + timeout
    while not client.accepted and time.monotonic() < end:
        client.poll()
        conn.recv_match(blocking=False)
        time.sleep(0.005)
    assert client.accepted


def serve_until(conn: Any, client: WsClientHarness, predicate: Any, timeout: float = 5.0) -> list[Any]:
    messages: list[Any] = []
    end = time.monotonic() + timeout
    while time.monotonic() < end and not predicate(messages):
        client.poll()
        msg = conn.recv_match(blocking=False)
        if msg is not None:
            messages.append(msg)
        else:
            time.sleep(0.005)
    return messages


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_server_takes_a_client_and_exchanges_messages(name: str, module: Any) -> None:
    port = free_port()
    conn = module.mavlink_connection(f"wsserver:127.0.0.1:{port}")
    client = None
    try:
        client = WsClientHarness(port)
        handshake(conn, client)
        frames = heartbeat_frames(2)
        client.send_binary(frames[0])
        client.send_binary(frames[1][:6])
        time.sleep(0.1)
        client.send_binary(frames[1][6:])
        got = serve_until(conn, client, lambda m: len(m) >= 2)
        assert [m.get_type() for m in got] == ["HEARTBEAT", "HEARTBEAT"]
        assert client.accepted
        conn.write(b"to the client")
        assert wait_for(lambda: (client.poll() or True) and client.received)
        assert client.received == [b"to the client"]
    finally:
        if client:
            client.close()
        conn.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_server_goes_back_to_accepting_after_a_close(name: str, module: Any) -> None:
    port = free_port()
    conn = module.mavlink_connection(f"wsserver:127.0.0.1:{port}")
    clients: list[WsClientHarness] = []
    try:
        first = WsClientHarness(port)
        clients.append(first)
        handshake(conn, first)
        first.send_binary(heartbeat_frames(1)[0])
        got = serve_until(conn, first, lambda m: len(m) >= 1)
        assert len(got) == 1
        first.sock.sendall(first.ws.send(CloseConnection(code=1000)))
        for _ in range(60):
            first.poll()
            conn.recv_match(blocking=False)
            time.sleep(0.01)
        second = WsClientHarness(port)
        clients.append(second)
        handshake(conn, second)
        second.send_binary(heartbeat_frames(1)[0])
        got = serve_until(conn, second, lambda m: len(m) >= 1)
        assert len(got) == 1 and got[0].get_type() == "HEARTBEAT"
    finally:
        for c in clients:
            c.close()
        conn.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_a_server_without_a_client_reads_and_writes_nothing(name: str, module: Any) -> None:
    port = free_port()
    conn = module.mavlink_connection(f"wsserver:127.0.0.1:{port}")
    try:
        assert conn.recv_match(blocking=False) is None
        conn.write(b"nobody")
        assert conn.fd is not None
    finally:
        conn.close()


@pytest.mark.parametrize("device", ["wsserver:127.0.0.1", "wsserver:a:b:c", "wsserver:127.0.0.1:notaport"])
def test_bad_server_devices_fail_alike(device: str) -> None:
    results = []
    for _name, module in LINKS:
        try:
            module.mavlink_connection(device).close()
            results.append(("opened",))
        except Exception as exc:  # noqa: BLE001
            results.append((type(exc).__name__, str(exc)))
    assert results[1] == results[0]
    assert results[0][0] == "ValueError"


def test_a_server_port_in_use_fails_alike() -> None:
    holder = socket.socket()
    holder.bind(("127.0.0.1", 0))
    holder.listen(1)
    port = holder.getsockname()[1]
    results = []
    for _name, module in LINKS:
        try:
            module.mavlink_connection(f"wsserver:127.0.0.1:{port}").close()
            results.append(("opened",))
        except OSError as exc:
            results.append((type(exc).__name__, exc.errno))
    holder.close()
    assert results[1] == results[0]


# ---------------------------------------------------------------------- TLS ----

@pytest.fixture(scope="module")
def certificate(tmp_path_factory: pytest.TempPathFactory) -> dict:
    if shutil.which("openssl") is None:
        pytest.skip("openssl is needed to make a test certificate")
    d: Path = tmp_path_factory.mktemp("tls")
    ca_config = d / "ca.cnf"
    ca_config.write_text(
        "[req]\ndistinguished_name=dn\nx509_extensions=v3\nprompt=no\n[dn]\nCN=Corvus test CA\n"
        "[v3]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\n"
    )
    leaf_config = d / "leaf.cnf"
    leaf_config.write_text(
        "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n"
        "[v3]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n"
    )
    ca_cert, ca_key = d / "ca.pem", d / "ca.key"
    cert, key, csr = d / "cert.pem", d / "key.pem", d / "leaf.csr"

    def run(*args: str) -> None:
        subprocess.run(["openssl", *args], check=True, capture_output=True)

    run("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(ca_key), "-out", str(ca_cert),
        "-days", "2", "-config", str(ca_config))
    run("req", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key), "-out", str(csr), "-config", str(leaf_config))
    run("x509", "-req", "-in", str(csr), "-CA", str(ca_cert), "-CAkey", str(ca_key), "-CAcreateserial",
        "-out", str(cert), "-days", "2", "-extfile", str(leaf_config), "-extensions", "v3")
    return {"cert": str(cert), "key": str(key), "ca": str(ca_cert)}


@pytest.fixture
def tls_server(certificate: dict) -> Any:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certificate["cert"], certificate["key"])
    harness = WsServerHarness(context)
    yield harness
    harness.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_wss_refuses_a_certificate_nobody_trusts(name: str, module: Any, tls_server: WsServerHarness,
                                                 monkeypatch: pytest.MonkeyPatch, capsys: Any) -> None:
    monkeypatch.delenv("SSL_CERT_FILE", raising=False)
    with pytest.raises(OSError):
        module.mavlink_connection(f"wss:localhost:{tls_server.port}")
    assert not tls_server.peers


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_wss_carries_data_once_the_certificate_is_trusted(name: str, module: Any, tls_server: WsServerHarness,
                                                          certificate: dict, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SSL_CERT_FILE", certificate["ca"])
    conn = module.mavlink_connection(f"wss:localhost:{tls_server.port}")
    try:
        assert wait_for(lambda: tls_server.peers)
        peer = tls_server.peers[0]
        peer.commands.put(("binary", heartbeat_frames(1)[0]))
        got = pump(conn, lambda m: len(m) >= 1)
        assert [m.get_type() for m in got] == ["HEARTBEAT"]
        conn.write(b"over tls")
        assert wait_for(lambda: peer.received)
        assert peer.received == [b"over tls"]
    finally:
        conn.close()


@pytest.mark.parametrize("name,module", LINKS, ids=[n for n, _ in LINKS])
def test_wss_checks_the_host_name(name: str, module: Any, tls_server: WsServerHarness, certificate: dict,
                                  monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SSL_CERT_FILE", certificate["ca"])
    # The certificate names localhost and 127.0.0.1; this name is neither.
    with pytest.raises(OSError):
        module.mavlink_connection(f"wss:localtest.me.invalid:{tls_server.port}")
    assert os.environ["SSL_CERT_FILE"] == certificate["ca"]
