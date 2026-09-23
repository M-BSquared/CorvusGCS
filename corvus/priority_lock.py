"""The bridge's command lock: re-entrant, and abort commands go first."""
from __future__ import annotations

import threading
from typing import Any



class _PriorityLock:
    """A re-entrant lock that serves abort commands ahead of routine work.

    Every operator command is serialised through one lock, which is what keeps
    a command's send and its ACK from interleaving with another's. The cost of
    that is a queue, and a queue does not care what is in it: an operator
    reaching for RTL while a parameter upload is running waited behind every
    remaining write, because a plain lock hands off in whatever order threads
    happen to arrive. The one command that must not wait was the one waiting.

    So there are two tiers. While any abort is waiting or running, routine
    acquirers stand aside; an abort therefore waits for at most the single
    operation already in progress, never for the queue behind it.

    Standing aside has to be re-checked rather than decided once on the way
    in, and that is the whole subtlety here. A routine thread that has already
    passed the gate and parked on the mutex is committed — an abort arriving
    afterwards would join the queue *behind* it and every other parked thread,
    which is exactly the behaviour this class exists to prevent. So routine
    acquirers never park: they poll, re-reading the gate each time, and a
    routine thread that wins the mutex in the instant an abort registers hands
    it straight back.

    Re-entrant, because the command paths nest (``takeoff`` calls ``arm``,
    everything calls ``_send_command_and_wait``). A thread that already holds
    the lock bypasses the gate entirely: making it stand aside for a waiting
    abort would deadlock, since that abort is waiting for the lock this thread
    is holding. Depth is tracked per thread because ``RLock`` does not expose
    ownership.
    """

    # How long a routine acquirer parks on the mutex before re-reading the
    # gate. Short enough that an abort is never held up by much more than the
    # operation already running; long enough that waiting costs no real CPU.
    _POLL_S = 0.02

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._turnstile = threading.Condition()
        self._aborts_waiting = 0
        self._local = threading.local()

    def _acquire(self, priority: bool) -> bool:
        """Take the lock. Returns whether this call registered an abort.

        The return value matters for nesting: an abort nested inside an
        operation this thread already holds must not register a second time,
        or the release would decrement a count it never incremented.
        """
        if getattr(self._local, "depth", 0) > 0:
            # Already ours. Never consult the gate — anything waiting on it is
            # waiting for this thread.
            self._lock.acquire()
            self._local.depth += 1
            return False
        registered = False
        if priority:
            with self._turnstile:
                self._aborts_waiting += 1
            registered = True
            try:
                self._lock.acquire()
            except BaseException:
                self._unregister()
                raise
        else:
            while True:
                with self._turnstile:
                    while self._aborts_waiting:
                        self._turnstile.wait()
                if not self._lock.acquire(timeout=self._POLL_S):
                    continue
                with self._turnstile:
                    if not self._aborts_waiting:
                        break
                # An abort registered while we were taking the mutex. It is
                # already blocked on it, so give it back rather than run ahead.
                self._lock.release()
        self._local.depth = getattr(self._local, "depth", 0) + 1
        return registered

    def _release(self, registered: bool) -> None:
        self._local.depth = getattr(self._local, "depth", 1) - 1
        self._lock.release()
        if registered:
            self._unregister()

    def _unregister(self) -> None:
        with self._turnstile:
            self._aborts_waiting -= 1
            self._turnstile.notify_all()

    # Routine use keeps the plain ``with lock:`` spelling every existing call
    # site already uses, so only the abort paths had to change.
    def __enter__(self) -> _PriorityLock:
        self._acquire(priority=False)
        return self

    def __exit__(self, *exc: Any) -> None:
        self._release(registered=False)

    def priority(self) -> _PriorityLockAbort:
        """Context manager for a command that must not queue behind routine work."""
        return _PriorityLockAbort(self)

    def abort_waiting(self) -> bool:
        """Is an abort queued for the lock right now?

        For the one routine operation long enough to matter: a mission
        download over a radio holds the lock for as long as the vehicle takes
        to send every item. It asks this between items and gives way, so an
        RTL waits for one item rather than for the whole mission.
        """
        with self._turnstile:
            return self._aborts_waiting > 0


class _PriorityLockAbort:
    """The abort half of :class:`_PriorityLock`; see :meth:`_PriorityLock.priority`."""

    __slots__ = ("_owner", "_registered")

    def __init__(self, owner: _PriorityLock) -> None:
        self._owner = owner
        self._registered = False

    def __enter__(self) -> _PriorityLockAbort:
        self._registered = self._owner._acquire(priority=True)
        return self

    def __exit__(self, *exc: Any) -> None:
        self._owner._release(self._registered)
