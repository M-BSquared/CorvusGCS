"""The Rust SiK engines against the Python modules they replaced.

``tests/reference/sik_config_python.py`` and ``sik_service_python.py`` keep
the radio page's schema and logic and the AT session as they were in Python.
The tests compare the parsers on random radio output, the conversions, the
comparison, the copy and the validation of writes on random radios and
requests (odd values too), the write plans, and then whole AT conversations:
loads, saves, resets, baud scans, refused commands, silent radios and a
bridge that is stopped and restarted around them, on a fake clock, with every
command sent, every log line and every bridge event compared.
"""
from __future__ import annotations

import logging
import math
import random
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import sik_config as rust_config  # noqa: E402
from corvus import sik_service as rust_service  # noqa: E402
from tests.reference import sik_config_python as reference_config  # noqa: E402
from tests.reference import sik_service_python as reference_service  # noqa: E402
from tests.test_sik_service import ATI5_LOCAL, ATI5_REMOTE, FakeBridge, FakePort, FakeStore  # noqa: E402

CONFIGS = (reference_config, rust_config)
SERVICES = (reference_service, rust_service)


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return (type(value).__name__, value)
    return value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def both(name: str, *args: Any, **kwargs: Any) -> None:
    results = [outcome(getattr(module, name), *args, **kwargs) for module in CONFIGS]
    assert results[1] == results[0], (name, args)


ODD = [0, 1, -1, 2, 57, 115, 2000, 2001, 57600, 10**30, -10**30, 2.0, 2.5, -0.0, float("nan"),
       float("inf"), True, False, "57", " 9 ", "x", None, [], "٥٧"]


# ----------------------------------------------------------------- parsers

REGISTER_LINES = [
    "S0: FORMAT=25", "S1: SERIAL_SPEED=57", "S2: AIR_SPEED=64", "S3: NETID=25", "S4: TXPOWER=20",
    "S8: MIN_FREQ=915000", "S9: MAX_FREQ=928000", "S15: MAX_WINDOW=131", "S16: ENCRYPTION_LEVEL=-1",
    "S12=5", "S12SERIAL=5", "S1 2X=3", "S7:  ecc = 0 ", "S5; ECC=0", "S6: MAVLINK=", "S١: A=٢",
    "S99: NEW_THING=7", "s1: lower=1", "OK", "ATI5", "", "  S3: NETID=25  ", "S3: NETID=2 5", "S3: NET ID=25",
    "L/R RSSI: 208/205  L/R noise: 41/38 pkts: 20 txe=0 rxe=0", "L/R RSSI: 1/2 L/R noise: 3/4",
    "l/r rssi: 10 / 20 junk\nL/R noise: 5 /6", "L/R RSSI: 99999999999999999999/1 L/R noise: 0/0",
    "SiK 2.0 on HM-TRP", "RFD SiK 2.7 on RFD900P", "ATI", "rti5", "Error", "ok", "ß ok", "ERROR",
]


def random_text(rng: random.Random) -> Any:
    if rng.random() < 0.05:
        return rng.choice([None, "", 0, 5, [], b"S1: A=1", ["S1: A=1"], False])
    sep = rng.choice(["\r\n", "\n", "\r", "\n\r"])
    return sep.join(rng.choice(REGISTER_LINES) for _ in range(rng.randint(0, 8)))


@pytest.mark.parametrize("seed", range(15))
def test_parsers_agree(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(150):
        text = random_text(rng)
        both("parse_registers", text)
        both("parse_rssi", text)
        both("parse_version", text)


@pytest.mark.parametrize("value", ODD)
def test_conversions_agree(value: Any) -> None:
    both("baud_from_register", value)
    both("register_from_baud", value)
    both("rssi_to_dbm", value)


def test_every_register_and_baud_converts_alike() -> None:
    for n in list(range(0, 2100, 7)) + [57, 115, 230, 1, 2, 4, 9, 19, 38, 57600, 115200, 9600]:
        both("baud_from_register", n)
        both("register_from_baud", n)
    for n in range(0, 300):
        both("rssi_to_dbm", n)
        both("parse_rssi", f"L/R RSSI: {n}/{300 - n} L/R noise: {n % 50}/{n % 70}")


# ------------------------------------------------- radios and their writes

def random_radio(rng: random.Random) -> dict:
    names = [r["name"] for r in reference_config.REGISTERS] + ["EXTRA_ONE", "EXTRA_TWO"]
    registers = {}
    for number, name in enumerate(names):
        if rng.random() < 0.8:
            value = rng.choice([0, 1, 2, 5, 25, 57, 64, 100, 131, 915000, 928000, 868000, 50, 250, 9, 3, -1])
            registers[name] = {"number": number, "value": value}
    return reference_config.describe_radio(registers, version="SiK 2.0", board="HM-TRP")


def random_settings(rng: random.Random, radio: dict) -> Any:
    if rng.random() < 0.05:
        return rng.choice([None, [], "x", 3, [("NETID", 1)]])
    names = list(radio["values"]) + ["NOPE", "format", "netid", 7]
    out: dict = {}
    for _ in range(rng.randint(0, 5)):
        name = rng.choice(names)
        out[name] = rng.choice([0, 1, 2, 5, 25, 30, 57, 64, 100, 128, 499, 500, -1, 20, 21, 915000, 928000, 900000,
                                950000, 1000001, 239999, 2.0, 2.5, True, "5", None, float("nan"), float("inf"),
                                10**30, 131, 33, 19, 3])
    return out


@pytest.mark.parametrize("seed", range(15))
def test_comparing_and_validating_agree(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(120):
        local, remote = random_radio(rng), random_radio(rng)
        for r in (remote, None, {}, {"values": None}):
            both("compare", local, r)
            both("copy_to_remote", local, r)
        both("compare", {"values": {"NETID": "x"}}, {"values": {"NETID": 1}})
        present = {k: {"number": n, "value": v} for n, (k, v) in enumerate(local["values"].items())}
        settings = random_settings(rng, local)
        both("validate_writes", settings, present)
        if rng.random() < 0.3:
            both("validate_writes", settings, {})
        both("describe_radio", present, "v", "b")
        writes = [{"name": "A", "register": rng.randint(0, 20), "value": rng.randint(-5, 900)} for _ in range(rng.randint(0, 4))]
        both("write_plan", writes, rng.choice([True, False, 0, 1, None, "x"]))
    both("schema")


def test_validation_reaches_every_message() -> None:
    seen = set()
    for seed in range(15):
        rng = random.Random(seed)
        for _ in range(120):
            radio = random_radio(rng)
            present = {k: {"number": n, "value": v} for n, (k, v) in enumerate(radio["values"].items())}
            result = outcome(rust_config.validate_writes, random_settings(rng, radio), present)
            seen.add(result[0] if result[0] == "ok" else result[2])
    for expected in ("ok", "settings must be an object", "this radio has no NOPE setting",
                     "FORMAT is set by the radio firmware and cannot be written",
                     "the maximum frequency must be above the minimum frequency"):
        assert expected in seen, (expected, seen)
    assert any(s.startswith("Air data rate does") for s in seen) and any(s.startswith("Network ID must") for s in seen), seen


# ------------------------------------------------------------ AT sessions

class Clock:
    def __init__(self) -> None:
        self.t = 5000.0

    def monotonic(self) -> float:
        self.t += 0.01
        return self.t

    def sleep(self, seconds: float) -> None:
        self.t += seconds


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage(), bool(record.exc_info)))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    log = logging.getLogger("corvus.sik")
    previous = log.level
    log.addHandler(handler)
    log.setLevel(logging.DEBUG)
    yield handler
    log.removeHandler(handler)
    log.setLevel(previous)


def serial_module(port: Any, tried: list | None = None) -> Any:
    """A ``serial`` stand-in that hands back *port* and notes how it was opened."""

    class SerialModule:
        @staticmethod
        def Serial(device: str, baudrate: int = 57600, **kw: Any) -> Any:  # noqa: N802 - pyserial's spelling
            if tried is not None:
                tried.append((device, baudrate, tuple(sorted(kw.items()))))
            port.baudrate = baudrate
            port.closed = False
            return port

    return SerialModule


class OddPort(FakePort):
    """A port that fails in the ways a real one does."""

    def __init__(self, *args: Any, fail: str = "", **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self.fail = fail
        self.reads = 0

    def write(self, data: bytes) -> int:
        if self.fail == "write":
            raise OSError(5, "Input/output error")
        return super().write(data)

    def read(self, count: int) -> bytes:
        self.reads += 1
        if self.fail == "read" and self.reads > 2:
            raise OSError(5, "Input/output error")
        return super().read(count)

    def reset_input_buffer(self) -> None:
        if self.fail == "reset":
            raise OSError(5, "Input/output error")
        super().reset_input_buffer()


SCENARIOS = [
    ("load", {}, {}),
    ("load-no-remote", {"include_remote": False}, {}),
    ("load-silent-remote", {}, {"answers": {"RTI5": ""}}),
    ("load-no-registers", {}, {"answers": {"ATI5": "OK\r\n"}}),
    ("load-baud-scan", {"baud": 115200}, {"baud_required": 38400}),
    ("load-no-escape", {}, {"escape_ok": False}),
    ("load-unreachable", {}, {"baud_required": 12345}),
    ("load-write-fails", {}, {"fail": "write"}),
    ("load-read-fails", {}, {"fail": "read"}),
    ("load-reset-fails", {}, {"fail": "reset"}),
    ("save-both", {"local": {"NETID": 31, "TXPOWER": 14}, "remote": {"NETID": 31, "AIR_SPEED": 32}}, {}),
    ("save-speed", {"local": {"SERIAL_SPEED": 115}}, {}),
    ("save-rejected", {"local": {"NETID": 31}}, {"errors": {"ATS3=31"}}),
    ("save-remote-silent", {"remote": {"NETID": 31}}, {"answers": {"RTI5": ""}}),
    ("save-invalid", {"local": {"NETID": 9999}}, {}),
    ("save-nothing", {"local": {"NETID": 25}}, {}),
    ("reset-local", {"target": "local"}, {}),
    ("reset-remote", {"target": "remote"}, {}),
    ("reset-remote-silent", {"target": "remote"}, {"answers": {"RTI5": ""}}),
    ("reset-refused", {"target": "local"}, {"errors": {"AT&F"}}),
    ("reset-bad-target", {"target": "both"}, {}),
]


def answers_with(extra: dict | None) -> dict:
    answers = {
        "ATI5": ATI5_LOCAL, "RTI5": ATI5_REMOTE, "ATI": "SiK 2.0 on HM-TRP\r\nOK\r\n", "RTI": "SiK 2.0 on HM-TRP\r\nOK\r\n",
        "ATI2": "HM-TRP\r\n", "RTI2": "HM-TRP\r\n",
        "ATI7": "L/R RSSI: 208/205  L/R noise: 41/38 pkts: 20 txe=0 rxe=0\r\n",
    }
    answers.update(extra or {})
    return answers


@pytest.mark.parametrize("name,kwargs,port_options", SCENARIOS, ids=[s[0] for s in SCENARIOS])
@pytest.mark.parametrize("bridge_setup", ["link-port", "other-port", "stopped", "none"])
@pytest.mark.parametrize("armed", [False, True])
def test_sessions_agree(name: str, kwargs: dict, port_options: dict, bridge_setup: str, armed: bool,
                        monkeypatch: pytest.MonkeyPatch, logs: Logs) -> None:
    results = []
    for module in SERVICES:
        logs.lines.clear()
        clock = Clock()
        monkeypatch.setattr(module, "time", clock)
        options = dict(port_options)
        options["answers"] = answers_with(options.get("answers"))
        port = OddPort(**options)
        tried: list = []
        monkeypatch.setattr(module, "_serial", serial_module(port, tried))
        bridge = None if bridge_setup == "none" else FakeBridge(
            device="/dev/ttyUSB0" if bridge_setup != "other-port" else "/dev/ttyUSB5", running=bridge_setup != "stopped")
        service = module.SikService(bridge, FakeStore(armed=armed))
        operation = name.split("-")[0]
        call = {"load": service.load, "save": service.save, "reset": service.reset}[operation]
        result = outcome(call, "/dev/ttyUSB0", **kwargs)
        results.append((result, port.commands, port.closed, tried, None if bridge is None else (bridge.events, bridge.device, bridge.baud, bridge.running),
                        service.status() if bridge is not None else None, logs.lines[:], clock.t))
    assert results[1] == results[0]


def test_the_scenarios_reach_the_main_answers(monkeypatch: pytest.MonkeyPatch) -> None:
    seen = set()
    for name, kwargs, port_options in SCENARIOS:
        clock = Clock()
        monkeypatch.setattr(rust_service, "time", clock)
        options = dict(port_options)
        options["answers"] = answers_with(options.get("answers"))
        port = OddPort(**options)
        monkeypatch.setattr(rust_service, "_serial", serial_module(port))
        service = rust_service.SikService(FakeBridge(), FakeStore())
        call = {"load": service.load, "save": service.save, "reset": service.reset}[name.split("-")[0]]
        result = outcome(call, "/dev/ttyUSB0", **kwargs)
        seen.add(result[0] if result[0] == "ok" else result[2])
    for expected in ("ok", "could not reach a radio on", "the radio answered the escape", "the radio rejected ATS3=31",
                     "the remote radio is not ans", "serial write failed: [Errno 5]", "serial read failed: [Errno 5]"):
        assert any(s == expected or s.startswith(expected) for s in seen), (expected, seen)


def test_the_real_serial_port_is_the_default() -> None:
    from corvus_link import rtk

    assert rust_service._serial.Serial is rtk.SerialPort
    with pytest.raises(OSError):
        rtk.SerialPort("/dev/corvus-no-such-device", 57600, timeout=0.1, write_timeout=2.0)
