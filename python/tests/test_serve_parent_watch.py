"""serve.py stops when the app that started it is gone (no stale instance lock)."""

import os
import sys
import threading
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import serve  # noqa: E402


def test_the_backend_stops_when_its_named_parent_is_not_its_parent(monkeypatch):
    if os.name != "posix":
        return
    monkeypatch.setenv("CORVUS_PARENT_PID", str(os.getppid() + 1))
    stop = threading.Event()
    serve._watch_parent(stop)
    assert stop.wait(5.0)


def test_the_backend_keeps_running_while_its_parent_lives(monkeypatch):
    monkeypatch.setenv("CORVUS_PARENT_PID", str(os.getppid()))
    stop = threading.Event()
    serve._watch_parent(stop)
    time.sleep(0.2)
    assert not stop.is_set()
    stop.set()


def test_without_a_parent_named_nothing_is_watched(monkeypatch):
    monkeypatch.delenv("CORVUS_PARENT_PID", raising=False)
    stop = threading.Event()
    before = threading.active_count()
    serve._watch_parent(stop)
    assert threading.active_count() == before
