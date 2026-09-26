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

import os
from typing import Any

import pytest

pytest.importorskip("paramiko")

import paramiko  # noqa: E402

from corvus import ssh_bridge  # noqa: E402
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


# ---------------------------------------------------------------------------
# Why a connect failed
# ---------------------------------------------------------------------------

class _FailingClient:
    """An SSHClient stand-in whose connect raises *exc*."""

    def __init__(self, exc: Exception) -> None:
        self._exc = exc

    def connect(self, **kwargs: Any) -> None:
        raise self._exc

    def close(self) -> None:
        pass


def _connects_fail_with(monkeypatch: pytest.MonkeyPatch, exc: Exception) -> None:
    monkeypatch.setattr(ssh_bridge, "_new_client", lambda: _FailingClient(exc))


def test_a_timeout_names_the_address_that_did_not_answer(monkeypatch) -> None:
    _connects_fail_with(monkeypatch, TimeoutError("timed out"))
    s = SshSession("dev", "192.168.1.99", 22, "pilot")
    assert s.connect() is False
    assert s.error == "No answer from 192.168.1.99:22 within 8 s."


def test_a_refused_port_says_so_without_the_errno(monkeypatch) -> None:
    refused = ConnectionRefusedError(61, "Connection refused")
    _connects_fail_with(monkeypatch, paramiko.ssh_exception.NoValidConnectionsError(
        {("127.0.0.1", 1): refused}))
    s = SshSession("dev", "127.0.0.1", 1, "pilot")
    assert s.connect() is False
    assert s.error == "Connection refused (127.0.0.1:1)."


def test_a_host_that_is_down_names_the_address(monkeypatch) -> None:
    _connects_fail_with(monkeypatch, OSError(64, "Host is down"))
    s = SshSession("dev", "192.168.1.99", 22, "pilot")
    assert s.connect() is False
    assert s.error == "Host is down (192.168.1.99:22)."


def test_any_other_failure_keeps_its_own_text(monkeypatch) -> None:
    _connects_fail_with(monkeypatch, paramiko.AuthenticationException("Authentication failed."))
    s = SshSession("dev", "10.0.0.5", 22, "pilot")
    assert s.connect() is False
    assert s.error == "Authentication failed."


def test_bridge_keeps_the_reason_until_the_next_attempt(monkeypatch) -> None:
    """A failed session is discarded, so the bridge is the only place the reason
    survives for the HTTP layer to hand back."""
    _connects_fail_with(monkeypatch, ConnectionRefusedError(61, "Connection refused"))
    bridge = SshBridge()
    assert bridge.connect("launcher/a", "127.0.0.1", 1, "pilot") is False
    assert bridge.connect_error("launcher/a") == "Connection refused (127.0.0.1:1)."
    assert bridge.connect_error("launcher/b") == ""

    bridge.disconnect("launcher/a")
    assert bridge.connect_error("launcher/a") == ""


@pytest.mark.parametrize("exc,expected", [
    (paramiko.AuthenticationException("Authentication failed."), True),
    (paramiko.BadAuthenticationType("Bad authentication type", ["publickey"]), True),
    (paramiko.SSHException("No authentication methods available"), True),
    (paramiko.SSHException("Error reading SSH protocol banner"), False),
    (TimeoutError("timed out"), False),
    (OSError(64, "Host is down"), False),
])
def test_only_a_refused_login_counts_as_one(exc, expected) -> None:
    assert ssh_bridge.is_auth_failure(exc) is expected


def test_bridge_remembers_a_refused_login_until_the_next_attempt(monkeypatch) -> None:
    """What lets the HTTP layer tell the UI to ask for a password rather than
    only saying that the connect failed."""
    _connects_fail_with(monkeypatch, paramiko.AuthenticationException("Authentication failed."))
    bridge = SshBridge()
    assert bridge.connect("schwalby/a", "10.0.0.5", 22, "pilot") is False
    assert bridge.connect_auth_failed("schwalby/a") is True
    assert bridge.connect_auth_failed("schwalby/b") is False

    _connects_fail_with(monkeypatch, TimeoutError("timed out"))
    assert bridge.connect("schwalby/a", "10.0.0.5", 22, "pilot") is False
    assert bridge.connect_auth_failed("schwalby/a") is False


def test_run_command_flags_a_refused_login(monkeypatch) -> None:
    _connects_fail_with(monkeypatch, paramiko.AuthenticationException("Authentication failed."))
    result = ssh_bridge.run_command("10.0.0.5", "uptime", username="pilot")
    assert result["ok"] is False
    assert result["auth_failed"] is True

    _connects_fail_with(monkeypatch, TimeoutError("timed out"))
    assert ssh_bridge.run_command("10.0.0.5", "uptime", username="pilot")["auth_failed"] is False


def test_the_first_connection_to_a_new_host_can_remember_its_key(tmp_path, monkeypatch) -> None:
    """Regression: on a station that had never accepted a host, every connect failed.

    paramiko's save_host_keys reads the known_hosts file again before writing
    it, and raised inside the missing-key policy while ``~/.corvus/known_hosts``
    did not exist yet. Nothing was ever saved, so the next attempt failed the
    same way. Only a laptop whose companion computers were already in
    ``~/.ssh/known_hosts`` could connect at all.
    """
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    monkeypatch.delenv(ssh_bridge.HOST_KEY_POLICY_ENV, raising=False)
    path = ssh_bridge.known_hosts_path()
    assert not os.path.exists(path)

    client = ssh_bridge._new_client()
    client._log = lambda *args, **kwargs: None    # paramiko logs through a live transport
    key = paramiko.RSAKey.generate(1024)
    policy = client._policy
    policy.missing_host_key(client, "[10.0.0.7]:22", key)     # raised FileNotFoundError before

    saved = paramiko.HostKeys(path)
    assert saved.lookup("[10.0.0.7]:22") is not None, "and the key is remembered"
    if os.name != "nt":
        assert (os.stat(path).st_mode & 0o777) == 0o600, "owner-only, like the config file"


class _Channel:
    """A shell channel that hands out its output in the chunks given."""

    def __init__(self, chunks=()):
        self.chunks = list(chunks)
        self.sent: list[bytes] = []

    def recv_ready(self):
        return bool(self.chunks)

    def recv(self, _n):
        return self.chunks.pop(0)

    def exit_status_ready(self):
        return not self.chunks

    def close(self):
        pass

    def sendall(self, data):
        self.sent.append(data)

    def send(self, data):
        raise AssertionError("send() may take less than all of it; input has to arrive whole")


def test_a_character_split_between_two_reads_arrives_whole():
    """The channel's window ends where it ends, often inside a character."""
    session = SshSession("split", "host")
    session._channel = _Channel([b"Gr\xc3", b"\xbc\xc3\x9fe \xe2\x94", b"\x80\xe2\x94\x80\r\n"])
    session._running.set()
    session._connected = True
    seen: list[str] = []
    session.add_sub(seen.append)
    session._read_loop()
    text = "".join(seen)
    assert "Grüße ──\r\n" in text
    assert "�" not in text


def test_a_large_paste_is_sent_whole():
    session = SshSession("paste", "host")
    channel = _Channel()
    session._channel = channel
    session._connected = True
    text = "ä" * 70000
    session.send(text)
    assert b"".join(channel.sent) == text.encode("utf-8")
