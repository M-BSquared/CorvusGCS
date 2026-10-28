"""corvus_link's links, each against a far end that speaks pymavlink.

The far end is pymavlink's generated dialect module writing to a plain
socket, pty or file, so these tests check corvus_link's transports and the
per-system state of its ``mavfile`` against what pymavlink itself produces
and expects.
"""
from __future__ import annotations

import os
import socket
import struct
import sys
import threading
import time

import pytest

corvus_link = pytest.importorskip("corvus_link")
pv1 = pytest.importorskip("pymavlink.dialects.v10.all")
pv2 = pytest.importorskip("pymavlink.dialects.v20.all")
from corvus_link import mavutil  # noqa: E402

POSIX = os.name != "nt"


class Out:
    """pymavlink's MAVLink wants a file; this one hands frames to a callable."""

    def __init__(self, send) -> None:
        self.send = send

    def write(self, buf) -> None:
        self.send(bytes(buf))


def vehicle(dialect, send, sysid: int = 1, compid: int = 1):
    return dialect.MAVLink(Out(send), srcSystem=sysid, srcComponent=compid)


def px4_heartbeat(mav, armed: bool = False) -> None:
    base = 1 | (128 if armed else 0)
    mav.heartbeat_send(2, 12, base, 0x04040000, 4)


def free_port(kind=socket.SOCK_DGRAM) -> int:
    with socket.socket(socket.AF_INET, kind) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def test_udpin_learns_the_vehicle_and_answers_it() -> None:
    conn = mavutil.mavlink_connection("udpin:127.0.0.1:0", source_system=254, source_component=190)
    try:
        assert conn.udp_server is True
        host, port = conn.port.getsockname()
        peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        peer.bind(("127.0.0.1", 0))
        peer.settimeout(2)
        v = vehicle(pv1, lambda b: peer.sendto(b, (host, port)))
        px4_heartbeat(v)
        hb = conn.wait_heartbeat(timeout=2)
        assert hb is not None and hb.get_srcSystem() == 1
        assert conn.target_system == 1
        assert conn.flightmode == "MISSION"
        assert conn.WIRE_PROTOCOL_VERSION == "1.0"
        conn.mav.heartbeat_send(6, 8, 0, 0, 0)
        data, _ = peer.recvfrom(1024)
        reply = pv1.MAVLink(None).parse_char(data)
        assert reply.get_type() == "HEARTBEAT" and reply.get_srcSystem() == 254
        assert reply.get_srcComponent() == 190
        assert {c[1] for c in conn.clients} == {peer.getsockname()[1]}
        peer.close()
    finally:
        conn.close()


def test_first_mavlink2_frame_switches_the_link_and_mavutil() -> None:
    conn = mavutil.mavlink_connection("udpin:127.0.0.1:0")
    try:
        host, port = conn.port.getsockname()
        peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        v = vehicle(pv2, lambda b: peer.sendto(b, (host, port)))
        old_mav = conn.mav
        px4_heartbeat(v)
        assert conn.wait_heartbeat(timeout=2) is not None
        assert conn.WIRE_PROTOCOL_VERSION == "2.0"
        assert conn.mav is not old_mav
        assert conn.mav.WIRE_PROTOCOL_VERSION == "2.0"
        assert mavutil.mavlink.WIRE_PROTOCOL_VERSION == "2.0"
        assert hasattr(conn.mav, "open_drone_id_basic_id_send")
        assert os.environ.get("MAVLINK20") == "1"
        peer.close()
    finally:
        conn.close()


def test_udpout_sends_to_its_destination() -> None:
    peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    peer.bind(("127.0.0.1", 0))
    peer.settimeout(2)
    conn = mavutil.mavlink_connection(f"udpout:127.0.0.1:{peer.getsockname()[1]}", source_system=254)
    try:
        assert conn.udp_server is False
        conn.mav.heartbeat_send(6, 8, 0, 0, 0)
        data, addr = peer.recvfrom(1024)
        assert pv2.MAVLink(None).parse_char(data).get_type() == "HEARTBEAT"
        v = vehicle(pv2, lambda b: peer.sendto(b, addr))
        px4_heartbeat(v)
        assert conn.wait_heartbeat(timeout=2) is not None
    finally:
        conn.close()
        peer.close()


def test_tcp_client_and_its_reconnect_free_eof() -> None:
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.bind(("127.0.0.1", 0))
    server.listen(1)
    port = server.getsockname()[1]
    conn = mavutil.mavlink_connection(f"tcp:127.0.0.1:{port}")
    try:
        peer, _ = server.accept()
        peer.settimeout(2)
        v = vehicle(pv2, peer.sendall)
        px4_heartbeat(v)
        v.attitude_send(1000, 0.1, 0.2, 0.3, 0, 0, 0)
        assert conn.wait_heartbeat(timeout=2) is not None
        att = conn.recv_match(type="ATTITUDE", blocking=True, timeout=2)
        assert att is not None and abs(att.pitch - 0.2) < 1e-6
        conn.mav.command_long_send(1, 1, 400, 0, 1, 0, 0, 0, 0, 0, 0)
        got = pv2.MAVLink(None).parse_char(peer.recv(1024))
        assert got.get_type() == "COMMAND_LONG" and got.command == 400
        peer.close()
        assert conn.recv_match(blocking=True, timeout=0.3) is None
    finally:
        conn.close()
        server.close()


def test_tcp_refused_raises_after_its_retries() -> None:
    port = free_port(socket.SOCK_STREAM)
    started = time.monotonic()
    with pytest.raises(ConnectionRefusedError):
        mavutil.mavlink_connection(f"tcp:127.0.0.1:{port}", retries=2, reconnect_delay=0.05)
    assert time.monotonic() - started < 2


def test_tcpin_accepts_one_client() -> None:
    conn = mavutil.mavlink_connection("tcpin:127.0.0.1:0")
    try:
        assert conn.port is None
        listen_port = conn_listen_port(conn)
        client = socket.create_connection(("127.0.0.1", listen_port))
        v = vehicle(pv2, client.sendall)
        px4_heartbeat(v)
        assert conn.wait_heartbeat(timeout=2) is not None
        conn.mav.heartbeat_send(6, 8, 0, 0, 0)
        client.settimeout(2)
        assert pv2.MAVLink(None).parse_char(client.recv(1024)).get_type() == "HEARTBEAT"
        client.close()
    finally:
        conn.close()


def conn_listen_port(conn) -> int:
    # The listener's port is what getsockname reports once a port object
    # exists; before a client, read it from the fd.
    sock = socket.socket(fileno=os.dup(conn.fd))
    try:
        return sock.getsockname()[1]
    finally:
        sock.close()


@pytest.mark.skipif(not POSIX, reason="pseudo-terminals are POSIX")
def test_serial_over_a_pty_matches_pyserial_behaviour() -> None:
    master, slave = os.openpty()
    name = os.ttyname(slave)
    conn = mavutil.mavlink_connection(name, baud=57600, source_system=254)
    try:
        assert conn.device == name and conn.baud == 57600
        fd = conn.port.fileno()
        assert isinstance(fd, int)
        import fcntl
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        v = vehicle(pv2, lambda b: os.write(master, b))
        px4_heartbeat(v)
        v.sys_status_send(0, 0, 0, 500, 12000, -1, 80, 0, 0, 0, 0, 0, 0)
        assert conn.wait_heartbeat(timeout=2) is not None
        status = conn.recv_match(type="SYS_STATUS", blocking=True, timeout=2)
        assert status is not None and status.voltage_battery == 12000
        conn.mav.heartbeat_send(6, 8, 0, 0, 0)
        time.sleep(0.1)
        got = pv2.MAVLink(None).parse_buffer(os.read(master, 1024))
        assert got and got[0].get_type() == "HEARTBEAT"
    finally:
        conn.close()
        os.close(master)
        os.close(slave)


def test_serial_open_errors_carry_errno() -> None:
    with pytest.raises(OSError) as info:
        mavutil.mavlink_connection("/dev/corvus-no-such-port", baud=57600)
    if POSIX:
        assert info.value.errno == 2
        assert "could not open port /dev/corvus-no-such-port" in str(info.value)


@pytest.mark.skipif(not POSIX, reason="Unix domain sockets")
def test_unix_domain_socket() -> None:
    import tempfile
    # sun_path holds about a hundred bytes; pytest's tmp_path on macOS is longer.
    path = os.path.join(tempfile.mkdtemp(prefix="cl", dir="/tmp"), "s")
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(path)
    server.listen(1)
    conn = mavutil.mavlink_connection(f"uds:{path}")
    try:
        peer, _ = server.accept()
        v = vehicle(pv2, peer.sendall)
        px4_heartbeat(v)
        assert conn.wait_heartbeat(timeout=2) is not None
        conn.mav.heartbeat_send(6, 8, 0, 0, 0)
        peer.settimeout(2)
        assert pv2.MAVLink(None).parse_char(peer.recv(1024)).get_type() == "HEARTBEAT"
        peer.close()
    finally:
        conn.close()
        server.close()


def test_multicast_drops_its_own_frames() -> None:
    port = free_port()
    try:
        a = mavutil.mavlink_connection(f"mcast:239.255.145.50:{port}", source_system=254)
        b = mavutil.mavlink_connection(f"mcast:239.255.145.50:{port}", source_system=1)
    except OSError as exc:
        pytest.skip(f"no multicast here: {exc}")
    try:
        b.mav.heartbeat_send(2, 12, 1, 0x04040000, 4)
        hb = a.wait_heartbeat(timeout=1)
        if hb is None:
            pytest.skip("multicast loopback is not delivered here")
        assert hb.get_srcSystem() == 1
        own = b.recv_match(type="HEARTBEAT", blocking=True, timeout=0.3)
        assert own is None
    finally:
        a.close()
        b.close()


def test_tlog_reading_matches_pymavlink(tmp_path) -> None:
    from pymavlink import mavutil as pmu
    path = tmp_path / "flight.tlog"
    frames = []
    v = vehicle(pv2, frames.append)
    for i in range(20):
        px4_heartbeat(v, armed=i > 10)
        v.attitude_send(i * 100, 0.01 * i, 0, 0, 0, 0, 0)
    with open(path, "wb") as f:
        for i, frame in enumerate(frames):
            f.write(struct.pack(">Q", 1_700_000_000_000_000 + i * 10_000) + frame)
    ours = mavutil.mavlink_connection(str(path))
    theirs = pmu.mavlink_connection(str(path))
    try:
        got_ours, got_theirs = [], []
        for conn, out in ((ours, got_ours), (theirs, got_theirs)):
            while True:
                m = conn.recv_match(blocking=False)
                if m is None:
                    break
                out.append((m.get_type(), round(m._timestamp, 6), m.to_dict()))
        assert got_ours == got_theirs
        assert ours.flightmode == theirs.flightmode
    finally:
        ours.close()
        theirs.close()


@pytest.mark.skipif(not POSIX, reason="child process links are POSIX")
def test_child_process_link_and_its_teardown(tmp_path) -> None:
    script = tmp_path / "bin" / "vehicle.elf"
    script.parent.mkdir()
    frame = pv2.MAVLink_heartbeat_message(2, 12, 1, 0x04040000, 4, 3).pack(pv2.MAVLink(None, 1, 1))
    script.write_text(
        f"#!{sys.executable}\nimport sys, time\n"
        f"sys.stdout.buffer.write({frame!r}); sys.stdout.flush()\n"
        "time.sleep(30)\n"
    )
    script.chmod(0o755)
    conn = mavutil.mavlink_connection(str(script))
    try:
        assert conn.wait_heartbeat(timeout=5) is not None
    finally:
        started = time.monotonic()
        conn.close()
        assert time.monotonic() - started < 6


def test_state_tracking_matches_pymavlink() -> None:
    """The same MAVLink 1 stream into pymavlink's mavudp and ours."""
    from pymavlink import mavutil as pmu
    ours = mavutil.mavlink_connection("udpin:127.0.0.1:0")
    theirs = pmu.mavlink_connection(f"udpin:127.0.0.1:{free_port()}")
    peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    targets = [ours.port.getsockname(), theirs.port.getsockname()]

    def send(b: bytes) -> None:
        for t in targets:
            peer.sendto(b, t)

    gcs = vehicle(pv1, send, sysid=255, compid=190)
    v = vehicle(pv1, send, sysid=7, compid=1)
    ap = vehicle(pv1, send, sysid=9, compid=1)
    try:
        gcs.heartbeat_send(6, 8, 0, 0, 4)
        v.heartbeat_send(2, 3, 1 | 128, 4, 4)
        ap.heartbeat_send(1, 3, 1, 10, 4)
        v.param_value_send(b"WPNAV_SPEED", 500.0, 9, 900, 3)
        v.seq = 40  # a gap the loss counter must see
        v.gps_raw_int_send(0, 3, 1, 2, 3, 4, 5, 6, 7, 8)
        v.statustext_send(4, b"PreArm: hello")
        results = []
        for conn in (ours, theirs):
            seen = []
            deadline = time.monotonic() + 3
            while len(seen) < 6 and time.monotonic() < deadline:
                m = conn.recv_match(blocking=True, timeout=0.5)
                if m is not None:
                    seen.append(m.get_type())
            conn.target_system = 7
            results.append({
                "seen": seen,
                "sysid": conn.sysid,
                "flightmode": conn.flightmode,
                "systems": sorted(conn.sysid_state),
                "loss": (conn.mav_loss, conn.mav_count),
                "params": dict(conn.params),
                "mapping": conn.mode_mapping(),
                "armed": conn.motors_armed(),
                "messages": sorted(conn.messages),
                "home_fix": conn.messages["HOME"].fix_type,
            })
        assert results[0] == results[1]
    finally:
        peer.close()
        ours.close()
        theirs.close()


def test_signing_setup_on_a_link() -> None:
    conn = mavutil.mavlink_connection("udpin:127.0.0.1:0")
    try:
        key = bytes(range(1, 33))
        conn.setup_signing(key, sign_outgoing=True, allow_unsigned_callback=lambda mav, mid: mid == 109)
        signing = conn.mav.signing
        assert signing.secret_key == key and signing.sign_outgoing
        assert signing.timestamp > 0
        host, port = conn.port.getsockname()
        peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        peer.bind(("127.0.0.1", 0))
        peer.settimeout(2)
        v = vehicle(pv2, lambda b: peer.sendto(b, (host, port)))
        v.signing.secret_key = key
        v.signing.sign_outgoing = True
        v.signing.timestamp = signing.timestamp + 10
        px4_heartbeat(v)
        hb = conn.wait_heartbeat(timeout=2)
        assert hb is not None and hb.get_signed()
        conn.mav.heartbeat_send(6, 8, 0, 0, 0)
        data, _ = peer.recvfrom(1024)
        check = pv2.MAVLink(None)
        check.signing.secret_key = key
        assert check.parse_char(data).get_signed()
        conn.disable_signing()
        assert conn.mav.signing.secret_key is None
        peer.close()
    finally:
        conn.close()


def test_threads_can_send_while_one_waits() -> None:
    conn = mavutil.mavlink_connection("udpin:127.0.0.1:0")
    try:
        stop = threading.Event()
        errors = []

        def reader() -> None:
            while not stop.is_set():
                try:
                    conn.recv_match(blocking=True, timeout=0.2)
                except Exception as exc:  # noqa: BLE001 - collected
                    errors.append(exc)

        t = threading.Thread(target=reader)
        t.start()
        for _ in range(500):
            conn.mav.heartbeat_send(6, 8, 0, 0, 0)
        stop.set()
        t.join(2)
        assert not t.is_alive() and not errors
    finally:
        conn.close()


@pytest.mark.parametrize("device", ["log.bin"])
def test_links_not_built_yet_say_so(device: str) -> None:
    with pytest.raises(NotImplementedError):
        mavutil.mavlink_connection(device)
