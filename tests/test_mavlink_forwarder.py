"""Second-station forwarding: framing, the two directions, and teardown.

The safety-relevant half of this module is the direction QGroundControl sends,
so that is what most of these pin: nothing reaches the aircraft unless the
operator turned commanding on, and what does reach it is whole frames only.
"""
from __future__ import annotations

import socket
import time

import pytest

from corvus.mavlink_forwarder import (
    DEFAULT_HOST,
    DEFAULT_LISTEN_HOST,
    DEFAULT_LISTEN_PORT,
    DEFAULT_PORT,
    MavlinkForwarder,
    frame_source_system,
    parse_endpoint,
    split_frames,
)


def _v1(payload_len: int = 3, seq: int = 0) -> bytes:
    """A MAVLink v1 frame: STX,len,seq,sys,comp,msgid + payload + 2 CRC."""
    return bytes([0xFE, payload_len, seq, 1, 1, 0]) + b"\x11" * payload_len + b"\xAB\xCD"


def _v2(payload_len: int = 4, signed: bool = False) -> bytes:
    """A MAVLink v2 frame, optionally carrying the 13-byte signature block."""
    incompat = 0x01 if signed else 0x00
    frame = bytes([0xFD, payload_len, incompat, 0, 0, 1, 1, 0, 0, 0])
    frame += b"\x22" * payload_len + b"\xAB\xCD"
    if signed:
        frame += b"\x99" * 13
    return frame


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def _forwarder(inject=None, *, listen_port: int, **kwargs) -> MavlinkForwarder:
    """A forwarder wired for a test: a known listen port, and a mirror target
    that is a dead port unless the test says otherwise.

    The target matters because the real default is 127.0.0.1:14550 and the
    forwarder now talks there from the first frame — which is the point of the
    feature and exactly what a test must not spray at the developer's own
    QGroundControl.
    """
    kwargs.setdefault("host", "127.0.0.1")
    kwargs.setdefault("port", _free_port())
    return MavlinkForwarder(
        inject, listen_host="127.0.0.1", listen_port=listen_port, **kwargs)


# ---------------------------------------------------------------------------
# Framing
# ---------------------------------------------------------------------------

def test_whole_frames_are_split_and_a_partial_one_is_held_back() -> None:
    a, b = _v1(), _v2()
    frames, rest = split_frames(a + b)
    assert frames == [a, b]
    assert rest == b""
    # A frame cut mid-flight is kept for the next datagram, never emitted.
    frames, rest = split_frames(a + b[:5])
    assert frames == [a]
    assert rest == b[:5]


def test_a_signed_v2_frame_keeps_its_signature() -> None:
    signed = _v2(signed=True)
    frames, rest = split_frames(signed)
    assert frames == [signed] and rest == b""
    assert len(signed) == 4 + 12 + 13


def test_garbage_resynchronises_on_the_next_start_byte() -> None:
    frame = _v1()
    frames, _ = split_frames(b"\x00\x01junk" + frame)
    assert frames == [frame], "the leading noise is skipped, not forwarded"


@pytest.mark.parametrize("text,expected", [
    ("127.0.0.1:14550", ("127.0.0.1", 14550)),
    ("192.168.1.5", ("192.168.1.5", DEFAULT_PORT)),
    ("  10.0.0.2:1 ", ("10.0.0.2", 1)),
    ("host:notaport", None),
    ("host:70000", None),
    ("", None),
])
def test_endpoints_parse_or_are_dropped(text: str, expected) -> None:
    """A typo costs that endpoint, never the whole feature."""
    assert parse_endpoint(text) == expected


# ---------------------------------------------------------------------------
# Outbound: the aircraft's frames reach a second station
# ---------------------------------------------------------------------------

def test_a_station_that_says_hello_is_learned_and_then_fed() -> None:
    port = _free_port()
    fwd = _forwarder(listen_port=port)
    assert fwd.start() is True
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    qgc.settimeout(2.0)
    try:
        qgc.sendto(_v1(), ("127.0.0.1", port))
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline and not fwd.status()["peers"]:
            time.sleep(0.02)
        assert fwd.status()["peers"], "the endpoint was learned from its own frame"

        frame = _v2()
        fwd.feed(frame)
        assert qgc.recv(1024) == frame, "telemetry arrives byte-for-byte"
    finally:
        qgc.close()
        fwd.stop()


def test_feeding_a_stopped_forwarder_is_a_no_op() -> None:
    """The receive loop calls feed() on every frame; a torn-down forwarder must
    absorb that silently rather than raise into the link."""
    fwd = _forwarder(listen_port=_free_port())
    fwd.feed(_v1())                        # never started
    assert fwd.start() is True
    fwd.stop()
    fwd.feed(_v1())                        # already stopped
    assert fwd.status()["running"] is False


# ---------------------------------------------------------------------------
# Inbound: commanding is off until the operator says otherwise
# ---------------------------------------------------------------------------

def test_nothing_reaches_the_aircraft_while_commanding_is_off() -> None:
    """The default. A second station is a screen until an operator decides it
    is a station — two places that can both arm one aircraft is a hazard, not
    a convenience, and no default can know it was wanted."""
    injected: list[bytes] = []
    port = _free_port()
    fwd = _forwarder(injected.append, listen_port=port)
    assert fwd.start() is True
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        qgc.sendto(_v1() + _v2(), ("127.0.0.1", port))
        time.sleep(0.3)
        assert injected == []
        assert fwd.status()["frames_injected"] == 0
    finally:
        qgc.close()
        fwd.stop()


def test_an_enabled_station_reaches_the_aircraft_frame_for_frame() -> None:
    injected: list[bytes] = []

    def inject(frame: bytes) -> bool:
        injected.append(frame)
        return True

    port = _free_port()
    fwd = _forwarder(inject, listen_port=port, allow_commands=True)
    assert fwd.start() is True
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        a, b = _v1(seq=7), _v2()
        qgc.sendto(a + b, ("127.0.0.1", port))
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline and len(injected) < 2:
            time.sleep(0.02)
        # Byte-for-byte: re-encoding here would break MAVLink signing and
        # scramble PX4's per-sender sequence tracking.
        assert injected == [a, b]
    finally:
        qgc.close()
        fwd.stop()


def test_a_half_frame_is_never_put_on_the_link() -> None:
    injected: list[bytes] = []
    port = _free_port()
    fwd = _forwarder(injected.append, listen_port=port, allow_commands=True)
    assert fwd.start() is True
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        frame = _v2()
        qgc.sendto(frame[:6], ("127.0.0.1", port))
        time.sleep(0.2)
        assert injected == [], "half a frame waits for its other half"
        qgc.sendto(frame[6:], ("127.0.0.1", port))
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline and not injected:
            time.sleep(0.02)
        assert injected == [frame]
    finally:
        qgc.close()
        fwd.stop()


# ---------------------------------------------------------------------------
# Lifecycle
# ---------------------------------------------------------------------------

def test_a_busy_listen_port_falls_back_instead_of_losing_the_mirror() -> None:
    """The listen port only matters to a station that dials IN. Mirroring
    outward — which is what the operator actually asked for — needs no fixed
    local port at all, so a taken one must not cost the whole feature. It used
    to: start() returned False and the LINK tab showed a switch that did
    nothing."""
    port = _free_port()
    holder = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    holder.bind(("127.0.0.1", port))
    fwd = _forwarder(listen_port=port)
    try:
        assert fwd.start() is True
        status = fwd.status()
        assert status["running"] is True
        assert status["listen_port"] != port, "an ephemeral port was taken"
        assert status["listen_port_requested"] == port
        assert not status["error"], "nothing failed, so nothing is an error"
        assert str(port) in status["notice"], "but the operator is told"
    finally:
        holder.close()
        fwd.stop()


def test_the_mirror_still_reaches_a_station_after_a_busy_listen_port() -> None:
    """The fallback is only worth having if the stream actually flows over it."""
    taken = _free_port()
    holder = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    holder.bind(("127.0.0.1", taken))
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    qgc.bind(("127.0.0.1", 0))
    qgc.settimeout(2.0)
    fwd = _forwarder(listen_port=taken, port=qgc.getsockname()[1])
    try:
        assert fwd.start() is True
        frame = _v2()
        fwd.feed(frame)
        assert qgc.recv(1024) == frame
    finally:
        holder.close()
        qgc.close()
        fwd.stop()


def test_stop_releases_the_socket_and_joins_its_threads() -> None:
    port = _free_port()
    fwd = _forwarder(listen_port=port)
    assert fwd.start() is True
    fwd.stop()
    fwd.stop()                              # idempotent
    assert fwd.status()["running"] is False
    # The port is free again: the field laptop is rebooted between flights and
    # a leaked socket would make the next launch silently drop forwarding.
    rebind = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        rebind.bind(("127.0.0.1", port))
    finally:
        rebind.close()


def test_the_defaults_are_loopback_and_the_port_qgroundcontrol_watches() -> None:
    assert DEFAULT_HOST == "127.0.0.1", "not every network the laptop joins"
    assert DEFAULT_PORT == 14550, "so neither end has to be configured"
    assert DEFAULT_LISTEN_HOST == "127.0.0.1"


def test_corvus_does_not_bind_the_port_the_other_station_listens_on() -> None:
    """The bug this pins is the whole feature quietly doing nothing.

    QGroundControl's default UDP link BINDS 14550 and waits to be spoken to.
    Corvus used to bind 14550 and wait too — on Linux the second bind fails and
    QGroundControl has no link, and on macOS both binds succeed under
    SO_REUSEADDR while the more specific 127.0.0.1 socket takes every datagram,
    so QGroundControl shows a connected link that receives nothing and neither
    program reports an error. Two listeners and no talker."""
    assert DEFAULT_LISTEN_PORT != DEFAULT_PORT


def test_the_default_configuration_reaches_a_default_qgroundcontrol() -> None:
    """End to end, with nothing configured on either side: QGroundControl binds
    14550 and stays silent, Corvus talks first, and the reply comes back."""
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    qgc.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        qgc.bind(("0.0.0.0", DEFAULT_PORT))
    except OSError:
        pytest.skip("something on this machine already owns port 14550")
    qgc.settimeout(2.0)
    injected: list[bytes] = []
    # Defaults only — the configuration an operator gets from ticking the box.
    fwd = MavlinkForwarder(
        lambda frame: bool(injected.append(frame)) or True, allow_commands=True)
    try:
        if not fwd.start():
            pytest.skip(f"cannot bind the default listen port: {fwd.error}")
        frame = _v2()
        fwd.feed(frame)
        # QGroundControl hears the aircraft without having been pointed at
        # anything, which is what "nothing to configure" has to mean.
        assert qgc.recv(1024) == frame
        sender = fwd.listen_address()
        assert sender[1] == fwd.status()["listen_port"]

        # QGroundControl's UDPLink adds any sender it hears from to its
        # targets, so its own frames come back to exactly that address.
        reply = _v1(seq=9)
        qgc.sendto(reply, sender)
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline and not injected:
            time.sleep(0.02)
        assert injected == [reply]
    finally:
        qgc.close()
        fwd.stop()


# ---------------------------------------------------------------------------
# Endpoints that would talk to us instead of to a second station
# ---------------------------------------------------------------------------

def test_the_forwarder_refuses_to_mirror_to_its_own_address() -> None:
    """Typing the listening address into the endpoint list is an easy mistake
    and an expensive one: every mirrored frame arrives straight back as an
    inbound datagram, and with commanding on that puts the aircraft's own
    telemetry onto the uplink — double the load on a 57 kbps radio, for
    nothing."""
    fwd = MavlinkForwarder(
        host="127.0.0.1", port=14550,
        listen_host="127.0.0.1", listen_port=14551,
        endpoints=["127.0.0.1:14551", "127.0.0.1:14552"],
    )
    assert fwd.status()["endpoints"] == ["127.0.0.1:14550", "127.0.0.1:14552"]


def test_the_station_corvus_was_told_about_is_a_target_without_being_typed() -> None:
    """host:port is where the other station listens, so it IS an endpoint —
    the one an operator never had to type, and the one that has to work when
    all they did was tick the box."""
    fwd = MavlinkForwarder(host="127.0.0.1", port=14550,
                           listen_host="127.0.0.1", listen_port=14551)
    assert fwd.status()["endpoints"] == ["127.0.0.1:14550"]


def test_a_datagram_from_our_own_socket_is_not_learned_as_a_peer() -> None:
    """A forwarder that learns itself would mirror the aircraft's telemetry
    back into its own inbound path, and with commanding on, onto the uplink."""
    port = _free_port()
    fwd = _forwarder(listen_port=port)
    assert fwd.start() is True
    try:
        # Send from the forwarder's own socket to itself: the datagram arrives
        # with the socket's own name as its source address.
        fwd._sock.sendto(_v1(), ("127.0.0.1", port))
        time.sleep(0.3)
        assert fwd.status()["peers"] == []

        # A genuine second station is still learned, so the guard above is not
        # passing because nothing is ever learned.
        other = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        other.sendto(_v1(), ("127.0.0.1", port))
        time.sleep(0.3)
        other.close()
        assert len(fwd.status()["peers"]) == 1
    finally:
        fwd.stop()


# ---------------------------------------------------------------------------
# Half-frame bookkeeping
# ---------------------------------------------------------------------------

def test_whole_datagrams_leave_no_reassembly_state_behind() -> None:
    """_rx_buf used to gain an empty entry per source address and lose none,
    which on a forwarder bound to 0.0.0.0 grows with every address the socket
    has ever seen."""
    port = _free_port()
    fwd = _forwarder(listen_port=port)
    assert fwd.start() is True
    try:
        for _ in range(5):
            sender = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sender.sendto(_v1() + _v2(), ("127.0.0.1", port))
            sender.close()
        time.sleep(0.3)
        assert fwd._rx_buf == {}, "nothing was left over, so nothing is kept"
    finally:
        fwd.stop()


def test_a_half_frame_is_kept_only_while_its_endpoint_is_still_talking() -> None:
    fwd = _forwarder(listen_port=0)
    fwd._rx_buf = {("10.0.0.%d" % i, 14550): b"\xfe" for i in range(3)}
    fwd._peers = {("10.0.0.1", 14550): time.monotonic()}

    fwd._drop_stale_rx_buffers()

    assert list(fwd._rx_buf) == [("10.0.0.1", 14550)]


# ---------------------------------------------------------------------------
# Two stations under one system id
# ---------------------------------------------------------------------------

def test_the_source_system_of_a_frame_is_read_from_both_wire_versions() -> None:
    assert frame_source_system(bytes([0xFE, 3, 0, 42, 1, 0]) + b"\x00" * 5) == 42
    assert frame_source_system(
        bytes([0xFD, 4, 0, 0, 0, 42, 1, 0, 0, 0]) + b"\x00" * 6) == 42
    assert frame_source_system(b"") == 0
    assert frame_source_system(b"\x00\x01\x02") == 0


def test_a_second_station_sharing_corvus_system_id_is_reported() -> None:
    """Corvus transmits as system 254 deliberately. A second station using the
    same id makes PX4 fold two senders' sequence numbers into one counter and
    report packet loss that is not happening — and leaves a GCS failsafe
    unable to say which station went away. Nothing here can fix it from this
    end, so it is surfaced where the operator will see it."""
    port = _free_port()
    fwd = _forwarder(lambda _f: True, listen_port=port, allow_commands=True)
    assert fwd.start() is True
    try:
        assert fwd.status()["sysid_conflict"] is False
        other = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        # A v1 frame whose sysid byte is 254 — Corvus' own.
        other.sendto(bytes([0xFE, 3, 0, 254, 190, 0]) + b"\x11" * 3 + b"\xAB\xCD",
                     ("127.0.0.1", port))
        time.sleep(0.3)
        other.close()
        assert fwd.status()["sysid_conflict"] is True
    finally:
        fwd.stop()


def test_the_system_id_clash_is_reported_with_commanding_off() -> None:
    """The default is telemetry-only, and the clash is just as real there.

    It is about sequence numbers on the DOWNLINK — PX4 folds two senders under
    one id into a single counter and reports packet loss that is not happening
    — so a station that only listens still causes it. The check used to sit
    BELOW the commanding gate, which made the LINK tab's promise to name the
    setting unreachable in every default installation."""
    port = _free_port()
    fwd = _forwarder(lambda _f: True, listen_port=port)     # commanding off
    assert fwd.start() is True
    try:
        other = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        other.sendto(bytes([0xFE, 3, 0, 254, 190, 0]) + b"\x11" * 3 + b"\xAB\xCD",
                     ("127.0.0.1", port))
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline and not fwd.status()["sysid_conflict"]:
            time.sleep(0.02)
        other.close()
        assert fwd.status()["sysid_conflict"] is True
        assert fwd.status()["frames_injected"] == 0, "still nothing on the uplink"
    finally:
        fwd.stop()


def test_a_station_with_its_own_system_id_raises_no_conflict() -> None:
    port = _free_port()
    fwd = _forwarder(lambda _f: True, listen_port=port, allow_commands=True)
    assert fwd.start() is True
    try:
        other = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        other.sendto(bytes([0xFE, 3, 0, 255, 190, 0]) + b"\x11" * 3 + b"\xAB\xCD",
                     ("127.0.0.1", port))
        time.sleep(0.3)
        other.close()
        assert fwd.status()["sysid_conflict"] is False
        assert fwd.status()["frames_injected"] == 1
    finally:
        fwd.stop()


# ---------------------------------------------------------------------------
# Who gets subscribed to the aircraft's telemetry
# ---------------------------------------------------------------------------

def test_a_datagram_that_is_not_mavlink_does_not_subscribe_its_sender() -> None:
    """A learned peer receives the whole stream at whatever rate the aircraft
    sends it. A port scan, another program's stale socket, or a broadcast
    picked up once listen_host is widened past loopback must not be able to
    sign itself up for that with one stray packet."""
    port = _free_port()
    fwd = _forwarder(listen_port=port)
    assert fwd.start() is True
    stray = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        stray.sendto(b"GET / HTTP/1.1\r\n\r\n", ("127.0.0.1", port))
        time.sleep(0.3)
        assert fwd.status()["peers"] == []
        assert fwd.status()["datagrams_received"] == 1, "it was read, just not trusted"

        # One that IS MAVLink still is — the guard rejects noise, it does not
        # stop the feature working.
        stray.sendto(_v1(), ("127.0.0.1", port))
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline and not fwd.status()["peers"]:
            time.sleep(0.02)
        assert len(fwd.status()["peers"]) == 1
    finally:
        stray.close()
        fwd.stop()


# ---------------------------------------------------------------------------
# Name resolution, off the send path
# ---------------------------------------------------------------------------

def test_endpoint_names_are_resolved_once_and_not_per_frame(monkeypatch) -> None:
    """sendto() resolves a name on every call, on the tx thread. One slow or
    unreachable DNS server then stalls the mirror for as long as the resolver
    takes, per frame — which overflows the outbound queue and drops the
    aircraft's telemetry to keep a name lookup company."""
    calls: list[str] = []
    real = socket.getaddrinfo

    def counting(host, port, *args, **kwargs):
        calls.append(str(host))
        return real(host, port, *args, **kwargs)

    monkeypatch.setattr(socket, "getaddrinfo", counting)
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    qgc.bind(("127.0.0.1", 0))
    qgc.settimeout(2.0)
    fwd = MavlinkForwarder(
        host="localhost", port=qgc.getsockname()[1],
        listen_host="127.0.0.1", listen_port=_free_port(),
    )
    try:
        assert fwd.start() is True
        after_start = len(calls)
        assert after_start >= 1, "resolved at start"
        for _ in range(20):
            fwd.feed(_v1())
        assert qgc.recv(1024) == _v1()
        # The address the socket was handed is already an address.
        assert fwd._resolved and fwd._resolved[0][0] == "127.0.0.1"
        assert len(calls) == after_start, "and not once more per frame"
    finally:
        qgc.close()
        fwd.stop()


def test_a_name_that_does_not_resolve_costs_that_endpoint_and_nothing_else() -> None:
    """An operator typo in the endpoint list must not take the mirror down."""
    qgc = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    qgc.bind(("127.0.0.1", 0))
    qgc.settimeout(2.0)
    fwd = MavlinkForwarder(
        host="127.0.0.1", port=qgc.getsockname()[1],
        listen_host="127.0.0.1", listen_port=_free_port(),
        endpoints=["no-such-host.invalid:14550"],
    )
    try:
        assert fwd.start() is True
        assert fwd._unresolved == [("no-such-host.invalid", 14550)]
        frame = _v2()
        fwd.feed(frame)
        assert qgc.recv(1024) == frame
    finally:
        qgc.close()
        fwd.stop()
