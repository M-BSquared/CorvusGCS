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
"""
from __future__ import annotations

import codecs
import logging
import threading
from collections.abc import Callable
from typing import Any

from pymavlink import mavutil

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

    def _init_shell_state(self) -> None:
        """Set up the shell state; called from the bridge's __init__."""
        # NSH debug-shell state and UTF-8-safe line reassembly.
        self._shell_lock = threading.RLock()
        self._shell_buffer: str = ""
        self._shell_decoder: Any = codecs.getincrementaldecoder("utf-8")(
            errors="replace",
        )
        self._shell_active: bool = False
        # True while a terminal owns the shell (write_shell); its output then
        # goes, unchanged, to the subscribers and the backlog instead of the
        # console.
        self._shell_raw: bool = False
        self._shell_backlog: str = ""
        self._shell_subs: list[Callable[[dict[str, Any]], None]] = []

    # ------------------------------------------------------------------
    # PX4 NuttShell (NSH) debug shell — SERIAL_CONTROL device=SHELL
    # ------------------------------------------------------------------
    #
    # The NSH shell is the only MAVLink path to NSH builtins with no MAV_CMD
    # equivalent — ``listener <topic>``, ``top``, ``free``, ``dmesg``. PX4
    # exposes it over SERIAL_CONTROL with device=SERIAL_CONTROL_DEV_SHELL;
    # this is the standard QGC MAVLink Console mechanism, stable across PX4
    # v1.16, v1.17, and v1.18. The debug shell must be enabled on the
    # autopilot: it is on by default for SITL; on hardware the MAVLink
    # instance must permit shell access (MAV_ADVANCED_PARAMS / instance
    # config). RESPOND|EXCLUSIVE|MULTI is the QGroundControl framing: PX4
    # keeps the shell open and streams every available 70-byte response chunk.

    def _reset_shell_state(self) -> None:
        """Clear all state associated with the current PX4 shell session."""
        with self._shell_lock:
            self._shell_active = False
            self._shell_buffer = ""
            self._shell_decoder.reset()

    def _send_serial_control(
        self, conn: Any, flags: int, count: int, data: bytes,
    ) -> None:
        """Send SERIAL_CONTROL, using target extensions when available."""
        args: tuple[Any, ...] = (
            mavutil.mavlink.SERIAL_CONTROL_DEV_SHELL,
            flags,
            0,
            0,
            count,
            data,
        )
        fields = getattr(
            mavutil.mavlink.MAVLink_serial_control_message,
            "fieldnames",
            (),
        )
        if "target_system" in fields and "target_component" in fields:
            args += (self._target_system, self._target_component)
        conn.mav.serial_control_send(*args)

    def send_shell_command(self, text: str) -> bool:
        """Send ``text`` (plus a newline) to the PX4 NSH debug shell.

        Sends one or more ``SERIAL_CONTROL`` packets with ``device=SHELL`` and
        ``flags=RESPOND|EXCLUSIVE|MULTI`` so PX4 streams the response back.
        Returns
        ``True`` if the SERIAL_CONTROL was sent, ``False`` on disconnect
        or send failure (mirrors the ``-2``/``False`` convention of the
        other command methods).
        """
        # Clear any stale thread-local error so the caller sees a fresh result
        # (BUG 10). The success path leaves it empty; the failure paths set it.
        self._set_command_error("")
        if not isinstance(text, str):
            self._set_command_error("shell command must be text")
            return False
        if not self._dialect.supports_shell:
            # ArduPilot has no NSH. SERIAL_CONTROL exists on its wire, but it
            # is a passthrough to a *peripheral* port rather than a console, so
            # sending a command here would open a terminal that can never
            # answer — and on a board with something attached to that port, it
            # would take the port away from whatever is using it.
            self._set_command_error(
                f"{self._dialect.label} has no MAVLink shell. The console is "
                f"a PX4 feature (NSH over SERIAL_CONTROL)"
            )
            return False
        with self._send_lock:
            conn = self._conn
            # A6: bail if stop()/reconnect swapped the connection, or the
            # link isn't healthy enough to dispatch an operator command.
            if conn is not self._conn or not self._connection_ready():
                return self._command_failure("Shell", -2)
            payload = (text if text.endswith("\n") else text + "\n").encode(
                "utf-8", errors="replace",
            )
            flags = (
                mavutil.mavlink.SERIAL_CONTROL_FLAG_RESPOND
                | mavutil.mavlink.SERIAL_CONTROL_FLAG_EXCLUSIVE
                | mavutil.mavlink.SERIAL_CONTROL_FLAG_MULTI
            )
            with self._shell_lock:
                self._reset_shell_state()
                self._shell_active = True
                self._shell_raw = False
                try:
                    for offset in range(0, len(payload), 70):
                        chunk = payload[offset:offset + 70]
                        self._send_serial_control(
                            conn, flags, len(chunk), chunk.ljust(70, b"\x00"),
                        )
                except Exception as exc:
                    self._reset_shell_state()
                    logger.error("shell command send failed: %s", exc)
                    # Surface the real cause instead of a stale/generic error (BUG 10).
                    self._set_command_error(f"shell send failed: {exc}")
                    return False
            return True

    def write_shell(self, data: str) -> bool:
        """Type ``data`` into the PX4 NSH shell exactly as given.

        The terminal's half of the shell: nothing is appended (a line ends
        with the newline the terminal sends) and nothing is reset between
        writes, so a ``top`` that is running keeps its screen and a Ctrl-C
        (``\\x03``) reaches it. From here on the output goes, unchanged, to
        :meth:`add_shell_sub` subscribers instead of the console. The first
        write opens the shell on the autopilot; a newline alone is how a
        terminal asks for a prompt.

        Returns ``False`` with :meth:`get_last_command_error` set when the
        stack has no shell, nothing is connected, or the send failed.
        """
        self._set_command_error("")
        if not isinstance(data, str) or not data:
            self._set_command_error("nothing to send to the shell")
            return False
        if len(data) > SHELL_WRITE_MAX_CHARS:
            self._set_command_error(
                f"too much at once for the shell ({len(data)} characters, at most "
                f"{SHELL_WRITE_MAX_CHARS})"
            )
            return False
        if not self._dialect.supports_shell:
            self._set_command_error(
                f"{self._dialect.label} has no NuttX shell. It is a PX4 feature "
                f"(NSH over SERIAL_CONTROL)"
            )
            return False
        with self._send_lock:
            conn = self._conn
            if conn is None or not self._connection_ready():
                self._set_command_error("not connected")
                return False
            payload = data.encode("utf-8", errors="replace")
            flags = (
                mavutil.mavlink.SERIAL_CONTROL_FLAG_RESPOND
                | mavutil.mavlink.SERIAL_CONTROL_FLAG_EXCLUSIVE
                | mavutil.mavlink.SERIAL_CONTROL_FLAG_MULTI
            )
            with self._shell_lock:
                if not self._shell_raw:
                    # Taken over from the console: the half line it was
                    # assembling belongs to its command, not to this terminal.
                    self._shell_buffer = ""
                    self._shell_decoder.reset()
                self._shell_raw = True
                self._shell_active = True
                try:
                    for offset in range(0, len(payload), 70):
                        chunk = payload[offset:offset + 70]
                        self._send_serial_control(
                            conn, flags, len(chunk), chunk.ljust(70, b"\x00"),
                        )
                except Exception as exc:
                    logger.error("shell write failed: %s", exc)
                    self._set_command_error(f"shell send failed: {exc}")
                    return False
            return True

    def add_shell_sub(self, fn: Callable[[dict[str, Any]], None]) -> str:
        """Hear the terminal's shell output; returns what it printed so far.

        ``fn`` is called with ``{"text": ...}`` on the receive thread and must
        not block. The backlog is read under the same lock the receive thread
        takes to extend it, so a chunk arriving meanwhile is in the backlog or
        delivered to ``fn``, never both and never neither.
        """
        with self._shell_lock:
            self._shell_subs.append(fn)
            return self._shell_backlog

    def remove_shell_sub(self, fn: Callable[[dict[str, Any]], None]) -> None:
        """Stop delivering shell output to ``fn``. Unknown ``fn`` is a no-op."""
        with self._shell_lock:
            try:
                self._shell_subs.remove(fn)
            except ValueError:
                pass

    def _append_shell_backlog(self, text: str) -> None:
        """Keep the newest :data:`SHELL_BACKLOG_CHARS`; call with the lock held."""
        backlog = self._shell_backlog + text
        if len(backlog) > SHELL_BACKLOG_CHARS:
            cut = len(backlog) - SHELL_BACKLOG_CHARS
            newline = backlog.find("\n", cut)
            backlog = backlog[newline + 1:] if newline != -1 else backlog[cut:]
        self._shell_backlog = backlog

    def stop_shell(self) -> None:
        """Release exclusive NSH shell mode and clear the line buffer.

        Sends a ``SERIAL_CONTROL`` with ``flags=0`` and ``count=0`` (the
        documented release) so the autopilot stops streaming shell output
        and frees the shell for other clients. Best-effort and idempotent:
        swallows exceptions and is a safe no-op when not connected. Called
        from :meth:`stop` so exclusive mode is released on shutdown, and by
        a terminal that closes. Either way the terminal's backlog goes too:
        the shell it showed has ended.
        """
        with self._send_lock:
            conn = self._conn
            self._reset_shell_state()
            with self._shell_lock:
                self._shell_raw = False
                self._shell_backlog = ""
            # A6: only release on the connection we snapshotted; no-op if
            # the link was already torn down (best-effort release).
            if conn is None or conn is not self._conn:
                return
            try:
                self._send_serial_control(conn, 0, 0, b"\x00" * 70)
            except Exception:
                pass

    def _handle_serial_control(self, msg: Any) -> None:
        """Route an NSH-shell SERIAL_CONTROL reply to whoever owns the shell.

        PX4 streams the shell response in 70-byte SERIAL_CONTROL chunks, each
        decoded incrementally (a character split across two chunks survives).
        A terminal (:meth:`write_shell`) gets the text as it came. For the
        console it is sanitized (CR stripped, backspace applied), appended to
        the line buffer, and complete lines are published to the console
        subscribers. Runs in the receive loop and never blocks.
        """
        try:
            if int(getattr(msg, "device", -1)) != mavutil.mavlink.SERIAL_CONTROL_DEV_SHELL:
                return
            if not self._shell_active or not self._ack_is_for_us(msg):
                return
            lines: list[str] = []
            raw_subs: list[Callable[[dict[str, Any]], None]] = []
            with self._shell_lock:
                if not self._shell_active:
                    return
                count = int(getattr(msg, "count", 0))
                if count <= 0 or count > 70:
                    return
                raw_data = bytes(msg.data)
                if count > len(raw_data):
                    return
                text = self._shell_decoder.decode(raw_data[:count], final=False)
                if self._shell_raw:
                    if text:
                        self._append_shell_backlog(text)
                        raw_subs = list(self._shell_subs)
                else:
                    # Sanitize: drop CR (NSH sends \r\n); apply BS by removing
                    # the previous char — in this chunk, or the buffer tail if a
                    # backspace crosses a chunk boundary (NSH line-edit echo).
                    chunk: list[str] = []
                    for ch in text:
                        if ch == "\r":
                            continue
                        if ch == "\b":
                            if chunk:
                                chunk.pop()
                            elif self._shell_buffer:
                                self._shell_buffer = self._shell_buffer[:-1]
                            continue
                        chunk.append(ch)
                    self._shell_buffer += "".join(chunk)
                    # Publish each complete line; keep the trailing partial line.
                    while "\n" in self._shell_buffer:
                        line, self._shell_buffer = self._shell_buffer.split("\n", 1)
                        if line:
                            lines.append(line)
                    # No newline in sight and the buffer has run long: flush
                    # what is there rather than keep growing it.
                    if len(self._shell_buffer) >= SHELL_LINE_MAX_CHARS:
                        lines.append(self._shell_buffer)
                        self._shell_buffer = ""
            # Published outside the lock: subscribers are SSE buffers, and a
            # slow one must not hold up the next chunk.
            if raw_subs:
                entry = {"text": text}
                for sub in raw_subs:
                    try:
                        sub(entry)
                    except Exception:  # noqa: BLE001 - one subscriber must not cost the others
                        pass
            for line in lines:
                self._console_publish("SHELL", line, "info")
        except Exception as exc:
            logger.debug("SERIAL_CONTROL handling failed: %s", exc)
