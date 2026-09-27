"""The bridge's half of the forwarding-loop fix, and the notification noise.

Seen with PX4 v1.18 SITL on udp:0.0.0.0:14550 and forwarding to
127.0.0.1:14550: every frame came back through the forwarder. Old HEARTBEATs
made the mode and armed state flicker, and Corvus' own commands went round and
reached PX4 again and again ("Already higher than takeoff altitude",
"Executing Mission" dozens of times after one fly-to). The forwarder no longer
mirrors into the link; these pin the defences behind that.
"""
from __future__ import annotations

import socket
import time
from types import SimpleNamespace

from corvus import autopilot
from corvus.mavlink_bridge import DUPLICATE_FRAME_WINDOW_S, GCS_SYSTEM_ID, MavlinkBridge
from corvus.state_store import VehicleStateStore


class RawMessage:
    def __init__(self, raw: bytes) -> None:
        self.raw = raw

    def get_msgbuf(self) -> bytes:
        return self.raw


def frame(sysid: int, seq: int = 0) -> bytes:
    return bytes([0xFE, 3, seq, sysid, 1, 0]) + b"\x11" * 3 + b"\xAB\xCD"


def bridge() -> MavlinkBridge:
    b = MavlinkBridge(VehicleStateStore())
    b._target_system = 1
    return b


class WritingConn:
    def __init__(self) -> None:
        self.written: list[bytes] = []

    def write(self, data: bytes) -> None:
        self.written.append(data)


def test_the_same_frame_twice_in_quick_succession_is_dropped() -> None:
    b = bridge()
    assert b._is_duplicate_frame(RawMessage(frame(1, seq=5))) is False
    assert b._is_duplicate_frame(RawMessage(frame(1, seq=5))) is True
    assert b._is_duplicate_frame(RawMessage(frame(1, seq=6))) is False, (
        "the next frame differs at least by its sequence number"
    )


def test_a_repeat_after_the_window_is_news_again() -> None:
    b = bridge()
    msg = RawMessage(frame(1, seq=9))
    assert b._is_duplicate_frame(msg) is False
    b._recent_frames[msg.raw] = time.monotonic() - DUPLICATE_FRAME_WINDOW_S - 0.1
    assert b._is_duplicate_frame(msg) is False


def test_a_message_without_raw_bytes_is_never_dropped() -> None:
    b = bridge()
    assert b._is_duplicate_frame(SimpleNamespace()) is False
    assert b._is_duplicate_frame(SimpleNamespace()) is False


def test_the_duplicate_memory_stays_small() -> None:
    b = bridge()
    stale = time.monotonic() - 10.0
    b._recent_frames = {frame(1, seq=i): stale for i in range(200)}
    b._recent_frames_swept = stale
    b._is_duplicate_frame(RawMessage(frame(2)))
    assert len(b._recent_frames) == 1


def test_a_second_station_frame_is_written_to_the_vehicle() -> None:
    b = bridge()
    b._conn = WritingConn()
    assert b.inject_raw(frame(255)) is True
    assert b._conn.written == [frame(255)]


def test_our_own_frames_are_never_put_back_on_the_uplink() -> None:
    b = bridge()
    b._conn = WritingConn()
    assert b.inject_raw(frame(GCS_SYSTEM_ID)) is False
    assert b._conn.written == []


def test_the_vehicles_own_frames_are_never_sent_back_to_it() -> None:
    b = bridge()
    b._conn = WritingConn()
    assert b.inject_raw(frame(1)) is False
    assert b._conn.written == []


def test_a_listening_udp_link_names_where_it_is_bound() -> None:
    b = bridge()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    try:
        b._conn = SimpleNamespace(udp_server=True, port=sock)
        assert b.local_udp_address() == ("127.0.0.1", sock.getsockname()[1])
    finally:
        sock.close()


def test_other_links_have_no_udp_address() -> None:
    b = bridge()
    assert b.local_udp_address() is None
    b._conn = SimpleNamespace(udp_server=False, port=None)
    assert b.local_udp_address() is None
    b._conn = SimpleNamespace(udp_server=True, port=SimpleNamespace())
    assert b.local_udp_address() is None


def test_px4_log_file_lines_stay_in_the_console_only() -> None:
    b = bridge()
    lines: list[dict] = []
    b.add_console_sub(lines.append)
    b._publish_statustext("[logger] ./log/2026-09-27/19_51_37.ulg", 6)
    assert [line["text"] for line in lines] == ["[logger] ./log/2026-09-27/19_51_37.ulg"]
    assert b._store.get_snapshot()["warnings"] == []


def test_a_logger_warning_still_reaches_the_board() -> None:
    b = bridge()
    b._publish_statustext("[logger] SD card full", 4)
    assert [w["msg"] for w in b._store.get_snapshot()["warnings"]] == [
        "[logger] SD card full"]


def test_ardupilot_lines_are_not_filtered_by_px4_rules() -> None:
    b = bridge()
    b._dialect = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT)
    b._publish_statustext("[logger] something", 6)
    assert len(b._store.get_snapshot()["warnings"]) == 1
