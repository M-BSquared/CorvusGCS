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
    fwd = MavlinkForwarder(host="127.0.0.1", port=port)
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
    fwd = MavlinkForwarder(host="127.0.0.1", port=_free_port())
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
    fwd = MavlinkForwarder(injected.append, host="127.0.0.1", port=port)
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
    fwd = MavlinkForwarder(inject, host="127.0.0.1", port=port, allow_commands=True)
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
    fwd = MavlinkForwarder(injected.append, host="127.0.0.1", port=port,
                           allow_commands=True)
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

def test_a_busy_port_disables_forwarding_instead_of_failing_the_launch() -> None:
    """A field laptop must come up with telemetry working even when the port
    it wanted is already taken — by QGroundControl itself, most likely."""
    port = _free_port()
    holder = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    holder.bind(("127.0.0.1", port))
    fwd = MavlinkForwarder(host="127.0.0.1", port=port)
    try:
        assert fwd.start() is False
        assert str(port) in fwd.error
        assert fwd.status()["running"] is False
    finally:
        holder.close()
        fwd.stop()


def test_stop_releases_the_socket_and_joins_its_threads() -> None:
    port = _free_port()
    fwd = MavlinkForwarder(host="127.0.0.1", port=port)
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
        endpoints=["127.0.0.1:14550", "127.0.0.1:14551"],
    )
    assert fwd.status()["endpoints"] == ["127.0.0.1:14551"]


def test_a_datagram_from_our_own_socket_is_not_learned_as_a_peer() -> None:
    """A forwarder that learns itself would mirror the aircraft's telemetry
    back into its own inbound path, and with commanding on, onto the uplink."""
    port = _free_port()
    fwd = MavlinkForwarder(host="127.0.0.1", port=port)
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
    fwd = MavlinkForwarder(host="127.0.0.1", port=port)
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
    fwd = MavlinkForwarder(host="127.0.0.1", port=0)
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
    fwd = MavlinkForwarder(lambda _f: True, host="127.0.0.1", port=port,
                           allow_commands=True)
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


def test_a_station_with_its_own_system_id_raises_no_conflict() -> None:
    port = _free_port()
    fwd = MavlinkForwarder(lambda _f: True, host="127.0.0.1", port=port,
                           allow_commands=True)
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
