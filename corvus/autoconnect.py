"""Smart-default MAVLink auto-connect: pick a link, and keep watching for one.

A ground station that needs a click before it will talk to the aircraft plugged
into it is a ground station that gets opened in the field and then debugged in
the field. QGroundControl has answered this for years with per-transport
AutoConnect toggles; this is the small version of that idea — one bridge, one
active connection string, a fixed priority, and no link manager.

The priority is USB first, then a SiK radio, then the UDP port a simulator or a
router publishes on:

    command-line argument
      > direct USB flight controller
      > SiK telemetry radio
      > the configured connection string
      > the UDP fallback (SITL)
      > the built-in default

Two rules shape everything else here. **Never steal an active link**: the
watcher acts only while the bridge is disconnected or reconnecting with no
fresh heartbeat, and a device plugged in mid-session is published as a
suggestion for the operator rather than dialled. **Manual always wins the
session**: a successful ``POST /api/mavlink/connect`` sets ``manual_override``
and nothing auto-dials again until the process restarts. Both exist because the
one thing worse than a ground station that will not connect is one that
disconnects itself from a flying aircraft.

Everything that decides is a pure function taking an already-enumerated port
list, so the policy is testable without a socket, a thread or a serial device.
The watcher is the only moving part: one daemon thread, one ``Event``-driven
sleep, no sockets of its own, and no serial port is ever opened to find out
what is behind it — enumeration is the read-only
:meth:`~corvus.mavlink_bridge.MavlinkBridge.list_serial_ports` glob.
"""
from __future__ import annotations

import logging
import threading
import time
from dataclasses import dataclass, field
from typing import Any
from collections.abc import Callable, Iterable, Sequence

from .mavlink_bridge import (
    MavlinkBridge,
    classify_serial_device,
    is_bootloader_port,
    is_phantom_device,
)

logger = logging.getLogger(__name__)

# The reason strings are a contract, not prose: they are logged, published in
# the store as ``link_auto.reason`` and asserted in the test suite. Renaming one
# is an API change.
REASON_CLI = "cli"
REASON_USB = "usb-direct"
REASON_SIK = "sik"
REASON_CONFIGURED = "configured"
REASON_UDP_FALLBACK = "udp-fallback"
REASON_DEFAULT_FALLBACK = "default-invalid-fallback"
REASON_MANUAL = "manual"
REASON_NO_CANDIDATE = "no-candidate"
REASON_BOOTLOADER_SKIP = "bootloader-skip"
REASON_NEW_DEVICE = "new-device-while-connected"

REASONS: frozenset[str] = frozenset({
    REASON_CLI, REASON_USB, REASON_SIK, REASON_CONFIGURED, REASON_UDP_FALLBACK,
    REASON_DEFAULT_FALLBACK, REASON_MANUAL, REASON_NO_CANDIDATE,
    REASON_BOOTLOADER_SKIP, REASON_NEW_DEVICE,
})

# The one literal this module owns. 0.0.0.0 rather than 127.0.0.1 because it is
# the superset bind: it accepts a simulator on loopback AND one on a bridged
# network interface, while the loopback-only form silently drops the second and
# looks exactly like a simulator that is not running. An explicit 127.0.0.1 from
# the command line or the config file is still honoured verbatim — this is only
# what is reached for when nothing else said anything.
UDP_FALLBACK_CONNECTION = "udp:0.0.0.0:14550"

# SiK Telemetry Radio V3 factory default, the same one _parse_serial falls back
# to. Auto-connect never probes for a baud rate: opening a port to find out is
# exactly the behaviour this module promises not to have.
DEFAULT_BAUD = 57600

# How long without a vehicle heartbeat before the watcher considers the link
# genuinely gone. Derived from the A3 drop timeouts (UDP 8 s, serial 15 s): at
# the UDP boundary the bridge has already given up and is reconnecting, while a
# serial link is still inside its own warn band and is left alone.
STALE_AFTER_S = 8.0

# Fast enough that a radio plugged into a disconnected station is picked up
# inside one 10 s heartbeat wait, slow enough that the cost — one non-opening
# port enumeration — is noise beside the bridge's own 1 Hz heartbeat.
DEFAULT_POLL_S = 2.5

# Link states in which there is nothing to steal.
IDLE_STATES: frozenset[str] = frozenset({"disconnected", "reconnecting"})


@dataclass(frozen=True)
class PortInfo:
    """One row of :meth:`MavlinkBridge.list_serial_ports`, classified.

    ``kind`` is ``usb`` / ``sik`` / ``unknown`` as
    :func:`corvus.mavlink_bridge.classify_serial_device` reads it — the same
    classifier the firmware flasher trusts, so the two can never disagree about
    what is a flight controller.
    """

    device: str
    description: str = ""
    hwid: str = ""
    kind: str = "unknown"

    @classmethod
    def from_row(cls, row: dict) -> PortInfo:
        """Build from a ``list_serial_ports()`` dict, classifying as it goes."""
        device = str(row.get("device", "") or "")
        description = str(row.get("description", "") or "")
        hwid = str(row.get("hwid", "") or "")
        return cls(
            device=device,
            description=description,
            hwid=hwid,
            kind=classify_serial_device(device, hwid, description),
        )


@dataclass(frozen=True)
class StartupDecision:
    """What the resolver chose at startup, and why."""

    connection_string: str
    reason: str
    detail: str = ""


@dataclass(frozen=True)
class Suggestion:
    """A device that appeared while the link was busy elsewhere.

    Display only. The backend never dials a suggestion; the operator does, and
    the frontend's button is the ordinary ``POST /api/mavlink/connect`` path.
    """

    connection_string: str
    kind: str
    device: str
    reason: str = REASON_NEW_DEVICE

    def as_dict(self) -> dict[str, Any]:
        """The shape published in the store as ``link_suggestion``."""
        return {
            "connection_string": self.connection_string,
            "kind": self.kind,
            "device": self.device,
            "reason": self.reason,
            "ts": time.time(),
        }


@dataclass
class SessionState:
    """Auto-connect state that lives for one process, and is never persisted.

    ``manual_override`` is the operator's "I chose this link". It must not
    survive a restart: next time the laptop is opened at the field, a USB cable
    should win again on its own. It is equally never written to the config
    file, which is shared with every future session.
    """

    enabled: bool = True
    usb: bool = True
    sik: bool = True
    udp_fallback: bool = True
    manual_override: bool = False
    last_suggestion_key: str | None = None
    last_dial_key: str | None = None
    reason: str = ""
    winner: str = ""
    _lock: threading.Lock = field(default_factory=threading.Lock, repr=False)

    def apply_config(self, block: dict | None) -> None:
        """Refresh the toggles from a coerced ``autoconnect`` config block."""
        values = block or {}
        with self._lock:
            self.enabled = bool(values.get("enabled", True))
            self.usb = bool(values.get("usb", True))
            self.sik = bool(values.get("sik", True))
            self.udp_fallback = bool(values.get("udp_fallback", True))

    def note_manual_connect(self) -> None:
        """A manual connect took the session: stop dialling, stop suggesting."""
        with self._lock:
            self.manual_override = True
            self.last_suggestion_key = None
            self.reason = REASON_MANUAL

    def note_decision(self, reason: str, winner: str) -> None:
        """Record the last automatic decision for ``link_auto``."""
        with self._lock:
            self.reason = reason
            self.winner = winner

    def as_dict(self) -> dict[str, Any]:
        """The shape published in the store as ``link_auto``."""
        with self._lock:
            return {
                "enabled": self.enabled,
                "usb": self.usb,
                "sik": self.sik,
                "udp_fallback": self.udp_fallback,
                "manual_override": self.manual_override,
                "reason": self.reason,
                "winner": self.winner or None,
            }


def classify_ports(rows: Iterable[dict]) -> list[PortInfo]:
    """Turn ``list_serial_ports()`` rows into classified :class:`PortInfo`.

    Phantom and pseudo-terminal nodes are dropped here rather than in every
    caller: they are serial ports to the kernel and never a vehicle, and an
    automatic path has no operator to notice that it picked one.
    """
    out: list[PortInfo] = []
    for row in rows or ():
        try:
            port = PortInfo.from_row(row)
        except Exception:  # noqa: BLE001 - a malformed row costs that row only
            continue
        if not port.device or is_phantom_device(port.device):
            continue
        out.append(port)
    return out


def _usable(port: PortInfo) -> bool:
    """Is this a port auto-connect may dial?"""
    return not is_bootloader_port(port.device, port.hwid, port.description)


def _pick(ports: Sequence[PortInfo], kind: str) -> PortInfo | None:
    """First port of *kind*, by device name, skipping bootloaders.

    Sorted rather than first-enumerated: with two flight controllers on one
    laptop the choice has to be the same on every launch, or the operator is
    connected to a different aircraft depending on which USB port enumerated
    faster.
    """
    matches = sorted(
        (p for p in ports if p.kind == kind and _usable(p)),
        key=lambda p: p.device,
    )
    return matches[0] if matches else None


def serial_connection_string(device: str, baud: int = DEFAULT_BAUD) -> str:
    """``serial:<device>:<baud>`` — the same spelling the LINK tab builds."""
    return f"serial:{device}:{int(baud)}"


def pick_usb_serial(
    ports: Sequence[PortInfo],
    *,
    usb_enabled: bool = True,
    sik_enabled: bool = True,
) -> tuple[str, int, str] | None:
    """Return ``(device, baud, kind)`` for the best serial candidate, or None.

    USB before SiK, because a direct cable to the flight controller is the link
    the operator is standing next to; a radio may be pointing at a different
    aircraft entirely. Never opens a port: the classification comes from the
    device name or the USB descriptor that enumeration already reported.
    """
    if usb_enabled:
        hit = _pick(ports, "usb")
        if hit is not None:
            return hit.device, DEFAULT_BAUD, "usb"
    if sik_enabled:
        hit = _pick(ports, "sik")
        if hit is not None:
            return hit.device, DEFAULT_BAUD, "sik"
    return None


def _is_valid(conn: Any, validator: Callable[[str], None] | None) -> bool:
    """Would the bridge accept *conn*? Unvalidatable values are not usable."""
    if not isinstance(conn, str) or not conn.strip():
        return False
    if validator is None:
        return True
    try:
        validator(conn)
    except Exception:  # noqa: BLE001 - any refusal means "not usable"
        return False
    return True


def resolve_startup_connection(
    cli_arg: str | None,
    configured: str | None,
    ports: Sequence[PortInfo],
    *,
    usb_enabled: bool = True,
    sik_enabled: bool = True,
    udp_fallback_enabled: bool = True,
    default_connection: str = UDP_FALLBACK_CONNECTION,
    udp_fallback: str = UDP_FALLBACK_CONNECTION,
    validator: Callable[[str], None] | None = None,
) -> StartupDecision:
    """Decide the connection string to start on. Pure: *ports* is injected.

    An explicit command-line argument outranks everything, including a flight
    controller on a cable — it is the one input that can only have come from
    somebody who meant it. Below that the hardware in front of the operator
    wins over the string in the config file, which is the whole point: a file
    written at a desk last week should not beat the cable plugged in now.

    An unusable value never fails the launch. A ground station that comes up on
    the wrong port is recoverable from inside the app; one that exits on a
    config typo is recoverable only by editing JSON in a field.
    """
    if _is_valid(cli_arg, validator):
        return StartupDecision(str(cli_arg).strip(), REASON_CLI)

    pick = pick_usb_serial(ports, usb_enabled=usb_enabled, sik_enabled=sik_enabled)
    if pick is not None:
        device, baud, kind = pick
        conn = serial_connection_string(device, baud)
        reason = REASON_USB if kind == "usb" else REASON_SIK
        candidates = [p.device for p in ports if p.kind == kind and _usable(p)]
        detail = device if len(candidates) < 2 else (
            f"{device} ({len(candidates)} candidates)"
        )
        return StartupDecision(conn, reason, detail)

    if _is_valid(configured, validator):
        return StartupDecision(str(configured).strip(), REASON_CONFIGURED)

    if udp_fallback_enabled and _is_valid(udp_fallback, validator):
        return StartupDecision(udp_fallback, REASON_UDP_FALLBACK)

    return StartupDecision(default_connection, REASON_DEFAULT_FALLBACK)


def suggest_connection(
    snapshot: dict,
    ports: Sequence[PortInfo],
    session: SessionState,
) -> Suggestion | None:
    """A device worth offering the operator, or None.

    Only called when the link is busy: this is the mid-session plug, where the
    answer is a row in the LINK tab and never a teardown. Suppressed entirely
    once the operator has chosen a link by hand — nagging somebody about the
    port they just decided against is how a suggestion becomes noise.
    """
    if not session.enabled or session.manual_override:
        return None
    pick = pick_usb_serial(ports, usb_enabled=session.usb, sik_enabled=session.sik)
    if pick is None:
        return None
    device, baud, kind = pick
    conn = serial_connection_string(device, baud)
    # Already connected on it: there is nothing to suggest.
    if str(snapshot.get("link_connection", "") or "") == conn:
        return None
    return Suggestion(
        connection_string=conn,
        kind="usb-direct" if kind == "usb" else "sik",
        device=device,
    )


class AutoConnectWatcher:
    """One daemon thread that dials while disconnected and suggests otherwise.

    It lives in the server layer rather than inside
    :class:`~corvus.mavlink_bridge.MavlinkBridge` on purpose. The bridge owns
    one transport string and the backoff that reconnects it; deciding *which*
    transport is policy, and policy belongs where the store, the config and the
    forwarder wiring already are. Keeping them apart means the bridge keeps its
    single stop/set/start contract, the policy stays unit-testable without a
    socket, and there is exactly one shutdown path for each.

    The dial is the same ``stop → set_connection → start`` sequence
    ``POST /api/mavlink/connect`` performs, validated first, so the exclusive
    serial ``flock``, the tlog rotation and the forwarder sink behave exactly as
    they do for a manual connect.
    """

    def __init__(
        self,
        *,
        bridge: MavlinkBridge,
        store: Any,
        session: SessionState,
        list_ports_fn: Callable[[], list[dict]] | None = None,
        poll_s: float = DEFAULT_POLL_S,
        stale_after_s: float = STALE_AFTER_S,
    ) -> None:
        self._bridge = bridge
        self._store = store
        self._session = session
        self._list_ports = list_ports_fn or MavlinkBridge.list_serial_ports
        self._poll_s = max(0.1, float(poll_s))
        self._stale_after_s = float(stale_after_s)
        self._stop_event = threading.Event()
        self._thread: threading.Thread | None = None

    # ---- lifecycle ----

    def start(self) -> None:
        """Begin watching. Idempotent; a no-op when auto-connect is disabled."""
        if not self._session.enabled:
            logger.info("autoconnect: watcher not started (disabled in config)")
            return
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop_event.clear()
        self._thread = threading.Thread(
            target=self._loop, name="autoconnect-watcher", daemon=True,
        )
        self._thread.start()

    def stop(self, timeout: float = 2.0) -> None:
        """Wake the thread and join it. Never raises; safe to call twice.

        The join budget sits inside the launcher's 2 s teardown watchdog, and
        the thread is a daemon, so a port enumeration that hangs on a wedged
        driver can delay the exit by at most that long and can never prevent it.
        """
        self._stop_event.set()
        thread = self._thread
        if thread is not None and thread.is_alive():
            thread.join(timeout=timeout)
        self._thread = None

    def is_alive(self) -> bool:
        """Is the watcher thread running?"""
        return self._thread is not None and self._thread.is_alive()

    # ---- the loop ----

    def _loop(self) -> None:
        # Event.wait rather than time.sleep so stop() is felt at once instead of
        # at the end of the current poll interval.
        while not self._stop_event.wait(self._poll_s):
            try:
                self.tick()
            except Exception:  # noqa: BLE001 - a watcher must never kill the app
                logger.exception("autoconnect: watcher iteration failed")

    def tick(self) -> str:
        """One decision pass. Returns what it did, for logs and tests.

        Public so the policy can be exercised without starting a thread.
        """
        if self._stop_event.is_set() or not self._session.enabled:
            return "disabled"
        if self._session.manual_override:
            return "noop: manual-override"
        if not self._bridge.is_running():
            return "noop: bridge-stopped"

        snapshot = self._snapshot()
        status = str(snapshot.get("link_status", "") or "").lower()
        idle = status in IDLE_STATES and self._is_stale()

        ports = classify_ports(self._list_ports())
        pick = pick_usb_serial(
            ports, usb_enabled=self._session.usb, sik_enabled=self._session.sik,
        )
        if pick is None:
            self._publish_suggestion(None)
            # Nothing is plugged in, so the "already dialled this one" latch has
            # nothing left to describe. Clearing it makes the latch per
            # appearance rather than per process: an operator who unplugs a
            # cable and plugs it back in is telling us to try again, and a
            # latch that outlived the device would answer that with silence.
            self._session.last_dial_key = None
            return f"noop: {REASON_NO_CANDIDATE}"

        if not idle:
            suggestion = suggest_connection(snapshot, ports, self._session)
            published = self._publish_suggestion(suggestion)
            if suggestion is None:
                return "noop: fresh-heartbeat"
            return "suggested" if published else "noop: suggestion-unchanged"

        return self._auto_dial(pick)

    # ---- actions ----

    def _auto_dial(self, pick: tuple[str, int, str]) -> str:
        device, baud, kind = pick
        conn = serial_connection_string(device, baud)
        if conn == self._session.last_dial_key:
            # One attempt per device. The bridge's own backoff owns the retry;
            # re-dialling every poll would tear a half-open link down mid-
            # handshake, forever, which reads as a device that never connects.
            return "noop: already-dialled"
        try:
            self._bridge.validate_connection(conn)
        except ValueError as exc:
            logger.warning("autoconnect: refusing %s (%s)", conn, exc)
            self._session.last_dial_key = conn
            return "noop: invalid"

        reason = REASON_USB if kind == "usb" else REASON_SIK
        logger.info(
            "autoconnect: dialling winner=%s reason=%s detail=%s", conn, reason, device,
        )
        self._session.last_dial_key = conn
        try:
            self._bridge.stop()
            self._bridge.set_connection(conn)
            self._bridge.start()
        except Exception:  # noqa: BLE001 - a failed dial must not kill the watcher
            logger.exception("autoconnect: dial of %s failed", conn)
            return "dial-failed"
        self._session.note_decision(reason, conn)
        self._publish_suggestion(None)
        self._publish_auto()
        return f"dialled: {reason}"

    def _publish_suggestion(self, suggestion: Suggestion | None) -> bool:
        """Push (or clear) ``link_suggestion``; True when the store changed.

        Deduped because the watcher re-decides every couple of seconds and the
        answer is usually the same one: republishing it would reopen a row the
        operator has already dismissed, every tick, until they unplug the cable.
        """
        key = suggestion.connection_string if suggestion else None
        if key == self._session.last_suggestion_key:
            return False
        self._session.last_suggestion_key = key
        if suggestion is not None:
            logger.info(
                "autoconnect: suggestion-published device=%s kind=%s",
                suggestion.device, suggestion.kind,
            )
        self._update_store(link_suggestion=suggestion.as_dict() if suggestion else None)
        return True

    def _publish_auto(self) -> None:
        self._update_store(link_auto=self._session.as_dict())

    # ---- read-only helpers ----

    def _snapshot(self) -> dict:
        try:
            return self._store.get_snapshot() or {}
        except Exception:  # noqa: BLE001
            return {}

    def _is_stale(self) -> bool:
        try:
            return bool(self._store.is_stale(self._stale_after_s))
        except Exception:  # noqa: BLE001
            return True

    def _update_store(self, **kwargs: Any) -> None:
        try:
            self._store.update(**kwargs)
        except Exception:  # noqa: BLE001 - publishing is never worth a crash
            logger.exception("autoconnect: store update failed")
