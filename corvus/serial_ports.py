"""Which serial port is what: flight controller, radio, RTK base, bootloader.

Pure functions over a device name and its USB descriptor, shared by the MAVLink
bridge (link transport, firmware flashing) and by auto-connect (which port to
dial). Nothing here opens a port.
"""
from __future__ import annotations

import re
# Serial nodes that are never an autopilot or a radio, and that bury the one
# that is. /dev/ttyS* are the Linux 8250 platform ports (always phantom on a
# field laptop); the two macOS entries are present on every Mac whether or not
# anything is plugged in — the Bluetooth one in particular sits at the top of
# an alphabetical list, which is where the operator looks first.
_PHANTOM_TTY_RE = re.compile(
    r"^/dev/ttyS[0-9]+$"
    r"|^/dev/(?:cu|tty)\.Bluetooth-Incoming-Port$"
    r"|^/dev/(?:cu|tty)\.debug-console$"
)

# Firmware-Flash transport classification. A CDC ACM node is a DIRECT USB link
# to a Pixhawk-class flight controller (flashable); a /dev/ttyUSB* node is a
# USB-to-serial adapter (SiK radio, NOT flashable). _BYID_TOKENS are the
# Pixhawk/STM32 vendor identifiers that appear in /dev/serial/by-id/ names and
# pyserial hwid strings (26AC = Pixhawk vendor ID, 0483 = STMicro STM32).
_DIRECT_USB_ACM_RE = re.compile(r"^/dev/ttyACM[0-9]+$")
_SIK_RADIO_RE = re.compile(r"^/dev/ttyUSB[0-9]+$")
_BYID_TOKENS: tuple[str, ...] = (
    "px4", "pixhawk", "fmu",
    "3d_robotics", "3d robotics",
    "hex_proficnc", "arducy", "mro",
    "hex ",
    "usb vid:pid=26ac", "usb vid:pid=0483",
)
_PIXHAWK_BYID_RE = re.compile(
    "|".join(re.escape(t) for t in _BYID_TOKENS), re.IGNORECASE,
)

# Windows names every serial port the same way, so the port name carries no
# information at all: COM7 is a Pixhawk, a SiK radio or a Bluetooth pairing
# with equal probability. The identity is in the USB descriptor pyserial
# reports as ``hwid`` instead, which is why the classification below looks the
# port up rather than reading its name.
_WINDOWS_COM_RE = re.compile(r"^(?:\\\\\.\\)?COM[0-9]+$", re.IGNORECASE)
# macOS names a USB serial node after the driver that claimed it, not after the
# class of device behind it: a Pixhawk on a USB cable comes up as
# /dev/cu.usbmodem14201 and a SiK radio as /dev/cu.usbserial-0001 or
# /dev/cu.SLAB_USBtoUART, and neither shape is one Linux ever produces. So the
# name is as uninformative here as a Windows COM number, and is resolved the
# same way: by asking the USB descriptor. Without this every macOS serial port
# classified as "unknown", which refused firmware flashing over a USB cable on
# the one platform Corvus ships a .dmg for. ("cu" is the call-out node, the one
# to open; "tty" is matched too because an operator may well type it.)
_MACOS_SERIAL_RE = re.compile(r"^/dev/(?:cu|tty)\..+$")
# The USB-to-serial bridges a SiK Telemetry Radio (and its clones) is built on.
# These are adapters: whatever is behind them, it is not a flashable FMU.
_USB_SERIAL_BRIDGE_TOKENS: tuple[str, ...] = (
    "vid:pid=0403",   # FTDI FT232 — Holybro/3DR SiK V2 and V3
    "vid:pid=10c4",   # Silicon Labs CP210x — the common SiK clone bridge
    "vid:pid=1a86",   # WCH CH340/CH341
    "vid:pid=067b",   # Prolific PL2303
)
_USB_SERIAL_BRIDGE_RE = re.compile(
    "|".join(re.escape(t) for t in _USB_SERIAL_BRIDGE_TOKENS), re.IGNORECASE,
)

# A board sitting in its USB bootloader enumerates as a serial port like any
# other, but it is not a link: it speaks the PX4 bootloader protocol (see
# corvus/firmware_uploader.py), not MAVLink, and opening it would hold the
# device the flasher is about to claim. 26AC:0011 is the PX4 BL FMU descriptor,
# 0483:DF11 is STM32 DFU; the words are what the descriptor strings say when
# the ids do not.
_BOOTLOADER_TOKENS: tuple[str, ...] = (
    "vid:pid=26ac:0011",
    "vid:pid=0483:df11",
    "bl fmu",
    "bootloader",
    "dfu",
)
_BOOTLOADER_RE = re.compile(
    "|".join(re.escape(t) for t in _BOOTLOADER_TOKENS), re.IGNORECASE,
)

# An RTK base station is a serial port that must never be dialled as a link.
# The distinction is not academic: a ZED-F9P on a USB lead enumerates as
# /dev/ttyACM0 exactly like a Pixhawk does, so without this table auto-connect
# would pick up the GPS receiver plugged in beside the aircraft, hold its port,
# and wait for a heartbeat that a GNSS receiver has no way to send. 0x1546 is
# u-blox's own vendor id and is never a flight controller; 0x152a is
# Septentrio. The words are for the descriptors that carry a product name
# instead of a recognised id.
#
# Closed-world, in the opposite direction from is_bootloader_port(): only a
# positive match is treated as a base station, because the cost of the two
# errors is again asymmetric. Refusing to auto-connect a real flight
# controller because its descriptor said something unexpected is the worse
# failure by a distance.
_RTK_TOKENS: tuple[str, ...] = (
    "vid:pid=1546",   # u-blox — F9P/M8P dev kits and the boards built on them
    "vid:pid=152a",   # Septentrio
    "u-blox",
    "ublox",
    "septentrio",
    "zed-f9p",
    "simplertk",
)
_RTK_RE = re.compile(
    "|".join(re.escape(t) for t in _RTK_TOKENS), re.IGNORECASE,
)

# Devices that are a serial port to the kernel and never a vehicle link: the
# pseudo-terminals a test harness or a terminal emulator creates, and the
# phantom nodes _PHANTOM_TTY_RE already names. Auto-connect enumerates without
# an operator watching, so it has to refuse these by itself.
_PTY_DEVICE_RE = re.compile(r"^/dev/(?:pts/[0-9]+|ptyp?[0-9a-z]+|ttyp[0-9a-z]+)$")


def is_phantom_device(device: str) -> bool:
    """Is *device* a serial node that is never a vehicle link?

    The 8250 platform ports and the two macOS entries that exist whether or not
    anything is plugged in (:data:`_PHANTOM_TTY_RE`), plus pseudo-terminals.
    Shared with :meth:`MavlinkBridge.list_serial_ports` so the enumeration the
    operator sees and the one auto-connect picks from agree on what is real.
    """
    name = str(device or "").strip()
    if not name:
        return True
    return bool(_PHANTOM_TTY_RE.match(name) or _PTY_DEVICE_RE.match(name))


def is_bootloader_port(device: str, hwid: str = "", description: str = "") -> bool:
    """Is this port a flight controller sitting in its USB bootloader?

    Closed-world on purpose: only a positive match counts. An unknown
    descriptor is treated as a normal flight controller, because the cost of
    the two errors is not symmetric — skipping a real FC leaves an operator
    with a ground station that will not connect to the aircraft in front of
    them, while dialling a bootloader costs one failed connect attempt that the
    reconnect loop already handles. The device name carries no bootloader
    marking on any platform, so this reads the descriptor only.
    """
    text = f"{hwid or ''} {description or ''}"
    return bool(text.strip()) and bool(_BOOTLOADER_RE.search(text))


def is_rtk_device(device: str, hwid: str = "", description: str = "") -> bool:
    """Is this port an RTK GNSS receiver rather than something to fly?

    Reads the descriptor only. The device *name* cannot answer this on any
    platform — a receiver's USB CDC node is indistinguishable from a flight
    controller's — which is the whole reason auto-connect used to dial one.
    See :data:`_RTK_TOKENS` for why the match is closed-world.
    """
    text = f"{hwid or ''} {description or ''}"
    return bool(text.strip()) and bool(_RTK_RE.search(text))


def classify_hwid(hwid: str) -> str:
    """'usb' / 'sik' / 'rtk' / 'unknown' from a USB descriptor string.

    Bridge first: an FTDI descriptor may also carry a product string with a
    vendor name in it, and a bridge is never the flight controller. Unknown
    wins ties — see :meth:`MavlinkBridge._classify_by_descriptor` for why
    guessing permissively here would offer to flash firmware down a radio.
    """
    if not hwid or not hwid.strip():
        return "unknown"
    if _RTK_RE.search(hwid):
        return "rtk"
    if _USB_SERIAL_BRIDGE_RE.search(hwid):
        return "sik"
    if _PIXHAWK_BYID_RE.search(hwid):
        return "usb"
    return "unknown"


def classify_serial_device(device: str, hwid: str = "", description: str = "") -> str:
    """'usb' / 'sik' / 'rtk' / 'unknown' for an enumerated serial port.

    The port-shaped half of :meth:`MavlinkBridge.transport`, factored out so a
    caller holding a ``list_serial_ports()`` row can classify it without
    building a connection string and without a second enumeration pass. Linux
    device names carry the answer; Windows COM numbers and macOS ``/dev/cu.*``
    nodes do not, so those fall through to the descriptor the caller already
    has.
    """
    name = str(device or "").strip()
    if not name:
        return "unknown"
    # Before the name is read, not after: a u-blox receiver's CDC node is a
    # /dev/ttyACM* like any Pixhawk's, so a name-first order classifies every
    # RTK base on Linux as a flight controller.
    if is_rtk_device(name, hwid, description):
        return "rtk"
    if _DIRECT_USB_ACM_RE.match(name):
        return "usb"
    if name.startswith("/dev/serial/by-id/") and _PIXHAWK_BYID_RE.search(name):
        return "usb"
    if _SIK_RADIO_RE.match(name):
        return "sik"
    if is_windows_com_port(name) or _MACOS_SERIAL_RE.match(name):
        return classify_hwid(f"{hwid or ''} {description or ''}")
    return "unknown"


def is_windows_com_port(device: str) -> bool:
    """Is *device* a Windows COM port name (``COM7``, ``\\\\.\\COM12``)?

    Matched by shape rather than by ``os.name`` so the classification can be
    exercised from a test on any host — and so a COM name typed into the
    connection field on Linux is still recognised for what it is.
    """
    return bool(_WINDOWS_COM_RE.match(str(device or "").strip()))
