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

And the switch grants that privilege to the *configured* endpoints, not to
whoever happens to reach the socket. The two directions are deliberately not
symmetric: being learned as a peer earns a copy of the telemetry, because that
costs nothing and is the whole mechanism by which a second screen works, while
putting a frame on the uplink is restricted to the addresses the operator
named. The difference matters the moment ``listen_host`` is widened past
loopback — an operator who does that to reach a tablet across the field would
otherwise have handed an arm command to every machine on the network.

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

One socket, two threads, all released by :meth:`stop`. The logic, the frame
splitting and both threads are ``corvus_link.bridge.ForwarderEngine``, in Rust
(``CoreThread``s under the link's shutdown guard); the socket and the state
stay on the forwarder.
"""
from __future__ import annotations

import collections
import logging
import socket
import sys
import threading
import time  # noqa: F401 - the engine's clock
from typing import Any
from collections.abc import Callable, Iterable

from corvus_link import bridge as _bridge

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
    return _bridge.parse_endpoint(text, default_port)


def split_frames(buf: bytes) -> tuple[list[bytes], bytes]:
    """Split a byte run into whole MAVLink frames plus the trailing remainder.

    Validation is by length and framing only — the CRC needs the per-message
    CRC_EXTRA table, which belongs to corvus_link, not here. That is enough for
    the job: this exists so a truncated or garbage datagram can never put a
    partial frame onto the link to the aircraft, not to police message content.
    """
    frames, rest = _bridge.split_frames(buf)
    return frames, rest


def frame_source_system(frame: bytes) -> int:
    """The system id a whole MAVLink frame was sent under (0 if unreadable).

    Read positionally rather than parsed: the header layout is fixed in both
    wire versions, and this is only ever asked of frames :func:`split_frames`
    has already vouched for.
    """
    return _bridge.frame_source_system(frame)


def _local_host_addresses() -> frozenset[str]:
    """This machine's own IPv4 addresses, as far as the stdlib can tell.

    Best effort: a host whose name does not resolve still has loopback, which
    is where the looping default lives.
    """
    found: set[str] = set()
    try:
        _name, _aliases, addrs = socket.gethostbyname_ex(socket.gethostname())
        found.update(addrs)
    except OSError:
        pass
    return frozenset(found)


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
        link_address: Callable[[], tuple[str, int] | None] | None = None,
    ) -> None:
        self._inject = inject
        # Where the vehicle link itself is bound, asked afresh each time since
        # the link reconnects under us. See _is_vehicle_link.
        self._link_address = link_address
        self._local_hosts: frozenset[str] = frozenset()
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
        # Our own address is left out: every mirrored frame would come
        # straight back, and with commanding on put the aircraft's own
        # telemetry onto the uplink.
        self._static: list[tuple[str, int]] = list(_engine.static_endpoints(self, endpoints))
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
        # corvus_link CoreThreads, under the link's shutdown guard.
        self._rx_thread: Any = None
        self._tx_thread: Any = None
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
        # Frames dropped because their sender is not a configured endpoint,
        # and the addresses they came from, so the refusal is visible rather
        # than a station whose commands quietly do nothing.
        self._refused = 0
        self._refused_peers: set[tuple[str, int]] = set()
        # The endpoint that turned out to be the vehicle link's own socket,
        # latched so status() can say why it gets nothing.
        self._loop_blocked: tuple[str, int] | None = None

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
        return _engine.start(self)

    def _bind(self) -> tuple[socket.socket | None, tuple[str, int], str]:
        """Bind the listen socket, falling back to an ephemeral port.

        Returns ``(socket, bound_address, note)``; the socket is None on a
        failure that no fallback covers, with :attr:`error` already set. The
        note is non-empty when the operator's chosen port was not the one we
        got — the LINK tab shows it, because a station configured to dial into
        the old port needs to know.
        """
        return tuple(_engine.bind(self))

    def stop(self) -> None:
        """Idempotent teardown: threads joined, socket closed, queue dropped."""
        _engine.stop(self)

    # ------------------------------------------------------------------
    # Data path
    # ------------------------------------------------------------------

    def feed(self, raw: bytes) -> None:
        """Queue one raw frame from the aircraft. Never blocks, never raises.

        Called from the MAVLink receive loop, so it does exactly one thing: a
        bounded append. Any send that would block belongs on the tx thread.
        """
        _engine.feed(self, raw)

    def _resolve_static(self, force: bool = False) -> None:
        """Turn the static endpoints' host names into addresses, once.

        ``sendto`` resolves a name on every call, and it does it on the tx
        thread: one slow or unreachable DNS server then stalls the mirror for
        as long as the resolver takes, per frame, which overflows the outbound
        queue and drops the aircraft's telemetry to keep a name lookup company.
        So the lookup happens here — at start, and afterwards only for the
        names that failed, at most every ``_RESOLVE_RETRY_S``.
        """
        _engine.resolve_static(self, force)

    def _live_peers(self) -> list[tuple[str, int]]:
        """The endpoints that have spoken MAVLink to us recently, expired swept."""
        return list(_engine.live_peers(self))

    def _targets(self) -> list[tuple[str, int]]:
        """Everywhere one mirrored frame goes: the configured endpoints, plus
        whoever has dialled in, minus ourselves and the vehicle link."""
        return list(_engine.targets(self))

    def _is_vehicle_link(self, addr: tuple[str, int]) -> bool:
        """Is *addr* the socket Corvus receives the aircraft on?

        SITL and QGroundControl both default to UDP 14550, so "listen on
        14550 for the vehicle" and "mirror to 127.0.0.1:14550" is the setup an
        operator gets by accepting both defaults. Every mirrored frame then
        lands back on the vehicle link, is mirrored again, and with commanding
        on is sent to the aircraft again: old takeoff and mission commands
        replayed for as long as the loop runs, and old heartbeats making the
        mode and armed state flicker.
        """
        return _engine.is_vehicle_link(self, addr)

    def _note_loop(self, addr: tuple[str, int]) -> None:
        """Latch (and log once) an endpoint skipped as the vehicle link."""
        _engine.note_loop(self, addr)

    def _loop_text(self) -> str:
        return _engine.loop_text(self)

    def _tx_loop(self) -> None:
        _engine.tx_loop(self)

    def _rx_loop(self) -> None:
        _engine.rx_loop(self)

    def _drop_stale_rx_buffers(self) -> None:
        """Forget the half-frames of endpoints that have stopped talking.

        Runs on the rx thread alone, so ``_rx_buf`` needs no lock; the peer
        table it consults does.
        """
        _engine.drop_stale_rx_buffers(self)

    def _note_command_refused(self, addr: tuple[str, int]) -> None:
        """Record (and log once) a station whose frames were not put on the uplink.

        Without this the refusal is silent, and silent is the wrong shape for
        it: the operator turned commanding on, the other station is plainly
        connected and receiving telemetry, and its commands simply do nothing.
        The fix is one line of config, but only if they can find out that this
        is what happened.
        """
        _engine.note_command_refused(self, addr)

    def _note_sysid_conflict(self) -> None:
        """Latch (and log once) that the other station shares our system id.

        Corvus transmits as system 254 on purpose. A second station using the
        same id makes PX4 fold two senders' sequence numbers into one counter
        and report packet loss that is not happening — and leaves a GCS
        failsafe unable to say which station went away. Nothing here can fix
        that from this end, so it is reported where the operator will see it.
        """
        _engine.note_sysid_conflict(self)

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
        return _engine.status(self)


# The logic, built over this module's constants, clock and logger.
_engine = _bridge.ForwarderEngine(sys.modules[__name__])
