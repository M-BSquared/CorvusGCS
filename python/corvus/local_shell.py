"""Programs on this computer: an interactive shell in a terminal window, or one run in the background.

The Schwalby plugin (``plugins/schwalby``) can run a button's program over SSH
on a companion computer or here, on the ground station itself. Here means one
of two things, the same two as over SSH:

``LocalSession``
    A login shell on a pseudo-terminal, living in the SSH bridge's session
    registry beside the SSH sessions (:meth:`corvus.ssh_bridge.SshBridge.connect_local`).
    That is the whole point of it being a session: everything a terminal
    window already does with a session (the output stream and its replay,
    typed input, resizes, the disconnect button, a window of its own) works on
    it unchanged, under ``/api/ssh/*``, by its name.

``LocalRunner``
    A program started without a window. Its output is kept only as far as the
    first second's worth, so a program that fails at once can say why. Unlike
    the SSH launcher's background mode, which leaves a program running on the
    companion computer after Corvus exits, a program here is Corvus' own child
    and is stopped with it (``SIGTERM``, then ``SIGKILL``): the ground station
    must shut down leaving nothing behind.

The shell is started through a two-line trampoline in this same interpreter
that makes the pseudo-terminal its controlling terminal. Without that the shell
has no job control and Ctrl-C reaches nothing, and the only stdlib way to get it
otherwise is ``pty.fork()``, which forks a process full of threads.

A pseudo-terminal is POSIX only. On Windows the terminal is reported as
unavailable, with the reason, and the background mode still works. There the
program runs under ``cmd.exe``, and stopping it means stopping the whole
process tree (``taskkill /T``): ending ``cmd.exe`` alone would leave the
program it started running after Corvus has gone.

Neither the shell nor a background program inherits the variables a packaged
Corvus runs with (see :mod:`corvus.child_env`): they are the operator's
programs, not part of Corvus.

Who may ask: only a process on this computer (see ``CorvusHandler._local_only``).
Running a command here is no more than the operator can already do on their own
laptop, but it is not something a machine on the flight-line network may do,
even with ``CORVUS_BIND`` opening the rest of the API to it.

stdlib only.
"""
from __future__ import annotations

import codecs
import collections
import getpass
import json
import logging
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Mapping
from typing import Any

from .child_env import child_env
from .ssh_bridge import SshSession

logger = logging.getLogger("corvus.local")

try:
    import fcntl
    import select
    import struct
    import termios
    HAVE_PTY = hasattr(os, "openpty")
except ImportError:          # Windows
    HAVE_PTY = False

LOCAL_HOST = "this computer"
MAX_COMMAND = 8192
# How long a background start waits to see whether the program failed at once.
EARLY_EXIT_S = 1.0
# How much of a background program's output is kept for that answer.
OUTPUT_TAIL = 4096
TERM_TIMEOUT_S = 2.0

# Makes the pty the shell's controlling terminal, then becomes the shell. The
# trampoline itself needs the bundle's variables to start (it is Corvus' own
# interpreter); the shell gets the operator's, which travel in SHELL_ENV_VAR
# rather than on the command line, where every account could read them (ps).
SHELL_ENV_VAR = "CORVUS_SHELL_ENV"
_TRAMPOLINE = (
    "import fcntl,json,os,sys,termios\n"
    "fcntl.ioctl(0, termios.TIOCSCTTY, 0)\n"
    "env = json.loads(os.environ.pop('" + SHELL_ENV_VAR + "', '') or 'null')\n"
    "os.execvpe(sys.argv[1], sys.argv[1:], env if isinstance(env, dict) else os.environ)\n"
)


def default_shell() -> str:
    """The operator's login shell, or the system's."""
    if sys.platform == "win32":
        return os.environ.get("COMSPEC") or "cmd.exe"
    shell = os.environ.get("SHELL", "")
    if shell and os.path.isfile(shell) and os.access(shell, os.X_OK):
        return shell
    return shutil.which("bash") or "/bin/sh"


def shell_command(text: str, shell: str = "", platform: str = sys.platform) -> list[str] | str:
    """How the login shell is asked to run *text*, as ``Popen`` takes it.

    POSIX: ``<shell> -l -c <text>``, an argument list, so nothing is re-parsed.
    Windows: one command line, ``"<cmd>" /d /s /c "<text>"``. A list would go
    through ``subprocess.list2cmdline``, which escapes a quote inside *text*
    with a backslash, and cmd does not read backslashes: a command with a
    quoted path in it would arrive mangled. ``/s`` with the outer quotes is
    cmd's own rule for "run exactly what is between them".
    """
    shell = shell or default_shell()
    if platform == "win32":
        return f'"{shell}" /d /s /c "{text}"'
    return [shell, "-l", "-c", text]


def capabilities() -> dict[str, Any]:
    """What running things on this computer can do here."""
    return {
        "terminal": HAVE_PTY,
        "reason": "" if HAVE_PTY else (
            "A terminal on this computer needs a pseudo-terminal, which Windows "
            "does not offer here. Run the program in the background instead."),
        "shell": default_shell(),
        "host": LOCAL_HOST,
    }


def shell_env(env: Mapping[str, str] | None = None, platform: str = sys.platform) -> dict[str, str]:
    """The environment the terminal's shell starts with.

    The operator's own (:func:`corvus.child_env.child_env`), a terminal type
    xterm.js answers to, and a UTF-8 character set when the session named
    none. A program started from the Finder or the Dock on macOS gets no
    ``LANG`` at all, and a shell without one shows every umlaut as an escape;
    Terminal.app sets it for the same reason. ``LC_CTYPE`` alone, so messages
    stay in whatever language the system uses.
    """
    out = child_env(env)
    out["TERM"] = "xterm-256color"
    out["COLORTERM"] = "truecolor"
    if not any(out.get(k) for k in ("LC_ALL", "LC_CTYPE", "LANG")):
        out["LC_CTYPE"] = "UTF-8" if platform == "darwin" else "C.UTF-8"
    return out


def _no_window_flags() -> int:
    if sys.platform != "win32":
        return 0
    return (getattr(subprocess, "CREATE_NO_WINDOW", 0)
            | getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0))


def taskkill_command(pid: int, system_root: str | None = None) -> list[str]:
    """The command that ends *pid* and every process it started, on Windows.

    By its full path under the system folder: a bare ``taskkill`` is looked
    for in the current folder first, which is the operator's.
    """
    root = system_root if system_root is not None else os.environ.get("SystemRoot", "")
    exe = os.path.join(root, "System32", "taskkill.exe") if root else "taskkill"
    return [exe, "/F", "/T", "/PID", str(int(pid))]


def _stop_tree_windows(proc: subprocess.Popen) -> None:
    """End ``cmd.exe`` and what it started. Never raises.

    ``terminate()`` is ``TerminateProcess`` on ``cmd.exe`` alone, and the
    program the command line started is not its job: it would keep running
    after Corvus exits. ``taskkill /T`` walks the tree from the pid down, so it
    has to run while ``cmd.exe`` is still there to be walked from.
    """
    try:
        subprocess.run(
            taskkill_command(proc.pid), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, timeout=TERM_TIMEOUT_S * 2, check=False,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
    except (OSError, subprocess.SubprocessError):
        logger.debug("taskkill for %s failed", proc.pid, exc_info=True)
    try:
        proc.wait(TERM_TIMEOUT_S)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        proc.kill()
        proc.wait(TERM_TIMEOUT_S)
    except (OSError, subprocess.TimeoutExpired):
        logger.warning("local process %s did not exit", proc.pid)


def session_groups(sid: int) -> set[int]:
    """The process groups of every process still running in session *sid*. Never raises.

    Zombies are left out: they have ended, and waiting for them to be reaped
    would only hold up the shutdown. Read from ``/proc`` where there is one
    (Linux), from ``ps`` and ``getsid`` elsewhere (macOS, whose ``ps`` has no
    session column). Neither kernel hands a session's number to a new process
    while anything is still in that session, so a match is always one of ours.
    """
    groups: set[int] = set()
    if os.path.isfile("/proc/self/stat"):
        try:
            names = os.listdir("/proc")
        except OSError:
            names = []
        for name in names:
            if not name.isdigit():
                continue
            try:
                with open(f"/proc/{name}/stat", "rb") as fh:
                    # After the name in parentheses: state ppid pgrp session ...
                    fields = fh.read().rsplit(b")", 1)[1].split()
                if fields[0] != b"Z" and int(fields[3]) == sid:
                    groups.add(int(fields[2]))
            except (OSError, IndexError, ValueError):
                continue
        return groups
    try:
        listing = subprocess.run(
            ["/bin/ps", "-A", "-o", "pid=,pgid=,stat="], stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=TERM_TIMEOUT_S, check=False, env=child_env(),
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return groups
    for line in listing.splitlines():
        parts = line.split()
        if len(parts) < 3 or parts[2].startswith("Z"):
            continue
        try:
            if os.getsid(int(parts[0])) == sid:
                groups.add(int(parts[1]))
        except (OSError, ValueError):
            continue
    return groups


def _signal_groups(groups: set[int], sig: int) -> None:
    for group in groups:
        try:
            os.killpg(group, sig)
        except OSError:
            pass


def _stop_session(proc: subprocess.Popen, first: int, platform: str = sys.platform) -> None:
    """Signal everything in the session *proc* leads, then make sure it is gone. Never raises.

    The session, not only the process's own group: a shell with job control
    puts every background job in a group of its own, and whether a hangup is
    passed on to them is the shell's choice (bash does, dash does not). A job
    that ignores the hangup outlives any shell. Both would keep running after
    Corvus exits. What is still in the session after :data:`TERM_TIMEOUT_S` is
    killed. A program that left the session on purpose (``setsid``) is not
    Corvus' to stop, as it would not be a terminal window's.

    Asked even when *proc* has already ended: an ``exit`` typed in the shell
    leaves its background jobs running.
    """
    if platform == "win32":
        if proc.poll() is None:
            _stop_tree_windows(proc)
        return
    sid = proc.pid
    _signal_groups(session_groups(sid) | {sid}, first)
    # A stopped job acts on nothing but SIGKILL until it is continued.
    _signal_groups(session_groups(sid), signal.SIGCONT)
    deadline = time.monotonic() + TERM_TIMEOUT_S
    try:
        proc.wait(TERM_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        pass
    left = session_groups(sid)
    while left and time.monotonic() < deadline:
        time.sleep(0.05)
        left = session_groups(sid)
    if not left and proc.poll() is not None:
        return
    _signal_groups(left | {sid}, signal.SIGKILL)
    try:
        proc.wait(TERM_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        logger.warning("local process %s did not exit", proc.pid)


def clean_directory(directory: str) -> tuple[str, str]:
    """``(path, "")`` for a folder that exists, or ``("", why)``.

    Empty means home, and a relative folder is read from home, the way the
    login shell in a terminal reads it: ``mission`` is the same folder whether
    the button runs in a terminal or in the background, and never one relative
    to wherever Corvus happened to be started from.

    Given back in the system's own spelling: ``~/mission`` on Windows would
    otherwise come back as ``C:\\Users\\op/mission``.
    """
    text = str(directory or "").strip()
    home = os.path.expanduser("~")
    if not text:
        return home, ""
    path = os.path.normpath(os.path.join(home, os.path.expanduser(text)))
    if not os.path.isdir(path):
        return "", f"There is no folder {text} on this computer."
    return path, ""


class LocalSession(SshSession):
    """A login shell on this computer, on a pseudo-terminal.

    A subclass of :class:`SshSession` for its output side: the subscribers,
    the replay buffer and the lock that keeps a late subscriber from seeing a
    chunk twice are the SSH session's, unchanged. What differs is where the
    bytes come from and go to.
    """

    def __init__(self, name: str, directory: str = "", shell: str = "") -> None:
        try:
            user = getpass.getuser()
        except Exception:  # noqa: BLE001 - a name for the window header, nothing more
            user = ""
        super().__init__(name, LOCAL_HOST, 0, user)
        self._shell = shell or default_shell()
        self._directory = directory
        self._proc: subprocess.Popen | None = None
        self._master: int | None = None

    def connect(self) -> bool:
        """Start the shell and its reader thread."""
        if not HAVE_PTY:
            self.error = capabilities()["reason"]
            self._publish(self.error + "\r\n")
            return False
        cwd, problem = clean_directory(self._directory)
        if problem:
            self.error = problem
            return False
        master, slave = os.openpty()
        self._set_size(slave)
        env = dict(os.environ)
        env[SHELL_ENV_VAR] = json.dumps(shell_env())
        try:
            self._proc = subprocess.Popen(
                [sys.executable, "-c", _TRAMPOLINE, self._shell, "-l"],
                stdin=slave, stdout=slave, stderr=slave, cwd=cwd, env=env,
                start_new_session=True, close_fds=True,
            )
        except OSError as exc:
            os.close(master)
            os.close(slave)
            self.error = f"The shell could not be started: {exc.strerror or exc}"
            self._publish(self.error + "\r\n")
            return False
        os.close(slave)
        with self._io_lock:
            self._master = master
        self._connected = True
        self._running.set()
        self._thread = threading.Thread(
            target=self._read_loop, name=f"local-{self.name}", daemon=True)
        self._thread.start()
        self._publish(f"Shell on this computer ({os.path.basename(self._shell)})\r\n")
        return True

    def _read_loop(self) -> None:
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        master = self._master
        while self._running.is_set() and master is not None:
            try:
                ready, _, _ = select.select([master], [], [], 0.5)
                if not ready:
                    continue
                data = os.read(master, 65536)
            except (OSError, ValueError):
                break            # EIO: the shell and everything on its pty ended
            if not data:
                break
            self._publish(decoder.decode(data))
        self._close_transport()
        self._publish("\r\n[Shell ended]\r\n")

    def _close_transport(self) -> None:
        """Stop the shell and every job in its session, and let the pty go.

        The pty is closed by the reader thread, never under it: closed from
        another thread while the reader waits in ``select``, its number could be
        handed to the next socket this process opens, and the reader would
        then read that socket's data as shell output. Asked from elsewhere,
        this stops the shell and leaves the reader to notice within its
        half-second wait and close the pty on its way out.
        """
        self._running.clear()
        self._connected = False
        with self._io_lock:
            proc = self._proc
            self._proc = None
            reader = self._thread
            owns_pty = (reader is None or reader is threading.current_thread()
                        or not reader.is_alive())
            master = self._master if owns_pty else None
            if owns_pty:
                self._master = None
        if proc is not None:
            # SIGHUP first: what closing a terminal sends. Sent to every job
            # here, since not every shell passes it on.
            _stop_session(proc, signal.SIGHUP)
        if master is not None:
            try:
                os.close(master)
            except OSError:
                pass

    def _pty_copy(self, need_connected: bool = True) -> int | None:
        """A duplicate of the pty for one write or resize, or None when there is none.

        Taken under the lock, so it is the pty's own and not a number the
        reader may be closing: a write can take a moment when the shell is not
        reading, and by then the original number could name another file.
        """
        with self._io_lock:
            if self._master is None or (need_connected and not self._connected):
                return None
            try:
                return os.dup(self._master)
            except OSError:
                return None

    def send(self, data: str) -> None:
        fd = self._pty_copy()
        if fd is None:
            return
        payload = data.encode("utf-8")
        try:
            while payload:
                written = os.write(fd, payload)
                payload = payload[written:]
        except OSError as exc:
            logger.debug("local shell write: %s", exc)
        finally:
            os.close(fd)

    def _set_size(self, fd: int) -> None:
        try:
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, 0, 0))
        except (OSError, ValueError):
            pass

    def resize(self, cols: int, rows: int) -> bool:
        self.cols = max(2, min(int(cols), 1000))
        self.rows = max(1, min(int(rows), 1000))
        fd = self._pty_copy(need_connected=False)
        if fd is None:
            return False
        try:
            self._set_size(fd)      # the kernel tells the foreground program
        finally:
            os.close(fd)
        return True


class LocalRunner:
    """Programs started on this computer without a window, stopped with Corvus.

    Each gets a reader thread that keeps the tail of its output (so its pipe
    never fills and blocks it) and ends with it. All of them, and their
    threads, are ended by :meth:`shutdown`.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._running: dict[int, dict[str, Any]] = {}
        self._closed = False

    def run(self, command: str, directory: str = "") -> dict[str, Any]:
        """Start *command* in the login shell; say whether it failed at once.

        ``{"ok": True, "pid": n}`` while it runs, ``{"ok": bool, "exited": True,
        "code": n, "output": "..."}`` when it ended within :data:`EARLY_EXIT_S`,
        or ``{"ok": False, "error": "..."}`` when it could not start.
        """
        text = str(command or "").strip()
        if not text:
            return {"ok": False, "error": "There is no command to run."}
        if len(text) > MAX_COMMAND or "\x00" in text:
            return {"ok": False, "error": "That command is too long."}
        cwd, problem = clean_directory(directory)
        if problem:
            return {"ok": False, "error": problem}
        with self._lock:
            if self._closed:
                return {"ok": False, "error": "Corvus is shutting down."}
        self._reap()
        try:
            proc = subprocess.Popen(
                shell_command(text), cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, start_new_session=(sys.platform != "win32"),
                creationflags=_no_window_flags(), env=child_env(),
            )
        except OSError as exc:
            return {"ok": False, "error": f"It could not be started: {exc.strerror or exc}"}
        tail: collections.deque[bytes] = collections.deque()
        size = [0]

        def read() -> None:
            stream = proc.stdout
            while True:
                try:
                    chunk = stream.read1(4096) if hasattr(stream, "read1") else stream.read(4096)
                except (OSError, ValueError):
                    break
                if not chunk:
                    break
                tail.append(chunk)
                size[0] += len(chunk)
                while size[0] > OUTPUT_TAIL and len(tail) > 1:
                    size[0] -= len(tail.popleft())
            try:
                stream.close()
            except OSError:
                pass

        reader = threading.Thread(target=read, name=f"local-run-{proc.pid}", daemon=True)
        reader.start()
        entry = {"proc": proc, "reader": reader, "command": text, "started": time.time()}
        with self._lock:
            self._running[proc.pid] = entry
        try:
            code = proc.wait(EARLY_EXIT_S)
        except subprocess.TimeoutExpired:
            return {"ok": True, "pid": proc.pid}
        reader.join(TERM_TIMEOUT_S)
        with self._lock:
            self._running.pop(proc.pid, None)
        output = b"".join(tail).decode("utf-8", "replace")[-OUTPUT_TAIL:].strip()
        return {"ok": code == 0, "exited": True, "code": code, "output": output}

    def _reap(self) -> None:
        with self._lock:
            done = [pid for pid, e in self._running.items() if e["proc"].poll() is not None]
            entries = [self._running.pop(pid) for pid in done]
        for e in entries:
            e["reader"].join(0.5)

    def count(self) -> int:
        """How many programs started here are still running."""
        self._reap()
        with self._lock:
            return len(self._running)

    def shutdown(self) -> None:
        """Stop every program started here and join its reader. Never raises."""
        with self._lock:
            self._closed = True
            entries = list(self._running.values())
            self._running.clear()
        for e in entries:
            try:
                _stop_session(e["proc"], signal.SIGTERM)
            except Exception:  # noqa: BLE001 - teardown never raises
                logger.exception("stopping a local program failed")
        for e in entries:
            e["reader"].join(TERM_TIMEOUT_S)
