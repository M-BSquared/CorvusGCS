"""Race-free shutdown wait tests for serve.py.

The old serve.py main loop used ``signal.pause()`` inside a
``while not shutting_down.is_set():`` check. That has a classic race: if the
signal is delivered between the check and the ``pause()`` call, the handler
runs and sets the Event, but ``pause()`` then blocks forever waiting for the
*next* signal — which is why SIGTERM/SIGINT needed SIGKILL in the live SiK
Radio test (the serial device stayed held).

The fix replaces the loop with a single ``shutting_down.wait()``. These tests
verify the race-free mechanism deterministically, without sending real
signals or spawning subprocesses: they exercise ``threading.Event`` directly,
mimicking serve.py's signal handler (which only calls ``Event.set()``).
"""
from __future__ import annotations

import threading
import time


def test_wait_returns_when_event_set_before_wait() -> None:
    """The signal arrives BEFORE the main thread calls wait().

    With ``signal.pause()`` this is exactly the lost-signal race: the handler
    runs and sets the Event, but ``pause()`` has nothing to wake on and blocks
    forever. With ``Event.wait()`` an already-set event returns immediately,
    so the race is gone.
    """
    event = threading.Event()

    def shutdown_handler() -> None:
        # Mimic serve.py's signal handler, which only sets the event.
        event.set()

    worker = threading.Thread(target=shutdown_handler, name="signal-handler")
    worker.start()
    worker.join()

    start = time.monotonic()
    woke = event.wait(timeout=1.0)
    elapsed = time.monotonic() - start

    assert woke is True
    assert elapsed < 0.1, f"wait() took {elapsed:.3f}s — not prompt"


def test_wait_blocks_when_event_not_set() -> None:
    """wait() must actually block (return False on timeout) when the event is
    never set, proving it does not busy-loop."""
    event = threading.Event()

    start = time.monotonic()
    woke = event.wait(timeout=0.2)
    elapsed = time.monotonic() - start

    assert woke is False
    # Not `>= 0.2`. Windows' default timer granularity is ~15.6 ms, so a 200 ms
    # wait routinely measures as a hair under it (0.188 s in CI) — the wait
    # blocked correctly and the clock is simply coarse. The claim under test is
    # "it blocks rather than busy-looping", and a busy-loop or an early return
    # lands near zero, nowhere near this bound.
    assert elapsed >= 0.15, f"wait() returned after {elapsed:.3f}s; it did not block"


def test_wait_returns_immediately_after_set() -> None:
    """After set(), a subsequent wait() returns True immediately."""
    event = threading.Event()
    event.set()

    start = time.monotonic()
    woke = event.wait(timeout=1.0)
    elapsed = time.monotonic() - start

    assert woke is True
    assert elapsed < 0.01, f"wait() took {elapsed:.6f}s — not immediate"
