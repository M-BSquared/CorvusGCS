"""SiK telemetry-radio AT sessions: the half of the radio page that owns a port.

:mod:`corvus.sik_config` knows what the settings mean. This module knows how to
get at them: open the serial device, escape the radio into command mode, hold a
short conversation in the AT dialect, and put the port back the way it was
found.

The port is the whole problem
-----------------------------
A radio in AT command mode is a radio that has stopped relaying. For the radio
that carries the live link, a configuration session therefore *is* a telemetry
outage, and the MAVLink bridge cannot be left holding the device while it
happens — two readers on one ``/dev/tty*`` do not error, they each get an
arbitrary half of the bytes, which is the failure mode
``MavlinkBridge._claim_serial_exclusive`` exists to prevent. So a session on the
live link's device stops the bridge for its duration and restarts it in
``finally``, the same shape :class:`corvus.flash_service.FlashService` uses for
the bootloader.

A session on any *other* port does not touch the bridge at all. That is the case
Mission Planner cannot express — it configures whatever COM port is selected and
requires you to be disconnected regardless — and it is worth supporting, because
configuring the spare radio on the bench while the vehicle is connected is a
normal thing to want.

Both cases are refused while armed. The rule is one sentence rather than two
because it is the one an operator can hold: radios are not configured while the
aircraft is flying.

Escaping into command mode
--------------------------
``+++`` with a second of silence either side. The silence is what makes the
sequence unambiguous — a MAVLink stream that happened to contain three plus
signs would otherwise drop the radio out of the air mid-flight — and it is why
:data:`GUARD_S` is generous rather than exactly the firmware's 1000 ms: the
guard is measured by the radio, against its own clock, and a Windows host that
sleeps 1.000 s sometimes delivers 0.98.

The whole session is synchronous, on the HTTP worker thread, under one lock. It
is short (a local read is a few hundred milliseconds; a read that includes the
far radio is a second or two more) and there is nothing useful to show while it
runs, so it does not carry the worker-thread-plus-SSE machinery the firmware
flash needs for its minutes-long upload.
"""

from __future__ import annotations

import logging
import threading
import time
from typing import Any, TYPE_CHECKING
from collections.abc import Callable

from . import sik_config
from .mavlink_bridge import classify_serial_device, is_windows_com_port

if TYPE_CHECKING:  # avoid an import cycle at runtime
    from .mavlink_bridge import MavlinkBridge
    from .state_store import VehicleStateStore

logger = logging.getLogger("corvus.sik")

try:  # pragma: no cover - import-time guard, mirrors firmware_uploader
    import serial as _serial  # type: ignore[import-not-found]
except ImportError:  # pragma: no cover
    _serial = None  # type: ignore[assignment]

# Silence the radio requires either side of ``+++`` before it accepts the
# sequence as an escape rather than as data. The firmware default is 1000 ms;
# the extra 250 ms is slack for a host whose sleep is not exact.
GUARD_S = 1.25

# How long a single AT command may stay quiet before its reply is considered
# complete. A local command answers in tens of milliseconds; the margin is for
# the remote ones, which cross the air twice.
IDLE_S = 0.35

# Hard ceiling on one command's reply, so a radio that streams garbage can never
# hold the HTTP thread. Remote commands get the longer one: a ``RTI5`` has to
# reach the far radio, be answered, and come back, and it does that at the air
# data rate rather than the serial one.
LOCAL_TIMEOUT_S = 1.5
REMOTE_TIMEOUT_S = 6.0

# Time the radio is given to come back up after ``ATZ``. A reboot is a few
# hundred milliseconds; the rest is for the link to re-establish before anything
# reads from the port again.
REBOOT_S = 2.0

# Gate messages, kept as constants so the HTTP layer and the tests agree on the
# exact wording the operator sees.
ARMED_MESSAGE = (
    "Radio configuration is refused while the vehicle is armed — a radio in "
    "command mode is not relaying telemetry."
)
BUSY_MESSAGE = "A radio configuration session is already running."
NO_SERIAL_MESSAGE = "pyserial is unavailable, so no serial port can be opened."


class SikError(RuntimeError):
    """A session could not be completed, with the reason an operator can act on."""


class SikSession:
    """One open serial port, held in AT command mode.

    Not reusable and not thread-safe: :class:`SikService` builds one per
    request, inside its lock, and closes it in ``finally``.
    """

    def __init__(self, port: Any) -> None:
        self._port = port

    # -- raw IO ---------------------------------------------------------

    def _read_reply(self, timeout: float) -> str:
        """Read until the radio goes quiet, or until *timeout*.

        There is no framing to key on. An AT reply ends with ``OK``, an ``ATI5``
        report ends with the last register line, and a radio that did not
        understand says ``ERROR`` — none of which can be waited for reliably,
        because a partner radio's traffic may still be arriving between them. So
        the end of a reply is defined the only way it can be: by silence.
        """
        deadline = time.monotonic() + timeout
        chunks: list[bytes] = []
        last = time.monotonic()
        while time.monotonic() < deadline:
            waiting = 0
            try:
                waiting = int(getattr(self._port, "in_waiting", 0) or 0)
            except Exception:  # noqa: BLE001 - a closing port reports nothing
                waiting = 0
            data = b""
            if waiting:
                try:
                    data = self._port.read(waiting) or b""
                except Exception as exc:  # noqa: BLE001
                    raise SikError(f"serial read failed: {exc}") from exc
            if data:
                chunks.append(data)
                last = time.monotonic()
            elif chunks and (time.monotonic() - last) >= IDLE_S:
                break
            else:
                time.sleep(0.02)
        return b"".join(chunks).decode("ascii", errors="replace")

    def command(self, text: str, timeout: float | None = None) -> str:
        """Send one AT/RT command and return whatever came back.

        The command's own echo is left in the reply rather than stripped: the
        parsers in :mod:`corvus.sik_config` already ignore anything that is not
        the shape they are looking for, and an echo that *is* present is useful
        evidence in a log when a radio is not answering.
        """
        remote = text.upper().startswith("RT")
        if timeout is None:
            timeout = REMOTE_TIMEOUT_S if remote else LOCAL_TIMEOUT_S
        try:
            self._port.reset_input_buffer()
            self._port.write((text + "\r\n").encode("ascii"))
            self._port.flush()
        except Exception as exc:  # noqa: BLE001
            raise SikError(f"serial write failed: {exc}") from exc
        return self._read_reply(timeout)

    # -- the dialogue ---------------------------------------------------

    def enter_command_mode(self, attempts: int = 2) -> bool:
        """``+++`` with its guard times. True when the radio answered.

        Each attempt costs two guard intervals, so *attempts* is the knob that
        keeps a baud scan bounded: the baud the caller actually asked for is
        worth a retry, the six or seven being guessed after it are not.

        The retry is not superstition. One failure mode is a radio that was
        *already* in command mode — a previous session that died before ``ATO``
        — which treats ``+++`` as three characters of a command it does not
        recognise and answers ``ERROR``. Probing with ``ATI`` catches that case
        without a second guard interval, so the retry only runs when the radio
        really is still passing data.
        """
        for attempt in range(1, max(1, int(attempts)) + 1):
            try:
                self._port.reset_input_buffer()
            except Exception:  # noqa: BLE001
                pass
            time.sleep(GUARD_S)
            try:
                self._port.write(b"+++")
                self._port.flush()
            except Exception as exc:  # noqa: BLE001
                raise SikError(f"serial write failed: {exc}") from exc
            time.sleep(GUARD_S)
            reply = self._read_reply(LOCAL_TIMEOUT_S)
            if "OK" in reply.upper():
                return True
            # Already in command mode? A banner comes back for ATI either way.
            if sik_config.parse_version(self.command("ATI")):
                return True
            logger.debug("sik: +++ attempt %d got %r", attempt, reply)
        return False

    def leave_command_mode(self) -> None:
        """``ATO`` — hand the port back to the telemetry stream.

        Best-effort: a radio that was rebooted by the session is not in command
        mode any more and will ignore this, and a radio that does not get it
        leaves command mode on its own at the next power cycle. Failing a
        completed configuration over the exit is not worth it.
        """
        try:
            self.command("ATO", timeout=0.4)
        except Exception:  # noqa: BLE001
            logger.debug("sik: ATO failed", exc_info=True)

    def read_radio(self, remote: bool) -> dict[str, Any] | None:
        """Read one end of the link: version, board and every register.

        ``None`` when the radio did not answer — which for the remote is the
        ordinary case of a partner that is switched off, out of range, or
        already misconfigured, and is reported as such rather than as an error.
        """
        prefix = "RT" if remote else "AT"
        registers = sik_config.parse_registers(self.command(f"{prefix}I5"))
        if not registers:
            return None
        version = sik_config.parse_version(self.command(f"{prefix}I"))
        board = sik_config.parse_version(self.command(f"{prefix}I2"))
        return sik_config.describe_radio(registers, version=version, board=board)

    def read_link_quality(self) -> dict[str, Any] | None:
        """``ATI7`` — signal and noise at both ends, or ``None`` if unreported."""
        try:
            return sik_config.parse_rssi(self.command("ATI7"))
        except SikError:
            return None

    def apply(self, writes: list[dict[str, Any]], remote: bool) -> None:
        """Run a write plan, checking each command before sending the next.

        A register the radio refuses aborts the run *before* ``AT&W``, so the
        EEPROM still holds the configuration the operator started with and a
        reboot returns to it. The alternative — pressing on and committing the
        subset that happened to take — produces a radio in a state nobody chose
        and, if an air setting was among them, one that can no longer be reached
        over the air to fix.
        """
        for command in sik_config.write_plan(writes, remote):
            reply = self.command(command)
            if command.upper().endswith("Z"):
                # ATZ/RTZ reboots; there is no reply worth checking and the
                # radio is briefly absent from the port.
                time.sleep(REBOOT_S)
                continue
            if "ERROR" in reply.upper():
                raise SikError(f"the radio rejected {command}")

    def factory_reset(self, remote: bool) -> None:
        """``AT&F`` then write and reboot — back to the firmware's defaults.

        Deliberately does not touch the remote *and* the local in one call. A
        reset of both ends at once is two radios simultaneously reverting to
        defaults, which does bring them back into agreement, but it does so by
        way of a window in which neither is reachable from the other and the
        remote one is on an aircraft.
        """
        prefix = "RT" if remote else "AT"
        reply = self.command(f"{prefix}&F")
        if "ERROR" in reply.upper():
            raise SikError("the radio rejected the factory reset")
        self.command(f"{prefix}&W")
        self.command(f"{prefix}Z")
        time.sleep(REBOOT_S)


class SikService:
    """Gate, port ownership and bridge orchestration for SiK radio sessions.

    One session at a time, enforced by ``_lock``: the lock is held for the whole
    request rather than around each command, because the thing being serialised
    is possession of a serial port and of the MAVLink bridge's running state,
    not the individual reads.
    """

    def __init__(self, mavlink: MavlinkBridge | None,
                 store: VehicleStateStore | None = None) -> None:
        self.mavlink = mavlink
        self._store = store
        self._lock = threading.Lock()
        self._busy = False
        self._shutting_down = False

    # ------------------------------------------------------------------
    # Status and gate
    # ------------------------------------------------------------------

    def _armed(self) -> bool:
        if self._store is None:
            return False
        try:
            return bool(self._store.get_snapshot().get("armed"))
        except Exception:  # noqa: BLE001 - a status call must never raise
            return False

    def status(self) -> dict[str, Any]:
        """What the page needs before any radio has been touched.

        The port list is every serial port that could plausibly be a radio, each
        flagged with whether it is the one the live link runs on. It is not
        filtered down to ports classified ``sik``: that classification reads a
        USB descriptor, and a radio behind an adapter chip Corvus does not
        recognise would vanish from a list that trusted it. A port that is not a
        radio simply fails to answer ``+++``, which costs one guard interval and
        says so clearly.
        """
        mavlink = self.mavlink
        link_device = ""
        link_baud = sik_config.DEFAULT_BAUD
        transport = "unknown"
        if mavlink is not None:
            try:
                transport = mavlink.transport()
                link_device = mavlink.serial_device()
                if link_device:
                    _, link_baud = mavlink._parse_serial(mavlink.connection_string())
            except Exception:  # noqa: BLE001
                logger.debug("sik: reading link details failed", exc_info=True)

        ports: list[dict[str, Any]] = []
        if mavlink is not None:
            try:
                for row in mavlink.list_serial_ports():
                    device = row.get("device", "")
                    ports.append({
                        "device": device,
                        "description": row.get("description", ""),
                        "kind": classify_serial_device(
                            device, row.get("hwid", ""), row.get("description", "")),
                        "is_link": _same_device(device, link_device),
                    })
            except Exception:  # noqa: BLE001 - the page must still render
                logger.debug("sik: port enumeration failed", exc_info=True)

        armed = self._armed()
        with self._lock:
            busy = self._busy
        blocked = ""
        if _serial is None:
            blocked = NO_SERIAL_MESSAGE
        elif armed:
            blocked = ARMED_MESSAGE
        elif busy:
            blocked = BUSY_MESSAGE
        return {
            "ports": ports,
            "link_device": link_device,
            "link_baud": link_baud,
            "transport": transport,
            "armed": armed,
            "busy": busy,
            "can_configure": not blocked,
            "blocked_reason": blocked,
            "default_baud": sik_config.DEFAULT_BAUD,
            "bauds": list(sik_config.BAUD_CANDIDATES),
        }

    def _gate(self) -> str | None:
        if _serial is None:
            return NO_SERIAL_MESSAGE
        if self._armed():
            return ARMED_MESSAGE
        return None

    # ------------------------------------------------------------------
    # Session plumbing
    # ------------------------------------------------------------------

    def _run(self, device: str, baud: int | None,
             body: Callable[[SikSession, int], dict[str, Any]]) -> dict[str, Any]:
        """Take the port, hold an AT session, and give the port back.

        *body* receives the live session and the baud it was reached at. The
        bridge is stopped only when *device* is the link's own device, and is
        restarted in ``finally`` whatever happened inside — including a refusal,
        which must not cost the operator their telemetry link.
        """
        blocked = self._gate()
        if blocked:
            raise SikError(blocked)
        device = str(device or "").strip()
        if not device:
            raise SikError("no serial port was selected")

        with self._lock:
            if self._busy:
                raise SikError(BUSY_MESSAGE)
            self._busy = True
        try:
            return self._run_locked(device, baud, body)
        finally:
            with self._lock:
                self._busy = False

    def _run_locked(self, device: str, baud: int | None,
                    body: Callable[[SikSession, int], dict[str, Any]]) -> dict[str, Any]:
        mavlink = self.mavlink
        owns_link = (
            mavlink is not None
            and _same_device(device, mavlink.serial_device())
            and mavlink.is_running()
        )
        restore_conn = mavlink.connection_string() if owns_link else ""
        if owns_link:
            logger.info("sik: stopping the MAVLink bridge to configure %s", device)
            try:
                mavlink.stop()
            except Exception:  # noqa: BLE001 - the bridge may already be down
                logger.debug("sik: bridge stop raised", exc_info=True)

        session: SikSession | None = None
        port: Any = None
        try:
            port, session, reached_baud = self._open(device, baud)
            result = body(session, reached_baud)
            result.setdefault("device", device)
            result.setdefault("baud", reached_baud)
            return result
        finally:
            if session is not None:
                session.leave_command_mode()
            if port is not None:
                try:
                    port.close()
                except Exception:  # noqa: BLE001
                    logger.debug("sik: closing %s raised", device, exc_info=True)
            self._restart_bridge(restore_conn)

    def _open(self, device: str, baud: int | None) -> tuple[Any, SikSession, int]:
        """Open *device* and escape into command mode, finding the baud if needed.

        A wrong baud opens exactly as cleanly as a right one — the port is a
        port either way — so the only test that distinguishes them is whether
        the radio answers ``+++``. Which makes opening and entering command mode
        one operation rather than two, and makes the baud scan the loop around
        it.

        The scan matters because the operator who most needs this page is often
        the one whose radio is no longer at the baud they remember setting: a
        SERIAL_SPEED written and then forgotten locks them out of the very
        register that locked them out. It is bounded but not cheap — every baud
        that does not answer costs a full guard interval — so the caller's own
        baud gets the retry and the guesses after it get one attempt each.
        """
        if _serial is None:  # pragma: no cover - guarded by _gate
            raise SikError(NO_SERIAL_MESSAGE)
        candidates: list[int] = []
        for candidate in [baud] + list(sik_config.BAUD_CANDIDATES):
            if candidate and int(candidate) not in candidates:
                candidates.append(int(candidate))

        last_error = ""
        for index, candidate in enumerate(candidates):
            try:
                port = _serial.Serial(device, baudrate=candidate, timeout=0.1,
                                      write_timeout=2.0)
            except Exception as exc:  # noqa: BLE001 - pyserial raises broadly
                last_error = str(exc)
                continue
            session = SikSession(port)
            if session.enter_command_mode(attempts=2 if index == 0 else 1):
                return port, session, candidate
            try:
                port.close()
            except Exception:  # noqa: BLE001
                pass
            last_error = f"no answer at {candidate} baud"

        raise SikError(
            f"could not reach a radio on {device} ({last_error}). Check that this "
            "is the radio's port, that nothing else has it open, and that the "
            "radio has power."
            if last_error else f"could not reach a radio on {device}"
        )

    def _restart_bridge(self, conn: str) -> None:
        """Put the MAVLink bridge back on the port this session borrowed."""
        if not conn or self._shutting_down or self.mavlink is None:
            return
        try:
            self.mavlink.set_connection(conn)
            self.mavlink.start()
            logger.info("sik: MAVLink bridge restarted on %s", conn)
        except Exception:  # noqa: BLE001 - best-effort, mirrors FlashService
            logger.exception("sik: restarting the MAVLink bridge failed")

    # ------------------------------------------------------------------
    # Operations
    # ------------------------------------------------------------------

    def load(self, device: str, baud: int | None = None,
             include_remote: bool = True) -> dict[str, Any]:
        """Read both ends of the link and report where they disagree."""
        def body(session: SikSession, reached: int) -> dict[str, Any]:
            local = session.read_radio(remote=False)
            if local is None:
                raise SikError(
                    "the radio answered the escape but reported no settings — "
                    "its firmware may not support ATI5."
                )
            remote = session.read_radio(remote=True) if include_remote else None
            return {
                "local": local,
                "remote": remote,
                "remote_reachable": remote is not None,
                "mismatches": sik_config.compare(local, remote),
                "link": session.read_link_quality(),
            }

        return self._run(device, baud, body)

    def save(self, device: str, baud: int | None = None,
             local: dict[str, Any] | None = None,
             remote: dict[str, Any] | None = None) -> dict[str, Any]:
        """Write settings to the far radio first, then the near one.

        The order is the important part and it is not a preference. The local
        radio is the one the session is holding a cable to; the remote is
        reachable only *through* the local one, and only while the two still
        agree on the air settings. Write the local end first and a changed
        NETID, air rate or band severs the path to the remote before it has been
        told — leaving an aircraft whose radio can now only be reached by
        physically retrieving it and plugging a cable in.

        Writing the far end first has the opposite failure, and it is the benign
        one: if the local write then fails, the pair is mismatched but both
        radios are on the bench-accessible side of a cable.
        """
        def body(session: SikSession, reached: int) -> dict[str, Any]:
            applied: dict[str, list[dict[str, Any]]] = {"remote": [], "local": []}
            warnings: list[str] = []

            if remote:
                current = session.read_radio(remote=True)
                if current is None:
                    raise SikError(
                        "the remote radio is not answering, so its settings cannot "
                        "be written. Power the aircraft and wait for a solid green "
                        "LED on both radios."
                    )
                writes = sik_config.validate_writes(remote, _registers_of(current))
                session.apply(writes, remote=True)
                applied["remote"] = writes

            new_serial_speed = 0
            if local:
                current = session.read_radio(remote=False)
                if current is None:
                    raise SikError("the local radio stopped reporting its settings")
                writes = sik_config.validate_writes(local, _registers_of(current))
                session.apply(writes, remote=False)
                applied["local"] = writes
                for write in writes:
                    if write["name"] == "SERIAL_SPEED":
                        new_serial_speed = sik_config.baud_from_register(write["value"])

            if new_serial_speed:
                warnings.append(
                    f"This radio now talks to its cable at {new_serial_speed} baud. "
                    "Corvus has moved the link to match; the radio at the other end "
                    "keeps its own baud, which must still match the autopilot's "
                    "telemetry port."
                )
            return {
                "applied": applied,
                "warnings": warnings,
                "new_serial_speed": new_serial_speed,
            }

        result = self._run(device, baud, body)
        # Reconnect at the new baud when the operator just changed the speed of
        # the wire this link runs over. Without this the bridge restarts on the
        # old baud and the link simply never comes back, with nothing on screen
        # explaining why.
        speed = int(result.get("new_serial_speed") or 0)
        if speed:
            self._retune_link(str(result.get("device") or device), speed)
        return result

    def reset(self, device: str, baud: int | None = None,
              target: str = "local") -> dict[str, Any]:
        """Factory-default one end of the link (``AT&F`` / ``RT&F``)."""
        if target not in {"local", "remote"}:
            raise SikError("target must be 'local' or 'remote'")

        def body(session: SikSession, reached: int) -> dict[str, Any]:
            if target == "remote" and session.read_radio(remote=True) is None:
                raise SikError("the remote radio is not answering")
            session.factory_reset(remote=(target == "remote"))
            return {"target": target}

        return self._run(device, baud, body)

    def _retune_link(self, device: str, baud: int) -> None:
        """Move the live link to a new baud after the local radio changed speed."""
        mavlink = self.mavlink
        if mavlink is None or not _same_device(device, mavlink.serial_device()):
            return
        try:
            mavlink.stop()
        except Exception:  # noqa: BLE001
            logger.debug("sik: bridge stop before retune raised", exc_info=True)
        self._restart_bridge(f"serial:{device}:{baud}")

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def shutdown(self) -> None:
        """Stop restarting the bridge; a session in flight finishes its close.

        Sessions are synchronous and short, so there is no worker to join. The
        flag exists for the same reason :class:`FlashService` has one: a
        tear-down must not respawn the bridge it is racing to stop.
        """
        self._shutting_down = True


def _registers_of(described: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Rebuild the ``{name: {number, value}}`` map from a described radio."""
    return {
        field["name"]: {"number": field["register"], "value": field["value"]}
        for field in described.get("fields", [])
    }


def _same_device(left: str, right: str) -> bool:
    """Do two port names refer to the same device?

    Case-insensitive for Windows COM names, which are written ``COM7`` and
    ``com7`` with equal frequency and are the same port either way. Exact
    elsewhere, because a POSIX device path is case-sensitive and
    ``/dev/ttyUSB1`` is emphatically not ``/dev/ttyusb1``.
    """
    left, right = str(left or "").strip(), str(right or "").strip()
    if not left or not right:
        return False
    if is_windows_com_port(left) or is_windows_com_port(right):
        return left.lower() == right.lower()
    return left == right
