"""REQUEST_DATA_STREAM is a fallback, and now it waits to be needed.

Corvus used to send the deprecated REQUEST_DATA_STREAM set from _connect()
on every link, then configure the same streams again per message with
MAV_CMD_SET_MESSAGE_INTERVAL. On the supported targets (PX4 v1.16-v1.18) the
first request is pure noise, and on UDP the autopilot is rarely Corvus' alone
— MAVROS, a companion computer or mavlink-router are usually on it too, and a
blanket legacy rate request is the loudest thing a ground station can say to a
stack it did not set up.

So on UDP the legacy set is now sent only when the per-message requests come
back saying it is needed, and the COMMAND_ACK is what decides. Serial keeps
asking up front: point to point, nobody else to disturb, and a 57 kbps radio
must not hold the HUD empty for a command round trip.
"""
from __future__ import annotations

from types import SimpleNamespace as NS

from pymavlink import mavutil as mv

from corvus import mavlink_bridge as mb
from corvus.mavlink_bridge import INTERVAL_REQUEST_SILENT_LIMIT, MavlinkBridge
from corvus.state_store import VehicleStateStore


def _fake_conn(streams: list[tuple[int, int]]) -> NS:
    hb = mv.mavlink.MAVLink_heartbeat_message(
        type=mv.mavlink.MAV_TYPE_QUADROTOR, autopilot=mv.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=0, custom_mode=0, system_status=4, mavlink_version=3)
    hb.pack(mv.mavlink.MAVLink(None, srcSystem=1, srcComponent=1))
    return NS(
        wait_heartbeat=lambda blocking=True, timeout=None: hb,
        mode_mapping=lambda: None,
        target_system=1, target_component=1,
        mav=NS(
            request_data_stream_send=(
                lambda ts, tc, stream, rate, start: streams.append((stream, rate))
            ),
            command_long_send=lambda *a, **k: None,
            autopilot_version_request_send=lambda *a, **k: None,
        ),
    )


def _connect(bridge: MavlinkBridge, conn: NS, monkeypatch) -> None:
    monkeypatch.setattr(mb.mavutil, "mavlink_connection", lambda *a, **k: conn)
    bridge._connect()


# ---------------------------------------------------------------------------
# _connect
# ---------------------------------------------------------------------------

def test_udp_connect_sends_no_request_data_stream(monkeypatch) -> None:
    """The connect path is where the interference happened: a second station
    joining a shared autopilot rewrote rates for a link it did not own."""
    streams: list[tuple[int, int]] = []
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")

    _connect(bridge, _fake_conn(streams), monkeypatch)

    assert streams == []
    assert bridge._request_sent is False, "and the fallback is still available"


def test_serial_connect_still_asks_up_front(monkeypatch) -> None:
    """Nothing else is listening on a SiK radio, and waiting a command round
    trip for the first telemetry is a real cost there."""
    streams: list[tuple[int, int]] = []
    bridge = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")
    monkeypatch.setattr(MavlinkBridge, "_claim_serial_exclusive", lambda self, dev: None)

    _connect(bridge, _fake_conn(streams), monkeypatch)

    assert mv.mavlink.MAV_DATA_STREAM_EXTRA1 in {s for s, _ in streams}


# ---------------------------------------------------------------------------
# _request_message_intervals — who decides the fallback, and on what evidence
# ---------------------------------------------------------------------------

def _bridge_with_results(results: list[int], monkeypatch):
    """A UDP bridge whose SET_MESSAGE_INTERVAL answers are scripted."""
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")
    asked: list[int] = []
    fallbacks: list[int] = []
    answers = iter(results)

    def _interval(msg_id: int, interval_us: int, quiet: bool = False) -> tuple[bool, int]:
        assert quiet, "the connect-time batch must not report on the status bar"
        asked.append(msg_id)
        result = next(answers, results[-1])
        return result == mv.mavlink.MAV_RESULT_ACCEPTED, result

    monkeypatch.setattr(bridge, "_set_message_interval", _interval)
    monkeypatch.setattr(bridge, "_request_streams", lambda: fallbacks.append(1))
    return bridge, asked, fallbacks


def test_accepted_intervals_never_reach_for_the_legacy_request(monkeypatch) -> None:
    bridge, asked, fallbacks = _bridge_with_results(
        [mv.mavlink.MAV_RESULT_ACCEPTED], monkeypatch)

    bridge._request_message_intervals()

    assert asked == list(bridge._message_intervals())
    assert fallbacks == []


def test_unsupported_falls_back_and_stops_asking(monkeypatch) -> None:
    """UNSUPPORTED is the firmware saying it has never heard of the command —
    v1.12-v1.15. The rest of the list would spend a round trip each proving
    the same thing."""
    bridge, asked, fallbacks = _bridge_with_results(
        [mv.mavlink.MAV_RESULT_UNSUPPORTED], monkeypatch)

    bridge._request_message_intervals()

    assert len(asked) == 1
    assert fallbacks == [1]


def test_one_denied_message_does_not_condemn_the_modern_path(monkeypatch) -> None:
    """DENIED is a build that understood the command and refuses one message
    it does not carry. That is not grounds for shouting at every other client
    of the autopilot."""
    denied = [mv.mavlink.MAV_RESULT_DENIED] + [mv.mavlink.MAV_RESULT_ACCEPTED] * 16
    bridge, asked, fallbacks = _bridge_with_results(denied, monkeypatch)

    bridge._request_message_intervals()

    assert asked == list(bridge._message_intervals())
    assert fallbacks == []


def test_silence_sends_the_fallback_and_keeps_going_for_a_while(monkeypatch) -> None:
    """No ACK is the link saying nothing at all. The operator still needs
    telemetry, so the legacy request goes out — and the next message may well
    be answered, so one silence does not condemn the batch."""
    intervals = list(MavlinkBridge(
        VehicleStateStore(), "udp:0.0.0.0:14550")._message_intervals())
    # Silent, then answered, repeatedly: never three in a row.
    results = [-1, mv.mavlink.MAV_RESULT_ACCEPTED] * len(intervals)
    bridge, asked, fallbacks = _bridge_with_results(results, monkeypatch)

    bridge._request_message_intervals()

    assert asked == list(bridge._message_intervals())
    assert fallbacks, "insurance, once _request_streams' own latch has had it"


def test_a_link_answering_nothing_stops_rather_than_holding_the_command_lock(
    monkeypatch,
) -> None:
    """Total silence is its own answer, and waiting it out has a real cost.

    Each unanswered request spends two full ACK timeouts holding
    ``_operation_lock``, which is the same lock arm, land and RTL take — so
    walking the whole list on a link answering nothing parked every operator
    command behind a batch already known to be going nowhere. The fallback has
    been sent by then; the remaining messages only prove the same thing again.
    """
    bridge, asked, fallbacks = _bridge_with_results([-1], monkeypatch)

    bridge._request_message_intervals()

    assert len(asked) == INTERVAL_REQUEST_SILENT_LIMIT
    assert len(asked) < len(bridge._message_intervals())
    assert fallbacks, "the operator is still left with telemetry"


def test_a_torn_down_link_ends_the_batch(monkeypatch) -> None:
    bridge, asked, fallbacks = _bridge_with_results([-2], monkeypatch)

    bridge._request_message_intervals()

    assert len(asked) == 1
    assert fallbacks == []


# ---------------------------------------------------------------------------
# Coverage of the streams the legacy request used to carry
# ---------------------------------------------------------------------------

def test_udp_intervals_cover_rc_channels() -> None:
    """MAV_DATA_STREAM_RC_CHANNELS was the one group Corvus actually rendered
    that the per-message list did not ask for. Dropping the legacy request on
    UDP must not quietly drop the RC page with it."""
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")
    intervals = bridge._message_intervals()

    assert mv.mavlink.MAVLINK_MSG_ID_RC_CHANNELS in intervals
    assert intervals[mv.mavlink.MAVLINK_MSG_ID_RC_CHANNELS] <= 200_000, "5 Hz or better"


def test_the_serial_set_still_leaves_rc_off_the_radio() -> None:
    """57 kbps buys attitude and position instead; the serial stream rates
    have always asked for RC at 0."""
    bridge = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")

    assert mv.mavlink.MAVLINK_MSG_ID_RC_CHANNELS not in bridge._message_intervals()
    assert bridge._stream_rates()[mv.mavlink.MAV_DATA_STREAM_RC_CHANNELS] == 0


# ---------------------------------------------------------------------------
# The port Corvus is holding
# ---------------------------------------------------------------------------

def test_binding_px4s_onboard_port_says_so(monkeypatch) -> None:
    """Linux refuses the second bind and the EADDRINUSE message explains it;
    macOS lets it through and MAVROS simply goes quiet. That case needs a line
    of its own, or the operator debugs an autopilot that is working fine."""
    lines: list[tuple[str, str, str]] = []
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    monkeypatch.setattr(bridge, "_console_publish",
                        lambda name, text, level: lines.append((name, text, level)))

    _connect(bridge, _fake_conn([]), monkeypatch)

    assert [t for _n, t, _l in lines if "14550" in t], "and it names the way out"
    assert all(level == "warning" for _n, _t, level in lines)


def test_the_ground_station_port_is_not_warned_about(monkeypatch) -> None:
    lines: list[tuple[str, str, str]] = []
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")
    monkeypatch.setattr(bridge, "_console_publish",
                        lambda name, text, level: lines.append((name, text, level)))

    _connect(bridge, _fake_conn([]), monkeypatch)

    assert lines == []


def test_an_outbound_link_takes_nobody_s_socket(monkeypatch) -> None:
    """udpout: names somebody else's listening socket rather than binding."""
    lines: list[tuple[str, str, str]] = []
    bridge = MavlinkBridge(VehicleStateStore(), "udpout:127.0.0.1:14540")
    monkeypatch.setattr(bridge, "_console_publish",
                        lambda name, text, level: lines.append((name, text, level)))

    _connect(bridge, _fake_conn([]), monkeypatch)

    assert lines == []


def test_the_explicit_spelling_of_a_bind_is_warned_about_too(monkeypatch) -> None:
    """``udpin:`` is ``udp:`` written out — pymavlink gives both input=True —
    so it takes the companion port the same way. The check used to read
    ``udp:`` alone, which meant an operator who spelled the bind out held
    MAVROS' socket with no warning at all.
    """
    lines: list[tuple[str, str, str]] = []
    bridge = MavlinkBridge(VehicleStateStore(), "udpin:0.0.0.0:14540")
    monkeypatch.setattr(bridge, "_console_publish",
                        lambda name, text, level: lines.append((name, text, level)))

    _connect(bridge, _fake_conn([]), monkeypatch)

    assert [t for _n, t, _l in lines if "14550" in t]


def test_a_broadcast_link_is_still_nobody_s_socket(monkeypatch) -> None:
    """``udpbcast:`` dials out from an ephemeral local port, like ``udpout:``:
    widening the warning must not start crying wolf about the forms that take
    nothing from anyone."""
    lines: list[tuple[str, str, str]] = []
    bridge = MavlinkBridge(VehicleStateStore(), "udpbcast:255.255.255.255:14540")
    monkeypatch.setattr(bridge, "_console_publish",
                        lambda name, text, level: lines.append((name, text, level)))

    _connect(bridge, _fake_conn([]), monkeypatch)

    assert lines == []
