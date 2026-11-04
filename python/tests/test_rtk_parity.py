"""The Rust RTK half against the Python one it replaced.

``tests/reference/rtk_python.py``, ``rtk_service_python.py`` and
``mavlink_bridge_rtcm_python.py`` keep the RTK protocol module, the base
station session and the bridge's GPS_RTCM_DATA injection as they were in
Python. The tests compare the wire formats on random input (RTCM framing,
fragments, every UBX message builder with odd numbers too, the two reports a
receiver sends back), the settings resolver on random config blocks, the
NTRIP request and response parsing against scripted casters and a real TCP
server, each step of the session against scripted receivers on a fake clock,
a whole session on its thread, and the injection with its counters.
"""
from __future__ import annotations

import logging
import math
import random
import socket
import threading
import time
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import mavlink_bridge as bridge_module  # noqa: E402
from corvus import rtk as rust_rtk  # noqa: E402
from corvus import rtk_service as rust_service  # noqa: E402
from tests.reference import mavlink_bridge_rtcm_python as reference_rtcm  # noqa: E402
from tests.reference import rtk_python as reference_rtk  # noqa: E402
from tests.reference import rtk_service_python as reference_service  # noqa: E402
from tests.test_rtk_service import FakeBridge, FakeReceiver, FakeStore, _mon_ver, _nav_svin, _rtcm  # noqa: E402

RTKS = (reference_rtk, rust_rtk)
SERVICES = (reference_service, rust_service)


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, (bytes, bytearray)):
        return bytes(value)
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return (type(value).__name__, value)
    return value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def both(name: str, *args: Any, **kwargs: Any) -> None:
    results = [outcome(getattr(module, name), *args, **kwargs) for module in RTKS]
    assert results[1] == results[0], (name, args, kwargs)


ODD = [0, 1, -1, 2.5, -0.0, 1e-12, 89.999999999, 90, 180.0, -180.0000001, 1e9, 2**31, -2**31, 2**40, 10**30,
       float("inf"), float("-inf"), float("nan"), "12.5", " 3 ", "x", None, True, [], 1e300]


# ----------------------------------------------------------- RTCM and UBX

def random_stream(rng: random.Random) -> bytes:
    out = b""
    for _ in range(rng.randint(0, 8)):
        roll = rng.random()
        if roll < 0.35:
            out += _rtcm(rng.choice([1005, 1074, 1084, 1230, 4095, 0]), bytes(rng.randrange(256) for _ in range(rng.randint(0, 300))))
        elif roll < 0.5:
            frame = bytearray(_rtcm(1077, bytes(rng.randrange(256) for _ in range(rng.randint(0, 40)))))
            frame[rng.randrange(len(frame))] ^= 0xFF
            out += bytes(frame)
        elif roll < 0.7:
            out += _nav_svin(rng.randrange(1000), rng.random() * 10, rng.random() < 0.5, rng.random() < 0.5)
        elif roll < 0.8:
            out += _mon_ver(rng.choice(["27.11", "18.00", "", "abc", "nan"]), rng.choice(["ZED-F9P", "NEO-M8P", ""]))
        else:
            out += bytes(rng.choice([0xD3, 0xB5, 0x62, rng.randrange(256)]) for _ in range(rng.randint(1, 50)))
    return out


@pytest.mark.parametrize("seed", range(20))
def test_streams_parse_alike(seed: int) -> None:
    rng = random.Random(seed)
    framers = [m.RtcmFramer() for m in RTKS]
    parsers = [m.UbxParser() for m in RTKS]
    for _ in range(40):
        stream = random_stream(rng)
        if rng.random() < 0.05:
            stream += bytes(rng.randrange(256) for _ in range(5000))
        cut = 0
        while cut < len(stream):
            step = rng.randint(1, 400)
            chunk = stream[cut:cut + step]
            cut += step
            got = [(f.feed(chunk), f.frames, f.crc_errors, f.discarded, bytes(f._buf)) for f in framers]
            assert got[1] == got[0]
            got = [(p.feed(bytearray(chunk) if rng.random() < 0.3 else chunk), bytes(p._buf)) for p in parsers]
            assert got[1] == got[0]
        if rng.random() < 0.1:
            for f in framers + parsers:
                f.reset()
        for frame in [stream, stream[:6], b"", bytearray(stream[:40])]:
            both("crc24q", frame)
            both("message_number", frame)
            both("ubx_checksum", frame)
            both("parse_nav_svin", frame[6:])
            both("parse_mon_ver", frame[6:])
    assert framers[1].frames > 0 and rust_rtk.UbxParser().feed(_mon_ver())


@pytest.mark.parametrize("length", [0, 1, 179, 180, 181, 359, 360, 361, 539, 540, 541, 719, 720, 1029])
@pytest.mark.parametrize("sequence", [0, 7, 31, 32, -1, 2**70, "3", 2.9, None])
def test_fragments_match(length: int, sequence: Any) -> None:
    for frame in (bytes(range(256)) * 5)[:length], bytearray(length):
        both("fragments", frame, sequence)


def test_mon_ver_and_nav_svin_fields_match() -> None:
    rng = random.Random(4)
    for _ in range(300):
        payload = bytes(rng.randrange(256) for _ in range(rng.choice([39, 40, 41, 69, 70, 100, 160])))
        extensions = b"".join(
            rng.choice([b"PROTVER=27.50", b"PROTVER 18.00", b"PROTVER=", b"PROTVER=x", b"MOD= ZED-F9P ",
                        b"FWVER=HPG 1.32", b"\x1fMOD=x", b"PROTVER=\xff", b"PROTVER=1_0"]).ljust(30, b"\x00")
            for _ in range(rng.randint(0, 4))
        )
        both("parse_mon_ver", payload[:40] + extensions)
        both("parse_nav_svin", payload)
        version = reference_rtk.parse_mon_ver(payload[:40] + extensions)
        both("receiver_label", version)
        both("supports_valset", version)
    for version in [None, {}, {"protocol": "x"}, {"protocol": None, "model": " M8P "}, {"protocol": 1e20},
                    {"protocol": [1]}, {"software": 7, "protocol": 27}]:
        both("receiver_label", version)
        both("supports_valset", version)


@pytest.mark.parametrize("seed", range(10))
def test_ubx_builders_match(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(100):
        a, b, c, d = (rng.choice(ODD) for _ in range(4))
        msm7 = rng.random() < 0.5
        both("survey_in_frames", a, b, msm7)
        both("legacy_survey_in_frames", a, b, msm7)
        both("fixed_position_frames", a, b, c, d, msm7)
        both("legacy_fixed_position_frames", a, b, c, d, msm7)
        both("fixed_position_frames", rng.uniform(-90, 90), rng.uniform(-180, 180), rng.uniform(-100, 3000))
        both("legacy_fixed_position_frames", rng.uniform(-90, 90), rng.uniform(-180, 180), rng.uniform(-100, 3000))
        both("_hp_split", a, rng.choice([1e9, 1e4]))
        both("ubx_frame", rng.choice([6, 0x1FF, -1, True]), rng.choice([0x8A, 300]), bytes(rng.randrange(256) for _ in range(rng.randint(0, 20))))
        both("valset", [(rng.choice([0x20030001, 0x4003000B, 0x30210001, 0x10000001, -1, 2**40]),
                         rng.choice([0, -37, 255, 2**33, -2**70]))
                        for _ in range(rng.randint(0, 4))], rng.choice([1, 7, 0x1FF]))
        both("valset", [[0x20030001, 1], (1, 2, 3)])
        both("valset", [(0x20030001, 1.5)])
        both("legacy_cfg_msg", rng.choice([0xF5, 0x1F5]), rng.randrange(300), rng.randrange(300))
        both("poll_frame", 0x0A, 0x04)
        both("_value_bytes", rng.choice([0x20030001, 0x30210001, 0x40030009]), rng.choice([0, -1, 2**40, -37]))
    for name in ("stop_output_frames", "activate_output_frames", "disable_base_frames", "legacy_stop_output_frames",
                 "legacy_activate_output_frames", "legacy_disable_base_frames"):
        both(name)
        both(name, True)
    both("activate_output_frames", False, False)
    assert reference_rtk.rtcm_keys(True) == rust_rtk.rtcm_keys(True)


# ---------------------------------------------------------------- settings

def random_settings(rng: random.Random) -> Any:
    if rng.random() < 0.05:
        return rng.choice([None, [], "x", 3])
    raw: dict = {}
    for key, choices in {
        "enabled": [True, False, 1, "yes"], "source": ["usb", "ntrip", "NTRIP", 3], "mode": ["survey", "fixed", ""],
        "device": ["  /dev/ttyACM0 ", "x" * 300, 5, None], "baud": [0, 38400, -5, 10**12, 3.7, True, "9600", float("nan")],
        "survey_accuracy": ODD, "survey_duration": ODD, "msm7": [True, False, "true"],
    }.items():
        if rng.random() < 0.7:
            raw[key] = rng.choice(choices)
    if rng.random() < 0.7:
        raw["fixed"] = {k: rng.choice(ODD) for k in ("latitude", "longitude", "altitude", "accuracy") if rng.random() < 0.8}
    if rng.random() < 0.7:
        raw["ntrip"] = {"host": rng.choice([" caster.example ", "", "h" * 300, 7]),
                        "port": rng.choice([2101, 0, 70000, "2101", 2.5, True]),
                        "mountpoint": rng.choice(["MSM4", " /RTCM3 ", ""]),
                        "username": rng.choice(["u", "", None]), "password": rng.choice(["p", "", "ü"])}
    return raw


@pytest.mark.parametrize("seed", range(10))
def test_settings_resolve_alike(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(200):
        raw = random_settings(rng)
        both("settings", raw)
        resolved = reference_rtk.settings(raw)
        both("public_settings", resolved)
        both("public_settings", None)
        both("fixed_position_problem", resolved["fixed"])
        both("fixed_position_problem", (raw or {}).get("fixed") if isinstance(raw, dict) and isinstance(raw.get("fixed"), dict) else {})
        both("ntrip_problem", resolved["ntrip"])
        survey = rng.choice([None, {}, {"valid": True, "active": False}, {"duration": rng.choice(ODD[:16]),
                             "accuracy": rng.choice([0, 0.5, 3.0, None, -1]), "valid": rng.random() < 0.3,
                             "active": True}])
        both("survey_progress", survey, rng.choice([180, 0, 1, -5, 10**6]), rng.choice([2.0, 0.0, -1.0, 1, float("nan")]))
    both("defaults")


# -------------------------------------------------------------- NTRIP

class Caster:
    """A socket that answers with scripted chunks, then timeouts or a close."""

    def __init__(self, chunks: list[Any]) -> None:
        self.chunks = list(chunks)
        self.sent = b""
        self.closed = False

    def sendall(self, data: bytes) -> None:
        if self.chunks and self.chunks[0] == "send-fails":
            self.chunks.pop(0)
            raise ConnectionResetError(54, "Connection reset by peer")
        self.sent += data

    def recv(self, n: int) -> bytes:
        if not self.chunks:
            return b""
        item = self.chunks.pop(0)
        if isinstance(item, BaseException):
            raise item
        return item

    def close(self) -> None:
        self.closed = True


ANSWERS = [
    [b"ICY 200 OK\r\n\r\n" + _rtcm(1005)],
    [b"HTTP/1.1 200 OK\r\nNtrip-Version: Ntrip/2.0\r\n", b"\r\n\r\n"],
    [b"HTTP/1.1 401 Unauthorized\r\n\r\n"],
    [b"HTTP/1.1 404 Not Found\r\n\r\n"],
    [b"SOURCETABLE 200 OK\r\n\r\nSTR;x"],
    [b"\xff\x85 junk \xa0\r\n\r\n"],
    [b"\r\n\r\n"],
    [TimeoutError("timed out"), b"ICY 200 OK\r\n\r\n"],
    [ConnectionResetError(54, "Connection reset by peer")],
    [b"x" * 9000],
    [],
    ["send-fails"],
]


class Clock:
    def __init__(self) -> None:
        self.t = 1000.0

    def monotonic(self) -> float:
        self.t += 0.25
        return self.t

    def sleep(self, seconds: float) -> None:
        self.t += seconds

    def time(self) -> float:
        return 1.7e9 + self.t


@pytest.fixture
def clocks(monkeypatch: pytest.MonkeyPatch) -> Any:
    made = []
    for module in SERVICES:
        clock = Clock()
        monkeypatch.setattr(module, "time", clock)
        made.append(clock)
    return made


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage()))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    log = logging.getLogger("corvus.rtk")
    previous = log.level
    log.addHandler(handler)
    log.setLevel(logging.DEBUG)
    yield handler
    log.removeHandler(handler)
    log.setLevel(previous)


@pytest.mark.parametrize("answer", range(len(ANSWERS)))
@pytest.mark.parametrize("ntrip", [
    {"host": "caster.example", "port": 2101, "mountpoint": "/MSM4", "username": "user", "password": "päss"},
    {"host": "caster.example", "port": "2102", "mountpoint": "RTCM3", "username": "", "password": ""},
    {"host": "", "mountpoint": "x"},
    {"host": "hü", "port": 1, "mountpoint": "m\ud800"},
], ids=["auth", "anonymous", "no-host", "odd"])
def test_an_ntrip_connect_matches(answer: int, ntrip: dict, clocks: Any, logs: Logs) -> None:
    results = []
    for module in SERVICES:
        logs.lines.clear()
        caster = Caster(ANSWERS[answer])
        opened: list = []
        service = module.RtkService(FakeBridge(), FakeStore(), {"enabled": True, "source": "ntrip", "ntrip": ntrip},
                                    open_socket=lambda h, p, t, _o=opened, _c=caster: (_o.append((h, p, t)), _c)[1])
        transport = outcome(service._acquire_ntrip, service.settings())
        pending = getattr(service, "_transport", None)
        results.append((transport[0], transport[1:] if transport[0] == "error" else None,
                        caster.sent, caster.closed, opened, service.status()["state"], service.status()["message"],
                        service.status()["device"], logs.lines[:], pending))
    assert results[1] == results[0]


def test_the_request_and_header_match_on_their_own(clocks: Any) -> None:
    for args in [("h", 2101, "m", "", ""), ("h", 2101, "m", "u", ""), ("hé", "x", "ü/", "ä", "ö"),
                 ("h", 2101, "m", "u\ud800", "p")]:
        results = [outcome(module._ntrip_request, *args) for module in SERVICES]
        assert results[1] == results[0]
    for chunks in ANSWERS:
        results = [outcome(module._read_ntrip_header, Caster(chunks)) for module in SERVICES]
        assert results[1] == results[0]


def test_a_real_caster_is_read_alike() -> None:
    """The default socket against a real TCP server: request, header, stream."""
    server = socket.socket()
    server.bind(("127.0.0.1", 0))
    server.listen(4)
    port = server.getsockname()[1]
    requests: list[bytes] = []

    def answer() -> None:
        for _ in range(2):
            conn, _ = server.accept()
            with conn:
                requests.append(conn.recv(4096))
                conn.sendall(b"ICY 200 OK\r\n\r\n" + _rtcm(1005))
                time.sleep(0.1)
                conn.sendall(_rtcm(1074) * 3)
                time.sleep(0.3)

    worker = threading.Thread(target=answer, daemon=True)
    worker.start()
    results = []
    try:
        for module in SERVICES:
            service = module.RtkService(FakeBridge(), FakeStore(), {"enabled": True, "source": "ntrip", "ntrip": {
                "host": "127.0.0.1", "port": port, "mountpoint": "MSM4", "username": "u", "password": "p"}})
            transport = service._acquire_ntrip(service.settings())
            got = transport.pending
            deadline = time.monotonic() + 2
            while got.count(b"\xd3") < 4 and time.monotonic() < deadline:
                got += transport.read()
            transport.close()
            after = outcome(transport.read)
            results.append((got, transport.label, after[0]))
    finally:
        worker.join(timeout=3)
        server.close()
    assert requests[0] == requests[1]
    assert results[1][:2] == results[0][:2]
    # A closed socket: an error on both sides (the reference's Python socket
    # raises EBADF, the Rust one too).
    assert results[0][2] == results[1][2] == "error"


def test_a_refused_caster_fails_alike() -> None:
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    results = [outcome(module.RtkService._default_open_socket, "127.0.0.1", port, 2.0) for module in SERVICES]
    assert results[1] == results[0]
    results = [outcome(module.RtkService._default_open_socket, "no-such-host.invalid", port, 2.0) for module in SERVICES]
    assert results[1][:2] == results[0][:2]


# ------------------------------------------------- the session, step by step

class ScriptedPort:
    """A serial port whose reads come from a script, recording writes."""

    def __init__(self, script: list[Any], fail_write: bool = False) -> None:
        self.script = list(script)
        self.writes: list[bytes] = []
        self.closed = False
        self.fail_write = fail_write

    @property
    def in_waiting(self) -> int:
        if self.script and self.script[0] == "waiting-raises":
            self.script.pop(0)
            raise OSError("gone")
        return len(self.script[0]) if self.script and isinstance(self.script[0], bytes) else 0

    def read(self, n: int) -> bytes:
        if not self.script:
            return b""
        item = self.script.pop(0)
        if isinstance(item, BaseException):
            raise item
        return item if isinstance(item, bytes) else b""

    def write(self, data: bytes) -> None:
        if self.fail_write:
            raise OSError(5, "Input/output error")
        self.writes.append(bytes(data))

    def close(self) -> None:
        self.closed = True


SCRIPTS = [
    [_mon_ver()],
    [_mon_ver("18.00", "NEO-M8P")],
    [b"\x00junk", _rtcm(1005)],
    [b"", b"", b""],
    [OSError("device gone")],
    ["waiting-raises", _mon_ver()],
    [_nav_svin(0, 0.0, False, False), _mon_ver()],
]


@pytest.mark.parametrize("script", range(len(SCRIPTS)))
@pytest.mark.parametrize("pinned", ["", "/dev/ttyACM9", "/dev/ttyUSB7"])
@pytest.mark.parametrize("baud", [0, 115200])
def test_acquiring_a_serial_base_matches(script: int, pinned: str, baud: int, clocks: Any, logs: Logs) -> None:
    results = []
    for module in SERVICES:
        logs.lines.clear()
        ports: list = []

        def open_port(device: str, rate: int, _p: list = ports) -> Any:
            if device == "/dev/ttyUSB7":
                raise PermissionError(13, "Permission denied")
            port = ScriptedPort(SCRIPTS[script], fail_write=rate == 9600)
            _p.append((device, rate, port))
            return port

        bridge = FakeBridge(connection="serial:/dev/ttyUSB7:57600")
        service = module.RtkService(bridge, FakeStore(), {"enabled": True, "device": pinned, "baud": baud},
                                    open_port=open_port, list_ports=lambda: [
                                        {"device": "/dev/ttyACM9", "description": "u-blox GNSS receiver", "hwid": "USB VID:PID=1546:01A9"},
                                        {"device": "/dev/ttyUSB7", "description": "u-blox GNSS receiver", "hwid": "USB VID:PID=1546:01A9"},
                                        {"device": "/dev/ttyACM0", "description": "PX4 FMU", "hwid": "USB VID:PID=26AC:0011"}])
        got = outcome(service._acquire_serial, service.settings())
        transport = got[1] if got[0] == "ok" else None
        results.append((got[0], got[1:] if got[0] == "error" else None,
                        None if transport is None else (transport.label, transport.device, transport.baud),
                        [(d, r, p.writes, p.closed) for d, r, p in ports],
                        service._device, service._baud, service._receiver, service.status()["state"],
                        service._candidate_ports(), service._link_device(), logs.lines[:]))
    assert results[1] == results[0]


class QuietingReceiver(FakeReceiver):
    """A base that streams for a while and then falls silent."""

    def __init__(self, reads: int = 60, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.reads_left = reads

    def _react(self, msg_class: int, msg_id: int, payload: bytes) -> None:
        # The M8P generation's messages, which FakeReceiver does not speak.
        if msg_class == reference_rtk.UBX_CLASS_CFG and msg_id == reference_rtk.UBX_ID_CFG_TMODE3:
            self.mode = payload[2]
            self.survey_running = self.mode == 1
            if self.mode != 1:
                self.svin_polls = 0
            return
        if msg_class == reference_rtk.UBX_CLASS_CFG and msg_id == reference_rtk.UBX_ID_CFG_MSG:
            if payload[:2] == bytes((reference_rtk.UBX_CLASS_RTCM3, 0x05)):
                self.streaming = payload[2] > 0
            return
        super()._react(msg_class, msg_id, payload)

    def read(self, size: int = 1) -> bytes:
        self.reads_left -= 1
        if self.reads_left <= 0:
            self.streaming = False
        return super().read(size)


RECEIVERS = [
    dict(identify=True, survey_polls=2),
    dict(identify=True, survey_polls=0, protocol="18.00"),
    dict(identify=False),
]


@pytest.mark.parametrize("receiver", range(len(RECEIVERS)))
@pytest.mark.parametrize("settings", [
    {"mode": "survey", "survey_duration": 60, "survey_accuracy": 1.5},
    {"mode": "survey", "msm7": True},
    {"mode": "fixed", "fixed": {"latitude": 48.08, "longitude": 11.64, "altitude": 545.2, "accuracy": 0.02}},
    {"mode": "fixed"},
])
def test_a_whole_session_matches_step_by_step(receiver: int, settings: dict, clocks: Any, logs: Logs) -> None:
    """Configure, survey and stream on a fake clock, with no thread."""
    results = []
    for module in SERVICES:
        logs.lines.clear()
        fake = QuietingReceiver(**RECEIVERS[receiver])
        bridge = FakeBridge()
        store = FakeStore()
        service = module.RtkService(bridge, store, {"enabled": True, **settings},
                                    open_port=lambda d, b, _f=fake: (setattr(_f, "baud", b), _f)[1],
                                    list_ports=lambda: [{"device": "/dev/ttyACM9", "description": "u-blox GNSS receiver",
                                                         "hwid": "USB VID:PID=1546:01A9"}])
        resolved = service.settings()
        transport = service._acquire(resolved)
        version = dict(service._receiver) if service._receiver else None
        # The session ends itself once the base falls silent long enough.
        session = outcome(service._run_session, transport, resolved, version)
        service._release(transport, resolved, version)
        status = service.status()
        for key in ("source_age", "uptime"):
            status.pop(key)
        results.append((session, fake.writes, fake.mode, fake.closed, len(bridge.injected), bridge.injected[:5],
                        bridge.forgotten, store.updates, _plain(status), logs.lines[:]))
    assert results[1] == results[0]


def test_a_session_on_its_thread_matches() -> None:
    """The real clock and thread: start, go active, stop, put the receiver back."""
    results = []
    for module in SERVICES:
        fake = FakeReceiver()
        bridge = FakeBridge()
        service = module.RtkService(bridge, FakeStore(), {"enabled": True},
                                    open_port=lambda d, b, _f=fake: (setattr(_f, "baud", b), setattr(_f, "closed", False), _f)[2],
                                    list_ports=lambda: [{"device": "/dev/ttyACM9", "description": "u-blox GNSS receiver",
                                                         "hwid": "USB VID:PID=1546:01A9"}])
        service.start()
        name = service._thread.name
        deadline = time.monotonic() + 8
        while service.status()["state"] != "active" and time.monotonic() < deadline:
            time.sleep(0.02)
        state = service.status()["state"]
        while len(bridge.injected) < 4 and time.monotonic() < deadline:
            time.sleep(0.02)
        service.stop(timeout=5.0)
        configured = [p for c, i, p in fake.writes if c == reference_rtk.UBX_CLASS_CFG]
        results.append((name, state, fake.mode, fake.closed, service.is_alive(), service.status()["state"],
                        configured[0], configured[-1], all(f.startswith(b"\xd3") for f in bridge.injected),
                        "rtk-base" in {t.name for t in threading.enumerate()}))
    assert results[1] == results[0]
    assert results[0][:6] == ("rtk-base", "active", 0, True, False, "off")


# ---------------------------------------------------------- the injection

class Mav:
    def __init__(self, fail_at: int | None) -> None:
        self.sent: list = []
        self.fail_at = fail_at

    def gps_rtcm_data_send(self, flags: int, length: int, data: list) -> None:
        if self.fail_at is not None and len(self.sent) == self.fail_at:
            raise OSError("link gone")
        self.sent.append((flags, length, list(data)))


class Conn:
    def __init__(self, fail_at: int | None) -> None:
        self.mav = Mav(fail_at)


class ReferenceBridge(reference_rtcm.RtcmInjectionMixin):
    pass


def injection_bridge(kind: type) -> Any:
    b = kind.__new__(kind)
    b._rtcm_lock = threading.Lock()
    b._send_lock = threading.RLock()
    b._rtcm_sequence = 0
    b._rtcm_bytes = 0
    b._rtcm_messages = 0
    b._rtcm_dropped = 0
    b._rtcm_last_send = 0.0
    b._ready = True
    b._conn = Conn(None)
    b._connection_ready = lambda: b._ready
    return b


@pytest.mark.parametrize("seed", range(10))
def test_injection_matches(seed: int, monkeypatch: pytest.MonkeyPatch) -> None:
    clock = Clock()
    monkeypatch.setattr(reference_rtcm, "time", clock)
    monkeypatch.setattr(bridge_module, "time", clock)
    results = []
    for kind in (ReferenceBridge, bridge_module.MavlinkBridge):
        clock.t = 1000.0
        rng = random.Random(seed)
        b = injection_bridge(kind)
        log = []
        for _ in range(80):
            roll = rng.random()
            if roll < 0.05:
                b._ready = not b._ready
            elif roll < 0.1:
                b._conn = rng.choice([None, Conn(None), Conn(rng.randrange(3))])
            elif roll < 0.13:
                b.forget_rtcm_session()
            frame = rng.choice([b"", bytes(rng.randrange(256) for _ in range(rng.choice([1, 50, 179, 180, 360, 540, 719, 720, 1000])))])
            log.append((outcome(b.inject_rtcm, frame), b._rtcm_sequence, b.rtcm_stats(),
                        None if b._conn is None else list(b._conn.mav.sent)))
        results.append(log)
    assert results[1] == results[0]


def test_injection_logs_alike(monkeypatch: pytest.MonkeyPatch) -> None:
    handler = Logs()
    log = logging.getLogger("corvus.mavlink")
    log.addHandler(handler)
    previous = log.level
    log.setLevel(logging.DEBUG)
    try:
        results = []
        for kind in (ReferenceBridge, bridge_module.MavlinkBridge):
            handler.lines.clear()
            b = injection_bridge(kind)
            b.inject_rtcm(bytes(800))
            b._conn = Conn(0)
            b.inject_rtcm(bytes(10))
            results.append(handler.lines[:])
        assert results[1] == results[0] and len(results[0]) == 2
    finally:
        log.removeHandler(handler)
        log.setLevel(previous)

