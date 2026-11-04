"""The Rust forwarder against the Python one it replaced.

``tests/reference/mavlink_forwarder_python.py`` keeps the second station
forwarder as it was in Python. The tests compare the frame splitter on random
byte runs, endpoint parsing, the static endpoint list, the vehicle link loop
check, name resolution, and a live exchange over real UDP sockets: a station
that sends whole, split, junk and Corvus-id frames from a configured and from
an unknown address while frames are mirrored out, with commanding on and off.
"""
from __future__ import annotations

import logging
import random
import socket
import time
from typing import Any

import pytest

pytest.importorskip("corvus_link")

import corvus.mavlink_forwarder as rust_side  # noqa: E402
from tests.reference import mavlink_forwarder_python as reference  # noqa: E402

MODULES = (reference, rust_side)


def frame_v1(sys_id: int = 1, payload: bytes = b"\x01\x02") -> bytes:
    return bytes([0xFE, len(payload), 0, sys_id, 1, 0]) + payload + b"\xaa\xbb"


def frame_v2(sys_id: int = 1, payload: bytes = b"\x01\x02\x03", signed: bool = False) -> bytes:
    head = bytes([0xFD, len(payload), 0x01 if signed else 0, 0, 0, sys_id, 1, 0, 0, 0])
    return head + payload + b"\xaa\xbb" + (bytes(13) if signed else b"")


def random_run(rng: random.Random) -> bytes:
    out = b""
    for _ in range(rng.randint(0, 8)):
        roll = rng.random()
        if roll < 0.35:
            out += frame_v1(rng.randrange(256), bytes(rng.randrange(256) for _ in range(rng.randint(0, 40))))
        elif roll < 0.7:
            out += frame_v2(rng.randrange(256), bytes(rng.randrange(256) for _ in range(rng.randint(0, 60))),
                            signed=rng.random() < 0.3)
        else:
            out += bytes(rng.randrange(256) for _ in range(rng.randint(1, 30)))
    if out and rng.random() < 0.4:
        out = out[:rng.randrange(len(out))]
    return out


@pytest.mark.parametrize("seed", range(10))
def test_frames_split_alike(seed: int) -> None:
    rng = random.Random(seed)
    for _ in range(500):
        run = random_run(rng)
        assert rust_side.split_frames(run) == reference.split_frames(run), run
        for frame in rust_side.split_frames(run)[0] + [run[:7], b"", b"\xfe\x00\x00\x2a\x01\x00"]:
            assert rust_side.frame_source_system(frame) == reference.frame_source_system(frame)


@pytest.mark.parametrize("text", [
    "127.0.0.1:14550", "localhost", "host:0", "host:65536", "host:+12", "host: 12 ", ":14550", "[::1]:14550",
    "a:b:c:14560", "host:x", "", "   ", None, 14550, "host:1_000", "ü:14550", "host:99999999999999999999",
], ids=repr)
@pytest.mark.parametrize("default", [14550, 0])
def test_endpoints_parse_alike(text: Any, default: int) -> None:
    assert rust_side.parse_endpoint(text, default) == reference.parse_endpoint(text, default)


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list[tuple[str, str]] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage()))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    log = logging.getLogger("corvus.forward")
    previous = log.level
    log.addHandler(handler)
    log.setLevel(logging.DEBUG)
    yield handler
    log.removeHandler(handler)
    log.setLevel(previous)


@pytest.mark.parametrize("endpoints", [(), ("127.0.0.1:14560", "bad:x", "127.0.0.1:14560", "127.0.0.1:14551", "lan:7")],
                         ids=["none", "mixed"])
@pytest.mark.parametrize("own", [("127.0.0.1", 14550), ("0.0.0.0", 14550), ("10.0.0.5", 14550), None, "raise"])
def test_the_static_list_and_loop_check_match(endpoints: tuple, own: Any, logs: Logs) -> None:
    def link() -> Any:
        if own == "raise":
            raise OSError("reconnecting")
        return own

    results = []
    for module in MODULES:
        logs.lines.clear()
        fwd = module.MavlinkForwarder(endpoints=endpoints, link_address=link)
        fwd._local_hosts = frozenset({"10.0.0.5"})
        checks = [fwd._is_vehicle_link(addr) for addr in
                  [("127.0.0.1", 14550), ("127.0.0.2", 14550), ("10.0.0.5", 14550), ("10.0.0.6", 14550),
                   ("127.0.0.1", 14560), ("127.0.0.1", "14550")]]
        fwd._note_loop(("127.0.0.1", 14550))
        fwd._note_loop(("127.0.0.1", 14550))
        results.append((fwd._static, checks, fwd._loop_text(), logs.lines[:]))
    assert results[1] == results[0]


def test_name_resolution_matches(monkeypatch: pytest.MonkeyPatch, logs: Logs) -> None:
    calls: list = []
    real = socket.getaddrinfo

    def lookup(host: str, port: int, *args: Any) -> Any:
        calls.append((host, port))
        if host.endswith(".invalid"):
            raise socket.gaierror(8, "nodename nor servname provided")
        return real("127.0.0.1", port, *args)

    monkeypatch.setattr(socket, "getaddrinfo", lookup)
    results = []
    for module in MODULES:
        logs.lines.clear()
        calls.clear()
        clock = [100.0]
        monkeypatch.setattr(module, "time", type("T", (), {"monotonic": staticmethod(lambda _c=clock: _c[0]),
                                                            "sleep": staticmethod(lambda s: None)}))
        fwd = module.MavlinkForwarder(endpoints=("lan.example:14560", "gone.invalid:7", "127.0.0.1:14570"))
        fwd._resolve_static(force=True)
        first = (list(fwd._resolved), list(fwd._unresolved))
        fwd._resolve_static()
        clock[0] += 31
        fwd._resolve_static()
        results.append((first, list(fwd._resolved), list(fwd._unresolved), calls[:], logs.lines[:]))
    assert results[1] == results[0]


# ----------------------------------------------------------- live over UDP

def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def drain(sock: socket.socket, wait: float = 0.4) -> list[bytes]:
    got: list[bytes] = []
    sock.settimeout(0.05)
    end = time.monotonic() + wait
    while time.monotonic() < end:
        try:
            got.append(sock.recvfrom(65535)[0])
        except (TimeoutError, OSError):
            pass
    return got


@pytest.mark.parametrize("commands", [False, True])
def test_a_live_exchange_matches(commands: bool, logs: Logs) -> None:
    results = []
    for module in MODULES:
        logs.lines.clear()
        station = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        station.bind(("127.0.0.1", 0))
        stranger = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        stranger.bind(("127.0.0.1", 0))
        injected: list[bytes] = []
        listen = free_port()
        fwd = module.MavlinkForwarder(
            lambda frame, _i=injected: (_i.append(frame), True)[1],
            host="127.0.0.1", port=station.getsockname()[1], listen_port=listen, allow_commands=commands,
        )
        try:
            assert fwd.start()
            for n in range(5):
                fwd.feed(frame_v1(1, bytes([n])))
            mirrored = drain(station)
            whole = frame_v2(255, b"\x10\x20")
            station.sendto(whole, ("127.0.0.1", listen))
            station.sendto(whole[:5], ("127.0.0.1", listen))
            time.sleep(0.05)
            station.sendto(whole[5:], ("127.0.0.1", listen))
            station.sendto(b"junk" + frame_v1(254), ("127.0.0.1", listen))
            stranger.sendto(frame_v1(200), ("127.0.0.1", listen))
            stranger.sendto(b"\x00\x01\xfd", ("127.0.0.1", listen))
            time.sleep(0.4)
            fwd.feed(frame_v1(1, b"after"))
            to_station = drain(station)
            to_stranger = drain(stranger)
            status = fwd.status()
        finally:
            fwd.stop()
            station.close()
            stranger.close()
        for key in ("listen_port", "listen_port_requested", "port", "peers", "targets", "endpoints",
                    "commands_refused_from"):
            status.pop(key)
        lines = [(level, text.split(" from ")[0].split(" to ")[0]) for level, text in logs.lines]
        results.append((mirrored, injected, to_station, to_stranger, status, sorted(set(lines)),
                        fwd._rx_thread, fwd._tx_thread))
    assert results[1] == results[0]
    mirrored, injected, *_ = results[0]
    assert len(mirrored) == 5
    assert (len(injected) == 2) is commands
