"""Open a serial MAVLink link with its DTR and RTS lines under control.

An ExpressLRS transmitter module, like most ESP32 and ESP8285 boards behind a
CP210x or CH340 bridge, wires DTR and RTS to its boot-mode and reset pins.
Opening a port asserts both lines, so a plain ``mavlink_connection`` reboots
the module (or drops it into its bootloader) every time Corvus connects, and
the link never comes up. corvus_link sets the lines to the asked state in the
same step as it opens the port (``dtr`` and ``rts`` keyword arguments of
``mavlink_connection``), and a link that asks for nothing special opens as it
always did.
"""
from __future__ import annotations

import logging
from typing import Any

from corvus_link import mavutil

from .serial_ports import SerialOptions

logger = logging.getLogger("corvus.serial")


def open_serial_link(device: str, baud: int, options: SerialOptions,
                     source_system: int, source_component: int) -> Any:
    """The connection for a serial port, honouring *options*.

    corvus_link sets the lines as the port opens, so a transmitter module is
    not reset by the act of connecting.
    """
    kwargs: dict[str, Any] = {
        "baud": baud, "source_system": source_system,
        "source_component": source_component,
    }
    if options.sets_lines:
        logger.info("Opening %s with DTR=%s RTS=%s", device, options.dtr, options.rts)
        kwargs["dtr"] = options.dtr
        kwargs["rts"] = options.rts
    return mavutil.mavlink_connection(device, **kwargs)
