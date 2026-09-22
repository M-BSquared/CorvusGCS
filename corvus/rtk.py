"""RTK corrections: the protocol half, with no port and no thread in sight.

An RTK fix is not something the ground station computes. It is something a
*second* GNSS receiver — one standing still on a tripod, on a roof, or in a
national network — computes about its own errors, and the ground station's only
job is to be the courier: read the corrections out of the base, and put them on
the MAVLink link so the autopilot's receiver can subtract them. Everything that
makes that hard is in this module, and none of it needs hardware to test.

Three protocols meet here
-------------------------
**RTCM 3** is what a base emits and what an autopilot wants: a byte stream of
self-framed messages, each one a ``0xD3`` preamble, a 10-bit length, a payload
and a 24-bit CRC. It arrives in arbitrary chunks from a serial port or a socket,
so :class:`RtcmFramer` is a resynchronising parser rather than a splitter — a
byte lost on a cable must cost the one message it was in, not the rest of the
session.

**GPS_RTCM_DATA** is how that stream crosses MAVLink, and it is the awkward
part. The message carries at most 180 bytes and its fragment counter is *two
bits wide*, so a correction longer than four fragments cannot be expressed at
all, and one whose length is an exact multiple of 180 needs an empty fragment
after it or the receiver never learns the sequence ended (PX4 and ArduPilot both
key "last fragment" on a short one). :func:`fragments` encodes both rules; see
its docstring for why the limit is 719 bytes and not 720.

**UBX** is how a u-blox receiver is told to *be* a base station. Out of the box
an F9P is a rover: it computes its own position and emits nothing. Survey-in —
"stand still until you know where you are to within X metres, then start
correcting" — is a configuration write, and the register keys for it differ
between the F9 generation (``CFG-VALSET`` key/value pairs) and the M8P before it
(``CFG-TMODE3`` structs). Both are built here, because the alternative to
supporting both is telling an operator with working hardware that their base is
unsupported.

The keys, scalings and the survey-in state machine's terms are taken from the
PX4 GPS driver (``PX4-GPSDrivers/src/ubx.h``), which is the same source
QGroundControl's base-station support is built on. Where a magic number appears
below it is named after its constant there, so the two can be compared.

What this module does *not* do is open anything. The port, the socket, the
thread and the link all belong to :mod:`corvus.rtk_service`; this half is pure
functions over bytes so the wire format can be proven correct in the test suite
rather than on a tripod.
"""

from __future__ import annotations

import struct
from typing import Any

# ---------------------------------------------------------------------------
# RTCM 3
# ---------------------------------------------------------------------------

RTCM_PREAMBLE = 0xD3

# The largest payload a 10-bit length field can announce. A frame is the
# 3-byte header plus this plus the 3-byte CRC.
RTCM_MAX_PAYLOAD = 1023

# CRC-24Q ("Qualcomm"), the check every RTCM 3 frame ends with: polynomial
# 0x1864CFB, initial value 0, no final XOR. Built once at import rather than
# written out as 256 literals — the table is derived, so a typo in it would be
# a silent wrong answer on one byte value in 256.
_CRC24Q_POLY = 0x1864CFB


def _crc24q_table() -> tuple[int, ...]:
    """The CRC of each single byte, precomputed so the hot path is a lookup."""
    table: list[int] = []
    for byte in range(256):
        reg = byte << 16
        for _ in range(8):
            reg = ((reg << 1) ^ _CRC24Q_POLY) if reg & 0x800000 else (reg << 1)
        table.append(reg & 0xFFFFFF)
    return tuple(table)


_CRC24Q_TABLE: tuple[int, ...] = _crc24q_table()


def crc24q(data: bytes) -> int:
    """The CRC-24Q of *data*, as the 24-bit integer an RTCM frame carries."""
    reg = 0
    for byte in data:
        reg = ((reg << 8) & 0xFFFFFF) ^ _CRC24Q_TABLE[((reg >> 16) ^ byte) & 0xFF]
    return reg


def message_number(frame: bytes) -> int:
    """The RTCM message type of a complete *frame*, or 0 if it has none.

    The number is the first 12 bits of the payload — 1005 for the base's own
    position, 1074/1084/1094/1124 for the per-constellation observations, 1230
    for the GLONASS biases. Reported rather than acted on: Corvus forwards
    whatever the base emits, and the numbers exist so the page can show that a
    base which is streaming *something* is streaming the right things.
    """
    if len(frame) < 6:
        return 0
    return (frame[3] << 4) | (frame[4] >> 4)


class RtcmFramer:
    """Byte stream in, complete RTCM 3 frames out.

    Fed arbitrary chunks — a serial read, a socket read — and yields only
    frames whose CRC checks. Three things make this more than a split:

    *Resynchronisation.* ``0xD3`` is a perfectly ordinary byte inside a
    payload, so a stream joined mid-message will produce a false start. The
    parser therefore commits to nothing until the CRC agrees, and on a
    mismatch it steps forward by **one byte** rather than discarding the frame
    it guessed at — the real preamble may be inside what it just read.

    *Boundedness.* The buffer is capped at a little over one maximum frame.
    A device that streams NMEA, or noise, at 38400 baud would otherwise grow
    this without limit on a thread nobody is watching.

    *Counting.* ``crc_errors`` and ``discarded`` are the two numbers that tell
    a stalled RTK session apart from a quiet one: a base that is producing
    nothing and a base whose cable is producing rubbish both show no frames,
    and only one of them is fixed by waiting.
    """

    # Header (3) + the longest payload a 10-bit field can announce + CRC (3).
    MAX_FRAME = 3 + RTCM_MAX_PAYLOAD + 3
    _MAX_BUFFER = MAX_FRAME * 2

    def __init__(self) -> None:
        self._buf = bytearray()
        self.frames = 0
        self.crc_errors = 0
        self.discarded = 0

    def feed(self, chunk: bytes) -> list[bytes]:
        """Add *chunk* and return every complete, CRC-valid frame it finished."""
        if chunk:
            self._buf.extend(chunk)
        out: list[bytes] = []
        while True:
            start = self._buf.find(RTCM_PREAMBLE)
            if start < 0:
                # Nothing in the buffer can begin a frame. Keep the last two
                # bytes only in case a preamble is about to be split across
                # this read and the next.
                self.discarded += max(0, len(self._buf) - 2)
                del self._buf[:-2]
                break
            if start:
                self.discarded += start
                del self._buf[:start]
            if len(self._buf) < 3:
                break
            length = ((self._buf[1] & 0x03) << 8) | self._buf[2]
            total = 3 + length + 3
            if len(self._buf) < total:
                break
            frame = bytes(self._buf[:total])
            expected = int.from_bytes(frame[-3:], "big")
            if crc24q(frame[:-3]) == expected:
                out.append(frame)
                self.frames += 1
                del self._buf[:total]
                continue
            # Not a frame after all: that 0xD3 was payload. Step one byte, not
            # one frame, or a real message starting inside this window is lost
            # along with the false one.
            self.crc_errors += 1
            self.discarded += 1
            del self._buf[:1]
        if len(self._buf) > self._MAX_BUFFER:
            self.discarded += len(self._buf) - self._MAX_BUFFER
            del self._buf[:-self._MAX_BUFFER]
        return out

    def reset(self) -> None:
        """Drop any partial frame. Called when the source is reconnected."""
        self._buf.clear()


# ---------------------------------------------------------------------------
# GPS_RTCM_DATA
# ---------------------------------------------------------------------------

# The message's data field is fixed at 180 bytes.
FRAGMENT_BYTES = 180

# ``flags`` packs three things into one byte: bit 0 says the message is part of
# a fragmented correction, bits 1-2 are the fragment index, bits 3-7 the
# sequence number. Two bits of fragment index is the ceiling everything below
# is shaped by — four fragments, and no fifth.
MAX_FRAGMENTS = 4

# The longest correction that can be expressed. 4 x 180 = 720 looks like the
# answer and is not: a receiver recognises the end of a sequence by a fragment
# shorter than 180, so a 720-byte message needs a fifth, empty fragment that
# the 2-bit index cannot number. 719 fits in four (180+180+180+179); so does
# 540, whose terminator is the fourth. Anything longer is dropped and counted,
# because sending three quarters of a correction is worse than sending none.
MAX_MESSAGE_BYTES = FRAGMENT_BYTES * MAX_FRAGMENTS - 1


def fragments(frame: bytes, sequence: int) -> list[tuple[int, bytes]]:
    """Split one RTCM frame into ``(flags, data)`` pairs for GPS_RTCM_DATA.

    Returns an empty list for an empty frame and for one too long to express
    (see :data:`MAX_MESSAGE_BYTES`); the caller counts those rather than
    sending a sequence the far end will wait on forever.

    *sequence* increments per correction, modulo 32, and is what lets a
    receiver tell the fragments of one message from those of the next after a
    dropped packet. The unfragmented case keeps bit 0 clear and still carries
    the sequence, exactly as QGroundControl's ``RTCMMavlink`` does.
    """
    if not frame or len(frame) > MAX_MESSAGE_BYTES:
        return []
    seq_bits = (int(sequence) & 0x1F) << 3
    if len(frame) < FRAGMENT_BYTES:
        return [(seq_bits, frame)]
    out: list[tuple[int, bytes]] = []
    index = 0
    for start in range(0, len(frame), FRAGMENT_BYTES):
        out.append((1 | (index << 1) | seq_bits, frame[start:start + FRAGMENT_BYTES]))
        index += 1
    if len(frame) % FRAGMENT_BYTES == 0:
        # The last fragment was a full 180 bytes, so nothing in it said "end".
        out.append((1 | (index << 1) | seq_bits, b""))
    return out


# ---------------------------------------------------------------------------
# UBX
# ---------------------------------------------------------------------------

UBX_SYNC = b"\xb5\x62"

UBX_CLASS_NAV = 0x01
UBX_CLASS_ACK = 0x05
UBX_CLASS_CFG = 0x06
UBX_CLASS_MON = 0x0A
UBX_CLASS_RTCM3 = 0xF5

UBX_ID_NAV_SVIN = 0x3B
UBX_ID_ACK_ACK = 0x01
UBX_ID_ACK_NAK = 0x00
UBX_ID_CFG_MSG = 0x01
UBX_ID_CFG_RATE = 0x08
UBX_ID_CFG_TMODE3 = 0x71
UBX_ID_CFG_VALSET = 0x8A
UBX_ID_MON_VER = 0x04

# CFG-VALSET layers. RAM only: a base station is configured for the session it
# is plugged in for, and writing flash on every connect would both wear the
# part and leave somebody's receiver permanently in base mode after Corvus has
# been unplugged from it.
UBX_LAYER_RAM = 0x01

# CFG-TMODE-* keys (F9 generation, protocol 27+). Values as named in the PX4
# driver, which names them after the u-blox interface description.
KEY_TMODE_MODE = 0x20030001            # 0 disabled, 1 survey-in, 2 fixed
KEY_TMODE_POS_TYPE = 0x20030002        # 0 ECEF, 1 lat/lon/height
KEY_TMODE_LAT = 0x40030009             # 1e-7 deg
KEY_TMODE_LON = 0x4003000A             # 1e-7 deg
KEY_TMODE_HEIGHT = 0x4003000B          # cm
KEY_TMODE_LAT_HP = 0x2003000C          # 1e-9 deg remainder, [-99, 99]
KEY_TMODE_LON_HP = 0x2003000D
KEY_TMODE_HEIGHT_HP = 0x2003000E       # 0.1 mm remainder, [-99, 99]
KEY_TMODE_FIXED_POS_ACC = 0x4003000F   # 0.1 mm
KEY_TMODE_SVIN_MIN_DUR = 0x40030010    # s
KEY_TMODE_SVIN_ACC_LIMIT = 0x40030011  # 0.1 mm

KEY_RATE_MEAS = 0x30210001             # ms between measurements

# CFG-MSGOUT-* keys, given for the I2C port. The other ports are the same key
# plus a fixed offset — UART1 +1, UART2 +2, USB +3, SPI +4 — which is why only
# one number per message is written down. See :func:`_port_keys`.
KEY_MSGOUT_NAV_SVIN_I2C = 0x20910088
KEY_MSGOUT_RTCM_1005_I2C = 0x209102BD
KEY_MSGOUT_RTCM_1074_I2C = 0x2091035E
KEY_MSGOUT_RTCM_1077_I2C = 0x209102CC
KEY_MSGOUT_RTCM_1084_I2C = 0x20910363
KEY_MSGOUT_RTCM_1087_I2C = 0x209102D1
KEY_MSGOUT_RTCM_1094_I2C = 0x20910368
KEY_MSGOUT_RTCM_1097_I2C = 0x20910318
KEY_MSGOUT_RTCM_1124_I2C = 0x2091036D
KEY_MSGOUT_RTCM_1127_I2C = 0x209102D6
KEY_MSGOUT_RTCM_1230_I2C = 0x20910303

# The set a base emits, as MSM4. MSM7 (1077/1087/1097/1127) carries more
# resolution than an RTK fix can use and roughly doubles the bytes per second,
# which matters here in a way it does not on a wired base: these corrections
# cross a 57600-baud telemetry radio shared with the rest of the link.
RTCM_MSM4_KEYS: tuple[int, ...] = (
    KEY_MSGOUT_RTCM_1005_I2C,
    KEY_MSGOUT_RTCM_1074_I2C,
    KEY_MSGOUT_RTCM_1084_I2C,
    KEY_MSGOUT_RTCM_1094_I2C,
    KEY_MSGOUT_RTCM_1124_I2C,
    KEY_MSGOUT_RTCM_1230_I2C,
)
RTCM_MSM7_KEYS: tuple[int, ...] = (
    KEY_MSGOUT_RTCM_1005_I2C,
    KEY_MSGOUT_RTCM_1077_I2C,
    KEY_MSGOUT_RTCM_1087_I2C,
    KEY_MSGOUT_RTCM_1097_I2C,
    KEY_MSGOUT_RTCM_1127_I2C,
    KEY_MSGOUT_RTCM_1230_I2C,
)

# The same set for a receiver too old for CFG-VALSET, addressed the old way:
# RTCM 3 output is a message class of its own there, one id per type.
LEGACY_RTCM_MSM4: tuple[tuple[int, int], ...] = (
    (UBX_CLASS_RTCM3, 0x05),  # 1005
    (UBX_CLASS_RTCM3, 0x4A),  # 1074
    (UBX_CLASS_RTCM3, 0x54),  # 1084
    (UBX_CLASS_RTCM3, 0x5E),  # 1094
    (UBX_CLASS_RTCM3, 0x7C),  # 1124
    (UBX_CLASS_RTCM3, 0xE6),  # 1230
)
LEGACY_RTCM_MSM7: tuple[tuple[int, int], ...] = (
    (UBX_CLASS_RTCM3, 0x05),  # 1005
    (UBX_CLASS_RTCM3, 0x4D),  # 1077
    (UBX_CLASS_RTCM3, 0x57),  # 1087
    (UBX_CLASS_RTCM3, 0x61),  # 1097
    (UBX_CLASS_RTCM3, 0x7F),  # 1127
    (UBX_CLASS_RTCM3, 0xE6),  # 1230
)

# The rate 1005 is emitted at. The base's own position does not move, so it is
# sent every fifth epoch rather than every one; the observations are what has
# to be current.
RTCM_1005_RATE = 5

# Baud rates a base station is found at, most likely first: 38400 is the F9P's
# default, 9600 the M8P's, and the other two are what a board maker changes it
# to. The port is USB-CDC on most modern receivers, where the rate is ignored
# entirely — which is why a wrong guess there still works and a wrong guess on
# a real UART does not.
BAUD_CANDIDATES: tuple[int, ...] = (38400, 9600, 115200, 57600, 19200, 460800)


def ubx_checksum(payload: bytes) -> tuple[int, int]:
    """The 8-bit Fletcher checksum UBX ends every frame with."""
    ck_a = 0
    ck_b = 0
    for byte in payload:
        ck_a = (ck_a + byte) & 0xFF
        ck_b = (ck_b + ck_a) & 0xFF
    return ck_a, ck_b


def ubx_frame(msg_class: int, msg_id: int, payload: bytes = b"") -> bytes:
    """One complete UBX message, ready to write to the port."""
    body = bytes((msg_class & 0xFF, msg_id & 0xFF)) + struct.pack("<H", len(payload)) + payload
    ck_a, ck_b = ubx_checksum(body)
    return UBX_SYNC + body + bytes((ck_a, ck_b))


class UbxParser:
    """Byte stream in, ``(class, id, payload)`` tuples out.

    The mirror of :class:`RtcmFramer`, and deliberately its twin rather than
    its sibling: a configured base emits both protocols down the same cable,
    so both parsers are fed the same bytes and each ignores what the other
    claims. A UBX frame whose checksum fails resynchronises one byte at a
    time for the same reason an RTCM one does.
    """

    _MAX_PAYLOAD = 4096

    def __init__(self) -> None:
        self._buf = bytearray()

    def feed(self, chunk: bytes) -> list[tuple[int, int, bytes]]:
        """Add *chunk* and return every complete, checksum-valid message."""
        if chunk:
            self._buf.extend(chunk)
        out: list[tuple[int, int, bytes]] = []
        while True:
            start = self._buf.find(UBX_SYNC)
            if start < 0:
                del self._buf[:-1]
                break
            if start:
                del self._buf[:start]
            if len(self._buf) < 6:
                break
            length = struct.unpack_from("<H", self._buf, 4)[0]
            if length > self._MAX_PAYLOAD:
                del self._buf[:2]
                continue
            total = 6 + length + 2
            if len(self._buf) < total:
                break
            body = bytes(self._buf[2:6 + length])
            ck_a, ck_b = ubx_checksum(body)
            if (ck_a, ck_b) == (self._buf[total - 2], self._buf[total - 1]):
                out.append((body[0], body[1], body[4:]))
                del self._buf[:total]
                continue
            del self._buf[:2]
        return out

    def reset(self) -> None:
        """Drop any partial message."""
        self._buf.clear()


def _value_bytes(key: int, value: int) -> bytes:
    """Encode *value* at the width the key's own storage-size bits declare.

    Bits 28-30 of a configuration key are its size class — 1 and 2 are one
    byte, 3 is two, 4 is four — so the key carries the width and the caller
    never has to state it. Signed values are folded into the unsigned range
    first, which is what makes a -37 latitude-remainder encode as the byte the
    receiver reads back as -37.
    """
    size_field = (key >> 28) & 0x07
    width = 1 if size_field <= 2 else 2 if size_field == 3 else 4
    return int(value & ((1 << (8 * width)) - 1)).to_bytes(width, "little")


def valset(items: list[tuple[int, int]], layers: int = UBX_LAYER_RAM) -> bytes:
    """A CFG-VALSET message carrying *items* as ``(key, value)`` pairs."""
    payload = bytearray((0x00, layers & 0xFF, 0x00, 0x00))
    for key, value in items:
        payload.extend(struct.pack("<I", key & 0xFFFFFFFF))
        payload.extend(_value_bytes(key, value))
    return ubx_frame(UBX_CLASS_CFG, UBX_ID_CFG_VALSET, bytes(payload))


def _port_keys(key: int) -> tuple[int, ...]:
    """The UART1 and USB forms of a per-port message-output key.

    Both, always, and for the reason the PX4 driver gives: which port the
    receiver is answering on is not knowable from this side of the cable. A
    ZED-F9P on a USB lead is on USB; the same chip on an ArduSimple board
    behind an FTDI is on UART1; enabling one and guessing wrong produces a
    base that configures cleanly and streams nothing at all.
    """
    return (key + 1, key + 3)


def _msgout(keys: tuple[int, ...], rate: int) -> list[tuple[int, int]]:
    """``(key, rate)`` pairs for *keys* on both output ports."""
    out: list[tuple[int, int]] = []
    for key in keys:
        for port_key in _port_keys(key):
            out.append((port_key, rate))
    return out


def rtcm_keys(msm7: bool = False) -> tuple[int, ...]:
    """The RTCM message set a base is configured to emit."""
    return RTCM_MSM7_KEYS if msm7 else RTCM_MSM4_KEYS


def stop_output_frames(msm7: bool = False) -> list[bytes]:
    """Silence the RTCM output before the time mode is changed.

    Sent first by both configuration paths. A receiver that is mid-survey
    keeps streaming while its mode is rewritten, and those corrections are
    computed against a position it is in the middle of abandoning.
    """
    return [valset(_msgout(rtcm_keys(msm7), 0))]


def survey_in_frames(duration_s: int, accuracy_m: float, msm7: bool = False) -> list[bytes]:
    """Configure survey-in: stand still, then start correcting.

    Two messages, in this order, and the order is the whole point: setting the
    mode to survey-in while a survey is already running does *not* restart it
    (u-blox treats it as a no-op), so the mode is taken to disabled first. The
    receiver applies a time-mode change on a navigation epoch rather than on
    the ACK, which is why the service waits for ``NAV-SVIN`` to report the old
    survey stopped before it sends the second message.

    ``accuracy_m`` is metres here and 0.1 mm on the wire; ``duration_s`` is
    seconds in both.
    """
    stop = valset([(KEY_TMODE_MODE, 0)])
    start = valset([
        (KEY_TMODE_MODE, 1),
        (KEY_TMODE_SVIN_MIN_DUR, max(0, int(duration_s))),
        (KEY_TMODE_SVIN_ACC_LIMIT, max(0, int(round(float(accuracy_m) * 10000.0)))),
        *_msgout((KEY_MSGOUT_NAV_SVIN_I2C,), 5),
    ])
    return [*stop_output_frames(msm7), stop, start]


def _hp_split(value: float, scale: float) -> tuple[int, int]:
    """Split *value* into a coarse count and a [-99, 99] high-precision part.

    u-blox stores a base position as two numbers per axis — degrees at 1e-7
    plus a remainder at 1e-9, centimetres plus a remainder at 0.1 mm — because
    a single 32-bit field cannot hold a millimetre-accurate position on a
    planet this size. Computed in integers after one multiply so the split is
    exact rather than a difference of two floats.
    """
    fine = int(round(float(value) * scale))
    coarse = int(fine / 100)
    return coarse, fine - coarse * 100


def fixed_position_frames(
    latitude: float, longitude: float, altitude: float,
    accuracy_m: float = 0.1, msm7: bool = False,
) -> list[bytes]:
    """Configure a base at a position the operator already knows.

    The alternative to survey-in, and the better answer whenever the base goes
    back to the same surveyed mark: a survey's whole cost is the three minutes
    of standing still, and its result is no better than the position it
    converged on. A known mark skips both.

    *altitude* is metres above the ellipsoid — not AMSL. The distinction is
    worth stating because the number is usually read off a survey record that
    gives one or the other, and they differ by tens of metres in most of
    Europe.
    """
    lat, lat_hp = _hp_split(latitude, 1e9)
    lon, lon_hp = _hp_split(longitude, 1e9)
    height, height_hp = _hp_split(altitude, 1e4)
    apply = valset([
        (KEY_TMODE_MODE, 2),
        (KEY_TMODE_POS_TYPE, 1),
        (KEY_TMODE_LAT, lat), (KEY_TMODE_LAT_HP, lat_hp),
        (KEY_TMODE_LON, lon), (KEY_TMODE_LON_HP, lon_hp),
        (KEY_TMODE_HEIGHT, height), (KEY_TMODE_HEIGHT_HP, height_hp),
        (KEY_TMODE_FIXED_POS_ACC, max(0, int(round(float(accuracy_m) * 10000.0)))),
    ])
    return [*stop_output_frames(msm7), apply]


def activate_output_frames(msm7: bool = False, reduce_rate: bool = True) -> list[bytes]:
    """Start the RTCM stream, and stop the survey-in reports.

    *reduce_rate* drops the measurement rate to 1 Hz. A survey converges
    faster at 5 Hz and there is nothing to spend the other four epochs on once
    it has: a base that is not moving has no reason to recompute its
    corrections more than once a second, and each extra epoch is bytes down
    the telemetry link.
    """
    items: list[tuple[int, int]] = []
    if reduce_rate:
        items.append((KEY_RATE_MEAS, 1000))
    items.extend(_msgout((KEY_MSGOUT_RTCM_1005_I2C,), RTCM_1005_RATE))
    observations = tuple(k for k in rtcm_keys(msm7) if k != KEY_MSGOUT_RTCM_1005_I2C)
    items.extend(_msgout(observations, 1))
    items.extend(_msgout((KEY_MSGOUT_NAV_SVIN_I2C,), 0))
    return [valset(items)]


def disable_base_frames(msm7: bool = False) -> list[bytes]:
    """Put the receiver back the way it was found: no RTCM, no time mode.

    Sent when the service lets go of a device. A receiver left in base mode is
    one that computes no position of its own, which is not a state to hand
    back to whoever plugs it in next.
    """
    return [
        valset([
            *_msgout(rtcm_keys(msm7), 0),
            *_msgout((KEY_MSGOUT_NAV_SVIN_I2C,), 0),
            (KEY_TMODE_MODE, 0),
        ]),
    ]


# -- the same three operations for a receiver too old for CFG-VALSET --------


def legacy_cfg_msg(msg_class: int, msg_id: int, rate: int) -> bytes:
    """CFG-MSG: set the output rate of one message on the current port."""
    return ubx_frame(
        UBX_CLASS_CFG, UBX_ID_CFG_MSG,
        bytes((msg_class & 0xFF, msg_id & 0xFF, rate & 0xFF)),
    )


def _legacy_rtcm(msm7: bool) -> tuple[tuple[int, int], ...]:
    return LEGACY_RTCM_MSM7 if msm7 else LEGACY_RTCM_MSM4


def legacy_stop_output_frames(msm7: bool = False) -> list[bytes]:
    """Silence an M8P-generation receiver's RTCM output."""
    return [legacy_cfg_msg(cls, mid, 0) for cls, mid in _legacy_rtcm(msm7)]


def legacy_survey_in_frames(
    duration_s: int, accuracy_m: float, msm7: bool = False,
) -> list[bytes]:
    """CFG-TMODE3 survey-in, for an M8P-generation receiver."""
    frames = [legacy_cfg_msg(cls, mid, 0) for cls, mid in _legacy_rtcm(msm7)]
    frames.append(_tmode3(flags=0))
    frames.append(_tmode3(
        flags=1,
        svin_min_dur=max(0, int(duration_s)),
        svin_acc_limit=max(0, int(round(float(accuracy_m) * 10000.0))),
    ))
    frames.append(legacy_cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 5))
    return frames


def legacy_fixed_position_frames(
    latitude: float, longitude: float, altitude: float,
    accuracy_m: float = 0.1, msm7: bool = False,
) -> list[bytes]:
    """CFG-TMODE3 fixed position, for an M8P-generation receiver."""
    lat, lat_hp = _hp_split(latitude, 1e9)
    lon, lon_hp = _hp_split(longitude, 1e9)
    height, height_hp = _hp_split(altitude, 1e4)
    frames = [legacy_cfg_msg(cls, mid, 0) for cls, mid in _legacy_rtcm(msm7)]
    frames.append(_tmode3(flags=0))
    frames.append(_tmode3(
        # Bit 8 of flags selects lat/lon/height over ECEF; the low byte is the
        # mode itself.
        flags=2 | (1 << 8),
        x_or_lat=lat, y_or_lon=lon, z_or_alt=height,
        x_hp=lat_hp, y_hp=lon_hp, z_hp=height_hp,
        fixed_pos_acc=max(0, int(round(float(accuracy_m) * 10000.0))),
    ))
    return frames


def legacy_activate_output_frames(msm7: bool = False) -> list[bytes]:
    """Start the RTCM stream on an M8P-generation receiver."""
    frames = [legacy_cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 0)]
    for cls, mid in _legacy_rtcm(msm7):
        rate = RTCM_1005_RATE if mid == 0x05 else 1
        frames.append(legacy_cfg_msg(cls, mid, rate))
    return frames


def legacy_disable_base_frames(msm7: bool = False) -> list[bytes]:
    """Take an M8P-generation receiver back out of base mode."""
    frames = [legacy_cfg_msg(cls, mid, 0) for cls, mid in _legacy_rtcm(msm7)]
    frames.append(legacy_cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 0))
    frames.append(_tmode3(flags=0))
    return frames


def _tmode3(
    flags: int = 0, x_or_lat: int = 0, y_or_lon: int = 0, z_or_alt: int = 0,
    x_hp: int = 0, y_hp: int = 0, z_hp: int = 0,
    fixed_pos_acc: int = 0, svin_min_dur: int = 0, svin_acc_limit: int = 0,
) -> bytes:
    """One CFG-TMODE3 message, 40 bytes, in the order the receiver reads it."""
    payload = struct.pack(
        "<BBHiiibbbBIII8s",
        0, 0, flags & 0xFFFF,
        x_or_lat, y_or_lon, z_or_alt,
        x_hp, y_hp, z_hp, 0,
        fixed_pos_acc & 0xFFFFFFFF,
        svin_min_dur & 0xFFFFFFFF,
        svin_acc_limit & 0xFFFFFFFF,
        b"\x00" * 8,
    )
    return ubx_frame(UBX_CLASS_CFG, UBX_ID_CFG_TMODE3, payload)


def poll_frame(msg_class: int, msg_id: int) -> bytes:
    """A zero-length UBX message, which is how one is polled for."""
    return ubx_frame(msg_class, msg_id)


# -- what comes back -------------------------------------------------------


def parse_nav_svin(payload: bytes) -> dict[str, Any] | None:
    """NAV-SVIN: how the survey is going, in the operator's units.

    ``valid`` and ``active`` are the pair that matters and they are not
    opposites. ``active`` says a survey is running; ``valid`` says its result
    meets the accuracy limit. Both true is a survey that has converged and is
    still refining; valid-and-not-active is the finished state the service
    waits for; neither is a receiver that is not surveying at all, which is
    how a *stopped* survey is recognised before a new one is started.

    ``meanAcc`` is 0.1 mm on the wire and metres here, because metres is what
    the operator set the limit in.
    """
    if len(payload) < 40:
        return None
    (_version, _res1, _itow, duration, mean_x, mean_y, mean_z,
     _hpx, _hpy, _hpz, _res2, mean_acc, observations, valid, active) = struct.unpack_from(
        "<B3sIIiiibbbbIIBB", payload, 0,
    )
    return {
        "duration": int(duration),
        "accuracy": round(mean_acc / 10000.0, 4),
        "observations": int(observations),
        "valid": bool(valid),
        "active": bool(active),
        "ecef": [int(mean_x), int(mean_y), int(mean_z)],
    }


def parse_mon_ver(payload: bytes) -> dict[str, Any]:
    """MON-VER: who answered, and which protocol they speak.

    The protocol version decides which of the two configuration paths above is
    used, and it is not in a field of its own — it is a line of free text in a
    variable-length list of 30-byte extension strings. 27 is the boundary: at
    or above it the receiver takes CFG-VALSET, below it the CFG-* structs.
    A receiver that does not say is assumed to be old, because the legacy
    messages are still accepted by the new parts and the reverse is not true.
    """
    if len(payload) < 40:
        return {"software": "", "hardware": "", "extensions": [], "protocol": 0.0, "model": ""}

    def _text(raw: bytes) -> str:
        return raw.split(b"\x00", 1)[0].decode("ascii", errors="replace").strip()

    software = _text(payload[0:30])
    hardware = _text(payload[30:40])
    extensions = [
        _text(payload[offset:offset + 30])
        for offset in range(40, len(payload) - 29, 30)
    ]
    protocol = 0.0
    model = ""
    for line in extensions:
        if line.startswith("PROTVER"):
            try:
                protocol = float(line.split("=", 1)[-1].strip() or line[7:].strip())
            except ValueError:
                protocol = 0.0
        elif line.startswith("MOD="):
            model = line[4:].strip()
    return {
        "software": software,
        "hardware": hardware,
        "extensions": extensions,
        "protocol": protocol,
        "model": model,
    }


def receiver_label(version: dict[str, Any] | None) -> str:
    """One line naming the receiver, for the page's status row."""
    if not version:
        return ""
    model = str(version.get("model") or "").strip()
    software = str(version.get("software") or "").strip()
    protocol = version.get("protocol") or 0.0
    parts: list[str] = []
    parts.append(f"u-blox {model}" if model else "u-blox receiver")
    if software:
        parts.append(software)
    if protocol:
        parts.append(f"protocol {protocol:g}")
    return " · ".join(parts)


def supports_valset(version: dict[str, Any] | None) -> bool:
    """Does this receiver take CFG-VALSET rather than the CFG-* structs?"""
    if not version:
        return False
    try:
        return float(version.get("protocol") or 0.0) >= 27.0
    except (TypeError, ValueError):
        return False


# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

# QGroundControl's defaults, kept deliberately: two metres and three minutes
# is the survey most operators have been flying against for years, and a
# ground station that quietly used a different pair would produce a base that
# takes longer or corrects worse than the same hardware did yesterday.
DEFAULT_SURVEY_ACCURACY_M = 2.0
DEFAULT_SURVEY_DURATION_S = 180

SURVEY_ACCURACY_MIN_M = 0.1
SURVEY_ACCURACY_MAX_M = 100.0
SURVEY_DURATION_MIN_S = 10
SURVEY_DURATION_MAX_S = 86400

DEFAULT_NTRIP_PORT = 2101

SOURCES: tuple[str, ...] = ("usb", "ntrip")
MODES: tuple[str, ...] = ("survey", "fixed")


def defaults() -> dict[str, Any]:
    """The settings a station with no config file runs with.

    ``enabled`` is true and ``source`` is ``usb``, which together are the
    whole of "plug and play": a base plugged into a station that has never
    been configured is found, surveyed and streaming without anybody opening
    the page. Everything else here is a default that only matters once one of
    those two has been changed.
    """
    return {
        "enabled": True,
        "source": "usb",
        "device": "",
        "baud": 0,
        "mode": "survey",
        "survey_accuracy": DEFAULT_SURVEY_ACCURACY_M,
        "survey_duration": DEFAULT_SURVEY_DURATION_S,
        "msm7": False,
        "fixed": {
            "latitude": 0.0,
            "longitude": 0.0,
            "altitude": 0.0,
            "accuracy": 0.1,
        },
        "ntrip": {
            "host": "",
            "port": DEFAULT_NTRIP_PORT,
            "mountpoint": "",
            "username": "",
            "password": "",
        },
    }


def _number(raw: Any, default: float, lo: float, hi: float) -> float:
    try:
        value = float(raw)
    except (TypeError, ValueError):
        return default
    if value != value or value in (float("inf"), float("-inf")):
        return default
    return min(hi, max(lo, value))


def _count(raw: Any, default: int, lo: int, hi: int) -> int:
    if isinstance(raw, bool) or not isinstance(raw, (int, float)):
        return default
    try:
        value = int(raw)
    except (TypeError, ValueError, OverflowError):
        return default
    return min(hi, max(lo, value))


def _text(raw: Any, limit: int) -> str:
    if not isinstance(raw, str):
        return ""
    return raw.strip()[:limit]


def settings(raw: Any) -> dict[str, Any]:
    """Resolve stored RTK settings against the defaults, bounding every field.

    Written to be total: anything that is not a usable value becomes the
    default rather than an error, because this runs against a config file an
    operator may have edited by hand and the failure it prevents is a ground
    station that will not start. The bounds are not cosmetic either — a
    survey accuracy of zero is a survey that never finishes, and a negative
    one is a base that declares itself valid immediately and corrects the
    aircraft towards a position it never established.
    """
    base = defaults()
    if not isinstance(raw, dict):
        return base
    out = dict(base)

    if isinstance(raw.get("enabled"), bool):
        out["enabled"] = raw["enabled"]
    source = raw.get("source")
    if isinstance(source, str) and source in SOURCES:
        out["source"] = source
    mode = raw.get("mode")
    if isinstance(mode, str) and mode in MODES:
        out["mode"] = mode
    out["device"] = _text(raw.get("device"), 256)
    out["baud"] = _count(raw.get("baud"), 0, 0, 4_000_000)
    out["survey_accuracy"] = round(_number(
        raw.get("survey_accuracy"), DEFAULT_SURVEY_ACCURACY_M,
        SURVEY_ACCURACY_MIN_M, SURVEY_ACCURACY_MAX_M,
    ), 3)
    out["survey_duration"] = _count(
        raw.get("survey_duration"), DEFAULT_SURVEY_DURATION_S,
        SURVEY_DURATION_MIN_S, SURVEY_DURATION_MAX_S,
    )
    if isinstance(raw.get("msm7"), bool):
        out["msm7"] = raw["msm7"]

    fixed_raw = raw.get("fixed")
    if isinstance(fixed_raw, dict):
        out["fixed"] = {
            "latitude": round(_number(fixed_raw.get("latitude"), 0.0, -90.0, 90.0), 9),
            "longitude": round(_number(fixed_raw.get("longitude"), 0.0, -180.0, 180.0), 9),
            "altitude": round(_number(fixed_raw.get("altitude"), 0.0, -1000.0, 20000.0), 4),
            "accuracy": round(_number(fixed_raw.get("accuracy"), 0.1, 0.001, 100.0), 4),
        }

    ntrip_raw = raw.get("ntrip")
    if isinstance(ntrip_raw, dict):
        out["ntrip"] = {
            "host": _text(ntrip_raw.get("host"), 253),
            "port": _count(ntrip_raw.get("port"), DEFAULT_NTRIP_PORT, 1, 65535),
            "mountpoint": _text(ntrip_raw.get("mountpoint"), 100),
            "username": _text(ntrip_raw.get("username"), 128),
            "password": _text(ntrip_raw.get("password"), 128),
        }
    return out


def public_settings(settings: dict[str, Any]) -> dict[str, Any]:
    """A copy of *settings* safe to put in an HTTP response.

    The NTRIP password is the only secret this block can hold, and the RTK
    page builds its form from a response — so a form pre-filled with the
    password would have put it on the wire on every poll. ``has_password``
    replaces it, because "one is stored" and "there is none" are different
    things to an operator looking at an empty box.

    The one redaction point for this block. :meth:`corvus.rtk_service.
    RtkService.status` calls it, which is what ``GET /api/rtk/status`` returns
    and what ``POST /api/rtk/settings`` echoes back; ``corvus.config.
    to_public_dict`` does the equivalent for the copy that leaves through
    ``GET /api/config``.
    """
    out = {
        k: (dict(v) if isinstance(v, dict) else v)
        for k, v in (settings or {}).items()
    }
    ntrip = out.get("ntrip")
    if isinstance(ntrip, dict):
        ntrip["has_password"] = bool(ntrip.get("password"))
        ntrip["password"] = ""
    return out


def fixed_position_problem(fixed: dict[str, Any]) -> str:
    """Why a fixed base position cannot be used, or "" when it can.

    Null Island is the check that matters. Latitude and longitude both zero is
    what an unfilled form produces, it is a valid coordinate, and a base
    configured there tells the aircraft its errors are those of a receiver in
    the Atlantic — which the autopilot will believe.
    """
    lat = float(fixed.get("latitude") or 0.0)
    lon = float(fixed.get("longitude") or 0.0)
    if abs(lat) < 1e-9 and abs(lon) < 1e-9:
        return "the fixed base position is still 0, 0 — enter the surveyed mark first"
    if not -90.0 <= lat <= 90.0 or not -180.0 <= lon <= 180.0:
        return "the fixed base position is outside the range of a coordinate"
    return ""


def ntrip_problem(ntrip: dict[str, Any]) -> str:
    """Why an NTRIP caster cannot be reached, or "" when it can."""
    if not str(ntrip.get("host") or "").strip():
        return "no NTRIP caster address is set"
    if not str(ntrip.get("mountpoint") or "").strip():
        return "no NTRIP mountpoint is set"
    return ""


def survey_progress(survey: dict[str, Any] | None, target_duration: int,
                    target_accuracy: float) -> int:
    """How far along a survey is, as a percentage, for a progress bar.

    A survey ends when *both* its minimum duration has passed and its accuracy
    limit has been met, and neither alone predicts the other: a receiver under
    a tree can sit at 40% accuracy-wise for an hour. So the figure shown is the
    lesser of the two, which is the one still holding the survey up, and it
    stops at 99 until the receiver itself says valid — a bar that reads 100%
    for two minutes is worse than one that reads 99%.
    """
    if not survey:
        return 0
    if survey.get("valid") and not survey.get("active"):
        return 100
    duration = max(0, int(survey.get("duration") or 0))
    accuracy = float(survey.get("accuracy") or 0.0)
    by_time = 100.0 * duration / max(1, int(target_duration))
    if accuracy > 0 and target_accuracy > 0:
        # Accuracy converges roughly as one over the square root of time, so a
        # linear read of "how far from the limit" sits near zero for most of
        # the survey and then jumps. The ratio is used instead: at twice the
        # limit the survey is meaningfully half way there.
        by_accuracy = 100.0 * min(1.0, target_accuracy / accuracy)
    else:
        by_accuracy = 0.0
    return max(0, min(99, int(min(by_time, by_accuracy))))
