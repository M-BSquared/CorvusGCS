"""The single-instance guard: one ground station per machine.

Two Corvus processes cannot share the serial link (POSIX lets both open the
same tty and gives each an arbitrary half of the MAVLink stream), the
forwarder's UDP port, or ``~/.corvus/config.json``. The lock is what stops
the second launch before any of that is opened.

The lock is an OS-held advisory lock on a file handle, so the interesting
assertions are cross-*process*: a second process must be refused, and a
holder that dies — however it dies — must leave nothing behind.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap

import pytest

from corvus.instance_lock import (
    ALLOW_MULTI_ENV,
    InstanceLock,
    allow_multi,
    default_lock_path,
    describe_peer,
)

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _holder_script(lock_path: str, port: int, hold_seconds: float) -> str:
    """Source for a child process that takes the lock and then waits."""
    return textwrap.dedent(f"""
        import sys, time
        sys.path.insert(0, {REPO_ROOT!r})
        from corvus.instance_lock import InstanceLock
        lock = InstanceLock({lock_path!r})
        print("held" if lock.acquire(port={port}) else "refused", flush=True)
        time.sleep({hold_seconds!r})
    """)


def _spawn_holder(lock_path: str, port: int = 8000,
                  hold_seconds: float = 30.0) -> subprocess.Popen:
    """Start a child holding the lock; return once it says it has it."""
    child = subprocess.Popen(
        [sys.executable, "-c", _holder_script(lock_path, port, hold_seconds)],
        stdout=subprocess.PIPE, text=True,
    )
    assert child.stdout is not None
    assert child.stdout.readline().strip() == "held"
    return child


def test_a_second_process_is_refused_while_the_first_holds_the_lock(
    tmp_path,
) -> None:
    """The whole point: launch two, only one gets in."""
    path = str(tmp_path / "corvus.lock")
    child = _spawn_holder(path)
    try:
        second = InstanceLock(path)
        assert second.acquire(port=8001) is False
        assert second.held is False
    finally:
        child.kill()
        child.wait()


def test_the_refused_process_learns_where_the_running_one_is(
    tmp_path,
) -> None:
    """Refusing is only useful with somewhere to send the operator."""
    path = str(tmp_path / "corvus.lock")
    child = _spawn_holder(path, port=8042)
    try:
        second = InstanceLock(path)
        second.acquire()
        peer = second.peer()
        assert peer["port"] == 8042
        assert peer["pid"] == child.pid
        assert "8042" in describe_peer(peer)
    finally:
        child.kill()
        child.wait()


def test_a_killed_holder_leaves_no_stale_lock(
    tmp_path,
) -> None:
    """SIGKILL, a crash and a power cut all look the same to the next launch.

    This is why the guard is an OS lock and not a pid file: the kernel drops
    it when the process dies, so there is no stale state to detect or clean
    up, and a crashed Corvus never locks the operator out of restarting.
    """
    path = str(tmp_path / "corvus.lock")
    child = _spawn_holder(path)
    child.kill()
    child.wait()

    after = InstanceLock(path)
    assert after.acquire(port=8000) is True
    after.release()


def test_releasing_hands_the_lock_to_the_next_instance(
    tmp_path,
) -> None:
    """A clean quit must free the machine for the next launch immediately."""
    path = str(tmp_path / "corvus.lock")
    first = InstanceLock(path)
    assert first.acquire(port=8000) is True
    first.release()

    second = InstanceLock(path)
    assert second.acquire(port=8000) is True
    second.release()


def test_release_and_acquire_are_both_idempotent(
    tmp_path,
) -> None:
    """Teardown runs from several paths; none of them may raise."""
    path = str(tmp_path / "corvus.lock")
    lock = InstanceLock(path)
    assert lock.acquire(port=8000) is True
    assert lock.acquire(port=8000) is True   # still ours
    lock.release()
    lock.release()                            # no-op, no raise
    assert lock.held is False


def test_the_recorded_port_is_updated_after_the_real_bind(
    tmp_path,
) -> None:
    """The launch takes the lock before it knows its port, then writes it back.

    Without the second write the file would advertise ``null`` and the next
    launch could not tell the operator where to look.
    """
    path = str(tmp_path / "corvus.lock")
    lock = InstanceLock(path)
    lock.acquire(port=None)
    assert lock.peer()["port"] is None
    lock.write_port(8003)
    assert lock.peer()["port"] == 8003
    lock.release()


def test_an_unreadable_lock_file_reports_no_peer_instead_of_raising(
    tmp_path,
) -> None:
    """The record is a hint for a message, never something to fail over."""
    path = str(tmp_path / "corvus.lock")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("{ this is not json")
    assert InstanceLock(path).peer() == {}
    assert "port unknown" in describe_peer({})


def test_a_home_that_cannot_be_locked_does_not_block_the_launch(
    tmp_path,
) -> None:
    """A locking quirk must never cost the operator their ground station.

    Refusing to start because a home directory cannot hold the lock file
    would be a far worse failure than the race the lock guards against, so an
    unopenable lock path degrades to "allowed". Here ``.corvus`` is a regular
    file, so creating a lock *inside* it cannot work.
    """
    blocker = tmp_path / ".corvus"
    blocker.write_text("not a directory", encoding="utf-8")
    assert InstanceLock(str(blocker / "corvus.lock")).acquire(port=8000) is True


def test_a_path_the_os_will_not_even_look_at_is_survived(tmp_path) -> None:
    """A mangled HOME fails argument validation, not the syscall.

    ``acquire`` promises never to raise; a null byte in the path must reach
    the same "no enforcement, carry on" answer as any other unusable path.
    """
    unusable = str(tmp_path / "\x00bad" / "corvus.lock")
    assert InstanceLock(unusable).acquire(port=8000) is True


@pytest.mark.parametrize("value,expected", [
    ("1", True), ("true", True), ("TRUE", True), ("yes", True), ("on", True),
    ("0", False), ("", False), ("no", False), ("maybe", False),
])
def test_the_opt_out_reads_the_usual_spellings_of_yes(
    monkeypatch: pytest.MonkeyPatch, value: str, expected: bool,
) -> None:
    """An operator with two aircraft sets this; it should just work."""
    monkeypatch.setenv(ALLOW_MULTI_ENV, value)
    assert allow_multi() is expected


def test_the_opt_out_is_off_when_the_variable_is_absent(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Single instance is the default, not something to be configured into."""
    monkeypatch.delenv(ALLOW_MULTI_ENV, raising=False)
    assert allow_multi() is False


def test_the_default_lock_lives_beside_the_rest_of_the_state() -> None:
    """One directory for everything Corvus owns on the machine."""
    path = default_lock_path()
    assert path.endswith("corvus.lock")
    assert ".corvus" in path
