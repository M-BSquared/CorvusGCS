"""The PX4 NuttShell (NSH) over SERIAL_CONTROL.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.

The shell has two readers, and whoever wrote to it last gets its answer:

* the MAVLink console (:meth:`ShellMixin.send_shell_command`), which wants
  whole lines and gets them reassembled, as console entries named ``SHELL``;
* a terminal (:meth:`ShellMixin.write_shell`, the NuttX Console plugin), which
  wants the bytes as the shell printed them: escape sequences, carriage
  returns, a prompt with no newline after it. ``top`` redraws its screen with
  escapes, and a line console can only show that as noise.

The shell itself is ``corvus_link.shell.ShellEngine``, in Rust; it reads the
limits below from here.
"""
from __future__ import annotations

import logging
import sys
import threading
from collections.abc import Callable
from typing import Any

from corvus_link import shell as _engine

logger = logging.getLogger("corvus.mavlink")

# The NSH shell reassembles 70-byte chunks into lines, so a "line" is only
# bounded by the vehicle sending a newline. `dd`-ing a binary to the console,
# or a firmware that streams without one, would otherwise grow the buffer for
# as long as the shell stays open — on the receive thread. At the cap the
# buffer is flushed as a line of its own, which keeps the output visible
# instead of silently discarding it.
SHELL_LINE_MAX_CHARS = 8192

# What a terminal attaching late (or reconnecting its stream) is handed of
# what the shell already printed: a screen of `top` and the commands before
# it. Trimmed at a line break, so a replay does not open on half an escape.
SHELL_BACKLOG_CHARS = 64 * 1024

# One write from a terminal: a pasted block, not a file. 70 bytes go per
# SERIAL_CONTROL, so this is about 60 packets on a link that also carries the
# telemetry.
SHELL_WRITE_MAX_CHARS = 4096


class ShellMixin:
    """Send lines to NSH and reassemble its answer into console lines."""

    # Kept addressable by the names the bridge used: the tests look at them.
    _shell_active = property(
        lambda self: self._shell_engine.active,
        lambda self, value: setattr(self._shell_engine, "active", value))
    _shell_raw = property(
        lambda self: self._shell_engine.raw,
        lambda self, value: setattr(self._shell_engine, "raw", value))
    _shell_buffer = property(
        lambda self: self._shell_engine.buffer,
        lambda self, value: setattr(self._shell_engine, "buffer", value))
    _shell_backlog = property(
        lambda self: self._shell_engine.backlog,
        lambda self, value: setattr(self._shell_engine, "backlog", value))

    def _init_shell_state(self) -> None:
        """Set up the shell state; called from the bridge's __init__."""
        self._shell_engine = _engine.ShellEngine(sys.modules[__name__])
        self._shell_lock = threading.RLock()

    # The NSH shell is the only MAVLink path to NSH builtins with no MAV_CMD
    # equivalent (``listener <topic>``, ``top``, ``free``, ``dmesg``). PX4
    # exposes it over SERIAL_CONTROL with device=SERIAL_CONTROL_DEV_SHELL, the
    # QGroundControl MAVLink Console mechanism, stable across PX4 v1.16 to
    # v1.18. RESPOND|EXCLUSIVE|MULTI keeps the shell open and streams every
    # 70-byte response chunk.

    def _reset_shell_state(self) -> None:
        """Clear all state associated with the current PX4 shell session."""
        self._shell_engine.reset(self)

    def _send_serial_control(
        self, conn: Any, flags: int, count: int, data: bytes,
    ) -> None:
        """Send SERIAL_CONTROL, using target extensions when available."""
        self._shell_engine.send_serial_control(self, conn, flags, count, data)

    def send_shell_command(self, text: str) -> bool:
        """Send ``text`` (plus a newline) to the PX4 NSH debug shell.

        True when the SERIAL_CONTROL went out, False on disconnect or send
        failure with the reason as the last command error. ArduPilot has no
        NSH and is refused with the reason.
        """
        return self._shell_engine.send_command(self, text)

    def write_shell(self, data: str) -> bool:
        """Type ``data`` into the PX4 NSH shell exactly as given.

        The terminal's half of the shell: nothing is appended and nothing is
        reset between writes, so a running ``top`` keeps its screen and a
        Ctrl-C reaches it. From here on the output goes, unchanged, to
        :meth:`add_shell_sub` subscribers instead of the console.
        """
        return self._shell_engine.write(self, data)

    def add_shell_sub(self, fn: Callable[[dict[str, Any]], None]) -> str:
        """Hear the terminal's shell output; returns what it printed so far.

        ``fn`` is called with ``{"text": ...}`` on the receive thread and must
        not block. A chunk arriving meanwhile is in the backlog or delivered
        to ``fn``, never both and never neither.
        """
        return self._shell_engine.add_sub(self, fn)

    def remove_shell_sub(self, fn: Callable[[dict[str, Any]], None]) -> None:
        """Stop delivering shell output to ``fn``. Unknown ``fn`` is a no-op."""
        self._shell_engine.remove_sub(self, fn)

    def stop_shell(self) -> None:
        """Release exclusive NSH shell mode and clear the line buffer.

        Sends SERIAL_CONTROL with flags=0 and count=0, the documented release.
        Best effort and idempotent; the terminal's backlog goes too.
        """
        self._shell_engine.stop(self)

    def _handle_serial_control(self, msg: Any) -> None:
        """Route an NSH-shell SERIAL_CONTROL reply to whoever owns the shell.

        Runs in the receive loop and never blocks or raises.
        """
        self._shell_engine.handle(self, msg)
