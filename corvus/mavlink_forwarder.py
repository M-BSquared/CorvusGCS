"""Second-station MAVLink forwarding — QGroundControl alongside Corvus GCS.

A serial port, a USB autopilot and a SiK radio can each be opened by exactly
one program. So running QGroundControl next to Corvus GCS has always meant
closing one of them, and in the field that means giving up the telemetry you
are watching in order to look at a mission plan.

This module removes the choice. Corvus keeps the one real link and
re-broadcasts every frame it receives to local UDP endpoints; QGroundControl
(or MAVProxy, or a laptop on the same network) connects to one of those and
sees the identical stream. Frames those endpoints send back are injected into
the vehicle link, which is what turns a second screen into a second station.

**That last half is off by default and that is deliberate.** Two stations that
can both arm the aircraft and both change its mode is a genuine hazard, and no
default can know whether a given operator wants it. Forwarding telemetry
outward carries no such risk and is what the toggle turns on first; commanding
inward is a second, explicit switch. Neither is inferred.

Two ports, not one, and the distinction is the whole feature working
=================================================================

``host``/``port`` is where the *other station* listens — 127.0.0.1:14550,
which is the UDP link QGroundControl opens out of the box. Corvus mirrors
every frame there from the first one, without waiting to be spoken to.

``listen_host``/``listen_port`` is Corvus' own socket, and it is a *different*
port (14551) for a reason worth writing down, because getting it wrong makes
the feature look like it works and deliver nothing. QGroundControl's default
UDP link *binds* 14550 and waits. If Corvus binds 14550 too, then on Linux the
second bind fails and QGroundControl silently has no link, while on macOS both
binds succeed — SO_REUSEADDR — and the more specific 127.0.0.1 socket wins
every datagram, so QGroundControl shows a link that receives nothing and
neither program reports an error. Two listeners and no talker. Corvus binds
its own port and talks first, which is the same shape mavlink-router uses.

The listen port only matters for a station that dials *in* to a known address,
so a busy one is not worth failing over: the bind falls back to an ephemeral
port and ``status()`` reports which. Mirroring outward — the part the operator
actually asked for — needs no fixed local port at all.

Design, in one paragraph. One UDP socket, bound once. Outbound frames go onto a
bounded drop-oldest deque that a daemon thread drains — the MAVLink receive
loop only appends, exactly as it does for the tlog writer, so a wedged endpoint
can never stall the link to the aircraft. Inbound datagrams are parsed into
whole MAVLink frames (v1 and v2) before anything is injected, so a stray UDP
packet cannot put half a frame on the wire. Every endpoint that speaks MAVLink
to us is learned and forgotten again after a silence, which is what lets an
operator start and stop QGroundControl as often as they like without touching
Corvus.

Stdlib only. One socket, two daemon threads, all released by :meth:`stop`.
"""
from __future__ import annotations

import collections
import logging
import socket
import threading
import time
from typing import Any, Callable, Iterable

logger = logging.getLogger("corvus.forward")

# The port QGroundControl LISTENS on out of the box, so mirroring there is
# what "nothing to configure on the other end" actually means.
DEFAULT_PORT = 14550
# Loopback by default: mirroring to a routable address would put this
# aircraft's telemetry — and, with commanding on, a path to its uplink — on
# every network the laptop happens to be joined to.
DEFAULT_HOST = "127.0.0.1"

# Corvus' own socket. Deliberately NOT DEFAULT_PORT: see the module docstring.
DEFAULT_LISTEN_HOST = "127.0.0.1"
DEFAULT_LISTEN_PORT = 14551

# MAVLink frame start bytes and the fixed overhead around the payload.
_STX_V1 = 0xFE
_STX_V2 = 0xFD
_V1_OVERHEAD = 8              # STX,len,seq,sysid,compid,msgid + 2 CRC
_V2_OVERHEAD = 12             # STX,len,incompat,compat,seq,sys,comp,msgid[3] + 2 CRC
_V2_SIGNATURE_LEN = 13        # present when the incompat flag says signed
_MAVLINK_IFLAG_SIGNED = 0x01
_MAX_FRAME = 280

# Bound the outbound buffer so a stalled endpoint costs memory, never frames
# from the aircraft. Drop-oldest: on a second station, fresh telemetry beats a
# complete one.
_MAX_QUEUE = 2000
# An endpoint that has said nothing for this long is assumed gone, so a closed
# QGroundControl stops being sent a stream nobody reads.
_PEER_TTL_S = 30.0
_DRAIN_TIMEOUT_S = 0.2
# Half-frames waiting for their next datagram. One entry per endpoint that is
# mid-frame; past this many, the ones whose endpoint has gone quiet are swept.
_MAX_RX_BUFFERS = 64
# How long a static endpoint that would not resolve is left alone before the
# name is tried again. Long enough that a permanently bad name costs one
# lookup a minute rather than one per frame.
_RESOLVE_RETRY_S = 30.0

# The system id Corvus itself transmits under — mavlink_bridge.GCS_SYSTEM_ID,
# repeated here rather than imported so this module stays stdlib-only (a test
# pins the two together). A second station sending under the same id makes PX4
# attribute two senders' sequence numbers to one, and report packet loss that
# is not happening.
_CORVUS_SYSTEM_ID = 254


def parse_endpoint(text: str, default_port: int = DEFAULT_PORT) -> tuple[str, int] | None:
    """``"host:port"`` / ``"host"`` -> ``(host, port)``. None if unusable.

    Returns None rather than raising: an endpoint list is operator-typed
    config, and one bad line must cost that line, not the whole feature.
    """
    raw = str(text or "").strip()
    if not raw:
        return None
    host, _, port_text = raw.rpartition(":")
    if not host:                      # bare "host", or a leading colon
        host, port_text = raw, ""
    try:
        port = int(port_text) if port_text else default_port
    except ValueError:
        return None
    if not (0 < port < 65536) or not host:
        return None
    return host, port


def split_frames(buf: bytes) -> tuple[list[bytes], bytes]:
    """Split a byte run into whole MAVLink frames plus the trailing remainder.

    Validation is by length and framing only — the CRC needs the per-message
    CRC_EXTRA table, which belongs to pymavlink, not here. That is enough for
    the job: this exists so a truncated or garbage datagram can never put a
    partial frame onto the link to the aircraft, not to police message content.
    """
    frames: list[bytes] = []
    i = 0
    n = len(buf)
    while i < n:
        stx = buf[i]
        if stx == _STX_V1:
            if n - i < 2:
                break
            total = buf[i + 1] + _V1_OVERHEAD
        elif stx == _STX_V2:
            if n - i < 3:
                break
            total = buf[i + 1] + _V2_OVERHEAD
            if buf[i + 2] & _MAVLINK_IFLAG_SIGNED:
                total += _V2_SIGNATURE_LEN
        else:
            i += 1                    # resynchronise on the next start byte
            continue
        if total > _MAX_FRAME:
            i += 1
            continue
        if n - i < total:
            break                     # a whole frame has not arrived yet
        frames.append(buf[i:i + total])
        i += total
    return frames, buf[i:]


def frame_source_system(frame: bytes) -> int:
    """The system id a whole MAVLink frame was sent under (0 if unreadable).

    Read positionally rather than parsed: the header layout is fixed in both
    wire versions, and this is only ever asked of frames :func:`split_frames`
    has already vouched for.
    """
    if len(frame) >= 6 and frame[0] == _STX_V1:
        return frame[3]
    if len(frame) >= 8 and frame[0] == _STX_V2:
        return frame[5]
    return 0


class MavlinkForwarder:
    """Mirrors the vehicle link onto UDP endpoints; optionally accepts theirs.

    ``feed`` is called from the MAVLink receive loop and never blocks.
    ``inject`` is the bridge callback that writes bytes to the aircraft; it is
    called only when ``allow_commands`` is true, and only with whole frames.

    ``host``/``port`` name where the other station listens and are mirrored to
    unconditionally; ``listen_host``/``listen_port`` are this forwarder's own
    socket, which a station may dial into instead. See the module docstring for
    why those are two different ports.
    """

    def __init__(
        self,
        inject: Callable[[bytes], bool] | None = None,
        *,
        host: str = DEFAULT_HOST,
        port: int = DEFAULT_PORT,
        listen_host: str = DEFAULT_LISTEN_HOST,
        listen_port: int = DEFAULT_LISTEN_PORT,
        endpoints: Iterable[str] = (),
        allow_commands: bool = False,
    ) -> None:
        self._inject = inject
        self._host = host or DEFAULT_HOST
        self._port = int(port or DEFAULT_PORT)
        self._listen_host = listen_host or DEFAULT_LISTEN_HOST
        # 0 is meaningful here — "any free port" — so it must survive the
        # or-default that the target port gets.
        self._listen_port = (
            DEFAULT_LISTEN_PORT if listen_port is None else int(listen_port)
        )
        self._allow_commands = bool(allow_commands)
        # The primary target first: it is the one an operator never had to
        # type, and the one a bare "turn it on" has to reach.
        self._static: list[tuple[str, int]] = []
        for item in (f"{self._host}:{self._port}", *endpoints):
            parsed = parse_endpoint(item)
            if parsed is None:
                logger.warning("ignoring unusable forward endpoint %r", item)
                continue
            if parsed in self._static:
                continue
            if parsed == (self._listen_host, self._listen_port):
                # Our own address. Every mirrored frame would come straight
                # back as an inbound datagram, and with commanding on that
                # puts the aircraft's own telemetry onto the uplink — double
                # the load on a 57 kbps radio, for nothing.
                logger.warning(
                    "ignoring forward endpoint %r: it is this forwarder's own address",
                    item,
                )
                continue
            self._static.append(parsed)
        # Static endpoints with their names already resolved to addresses, so
        # the send path never calls the resolver. Rebuilt by _resolve_static.
        self._resolved: list[tuple[str, int]] = []
        self._unresolved: list[tuple[str, int]] = []
        self._resolved_at: float = 0.0
        self._sock: socket.socket | None = None
        self._peers: dict[tuple[str, int], float] = {}
        self._peer_lock = threading.Lock()
        self._out: collections.deque[bytes] = collections.deque(maxlen=_MAX_QUEUE)
        self._cond = threading.Condition()
        self._stop = threading.Event()
        self._rx_thread: threading.Thread | None = None
        self._tx_thread: threading.Thread | None = None
        self._error = ""
        # Non-fatal but worth saying out loud — today, only "the port you
        # asked for was busy, here is the one you got".
        self._notice = ""
        self._sent = 0
        self._received = 0
        self._injected = 0
        self._dropped = 0
        self._rx_buf: dict[tuple[str, int], bytes] = {}
        # The socket's own name once bound, so a datagram that somehow came
        # from us is not learned as a peer to send to. Also the authority on
        # which port we ended up with when the requested one was busy.
        self._bound: tuple[str, int] | None = None
        # Latched once a second station is seen transmitting under Corvus'
        # system id; surfaced in status() so the LINK tab can say so.
        self._sysid_conflict = False

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def start(self) -> bool:
        """Bind and run. False (with :attr:`error` set) only if nothing binds.

        A busy listen port is the ordinary case — a second Corvus, or another
        station already there — and it is not worth the feature over: the port
        matters only to a station that dials in, while mirroring outward needs
        no fixed local port at all. So a taken port falls back to an ephemeral
        one and says so, and only a socket that cannot be created at all is a
        failure. Either way the app comes up with telemetry working.
        """
        if self._sock is not None:
            return True
        sock, bound, note = self._bind()
        if sock is None:
            logger.warning("MAVLink forwarding disabled: %s", self._error)
            return False
        self._sock = sock
        self._bound = bound
        # A fallback is a NOTICE, not an error: the mirror is running and the
        # operator is not looking at a failure. Only a forwarder that did not
        # come up leaves _error set.
        self._error = ""
        self._notice = note
        self._sysid_conflict = False
        self._resolve_static(force=True)
        self._stop.clear()
        self._rx_thread = threading.Thread(
            target=self._rx_loop, name="mav-forward-rx", daemon=True)
        self._tx_thread = threading.Thread(
            target=self._tx_loop, name="mav-forward-tx", daemon=True)
        self._rx_thread.start()
        self._tx_thread.start()
        logger.info(
            "MAVLink forwarding: mirroring to %s, listening on %s:%d "
            "(commands from other stations: %s)",
            ", ".join(f"{h}:{p}" for h, p in self._static) or "nothing",
            bound[0], bound[1],
            "allowed" if self._allow_commands else "blocked",
        )
        return True

    def _bind(self) -> tuple[socket.socket | None, tuple[str, int], str]:
        """Bind the listen socket, falling back to an ephemeral port.

        Returns ``(socket, bound_address, note)``; the socket is None on a
        failure that no fallback covers, with :attr:`error` already set. The
        note is non-empty when the operator's chosen port was not the one we
        got — the LINK tab shows it, because a station configured to dial into
        the old port needs to know.
        """
        for port, fallback in ((self._listen_port, False), (0, True)):
            if fallback and self._listen_port == 0:
                break                 # already asked for ephemeral; no retry
            try:
                sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind((self._listen_host, port))
                sock.settimeout(_DRAIN_TIMEOUT_S)
            except OSError as exc:
                try:
                    sock.close()
                except Exception:  # noqa: BLE001
                    pass
                self._error = (
                    f"cannot listen on {self._listen_host}:{port} "
                    f"({exc.strerror or exc})"
                )
                continue
            try:
                bound = sock.getsockname()
            except OSError:
                bound = (self._listen_host, port)
            note = ""
            if fallback:
                note = (
                    f"port {self._listen_port} was busy — listening on "
                    f"{bound[0]}:{bound[1]} instead"
                )
                logger.warning("MAVLink forwarding: %s", note)
            return sock, (bound[0], int(bound[1])), note
        return None, (self._listen_host, self._listen_port), self._error

    def stop(self) -> None:
        """Idempotent teardown: threads joined, socket closed, queue dropped."""
        self._stop.set()
        with self._cond:
            self._out.clear()
            self._cond.notify_all()
        sock = self._sock
        self._sock = None
        if sock is not None:
            try:
                sock.close()
            except Exception:  # noqa: BLE001 - teardown never raises
                pass
        for thread in (self._rx_thread, self._tx_thread):
            if thread is not None:
                try:
                    thread.join(timeout=2.0)
                except Exception:  # noqa: BLE001
                    pass
        self._rx_thread = None
        self._tx_thread = None
        with self._peer_lock:
            self._peers.clear()
        self._rx_buf.clear()
        self._bound = None

    # ------------------------------------------------------------------
    # Data path
    # ------------------------------------------------------------------

    def feed(self, raw: bytes) -> None:
        """Queue one raw frame from the aircraft. Never blocks, never raises.

        Called from the MAVLink receive loop, so it does exactly one thing: a
        bounded append. Any send that would block belongs on the tx thread.
        """
        if self._stop.is_set() or self._sock is None or not raw:
            return
        with self._cond:
            if len(self._out) == self._out.maxlen:
                self._dropped += 1
            self._out.append(raw)
            self._cond.notify()

    def _resolve_static(self, force: bool = False) -> None:
        """Turn the static endpoints' host names into addresses, once.

        ``sendto`` resolves a name on every call, and it does it on the tx
        thread: one slow or unreachable DNS server then stalls the mirror for
        as long as the resolver takes, per frame, which overflows the outbound
        queue and drops the aircraft's telemetry to keep a name lookup company.
        So the lookup happens here — at start, and afterwards only for the
        names that failed, at most every ``_RESOLVE_RETRY_S``.
        """
        now = time.monotonic()
        if not force and (
            not self._unresolved or now - self._resolved_at < _RESOLVE_RETRY_S
        ):
            return
        pending = list(self._static) if force else list(self._unresolved)
        keep = [] if force else [a for a in self._resolved]
        unresolved: list[tuple[str, int]] = []
        for host, port in pending:
            try:
                info = socket.getaddrinfo(
                    host, port, socket.AF_INET, socket.SOCK_DGRAM)
            except OSError as exc:
                unresolved.append((host, port))
                logger.warning("forward endpoint %s:%d does not resolve (%s)",
                               host, port, exc)
                continue
            addr = info[0][4][:2]
            entry = (str(addr[0]), int(addr[1]))
            if entry not in keep:
                keep.append(entry)
        self._resolved = keep
        self._unresolved = unresolved
        self._resolved_at = now

    def _live_peers(self) -> list[tuple[str, int]]:
        """The endpoints that have spoken MAVLink to us recently, expired swept."""
        now = time.monotonic()
        with self._peer_lock:
            live = [addr for addr, seen in self._peers.items()
                    if now - seen <= _PEER_TTL_S]
            if len(live) != len(self._peers):
                self._peers = {a: self._peers[a] for a in live}
        return live

    def _targets(self) -> list[tuple[str, int]]:
        """Everywhere one mirrored frame goes: the configured endpoints, plus
        whoever has dialled in, minus ourselves."""
        bound = self._bound
        out = [a for a in self._resolved if a != bound]
        for addr in self._live_peers():
            if addr not in out and addr != bound:
                out.append(addr)
        return out

    def _tx_loop(self) -> None:
        while not self._stop.is_set():
            with self._cond:
                if not self._out:
                    self._cond.wait(_DRAIN_TIMEOUT_S)
                    continue
                batch = list(self._out)
                self._out.clear()
            sock = self._sock
            if sock is None:
                continue
            if self._unresolved:
                self._resolve_static()
            targets = self._targets()
            if not targets:
                continue
            for frame in batch:
                for addr in targets:
                    try:
                        sock.sendto(frame, addr)
                        self._sent += 1
                    except OSError:
                        # An endpoint that has gone away must not take the
                        # aircraft's telemetry down with it. It ages out of
                        # _peers on its own; a static one keeps being tried.
                        self._dropped += 1
                    except Exception:  # noqa: BLE001
                        self._dropped += 1

    def _rx_loop(self) -> None:
        while not self._stop.is_set():
            sock = self._sock
            if sock is None:
                break
            try:
                data, addr = sock.recvfrom(65535)
            except socket.timeout:
                continue
            except OSError:
                if self._stop.is_set():
                    break
                time.sleep(0.05)
                continue
            if not data:
                continue
            if addr == self._bound:
                continue          # our own datagram; never a peer to mirror to
            self._received += 1
            # Reassemble per endpoint: UDP preserves datagram boundaries, but a
            # sender is free to split a frame across two of them.
            buffered = self._rx_buf.get(addr, b"") + data
            frames, remainder = split_frames(buffered)
            # Only an endpoint that is actually mid-frame keeps an entry. The
            # common case — a datagram holding whole frames — leaves nothing
            # over, and used to store an empty bytestring that nothing ever
            # removed, one per source address the socket had ever seen.
            if remainder and len(remainder) < _MAX_FRAME:
                self._rx_buf[addr] = remainder
            else:
                self._rx_buf.pop(addr, None)
            if len(self._rx_buf) > _MAX_RX_BUFFERS:
                self._drop_stale_rx_buffers()
            # Learned only on MAVLink, not on any datagram that arrives. A
            # peer is subscribed to the whole telemetry stream, so a stray
            # packet — a port scan, another program's stale socket, a broadcast
            # picked up when the operator widened listen_host — must not be
            # able to sign itself up for the aircraft's position at 10 Hz.
            if not frames and not remainder:
                continue
            with self._peer_lock:
                self._peers[addr] = time.monotonic()
            for frame in frames:
                # Checked before the commanding gate: the conflict is about
                # sequence numbers on the DOWNLINK, so it is just as real when
                # the second station is only listening — and telemetry-only is
                # the default, which is where the LINK tab promises to say so.
                if frame_source_system(frame) == _CORVUS_SYSTEM_ID:
                    self._note_sysid_conflict()
                if not self._allow_commands or self._inject is None:
                    continue
                try:
                    if self._inject(frame):
                        self._injected += 1
                except Exception:  # noqa: BLE001 - a bad inject never kills the loop
                    logger.debug("forward inject failed", exc_info=True)

    def _drop_stale_rx_buffers(self) -> None:
        """Forget the half-frames of endpoints that have stopped talking.

        Runs on the rx thread alone, so ``_rx_buf`` needs no lock; the peer
        table it consults does.
        """
        now = time.monotonic()
        with self._peer_lock:
            live = {a for a, seen in self._peers.items() if now - seen <= _PEER_TTL_S}
        for addr in [a for a in self._rx_buf if a not in live]:
            self._rx_buf.pop(addr, None)

    def _note_sysid_conflict(self) -> None:
        """Latch (and log once) that the other station shares our system id.

        Corvus transmits as system 254 on purpose. A second station using the
        same id makes PX4 fold two senders' sequence numbers into one counter
        and report packet loss that is not happening — and leaves a GCS
        failsafe unable to say which station went away. Nothing here can fix
        that from this end, so it is reported where the operator will see it.
        """
        if self._sysid_conflict:
            return
        self._sysid_conflict = True
        logger.warning(
            "the other station is transmitting as MAVLink system %d, the same id "
            "Corvus uses — give it its own (QGroundControl: Application Settings "
            "→ MAVLink → Ground Station system ID)",
            _CORVUS_SYSTEM_ID,
        )

    # ------------------------------------------------------------------
    # Introspection
    # ------------------------------------------------------------------

    @property
    def error(self) -> str:
        return self._error

    @property
    def notice(self) -> str:
        return self._notice

    def listen_address(self) -> tuple[str, int]:
        """The address actually bound, which is not always the one asked for."""
        return self._bound or (self._listen_host, self._listen_port)

    def status(self) -> dict[str, Any]:
        """What the LINK tab shows: where the stream is being sent, where this
        forwarder can be reached, and whether anything is answering."""
        listen_host, listen_port = self.listen_address()
        return {
            "running": self._sock is not None and not self._stop.is_set(),
            "host": self._host,
            "port": self._port,
            "listen_host": listen_host,
            "listen_port": listen_port,
            "listen_port_requested": self._listen_port,
            "allow_commands": self._allow_commands,
            "endpoints": [f"{h}:{p}" for h, p in self._static],
            # "peers" is the honest answer to "is anything actually there?":
            # only stations that have spoken to us, never the endpoints we
            # were told to mirror at whether or not anybody is listening.
            "peers": [f"{h}:{p}" for h, p in self._live_peers()],
            "targets": [f"{h}:{p}" for h, p in self._targets()],
            "frames_sent": self._sent,
            "datagrams_received": self._received,
            "frames_injected": self._injected,
            "dropped": self._dropped,
            "sysid_conflict": self._sysid_conflict,
            "notice": self._notice,
            "error": self._error,
        }
