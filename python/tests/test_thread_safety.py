"""Concurrency invariants for the state that many threads share.

Corvus runs telemetry on the MAVLink receive thread, serves HTTP and SSE from
a thread per request, and drives the window from the Qt main thread. Three of
its shared structures were reachable from more than one of those without a
lock. Each test below reproduces the specific damage that caused.
"""
from __future__ import annotations

import json
import threading
from typing import Any

import pytest

from corvus.state_store import VehicleStateStore


# --- The state store's listener fan-out -------------------------------------

def test_one_failing_listener_does_not_rob_the_others(
    store: VehicleStateStore,
) -> None:
    """A wedged browser tab must not cost every other tab its telemetry.

    Listeners were called in a bare loop, so the first one to raise skipped
    every listener after it in the list.
    """
    seen: list[str] = []
    store.add_listener(lambda _snap: seen.append("before"))
    store.add_listener(lambda _snap: (_ for _ in ()).throw(RuntimeError("boom")))
    store.add_listener(lambda _snap: seen.append("after"))

    store.update(armed=True)

    assert seen == ["before", "after"]


def test_a_failing_listener_does_not_reach_the_receive_thread(
    store: VehicleStateStore,
) -> None:
    """A bug in the presentation layer must not take down the link.

    ``update`` is called on the MAVLink receive thread, whose loop treats an
    exception as a link error: it closes the connection and reconnects. An
    unguarded listener therefore let a rendering bug drop the aircraft link.
    """
    store.add_listener(lambda _snap: (_ for _ in ()).throw(ValueError("bad")))
    store.update(armed=True)          # must not raise
    store.merge_warning("low battery", "warning")
    store.clear_warnings()
    store.heartbeat()
    store.set_disconnected()
    assert store.get_snapshot()["connected"] is False


def test_concurrent_writers_and_readers_keep_the_snapshot_coherent(
    store: VehicleStateStore,
) -> None:
    """Every snapshot a reader sees must be one some writer actually wrote."""
    stop = threading.Event()
    errors: list[BaseException] = []

    def write(base: float) -> None:
        try:
            for i in range(400):
                store.update(roll=base + i, pitch=base + i)
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            errors.append(exc)

    def read() -> None:
        try:
            while not stop.is_set():
                snap = store.get_snapshot()
                assert snap["roll"] == snap["pitch"]
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            errors.append(exc)

    readers = [threading.Thread(target=read) for _ in range(3)]
    for thread in readers:
        thread.start()
    writers = [threading.Thread(target=write, args=(n * 1000.0,)) for n in range(4)]
    for thread in writers:
        thread.start()
    for thread in writers:
        thread.join()
    stop.set()
    for thread in readers:
        thread.join()

    assert errors == []


def test_warnings_survive_concurrent_merges(store: VehicleStateStore) -> None:
    """merge_warning is read-modify-write on the warnings list."""
    def merge(start: int) -> None:
        for i in range(50):
            store.merge_warning(f"warning-{start}-{i}", "warning")

    threads = [threading.Thread(target=merge, args=(n,)) for n in range(4)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    warnings = store.get_snapshot()["warnings"]
    assert len(warnings) == len({w["msg"] for w in warnings})
    assert json.loads(store.to_json())["warnings"] == warnings


# --- The MAVLink console subscriber list ------------------------------------

def _console_bridge() -> Any:
    """A MavlinkBridge built without opening anything, or skip the test."""
    pytest.importorskip("pymavlink")
    from corvus.mavlink_bridge import MavlinkBridge
    return MavlinkBridge(VehicleStateStore(), "udp:127.0.0.1:14550")


def test_a_subscriber_leaving_does_not_skip_the_one_behind_it() -> None:
    """The console list was the one subscriber list without a lock.

    It is appended to and removed from by HTTP handler threads as tabs open
    and close, and iterated by the receive thread on every STATUSTEXT.
    Removing an entry while that iteration is in flight shifts the list under
    it, and the subscriber that slides into the vacated slot is skipped — so
    one tab closing dropped a console line in a *different* tab.

    A removal from inside the fan-out is the deterministic form of the same
    thing: it is exactly the mid-iteration mutation the two threads produced
    at random. The fix — publish to a copy taken under the lock — covers both.
    """
    bridge = _console_bridge()
    seen: list[str] = []

    def leaves(_entry: dict[str, Any]) -> None:
        bridge.remove_console_sub(leaves)

    bridge.add_console_sub(leaves)
    bridge.add_console_sub(lambda entry: seen.append(entry["text"]))

    bridge._console_publish("STATUSTEXT", "mayday", "critical")

    assert seen == ["mayday"]


def test_console_lines_survive_tabs_opening_and_closing_underneath() -> None:
    """The same invariant under the concurrent churn that produced it.

    A stress check rather than a proof: the GIL makes the interleaving rare,
    so this passing is not on its own evidence the lock is there. It is here
    to catch a regression that widens the window — a fan-out that starts
    doing real work per subscriber, say.
    """
    bridge = _console_bridge()
    received: list[dict[str, Any]] = []
    bridge.add_console_sub(received.append)

    stop = threading.Event()

    def churn() -> None:
        while not stop.is_set():
            transient = lambda _entry: None
            bridge.add_console_sub(transient)
            bridge.remove_console_sub(transient)

    churner = threading.Thread(target=churn, daemon=True)
    churner.start()
    try:
        for i in range(500):
            bridge._console_publish("STATUSTEXT", f"line {i}", "info")
    finally:
        stop.set()
        churner.join(timeout=5)

    assert [entry["text"] for entry in received] == [f"line {i}" for i in range(500)]


def test_a_failing_console_subscriber_is_isolated() -> None:
    """Same contract as the state store: one stream cannot stop the rest."""
    bridge = _console_bridge()
    seen: list[str] = []
    bridge.add_console_sub(lambda entry: seen.append(entry["text"]))
    bridge.add_console_sub(lambda _e: (_ for _ in ()).throw(RuntimeError("boom")))
    bridge.add_console_sub(lambda entry: seen.append(entry["text"].upper()))

    bridge._console_publish("STATUSTEXT", "hello", "info")

    assert seen == ["hello", "HELLO"]


# --- The live operator config -----------------------------------------------

def _handler_on_config(tmp_path) -> Any:
    """A CorvusHandler class pointed at a throwaway config file.

    The handler methods under test only touch ``config``/``config_path`` and
    ``_send_json``, so a bare instance is enough — no socket, no request.
    """
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.config import CorvusConfig
    from corvus.server import CorvusHandler

    handler = CorvusHandler.__new__(CorvusHandler)
    handler.config = CorvusConfig()
    handler.config_path = str(tmp_path / "config.json")
    handler.ssh = None
    handler._send_json = lambda *_a, **_k: None       # type: ignore[method-assign]
    handler._ssh_connections_public = lambda: []      # type: ignore[method-assign]
    return handler


def test_one_config_endpoint_at_a_time_reaches_the_file(tmp_path) -> None:
    """The mutual exclusion the config lock exists to provide, asserted directly.

    Add scans the shared connection list then appends to it; remove rebuilds
    it from a filter. Two of those interleaving dropped an entry outright —
    remove rebuilt from a snapshot taken before the append, so the connection
    the operator had just saved was simply gone from the file. The property
    that rules that out is that no second writer can be inside its
    read-modify-write while the first one is, which is what this pins: with
    one endpoint held open inside its critical section, the other cannot get
    past the lock.
    """
    handler = _handler_on_config(tmp_path)
    from corvus import server as server_mod

    inside = threading.Event()
    release = threading.Event()
    real_save = server_mod.save_config
    first_call = threading.Lock()
    blocked_once: list[bool] = []

    def blocking_save(cfg: Any, path: Any = None) -> None:
        # Only the FIRST writer is held. If the second one gets this far it
        # must sail straight through, so that a second_finished that stays
        # clear can only mean it is parked on the config lock — and not that
        # this stub is holding it too.
        with first_call:
            hold = not blocked_once
            if hold:
                blocked_once.append(True)
        if hold:
            inside.set()
            release.wait(timeout=5)
        real_save(cfg, path)

    server_mod.save_config = blocking_save          # type: ignore[assignment]
    second_finished = threading.Event()
    try:
        holder = threading.Thread(target=lambda: handler._api_ssh_connections_upsert({
            "name": "first", "host": "10.0.0.1", "port": 22,
            "username": "pilot", "key_path": "", "password": "",
        }))
        holder.start()
        assert inside.wait(timeout=5), "the first writer never reached its save"

        def second() -> None:
            handler._api_ssh_connections_remove({"name": "first"})
            second_finished.set()

        contender = threading.Thread(target=second)
        contender.start()
        # The second writer must be parked on the lock, not racing the first.
        assert not second_finished.wait(timeout=0.5), (
            "a second config writer entered while the first held the lock"
        )
        release.set()
        holder.join(timeout=5)
        contender.join(timeout=5)
        assert second_finished.is_set()
    finally:
        release.set()
        server_mod.save_config = real_save          # type: ignore[assignment]


def test_saved_ssh_connections_survive_concurrent_adds_and_removes(tmp_path) -> None:
    """The same invariant under load, end to end.

    A stress check rather than a proof — the window between the filter and
    the rebind is a few bytecodes wide, so this passing is not on its own
    evidence the lock is there (see the test above for that). It is here to
    catch a regression that widens the window.
    """
    handler = _handler_on_config(tmp_path)
    from corvus.config import load_config

    names = [f"host-{i}" for i in range(30)]
    doomed = {f"gone-{i}" for i in range(30)}

    def add(subset: list[str]) -> None:
        for name in subset:
            handler._api_ssh_connections_upsert({
                "name": name, "host": "10.0.0.1", "port": 22,
                "username": "pilot", "key_path": "", "password": "",
            })

    def remove(subset: list[str]) -> None:
        for name in subset:
            handler._api_ssh_connections_remove({"name": name})

    add(sorted(doomed))
    threads = [
        threading.Thread(target=add, args=(names[:15],)),
        threading.Thread(target=add, args=(names[15:],)),
        threading.Thread(target=remove, args=(sorted(doomed),)),
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    saved = {entry["name"] for entry in load_config(handler.config_path).ssh_connections}
    assert set(names) <= saved
    assert not (doomed & saved)


def test_the_config_file_is_never_written_half_applied(tmp_path) -> None:
    """``_apply_config_partial`` assigns seventeen fields one at a time.

    A save running between the third and the fourth wrote a file that was
    part old config and part new. Reading the file back mid-storm must always
    yield one coherent generation, never a mixture of two.
    """
    handler = _handler_on_config(tmp_path)
    from corvus.config import load_config

    errors: list[BaseException] = []
    stop = threading.Event()

    def apply(theme: str, port: int) -> None:
        try:
            for _ in range(40):
                public, error = handler._apply_config_partial({
                    "theme": {"name": theme},
                    "http_port": port,
                    "tlog_dir": f"/tmp/{theme}",
                })
                assert error is None, error
                # The generation the writer produced must be internally
                # consistent even while another writer is running.
                assert public["theme"]["name"] == theme
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            errors.append(exc)

    def read() -> None:
        try:
            while not stop.is_set():
                cfg = load_config(handler.config_path)
                if not cfg.theme:
                    continue
                # tlog_dir is written from the same generation as theme, so a
                # file where they disagree is a torn write.
                assert cfg.tlog_dir.endswith(cfg.theme["name"]), (
                    f"torn config: theme={cfg.theme} tlog_dir={cfg.tlog_dir}"
                )
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            errors.append(exc)

    reader = threading.Thread(target=read)
    reader.start()
    writers = [
        threading.Thread(target=apply, args=("midnight", 8000)),
        threading.Thread(target=apply, args=("daylight", 8100)),
    ]
    for thread in writers:
        thread.start()
    for thread in writers:
        thread.join()
    stop.set()
    reader.join()

    assert errors == []
    assert json.loads(open(handler.config_path, encoding="utf-8").read())


def test_a_permanently_broken_listener_does_not_flood_the_log(
    store: VehicleStateStore, caplog: pytest.LogCaptureFixture,
) -> None:
    """A listener that fails once fails on every update, at ~30 Hz.

    Isolating the failure is only half the job: an unthrottled traceback per
    failure would cost more than the failure does, on the MAVLink receive
    thread, and would bury everything else in the log during a flight. One
    report per interval, carrying the count of what it stood in for.
    """
    store.add_listener(lambda _snap: (_ for _ in ()).throw(RuntimeError("boom")))

    with caplog.at_level("ERROR", logger="corvus.state_store"):
        for i in range(200):
            store.update(armed=bool(i % 2))       # IMMEDIATE_KEYS: never coalesced

    failures = [r for r in caplog.records if "state listener failed" in r.message]
    assert len(failures) == 1, f"expected one throttled report, got {len(failures)}"
    assert store.get_snapshot()["armed"] is not None
