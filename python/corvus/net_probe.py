"""Is a host on the network answering? One ICMP echo through the system ``ping``.

A browser cannot send an ICMP echo, and a raw socket needs privileges the
operator's account does not have, so this asks the operating system's own
``ping`` for exactly one echo and reads its answer. That is what the Schwalby
plugin's companion-computer indicator polls: one probe per request, the
frontend decides how often and when silence counts as offline.

The host is checked before it goes anywhere near a command line (letters,
digits and the punctuation of a name or an address, never a leading dash) and
is passed as its own argv entry, never through a shell.

Every probe is a child of Corvus, bounded by its own deadline and ended by
:meth:`Pinger.shutdown`, so a probe in flight when the app closes is not left
behind.

stdlib only.
"""
from __future__ import annotations

import logging
import math
import re
import shutil
import subprocess
import sys
import threading
from typing import Any

from corvus.child_env import child_env

logger = logging.getLogger(__name__)

MAX_HOST = 253
MIN_WAIT_S = 0.5
MAX_WAIT_S = 10.0
# Past the echo's own wait: what a slow ping gets to start and print.
GRACE_S = 2.0
# Probes in flight at once. The plugin runs one at a time; this bounds a
# misbehaving caller rather than shaping normal use.
MAX_CONCURRENT = 4

_HOST_RE = re.compile(r"^[A-Za-z0-9:](?:[A-Za-z0-9._:%-]*[A-Za-z0-9])?$")
_RTT_RE = re.compile(r"[=<]\s*([0-9]+(?:[.,][0-9]+)?)\s*ms\b", re.IGNORECASE)


def clean_host(raw: Any) -> tuple[str, str]:
    """``(host, "")`` for a usable host name or address, else ``("", why)``."""
    host = str(raw if raw is not None else "").strip()
    if not host:
        return "", "Enter an IP address or host name."
    if len(host) > MAX_HOST or not _HOST_RE.match(host):
        return "", "That is not an IP address or host name."
    return host, ""


def ping_command(ping: str, host: str, wait_s: float,
                 platform: str | None = None) -> list[str]:
    """The argv for one echo to *host*, waiting at most *wait_s* for the reply."""
    plat = sys.platform if platform is None else platform
    seconds = str(max(1, math.ceil(wait_s)))
    if plat == "win32":
        return [ping, "-n", "1", "-w", str(max(1, int(wait_s * 1000))), host]
    if plat == "darwin":
        # macOS' ping is IPv4 only; ping6 has no overall timeout, so the
        # process deadline is what bounds it.
        if ":" in host:
            return [shutil.which("ping6") or "ping6", "-c", "1", "-n", host]
        return [ping, "-c", "1", "-n", "-t", seconds, host]
    return [ping, "-c", "1", "-n", "-W", seconds, host]


def parse_reply(returncode: int, output: str, host: str = "",
                platform: str | None = None) -> tuple[bool, float | None]:
    """Whether *output* shows an echo reply, and its round trip in ms.

    Windows answers "Destination host unreachable" from a router with exit
    code 0, so a reply there also needs the TTL an IPv4 echo carries (an IPv6
    one carries none, and has to show its time instead).
    """
    plat = sys.platform if platform is None else platform
    if returncode != 0:
        return False, None
    match = _RTT_RE.search(output or "")
    rtt = float(match.group(1).replace(",", ".")) if match else None
    if plat == "win32":
        if ":" in host:
            return rtt is not None, rtt
        return "ttl=" in (output or "").lower(), rtt
    return True, rtt


def _no_window_flags() -> int:
    return getattr(subprocess, "CREATE_NO_WINDOW", 0) if sys.platform == "win32" else 0


class Pinger:
    """One-shot pings, each a child of Corvus that ends with it."""

    def __init__(self, ping: str | None = None) -> None:
        self._ping = ping if ping is not None else (shutil.which("ping") or "")
        self._lock = threading.Lock()
        self._procs: set[subprocess.Popen[bytes]] = set()
        self._slots = threading.BoundedSemaphore(MAX_CONCURRENT)
        self._closed = False

    @property
    def available(self) -> bool:
        """Whether there is a ``ping`` to ask."""
        return bool(self._ping)

    def ping(self, raw_host: Any, wait_s: float = 2.0) -> dict[str, Any]:
        """Send one echo and wait for it.

        ``{"ok": True, "host", "reachable", "rtt_ms"}`` once ping has answered
        either way, ``{"ok": False, "error"}`` when the probe could not be made.
        """
        host, problem = clean_host(raw_host)
        if problem:
            return {"ok": False, "error": problem}
        if not self._ping:
            return {"ok": False, "error": "There is no ping program on this computer."}
        wait = min(max(float(wait_s), MIN_WAIT_S), MAX_WAIT_S)
        if not self._slots.acquire(blocking=False):
            return {"ok": False, "error": "Too many pings at once."}
        try:
            return self._probe(host, wait)
        finally:
            self._slots.release()

    def _probe(self, host: str, wait: float) -> dict[str, Any]:
        with self._lock:
            if self._closed:
                return {"ok": False, "error": "Corvus is shutting down."}
            try:
                proc = subprocess.Popen(
                    ping_command(self._ping, host, wait), stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    creationflags=_no_window_flags(), env=child_env(),
                )
            except OSError as exc:
                return {"ok": False, "error": f"ping could not be started: {exc.strerror or exc}"}
            self._procs.add(proc)
        try:
            try:
                out, _ = proc.communicate(timeout=wait + GRACE_S)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.communicate()
                return {"ok": True, "host": host, "reachable": False, "rtt_ms": None}
        finally:
            with self._lock:
                self._procs.discard(proc)
        reachable, rtt = parse_reply(proc.returncode, out.decode("utf-8", "replace"), host)
        return {"ok": True, "host": host, "reachable": reachable,
                "rtt_ms": rtt if reachable else None}

    def shutdown(self) -> None:
        """End every probe in flight and refuse new ones. Never raises."""
        with self._lock:
            self._closed = True
            procs = list(self._procs)
        for proc in procs:
            try:
                proc.kill()
                proc.wait(1.0)
            except Exception:  # noqa: BLE001 - teardown never raises
                logger.exception("ending a ping failed")
