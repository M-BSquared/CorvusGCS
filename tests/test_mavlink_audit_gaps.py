"""The candidate gaps from `MAVLINK_CONNECTION_AUDIT.md` §8.2, checked.

An audit that ends in a list of "verify this" items is only half finished. Most
of that list turned out to describe behaviour that is correct and deliberate —
but "correct and deliberate" is a claim about intent, and intent that is not
written down as a test is intent that the next change quietly reverses.

So each gap gets a test that states what the behaviour is and why it is the one
that was chosen. Two of them found something:

  * **(a)** was a real defect — the onboard-port warning matched ``udp:`` only,
    so the explicit spelling of the same bind held a companion process' socket
    in silence. Fixed; the regression tests live with the rest of the
    onboard-port checks in ``test_mavlink_stream_fallback.py``.
  * **(i)** was real too, in the frontend rather than the backend: the forwarder
    has counted refused commands and named the address from the start, and
    nothing ever showed it. Now surfaced in the LINK tab
    (``test_frontend_link.js``).

The rest — (b), (c), (d), (e), (f), (g), (h) — are pinned here and in
``test_mavlink_router.py``, with the reasoning that makes each one a decision
rather than an accident.
"""
from __future__ import annotations

import socket

import pytest

pytest.importorskip("pymavlink")

from pymavlink import mavutil as mv  # noqa: E402

from corvus import mavlink_bridge as mb  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402


def bridge(conn: str = "udp:127.0.0.1:14550") -> MavlinkBridge:
    return MavlinkBridge(VehicleStateStore(), conn)


# ---------------------------------------------------------------------------
# (b) Every default is a BIND, so two ground stations on one port is an OS coin
#     toss — which is why the forwarder, not a second bind, is the way to run
#     QGroundControl alongside Corvus.
# ---------------------------------------------------------------------------

def test_every_default_connection_binds_rather_than_dials():
    """All three defaults are bind forms. That is the fact the coexistence
    guidance rests on: if any of them dialled out instead, "just point the
    second station at the same port" would work and the forwarder would be
    optional. It does not, and it is not."""
    from corvus.autoconnect import UDP_FALLBACK_CONNECTION
    from corvus.config import CorvusConfig
    from corvus.server import DEFAULT_MAVLINK_CONNECTION

    for conn in (
        DEFAULT_MAVLINK_CONNECTION,
        CorvusConfig().mavlink_connection,
        UDP_FALLBACK_CONNECTION,
    ):
        assert conn.startswith(("udp:", "udpin:")), (
            f"{conn} is not a binding form; the QGC coexistence note depends on it"
        )


def test_two_ground_stations_on_one_udp_port_is_never_two_working_links():
    """The trap in §7, demonstrated on real sockets. Linux refuses the second
    bind outright; macOS accepts it and hands the datagrams to one socket
    silently. Neither outcome is two fed stations, which is the whole reason
    Corvus binds 14551 and mirrors to 14550 instead of binding it twice."""
    first = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    first.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    first.bind(("127.0.0.1", 0))
    port = first.getsockname()[1]

    second = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    second.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        try:
            second.bind(("127.0.0.1", port))
        except OSError:
            # Linux: the second binder is told. This is the good case — the
            # EADDRINUSE rewrite turns it into a sentence naming the likely
            # holder.
            return
        # macOS: both sockets exist and exactly one of them will be fed. Prove
        # the silent half — a datagram arrives at one, not both.
        sender = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            sender.sendto(b"x", ("127.0.0.1", port))
            first.settimeout(0.25)
            second.settimeout(0.25)
            got = 0
            for sock in (first, second):
                try:
                    sock.recv(16)
                    got += 1
                except OSError:
                    pass
            assert got <= 1, (
                "both sockets were fed, which would make the coexistence "
                "warning unnecessary — re-read §7 if this ever fires"
            )
        finally:
            sender.close()
    finally:
        first.close()
        second.close()


def test_the_busy_port_message_names_who_is_probably_holding_it(monkeypatch):
    """An errno is not an answer at a field. "[Errno 48] Address already in
    use" is true and tells the operator nothing they can act on; the rewrite
    names the three programs that actually take this port, and says what to do.

    Driven by injecting the error rather than by racing a real bind, because
    the two platforms produce it under different conditions (§7) and the
    message must not depend on which one you are standing in front of."""
    import errno as _errno

    b = bridge("udp:0.0.0.0:14550")
    busy = OSError(_errno.EADDRINUSE, "Address already in use")

    def refuse(*_a, **_k):
        raise busy

    monkeypatch.setattr(mb.mavutil, "mavlink_connection", refuse)

    with pytest.raises(ConnectionError) as caught:
        b._connect()

    text = str(caught.value)
    assert "14550" in text, "names the port that is taken"
    for who in ("QGroundControl", "MAVROS", "Corvus"):
        assert who in text, f"names {who} as a likely holder"
    assert "different port" in text, "and says what to do about it"
    # The same sentence reaches the LINK tab, not only the log.
    assert b._store.get_snapshot()["link_error"] == text


def test_a_serial_device_that_is_not_there_says_that_instead(monkeypatch):
    """The same except branch serves both, and a missing cable must not be
    reported as a busy port."""
    b = bridge("serial:/dev/ttyACM9:57600")

    def missing(*_a, **_k):
        raise FileNotFoundError(2, "No such file or directory")

    monkeypatch.setattr(mb.mavutil, "mavlink_connection", missing)

    with pytest.raises(ConnectionError) as caught:
        b._connect()
    assert "serial device unavailable" in str(caught.value)
    assert "/dev/ttyACM9" in str(caught.value)


# ---------------------------------------------------------------------------
# (e) The display tier and the dispatch tier disagree on purpose.
# ---------------------------------------------------------------------------

def test_a_briefly_degraded_link_is_shown_as_degraded_and_still_accepts_commands():
    """Two thresholds, two jobs. The A3 pair drives what the operator SEES
    (3 s to degraded on UDP, 8 s to a teardown); the legacy pair gates what the
    operator may SEND (5 s). In the 3-5 s window the link reads degraded and
    still commands, and that is the intended answer: a momentarily late
    heartbeat is a reason to warn, not a reason to refuse an RTL."""
    b = bridge("udp:127.0.0.1:14550")

    warn = b._warn_timeout()
    drop = b._drop_timeout()
    command_gate = b._heartbeat_timeout()

    assert warn < command_gate < drop, (
        f"warn {warn} / command gate {command_gate} / drop {drop}: the gate has "
        "to sit between them, or the UI and the dispatcher are describing "
        "different links"
    )
    # Named explicitly so a change to any single constant fails here rather
    # than silently re-ordering the tiers.
    assert (warn, command_gate, drop) == (3.0, 5.0, 8.0)


def test_a_serial_link_is_given_longer_on_every_tier_than_a_udp_one():
    """A SiK radio drops frames for reasons a loopback socket never does."""
    serial = bridge("serial:/dev/ttyUSB0:57600")
    udp = bridge("udp:127.0.0.1:14550")
    assert serial._warn_timeout() > udp._warn_timeout()
    assert serial._drop_timeout() > udp._drop_timeout()
    assert serial._heartbeat_timeout() > udp._heartbeat_timeout()


# ---------------------------------------------------------------------------
# (g) System id 0 is accepted, and the autopilot tier is the backstop.
# ---------------------------------------------------------------------------

def _heartbeat(sysid: int, compid: int = 1):
    msg = mv.mavlink.MAVLink_heartbeat_message(
        type=mv.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mv.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=0, custom_mode=0,
        system_status=mv.mavlink.MAV_STATE_STANDBY, mavlink_version=3,
    )
    msg.pack(mv.mavlink.MAVLink(None, srcSystem=sysid, srcComponent=compid))
    return msg


def test_an_unidentified_frame_is_accepted_because_bad_data_carries_no_sender():
    """pymavlink's BAD_DATA and synthetic test messages have no source id.
    Rejecting them would throw away the parser's own error reporting, so sysid
    0 is let through by design."""
    b = bridge()
    b._target_system, b._target_component = 3, 1
    assert b._is_from_vehicle(_heartbeat(0)) is True


def test_a_frame_from_a_different_aircraft_is_not():
    b = bridge()
    b._target_system, b._target_component = 3, 1
    assert b._is_from_vehicle(_heartbeat(7)) is False


def test_the_autopilot_tier_is_what_guards_mode_and_armed():
    """This is the backstop that makes accepting sysid 0 safe. A gimbal under
    the aircraft's own system id passes ``_is_from_vehicle`` — it IS the
    vehicle — and must still not be allowed to write the flight mode."""
    b = bridge()
    b._target_system, b._target_component = 3, 1

    gimbal = _heartbeat(3, mv.mavlink.MAV_COMP_ID_GIMBAL)
    assert b._is_from_vehicle(gimbal) is True, "same aircraft"
    assert b._is_from_autopilot(gimbal) is False, "but not the autopilot"

    autopilot = _heartbeat(3, mv.mavlink.MAV_COMP_ID_AUTOPILOT1)
    assert b._is_from_autopilot(autopilot) is True


# ---------------------------------------------------------------------------
# (h) An existing config keeps the port it names — including the awkward one.
# ---------------------------------------------------------------------------

def test_a_config_written_before_the_default_moved_keeps_its_own_port(tmp_path):
    """Auto-changing the link out from under an operator is worse than the
    problem it would solve: the file may name 14540 because a companion
    computer is not on this machine at all."""
    from corvus.config import load_config

    path = tmp_path / "config.json"
    path.write_text('{"mavlink_connection": "udp:0.0.0.0:14540"}')
    assert load_config(str(path)).mavlink_connection == "udp:0.0.0.0:14540"


def test_auto_connect_does_not_quietly_rewrite_that_port_either(tmp_path):
    """The new resolver could have "helpfully" moved a stale 14540 config to
    14550. It must not: an explicitly configured string outranks the fallback,
    and only real hardware outranks it. The operator gets the warning and keeps
    the link they configured."""
    from corvus import autoconnect as ac

    decision = ac.resolve_startup_connection(
        None, "udp:0.0.0.0:14540", ac.classify_ports([]),
    )
    assert decision.connection_string == "udp:0.0.0.0:14540"
    assert decision.reason == ac.REASON_CONFIGURED


def test_and_the_operator_is_told_what_that_port_costs(monkeypatch):
    """Warned, not overruled — and now on both spellings of the bind."""
    for conn in ("udp:0.0.0.0:14540", "udpin:0.0.0.0:14540"):
        lines: list[str] = []
        b = bridge(conn)
        monkeypatch.setattr(b, "_console_publish",
                            lambda n, t, lvl: lines.append(t))
        b._warn_if_onboard_port()
        assert any("14550" in t for t in lines), f"{conn} warned and named the way out"


# ---------------------------------------------------------------------------
# (i) A station that is refused is named, and now it is named where the
#     operator is looking rather than only in a log file.
# ---------------------------------------------------------------------------

def test_the_forwarder_status_carries_both_the_count_and_the_address():
    """The frontend cannot show what the backend does not report. This is the
    contract the LINK tab's new line reads."""
    from corvus.mavlink_forwarder import MavlinkForwarder

    fwd = MavlinkForwarder(lambda _raw: True, allow_commands=True)
    status = fwd.status()
    assert "commands_refused" in status
    assert "commands_refused_from" in status
    assert isinstance(status["commands_refused_from"], list)
