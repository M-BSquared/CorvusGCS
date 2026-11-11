"""MSP: what a Betaflight flight controller tells a configurator, read only.

Betaflight takes no parameters or commands over MAVLink (see
:class:`corvus.autopilot.BetaflightDialect`). It is configured over MSP, the
MultiWii Serial Protocol, on the flight controller's USB port, which is not the
UART that carries the MAVLink telemetry. This module speaks enough of it to
*read* a controller: which firmware and board, which sensors, the PID and rate
profile in use, the battery. Nothing here writes to the controller.

A read is one short session on one port: open, ask a handful of questions,
close in ``finally``. There is no thread and no handle that outlives the call,
so there is nothing for the shutdown path to tear down.

Wire format (MSP v1, which every Betaflight 4.x answers): a request is
``$M<`` + length + command + payload + checksum, the answer ``$M>`` (or ``$M!``
for a command the firmware does not know), and the checksum is the XOR of the
length, command and payload bytes. Multi-byte fields are little-endian.
"""
from __future__ import annotations

import logging
import struct
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Protocol

logger = logging.getLogger("corvus.msp")

DEFAULT_BAUD = 115200
# A USB serial port ignores the baud rate. A UART does not, and 115200 is the
# Betaflight default for an MSP port.
BAUD_CANDIDATES = (115200, 57600, 230400, 460800, 921600, 38400, 19200, 9600)

MSP_API_VERSION = 1
MSP_FC_VARIANT = 2
MSP_FC_VERSION = 3
MSP_BOARD_INFO = 4
MSP_BUILD_INFO = 5
MSP_NAME = 10
MSP_STATUS_EX = 150
MSP_RC_TUNING = 111
MSP_PID = 112
MSP_BATTERY_STATE = 130

MAX_PAYLOAD = 255
REPLY_TIMEOUT_S = 0.8
FIRST_REPLY_TIMEOUT_S = 1.5
READ_CHUNK = 256
RETRIES = 1

VARIANTS: dict[str, str] = {
    "BTFL": "Betaflight",
    "CLFL": "Cleanflight",
    "EMUF": "EmuFlight",
    "INAV": "iNav",
    "KISS": "KISS",
    "RACE": "RaceFlight",
}

SENSOR_BITS: tuple[tuple[int, str], ...] = (
    (1 << 5, "gyro"), (1 << 0, "accelerometer"), (1 << 1, "barometer"),
    (1 << 2, "magnetometer"), (1 << 3, "gps"), (1 << 4, "rangefinder"),
)

PID_AXES = ("roll", "pitch", "yaw")
# Betaflight orders MSP_PID roll, pitch, yaw, altitude, position, position
# rate, navigation rate, level, mag, velocity. Level carries the angle mode
# strength, which is the one besides the axes a pilot tunes.
PID_LEVEL_INDEX = 7


class MspError(RuntimeError):
    """A read could not be completed, with the reason an operator can act on."""


@dataclass(frozen=True)
class MspFrame:
    """One checked reply. ``error`` is True for ``$M!``, a command not understood."""

    command: int
    payload: bytes
    error: bool = False


def build_request(command: int, payload: bytes = b"") -> bytes:
    """Encode an MSP v1 request."""
    if not 0 <= command <= 0xFF:
        raise ValueError("MSP v1 commands are one byte")
    if len(payload) > MAX_PAYLOAD:
        raise ValueError("MSP v1 payload is at most 255 bytes")
    body = bytes([len(payload), command]) + bytes(payload)
    checksum = 0
    for byte in body:
        checksum ^= byte
    return b"$M<" + body + bytes([checksum])


class MspParser:
    """Turns a byte stream into reply frames, resynchronising on garbage.

    A port may hand over the tail of an earlier frame, a CLI banner or a
    MAVLink burst when it is the wrong port. Anything that is not a well-formed
    ``$M>`` or ``$M!`` frame with a matching checksum costs one byte of resync,
    never an exception.
    """

    def __init__(self) -> None:
        self._buf = bytearray()
        self.frames_ok = 0
        self.checksum_errors = 0

    def feed(self, data: bytes) -> list[MspFrame]:
        """Add *data* and return every complete frame it finished."""
        self._buf.extend(data)
        frames: list[MspFrame] = []
        buf = self._buf
        while True:
            start = buf.find(b"$M")
            if start < 0:
                del buf[:max(0, len(buf) - 1)]
                break
            if start:
                del buf[:start]
            if len(buf) < 3:
                break
            if buf[2] not in (ord(">"), ord("!")):
                del buf[0]
                continue
            if len(buf) < 5:
                break
            size = buf[3]
            total = 6 + size
            if len(buf) < total:
                break
            checksum = 0
            for byte in buf[3:total - 1]:
                checksum ^= byte
            if checksum != buf[total - 1]:
                self.checksum_errors += 1
                del buf[0]
                continue
            frames.append(MspFrame(buf[4], bytes(buf[5:total - 1]), buf[2] == ord("!")))
            self.frames_ok += 1
            del buf[:total]
        return frames


def _text(data: bytes) -> str:
    return data.split(b"\x00", 1)[0].decode("ascii", "replace").strip()


def decode_api_version(payload: bytes) -> dict[str, Any]:
    """API version (1): protocol, then the MSP API major and minor."""
    if len(payload) < 3:
        raise MspError("API version reply is too short")
    return {"protocol": payload[0], "api": f"{payload[1]}.{payload[2]}"}


def decode_variant(payload: bytes) -> dict[str, Any]:
    """Flight controller variant (2): four characters, ``BTFL`` for Betaflight."""
    code = _text(payload[:4])
    return {"variant": code, "variant_name": VARIANTS.get(code, code)}


def decode_version(payload: bytes) -> dict[str, Any]:
    """Flight controller version (3): major, minor, patch."""
    if len(payload) < 3:
        raise MspError("firmware version reply is too short")
    return {"version": f"{payload[0]}.{payload[1]}.{payload[2]}"}


def decode_board_info(payload: bytes) -> dict[str, Any]:
    """Board info (4): the board id, its revision and, on 4.x, the target name.

    The tail grew with the API (board type, capabilities, target name), so
    each part is read only when the reply is long enough to hold it.
    """
    if len(payload) < 6:
        raise MspError("board info reply is too short")
    out: dict[str, Any] = {
        "board": _text(payload[:4]),
        "hardware_revision": struct.unpack_from("<H", payload, 4)[0],
        "target": "",
    }
    if len(payload) >= 9:
        name_len = payload[8]
        out["target"] = _text(payload[9:9 + name_len])
    return out


def decode_build_info(payload: bytes) -> dict[str, Any]:
    """Build info (5): build date, build time and the short git revision."""
    if len(payload) < 19:
        raise MspError("build info reply is too short")
    return {"build": {
        "date": _text(payload[:11]),
        "time": _text(payload[11:19]),
        "revision": _text(payload[19:26]),
    }}


def decode_name(payload: bytes) -> dict[str, Any]:
    """Craft name (10): whatever the pilot called the aircraft, often empty."""
    return {"name": payload.decode("ascii", "replace").strip()}


def decode_status(payload: bytes) -> dict[str, Any]:
    """Extended status (150): loop time, sensors, CPU load and the active profiles.

    The plain status (101) ends with a zero where this one carries the profile
    count and the rate profile, so this is the one asked.
    """
    if len(payload) < 11:
        raise MspError("status reply is too short")
    cycle, i2c, sensors, _modes, pid_profile = struct.unpack_from("<HHHIB", payload)
    out: dict[str, Any] = {
        "cycle_time_us": cycle,
        "i2c_errors": i2c,
        "sensors": [name for bit, name in SENSOR_BITS if sensors & bit],
        "pid_profile": pid_profile + 1,
    }
    if len(payload) >= 13:
        out["cpu_load"] = struct.unpack_from("<H", payload, 11)[0]
    if len(payload) >= 15:
        out["pid_profile_count"] = payload[13]
        out["rate_profile"] = payload[14] + 1
    return out


def decode_pid(payload: bytes) -> dict[str, Any]:
    """PID (112): P, I and D per axis, and the level (angle mode) strength."""
    if len(payload) < 9:
        raise MspError("PID reply is too short")
    pids: dict[str, dict[str, int]] = {
        axis: {"p": payload[i * 3], "i": payload[i * 3 + 1], "d": payload[i * 3 + 2]}
        for i, axis in enumerate(PID_AXES)
    }
    if len(payload) >= (PID_LEVEL_INDEX + 1) * 3:
        base = PID_LEVEL_INDEX * 3
        pids["level"] = {"p": payload[base], "i": payload[base + 1], "d": payload[base + 2]}
    return {"pids": pids}


def decode_rc_tuning(payload: bytes) -> dict[str, Any]:
    """RC tuning (111): the rates of the active rate profile.

    ``rc_rate`` and ``rc_expo`` are the roll values, which Betaflight sends
    first; the three ``rates`` are the super rates per axis. Later fields
    arrived with later API versions and are read only when present.
    """
    if len(payload) < 8:
        raise MspError("rates reply is too short")
    out: dict[str, Any] = {"rates": {
        "rc_rate": payload[0],
        "rc_expo": payload[1],
        "roll": payload[2],
        "pitch": payload[3],
        "yaw": payload[4],
        "tpa_rate": payload[5],
        "throttle_mid": payload[6],
        "throttle_expo": payload[7],
    }}
    if len(payload) >= 10:
        out["rates"]["tpa_breakpoint"] = struct.unpack_from("<H", payload, 8)[0]
    return out


def decode_battery_state(payload: bytes) -> dict[str, Any]:
    """Battery state (130): cell count, capacity, voltage and current drawn."""
    if len(payload) < 11:
        raise MspError("battery reply is too short")
    cells, capacity, _legacy, used, amps, _state, volts = struct.unpack_from("<BHBHhBH", payload)
    return {"battery": {
        "cells": cells,
        "capacity_mah": capacity,
        "voltage": volts / 100.0,
        "used_mah": used,
        "current": amps / 100.0,
    }}


# The questions, in the order they are asked. The first three decide whether
# there is a flight controller on the port at all; the rest are optional, and a
# firmware that answers one with ``$M!`` simply leaves that part out.
REQUIRED: tuple[tuple[int, Callable[[bytes], dict[str, Any]]], ...] = (
    (MSP_API_VERSION, decode_api_version),
    (MSP_FC_VARIANT, decode_variant),
    (MSP_FC_VERSION, decode_version),
)
OPTIONAL: tuple[tuple[int, Callable[[bytes], dict[str, Any]]], ...] = (
    (MSP_BOARD_INFO, decode_board_info),
    (MSP_BUILD_INFO, decode_build_info),
    (MSP_NAME, decode_name),
    (MSP_STATUS_EX, decode_status),
    (MSP_PID, decode_pid),
    (MSP_RC_TUNING, decode_rc_tuning),
    (MSP_BATTERY_STATE, decode_battery_state),
)


class ByteLink(Protocol):
    """The part of a ``serial.Serial`` a session uses."""

    def read(self, size: int = 1) -> bytes: ...

    def write(self, data: bytes) -> Any: ...

    def reset_input_buffer(self) -> None: ...

    def close(self) -> None: ...


class MspSession:
    """Asks questions on an open port, one at a time, until each is answered."""

    def __init__(self, link: ByteLink, clock: Callable[[], float] = time.monotonic) -> None:
        self._link = link
        self._clock = clock
        self._parser = MspParser()

    def ask(self, command: int, timeout: float = REPLY_TIMEOUT_S) -> MspFrame | None:
        """The reply to *command*, or None when nothing answered in time."""
        for _ in range(RETRIES + 1):
            self._link.reset_input_buffer()
            self._link.write(build_request(command))
            deadline = self._clock() + timeout
            while self._clock() < deadline:
                data = self._link.read(READ_CHUNK)
                if not data:
                    continue
                for frame in self._parser.feed(data):
                    if frame.command == command:
                        return frame
        return None


def read_with_session(session: MspSession) -> dict[str, Any]:
    """Run the questions on *session* and fold the answers into one document."""
    result: dict[str, Any] = {"unanswered": []}
    for index, (command, decode) in enumerate(REQUIRED):
        frame = session.ask(command, FIRST_REPLY_TIMEOUT_S if index == 0 else REPLY_TIMEOUT_S)
        if frame is None or frame.error:
            raise MspError(
                "No flight controller answered. Use the USB port of the flight "
                "controller, not the telemetry port, with MSP on that port."
            )
        result.update(decode(frame.payload))
    for command, decode in OPTIONAL:
        frame = session.ask(command)
        if frame is None or frame.error:
            result["unanswered"].append(command)
            continue
        try:
            result.update(decode(frame.payload))
        except MspError:
            result["unanswered"].append(command)
    result["is_betaflight"] = result.get("variant") == "BTFL"
    return result


_lock = threading.Lock()


def open_serial(device: str, baud: int) -> ByteLink:
    """The port a read uses: short timeouts, so a silent port costs a second."""
    import serial

    return serial.Serial(device, baudrate=baud, timeout=0.05, write_timeout=1.0)


def read_flight_controller(
    device: str,
    baud: int = DEFAULT_BAUD,
    opener: Callable[[str, int], ByteLink] = open_serial,
    clock: Callable[[], float] = time.monotonic,
) -> dict[str, Any]:
    """Open *device*, read the flight controller on it and close the port again.

    Serialised, so two requests cannot talk over one port. Always closes the
    port, whatever happened.
    """
    if not _lock.acquire(timeout=2.0):
        raise MspError("another read is in progress")
    try:
        try:
            link = opener(device, baud)
        except Exception as exc:  # noqa: BLE001 - pyserial raises broadly
            raise MspError(f"could not open {device}: {exc}") from exc
        try:
            return read_with_session(MspSession(link, clock))
        finally:
            try:
                link.close()
            except Exception:  # noqa: BLE001 - nothing left to do with it
                logger.debug("closing %s failed", device, exc_info=True)
    finally:
        _lock.release()
