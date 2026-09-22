"""The RTK base station: one thread, one source of corrections, one link.

:mod:`corvus.rtk` knows the wire formats. This module is the part that has to
deal with reality — a receiver that may or may not be plugged in, may or may not
be the thing the operator meant, and may be halfway through a survey when the
laptop lid is closed.

Plug and play, and what that costs
----------------------------------
The default is that this runs. An operator who plugs a base station into the
ground station and never opens the RTK page gets an RTK fix, because that is
what every other station does and an option nobody turns on is an option nobody
has. The price of a default that *acts* is that it must be impossible for it to
act on the wrong thing, so the search is narrow in three separate ways:

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

import base64
import logging
import socket
import threading
import time
from typing import Any
from collections.abc import Callable

from . import rtk
from .mavlink_bridge import is_phantom_device, is_rtk_device

logger = logging.getLogger("corvus.rtk")

try:  # pragma: no cover - import-time guard, mirrors sik_service
    import serial as _serial  # type: ignore[import-not-found]
except ImportError:  # pragma: no cover
    _serial = None  # type: ignore[assignment]

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


class RtkError(RuntimeError):
    """A session could not be run, with the reason an operator can act on."""


class _Transport:
    """What a correction source has to be able to do.

    Two implementations — a serial port and an NTRIP socket — and the session
    loop is written against this rather than against either, so the survey and
    injection code is the same whichever end the bytes came from. ``write`` is
    a no-op on a source that cannot be configured.
    """

    label = ""
    configurable = False
    # Bytes read while opening the source that are already corrections. An
    # NTRIP caster routinely puts the first one in the same packet as the last
    # header line; dropping it would cost a correction on every connect.
    pending = b""

    def read(self) -> bytes:  # pragma: no cover - interface
        raise NotImplementedError

    def write(self, data: bytes) -> None:  # pragma: no cover - interface
        raise NotImplementedError

    def close(self) -> None:  # pragma: no cover - interface
        raise NotImplementedError


class SerialTransport(_Transport):
    """A serial port, opened read/write at one baud rate."""

    configurable = True

    def __init__(self, port: Any, device: str, baud: int) -> None:
        self._port = port
        self.device = device
        self.baud = baud
        self.label = f"{device} @ {baud}"

    def read(self) -> bytes:
        waiting = 0
        try:
            waiting = int(getattr(self._port, "in_waiting", 0) or 0)
        except Exception:  # noqa: BLE001 - a closing port raises here
            waiting = 0
        try:
            return self._port.read(max(1, min(waiting, 4096))) or b""
        except Exception as exc:  # noqa: BLE001
            raise RtkError(f"read failed: {exc}") from exc

    def write(self, data: bytes) -> None:
        try:
            self._port.write(data)
            flush = getattr(self._port, "flush", None)
            if callable(flush):
                flush()
        except Exception as exc:  # noqa: BLE001
            raise RtkError(f"write failed: {exc}") from exc

    def close(self) -> None:
        try:
            self._port.close()
        except Exception:  # noqa: BLE001 - teardown must not raise
            pass


class NtripTransport(_Transport):
    """A socket holding one NTRIP caster stream open."""

    configurable = False

    def __init__(self, sock: Any, label: str) -> None:
        self._sock = sock
        self.label = label

    def read(self) -> bytes:
        try:
            return self._sock.recv(NTRIP_READ_BYTES) or b""
        except TimeoutError:
            return b""
        except OSError as exc:
            raise RtkError(f"caster read failed: {exc}") from exc

    def write(self, data: bytes) -> None:
        # A caster takes a position upload (NMEA GGA) from a rover and nothing
        # from a ground station relaying to one. Nothing to send.
        return None

    def close(self) -> None:
        try:
            self._sock.close()
        except Exception:  # noqa: BLE001 - teardown must not raise
            pass


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
        self._transport: _Transport | None = None

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
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop.clear()
        self._restart.clear()
        self._thread = threading.Thread(target=self._loop, name="rtk-base", daemon=True)
        self._thread.start()
        logger.info("RTK service started (%s)", self._describe_intent())

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
        self._stop.set()
        self._restart.set()
        thread = self._thread
        if thread is not None and thread.is_alive():
            thread.join(timeout=timeout)
            if thread.is_alive():
                transport = self._transport
                if transport is not None:
                    transport.close()
                thread.join(timeout=1.0)
            if thread.is_alive():
                logger.warning("RTK session thread did not stop within %.1fs", timeout)
        self._thread = None
        self._publish(STATE_OFF, "RTK corrections are not running")

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
        with self._lock:
            return rtk.settings(self._settings)

    def apply_settings(self, raw: Any) -> dict[str, Any]:
        """Replace the settings and restart the session against them.

        Everything the running session learned belongs to the source it
        learned it from, so it is dropped rather than carried across: a survey
        that converged under a two-metre limit says nothing about one asked for
        half a metre, and a receiver identified on one port is not the one on
        another.
        """
        resolved = rtk.settings(raw)
        with self._lock:
            self._settings = resolved
            self._receiver = None
            self._survey = None
            self._warning = ""
            self._error = ""
        self._reset_counters()
        # The restart flag alone, and deliberately not a close: every read
        # carries a timeout, so the session notices within one of them and
        # still has a live port to undo its own configuration on.
        self._restart.set()
        if resolved.get("enabled") and not self.is_alive() and not self._stop.is_set():
            self.start()
        return resolved

    def restart_survey(self) -> None:
        """Start the survey again from zero.

        The operator's answer to a base that converged somewhere it should not
        have — a survey run before the tripod was level, or with the antenna
        still indoors. There is no way to ask a receiver to redo a survey in
        place, so the session is torn down and rebuilt, which re-runs the
        configuration from the top.
        """
        with self._lock:
            self._survey = None
            self._warning = ""
        self._restart.set()

    # -- status ------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Everything the RTK page shows, in one read.

        Deliberately one call rather than several: the page polls this while a
        survey runs, and a status assembled from three endpoints can show a
        survey that is finished next to a link that has not started.
        """
        settings = self.settings()
        with self._lock:
            state = self._state
            message = self._message
            error = self._error
            device = self._device
            baud = self._baud
            receiver = dict(self._receiver) if self._receiver else None
            survey = dict(self._survey) if self._survey else None
            warning = self._warning
            frames = self._frames
            frame_bytes = self._frame_bytes
            crc_errors = self._crc_errors
            last_frame_at = self._last_frame_at
            started_at = self._started_at
            messages_seen = dict(self._messages_seen)

        injected = {}
        try:
            injected = self._mavlink.rtcm_stats()
        except Exception:  # noqa: BLE001 - status must never raise
            injected = {"bytes": 0, "messages": 0, "dropped": 0, "age": None}

        snapshot = {}
        try:
            snapshot = self._store.get_snapshot()
        except Exception:  # noqa: BLE001
            snapshot = {}

        progress = rtk.survey_progress(
            survey, settings["survey_duration"], settings["survey_accuracy"],
        )
        return {
            "enabled": bool(settings.get("enabled")),
            "state": state,
            "message": message,
            "error": error,
            "warning": warning,
            "source": settings.get("source", "usb"),
            "mode": settings.get("mode", "survey"),
            "device": device,
            "baud": baud,
            "receiver": receiver,
            "receiver_label": rtk.receiver_label(receiver),
            "survey": survey,
            "survey_progress": progress,
            "survey_target": {
                "accuracy": settings["survey_accuracy"],
                "duration": settings["survey_duration"],
            },
            "frames": frames,
            "frame_bytes": frame_bytes,
            "crc_errors": crc_errors,
            "source_age": (time.monotonic() - last_frame_at) if last_frame_at else None,
            "uptime": (time.monotonic() - started_at) if started_at else None,
            "messages": messages_seen,
            "injected": injected,
            "link_ready": self._link_ready(),
            "vehicle": {
                "fix": snapshot.get("gps_fix", ""),
                "satellites": snapshot.get("gps_satellites", 0),
                "hdop": snapshot.get("gps_hdop", 0),
            },
            "ports": self._candidate_ports(),
            "settings": rtk.public_settings(settings),
            "defaults": rtk.public_settings(rtk.defaults()),
        }

    # -- the session thread ------------------------------------------------

    def _loop(self) -> None:
        """Open a source, run it until it ends, and start over."""
        while not self._stop.is_set():
            settings = self.settings()
            if not settings.get("enabled"):
                self._publish(STATE_OFF, "RTK corrections are switched off")
                self._stop.wait(SEARCH_INTERVAL_S)
                continue
            self._restart.clear()
            transport = None
            try:
                transport = self._acquire(settings)
            except RtkError as exc:
                self._fail(str(exc))
            except Exception as exc:  # noqa: BLE001 - never end the thread
                logger.exception("RTK source could not be opened")
                self._fail(f"could not open the correction source: {exc}")
            if transport is None:
                self._stop.wait(
                    RETRY_INTERVAL_S if self._state == STATE_ERROR else SEARCH_INTERVAL_S
                )
                continue
            self._transport = transport
            # Captured here, not read back in the teardown: a settings change
            # clears the published receiver (it describes the *next* session),
            # and the session that is ending still has to know it configured
            # something in order to undo it.
            with self._lock:
                session_receiver = dict(self._receiver) if self._receiver else None
            try:
                self._run_session(transport, settings, session_receiver)
            except RtkError as exc:
                self._fail(str(exc))
            except Exception as exc:  # noqa: BLE001 - never end the thread
                logger.exception("RTK session failed")
                self._fail(f"the correction session failed: {exc}")
            finally:
                self._release(transport, settings, session_receiver)
                self._transport = None
            if not self._stop.is_set():
                self._stop.wait(
                    RETRY_INTERVAL_S if self._state == STATE_ERROR else 0.5
                )

    def _acquire(self, settings: dict[str, Any]) -> _Transport | None:
        """Open whichever source the settings name, or return None to retry."""
        if settings.get("source") == "ntrip":
            return self._acquire_ntrip(settings)
        return self._acquire_serial(settings)

    # -- serial ------------------------------------------------------------

    def _acquire_serial(self, settings: dict[str, Any]) -> _Transport | None:
        if _serial is None:
            raise RtkError("pyserial is unavailable, so no base station can be opened")
        pinned = str(settings.get("device") or "").strip()
        candidates = self._candidate_ports()
        if pinned:
            device = pinned
            if self._link_device() == pinned:
                raise RtkError(
                    "that port is the vehicle's telemetry link — pick another, "
                    "or let Corvus find the base station itself"
                )
        else:
            if not candidates:
                self._publish(
                    STATE_SEARCHING,
                    "Looking for a base station — plug an RTK GNSS receiver into this computer",
                )
                return None
            device = candidates[0]["device"]
        self._publish(STATE_CONNECTING, f"Opening {device}")
        bauds = ([int(settings["baud"])] if settings.get("baud")
                 else list(rtk.BAUD_CANDIDATES))
        last_error = ""
        for baud in bauds:
            try:
                port = self._open_port(device, baud)
            except Exception as exc:  # noqa: BLE001 - a serial layer raises broadly
                last_error = str(exc) or exc.__class__.__name__
                continue
            transport = SerialTransport(port, device, baud)
            kind, version = self._probe(transport)
            if kind is not None:
                with self._lock:
                    self._device = device
                    self._baud = baud
                    self._receiver = version
                return transport
            transport.close()
            last_error = "the receiver did not answer"
        if last_error and last_error != "the receiver did not answer":
            raise RtkError(f"could not open {device}: {last_error}")
        # Every rate opened and none of them said anything. Reopen at the first
        # and let the session listen rather than declaring the device dead
        # here: a base that has just been powered can be quiet for several
        # seconds, and the session's own silence timeout is the honest place
        # for that verdict.
        port = self._open_port(device, bauds[0])
        with self._lock:
            self._device = device
            self._baud = bauds[0]
            self._receiver = None
        return SerialTransport(port, device, bauds[0])

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
        try:
            transport.write(rtk.poll_frame(rtk.UBX_CLASS_MON, rtk.UBX_ID_MON_VER))
        except RtkError:
            return None, None
        ubx = rtk.UbxParser()
        framer = rtk.RtcmFramer()
        deadline = time.monotonic() + IDENTIFY_TIMEOUT_S
        while time.monotonic() < deadline and not self._stop.is_set():
            try:
                chunk = transport.read()
            except RtkError:
                return None, None
            if not chunk:
                time.sleep(0.02)
                continue
            for msg_class, msg_id, payload in ubx.feed(chunk):
                if msg_class == rtk.UBX_CLASS_MON and msg_id == rtk.UBX_ID_MON_VER:
                    version = rtk.parse_mon_ver(payload)
                    logger.info(
                        "RTK receiver on %s at %d: %s",
                        transport.device, transport.baud,
                        rtk.receiver_label(version) or "unknown",
                    )
                    return "ubx", version
            if framer.feed(chunk):
                logger.info(
                    "RTK corrections already streaming from %s at %d",
                    transport.device, transport.baud,
                )
                return "rtcm", None
        return None, None

    # -- NTRIP -------------------------------------------------------------

    def _acquire_ntrip(self, settings: dict[str, Any]) -> _Transport:
        """Open a caster stream, or raise with the reason it could not be."""
        ntrip = settings.get("ntrip") or {}
        problem = rtk.ntrip_problem(ntrip)
        if problem:
            raise RtkError(problem)
        host = str(ntrip["host"]).strip()
        port = int(ntrip["port"])
        mountpoint = str(ntrip["mountpoint"]).strip().lstrip("/")
        label = f"{host}:{port}/{mountpoint}"
        self._publish(STATE_CONNECTING, f"Connecting to {label}")
        sock = self._open_socket(host, port, NTRIP_CONNECT_TIMEOUT_S)
        transport = NtripTransport(sock, label)
        try:
            sock.sendall(_ntrip_request(host, port, mountpoint,
                                        str(ntrip.get("username") or ""),
                                        str(ntrip.get("password") or "")))
            header = _read_ntrip_header(sock)
        except RtkError:
            transport.close()
            raise
        except OSError as exc:
            transport.close()
            raise RtkError(f"could not reach {label}: {exc}") from exc
        with self._lock:
            self._device = label
            self._baud = 0
        transport.pending = header
        return transport

    # -- one session -------------------------------------------------------

    def _run_session(self, transport: _Transport, settings: dict[str, Any],
                     version: dict[str, Any] | None) -> None:
        """Configure the source if it can be, then forward until it ends."""
        framer = rtk.RtcmFramer()
        ubx = rtk.UbxParser()
        self._reset_counters()
        with self._lock:
            self._started_at = time.monotonic()

        if transport.pending:
            self._consume(transport.pending, framer, ubx)
            transport.pending = b""

        configurable = (
            transport.configurable
            and version is not None
            and settings.get("source") != "ntrip"
        )
        if configurable:
            self._configure(transport, settings, framer, ubx, version)
        else:
            self._publish(
                STATE_ACTIVE,
                f"Forwarding corrections from {transport.label}",
            )

        deadline_quiet = time.monotonic() + LISTEN_TIMEOUT_S
        while not self._stop.is_set() and not self._restart.is_set():
            chunk = transport.read()
            if chunk:
                self._consume(chunk, framer, ubx)
            else:
                time.sleep(0.02)
            with self._lock:
                last = self._last_frame_at
                frames = self._frames
            now = time.monotonic()
            if not frames and now > deadline_quiet:
                raise RtkError(
                    f"{transport.label} produced no corrections — "
                    "check that the base station has power and a clear view of the sky"
                )
            if frames and last and now - last > SOURCE_SILENCE_S:
                raise RtkError(
                    f"{transport.label} stopped sending corrections"
                )

    def _configure(
        self, transport: _Transport, settings: dict[str, Any],
        framer: rtk.RtcmFramer, ubx: rtk.UbxParser,
        version: dict[str, Any] | None,
    ) -> None:
        """Put a recognised receiver into base mode and wait for it to be ready."""
        modern = rtk.supports_valset(version)
        msm7 = bool(settings.get("msm7"))

        if settings.get("mode") == "fixed":
            fixed = settings.get("fixed") or {}
            problem = rtk.fixed_position_problem(fixed)
            if problem:
                raise RtkError(problem)
            frames = (rtk.fixed_position_frames if modern else rtk.legacy_fixed_position_frames)(
                float(fixed["latitude"]), float(fixed["longitude"]),
                float(fixed["altitude"]), float(fixed["accuracy"]), msm7,
            )
            self._publish(STATE_CONNECTING, "Setting the fixed base position")
            self._write_all(transport, frames)
            self._activate(transport, modern, msm7)
            self._publish(
                STATE_ACTIVE,
                "Correcting from the fixed base position",
            )
            return

        self._publish(STATE_CONNECTING, "Starting the survey")
        stopper = rtk.stop_output_frames if modern else rtk.legacy_stop_output_frames
        self._write_all(transport, stopper(msm7))
        self._await_survey_stopped(transport, ubx, framer, modern)
        frames = (rtk.survey_in_frames if modern else rtk.legacy_survey_in_frames)(
            int(settings["survey_duration"]), float(settings["survey_accuracy"]), msm7,
        )
        # The first frames of each list stop the output and the old survey,
        # which _await_survey_stopped has already done; resending them is
        # harmless and keeps the two paths one call each.
        self._write_all(transport, frames)
        self._publish(STATE_SURVEYING, "Surveying — the base is learning where it is")
        self._await_survey_complete(transport, ubx, framer, settings)
        self._activate(transport, modern, msm7)
        self._publish(STATE_ACTIVE, "The survey is complete and corrections are flowing")

    def _activate(self, transport: _Transport, modern: bool, msm7: bool) -> None:
        frames = (rtk.activate_output_frames(msm7) if modern
                  else rtk.legacy_activate_output_frames(msm7))
        self._write_all(transport, frames)

    def _write_all(self, transport: _Transport, frames: list[bytes],
                   force: bool = False) -> None:
        """Write *frames* in order, pausing between them.

        *force* is for the teardown path only: it is the one write that has to
        happen **because** the service is stopping, so it is the one that must
        not check the stop flag it would always find set.
        """
        for frame in frames:
            if self._stop.is_set() and not force:
                return
            transport.write(frame)
            # A receiver applies these in order and drops a burst written
            # faster than it parses. The pause is the cheapest thing that
            # makes a configuration deterministic.
            time.sleep(0.05)

    def _await_survey_stopped(
        self, transport: _Transport, ubx: rtk.UbxParser,
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
        deadline = time.monotonic() + SURVEY_STOP_TIMEOUT_S
        poll = rtk.poll_frame(rtk.UBX_CLASS_NAV, rtk.UBX_ID_NAV_SVIN)
        seen = False
        while time.monotonic() < deadline and not self._stop.is_set():
            transport.write(poll)
            end = time.monotonic() + 0.3
            while time.monotonic() < end:
                chunk = transport.read()
                if not chunk:
                    time.sleep(0.02)
                    continue
                survey = self._consume(chunk, framer, ubx)
                if survey is None:
                    continue
                seen = True
                if not survey["active"] and not survey["valid"]:
                    return
        if not seen:
            self._warn("the receiver did not report its survey state")
        else:
            self._warn("the previous survey did not stop; its settings may still apply")

    def _await_survey_complete(
        self, transport: _Transport, ubx: rtk.UbxParser,
        framer: rtk.RtcmFramer, settings: dict[str, Any],
    ) -> None:
        """Read NAV-SVIN until the survey is valid and no longer running.

        No timeout, on purpose. A survey under a limit the sky will not allow
        runs for as long as the operator leaves it running, and the honest
        report of that is a progress bar that is not moving — not a session
        that gives up after some number this module invented and leaves the
        base configured but not streaming.
        """
        poll = rtk.poll_frame(rtk.UBX_CLASS_NAV, rtk.UBX_ID_NAV_SVIN)
        next_poll = 0.0
        while not self._stop.is_set() and not self._restart.is_set():
            now = time.monotonic()
            if now >= next_poll:
                transport.write(poll)
                next_poll = now + 1.0
            chunk = transport.read()
            if not chunk:
                time.sleep(0.05)
                continue
            survey = self._consume(chunk, framer, ubx)
            if survey and survey["valid"] and not survey["active"]:
                return

    # -- bytes in ----------------------------------------------------------

    def _consume(
        self, chunk: bytes, framer: rtk.RtcmFramer, ubx: rtk.UbxParser,
    ) -> dict[str, Any] | None:
        """Feed one read to both parsers; inject the corrections it finished.

        Returns the survey report this chunk carried, if it carried one, so the
        two waiting loops above can key on it without a second parse.
        """
        survey: dict[str, Any] | None = None
        for frame in framer.feed(chunk):
            self._note_frame(frame)
            self._mavlink.inject_rtcm(frame)
        for msg_class, msg_id, payload in ubx.feed(chunk):
            if msg_class == rtk.UBX_CLASS_NAV and msg_id == rtk.UBX_ID_NAV_SVIN:
                parsed = rtk.parse_nav_svin(payload)
                if parsed is not None:
                    survey = parsed
                    with self._lock:
                        self._survey = parsed
                    self._publish_progress(parsed)
        with self._lock:
            self._crc_errors = framer.crc_errors
        return survey

    def _note_frame(self, frame: bytes) -> None:
        number = rtk.message_number(frame)
        with self._lock:
            self._frames += 1
            self._frame_bytes += len(frame)
            self._last_frame_at = time.monotonic()
            if number:
                key = str(number)
                self._messages_seen[key] = self._messages_seen.get(key, 0) + 1

    # -- teardown ----------------------------------------------------------

    def _release(self, transport: _Transport, settings: dict[str, Any],
                 version: dict[str, Any] | None) -> None:
        """Close the source, taking a configured receiver out of base mode.

        Runs on the way out of *every* session, including the one that ends
        because the application is shutting down. A receiver left in base mode
        computes no position of its own, and the next thing that opens it —
        u-center, a rover configuration, the operator's other laptop — finds a
        GNSS receiver that reports nothing and no indication why.
        """
        if transport.configurable and version is not None:
            msm7 = bool(settings.get("msm7"))
            frames = (rtk.disable_base_frames(msm7) if rtk.supports_valset(version)
                      else rtk.legacy_disable_base_frames(msm7))
            try:
                self._write_all(transport, frames, force=True)
            except Exception:  # noqa: BLE001 - teardown must not raise
                logger.debug("could not take the receiver out of base mode")
        transport.close()

    # -- state -------------------------------------------------------------

    def _link_device(self) -> str:
        """The serial device the MAVLink bridge is using, or ""."""
        try:
            conn = str(self._mavlink.connection_string() or "")
        except Exception:  # noqa: BLE001
            return ""
        if not conn.startswith("serial:"):
            return ""
        return conn.split(":", 2)[1] if conn.count(":") >= 1 else ""

    def _link_ready(self) -> bool:
        try:
            return bool(self._mavlink.is_connected())
        except Exception:  # noqa: BLE001
            return False

    def _candidate_ports(self) -> list[dict[str, str]]:
        """Serial ports that look like a base station, link port excluded."""
        try:
            rows = self._list_ports()
        except Exception:  # noqa: BLE001 - enumeration must never raise out
            return []
        link = self._link_device()
        out: list[dict[str, str]] = []
        for row in rows or []:
            device = str(row.get("device") or "")
            if not device or device == link or is_phantom_device(device):
                continue
            hwid = str(row.get("hwid") or "")
            description = str(row.get("description") or "")
            if not is_rtk_device(device, hwid, description):
                continue
            out.append({
                "device": device,
                "description": description,
                "hwid": hwid,
            })
        return out

    def _publish(self, state: str, message: str) -> None:
        with self._lock:
            self._state = state
            self._message = message
            if state not in (STATE_ERROR,):
                self._error = ""
            survey = dict(self._survey) if self._survey else None
            settings = rtk.settings(self._settings)
        self._store_update(state, survey, settings)
        logger.debug("RTK %s: %s", state, message)

    def _publish_progress(self, survey: dict[str, Any]) -> None:
        with self._lock:
            state = self._state
            settings = rtk.settings(self._settings)
        self._store_update(state, survey, settings)

    def _store_update(
        self, state: str, survey: dict[str, Any] | None, settings: dict[str, Any],
    ) -> None:
        """Put the four fields the rest of the app reads into the store.

        Only four, and all scalars: this goes out on the telemetry stream with
        every frame, and the detail the page needs belongs in ``status()``,
        which is polled once a second while somebody is looking at it.
        """
        try:
            self._store.update(
                rtk_state=state,
                rtk_source=str(settings.get("source") or ""),
                rtk_accuracy=float((survey or {}).get("accuracy") or 0.0),
                rtk_progress=rtk.survey_progress(
                    survey, settings["survey_duration"], settings["survey_accuracy"],
                ),
            )
        except Exception:  # noqa: BLE001 - the store must never end the thread
            logger.debug("RTK store update failed")

    def _fail(self, reason: str) -> None:
        with self._lock:
            self._state = STATE_ERROR
            self._error = reason
            self._message = reason
            survey = dict(self._survey) if self._survey else None
            settings = rtk.settings(self._settings)
        self._store_update(STATE_ERROR, survey, settings)
        logger.info("RTK: %s", reason)

    def _warn(self, text: str) -> None:
        with self._lock:
            self._warning = text
        logger.info("RTK: %s", text)

    def _reset_counters(self) -> None:
        with self._lock:
            self._frames = 0
            self._frame_bytes = 0
            self._crc_errors = 0
            self._last_frame_at = 0.0
            self._messages_seen = {}
        try:
            self._mavlink.forget_rtcm_session()
        except Exception:  # noqa: BLE001
            pass

    def _describe_intent(self) -> str:
        settings = self.settings()
        if not settings.get("enabled"):
            return "disabled"
        if settings.get("source") == "ntrip":
            return "NTRIP"
        return "USB base station, " + str(settings.get("mode"))

    # -- defaults for the injectables --------------------------------------

    @staticmethod
    def _default_open_port(device: str, baud: int) -> Any:
        if _serial is None:  # pragma: no cover - guarded by the caller
            raise RtkError("pyserial is unavailable")
        return _serial.Serial(
            port=device, baudrate=baud, timeout=READ_TIMEOUT_S, write_timeout=2.0,
        )

    @staticmethod
    def _default_list_ports() -> list[dict[str, str]]:
        from .mavlink_bridge import MavlinkBridge
        return MavlinkBridge.list_serial_ports()

    @staticmethod
    def _default_open_socket(host: str, port: int, timeout: float) -> Any:
        sock = socket.create_connection((host, port), timeout=timeout)
        sock.settimeout(READ_TIMEOUT_S)
        return sock


def _ntrip_request(
    host: str, port: int, mountpoint: str, username: str, password: str,
) -> bytes:
    """The GET a caster expects, as NTRIP 2.0 with a 1.0 fallback built in.

    Version 2 is asked for by header and version-1 casters ignore the header
    and answer ``ICY 200 OK`` anyway, so one request serves both — which is
    why there is no version setting on the page.
    """
    from .version import get_version
    lines = [
        f"GET /{mountpoint} HTTP/1.1",
        f"Host: {host}:{port}",
        "Ntrip-Version: Ntrip/2.0",
        f"User-Agent: NTRIP CorvusGCS/{get_version()}",
        "Accept: */*",
        "Connection: close",
    ]
    if username or password:
        token = base64.b64encode(f"{username}:{password}".encode()).decode("ascii")
        lines.append(f"Authorization: Basic {token}")
    return ("\r\n".join(lines) + "\r\n\r\n").encode("ascii", errors="replace")


def _read_ntrip_header(sock: Any) -> bytes:
    """Consume the caster's response header; return the bytes after it.

    A caster answers and then streams in the same socket with no framing
    between the two, so the first correction is routinely in the same packet
    as the last header line. Those bytes are returned rather than dropped —
    dropping them costs one correction on every connect, which is a fix that
    takes a second longer to come back for no reason.
    """
    buf = b""
    deadline = time.monotonic() + NTRIP_CONNECT_TIMEOUT_S
    while time.monotonic() < deadline:
        try:
            chunk = sock.recv(NTRIP_READ_BYTES)
        except TimeoutError:
            continue
        except OSError as exc:
            raise RtkError(f"the caster closed the connection: {exc}") from exc
        if not chunk:
            raise RtkError("the caster closed the connection without answering")
        buf += chunk
        split = buf.find(b"\r\n\r\n")
        if split < 0:
            if len(buf) > 8192:
                raise RtkError("the caster sent no usable response")
            continue
        head = buf[:split].decode("latin-1", errors="replace")
        rest = buf[split + 4:]
        first = head.split("\r\n", 1)[0]
        if "200" in first or first.upper().startswith("ICY 200"):
            return rest
        if "401" in first:
            raise RtkError("the caster rejected the username or password")
        if "404" in first:
            raise RtkError("the caster has no such mountpoint")
        raise RtkError(f"the caster refused the stream: {first.strip() or 'no response'}")
    raise RtkError("the caster did not answer in time")
