"""Open a serial MAVLink link with its DTR and RTS lines under control.

An ExpressLRS transmitter module, like most ESP32 and ESP8285 boards behind a
CP210x or CH340 bridge, wires DTR and RTS to its boot-mode and reset pins.
Opening a port asserts both lines, so a plain ``mavlink_connection`` reboots
the module (or drops it into its bootloader) every time Corvus connects, and
the link never comes up. The lines must be released *before* the port opens,
not right after it: the reset pulse is already gone by then.

pymavlink's ``mavserial`` opens the port inside its own constructor and offers
no hook for that, so :class:`HeldLineSerial` is a pyserial port that sets its
line state first, and :func:`open_serial_link` builds the pymavlink serial
object around it the way ``mavserial.__init__`` does. A link that asks for
nothing special still goes through ``mavlink_connection`` untouched.
"""
from __future__ import annotations

import logging
from typing import Any

from pymavlink import mavutil

from .serial_ports import SerialOptions

logger = logging.getLogger("corvus.serial")


def held_line_port_class() -> type:
    """A ``serial.Serial`` subclass that opens with DTR and RTS already set."""
    import serial

    class HeldLineSerial(serial.Serial):
        """A port whose DTR and RTS are never asserted by the act of opening it."""

        def __init__(self, *args: Any, dtr: bool | None = None,
                     rts: bool | None = None, **kwargs: Any) -> None:
            self._held_dtr = dtr
            self._held_rts = rts
            super().__init__(*args, **kwargs)

        def open(self) -> None:
            if self._held_dtr is not None:
                self._dtr_state = self._held_dtr
            if self._held_rts is not None:
                self._rts_state = self._held_rts
            super().open()

    return HeldLineSerial


class HeldLineMavSerial(mavutil.mavserial):
    """``mavutil.mavserial`` around a port that opens with its lines held.

    Mirrors ``mavserial.__init__`` of pymavlink 2.4: the port is opened at
    1200 baud and moved to the real rate afterwards, which works around a
    kernel bug in the baud rate of the first open.
    """

    def __init__(self, device: str, baud: int, options: SerialOptions,
                 source_system: int, source_component: int,
                 port_class: type | None = None) -> None:
        port_class = port_class or held_line_port_class()
        self.baud = baud
        self.device = device
        self.autoreconnect = False
        self.force_connected = False
        self.port = port_class(
            device, 1200, timeout=0, dsrdtr=False, rtscts=False, xonxoff=False,
            dtr=options.dtr, rts=options.rts,
        )
        try:
            fd = self.port.fileno()
            mavutil.set_close_on_exec(fd)
        except Exception:  # noqa: BLE001 - Windows has no descriptor to mark
            fd = None
        self.set_baudrate(self.baud)
        mavutil.mavfile.__init__(
            self, fd, device, source_system=source_system,
            source_component=source_component, use_native=mavutil.default_native,
        )
        self.rtscts = False


def open_serial_link(device: str, baud: int, options: SerialOptions,
                     source_system: int, source_component: int) -> Any:
    """The pymavlink connection for a serial port, honouring *options*."""
    if not options.sets_lines:
        return mavutil.mavlink_connection(
            device, baud=baud, source_system=source_system,
            source_component=source_component,
        )
    logger.info("Opening %s with DTR=%s RTS=%s", device, options.dtr, options.rts)
    return HeldLineMavSerial(device, baud, options, source_system, source_component)
