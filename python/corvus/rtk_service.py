"""The RTK base station: one thread, one source of corrections, one link.

:mod:`corvus.rtk` knows the wire formats. This module is the part that has to
deal with reality — a receiver that may or may not be plugged in, may or may not
be the thing the operator meant, and may be halfway through a survey when the
laptop lid is closed.

Plug and play, and what that costs
----------------------------------
This runs only once the operator has switched RTK on; a station that has never
been configured opens no serial port looking for a base. Once on, it needs
nothing else: a base plugged into the ground station is found, surveyed and
streaming without another setting being touched. The price of a search that
*acts* is that it must be impossible for it to act on the wrong thing, so it is
narrow in three separate ways:

1. Only a port whose USB descriptor identifies it as a GNSS receiver is
   considered (:func:`corvus.mavlink_bridge.is_rtk_device`). A Pixhawk is never
   a candidate, whatever else is or is not plugged in.
2. The port the MAVLink bridge is connected on is excluded outright, even if it
   somehow matched. Two readers on one serial device do not error; they each get
   an arbitrary half of the bytes, and half a telemetry link is a worse outcome
   than no RTK.
3. Nothing is ever written to a device that has not first answered ``MON-VER``
   as a u-blox receiver — except the RTCM it is already emitting, which is read
   and forwarded without configuring anything.

Point 3 is the pass-through case and it matters more than it looks: a base that
somebody already set up in u-center, or a make of receiver this module has never
heard of, streams RTCM 3 the moment it is powered. Forwarding that requires
knowing nothing about it. So an unrecognised device is not refused, it is
*listened* to, and only a receiver that identifies itself is configured.

The survey, and why it is a state machine rather than a function
----------------------------------------------------------------
Survey-in is minutes long, and every one of those minutes is one the operator is
watching a progress bar for. It also cannot be driven to completion by writing
and waiting: the receiver applies a time-mode change on a navigation epoch
rather than on the ACK, reapplying survey-in mode does not restart a survey that
is already running, and the only way to know any of this happened is to read
``NAV-SVIN`` back. So the session is a loop over whatever arrives — UBX and RTCM
on the same cable, parsed by two parsers that each ignore the other's bytes —
and the state is what the receiver last said, not what it was last told.

Lifecycle
---------
One daemon thread, one ``Event``-driven sleep, one transport open at a time.
``stop()`` sets the event and closes the transport under the thread, which is
what makes a blocking read return; the thread is then joined. A session that
configured a receiver puts it back out of base mode on the way out, so Corvus
never leaves somebody's GNSS receiver in a state where it computes no position
of its own.
"""

from __future__ import annotations

import logging
import sys
import threading
import time  # noqa: F401 - the engine reads the clock here
from typing import Any
from collections.abc import Callable

from corvus_link import rtk as _rtk

from . import rtk
from .serial_ports import is_phantom_device, is_rtk_device  # noqa: F401 - read by the engine

logger = logging.getLogger("corvus.rtk")


# States, as the page renders them. Contract, not prose: they are published in
# the store as ``rtk_state`` and asserted in the test suite.
STATE_OFF = "off"
STATE_SEARCHING = "searching"
STATE_CONNECTING = "connecting"
STATE_SURVEYING = "surveying"
STATE_ACTIVE = "active"
STATE_ERROR = "error"

STATES: frozenset[str] = frozenset({
    STATE_OFF, STATE_SEARCHING, STATE_CONNECTING, STATE_SURVEYING,
    STATE_ACTIVE, STATE_ERROR,
})

# How often the search looks for a newly plugged-in base. Matches the
# auto-connect watcher's cadence for the same reason it chose it: fast enough
# that plugging something in feels immediate, slow enough that enumerating
# serial ports is not a background load.
SEARCH_INTERVAL_S = 2.5

# Backoff after a session ends badly, so a receiver that answers and then fails
# to configure is not retried in a tight loop.
RETRY_INTERVAL_S = 5.0

# How long one blocking read may wait. Short enough that ``stop()`` is prompt
# without needing the port closed under the thread, long enough that an idle
# base does not spin.
READ_TIMEOUT_S = 0.5

# How long a candidate baud rate is given to produce a MON-VER reply before the
# next is tried. A receiver answers a poll in tens of milliseconds; the rest is
# for a device that is mid-sentence when the port opens.
IDENTIFY_TIMEOUT_S = 1.2

# How long a device that never identified itself is listened to before it is
# accepted as a pass-through base or given up on. One second of a 1 Hz base is
# not enough to be sure, so this is deliberately longer than the identify.
LISTEN_TIMEOUT_S = 5.0

# How long the old survey is given to stop before the new one is configured
# anyway. Matches the PX4 driver's three seconds.
SURVEY_STOP_TIMEOUT_S = 3.0

# Corrections older than this mean the source has gone quiet even though the
# port is still open — an unpowered base, an NTRIP caster that stopped without
# closing the socket. The session is restarted rather than left looking alive.
SOURCE_SILENCE_S = 20.0

# NTRIP: how long the caster is given to accept the request, and the size of
# one read. Kept small because the socket is read on the same thread that
# injects.
NTRIP_CONNECT_TIMEOUT_S = 10.0
NTRIP_READ_BYTES = 4096


RtkError = _rtk.RtkError

# What a correction source has to be able to do (read, write, close, a label,
# whether it can be configured, and bytes already read while opening it):
# a serial port, or a socket held open to an NTRIP caster.
SerialTransport = _rtk.SerialTransport
NtripTransport = _rtk.NtripTransport


class RtkService:
    """Find a source of RTCM corrections and keep it on the link.

    *open_port*, *list_ports* and *open_socket* are injectable so the whole
    state machine — search, identify, survey, inject, recover — can be driven
    from the test suite without a receiver, a tripod or three minutes.
    """

    def __init__(
        self,
        mavlink: Any,
        store: Any,
        settings: Any = None,
        *,
        open_port: Callable[[str, int], Any] | None = None,
        list_ports: Callable[[], list[dict[str, str]]] | None = None,
        open_socket: Callable[[str, int, float], Any] | None = None,
    ) -> None:
        self._mavlink = mavlink
        self._store = store
        self._lock = threading.Lock()
        self._settings = rtk.settings(settings)
        self._open_port = open_port or self._default_open_port
        self._list_ports = list_ports or self._default_list_ports
        self._open_socket = open_socket or self._default_open_socket

        self._thread: threading.Thread | None = None
        self._stop = threading.Event()
        # Set to ask the running session to end and the loop to start over —
        # a settings change, or the operator pressing Restart survey.
        self._restart = threading.Event()
        self._transport: Any = None

        # Everything the page reads. Held under _lock and copied out, never
        # handed over: status() is called from HTTP threads while the session
        # thread is writing.
        self._state = STATE_OFF
        self._message = ""
        self._error = ""
        self._device = ""
        self._baud = 0
        self._receiver: dict[str, Any] | None = None
        self._survey: dict[str, Any] | None = None
        self._warning = ""
        self._frames = 0
        self._frame_bytes = 0
        self._crc_errors = 0
        self._last_frame_at = 0.0
        self._started_at = 0.0
        self._messages_seen: dict[str, int] = {}

    # -- lifecycle ---------------------------------------------------------

    def start(self) -> None:
        """Start the session thread. Idempotent."""
        _engine.start(self)

    def stop(self, timeout: float = 3.0) -> None:
        """Stop the session thread and release the port or socket.

        The transport is *not* closed from this thread first, which looks like
        the obvious way to unblock a read and is the thing that quietly broke
        the teardown: closing it under the session thread means the session's
        own exit path cannot write to it any more, and that path is what takes
        the receiver back out of base mode. Every read carries a timeout, so
        the thread notices ``_stop`` within one of them on its own.

        Closing under it stays as the last resort for a thread that did not
        come back — a port that has gone away mid-read can block past its
        timeout — at which point a receiver left configured is the lesser
        problem.
        """
        _engine.stop(self, timeout)

    def shutdown(self) -> None:
        """Alias for :meth:`stop`, for the launcher teardown path."""
        self.stop()

    def is_alive(self) -> bool:
        """Is the session thread running?"""
        thread = self._thread
        return bool(thread is not None and thread.is_alive())

    # -- settings ----------------------------------------------------------

    def settings(self) -> dict[str, Any]:
        """The settings in force, resolved against the defaults."""
        return _engine.settings(self)

    def apply_settings(self, raw: Any) -> dict[str, Any]:
        """Replace the settings and restart the session against them.

        Everything the running session learned belongs to the source it
        learned it from, so it is dropped rather than carried across: a survey
        that converged under a two-metre limit says nothing about one asked for
        half a metre, and a receiver identified on one port is not the one on
        another.
        """
        return _engine.apply_settings(self, raw)

    def restart_survey(self) -> None:
        """Start the survey again from zero.

        The operator's answer to a base that converged somewhere it should not
        have — a survey run before the tripod was level, or with the antenna
        still indoors. There is no way to ask a receiver to redo a survey in
        place, so the session is torn down and rebuilt, which re-runs the
        configuration from the top.
        """
        _engine.restart_survey(self)

    # -- status ------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Everything the RTK page shows, in one read.

        Deliberately one call rather than several: the page polls this while a
        survey runs, and a status assembled from three endpoints can show a
        survey that is finished next to a link that has not started.
        """
        return _engine.status(self)

    # -- the session thread ------------------------------------------------

    def _loop(self) -> None:
        """Open a source, run it until it ends, and start over."""
        _engine.run(self)

    def _acquire(self, settings: dict[str, Any]) -> Any:
        """Open whichever source the settings name, or return None to retry."""
        return _engine.acquire(self, settings)

    # -- serial ------------------------------------------------------------

    def _acquire_serial(self, settings: dict[str, Any]) -> Any:
        return _engine.acquire_serial(self, settings)

    def _probe(self, transport: SerialTransport) -> tuple[str | None, dict[str, Any] | None]:
        """What is on this port at this baud rate, in one listen.

        Two answers count, and either ends the search:

        ``("ubx", version)``
            The device answered ``MON-VER``. It is a u-blox receiver, it is at
            the right baud rate, and it can be configured.

        ``("rtcm", None)``
            The device said nothing about itself but emitted a *valid* RTCM 3
            frame. That is conclusive — a 24-bit CRC over a whole frame is not
            passed by noise read at the wrong baud rate — and it is the answer
            for a base somebody already configured elsewhere, which is
            forwarded without being written to.

        Checking for both in the same window rather than one after the other
        is what keeps an already-streaming base from costing a full sweep of
        every candidate rate before anybody notices it was working all along.

        The poll is the only thing ever written to an unrecognised device.
        """
        return tuple(_engine.probe(self, transport))

    # -- NTRIP -------------------------------------------------------------

    def _acquire_ntrip(self, settings: dict[str, Any]) -> Any:
        """Open a caster stream, or raise with the reason it could not be."""
        return _engine.acquire_ntrip(self, settings)

    # -- one session -------------------------------------------------------

    def _run_session(self, transport: Any, settings: dict[str, Any],
                     version: dict[str, Any] | None) -> None:
        """Configure the source if it can be, then forward until it ends."""
        _engine.run_session(self, transport, settings, version)

    def _configure(
        self, transport: Any, settings: dict[str, Any],
        framer: rtk.RtcmFramer, ubx: rtk.UbxParser,
        version: dict[str, Any] | None,
    ) -> None:
        """Put a recognised receiver into base mode and wait for it to be ready."""
        _engine.configure(self, transport, settings, framer, ubx, version)

    def _activate(self, transport: Any, modern: bool, msm7: bool) -> None:
        _engine.activate(self, transport, modern, msm7)

    def _write_all(self, transport: Any, frames: list[bytes],
                   force: bool = False) -> None:
        """Write *frames* in order, pausing between them.

        *force* is for the teardown path only: it is the one write that has to
        happen **because** the service is stopping, so it is the one that must
        not check the stop flag it would always find set.
        """
        _engine.write_all(self, transport, frames, force)

    def _await_survey_stopped(
        self, transport: Any, ubx: rtk.UbxParser,
        framer: rtk.RtcmFramer, modern: bool,
    ) -> None:
        """Wait until NAV-SVIN reports no survey running.

        Not optional and not cosmetic: asking for survey-in while one is
        already running is a no-op on a u-blox receiver, so without this the
        accuracy and duration the operator just set would silently not apply
        and the page would show the *old* survey converging.

        A receiver that never answers is not an error. It is reported as a
        warning and the configuration goes ahead, because the alternative —
        refusing to run — turns a receiver that is merely quiet about its
        survey state into one that cannot be used at all.
        """
        _engine.await_survey_stopped(self, transport, ubx, framer, modern)

    def _await_survey_complete(
        self, transport: Any, ubx: rtk.UbxParser,
        framer: rtk.RtcmFramer, settings: dict[str, Any],
    ) -> None:
        """Read NAV-SVIN until the survey is valid and no longer running.

        No timeout, on purpose. A survey under a limit the sky will not allow
        runs for as long as the operator leaves it running, and the honest
        report of that is a progress bar that is not moving — not a session
        that gives up after some number this module invented and leaves the
        base configured but not streaming.
        """
        _engine.await_survey_complete(self, transport, ubx, framer, settings)

    # -- bytes in ----------------------------------------------------------

    def _consume(
        self, chunk: bytes, framer: rtk.RtcmFramer, ubx: rtk.UbxParser,
    ) -> dict[str, Any] | None:
        """Feed one read to both parsers; inject the corrections it finished.

        Returns the survey report this chunk carried, if it carried one, so the
        two waiting loops above can key on it without a second parse.
        """
        return _engine.consume(self, chunk, framer, ubx)

    def _note_frame(self, frame: bytes) -> None:
        _engine.note_frame(self, frame)

    # -- teardown ----------------------------------------------------------

    def _release(self, transport: Any, settings: dict[str, Any],
                 version: dict[str, Any] | None) -> None:
        """Close the source, taking a configured receiver out of base mode.

        Runs on the way out of *every* session, including the one that ends
        because the application is shutting down. A receiver left in base mode
        computes no position of its own, and the next thing that opens it —
        u-center, a rover configuration, the operator's other laptop — finds a
        GNSS receiver that reports nothing and no indication why.
        """
        _engine.release(self, transport, settings, version)

    # -- state -------------------------------------------------------------

    def _link_device(self) -> str:
        """The serial device the MAVLink bridge is using, or ""."""
        return _engine.link_device(self)

    def _link_ready(self) -> bool:
        return _engine.link_ready(self)

    def _candidate_ports(self) -> list[dict[str, str]]:
        """Serial ports that look like a base station, link port excluded."""
        return _engine.candidate_ports(self)

    def _publish(self, state: str, message: str) -> None:
        _engine.publish(self, state, message)

    def _publish_progress(self, survey: dict[str, Any]) -> None:
        _engine.publish_progress(self, survey)

    def _store_update(
        self, state: str, survey: dict[str, Any] | None, settings: dict[str, Any],
    ) -> None:
        """Put the four fields the rest of the app reads into the store.

        Only four, and all scalars: this goes out on the telemetry stream with
        every frame, and the detail the page needs belongs in ``status()``,
        which is polled once a second while somebody is looking at it.
        """
        _engine.store_update(self, state, survey, settings)

    def _fail(self, reason: str) -> None:
        _engine.fail(self, reason)

    def _warn(self, text: str) -> None:
        _engine.warn(self, text)

    def _reset_counters(self) -> None:
        _engine.reset_counters(self)

    def _describe_intent(self) -> str:
        return _engine.describe_intent(self)

    # -- defaults for the injectables --------------------------------------

    @staticmethod
    def _default_open_port(device: str, baud: int) -> Any:
        return _engine.open_port(device, baud)

    @staticmethod
    def _default_list_ports() -> list[dict[str, str]]:
        from .mavlink_bridge import MavlinkBridge
        return MavlinkBridge.list_serial_ports()

    @staticmethod
    def _default_open_socket(host: str, port: int, timeout: float) -> Any:
        return _engine.open_socket(host, port, timeout)


def _ntrip_request(
    host: str, port: int, mountpoint: str, username: str, password: str,
) -> bytes:
    """The GET a caster expects, as NTRIP 2.0 with a 1.0 fallback built in.

    Version 2 is asked for by header and version-1 casters ignore the header
    and answer ``ICY 200 OK`` anyway, so one request serves both — which is
    why there is no version setting on the page.
    """
    return _engine.ntrip_request(host, port, mountpoint, username, password)


def _read_ntrip_header(sock: Any) -> bytes:
    """Consume the caster's response header; return the bytes after it.

    A caster answers and then streams in the same socket with no framing
    between the two, so the first correction is routinely in the same packet
    as the last header line. Those bytes are returned rather than dropped —
    dropping them costs one correction on every connect, which is a fix that
    takes a second longer to come back for no reason.
    """
    return _engine.read_ntrip_header(sock)


_engine = _rtk.RtkEngine(sys.modules[__name__])
