"""RC_CHANNELS parsing and the on-demand high-rate RC stream.

The Radio Control page's calibration is measured from these values and from
nothing else — PX4 has no autopilot-side RC calibration to narrate — so a
channel value that arrives wrong here becomes an endpoint written to the
vehicle. The assertions below are about the ways a frame can be wrong: a
channel the receiver does not deliver, a chancount that disagrees with the
payload, a receiver that reports no RSSI, and a link that dies mid-sweep.
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Callable

import pytest
from pymavlink import mavutil

from corvus import rc_config
from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)


class FakeMav:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.commands: list[tuple] = []
        self.on_send = on_send

    def command_long_send(self, *args: float) -> None:
        self.commands.append(args)
        if self.on_send:
            self.on_send(args)


class FakeConnection:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.mav = FakeMav(on_send)
        self.source_system = 255
        self.source_component = 190


def ready_bridge() -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    return bridge


def rc_msg(values: list[int], chancount: int | None = None,
           rssi: int = 70) -> FakeMessage:
    fields: dict[str, object] = {
        "message_type": "RC_CHANNELS",
        "time_boot_ms": 0,
        "chancount": len(values) if chancount is None else chancount,
        "rssi": rssi,
        "source_system": 1,
    }
    for index in range(1, 19):
        fields[f"chan{index}_raw"] = values[index - 1] if index <= len(values) else 65535
    return FakeMessage(**fields)


def raw_msg(values: list[int], port: int = 0, rssi: int = 255) -> FakeMessage:
    fields: dict[str, object] = {
        "message_type": "RC_CHANNELS_RAW",
        "time_boot_ms": 0,
        "port": port,
        "rssi": rssi,
        "source_system": 1,
    }
    for index in range(1, 9):
        fields[f"chan{index}_raw"] = values[index - 1] if index <= len(values) else 65535
    return FakeMessage(**fields)


# ---------------------------------------------------------------------------
# RC_CHANNELS
# ---------------------------------------------------------------------------

def test_rc_channels_publishes_the_pulses_and_marks_the_link_live() -> None:
    bridge = ready_bridge()
    bridge._dispatch(rc_msg([1100, 1500, 1900, 1234, 1000, 2000, 1500, 1500]))
    snap = bridge._store.get_snapshot()
    assert snap["rc_channels"] == [1100, 1500, 1900, 1234, 1000, 2000, 1500, 1500]
    assert snap["rc_channel_count"] == 8
    assert snap["rc_rssi"] == 70
    assert snap["rc_live"] is True


def test_the_list_is_trimmed_to_what_the_receiver_actually_delivers() -> None:
    bridge = ready_bridge()
    bridge._dispatch(rc_msg([1500] * 18, chancount=6))
    assert bridge._store.get_snapshot()["rc_channels"] == [1500] * 6


def test_chancount_is_honoured_even_when_the_extra_channels_are_empty() -> None:
    bridge = ready_bridge()
    bridge._dispatch(rc_msg([1500] * 4, chancount=12))
    snap = bridge._store.get_snapshot()
    # The receiver says twelve, so twelve are published: eight of them empty.
    # Trimming to the four that carried a value would hide a receiver that has
    # been configured for more channels than it is being fed.
    assert snap["rc_channels"] == [1500] * 4 + [0] * 8
    assert snap["rc_channel_count"] == 12


def test_a_chancount_past_the_payload_never_grows_the_list() -> None:
    bridge = ready_bridge()
    bridge._dispatch(rc_msg([1500] * 18, chancount=25))
    snap = bridge._store.get_snapshot()
    assert len(snap["rc_channels"]) == 18
    assert snap["rc_channel_count"] == 18


def test_an_undelivered_channel_is_zero_not_a_pinned_bar() -> None:
    bridge = ready_bridge()
    bridge._dispatch(rc_msg([1500, 65535, 1600], chancount=3))
    assert bridge._store.get_snapshot()["rc_channels"] == [1500, 0, 1600]


@pytest.mark.parametrize("reported, expected", [
    (255, -1),      # MAVLink's "unknown"
    (0, 0),         # a real zero: the link is there and terrible
    (70, 70),       # PX4 publishes a percentage
    (254, 100),     # a receiver on the raw 0-254 scale
    (127, 50),
])
def test_rssi_is_a_percentage_or_an_honest_unknown(reported: int, expected: int) -> None:
    bridge = ready_bridge()
    bridge._dispatch(rc_msg([1500], rssi=reported))
    assert bridge._store.get_snapshot()["rc_rssi"] == expected


def test_rc_channels_raw_banks_are_merged_not_swapped() -> None:
    bridge = ready_bridge()
    bridge._dispatch(raw_msg([1000, 1100, 1200, 1300, 1400, 1500, 1600, 1700], port=0))
    bridge._dispatch(raw_msg([1800, 1900], port=1))
    snap = bridge._store.get_snapshot()
    assert snap["rc_channels"][:8] == [1000, 1100, 1200, 1300, 1400, 1500, 1600, 1700]
    assert snap["rc_channels"][8:10] == [1800, 1900]


def test_the_last_bank_is_trimmed_to_the_channels_px4_defines() -> None:
    """Port 2 is channels 17-24; only the first two of those exist here.

    The offset guard admits the bank (16 < 18) but says nothing about where it
    ends, so the merge used to extend the published list to 24 entries and the
    Radio Control page drew six channels no firmware in range has.
    """
    bridge = ready_bridge()
    bridge._dispatch(raw_msg([1000] * 8, port=0))
    bridge._dispatch(raw_msg([1100] * 8, port=1))
    bridge._dispatch(raw_msg([1200, 1300, 1400, 1500, 1600, 1700, 1800, 1900], port=2))
    snap = bridge._store.get_snapshot()
    assert len(snap["rc_channels"]) == rc_config.MAX_CHANNELS
    assert snap["rc_channels"][16:] == [1200, 1300]
    assert snap["rc_channel_count"] == rc_config.MAX_CHANNELS


def test_a_bank_past_the_last_channel_is_ignored() -> None:
    bridge = ready_bridge()
    bridge._dispatch(rc_msg([1500] * 8))
    bridge._dispatch(raw_msg([1000], port=9))
    assert bridge._store.get_snapshot()["rc_channels"] == [1500] * 8


def test_a_message_from_another_vehicle_is_not_taken_as_ours() -> None:
    bridge = ready_bridge()
    msg = rc_msg([1500] * 8)
    msg.source_system = 42
    bridge._dispatch(msg)
    assert bridge._store.get_snapshot()["rc_live"] is False


def test_a_dropped_link_stops_the_channels_being_live() -> None:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    bridge._dispatch(rc_msg([1500] * 8))
    store.set_disconnected()
    # The last frame is kept (the page can still show what it last saw) but
    # nothing may keep measuring from it.
    assert store.get_snapshot()["rc_live"] is False


# ---------------------------------------------------------------------------
# set_rc_stream
# ---------------------------------------------------------------------------

def _accepting_bridge() -> MavlinkBridge:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(FakeMessage(
            message_type="COMMAND_ACK", command=int(args[2]),
            result=mavutil.mavlink.MAV_RESULT_ACCEPTED,
            target_system=255, target_component=190, source_system=1,
        ))

    bridge._conn = FakeConnection(on_send)
    return bridge


def test_enabling_asks_for_the_requested_interval() -> None:
    bridge = _accepting_bridge()
    assert bridge.set_rc_stream(True, 20) is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[2] == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL
    assert cmd[4] == pytest.approx(float(mavutil.mavlink.MAVLINK_MSG_ID_RC_CHANNELS))
    assert cmd[5] == pytest.approx(50000.0)  # 20 Hz


def test_disabling_hands_the_rate_back_and_clears_live() -> None:
    bridge = _accepting_bridge()
    bridge._dispatch(rc_msg([1500] * 8))
    assert bridge._store.get_snapshot()["rc_live"] is True

    assert bridge.set_rc_stream(False) is True
    assert bridge._conn.mav.commands[-1][5] == pytest.approx(0.0)
    assert bridge._store.get_snapshot()["rc_live"] is False


@pytest.mark.parametrize("rate", [0, 51, True, 12.5, "20"])
def test_a_bad_rate_sends_nothing(rate: object) -> None:
    bridge = _accepting_bridge()
    assert bridge.set_rc_stream(True, rate) is False  # type: ignore[arg-type]
    assert bridge._conn.mav.commands == []


def test_the_stream_is_safe_while_armed() -> None:
    bridge = _accepting_bridge()
    bridge._store.update(armed=True)
    # Read-only telemetry: unlike a parameter write, this must still work in
    # flight — checking a switch does what the operator thinks is exactly the
    # thing you want to do before relying on it.
    assert bridge.set_rc_stream(True, 10) is True


def test_the_stream_without_a_link_is_refused_with_a_reason() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.set_rc_stream(True, 20) is False
    # The page opened over a dead link is the common case for this call, so the
    # refusal has to name the link — an unexplained one reaches the browser as
    # a conflict rather than as "there is no vehicle".
    assert bridge.get_last_command_error() == "not connected"
