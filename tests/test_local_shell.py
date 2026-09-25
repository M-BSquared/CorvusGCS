"""Programs on this computer: the Local target of the Schwalby launcher.

Covers :mod:`corvus.local_shell` (a login shell on a pseudo-terminal, and the
background runner), the SSH bridge keeping a local shell among its sessions,
and the ``/api/local/*`` endpoints, including the rule that only this computer
may start anything on it. The lifecycle checks are real processes: a program
started here must be gone, with its children, once Corvus has shut down.
"""
from __future__ import annotations

import os
import sys
import threading
import time
from typing import Any

import pytest

from corvus import local_shell
from corvus.local_shell import LocalRunner, LocalSession, clean_directory, shell_command
from corvus.server import CorvusHandler, stop_backend
from corvus.ssh_bridge import SshBridge

posix_only = pytest.mark.skipif(sys.platform == "win32", reason="POSIX shell and signals")
needs_pty = pytest.mark.skipif(not local_shell.HAVE_PTY, reason="a pseudo-terminal is POSIX only")


def _wait(predicate, timeout: float = 8.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return bool(predicate())


def _gone(pid: int) -> bool:
    """Whether *pid* no longer runs (a zombie waiting for its parent counts as gone)."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    try:
        import subprocess
        state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                               capture_output=True, text=True, timeout=5).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return False
    return state == "" or state.startswith("Z")


# ---------------------------------------------------------------------------
# The command line and the folder
# ---------------------------------------------------------------------------

def test_posix_runs_the_command_through_a_login_shell():
    assert shell_command("./run.sh --fast", shell="/bin/zsh", platform="darwin") == [
        "/bin/zsh", "-l", "-c", "./run.sh --fast"]


def test_windows_hands_cmd_the_command_line_as_typed():
    """A list would be re-quoted with backslashes, which cmd does not read."""
    line = shell_command('"C:\\Program Files\\Tool\\tool.exe" --port 5', shell="C:\\Windows\\cmd.exe",
                         platform="win32")
    assert line == '"C:\\Windows\\cmd.exe" /d /s /c ""C:\\Program Files\\Tool\\tool.exe" --port 5"'


def test_an_empty_folder_is_home(tmp_path):
    assert clean_directory("") == (os.path.expanduser("~"), "")


def test_a_folder_in_home_is_written_with_a_tilde(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    (tmp_path / "mission").mkdir()
    assert clean_directory("~/mission") == (str(tmp_path / "mission"), "")


@pytest.mark.parametrize("typed", ["~/mission/", "~//mission", "~/./mission"])
def test_a_folder_comes_back_in_the_systems_own_spelling(tmp_path, monkeypatch, typed):
    """On Windows ``~/mission`` came back as ``C:\\Users\\op/mission``. The same
    missing normalisation shows here on every platform."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    (tmp_path / "mission").mkdir()
    assert clean_directory(typed) == (str(tmp_path / "mission"), "")


def test_a_missing_folder_says_so(tmp_path):
    path, why = clean_directory(str(tmp_path / "nope"))
    assert path == ""
    assert why == f"There is no folder {tmp_path / 'nope'} on this computer."


def test_a_relative_folder_is_read_from_home(tmp_path, monkeypatch):
    """As the login shell in a terminal reads it, never from wherever Corvus
    was started."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    (tmp_path / "mission").mkdir()
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)
    assert clean_directory("mission") == (str(tmp_path / "mission"), "")
    assert clean_directory(str(elsewhere)) == (str(elsewhere), "")


# ---------------------------------------------------------------------------
# LocalRunner: a program in the background
# ---------------------------------------------------------------------------

@pytest.fixture
def runner():
    r = LocalRunner()
    yield r
    r.shutdown()


def test_a_program_that_finishes_at_once_answers_with_its_output(runner, tmp_path):
    res = runner.run("echo corvus-run", str(tmp_path))
    assert res["ok"] is True and res["exited"] is True and res["code"] == 0
    assert "corvus-run" in res["output"]
    assert runner.count() == 0


def test_a_program_that_fails_at_once_says_how(runner, tmp_path):
    res = runner.run("exit 3", str(tmp_path))
    assert res == {"ok": False, "exited": True, "code": 3, "output": ""}


def test_nothing_starts_without_a_command_or_in_a_missing_folder(runner, tmp_path):
    assert runner.run("   ") == {"ok": False, "error": "There is no command to run."}
    assert runner.run("x" * (local_shell.MAX_COMMAND + 1)) == {
        "ok": False, "error": "That command is too long."}
    assert runner.run("echo x", str(tmp_path / "nope"))["error"].startswith("There is no folder")
    assert runner.count() == 0


@posix_only
def test_a_long_running_program_answers_with_its_pid_and_stops_with_corvus(tmp_path):
    runner = LocalRunner()
    marker = tmp_path / "child.pid"
    # A child of the shell as well: a program here is often a script that
    # starts others, and all of them go when Corvus does.
    res = runner.run(f"sleep 60 & echo $! > '{marker}'; wait", str(tmp_path))
    assert res["ok"] is True and isinstance(res["pid"], int)
    assert runner.count() == 1
    assert _wait(lambda: marker.exists() and marker.read_text().strip() != "")
    child = int(marker.read_text().strip())
    assert not _gone(res["pid"]) and not _gone(child)

    runner.shutdown()
    assert _wait(lambda: _gone(res["pid"])), "the shell outlived Corvus"
    assert _wait(lambda: _gone(child)), "a program the shell started outlived Corvus"
    assert runner.count() == 0
    assert runner.run("echo late") == {"ok": False, "error": "Corvus is shutting down."}


@posix_only
def test_a_program_that_ignores_sigterm_is_killed(tmp_path):
    runner = LocalRunner()
    res = runner.run("trap '' TERM; sleep 60", str(tmp_path))
    assert res["ok"] is True
    started = time.monotonic()
    runner.shutdown()
    assert _wait(lambda: _gone(res["pid"]))
    assert time.monotonic() - started < local_shell.TERM_TIMEOUT_S + 5


@posix_only
def test_a_program_in_a_group_of_its_own_still_stops_with_corvus(tmp_path):
    """Signalling the shell's group alone missed it: it had left that group."""
    runner = LocalRunner()
    marker = tmp_path / "child.pid"
    (tmp_path / "detach.py").write_text(
        "import os, sys, time\n"
        "os.setpgid(0, 0)\n"
        "open(sys.argv[1], 'w').write(str(os.getpid()))\n"
        "time.sleep(60)\n", encoding="utf-8")
    # Not the command alone: a shell execs that, and a session leader cannot
    # change its group.
    res = runner.run(f"'{sys.executable}' detach.py '{marker}'; echo done", str(tmp_path))
    assert res["ok"] is True
    assert _wait(lambda: marker.exists() and marker.read_text().strip() != "")
    child = int(marker.read_text().strip())
    runner.shutdown()
    assert _wait(lambda: _gone(child)), "a program in its own group outlived Corvus"


@posix_only
def test_the_output_kept_is_bounded(runner, tmp_path):
    res = runner.run("head -c 200000 /dev/zero | tr '\\0' 'a'; exit 1", str(tmp_path))
    assert res["exited"] is True and res["ok"] is False
    assert 0 < len(res["output"]) <= local_shell.OUTPUT_TAIL


# ---------------------------------------------------------------------------
# LocalSession: a shell in a terminal window
# ---------------------------------------------------------------------------

class _Screen:
    """Everything a session printed, as a terminal subscriber sees it."""

    def __init__(self) -> None:
        self.text = ""
        self._lock = threading.Lock()

    def __call__(self, chunk: str) -> None:
        with self._lock:
            self.text += chunk


@needs_pty
def test_a_local_shell_is_a_real_terminal(tmp_path):
    session = LocalSession("schwalby/t", str(tmp_path), shell="/bin/sh")
    screen = _Screen()
    session.add_sub(screen)
    try:
        assert session.connect() is True
        assert session.connected
        assert session.host == local_shell.LOCAL_HOST
        session.send("echo corvus-$((6*7)); pwd\n")
        assert _wait(lambda: "corvus-42" in screen.text), screen.text
        assert _wait(lambda: str(tmp_path.resolve()) in screen.text or str(tmp_path) in screen.text)
        assert session.resize(120, 40) is True
        session.send("stty size; tty\n")
        assert _wait(lambda: "40 120" in screen.text), screen.text
        assert "/dev/" in screen.text
    finally:
        session.disconnect()
    assert not session.connected


@needs_pty
def test_ctrl_c_reaches_the_program_in_the_foreground(tmp_path):
    """Job control: without a controlling terminal, Ctrl-C would reach nothing."""
    session = LocalSession("schwalby/c", str(tmp_path), shell="/bin/sh")
    screen = _Screen()
    session.add_sub(screen)
    try:
        assert session.connect()
        session.send("sleep 30; echo after-sleep-$?\n")
        time.sleep(0.5)
        session.send("\x03")
        session.send("echo still-here\n")
        assert _wait(lambda: "still-here" in screen.text, timeout=5), screen.text
    finally:
        session.disconnect()


def _shells() -> list[Any]:
    """Every shell a terminal may run, where this machine has it. dash is
    Ubuntu's /bin/sh and does not pass a hangup on to its jobs; macOS ships
    it too, so that case is not left to the Linux runner to find."""
    return [pytest.param(sh, id=os.path.basename(sh),
                         marks=pytest.mark.skipif(not os.path.isfile(sh), reason=f"no {sh} here"))
            for sh in ("/bin/sh", "/bin/dash", "/bin/bash", "/bin/zsh")]


def _job_in(session: LocalSession, marker: Any, job: str = "sleep 60") -> int:
    session.send(f"{job} & echo $! > '{marker}'\n")
    assert _wait(lambda: marker.exists() and marker.read_text().strip() != "")
    return int(marker.read_text().strip())


@needs_pty
@pytest.mark.parametrize("shell", _shells())
def test_closing_a_local_shell_ends_what_runs_in_it(tmp_path, shell):
    session = LocalSession("schwalby/k", str(tmp_path), shell=shell)
    assert session.connect()
    child = _job_in(session, tmp_path / "shell.pid")
    shell_pid = session._proc.pid
    session.disconnect()
    assert _wait(lambda: _gone(shell_pid)), "the shell outlived its window's session"
    assert _wait(lambda: _gone(child)), "a job in the shell outlived it"


@needs_pty
def test_closing_a_local_shell_ends_a_job_that_ignores_the_hangup(tmp_path):
    """Like ``nohup``: whatever the shell passes on, the job would outlive Corvus."""
    session = LocalSession("schwalby/n", str(tmp_path), shell="/bin/sh")
    assert session.connect()
    child = _job_in(session, tmp_path / "shell.pid", job="( trap '' HUP; exec sleep 60 )")
    started = time.monotonic()
    session.disconnect()
    assert _wait(lambda: _gone(child)), "a job that ignores SIGHUP outlived the shell"
    assert time.monotonic() - started < local_shell.TERM_TIMEOUT_S + 5


@needs_pty
def test_a_job_left_behind_by_exit_ends_with_the_window(tmp_path):
    """``exit`` ends the shell but not its background jobs. On Linux they keep
    the terminal and the window open; macOS revokes the terminal and the
    window ends by itself. Either way the job must not outlive it."""
    session = LocalSession("schwalby/e", str(tmp_path), shell="/bin/sh")
    assert session.connect()
    child = _job_in(session, tmp_path / "shell.pid")
    shell_pid = session._proc.pid
    session.send("exit\n")
    assert _wait(lambda: _gone(shell_pid))
    session.disconnect()
    assert _wait(lambda: _gone(child)), "the job outlived the window it was started in"


@needs_pty
def test_a_local_shell_in_a_missing_folder_says_why(tmp_path):
    session = LocalSession("schwalby/m", str(tmp_path / "nope"), shell="/bin/sh")
    assert session.connect() is False
    assert session.error.startswith("There is no folder")


@pytest.mark.skipif(local_shell.HAVE_PTY, reason="only where there is no pseudo-terminal")
def test_without_a_pseudo_terminal_the_shell_says_why():
    session = LocalSession("schwalby/w")
    assert session.connect() is False
    assert session.error == local_shell.capabilities()["reason"]


def test_capabilities_say_whether_there_is_a_terminal():
    caps = local_shell.capabilities()
    assert caps["terminal"] is local_shell.HAVE_PTY
    assert caps["host"] == "this computer"
    assert (caps["reason"] == "") is local_shell.HAVE_PTY
    assert caps["shell"]


@needs_pty
def test_the_bridge_keeps_a_local_shell_among_its_sessions(tmp_path):
    bridge = SshBridge()
    try:
        assert bridge.connect_local("schwalby/b", str(tmp_path)) is True
        listed = {s["name"]: s for s in bridge.list_sessions()}
        assert listed["schwalby/b"]["connected"] is True
        assert listed["schwalby/b"]["host"] == "this computer"
        screen = _Screen()
        bridge.get_session("schwalby/b").add_sub(screen)
        assert bridge.send("schwalby/b", "echo via-bridge\n") is True
        assert _wait(lambda: "via-bridge" in screen.text)
        # Replaced under its name, like an SSH session that reconnects.
        first = bridge.get_session("schwalby/b")
        assert bridge.connect_local("schwalby/b", str(tmp_path)) is True
        assert bridge.get_session("schwalby/b") is not first
        assert not first.connected
        assert bridge.disconnect("schwalby/b") is True
        assert bridge.send("schwalby/b", "x\n") is False
    finally:
        bridge.shutdown()


@needs_pty
def test_a_failed_local_shell_leaves_its_reason_on_the_bridge(tmp_path):
    bridge = SshBridge()
    try:
        assert bridge.connect_local("schwalby/f", str(tmp_path / "nope")) is False
        assert bridge.connect_error("schwalby/f").startswith("There is no folder")
    finally:
        bridge.shutdown()


@needs_pty
def test_shutting_the_bridge_down_ends_every_local_shell(tmp_path):
    bridge = SshBridge()
    assert bridge.connect_local("schwalby/1", str(tmp_path))
    assert bridge.connect_local("schwalby/2", str(tmp_path))
    pids = [bridge.get_session(n)._proc.pid for n in ("schwalby/1", "schwalby/2")]
    bridge.shutdown()
    assert all(_wait(lambda p=p: _gone(p)) for p in pids)
    assert bridge.connect_local("schwalby/3", str(tmp_path)) is False, "nothing new once it is closing"


# ---------------------------------------------------------------------------
# /api/local/*
# ---------------------------------------------------------------------------

class _BridgeSpy:
    def __init__(self, ok: bool = True, reason: str = "") -> None:
        self.calls: list[tuple[str, str]] = []
        self.ok = ok
        self.reason = reason

    def connect_local(self, name: str, directory: str = "") -> bool:
        self.calls.append((name, directory))
        return self.ok

    def connect_error(self, name: str) -> str:
        return self.reason


class _RunnerSpy:
    def __init__(self, answer: dict[str, Any] | None = None) -> None:
        self.calls: list[tuple[str, str]] = []
        self.answer = answer or {"ok": True, "pid": 4242}
        self.stopped = False

    def run(self, command: str, directory: str = "") -> dict[str, Any]:
        self.calls.append((command, directory))
        return dict(self.answer)

    def count(self) -> int:
        return 2

    def shutdown(self) -> None:
        self.stopped = True


def _handler(client: str = "127.0.0.1", **attrs: Any):
    handler = object.__new__(CorvusHandler)
    handler.client_address = (client, 50123)
    handler.ssh = None
    handler.local = None
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    for key, value in attrs.items():
        setattr(handler, key, value)
    return handler, responses


@pytest.mark.parametrize("client", ["10.0.0.5", "192.168.2.20", "fe80::1", "::ffff:10.0.0.5", ""])
def test_only_this_computer_may_start_a_program_here(client):
    """CORVUS_BIND can open the API to the flight line. Never this part of it."""
    spy, runner = _BridgeSpy(), _RunnerSpy()
    handler, responses = _handler(client, ssh=spy, local=runner)
    handler._api_local_connect({"name": "schwalby/a"})
    handler._api_local_run({"command": "./x"})
    assert [status for _, status in responses] == [403, 403]
    assert responses[0][0]["error"] == "Programs on this computer can only be started from this computer."
    assert spy.calls == [] and runner.calls == []


@pytest.mark.parametrize("client", ["127.0.0.1", "127.0.1.1", "::1", "::ffff:127.0.0.1"])
def test_this_computer_may(client):
    runner = _RunnerSpy()
    handler, responses = _handler(client, local=runner)
    handler._api_local_run({"command": "./x", "directory": "~/m"})
    assert responses[0][1] == 200
    assert runner.calls == [("./x", "~/m")]


def test_run_answers_with_the_runners_result_and_the_command():
    runner = _RunnerSpy({"ok": False, "exited": True, "code": 2, "output": "bad"})
    handler, responses = _handler(local=runner)
    handler._api_local_run({"command": "./x"})
    body, status = responses[0]
    assert status == 200
    assert body == {"ok": False, "exited": True, "code": 2, "output": "bad", "command": "./x"}


@pytest.mark.parametrize("payload", [{}, {"command": 5}, {"command": "x", "directory": 3}])
def test_run_rejects_a_bad_payload(payload):
    runner = _RunnerSpy()
    handler, responses = _handler(local=runner)
    handler._api_local_run(payload)
    assert responses[0][1] == 400
    assert runner.calls == []


def test_run_without_a_runner_is_unavailable():
    handler, responses = _handler()
    handler._api_local_run({"command": "./x"})
    assert responses[0][1] == 503


def test_connect_opens_a_named_shell():
    spy = _BridgeSpy()
    handler, responses = _handler(ssh=spy)
    handler._api_local_connect({"name": "schwalby/a", "directory": "~/m"})
    assert spy.calls == [("schwalby/a", "~/m")]
    assert responses[0] == ({"ok": True, "connected": True, "name": "schwalby/a"}, 200)


def test_a_failed_connect_says_why():
    spy = _BridgeSpy(ok=False, reason="There is no folder ~/m on this computer.")
    handler, responses = _handler(ssh=spy)
    handler._api_local_connect({"name": "schwalby/a", "directory": "~/m"})
    body, status = responses[0]
    assert status == 200
    assert body["ok"] is False and body["connected"] is False
    assert body["error"] == "There is no folder ~/m on this computer."


@pytest.mark.parametrize("payload", [{}, {"name": ""}, {"name": 3}, {"name": "x" * 129}])
def test_connect_rejects_a_bad_name(payload):
    spy = _BridgeSpy()
    handler, responses = _handler(ssh=spy)
    handler._api_local_connect(payload)
    assert responses[0][1] == 400
    assert spy.calls == []


def test_status_says_what_is_possible_here():
    handler, responses = _handler(local=_RunnerSpy())
    handler._api_local_status()
    body, status = responses[0]
    assert status == 200
    assert body["terminal"] is local_shell.HAVE_PTY
    assert body["background"] is True
    assert body["running"] == 2
    assert body["host"] == "this computer"


def test_status_without_a_runner():
    handler, responses = _handler()
    handler._api_local_status()
    assert responses[0][0]["background"] is False
    assert responses[0][0]["running"] == 0


def test_stop_backend_stops_the_programs_started_here():
    class _Server:
        pass

    server = _Server()
    runner = _RunnerSpy()
    server.local = runner  # type: ignore[attr-defined]
    stop_backend(server)
    assert runner.stopped is True


def test_the_local_routes_are_registered():
    assert CorvusHandler._GET_ROUTES["/api/local/status"] == "_api_local_status"
    assert CorvusHandler._POST_ROUTES["/api/local/connect"] == "_api_local_connect"
    assert CorvusHandler._POST_ROUTES["/api/local/run"] == "_api_local_run"


# ---------------------------------------------------------------------------
# What the operator's programs inherit, and how they are stopped on Windows
# ---------------------------------------------------------------------------

def test_the_shell_gets_a_utf8_character_set_when_the_session_named_none():
    mac = local_shell.shell_env({"HOME": "/Users/p"}, platform="darwin")
    assert mac["LC_CTYPE"] == "UTF-8" and "LANG" not in mac
    assert local_shell.shell_env({}, platform="linux")["LC_CTYPE"] == "C.UTF-8"
    assert mac["TERM"] == "xterm-256color" and mac["COLORTERM"] == "truecolor"


def test_the_shell_keeps_the_character_set_the_session_named():
    for key in ("LANG", "LC_ALL", "LC_CTYPE"):
        env = local_shell.shell_env({key: "de_DE.UTF-8"}, platform="darwin")
        assert env[key] == "de_DE.UTF-8"
        assert list(k for k in ("LANG", "LC_ALL", "LC_CTYPE") if k in env) == [key]


def test_windows_stops_the_whole_tree_by_the_system_taskkill():
    assert local_shell.taskkill_command(4242, system_root="C:\\Windows") == [
        os.path.join("C:\\Windows", "System32", "taskkill.exe"), "/F", "/T", "/PID", "4242"]
    assert local_shell.taskkill_command(7, system_root="")[0] == "taskkill"


class _FakeProc:
    def __init__(self, exits_after_taskkill: bool = True) -> None:
        self.pid = 4242
        self.alive = True
        self.exits = exits_after_taskkill
        self.killed = False

    def poll(self):
        return None if self.alive else 1

    def wait(self, timeout=None):
        if self.alive:
            raise local_shell.subprocess.TimeoutExpired("cmd", timeout)
        return 1

    def terminate(self):
        raise AssertionError("terminate() ends cmd.exe alone; the program it started lives on")

    def kill(self):
        self.killed = True
        self.alive = False


def test_stopping_on_windows_ends_the_tree_not_cmd_alone(monkeypatch):
    proc = _FakeProc()
    ran = []

    def fake_run(argv, **kwargs):
        ran.append(argv)
        proc.alive = False
        return local_shell.subprocess.CompletedProcess(argv, 0)

    monkeypatch.setattr(local_shell.subprocess, "run", fake_run)
    local_shell._stop_session(proc, 15, platform="win32")
    assert ran and ran[0][1:] == ["/F", "/T", "/PID", "4242"]
    assert not proc.killed


def test_stopping_on_windows_falls_back_to_kill_when_taskkill_cannot_run(monkeypatch):
    proc = _FakeProc()

    def missing(argv, **kwargs):
        raise FileNotFoundError(argv[0])

    monkeypatch.setattr(local_shell.subprocess, "run", missing)
    monkeypatch.setattr(local_shell, "TERM_TIMEOUT_S", 0.01)
    local_shell._stop_session(proc, 15, platform="win32")
    assert proc.killed


def _bundle_like_env(monkeypatch) -> str:
    """This process as a packaged build runs: the interpreter's own PYTHONHOME,
    and the app folder on PYTHONPATH, next to an entry of the operator's."""
    from corvus.child_env import app_root
    theirs = os.path.join(os.sep, "opt", "operator-tools")
    monkeypatch.setenv("PYTHONHOME", sys.base_prefix)
    monkeypatch.setenv("PYTHONPATH", os.pathsep.join([app_root(), theirs]))
    monkeypatch.setenv("QTWEBENGINE_CHROMIUM_FLAGS", "--no-sandbox")
    return theirs


@posix_only
def test_a_background_program_does_not_inherit_the_bundles_python(monkeypatch, runner, tmp_path):
    theirs = _bundle_like_env(monkeypatch)
    res = runner.run("printenv PYTHONHOME || echo no-home; printenv PYTHONPATH; "
                     "printenv QTWEBENGINE_CHROMIUM_FLAGS || echo no-flags", str(tmp_path))
    assert res["exited"] is True, res
    lines = res["output"].splitlines()
    assert lines == ["no-home", theirs, "no-flags"], res["output"]


@needs_pty
def test_the_terminal_shell_does_not_inherit_the_bundles_python(monkeypatch, tmp_path):
    theirs = _bundle_like_env(monkeypatch)
    session = LocalSession("schwalby/env", str(tmp_path), shell="/bin/sh")
    screen = _Screen()
    session.add_sub(screen)
    try:
        assert session.connect(), session.error
        session.send("printenv PYTHONHOME || echo no-home-$((1+1)); printenv PYTHONPATH; "
                     "printenv CORVUS_SHELL_ENV || echo no-carrier-$((2+2))\n")
        assert _wait(lambda: "no-home-2" in screen.text and "no-carrier-4" in screen.text), screen.text
        assert theirs in screen.text
    finally:
        session.disconnect()
