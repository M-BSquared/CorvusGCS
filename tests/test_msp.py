"""MSP: reading a Betaflight flight controller (corvus/msp.py and its routes)."""
from __future__ import annotations

import struct
from typing import Any

import pytest

from corvus import msp
from corvus.msp import MspError, MspParser, MspSession, build_request, read_with_session
from corvus.server import CorvusHandler


def reply(command: int, payload: bytes = b"", error: bool = False) -> bytes:
    body = bytes([len(payload), command]) + payload
    checksum = 0
    for byte in body:
        checksum ^= byte
    return (b"$M!" if error else b"$M>") + body + bytes([checksum])


def betaflight_answers() -> dict[int, bytes]:
    pids = bytes([45, 80, 30, 47, 84, 35, 45, 80, 0]) + bytes(12) + bytes([50, 75, 0])
    board = b"S405" + struct.pack("<H", 0) + bytes([1, 0, 8]) + b"SPEEDYF4" + b"\x00"
    return {
        msp.MSP_API_VERSION: bytes([0, 1, 46]),
        msp.MSP_FC_VARIANT: b"BTFL",
        msp.MSP_FC_VERSION: bytes([4, 5, 1]),
        msp.MSP_BOARD_INFO: board,
        msp.MSP_BUILD_INFO: b"Oct 28 2026" + b"12:00:00" + b"abcdef1",
        msp.MSP_NAME: b"racer",
        msp.MSP_STATUS_EX: struct.pack("<HHHIBHBB", 125, 0, 0b100011, 0, 1, 12, 4, 0),
        msp.MSP_PID: pids,
        msp.MSP_RC_TUNING: bytes([100, 0, 70, 70, 70, 65, 50, 0]) + struct.pack("<H", 1350),
        msp.MSP_BATTERY_STATE: struct.pack("<BHBHhBH", 4, 1500, 160, 120, 250, 1, 1620),
    }


class FakeLink:
    """A flight controller on the other end of a byte link."""

    def __init__(self, answers: dict[int, bytes], errors: set[int] = frozenset(),
                 silent: bool = False) -> None:
        self.answers, self.errors, self.silent = answers, set(errors), silent
        self.pending = b""
        self.closed = False
        self.requests: list[int] = []
        self.now = 0.0

    def reset_input_buffer(self) -> None:
        self.pending = b""

    def write(self, data: bytes) -> None:
        assert data[:3] == b"$M<"
        command = data[4]
        self.requests.append(command)
        if self.silent:
            return
        if command in self.errors or command not in self.answers:
            self.pending += reply(command, b"", error=True)
        else:
            self.pending += reply(command, self.answers[command])

    def read(self, size: int = 1) -> bytes:
        self.now += 0.05
        data, self.pending = self.pending[:size], self.pending[size:]
        return data

    def close(self) -> None:
        self.closed = True


def session_for(link: FakeLink) -> MspSession:
    return MspSession(link, clock=lambda: link.now)


# --- wire format ----------------------------------------------------------

def test_a_request_is_framed_and_checksummed():
    assert build_request(msp.MSP_API_VERSION) == b"$M<\x00\x01\x01"
    assert build_request(112) == b"$M<\x00\x70\x70"
    with pytest.raises(ValueError):
        build_request(300)


def test_the_parser_reads_a_frame_split_across_reads_and_after_noise():
    frame = reply(msp.MSP_FC_VERSION, bytes([4, 5, 1]))
    parser = MspParser()
    assert parser.feed(b"garbage$" + frame[:4]) == []
    frames = parser.feed(frame[4:])
    assert [(f.command, f.payload, f.error) for f in frames] == [(3, bytes([4, 5, 1]), False)]


def test_the_parser_drops_a_bad_checksum_and_recovers():
    good = reply(msp.MSP_FC_VARIANT, b"BTFL")
    bad = bytearray(good)
    bad[-1] ^= 0xFF
    parser = MspParser()
    frames = parser.feed(bytes(bad) + good)
    assert [f.command for f in frames] == [msp.MSP_FC_VARIANT]
    assert parser.checksum_errors == 1


def test_an_error_reply_is_marked():
    (frame,) = MspParser().feed(reply(msp.MSP_NAME, b"", error=True))
    assert frame.error


def test_a_mavlink_burst_is_not_mistaken_for_msp():
    assert MspParser().feed(bytes([0xFD, 0x09, 0x00, 0x00, 0x00, 0x01, 0x01]) * 10) == []


# --- reading a controller -------------------------------------------------

def test_a_betaflight_controller_is_read_in_full():
    result = read_with_session(session_for(FakeLink(betaflight_answers())))
    assert result["is_betaflight"] is True
    assert result["variant_name"] == "Betaflight"
    assert result["version"] == "4.5.1" and result["api"] == "1.46"
    assert result["board"] == "S405" and result["target"] == "SPEEDYF4"
    assert result["build"]["revision"] == "abcdef1"
    assert result["name"] == "racer"
    assert result["sensors"] == ["gyro", "accelerometer", "barometer"]
    assert result["cycle_time_us"] == 125 and result["cpu_load"] == 12
    assert result["pid_profile"] == 2 and result["pid_profile_count"] == 4
    assert result["rate_profile"] == 1
    assert result["pids"]["roll"] == {"p": 45, "i": 80, "d": 30}
    assert result["pids"]["yaw"] == {"p": 45, "i": 80, "d": 0}
    assert result["pids"]["level"] == {"p": 50, "i": 75, "d": 0}
    assert result["rates"]["roll"] == 70 and result["rates"]["tpa_breakpoint"] == 1350
    assert result["battery"] == {
        "cells": 4, "capacity_mah": 1500, "voltage": 16.2, "used_mah": 120, "current": 2.5,
    }
    assert result["unanswered"] == []


def test_what_the_firmware_does_not_know_is_left_out_not_fatal():
    answers = betaflight_answers()
    link = FakeLink(answers, errors={msp.MSP_BATTERY_STATE, msp.MSP_NAME})
    result = read_with_session(session_for(link))
    assert "battery" not in result and "name" not in result
    assert sorted(result["unanswered"]) == [msp.MSP_NAME, msp.MSP_BATTERY_STATE]
    assert result["version"] == "4.5.1"


def test_a_short_reply_is_left_out_not_fatal():
    answers = betaflight_answers()
    answers[msp.MSP_PID] = b"\x01\x02"
    result = read_with_session(session_for(FakeLink(answers)))
    assert "pids" not in result and msp.MSP_PID in result["unanswered"]


def test_another_variant_is_reported_as_what_it_says_it_is():
    answers = betaflight_answers()
    answers[msp.MSP_FC_VARIANT] = b"INAV"
    result = read_with_session(session_for(FakeLink(answers)))
    assert result["variant_name"] == "iNav" and result["is_betaflight"] is False


def test_a_silent_port_is_reported_with_what_to_do():
    with pytest.raises(MspError, match="USB port"):
        read_with_session(session_for(FakeLink({}, silent=True)))


def test_nothing_is_written_but_questions():
    link = FakeLink(betaflight_answers())
    read_with_session(session_for(link))
    assert set(link.requests) <= {
        msp.MSP_API_VERSION, msp.MSP_FC_VARIANT, msp.MSP_FC_VERSION, msp.MSP_BOARD_INFO,
        msp.MSP_BUILD_INFO, msp.MSP_NAME, msp.MSP_STATUS_EX, msp.MSP_PID,
        msp.MSP_RC_TUNING, msp.MSP_BATTERY_STATE,
    }


def test_the_port_is_closed_after_a_read_and_after_a_failure():
    link = FakeLink(betaflight_answers())
    msp.read_flight_controller("/dev/ttyACM0", opener=lambda d, b: link)
    assert link.closed
    silent = FakeLink({}, silent=True)
    with pytest.raises(MspError):
        msp.read_flight_controller(
            "/dev/ttyACM0", opener=lambda d, b: silent, clock=lambda: silent.now,
        )
    assert silent.closed


def test_a_port_that_will_not_open_is_an_operator_error():
    def refuse(device: str, baud: int) -> Any:
        raise OSError("busy")

    with pytest.raises(MspError, match="could not open"):
        msp.read_flight_controller("/dev/ttyACM0", opener=refuse)


# --- the routes -----------------------------------------------------------

class _Bridge:
    def __init__(self, link: str = "", ports: list[dict] | None = None) -> None:
        self._link, self._ports = link, ports or []

    def serial_device(self) -> str:
        return self._link

    def list_serial_ports(self) -> list[dict]:
        return self._ports


def _handler(bridge: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))
    return handler, responses


def test_the_status_marks_the_port_the_link_holds():
    ports = [
        {"device": "/dev/ttyACM0", "description": "STM32 Virtual ComPort", "hwid": "USB VID:PID=0483:5740"},
        {"device": "/dev/ttyUSB0", "description": "CP2102", "hwid": "USB VID:PID=10C4:EA60"},
    ]
    handler, out = _handler(_Bridge(link="/dev/ttyUSB0", ports=ports))
    handler._api_betaflight_status()
    data, status = out[0]
    assert status == 200 and data["link_device"] == "/dev/ttyUSB0"
    assert [(p["device"], p["link"]) for p in data["ports"]] == [
        ("/dev/ttyACM0", False), ("/dev/ttyUSB0", True),
    ]


def test_the_status_answers_without_a_bridge():
    handler, out = _handler(None)
    handler._api_betaflight_status()
    assert out[0][0]["ports"] == [] and out[0][1] == 200


def test_the_read_route_refuses_the_link_port_and_bad_requests(monkeypatch: pytest.MonkeyPatch):
    called: list[str] = []
    monkeypatch.setattr(msp, "read_flight_controller", lambda d, b: called.append(d) or {})
    handler, out = _handler(_Bridge(link="/dev/ttyUSB0"))
    handler._api_betaflight_read({"device": "/dev/ttyUSB0"})
    handler._api_betaflight_read({})
    handler._api_betaflight_read({"device": "/dev/ttyACM0", "baud": "fast"})
    assert [status for _, status in out] == [409, 400, 400]
    assert called == []


def test_the_read_route_returns_the_controller(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(msp, "read_flight_controller", lambda d, b: {"version": "4.5.1", "unanswered": []})
    handler, out = _handler(_Bridge())
    handler._api_betaflight_read({"device": "/dev/ttyACM0"})
    data, status = out[0]
    assert status == 200 and data["ok"] and data["version"] == "4.5.1"
    assert data["device"] == "/dev/ttyACM0" and data["baud"] == msp.DEFAULT_BAUD


def test_the_read_route_maps_a_silent_port_to_409(monkeypatch: pytest.MonkeyPatch):
    def fail(device: str, baud: int) -> dict:
        raise MspError("No flight controller answered.")

    monkeypatch.setattr(msp, "read_flight_controller", fail)
    handler, out = _handler(_Bridge())
    handler._api_betaflight_read({"device": "/dev/ttyACM0"})
    assert out[0][1] == 409 and "answered" in out[0][0]["error"]
