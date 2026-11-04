"""The Rust auto-connect against the Python one it replaced.

``tests/reference/autoconnect_python.py`` keeps the auto-connect module as it
was in Python. The tests compare the pure policy (port classification, the
pick, the startup resolver, the suggestion) on random port lists, then drive
both watchers tick by tick through the same random story: devices plugged,
pulled, renamed and dropped into the bootloader, the link going busy and idle,
a manual connect, dials refused and failing, and the store writes, session
state and log lines each produces. Last, both start and stop their thread.
"""
from __future__ import annotations

import logging
import random
import threading
import time
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import autoconnect as rust_side  # noqa: E402
from tests.reference import autoconnect_python as reference  # noqa: E402

MODULES = (reference, rust_side)

ROWS = [
    {"device": "/dev/ttyACM0", "description": "Pixhawk FMU v6X", "hwid": "USB VID:PID=26AC:0032 SER=0001"},
    {"device": "/dev/ttyACM1", "description": "Pixhawk FMU v6X", "hwid": "USB VID:PID=26AC:0032 SER=0001"},
    {"device": "/dev/ttyACM2", "description": "Pixhawk FMU v6X", "hwid": "USB VID:PID=26AC:0032 SER=0002"},
    {"device": "/dev/ttyACM0", "description": "PX4 BL FMU v6X.x", "hwid": "USB VID:PID=26AC:0011"},
    {"device": "/dev/ttyUSB0", "description": "FT232R USB UART", "hwid": "USB VID:PID=0403:6001"},
    {"device": "/dev/ttyUSB1", "description": "FT232R USB UART", "hwid": "USB VID:PID=0403:6001 SER=A1"},
    {"device": "/dev/cu.usbmodem01", "description": "CubeOrange", "hwid": "USB VID:PID=2DAE:1016 SER=77"},
    {"device": "COM7", "description": "Silicon Labs CP210x", "hwid": "USB VID:PID=10C4:EA60"},
    {"device": "/dev/ttyS0", "description": "", "hwid": ""},
    {"device": "/dev/pts/3", "description": "", "hwid": ""},
    {"device": "/dev/cu.Bluetooth-Incoming-Port", "description": "", "hwid": ""},
    {"device": "", "description": "x", "hwid": ""},
    {"device": None, "description": None, "hwid": None},
    {"device": "/dev/ttyACM3"},
    "not a row",
    None,
]

CONNECTIONS = ["serial:/dev/ttyACM0:115200", "serial:/dev/ttyACM1:57600", "serial:/dev/ttyUSB0:115200",
               "serial:COM7:921600", "udp:0.0.0.0:14550", "tcp:127.0.0.1:5760", "", "   ", None, 14550,
               "serial:/dev/ttyACM0", "  serial:/dev/ttyUSB1:57600  "]


def port_tuple(port: Any) -> Any:
    if port is None:
        return None
    return (port.device, port.description, port.hwid, port.kind)


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", call(*args, **kwargs))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def validator(conn: str) -> None:
    if "COM" in conn or conn.strip().startswith("tcp"):
        raise ValueError("refused")


@pytest.mark.parametrize("seed", range(20))
def test_the_policy_matches(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(150):
        rows = rng.sample(ROWS, rng.randint(0, 7))
        cli = rng.choice(CONNECTIONS)
        configured = rng.choice(CONNECTIONS)
        flags = [rng.random() < 0.8 for _ in range(3)]
        check = rng.choice([None, validator])
        session_flags = dict(enabled=rng.random() < 0.9, usb=flags[0], sik=flags[1],
                             manual_override=rng.random() < 0.2)
        link = rng.choice(CONNECTIONS)
        baud = rng.choice([57600, "115200", 9.7])
        results = []
        for module in MODULES:
            ports = module.classify_ports(rows)
            decision = module.resolve_startup_connection(
                cli, configured, ports, usb_enabled=flags[0], sik_enabled=flags[1],
                udp_fallback_enabled=flags[2], validator=check,
            )
            session = module.SessionState(**session_flags)
            suggestion = module.suggest_connection({"link_connection": link}, ports, session)
            device = ports[0].device if ports else "/dev/ttyACM0"
            results.append((
                [port_tuple(p) for p in ports],
                (decision.connection_string, decision.reason, decision.detail),
                module.pick_usb_serial(ports, usb_enabled=flags[0], sik_enabled=flags[1]),
                port_tuple(module._pick(ports, "usb")), port_tuple(module._pick(ports, "sik")),
                [module._usable(p) for p in ports],
                module._baud_for(device, link, configured),
                outcome(module._serial_device, link), module._is_valid(cli, check),
                outcome(module.serial_connection_string, device, baud),
                None if suggestion is None else (suggestion.connection_string, suggestion.kind,
                                                 suggestion.device, suggestion.reason),
            ))
        assert results[1] == results[0]


def test_classify_ports_survives_what_python_survived() -> None:
    for rows in (None, [], (), [{"device": "/dev/ttyACM0"}], [1, 2], iter(ROWS)):
        listed = list(rows) if rows is not None and not isinstance(rows, (list, tuple)) else rows
        results = [outcome(lambda m=m, rows=listed: [port_tuple(p) for p in m.classify_ports(rows)]) for m in MODULES]
        assert results[1] == results[0]


def replug(plugged: list) -> None:
    """The same board back under its other name, as Linux does it."""
    for i, row in enumerate(plugged):
        if row is ROWS[0] or row is ROWS[1]:
            plugged[i] = ROWS[1] if row is ROWS[0] else ROWS[0]


# ------------------------------------------------------------- the watcher

class Bridge:
    def __init__(self, rng: random.Random) -> None:
        self.rng = rng
        self.calls: list = []
        self.connection = "udp:0.0.0.0:14550"
        self.running = True

    def is_running(self) -> bool:
        return self.running

    def connection_string(self) -> str:
        if self.rng.random() < 0.05:
            raise RuntimeError("busy")
        return self.connection

    def validate_connection(self, conn: str) -> None:
        self.calls.append(("validate", conn))
        if conn.startswith("serial:COM"):
            raise ValueError("no such port here")

    def switch_connection(self, conn: str) -> None:
        self.calls.append(("switch", conn))
        if self.rng.random() < 0.1:
            raise OSError("port busy")
        self.connection = conn


class Store:
    def __init__(self) -> None:
        self.snapshot: dict = {"link_status": "connected"}
        self.updates: list = []
        self.stale = False

    def get_snapshot(self) -> dict:
        return dict(self.snapshot)

    def is_stale(self, after: float) -> bool:
        return self.stale

    def update(self, **kwargs: Any) -> None:
        self.updates.append(kwargs)
        self.snapshot.update(kwargs)


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage(), bool(record.exc_info)))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    log = logging.getLogger("corvus.autoconnect")
    previous = log.level
    log.addHandler(handler)
    log.setLevel(logging.DEBUG)
    reference.logger.addHandler(handler)
    reference.logger.setLevel(logging.DEBUG)
    yield handler
    log.removeHandler(handler)
    log.setLevel(previous)
    reference.logger.removeHandler(handler)


def session_tuple(session: Any) -> Any:
    return (session.enabled, session.usb, session.sik, session.manual_override, session.last_suggestion_key,
            session.last_dial_key, session.reason, session.winner)


@pytest.mark.parametrize("seed", range(25))
def test_a_watcher_story_matches(seed: int, logs: Logs, monkeypatch: pytest.MonkeyPatch) -> None:
    results = []
    for module in MODULES:
        monkeypatch.setattr(module, "time", type("T", (), {"time": staticmethod(lambda: 1700000000.0)}))
        logs.lines.clear()
        rng = random.Random(seed)
        bridge = Bridge(random.Random(seed + 1000))
        store = Store()
        session = module.SessionState()
        plugged = rng.sample(ROWS[:8], rng.randint(0, 3))
        busy = [rng.choice(["", "/dev/ttyACM2", "/dev/ttyUSB1"])]

        def busy_fn(_b: list = busy) -> Any:
            if _b[0] == "boom":
                raise RuntimeError("fleet gone")
            return _b

        watcher = module.AutoConnectWatcher(
            bridge=bridge, store=store, session=session,
            list_ports_fn=lambda _p=plugged: list(_p), busy_devices_fn=busy_fn,
        )
        log: list = []
        for step in range(120):
            roll = rng.random()
            if roll < 0.15 and len(plugged) < 5:
                plugged.append(rng.choice(ROWS[:8]))
            elif roll < 0.25 and plugged:
                plugged.pop(rng.randrange(len(plugged)))
            elif roll < 0.32:
                store.snapshot["link_status"] = rng.choice(["connected", "disconnected", "reconnecting",
                                                            "Disconnected", "connecting", None])
            elif roll < 0.38:
                store.stale = not store.stale
            elif roll < 0.41:
                bridge.running = not bridge.running
            elif roll < 0.43:
                session.note_manual_connect()
            elif roll < 0.45:
                session.apply_config({"usb": rng.random() < 0.7, "sik": rng.random() < 0.7,
                                      "enabled": rng.random() < 0.9})
            elif roll < 0.48:
                busy[0] = rng.choice(["", "/dev/ttyACM2", "/dev/ttyUSB1", "boom"])
            elif roll < 0.52:
                bridge.connection = rng.choice(CONNECTIONS[:6])
            elif roll < 0.54:
                store.snapshot["link_connection"] = rng.choice(CONNECTIONS)
            elif roll < 0.62:
                replug(plugged)
                store.snapshot["link_status"] = "reconnecting"
                store.stale = True
            log.append((step, outcome(watcher.tick), session_tuple(session), watcher._link_identity))
        results.append((log, bridge.calls, store.updates, logs.lines[:]))
    assert results[1] == results[0]


def test_the_stories_reach_every_answer(logs: Logs, monkeypatch: pytest.MonkeyPatch) -> None:
    seen: set[str] = set()
    for seed in range(25):
        rng = random.Random(seed)
        bridge = Bridge(random.Random(seed + 1000))
        store = Store()
        session = rust_side.SessionState()
        plugged = rng.sample(ROWS[:8], rng.randint(0, 3))
        watcher = rust_side.AutoConnectWatcher(bridge=bridge, store=store, session=session,
                                               list_ports_fn=lambda _p=plugged: list(_p))
        for _ in range(120):
            roll = rng.random()
            if roll < 0.15 and len(plugged) < 5:
                plugged.append(rng.choice(ROWS[:8]))
            elif roll < 0.25 and plugged:
                plugged.pop(rng.randrange(len(plugged)))
            elif roll < 0.35:
                store.snapshot["link_status"] = rng.choice(["connected", "disconnected", "reconnecting"])
            elif roll < 0.42:
                store.stale = not store.stale
            elif roll < 0.44:
                bridge.running = not bridge.running
            elif roll < 0.45:
                session.note_manual_connect()
            elif roll < 0.55:
                replug(plugged)
                store.snapshot["link_status"] = "reconnecting"
                store.stale = True
            seen.add(str(outcome(watcher.tick)[1]).split(":")[0])
    for answer in ("dialled", "followed", "suggested", "noop", "dial-failed"):
        assert answer in seen, (answer, seen)


def test_the_watcher_thread_starts_and_stops_alike() -> None:
    results = []
    for module in MODULES:
        ticks: list = []
        store = Store()
        watcher = module.AutoConnectWatcher(bridge=Bridge(random.Random(0)), store=store,
                                            session=module.SessionState(), list_ports_fn=list, poll_s=0.05)
        real_tick = watcher.tick
        watcher.tick = lambda _t=ticks, _r=real_tick: (_t.append(1), _r())[1]
        watcher.start()
        thread = watcher._thread
        names = [t.name for t in threading.enumerate()].count("autoconnect-watcher")
        watcher.start()
        same = watcher._thread is thread
        time.sleep(0.3)
        watcher.stop()
        results.append((thread.name, thread.daemon, names, same, bool(ticks), watcher.is_alive(),
                        thread.is_alive(), "autoconnect-watcher" in {t.name for t in threading.enumerate()}))
    assert results[1] == results[0]
    assert results[0][:5] == ("autoconnect-watcher", True, 1, True, True)
