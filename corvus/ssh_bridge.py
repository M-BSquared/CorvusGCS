"""SSH bridge — real remote shell access via paramiko.

Manages SSH connections to companion computers or other devices. Each
connection runs an interactive shell on a background thread; output is
streamed to subscribers. Commands are sent via ``exec``.
"""
from __future__ import annotations

import logging
import threading
import time
from typing import Any, Callable

import paramiko

logger = logging.getLogger("corvus.ssh")


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
        self._subs: list[Callable[[str], None]] = []
        self._connected = False

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
            self._channel = self._client.invoke_shell(term="xterm-256color")
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
        """Push received output to all subscribers."""
        for sub in self._subs:
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

    def add_sub(self, fn: Callable[[str], None]) -> None:
        """Register an output subscriber."""
        self._subs.append(fn)

    def remove_sub(self, fn: Callable[[str], None]) -> None:
        """Unregister an output subscriber."""
        try:
            self._subs.remove(fn)
        except ValueError:
            pass

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
        """Open (or replace) a named SSH session."""
        with self._lock:
            if name in self._sessions:
                self._sessions[name].disconnect()
            session = SshSession(name, host, port, username, password, key_path)
            ok = session.connect()
            if ok:
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
