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
import os
import threading
import time
from typing import Any, Callable

import paramiko

from .paths import corvus_path

logger = logging.getLogger("corvus.ssh")

# How much recent shell output a session keeps for replay to a subscriber that
# attaches after the fact. One screenful of a scrolling build log is a few KB;
# 256 KB is enough that reopening the SSH tab shows real history, small enough
# that an idle session's memory stays negligible.
REPLAY_LIMIT = 256 * 1024


# Host-key verification.
#
# Both connect paths used to hand paramiko a bare ``AutoAddPolicy`` and load no
# host keys at all, which is weaker than it looks: AutoAddPolicy only decides
# what to do about a host that is *not known*, and with nothing loaded every
# host is unknown forever. A companion computer whose key changed — the one
# signal that says someone else is answering on that address — looked exactly
# like the first connection of the day.
#
# So keys are loaded now: the operator's own ``~/.ssh/known_hosts`` read-only,
# plus Corvus' own store at ``~/.corvus/known_hosts`` that the auto-add policy
# writes back to. The field default stays trust-on-first-use, because the
# operator connecting to a fresh companion over a point-to-point link has no
# known_hosts entry and must not be stopped by one. What changes is the second
# connection: a key that does not match what was recorded raises
# ``BadHostKeyException`` and the connection is refused.
#
# CORVUS_SSH_HOST_KEYS=strict turns the first connection into a refusal too,
# for a shared network where pre-populating the file is the right answer.
HOST_KEY_POLICY_ENV = "CORVUS_SSH_HOST_KEYS"


def known_hosts_path() -> str:
    """Corvus' own known_hosts file — the one auto-accepted keys are saved to."""
    return corvus_path("known_hosts")


def strict_host_keys() -> bool:
    """True when the operator asked for no auto-accept at all."""
    return (os.environ.get(HOST_KEY_POLICY_ENV) or "").strip().lower() == "strict"


class _RememberAndWarnPolicy(paramiko.AutoAddPolicy):
    """Accept an unknown host key, but say so and write it down.

    The warning is the point: an auto-accepted key is a decision made on the
    operator's behalf, and the log is where it gets recorded.
    """

    def missing_host_key(self, client: Any, hostname: str, key: Any) -> None:
        fingerprint = ""
        try:
            fingerprint = key.get_fingerprint().hex()
        except Exception:  # noqa: BLE001 - the warning matters more than the hex
            pass
        logger.warning(
            "accepting a new SSH host key for %s (%s %s) without verification; "
            "it is remembered in %s, and a different key from this host will be "
            "refused from now on. Set %s=strict to refuse unknown hosts instead.",
            hostname, key.get_name(), fingerprint, known_hosts_path(),
            HOST_KEY_POLICY_ENV,
        )
        super().missing_host_key(client, hostname, key)


def _new_client() -> "paramiko.SSHClient":
    """An SSHClient with host keys loaded and the configured missing-key policy."""
    client = paramiko.SSHClient()
    try:
        client.load_system_host_keys()
    except Exception:  # noqa: BLE001 - an unreadable ~/.ssh must not block a connect
        logger.debug("could not load ~/.ssh/known_hosts", exc_info=True)
    path = known_hosts_path()
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
    except OSError:
        logger.debug("could not create %s", os.path.dirname(path), exc_info=True)
    try:
        # Sets _host_keys_filename before it reads, so a file that does not
        # exist yet still becomes the one auto-added keys are saved to.
        client.load_host_keys(path)
    except OSError:
        logger.debug("no Corvus known_hosts at %s yet", path)
    client.set_missing_host_key_policy(
        paramiko.RejectPolicy() if strict_host_keys() else _RememberAndWarnPolicy()
    )
    return client


def _host_key_hint(exc: Exception) -> str:
    """The operator-facing explanation for a host key that did not match."""
    return (
        f"{exc}\n"
        f"The key this host presented is not the one recorded for it. Either "
        f"the companion computer was reinstalled — remove its line from "
        f"{known_hosts_path()} and connect again — or something else is "
        f"answering on that address."
    )


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
        self._io_lock = threading.Lock()
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
            self._client = _new_client()
            kwargs: dict[str, Any] = {
                "hostname": self.host,
                "port": self.port,
                "username": self.username,
                "timeout": 8,
            }
            if self._key_path:
                kwargs["key_filename"] = self._key_path
                kwargs["allow_agent"] = False
                kwargs["look_for_keys"] = False
            elif self._password:
                kwargs["password"] = self._password
                kwargs["allow_agent"] = False
                kwargs["look_for_keys"] = False
            kwargs["banner_timeout"] = 8
            kwargs["auth_timeout"] = 8
            kwargs["channel_timeout"] = 8
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
        except paramiko.BadHostKeyException as exc:
            logger.error("SSH host key mismatch for %s: %s", self.host, exc)
            self._close_transport()
            self._publish("Connection refused: " + _host_key_hint(exc).replace("\n", "\r\n") + "\r\n")
            return False
        except Exception as exc:
            logger.error("SSH connect to %s: %s", self.host, exc)
            self._close_transport()
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
        self._close_transport()
        self._publish("\r\n[Connection closed]\r\n")

    def _close_transport(self) -> None:
        """Close and clear a partially or fully opened Paramiko transport."""
        self._running.clear()
        self._connected = False
        with self._io_lock:
            channel = self._channel
            client = self._client
            self._channel = None
            self._client = None
        if channel is not None:
            try:
                channel.close()
            except Exception:
                pass
        if client is not None:
            try:
                client.close()
            except Exception:
                pass

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
        with self._io_lock:
            channel = self._channel
            connected = self._connected
        if channel and connected:
            try:
                channel.send(data)
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
        with self._io_lock:
            channel = self._channel
            connected = self._connected
        if not (channel and connected):
            return False
        try:
            channel.resize_pty(width=cols, height=rows)
            return True
        except Exception as exc:
            logger.debug("SSH resize: %s", exc)
            return False

    def disconnect(self) -> None:
        """Close the SSH session cleanly."""
        self._close_transport()
        if self._thread and self._thread is not threading.current_thread():
            self._thread.join(timeout=2)


class SshBridge:
    """Manages multiple SSH sessions keyed by name."""

    def __init__(self) -> None:
        self._sessions: dict[str, SshSession] = {}
        self._lock = threading.Lock()
        self._attempts: dict[str, int] = {}
        self._shutting_down = False

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
            if self._shutting_down:
                return False
            attempt = self._attempts.get(name, 0) + 1
            self._attempts[name] = attempt
            old = self._sessions.get(name)
        if old is not None:
            old.disconnect()
        # Build + connect OUTSIDE the lock so the SSH API stays responsive.
        session = SshSession(name, host, port, username, password, key_path)
        ok = session.connect()
        if not ok:
            return ok
        with self._lock:
            if self._shutting_down or self._attempts.get(name) != attempt:
                stale = True
                sneaked = None
            else:
                stale = False
                sneaked = self._sessions.get(name)
                self._sessions[name] = session
        if stale:
            session.disconnect()
            return False
        # A concurrent connect() under the same name may have replaced the
        # entry while we connected without the lock; disconnect that one so
        # only the latest successful session survives.
        if sneaked is not None and sneaked is not session:
            sneaked.disconnect()
        return ok

    def disconnect(self, name: str) -> bool:
        """Close a named session."""
        with self._lock:
            self._attempts[name] = self._attempts.get(name, 0) + 1
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
            self._shutting_down = True
            for name in list(self._attempts):
                self._attempts[name] += 1
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


def _drain_command_output(
    stdout: Any, stderr: Any, timeout: float,
) -> tuple[str, str, int]:
    """Drain an exec channel to a bounded buffer before the deadline."""
    channel = stdout.channel
    deadline = time.monotonic() + timeout
    out = bytearray()
    err = bytearray()
    out_truncated = False
    err_truncated = False

    def append_limited(target: bytearray, data: bytes) -> bool:
        room = max(0, OUTPUT_LIMIT - len(target))
        target.extend(data[:room])
        return len(data) > room

    while True:
        progressed = False
        while channel.recv_ready():
            chunk = channel.recv(32768)
            if not chunk:
                break
            progressed = True
            out_truncated = append_limited(out, chunk) or out_truncated
        while channel.recv_stderr_ready():
            chunk = channel.recv_stderr(32768)
            if not chunk:
                break
            progressed = True
            err_truncated = append_limited(err, chunk) or err_truncated
        if (
            channel.exit_status_ready()
            and not channel.recv_ready()
            and not channel.recv_stderr_ready()
        ):
            break
        if time.monotonic() >= deadline:
            try:
                channel.close()
            except Exception:
                pass
            raise TimeoutError(f"remote command exceeded {timeout:g}s timeout")
        if not progressed:
            time.sleep(0.02)

    status = channel.recv_exit_status()
    out_text = out.decode("utf-8", errors="replace")
    err_text = err.decode("utf-8", errors="replace")
    if out_truncated:
        out_text += "\n… output truncated"
    if err_truncated:
        err_text += "\n… output truncated"
    return out_text, err_text, status


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
    client = _new_client()
    try:
        try:
            timeout_s = float(timeout)
        except (TypeError, ValueError):
            timeout_s = 20.0
        if timeout_s != timeout_s or timeout_s <= 0:
            timeout_s = 20.0
        kwargs: dict[str, Any] = {
            "hostname": host,
            "port": port,
            "username": username,
            "timeout": 8,
            "banner_timeout": 8,
            "auth_timeout": 8,
            "channel_timeout": 8,
        }
        if key_path:
            kwargs["key_filename"] = key_path
            kwargs["allow_agent"] = False
            kwargs["look_for_keys"] = False
        elif password:
            kwargs["password"] = password
            kwargs["allow_agent"] = False
            kwargs["look_for_keys"] = False
        client.connect(**kwargs)
        _stdin, stdout, stderr = client.exec_command(command, timeout=timeout_s)
        out, err, status = _drain_command_output(stdout, stderr, timeout_s)
        return {
            "ok": status == 0,
            "exit_status": status,
            "stdout": _truncate(out),
            "stderr": _truncate(err),
            "error": "" if status == 0 else f"command exited with status {status}",
        }
    except paramiko.BadHostKeyException as exc:
        logger.error("SSH host key mismatch for %s: %s", host, exc)
        return {
            "ok": False,
            "exit_status": None,
            "stdout": "",
            "stderr": "",
            "error": _host_key_hint(exc),
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
