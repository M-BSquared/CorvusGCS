"""SshSession behaviour the terminal depends on: replay and pty size.

Both exist for the same reason — the SSH tab is a real terminal now, not a
line-at-a-time command box.

``add_sub`` replays because the UI opens its SSE stream *after* the connect
call returns, so without a buffer the login banner and the first prompt are
already gone by the time anyone is listening and the terminal looks dead.

``resize`` exists because a shell laid out against the 80x24 the channel was
opened with wraps into a mess in any other window, and the panel this runs in
is nowhere near 80 columns wide.

No network here: the paramiko channel is faked, which is enough because both
behaviours are the session's own bookkeeping.
"""
from __future__ import annotations

import pytest

pytest.importorskip("paramiko")

from corvus.ssh_bridge import REPLAY_LIMIT, SshBridge, SshSession  # noqa: E402


class _FakeChannel:
    """The two channel calls a session makes outside its reader loop."""

    def __init__(self, fail_resize: bool = False) -> None:
        self.sent: list[str] = []
        self.resizes: list[tuple[int, int]] = []
        self._fail_resize = fail_resize

    def send(self, data: str) -> None:
        self.sent.append(data)

    def resize_pty(self, width: int, height: int) -> None:
        if self._fail_resize:
            raise OSError("channel closed")
        self.resizes.append((width, height))


def _live_session(channel: _FakeChannel | None = None) -> SshSession:
    """A session wired to a fake channel, without connecting to anything."""
    s = SshSession("dev", "10.0.0.5", 22, "schwalby")
    s._channel = channel if channel is not None else _FakeChannel()
    s._connected = True
    return s


# ---------------------------------------------------------------------------
# Replay
# ---------------------------------------------------------------------------

def test_add_sub_replays_output_published_before_it_subscribed() -> None:
    s = _live_session()
    s._publish("Welcome to companion\r\n")
    s._publish("schwalby@companion:~$ ")

    seen: list[str] = []
    s.add_sub(seen.append)

    assert "".join(seen) == "Welcome to companion\r\nschwalby@companion:~$ "


def test_replay_is_one_chunk_then_live_output_follows() -> None:
    s = _live_session()
    s._publish("before\r\n")
    seen: list[str] = []
    s.add_sub(seen.append)
    s._publish("after\r\n")

    assert seen == ["before\r\n", "after\r\n"]


def test_add_sub_can_opt_out_of_the_replay() -> None:
    s = _live_session()
    s._publish("before\r\n")
    seen: list[str] = []
    s.add_sub(seen.append, replay=False)

    assert seen == []


def test_replay_buffer_is_bounded() -> None:
    """A session left running for hours must not grow without limit."""
    s = _live_session()
    for _ in range(200):
        s._publish("x" * 4096)

    seen: list[str] = []
    s.add_sub(seen.append)
    assert len("".join(seen)) <= REPLAY_LIMIT


def test_removed_subscriber_stops_receiving() -> None:
    s = _live_session()
    seen: list[str] = []
    s.add_sub(seen.append)
    s.remove_sub(seen.append)   # a bound method compares equal by receiver
    s._publish("after removal\r\n")

    assert seen == []


def test_a_raising_subscriber_does_not_break_the_others() -> None:
    """One dead SSE client must not stop the shell reaching every other one."""
    s = _live_session()
    seen: list[str] = []
    s.add_sub(lambda _t: (_ for _ in ()).throw(RuntimeError("gone")))
    s.add_sub(seen.append)
    s._publish("still delivered\r\n")

    assert seen == ["still delivered\r\n"]


# ---------------------------------------------------------------------------
# pty size
# ---------------------------------------------------------------------------

def test_resize_reaches_the_channel_and_is_remembered() -> None:
    ch = _FakeChannel()
    s = _live_session(ch)

    assert s.resize(120, 40) is True
    assert ch.resizes == [(120, 40)]
    assert (s.cols, s.rows) == (120, 40)


def test_resize_before_connect_is_remembered_for_the_shell_request() -> None:
    """The size is what invoke_shell opens the channel with, so it has to
    survive being set while the session is still offline."""
    s = SshSession("dev", "10.0.0.5", 22, "schwalby")

    assert s.resize(100, 30) is False   # nothing to talk to yet
    assert (s.cols, s.rows) == (100, 30)


def test_resize_clamps_absurd_dimensions() -> None:
    ch = _FakeChannel()
    s = _live_session(ch)
    s.resize(0, 0)
    assert ch.resizes == [(2, 1)]


def test_resize_survives_a_channel_that_has_gone_away() -> None:
    s = _live_session(_FakeChannel(fail_resize=True))
    assert s.resize(90, 25) is False


def test_bridge_resize_targets_the_named_session() -> None:
    bridge = SshBridge()
    a, b = _FakeChannel(), _FakeChannel()
    bridge._sessions["A"] = _live_session(a)
    bridge._sessions["B"] = _live_session(b)

    assert bridge.resize("B", 100, 30) is True
    assert a.resizes == []
    assert b.resizes == [(100, 30)]


def test_bridge_resize_unknown_session_is_false_not_an_error() -> None:
    assert SshBridge().resize("nope", 80, 24) is False
