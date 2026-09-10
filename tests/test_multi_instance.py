"""Two copies of Corvus on one machine: ports, and what they must not share.

The single-instance lock (``tests/test_instance_lock.py``) stops the second
launch in the normal case. These tests cover what happens underneath it: the
port claim itself, which has to be correct whether the second process is
another Corvus, a leftover from a crashed one, or something else entirely
that happens to be on 8000.
"""
from __future__ import annotations

import errno
import os
import socket
import threading

import pytest

server_mod = pytest.importorskip("corvus.server")

from corvus.server import PORT_SEARCH_SPAN, CorvusHandler, CorvusServer, bind_server


def _free_port() -> int:
    """An ephemeral port, released before it is returned.

    Only used to pick a *starting* number for a search; nothing here relies
    on it still being free, which is the whole point of ``bind_server``.
    """
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def test_a_second_server_moves_to_the_next_port_instead_of_crashing() -> None:
    """The launch that loses the race gets a port, not a traceback.

    Two windows opened together both used to probe 8000, both saw it free
    (the probe socket is closed before the real bind), and the loser died on
    ``Address already in use`` inside the server constructor — before any
    window appeared, so it looked like the app simply failed to start.
    """
    first = bind_server(_free_port(), CorvusHandler, host="127.0.0.1")
    try:
        taken = first.server_address[1]
        second = bind_server(taken, CorvusHandler, host="127.0.0.1")
        try:
            assert second.server_address[1] != taken
            assert taken < second.server_address[1] <= taken + PORT_SEARCH_SPAN
        finally:
            second.server_close()
    finally:
        first.server_close()


def test_the_requested_port_is_used_when_it_is_free() -> None:
    """Searching is the fallback, not the behaviour: 8000 stays 8000."""
    wanted = _free_port()
    server = bind_server(wanted, CorvusHandler, host="127.0.0.1")
    try:
        assert server.server_address[1] == wanted
    finally:
        server.server_close()


def test_an_exhausted_range_names_the_ports_it_tried() -> None:
    """"No free port in 9000-9001" is actionable; a bare errno is not."""
    holders = []
    start = _free_port()
    try:
        for offset in range(2):
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                sock.bind(("127.0.0.1", start + offset))
                sock.listen(1)
                holders.append(sock)
            except OSError:
                sock.close()
                pytest.skip("could not reserve a contiguous pair of ports")
        with pytest.raises(OSError) as caught:
            bind_server(start, CorvusHandler, host="127.0.0.1", span=2)
        assert caught.value.errno == errno.EADDRINUSE
        assert f"{start}-{start + 1}" in str(caught.value)
    finally:
        for sock in holders:
            sock.close()


def test_a_failure_that_is_not_a_busy_port_is_raised_straight_away() -> None:
    """Retrying twenty ports that will all fail the same way helps nobody.

    A permission problem or an unusable interface is a property of the
    request, not of the port number, so it surfaces immediately with its own
    errno rather than being reshaped into "no free port in ...".
    """
    with pytest.raises(OSError) as caught:
        bind_server(1, CorvusHandler, host="203.0.113.1", span=PORT_SEARCH_SPAN)
    assert caught.value.errno not in (errno.EADDRINUSE,)


@pytest.mark.skipif(os.name == "nt", reason="POSIX reuse semantics")
def test_posix_keeps_address_reuse_so_a_restart_is_not_blocked() -> None:
    """On POSIX SO_REUSEADDR only skips TIME_WAIT — which is what we want.

    Restarting Corvus within seconds of quitting it must not fail on a port
    the kernel is still holding down from the previous run's closed sockets.
    """
    assert CorvusServer.allow_reuse_address is True


def test_windows_does_not_let_a_second_process_share_the_port() -> None:
    """SO_REUSEADDR means something different — and dangerous — on Windows.

    There it lets a second process bind a port another is *actively
    listening on*, after which the kernel hands each incoming connection to
    one of them arbitrarily. A second Corvus would silently take over half
    the first one's HTTP and SSE traffic: telemetry streams landing in the
    wrong window, config writes hitting the wrong backend. The flag is off
    there, so the second bind fails cleanly and ``bind_server`` moves on.
    """
    if os.name == "nt":
        assert CorvusServer.allow_reuse_address is False
    else:
        # The value is derived from the platform, so assert the rule itself
        # rather than skipping the check entirely off Windows.
        source = os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            "corvus", "server.py",
        )
        with open(source, encoding="utf-8") as handle:
            assert 'allow_reuse_address = os.name != "nt"' in handle.read()


def test_the_server_is_marked_stopping_before_its_resources_go_away() -> None:
    """SSE handlers park on a 15s wait and know nothing about shutdown.

    ``super().shutdown()`` only stops the accept loop, so every stream
    already connected would wake up afterwards and write into a backend
    whose caches are closed and whose downloader is None. The event is what
    those loops check, so they leave on their own.
    """
    server = bind_server(_free_port(), CorvusHandler, host="127.0.0.1")
    try:
        assert isinstance(server.stopping, threading.Event)
        assert not server.stopping.is_set()
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        server.shutdown()
        assert server.stopping.is_set()
        thread.join(timeout=5)
        assert not thread.is_alive()
    finally:
        server.server_close()
