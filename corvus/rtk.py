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

from corvus_link import rtk as _rtk
from corvus_link.rtk import (  # noqa: F401 - this module's interface
    RtcmFramer,
    UbxParser,
    _hp_split,
    _value_bytes,
    activate_output_frames,
    crc24q,
    defaults,
    disable_base_frames,
    fixed_position_problem,
    fragments,
    legacy_activate_output_frames,
    legacy_cfg_msg,
    legacy_disable_base_frames,
    legacy_stop_output_frames,
    legacy_survey_in_frames,
    message_number,
    ntrip_problem,
    parse_mon_ver,
    parse_nav_svin,
    poll_frame,
    public_settings,
    receiver_label,
    settings,
    stop_output_frames,
    supports_valset,
    survey_in_frames,
    survey_progress,
    ubx_checksum,
    ubx_frame,
    valset,
)

RTCM_PREAMBLE = 0xD3

# The largest payload a 10-bit length field can announce. A frame is the
# 3-byte header plus this plus the 3-byte CRC.
RTCM_MAX_PAYLOAD = 1023

# CRC-24Q ("Qualcomm"), the check every RTCM 3 frame ends with: polynomial
# 0x1864CFB, initial value 0, no final XOR. Built once at import rather than
# written out as 256 literals — the table is derived, so a typo in it would be
# a silent wrong answer on one byte value in 256.
_CRC24Q_POLY = 0x1864CFB

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


def rtcm_keys(msm7: bool = False) -> tuple[int, ...]:
    """The RTCM message set a base is configured to emit."""
    return RTCM_MSM7_KEYS if msm7 else RTCM_MSM4_KEYS


def fixed_position_frames(
    latitude: float, longitude: float, altitude: float,
    accuracy_m: float = 0.1, msm7: bool = False,
) -> list[bytes]:
    """Configure a base at a position the operator already knows.

    *altitude* is metres above the ellipsoid, not AMSL.
    """
    return _rtk.fixed_position_frames(latitude, longitude, altitude, accuracy_m, msm7)


def legacy_fixed_position_frames(
    latitude: float, longitude: float, altitude: float,
    accuracy_m: float = 0.1, msm7: bool = False,
) -> list[bytes]:
    """CFG-TMODE3 fixed position, for an M8P generation receiver."""
    return _rtk.legacy_fixed_position_frames(latitude, longitude, altitude, accuracy_m, msm7)
