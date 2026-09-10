"""SSH bridge — real remote shell access via paramiko.

Manages SSH connections to companion computers or other devices. Each
connection runs an interactive shell on a background thread; output is
streamed byte-for-byte to subscribers, escape sequences included, so the
frontend can drive a real terminal emulator rather than a line-at-a-time
command box. Input is written straight to the shell channel, which is what
makes interactive programs (``top``, ``vim``, a password prompt, Ctrl-C)
behave the way they do in a terminal.

Two details exist for that terminal: a bounded **replay buffer** of recent
output, handed to every new subscriber so a stream opened after the
connection still shows the login banner and the first prompt; and
``resize``, which tells the remote pty how large the operator's terminal
actually is so full-screen programs wrap correctly.
"""
from __future__ import annotations

import collections
import logging
import threading
import time
from typing import Any, Callable

import paramiko

logger = logging.getLogger("corvus.ssh")

# How much recent shell output a session keeps for replay to a subscriber that
# attaches after the fact. One screenful of a scrolling build log is a few KB;
# 256 KB is enough that reopening the SSH tab shows real history, small enough
# that an idle session's memory stays negligible.
REPLAY_LIMIT = 256 * 1024


class SshSession:
    """A single SSH session with an interactive shell."""

    def __init__(
        self,
        name: str,
        host: str,
        port: int = 22,
        username: str = "corvus",
        password: str | None = None,
        key_path: str | None = None,
    ) -> None:
        self.name = name
        self.host = host
        self.port = port
        self.username = username
        self._password = password
        self._key_path = key_path
        self._client: paramiko.SSHClient | None = None
        self._channel: Any = None
        self._thread: threading.Thread | None = None
        self._running = threading.Event()
        # _sub_lock guards BOTH the subscriber list and the replay buffer, and
        # is held across a whole publish. That is what makes a late subscriber
        # see every chunk exactly once: it cannot take its replay copy in the
        # middle of a publish. Subscribers are queue puts, so the reader thread
        # is never blocked for long.
        self._sub_lock = threading.Lock()
        self._subs: list[Callable[[str], None]] = []
        self._replay: collections.deque[str] = collections.deque()
        self._replay_len = 0
        self._connected = False
        self.cols = 80
        self.rows = 24

    @property
    def connected(self) -> bool:
        return self._connected

    def connect(self) -> bool:
        """Open the SSH connection and start the shell reader thread."""
        try:
            self._client = paramiko.SSHClient()
            self._client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
            kwargs: dict[str, Any] = {
                "hostname": self.host,
                "port": self.port,
                "username": self.username,
                "timeout": 8,
            }
            if self._key_path:
                kwargs["key_filename"] = self._key_path
            elif self._password:
                kwargs["password"] = self._password
            self._client.connect(**kwargs)
            self._channel = self._client.invoke_shell(
                term="xterm-256color", width=self.cols, height=self.rows,
            )
            self._channel.settimeout(0.5)
            self._connected = True
            self._running.set()
            self._thread = threading.Thread(
                target=self._read_loop, name=f"ssh-{self.name}", daemon=True,
            )
            self._thread.start()
            self._publish(f"Connected to {self.username}@{self.host}:{self.port}\r\n")
            return True
        except Exception as exc:
            logger.error("SSH connect to %s: %s", self.host, exc)
            self._connected = False
            self._publish(f"Connection failed: {exc}\r\n")
            return False

    def _read_loop(self) -> None:
        """Read shell output continuously and push to subscribers."""
        while self._running.is_set() and self._channel:
            try:
                if self._channel.recv_ready():
                    data = self._channel.recv(65536).decode("utf-8", errors="replace")
                    self._publish(data)
                elif self._channel.exit_status_ready():
                    break
                else:
                    time.sleep(0.05)
            except Exception as exc:
                logger.debug("SSH read error: %s", exc)
                break
        self._connected = False
        self._publish("\r\n[Connection closed]\r\n")

    def _publish(self, text: str) -> None:
        """Append to the replay buffer and push to all subscribers."""
        if not text:
            return
        with self._sub_lock:
            self._replay.append(text)
            self._replay_len += len(text)
            while self._replay_len > REPLAY_LIMIT and len(self._replay) > 1:
                self._replay_len -= len(self._replay.popleft())
            for sub in list(self._subs):
                try:
                    sub(text)
                except Exception:
                    pass

    def send(self, data: str) -> None:
        """Send user input to the remote shell."""
        if self._channel and self._connected:
            try:
                self._channel.send(data)
            except Exception as exc:
                logger.error("SSH send: %s", exc)

    def add_sub(self, fn: Callable[[str], None], replay: bool = True) -> None:
        """Register an output subscriber, replaying recent output to it first.

        The replay is what lets the UI open its SSE stream *after* the connect
        call returns and still see the login banner and the first prompt —
        without it the terminal looks blank until the operator types.
        """
        with self._sub_lock:
            if replay and self._replay:
                buffered = "".join(self._replay)
                try:
                    fn(buffered)
                except Exception:
                    pass
            self._subs.append(fn)

    def remove_sub(self, fn: Callable[[str], None]) -> None:
        """Unregister an output subscriber."""
        with self._sub_lock:
            try:
                self._subs.remove(fn)
            except ValueError:
                pass

    def resize(self, cols: int, rows: int) -> bool:
        """Tell the remote pty how large the operator's terminal is.

        Full-screen programs (``top``, ``vim``, ``less``) lay out against the
        pty size, so without this they draw against the 80x24 default and wrap
        into a mess in any other window.
        """
        cols = max(2, min(int(cols), 1000))
        rows = max(1, min(int(rows), 1000))
        self.cols, self.rows = cols, rows
        if not (self._channel and self._connected):
            return False
        try:
            self._channel.resize_pty(width=cols, height=rows)
            return True
        except Exception as exc:
            logger.debug("SSH resize: %s", exc)
            return False

    def disconnect(self) -> None:
        """Close the SSH session cleanly."""
        self._running.clear()
        if self._channel:
            try:
                self._channel.close()
            except Exception:
                pass
        if self._client:
            try:
                self._client.close()
            except Exception:
                pass
        self._connected = False
        if self._thread:
            self._thread.join(timeout=2)


class SshBridge:
    """Manages multiple SSH sessions keyed by name."""

    def __init__(self) -> None:
        self._sessions: dict[str, SshSession] = {}
        self._lock = threading.Lock()

    def connect(
        self,
        name: str,
        host: str,
        port: int = 22,
        username: str = "corvus",
        password: str | None = None,
        key_path: str | None = None,
    ) -> bool:
        """Open (or replace) a named SSH session.

        The blocking ``paramiko`` connect can take up to ~8s, so it runs
        OUTSIDE ``self._lock`` — otherwise the whole SSH API surface
        (list_sessions/disconnect/send) freezes for every session during a
        connect. The lock is only held for the brief dict mutation: disconnect
        the existing same-name session first, and insert the new one after a
        successful connect.
        """
        # Disconnect any existing session under the same name first (under the
        # lock, brief — only the reader-thread join, up to 2s). The entry stays
        # in the dict (now disconnected) so a failed reconnect matches the
        # prior behaviour: the name remains, just not connected.
        with self._lock:
            old = self._sessions.get(name)
            if old is not None:
                old.disconnect()
        # Build + connect OUTSIDE the lock so the SSH API stays responsive.
        session = SshSession(name, host, port, username, password, key_path)
        ok = session.connect()
        if not ok:
            return ok
        with self._lock:
            # A concurrent connect() under the same name may have replaced the
            # entry while we connected without the lock; disconnect that one so
            # only the latest successful session survives.
            sneaked = self._sessions.get(name)
            if sneaked is not None and sneaked is not session:
                sneaked.disconnect()
            self._sessions[name] = session
        return ok

    def disconnect(self, name: str) -> bool:
        """Close a named session."""
        with self._lock:
            session = self._sessions.pop(name, None)
        if session:
            session.disconnect()
            return True
        return False

    def send(self, name: str, data: str) -> bool:
        """Send input to a named session's shell."""
        with self._lock:
            session = self._sessions.get(name)
        if session and session.connected:
            session.send(data)
            return True
        return False

    def resize(self, name: str, cols: int, rows: int) -> bool:
        """Resize a named session's remote pty."""
        with self._lock:
            session = self._sessions.get(name)
        if session is None:
            return False
        return session.resize(cols, rows)

    def get_session(self, name: str) -> SshSession | None:
        """Return a named session if it exists."""
        with self._lock:
            return self._sessions.get(name)

    def list_sessions(self) -> list[dict[str, Any]]:
        """Return connection info for all sessions."""
        with self._lock:
            return [
                {
                    "name": s.name,
                    "host": s.host,
                    "port": s.port,
                    "username": s.username,
                    "connected": s.connected,
                }
                for s in self._sessions.values()
            ]

    def shutdown(self) -> None:
        """Disconnect all sessions."""
        with self._lock:
            sessions = list(self._sessions.values())
            self._sessions.clear()
        for s in sessions:
            s.disconnect()


# How much of a command's output is carried back. A one-shot launcher wants the
# first screenful of a failure, not a build log; anything past this is cut and
# marked, so a runaway program cannot fill the HTTP response.
OUTPUT_LIMIT = 64 * 1024


def _truncate(text: str) -> str:
    """Cut *text* to OUTPUT_LIMIT, marking it when something was dropped."""
    if len(text) <= OUTPUT_LIMIT:
        return text
    return text[:OUTPUT_LIMIT] + "\n… output truncated"


def run_command(
    host: str,
    command: str,
    port: int = 22,
    username: str = "corvus",
    password: str | None = None,
    key_path: str | None = None,
    timeout: float = 20.0,
) -> dict[str, Any]:
    """Run one command over its own SSH connection and return the result.

    Deliberately *not* a method on :class:`SshBridge`: the bridge's sessions are
    interactive shells whose output belongs to the terminal's subscribers, and
    a command run through one would have to be parsed back out of that stream.
    A short-lived connection of its own gives a real exit status and clean
    stdout/stderr, and leaves nothing behind — it is closed in a ``finally``
    whatever happens.

    *command* is executed by the remote login shell, so the caller composes the
    whole line (``cd … && …``) and owns its quoting. Returns
    ``{"ok", "exit_status", "stdout", "stderr", "error"}``; ``ok`` is true only
    when the command actually ran AND exited 0. Never raises.

    Lifecycle: this blocks its calling thread (an HTTP handler thread, which is
    a daemon) for at most the 8s connect plus *timeout*, and it owns no state
    past the call — there is nothing for the shutdown path to join or close,
    unlike a bridge session. A caller that wants a program to keep running
    composes it with ``nohup`` rather than holding this open.
    """
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    try:
        kwargs: dict[str, Any] = {
            "hostname": host,
            "port": port,
            "username": username,
            "timeout": 8,
        }
        if key_path:
            kwargs["key_filename"] = key_path
        elif password:
            kwargs["password"] = password
        client.connect(**kwargs)
        _stdin, stdout, stderr = client.exec_command(command, timeout=timeout)
        out = stdout.read().decode("utf-8", errors="replace")
        err = stderr.read().decode("utf-8", errors="replace")
        status = stdout.channel.recv_exit_status()
        return {
            "ok": status == 0,
            "exit_status": status,
            "stdout": _truncate(out),
            "stderr": _truncate(err),
            "error": "" if status == 0 else f"command exited with status {status}",
        }
    except Exception as exc:  # noqa: BLE001 - every failure is a message, not a 500
        logger.error("SSH run on %s: %s", host, exc)
        return {
            "ok": False,
            "exit_status": None,
            "stdout": "",
            "stderr": "",
            "error": str(exc) or exc.__class__.__name__,
        }
    finally:
        try:
            client.close()
        except Exception:
            pass
